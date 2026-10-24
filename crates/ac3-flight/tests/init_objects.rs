//! 8007C41C, 80083414, 8006ADB8, 8006B6A0 and 8004C1A0 (direct callees of
//! 80032510) and every routine of their trees, run on original entry
//! snapshots, must reproduce the original return snapshots: all RAM outside
//! the stack, the scratchpad, the GTE registers and `v0`. Routines that write
//! stack locals of their caller (MATRIX outputs) are also compared on those
//! stack bytes, and every routine also runs through `dispatch::call`. The
//! trees' SDK (rand, RotMatrixYXZ, ApplyMatrixLV, MulMatrix) and shared
//! routines (80030534, 800306AC, 80030824, 80030B10, 800710FC, 8006D7DC) are
//! validated here on their `sdk.rs`/`shared.rs` implementations.
//!
//! Top level: `analysis/flight-runtime/init-callees-001`. Subtrees:
//! `analysis/flight-runtime/objects-001` (window 80032510, limit 3).
use ac3_flight::{
    Machine, dispatch, init_objects as objects, sdk, shared,
    snapshot::{self, Snapshot},
};
use std::path::{Path, PathBuf};

fn data(capture: &str, index: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR"))
        .join("analysis/flight-runtime")
        .join(capture)
        .join(index)
}
fn callees() -> PathBuf {
    data("init-callees-001", "callees.jsonl")
}
fn subtree() -> PathBuf {
    data("objects-001", "snapshots.jsonl")
}

struct Checked {
    /// Bytes the original changed outside the stack.
    changed: usize,
    /// Stack bytes compared.
    stack_bytes: usize,
    /// GTE registers differ between the entry and return snapshots.
    gte_changed: bool,
}

fn ram_offset(address: u32) -> usize {
    (address & 0x1f_ffff) as usize
}

/// [`snapshot::check_call`] with `v0` always required, plus the stack ranges
/// `(address, length)` the routine writes for its caller.
fn check(
    index: &Path,
    callee: u32,
    occurrence: usize,
    stack: &dyn Fn(&Snapshot) -> Vec<(u32, usize)>,
    native: impl FnOnce(&mut Machine, &Snapshot) -> u32,
) -> Checked {
    let directory = index.parent().unwrap();
    let all = snapshot::read_index(index).unwrap();
    let entry = all
        .iter()
        .filter(|s| s.callee == callee && s.stage == "entry")
        .nth(occurrence)
        .unwrap_or_else(|| panic!("{callee:08x}: no entry #{occurrence}"));
    let exit = all
        .iter()
        .find(|s| {
            s.callee == callee
                && s.stage == "return"
                && s.sequence > entry.sequence
                && s.ra == entry.ra
                && s.registers.gpr[29] == entry.registers.gpr[29]
        })
        .unwrap_or_else(|| panic!("{callee:08x}: no return"));
    let entry = Snapshot::load(directory, entry).unwrap();
    let exit = Snapshot::load(directory, exit).unwrap();
    let mut machine = entry.machine();
    let v0 = native(&mut machine, &entry);
    if let Err(fault) = machine.take_fault() {
        panic!("{callee:08x}: {fault}");
    }
    assert_eq!(machine.sp, entry.gpr(29), "{callee:08x}: frames balanced");
    let differences = snapshot::compare(&machine, &exit);
    assert!(
        differences.is_empty(),
        "{callee:08x}: {}",
        differences.summary()
    );
    assert_eq!(v0, exit.gpr(2), "{callee:08x}: v0 {v0:#010x}");
    let mut stack_bytes = 0;
    for (address, length) in stack(&entry) {
        let at = ram_offset(address);
        assert_eq!(
            machine.ram()[at..at + length],
            exit.ram[at..at + length],
            "{callee:08x}: stack {address:#010x}"
        );
        stack_bytes += length;
    }
    Checked {
        changed: snapshot::changed_bytes(&entry, &exit),
        stack_bytes,
        gte_changed: entry.info.registers.cp2d != exit.info.registers.cp2d
            || entry.info.registers.cp2c != exit.info.registers.cp2c,
    }
}

fn no_stack(_: &Snapshot) -> Vec<(u32, usize)> {
    Vec::new()
}
fn matrix_at_a1(entry: &Snapshot) -> Vec<(u32, usize)> {
    vec![(entry.gpr(5), 18)]
}

fn args(entry: &Snapshot) -> [u32; 4] {
    [entry.gpr(4), entry.gpr(5), entry.gpr(6), entry.gpr(7)]
}

/// Run the same snapshot through `dispatch::call` (the convention other
/// modules use).
fn check_dispatch(
    index: &Path,
    callee: u32,
    occurrence: usize,
    stack: &dyn Fn(&Snapshot) -> Vec<(u32, usize)>,
) {
    check(index, callee, occurrence, stack, |m, entry| {
        dispatch::call(m, callee, args(entry))
    });
}

// ---- top level (init-callees-001) ---------------------------------------------

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/init-callees-001"]
fn ring_channels_8007c41c() {
    let c = check(&callees(), 0x8007_c41c, 0, &no_stack, |m, _| {
        objects::init_ring_channels(m)
    });
    eprintln!("8007c41c: {} bytes", c.changed);
    assert!(c.changed >= 64);
    check_dispatch(&callees(), 0x8007_c41c, 0, &no_stack);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/init-callees-001"]
fn effect_tables_80083414() {
    let c = check(&callees(), 0x8008_3414, 0, &no_stack, |m, _| {
        objects::init_effect_tables(m)
    });
    eprintln!("80083414: {} bytes", c.changed);
    assert!(c.changed > 1000 && c.gte_changed);
    check_dispatch(&callees(), 0x8008_3414, 0, &no_stack);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/init-callees-001"]
fn slot_table_8006adb8() {
    let c = check(&callees(), 0x8006_adb8, 0, &no_stack, |m, _| {
        objects::init_slot_table(m)
    });
    eprintln!("8006adb8: {} bytes", c.changed);
    assert!(c.changed >= 64);
    check_dispatch(&callees(), 0x8006_adb8, 0, &no_stack);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/init-callees-001"]
fn player_flight_8006b6a0() {
    let c = check(&callees(), 0x8006_b6a0, 0, &no_stack, |m, entry| {
        objects::init_player_flight(m, entry.gpr(4))
    });
    eprintln!("8006b6a0: {} bytes", c.changed);
    assert!(c.changed > 40 && c.gte_changed);
    check_dispatch(&callees(), 0x8006_b6a0, 0, &no_stack);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/init-callees-001"]
fn player_start_8004c1a0() {
    // The start record at frame +0x10 survives in the return snapshot.
    let record = |entry: &Snapshot| vec![(entry.gpr(29) - 0x30 + 0x10, 0x18)];
    let c = check(&callees(), 0x8004_c1a0, 0, &record, |m, _| {
        objects::init_player_start(m)
    });
    eprintln!("8004c1a0: {} bytes", c.changed);
    assert!(c.changed > 20 && c.gte_changed && c.stack_bytes == 0x18);
    check_dispatch(&callees(), 0x8004_c1a0, 0, &record);
}

// ---- 8007C41C tree -------------------------------------------------------------

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/objects-001"]
fn reset_ring_channels_8007c3a0() {
    let c = check(&subtree(), 0x8007_c3a0, 0, &no_stack, |m, _| {
        objects::reset_ring_channels(m)
    });
    assert!(c.changed >= 64);
    check_dispatch(&subtree(), 0x8007_c3a0, 0, &no_stack);
}

// ---- 80083414 tree -------------------------------------------------------------

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/objects-001"]
fn rand_800ac178() {
    for occurrence in 0..3 {
        let c = check(&subtree(), 0x800a_c178, occurrence, &no_stack, |m, _| {
            sdk::rand(m)
        });
        assert!(c.changed > 0, "state word");
        check_dispatch(&subtree(), 0x800a_c178, occurrence, &no_stack);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/objects-001"]
fn fill_random_tables_8008352c() {
    let c = check(&subtree(), 0x8008_352c, 0, &no_stack, |m, entry| {
        objects::fill_random_tables(m, entry.gpr(4))
    });
    assert!(c.changed > 0x5c8 / 2);
    check_dispatch(&subtree(), 0x8008_352c, 0, &no_stack);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/objects-001"]
fn rot_matrix_yxz_800a5be8() {
    // Occurrences 0 and 1 (lighting in 80032510) write a stack MATRIX;
    // occurrence 2 (inside 80089A08) writes the scratchpad.
    let out = |entry: &Snapshot| {
        let out = entry.gpr(5);
        if (0x801f_e000..0x8020_0000).contains(&out) {
            vec![(out, 18)]
        } else {
            Vec::new()
        }
    };
    for occurrence in 0..3 {
        let c = check(&subtree(), 0x800a_5be8, occurrence, &out, |m, entry| {
            sdk::rot_matrix_yxz(m, entry.gpr(4), entry.gpr(5))
        });
        assert!(c.changed + c.stack_bytes > 0);
        check_dispatch(&subtree(), 0x800a_5be8, occurrence, &out);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/objects-001"]
fn light_direction_vectors_80089a08() {
    let c = check(&subtree(), 0x8008_9a08, 0, &no_stack, |m, entry| {
        objects::light_direction_vectors(m, entry.gpr(4))
    });
    assert!(c.changed > 0x20 && c.gte_changed);
    check_dispatch(&subtree(), 0x8008_9a08, 0, &no_stack);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/objects-001"]
fn copy_resource_parameters_8008e834() {
    let c = check(&subtree(), 0x8008_e834, 0, &no_stack, |m, entry| {
        objects::copy_resource_parameters(m, entry.gpr(4))
    });
    assert!(c.changed > 0);
    check_dispatch(&subtree(), 0x8008_e834, 0, &no_stack);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/objects-001"]
fn reset_effect_lists_800833a4() {
    // Fresh arena memory may already be zero; v0 (block - 0x14) shows the
    // routine ran against the right block.
    let c = check(&subtree(), 0x8008_33a4, 0, &no_stack, |m, _| {
        objects::reset_effect_lists(m)
    });
    eprintln!("800833a4: {} bytes", c.changed);
    check_dispatch(&subtree(), 0x8008_33a4, 0, &no_stack);
}

// ---- 8006ADB8 tree -------------------------------------------------------------

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/objects-001"]
fn reset_slot_table_8006ad60() {
    let c = check(&subtree(), 0x8006_ad60, 0, &no_stack, |m, _| {
        objects::reset_slot_table(m)
    });
    assert!(c.changed >= 64);
    check_dispatch(&subtree(), 0x8006_ad60, 0, &no_stack);
}

// ---- 8006B6A0 tree -------------------------------------------------------------

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/objects-001"]
fn allocate_player_blocks_8006b510() {
    let c = check(&subtree(), 0x8006_b510, 0, &no_stack, |m, _| {
        objects::allocate_player_blocks(m)
    });
    assert!(c.changed > 4);
    check_dispatch(&subtree(), 0x8006_b510, 0, &no_stack);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/objects-001"]
fn aircraft_parameters_8006b59c() {
    check(&subtree(), 0x8006_b59c, 0, &no_stack, |m, entry| {
        objects::aircraft_parameters(m, entry.gpr(4))
    });
    check_dispatch(&subtree(), 0x8006_b59c, 0, &no_stack);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/objects-001"]
fn rotate_vector_q14_80030cb4() {
    let c = check(&subtree(), 0x8003_0cb4, 0, &no_stack, |m, entry| {
        objects::rotate_vector_q14(m, entry.gpr(4), entry.gpr(5), entry.gpr(6))
    });
    assert!(c.gte_changed);
    check_dispatch(&subtree(), 0x8003_0cb4, 0, &no_stack);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/objects-001"]
fn matrix_yxz_q14_80030824() {
    let c = check(&subtree(), 0x8003_0824, 0, &matrix_at_a1, |m, entry| {
        shared::rotation_yxz_q14(m, entry.gpr(4), entry.gpr(5))
    });
    assert!(c.stack_bytes == 18 && c.gte_changed);
    check_dispatch(&subtree(), 0x8003_0824, 0, &matrix_at_a1);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/objects-001"]
fn apply_matrix_lv_800a4d78() {
    let c = check(&subtree(), 0x800a_4d78, 0, &no_stack, |m, entry| {
        sdk::apply_matrix_lv(m, entry.gpr(4), entry.gpr(5), entry.gpr(6))
    });
    assert!(c.gte_changed);
    check_dispatch(&subtree(), 0x800a_4d78, 0, &no_stack);
}

// ---- 8004C1A0 tree -------------------------------------------------------------

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/objects-001"]
fn place_player_800710fc() {
    // The MATRIX local at frame +0x18 survives in the return snapshot.
    let matrix = |entry: &Snapshot| vec![(entry.gpr(29) - 0x58 + 0x18, 18)];
    let c = check(&subtree(), 0x8007_10fc, 0, &matrix, |m, entry| {
        shared::place_player(m, entry.gpr(4))
    });
    assert!(c.changed > 20 && c.gte_changed);
    check_dispatch(&subtree(), 0x8007_10fc, 0, &matrix);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/objects-001"]
fn matrix_yxz_80030534() {
    for occurrence in 0..2 {
        let c = check(
            &subtree(),
            0x8003_0534,
            occurrence,
            &matrix_at_a1,
            |m, e| shared::rotation_yxz(m, e.gpr(4), e.gpr(5)),
        );
        assert!(c.stack_bytes == 18 && c.gte_changed);
        check_dispatch(&subtree(), 0x8003_0534, occurrence, &matrix_at_a1);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/objects-001"]
fn relative_angles_8006d7dc() {
    // Product MATRIX (+0x10, pad included) and the rotated vector (+0x50).
    let frame = |entry: &Snapshot| {
        let base = entry.gpr(29) - 0x78;
        vec![(base + 0x10, 0x14), (base + 0x50, 0xc)]
    };
    let c = check(&subtree(), 0x8006_d7dc, 0, &frame, |m, entry| {
        shared::relative_angles(m, entry.gpr(4), entry.gpr(5))
    });
    assert!(c.gte_changed && c.stack_bytes == 0x20);
    check_dispatch(&subtree(), 0x8006_d7dc, 0, &frame);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/objects-001"]
fn matrix_yxz_transposed_800306ac() {
    let c = check(&subtree(), 0x8003_06ac, 0, &matrix_at_a1, |m, entry| {
        shared::rotation_yxz_transposed(m, entry.gpr(4), entry.gpr(5))
    });
    assert!(c.stack_bytes == 18 && c.gte_changed);
    check_dispatch(&subtree(), 0x8003_06ac, 0, &matrix_at_a1);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/objects-001"]
fn mul_matrix_800a5128() {
    // m0 is a stack MATRIX of 8006D7DC: compare the product, pad included.
    let product = |entry: &Snapshot| vec![(entry.gpr(4), 0x14)];
    let c = check(&subtree(), 0x800a_5128, 0, &product, |m, entry| {
        sdk::mul_matrix(m, entry.gpr(4), entry.gpr(5))
    });
    assert!(c.stack_bytes == 0x14 && c.gte_changed);
    check_dispatch(&subtree(), 0x800a_5128, 0, &product);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/objects-001"]
fn atan2_fine_80030b10() {
    for occurrence in 0..2 {
        check(
            &subtree(),
            0x8003_0b10,
            occurrence,
            &no_stack,
            |m, entry| shared::atan2(m, entry.gpr(4), entry.gpr(5)),
        );
        check_dispatch(&subtree(), 0x8003_0b10, occurrence, &no_stack);
    }
}
