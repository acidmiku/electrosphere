// The effects frames' drawing (src/gpu_renderer/fx): appended to draw.wgsl,
// whose vertex and fragment entry points become the functions
// `draw_vertex` and `draw_pixel` (fx::card::fx_draw_source). Every pixel's
// colour is computed exactly as draw.wgsl does.
//
// Second target (`zorder`, Rg32Float): r the view z (the vertices' GTE SZ,
// interpolated with perspective: 1 / the screen-linear 1 / z; 0 without
// one), g the primitive's draw order in the world (an exact f32 integer).

const SKY_Z: f32 = 1e30;
const EFFECT_ORDER: u32 = 16777216u;
const DEFER_TOLERANCE: f32 = 0.015;

// The deferred primitives' depth snapshot (the world area, its pixel 0, 0
// at the area's origin) and where that is.
struct FxGlobals {
    origin: vec2<u32>,
    size: vec2<u32>,
    mode: u32,
    scale: u32,
    pad0: u32,
    pad1: u32,
};

@group(1) @binding(0) var snapshot: texture_2d<f32>;
@group(1) @binding(1) var<uniform> fx: FxGlobals;

struct FxVaryings {
    @builtin(position) position: vec4<f32>,
    @location(0) @interpolate(linear) uv: vec2<f32>,
    @location(1) @interpolate(linear) color: vec3<f32>,
    @location(2) @interpolate(flat) mode: u32,
    @location(3) @interpolate(flat) q0: vec4<u32>,
    @location(4) @interpolate(flat) q1: vec4<u32>,
    @location(5) @interpolate(flat) q2: vec4<u32>,
    @location(6) @interpolate(flat) q3: vec4<u32>,
    @location(7) @interpolate(linear) q: f32,
    // 1 / view z (0: none), screen-linear.
    @location(8) @interpolate(linear) iz: f32,
    @location(9) @interpolate(flat) order: u32,
};

@vertex
fn vs_fx(v: VertexInput, @location(9) depth: f32, @location(10) order: u32) -> FxVaryings {
    let b = draw_vertex(v);
    var out: FxVaryings;
    out.position = b.position;
    out.uv = b.uv;
    out.color = b.color;
    out.mode = b.mode;
    out.q0 = b.q0;
    out.q1 = b.q1;
    out.q2 = b.q2;
    out.q3 = b.q3;
    out.q = b.q;
    out.iz = select(0.0, 1.0 / depth, depth > 0.0);
    out.order = order;
    return out;
}

fn base_varyings(in: FxVaryings) -> Varyings {
    var v: Varyings;
    v.position = in.position;
    v.uv = in.uv;
    v.color = in.color;
    v.mode = in.mode;
    v.q0 = in.q0;
    v.q1 = in.q1;
    v.q2 = in.q2;
    v.q3 = in.q3;
    v.q = in.q;
    return v;
}

// The view z at this sample, at the pixel-centre position the polygon's
// other varyings are read at (half a 1 x pixel back, as fs_draw does); 0
// without one. Call in uniform control flow (derivatives).
fn view_z(in: FxVaryings) -> f32 {
    let step = (dpdx(in.iz) + dpdy(in.iz)) * 0.5 * f32(globals.scale);
    var iz = in.iz - step;
    if !(iz > 0.0) {
        iz = in.iz;
    }
    return select(0.0, 1.0 / iz, iz > 0.0 && in.iz > 0.0);
}

struct FxOut {
    @location(0) color: u32,
    @location(1) zorder: vec2<f32>,
};

@fragment
fn fs_fx_world(in: FxVaryings) -> FxOut {
    let z = view_z(in);
    var out: FxOut;
    out.color = draw_pixel(base_varyings(in));
    out.zorder = vec2<f32>(z, f32(in.order));
    return out;
}

@fragment
fn fs_fx_zclear(in: FxVaryings) -> FxOut {
    var out: FxOut;
    out.color = 0u;
    out.zorder = vec2<f32>(SKY_Z, 0.0);
    return out;
}

// A deferred primitive: hidden where a later opaque world primitive drew
// (its draw order above this one's), or behind effect geometry by more than
// DEFER_TOLERANCE of its own depth.
@fragment
fn fs_fx_deferred(in: FxVaryings) -> @location(0) u32 {
    let z = view_z(in);
    let color = draw_pixel(base_varyings(in));
    let local = vec2<i32>(floor(in.position.xy)) - vec2<i32>(fx.origin);
    let inside = all(local >= vec2<i32>(0)) && all(local < vec2<i32>(fx.size));
    if inside {
        let snap = textureLoad(snapshot, local, 0).rg;
        let last = u32(snap.g);
        if last >= EFFECT_ORDER {
            if z > 0.0 && snap.r > 0.0 && snap.r < z * (1.0 - DEFER_TOLERANCE) {
                discard;
            }
        } else if last > in.order {
            discard;
        }
    }
    return color;
}
