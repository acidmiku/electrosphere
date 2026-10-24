// The HDR sun, its lens flare and bloom (src/effects/sun.rs,
// docs/EFFECTS_SUN.md), after fx_common.wgsl (fx_encode, fx_decode).
//
// Passes, in N x VRAM pixels unless noted:
// - post: fs_visibility (into a 1 x 1 target: the share of the sun's disc
//   that shows sky in the depth snapshot), fs_sun (the disc and its aureole
//   over the sky's pixels);
// - late: fs_bright (the bloom's source at 1 x: bright pixels and the
//   sun's light beyond white), fs_down (13 taps) down a mip chain, fs_up
//   (a tent) back up, fs_composite (bloom and lens flare over the picture).
//
// Every pass that writes VRAM reads `shadow`, adds light (the sun and the
// bloom with a soft saturation, 1 - (1 - c) e^-add: never past white,
// smooth; the lens flare as the game adds its own, clamped at white) and
// writes back through the 8 x 8 dither, keeping the mask bit. A pixel it
// adds (almost) nothing to is discarded: it keeps its word exactly.

const ELEMENTS: u32 = 10u;
const PROFILE: u32 = 16u;
const SAMPLES: u32 = 64u;
// Below this much light (of 1) a pixel is left alone.
const NOTHING: f32 = 0.0005;

struct Sun {
    // The world area at N x: origin x, y and size w, h.
    area: vec4<f32>,
    // The projection centre (1 x VRAM pixels), widescreen's k, the scale N.
    centre_k_scale: vec4<f32>,
    // The sun's direction in view space (unit), and H.
    view_h: vec4<f32>,
    // Its projection (N x VRAM pixels), in front of the eye (0 or 1), the
    // disc drawn (0 or 1).
    screen: vec4<f32>,
    // The disc's colour, and its radiance at the centre (times white).
    disc_colour: vec4<f32>,
    // The aureole's and the halo's colour; the visibility radius (N x px).
    glow_colour: vec4<f32>,
    // Disc radius (rad), limb darkening, aureole near strength and width (rad).
    shape: vec4<f32>,
    // Aureole far strength and width (rad); unused.
    aureole: vec4<f32>,
    // Bloom strength (times its gain), threshold, unused, the sun's excess
    // in it.
    bloom: vec4<f32>,
    // Ghosts, halo, star (strengths); the flare on (0 or 1).
    flare: vec4<f32>,
    // Halo radius and width, star length (N x px), star rotation (rad).
    halo: vec4<f32>,
    // The veiling glare: the halo's strength and width (rad), the veil's
    // strength (all times proximity and the setting; times visibility here).
    glare: vec4<f32>,
    // The game's flare elements: centre (N x VRAM px), half size (N x px,
    // vertical; the horizontal one is k times it), intensity.
    elements: array<vec4<f32>, 10>,
    // Each element's colour by radius: PROFILE bins from the centre out.
    profiles: array<vec4<f32>, 160>,
};

@group(0) @binding(0) var<uniform> sun: Sun;
@group(0) @binding(1) var shadow: texture_2d<u32>;
@group(0) @binding(2) var snapshot: texture_2d<f32>;
@group(0) @binding(3) var visibility: texture_2d<f32>;
@group(0) @binding(4) var bloom_texture: texture_2d<f32>;
@group(0) @binding(5) var bilinear: sampler;
@group(0) @binding(6) var source: texture_2d<f32>;
@group(0) @binding(7) var level: texture_2d<f32>;

@vertex
fn vs_sun(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let p = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
}

fn max3(c: vec3<f32>) -> f32 {
    return max(c.r, max(c.g, c.b));
}

// Light `add` over colour `c`: towards white, never past it.
fn saturate_add(c: vec3<f32>, add: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(1.0) - (vec3<f32>(1.0) - c) * exp(-add);
}

// The angle (rad) between the view direction through N x VRAM pixel `p`
// and the sun (from the chord: exact near 0).
fn sun_angle(p: vec2<f32>) -> f32 {
    let n = sun.centre_k_scale.w;
    let h = sun.view_h.w;
    let s = p / n - sun.centre_k_scale.xy;
    let d = normalize(vec3<f32>(s.x / (sun.centre_k_scale.z * h), s.y / h, 1.0));
    return 2.0 * asin(min(length(d - sun.view_h.xyz) * 0.5, 1.0));
}

// The disc at angle `theta`: limb-darkened, its edge antialiased over one
// N x pixel; 0 outside.
fn disc(theta: f32) -> f32 {
    let radius = sun.shape.x;
    let pixel = 1.0 / (sun.centre_k_scale.w * sun.view_h.w);
    let x = min(theta / radius, 1.0);
    let mu = sqrt(max(1.0 - x * x, 0.0));
    let limb = 1.0 - sun.shape.y * (1.0 - mu);
    let cover = clamp((radius - theta) / pixel + 0.5, 0.0, 1.0);
    return cover * limb;
}

// The sky's glow around the sun at angle `theta`.
fn aureole(theta: f32) -> f32 {
    return sun.shape.z * exp(-theta / sun.shape.w) + sun.aureole.x * exp(-theta / sun.aureole.y);
}

// The sun's light (times white) at N x VRAM pixel `p`, where it shows sky.
fn sun_light(p: vec2<f32>) -> vec3<f32> {
    if sun.screen.z == 0.0 || sun.screen.w == 0.0 {
        return vec3<f32>(0.0);
    }
    let theta = sun_angle(p);
    return sun.disc_colour.rgb * (sun.disc_colour.w * disc(theta))
        + sun.glow_colour.rgb * aureole(theta);
}

// Whether the snapshot shows sky at area pixel `local`: the z clear value,
// nothing with a depth drew there (far terrain draws with order 0 but a
// depth; a world primitive without a depth has z 0).
fn sky(local: vec2<i32>) -> bool {
    return textureLoad(snapshot, local, 0).r >= 5.0e29;
}

fn in_area(local: vec2<i32>) -> bool {
    return all(local >= vec2<i32>(0)) && all(local < vec2<i32>(sun.area.zw));
}

// ---- post ----

// The share of the disc (and a little beyond it) where the snapshot shows
// sky, 0 when the sun is behind the eye or off the world area.
@fragment
fn fs_visibility(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    if sun.screen.z == 0.0 {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    let radius = sun.glow_colour.w;
    let k = sun.centre_k_scale.z;
    var seen = 0.0;
    for (var i = 0u; i < SAMPLES; i++) {
        let r = radius * sqrt((f32(i) + 0.5) / f32(SAMPLES));
        let a = f32(i) * 2.3999632;
        let p = sun.screen.xy + vec2<f32>(cos(a) * k, sin(a)) * r;
        let local = vec2<i32>(floor(p - sun.area.xy));
        if in_area(local) && sky(local) {
            seen += 1.0;
        }
    }
    return vec4<f32>(seen / f32(SAMPLES), 0.0, 0.0, 1.0);
}

// The disc and the aureole over the sky's pixels.
@fragment
fn fs_sun(@builtin(position) position: vec4<f32>) -> @location(0) u32 {
    let p = vec2<u32>(position.xy);
    let local = vec2<i32>(p) - vec2<i32>(sun.area.xy);
    if !in_area(local) || !sky(local) {
        discard;
    }
    let add = sun_light(position.xy);
    if max3(add) < NOTHING {
        discard;
    }
    let old = textureLoad(shadow, vec2<i32>(p), 0).r;
    return fx_encode(saturate_add(fx_decode(old), add), p) | (old & 0x8000u);
}

// ---- late: bloom ----

// The bright part of colour `c`: a smooth step of its largest channel from
// the threshold to white.
fn bright(c: vec3<f32>) -> vec3<f32> {
    return c * smoothstep(sun.bloom.y, 1.0, max3(c));
}

// The bloom's source at 1 x (the world area's size): the mean of each
// N x N block's bright light, with the sun's light beyond white where the
// picture still shows it white (smoke over the sun hides it).
@fragment
fn fs_bright(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let q = vec2<i32>(position.xy);
    let n = i32(sun.centre_k_scale.w);
    let origin = vec2<i32>(sun.area.xy);
    var sum = vec3<f32>(0.0);
    for (var j = 0; j < n; j++) {
        for (var i = 0; i < n; i++) {
            let local = q * n + vec2<i32>(i, j);
            let c = fx_decode(textureLoad(shadow, origin + local, 0).r);
            var b = bright(c);
            if sun.bloom.w > 0.0 && in_area(local) && sky(local) {
                let light = sun_light(vec2<f32>(origin + local) + 0.5);
                b += light * (sun.bloom.w * smoothstep(0.6, 0.95, max3(c)));
            }
            sum += b;
        }
    }
    return vec4<f32>(sum / f32(n * n), 1.0);
}

fn tap(uv: vec2<f32>) -> vec3<f32> {
    return textureSampleLevel(source, bilinear, uv, 0.0).rgb;
}

// One level down: 13 bilinear taps of the level above (a 6 x 6 kernel).
@fragment
fn fs_down(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let size = vec2<f32>(textureDimensions(source));
    let target_size = max(floor(size * 0.5), vec2<f32>(1.0));
    let uv = position.xy / target_size;
    let t = 1.0 / size;
    let a = tap(uv + t * vec2<f32>(-2.0, -2.0));
    let b = tap(uv + t * vec2<f32>(0.0, -2.0));
    let c = tap(uv + t * vec2<f32>(2.0, -2.0));
    let d = tap(uv + t * vec2<f32>(-1.0, -1.0));
    let e = tap(uv + t * vec2<f32>(1.0, -1.0));
    let f = tap(uv + t * vec2<f32>(-2.0, 0.0));
    let g = tap(uv);
    let h = tap(uv + t * vec2<f32>(2.0, 0.0));
    let i = tap(uv + t * vec2<f32>(-1.0, 1.0));
    let j = tap(uv + t * vec2<f32>(1.0, 1.0));
    let k = tap(uv + t * vec2<f32>(-2.0, 2.0));
    let l = tap(uv + t * vec2<f32>(0.0, 2.0));
    let m = tap(uv + t * vec2<f32>(2.0, 2.0));
    let sum = (d + e + i + j) * 0.125
        + (a + c + k + m) * 0.03125
        + (b + f + h + l) * 0.0625
        + g * 0.125;
    return vec4<f32>(sum, 1.0);
}

// One level up: a 3 x 3 tent of the coarser level plus this level's own.
@fragment
fn fs_up(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let size = vec2<f32>(textureDimensions(level));
    let uv = position.xy / size;
    let t = 1.0 / vec2<f32>(textureDimensions(source));
    var sum = tap(uv) * 4.0;
    sum += (tap(uv + vec2<f32>(t.x, 0.0)) + tap(uv - vec2<f32>(t.x, 0.0))
        + tap(uv + vec2<f32>(0.0, t.y)) + tap(uv - vec2<f32>(0.0, t.y))) * 2.0;
    sum += tap(uv + t) + tap(uv - t) + tap(uv + vec2<f32>(t.x, -t.y))
        + tap(uv + vec2<f32>(-t.x, t.y));
    let own = textureLoad(level, vec2<i32>(position.xy), 0).rgb;
    return vec4<f32>(sum / 16.0 + own, 1.0);
}

// ---- late: the lens flare ----

// The game's flare elements, each drawn from its colour profile.
fn ghosts(p: vec2<f32>) -> vec3<f32> {
    let k = sun.centre_k_scale.z;
    var sum = vec3<f32>(0.0);
    for (var e = 0u; e < ELEMENTS; e++) {
        let element = sun.elements[e];
        if element.w <= 0.0 || element.z <= 0.0 {
            continue;
        }
        var d = p - element.xy;
        d.x = d.x / k;
        let r = length(d) / element.z;
        if r >= 1.0 {
            continue;
        }
        let f = clamp(r * f32(PROFILE) - 0.5, 0.0, f32(PROFILE - 1u));
        let i = u32(f);
        let j = min(i + 1u, PROFILE - 1u);
        let c = mix(sun.profiles[e * PROFILE + i].rgb, sun.profiles[e * PROFILE + j].rgb, f - f32(i));
        sum += c * (element.w * smoothstep(1.0, 0.9, r));
    }
    return sum;
}

// The halo (a thin ring around the sun) and the star (four thin rays,
// turned as the game turns its sun's glow).
fn halo_star(p: vec2<f32>) -> vec3<f32> {
    let k = sun.centre_k_scale.z;
    var d = p - sun.screen.xy;
    d.x = d.x / k;
    let r = length(d);
    let ring = (r - sun.halo.x) / sun.halo.y;
    var light = sun.glow_colour.rgb * (sun.flare.y * exp(-ring * ring));
    var rays = 0.0;
    let width = 0.35 * sun.centre_k_scale.w;
    for (var i = 0; i < 2; i++) {
        let a = sun.halo.w + f32(i) * 1.5707964;
        let along = abs(dot(d, vec2<f32>(cos(a), sin(a))));
        let across = abs(dot(d, vec2<f32>(-sin(a), cos(a))));
        rays += exp(-across / width) * exp(-along / sun.halo.z);
    }
    light += sun.disc_colour.rgb * (sun.flare.z * rays);
    return light;
}

// Bloom and lens flare over the picture (deferred primitives included).
@fragment
fn fs_composite(@builtin(position) position: vec4<f32>) -> @location(0) u32 {
    let p = vec2<u32>(position.xy);
    let uv = (position.xy - sun.area.xy) / sun.area.zw;
    let bloom = textureSampleLevel(bloom_texture, bilinear, uv, 0.0).rgb * sun.bloom.x;
    let seen = textureLoad(visibility, vec2<i32>(0), 0).r;
    var flare = vec3<f32>(0.0);
    var veil = vec3<f32>(0.0);
    if sun.flare.w > 0.0 && seen > 0.0 {
        flare = ghosts(position.xy) * seen + halo_star(position.xy) * (seen * seen);
        let theta = sun_angle(position.xy);
        veil = sun.glow_colour.rgb
            * ((sun.glare.x * exp(-theta / max(sun.glare.y, 1e-4)) + sun.glare.z) * seen);
    }
    if max3(bloom + flare + veil) < NOTHING {
        discard;
    }
    let old = textureLoad(shadow, vec2<i32>(p), 0).r;
    // The flare adds as the game's does (additive, clamped at white); the
    // bloom and the veiling glare saturate softly.
    let c = min(fx_decode(old) + flare, vec3<f32>(1.0));
    return fx_encode(saturate_add(c, bloom + veil), p) | (old & 0x8000u);
}
