//! Effects translations on original entry snapshots must reproduce the
//! original return snapshots (see `snapshot::check_call`): all RAM outside
//! the stack, the scratchpad, GTE registers and `v0`.
//!
//! Capture `player-015`: mission-007/frame-00400.sav, UP+R1 held from frame
//! 171 (full-throttle dive); the ground contact at frame 503 (80081564
//! chain) and the crash (80071358 mode 0x100000) from frame 505: 8008094C,
//! 80080704, 80080BA4 at 505, 80081124/80080F4C/8007D66C at 507 and 509.
//! The particle pools need 8007E76C (`replay`) and the ring 8007C8AC
//! (`init_38818`), both called by address.
use ac3_flight::{
    Machine, effects,
    snapshot::{self, Snapshot},
};
use std::path::PathBuf;

fn capture(name: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR"))
        .join("analysis/flight-runtime")
        .join(name)
        .join("snapshots.jsonl")
}

/// Check entry snapshots `occurrences` of `callee`; returns the bytes the
/// original changed.
fn check(
    callee: u32,
    occurrences: std::ops::Range<usize>,
    run: impl Fn(&mut Machine, &Snapshot) -> Option<u32>,
) -> usize {
    occurrences
        .map(|occurrence| {
            snapshot::check_call(&capture("player-015"), callee, occurrence, &run)
                .unwrap_or_else(|e| panic!("player-015 #{occurrence}: {e}"))
        })
        .sum()
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-015"]
fn command_8007d720() {
    let changed = check(0x8007_d720, 0..3, |m, e| {
        Some(effects::command_8007d720(m, e.gpr(4), e.gpr(5), e.gpr(6)))
    });
    assert!(changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-015"]
fn command_8007d66c() {
    let changed = check(0x8007_d66c, 0..2, |m, e| {
        Some(effects::command_8007d66c(m, e.gpr(4), e.gpr(5)))
    });
    assert!(changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-015"]
fn smoke_800813f4() {
    let changed = check(0x8008_13f4, 0..1, |m, e| {
        Some(effects::smoke_800813f4(m, e.gpr(4), e.gpr(5)))
    });
    assert!(changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-015"]
fn debris_8008150c() {
    let changed = check(0x8008_150c, 0..1, |m, e| {
        Some(effects::debris_8008150c(m, e.gpr(4), e.gpr(5), e.gpr(6)))
    });
    assert!(changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-015"]
fn contact_burst_80081564() {
    let changed = check(0x8008_1564, 0..1, |m, e| {
        Some(effects::contact_burst_80081564(
            m,
            e.gpr(4),
            e.gpr(5),
            e.gpr(6),
            e.gpr(7),
        ))
    });
    assert!(changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-015"]
fn crash_explosion_8008094c() {
    let changed = check(0x8008_094c, 0..2, |m, e| {
        effects::crash_explosion_8008094c(m, e.gpr(4), e.gpr(5));
        None
    });
    assert!(changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-015"]
fn explosion_80080704() {
    let changed = check(0x8008_0704, 0..2, |m, e| {
        effects::explosion_80080704(m, e.gpr(4), e.gpr(5));
        None
    });
    assert!(changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-015"]
fn wreck_debris_80080ba4() {
    // An odd kind: the original returns after the gameplay-flag check.
    let changed = check(0x8008_0ba4, 0..1, |m, e| {
        assert_eq!(e.gpr(6) & 1, 1);
        effects::wreck_debris_80080ba4(m, e.gpr(4), e.gpr(5), e.gpr(6));
        None
    });
    assert_eq!(changed, 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-015"]
fn burning_80081124() {
    let changed = check(0x8008_1124, 0..2, |m, e| {
        effects::burning_80081124(m, e.gpr(4), e.gpr(5));
        None
    });
    assert!(changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-015"]
fn wreck_smoke_80080f4c() {
    let changed = check(0x8008_0f4c, 0..2, |m, e| {
        effects::wreck_smoke_80080f4c(m, e.gpr(4), e.gpr(5));
        None
    });
    assert!(changed > 0);
}

// ---- synthetic paths the first mission does not reach ----------------------------

/// 8008265C mode 0 decides the variant without a call when phase bits 4..6
/// are set, and returns early for phases not on a 16-frame boundary; mode 1
/// for phases not on a 4-frame boundary.
#[test]
fn unit_trail_gates() {
    let mut m = Machine::default();
    effects::unit_trail_8008265c(&mut m, 0x8010_0000, 0x11, 0);
    effects::unit_trail_8008265c(&mut m, 0x8010_0000, 0x3, 1);
    assert!(m.take_fault().is_ok(), "no command is reserved");
}

/// 80081198 ignores kinds from 11 up (no particle is allocated).
#[test]
fn fragment_kind_range() {
    let mut m = Machine::default();
    effects::fragment_80081198(&mut m, 0x8010_0000, 0x8010_0010, 0xb);
    assert!(m.take_fault().is_ok());
}
