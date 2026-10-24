//! Shared game routines (`ac3_flight::shared`) on original function
//! snapshots (see `snapshot::check_call`), plus edge cases.
//!
//! Captures: `shared-001` (window 80033C58, the first two flight callbacks,
//! up to three calls per routine), `shared-002` (window 80032510, gameplay
//! initialization), `shared-003` (flight, 800270FC) and `objects-001`. Every
//! captured call is checked through the uniform entry point, which also
//! asserts the original `v0`. Each call is re-run to compare the stack at
//! and above the entry `$sp` (output locals in the caller's frame).
use ac3_flight::SmallData;
use ac3_flight::{
    Fault, Machine, shared,
    snapshot::{self, Snapshot},
};
use std::path::PathBuf;

const FLIGHT: &str = "shared-001";
const INIT: &str = "shared-002";
const FLIGHT_COPY: &str = "shared-003";
const OBJECTS: &str = "objects-001";

fn index(capture: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR"))
        .join("analysis/flight-runtime")
        .join(capture)
        .join("snapshots.jsonl")
}

fn pair(capture: &str, callee: u32, occurrence: usize) -> (Snapshot, Snapshot) {
    let path = index(capture);
    let list = snapshot::read_index(&path).unwrap();
    let entry = list
        .iter()
        .filter(|s| s.callee == callee && s.stage == "entry")
        .nth(occurrence)
        .unwrap();
    let exit = list
        .iter()
        .find(|s| {
            s.callee == callee
                && s.stage == "return"
                && s.sequence > entry.sequence
                && s.ra == entry.ra
                && s.registers.gpr[29] == entry.registers.gpr[29]
        })
        .unwrap();
    let directory = path.parent().unwrap();
    (
        Snapshot::load(directory, entry).unwrap(),
        Snapshot::load(directory, exit).unwrap(),
    )
}

#[derive(Debug, Default)]
struct Outcome {
    calls: usize,
    changed: usize,
    stack_changed: usize,
    gte_changed: usize,
    nonzero_v0: usize,
}

/// Check every captured call of `callee` through `entry` (a0..a3 -> v0,
/// asserted against the original).
fn check_all(capture: &str, callee: u32, entry: fn(&mut Machine, [u32; 4]) -> u32) -> Outcome {
    let native =
        |m: &mut Machine, e: &Snapshot| Some(entry(m, [e.gpr(4), e.gpr(5), e.gpr(6), e.gpr(7)]));
    let calls = snapshot::read_index(&index(capture))
        .unwrap()
        .iter()
        .filter(|s| s.callee == callee && s.stage == "entry")
        .count();
    assert!(calls > 0, "{capture}: no {callee:08x} snapshots");
    let mut outcome = Outcome {
        calls,
        ..Outcome::default()
    };
    for occurrence in 0..calls {
        outcome.changed += snapshot::check_call(&index(capture), callee, occurrence, native)
            .unwrap_or_else(|error| panic!("{capture} #{occurrence}: {error}"));
        let (entry_snapshot, exit) = pair(capture, callee, occurrence);
        let mut m = entry_snapshot.machine();
        let v0 = native(&mut m, &entry_snapshot).unwrap();
        outcome.nonzero_v0 += usize::from(v0 != 0);
        let sp = (entry_snapshot.gpr(29) & 0x1f_ffff) as usize;
        let differing: Vec<usize> = (sp..0x20_0000)
            .filter(|&i| m.ram()[i] != exit.ram[i])
            .collect();
        assert!(
            differing.is_empty(),
            "{capture} #{occurrence}: stack differs at {:x?}",
            &differing[..differing.len().min(16)]
        );
        outcome.stack_changed += (sp..0x20_0000)
            .filter(|&i| entry_snapshot.ram[i] != exit.ram[i])
            .count();
        let (a, b) = (&entry_snapshot.info.registers, &exit.info.registers);
        outcome.gte_changed += (0..32).filter(|&i| a.cp2d[i] != b.cp2d[i]).count()
            + (0..32).filter(|&i| a.cp2c[i] != b.cp2c[i]).count();
    }
    eprintln!("{callee:08x} {capture}: {outcome:?}");
    outcome
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-001"]
fn set_channel_levels_8002daa0() {
    let o = check_all(FLIGHT, 0x8002daa0, shared::f_8002daa0);
    assert!(o.nonzero_v0 > 0);
    // The captured levels repeat: poison the two plain halfword stores of
    // each record (+4/+6; byte +1 is read-modify-write) so a match proves
    // them.
    for occurrence in 0..o.calls {
        snapshot::check_call(&index(FLIGHT), 0x8002daa0, occurrence, |m, e| {
            let record = shared::CHANNELS + e.gpr(4) * 20;
            if e.gpr(4) < 24 {
                for offset in [4, 6] {
                    let value = m.lhu(record + offset);
                    m.sh(record + offset, !value);
                }
            }
            Some(shared::f_8002daa0(m, [e.gpr(4), e.gpr(5), e.gpr(6), 0]))
        })
        .unwrap();
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-001,-002,objects-001"]
fn rotation_yxz_80030534() {
    for capture in [FLIGHT, INIT, OBJECTS] {
        let o = check_all(capture, 0x80030534, shared::f_80030534);
        assert!(o.gte_changed > 0 && o.changed + o.stack_changed > 0);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-001,-002,objects-001"]
fn rotation_yxz_transposed_800306ac() {
    for capture in [FLIGHT, INIT, OBJECTS] {
        let o = check_all(capture, 0x800306ac, shared::f_800306ac);
        assert!(o.gte_changed > 0 && o.changed + o.stack_changed > 0);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-001,-002,objects-001"]
fn rotation_yxz_q14_80030824() {
    for capture in [FLIGHT, INIT, OBJECTS] {
        let o = check_all(capture, 0x80030824, shared::f_80030824);
        assert!(o.gte_changed > 0 && o.changed + o.stack_changed > 0);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-001"]
fn rotation_zxy_q14_80030998() {
    let o = check_all(FLIGHT, 0x80030998, shared::f_80030998);
    assert!(o.gte_changed > 0 && o.changed + o.stack_changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-001,-002,objects-001"]
fn atan2_80030b10() {
    for capture in [FLIGHT, INIT, OBJECTS] {
        let o = check_all(capture, 0x80030b10, shared::f_80030b10);
        assert!(o.calls > 0);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-001"]
fn asin_80030c74() {
    let o = check_all(FLIGHT, 0x80030c74, shared::f_80030c74);
    assert!(o.calls > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-001"]
fn target_available_8003a08c() {
    let o = check_all(FLIGHT, 0x8003a08c, shared::f_8003a08c);
    assert!(o.calls > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-001"]
fn target_mode_code_8003a0f8() {
    let o = check_all(FLIGHT, 0x8003a0f8, shared::f_8003a0f8);
    assert!(o.nonzero_v0 == o.calls);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-001"]
fn square_root_80082740() {
    let o = check_all(FLIGHT, 0x80082740, shared::f_80082740);
    assert!(o.gte_changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-001,-002"]
fn vector_distance_800827b4() {
    for capture in [FLIGHT, INIT] {
        let o = check_all(capture, 0x800827b4, shared::f_800827b4);
        assert!(o.gte_changed > 0 && o.nonzero_v0 > 0);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-003"]
fn copy_words_800270fc() {
    let o = check_all(FLIGHT_COPY, 0x800270fc, shared::f_800270fc);
    assert!(o.nonzero_v0 == o.calls);
}

// ---- edge cases ------------------------------------------------------------------

#[test]
fn channel_levels_clamp_and_range() {
    let mut m = Machine::default();
    assert_eq!(shared::set_channel_levels(&mut m, 24, 1, 2), 0xff);
    assert_eq!(shared::set_channel_levels(&mut m, -1i32 as u32, 1, 2), 0xff);
    let record = shared::CHANNELS + 23 * 20;
    m.sb(record + 1, 0x81);
    assert_eq!(shared::set_channel_levels(&mut m, 23, 0x4000, 0x1_3fff), 23);
    assert_eq!(m.lhu(record + 4), 0x3fff, "0x4000 clamps");
    assert_eq!(m.lhu(record + 6), 0x3fff, "low halfword 0x3FFF stored");
    assert_eq!(m.lbu(record + 1), 0x91);
    // Negative halfwords are below 0x4000 and stored unchanged.
    shared::set_channel_levels(&mut m, 23, 0xffff_8000, 0x0001_c000);
    assert_eq!((m.lhu(record + 4), m.lhu(record + 6)), (0x8000, 0xc000));
}

#[test]
fn atan2_and_asin_without_tables() {
    let mut m = Machine::default();
    // Octant bases at 800234A4 as in the executable.
    for (i, base) in [0, 0x1000, 0x2000, 0x1000, 0x4000, 0x3000, 0x2000, 0x3000]
        .into_iter()
        .enumerate()
    {
        m.sw(shared::OCTANT_TABLE + i as u32 * 4, base);
    }
    assert_eq!(shared::atan2(&mut m, 0, 0), 0);
    // Zero tables leave only the octant bases.
    assert_eq!(shared::atan2(&mut m, 0, 5), 0);
    assert_eq!(shared::atan2(&mut m, 5, 0), 0x1000);
    assert_eq!(shared::atan2(&mut m, 0, -5i32 as u32), -0x2000i32 as u32);
    assert_eq!(shared::atan2(&mut m, -5i32 as u32, 0), -0x1000i32 as u32);
    assert_eq!(shared::atan2(&mut m, 5, 5), 0);
    shared::atan2(&mut m, 0x8000_0000, 3);
    shared::atan2(&mut m, 3, 0x8000_0000);
    shared::atan2(&mut m, 0x8000_0000, 0x8000_0000);
    assert!(m.take_fault().is_ok(), "the divide traps are unreachable");
    m.sh(shared::ASIN_TABLE + 0x4000 * 2, 4096);
    m.sh(shared::ASIN_TABLE + 2, 7);
    assert_eq!(shared::asin(&mut m, 0x4000), 4096);
    assert_eq!(shared::asin(&mut m, 0x7fff_ffff), 4096, "clamped");
    assert_eq!(shared::asin(&mut m, -0x5000i32 as u32), -4096i32 as u32);
    assert_eq!(shared::asin(&mut m, -1i32 as u32), -7i32 as u32);
    assert_eq!(shared::asin(&mut m, 0x8000_0000), 0, "wraps to entry 0");
}

#[test]
fn target_queries() {
    let mut m = Machine::default();
    m.sb(shared::TARGET_INDEX, 0xff);
    assert_eq!(shared::target_available(&mut m), 0);
    assert_eq!(shared::target_mode_code(&mut m), -1i32 as u32);
    let table = 0x8010_0000;
    let entity = 0x8010_1000;
    m.set_gw(ac3_flight::common::ENTITIES, table);
    m.sb(shared::TARGET_INDEX, 3);
    m.sw(table + 12, entity);
    assert_eq!(shared::target_available(&mut m), 1);
    m.sw(shared::ENTITY_FLAGS + 12, 0x400);
    assert_eq!(shared::target_available(&mut m), 0);
    m.sw(shared::ENTITY_FLAGS + 12, 0);
    m.sh(entity + 0x48, 100);
    assert_eq!(shared::target_available(&mut m), 0);
    m.sb(shared::MODE_BYTE, 0x16);
    assert_eq!(
        shared::target_mode_code(&mut m),
        -1i32 as u32,
        "flag 0x100 clear"
    );
    m.sw(shared::ENTITY_FLAGS + 12, 0x100);
    assert_eq!(shared::target_mode_code(&mut m), 11);
    m.sw(shared::ENTITY_FLAGS + 12, 0x500);
    assert_eq!(shared::target_mode_code(&mut m), -1i32 as u32);
}

#[test]
fn square_root_and_distance_edges() {
    let mut m = Machine::default();
    assert_eq!(shared::square_root(&mut m, 0), 0);
    assert_eq!(shared::square_root(&mut m, -5i32 as u32), 0);
    let (a, b) = (0x8010_0000, 0x8010_0010);
    assert_eq!(shared::vector_distance(&mut m, a, b), 0, "equal points");
    // Reduced component sum above 0x58000 saturates.
    m.sw(a, 0x5_8001 << 12);
    assert_eq!(shared::vector_distance(&mut m, a, b), 0x10_0000);
    m.sw(a, (-0x5_8001i32 << 12) as u32);
    assert_eq!(shared::vector_distance(&mut m, a, b), 0x10_0000);
    assert!(m.take_fault().is_ok());
}

/// Real tables from a flight snapshot: square roots and distances.
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-001"]
fn square_root_and_distance_with_tables() {
    let (entry, _) = pair(FLIGHT, 0x80082740, 0);
    let mut m = entry.machine();
    for k in [16u32, 100, 1000, 4096, 40000] {
        let root = shared::square_root(&mut m, k * k);
        assert!(root.abs_diff(k) <= k / 32 + 1, "sqrt({}) = {root}", k * k);
    }
    let (a, b) = (0x801f_0000, 0x801f_0010);
    // 3-4-5 triangles, below and above the 0xB000 rescaling threshold.
    for scale in [1u32, 100, 10_000] {
        m.sw(a, (3 * scale) << 12);
        m.sw(a + 4, (4 * scale) << 12);
        m.sw(a + 8, 0);
        for i in 0..3 {
            m.sw(b + i * 4, 0);
        }
        let d = shared::vector_distance(&mut m, a, b);
        assert!(d.abs_diff(5 * scale) <= 5 * scale / 32 + 8, "{scale}: {d}");
    }
    assert!(m.take_fault().is_ok());
}

#[test]
fn copy_words_counts() {
    let mut m = Machine::default();
    let (target, source) = (0x8010_0000, 0x8010_0100);
    for i in 0..4 {
        m.sw(source + i * 4, 0x1111_1111 * (i + 1));
    }
    assert_eq!(shared::copy_words(&mut m, target, source, 0), target);
    assert_eq!(
        shared::copy_words(&mut m, target, source, -2i32 as u32),
        target
    );
    assert_eq!(m.lw(target), 0);
    assert_eq!(shared::copy_words(&mut m, target, source, 3), target + 12);
    assert_eq!(m.read_bytes(target, 16)[..12], m.read_bytes(source, 12)[..]);
    assert_eq!(m.lw(target + 12), 0);
    // Misaligned addresses fault like the original's word accesses.
    shared::copy_words(&mut m, target + 2, source, 1);
    assert_eq!(
        m.take_fault(),
        Err(Fault::Misaligned {
            address: target + 2,
            width: 4
        })
    );
}

#[test]
fn dispatch_claims_every_routine() {
    let mut m = Machine::default();
    for address in [
        0x800270fc, 0x8002daa0, 0x80030534, 0x800306ac, 0x80030824, 0x80030998, 0x80030b10,
        0x80030c74, 0x8003a08c, 0x8003a0f8, 0x80082740, 0x800827b4,
    ] {
        assert!(shared::dispatch(&mut m, address, [0x8010_0000, 0x8010_0100, 0, 0]).is_some());
    }
    assert!(shared::dispatch(&mut m, 0x8003_0000, [0; 4]).is_none());
}
