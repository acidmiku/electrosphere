// The effects' debug views (src/gpu_renderer/fx, `--fx-debug`), drawn
// over the world's area into the N x VRAM at the post: after fx_common.wgsl.
//
// - depth (mode 1): the view z through a colour ramp, logarithmic from z 16
//   (white) to 65536 (dark red); beyond, far terrain, dark grey; the sky
//   (z cleared at world begin) dark blue; no depth magenta.
// - coverage (mode 2): the picture's brightness tinted green where the
//   pixel has a depth, red where it has none, blue for the sky.

const SKY_Z: f32 = 1e30;

struct FxGlobals {
    origin: vec2<u32>,
    size: vec2<u32>,
    mode: u32,
    scale: u32,
    pad0: u32,
    pad1: u32,
};

@group(0) @binding(0) var<uniform> fx: FxGlobals;
@group(0) @binding(1) var snapshot: texture_2d<f32>;
@group(0) @binding(2) var shadow: texture_2d<u32>;

@vertex
fn vs_debug(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let p = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
}

// A ramp through white, yellow, orange, red and dark red for t in 0..1.
fn ramp(t: f32) -> vec3<f32> {
    let stops = array<vec3<f32>, 5>(
        vec3<f32>(1.0, 1.0, 1.0),
        vec3<f32>(1.0, 0.9, 0.2),
        vec3<f32>(1.0, 0.5, 0.0),
        vec3<f32>(0.8, 0.1, 0.0),
        vec3<f32>(0.3, 0.0, 0.0));
    let x = clamp(t, 0.0, 1.0) * 4.0;
    let i = min(u32(x), 3u);
    return mix(stops[i], stops[i + 1u], x - f32(i));
}

@fragment
fn fs_debug(@builtin(position) position: vec4<f32>) -> @location(0) u32 {
    let p = vec2<u32>(position.xy);
    let local = vec2<i32>(p) - vec2<i32>(fx.origin);
    let back = textureLoad(shadow, vec2<i32>(p), 0).r;
    if any(local < vec2<i32>(0)) || any(local >= vec2<i32>(fx.size)) {
        discard;
    }
    let snap = textureLoad(snapshot, local, 0).rg;
    let z = snap.r;
    var rgb: vec3<f32>;
    if fx.mode == 1u {
        if z >= SKY_Z * 0.5 {
            rgb = vec3<f32>(0.05, 0.1, 0.35);
        } else if z <= 0.0 {
            rgb = vec3<f32>(0.9, 0.0, 0.9);
        } else if z > 65536.0 {
            rgb = vec3<f32>(0.2, 0.2, 0.2);
        } else {
            rgb = ramp((log2(max(z, 16.0)) - 4.0) / 12.0);
        }
    } else {
        let c = fx_decode(back);
        let grey = 0.25 + 0.6 * dot(c, vec3<f32>(0.3, 0.59, 0.11));
        if z >= SKY_Z * 0.5 {
            rgb = vec3<f32>(0.2, 0.3, 1.0) * grey;
        } else if z <= 0.0 {
            rgb = vec3<f32>(1.0, 0.15, 0.1) * grey;
        } else {
            rgb = vec3<f32>(0.2, 1.0, 0.25) * grey;
        }
    }
    return fx_encode(rgb, p) | (back & 0x8000u);
}
