#version 450
// Lola Retro - pass 3: glass glow (halation), horizontal blur and downsample.
// Works in linear light so bright areas spread the right amount of light.

#include "common.inc"

#pragma stage vertex
layout(location = 0) in vec4 Position;
layout(location = 1) in vec2 TexCoord;
layout(location = 0) out vec2 vTexCoord;
layout(location = 1) flat out float vScale;
layout(location = 2) flat out float vUnit;    // frame pixels per console pixel (Amiga hires: 2)

void main()
{
    gl_Position = global.MVP * Position;
    vTexCoord = TexCoord;
    Timing T = getTiming(params.OriginalSize.xy, params.FrameCount);
    vScale = T.scale;
    vUnit = consoleUnit(T).x;
}

#pragma stage fragment
layout(location = 0) in vec2 vTexCoord;
layout(location = 1) flat in float vScale;
layout(location = 2) flat in float vUnit;
layout(location = 0) out vec4 FragColor;
layout(set = 0, binding = 2) uniform sampler2D Source;

void main()
{
    // tap spacing sets the radius: half a console pixel per unit of Glass glow size, measured
    // on the frame, so it is the same at any internal resolution and decoder width
    float dx = 2.0 * global.LR_GLOW_SIZE * vUnit * vScale * params.OriginalSize.z;
    vec2 tc = vTexCoord;
    if (vScale > 1.5)
    {
        // upscaled frame: the decoder put native line l in row l, in the top 1/k of its output
        float lines = params.SourceSize.y / vScale;
        tc.y = (floor(tc.y * lines) + 0.5) * params.SourceSize.w;
    }
    vec3 acc = vec3(0.0);
    float ws = 0.0;
    for (int k = -6; k <= 6; k++)
    {
        float w = exp(-0.5 * float(k * k) / 9.0);
        vec2 p = tc + vec2(float(k) * dx, 0.0);
        vec3 c = 0.5 * (textureLod(Source, p - vec2(0.25 * dx, 0.0), 0.0).rgb + textureLod(Source, p + vec2(0.25 * dx, 0.0), 0.0).rgb);
        acc += w * pow(c, vec3(global.LR_GAMMA));
        ws += w;
    }
    FragColor = vec4(acc / ws, 1.0);
}
