// The movie layer: a movie pack's image over the movie frame's rectangle
// of the picture (the viewport). The image keeps its shape inside the
// rectangle (black beside it), and is filtered over each target pixel's
// footprint (up to 4x4 bilinear taps, so a 4K image shown smaller does not
// shimmer). Where the mask says the game drew over its frame (subtitles,
// fades), nothing is drawn and the picture below shows.

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOutput {
    let uv = vec2<f32>(f32(index & 1u), f32(index >> 1u));
    var out: VertexOutput;
    out.position = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    out.uv = uv;
    return out;
}

struct Params {
    // Image uv = (uv - offset) * scale; outside 0..1 is beside the image.
    offset: vec2<f32>,
    scale: vec2<f32>,
    // Taps per axis, and the step between them in image uv.
    taps: vec2<f32>,
    step: vec2<f32>,
    // The mask's size in texels (0: no mask).
    mask_size: vec2<f32>,
    padding: vec2<f32>,
};

@group(0) @binding(0) var image: texture_2d<f32>;
@group(0) @binding(1) var image_sampler: sampler;
@group(0) @binding(2) var mask: texture_2d<f32>;
@group(0) @binding(3) var<uniform> params: Params;

@fragment
fn fs_movie(in: VertexOutput) -> @location(0) vec4<f32> {
    if (params.mask_size.x > 0.0) {
        let texel = clamp(floor(in.uv * params.mask_size), vec2<f32>(0.0), params.mask_size - 1.0);
        if (textureLoad(mask, vec2<i32>(texel), 0).r > 0.5) {
            discard;
        }
    }
    let uv = (in.uv - params.offset) * params.scale;
    if (any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0))) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    let n = vec2<i32>(params.taps);
    var sum = vec3<f32>(0.0);
    for (var y = 0; y < n.y; y++) {
        for (var x = 0; x < n.x; x++) {
            let tap = (vec2<f32>(f32(x), f32(y)) - (params.taps - 1.0) * 0.5) * params.step;
            sum += textureSampleLevel(image, image_sampler, uv + tap, 0.0).rgb;
        }
    }
    return vec4<f32>(sum / f32(n.x * n.y), 1.0);
}
