// The GPU renderer's N x picture brought back to its 1x frame for a shader
// preset (`shader.rs`): each 1x pixel is the mean of its N x N block.

struct Params {
    scale: u32,
}

@group(0) @binding(0) var picture: texture_2d<f32>;
@group(0) @binding(1) var<uniform> params: Params;

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    // One triangle over the whole target.
    let x = f32((index << 1u) & 2u);
    let y = f32(index & 2u);
    return vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let n = max(params.scale, 1u);
    let base = vec2<u32>(position.xy) * n;
    var sum = vec4<f32>(0.0);
    for (var y = 0u; y < n; y++) {
        for (var x = 0u; x < n; x++) {
            sum += textureLoad(picture, base + vec2<u32>(x, y), 0);
        }
    }
    return vec4<f32>(sum.rgb / f32(n * n), 1.0);
}
