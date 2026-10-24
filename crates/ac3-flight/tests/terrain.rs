//! Terrain, fog and sky routines (the 800331D4 tree) on original entry
//! snapshots must reproduce the original return snapshots: all RAM outside
//! the stack, the scratchpad, every GTE register and `v0` (the
//! `snapshot::check_call` comparison), plus the listed outputs that live on
//! the stack (the view descriptor in 800331D4's frame).
//!
//! Captures (`analysis/flight-runtime/`, flight callbacks 80033C58 of frames
//! 307, 309 and 311 from the mission-006 checkpoint):
//! - `terrain-003`: two calls of every routine of the first-mission path;
//! - `terrain-004`: up to sixteen calls of the small visibility leaves;
//! - `terrain-005`..`terrain-008`: oracle runs (scratch/oracle-head.lua, see
//!   each capture's `oracle.json`) that patch guest RAM at every 80033748
//!   entry to activate the branches the first mission never takes: gameplay
//!   flags 4/8 (depth masks, second terrain view) and two animated palette
//!   entries (005), and background types 2, 3 and 4 (006, 007, 008).
//!
//! Tests named `blocked_*` exercise routines whose subtree calls shared and
//! SDK routines that were pending when they were written
//! (`sdk::apply_rot_matrix_lv`, `sdk::rot_matrix_yxz`, `sdk::ratan2`,
//! `sdk::rand`, `shared::f_800306ac`, `shared::f_80030534`,
//! `shared::f_80082740`, and rcos 800A4698, MulMatrix0 800A4B08,
//! SetMulMatrix 800A4ED8, NormalClip 800A58E8, now in `sdk.rs`).
use ac3_flight::{Machine, snapshot, terrain};
use std::path::PathBuf;

fn index(capture: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR"))
        .join("analysis/flight-runtime")
        .join(capture)
        .join("snapshots.jsonl")
}

/// An output that lives on the stack (excluded from `snapshot::compare`):
/// `length` bytes at argument register `register` plus `offset`.
#[derive(Clone, Copy)]
struct StackOutput {
    register: usize,
    offset: u32,
    length: u32,
}
const fn stack(register: usize, offset: u32, length: u32) -> StackOutput {
    StackOutput {
        register,
        offset,
        length,
    }
}

/// Like `snapshot::check_call` through the module's `dispatch` (which claims
/// every translated routine), and additionally requiring the listed stack
/// outputs to match the original byte for byte. Returns the bytes the
/// original changed (outside the stack plus the listed stack outputs).
fn check_dispatch_stack(
    capture: &str,
    callee: u32,
    occurrence: usize,
    with_v0: bool,
    outputs: &[StackOutput],
) -> Result<usize, String> {
    let path = index(capture);
    let directory = path.parent().expect("directory");
    let infos = snapshot::read_index(&path).map_err(|e| e.to_string())?;
    let entry = infos
        .iter()
        .filter(|s| s.callee == callee && s.stage == "entry")
        .nth(occurrence)
        .ok_or_else(|| format!("{callee:08x}: no entry snapshot #{occurrence}"))?;
    let exit = infos
        .iter()
        .find(|s| {
            s.callee == callee
                && s.stage == "return"
                && s.sequence > entry.sequence
                && s.ra == entry.ra
                && s.registers.gpr[29] == entry.registers.gpr[29]
        })
        .ok_or_else(|| format!("{callee:08x}: no return snapshot"))?;
    let entry = snapshot::Snapshot::load(directory, entry).map_err(|e| e.to_string())?;
    let exit = snapshot::Snapshot::load(directory, exit).map_err(|e| e.to_string())?;
    let mut m = entry.machine();
    let args = [entry.gpr(4), entry.gpr(5), entry.gpr(6), entry.gpr(7)];
    let v0 = terrain::dispatch(&mut m, callee, args).expect("claimed");
    m.take_fault().map_err(|f| format!("{callee:08x}: {f}"))?;
    let differences = snapshot::compare(&m, &exit);
    if !differences.is_empty() {
        return Err(format!("{callee:08x}: {}", differences.summary()));
    }
    if with_v0 && v0 != exit.gpr(2) {
        return Err(format!(
            "{callee:08x}: v0 {v0:#010x}, original {:#010x}",
            exit.gpr(2)
        ));
    }
    let mut changed = snapshot::changed_bytes(&entry, &exit);
    for output in outputs {
        let start = (entry.gpr(output.register).wrapping_add(output.offset) & 0x1f_ffff) as usize;
        for i in start..start + output.length as usize {
            if m.ram()[i] != exit.ram[i] {
                return Err(format!(
                    "{callee:08x}: stack output byte {:#010x}: {:#04x}, original {:#04x}",
                    0x8000_0000 + i as u32,
                    m.ram()[i],
                    exit.ram[i]
                ));
            }
            changed += usize::from(entry.ram[i] != exit.ram[i]);
        }
    }
    Ok(changed)
}

/// Number of entry snapshots of `callee` in `capture`.
fn entries(capture: &str, callee: u32) -> usize {
    snapshot::read_index(&index(capture))
        .expect("index")
        .iter()
        .filter(|s| s.callee == callee && s.stage == "entry")
        .count()
}

/// Check every captured call of `callee`; returns the bytes changed.
fn check_dispatch(capture: &str, callee: u32, with_v0: bool, outputs: &[StackOutput]) -> usize {
    let count = entries(capture, callee);
    assert!(count >= 2, "{callee:08x}: {count} snapshots");
    let mut changed = 0;
    for occurrence in 0..count {
        changed += check_dispatch_stack(capture, callee, occurrence, with_v0, outputs)
            .unwrap_or_else(|e| panic!("occurrence {occurrence}: {e}"));
    }
    changed
}

/// Non-vacuity: the original changed at least `min` compared bytes (tests
/// with `min` 0 rely on the asserted `v0`).
fn at_least(changed: usize, min: usize) {
    eprintln!("original changed {changed} compared bytes");
    assert!(changed >= min, "only {changed} bytes changed");
}

const T3: &str = "terrain-003";
/// Up to sixteen calls of each small visibility leaf (frames 307-311).
const T4: &str = "terrain-004";
/// Oracle: gameplay flags 4 and 8, two animated palette entries.
const T5: &str = "terrain-005";
/// Oracle: background type 2 (cloud layer).
const T6: &str = "terrain-006";
/// Oracle: background type 3 (lit dome).
const T7: &str = "terrain-007";
/// Oracle: background type 4 (night sky).
const T8: &str = "terrain-008";
/// Oracle: background type 2, camera just above the cloud layer.
const T13: &str = "terrain-013";
/// Oracle: camera altitude 1024.
const T14: &str = "terrain-014";
/// Oracle: camera pitch 0xC00 (up).
const T15: &str = "terrain-015";
/// Oracle: camera pitch -0xC00 (down).
const T16: &str = "terrain-016";
/// Oracle: background type 2, camera inside the cloud region above the layer.
const T17: &str = "terrain-017";
/// Oracle: camera altitude 512.
const T18: &str = "terrain-018";
/// The 8005FE48 view descriptor (0x118 bytes, in 800331D4's frame).
const VIEW: u32 = 0x118;

macro_rules! snapshot_test {
    ($name:ident, $callee:expr, $v0:expr, $min:expr) => {
        snapshot_test!($name, T3, $callee, $v0, $min, []);
    };
    ($name:ident, $callee:expr, $v0:expr, $min:expr, [$($output:expr),*]) => {
        snapshot_test!($name, T3, $callee, $v0, $min, [$($output),*]);
    };
    ($name:ident, $capture:expr, $callee:expr, $v0:expr, $min:expr, [$($output:expr),*]) => {
        #[test]
        #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/terrain-NNN captures"]
        fn $name() {
            let changed = check_dispatch($capture, $callee, $v0, &[$($output),*]);
            at_least(changed, $min);
        }
    };
}

// ---- terrain ---------------------------------------------------------------------
snapshot_test!(select_cells_8005cda4, 0x8005_cda4, true, 1000);
snapshot_test!(draw_cells_8005ff9c, 0x8005_ff9c, false, 1000);
snapshot_test!(mesh_gouraud_quads_80060470, 0x8006_0470, false, 1);
snapshot_test!(mesh_flat_quads_80060214, 0x8006_0214, false, 1);
snapshot_test!(subdivide_quad_80060dc0, 0x8006_0dc0, true, 1);
snapshot_test!(subdivide_level_800607dc, 0x8006_07dc, true, 1);
snapshot_test!(blocked_draw_terrain_8005cfb4, 0x8005_cfb4, true, 1000);

// ---- visibility --------------------------------------------------------------------
snapshot_test!(max_x_index_8005d958, T4, 0x8005_d958, true, 0, []);
snapshot_test!(min_x_index_8005d9a8, T4, 0x8005_d9a8, true, 0, []);
snapshot_test!(
    clip_segment_8005dae8,
    T4,
    0x8005_dae8,
    true,
    1,
    [stack(4, 0, 4), stack(5, 0, 4)]
);
snapshot_test!(
    plane_intersection_8005dd44,
    T4,
    0x8005_dd44,
    true,
    1,
    [stack(7, 0, 12)]
);
snapshot_test!(
    view_tiles_8005fd68,
    0x8005_fd68,
    false,
    1,
    [stack(4, 0, VIEW)]
);
snapshot_test!(
    make_grid_8005f4ec,
    0x8005_f4ec,
    false,
    1,
    [stack(4, 0, VIEW)]
);
snapshot_test!(
    blocked_unproject_8005df68,
    T4,
    0x8005_df68,
    true,
    1,
    [stack(5, 0, 12)]
);
snapshot_test!(
    blocked_project_vector_8005e030,
    T4,
    0x8005_e030,
    true,
    1,
    [stack(5, 0, 4)]
);
snapshot_test!(
    blocked_view_frustum_8005e140,
    0x8005_e140,
    false,
    1,
    [stack(7, 0, VIEW)]
);
snapshot_test!(
    blocked_build_view_8005fe48,
    0x8005_fe48,
    false,
    1,
    [stack(7, 0, VIEW)]
);

// ---- fog and sky ---------------------------------------------------------------------
snapshot_test!(draw_fog_80092bcc, 0x8009_2bcc, true, 1000);
snapshot_test!(project_bands_80092f90, 0x8009_2f90, false, 1);
snapshot_test!(classify_corners_8009309c, 0x8009_309c, false, 1);
snapshot_test!(sky_colors_800933cc, 0x8009_33cc, false, 1);
snapshot_test!(blend_color_800936a0, 0x8009_36a0, false, 1);
snapshot_test!(draw_bands_800936fc, 0x8009_36fc, false, 100);
snapshot_test!(flat_band_80093920, 0x8009_3920, false, 1);
snapshot_test!(gradient_band_80093af4, 0x8009_3af4, false, 1);
snapshot_test!(draw_stars_80094300, 0x8009_4300, false, 1);
snapshot_test!(blocked_sky_camera_80092e50, 0x8009_2e50, false, 1);
snapshot_test!(blocked_sky_bands_800931b8, 0x8009_31b8, true, 1);
snapshot_test!(blocked_draw_sun_80093fb8, 0x8009_3fb8, false, 1);
snapshot_test!(blocked_draw_sky_80093e2c, 0x8009_3e2c, true, 1000);

// ---- whole world pass ------------------------------------------------------------------
snapshot_test!(blocked_render_world_800331d4, 0x8003_31d4, true, 10_000);

// ---- oracle runs: branches inactive in the first mission ---------------------------------------
snapshot_test!(oracle_depth_masks_8005d824, T5, 0x8005_d824, true, 100, []);
snapshot_test!(
    oracle_animate_palette_8005d5fc,
    T5,
    0x8005_d5fc,
    true,
    100,
    []
);
snapshot_test!(
    oracle_view_tiles_8005fd68,
    T5,
    0x8005_fd68,
    false,
    1,
    [stack(4, 0, VIEW)]
);
snapshot_test!(
    oracle_make_grid_8005f4ec,
    T5,
    0x8005_f4ec,
    false,
    1,
    [stack(4, 0, VIEW)]
);
snapshot_test!(
    blocked_oracle_view_frustum_8005e140,
    T5,
    0x8005_e140,
    false,
    1,
    [stack(7, 0, VIEW)]
);
snapshot_test!(
    blocked_oracle_build_view_8005fe48,
    T5,
    0x8005_fe48,
    false,
    1,
    [stack(7, 0, VIEW)]
);
snapshot_test!(
    blocked_oracle_draw_terrain_8005cfb4,
    T5,
    0x8005_cfb4,
    true,
    1000,
    []
);
snapshot_test!(
    blocked_oracle_render_world_800331d4,
    T5,
    0x8003_31d4,
    true,
    10_000,
    []
);
snapshot_test!(oracle_cloud_layer_80095090, T6, 0x8009_5090, true, 1, []);
snapshot_test!(
    oracle_cloud_sky_bands_800945e8,
    T6,
    0x8009_45e8,
    false,
    1,
    []
);
snapshot_test!(oracle_cloud_bands_800947cc, T6, 0x8009_47cc, false, 1, []);
snapshot_test!(oracle_screen_outcode_80094c1c, T6, 0x8009_4c1c, true, 0, []);
snapshot_test!(
    blocked_oracle_cloud_sky_bands_80094900,
    T6,
    0x8009_4900,
    true,
    100,
    []
);
snapshot_test!(
    blocked_oracle_cloud_sky_80095b94,
    T6,
    0x8009_5b94,
    true,
    100,
    []
);
snapshot_test!(
    blocked_oracle_render_world_t2_800331d4,
    T6,
    0x8003_31d4,
    true,
    10_000,
    []
);
snapshot_test!(
    blocked_oracle_dome_ring_80095bc4,
    T7,
    0x8009_5bc4,
    false,
    1,
    []
);
snapshot_test!(
    blocked_oracle_draw_dome_80095d40,
    T7,
    0x8009_5d40,
    false,
    100,
    []
);
snapshot_test!(
    blocked_oracle_dome_sky_800964fc,
    T7,
    0x8009_64fc,
    true,
    100,
    []
);
snapshot_test!(
    blocked_oracle_render_world_t3_800331d4,
    T7,
    0x8003_31d4,
    true,
    10_000,
    []
);
snapshot_test!(
    oracle_compact_ot_runs_80083330,
    T8,
    0x8008_3330,
    false,
    1,
    []
);
snapshot_test!(oracle_fade_overlay_8008e238, T8, 0x8008_e238, true, 50, []);
snapshot_test!(oracle_night_ring_80096954, T8, 0x8009_6954, false, 100, []);
snapshot_test!(
    oracle_night_ground_row_80096d5c,
    T8,
    0x8009_6d5c,
    false,
    100,
    []
);
snapshot_test!(
    blocked_oracle_night_dome_80096a5c,
    T8,
    0x8009_6a5c,
    false,
    100,
    []
);
snapshot_test!(
    blocked_oracle_night_ground_80096e98,
    T8,
    0x8009_6e98,
    false,
    100,
    []
);
snapshot_test!(
    blocked_oracle_night_sky_8009668c,
    T8,
    0x8009_668c,
    true,
    100,
    []
);
snapshot_test!(
    blocked_oracle_render_world_t4_800331d4,
    T8,
    0x8003_31d4,
    true,
    10_000,
    []
);
// Camera oracles (patched at 800331D4 entry): low camera (014, frustum
// "above the -3000 plane" path), pitch up (015) and down (016, every corner
// hits the ground: 8005D9F8), camera just above the cloud layer (013).
snapshot_test!(
    oracle_cloud_layer_t13_80095090,
    T13,
    0x8009_5090,
    true,
    1,
    []
);
snapshot_test!(
    blocked_oracle_view_frustum_t13_8005e140,
    T13,
    0x8005_e140,
    false,
    1,
    [stack(7, 0, VIEW)]
);
snapshot_test!(
    blocked_oracle_render_world_t13_800331d4,
    T13,
    0x8003_31d4,
    true,
    10_000,
    []
);
snapshot_test!(
    oracle_view_tiles_t14_8005fd68,
    T14,
    0x8005_fd68,
    false,
    1,
    [stack(4, 0, VIEW)]
);
snapshot_test!(
    oracle_make_grid_t14_8005f4ec,
    T14,
    0x8005_f4ec,
    false,
    1,
    [stack(4, 0, VIEW)]
);
snapshot_test!(
    oracle_clip_segment_t14_8005dae8,
    T14,
    0x8005_dae8,
    true,
    0,
    [stack(4, 0, 4), stack(5, 0, 4)]
);
snapshot_test!(
    blocked_oracle_unproject_t14_8005df68,
    T14,
    0x8005_df68,
    true,
    1,
    [stack(5, 0, 12)]
);
snapshot_test!(
    blocked_oracle_view_frustum_t14_8005e140,
    T14,
    0x8005_e140,
    false,
    1,
    [stack(7, 0, VIEW)]
);
snapshot_test!(
    blocked_oracle_build_view_t14_8005fe48,
    T14,
    0x8005_fe48,
    false,
    1,
    [stack(7, 0, VIEW)]
);
snapshot_test!(
    blocked_oracle_draw_terrain_t14_8005cfb4,
    T14,
    0x8005_cfb4,
    true,
    1000,
    []
);
snapshot_test!(
    blocked_oracle_render_world_t14_800331d4,
    T14,
    0x8003_31d4,
    true,
    10_000,
    []
);
snapshot_test!(
    oracle_view_tiles_t15_8005fd68,
    T15,
    0x8005_fd68,
    false,
    1,
    [stack(4, 0, VIEW)]
);
snapshot_test!(
    oracle_make_grid_t15_8005f4ec,
    T15,
    0x8005_f4ec,
    false,
    1,
    [stack(4, 0, VIEW)]
);
snapshot_test!(
    oracle_clip_segment_t15_8005dae8,
    T15,
    0x8005_dae8,
    true,
    0,
    [stack(4, 0, 4), stack(5, 0, 4)]
);
snapshot_test!(
    blocked_oracle_view_frustum_t15_8005e140,
    T15,
    0x8005_e140,
    false,
    1,
    [stack(7, 0, VIEW)]
);
snapshot_test!(
    blocked_oracle_draw_terrain_t15_8005cfb4,
    T15,
    0x8005_cfb4,
    true,
    1,
    []
);
snapshot_test!(
    blocked_oracle_render_world_t15_800331d4,
    T15,
    0x8003_31d4,
    true,
    1000,
    []
);
snapshot_test!(
    oracle_view_tiles_t16_8005fd68,
    T16,
    0x8005_fd68,
    false,
    1,
    [stack(4, 0, VIEW)]
);
snapshot_test!(
    oracle_make_grid_t16_8005f4ec,
    T16,
    0x8005_f4ec,
    false,
    1,
    [stack(4, 0, VIEW)]
);
snapshot_test!(
    oracle_clip_segment_t16_8005dae8,
    T16,
    0x8005_dae8,
    true,
    0,
    [stack(4, 0, 4), stack(5, 0, 4)]
);
snapshot_test!(
    blocked_oracle_corners_same_side_t16_8005d9f8,
    T16,
    0x8005_d9f8,
    true,
    0,
    []
);
snapshot_test!(
    blocked_oracle_view_frustum_t16_8005e140,
    T16,
    0x8005_e140,
    false,
    1,
    [stack(7, 0, VIEW)]
);
snapshot_test!(
    blocked_oracle_draw_terrain_t16_8005cfb4,
    T16,
    0x8005_cfb4,
    true,
    1000,
    []
);
// Looking down, the world pass changes fewer bytes (7234 over the three
// calls) than the level views.
snapshot_test!(
    blocked_oracle_render_world_t16_800331d4,
    T16,
    0x8003_31d4,
    true,
    5000,
    []
);
// Camera inside the cloud region, just above the layer (017): near cloud
// cells are subdivided (80094C78/80094B60).
snapshot_test!(
    oracle_cloud_layer_t17_80095090,
    T17,
    0x8009_5090,
    true,
    1000,
    []
);
snapshot_test!(
    oracle_subdivide_cloud_quad_80094c78,
    T17,
    0x8009_4c78,
    false,
    100,
    []
);
snapshot_test!(
    oracle_cloud_midpoint_80094b60,
    T17,
    0x8009_4b60,
    false,
    1,
    []
);
snapshot_test!(
    oracle_screen_outcode_t17_80094c1c,
    T17,
    0x8009_4c1c,
    true,
    0,
    []
);
snapshot_test!(
    blocked_oracle_cloud_sky_t17_80095b94,
    T17,
    0x8009_5b94,
    true,
    1000,
    []
);
snapshot_test!(
    blocked_oracle_render_world_t17_800331d4,
    T17,
    0x8003_31d4,
    true,
    10_000,
    []
);

// Camera at altitude 512 (018): more near-face terrain set-ups.
snapshot_test!(
    oracle_subdivide_quad_t18_80060dc0,
    T18,
    0x8006_0dc0,
    true,
    1,
    []
);
snapshot_test!(
    oracle_subdivide_level_t18_800607dc,
    T18,
    0x8006_07dc,
    true,
    1,
    []
);
snapshot_test!(
    blocked_oracle_draw_terrain_t18_8005cfb4,
    T18,
    0x8005_cfb4,
    true,
    1000,
    []
);
snapshot_test!(
    blocked_oracle_render_world_t18_800331d4,
    T18,
    0x8003_31d4,
    true,
    1000,
    []
);
// ---- edge paths not reached by the captures ----------------------------------------------

#[test]
fn clip_segment_swaps_and_clips_both_ends() {
    let mut m = Machine::default();
    let (a, b) = (0x8010_0000, 0x8010_0004);
    // a inside, b beyond the right and bottom edges.
    m.sh(a, 0);
    m.sh(a + 2, 0);
    m.sh(b, 400);
    m.sh(b + 2, 200);
    assert_eq!(terrain::clip_segment(&mut m, a, b, 160, 128), 1);
    assert_eq!((m.lh(a), m.lh(a + 2)), (0, 0), "inside end untouched");
    assert_eq!((m.lh(b), m.lh(b + 2)), (160, 80), "clipped to x = 160");
    // Both ends left of the screen: rejected without changes.
    m.sh(a, -300_i16 as u16);
    m.sh(b, -200_i16 as u16);
    assert_eq!(terrain::clip_segment(&mut m, a, b, 160, 128), 0);
    assert_eq!(m.lh(a), -300);
    assert!(m.take_fault().is_ok());
}

#[test]
fn sky_bands_zero_distance_reports_break() {
    let mut m = Machine::default();
    let (gameplay, params, base, sky) = (0x8010_0000, 0x8011_0000, 0x1f80_0000, 0x1f80_001c);
    m.sw(ac3_flight::GP + 0x2d0, gameplay);
    m.sw(gameplay + 0x244, params);
    m.sw(base + 0xc, (-0x7000_i32) as u32); // camera far above every band
    m.sh(base + 0x1a, 0); // zero projection distance
    m.sw(sky + 0x170, 1);
    assert_eq!(terrain::sky_bands(&mut m, base, sky), 4);
    // The original divides 0x3BFF88 by the distance and executes `break 7`.
    assert_eq!(
        m.take_fault(),
        Err(ac3_flight::Fault::Trap {
            address: 0x8009_3274,
            code: 7
        })
    );
    assert_eq!(
        m.lh(sky + 0x30),
        -1,
        "hardware quotient of a positive dividend"
    );
}
#[test]
fn plane_intersection_parallel_and_behind() {
    let mut m = Machine::default();
    let (origin, direction, out) = (0x8010_0000, 0x8010_0010, 0x8010_0020);
    for (i, v) in [100_u32, 50, 300].into_iter().enumerate() {
        m.sw(origin + i as u32 * 4, v);
    }
    // Parallel ray at another height: no intersection, nothing written.
    for (i, v) in [7_u32, 0, 9].into_iter().enumerate() {
        m.sw(direction + i as u32 * 4, v);
    }
    assert_eq!(
        terrain::plane_intersection(&mut m, origin, direction, 0, out),
        0
    );
    assert_eq!(m.lw(out), 0);
    // Parallel ray in the plane: the direction itself.
    assert_eq!(
        terrain::plane_intersection(&mut m, origin, direction, 50, out),
        0
    );
    assert_eq!((m.lw(out), m.lw(out + 4), m.lw(out + 8)), (7, 0, 9));
    // Upward ray toward a plane below: behind.
    m.sw(direction + 4, 10);
    assert_eq!(
        terrain::plane_intersection(&mut m, origin, direction, 0, out),
        0
    );
    // Downward ray reaching the plane: scaled, limited to the ray extent.
    for (i, v) in [-40_i32, -10, 20].into_iter().enumerate() {
        m.sw(direction + i as u32 * 4, v as u32);
    }
    assert_eq!(
        terrain::plane_intersection(&mut m, origin, direction, 0, out),
        0
    );
    assert_eq!(m.lw(out) as i32, -40, "x limited to the direction");
    assert_eq!(m.lw(out + 4) as i32, -10, "y follows the limited end");
    assert_eq!(m.lw(out + 8), 20);
    // A longer ray reaches the plane inside its extent.
    m.sw(direction + 4, (-100_i32) as u32);
    assert_eq!(
        terrain::plane_intersection(&mut m, origin, direction, 0, out),
        1
    );
    assert_eq!(
        (m.lw(out) as i32, m.lw(out + 4) as i32, m.lw(out + 8)),
        (-20, -50, 10)
    );
    assert!(m.take_fault().is_ok());
}

#[test]
fn select_cells_border_stitching_and_sentinel() {
    let mut m = Machine::default();
    let (grid, view, out) = (0x8010_0000, 0x8014_0000, 0x8015_0000);
    // Cell (x 0..2, z 0) values 0x201, 0x000 (empty), 0x7ff (sign-extended).
    m.sh(grid, 0x201);
    m.sh(grid + 2, 0);
    m.sh(grid + 4, 0x87ff);
    m.sw(view + 4, 5); // height
    m.sw(view + 0x38, 0); // tile x
    m.sw(view + 0x3c, 0); // tile z
    m.sw(view + 0x40, 0x400);
    m.sw(view + 0x44, 0x400);
    m.sw(view + 0x48, 0); // scan x
    m.sw(view + 0x4c, 0); // scan z
    m.sw(view + 0x58, 3); // columns
    m.sw(view + 0x5c, 1); // rows
    m.sw(view + 0x60, 0b111);
    let end = terrain::select_cells(&mut m, grid, view, out);
    assert_eq!(end, out + 3 * 12);
    assert_eq!(m.lw(out + 8), 0x0040_0201, "first cell flag");
    assert_eq!(m.lh(out + 2), -5);
    assert_eq!(m.lh(out + 12), 0x1000, "column 2 x");
    assert_eq!(m.lw(out + 20), 0xffff_87ff | (2 << 16) | 0x0800_0000);
    assert_eq!(m.lw(out + 32), 0x0020_0000, "sentinel");
}

#[test]
fn midpoint_masks_colour_carries() {
    let mut m = Machine::default();
    let (out, a, b) = (0x8010_0000, 0x8010_0010, 0x8010_0020);
    m.sh(a, 10);
    m.sh(b, -20_i16 as u16);
    m.sw(a + 8, 0x3cff_01ff);
    m.sw(b + 8, 0x3c01_01ff);
    m.sh(a + 6, 0x8001);
    m.sh(b + 6, 0x0003);
    terrain::midpoint(&mut m, out, a, b);
    assert_eq!(m.lh(out), -5);
    // (0x3cfe00ff + 0x3c0000ff) >> 1 (arithmetic) = 0x3c7f00ff.
    assert_eq!(m.lw(out + 8), 0x3c7f_00ff);
    // (0xffff8001 & mask) + 3 = 0xfefe8004 -> sra 1 -> 0xff7f4002.
    assert_eq!(m.lhu(out + 6), 0x4002);
}

#[test]
fn quad_too_large_bounds() {
    let xy = |x: i16, y: i16| u32::from(x as u16) | (u32::from(y as u16) << 16);
    assert_eq!(
        terrain::quad_too_large(xy(0, 0), xy(10, 0), xy(0, 10), xy(10, 10)),
        0
    );
    assert_eq!(
        terrain::quad_too_large(xy(0, 0), xy(0, 0x200), xy(0, 0), xy(0, 0)),
        1
    );
    assert_eq!(
        terrain::quad_too_large(xy(0, -0x100), xy(0, 0xff), xy(0, 0), xy(0, 0)),
        0
    );
    assert_eq!(
        terrain::quad_too_large(xy(-0x200, 0), xy(0x200, 0), xy(0, 0), xy(0, 0)),
        1
    );
    assert_eq!(
        terrain::quad_too_large(xy(0, 0), xy(0, 0), xy(-0x200, 0), xy(0x1ff, 0)),
        0
    );
}

#[test]
fn cloud_midpoint_lanes_drop_the_top_carry() {
    let mut m = Machine::default();
    let (out, a, b) = (0x8010_0000, 0x8010_0020, 0x8010_0040);
    m.sh(a, 10);
    m.sh(b, -20_i16 as u16);
    m.sw(a + 0xc, 0xff80_ff80);
    m.sw(b + 0xc, 0xff00_0101);
    m.sh(a + 0x12, 0x80ff);
    m.sh(b + 0x12, 0x8001);
    terrain::cloud_midpoint(&mut m, out, a, b);
    assert_eq!(m.lh(out), -5);
    // 0xff00ff00 + 0xff000100 wraps to 0xfe010000 before the shift.
    assert_eq!(m.lw(out + 0xc), 0x7f40_8040);
    assert_eq!(m.lhu(out + 0x12), 0x8080);
}

#[test]
fn compact_ot_runs_links_past_empty_entries() {
    let mut m = Machine::default();
    let ot = 0x8010_0000;
    m.sw(ot, 0x00ff_ffff);
    for i in 1..8_u32 {
        m.sw(ot + i * 4, (ot + (i - 1) * 4) & 0x00ff_ffff);
    }
    m.sw(ot + 0x10, 0x0012_3456); // entry 4 holds a packet
    terrain::compact_ot_runs(&mut m, ot, 8);
    assert_eq!(m.lw(ot + 0xc), 0x0010_0000, "entry 3 skips entries 2..1");
    assert_eq!(m.lw(ot + 0x1c), 0x0010_0010, "entry 7 skips entries 6..5");
    assert_eq!(m.lw(ot + 0x10), 0x0012_3456);
    // A single empty entry before a packet is left alone.
    m.sw(ot + 4, ot & 0x00ff_ffff);
    m.sw(ot + 8, 0x0012_3456);
    m.sw(ot + 0xc, (ot + 8) & 0x00ff_ffff);
    terrain::compact_ot_runs(&mut m, ot, 4);
    assert_eq!(m.lw(ot + 4), 0x0010_0000);
    assert_eq!(m.lw(ot + 0xc), 0x0010_0008);
}
