// Shared WGSL of the effects (src/gpu_renderer/fx, docs/EFFECTS.md):
// the scene uniform and the 15-bit write-back. Effects prepend this to
// their shaders (`fx::card::FX_COMMON_WGSL`).

// The frame's scene constants (`Scene::uniform`, 36 f32).
struct FxScene {
    // The view matrix rows (world to view, 1.3.12 as floats), w unused.
    view0: vec4<f32>,
    view1: vec4<f32>,
    view2: vec4<f32>,
    // The eye (world units: the camera's 20.12 words / 4096) and H.
    eye_h: vec4<f32>,
    // The projection centre (1x VRAM pixels), widescreen's k, the scale N.
    centre_k_scale: vec4<f32>,
    // The sun's direction (unit, world) and near; its colour and far.
    sun_near: vec4<f32>,
    sun_colour_far: vec4<f32>,
    // The mission's fog colour (0..1) and its sky type.
    fog_sky: vec4<f32>,
    // The world area's origin at N x (the depth snapshot's pixel 0, 0).
    origin: vec4<f32>,
};

// A PlayStation pixel's 5-bit colour as 0..1 (c / 31).
fn fx_decode(word: u32) -> vec3<f32> {
    let c = vec3<u32>(word & 31u, (word >> 5u) & 31u, (word >> 10u) & 31u);
    return vec3<f32>(c) / 31.0;
}

// The 8 x 8 ordered-dither threshold of N x pixel `p`, in (0, 1).
fn fx_bayer(p: vec2<u32>) -> f32 {
    var m = array<u32, 64>(
        0u, 32u, 8u, 40u, 2u, 34u, 10u, 42u,
        48u, 16u, 56u, 24u, 50u, 18u, 58u, 26u,
        12u, 44u, 4u, 36u, 14u, 46u, 6u, 38u,
        60u, 28u, 52u, 20u, 62u, 30u, 54u, 22u,
        3u, 35u, 11u, 43u, 1u, 33u, 9u, 41u,
        51u, 19u, 59u, 27u, 49u, 17u, 57u, 25u,
        15u, 47u, 7u, 39u, 13u, 45u, 5u, 37u,
        63u, 31u, 55u, 23u, 61u, 29u, 53u, 21u);
    return (f32(m[(p.y & 7u) * 8u + (p.x & 7u)]) + 0.5) / 64.0;
}

// `rgb` (0..1) as a 15-bit PlayStation colour, ordered-dithered at N x
// pixel `p` (no mask bit). A colour of whole 5-bit steps (what fx_decode
// gives) comes back unchanged.
fn fx_encode(rgb: vec3<f32>, p: vec2<u32>) -> u32 {
    let v = clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)) * 31.0;
    let d = fx_bayer(p) - 0.5;
    // Within a hundredth of a step of a whole step: that step.
    let near = abs(v - round(v)) < vec3<f32>(0.01);
    let q = select(floor(v + 0.5 + d), round(v), near);
    let c = vec3<u32>(clamp(q, vec3<f32>(0.0), vec3<f32>(31.0)));
    return c.x | (c.y << 5u) | (c.z << 10u);
}
