// Draws one texture over the current viewport: the game picture (opaque)
// or the on-screen messages (alpha-blended). The viewport places and
// scales it; the sampler picks nearest or linear filtering.

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// A triangle strip of four vertices covering the viewport.
@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOutput {
    let uv = vec2<f32>(f32(index & 1u), f32(index >> 1u));
    var out: VertexOutput;
    out.position = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    out.uv = uv;
    return out;
}

@group(0) @binding(0) var picture: texture_2d<f32>;
@group(0) @binding(1) var picture_sampler: sampler;

@fragment
fn fs_picture(in: VertexOutput) -> @location(0) vec4<f32> {
    return vec4<f32>(textureSample(picture, picture_sampler, in.uv).rgb, 1.0);
}

@fragment
fn fs_overlay(in: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(picture, picture_sampler, in.uv);
}
