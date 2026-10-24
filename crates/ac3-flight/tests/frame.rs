//! End-to-end: the complete 25 Hz flight callback 80033C58 on four
//! consecutive original entry snapshots must reproduce each return snapshot
//! (all RAM outside the stack, scratchpad, GTE, `v0`), with two documented
//! allowances:
//!
//! - `0x800B9924` ([`sdk::GPU_TIMEOUT_POLLS`]): how many times libgpu polled
//!   the GPU DMA before it went idle (ClearOTagR/DrawSync). It counts
//!   hardware time; the native transfer is complete at once (0 polls).
//! - the upper halfword of VZ0/VZ1/VZ2: a stale stack word loaded by `lwc2`
//!   with an SVECTOR's pad, which the GTE never uses (FLIGHT_MACHINE.md
//!   rule 5). The low halfword must match.
mod support;
use support::oracle::{self, Rng};

use ac3_flight::SmallData;
use ac3_flight::{
    frame, sdk,
    snapshot::{self, Snapshot},
};
use std::path::PathBuf;

fn index() -> PathBuf {
    PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR"))
        .join("analysis/flight-runtime/whole-flight-001/snapshots.jsonl")
}

/// GTE data registers VZ0, VZ1, VZ2.
const VZ: [usize; 3] = [1, 3, 5];

fn check(occurrence: usize) -> Result<usize, String> {
    let index_file = index();
    let directory = index_file.parent().unwrap();
    let index = snapshot::read_index(&index_file).map_err(|e| e.to_string())?;
    let entry = index
        .iter()
        .filter(|s| s.callee == 0x8003_3c58 && s.stage == "entry")
        .nth(occurrence)
        .ok_or("no entry snapshot")?;
    let exit = index
        .iter()
        .find(|s| {
            s.callee == entry.callee
                && s.stage == "return"
                && s.sequence > entry.sequence
                && s.ra == entry.ra
                && s.registers.gpr[29] == entry.registers.gpr[29]
        })
        .ok_or("no return snapshot")?;
    let entry = Snapshot::load(directory, entry).map_err(|e| e.to_string())?;
    let exit = Snapshot::load(directory, exit).map_err(|e| e.to_string())?;
    let mut m = entry.machine();
    let v0 = frame::flight_callback(&mut m);
    m.take_fault().map_err(|f| f.to_string())?;
    let mut differences = snapshot::compare(&m, &exit);
    let polls = sdk::GPU_TIMEOUT_POLLS;
    differences
        .ram
        .retain(|&a| !(polls..polls + 4).contains(&a));
    differences.gte_data.retain(|&r| {
        !(VZ.contains(&r) && m.gte.data[r] & 0xffff == exit.info.registers.cp2d[r] & 0xffff)
    });
    if !differences.is_empty() {
        return Err(differences.summary());
    }
    if v0 != exit.gpr(2) {
        return Err(format!("v0 {v0:#010x}, original {:#010x}", exit.gpr(2)));
    }
    Ok(snapshot::changed_bytes(&entry, &exit))
}

#[test]
#[ignore = "requires AC3_DATA_DIR and whole-flight-001"]
fn whole_flight_callbacks_80033c58() {
    let mut failures = Vec::new();
    for occurrence in 0..4 {
        match check(occurrence) {
            Ok(changed) => {
                println!("callback {occurrence}: exact, {changed} original bytes");
                assert!(changed > 10_000, "{changed}");
            }
            Err(error) => failures.push(format!("callback {occurrence}: {error}")),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

// ==== the other resident state callbacks: snapshots and oracle cases ========================
//
// Only the replay state 80033DA8 is reached by a capture (ovmisc-015); the
// other states and their helpers are validated with the test-only oracle
// (`support::oracle`): the original instructions run from the flight
// callback's entry snapshots with randomised context and gameplay words,
// and must match the translation on all of RAM outside the dead stack, the
// scratchpad, the GTE, `sp`, the first fault and `v0`. Callees (the frame
// body included, called by address) run natively in both runs.

/// Free RAM for crafted blocks.
const CRAFT: u32 = 0x801c_0000;

fn word(rng: &mut Rng) -> u32 {
    (rng.next() << 8) ^ rng.next()
}

/// The flight callback entry snapshots of whole-flight-001.
fn state_bases() -> Vec<ac3_flight::Machine> {
    let bases: Vec<_> = oracle::entries("whole-flight-001", 0x8003_3c58)
        .iter()
        .map(Snapshot::machine)
        .collect();
    assert!(!bases.is_empty(), "no flight callback snapshots");
    bases
}

/// Randomise the context and gameplay words the states and their overlays
/// read.
fn randomise_state(m: &mut ac3_flight::Machine, rng: &mut Rng) {
    let context = m.gw(0x2dc);
    let gameplay = m.gw(0x2d0);
    for (offset, bits) in [
        (0u32, 0x3000_0001u32),
        (0x128, 0x10),
        (0x12c, 0x50),
        (0x130, 3),
        (0x24, 0xffff_ffff),
    ] {
        let value = (m.lw(context + offset) & !bits) | (word(rng) & bits);
        m.sw(context + offset, value);
    }
    m.sb(context + 0x3a9, (rng.next() % 4) as u8);
    m.sb(context + 0x3a2, (rng.next() % 2) as u8);
    m.sw(context + 0x398, rng.next() % 4);
    for (offset, bits) in [(0u32, 0x1000_0020u32), (4, 0x1000), (0xc, 0xffff_ffff)] {
        let value = (m.lw(gameplay + offset) & !bits) | (word(rng) & bits);
        m.sw(gameplay + offset, value);
    }
    m.sh(gameplay + 0x29e, (rng.next() % 4) as u16);
}

/// `cases` oracle cases per base of `range` (translation through
/// `dispatch::call`, `v0` compared when `value`).
fn oracle_cases(
    name: &str,
    range: (u32, u32),
    cases: usize,
    value: bool,
    mut setup: impl FnMut(&mut ac3_flight::Machine, &mut Rng) -> [u32; 4],
) {
    let mut rng = Rng(range.0 ^ cases as u32);
    let (mut total, mut changed) = (0, 0);
    for (b, base) in state_bases().iter().enumerate() {
        for case in 0..cases {
            let mut m = base.clone();
            let args = setup(&mut m, &mut rng);
            changed += oracle::diff(
                &format!("{name} base {b} case {case}"),
                &m,
                range,
                args,
                |m| {
                    let v0 = ac3_flight::dispatch::call(m, range.0, args);
                    value.then_some(v0)
                },
            );
            total += 1;
        }
    }
    println!("{name}: {total} oracle cases exact, original changed {changed} bytes");
    assert!(total > 0 && changed > 0, "{name}: vacuous");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/whole-flight-001"]
fn state_callbacks_oracle_80033d68_to_80035ef8() {
    for (name, start, end) in [
        ("sequence state 80033D68", 0x8003_3d68u32, 0x8003_3da8u32),
        ("replay state 80033DA8", 0x8003_3da8, 0x8003_3ed8),
        ("result state 80033ED8", 0x8003_3ed8, 0x8003_3fb4),
        ("state 80033FB4", 0x8003_3fb4, 0x8003_3fec),
        ("state 80033FEC", 0x8003_3fec, 0x8003_4024),
        ("state 80034024", 0x8003_4024, 0x8003_4064),
        ("pause state 80034064", 0x8003_4064, 0x8003_40d8),
    ] {
        oracle_cases(name, (start, end), 3, true, |m, rng| {
            randomise_state(m, rng);
            [0; 4]
        });
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/whole-flight-001"]
fn loading_states_oracle_80035e78_80035ef8() {
    oracle_cases(
        "aircraft load 80035E78",
        (0x8003_5e78, 0x8003_5ef8),
        2,
        true,
        |m, rng| {
            let context = m.gw(0x2dc);
            m.sb(context + 0x3aa, (rng.next() % 8) as u8);
            [0; 4]
        },
    );
    oracle_cases(
        "effects install 80035EF8",
        (0x8003_5ef8, 0x8003_5f5c),
        2,
        true,
        |_, _| [0; 4],
    );
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/whole-flight-001"]
fn state_helpers_oracle_80034a70_to_8007c548() {
    oracle_cases(
        "pause input 80034A70",
        (0x8003_4a70, 0x8003_4b78),
        20,
        false,
        |m, rng| {
            randomise_state(m, rng);
            let gameplay = m.gw(0x2d0);
            m.sh(gameplay + 0x29e, (rng.next() % 5).wrapping_sub(1) as u16);
            [0; 4]
        },
    );
    oracle_cases(
        "result flags 80034B78",
        (0x8003_4b78, 0x8003_4bb0),
        8,
        false,
        |m, rng| {
            randomise_state(m, rng);
            [0; 4]
        },
    );
    for (name, start, end) in [
        ("pause menu 80034BB0", 0x8003_4bb0u32, 0x8003_4df4u32),
        ("result caption 80034DF4", 0x8003_4df4, 0x8003_4e84),
        ("replay caption 80034E84", 0x8003_4e84, 0x8003_4f7c),
    ] {
        oracle_cases(name, (start, end), 6, true, |m, rng| {
            randomise_state(m, rng);
            let gameplay = m.gw(0x2d0);
            m.sw(gameplay + 0xc, rng.next() % 0x100);
            let context = m.gw(0x2dc);
            [m.lw(context + 0xc), m.lw(gameplay + 0x14), 0, 0]
        });
    }
    oracle_cases(
        "fade bar 80035080",
        (0x8003_5080, 0x8003_5144),
        10,
        true,
        |m, rng| {
            let context = m.gw(0x2dc);
            let gameplay = m.gw(0x2d0);
            let level = (word(rng) % 0x1400).wrapping_sub(0x200);
            [m.lw(context + 0xc), m.lw(gameplay + 0x14), level, 0]
        },
    );
    oracle_cases(
        "player reset 8006B8A8",
        (0x8006_b8a8, 0x8006_b90c),
        4,
        false,
        |m, rng| {
            let context = m.gw(0x2dc);
            m.sw(context + 0x398, rng.next() % 4);
            [0; 4]
        },
    );
    oracle_cases(
        "effect blocks 8007C548",
        (0x8007_c548, 0x8007_c5c0),
        8,
        false,
        |m, rng| {
            let block = CRAFT + 0x100;
            m.sw(CRAFT, block);
            m.sw(CRAFT + 4, CRAFT + 0x4000);
            for offset in [0xcu32, 0x10] {
                let value = if rng.next().is_multiple_of(3) {
                    u32::MAX
                } else {
                    rng.next() % 0x1000
                };
                m.sw(block + offset, value);
            }
            m.sw(block + 0x2050, rng.next() % 0x8000);
            [CRAFT, 0, 0, 0]
        },
    );
}

/// [`check`] for another state callback `run` at `callee` in `capture`,
/// with the same two allowances (GPU poll counter, VZ upper halfwords).
fn check_state(
    capture: &str,
    callee: u32,
    occurrence: usize,
    run: fn(&mut ac3_flight::Machine) -> u32,
) -> Result<usize, String> {
    let index_file = oracle::capture(capture);
    let directory = index_file.parent().unwrap();
    let index = snapshot::read_index(&index_file).map_err(|e| e.to_string())?;
    let entry = index
        .iter()
        .filter(|s| s.callee == callee && s.stage == "entry")
        .nth(occurrence)
        .ok_or("no entry snapshot")?;
    let exit = index
        .iter()
        .find(|s| {
            s.callee == entry.callee
                && s.stage == "return"
                && s.sequence > entry.sequence
                && s.ra == entry.ra
                && s.registers.gpr[29] == entry.registers.gpr[29]
        })
        .ok_or("no return snapshot")?;
    let entry = Snapshot::load(directory, entry).map_err(|e| e.to_string())?;
    let exit = Snapshot::load(directory, exit).map_err(|e| e.to_string())?;
    let mut m = entry.machine();
    let v0 = run(&mut m);
    m.take_fault().map_err(|f| f.to_string())?;
    let mut differences = snapshot::compare(&m, &exit);
    let polls = sdk::GPU_TIMEOUT_POLLS;
    differences
        .ram
        .retain(|&a| !(polls..polls + 4).contains(&a));
    differences.gte_data.retain(|&r| {
        !(VZ.contains(&r) && m.gte.data[r] & 0xffff == exit.info.registers.cp2d[r] & 0xffff)
    });
    if !differences.is_empty() {
        return Err(differences.summary());
    }
    if v0 != exit.gpr(2) {
        return Err(format!("v0 {v0:#010x}, original {:#010x}", exit.gpr(2)));
    }
    Ok(snapshot::changed_bytes(&entry, &exit))
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/ovmisc-015"]
fn replay_state_snapshots_80033da8() {
    let index = snapshot::read_index(&oracle::capture("ovmisc-015")).unwrap();
    let count = index
        .iter()
        .filter(|s| s.callee == 0x8003_3da8 && s.stage == "entry")
        .count();
    assert!(count > 0, "no 80033DA8 snapshots");
    let mut changed = 0;
    for occurrence in 0..count {
        match check_state("ovmisc-015", 0x8003_3da8, occurrence, frame::replay_state) {
            Ok(bytes) => changed += bytes,
            Err(error) => panic!("80033DA8 #{occurrence}: {error}"),
        }
    }
    println!("80033DA8: {count} replay frames exact, {changed} original bytes");
    assert!(changed > 0);
}
