#version 450
// Lola Retro - pass 0 of the RetroArch presets: the console's lines of an upscaled frame.
// A core running at a higher internal resolution sends k rows for every console line (PS1 at
// 4x: 4 rows). The encoder reads each pixel of a line many times (its filters have up to 40
// taps per sample), so the k rows of a line are averaged here once, into the line's first row,
// instead of again at every tap. A frame wider than the encoder's samples per line
// (ENC_SAMPLES, common.inc) is also averaged across, over the pixels of one sample.
// Native frames (k = 1) up to that width pass through unchanged.
// Float output, so the average is not rounded.

#include "common.inc"

#pragma stage vertex
layout(location = 0) in vec4 Position;
layout(location = 1) in vec2 TexCoord;
layout(location = 0) out vec2 vTexCoord;
layout(location = 1) flat out float vScale;   // rows per console line (internal resolution)
layout(location = 2) flat out float vBox;     // frame pixels per encoder sample, averaged across

void main()
{
    gl_Position = global.MVP * Position;
    vTexCoord = TexCoord;
    Timing T = getTiming(params.OriginalSize.xy, params.FrameCount);
    vScale = T.scale;
    // TV raster: the encoder takes ENC_SAMPLES samples per line, so a wider frame (PS1 at 8x:
    // 2560) is averaged over the pixels of one sample, or the samples would skip pixels
    vBox = T.scan < 0.5 ? max(1.0, floor(params.OriginalSize.x / ENC_SAMPLES + 0.5)) : 1.0;
}

#pragma stage fragment
layout(location = 0) in vec2 vTexCoord;
layout(location = 1) flat in float vScale;
layout(location = 2) flat in float vBox;
layout(location = 0) out vec4 FragColor;
layout(set = 0, binding = 2) uniform sampler2D Source;

void main()
{
    float y = floor(vTexCoord.y * params.SourceSize.y);
    if (mod(y, vScale) > 0.5)
    {
        // only the first row of each console line is read by the encoder
        FragColor = vec4(0.0);
        return;
    }
    // the sample that reads pixel j covers pixels j - box/2 .. j + box/2 - 1
    float x0 = floor(vTexCoord.x * params.SourceSize.x) - floor(vBox * 0.5);
    vec3 c = vec3(0.0);
    for (float b = 0.0; b < vBox; b += 1.0)
    {
        float u = (clamp(x0 + b, 0.0, params.SourceSize.x - 1.0) + 0.5) * params.SourceSize.z;
        for (float r = 0.0; r < vScale; r += 1.0)
            c += textureLod(Source, vec2(u, (y + r + 0.5) * params.SourceSize.w), 0.0).rgb;
    }
    FragColor = vec4(c / (vScale * vBox), 1.0);
}
