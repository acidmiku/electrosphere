// The haze of the atmosphere effect (docs/EFFECTS_ATMOSPHERE.md): the
// WGSL twin of `Haze::shade` in src/effects/atmosphere.rs, for the post
// pass at world end. A snippet: no bindings, no entry points; the including
// shader declares the uniform (`HazeUniform`, filled by `Haze::uniform`)
// and calls `haze_shade` per world pixel. `tests/effects_atmosphere.rs`
// runs it on the GPU against the Rust model.
//
// Units: world units (about 5 per metre), y down. `offset` is camera to
// point in world space: rebuild it from the pixel's view depth z and its
// screen position (x, y) relative to the projection centre as
// transpose(R) * (x z / H, y z / H, z), with R the view rotation (world
// +0x4C) and H the projection distance.

const HAZE_PI: f32 = 3.141592653589793;
const HAZE_SKY_BINS: u32 = 32u;

struct HazeUniform {
    // density per unit at sea level, scale height, sea level (world y),
    // the Earth's radius
    shape: vec4<f32>,
    // fog-colour share of the haze colour, Henyey-Greenstein g, sun
    // strength, the camera's world y
    scatter: vec4<f32>,
    // the draw-distance fade: start and end view depth (end <= start: none)
    edge: vec4<f32>,
    // the fog colour (rgb, 0..1)
    fog: vec4<f32>,
    // toward the sun (world, unit)
    sun: vec4<f32>,
    // the sun's colour (rgb, 0..1)
    sun_color: vec4<f32>,
    // the sky model: 32 colours by elevation (`sky_bin`)
    sky: array<vec4<f32>, 32>,
}

// The elevation (radians, up positive) of a world direction.
fn haze_elevation(direction: vec3<f32>) -> f32 {
    return atan2(-direction.y, length(direction.xz));
}

// The sky model at `elevation`, interpolated between bin centres
// (`SkyTable::sample`).
fn haze_sky(sky: ptr<function, array<vec4<f32>, 32>>, elevation: f32) -> vec3<f32> {
    let s = sin(elevation);
    let t = sign(s) * sqrt(abs(s));
    let position = (t + 1.0) * 0.5 * f32(HAZE_SKY_BINS) - 0.5;
    let low = clamp(floor(position), 0.0, f32(HAZE_SKY_BINS - 1u));
    let fraction = clamp(position - low, 0.0, 1.0);
    let i = u32(low);
    let j = min(i + 1u, HAZE_SKY_BINS - 1u);
    return mix((*sky)[i].rgb, (*sky)[j].rgb, fraction);
}

// The Henyey-Greenstein phase function per steradian.
fn haze_henyey_greenstein(cos_theta: f32, g: f32) -> f32 {
    let d = max(1.0 + g * g - 2.0 * g * cos_theta, 1e-6);
    return (1.0 - g * g) / (4.0 * HAZE_PI * d * sqrt(d));
}

// Optical depth from altitude `low_end` to `high_end` over `distance`:
// exponential density, altitude linear along the ray (`Haze::optical_depth`).
fn haze_optical_depth(u: HazeUniform, low_end: f32, high_end: f32, distance: f32) -> f32 {
    let h = u.shape.y;
    let k = (high_end - low_end) / h;
    var profile = 1.0 - k * 0.5;
    if (abs(k) >= 1e-4) {
        profile = (1.0 - exp(-k)) / k;
    }
    return u.shape.x * distance * exp(-low_end / h) * profile;
}

// The light the haze lets through from the point at `offset`; the point's
// altitude is above the curved sea (its flat altitude plus d^2 / 2R).
fn haze_transmittance(u: HazeUniform, offset: vec3<f32>) -> f32 {
    let camera = u.shape.z - u.scatter.w;
    let horizontal = length(offset.xz);
    let point_altitude = camera - offset.y + horizontal * horizontal / (2.0 * u.shape.w);
    let tau = haze_optical_depth(u, max(camera, 0.0), max(point_altitude, 0.0), length(offset));
    return exp(-tau);
}

// `color` (0..1) of the point at `offset` (view depth `depth`) seen
// through the haze, faded into the sky behind across the draw-distance edge.
fn haze_shade(u: HazeUniform, color: vec3<f32>, offset: vec3<f32>, depth: f32) -> vec3<f32> {
    var sky = u.sky;
    let direction = normalize(offset);
    let elevation = haze_elevation(direction);
    let through = haze_transmittance(u, offset);
    let horizon = haze_sky(&sky, max(elevation, 0.0));
    let base = mix(horizon, u.fog.rgb, u.scatter.x);
    let phase = 4.0 * HAZE_PI * haze_henyey_greenstein(dot(direction, u.sun.xyz), u.scatter.y);
    let haze_color = base + u.sun_color.rgb * (u.scatter.z * phase);
    var result = mix(haze_color, color, through);
    if (u.edge.y > u.edge.x) {
        let t = clamp((depth - u.edge.x) / (u.edge.y - u.edge.x), 0.0, 1.0);
        let fade = t * t * (3.0 - 2.0 * t);
        result = mix(result, haze_sky(&sky, elevation), fade);
    }
    return clamp(result, vec3<f32>(0.0), vec3<f32>(1.0));
}
