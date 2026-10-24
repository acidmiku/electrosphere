#version 450
// Lola Retro - pass 4: glass glow (halation), vertical blur.

#include "common.inc"

#pragma stage vertex
layout(location = 0) in vec4 Position;
layout(location = 1) in vec2 TexCoord;
layout(location = 0) out vec2 vTexCoord;
layout(location = 1) flat out float vUnit;    // frame rows per console line (Amiga doubled or interlaced: 2; upscaled: k)

void main()
{
    gl_Position = global.MVP * Position;
    vTexCoord = TexCoord;
    Timing T = getTiming(params.OriginalSize.xy, params.FrameCount);
    vUnit = consoleUnit(T).y * T.scale;
}

#pragma stage fragment
layout(location = 0) in vec2 vTexCoord;
layout(location = 1) flat in float vUnit;
layout(location = 0) out vec4 FragColor;
layout(set = 0, binding = 2) uniform sampler2D Source;

void main()
{
    vec3 acc = vec3(0.0);
    float ws = 0.0;
    for (int k = -6; k <= 6; k++)
    {
        float w = exp(-0.5 * float(k * k) / 9.0);
        acc += w * textureLod(Source, vTexCoord + vec2(0.0, float(k) * params.SourceSize.w * global.LR_GLOW_SIZE * vUnit), 0.0).rgb;
        ws += w;
    }
    FragColor = vec4(acc / ws, 1.0);
}
