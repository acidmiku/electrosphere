//! The script command effect targets (`src/commands.rs`, commands 5 and 8
//! of 80038B60) against the original code. No capture reaches them (the
//! replay and sequence captures run no effect commands), so each routine is
//! validated with the test-only oracle (`support::oracle`): the original
//! instructions run from flight snapshots (`entities-007`/`entities-008`,
//! with live particle pools) on random command records and must match the
//! translation on all of RAM outside the dead stack, the scratchpad, the
//! GTE, `sp`, the first fault and `v0` where returned. Callees (the
//! particle pools 8007E76C, `rand`, the tracer claim 800819A8) run natively
//! in both runs.
mod support;

use ac3_flight::Machine;
use ac3_flight::SmallData;
use support::oracle::{self, Rng};

/// The flight captures with live particle pools.
const CAPTURES: &[&str] = &["entities-007", "entities-008"];

/// Free RAM for the crafted command record.
const RECORD: u32 = 0x801c_0000;

/// Flight machines at the entries of 8004F100.
fn bases() -> Vec<Machine> {
    let mut bases = Vec::new();
    for capture in CAPTURES {
        for snapshot in oracle::entries(capture, 0x8004_f100) {
            bases.push(snapshot.machine());
        }
    }
    assert!(!bases.is_empty(), "no flight snapshots");
    bases
}

fn word(rng: &mut Rng) -> u32 {
    (rng.next() << 8) ^ rng.next()
}

/// A random command record: seed/size +2, velocity halfwords +4..+8,
/// scale +0xA, position words +0xC..+0x14 near the player, flags in +0.
fn craft_record(m: &mut Machine, rng: &mut Rng) {
    for offset in (0..0x20).step_by(4) {
        m.sw(RECORD + offset, word(rng));
    }
    for offset in [4u32, 6, 8] {
        let velocity = (word(rng) % 0x400).wrapping_sub(0x200);
        m.sh(RECORD + offset, velocity as u16);
    }
    m.sh(RECORD + 0xa, (word(rng) % 0x2000) as u16);
    let table = m.gw(0x2d8);
    let player = m.lw(table);
    for axis in 0..3 {
        let base = m.lw(player + 8 + axis * 4);
        let offset = (word(rng) % 0x200_0000).wrapping_sub(0x100_0000);
        m.sw(RECORD + 0xc + axis * 4, base.wrapping_add(offset));
    }
    let gameplay = m.gw(0x2d0);
    m.sw(gameplay + 0xc, word(rng));
}

/// Run `cases` oracle cases per base on random records; `extra` gives the
/// second and third arguments.
fn run_cases(
    name: &str,
    range: (u32, u32),
    cases: usize,
    value: bool,
    extra: impl Fn(&mut Rng) -> [u32; 2],
) {
    let mut rng = Rng(range.0);
    let (mut total, mut changed) = (0, 0);
    for (b, base) in bases().iter().enumerate() {
        for case in 0..cases {
            let mut m = base.clone();
            craft_record(&mut m, &mut rng);
            let [a1, a2] = extra(&mut rng);
            let args = [RECORD, a1, a2, 0];
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
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-007, entities-008"]
fn flash_claim_oracle_80091294() {
    run_cases(
        "flash claim 80091294",
        (0x8009_1294, 0x8009_1310),
        3,
        true,
        |_| [0, 0],
    );
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-007, entities-008"]
fn command_5_effects_oracle() {
    for (name, start, end) in [
        ("explosion 80091310", 0x8009_1310u32, 0x8009_1520u32),
        ("large explosion 80091520", 0x8009_1520, 0x8009_17d0),
        ("smoke 800917D0", 0x8009_17d0, 0x8009_192c),
        ("trail flash 8009192C", 0x8009_192c, 0x8009_1ab4),
    ] {
        run_cases(name, (start, end), 12, false, |rng| [rng.next() % 16, 0]);
    }
    run_cases(
        "smoke column 80091AB4",
        (0x8009_1ab4, 0x8009_1c14),
        12,
        false,
        |rng| [rng.next() % 3, 0],
    );
    run_cases(
        "shock wave 80091C14",
        (0x8009_1c14, 0x8009_1cac),
        8,
        false,
        |rng| [rng.next() % 2, 0],
    );
    run_cases(
        "flash 80091CAC",
        (0x8009_1cac, 0x8009_1d3c),
        12,
        false,
        |rng| [rng.next() % 8, 0],
    );
    run_cases(
        "burst 80091D3C",
        (0x8009_1d3c, 0x8009_1f68),
        12,
        false,
        |rng| [1 + rng.next() % 12, rng.next() % 3],
    );
    run_cases(
        "spray 80091F68",
        (0x8009_1f68, 0x8009_20d4),
        12,
        false,
        |rng| [1 + rng.next() % 12, 0],
    );
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/entities-007, entities-008"]
fn command_8_tracer_oracle_800920d4() {
    run_cases(
        "overlay tracer 800920D4",
        (0x8009_20d4, 0x8009_2164),
        6,
        false,
        |_| [0, 0],
    );
}
