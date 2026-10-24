// The GPU renderer's drawing shader (see src/gpu_renderer/mod.rs).
//
// Target: the N x VRAM, R16Uint PlayStation pixels (15-bit colour and the
// mask bit). Every write is computed here with the reference software GPU's
// integer arithmetic on 5-bit channels; nothing is blended in hardware.
// `shadow` is an up-to-date copy of the target wherever this draw reads it
// (the recorder inserts the copies); `mirror` is the 1 x software VRAM, for
// palettes and 4/8-bit texels.
//
// Vertex positions are 1 x VRAM coordinates whose sample points are the
// pixels' integer coordinates; position + 0.5 is the pixel centre, scaled
// to N x here.
//
// Polygons above 1 x are the exception: their coverage puts 1 x sample
// point i at the top-left corner of pixel i's N x block, so that an edge on
// whole pixels covers whole blocks, the same pixels as the reference's
// top-left rule at 1 x. With the pixel centre instead, every polygon was
// drawn half a pixel right of and below the rectangles, lines and fills, and
// of the 1 x picture: a full-screen polygon left the first N / 2 rows and
// columns of the draw area undrawn (stale pixels at the picture's top and
// left edges) and reached N / 2 rows and columns past its last pixel. Their
// texture coordinates and colours keep the pixel-centre mapping: the
// fragment reads them half a 1 x pixel back (see `fs_draw`), so a block's
// centre shows what the 1 x pixel shows.
//
// Texture replacement (src/gpu_renderer/replace.rs, atlas.rs): a textured
// primitive whose mode word names a slot (bits 16..31, slot + 1) samples
// that slot's replacement in `atlas` instead of VRAM; its window word then
// holds the texel position of its upload in the page instead (replaced
// primitives never have a texture window).
//
// Perspective-correct textures (src/gpu_renderer/record.rs, PERSPECTIVE):
// such a polygon's vertices carry their depth as w. The vertex stage
// sends uv/w, colour/w and 1/w as linear (screen-space) varyings, and the
// fragment divides: perspective interpolation, while the clip position,
// and so the coverage, stays exactly that of the affine path. The texture
// coordinates are then clamped to the triangle's own (flat), so they never
// reach a texel the triangle does not map: not across a 4-bit page, a
// texture window or a replacement's edge. Every other primitive takes the
// affine path unchanged.

struct Globals {
    scale: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(1) var mirror: texture_2d<u32>;
@group(0) @binding(2) var shadow: texture_2d<u32>;
@group(0) @binding(3) var atlas: texture_2d_array<u32>;
@group(0) @binding(4) var slots: texture_2d<u32>;

// The mode word (src/gpu_renderer/record.rs, `mode`).
const KIND_POLY: u32 = 0u;
const KIND_RECT: u32 = 1u;
const KIND_LINE: u32 = 2u;
const KIND_FILL: u32 = 3u;
const KIND_MIRROR: u32 = 4u;
const KIND_COPY: u32 = 5u;
const TEXTURED: u32 = 8u;
const RAW: u32 = 16u;
const GOURAUD: u32 = 32u;
const SEMI: u32 = 64u;
const DITHER: u32 = 512u;
const CHECK: u32 = 1024u;
const SET: u32 = 2048u;
const EXACT: u32 = 4096u;
const SCALAR: u32 = 8192u;
const SHARE: u32 = 16384u;
const PERSPECTIVE: u32 = 32768u;
const SLOT_SHIFT: u32 = 16u;
// Slot flags (atlas.rs).
const SLOT_PRESENT: u32 = 1u;
const SLOT_INDEXED: u32 = 2u;
// The window word without a window: every coordinate bit kept, no offset.
const NO_WINDOW: u32 = 0xffffu;

struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) mode: u32,
    @location(4) q0: vec4<u32>,
    @location(5) q1: vec4<u32>,
    @location(6) q2: vec4<u32>,
    @location(7) q3: vec4<u32>,
    // The depth of a PERSPECTIVE polygon's vertex; 1 otherwise.
    @location(8) w: f32,
};

struct Varyings {
    @builtin(position) position: vec4<f32>,
    // Affine, as on the PlayStation. A PERSPECTIVE polygon's are divided by
    // its vertices' w (the colour when Gouraud-shaded), with `q` = 1 / w.
    @location(0) @interpolate(linear) uv: vec2<f32>,
    @location(1) @interpolate(linear) color: vec3<f32>,
    @location(2) @interpolate(flat) mode: u32,
    @location(3) @interpolate(flat) q0: vec4<u32>,
    @location(4) @interpolate(flat) q1: vec4<u32>,
    @location(5) @interpolate(flat) q2: vec4<u32>,
    @location(6) @interpolate(flat) q3: vec4<u32>,
    @location(7) @interpolate(linear) q: f32,
};

@vertex
fn vs_draw(v: VertexInput) -> Varyings {
    var out: Varyings;
    // 1024 x 512 VRAM pixels fill the target whatever its scale. A polygon
    // above 1 x has its sample points at the blocks' top-left corners (see
    // the top of the file); everything else at the pixel centres.
    var at = v.position + vec2<f32>(0.5);
    if (v.mode & 7u) == KIND_POLY && (v.mode & EXACT) == 0u {
        at = v.position;
    }
    out.position = vec4<f32>(at.x / 512.0 - 1.0, 1.0 - at.y / 256.0, 0.0, 1.0);
    out.uv = v.uv;
    out.color = v.color.rgb * 255.0;
    out.q = 1.0;
    if (v.mode & PERSPECTIVE) != 0u {
        out.q = 1.0 / v.w;
        out.uv = v.uv * out.q;
        if (v.mode & GOURAUD) != 0u {
            out.color = out.color * out.q;
        }
    }
    out.mode = v.mode;
    out.q0 = v.q0;
    out.q1 = v.q1;
    out.q2 = v.q2;
    out.q3 = v.q3;
    return out;
}

// ---- VRAM reads ---------------------------------------------------------------------------

fn load_mirror(index: i32) -> i32 {
    return i32(textureLoad(mirror, vec2<i32>(index & 1023, (index >> 10u) & 511), 0).r);
}

fn load_shadow(p: vec2<i32>) -> i32 {
    return i32(textureLoad(shadow, p, 0).r);
}

// The texel at 8-bit texture coordinates `coords` (before the window), and
// `sub` the N x sub-texel of a 15-bit texture.
fn texel(coords: vec2<i32>, sub: vec2<i32>, tex: u32, window: u32) -> i32 {
    let keep = vec2<i32>(i32(window & 255u), i32((window >> 8u) & 255u));
    let offset = vec2<i32>(i32((window >> 16u) & 255u), i32(window >> 24u));
    let t = (coords & keep) | offset;
    let base_x = i32(tex & 15u) * 64;
    let base_y = i32((tex >> 4u) & 1u) * 256;
    let depth = (tex >> 5u) & 3u;
    if depth >= 2u {
        // Linear halfword addressing, as the reference.
        let index = (t.x + base_x) + ((t.y + base_y) << 10u);
        let n = i32(globals.scale);
        return load_shadow(vec2<i32>((index & 1023) * n + sub.x, ((index >> 10u) & 511) * n + sub.y));
    }
    var byte = (t.y << 11u) + (base_y << 11u) + (base_x << 1u);
    var shift = 0u;
    var entries = 255;
    if depth == 0u {
        byte += t.x >> 1u;
        shift = u32(t.x & 1) << 2u;
        entries = 15;
    } else {
        byte += t.x;
    }
    let word = load_mirror(byte >> 1u);
    let value = (word >> (u32(byte & 1) * 8u)) & 255;
    let clut_x = i32((tex >> 7u) & 63u) * 16;
    let clut_y = i32((tex >> 13u) & 511u);
    return load_mirror((clut_y << 10u) + clut_x + ((value >> shift) & entries));
}

// The texel of replacement `slot` at 8-bit texture coordinates `coords`,
// `fraction` inside that texel; `origin` is the upload's first texel in the
// page's coordinates (an i16 pair). A replacement scaled to `size` pixels
// for the original's texels is read at the same relative position (any
// scale), clamped to its edges. -1 when the slot holds no replacement: the
// original texels are drawn. Indexed replacements read the live palette.
fn replaced_texel(coords: vec2<i32>, fraction: vec2<f32>, tex: u32, origin: u32, slot: u32) -> i32 {
    let d = textureLoad(slots, vec2<i32>(i32(slot & 1023u), i32(slot >> 10u)), 0);
    let flags = d.w >> 16u;
    if (flags & SLOT_PRESENT) == 0u {
        return -1;
    }
    let size = vec2<i32>(i32(d.y & 0xffffu), i32(d.y >> 16u));
    let original = vec2<f32>(f32(d.z & 0xffffu), f32(d.z >> 16u));
    let local = vec2<f32>((coords & vec2<i32>(255)) - xy(origin)) + fraction;
    let p = clamp(vec2<i32>(floor(local * vec2<f32>(size) / original)), vec2<i32>(0), size - vec2<i32>(1));
    let corner = vec2<i32>(i32(d.x & 0xffffu), i32(d.x >> 16u));
    let value = i32(textureLoad(atlas, corner + p, i32(d.w & 0xffffu), 0).r);
    if (flags & SLOT_INDEXED) == 0u {
        return value;
    }
    let entries = select(255, 15, ((tex >> 5u) & 3u) == 0u);
    let clut_x = i32((tex >> 7u) & 63u) * 16;
    let clut_y = i32((tex >> 13u) & 511u);
    return load_mirror((clut_y << 10u) + clut_x + (value & entries));
}

// A textured primitive's texel: its replacement when it has one, else the
// texture in VRAM (`sub` the N x sub-texel of a 15-bit texture).
fn sample_texel(coords: vec2<i32>, fraction: vec2<f32>, sub: vec2<i32>, tex: u32, window: u32, mode: u32) -> i32 {
    let slot = mode >> SLOT_SHIFT;
    if slot == 0u {
        return texel(coords, sub, tex, window);
    }
    let t = replaced_texel(coords, fraction, tex, window, slot - 1u);
    if t >= 0 {
        return t;
    }
    return texel(coords, sub, tex, NO_WINDOW);
}

// ---- pixel arithmetic (ac3-graphics soft_pixel.rs) ------------------------------------------

// The reference's saturation of one channel at bit `s`: an overflow (any
// bit above the channel, ignoring the sign) gives 31.
fn saturate(r: i32, s: u32) -> i32 {
    let high = r & 0x7fffffff & ~((32 << s) - 1);
    return select(r & (31 << s), 31 << s, high != 0);
}

// Untextured single-pixel write (`color_scalar`) of a semi-transparent colour.
fn blend_color(color: i32, back: i32, abr: u32) -> i32 {
    var out = 0;
    for (var s = 0u; s < 15u; s += 5u) {
        let m = 31 << s;
        var r: i32;
        if abr == 0u {
            r = ((((back >> s) & 31) + ((color >> s) & 31)) >> 1u) << s;
        } else if abr == 1u {
            r = (back & m) + (color & m);
        } else if abr == 2u {
            r = max((back & m) - (color & m), 0);
        } else {
            r = (back & m) + ((color & m) >> 2u);
        }
        out |= saturate(r, s);
    }
    return out;
}

// Textured single-pixel write (`texel_scalar`), without the flags.
fn texel_scalar(t: i32, m: vec3<i32>, back: i32, semi: bool, abr: u32) -> i32 {
    let blend = semi && (t & 0x8000) != 0;
    var out = 0;
    for (var i = 0u; i < 3u; i++) {
        let s = i * 5u;
        let mask = 31 << s;
        let f = m[i];
        var r: i32;
        if !blend {
            r = ((t & mask) * f) >> 7u;
        } else if abr == 0u {
            r = ((((back >> s) & 31) + ((((t >> s) & 31) * f) >> 7u)) >> 1u) << s;
        } else if abr == 1u {
            r = (back & mask) + (((t & mask) * f) >> 7u);
        } else if abr == 2u {
            r = max((back & mask) - (((t & mask) * f) >> 7u), 0);
        } else {
            r = (back & mask) + ((((t & mask) >> 2u) * f) >> 7u);
        }
        out |= saturate(r, s);
    }
    return out;
}

// Textured write of one pixel of a pair (`texel_pair`), without the flags.
fn texel_pair(t: i32, m: vec3<i32>, back: i32, semi: bool, abr: u32) -> i32 {
    let blend = semi && (t & 0x8000) != 0;
    var out = 0;
    for (var i = 0u; i < 3u; i++) {
        let s = i * 5u;
        let t5 = (t >> s) & 31;
        let b5 = (back >> s) & 31;
        let f = m[i];
        let plain = (t5 * f) >> 7u;
        var r: i32;
        if !blend {
            r = plain;
        } else if abr == 0u {
            r = ((b5 << 7u) + t5 * f) >> 8u;
        } else if abr == 1u {
            r = b5 + plain;
        } else if abr == 2u {
            r = max(b5 - (plain & 63), 0);
        } else {
            r = b5 + ((((t5 & 28) >> 2u) * f) >> 7u);
        }
        out |= select(r & 31, 31, (r & 0x7fe0) != 0) << s;
    }
    return out;
}

// Eight-bit shaded write with blending, saturation and ordered dithering
// (`shade_dither`) at VRAM pixel `p`, without the flags.
fn shade_dither(shade: vec3<i32>, back: i32, semi: bool, abr: u32, p: vec2<i32>) -> i32 {
    var cells = array<i32, 16>(-4, 0, -3, 1, 2, -2, 3, -1, -3, 1, -4, 0, 3, -1, 2, -2);
    let offset = cells[(p.y & 3) * 4 + (p.x & 3)];
    var out = 0;
    for (var i = 0u; i < 3u; i++) {
        var v = shade[i];
        if semi {
            let b = ((back >> (i * 5u)) & 31) << 3u;
            if abr == 0u {
                v = (b + v) >> 1u;
            } else if abr == 1u {
                v = b + v;
            } else if abr == 2u {
                v = max(b - v, 0);
            } else {
                v = b + (v >> 2u);
            }
        }
        if (v & 0x7fffff00) != 0 {
            v = 255;
        }
        out |= (clamp(v + offset, 0, 255) >> 3u) << (i * 5u);
    }
    return out;
}

fn command_color(rgb: vec3<i32>) -> i32 {
    return (rgb.x >> 3u) | ((rgb.y >> 3u) << 5u) | ((rgb.z >> 3u) << 10u);
}

// ---- the reference's integer rasterizer (1 x) ------------------------------------------------

fn xy(word: u32) -> vec2<i32> {
    return vec2<i32>(i32(word << 16u) >> 16u, i32(word) >> 16u);
}

fn uv_of(word: u32) -> vec2<i32> {
    return vec2<i32>(i32(word & 255u), i32((word >> 8u) & 255u));
}

fn rgb_of(word: u32) -> vec3<i32> {
    return vec3<i32>(i32(word & 255u), i32((word >> 8u) & 255u), i32((word >> 16u) & 255u));
}

// trunc(a * 65536 / h) for |a| < 2^21 and 0 < h < 1024, without 64 bits.
fn mul16_div(a: i32, h: i32) -> i32 {
    let q = a / h;
    let r = a - q * h;
    return q * 65536 + (r * 65536) / h;
}

// floor(d * delta / 65536) for 0 <= d < 65536.
fn mul_hi16(d: i32, delta: i32) -> i32 {
    return d * (delta >> 16u) + i32((u32(d) * u32(delta & 0xffff)) >> 16u);
}

// ceil(a / b) for b > 0.
fn ceil_div(a: i32, b: i32) -> i32 {
    return select(a / b, (a + b - 1) / b, a > 0);
}

struct Span {
    covered: bool,
    // The second pixel of a pair, and the lone last pixel of an odd count.
    odd: bool,
    single: bool,
    // 16.16 values at the pixel.
    u: i32,
    v: i32,
    c: vec3<i32>,
    dc: vec3<i32>,
};

// `draw_triangle`'s row walk at VRAM pixel `p`.
fn exact_triangle(p: vec2<i32>, q0: vec4<u32>, q1: vec4<u32>, q2: vec4<u32>, q3: vec4<u32>) -> Span {
    var out: Span;
    out.covered = false;
    var pos = array<vec2<i32>, 3>(xy(q1.x), xy(q1.y), xy(q1.z));
    var tc = array<vec2<i32>, 3>(uv_of(q1.w), uv_of(q1.w >> 16u), uv_of(q2.x));
    var rgb = array<vec3<i32>, 3>(rgb_of(q2.y), rgb_of(q2.z), rgb_of(q2.w));
    let du = i32(q3.x);
    let dv = i32(q3.y);
    let dc = vec3<i32>(i32(q3.z), i32(q3.w), i32(q0.w));
    let long_left = (q0.z & 1u) != 0u;
    let area_left = i32((q0.z >> 1u) & 1023u);
    let area_right = i32((q0.z >> 11u) & 1023u);
    let y = p.y;
    if y < pos[0].y || y >= pos[2].y {
        return out;
    }
    // The long edge 0 -> 2; the short edge 0 -> 1, then 1 -> 2.
    var sa = 0;
    var sb = 1;
    if y >= pos[1].y {
        sa = 1;
        sb = 2;
    }
    var la = 0;
    var lb = 2;
    var ra = sa;
    var rb = sb;
    if !long_left {
        la = sa;
        lb = sb;
        ra = 0;
        rb = 2;
    }
    let lh = pos[lb].y - pos[la].y;
    let lk = y - pos[la].y;
    let rh = pos[rb].y - pos[ra].y;
    let rk = y - pos[ra].y;
    let left_x = (pos[la].x << 16u) + mul16_div((pos[lb].x - pos[la].x) * lk, lh);
    let right_x = (pos[ra].x << 16u) + mul16_div((pos[rb].x - pos[ra].x) * rk, rh);
    let x0 = (left_x + 0xffff) >> 16u;
    let x1 = min((right_x - 1) >> 16u, area_right);
    let first = max(x0, area_left);
    if p.x < first || p.x > x1 {
        return out;
    }
    out.covered = true;
    out.odd = ((p.x - first) & 1) == 1;
    out.single = p.x == x1 && ((x1 - first) & 1) == 0;
    let dist = (x0 << 16u) - left_x;
    let steps = p.x - x0;
    let lu = (tc[la].x << 16u) + lk * (((tc[lb].x - tc[la].x) << 16u) / lh);
    let lv = (tc[la].y << 16u) + lk * (((tc[lb].y - tc[la].y) << 16u) / lh);
    out.u = lu + mul_hi16(dist, du) + 0x8000 + steps * du;
    out.v = lv + mul_hi16(dist, dv) + 0x8000 + steps * dv;
    var c = vec3<i32>(0, 0, 0);
    for (var i = 0u; i < 3u; i++) {
        let lc = (rgb[la][i] << 16u) + mul16_div((rgb[lb][i] - rgb[la][i]) * lk, lh);
        c[i] = lc + mul_hi16(dist, dc[i]) + 0x8000 + steps * dc[i];
    }
    out.c = c;
    out.dc = dc;
    return out;
}

struct LinePixel {
    covered: bool,
    color: i32,
};

// `draw_line` at VRAM pixel `p`: which pixels it plots, and their colour.
fn exact_line(p: vec2<i32>, gouraud: bool, q0: vec4<u32>, q1: vec4<u32>) -> LinePixel {
    var out: LinePixel;
    out.covered = false;
    var a = xy(q1.x);
    var b = xy(q1.y);
    let lt = xy(q0.z);
    let rb = xy(q0.w);
    let inside = p.x >= lt.x && p.x < rb.x && p.y >= lt.y && p.y < rb.y;
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    var walked = 0;
    var steps = 0;
    if dx == 0 && dy == 0 {
        out.covered = all(p == a) && inside;
        out.color = command_color(rgb_of(q0.x));
        return out;
    } else if dx == 0 || dy == 0 {
        let vertical = dx == 0;
        let fixed_axis = select(a.y, a.x, vertical);
        let other = select(p.y, p.x, vertical);
        let value = select(p.x, p.y, vertical);
        let lo = select(min(a.x, b.x), min(a.y, b.y), vertical);
        let hi = select(max(a.x, b.x), max(a.y, b.y), vertical);
        let low = select(lt.x, lt.y, vertical);
        let high = select(rb.x, rb.y, vertical);
        out.covered = other == fixed_axis && value >= max(lo, low) && value <= min(hi, high);
        walked = value - lo;
        steps = hi - lo;
    } else {
        if dx < 0 {
            let t = a;
            a = b;
            b = t;
        }
        let adx = abs(b.x - a.x);
        let ady = abs(b.y - a.y);
        let sy = select(-1, 1, b.y > a.y);
        if adx >= ady {
            steps = adx;
            walked = p.x - a.x;
            let m = ceil_div(3 * ady - adx + 2 * (walked - 1) * ady, 2 * adx);
            out.covered = walked >= 0 && walked <= steps && p.y == a.y + sy * m;
        } else {
            steps = ady;
            walked = (p.y - a.y) * sy;
            let m = ceil_div(2 * adx - ady + 2 * (walked - 1) * adx, 2 * ady);
            out.covered = walked >= 0 && walked <= steps && p.x == a.x + m;
        }
        out.covered = out.covered && inside;
    }
    if gouraud {
        let c0 = rgb_of(q0.x);
        let c1 = rgb_of(q0.y);
        var color = 0;
        for (var i = 0u; i < 3u; i++) {
            var v = c0[i] << 16u;
            if steps != 0 {
                v += mul16_div((c1[i] - c0[i]) * walked, steps);
            }
            color |= ((v >> 19u) & 31) << (i * 5u);
        }
        out.color = color;
    } else {
        out.color = command_color(rgb_of(q0.x));
    }
    return out;
}

// ---- the fragment ---------------------------------------------------------------------------

@fragment
fn fs_draw(in: Varyings) -> @location(0) u32 {
    // A polygon above 1 x covers samples half a 1 x pixel (N / 2 samples)
    // up and left of where its varyings were set up (vs_draw): what the
    // varyings change by over that step, read here while control flow is
    // still uniform, and taken off below.
    let half_pixel = 0.5 * f32(globals.scale);
    let uv_step = (dpdx(in.uv) + dpdy(in.uv)) * half_pixel;
    let color_step = (dpdx(in.color) + dpdy(in.color)) * half_pixel;
    let q_step = (dpdx(in.q) + dpdy(in.q)) * half_pixel;
    let n = i32(globals.scale);
    let fp = vec2<i32>(floor(in.position.xy));
    // The 1 x VRAM pixel, and the N x sample inside it.
    let px = fp / n;
    let sub = fp - px * n;
    let kind = in.mode & 7u;
    if kind == KIND_FILL {
        return in.q0.x;
    }
    if kind == KIND_MIRROR {
        return u32(load_mirror(px.x | (px.y << 10u)));
    }
    let set_bit = select(0, 0x8000, (in.mode & SET) != 0u);
    let check = (in.mode & CHECK) != 0u;
    if kind == KIND_COPY {
        let source = xy(in.q0.x);
        let dest = xy(in.q0.y);
        if (in.q0.z & 2u) != 0u && (load_shadow(fp) & 0x8000) != 0 {
            discard;
        }
        let at = vec2<i32>((px.x - dest.x + source.x) & 1023, (px.y - dest.y + source.y) & 511);
        return u32(load_shadow(at * n + sub) | select(0, 0x8000, (in.q0.z & 1u) != 0u));
    }
    let semi = (in.mode & SEMI) != 0u;
    let abr = (in.mode >> 7u) & 3u;
    let exact = (in.mode & EXACT) != 0u;
    let gouraud = (in.mode & GOURAUD) != 0u;
    let textured = (in.mode & TEXTURED) != 0u;
    var back = 0;
    if semi || check {
        back = load_shadow(fp);
        if check && (back & 0x8000) != 0 {
            discard;
        }
    }
    let flat_rgb = vec3<i32>(round(in.color));

    if kind == KIND_LINE {
        var color = 0;
        if exact {
            let pixel = exact_line(px, gouraud, in.q0, in.q1);
            if !pixel.covered {
                discard;
            }
            color = pixel.color;
        } else {
            // Diagonal lines stop before the area's right and bottom edges.
            let a = xy(in.q1.x);
            let b = xy(in.q1.y);
            let rb = xy(in.q0.w);
            if a.x != b.x && a.y != b.y && (px.x >= rb.x || px.y >= rb.y) {
                discard;
            }
            if gouraud {
                color = command_color(vec3<i32>(floor(in.color)));
            } else {
                color = command_color(rgb_of(in.q0.x));
            }
        }
        if semi {
            color = blend_color(color, back, abr);
        }
        return u32(color | set_bit);
    }

    if kind == KIND_RECT {
        if !textured {
            var color = command_color(flat_rgb);
            if semi {
                color = blend_color(color, back, abr);
            }
            return u32(color | set_bit);
        }
        let coords = floor(in.uv);
        let sub_texel = clamp(vec2<i32>(floor((in.uv - coords) * f32(n))), vec2<i32>(0), vec2<i32>(n - 1));
        let t = sample_texel(vec2<i32>(coords), in.uv - coords, sub_texel, in.q0.x, in.q0.y, in.mode);
        if t == 0 {
            discard;
        }
        var m = flat_rgb;
        if (in.mode & RAW) != 0u {
            m = vec3<i32>(128);
        }
        // Pairs from the clipped left edge; an odd count ends with one pixel.
        let first = i32(in.q0.z & 0xffffu);
        let end = i32(in.q0.z >> 16u);
        var color: i32;
        if px.x == end - 1 && ((end - first) & 1) == 1 {
            color = texel_scalar(t, m, back, semi, abr);
        } else {
            color = texel_pair(t, m, back, semi, abr);
        }
        return u32(color | set_bit | (t & 0x8000));
    }

    // Polygons.
    var span: Span;
    var shade = flat_rgb;
    var shade_first = flat_rgb;
    var coords = vec2<i32>(0);
    var fraction = vec2<f32>(0.5);
    var sub_texel = vec2<i32>(0);
    if exact {
        span = exact_triangle(px, in.q0, in.q1, in.q2, in.q3);
        if !span.covered {
            discard;
        }
        coords = vec2<i32>(span.u >> 16u, span.v >> 16u);
        fraction = vec2<f32>(f32(span.u & 0xffff), f32(span.v & 0xffff)) / 65536.0;
        if gouraud {
            shade = span.c >> vec3<u32>(16u);
            shade_first = select(shade, (span.c - span.dc) >> vec3<u32>(16u), span.odd);
        }
    } else {
        span.odd = false;
        span.single = false;
        // The varyings at the pixel-centre position of this sample (half a
        // 1 x pixel down and right; see the top of fs_draw). A sliver whose
        // 1 / w would not stay positive over that step keeps them unshifted.
        var uv = in.uv - uv_step;
        var rgb = in.color - color_step;
        var q = in.q - q_step;
        if !(q > 0.0) {
            uv = in.uv;
            rgb = in.color;
            q = in.q;
        }
        if (in.mode & PERSPECTIVE) != 0u {
            // Perspective: the divided varyings.
            uv = uv / q;
            if gouraud {
                rgb = rgb / q;
            }
        }
        // Kept inside the triangle's own texture coordinates (any order of
        // its vertices) and colours: the samples along its top and left
        // edges lie up to half a pixel outside it in the pixel-centre
        // mapping, and perspective may overshoot anywhere.
        let a = vec2<f32>(uv_of(in.q1.w));
        let b = vec2<f32>(uv_of(in.q1.w >> 16u));
        let c = vec2<f32>(uv_of(in.q2.x));
        uv = clamp(uv, min(min(a, b), c), max(max(a, b), c));
        if gouraud {
            let c0 = vec3<f32>(rgb_of(in.q2.y));
            let c1 = vec3<f32>(rgb_of(in.q2.z));
            let c2 = vec3<f32>(rgb_of(in.q2.w));
            rgb = clamp(rgb, min(min(c0, c1), c2), max(max(c0, c1), c2));
        }
        // Texel centres: the reference rounds its texture coordinates.
        let tc = uv + 0.5;
        let whole = floor(tc);
        coords = vec2<i32>(whole);
        fraction = tc - whole;
        sub_texel = clamp(vec2<i32>(floor(fraction * f32(n))), vec2<i32>(0), vec2<i32>(n - 1));
        if gouraud {
            shade = vec3<i32>(floor(rgb + 0.5));
            shade_first = shade;
        }
    }
    let dither = (in.mode & DITHER) != 0u;
    if !textured {
        var color: i32;
        if gouraud && dither {
            color = shade_dither(shade, back, semi, abr, px);
        } else {
            if gouraud {
                color = ((shade.x >> 3u) & 31) | (((shade.y >> 3u) & 31) << 5u) | (((shade.z >> 3u) & 31) << 10u);
            } else {
                color = command_color(flat_rgb);
            }
            if semi {
                color = blend_color(color, back, abr);
            }
        }
        return u32(color | set_bit);
    }
    let t = sample_texel(coords, fraction, sub_texel, in.q0.x, in.q0.y, in.mode);
    if t == 0 {
        discard;
    }
    let raw = (in.mode & RAW) != 0u;
    var color: i32;
    if gouraud && dither {
        var modulated = vec3<i32>(0);
        for (var i = 0u; i < 3u; i++) {
            modulated[i] = (((t >> (i * 5u)) & 31) * shade[i]) >> 4u;
        }
        color = shade_dither(modulated, back, semi && (t & 0x8000) != 0, abr, px);
    } else {
        var m = shade;
        if raw {
            m = vec3<i32>(128);
        } else if gouraud {
            // The reference takes 16-bit shade factors here.
            m = (shade << vec3<u32>(16u)) >> vec3<u32>(16u);
            if (in.mode & SHARE) != 0u && exact {
                m = (shade_first << vec3<u32>(16u)) >> vec3<u32>(16u);
            }
        }
        if (in.mode & SCALAR) != 0u || span.single {
            color = texel_scalar(t, m, back, semi, abr);
        } else {
            color = texel_pair(t, m, back, semi, abr);
        }
    }
    return u32(color | set_bit | (t & 0x8000));
}
