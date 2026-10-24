//! Overlay-2 hub routines (`ov2_hub`) on original entry snapshots must
//! reproduce the matching return snapshots: all RAM outside the stack, the
//! scratchpad, every GTE register and `v0`.
//!
//! Captures (PCSX-Redux, `.ov2hub/ov2-snapshots.lua`: windows on the state-3
//! callback 800CBB88 while module 2 is loaded; every listed routine is
//! bracketed once per distinct hub situation, keyed by inner state, hub
//! phase, pressed buttons, flags, selection and reveal/slide timer buckets):
//! - `ov2hub-001`: hub entry from `hub-intro-001/frame-00240.sav` (hub init
//!   at frame 15 with option 0x40, slide-in, reveal, steady), RIGHT at frame
//!   22, LEFT at 60 and 110, RIGHT at 125.
//! - `ov2hub-002`: steady hub (`mission-002/final.sav`), CROSS at frame 1:
//!   confirm, the 25-frame slide-out with the exit offset, leaving (0x100).
//! - `ov2hub-003`: the reveal frames of `ov2hub-001` without input (reveal
//!   timers 1..31 for the growing grid and the marker fade).
//! - `ov2hub-004`: CROSS during the slide-in (frame 20): the confirm skip
//!   (phase 0 confirm, phase 1 at once, reveal 0x78: full grid, black
//!   markers).
//! - `ov2hub-005`: back from the briefing (`briefing-wrapper-fast-001/
//!   frame-00180.sav`, TRIANGLE/START/CROSS at frame 1): hub init at frame
//!   29 without option 0x40 and the slide-in without the fade.
//! - `ov2hub-006`: as `ov2hub-001` without the early input: the natural end
//!   of the slide-in and of the reveal, RIGHT twice (selection 2) and CROSS
//!   at frame 118: the slide-out of selection 2 and leaving the hub.
//!
//! Every callee these routines reach is translated in the workspace (the
//! `ov2core` visual-script, text and map routines, SetDrawMode in `sdk`),
//! so every occurrence is a full comparison.
//!
//! The other inner states the hub menu reaches (save screen and memory card,
//! aircraft unlock, CONNECTION LOST) are tested in [`states`]
//! (`tests/ov2_hub/states.rs`).
use ac3_flight::SmallData;
use ac3_flight::{
    Machine, ov2_hub,
    snapshot::{self, Snapshot},
};
use std::path::{Path, PathBuf};

#[path = "ov2_hub/states.rs"]
mod states;
mod support;

const CAPTURES: [&str; 6] = [
    "ov2hub-001",
    "ov2hub-002",
    "ov2hub-003",
    "ov2hub-004",
    "ov2hub-005",
    "ov2hub-006",
];

fn index(capture: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR"))
        .join("analysis/flight-runtime")
        .join(capture)
        .join("snapshots.jsonl")
}

fn word(e: &Snapshot, address: u32) -> u32 {
    let at = (address & 0x1f_ffff) as usize;
    u32::from_le_bytes(e.ram[at..at + 4].try_into().unwrap())
}

/// Run `callee` through `ov2_hub::dispatch` on the `occurrence`th entry
/// snapshot of `index` with its original arguments and compare with the
/// matching return. Returns the bytes the original changed.
fn run_case(index: &Path, callee: u32, occurrence: usize) -> Result<usize, String> {
    snapshot::check_call(index, callee, occurrence, |m, entry| {
        let args = [entry.gpr(4), entry.gpr(5), entry.gpr(6), entry.gpr(7)];
        ov2_hub::dispatch(m, callee, args)
    })
    .map_err(|e| format!("{} #{occurrence}: {e}", index.display()))
}

#[derive(Debug, Default)]
struct Tally {
    exact: usize,
    changed: usize,
}

/// Every captured occurrence of `callee`; none may fail. Prints one line per
/// occurrence and returns the tally.
fn check(callee: u32) -> Tally {
    let mut tally = Tally::default();
    for capture in CAPTURES {
        let index = index(capture);
        let count = snapshot::read_index(&index)
            .unwrap()
            .iter()
            .filter(|s| s.callee == callee && s.stage == "entry")
            .count();
        for occurrence in 0..count {
            match run_case(&index, callee, occurrence) {
                Ok(changed) => {
                    eprintln!("{callee:08x} {capture} #{occurrence}: exact, {changed} changed");
                    tally.exact += 1;
                    tally.changed += changed;
                }
                Err(error) => panic!("{error}"),
            }
        }
    }
    assert!(tally.exact > 0, "{callee:08x}: no captured occurrence");
    eprintln!("{callee:08x}: {tally:?}");
    tally
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/ov2hub-001..006"]
fn tile_cf404() {
    check(0x800c_f404);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/ov2hub-001..006"]
fn mission_cell_d186c() {
    check(0x800d_186c);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/ov2hub-001..006"]
fn spin_d12ec() {
    assert!(check(0x800d_12ec).changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/ov2hub-001..006"]
fn propagate_d3c50() {
    assert!(check(0x800d_3c50).changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/ov2hub-001..006"]
fn grid_d3908() {
    assert!(check(0x800d_3908).changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/ov2hub-001..006"]
fn background_fade_e7ce8() {
    assert!(check(0x800e_7ce8).changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/ov2hub-001..006"]
fn phase_slide_in_d4180() {
    assert!(check(0x800d_4180).changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/ov2hub-001..006"]
fn phase_reveal_d4448() {
    assert!(check(0x800d_4448).changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/ov2hub-001..006"]
fn phase_select_d45e4() {
    assert!(check(0x800d_45e4).changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/ov2hub-001..006"]
fn phase_slide_out_d4760() {
    assert!(check(0x800d_4760).changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/ov2hub-001..006"]
fn markers_d3ddc() {
    assert!(check(0x800d_3ddc).changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/ov2hub-001..006"]
fn wedges_d13e8() {
    assert!(check(0x800d_13e8).changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/ov2hub-001..006"]
fn draw_d170c() {
    assert!(check(0x800d_170c).changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/ov2hub-001..006"]
fn init_d483c() {
    assert!(check(0x800d_483c).changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/ov2hub-001..006"]
fn tick_d4b44() {
    assert!(check(0x800d_4b44).changed > 0);
}

// ---- paths the captures do not reach ------------------------------------

/// A machine on the first hub-init entry of `ov2hub-001` (overlay 2, its
/// data and the hub state are resident).
fn hub_machine() -> (Machine, Snapshot) {
    let index = index("ov2hub-001");
    let infos = snapshot::read_index(&index).unwrap();
    let entry = infos
        .iter()
        .find(|s| s.callee == 0x800d_483c && s.stage == "entry")
        .unwrap();
    let entry = Snapshot::load(index.parent().unwrap(), entry).unwrap();
    (entry.machine(), entry)
}

const HUB_STATE: u32 = 0x800f_9448;

/// Run a phase callback (no fault allowed).
fn phase(m: &mut Machine, address: u32) -> u32 {
    let result = ov2_hub::dispatch(m, address, [0; 4]).expect("claimed");
    m.take_fault().unwrap();
    result
}

fn context(m: &mut Machine) -> u32 {
    m.gw(0x2dc)
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/ov2hub-001"]
fn slide_in_without_slide_flag_finishes_at_once() {
    let (mut m, _) = hub_machine();
    assert_eq!(m.lw(0x800f_5e98), HUB_STATE);
    m.sw(HUB_STATE, 0);
    m.sw(HUB_STATE + 0x10, 7);
    assert_eq!(phase(&mut m, 0x800d_4180), 1);
    assert_eq!(m.lw(HUB_STATE + 0x10), 0);
    assert_eq!(m.lw(HUB_STATE), 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/ov2hub-001"]
fn reveal_ends_at_sixty() {
    let (mut m, _) = hub_machine();
    let ctx = context(&mut m);
    m.sw(ctx + 0x12c, 0);
    m.sw(HUB_STATE, 0);
    m.sw(HUB_STATE + 0x14, 0x3b);
    assert_eq!(phase(&mut m, 0x800d_4448), 0);
    assert_eq!(m.lw(HUB_STATE + 0x14), 0x3c);
    assert_eq!(phase(&mut m, 0x800d_4448), 1);
    assert_eq!(m.lw(HUB_STATE + 0x14), 0x3c);
    // A signed compare: a negative timer keeps counting.
    m.sw(HUB_STATE + 0x14, 0xffff_fff0);
    assert_eq!(phase(&mut m, 0x800d_4448), 0);
    assert_eq!(m.lw(HUB_STATE + 0x14), 0xffff_fff1);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/ov2hub-001"]
fn confirm_marks_the_slide_out_by_option_and_selection() {
    for (options, selection, slides) in [
        (0, 0, true),
        (0, 2, true),
        (0, 3, false),
        (0x10, 0, false),
        (0x10, 1, true),
        (0x10, 2, true),
        (0x10, 3, false),
    ] {
        let (mut m, _) = hub_machine();
        let ctx = context(&mut m);
        m.sw(ctx + 0x12c, 0x10);
        m.sw(ctx + 0x970, options);
        m.sw(HUB_STATE, 0);
        m.sw(HUB_STATE + 4, selection);
        assert_eq!(phase(&mut m, 0x800d_45e4), 1);
        assert_eq!(m.lw(HUB_STATE) & 2 != 0, slides, "{options:#x} {selection}");
        let mission = m.lw(0x800f_69d0);
        assert_eq!(u32::from(m.lhu(mission + 6)), selection);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/ov2hub-001"]
fn slide_out_steps_and_finishes() {
    let (mut m, _) = hub_machine();
    m.sw(HUB_STATE, 2);
    m.sw(HUB_STATE + 4, 2);
    m.sw(HUB_STATE + 0x10, 3);
    assert_eq!(phase(&mut m, 0x800d_4760), 0);
    assert_eq!(m.lw(HUB_STATE + 0x10), 4);
    assert_eq!(m.lw(HUB_STATE + 0x11c) as i32, -9);
    m.sw(HUB_STATE + 0x10, 0x19);
    assert_eq!(phase(&mut m, 0x800d_4760), 2);
    assert_eq!(m.lw(HUB_STATE), 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/ov2hub-001"]
fn navigation_wraps() {
    for (pressed, from, to) in [(8, 3, 0), (8, 1, 2), (4, 0, 3), (4, 2, 1)] {
        let (mut m, _) = hub_machine();
        let ctx = context(&mut m);
        m.sw(ctx + 0x12c, pressed);
        m.sw(HUB_STATE, 0);
        m.sw(HUB_STATE + 4, from);
        assert_eq!(phase(&mut m, 0x800d_45e4), 0);
        assert_eq!(m.lw(HUB_STATE + 4), to);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/ov2hub-001"]
fn save_areas_cec18_allocates_and_stores_three_pairs() {
    let (mut m, _) = hub_machine();
    let ctx = context(&mut m);
    let arena = m.lw(ctx + 0x1c);
    let mission = m.lw(0x800f_69d0);
    let calls = m.host_calls.len();
    let result = ov2_hub::save_areas_cec18(&mut m);
    m.take_fault().unwrap();
    let mut next = arena;
    let mut rects = Vec::new();
    for pair in 0..3u32 {
        for half in 0..2u32 {
            let rect = 0x800f_5b54 + pair * 0x10 + half * 8;
            let w = m.lh(rect + 4) as i32;
            let h = m.lh(rect + 6) as i32;
            let buffer = m.lw(mission + 0x2_00c8 + half * 0xc + pair * 4);
            assert_eq!(buffer, next, "pair {pair} half {half}");
            next += ((w * h * 2) as u32 + 3) & !3;
            rects.push((rect, buffer));
        }
    }
    assert_eq!(result, next);
    assert_eq!(m.lw(mission + 0x2_00e0), next);
    assert_eq!(m.lw(ctx + 0x1c), next + 4);
    let stores: Vec<(u32, u32)> = m.host_calls[calls..]
        .iter()
        .filter(|c| c.function == 0x800a_6970)
        .map(|c| (c.args[0], c.args[1]))
        .collect();
    let mut expected = Vec::new();
    for pair in 0..3 {
        expected.push(rects[pair * 2]);
        expected.push(rects[pair * 2 + 1]);
    }
    assert_eq!(stores, expected);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/ov2hub-001"]
fn init_without_fade_options() {
    for (options, choice) in [(0u32, 3i16), (0, 2), (0x10, 0), (0x10, 1)] {
        let (mut m, entry) = hub_machine();
        let ctx = context(&mut m);
        m.sw(ctx + 0x970, options);
        let mission = m.lw(0x800f_69d0);
        m.sh(mission + 6, choice as u16);
        let result = ov2_hub::dispatch(&mut m, 0x800d_483c, [0; 4]).unwrap();
        m.take_fault().unwrap();
        assert_eq!(result, 1);
        let slides = if options & 0x10 != 0 {
            choice == 1 || choice == 2
        } else {
            choice != 3
        };
        assert_eq!(m.lw(HUB_STATE) & 1 != 0, slides, "{options:#x} {choice}");
        assert_eq!(m.lw(HUB_STATE + 4), choice as u32);
        assert_eq!(m.lw(HUB_STATE + 0x10), 0x19);
        let cell = if options & 0x10 != 0 {
            0x3b
        } else {
            let current = word(&entry, ctx + 0x390) as i16 as i32 as u32;
            u32::from(m.lbu(0x800f_5958 + current * 12 + 1))
        };
        let x = m.lhu(0x800f_5c04 + cell * 4);
        let y = m.lhu(0x800f_5c06 + cell * 4);
        assert_eq!(m.lhu(HUB_STATE + 0x118), x);
        assert_eq!(m.lhu(HUB_STATE + 0x11a), y);
        let visible = m.lw(HUB_STATE + 0x98 + u32::from(x) * 4);
        assert_eq!(visible, 1 << (y & 31));
    }
}

/// Synthetic machine with the hub state pointer at HUB_STATE.
fn synthetic() -> Machine {
    let mut m = Machine::default();
    m.sw(0x800f_5e98, HUB_STATE);
    m
}

#[test]
fn tile_layout_and_link() {
    let mut m = Machine::default();
    let prim = 0x8010_0000;
    let slot = 0x8011_0000;
    m.sw(prim, 0xaabb_ccdd);
    m.sw(slot, 0x1200_3456);
    let next = ov2_hub::minimap::tile_cf404(&mut m, prim, slot, 0x1_0005, 7, [8, 9, 1, 2, 0x103]);
    assert_eq!(next, prim + 0x10);
    assert_eq!(m.lw(prim), 0x0300_3456);
    assert_eq!(m.lw(prim + 4), 0x6003_0201);
    assert_eq!(m.lw(prim + 8), 0x0007_0005);
    assert_eq!(m.lw(prim + 0xc), 0x0009_0008);
    assert_eq!(m.lw(slot), 0x1210_0000);
}

#[test]
fn propagation_pattern() {
    // Odd row 5 column 9: below gets column 9, above columns 8..10, the row
    // itself columns 8 and 10. Even row 6 column 0 and column 31: above gets
    // the column, below two edge columns.
    let mut m = synthetic();
    m.sw(HUB_STATE + 0x98 + 5 * 4, 1 << 9);
    ov2_hub::minimap::propagate_d3c50(&mut m);
    let visible = |m: &mut Machine, row: u32| m.lw(HUB_STATE + 0x98 + row * 4);
    assert_eq!(visible(&mut m, 4), 0b111 << 8);
    assert_eq!(visible(&mut m, 5), 0b111 << 8);
    assert_eq!(visible(&mut m, 6), 1 << 9);
    for row in 0..32 {
        assert_eq!(m.lw(HUB_STATE + 0x18 + row * 4), 0, "pending row {row}");
    }
    let mut m = synthetic();
    m.sw(HUB_STATE + 0x98 + 6 * 4, 1 | 1 << 31);
    ov2_hub::minimap::propagate_d3c50(&mut m);
    assert_eq!(visible(&mut m, 5), 1 | 1 << 31);
    assert_eq!(visible(&mut m, 6), 0b11 | 0b11 << 30);
    assert_eq!(visible(&mut m, 7), 0b11 | 0b11 << 30);
    // Row 0 leaves the reveal timer (the word above it) untouched, row 31
    // leaves visible row 0 (the word below it) untouched.
    let mut m = synthetic();
    m.sw(HUB_STATE + 0x14, 0x1234);
    m.sw(HUB_STATE + 0x98, 1 << 4);
    m.sw(HUB_STATE + 0x98 + 31 * 4, 1 << 20);
    ov2_hub::minimap::propagate_d3c50(&mut m);
    assert_eq!(m.lw(HUB_STATE + 0x14), 0x1234);
    assert_eq!(visible(&mut m, 0), 0b111 << 3);
    assert_eq!(visible(&mut m, 1), 0b111 << 3);
    assert_eq!(visible(&mut m, 30), 0b111 << 19);
    assert_eq!(visible(&mut m, 31), 0b111 << 19);
}

#[test]
fn full_grid_is_35_tiles() {
    let mut m = synthetic();
    m.sw(HUB_STATE + 0x11c, (-4i32) as u32);
    let prim = 0x8010_0000;
    let slot = 0x8011_0000;
    let next = ov2_hub::minimap::grid_d3908(&mut m, prim, slot, 0x78);
    assert_eq!(next, prim + 35 * 0x10);
    // First horizontal line at x 0xA8 - 4, y 0x102, 0x8C x 2.
    assert_eq!(m.lw(prim + 8), 0x0102_00a4);
    assert_eq!(m.lw(prim + 0xc), 0x0002_008c);
    // The last tile is the closing bar 1 x 0x90 at (0xA4, 0x102).
    let last = prim + 34 * 0x10;
    assert_eq!(m.lw(last + 8), 0x0102_00a4);
    assert_eq!(m.lw(last + 0xc), 0x0090_0001);
    assert_eq!(m.lw(slot) & 0x00ff_ffff, last & 0x00ff_ffff);
    // Nothing for a non-positive timer.
    assert_eq!(ov2_hub::minimap::grid_d3908(&mut m, prim, slot, 0), prim);
    assert_eq!(
        ov2_hub::minimap::grid_d3908(&mut m, prim, slot, 0xffff_ffff),
        prim
    );
}

#[test]
fn wedge_speeds() {
    let mut m = Machine::default();
    let state = 0x8012_0000;
    m.sw(0x800f_539c, state);
    // Wedge 0 selected at the 0x80 limit, wedge 1 crossing 6 -> 5, wedge 2
    // at 4 (stops), wedge 3 at a negative signed speed (below 5: stops).
    for (wedge, speed, angle) in [
        (0u32, 0x80u16, 0x100u16),
        (1, 6, 0x200),
        (2, 4, 0x300),
        (3, 0x8000, 0x10),
    ] {
        m.sh(state + 4 + wedge * 2, speed);
        m.sh(state + 0xc + wedge * 2, angle);
    }
    m.sw(state, 1);
    ov2_hub::backdrop::spin_d12ec(&mut m, 0);
    assert_eq!(m.lhu(state + 4), 0x80);
    assert_eq!(m.lhu(state + 0xc), 0x80);
    assert_eq!(m.lhu(state + 6), 5);
    assert_eq!(m.lhu(state + 0xe), 0x200 - 0x20 + 5);
    assert_eq!(m.lhu(state + 8), 0);
    assert_eq!(m.lhu(state + 0x10), 0x300);
    assert_eq!(m.lhu(state + 0xa), 0);
    assert_eq!(m.lhu(state + 0x12), 0x10);
    // Direction clear: the selected wedge turns forward.
    m.sw(state, 0);
    m.sh(state + 4, 0x10);
    ov2_hub::backdrop::spin_d12ec(&mut m, 0);
    assert_eq!(m.lhu(state + 4), 0x11);
    assert_eq!(m.lhu(state + 0xc), 0x80 + 0x11);
}

#[test]
fn dispatch_needs_module_2() {
    let mut m = Machine::default();
    assert_eq!(ov2_hub::dispatch(&mut m, 0x800d_3c50, [0; 4]), None);
    let context = 0x8013_0000;
    m.set_gw(0x2dc, context);
    m.sw(context + 0x38, 1);
    assert_eq!(ov2_hub::dispatch(&mut m, 0x800d_186c, [0; 4]), None);
    m.sw(context + 0x38, 2);
    m.sb(0x800f_5958 + 12 * 3 + 1, 0x2a);
    assert_eq!(
        ov2_hub::dispatch(&mut m, 0x800d_186c, [3, 0, 0, 0]),
        Some(0x2a)
    );
    assert_eq!(ov2_hub::dispatch(&mut m, 0x800d_169c, [0; 4]), None);
}

// ---- round 2: inner states 9, 12 and 13 ----------------------------------------------------------

/// One snapshot test per captured round-2 routine: every occurrence in
/// `ov2hub-008..011` exact (or, past an untranslated libc routine, equal to
/// the instruction oracle from the same entry state).
macro_rules! captured {
    ($($name:ident = $address:literal),* $(,)?) => {$(
        #[test]
        #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/ov2hub-008..011"]
        fn $name() {
            states::captured($address);
        }
    )*};
}

captured! {
    reset_ports_cbca8 = 0x800c_bca8,
    stop_monitor_cbd20 = 0x800c_bd20,
    poll_sw_cbd50 = 0x800c_bd50,
    clear_sw_cbe30 = 0x800c_be30,
    clear_hw_cbe9c = 0x800c_be9c,
    poll_hw_cbef4 = 0x800c_bef4,
    monitor_cbfac = 0x800c_bfac,
    status_cc554 = 0x800c_c554,
    open_events_cc66c = 0x800c_c66c,
    enable_events_cc774 = 0x800c_c774,
    close_events_cc8f4 = 0x800c_c8f4,
    header_ccac8 = 0x800c_cac8,
    fresh_data_ccbac = 0x800c_cbac,
    clear_record_ccdc0 = 0x800c_cdc0,
    names_ccdc8 = 0x800c_cdc8,
    prepare_cceb8 = 0x800c_ceb8,
    current_record_ccf50 = 0x800c_cf50,
    pack_progress_cd580 = 0x800c_d580,
    pack_record_cd98c = 0x800c_d98c,
    format_cdb18 = 0x800c_db18,
    directory_cdb68 = 0x800c_db68,
    create_cdca8 = 0x800c_dca8,
    seal_data_cdd50 = 0x800c_dd50,
    seal_records_cddf0 = 0x800c_ddf0,
    save_cded4 = 0x800c_ded4,
    check_data_ce09c = 0x800c_e09c,
    check_records_ce1a4 = 0x800c_e1a4,
    load_ce2fc = 0x800c_e2fc,
    record_ce500 = 0x800c_e500,
    best_rank_ce554 = 0x800c_e554,
    merge_ce5c8 = 0x800c_e5c8,
    text_style_d0efc = 0x800d_0efc,
    text_highlight_d0f50 = 0x800d_0f50,
    fade_in_d4c8c = 0x800d_4c8c,
    fade_d4d08 = 0x800d_4d08,
    panels_d4de8 = 0x800d_4de8,
    slide_in_d4e48 = 0x800d_4e48,
    ask_save_d4ed4 = 0x800d_4ed4,
    answer_save_d4f5c = 0x800d_4f5c,
    check_card_d50e8 = 0x800d_50e8,
    ask_no_card_d5170 = 0x800d_5170,
    ask_card_d53a0 = 0x800d_53a0,
    ask_format_d560c = 0x800d_560c,
    answer_format_d56c4 = 0x800d_56c4,
    format_d5880 = 0x800d_5880,
    wait_d5918 = 0x800d_5918,
    check_file_d59c0 = 0x800d_59c0,
    list_card_d5a30 = 0x800d_5a30,
    ask_new_file_d5c7c = 0x800d_5c7c,
    answer_new_file_d5d50 = 0x800d_5d50,
    start_load_d5f10 = 0x800d_5f10,
    load_d5f80 = 0x800d_5f80,
    ask_bad_data_d60a8 = 0x800d_60a8,
    ask_other_d6304 = 0x800d_6304,
    answer_other_d63c4 = 0x800d_63c4,
    slot_icons_d6560 = 0x800d_6560,
    slot_info_d6634 = 0x800d_6634,
    choose_slot_d6890 = 0x800d_6890,
    browse_slots_d6988 = 0x800d_6988,
    ask_slot_d6ad0 = 0x800d_6ad0,
    zoom_in_d6be8 = 0x800d_6be8,
    answer_slot_d6c50 = 0x800d_6c50,
    zoom_out_d6fb4 = 0x800d_6fb4,
    saving_d7018 = 0x800d_7018,
    create_file_d70ac = 0x800d_70ac,
    check_merge_d718c = 0x800d_718c,
    merge_d71e0 = 0x800d_71e0,
    pack_progress_d7230 = 0x800d_7230,
    pack_record_d725c = 0x800d_725c,
    write_d72a0 = 0x800d_72a0,
    saved_d734c = 0x800d_734c,
    wait_d7384 = 0x800d_7384,
    leave_d73b8 = 0x800d_73b8,
    slide_out_d7400 = 0x800d_7400,
    back_to_hub_d7468 = 0x800d_7468,
    fade_back_d74c0 = 0x800d_74c0,
    init_d75a4 = 0x800d_75a4,
    tick_d7640 = 0x800d_7640,
    backdrop_on_ea008 = 0x800e_a008,
    backdrop_off_ea020 = 0x800e_a020,
    scroll_f1954 = 0x800f_1954,
}

/// The save captures reach every state-9 and card routine but these eight
/// (no card, card full, cancelled waits, failed load, bad data): the oracle
/// variants cover them.
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/ov2hub-008..011"]
fn save_captures_reach_all_but_eight() {
    let mut none = Vec::new();
    for routine in states::SAVE_ROUTINES.iter().chain(&states::CARD_ROUTINES) {
        let (tally, errors) = states::survey(*routine);
        assert!(errors.is_empty(), "{}", errors.join("\n"));
        if tally.exact + tally.oracle == 0 {
            none.push(*routine);
        }
    }
    assert_eq!(
        none,
        [
            0x800d_5210,
            0x800d_5460,
            0x800d_594c,
            0x800d_5ad0,
            0x800d_6034,
            0x800d_6178,
            0x800d_7118,
            0x800d_730c
        ]
    );
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/ov2hub-008"]
fn save_oracle_variants() {
    assert!(states::save_variants() > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/ov2hub-008"]
fn card_oracle_variants() {
    assert!(states::card_variants() > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/ov2hub-008"]
fn unlock_oracle_variants() {
    assert!(states::unlock_variants() > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/ov2hub-008"]
fn lost_oracle_variants() {
    assert!(states::lost_variants() > 0);
}

/// A blank machine with module `module` loaded (context at 0x80130000).
fn loaded(module: u32) -> Machine {
    let mut m = Machine::default();
    let context = 0x8013_0000;
    m.set_gw(0x2dc, context);
    m.sw(context + 0x38, module);
    m
}

const STUB_ADDRESSES: [u32; 15] = [
    0x800a_d7e8,
    0x800a_d7f8,
    0x800a_d758,
    0x800a_d768,
    0x800a_d798,
    0x800a_b7e8,
    0x800a_b7f8,
    0x800a_b808,
    0x800a_bc38,
    0x800a_d808,
    0x800a_d818,
    0x800a_d828,
    0x800a_d838,
    0x800a_da08,
    0x800a_d848,
];

#[test]
fn round_two_claims_need_module_2() {
    let mut m = loaded(1);
    let routines = states::SAVE_ROUTINES
        .iter()
        .chain(&states::CARD_ROUTINES)
        .chain(&states::UNLOCK_ROUTINES)
        .chain(&states::LOST_ROUTINES)
        .chain(&STUB_ADDRESSES);
    for &routine in routines {
        assert_eq!(
            ov2_hub::dispatch(&mut m, routine, [0; 4]),
            None,
            "{routine:08x}"
        );
    }
    assert!(m.host_calls.is_empty());
}

/// The BIOS / libcard entries (`ac3_flight::bios`) are host calls with the
/// original argument registers they read; without a device or queued result they return the
/// documented defaults.
#[test]
fn bios_stubs_are_host_calls() {
    let mut m = loaded(2);
    m.host_results.push_back(3);
    let open = [0x800f_7e68, 0x8001, 0xff00_0000, 0x8011_9f00];
    assert_eq!(ac3_flight::dispatch::call(&mut m, 0x800a_d808, open), 3);
    assert_eq!(
        ac3_flight::dispatch::call(&mut m, 0x800a_d808, open),
        u32::MAX
    );
    let call = m.host_calls[0];
    assert_eq!(call.function, 0x800a_d808);
    assert_eq!(call.args, [0x800f_7e68, 0x8001, 0, 0]);
    let event = [0xf400_0001, 4, 0x2000, 0];
    assert_eq!(
        ac3_flight::dispatch::call(&mut m, 0x800a_d758, event),
        0xf100_0000
    );
    assert_eq!(m.host_calls[2].args, event);
    assert_eq!(
        ac3_flight::dispatch::call(&mut m, 0x800a_da08, [0x801f_fef8, 0x801f_ff08, 7, 7]),
        0
    );
    assert_eq!(m.host_calls[3].args, [0x801f_fef8, 0x801f_ff08, 0, 0]);
    assert_eq!(m.host_calls.len(), 4);
}

/// 800D17AC / 800D1828: mission number of a record, first record of a
/// number (record 0's id when none matches).
#[test]
fn mission_record_lookups() {
    let mut m = loaded(2);
    for i in 0..41u32 {
        m.sb(0x800f_5958 + 12 * i, (100 + i) as u8);
        m.sb(0x800f_5958 + 12 * i + 2, (i / 2) as u8);
    }
    assert_eq!(
        ov2_hub::dispatch(&mut m, 0x800d_17ac, [7, 0, 0, 0]),
        Some(3)
    );
    assert_eq!(
        ov2_hub::dispatch(&mut m, 0x800d_1828, [5, 0, 0, 0]),
        Some(110)
    );
    assert_eq!(
        ov2_hub::dispatch(&mut m, 0x800d_1828, [20, 0, 0, 0]),
        Some(140)
    );
    assert_eq!(
        ov2_hub::dispatch(&mut m, 0x800d_1828, [99, 0, 0, 0]),
        Some(100)
    );
}

/// 800F17C0: the CONNECTION LOST colour runs from `to` (frame counter 0)
/// to `from` (25) and back, `from + (to - from) * |n mod 50 - 25| / 25`.
#[test]
fn pulse_follows_the_frame_counter() {
    let mut m = loaded(2);
    let visual = 0x8012_0000;
    m.sw(0x800f_9e14, visual);
    let (from, to) = (0x8011_0000, 0x8011_0010);
    for (i, value) in [0u8, 0xe8, 0xe0].into_iter().enumerate() {
        m.sb(from + i as u32, value);
    }
    let colour = |m: &mut Machine| -> [u8; 3] {
        std::array::from_fn(|i| m.lbu(visual + 0x78 * 4 + 0x98 + i as u32))
    };
    let context = 0x8013_0000;
    for (counter, expected, v0) in [
        (0u32, [0u8, 0, 0], u32::MAX),
        (25, [0, 0xe8, 0xe0], 0),
        (37, [0, 121, 117], u32::MAX),
        (63, [0, 121, 117], u32::MAX),
    ] {
        m.sw(context + 0x20, counter);
        assert_eq!(
            ov2_hub::dispatch(&mut m, 0x800f_17c0, [0x78, from, to, 0]),
            Some(v0)
        );
        assert_eq!(colour(&mut m), expected, "counter {counter}");
    }
}

/// 800DC288: the unlock picture's slide-out request.
#[test]
fn unlock_slide_out_flags() {
    let mut m = loaded(2);
    let s = 0x8012_1000;
    m.sw(0x800f_6048, s);
    for (flags, after, v0, timer) in [
        (0u32, 2u32, 2u32, 0u32),
        (4, 2, 2, 9),
        (2, 2, 0, 9),
        (6, 6, 4, 9),
    ] {
        m.sw(s + 4, flags);
        m.sw(s + 0xc, 9);
        assert_eq!(ov2_hub::dispatch(&mut m, 0x800d_c288, [0; 4]), Some(v0));
        assert_eq!(
            (m.lw(s + 4), m.lw(s + 0xc)),
            (after, timer),
            "flags {flags}"
        );
    }
}

/// 800DB980: the aircraft unlocked by a mission join the list (not
/// without mission data).
#[test]
fn unlock_by_mission() {
    let mut m = loaded(2);
    let s = 0x8012_1000;
    m.sw(0x800f_6048, s);
    for index in [3u32, 9] {
        m.sb(0x800f_53a0 + 0x30 * index + 0x24, 7);
    }
    m.sw(s + 4, 1);
    assert_eq!(
        ov2_hub::dispatch(&mut m, 0x800d_b980, [7, 0, 0, 0]),
        Some(1)
    );
    assert_eq!(m.lhu(s + 0x10), 0);
    m.sw(s + 4, 0);
    assert_eq!(
        ov2_hub::dispatch(&mut m, 0x800d_b980, [7, 0, 0, 0]),
        Some(0)
    );
    assert_eq!(m.lhu(s + 0x10), 2);
    assert_eq!((m.lbu(s + 0x14), m.lbu(s + 0x15)), (3, 9));
}
