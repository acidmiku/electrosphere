//! 80039A68 and its complete call subtree on original snapshots (see
//! `snapshot::check_call`). The top level uses `init-callees-001`; every
//! routine below it uses the first call bracketed in `world-002` (window
//! 80032510). Leaves are listed first, then their callers.
use ac3_flight::SmallData;
use ac3_flight::{GP, Machine, dispatch, init_39a68 as tree, sdk, snapshot};
use std::path::{Path, PathBuf};

fn data(relative: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR")).join(relative)
}

fn top_index() -> PathBuf {
    data("analysis/flight-runtime/init-callees-001/callees.jsonl")
}

fn subtree_index() -> PathBuf {
    data("analysis/flight-runtime/world-002/snapshots.jsonl")
}

/// Check the first `callee` call of world-002; returns the bytes the original
/// changed.
fn check(
    callee: u32,
    native: impl FnOnce(&mut Machine, &snapshot::Snapshot) -> Option<u32>,
) -> usize {
    snapshot::check_call(&subtree_index(), callee, 0, native).unwrap()
}

/// Invert every byte of `ranges` before running `native`, so that matching
/// the return snapshot proves the translation stores them (the original
/// rewrites values that were already present). Returns the poisoned count.
fn check_poisoned(callee: u32, ranges: &[(u32, u32)], native: fn(&mut Machine)) -> usize {
    let mut poisoned = 0;
    check(callee, |m, _| {
        for &(start, end) in ranges {
            for address in start..end {
                let value = m.lbu(address);
                m.sb(address, !value);
                poisoned += 1;
            }
        }
        native(m);
        None
    });
    poisoned
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/world-002"]
fn cat_prim_800aabd8() {
    let changed = check(0x800aabd8, |m, e| {
        sdk::cat_prim(m, e.gpr(4), e.gpr(5));
        None
    });
    assert!(changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/world-002"]
fn set_draw_tpage_800aac08() {
    let changed = check(0x800aac08, |m, e| {
        sdk::set_draw_tpage(m, e.gpr(4), e.gpr(5), e.gpr(6), e.gpr(7));
        None
    });
    assert!(changed > 0);
}

/// The callers pass sign-extended halfwords; v0 is asserted.
fn sign_extended(value: u32) -> i16 {
    let narrow = value as i16;
    assert_eq!(
        i32::from(narrow) as u32,
        value,
        "argument is a sign-extended halfword"
    );
    narrow
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/world-002"]
fn draw_area_start_800a7938() {
    check(0x800a7938, |m, e| {
        let v0 = sdk::draw_area_start(m, sign_extended(e.gpr(4)), sign_extended(e.gpr(5)));
        assert_ne!(v0 & 0xf_ffff, 0);
        Some(v0)
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/world-002"]
fn draw_area_end_800a79d0() {
    check(0x800a79d0, |m, e| {
        let v0 = sdk::draw_area_end(m, sign_extended(e.gpr(4)), sign_extended(e.gpr(5)));
        assert_ne!(v0 & 0xf_ffff, 0);
        Some(v0)
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/world-002"]
fn set_draw_area_800a73f0() {
    let changed = check(0x800a73f0, |m, e| {
        // a1 is the caller's stack RECT, present in the entry image.
        sdk::set_draw_area(m, e.gpr(4), e.gpr(5));
        None
    });
    assert!(changed > 0);
}

fn check_block(callee: u32, native: fn(&mut Machine, u32)) -> usize {
    check(callee, |m, e| {
        native(m, e.gpr(4));
        None
    })
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/world-002"]
fn block_4f4_008_8005aeb8() {
    assert!(check_block(0x8005aeb8, tree::block_4f4_008) > 30);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/world-002"]
fn block_4f4_09c_8005b298() {
    assert!(check_block(0x8005b298, tree::block_4f4_09c) > 100);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/world-002"]
fn block_4f4_1b8_8005b7e4() {
    assert!(check_block(0x8005b7e4, tree::block_4f4_1b8) > 50);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/world-002"]
fn block_4f4_30c_8005bc1c() {
    assert!(check_block(0x8005bc1c, tree::block_4f4_30c) > 50);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/world-002"]
fn block_4f4_244_8005c11c() {
    assert!(check_block(0x8005c11c, tree::block_4f4_244) > 100);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/world-002"]
fn block_4f4_384_8005c580() {
    assert!(check_block(0x8005c580, tree::block_4f4_384) > 60);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/world-002"]
fn block_4f4_3dc_8005c7d4() {
    assert!(check_block(0x8005c7d4, tree::block_4f4_3dc) > 50);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/world-002"]
fn packet_block_4f4_8005c9e4() {
    assert!(check_block(0x8005c9e4, tree::packet_block_4f4) > 500);
}

fn check_void(callee: u32, native: fn(&mut Machine)) -> usize {
    check(callee, |m, _| {
        native(m);
        None
    })
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/world-002"]
fn packets_4f4_8005cb98() {
    assert!(check_void(0x8005cb98, tree::packets_4f4) > 1000);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/world-002"]
fn packets_508_80062324() {
    assert!(check_void(0x80062324, tree::packets_508) > 600);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/world-002"]
fn packets_424_quads_8003ba98() {
    assert!(check_void(0x8003ba98, tree::packets_424_quads) > 700);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/world-002"]
fn packets_424_8003b180() {
    assert!(check_void(0x8003b180, tree::packets_424) > 3000);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/world-002"]
fn clear_records_4568_8003eacc() {
    assert!(check_void(0x8003eacc, tree::clear_records_4568) > 80);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/world-002"]
fn reset_gp_344_800400f4() {
    let poisoned = check_poisoned(0x800400f4, &[(GP + 0x344, GP + 0x34c)], tree::reset_gp_344);
    assert_eq!(poisoned, 8);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/world-002"]
fn packets_434_8003e4fc() {
    assert!(check_void(0x8003e4fc, tree::packets_434) > 3000);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/world-002"]
fn reset_gp_44c_80040c90() {
    let ranges = [(GP + 0x44c, GP + 0x451), (GP + 0x452, GP + 0x454)];
    assert_eq!(check_poisoned(0x80040c90, &ranges, tree::reset_gp_44c), 7);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/world-002"]
fn reset_4f38_80040f50() {
    let ranges = [(0x800c_4f38, 0x800c_4f3d)];
    assert_eq!(check_poisoned(0x80040f50, &ranges, tree::reset_4f38), 5);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/world-002"]
fn reset_4f48_80041a48() {
    let ranges = [(0x800c_4f48, 0x800c_4f51), (0x800c_4f54, 0x800c_4f55)];
    assert_eq!(
        check_poisoned(0x80041a48, &ranges, |m| {
            dispatch::call(m, 0x8004_1a48, [0; 4]);
        }),
        10
    );
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/world-002"]
fn reset_gp_454_80041dbc() {
    let ranges = [(GP + 0x454, GP + 0x455)];
    assert_eq!(check_poisoned(0x80041dbc, &ranges, tree::reset_gp_454), 1);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/world-002"]
fn packets_440_80040748() {
    assert!(check_void(0x80040748, tree::packets_440) > 250);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/world-002"]
fn packets_45c_80041e08() {
    assert!(check_void(0x80041e08, tree::packets_45c) > 2800);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/init-callees-001"]
fn initialize_39a68_80039a68() {
    let changed = snapshot::check_call(&top_index(), 0x80039a68, 0, |m, _| {
        let sp = m.sp;
        tree::initialize_39a68(m);
        assert_eq!(m.sp, sp, "guest stack frames balanced");
        None
    })
    .unwrap();
    // About 37 KB is allocated and filled; most of it already held the same
    // packets, so 11,407 bytes differ between entry and return.
    assert!(changed > 11_000, "changed {changed}");
}

/// Load the 80039A68 entry and return snapshots of init-callees-001.
fn top_pair(index_file: &Path) -> (snapshot::Snapshot, snapshot::Snapshot) {
    let directory = index_file.parent().unwrap();
    let index = snapshot::read_index(index_file).unwrap();
    let entry = index
        .iter()
        .find(|s| s.callee == 0x80039a68 && s.stage == "entry")
        .unwrap();
    let exit = index
        .iter()
        .find(|s| s.callee == 0x80039a68 && s.stage == "return" && s.sequence > entry.sequence)
        .unwrap();
    (
        snapshot::Snapshot::load(directory, entry).unwrap(),
        snapshot::Snapshot::load(directory, exit).unwrap(),
    )
}

/// The capture never takes the `[gp+0x4C0] == 0x1E` branch (80039B30). With
/// that byte set on the original entry state, everything except the 16-byte
/// name copy must still match the original return, and the copy must be the
/// upper-cased context name.
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/init-callees-001"]
fn initialize_39a68_name_branch() {
    const MODE: u32 = 0x800b_aeb4;
    const NAME: u32 = 0x800c_4f18;
    let (entry, exit) = top_pair(&top_index());
    let mut m = entry.machine();
    assert_ne!(m.lbu(MODE), 0x1e, "traced path skips the copy");
    m.sb(MODE, 0x1e);
    tree::initialize_39a68(&mut m);
    m.take_fault().unwrap();
    let differences = snapshot::compare(&m, &exit);
    let unexpected: Vec<u32> = differences
        .ram
        .iter()
        .copied()
        .filter(|&a| a != MODE && !(NAME..NAME + 16).contains(&a))
        .collect();
    assert!(
        unexpected.is_empty(),
        "unexpected differences {unexpected:08x?}"
    );
    assert!(differences.scratch.is_empty());
    assert!(differences.gte_data.is_empty() && differences.gte_control.is_empty());
    let context = m.gw(ac3_flight::common::CONTEXT);
    let source = m.read_bytes(context + 0x94c, 16);
    let expected: Vec<u8> = source.iter().map(u8::to_ascii_uppercase).collect();
    assert_eq!(m.read_bytes(NAME, 16), expected);
}
