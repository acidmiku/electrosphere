// The atmosphere's post pass at world end (src/effects/atmosphere_fx.rs,
// docs/EFFECTS_ATMOSPHERE.md §8): every world pixel with a depth is seen
// through fx-fog's haze (`haze_shade`, haze.wgsl) and written back into the
// N x VRAM with fx-core's dithered `fx_encode`. Follows fx_common.wgsl
// (`FxScene`, `fx_decode`, `fx_encode`) and haze.wgsl (`HazeUniform`,
// `haze_shade`).
//
// - The sky (z cleared to SKY_Z at world begin) is left alone.
// - A pixel without a depth (z = 0: the external view's large near terrain
//   faces, 2D primitives) takes a coarse one from the ordering-table slot of
//   the primitive that drew it (`atmo_coarse_depth`); without a slot in the
//   world it is left alone.
// - The draw-distance fade into the sky (HazeUniform.edge) applies to
//   terrain only: the depth handed to `haze_shade` is 0 for anything else.

const ATMO_SKY_Z: f32 = 1e30;
// Draw orders per row of the order table.
const ATMO_TABLE_WIDTH: u32 = 1024u;
// `fx::Class::Terrain`.
const ATMO_TERRAIN: u32 = 1u;

struct AtmoPost {
    // The world area's origin (N x VRAM pixels: the snapshot's 0, 0), the
    // scale N, and the draw orders in the order table.
    origin: vec2<u32>,
    scale: u32,
    orders: u32,
    // The coarse depth: view z per slot, its offset (z = slot x + y), the
    // sea level (world y), and 1 to bound it by the sea plane.
    coarse: vec4<f32>,
};

@group(0) @binding(0) var<uniform> atmo_scene: FxScene;
@group(0) @binding(1) var<uniform> atmo_haze: HazeUniform;
@group(0) @binding(2) var<uniform> atmo_post: AtmoPost;
// The picture (N x VRAM, up to date over the world area).
@group(0) @binding(3) var atmo_shadow: texture_2d<u32>;
// The world's depths: r view z, g draw order (the area at 0, 0).
@group(0) @binding(4) var atmo_snapshot: texture_2d<f32>;
// Per draw order (order - 1 at x % 1024, x / 1024): its slot | class << 16.
@group(0) @binding(5) var atmo_orders: texture_2d<u32>;

@vertex
fn vs_atmo(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let p = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
}

// The order table's entry of draw order `order` (0: none).
fn atmo_entry(order: u32) -> u32 {
    if order == 0u || order > atmo_post.orders {
        return 0u;
    }
    let i = order - 1u;
    let at = vec2<i32>(i32(i % ATMO_TABLE_WIDTH), i32(i / ATMO_TABLE_WIDTH));
    return textureLoad(atmo_orders, at, 0).r;
}

// The coarse depth of a pixel without one, from its primitive's slot `slot`
// and its ray's world y per unit of view z (`ray_y`, y down): the slot's
// depth (terrain hangs off its farthest vertex's z / 8 + 1), no farther than
// where the ray meets the sea plane. 0: no estimate (not a world slot).
// `coarse_depth` in atmosphere_fx.rs is its twin.
fn atmo_coarse_depth(slot: u32, ray_y: f32) -> f32 {
    if slot < 2u || slot > 4094u {
        return 0.0;
    }
    var z = f32(slot) * atmo_post.coarse.x + atmo_post.coarse.y;
    if atmo_post.coarse.w > 0.0 && ray_y > 1e-6 {
        let below = atmo_post.coarse.z - atmo_scene.eye_h.y;
        if below > 0.0 {
            z = min(z, below / ray_y);
        }
    }
    return z;
}

@fragment
fn fs_atmo(@builtin(position) position: vec4<f32>) -> @location(0) u32 {
    let p = vec2<u32>(position.xy);
    let local = vec2<i32>(p) - vec2<i32>(atmo_post.origin);
    let size = vec2<i32>(textureDimensions(atmo_snapshot));
    if any(local < vec2<i32>(0)) || any(local >= size) {
        discard;
    }
    let snap = textureLoad(atmo_snapshot, local, 0).rg;
    var z = snap.r;
    if z >= ATMO_SKY_Z * 0.5 {
        discard;
    }
    // The pixel's ray: view space at unit depth (x squeezed by widescreen's
    // k), then world space (the transpose of the view matrix).
    let s = position.xy / f32(atmo_post.scale) - atmo_scene.centre_k_scale.xy;
    let h = atmo_scene.eye_h.w;
    let ray = vec3<f32>(s.x / (atmo_scene.centre_k_scale.z * h), s.y / h, 1.0);
    let world_ray = atmo_scene.view0.xyz * ray.x + atmo_scene.view1.xyz * ray.y
        + atmo_scene.view2.xyz * ray.z;
    var terrain = false;
    if z <= 0.0 || (atmo_haze.edge.y > atmo_haze.edge.x && z > atmo_haze.edge.x) {
        let entry = atmo_entry(u32(snap.g));
        terrain = (entry >> 16u) == ATMO_TERRAIN;
        if z <= 0.0 {
            z = atmo_coarse_depth(entry & 0xffffu, world_ray.y);
            if z <= 0.0 {
                discard;
            }
        }
    }
    let old = textureLoad(atmo_shadow, vec2<i32>(p), 0).r;
    let rgb = haze_shade(atmo_haze, fx_decode(old), world_ray * z, select(0.0, z, terrain));
    return fx_encode(rgb, p) | (old & 0x8000u);
}
