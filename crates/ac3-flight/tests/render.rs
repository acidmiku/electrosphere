//! Camera and object rendering (`render.rs`) on original function
//! snapshots: every translated routine runs on the captured entry state and
//! must reproduce the captured return state exactly: all of RAM except the
//! routine's own dead stack frames (below the entry `$sp`), the scratchpad,
//! every GTE register, and `v0` where the routine returns one. The
//! primitive packets and ordering-table links in RAM are the renderers'
//! output, so RAM equality proves them. Unlike `snapshot::check_call`, the
//! caller's frames above the entry `$sp` are compared too: they hold the
//! display lists that the list builders return, and 8006AE1C's 0x3098-byte
//! frame reaches below the generic stack bound.
//!
//! Captures (normal input from `mission-006/final.sav`, weapons fired from
//! frame 330):
//! - `render-002`: the first three flight callbacks (frames 307..311),
//!   three calls of every routine;
//! - `render-003`: callbacks 80..82 (frame 467 on), with gunfire and
//!   missiles in flight, two calls of every routine;
//! - `render-005`: the textured-face leaves and trail renderers in the same
//!   frames;
//! - `render-006`: callbacks 450..452 (frame 1207 on, periodic gunfire and
//!   missiles), two calls of every routine;
//! - `render-008`: callbacks 400..402 (frame 1107 on, same inputs as the
//!   6000-frame call-tree probe `render-007`): flak and smoke objects,
//!   projectile trails and the external camera;
//! - `render-011`, `render-012`: calls 700..702 and 1050..1052 of the frame
//!   body 80033748 (frames 1707 and 2407 on, after the mission changes the
//!   frame callback): enemy aircraft in the lit lists, missiles and flak,
//!   and the mission-end camera.
//!
//! Routines that reach a still-untranslated shared routine (owned by the
//! `shared` agent) list it as an accepted blocker: such an occurrence is
//! reported as BLOCKED instead of failing, and becomes a full comparison
//! as soon as the shared translation lands. 8006AE1C's original 0x3098-byte
//! frame reaches 0x801FCEE8; `machine::STACK_LIMIT` is 0x801FC000 for it
//! (the deep-frame allowance below only applies to an older, higher
//! limit). Any other fault or difference fails.
mod support;
use support::oracle::{self, Rng};

use ac3_flight::SmallData;
use ac3_flight::{
    Fault, Machine,
    machine::{RAM_BYTES, SCRATCH_BYTES},
    render,
    snapshot::{self, Snapshot},
};
use std::path::{Path, PathBuf};

fn index(capture: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR"))
        .join("analysis/flight-runtime")
        .join(capture)
        .join("snapshots.jsonl")
}

const EARLY: (&str, usize) = ("render-002", 3);
const WEAPONS: (&str, usize) = ("render-003", 2);
const LEAVES: (&str, usize) = ("render-005", 2);
const LATE: (&str, usize) = ("render-006", 2);
const OBJECTS: (&str, usize) = ("render-008", 2);
/// The captures in the cockpit view (camera mode 1).
const FLIGHT: &[(&str, usize)] = &[EARLY, WEAPONS, LATE];
const LIT: (&str, usize) = ("render-011", 2);
const PROJECTILES: (&str, usize) = ("render-012", 2);
/// Frames 1707..2707 bracketing only calls with work to do: non-empty face
/// lists, the overlay toggles (R2 at frame 1760) and the projectile leaves.
const FACES: (&str, usize) = ("render-013", 2);
/// Calls that changed the machine (any byte below the stack, the scratchpad
/// or a GTE register), frames 307..2907: debris pieces at frames 979/981.
const DEBRIS: (&str, usize) = ("render-014", 2);
/// Changed-state calls during hard pulls (frames 307..1507): ribbon trails
/// at frames 371/373 and debris at 1109/1111.
const MANOEUVRE: (&str, usize) = ("render-015", 2);
/// Changed-state calls of the look camera (mode 2, entered with SELECT at
/// frame 545): frames 555/557.
const LOOK: (&str, usize) = ("render-016", 2);
/// Changed-state calls during the replay (frames 1687..4687): 8007998C,
/// 800788BC and 800797D0 (the other scripts never ran).
const REPLAY: (&str, usize) = ("render-018", 2);
/// The noise overlay switched on with L2 at frame 1860 (inputs as
/// render-013): 80083244 and 8008E73C with overlay bit 1 set.
const NOISE: (&str, usize) = ("render-019", 2);
/// Replay shots cut by buttons (R1 at 1850, SQUARE at 2450): 80078EBC and
/// its helpers, 80078D44, 8007905C.
const SHOTS: (&str, usize) = ("render-020", 2);
/// Replay shots cut by L1 (1750: 80078A80) and CROSS (1850: 80078BF0).
const SHOTS_NEAR: (&str, usize) = ("render-021", 2);
/// The captures up to the external camera (frames 307..1110).
const ALL: &[(&str, usize)] = &[EARLY, WEAPONS, LATE, OBJECTS];
/// Every capture: the object passes run in all of them.
const WIDE: &[(&str, usize)] = &[EARLY, WEAPONS, LATE, OBJECTS, LIT, PROJECTILES];

/// Number of occurrences in `captures`.
fn total(captures: &[(&str, usize)]) -> usize {
    captures.iter().map(|c| c.1).sum()
}

/// Lowest address the resident stack reaches in these routines (8006AE1C's
/// callees run at 0x801FCEE8); everything from here up to the entry `$sp`
/// is dead after the return.
const STACK_FLOOR: usize = 0x1f_c000;

/// The callee's frame below an entry `$sp` on the scratchpad stack of
/// 8008589C (80084BB0 and 8008529C, 0x30 bytes each): it holds only the
/// caller's spilled s0..s5 and `ra`, which 8008589C's translation writes
/// before each call and a callee run on its own cannot know.
fn scratch_spills(sp: u32) -> std::ops::Range<usize> {
    let scratch = 0x1f80_0000..0x1f80_0400;
    if scratch.contains(&sp) {
        (sp - 0x30 - scratch.start) as usize..(sp - scratch.start) as usize
    } else {
        0..0
    }
}

/// Run `native` on the `occurrence`th entry snapshot of `callee` and compare
/// with its return snapshot. Returns the number of bytes and GTE registers
/// the original changed, or a description of the first differences.
fn compare_call(
    index_file: &Path,
    callee: u32,
    occurrence: usize,
    native: impl FnOnce(&mut Machine, &Snapshot) -> Option<u32>,
) -> Result<usize, String> {
    let directory = index_file.parent().ok_or("index directory")?;
    let index = snapshot::read_index(index_file).map_err(|e| e.to_string())?;
    let entry = index
        .iter()
        .filter(|s| s.callee == callee && s.stage == "entry")
        .nth(occurrence)
        .ok_or_else(|| format!("{callee:08x}: no entry snapshot #{occurrence}"))?;
    let exit = index
        .iter()
        .find(|s| {
            s.callee == callee
                && s.stage == "return"
                && s.sequence > entry.sequence
                && s.ra == entry.ra
                && s.registers.gpr[29] == entry.registers.gpr[29]
        })
        .ok_or_else(|| format!("{callee:08x}: no return snapshot"))?;
    let entry = Snapshot::load(directory, entry).map_err(|e| e.to_string())?;
    let exit = Snapshot::load(directory, exit).map_err(|e| e.to_string())?;
    let mut m = entry.machine();
    let v0 = native(&mut m, &entry);
    match m.take_fault() {
        Ok(()) => {}
        // A push below `machine::STACK_LIMIT`: the original's frames really
        // reach there (8006AE1C), and the frame is ordinary RAM, so the run
        // continues correctly; compare the state anyway (`check` reports it).
        Err(fault) if deep_frame(&fault) => {}
        Err(fault) => return Err(format!("{callee:08x}: {fault}")),
    }
    let dead = STACK_FLOOR..(entry.gpr(29) as usize & (RAM_BYTES - 1));
    let spills = scratch_spills(entry.gpr(29));
    let ram: Vec<usize> = (0..RAM_BYTES)
        .filter(|i| !dead.contains(i) && m.ram()[*i] != exit.ram[*i])
        .collect();
    let scratch: Vec<usize> = (0..SCRATCH_BYTES)
        .filter(|&i| !spills.contains(&i) && m.scratch()[i] != exit.scratch[i])
        .collect();
    let data: Vec<usize> = (0..32)
        .filter(|&i| m.gte.data[i] != exit.info.registers.cp2d[i])
        .collect();
    let control: Vec<usize> = (0..32)
        .filter(|&i| m.gte.control[i] != exit.info.registers.cp2c[i])
        .collect();
    if !(ram.is_empty() && scratch.is_empty() && data.is_empty() && control.is_empty()) {
        let hex = |v: &[usize], base: u32| {
            v.iter()
                .take(12)
                .map(|&a| format!("{:08x}", base + a as u32))
                .collect::<Vec<_>>()
                .join(" ")
        };
        return Err(format!(
            "{callee:08x}: ram {} [{}] scratch {} [{}] gte data {data:?} control {control:?}",
            ram.len(),
            hex(&ram, 0x8000_0000),
            scratch.len(),
            hex(&scratch, 0x1f80_0000)
        ));
    }
    if let Some(v0) = v0
        && v0 != exit.gpr(2)
    {
        return Err(format!(
            "{callee:08x}: v0 {v0:#010x}, original {:#010x}",
            exit.gpr(2)
        ));
    }
    let changed = (0..RAM_BYTES)
        .filter(|i| !dead.contains(i) && entry.ram[*i] != exit.ram[*i])
        .count()
        + (0..SCRATCH_BYTES)
            .filter(|&i| !spills.contains(&i) && entry.scratch[i] != exit.scratch[i])
            .count()
        + (0..32)
            .filter(|&i| entry.info.registers.cp2d[i] != exit.info.registers.cp2d[i])
            .count()
        + (0..32)
            .filter(|&i| entry.info.registers.cp2c[i] != exit.info.registers.cp2c[i])
            .count();
    Ok(changed)
}

/// Run `callee` through `render::dispatch` with its original arguments on
/// every listed snapshot occurrence. Returns (passed, changed, blocked).
fn check(
    callee: u32,
    captures: &[(&str, usize)],
    returns_value: bool,
    blockers: &[Fault],
) -> (usize, usize, usize) {
    let mut passed = 0;
    let mut changed = 0;
    let mut blocked = 0;
    for &(capture, count) in captures {
        for occurrence in 0..count {
            let mut fault = None;
            let result = compare_call(&index(capture), callee, occurrence, |m, entry| {
                let args = [entry.gpr(4), entry.gpr(5), entry.gpr(6), entry.gpr(7)];
                let v0 = render::dispatch(m, callee, args).expect("claimed by render::dispatch");
                fault = m.fault();
                returns_value.then_some(v0)
            });
            match result {
                Ok(count) => {
                    passed += 1;
                    changed += count;
                    let note = match &fault {
                        Some(fault) if deep_frame(fault) => " (frame below machine::STACK_LIMIT)",
                        _ => "",
                    };
                    eprintln!("{callee:08x} {capture} #{occurrence}: exact, {count} changed{note}");
                }
                Err(error) => match fault {
                    Some(fault) if blockers.contains(&fault) => {
                        blocked += 1;
                        eprintln!("{callee:08x} {capture} #{occurrence}: BLOCKED: {fault}");
                    }
                    _ => panic!("{capture} #{occurrence}: {error}"),
                },
            }
        }
    }
    (passed, changed, blocked)
}

/// Require every occurrence to pass and the comparisons to be non-vacuous.
fn exact(callee: u32, captures: &[(&str, usize)], returns_value: bool) {
    let expected: usize = captures.iter().map(|c| c.1).sum();
    let (passed, changed, _) = check(callee, captures, returns_value, &[]);
    assert_eq!(passed, expected);
    assert!(
        changed > 0 || returns_value,
        "{callee:08x}: nothing changed"
    );
}

/// A still-untranslated routine owned elsewhere (an SDK routine).
const fn untranslated(address: u32) -> Fault {
    Fault::Untranslated { address }
}

/// A frame pushed below `machine::STACK_LIMIT` (0x801FE000) but within the
/// stack the original uses (8006AE1C's 0x3098-byte frame reaches 0x801FCEE8).
/// Ordinary loads and stores there do not fault, so only `push_frame` raises
/// this.
fn deep_frame(fault: &Fault) -> bool {
    matches!(fault, Fault::Address { address, .. } if (0x801f_c000..0x801f_e000).contains(address))
}

// ---- 80033194: entity model pass ------------------------------------------------------

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn list_entities_80065974() {
    exact(0x8006_5974, WIDE, true);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn list_objects_80065bb0() {
    exact(0x8006_5bb0, WIDE, true);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn build_display_lists_80065cc4() {
    exact(0x8006_5cc4, WIDE, false);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn prepare_lists_800654b0_800655fc_800658b8() {
    // The captured lists need no morphing: these calls only scan.
    for callee in [0x8006_54b0, 0x8006_55fc, 0x8006_58b8] {
        let (passed, _, _) = check(callee, WIDE, false, &[]);
        assert_eq!(passed, total(WIDE));
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn draw_plain_list_80060f14() {
    // Sets the far colour; the captured plain lists are empty.
    exact(0x8006_0f14, WIDE, false);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn draw_textured_list_80061408() {
    exact(0x8006_1408, WIDE, false);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn draw_textured_faces_8006159c_800616fc() {
    // The captured textured records (missiles) carry no faces, so these
    // calls only read the empty face lists; the face paths are covered by
    // the unit tests below.
    for callee in [0x8006_159c, 0x8006_16fc] {
        let (passed, _, _) = check(callee, &[WEAPONS, LEAVES, LATE], false, &[]);
        assert_eq!(passed, 6, "{callee:08x}");
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn draw_glow_sprite_8008bcbc() {
    exact(0x8008_bcbc, &[WEAPONS, LEAVES, LATE], false);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn draw_lit_list_80061fe4() {
    // The captured lit lists are empty: the call only tests the terminator.
    let (passed, _, _) = check(0x8006_1fe4, WIDE, false, &[]);
    assert_eq!(passed, total(WIDE));
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn entity_model_pass_80066b64() {
    exact(0x8006_6b64, WIDE, true);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn draw_entity_models_80033194() {
    exact(0x8003_3194, WIDE, true);
}

#[test]
fn blend_vertices_interpolates_each_coordinate() {
    // 800652CC on one group of three vertices: factor 0x800 (one half).
    let mut m = Machine::default();
    let (from, to, out) = (0x8010_0000, 0x8010_0100, 0x8010_0200);
    m.sw(from, 3);
    m.sw(to, 3);
    let a = [(0i16, 0i16, 0i16), (100, -100, 40), (-8, 8, 2)];
    let b = [(200i16, 100i16, -40i16), (100, 100, 40), (8, -8, 6)];
    for (base, vertices) in [(from, a), (to, b)] {
        for (i, &(x, y, _)) in vertices.iter().enumerate() {
            m.sh(base + 4 + i as u32 * 4, x as u16);
            m.sh(base + 6 + i as u32 * 4, y as u16);
        }
        m.sh(base + 16, vertices[0].2 as u16);
        m.sh(base + 18, vertices[1].2 as u16);
        m.sh(base + 20, vertices[2].2 as u16);
    }
    let end = render::dispatch(&mut m, 0x8006_52cc, [0x800, from, to, out]).unwrap();
    assert_eq!(end, out + 24);
    assert_eq!(m.lw(out), 3);
    let expected = [(100i16, 50i16, -20i16), (100, 0, 40), (0, 0, 4)];
    for (i, &(x, y, z)) in expected.iter().enumerate() {
        assert_eq!(m.lh(out + 4 + i as u32 * 4), x, "x{i}");
        assert_eq!(m.lh(out + 6 + i as u32 * 4), y, "y{i}");
        assert_eq!(m.lh(out + 16 + i as u32 * 2), z, "z{i}");
    }
    assert!(m.take_fault().is_ok());
}

// ---- 80033140: effect segments -----------------------------------------------------------

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn sort_effect_segments_8006aa3c() {
    exact(0x8006_aa3c, WIDE, false);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn draw_effect_lists_8008f204_8008f388_8008f660_8008f940_8008fb00() {
    for callee in [
        0x8008_f204,
        0x8008_f388,
        0x8008_f660,
        0x8008_f940,
        0x8008_fb00,
    ] {
        exact(callee, WIDE, true);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn effect_pass_8006ae1c() {
    // Exact, but the 0x3098-byte frame reaches below machine::STACK_LIMIT.
    exact(0x8006_ae1c, WIDE, true);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn draw_effects_80033140() {
    // Exact, but 8006AE1C's frame reaches below machine::STACK_LIMIT.
    exact(0x8003_3140, WIDE, true);
}

// ---- 8003308C: trails and object systems ----------------------------------------------------

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn draw_smoke_trails_8007a990() {
    exact(0x8007_a990, WIDE, true);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn draw_ribbon_trails_8007b928() {
    exact(0x8007_b928, WIDE, true);
    exact(0x8007_b928, &[MANOEUVRE], false);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn draw_debris_800898d8() {
    // The shock rings (80088EAC) need the SDK's rsin (800A45C8),
    // ScaleMatrix (800A53D8) and SetMulMatrix (800A4ED8), not in sdk.rs yet.
    let captures = &[DEBRIS, MANOEUVRE];
    let sdk = [
        untranslated(0x800a_45c8),
        untranslated(0x800a_53d8),
        untranslated(0x800a_4ed8),
    ];
    let (passed, _, blocked) = check(0x8008_98d8, captures, false, &sdk);
    assert_eq!(passed + blocked, total(captures));
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn draw_trail_systems_8007c484() {
    exact(0x8007_c484, WIDE, true);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn object_system_parts() {
    for callee in [
        0x8008_a310u32,
        0x8008_9ba0,
        0x8008_c1c8,
        0x8008_3d48,
        0x8008_589c,
        0x8008_98d8,
        0x8008_ebdc,
        0x8009_0fe0,
    ] {
        let (passed, _, _) = check(callee, WIDE, false, &[]);
        assert_eq!(passed, total(WIDE), "{callee:08x}");
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn draw_trail_quad_8008c5a8() {
    let captures = &[EARLY, WEAPONS, LATE, OBJECTS, LIT];
    exact(0x8008_c5a8, captures, false);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn draw_ambient_particles_8008c7c8() {
    exact(0x8008_c7c8, WIDE, false);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn draw_object_systems_80083b78() {
    exact(0x8008_3b78, WIDE, true);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn draw_lit_systems_8003308c() {
    exact(0x8003_308c, WIDE, true);
}

// ---- 80079DA4 camera update, 8003304C view matrix --------------------------------------------

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn view_angles_80031164() {
    exact(0x8003_1164, FLIGHT, false);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn look_toward_80076358() {
    exact(0x8007_6358, FLIGHT, false);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn camera_update_800763c0_80079da4() {
    exact(0x8007_63c0, FLIGHT, false);
    exact(0x8007_9da4, ALL, false);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn view_matrix_8003304c() {
    exact(0x8003_304c, ALL, true);
}

#[test]
fn vector_length_matches_the_square_root_table() {
    // 800828F8 (owned by player.rs): |(3000, 4000, 0)| = 5000 within the
    // table's precision; a zero vector returns 0.
    let mut m = Machine::default();
    let table = 0x800b_4c54u32;
    // A synthetic table: entry i = sqrt(i + 64) * 4096 / 8 (the layout the
    // routine indexes), enough for this check.
    for i in 0..192u32 {
        let root = (f64::from(i + 64)).sqrt() * 512.0;
        m.sh(table + i * 2, root as u16);
    }
    assert_eq!(
        ac3_flight::dispatch::call(&mut m, 0x8008_28f8, [0, 0, 0, 0]),
        0
    );
    let length = ac3_flight::dispatch::call(&mut m, 0x8008_28f8, [3000, 4000, 0, 0]);
    assert!((4990..=5010).contains(&length), "{length}");
}

// ---- face renderers on synthetic meshes ---------------------------------------------------------

/// A work area like the scratchpad one: OT, packet cursor, face list,
/// depth shift, screen XY at +16 and Z at +676.
struct Faces {
    m: Machine,
    work: u32,
    ot: u32,
    packets: u32,
}

impl Faces {
    fn new(points: &[(i16, i16, i16)]) -> Self {
        let mut m = Machine::default();
        let (work, ot, packets) = (0x8010_0000u32, 0x8011_0000u32, 0x8012_0000u32);
        for slot in 0..512u32 {
            m.sw(ot + slot * 4, 0x00ff_ffff);
        }
        m.sw(work, ot);
        m.sw(work + 4, packets);
        m.sw(work + 12, 0);
        for (i, &(x, y, z)) in points.iter().enumerate() {
            let i = i as u32;
            m.sh(work + 16 + i * 4, x as u16);
            m.sh(work + 18 + i * 4, y as u16);
            m.sh(work + 676 + i * 2, z as u16);
        }
        // ZSF3 = 4096 / 3 and ZSF4 = 4096 / 4, as libgte sets them.
        m.gte.control[29] = 0x155;
        m.gte.control[30] = 0x100;
        Self {
            m,
            work,
            ot,
            packets,
        }
    }
    /// Face list at 0x80130000: count, then each face's words from +4.
    fn faces(&mut self, faces: &[&[u32]]) {
        let list = 0x8013_0000u32;
        self.m.sw(list, faces.len() as u32);
        let stride = faces.first().map_or(0, |f| f.len() as u32 * 4);
        for (i, face) in faces.iter().enumerate() {
            for (j, &word) in face.iter().enumerate() {
                self.m.sw(list + 4 + i as u32 * stride + j as u32 * 4, word);
            }
        }
        self.m.sw(self.work + 8, list);
    }
}

#[test]
fn textured_triangles_cull_sort_and_pack() {
    // 8006159C: one front-facing and one back-facing triangle at depth 100.
    let mut f = Faces::new(&[(0, 0, 100), (10, 0, 100), (0, 10, 100)]);
    let front = [0, 0x0002_0100, 0x3400_0080, 0x16, 0x26, 0x14, 0x24, 0x28];
    let back = [0, 0x0001_0200, 0x3400_0080, 0x16, 0x26, 0x14, 0x24, 0x28];
    f.faces(&[&front, &back]);
    render::dispatch(&mut f.m, 0x8006_159c, [f.work, 0, 0, 0]);
    let m = &mut f.m;
    let packet = f.packets;
    // AVSZ3: 0x155 * 300 = 102300, >> 10 = 99: slot 99 >> 3 = 12.
    let slot = f.ot + 12 * 4;
    assert_eq!(m.lw(slot), packet & 0x00ff_ffff);
    assert_eq!(m.lw(packet), 0x09ff_ffff);
    let expected = [
        (4, 0x3400_0080),
        (8, 0),
        (12, 0x14),
        (16, 0x16),
        (20, 10),
        (24, 0x24),
        (28, 0x26),
        (32, 10 << 16),
        (36, 0x28),
    ];
    for (offset, word) in expected {
        assert_eq!(m.lw(packet + offset), word, "+{offset}");
    }
    assert_eq!(m.lw(f.work + 4), packet + 40, "one packet");
    assert!(m.take_fault().is_ok());
}

#[test]
fn textured_quads_use_either_half() {
    // 800616FC: a quad whose first half faces away but whose second half
    // (vertex 3 replacing vertex 0) faces the camera is still drawn.
    let mut f = Faces::new(&[(0, 10, 200), (10, 0, 200), (0, 0, 200), (10, 10, 200)]);
    let quad = [
        0,
        0x0302_0100,
        0x3c00_0080,
        0x11,
        0x12,
        0x13,
        0x14,
        0x15,
        0x0016_0017,
    ];
    f.faces(&[&quad]);
    render::dispatch(&mut f.m, 0x8006_16fc, [f.work, 0, 0, 0]);
    let m = &mut f.m;
    let packet = f.packets;
    // Average Z 200, >> 3 = slot 25.
    let slot = f.ot + 25 * 4;
    assert_eq!(m.lw(slot), packet & 0x00ff_ffff);
    assert_eq!(m.lw(packet), 0x0cff_ffff);
    let expected = [
        (4, 0x3c00_0080),
        (8, 10 << 16),
        (12, 0x14),
        (16, 0x11),
        (20, 10),
        (24, 0x15),
        (28, 0x12),
        (32, 0),
        (36, 0x0016_0017),
        (40, 0x13),
        (44, (10 << 16) | 10),
        (48, 0x16),
    ];
    for (offset, word) in expected {
        assert_eq!(m.lw(packet + offset), word, "+{offset}");
    }
    assert_eq!(m.lw(f.work + 4), packet + 52);
    assert!(m.take_fault().is_ok());
}

// ---- later mission phases: aircraft, objects, projectiles -----------------------------------

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn lit_record_faces_80061890_80061a2c_80061c0c_80061dc0() {
    // No non-empty triangle list reached 80061890 in frames 307..2707: its
    // captured calls draw nothing, so only their exactness is checked.
    let (passed, _, _) = check(0x8006_1890, &[LIT], false, &[]);
    assert_eq!(passed, 2);
    for callee in [0x8006_1a2c, 0x8006_1c0c, 0x8006_1dc0] {
        exact(callee, &[LIT, FACES], false);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn object_sprites_80084324_8008451c_800841ac() {
    for callee in [0x8008_4324, 0x8008_451c, 0x8008_41ac] {
        exact(callee, &[OBJECTS, PROJECTILES], false);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn projectile_matrices_80085074_800851d8() {
    for callee in [0x8008_5074, 0x8008_51d8] {
        exact(callee, &[OBJECTS, PROJECTILES], false);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn projectiles_80084bb0_8008529c() {
    exact(0x8008_4bb0, &[PROJECTILES, FACES], false);
    exact(0x8008_529c, &[OBJECTS, PROJECTILES, FACES], false);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn lock_light_8008e9a0() {
    // No captured call (and none of the changed-state calls looked for in
    // render-014, frames 307..2907) had a lock target in an external view:
    // these calls leave the machine unchanged, so only exactness is checked.
    let captures = &[OBJECTS, LIT, PROJECTILES];
    let (passed, _, _) = check(0x8008_e9a0, captures, false, &[]);
    assert_eq!(passed, total(captures));
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn overlays_8008e73c() {
    // render-013 #0/#1: R2 toggles the full-screen blend (8008E0E0) on.
    exact(0x8008_e73c, &[LIT, PROJECTILES, FACES], false);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn orbit_camera_80077700() {
    exact(0x8007_7700, &[OBJECTS], false);
}

// ---- camera mode 512: the replay camera ------------------------------------------------------

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn replay_camera_8007998c_8007700c() {
    exact(0x8007_998c, &[LIT, PROJECTILES], false);
    exact(0x8007_700c, &[LIT, PROJECTILES], false);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn replay_scripts_80078d44_8007905c() {
    exact(0x8007_8d44, &[LIT], true);
    exact(0x8007_905c, &[PROJECTILES], true);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn ground_clearance_80078640_800784ac() {
    exact(0x8007_8640, &[LIT, PROJECTILES], true);
    exact(0x8007_84ac, &[LIT, PROJECTILES], true);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn camera_update_all_phases_80079da4() {
    exact(0x8007_9da4, WIDE, false);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn look_camera_80076760() {
    exact(0x8007_6760, &[LOOK], false);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn replay_helpers_800788bc_800797d0() {
    exact(0x8007_998c, &[REPLAY], false);
    exact(0x8007_88bc, &[REPLAY], false);
    exact(0x8007_97d0, &[REPLAY], false);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn noise_overlay_80083244() {
    exact(0x8008_3244, &[NOISE], true);
    exact(0x8008_e73c, &[NOISE], false);
}

#[test]
fn camera_box_clamps_each_axis() {
    // 8007868C: low/high bounds per word axis.
    let mut m = Machine::default();
    let (low, high, position) = (0x8010_0000u32, 0x8010_0010u32, 0x8010_0020u32);
    let axes = [(-10i32, 10i32, -20i32), (0, 100, 50), (5, 6, 7)];
    for (i, (l, h, v)) in (0u32..).zip(axes) {
        m.sw(low + i * 4, l as u32);
        m.sw(high + i * 4, h as u32);
        m.sw(position + i * 4, v as u32);
    }
    render::dispatch(&mut m, 0x8007_868c, [low, high, position, 0]);
    assert_eq!(m.lw(position) as i32, -10);
    assert_eq!(m.lw(position + 4), 50);
    assert_eq!(m.lw(position + 8), 6);
    assert!(m.take_fault().is_ok());
}

#[test]
fn replay_overhead_shot_traps_on_a_zero_duration() {
    // 80078D44 divides by the shot duration: the compiler's `break 7` after
    // the `div` at 80078D94 is reported as a trap.
    let mut m = Machine::default();
    let (shot, state) = (0x8010_0000u32, 0x8010_0100u32);
    m.sw(state, 1);
    m.sw(state + 0x20, 0);
    render::dispatch(&mut m, 0x8007_8d44, [shot, state, 0, 0]);
    assert_eq!(
        m.take_fault(),
        Err(Fault::Trap {
            address: 0x8007_8da0,
            code: 7
        })
    );
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-0*"]
fn replay_shots_80078a80_80078bf0_80078ebc() {
    for callee in [0x8007_8ebc, 0x8007_8d44, 0x8007_905c, 0x8007_8478] {
        exact(callee, &[SHOTS], true);
    }
    for callee in [0x8007_8364, 0x8003_1a94, 0x8007_89f0] {
        exact(callee, &[SHOTS], false);
    }
    exact(0x8007_8a80, &[SHOTS_NEAR], true);
    exact(0x8007_8bf0, &[SHOTS_NEAR], true);
    exact(0x8007_82ec, &[SHOTS_NEAR], false);
}

// ==== phase 3: original-code oracle cases ===================================================
//
// No capture reaches the routines below (other camera modes, the loading
// visuals, the lens flare, heat shimmer and explosion trails), so each one
// is validated with the test-only oracle (`support::oracle`): the original
// instructions of the routine run from a captured machine state with
// randomised inputs, the translation runs from a clone of the same state,
// and all of RAM outside the dead stack, the scratchpad, the GTE, `sp`,
// the first fault and `v0` (where returned) must agree. Callees run
// natively in both runs, so only the routine under test differs; each
// callee is validated by its own cases. Every test prints its case count
// and the bytes the original changed, and requires both to be nonzero.

/// Free RAM (above the loaded overlays, below the stack) for crafted
/// records, end points and primitive buffers.
const CRAFT: u32 = 0x801c_0000;

/// The oracle comparison of `range` (original) with the translation
/// claimed at `range.0` (through `dispatch::call`); `v0` is compared when
/// `value`. Returns the RAM bytes the original changed.
fn oracle_case(
    label: &str,
    base: &Machine,
    range: (u32, u32),
    args: [u32; 4],
    value: bool,
) -> usize {
    oracle::diff(label, base, range, args, |m| {
        let v0 = ac3_flight::dispatch::call(m, range.0, args);
        value.then_some(v0)
    })
}

/// The machines at every entry of `callee` in `capture` with their
/// `a0..a3`.
fn oracle_bases(capture: &str, callee: u32) -> Vec<(Machine, [u32; 4])> {
    let bases: Vec<_> = oracle::entries(capture, callee)
        .iter()
        .map(|s| (s.machine(), [s.gpr(4), s.gpr(5), s.gpr(6), s.gpr(7)]))
        .collect();
    assert!(!bases.is_empty(), "no {callee:08x} entries in {capture}");
    bases
}

/// A uniformly random word.
fn word(rng: &mut Rng) -> u32 {
    (rng.next() << 8) ^ rng.next()
}

/// A random value in `-range..range`.
fn signed(rng: &mut Rng, range: u32) -> u32 {
    (word(rng) % (2 * range)).wrapping_sub(range)
}

/// Entity pointer `index` of `[gp+0x2D8]`.
fn entity_pointer(m: &mut Machine, index: u32) -> u32 {
    let table = m.gw(0x2d8);
    m.lw(table + index * 4)
}

/// Set or clear `bits` of the word at `address`.
fn set_bits(m: &mut Machine, address: u32, bits: u32, on: bool) {
    let value = m.lw(address);
    m.sw(address, if on { value | bits } else { value & !bits });
}

/// Run `cases` oracle cases per base: `setup` randomises the base and
/// returns the arguments.
fn oracle_cases(
    name: &str,
    bases: &[(Machine, [u32; 4])],
    range: (u32, u32),
    cases: usize,
    value: bool,
    mut setup: impl FnMut(&mut Machine, [u32; 4], &mut Rng) -> [u32; 4],
) {
    let mut rng = Rng(range.0 ^ cases as u32);
    let (mut total, mut changed) = (0, 0);
    for (b, (base, entry)) in bases.iter().enumerate() {
        for case in 0..cases {
            let mut m = base.clone();
            let args = setup(&mut m, *entry, &mut rng);
            changed += oracle_case(
                &format!("{name} base {b} case {case}"),
                &m,
                range,
                args,
                value,
            );
            total += 1;
        }
    }
    println!("{name}: {total} oracle cases exact, original changed {changed} bytes");
    // A leaf that only returns a value (800907A4) changes no RAM: its
    // cases compare `v0`.
    assert!(total > 0 && (changed > 0 || value), "{name}: vacuous");
}

/// The camera bases: 80079DA4 entries (cockpit camera) and 80076760
/// entries (look camera), with the camera pointer as `a0`.
fn camera_bases() -> Vec<(Machine, [u32; 4])> {
    let mut bases = oracle_bases(EARLY.0, 0x8007_9da4);
    bases.extend(oracle_bases(LOOK.0, 0x8007_6760));
    bases
}

/// Randomise the orbit and chase fields of `camera` (angles, timers,
/// distance, offsets, target object, context pause bit).
fn randomise_camera(m: &mut Machine, camera: u32, rng: &mut Rng) {
    for offset in [0xb4u32, 0xb6, 0xb8, 0xd4, 0xd6, 0xd8, 0x14, 0x16, 0x18] {
        let angle = match rng.next() % 4 {
            0 => signed(rng, 0x80),
            1 => [0x1000u32, 0xf000, 0x2000, 0xe000, 0x1fff, 0x2001][rng.next() as usize % 6],
            _ => signed(rng, 0x2800),
        };
        m.sh(camera + offset, angle as u16);
    }
    for offset in [0x2cu32, 0x2e, 0x30] {
        m.sh(camera + offset, signed(rng, 0x1000) as u16);
    }
    let timer = match rng.next() % 4 {
        0 => 0,
        1 => rng.next() % 8,
        2 => rng.next() % 64,
        _ => word(rng),
    };
    m.sw(camera + 0xd0, timer);
    let distance = match rng.next() % 4 {
        0 => signed(rng, 0x1_0000),
        1 => 0x31_0000 + rng.next() % 0x2_0000,
        2 => 0xae_0000 + rng.next() % 0x2_0000,
        _ => rng.next() % 0x400_0000,
    };
    m.sw(camera + 0xe4, distance);
    let target = entity_pointer(m, rng.next() % 8);
    m.sw(camera + 0x34, target);
    set_bits(m, camera, 0x100, rng.next().is_multiple_of(2));
    let context = m.gw(0x2dc);
    set_bits(m, context, 0x2000_0000, rng.next().is_multiple_of(8));
    let gameplay = m.gw(0x2d0);
    m.sw(gameplay + 0xc, word(rng));
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the render captures"]
fn camera_modes_oracle_800772ec_800774b8_8007796c_80077b78() {
    let bases = camera_bases();
    for (name, start, end) in [
        ("sway 800772EC", 0x8007_72ec, 0x8007_74b8),
        ("pull away 800774B8", 0x8007_74b8, 0x8007_7700),
        ("pull back 8007796C", 0x8007_796c, 0x8007_7b78),
        ("swing 80077B78", 0x8007_7b78, 0x8007_7d54),
    ] {
        oracle_cases(name, &bases, (start, end), 60, false, |m, entry, rng| {
            randomise_camera(m, entry[0], rng);
            [entry[0], 0, 0, 0]
        });
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the render captures"]
fn chase_camera_oracle_80076bc4() {
    oracle_cases(
        "chase 80076BC4",
        &camera_bases(),
        (0x8007_6bc4, 0x8007_700c),
        80,
        false,
        |m, entry, rng| {
            let camera = entry[0];
            randomise_camera(m, camera, rng);
            let context = m.gw(0x2dc);
            m.sw(context + 0x138, rng.next() & 0x30);
            m.sw(context + 0x14c, signed(rng, 0x800));
            m.sw(context + 0x150, signed(rng, 0x800));
            m.sw(context + 0x128, word(rng) & 0x1f_0000);
            for offset in [0x12cu32, 0x134] {
                set_bits(m, context + offset, 0x10_0000, rng.next().is_multiple_of(4));
            }
            let target = entity_pointer(m, rng.next() % 8);
            [camera, target, 0, 0]
        },
    );
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the render captures"]
fn debug_cameras_oracle_80075994_80075edc_80075df0_80027350() {
    let bases = camera_bases();
    oracle_cases(
        "free camera 80075994",
        &bases,
        (0x8007_5994, 0x8007_5df0),
        80,
        false,
        |m, entry, rng| {
            let camera = entry[0];
            randomise_camera(m, camera, rng);
            let buttons = match rng.next() % 3 {
                0 => rng.next() & 0xffff,
                1 => (rng.next() & 0xfff0) | 4,
                _ => [
                    0xc00u32, 0x6, 0x1004, 0x4004, 0x8004, 0x2004, 0x404, 0x804, 0x14, 0x44,
                ][rng.next() as usize % 10],
            };
            [camera + 4, camera + 0x64, camera + 0x54, buttons]
        },
    );
    oracle_cases(
        "orbit camera 80075EDC",
        &bases,
        (0x8007_5edc, 0x8007_60f8),
        80,
        false,
        |m, entry, rng| {
            randomise_camera(m, entry[0], rng);
            [entry[0], rng.next() & 0xffff, 0, 0]
        },
    );
    oracle_cases(
        "camera readout 80075DF0",
        &bases,
        (0x8007_5df0, 0x8007_5edc),
        6,
        false,
        |m, entry, rng| {
            randomise_camera(m, entry[0], rng);
            for offset in [0u32, 4, 8, 0xc, 0x50] {
                m.sw(entry[0] + offset, word(rng));
            }
            [entry[0], 0, 0, 0]
        },
    );
    oracle_cases(
        "hex text 80027350",
        &bases,
        (0x8002_7350, 0x8002_7408),
        20,
        true,
        |_, _, rng| {
            [
                rng.next() % 0x140,
                rng.next() % 0x100,
                word(rng),
                rng.next() % 12,
            ]
        },
    );
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the render captures"]
fn camera_update_new_modes_oracle_80079da4() {
    const MODES: [u32; 7] = [4, 16, 64, 256, 2048, 0x2000_0000, 0x4000_0000];
    oracle_cases(
        "camera update 80079DA4 (new modes)",
        &camera_bases(),
        (0x8007_9da4, 0x8007_a60c),
        42,
        false,
        |m, entry, rng| {
            let camera = entry[0];
            randomise_camera(m, camera, rng);
            m.sw(camera + 0x50, MODES[rng.next() as usize % MODES.len()]);
            let target = entity_pointer(m, rng.next() % 8);
            m.sw(camera + 0xc0, target);
            let context = m.gw(0x2dc);
            set_bits(m, context, 1, rng.next().is_multiple_of(3));
            set_bits(m, context + 0x12c, 0x4000, rng.next().is_multiple_of(4));
            m.sh(context + 0x5e, rng.next() as u16);
            [camera, 0, 0, 0]
        },
    );
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the render captures"]
fn camera_setup_oracle_800722a4_8007574c_80077fac() {
    let bases = oracle_bases(EARLY.0, 0x8007_9da4);
    oracle_cases(
        "camera script 800722A4",
        &bases,
        (0x8007_22a4, 0x8007_23ac),
        40,
        false,
        |_, _, rng| {
            let mode =
                [0u32, 0x100, 0x200, 0x400, 0x800, 0x1000, 0x2000, 0x4000][rng.next() as usize % 8];
            [mode | (rng.next() & 0xff), 0, 0, 0]
        },
    );
    oracle_cases(
        "camera control 8007574C",
        &bases,
        (0x8007_574c, 0x8007_577c),
        1,
        false,
        |_, _, _| [0; 4],
    );
    oracle_cases(
        "start camera 80077FAC",
        &bases,
        (0x8007_7fac, 0x8007_8010),
        8,
        false,
        |m, _, rng| {
            let context = m.gw(0x2dc);
            m.sb(context + 0x3a3, (rng.next() % 2) as u8);
            let flags = [0u32, 0x100, 0x200, 0x2000, 0x4000, 0x400][rng.next() as usize % 6];
            [flags, 0, 0, 0]
        },
    );
}

/// The loading-screen bases: entries of the loading texts 800364D0
/// (`a0` OT, `a1` primitive buffer, `[gp+0x150]` the loading state).
fn loading_bases() -> Vec<(Machine, [u32; 4])> {
    oracle_bases("loading-m003", 0x8003_64d0)
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/loading-m003"]
fn loading_visuals_oracle_800360fc_8003617c_80082998_800362b4_8005cce4_80036724() {
    let bases = loading_bases();
    oracle_cases(
        "frame copy 800360FC",
        &bases,
        (0x8003_60fc, 0x8003_617c),
        4,
        true,
        |m, e, rng| {
            let context = m.gw(0x2dc);
            m.sw(context + 4, rng.next() % 2);
            [e[0], e[1], 0, 0]
        },
    );
    oracle_cases(
        "star screen 8003617C",
        &bases,
        (0x8003_617c, 0x8003_62b4),
        12,
        true,
        |m, e, rng| {
            let context = m.gw(0x2dc);
            m.sw(context + 0x20, word(rng));
            [e[0], e[1], 0, 0]
        },
    );
    oracle_cases(
        "starfield 80082998",
        &bases,
        (0x8008_2998, 0x8008_2cd0),
        12,
        true,
        |m, e, rng| {
            for register in 0..5 {
                m.gte.ctc2(register, word(rng));
            }
            ac3_flight::sdk::srand(m, word(rng));
            [e[0], e[1], 0, 0]
        },
    );
    oracle_cases(
        "border 800362B4",
        &bases,
        (0x8003_62b4, 0x8003_64d0),
        20,
        true,
        |m, e, rng| {
            let sp = m.sp;
            m.sw(sp + 0x10, word(rng) & 0xff_ffff);
            [e[0], e[1], signed(rng, 0x2000), word(rng) & 0xff_ffff]
        },
    );
    oracle_cases(
        "loading tiles 8005CCE4",
        &bases,
        (0x8005_cce4, 0x8005_cd38),
        2,
        true,
        |m, e, _| {
            let state = m.gw(0x150);
            [state + 0xc, e[0], e[1], 0]
        },
    );
    oracle_cases(
        "fade screen 80036724",
        &bases,
        (0x8003_6724, 0x8003_6b20),
        40,
        true,
        |m, e, rng| {
            let state = m.gw(0x150);
            m.sb(state + 6, (rng.next() % 4) as u8);
            m.sh(state + 2, (rng.next() % 0x1400) as u16);
            m.sh(state, (rng.next() % 0x60) as u16);
            m.sw(state + 0x38, rng.next() % 0x1400);
            let context = m.gw(0x2dc);
            m.sb(context + 0x3a2, (rng.next() % 2) as u8);
            [e[0], e[1], 0, 0]
        },
    );
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the render captures"]
fn lens_flare_oracle_8008a030() {
    oracle_cases(
        "lens flare 8008A030",
        &oracle_bases(EARLY.0, 0x8008_a310),
        (0x8008_a030, 0x8008_a310),
        60,
        false,
        |m, _, rng| {
            let work = 0x1f80_001c;
            m.sh(work + 8, signed(rng, 0xb0) as u16);
            m.sh(work + 0xa, signed(rng, 0x90) as u16);
            m.sw(work + 0x10, rng.next() % 300);
            [work, 0x1f80_0000, 0, 0]
        },
    );
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the render captures"]
fn heat_shimmer_oracle_8008ae18() {
    oracle_cases(
        "heat shimmer 8008AE18",
        &oracle_bases(EARLY.0, 0x8006_1fe4),
        (0x8008_ae18, 0x8008_b1a8),
        60,
        false,
        |m, e, rng| {
            let work = e[1];
            let record = CRAFT + 0x1000;
            m.sw(work + 4, CRAFT + 0x4000);
            m.sw(work + 0x3f8, CRAFT + 0x8000);
            m.sw(work + 0x3fc, word(rng));
            for offset in (0..0x60).step_by(4) {
                m.sw(record + offset, word(rng));
            }
            m.sw(
                record + 0x10,
                if rng.next().is_multiple_of(4) {
                    0
                } else {
                    word(rng)
                },
            );
            for offset in [0x14u32, 0x16] {
                m.sh(record + offset, signed(rng, 0x400) as u16);
            }
            m.sh(record + 0x18, (0x200 + rng.next() % 0x3000) as u16);
            for offset in [0x1cu32, 0x1e, 0x20] {
                m.sh(record + offset, signed(rng, 0x800) as u16);
            }
            m.sw(record + 0x44, signed(rng, 0x200));
            for offset in [0x48u32, 0x4a] {
                m.sh(
                    record + offset,
                    if rng.next().is_multiple_of(4) {
                        0
                    } else {
                        signed(rng, 0x40)
                    } as u16,
                );
            }
            m.sh(record + 0x4c, (rng.next() % 0x1000) as u16);
            m.sh(record + 0x4e, signed(rng, 0x200) as u16);
            [work, record, 0, 0]
        },
    );
}

/// The explosion bases: entries of 80090FE0 (scratch work area set up by
/// 80083B78), with the view rotation saved at 0x1F800118 as 80090FE0 does.
fn explosion_bases() -> Vec<(Machine, [u32; 4])> {
    let mut bases = oracle_bases(EARLY.0, 0x8009_0fe0);
    for (m, _) in &mut bases {
        for register in 0..5 {
            let value = m.gte.cfc2(register);
            m.sw(0x1f80_0118 + register as u32 * 4, value);
        }
    }
    bases
}

/// A random explosion record at `record` around the eye (scratch +8).
fn craft_explosion(m: &mut Machine, record: u32, rng: &mut Rng) {
    m.sh(record, word(rng) as u16);
    m.sh(record + 2, (rng.next() % 4) as u16);
    let reach = if rng.next().is_multiple_of(8) {
        0x4000_0000
    } else {
        0x200_0000
    };
    for axis in 0..3u32 {
        let eye = m.lw(0x1f80_0008 + axis * 4);
        let first = eye.wrapping_add(signed(rng, reach));
        m.sw(record + 4 + axis * 4, first);
        let second = first.wrapping_add(signed(rng, 0x40_0000));
        m.sw(record + 0x10 + axis * 4, second);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the render captures"]
fn explosion_renderer_oracle_80090afc_80090fe0() {
    let bases = explosion_bases();
    oracle_cases(
        "explosion 80090AFC",
        &bases,
        (0x8009_0afc, 0x8009_0fe0),
        300,
        false,
        |m, _, rng| {
            craft_explosion(m, CRAFT, rng);
            [0x1f80_0000, 0x1f80_001c, CRAFT, 0]
        },
    );
    oracle_cases(
        "explosions 80090FE0",
        &bases,
        (0x8009_0fe0, 0x8009_1084),
        20,
        false,
        |m, _, rng| {
            let list = CRAFT + 0x800;
            let mask = rng.next() & 0x3f;
            m.sh(list, mask as u16);
            for index in 0..6 {
                craft_explosion(m, list + 4 + index * 28, rng);
            }
            [list, 0, 0, 0]
        },
    );
}

/// A random view-space point (three words) at `point`.
fn craft_point(m: &mut Machine, point: u32, rng: &mut Rng) {
    m.sw(point, signed(rng, 0x4000));
    m.sw(point + 4, signed(rng, 0x4000));
    let z = if rng.next().is_multiple_of(3) {
        signed(rng, 0x400)
    } else {
        rng.next() % 0x8000
    };
    m.sw(point + 8, z);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the render captures"]
fn explosion_clipping_oracle_800908f8_800907a4_800908ac_800a57b4() {
    let bases = explosion_bases();
    let (a, b, planes) = (CRAFT + 0x100, CRAFT + 0x110, CRAFT + 0x200);
    oracle_cases(
        "segment clip 800908F8",
        &bases,
        (0x8009_08f8, 0x8009_0afc),
        200,
        true,
        |m, _, rng| {
            craft_point(m, a, rng);
            craft_point(m, b, rng);
            [a, b, 1 + rng.next() % 0x100, planes]
        },
    );
    oracle_cases(
        "plane distance 800907A4",
        &bases,
        (0x8009_07a4, 0x8009_08ac),
        100,
        true,
        |m, _, rng| {
            for axis in 0..3 {
                let mut value = signed(rng, 0x8000);
                if axis == 0 && (value as i32).unsigned_abs() < 0x100 {
                    value = 0x100;
                }
                m.sw(a + axis * 4, value);
            }
            if rng.next().is_multiple_of(16) {
                for axis in 0..3 {
                    m.sw(a + axis * 4, 0);
                }
            }
            craft_point(m, b, rng);
            [a, b, 0, 0]
        },
    );
    oracle_cases(
        "interpolate 800908AC",
        &bases,
        (0x8009_08ac, 0x8009_08f8),
        40,
        false,
        |m, _, rng| {
            craft_point(m, a, rng);
            craft_point(m, b, rng);
            [a, b, CRAFT + 0x120, rng.next() % 0x1001]
        },
    );
    oracle_cases(
        "outer product 800A57B4",
        &bases,
        (0x800a_57b4, 0x800a_580c),
        40,
        false,
        |m, _, rng| {
            for offset in (0..0xc).step_by(4) {
                m.sw(a + offset, signed(rng, 0x1000));
                m.sw(b + offset, signed(rng, 0x1000));
            }
            [a, b, CRAFT + 0x120, 0]
        },
    );
}

/// A random explosion trail end (radius, colour, screen centre) at `end`.
fn craft_end(m: &mut Machine, end: u32, rng: &mut Rng) {
    let radius = if rng.next().is_multiple_of(6) {
        0
    } else {
        2 + rng.next() % 0x60
    };
    m.sw(end, radius);
    m.sw(end + 4, word(rng) & 0xff_ffff);
    let x = signed(rng, 0xd0) & 0xffff;
    let y = signed(rng, 0xb0) << 16;
    m.sw(end + 0xc, x | y);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the render captures"]
fn explosion_layers_oracle_80090478_8008fe38_80090200() {
    let bases = explosion_bases();
    let (a, b, work, ot) = (CRAFT + 0x300, CRAFT + 0x320, CRAFT + 0x400, CRAFT + 0x500);
    oracle_cases(
        "trail layer 80090478",
        &bases,
        (0x8009_0478, 0x8009_07a4),
        120,
        false,
        |m, _, rng| {
            craft_end(m, a, rng);
            craft_end(m, b, rng);
            m.sw(ot, 0x00ff_ffff);
            [ot, a, b, work]
        },
    );
    for (name, start, end) in [
        ("trail fan 8008FE38", 0x8008_fe38, 0x8009_0200),
        ("trail cap 80090200", 0x8009_0200, 0x8009_0478),
    ] {
        oracle_cases(name, &bases, (start, end), 120, true, |m, _, rng| {
            craft_end(m, a, rng);
            m.sw(work + 0x20, word(rng));
            m.sw(work + 0x24, rng.next() % 0x900);
            let prim = m.lw(0x1f80_0004);
            [prim, work, a, 0]
        });
    }
}

// ==== phase 4: oracle cases for the table-reached renderers ================================

/// Bases at the entries of the sorted object pass 80083D48 (scratch work
/// area set up by 80083B78).
fn object_bases() -> Vec<(Machine, [u32; 4])> {
    oracle_bases(EARLY.0, 0x8008_3d48)
}

/// A random object screen record at `screen` (xy word, depth +4, size
/// +6) and object state word at `object` (life << 16 | age).
fn craft_object(m: &mut Machine, screen: u32, object: u32, rng: &mut Rng) {
    let x = signed(rng, 0xc0) & 0xffff;
    let y = signed(rng, 0x90) << 16;
    m.sw(screen, x | y);
    m.sh(screen + 4, (rng.next() % 0x1000) as u16);
    m.sh(screen + 6, (1 + rng.next() % 0x60) as u16);
    let life = if rng.next().is_multiple_of(8) {
        0
    } else {
        rng.next() % 64
    };
    let age = rng.next() % (life + 1);
    m.sw(object, (life << 16) | age);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the render captures"]
fn object_renderers_oracle_8008473c_80084934() {
    let bases = object_bases();
    for (name, start, end) in [
        ("spark sprite 8008473C", 0x8008_473c, 0x8008_4934),
        ("glow object 80084934", 0x8008_4934, 0x8008_4bb0),
    ] {
        oracle_cases(name, &bases, (start, end), 100, false, |m, _, rng| {
            let work = 0x1f80_001c;
            let (screen, object) = (CRAFT, CRAFT + 0x20);
            craft_object(m, screen, object, rng);
            m.sw(work + 8, rng.next() % 4);
            m.sw(work + 0xc, rng.next() % 5);
            m.sw(work + 0x10, word(rng) & 0xfff);
            m.sw(work + 0x60, word(rng) & 0xff_ffff);
            [work, screen, object, 0]
        });
    }
}

/// Bases for the debris renderers: entries of 800898D8 with its work area
/// prepared as it does (sine table at work +0x24, draw mode +0x78, view
/// rotation +0x88).
fn debris_bases() -> Vec<(Machine, [u32; 4])> {
    let mut bases = oracle_bases(EARLY.0, 0x8008_98d8);
    bases.extend(oracle_bases(DEBRIS.0, 0x8008_98d8));
    let work = 0x1f80_001c;
    for (m, _) in &mut bases {
        for i in 0..32u32 {
            let value = m.lhu(0x800b_4f84 + i * 512);
            m.sh(work + 36 + i * 2, value);
            if i < 9 {
                m.sh(work + 100 + i * 2, value);
            }
        }
        m.sw(work + 120, 0xe600_0001);
        for register in 0..5 {
            let value = m.gte.cfc2(register);
            m.sw(work + 136 + register as u32 * 4, value);
        }
    }
    bases
}

/// A random debris piece at `piece` around the eye.
fn craft_piece(m: &mut Machine, piece: u32, rng: &mut Rng) {
    let life = if rng.next().is_multiple_of(8) {
        0
    } else {
        rng.next() % 80
    };
    m.sh(piece, (rng.next() % (life + 1)) as u16);
    m.sh(piece + 2, life as u16);
    let reach = if rng.next().is_multiple_of(8) {
        0x4000_0000
    } else {
        0x100_0000
    };
    for axis in 0..3u32 {
        let eye = m.lw(0x1f80_0008 + axis * 4);
        m.sw(piece + 4 + axis * 4, eye.wrapping_add(signed(rng, reach)));
    }
    m.sh(piece + 0x16, (1 + rng.next() % 0x400) as u16);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the render captures"]
fn debris_renderers_oracle_800887d4_80089480() {
    let bases = debris_bases();
    for (name, start, end) in [
        ("debris star 800887D4", 0x8008_87d4, 0x8008_8eac),
        ("debris ember 80089480", 0x8008_9480, 0x8008_98d8),
    ] {
        oracle_cases(name, &bases, (start, end), 200, false, |m, _, rng| {
            craft_piece(m, CRAFT, rng);
            [CRAFT, 0x1f80_001c, 0, 0]
        });
    }
}

/// Randomise the weather record (gameplay +0x268, crafted), the water
/// height, context +0x28 and the camera's motion and angle fields.
fn craft_weather(m: &mut Machine, rng: &mut Rng) {
    let weather = CRAFT + 0x3000;
    let gameplay = m.gw(0x2d0);
    m.sw(gameplay + 0x268, weather);
    m.sb(weather + 0x1d, (rng.next() % 0x60) as u8);
    m.sb(weather + 0x1c, (rng.next() % 0x60) as u8);
    let fade = rng.next() % 0x1200;
    m.sh(weather + 0x1e, fade as u16);
    m.sw(weather + 0xc, word(rng));
    m.sh(weather + 8, signed(rng, 0x1400) as u16);
    let water = m.gw(0x32c);
    m.sw(water + 4, signed(rng, 0x1000_0000));
    let context = m.gw(0x2dc);
    m.sw(context + 0x28, word(rng));
    let camera = m.lw(gameplay + 0x28);
    for offset in [0x9cu32, 0xa0, 0xa4] {
        m.sw(camera + offset, signed(rng, 0x4_0000));
    }
    for offset in [0xacu32, 0xae, 0xb0] {
        let angle = u32::from(m.lhu(0x1f80_0014 + (offset - 0xac)));
        m.sh(
            camera + offset,
            angle.wrapping_add(signed(rng, 0x40)) as u16,
        );
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the render captures"]
fn ambient_renderers_oracle_8008cc88_8008d2e8_8008d948_8008dd10() {
    let bases = oracle_bases(EARLY.0, 0x8008_c7c8);
    for (name, start, end) in [
        ("rain 8008CC88", 0x8008_cc88u32, 0x8008_d2e8u32),
        ("snow 8008D2E8", 0x8008_d2e8, 0x8008_d948),
        ("motes 8008D948", 0x8008_d948, 0x8008_dd10),
        ("leaves 8008DD10", 0x8008_dd10, 0x8008_e0e0),
    ] {
        oracle_cases(name, &bases, (start, end), 12, false, |m, _, rng| {
            craft_weather(m, rng);
            [0; 4]
        });
    }
}

/// Place the object in front of the eye: translation (+-0x400, +-0x300,
/// 0x200..0x6000).
fn place_object(m: &mut Machine, rng: &mut Rng) {
    m.gte.ctc2(5, signed(rng, 0x400));
    m.gte.ctc2(6, signed(rng, 0x300));
    m.gte.ctc2(7, 0x200 + rng.next() % 0x5e00);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the render captures"]
fn lit_callbacks_oracle_8008a874_8008b1a8() {
    let bases = oracle_bases(EARLY.0, 0x8006_1fe4);
    oracle_cases(
        "afterburner 8008A874",
        &bases,
        (0x8008_a874, 0x8008_ae18),
        60,
        false,
        |m, e, rng| {
            let work = e[1];
            place_object(m, rng);
            m.sw(work + 4, CRAFT + 0x4000);
            m.sw(work + 0x3f8, CRAFT + 0x8200);
            m.sw(work + 0xc, rng.next() % 3);
            m.sh(work + 0x3f6, signed(rng, 0x20) as u16);
            let state = CRAFT + 0x2000;
            m.sw(work + 0x3fc, state);
            m.sw(
                state,
                if rng.next().is_multiple_of(2) {
                    0
                } else {
                    word(rng)
                },
            );
            m.sh(state + 6, (rng.next() % 0x1200).wrapping_sub(0x100) as u16);
            m.sw(state + 0xc, word(rng));
            let parameters = CRAFT + 0x2800;
            let resources = m.gw(0x2cc);
            m.sw(resources + 0xc, parameters);
            m.sw(parameters + 0xc, (rng.next() % 4) | ((rng.next() % 2) << 8));
            m.sw(parameters + 0x10, word(rng) & 0xff_ffff);
            m.sw(parameters + 0x14, word(rng) & 0xff_ffff);
            for offset in (0x18..0x58).step_by(2) {
                m.sh(parameters + offset, signed(rng, 0x200) as u16);
            }
            for offset in (0x58..0x78).step_by(2) {
                m.sh(parameters + offset, signed(rng, 0x100) as u16);
            }
            m.sw(parameters + 0x78, word(rng));
            let context = m.gw(0x2dc);
            m.sw(context + 0x28, word(rng));
            let flags = (m.lw(context) & !0x2000_0000) | (word(rng) & 0x2000_0000);
            m.sw(context, flags);
            [work, 0, 0, 0]
        },
    );
    oracle_cases(
        "engine glow 8008B1A8",
        &bases,
        (0x8008_b1a8, 0x8008_b554),
        60,
        false,
        |m, e, rng| {
            let work = e[1];
            place_object(m, rng);
            for register in 8..13 {
                m.gte.ctc2(register, word(rng) & 0x0fff_0fff);
            }
            m.sw(work + 4, CRAFT + 0x4000);
            m.sw(work + 0x3f8, CRAFT + 0x8200);
            let effects = m.gw(0x40c);
            m.sw(effects + 0x5c8, rng.next() & 4);
            let source = CRAFT + 0x2c00;
            let gameplay = m.gw(0x2d0);
            m.sw(gameplay + 0x244, source);
            m.sw(
                source + 0x70,
                if rng.next().is_multiple_of(8) {
                    0
                } else {
                    word(rng) & 0xff_ffff
                },
            );
            [work, 0, 0, 0]
        },
    );
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the render captures"]
fn draw_callbacks_oracle_8008be4c_8008b914_8008ba18_8008bb1c() {
    let bases = oracle_bases(EARLY.0, 0x8006_1408);
    for (name, start, end) in [
        ("sparkle 8008BE4C", 0x8008_be4cu32, 0x8008_c1c8u32),
        ("glint 8008B914", 0x8008_b914, 0x8008_ba18),
        ("glint 8008BA18", 0x8008_ba18, 0x8008_bb1c),
        ("glints 8008BB1C", 0x8008_bb1c, 0x8008_bcbc),
    ] {
        oracle_cases(name, &bases, (start, end), 40, false, |m, e, rng| {
            let work = e[1];
            place_object(m, rng);
            m.sw(work + 4, CRAFT + 0x4000);
            m.sw(work + 0xc, rng.next() % 3);
            let context = m.gw(0x2dc);
            m.sw(context + 0x28, word(rng));
            [work, 0, 0, 0]
        });
    }
    oracle_cases(
        "glint projection 8008B554",
        &bases,
        (0x8008_b554, 0x8008_b6d0),
        60,
        true,
        |m, e, rng| {
            let block = e[1] + 0x10;
            place_object(m, rng);
            for offset in [0x20u32, 0x22, 0x24] {
                m.sh(block + offset, signed(rng, 0x40) as u16);
            }
            m.sw(block + 0x28, rng.next() % 0x80);
            m.sw(block + 0x2c, word(rng));
            [block, rng.next() % 3, 0x100 + rng.next() % 0x200, 0]
        },
    );
    oracle_cases(
        "glint rays 8008B6D0",
        &bases,
        (0x8008_b6d0, 0x8008_b914),
        40,
        false,
        |m, e, rng| {
            let work = e[1];
            let block = work + 0x10;
            m.sw(work + 4, CRAFT + 0x4000);
            m.sw(block + 0x50, rng.next() % 0x400);
            m.sw(block + 0x54, word(rng));
            place_object(m, rng);
            [work, block, rng.next() % 7, signed(rng, 0x400)]
        },
    );
}
