#version 450
// Lola Retro - pass 7: linear light -> display, with a dither against banding.

#include "common.inc"

#pragma stage vertex
layout(location = 0) in vec4 Position;
layout(location = 1) in vec2 TexCoord;
layout(location = 0) out vec2 vTexCoord;

void main()
{
    gl_Position = global.MVP * Position;
    vTexCoord = TexCoord;
}

#pragma stage fragment
layout(location = 0) in vec2 vTexCoord;
layout(location = 0) out vec4 FragColor;
layout(set = 0, binding = 2) uniform sampler2D Source;

// Dither to the 8-bit output in linear light: each channel takes the code just below or just
// above its value, the upper one with the chance that makes their average in linear light the
// value itself (noise added to the gamma-encoded value would brighten dark gradients a little).
// The noise is interleaved gradient noise: high frequency, no clumps, the same for the three
// channels so it adds no colour noise.
void main()
{
    vec3 lin = clamp(textureLod(Source, vTexCoord, 0.0).rgb, 0.0, 1.0);
    vec3 code = min(floor(pow(lin, vec3(1.0 / 2.2)) * 255.0), 254.0);
    vec3 lo = pow(code / 255.0, vec3(2.2)), hi = pow((code + 1.0) / 255.0, vec3(2.2));
    vec3 up = clamp((lin - lo) / (hi - lo), 0.0, 1.0);
    vec2 fc = floor(vTexCoord * params.OutputSize.xy);
    float n = fract(52.9829189 * fract(dot(fc, vec2(0.06711056, 0.00583715))));
    FragColor = vec4((code + vec3(greaterThan(up, vec3(n)))) / 255.0, 1.0);
}
