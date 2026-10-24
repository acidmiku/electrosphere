// The GPU renderer's display area as an RGBA picture: one target pixel per
// N x VRAM pixel from `origin`, each 5-bit channel widened as the software
// path does (`ac3_graphics::rgba8`).

struct Display {
    origin: vec2<u32>,
    // The target is sRGB: write linear values, so the stored bytes are the
    // picture's.
    srgb: u32,
    pad: u32,
};

@group(0) @binding(0) var<uniform> display: Display;
@group(0) @binding(1) var vram: texture_2d<u32>;

@vertex
fn vs_display(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    // One triangle over the whole target.
    let p = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
}

fn to_linear(c: vec3<f32>) -> vec3<f32> {
    let low = c / 12.92;
    let high = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
    return select(high, low, c <= vec3<f32>(0.04045));
}

@fragment
fn fs_display(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let p = vec2<u32>(position.xy) + display.origin;
    let word = textureLoad(vram, p, 0).r;
    let c = vec3<u32>(word & 31u, (word >> 5u) & 31u, (word >> 10u) & 31u);
    var rgb = vec3<f32>((c << vec3<u32>(3u)) | (c >> vec3<u32>(2u))) / 255.0;
    if display.srgb != 0u {
        rgb = to_linear(rgb);
    }
    return vec4<f32>(rgb, 1.0);
}
