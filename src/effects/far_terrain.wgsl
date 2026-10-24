// Far terrain (src/effects/far_terrain.rs, docs/EFFECTS_TERRAIN.md): the
// mission's terrain beyond the game's own range, drawn at world begin
// beneath every world primitive.
//
// fx_common.wgsl is prepended (fx_encode).
//
// Three passes over the world's draw area at N x:
// 1. `vs_mesh`/`fs_mesh`: the tile meshes of the ring of cells around the
//    camera (instanced per tile type), into the area-sized intermediates
//    (colour, view z) with a reversed depth buffer;
// 2. `vs_full`/`fs_march`: where no mesh drew (depth still 0), a ray march
//    of the heightmap to the horizon, with the curved Earth's drop;
// 3. `vs_full`/`fs_composite`: the intermediates into the N x VRAM
//    (dithered PlayStation pixels, the old mask bit kept) and the z target
//    (view z, draw order 0) where anything drew.
//
// Coordinates: world units, y down; `rel` positions are relative to the eye.
// The game's projection: VRAM x = centre.x + k H x / z, y = centre.y +
// H y / z, z the view z (R (p - eye)), at N x pixel (VRAM - area) N.

struct Far {
    // The view matrix rows (world to view); w: H, k, N.
    view0: vec4<f32>,
    view1: vec4<f32>,
    view2: vec4<f32>,
    // The eye (world units); w: the near clip distance.
    eye: vec4<f32>,
    // The projection centre minus the area's origin (1x pixels); the
    // area's size (1x pixels).
    screen: vec4<f32>,
    // The area's origin at N x (VRAM pixels); unused.
    origin: vec4<f32>,
    // The highest point of the map (smallest y); the Earth's drop starts
    // at this horizontal distance; 1 / (2 R); unused.
    ground: vec4<f32>,
    // The game's depth cue: near, far, on (0 / 1); the relief strength.
    cue: vec4<f32>,
    // The relief fades in between these view depths; unused.
    relief: vec4<f32>,
    // The march: first horizontal distance (the ring's edge), last
    // distance, step in texels of the pixel's level; unused.
    march: vec4<f32>,
    // Toward the sun (world, unit); unused.
    sun: vec4<f32>,
    // Most march steps, bisection steps.
    steps: vec4<u32>,
    // The view matrix's inverse (view to world), by columns.
    inv0: vec4<f32>,
    inv1: vec4<f32>,
    inv2: vec4<f32>,
};

@group(0) @binding(0) var<uniform> far: Far;
// Tile type per grid cell (256 x 256).
@group(0) @binding(1) var grid: texture_2d<u32>;
// Mean heights (world y), 512 x 1024 with 6 levels; 32 >> level texels
// per tile type's cell.
@group(0) @binding(2) var heights: texture_2d<f32>;
// Colours, sRGB: as the game shades its terrain (texel x vertex colour /
// 128) and neutral (vertex colour 0x83, the game's far cue colour).
@group(0) @binding(3) var baked: texture_2d<f32>;
@group(0) @binding(4) var neutral: texture_2d<f32>;
// The highest point (smallest y) of each grid cell and its 8 neighbours,
// cells -16..271 (288 x 288), the wrap folded in.
@group(0) @binding(5) var tops: texture_2d<f32>;

const NO_GROUND: f32 = 1.0e9;
const CELL: f32 = 2048.0;
const TEXEL: f32 = 64.0;

// ---- the map -------------------------------------------------------------------------

// The game's edge wrap of a grid index (8005E4CC).
fn wrap_cell(c: i32) -> i32 {
    if c < 0 {
        return c & 7;
    }
    if c >= 256 {
        return (c & 7) | 248;
    }
    return c;
}

// Tile type of the cell `cell` (floor(world / 2048); grid index - 128).
fn tile_at(cell: vec2<i32>) -> u32 {
    let c = vec2<i32>(wrap_cell(cell.x + 128), wrap_cell(cell.y + 128));
    return textureLoad(grid, c, 0).r;
}

// The atlas texel of global texel `g` at `level` (floor(world / (64 <<
// level))); x < 0 over a cell without terrain.
fn atlas_at(g: vec2<i32>, level: u32) -> vec2<i32> {
    let shift = 5u - level;
    let n = i32(32u >> level);
    let cell = vec2<i32>(g.x >> shift, g.y >> shift);
    let t = tile_at(cell);
    if t == 0u {
        return vec2<i32>(-1, -1);
    }
    let inner = g & vec2<i32>(n - 1);
    return vec2<i32>(i32(t % 16u) * n + inner.x, i32(t / 16u) * n + inner.y);
}

// The bilinear weights' corners: (0,0), (1,0), (0,1), (1,1).
fn corner(c: u32) -> vec2<i32> {
    return vec2<i32>(i32(c & 1u), i32(c >> 1u));
}

fn weight(f: vec2<f32>, c: u32) -> f32 {
    let w = select(1.0 - f, f, vec2<bool>((c & 1u) != 0u, (c >> 1u) != 0u));
    return w.x * w.y;
}

// Height (world y) at world `xz`, bilinear between texel centres at
// `level`; NO_GROUND without terrain.
fn height_at(xz: vec2<f32>, level: u32) -> f32 {
    let u = xz / (TEXEL * f32(1u << level)) - 0.5;
    let base = floor(u);
    let f = u - base;
    let g0 = vec2<i32>(base);
    let n = i32(32u >> level);
    let a0 = atlas_at(g0, level);
    let inner = g0 & vec2<i32>(n - 1);
    if a0.x >= 0 && inner.x < n - 1 && inner.y < n - 1 {
        // One cell: its four texels.
        let h00 = textureLoad(heights, a0, i32(level)).r;
        let h10 = textureLoad(heights, a0 + vec2<i32>(1, 0), i32(level)).r;
        let h01 = textureLoad(heights, a0 + vec2<i32>(0, 1), i32(level)).r;
        let h11 = textureLoad(heights, a0 + vec2<i32>(1, 1), i32(level)).r;
        return mix(mix(h00, h10, f.x), mix(h01, h11, f.x), f.y);
    }
    var sum = 0.0;
    var total = 0.0;
    for (var c = 0u; c < 4u; c++) {
        let a = atlas_at(g0 + corner(c), level);
        if a.x >= 0 {
            let w = weight(f, c);
            sum += w * textureLoad(heights, a, i32(level)).r;
            total += w;
        }
    }
    if total <= 0.0 {
        return NO_GROUND;
    }
    return sum / total;
}

struct Pair {
    baked: vec3<f32>,
    neutral: vec3<f32>,
    total: f32,
};

// Both colours (linear) at global texel coordinates `t0` (level-0 units),
// bilinear at `level`.
fn colours_level(t0: vec2<f32>, level: u32) -> Pair {
    let u = t0 / f32(1u << level) - 0.5;
    let base = floor(u);
    let f = u - base;
    let g0 = vec2<i32>(base);
    var out: Pair;
    out.baked = vec3<f32>(0.0);
    out.neutral = vec3<f32>(0.0);
    out.total = 0.0;
    for (var c = 0u; c < 4u; c++) {
        let a = atlas_at(g0 + corner(c), level);
        if a.x >= 0 {
            let w = weight(f, c);
            out.baked += w * textureLoad(baked, a, i32(level)).rgb;
            out.neutral += w * textureLoad(neutral, a, i32(level)).rgb;
            out.total += w;
        }
    }
    if out.total > 0.0 {
        out.baked /= out.total;
        out.neutral /= out.total;
    }
    return out;
}

// Both colours, trilinear: `level` from the pixel's footprint in texels.
fn colours_at(t0: vec2<f32>, level: f32) -> Pair {
    let l = clamp(level, 0.0, 5.0);
    let l0 = u32(floor(l));
    let a = colours_level(t0, l0);
    let s = l - f32(l0);
    if s <= 0.0 || l0 >= 5u {
        return a;
    }
    let b = colours_level(t0, l0 + 1u);
    var out: Pair;
    out.baked = mix(a.baked, b.baked, s);
    out.neutral = mix(a.neutral, b.neutral, s);
    out.total = mix(a.total, b.total, s);
    return out;
}

// ---- shading ---------------------------------------------------------------------------

fn srgb_encode(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(max(c, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3<f32>(0.0031308));
}

// The game's GTE depth cue at view depth z (8005E7EC's DQA/DQB): IR0 =
// far (z - near) / (z (far - near)), 0..1. Terrain colours move toward
// the neutral 0x83 by it.
fn cue_at(z: f32) -> f32 {
    let near = far.cue.x;
    let farz = far.cue.y;
    if far.cue.z == 0.0 || farz <= near {
        return 0.0;
    }
    return clamp(farz * (z - near) / (z * (farz - near)), 0.0, 1.0);
}

// The colour of a surface (display values 0..1): the game's cue between
// its two colours, then a relief term from the normal `n` (unit, y down)
// and the sun's horizontal direction, fading in beyond the game's range.
fn shade(baked_rgb: vec3<f32>, neutral_rgb: vec3<f32>, n: vec3<f32>, z: f32) -> vec3<f32> {
    let base = mix(baked_rgb, neutral_rgb, cue_at(z));
    let ramp = smoothstep(far.relief.x, far.relief.y, z);
    let slope = dot(n.xz, far.sun.xz);
    let s = clamp(1.0 + far.cue.w * ramp * slope, 0.25, 2.0);
    return clamp(base * s, vec3<f32>(0.0), vec3<f32>(1.0));
}

// The Earth's drop (world y, down) at horizontal distance `d` from the eye.
fn drop_at(d: f32) -> f32 {
    let e = max(d - far.ground.y, 0.0);
    return e * e * far.ground.z;
}

// Clip position of an eye-relative world point: the game's projection
// over the area (x, y), reversed depth near / z (z), w the view z.
fn project(rel: vec3<f32>) -> vec4<f32> {
    let v = vec3<f32>(dot(far.view0.xyz, rel), dot(far.view1.xyz, rel), dot(far.view2.xyz, rel));
    let h = far.view0.w;
    let k = far.view1.w;
    let x = 2.0 * (far.screen.x * v.z + k * h * v.x) / far.screen.z - v.z;
    let y = v.z - 2.0 * (far.screen.y * v.z + h * v.y) / far.screen.w;
    return vec4<f32>(x, y, far.eye.w, v.z);
}

struct TerrainOut {
    @location(0) color: vec4<f32>,
    @location(1) z: f32,
};

// ---- pass 1: the tile meshes -------------------------------------------------------------

struct MeshIn {
    // Relative to the cell's mesh origin; water already at the sea level.
    @location(0) position: vec3<f32>,
    // The triangle's colours (display values; baked's A: its flags).
    @location(1) baked: vec4<f32>,
    @location(2) neutral: vec4<f32>,
    // The cell's mesh origin relative to the eye.
    @location(3) offset: vec4<f32>,
    // The cell (floor(world / 2048), before the wrap).
    @location(4) cell: vec4<i32>,
};

struct MeshOut {
    @builtin(position) position: vec4<f32>,
    // Global level-0 texel coordinates (world / 64).
    @location(0) texel: vec2<f32>,
    @location(1) rel: vec3<f32>,
    @location(2) z: f32,
    @location(3) @interpolate(flat) baked: vec3<f32>,
    @location(4) @interpolate(flat) neutral: vec3<f32>,
};

@vertex
fn vs_mesh(v: MeshIn) -> MeshOut {
    var rel = v.offset.xyz + v.position;
    rel.y += drop_at(length(rel.xz));
    var out: MeshOut;
    out.position = project(rel);
    out.z = out.position.w;
    out.texel = (vec2<f32>(v.cell.xy) * CELL + 1024.0 + v.position.xz) / TEXEL;
    out.rel = rel;
    out.baked = v.baked.rgb;
    out.neutral = v.neutral.rgb;
    return out;
}

@fragment
fn fs_mesh(in: MeshOut) -> TerrainOut {
    let dx = dpdx(in.texel);
    let dy = dpdy(in.texel);
    let footprint = max(length(dx), length(dy));
    var n = cross(dpdx(in.rel), dpdy(in.rel));
    let len = length(n);
    n = select(vec3<f32>(0.0, -1.0, 0.0), n / max(len, 1e-20), len > 0.0);
    // Facing the eye.
    if dot(n, in.rel) > 0.0 {
        n = -n;
    }
    var b = in.baked;
    var u = in.neutral;
    // A top surface takes the atlas (texture detail, filtered by the
    // footprint); a wall keeps its triangle's colour.
    if abs(n.y) > 0.6 {
        let pair = colours_at(in.texel, log2(max(footprint, 1e-6)));
        if pair.total > 0.0 {
            b = srgb_encode(pair.baked);
            u = srgb_encode(pair.neutral);
        }
    }
    var out: TerrainOut;
    out.color = vec4<f32>(shade(b, u, n, in.z), 1.0);
    out.z = in.z;
    return out;
}

// Cells wholly within the game's range, which its own terrain covers: their
// triangles' colours alone (cheap), seen only through the game's cracks.
@fragment
fn fs_mesh_near(in: MeshOut) -> TerrainOut {
    var n = cross(dpdx(in.rel), dpdy(in.rel));
    let len = length(n);
    n = select(vec3<f32>(0.0, -1.0, 0.0), n / max(len, 1e-20), len > 0.0);
    if dot(n, in.rel) > 0.0 {
        n = -n;
    }
    var out: TerrainOut;
    out.color = vec4<f32>(shade(in.baked, in.neutral, n, in.z), 1.0);
    out.z = in.z;
    return out;
}

// ---- pass 2: the ray march ---------------------------------------------------------------

@vertex
fn vs_full(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let x = f32(i32(i & 1u) * 4 - 1);
    let y = f32(i32(i >> 1u) * 4 - 1);
    return vec4<f32>(x, y, 0.0, 1.0);
}

// The highest point (smallest y) of cell `cell` and its neighbours.
fn cell_top(cell: vec2<i32>) -> f32 {
    var c = cell + vec2<i32>(128);
    // Beyond the map the wrap repeats every 8 cells.
    c = select(c, vec2<i32>(-16) + (c & vec2<i32>(7)), c < vec2<i32>(-8));
    c = select(c, vec2<i32>(264) + (c & vec2<i32>(7)), c >= vec2<i32>(264));
    return textureLoad(tops, c + vec2<i32>(16), 0).r;
}

// The highest point of the 8 x 8 cells of block `block` (floor(world /
// 16384)) and their neighbours: level 3 of the cell tops (blocks -2..33 of
// grid indices; beyond, the wrap repeats blocks -2 and 33).
fn block_top(block: vec2<i32>) -> f32 {
    let b = clamp(block + vec2<i32>(16), vec2<i32>(-2), vec2<i32>(33));
    return textureLoad(tops, b + vec2<i32>(2), 3).r;
}

// The ray's height over the flat map at distance t (the Earth's drop
// added back to the ray instead of to the ground).
fn ray_y(d: vec3<f32>, t: f32, dxz: f32) -> f32 {
    return far.eye.y + d.y * t - drop_at(t * dxz);
}

// The height level whose texels match a pixel's footprint at distance t.
fn level_at(t: f32) -> u32 {
    let texels = t / (far.view0.w * far.view2.w * TEXEL);
    return u32(clamp(floor(log2(max(texels, 1e-6))), 0.0, 5.0));
}

struct Hit {
    t: f32,
    level: u32,
};

// March the heightmap along unit direction `d` from the ring's edge;
// t < 0: no hit (the sky).
fn march(d: vec3<f32>) -> Hit {
    var hit: Hit;
    hit.t = -1.0;
    hit.level = 0u;
    let dxz = length(d.xz);
    if dxz < 1e-6 {
        return hit;
    }
    let tmax = far.march.y;
    var t = max(far.march.x / dxz, far.eye.w);
    let top = far.ground.x;
    // Grid cell steps (2048 units) in x and z.
    let inv = vec2<f32>(
        select(1e30, 1.0 / d.x, abs(d.x) > 1e-9),
        select(1e30, 1.0 / d.z, abs(d.z) > 1e-9),
    );
    let ahead = vec2<f32>(select(0.0, 1.0, d.x > 0.0), select(0.0, 1.0, d.z > 0.0));
    var steps = 0u;
    var prev = t;
    var found = false;
    var level = 0u;
    loop {
        if t > tmax || steps >= far.steps.x {
            break;
        }
        steps += 1u;
        let p = far.eye.xyz + d * t;
        let y = ray_y(d, t, dxz);
        if y < top {
            // Above every height of the map: rising (with the drop), the
            // sky; else on to where it can first meet the highest point
            // (y grows at most at the rate d.y).
            if d.y - 2.0 * max(t * dxz - far.ground.y, 0.0) * far.ground.z * dxz <= 0.0 {
                break;
            }
            let jump = (top - y) / max(d.y, 1e-9);
            if jump > CELL {
                prev = t;
                t += jump;
                continue;
            }
        }
        // A block of 8 x 8 cells the ray passes above (the open sea): its
        // exit at once. Over 23,000 units the drop's curve lifts the ray
        // at most 2 units above the line between the ends.
        let block = vec2<i32>(floor(p.xz / (8.0 * CELL)));
        let block_edge = (vec2<f32>(block) + ahead) * (8.0 * CELL);
        let block_exits = (block_edge - far.eye.xz) * inv;
        let block_exit = max(min(block_exits.x, block_exits.y), t) + max(1.0, t * 1e-5);
        if max(y, ray_y(d, block_exit, dxz)) + 4.0 < block_top(block) {
            prev = t;
            t = block_exit;
            continue;
        }
        // The cell's exit.
        let cell = vec2<i32>(floor(p.xz / CELL));
        let edge = (vec2<f32>(cell) + ahead) * CELL;
        let exits = (edge - far.eye.xz) * inv;
        // A little past the edge: at a few 100,000 units a hundredth
        // would vanish in f32 and the march would stall.
        let exit = max(min(exits.x, exits.y), t) + max(1.0, t * 1e-5);
        let ctop = cell_top(cell);
        let y_exit = ray_y(d, exit, dxz);
        if max(y, y_exit) + 1.0 < ctop {
            // Above everything in this cell (and its neighbours').
            prev = t;
            t = exit;
            continue;
        }
        // Through the cell in texel steps of the pixel's footprint level.
        level = level_at(t);
        let h = height_at(p.xz, level);
        if y >= h {
            found = true;
            break;
        }
        prev = t;
        t += TEXEL * f32(1u << level) * far.march.z;
    }
    if !found {
        return hit;
    }
    // Refine between the last point above and the first below.
    var lo = prev;
    var hi = t;
    for (var i = 0u; i < far.steps.y; i++) {
        let mid = 0.5 * (lo + hi);
        let p = far.eye.xyz + d * mid;
        if ray_y(d, mid, dxz) >= height_at(p.xz, level) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    hit.t = hi;
    hit.level = level;
    return hit;
}

@fragment
fn fs_march(@builtin(position) frag: vec4<f32>) -> TerrainOut {
    var out: TerrainOut;
    out.color = vec4<f32>(0.0);
    out.z = 0.0;
    let h = far.view0.w;
    let k = far.view1.w;
    let n = far.view2.w;
    let s = frag.xy / n - far.screen.xy;
    let v = vec3<f32>(s.x / (k * h), s.y / h, 1.0);
    let w = far.inv0.xyz * v.x + far.inv1.xyz * v.y + far.inv2.xyz * v.z;
    let d = w / length(w);
    let hit = march(d);
    if hit.t < 0.0 {
        return out;
    }
    let p = far.eye.xyz + d * hit.t;
    let z = hit.t * dot(d, far.view2.xyz);
    // The normal from the heights at the hit's level.
    let e = TEXEL * f32(1u << hit.level);
    let hx = height_at(p.xz + vec2<f32>(e, 0.0), hit.level) - height_at(p.xz - vec2<f32>(e, 0.0), hit.level);
    let hz = height_at(p.xz + vec2<f32>(0.0, e), hit.level) - height_at(p.xz - vec2<f32>(0.0, e), hit.level);
    var normal = normalize(vec3<f32>(hx / (2.0 * e), -1.0, hz / (2.0 * e)));
    if abs(hx) > 1.0e6 || abs(hz) > 1.0e6 {
        normal = vec3<f32>(0.0, -1.0, 0.0);
    }
    // The pixel's footprint on the ground, in texels.
    let slant = max(abs(dot(normal, d)), 0.2);
    let texels = hit.t / (h * n * TEXEL) / slant;
    let pair = colours_at(p.xz / TEXEL, log2(max(texels, 1e-6)));
    out.color = vec4<f32>(shade(srgb_encode(pair.baked), srgb_encode(pair.neutral), normal, z), 1.0);
    out.z = z;
    return out;
}

// ---- pass 3: into VRAM and the z target --------------------------------------------------

@group(1) @binding(0) var color_in: texture_2d<f32>;
@group(1) @binding(1) var z_in: texture_2d<f32>;
@group(1) @binding(2) var shadow: texture_2d<u32>;

struct CompositeOut {
    @location(0) color: u32,
    @location(1) zorder: vec2<f32>,
};

@fragment
fn fs_composite(@builtin(position) frag: vec4<f32>) -> CompositeOut {
    let p = vec2<i32>(floor(frag.xy));
    let local = p - vec2<i32>(far.origin.xy);
    let z = textureLoad(z_in, local, 0).r;
    if !(z > 0.0) {
        discard;
    }
    let c = textureLoad(color_in, local, 0).rgb;
    let old = textureLoad(shadow, p, 0).r;
    var out: CompositeOut;
    out.color = fx_encode(c, vec2<u32>(p)) | (old & 0x8000u);
    // Draw order 0: beneath every world primitive.
    out.zorder = vec2<f32>(z, 0.0);
    return out;
}
