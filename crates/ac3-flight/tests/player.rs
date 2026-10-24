//! Player flight update translations on original entry snapshots must
//! reproduce the original return snapshots (see `snapshot::check_call`):
//! all RAM outside the stack, the scratchpad, GTE registers and `v0`.
//!
//! Captures (`analysis/flight-runtime/`):
//! - `player-001`: mission-006 checkpoint, no input; flight frames 307
//!   (80071358 mode 2), 309 and 311 (mode 4); 2 calls per routine.
//! - `player-004`: mission-007 checkpoint in active flight, UP+LEFT held,
//!   CIRCLE (missile) at frame 192, CROSS (gun) from 194; frames 191, 193
//!   (missile launch) and 195 (gun burst); 2 calls per routine.
//! - `player-005`: mission-007 checkpoint, flight frames 171..219 (25), one
//!   pad input per two flight frames: none, DOWN (0x00800000), RIGHT
//!   (0x04000000), L1 (0x10000000), R1 (0x08000000), L2 (0x20000000), R2
//!   (0x40000000), TRIANGLE (0x20), SQUARE (0x8000), DOWN+RIGHT+L1+L2,
//!   UP+LEFT+R1+R2, none; 80071358, 800712B4, 8006B90C, 8006D218,
//!   8006D8D4 and 80070AF4 bracketed in every frame.
//! - `player-007`: mission-007/final.sav with periodic missile/gun/target
//!   inputs (as the 3000-frame call tree `player-006`), frames 433/435:
//!   the 80044D50/800501C8/80045950 subtrees with enemies in sensor range
//!   (8004444C, 800444DC, 80044548).
//! - `player-008`: the same run, frame 737: lock-slot release (80045874).
//! - `player-009`: mission-007 checkpoint, every 10th flight frame from 175
//!   to 655 with long holds: R2+DOWN, L1+DOWN, R1+RIGHT, R1+UP.
//! - `player-010`: mission-007 checkpoint, every 20th flight frame from 175
//!   to 1615: L2+LEFT, then R1+DOWN (loops) for 1000 frames, then R1+UP;
//!   speeds drop below the 0x14FFF scaling cap of 8006D218.
//! - `player-011`: the player-004 inputs, frames 195 and 271: a running
//!   missile reload timer (8007025C) and the 120-frame flight-path sample
//!   of 8004790C.
//! - `player-015`: mission-007/frame-00400.sav, UP+R1 (full-throttle dive)
//!   from frame 171; frames 501..509: ground contact at 503 (80081564
//!   effects, crash check 80074D5C/80077EF4), crash mode 0x100000 (80072EDC)
//!   from 505. Modes other than 1/2/4/0x100000 are not reachable from the
//!   first-mission checkpoints (mode survey player-012/013).
//!
//! Tests whose path reaches a pending shared/SDK stub fail with
//! `Fault::Untranslated` until those land (see the report).
use ac3_flight::SmallData;
use ac3_flight::{
    Machine, player,
    snapshot::{self, Snapshot},
};
use std::{cell::RefCell, path::PathBuf};

fn capture(name: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR"))
        .join("analysis/flight-runtime")
        .join(name)
        .join("snapshots.jsonl")
}

/// Check entry snapshots `occurrences` of `callee` in capture `name`;
/// returns the total number of bytes the original changed.
fn check(
    name: &str,
    callee: u32,
    occurrences: std::ops::Range<usize>,
    run: impl Fn(&mut Machine, &Snapshot) -> Option<u32>,
) -> usize {
    occurrences
        .map(|occurrence| {
            snapshot::check_call(&capture(name), callee, occurrence, &run)
                .unwrap_or_else(|e| panic!("{name} #{occurrence}: {e}"))
        })
        .sum()
}

/// The return snapshot paired with entry `occurrence` of `callee` (the
/// pairing `snapshot::check_call` uses), for outputs on the stack.
fn exit_snapshot(name: &str, callee: u32, occurrence: usize) -> Snapshot {
    let index_file = capture(name);
    let index = snapshot::read_index(&index_file).expect("index");
    let entry = index
        .iter()
        .filter(|s| s.callee == callee && s.stage == "entry")
        .nth(occurrence)
        .expect("entry");
    let exit = index
        .iter()
        .find(|s| {
            s.callee == callee
                && s.stage == "return"
                && s.sequence > entry.sequence
                && s.ra == entry.ra
                && s.registers.gpr[29] == entry.registers.gpr[29]
        })
        .expect("return");
    Snapshot::load(index_file.parent().expect("directory"), exit).expect("snapshot")
}

/// Compare `length` bytes at `address` (an output on the caller's stack,
/// outside `check_call`'s comparison) with the paired return snapshot and
/// require that the original changed them.
fn check_stack_output(
    name: &str,
    callee: u32,
    occurrence: usize,
    produced: &[u8],
    entry: &[u8],
    address: u32,
) {
    let exit = exit_snapshot(name, callee, occurrence);
    let at = (address & 0x1f_ffff) as usize;
    let expected = &exit.ram[at..at + produced.len()];
    assert_eq!(produced, expected, "{name} #{occurrence}: stack output");
    assert_ne!(entry, expected, "{name} #{occurrence}: output unchanged");
}

/// The caller's stack argument word `index` (0 = `[sp+0x10]`) at entry.
fn stack_arg(m: &mut Machine, e: &Snapshot, index: u32) -> u32 {
    m.lw(e.gpr(29).wrapping_add(0x10 + index * 4))
}

// ---- 80071358, controls and weapons --------------------------------------------------

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004"]
fn vector_length_800828f8() {
    for name in ["player-001", "player-004"] {
        check(name, 0x8008_28f8, 0..2, |m, e| {
            let v0 = player::vector_length_800828f8(m, e.gpr(4), e.gpr(5), e.gpr(6));
            assert_ne!(v0, 0);
            Some(v0)
        });
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-004"]
fn claim_vibration_80028f7c() {
    let changed = check("player-004", 0x8002_8f7c, 0..2, |m, e| {
        let v0 = player::claim_vibration_80028f7c(m, e.gpr(4));
        assert_ne!(v0, 0);
        Some(v0)
    });
    assert!(changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004"]
fn vibrate_80028fd0() {
    let mut changed = 0;
    for name in ["player-001", "player-004"] {
        changed += check(name, 0x8002_8fd0, 0..2, |m, e| {
            player::vibrate_80028fd0(m, e.gpr(4));
            None
        });
    }
    assert!(changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, -004, -005, -009"]
fn control_step_8006b90c() {
    let mut changed = 0;
    for (name, count) in &SWEEP[..4] {
        let (name, count) = (*name, *count);
        changed += check(name, 0x8006_b90c, 0..count, |m, e| {
            player::control_step_8006b90c(m, e.gpr(4), e.gpr(5), e.gpr(6), e.gpr(7));
            None
        });
    }
    assert!(changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004, player-005"]
fn scale_by_speed_8006d218() {
    let mut changed = 0;
    for (name, count) in [("player-001", 2), ("player-004", 2), ("player-005", 25)] {
        changed += check(name, 0x8006_d218, 0..count, |m, e| {
            assert!(e.gpr(4) > 0x14fff, "a capture below the speed cap");
            player::scale_by_speed_8006d218(m, e.gpr(4), e.gpr(5));
            None
        });
    }
    // Every captured speed is above 0x14FFF (scale 0x1000): the stores keep
    // the values. The scaled path is checked by `scale_by_speed_below_the_cap`.
    assert_eq!(changed, 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004, player-005"]
fn control_input_800712b4() {
    let mut changed = 0;
    for (name, count) in [("player-001", 2), ("player-004", 2), ("player-005", 25)] {
        changed += check(name, 0x8007_12b4, 0..count, |m, e| {
            player::control_input_800712b4(m, e.gpr(4), e.gpr(5));
            None
        });
    }
    assert!(changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-004"]
fn vibrate_weapon_80029028() {
    let changed = check("player-004", 0x8002_9028, 0..1, |m, e| {
        player::vibrate_weapon_80029028(m, e.gpr(4));
        None
    });
    // The captured pad has no rumble (context +0x120 & 3 == 1): the
    // original returns early. The rumble path: `vibration_slots_with_rumble`.
    assert_eq!(changed, 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-004"]
fn vibrate_gun_800290a4() {
    let changed = check("player-004", 0x8002_90a4, 0..1, |m, e| {
        player::vibrate_gun_800290a4(m, e.gpr(4));
        None
    });
    // The captured pad has no rumble (context +0x120 & 3 == 1): the
    // original returns early. The rumble path: `vibration_slots_with_rumble`.
    assert_eq!(changed, 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-004"]
fn queue_bullet_80069864() {
    check("player-004", 0x8006_9864, 0..1, |m, e| {
        let v0 = player::queue_bullet_80069864(m, e.gpr(4), e.gpr(5), e.gpr(6), e.gpr(7));
        assert_eq!(v0, 1);
        Some(v0)
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-004"]
fn allocate_projectile_80066df0() {
    check("player-004", 0x8006_6df0, 0..1, |m, e| {
        let v0 = player::allocate_projectile_80066df0(m, e.gpr(4), e.gpr(5), e.gpr(6), e.gpr(7));
        assert_ne!(v0, 0);
        Some(v0)
    });
}

/// Blocked until shared 800827B4 lands (flags 0x10102: class by distance).
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-004"]
fn spawn_80067030() {
    check("player-004", 0x8006_7030, 0..1, |m, e| {
        let flags = stack_arg(m, e, 0);
        let v0 = player::spawn_80067030(m, e.gpr(4), e.gpr(5), e.gpr(6), e.gpr(7), flags);
        assert_ne!(v0, 0);
        Some(v0)
    });
}

/// Blocked until shared 80030534 lands. The output is in the caller's
/// frame, so it is compared separately.
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-004"]
fn weapon_origin_800700ec() {
    let output = RefCell::new((0, Vec::new(), Vec::new()));
    check("player-004", 0x8007_00ec, 0..1, |m, e| {
        let out = e.gpr(5);
        let before = m.read_bytes(out, 0x16);
        player::weapon_origin_800700ec(m, e.gpr(4), out);
        *output.borrow_mut() = (out, m.read_bytes(out, 0x16), before);
        None
    });
    let (out, produced, before) = output.into_inner();
    check_stack_output("player-004", 0x8007_00ec, 0, &produced, &before, out);
}

/// Blocked until shared 80030534 lands.
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-004"]
fn fire_bullet_8006fd2c() {
    let changed = check("player-004", 0x8006_fd2c, 0..1, |m, e| {
        player::fire_bullet_8006fd2c(m, e.gpr(4));
        None
    });
    assert!(changed > 0);
}

/// Blocked until 80030534 (shared), 8002D724 (script) and 8008E8A0 land.
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-004"]
fn gun_8006fe5c() {
    let changed = check("player-004", 0x8006_fe5c, 0..1, |m, _| {
        player::gun_8006fe5c(m);
        None
    });
    assert!(changed > 0);
}

/// Blocked as `gun_8006fe5c`.
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-004"]
fn fire_gun_80070a10() {
    let changed = check("player-004", 0x8007_0a10, 0..1, |m, e| {
        player::fire_gun_80070a10(m, e.gpr(4));
        None
    });
    assert!(changed > 0);
}

/// Blocked until 80030534/800827B4 (shared) and 8002D724 (script) land.
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-004"]
fn launch_missile_800702cc() {
    let changed = check("player-004", 0x8007_02cc, 0..1, |m, e| {
        player::launch_missile_800702cc(m, e.gpr(4));
        None
    });
    assert!(changed > 0);
}

/// Blocked as `launch_missile_800702cc`.
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-004"]
fn fire_weapon_80070a50() {
    let changed = check("player-004", 0x8007_0a50, 0..1, |m, e| {
        player::fire_weapon_80070a50(m, e.gpr(4), e.gpr(5));
        None
    });
    assert!(changed > 0);
}

/// Blocked until the flight model's SDK/shared callees land.
/// Captures with whole flight frames bracketed: the first frames (mode 2
/// and 4), missile/gun frames, the 25-frame button sweep (player-005) and
/// the long holds (player-009, player-010).
const SWEEP: [(&str, usize); 5] = [
    ("player-001", 2),
    ("player-004", 2),
    ("player-005", 25),
    ("player-009", 25),
    ("player-010", 37),
];

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, -004, -005, -009"]
fn update_player_80071358() {
    let changed = sweep(&SWEEP, 0x8007_1358, |m, _| {
        player::update_player_80071358(m);
        None
    });
    assert!(changed > 0);
}

/// Occurrences whose path reaches a routine nobody translates yet (none
/// now; player-010 #21, the ground contact, runs the effects module).
const BLOCKED: [(&str, usize); 0] = [];

/// `check` over every occurrence of `captures` except `BLOCKED`.
fn sweep(
    captures: &[(&str, usize)],
    callee: u32,
    run: impl Fn(&mut Machine, &Snapshot) -> Option<u32>,
) -> usize {
    let mut changed = 0;
    for &(name, count) in captures {
        for occurrence in 0..count {
            if !BLOCKED.contains(&(name, occurrence)) {
                changed += check(name, callee, occurrence..occurrence + 1, &run);
            }
        }
    }
    changed
}

/// player-015: the full-throttle dive into the ground: frame 501 (normal
/// flight), 503 (ground contact: 80081564 effects, the crash check with
/// 80074D5C/80077EF4), 505 (mode 0x100000: crash handler 80072EDC).
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-015"]
fn crash_frames_80071358() {
    let changed = check("player-015", 0x8007_1358, 0..3, |m, _| {
        player::update_player_80071358(m);
        None
    });
    assert!(changed > 0);
    let changed = check("player-015", 0x8007_0af4, 0..3, |m, e| {
        player::player_effects_80070af4(m, e.gpr(4));
        None
    });
    assert!(changed > 0);
}

// ---- synthetic edge paths the captures cannot reach (digital pad, no rumble) ----

const CONTEXT_BLOCK: u32 = 0x8010_0000;
const VIBRATION_SLOTS: u32 = 0x8010_1000;

fn machine_with_context(pad_flags: u32) -> Machine {
    let mut m = Machine::default();
    m.set_gw(0x2dc, CONTEXT_BLOCK);
    m.set_gw(0x30, VIBRATION_SLOTS);
    m.sw(CONTEXT_BLOCK + 0x120, pad_flags);
    m
}

#[test]
fn scale_by_speed_below_the_cap() {
    let mut m = Machine::default();
    let vector = 0x8010_0000;
    m.sh(vector, 0x1000);
    m.sh(vector + 2, 0xf000);
    m.sh(vector + 4, 0x123);
    // 10752 / 21 = 512: x * 512 / 4096, rounded toward zero.
    player::scale_by_speed_8006d218(&mut m, 10752, vector);
    assert_eq!(m.lh(vector), 0x200);
    assert_eq!(m.lh(vector + 2), -0x200);
    assert_eq!(m.lh(vector + 4), 0x24);
    // -0x1001 * 512 / 4096 = -512.125 -> -512.
    m.sh(vector, 0xefff);
    player::scale_by_speed_8006d218(&mut m, 10752, vector);
    assert_eq!(m.lh(vector), -0x200);
    assert!(m.take_fault().is_ok());
}

#[test]
fn control_step_analog_axes() {
    let mut m = Machine::default();
    let (input, rates, out, controls) = (0x8010_0000, 0x8010_0100, 0x8010_0200, 0x8010_0300);
    m.sw(input + 0x18, 0xf);
    m.sw(input + 0x1c, 0x800);
    m.sw(input + 0x20, -0x400i32 as u32);
    m.sw(input + 0x24, 0x200);
    m.sw(input + 0x28, -0x1000i32 as u32);
    player::control_step_8006b90c(&mut m, input, rates, out, controls);
    let half = |m: &mut Machine, a: u32| m.lh(a);
    assert_eq!(half(&mut m, controls), -0x102);
    assert_eq!(half(&mut m, controls + 2), 0x202);
    assert_eq!(half(&mut m, controls + 4), 0x82);
    assert_eq!(half(&mut m, controls + 6), 0x800);
    assert_eq!(half(&mut m, controls + 8), 0x80);
    for (offset, value) in [(0, 0x100), (2, 0x80), (4, -0x80), (6, -0x80)] {
        assert_eq!(half(&mut m, rates + offset), value, "rate +{offset}");
    }
    for (offset, value) in [(4, 0x100), (6, 0x80), (8, -0x80), (0xe, -0x80)] {
        assert_eq!(half(&mut m, out + offset), value, "out +{offset}");
    }
    assert!(m.take_fault().is_ok());
}

#[test]
fn vibration_slots_with_rumble() {
    let mut m = machine_with_context(3);
    // Weapon kind 6: priority 9, byte 2 = 0xFF, byte 0 = 2.
    player::vibrate_weapon_80029028(&mut m, 6);
    assert_eq!(m.read_bytes(VIBRATION_SLOTS, 3), [2, 9, 0xff]);
    // Kinds 7 and 0xB do nothing.
    player::vibrate_weapon_80029028(&mut m, 7);
    assert_eq!(m.lbu(VIBRATION_SLOTS + 4), 0);
    // Gun kind 3 takes the third table pair and keeps priority 3 in the
    // second slot (the first holds priority 9).
    m.write_bytes(
        0x8001_036c,
        &[0x10, 0x11, 0x20, 0x21, 0x30, 0x31, 0, 0, 0, 0, 0, 0],
    );
    player::vibrate_gun_800290a4(&mut m, 3);
    assert_eq!(m.read_bytes(VIBRATION_SLOTS + 4, 3), [0x31, 3, 0x30]);
    // Equal priority re-claims that slot: kind 2 -> second pair, byte 1 = 1.
    player::vibrate_gun_800290a4(&mut m, 2);
    assert_eq!(m.read_bytes(VIBRATION_SLOTS + 4, 3), [0x21, 1, 0x20]);
    // Out-of-range kinds clear byte 0 of the claimed slot.
    player::vibrate_gun_800290a4(&mut m, 9);
    assert_eq!(m.read_bytes(VIBRATION_SLOTS + 4, 2), [0, 3]);
    assert!(m.take_fault().is_ok());
}

#[test]
fn vibration_slot_priorities() {
    let mut m = machine_with_context(1);
    for slot in 0..6 {
        m.sb(VIBRATION_SLOTS + slot * 4, 1);
        m.sb(VIBRATION_SLOTS + slot * 4 + 1, 9);
    }
    assert_eq!(player::claim_vibration_80028f7c(&mut m, 5), 0);
    // A request below 0x20 does nothing; above, a busy table keeps it out.
    player::vibrate_80028fd0(&mut m, 0x1f);
    player::vibrate_80028fd0(&mut m, 0x2000);
    assert_eq!(m.lbu(VIBRATION_SLOTS + 2), 0);
    m.sb(VIBRATION_SLOTS + 0x14, 0);
    // Strength 0x2000 >> 5 = 0x100 -> capped to 0x69 in the last slot.
    player::vibrate_80028fd0(&mut m, 0x2000);
    assert_eq!(m.read_bytes(VIBRATION_SLOTS + 0x14, 3), [1, 5, 0x69]);
    assert!(m.take_fault().is_ok());
}

#[test]
fn projectile_slots() {
    let mut m = Machine::default();
    let block = 0x8011_0000;
    let table = 0x8012_0000;
    m.set_gw(0x514, block);
    m.set_gw(0x2d8, table);
    // Queue full (8 pending) or no free slot: 0.
    m.sw(block + 0x154c, 8);
    assert_eq!(player::queue_bullet_80069864(&mut m, 1, 2, 3, 4), 0);
    m.sw(block + 0x154c, 0);
    for word in 0..4 {
        m.sw(block + 0x33d0 + word * 4, u32::MAX);
    }
    assert_eq!(player::queue_bullet_80069864(&mut m, 1, 2, 3, 4), 0);
    // The third word's first free bit is 0: slot 0x40.
    m.sw(block + 0x33d8, 0);
    assert_eq!(player::queue_bullet_80069864(&mut m, 1, 2, 3, 4), 1);
    assert_eq!(m.lhu(block + 0x1550), 0x40);
    assert_eq!(m.lw(block + 0x33d8), 1);
    // Projectile with a target in the upper lock word (index 0x23).
    m.sw(block + 0x1548, 0b0111);
    m.sw(table + 0x23 * 4, 0x8013_0000);
    let origin = 0x8014_0000;
    let velocity = 0x8014_0100;
    let p = player::allocate_projectile_80066df0(&mut m, 5, origin, velocity, 0x23);
    assert_eq!(p, block + 0xc8 + 3 * 0xa4);
    assert_eq!(m.lw(block + 4), 1 << 3);
    assert_eq!(m.lbu(block + 0x23 + 8), 3);
    assert_eq!(m.lbu(block + 0x23 + 0x88), 1);
    assert_eq!(m.lw(p + 0x74), 0x8013_0008);
    assert_eq!(m.lw(p + 0x78), 0x8013_0038);
    // Without a target the aim point is the projectile itself (flag 1).
    let p = player::allocate_projectile_80066df0(&mut m, 5, origin, velocity, u32::MAX);
    assert_eq!(m.lw(p) & 1, 1);
    assert_eq!(m.lw(p + 0x74), p + 0x1c);
    m.sw(block + 0x1548, u32::MAX);
    assert_eq!(
        player::allocate_projectile_80066df0(&mut m, 5, origin, velocity, 0),
        0
    );
    assert!(m.take_fault().is_ok());
}

// ---- group A: 80044D50, 800501C8, 80045950 subtrees ----
// Every test runs player-001, player-004 and player-007 (two occurrences
// each; player-008 adds the lock-slot release frame for 80044D50/800453F8).
// Routines that all captures reach without the original changing a byte (no
// hits, ground units or entity links in play) assert a zero count through
// `unexercised_group_a`, so a richer capture flags them for a real
// non-vacuity assertion.

/// player-001, player-004 and player-007, two occurrences each; returns the
/// changed byte total.
fn check_group_a(callee: u32, run: impl Fn(&mut Machine, &Snapshot) -> Option<u32>) -> usize {
    ["player-001", "player-004", "player-007"]
        .into_iter()
        .map(|name| check(name, callee, 0..2, &run))
        .sum()
}

/// player-015 (the full-throttle dive into the ground): flight frames 501,
/// 503 and 505 (crash); returns the changed byte total.
fn check_crash_group_a(callee: u32, run: impl Fn(&mut Machine, &Snapshot) -> Option<u32>) -> usize {
    check("player-015", callee, 0..3, run)
}

/// The captures run `callee` without the original changing any byte.
fn unexercised_group_a(callee: u32, changed: usize) {
    assert_eq!(
        changed, 0,
        "{callee:08x} now changes bytes: assert changed > 0 instead"
    );
}

/// Words `address..address + 4 * count` of the return snapshot matching entry
/// `occurrence` of `callee` (for results on the stack, which `check_call`
/// does not compare).
fn return_words_group_a(
    name: &str,
    callee: u32,
    occurrence: usize,
    address: u32,
    count: u32,
) -> Vec<u32> {
    let index_file = capture(name);
    let index = snapshot::read_index(&index_file).expect("snapshot index");
    let entry = index
        .iter()
        .filter(|s| s.callee == callee && s.stage == "entry")
        .nth(occurrence)
        .expect("entry snapshot");
    let exit = index
        .iter()
        .find(|s| {
            s.callee == callee
                && s.stage == "return"
                && s.sequence > entry.sequence
                && s.ra == entry.ra
                && s.registers.gpr[29] == entry.registers.gpr[29]
        })
        .expect("return snapshot");
    let exit = Snapshot::load(index_file.parent().expect("directory"), exit).expect("snapshot");
    let mut machine = exit.machine();
    (0..count).map(|i| machine.lw(address + i * 4)).collect()
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004, player-007, player-015"]
fn tick_hit_timers_80044d88() {
    // No hit timer runs (state bit 0x4) in any capture.
    let run = |m: &mut Machine, _: &Snapshot| {
        player::tick_hit_timers_80044d88(m);
        None
    };
    let changed = check_group_a(0x8004_4d88, run) + check_crash_group_a(0x8004_4d88, run);
    unexercised_group_a(0x8004_4d88, changed);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004, player-007, player-015"]
fn apply_damage_80044ef8() {
    // Slots 2..9 pass the entry checks, but carry no hit bits (also in the
    // player-015 crash frames).
    let run = |m: &mut Machine, _: &Snapshot| {
        player::apply_damage_80044ef8(m);
        None
    };
    let changed = check_group_a(0x8004_4ef8, run) + check_crash_group_a(0x8004_4ef8, run);
    unexercised_group_a(0x8004_4ef8, changed);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004, player-007"]
fn propagate_link_damage_8004ac74() {
    // No entity links ([gp+0x36D] = 0).
    let changed = check_group_a(0x8004_ac74, |m, _| {
        player::propagate_link_damage_8004ac74(m);
        None
    });
    unexercised_group_a(0x8004_ac74, changed);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004, player-007, player-008, player-015"]
fn update_lock_slots_800453f8() {
    // player-001 frame 307 opens the lock slots of slots 2..9 (8 bytes);
    // player-008 frame 737 releases a lock target through 80045874 (script).
    let run = |m: &mut Machine, _: &Snapshot| {
        player::update_lock_slots_800453f8(m);
        None
    };
    let changed = check_group_a(0x8004_53f8, run)
        + check("player-008", 0x8004_53f8, 0..1, run)
        + check_crash_group_a(0x8004_53f8, run);
    assert!(changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004, player-007, player-008, player-015"]
fn update_damage_80044d50() {
    let run = |m: &mut Machine, _: &Snapshot| {
        player::update_damage_80044d50(m);
        None
    };
    let changed = check_group_a(0x8004_4d50, run)
        + check("player-008", 0x8004_4d50, 0..1, run)
        + check_crash_group_a(0x8004_4d50, run);
    assert!(changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004, player-007"]
fn mark_units_8004fe70() {
    // No ground unit has entity flag 0x4000 or 0x8000.
    let changed = check_group_a(0x8004_fe70, |m, _| {
        player::mark_units_8004fe70(m);
        None
    });
    unexercised_group_a(0x8004_fe70, changed);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004, player-007"]
fn update_unit_sound_80050214() {
    // No voice and no qualifying unit: only the nearest-unit reset, which
    // already holds 0xFF / 0x100000.
    let changed = check_group_a(0x8005_0214, |m, _| {
        player::update_unit_sound_80050214(m);
        None
    });
    unexercised_group_a(0x8005_0214, changed);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004, player-007"]
fn update_units_8004fdf0() {
    let changed = check_group_a(0x8004_fdf0, |m, _| {
        player::update_units_8004fdf0(m);
        None
    });
    unexercised_group_a(0x8004_fdf0, changed);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004, player-007"]
fn move_units_8004ff54() {
    let changed = check_group_a(0x8004_ff54, |m, _| {
        player::move_units_8004ff54(m);
        None
    });
    unexercised_group_a(0x8004_ff54, changed);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004, player-007, player-015"]
fn update_ground_units_800501c8() {
    let run = |m: &mut Machine, _: &Snapshot| {
        player::update_ground_units_800501c8(m);
        None
    };
    let changed = check_group_a(0x8005_01c8, run) + check_crash_group_a(0x8005_01c8, run);
    unexercised_group_a(0x8005_01c8, changed);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004, player-007"]
fn update_linked_entities_8004a784() {
    let changed = check_group_a(0x8004_a784, |m, _| {
        player::update_linked_entities_8004a784(m);
        None
    });
    unexercised_group_a(0x8004_a784, changed);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004, player-007"]
fn copy_link_records_8004ae68() {
    let changed = check_group_a(0x8004_ae68, |m, _| {
        player::copy_link_records_8004ae68(m);
        None
    });
    unexercised_group_a(0x8004_ae68, changed);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-007"]
fn relative_vector_8004444c() {
    // Both player-007 calls come from 80048xxx with `out` on the stack, so
    // the result vector is compared with the return snapshot's stack here.
    for occurrence in 0..2 {
        let result = std::cell::Cell::new((0, [0; 3]));
        check(
            "player-007",
            0x8004_444c,
            occurrence..occurrence + 1,
            |m, e| {
                let out = e.gpr(6);
                let v0 = player::relative_vector_8004444c(m, e.gpr(4), e.gpr(5), out);
                result.set((out, [m.lw(out), m.lw(out + 4), m.lw(out + 8)]));
                Some(v0)
            },
        );
        let (out, words) = result.get();
        assert_ne!(words, [0; 3]);
        assert_eq!(
            return_words_group_a("player-007", 0x8004_444c, occurrence, out, 3),
            words
        );
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-007"]
fn view_vector_800444dc() {
    let changed = check("player-007", 0x8004_44dc, 0..2, |m, e| {
        Some(player::view_vector_800444dc(
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
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-007"]
fn off_axis_angle_80044548() {
    // v0 is the angle (0x1FBE and 0x1FCC in player-007).
    let angles = std::cell::RefCell::new(Vec::new());
    check("player-007", 0x8004_4548, 0..2, |m, e| {
        let v0 = player::off_axis_angle_80044548(m, e.gpr(4));
        angles.borrow_mut().push(v0);
        Some(v0)
    });
    assert!(angles.into_inner().iter().all(|&angle| angle != 0));
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004, player-007"]
fn update_home_records_80044a70() {
    // player-007: home and target records of the enemies in range.
    let changed = check_group_a(0x8004_4a70, |m, _| {
        Some(player::update_home_records_80044a70(m))
    });
    assert!(changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004, player-007"]
fn tick_record_timers_80044c20() {
    // No sight record carries timer flags 0x40/0x8/0x4.
    let changed = check_group_a(0x8004_4c20, |m, _| {
        Some(player::tick_record_timers_80044c20(m))
    });
    unexercised_group_a(0x8004_4c20, changed);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-004, player-007"]
fn record_flight_path_8004790c() {
    // Called only in active flight; counts calls (no sample frame).
    let run = |m: &mut Machine, _: &Snapshot| {
        player::record_flight_path_8004790c(m);
        None
    };
    let changed =
        check("player-004", 0x8004_790c, 0..2, run) + check("player-007", 0x8004_790c, 0..2, run);
    assert!(changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004, player-007"]
fn update_sight_records_80044730() {
    // Reaches shared 800306AC and 800827B4.
    let changed = check_group_a(0x8004_4730, |m, _| {
        Some(player::update_sight_records_80044730(m))
    });
    assert!(changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004, player-007"]
fn update_sight_800446f8() {
    let changed = check_group_a(0x8004_46f8, |m, _| Some(player::update_sight_800446f8(m)));
    assert!(changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004, player-007, player-015"]
fn update_sensors_80045950() {
    // player-015: frames 501/503 in flight, 505 after the crash.
    let run = |m: &mut Machine, _: &Snapshot| {
        player::update_sensors_80045950(m);
        None
    };
    let changed = check_group_a(0x8004_5950, run) + check_crash_group_a(0x8004_5950, run);
    assert!(changed > 0);
}

// ---- end of group A ----

// ---- group B: player effects (80070AF4 subtree) ---------------------------------

/// Run `run` on occurrences 0 and 1 of `callee` in player-001 and player-004;
/// returns the bytes the original changed in each capture.
fn both(callee: u32, run: impl Fn(&mut Machine, &Snapshot) -> Option<u32>) -> [usize; 2] {
    [
        check("player-001", callee, 0..2, &run),
        check("player-004", callee, 0..2, &run),
    ]
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004"]
fn gun_override_8003a0f0() {
    both(0x8003_a0f0, |_, _| {
        let v0 = player::gun_override_8003a0f0();
        assert_eq!(v0, 0xffff_ffff);
        Some(v0)
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004"]
fn pitch_add_8002e230() {
    both(0x8002_e230, |_, e| {
        let v0 = player::pitch_add_8002e230(e.gpr(4), e.gpr(5));
        assert_ne!(v0, 0);
        Some(v0)
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004"]
fn set_voice_pitch_8002d9fc() {
    let changed = both(0x8002_d9fc, |m, e| {
        let v0 = player::set_voice_pitch_8002d9fc(m, e.gpr(4), e.gpr(5));
        assert_eq!(v0, e.gpr(4));
        Some(v0)
    });
    assert!(changed.iter().sum::<usize>() > 0, "{changed:?}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004"]
fn integrate_position_8006f3b0() {
    let changed = both(0x8006_f3b0, |m, e| {
        player::integrate_position_8006f3b0(m, e.gpr(4));
        None
    });
    assert!(changed.iter().all(|&n| n > 0), "{changed:?}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004"]
fn tick_reload_timers_8007025c() {
    // Vacuous on these captures: all 24 timers are 0, the original changes
    // nothing (needs a capture a few frames after a missile launch).
    both(0x8007_025c, |m, _| {
        player::tick_reload_timers_8007025c(m);
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004"]
fn fill_free_slots_8007021c() {
    // player-004 #1 is the call from the weapon handler 800702CC.
    let changed = both(0x8007_021c, |m, e| {
        let v0 = player::fill_free_slots_8007021c(m, e.gpr(4), e.gpr(5), e.gpr(6));
        assert_ne!(v0, 0);
        Some(v0)
    });
    assert!(changed.iter().sum::<usize>() > 0, "{changed:?}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004"]
fn apply_attitude_8006f1c8() {
    // Vacuous on these captures: entity +0x54 has no request bits (16..23),
    // so the original rewrites +0x54 unchanged (needs a capture with an
    // attitude request).
    both(0x8006_f1c8, |m, e| {
        player::apply_attitude_8006f1c8(m, e.gpr(4), e.gpr(5));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004"]
fn collide_entities_8006f3fc() {
    // Vacuous on these captures: no entity is within contact range, the
    // original changes nothing (needs a capture with a player contact).
    both(0x8006_f3fc, |m, e| {
        player::collide_entities_8006f3fc(m, e.gpr(4), e.gpr(5));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004"]
fn world_collision_8006f670() {
    // Vacuous on these captures: inside the world bounds and above the
    // terrain-test height 0xFF448000, the original changes nothing (needs a
    // capture near the ground or a world bound).
    both(0x8006_f670, |m, e| {
        player::world_collision_8006f670(m, e.gpr(4), e.gpr(5));
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004"]
fn ground_height_8006fb78() {
    // Blocked until the ground ray 80098B80 (group C) is translated.
    let changed = both(0x8006_fb78, |m, e| {
        Some(player::ground_height_8006fb78(
            m,
            e.gpr(4),
            e.gpr(5),
            e.gpr(6),
        ))
    });
    assert!(changed.iter().sum::<usize>() > 0, "{changed:?}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004"]
fn engine_voices_8002ffac() {
    // Blocked by 8002D724 (first call) and the shared 8002DAA0.
    let changed = both(0x8002_ffac, |m, e| {
        player::engine_voices_8002ffac(m, e.gpr(4));
        None
    });
    assert!(changed.iter().sum::<usize>() > 0, "{changed:?}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004"]
fn engine_sound_80070a90() {
    // Blocked like 8002FFAC.
    let changed = both(0x8007_0a90, |m, _| {
        player::engine_sound_80070a90(m);
        None
    });
    assert!(changed.iter().sum::<usize>() > 0, "{changed:?}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004"]
fn player_effects_80070af4() {
    // Blocked by 80098B80 (group C), the shared 8002DAA0/8003A08C/8003A0F8,
    // and on player-004 #1 by 80070A50 (coordinator).
    let changed = both(0x8007_0af4, |m, e| {
        player::player_effects_80070af4(m, e.gpr(4));
        None
    });
    assert!(changed.iter().all(|&n| n > 0), "{changed:?}");
}

// ---- end of group B ------------------------------------------------------------

// ---- group C ----------------------------------------------------------------------

/// The original's `len` bytes at `address` in the return snapshot paired
/// with entry `occurrence` of `callee` (as `snapshot::check_call` pairs
/// them), and the same bytes at entry. For outputs on the stack, which the
/// generic comparison excludes.
fn stack_bytes(
    name: &str,
    callee: u32,
    occurrence: usize,
    address: u32,
    len: usize,
) -> (Vec<u8>, Vec<u8>) {
    let index_file = capture(name);
    let directory = index_file.parent().expect("capture directory");
    let index = snapshot::read_index(&index_file).expect("index");
    let entry = index
        .iter()
        .filter(|s| s.callee == callee && s.stage == "entry")
        .nth(occurrence)
        .expect("entry snapshot");
    let exit = index
        .iter()
        .find(|s| {
            s.callee == callee
                && s.stage == "return"
                && s.sequence > entry.sequence
                && s.ra == entry.ra
                && s.registers.gpr[29] == entry.registers.gpr[29]
        })
        .expect("return snapshot");
    let at = (address & 0x1f_ffff) as usize;
    let load = |info: &snapshot::SnapshotInfo| {
        Snapshot::load(directory, info).expect("snapshot").ram[at..at + len].to_vec()
    };
    (load(entry), load(exit))
}

/// Run `run` on each capture/occurrence of `callee` (RAM/scratchpad/GTE/v0
/// through `check`) and also require the `len` stack bytes at register
/// `register` of the entry to match the return snapshot. Returns the bytes
/// changed outside the stack and the stack output bytes that changed.
fn check_with_stack_output(
    callee: u32,
    register: usize,
    len: usize,
    run: impl Fn(&mut Machine, &Snapshot) -> Option<u32>,
) -> (usize, usize) {
    let (mut changed, mut stack_changed) = (0, 0);
    for name in ["player-001", "player-004"] {
        for occurrence in 0..2 {
            let output = std::cell::RefCell::new((0, Vec::new()));
            changed += check(name, callee, occurrence..occurrence + 1, |m, e| {
                let result = run(m, e);
                let address = e.gpr(register);
                *output.borrow_mut() = (address, m.read_bytes(address, len));
                result
            });
            let (address, actual) = output.into_inner();
            let (before, expected) = stack_bytes(name, callee, occurrence, address, len);
            assert_eq!(
                actual, expected,
                "{name} #{occurrence}: stack output at {address:#010x}"
            );
            stack_changed += before.iter().zip(&expected).filter(|(a, b)| a != b).count();
        }
    }
    (changed, stack_changed)
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004"]
fn transform_vertices_80097d98() {
    // Rotated block vertices go to the scratchpad work buffer (a2).
    let changed: usize = ["player-001", "player-004"]
        .into_iter()
        .map(|name| {
            check(name, 0x8009_7d98, 0..2, |m, e| {
                player::transform_vertices_80097d98(m, e.gpr(4), e.gpr(5), e.gpr(6));
                None
            })
        })
        .sum();
    assert!(changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004"]
fn direction_matrix_80097a70() {
    // The matrix (a0) is a stack local of 80097EDC: compare its rotation
    // halfwords (and the GTE registers through `check`).
    let (_, stack_changed) = check_with_stack_output(0x8009_7a70, 4, 0x12, |m, e| {
        player::direction_matrix_80097a70(m, e.gpr(4), e.gpr(5));
        None
    });
    assert!(stack_changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004"]
fn ground_hit_80097edc() {
    // Fifth argument: the caller's stack word +0x10. The hit point (a1) is
    // a stack local of 80098B80; v0 (the hit distance) is compared too.
    let (_, stack_changed) = check_with_stack_output(0x8009_7edc, 5, 12, |m, e| {
        let list = m.lw(e.gpr(29).wrapping_add(0x10));
        Some(player::ground_hit_80097edc(
            m,
            e.gpr(4),
            e.gpr(5),
            e.gpr(6),
            e.gpr(7),
            list,
        ))
    });
    assert!(stack_changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004"]
fn ground_ray_80098b80() {
    // The result point (a2) is a stack local of the caller; v0 (the hit
    // distance) is compared too.
    let (_, stack_changed) = check_with_stack_output(0x8009_8b80, 6, 12, |m, e| {
        Some(player::ground_ray_80098b80(
            m,
            e.gpr(4),
            e.gpr(5),
            e.gpr(6),
            e.gpr(7),
        ))
    });
    assert!(stack_changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004"]
fn vector_angles_80031970() {
    let changed: usize = ["player-001", "player-004"]
        .into_iter()
        .map(|name| {
            check(name, 0x8003_1970, 0..2, |m, e| {
                player::vector_angles_80031970(m, e.gpr(4), e.gpr(5));
                None
            })
        })
        .sum();
    assert!(changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004"]
fn orientation_80030d20() {
    let changed: usize = ["player-001", "player-004"]
        .into_iter()
        .map(|name| {
            check(name, 0x8003_0d20, 0..2, |m, e| {
                player::orientation_80030d20(m, e.gpr(4), e.gpr(5));
                None
            })
        })
        .sum();
    assert!(changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004"]
fn aim_camera_8006d7dc() {
    let changed: usize = ["player-001", "player-004"]
        .into_iter()
        .map(|name| {
            check(name, 0x8006_d7dc, 0..2, |m, e| {
                player::aim_camera_8006d7dc(m, e.gpr(4), e.gpr(5));
                None
            })
        })
        .sum();
    assert!(changed > 0);
}

// ---- end of group C ---------------------------------------------------------------

// ---- group D: flight model (8006D8D4, 8006D42C, 8006D2AC) ----------------------

/// The return snapshot `check_call` pairs with entry `occurrence` of `callee`.
fn return_snapshot(name: &str, callee: u32, occurrence: usize) -> Snapshot {
    let index_file = capture(name);
    let index = snapshot::read_index(&index_file).expect("index");
    let entry = index
        .iter()
        .filter(|s| s.callee == callee && s.stage == "entry")
        .nth(occurrence)
        .expect("entry snapshot");
    let exit = index
        .iter()
        .find(|s| {
            s.callee == callee
                && s.stage == "return"
                && s.sequence > entry.sequence
                && s.ra == entry.ra
                && s.registers.gpr[29] == entry.registers.gpr[29]
        })
        .expect("return snapshot");
    Snapshot::load(index_file.parent().expect("directory"), exit).expect("load")
}

/// Word of a snapshot's RAM image.
fn ram_word(snapshot: &Snapshot, address: u32) -> u32 {
    let at = (address & 0x1f_ffff) as usize;
    u32::from_le_bytes(snapshot.ram[at..at + 4].try_into().expect("word"))
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004"]
fn altitude_limits_8006d42c() {
    // Outputs go to the caller's frame (a3, [sp+0x10], [sp+0x14]), which the
    // RAM comparison excludes: compare those three words explicitly.
    for (name, count) in [
        ("player-001", 2),
        ("player-004", 2),
        ("player-009", 25),
        ("player-010", 37),
    ] {
        for occurrence in 0..count {
            let exit = return_snapshot(name, 0x8006_d42c, occurrence);
            check(name, 0x8006_d42c, occurrence..occurrence + 1, |m, e| {
                let sp = e.gpr(29);
                let (lift_out, cruise_out, max_out) = (e.gpr(7), m.lw(sp + 0x10), m.lw(sp + 0x14));
                let v0 = player::altitude_limits_8006d42c(
                    m,
                    e.gpr(4),
                    e.gpr(5),
                    e.gpr(6),
                    lift_out,
                    cruise_out,
                    max_out,
                );
                for address in [lift_out, cruise_out, max_out] {
                    assert_eq!(
                        m.lw(address),
                        ram_word(&exit, address),
                        "{name} #{occurrence}: output word {address:08x}"
                    );
                }
                assert_ne!(v0, 0);
                Some(v0)
            });
        }
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004"]
fn speed_change_8006d2ac() {
    // Pure: seven arguments (three on the caller's stack) -> v0.
    for (name, count) in [
        ("player-001", 2),
        ("player-004", 2),
        ("player-009", 25),
        ("player-010", 37),
    ] {
        check(name, 0x8006_d2ac, 0..count, |m, e| {
            let sp = e.gpr(29);
            let v0 = player::speed_change_8006d2ac(
                e.gpr(4),
                e.gpr(5),
                e.gpr(6),
                e.gpr(7),
                m.lw(sp + 0x10),
                m.lw(sp + 0x14),
                m.lw(sp + 0x18),
            );
            assert_ne!(v0, 0);
            Some(v0)
        });
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, -004, -005, -009"]
fn flight_model_8006d8d4() {
    // player-001: frame 307 (mode 2) and 309, no input; player-004: frames
    // 191/193 with UP+LEFT held (pitch and roll sticks at -0x1000);
    // player-005: one button per two frames; player-009: long holds (yaw +
    // pull-up, brake + pull-up, full throttle + roll, full throttle + push).
    for (name, count) in SWEEP {
        let changed = check(name, 0x8006_d8d4, 0..count, |m, e| {
            player::flight_model_8006d8d4(m, e.gpr(4), e.gpr(5), e.gpr(6), e.gpr(7));
            None
        });
        assert!(changed > 0, "{name}: original changed no bytes");
    }
}
// ---- end of group D ----

// ---- input sweeps over whole player-effect frames ----------------------------------

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-005, -009, -010"]
fn player_effects_80070af4_sweep() {
    let captures = [("player-005", 25), ("player-009", 25), ("player-010", 37)];
    let changed = sweep(&captures, 0x8007_0af4, |m, e| {
        player::player_effects_80070af4(m, e.gpr(4));
        None
    });
    assert!(changed > 0);
}

/// player-011: reload timers counting down after the frame-193 launch, and
/// gameplay +0xC = 120 at frame 271 (the flight-path sample of 8004790C).
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-011"]
fn timers_and_flight_path_sample() {
    let changed = check("player-011", 0x8007_025c, 0..2, |m, _| {
        player::tick_reload_timers_8007025c(m);
        None
    });
    assert!(changed >= 2, "{changed}");
    let changed = check("player-011", 0x8004_790c, 0..2, |m, _| {
        player::record_flight_path_8004790c(m);
        None
    });
    assert!(changed > 4, "{changed}");
    for (callee, run) in [
        (
            0x8004_5950,
            player::update_sensors_80045950 as fn(&mut Machine),
        ),
        (0x8007_1358, player::update_player_80071358),
    ] {
        let changed = check("player-011", callee, 0..2, |m, _| {
            run(m);
            None
        });
        assert!(changed > 0);
    }
    let changed = check("player-011", 0x8007_0af4, 0..2, |m, e| {
        player::player_effects_80070af4(m, e.gpr(4));
        None
    });
    assert!(changed > 0);
}

// ==== e ====
// 8006BEA8, 80072494, 800729F8 and 8006D184 run only in the autopilot and
// scripted modes of 80071358 (0x4000/0x8000, 0x200000/0x400000,
// 0x1000000..0x8000000) and on the context +0x128 < 0 path of 800712B4. No
// capture reaches them yet (the player-012/013 mode watch saw modes 1, 2,
// 4 and 0x100000 only), so these tests check every path against values
// worked out from the instructions; the reciprocal multiplies are checked
// against plain integer division (x * 128 / 200, / 220, / 140, speed / 21,
// all rounded toward zero).

const E_CONTEXT: u32 = 0x8010_0000;
const E_INPUT: u32 = E_CONTEXT + 0x120;
const E_ENTITY: u32 = 0x8010_1000;
const E_TABLE: u32 = 0x8010_1100;
const E_STATE: u32 = 0x8010_1200;
const E_RATES: u32 = 0x8010_1300;
const E_OUT: u32 = 0x8010_1400;
const E_CONTROLS: u32 = 0x8010_1500;
/// Machine RAM below the stack region (`check_call` compares the same).
const E_BELOW_STACK: usize = 0x1f_e000;

/// The player entity (`[[gp+0x2D8]]`) with pitch (+0x18), roll (+0x1C) and
/// height (+0xC); context `[gp+0x2DC]`, state `[gp+0x528]` and the control
/// blocks `[gp+0x51C]`/`[gp+0x520]`/`[gp+0x524]` all zero.
fn e_machine(pitch: i16, roll: i16, y: u32) -> Machine {
    let mut m = Machine::default();
    m.set_gw(0x2dc, E_CONTEXT);
    m.set_gw(0x2d8, E_TABLE);
    m.set_gw(0x528, E_STATE);
    m.set_gw(0x51c, E_RATES);
    m.set_gw(0x520, E_OUT);
    m.set_gw(0x524, E_CONTROLS);
    m.sw(E_TABLE, E_ENTITY);
    m.sw(E_ENTITY + 0xc, y);
    m.sh(E_ENTITY + 0x18, pitch as u16);
    m.sh(E_ENTITY + 0x1c, roll as u16);
    m
}

fn e_halves(m: &mut Machine, base: u32, offsets: &[u32]) -> Vec<i32> {
    offsets.iter().map(|&o| i32::from(m.lh(base + o))).collect()
}

fn e_set_halves(m: &mut Machine, base: u32, values: &[(u32, i32)]) {
    for &(offset, value) in values {
        m.sh(base + offset, value as u16);
    }
}

/// Rates (+0/+2/+4/+6), controls (+0/+2/+4/+6/+8), outputs (+4/+6/+8/+0xE).
fn e_blocks(m: &mut Machine) -> [Vec<i32>; 3] {
    [
        e_halves(m, E_RATES, &[0, 2, 4, 6]),
        e_halves(m, E_CONTROLS, &[0, 2, 4, 6, 8]),
        e_halves(m, E_OUT, &[4, 6, 8, 0xe]),
    ]
}

fn e_same_ram(a: &Machine, b: &Machine) -> bool {
    a.ram()[..E_BELOW_STACK] == b.ram()[..E_BELOW_STACK]
}

#[test]
fn auto_level_80072494_paths() {
    // Level out (climb 0, below the ceiling): pitch target -0x100 * 16 =
    // -0x1000, roll target -0x200 * 4 = -0x800 (|roll| >= 0x1C7: not
    // halved), no throttle. Pitch, yaw and throttle rates decay; the yaw
    // rate is then clamped to 0.
    let mut m = e_machine(0x100, 0x200, 0);
    e_set_halves(&mut m, E_RATES, &[(0, 0x1100), (2, 0x100), (6, 0x800)]);
    player::auto_level_80072494(&mut m, E_ENTITY, E_RATES, E_OUT, E_CONTROLS, 0);
    let pitch_rate = 0x1100 * 128 / 200 - 0x1000 / 8;
    let throttle_rate = 0x800 * 128 / 140;
    assert_eq!(
        e_blocks(&mut m),
        [
            vec![pitch_rate, 0, -0x800 / 32, throttle_rate],
            vec![-0x202, -0x402, 0, (throttle_rate + 0x1000) >> 1, 0],
            vec![pitch_rate, 0, -0x800 / 32, throttle_rate],
        ]
    );
    assert!(m.take_fault().is_ok());

    // Climb (climb 1, below the ceiling, |roll| < 0x800): pitch target
    // (0x800 - 0x300) * 16 clamped to 0x1000, throttle 0x1000; roll target
    // 0x100 * 4 halved twice (|roll| < 0x111). The lever returns from
    // -0x100 by 0x80.
    let mut m = e_machine(0x300, -0x100, 0);
    m.sh(E_CONTROLS + 8, 0xff00);
    player::auto_level_80072494(&mut m, E_ENTITY, E_RATES, E_OUT, E_CONTROLS, 1);
    assert_eq!(
        e_blocks(&mut m),
        [
            vec![0x200, 0, 8, 0x80],
            vec![0x42, 0x402, 0, 0x800, -0x80],
            vec![0x200, 0, 8, 0x80],
        ]
    );

    // Above the ceiling (y < 0xFF448000) `climb` is ignored: pitch target
    // -0x50 * 16 halved (|pitch| < 0xB6) = -0x280; roll target -0x700 * 4
    // clamped to -0x1000. The pitch rate -0x300 decays towards the target.
    let mut m = e_machine(0x50, 0x700, 0xff40_0000);
    m.sh(E_RATES, -0x300i16 as u16);
    player::auto_level_80072494(&mut m, E_ENTITY, E_RATES, E_OUT, E_CONTROLS, 1);
    let pitch_rate = -0x300 * 128 / 200 - 0x280 / 8;
    assert_eq!(
        e_blocks(&mut m),
        [
            vec![pitch_rate, 0, -0x80, 0],
            vec![-0x402, -0xa2, 0, 0x800, 0],
            vec![pitch_rate, 0, -0x80, 0],
        ]
    );

    // Climbing at pitch >= 0x800: no pitch target, throttle still 0x1000.
    let mut m = e_machine(0x900, 0, 0);
    player::auto_level_80072494(&mut m, E_ENTITY, E_RATES, E_OUT, E_CONTROLS, 1);
    assert_eq!(
        e_blocks(&mut m),
        [
            vec![0, 0, 0, 0x80],
            vec![0, 0, 0, 0x800, 0],
            vec![0, 0, 0, 0x80]
        ]
    );

    // Climb with |roll| >= 0x800: no pitch or throttle target; roll target
    // -0x800 * 4 clamped to -0x1000.
    let mut m = e_machine(0x100, 0x800, 0);
    player::auto_level_80072494(&mut m, E_ENTITY, E_RATES, E_OUT, E_CONTROLS, 1);
    assert_eq!(
        e_blocks(&mut m),
        [
            vec![0, 0, -0x80, 0],
            vec![-0x402, 0, 0, 0x800, 0],
            vec![0, 0, -0x80, 0]
        ]
    );
    assert!(m.take_fault().is_ok());
}

#[test]
fn auto_climb_800729f8_paths() {
    // |roll| < 0x800: pitch target (0x800 - 0x900) * 16 = -0x1000, throttle
    // 0x1000; roll target -0x100 * 4 halved twice = -0x100. Rates decay
    // (yaw -0x40 -> -40, then clamped to 0; roll 0x200 -> 327; throttle
    // 0x1100 -> 3978) and clamp (roll to 0x100, throttle to 0x1000). The
    // yaw control overshoots 0 by the minimum step 2 and snaps back.
    let mut m = e_machine(0x900, 0x100, 0);
    e_set_halves(&mut m, E_RATES, &[(2, -0x40), (4, 0x200), (6, 0x1100)]);
    e_set_halves(
        &mut m,
        E_CONTROLS,
        &[(0, 0x10), (2, -0xffe), (4, 1), (8, 0x40)],
    );
    player::auto_climb_800729f8(&mut m, E_ENTITY, E_RATES, E_OUT, E_CONTROLS);
    let throttle_rate = 0x1100 * 128 / 140;
    assert_eq!(0x200 * 128 / 200 - 0x100 / 32, 319);
    assert_eq!(throttle_rate + 0x1000 / 32, 4106);
    assert_eq!(
        e_blocks(&mut m),
        [
            vec![-0x200, 0, 0x100, 0x1000],
            vec![-0x36, -0x1000, 0, (throttle_rate + 0x1000) >> 1, -0x40],
            vec![-0x200, 0, 0x100, 0x1000],
        ]
    );

    // |roll| >= 0x800: roll target only (0x900 * 4 clamped to 0x1000).
    let mut m = e_machine(0x100, -0x900, 0);
    player::auto_climb_800729f8(&mut m, E_ENTITY, E_RATES, E_OUT, E_CONTROLS);
    assert_eq!(
        e_blocks(&mut m),
        [
            vec![0, 0, 0x80, 0],
            vec![0x402, 0, 0, 0x800, 0],
            vec![0, 0, 0x80, 0]
        ]
    );

    // Pitch exactly 0x800: pitch target 0, throttle 0x1000; |roll| < 0x88
    // halves the roll target -0x100 three times.
    let mut m = e_machine(0x800, 0x40, 0);
    player::auto_climb_800729f8(&mut m, E_ENTITY, E_RATES, E_OUT, E_CONTROLS);
    assert_eq!(
        e_blocks(&mut m),
        [
            vec![0, 0, -1, 0x80],
            vec![-10, 0, 0, 0x800, 0],
            vec![0, 0, -1, 0x80]
        ]
    );
    assert!(m.take_fault().is_ok());
}

#[test]
fn control_step_8006bea8_pad() {
    // Digital pad (input +8: DOWN, R2, LEFT, L1), context +0x120 bit 2
    // clear: targets pitch +0x1000, yaw +0x1000, roll -0x1000, throttle
    // -0x1000 (lever target 0x1000); every rate integrates target >> 5.
    let mut m = e_machine(0, 0x20, 0);
    m.sw(
        E_INPUT + 8,
        0x0080_0000 | 0x4000_0000 | 0x0200_0000 | 0x1000_0000,
    );
    m.sh(E_RATES + 6, 0x400);
    player::control_step_8006bea8(&mut m, E_INPUT, E_RATES, E_OUT, E_CONTROLS);
    assert_eq!(
        e_blocks(&mut m),
        [
            vec![0x80, 0x80, -0x80, 0x380],
            vec![-0x402, 0x402, 0x402, 0xa00, 0x80],
            vec![0x80, 0x80, -0x80, 0x380],
        ]
    );
    assert_eq!(m.lh(E_ENTITY + 0x1c), 0x20);

    // Analog axes (mask 0xF) with context +0x120 bit 2: rates above their
    // targets decay to 58% (throttle 91%), then integrate and clamp to the
    // axis magnitudes; out +6 = roll rate, +8 = yaw rate + roll rate / 32;
    // the roll angle moves 0xC towards 0.
    let mut m = e_machine(0, 0x20, 0);
    m.sw(E_INPUT, 4);
    m.sw(E_INPUT + 0x18, 0xf);
    m.sw(E_INPUT + 0x1c, -0x200i32 as u32);
    m.sw(E_INPUT + 0x20, 0x300);
    m.sw(E_INPUT + 0x24, 0x100);
    m.sw(E_INPUT + 0x28, 0x800);
    e_set_halves(
        &mut m,
        E_RATES,
        &[(0, 0x400), (2, -0x200), (4, 0x100), (6, -0xc00)],
    );
    player::control_step_8006bea8(&mut m, E_INPUT, E_RATES, E_OUT, E_CONTROLS);
    let throttle = -0xc00 * 128 / 140;
    assert_eq!(0x400 * 128 / 220 - 0x200 / 32, 579); // clamped to 0x200
    assert_eq!(-0x200 * 128 / 220 + 0x100 / 32, -289); // clamped to -0x100
    let roll = 0x100 + 0x300 / 32;
    assert_eq!(
        e_blocks(&mut m),
        [
            vec![0x200, -0x100, roll, throttle + 0x800 / 32],
            vec![0xc2, -0x82, 0x42, (throttle + 0x1000) >> 1, 0],
            vec![0x200, roll, -0x100 + roll / 32, throttle + 0x800 / 32],
        ]
    );
    assert_eq!(m.lh(E_ENTITY + 0x1c), 0x14);

    // Bit 2 with saturated rates: out +8 = 0x1000 + 0x80 is clamped to
    // 0x1000 (and -0x1080 to -0x1000); roll -0x10 moves to -4, 5 stays.
    for (sign, roll_angle, roll_after) in [(1, -0x10, -4), (-1, 5, 5)] {
        let mut m = e_machine(0, roll_angle, 0);
        m.sw(E_INPUT, 4);
        m.sw(E_INPUT + 0x18, 0xf);
        m.sw(E_INPUT + 0x20, (sign * 0x1000) as u32);
        m.sw(E_INPUT + 0x24, (sign * 0x1000) as u32);
        e_set_halves(&mut m, E_RATES, &[(2, sign * 0xff0), (4, sign * 0xff0)]);
        player::control_step_8006bea8(&mut m, E_INPUT, E_RATES, E_OUT, E_CONTROLS);
        assert_eq!(
            e_blocks(&mut m),
            [
                vec![0, sign * 0x1000, sign * 0x1000, 0],
                vec![sign * 0x402, 0, sign * 0x402, 0x800, 0],
                vec![0, sign * 0x1000, sign * 0x1000, 0],
            ]
        );
        assert_eq!(i32::from(m.lh(E_ENTITY + 0x1c)), roll_after);
    }
    assert!(m.take_fault().is_ok());
}

#[test]
fn control_step_8006bea8_autopilot() {
    // Input +8 bit 31 with |roll| >= 0x223 or context +4 set: 80072494
    // (climb 0) in mode 0x1000000, else 800729F8, for the player entity;
    // |roll| 0x222 keeps the pad step (bit 31 is no button).
    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Path {
        Level,
        Climb,
        Pad,
    }
    for (mode, context_word, roll, path) in [
        (0x0100_0000, 0, 0x223, Path::Level),
        (0x0100_0000, 0, -0x300, Path::Level),
        (0x0200_0000, 0, -0x300, Path::Climb),
        (0x0400_0000, 1, 0x10, Path::Climb),
        (0x0100_0000, 1, 0, Path::Level),
        (0x0100_0000, 0, 0x222, Path::Pad),
    ] {
        let mut m = e_machine(0x300, roll, 0);
        m.sw(E_STATE + 4, mode);
        m.sw(E_CONTEXT + 4, context_word);
        m.sw(E_INPUT + 8, 0x8080_0000);
        e_set_halves(&mut m, E_RATES, &[(0, 0x200), (6, 0x300)]);
        let initial = m.clone();
        let mut expected = m.clone();
        // The pad step on the same state (input +8 without bit 31).
        let mut pad = m.clone();
        pad.sw(E_INPUT + 8, 0x0080_0000);
        player::control_step_8006bea8(&mut pad, E_INPUT, E_RATES, E_OUT, E_CONTROLS);
        pad.sw(E_INPUT + 8, 0x8080_0000);
        player::control_step_8006bea8(&mut m, E_INPUT, E_RATES, E_OUT, E_CONTROLS);
        assert_eq!(m.sp, ac3_flight::machine::INITIAL_SP);
        match path {
            Path::Level => {
                player::auto_level_80072494(&mut expected, E_ENTITY, E_RATES, E_OUT, E_CONTROLS, 0)
            }
            Path::Climb => {
                player::auto_climb_800729f8(&mut expected, E_ENTITY, E_RATES, E_OUT, E_CONTROLS)
            }
            Path::Pad => expected = pad.clone(),
        }
        let case = format!("{path:?} mode {mode:#x} roll {roll}");
        assert!(e_same_ram(&m, &expected), "{case}");
        assert!(!e_same_ram(&m, &initial), "{case}: unchanged");
        // The autopilot and the pad step disagree on these inputs.
        assert_eq!(e_same_ram(&m, &pad), path == Path::Pad, "{case}");
        assert!(m.take_fault().is_ok());
    }
}

#[test]
fn scale_outputs_by_speed_8006d184_paths() {
    // out +4/+6/+8 become x * (speed / 21) / 4096 (scale 0x1000 above speed
    // 0x14FFF), both divisions rounded toward zero; +2 and +0xA stay. The
    // same values through 8006D218 (the controls variant) agree.
    let values = [0x1000, -0x1000, 0x123];
    for speed in [21 * 512, 0x14fff, 0x15000, 0, 1000, -21, -100_000] {
        let scale = if speed > 0x14fff { 0x1000 } else { speed / 21 };
        let mut m = Machine::default();
        e_set_halves(&mut m, E_OUT, &[(2, 0x7777), (0xa, 0x5555)]);
        for (i, &value) in values.iter().enumerate() {
            m.sh(E_OUT + 4 + i as u32 * 2, value as u16);
            m.sh(E_CONTROLS + i as u32 * 2, value as u16);
        }
        player::scale_outputs_by_speed_8006d184(&mut m, speed as u32, E_OUT);
        player::scale_by_speed_8006d218(&mut m, speed as u32, E_CONTROLS);
        let expected: Vec<i32> = values.iter().map(|&x| x * scale / 4096).collect();
        assert_eq!(
            e_halves(&mut m, E_OUT, &[4, 6, 8]),
            expected,
            "speed {speed}"
        );
        assert_eq!(e_halves(&mut m, E_CONTROLS, &[0, 2, 4]), expected);
        assert_eq!(e_halves(&mut m, E_OUT, &[2, 0xa]), [0x7777, 0x5555]);
        assert!(m.take_fault().is_ok());
    }
}

#[test]
fn e_dispatch_and_callers() {
    // Dispatch: original a0..a3; 80072494 reads `climb` from [sp+0x10].
    let setup = || {
        let mut m = e_machine(0x300, -0x100, 0);
        m.sw(E_INPUT + 8, 0x0080_0000);
        e_set_halves(&mut m, E_OUT, &[(4, 0x1000), (6, 0x800), (8, -0x400)]);
        m
    };
    let args = [E_ENTITY, E_RATES, E_OUT, E_CONTROLS];
    for climb in [0, 1] {
        let mut direct = setup();
        let mut via = setup();
        player::auto_level_80072494(&mut direct, E_ENTITY, E_RATES, E_OUT, E_CONTROLS, climb);
        let sp = via.push_frame(0x28);
        via.sw(sp + 0x10, climb);
        ac3_flight::dispatch::call(&mut via, 0x8007_2494, args);
        via.pop_frame(0x28);
        assert!(e_same_ram(&direct, &via), "climb {climb}");
        // Throttle 0x1000 >> 5 only when climbing.
        assert_eq!(direct.lh(E_OUT + 0xe), if climb == 1 { 0x80 } else { 0 });
        assert!(via.take_fault().is_ok());
    }
    let mut direct = setup();
    let mut via = setup();
    player::auto_climb_800729f8(&mut direct, E_ENTITY, E_RATES, E_OUT, E_CONTROLS);
    ac3_flight::dispatch::call(&mut via, 0x8007_29f8, args);
    assert!(e_same_ram(&direct, &via) && !e_same_ram(&direct, &setup()));
    let mut direct = setup();
    let mut via = setup();
    player::control_step_8006bea8(&mut direct, E_INPUT, E_RATES, E_OUT, E_CONTROLS);
    ac3_flight::dispatch::call(&mut via, 0x8006_bea8, [E_INPUT, E_RATES, E_OUT, E_CONTROLS]);
    assert!(e_same_ram(&direct, &via) && !e_same_ram(&direct, &setup()));
    let mut direct = setup();
    let mut via = setup();
    player::scale_outputs_by_speed_8006d184(&mut direct, 21 * 512, E_OUT);
    ac3_flight::dispatch::call(&mut via, 0x8006_d184, [21 * 512, E_OUT, 0, 0]);
    assert!(e_same_ram(&direct, &via) && !e_same_ram(&direct, &setup()));
    assert!(via.take_fault().is_ok());

    // 800712B4 with context +0x128 < 0 runs 80072494 with climb 0.
    let mut m = setup();
    m.sw(E_CONTEXT + 0x128, 0x8000_0000);
    let mut expected = m.clone();
    player::control_input_800712b4(&mut m, E_ENTITY, 0);
    player::auto_level_80072494(&mut expected, E_ENTITY, E_RATES, E_OUT, E_CONTROLS, 0);
    assert!(e_same_ram(&m, &expected) && !e_same_ram(&m, &setup()));
    assert_eq!(m.sp, ac3_flight::machine::INITIAL_SP);
    assert!(m.take_fault().is_ok());
}
// ==== end of e ====

// ==== f ====
// 8006C58C (analog pad: context +0x120 bit 2) and 80073DB0 (80071358 mode
// 0x800) are reached by no capture: every captured pad is digital, and the
// mode-watch runs player-012/013 saw modes 1, 4 and 0x100000 only. Their
// cases come from a reference run of the original instruction words
// (analysis/main/disassembly.txt) by a MIPS I interpreter with R3000 load
// delays: a greedy cover of every reachable instruction (all but the
// compiler's `break` traps and clamps whose condition cannot hold) plus
// random cases. Each case must reproduce the whole RAM image.

const F_INPUT: u32 = 0x8010_0000;
const F_RATES: u32 = 0x8010_0100;
const F_OUT: u32 = 0x8010_0200;
const F_CONTROLS: u32 = 0x8010_0300;
const F_ENTITY: u32 = 0x8010_0400;
const F_MOTION: u32 = 0x8010_0500;
const F_STATE: u32 = 0x8010_0600;
const F_SINE: u32 = 0x8001_0498;

/// One 8006C58C case: inputs, then the reference results.
struct FAnalogCase {
    /// Input block +0x08 (buttons), +0x18 (axis mask), +0x1C..+0x28 (axes).
    pad: [u32; 6],
    rates: [u16; 4],
    controls: [u16; 5],
    /// Entity +0x18 (pitch), +0x1A (heading), +0x1C (bank).
    angles: [u16; 3],
    motion: [u32; 2],
    /// State +0x4C.
    factor: u16,
    /// The sine-table entry of the bank.
    sine: u16,
    expect_rates: [u16; 4],
    expect_controls: [u16; 5],
    /// Output +4, +6, +8, +0xE.
    expect_out: [u16; 4],
    expect_angles: [u16; 3],
}

/// One 80073DB0 case: inputs, then the reference results.
struct FScriptedCase {
    /// State +8 (phase) and +0xC (frame count).
    phase: [u32; 2],
    rates: [u16; 4],
    controls: [u16; 5],
    /// Entity +0x18 (pitch), +0x1A, +0x1C (bank).
    angles: [u16; 3],
    expect_phase: [u32; 2],
    expect_rates: [u16; 4],
    expect_controls: [u16; 5],
    /// Output +4, +6, +8, +0xE.
    expect_out: [u16; 4],
}

fn f_halves(m: &mut Machine, address: u32, values: &[u16]) {
    for (i, &value) in values.iter().enumerate() {
        m.sh(address + 2 * i as u32, value);
    }
}

fn f_outputs(m: &mut Machine, values: [u16; 4]) {
    for (offset, value) in [4, 6, 8, 0xe].into_iter().zip(values) {
        m.sh(F_OUT + offset, value);
    }
}

fn f_analog_machine(case: &FAnalogCase) -> Machine {
    let mut m = Machine::default();
    for (offset, value) in [8, 0x18, 0x1c, 0x20, 0x24, 0x28].into_iter().zip(case.pad) {
        m.sw(F_INPUT + offset, value);
    }
    f_halves(&mut m, F_RATES, &case.rates);
    f_halves(&mut m, F_CONTROLS, &case.controls);
    f_halves(&mut m, F_ENTITY + 0x18, &case.angles);
    m.sw(F_MOTION, case.motion[0]);
    m.sw(F_MOTION + 4, case.motion[1]);
    m.sh(F_STATE + 0x4c, case.factor);
    m.sh(F_SINE + u32::from(case.angles[2] & 0x3fff) * 2, case.sine);
    m
}

fn f_analog_expected(case: &FAnalogCase, before: &Machine) -> Machine {
    let mut expected = before.clone();
    f_halves(&mut expected, F_RATES, &case.expect_rates);
    f_halves(&mut expected, F_CONTROLS, &case.expect_controls);
    f_outputs(&mut expected, case.expect_out);
    f_halves(&mut expected, F_ENTITY + 0x18, &case.expect_angles);
    expected
}

fn f_scripted_machine(case: &FScriptedCase) -> Machine {
    let mut m = Machine::default();
    m.sw(F_STATE + 8, case.phase[0]);
    m.sw(F_STATE + 0xc, case.phase[1]);
    f_halves(&mut m, F_RATES, &case.rates);
    f_halves(&mut m, F_CONTROLS, &case.controls);
    f_halves(&mut m, F_ENTITY + 0x18, &case.angles);
    m
}

fn f_scripted_expected(case: &FScriptedCase, before: &Machine) -> Machine {
    let mut expected = before.clone();
    expected.sw(F_STATE + 8, case.expect_phase[0]);
    expected.sw(F_STATE + 0xc, case.expect_phase[1]);
    f_halves(&mut expected, F_RATES, &case.expect_rates);
    f_halves(&mut expected, F_CONTROLS, &case.expect_controls);
    f_outputs(&mut expected, case.expect_out);
    expected
}

/// RAM below the stack (`0x801FE000..` holds frames of callers that push
/// one) must match; returns the bytes `expected` changed from `before`.
fn f_compare(index: usize, produced: &mut Machine, expected: &Machine, before: &Machine) -> usize {
    assert!(produced.take_fault().is_ok(), "case {index}: fault");
    const BELOW_STACK: usize = 0x1f_e000;
    let (p, e) = (
        &produced.ram()[..BELOW_STACK],
        &expected.ram()[..BELOW_STACK],
    );
    if p != e {
        let at = (0..p.len()).find(|&i| p[i] != e[i]).expect("difference");
        panic!(
            "case {index}: byte {:#010x} is {:#04x}, reference {:#04x}",
            0x8000_0000 | at as u32,
            p[at],
            e[at]
        );
    }
    e.iter()
        .zip(&before.ram()[..BELOW_STACK])
        .filter(|(x, y)| x != y)
        .count()
}

#[test]
fn analog_control_8006c58c_reference() {
    let mut changed = 0;
    for (index, case) in F_ANALOG_CASES.iter().enumerate() {
        let mut m = f_analog_machine(case);
        let before = m.clone();
        let expected = f_analog_expected(case, &before);
        player::analog_control_8006c58c(
            &mut m, F_INPUT, F_RATES, F_OUT, F_CONTROLS, F_ENTITY, F_MOTION, F_STATE,
        );
        changed += f_compare(index, &mut m, &expected, &before);
    }
    assert!(changed > 0);
}

#[test]
fn scripted_control_80073db0_reference() {
    let mut changed = 0;
    for (index, case) in F_SCRIPTED_CASES.iter().enumerate() {
        let mut m = f_scripted_machine(case);
        let before = m.clone();
        let expected = f_scripted_expected(case, &before);
        player::scripted_control_80073db0(&mut m, F_ENTITY, F_STATE, F_RATES, F_OUT, F_CONTROLS);
        changed += f_compare(index, &mut m, &expected, &before);
    }
    assert!(changed > 0);
}

/// Both routines by original address, with their stack arguments in a
/// pushed caller frame; and 800712B4 with context +0x120 bit 2 (its stack
/// words entity, s1 and `[gp+0x528]`).
#[test]
fn f_stack_arguments_and_800712b4() {
    let mut changed = 0;
    for (index, case) in F_ANALOG_CASES.iter().enumerate() {
        let mut m = f_analog_machine(case);
        let before = m.clone();
        let expected = f_analog_expected(case, &before);
        let sp = m.push_frame(0x28);
        m.sw(sp + 0x10, F_ENTITY);
        m.sw(sp + 0x14, F_MOTION);
        m.sw(sp + 0x18, F_STATE);
        let args = [F_INPUT, F_RATES, F_OUT, F_CONTROLS];
        ac3_flight::dispatch::call(&mut m, 0x8006_c58c, args);
        m.pop_frame(0x28);
        changed += f_compare(index, &mut m, &expected, &before);

        // 800712B4: context +0x128 (= input +8, the buttons) must be >= 0.
        if case.pad[0] & 0x8000_0000 != 0 {
            continue;
        }
        let mut m = f_analog_machine(case);
        let context = F_INPUT - 0x120;
        m.sw(F_INPUT, 4);
        m.set_gw(0x2dc, context);
        for (offset, value) in [
            (0x51c, F_RATES),
            (0x520, F_OUT),
            (0x524, F_CONTROLS),
            (0x528, F_STATE),
        ] {
            m.set_gw(offset, value);
        }
        let before = m.clone();
        let expected = f_analog_expected(case, &before);
        player::control_input_800712b4(&mut m, F_ENTITY, F_MOTION);
        changed += f_compare(index, &mut m, &expected, &before);
    }
    for (index, case) in F_SCRIPTED_CASES.iter().enumerate() {
        let mut m = f_scripted_machine(case);
        let before = m.clone();
        let expected = f_scripted_expected(case, &before);
        let sp = m.push_frame(0x128);
        m.sw(sp + 0x10, F_CONTROLS);
        let args = [F_ENTITY, F_STATE, F_RATES, F_OUT];
        ac3_flight::dispatch::call(&mut m, 0x8007_3db0, args);
        m.pop_frame(0x128);
        changed += f_compare(index, &mut m, &expected, &before);
    }
    assert!(changed > 0);
}

const F_ANALOG_CASES: [FAnalogCase; 24] = [
    FAnalogCase {
        pad: [
            0x1e000000, 0x0000000f, 0x000006c0, 0xfffff777, 0xfffff82a, 0xfffff180,
        ],
        rates: [0xef7d, 0x09c7, 0xf8a1, 0xe126],
        controls: [0xc1a0, 0x8804, 0x6bab, 0xfffb, 0x5855],
        angles: [0x118a, 0x3d80, 0xf5ff],
        motion: [0xfffe7f49, 0x0001b210],
        factor: 0xfa7b,
        sine: 0x03a7,
        expect_rates: [0xfaf1, 0x044b, 0xffc3, 0xf000],
        expect_controls: [0xd12a, 0xa748, 0x4ec9, 0xf9e5, 0x57d5],
        expect_out: [0xfaf1, 0x044b, 0xffc3, 0xf000],
        expect_angles: [0xcc4d, 0x4c98, 0xf5ff],
    },
    FAnalogCase {
        pad: [
            0x63000000, 0x00000000, 0x00000b92, 0xfffff69c, 0xfffff3e4, 0xfffff801,
        ],
        rates: [0xfffc, 0x13d1, 0x06cc, 0xc14f],
        controls: [0x0b54, 0x0f19, 0xf765, 0x0000, 0xffff],
        angles: [0xedd9, 0x97c0, 0x7154],
        motion: [0xffffe26b, 0xffff7ed0],
        factor: 0x9b66,
        sine: 0x8d79,
        expect_rates: [0xe7a7, 0x0cae, 0x064c, 0xf000],
        expect_controls: [0x047d, 0xdaa7, 0xf98d, 0xeb57, 0xffff],
        expect_out: [0xe7a7, 0x0cae, 0x064c, 0xf000],
        expect_angles: [0x544e, 0x3f32, 0x7154],
    },
    FAnalogCase {
        pad: [
            0x2c800000, 0x00000005, 0x00000e4f, 0x00000001, 0x42e0879a, 0xcc8338d6,
        ],
        rates: [0xfffe, 0x13c5, 0x05fc, 0xfffd],
        controls: [0xec4a, 0x0007, 0x6a8e, 0x0d9b, 0x11a2],
        angles: [0xaa4d, 0x2e9c, 0x5e9d],
        motion: [0x0000c89f, 0x27e313ec],
        factor: 0x1074,
        sine: 0x56cf,
        expect_rates: [0x1191, 0x1000, 0x0733, 0x007d],
        expect_controls: [0xfaf7, 0x232e, 0x53e9, 0x07fe, 0x1122],
        expect_out: [0x1191, 0x1000, 0x0733, 0x007d],
        expect_angles: [0x444e, 0xcb36, 0x5e9d],
    },
    FAnalogCase {
        pad: [
            0x03000000, 0x00000003, 0xfffffa34, 0xffffff8f, 0x0000033c, 0x000027b5,
        ],
        rates: [0x4b3d, 0xf272, 0x601c, 0x13af],
        controls: [0x0f40, 0xfbc6, 0x0002, 0xfb61, 0xf094],
        angles: [0xfbff, 0x23f7, 0xf600],
        motion: [0xfffe8701, 0x00012c9e],
        factor: 0x131f,
        sine: 0xb11e,
        expect_rates: [0x0459, 0xf754, 0x05a7, 0x1000],
        expect_controls: [0x0cd8, 0xfbbd, 0x0000, 0x10ff, 0xf114],
        expect_out: [0x0459, 0xf754, 0x05a7, 0x1000],
        expect_angles: [0x1c89, 0xa909, 0xf600],
    },
    FAnalogCase {
        pad: [
            0x1f000000, 0x00000005, 0x1fa4833f, 0x00000614, 0xa8c1457b, 0x00000392,
        ],
        rates: [0xfa0b, 0x0801, 0xd2f0, 0x0510],
        controls: [0xf5a7, 0xfffe, 0xfa0a, 0xed58, 0x01fa],
        angles: [0x2cbf, 0xc88c, 0x1000],
        motion: [0xffff328e, 0x0001f4ea],
        factor: 0x1f9d,
        sine: 0x0359,
        expect_rates: [0x0000, 0x0401, 0x041e, 0x04a0],
        expect_controls: [0x81b3, 0x0000, 0xf786, 0x0a50, 0x017a],
        expect_out: [0x0000, 0x0401, 0x041e, 0x04a0],
        expect_angles: [0x5a26, 0x2ff4, 0x1000],
    },
    FAnalogCase {
        pad: [
            0x32800000, 0x00000004, 0x00000f33, 0xffffffff, 0xfffff9be, 0xfffff977,
        ],
        rates: [0x0089, 0x9023, 0xfc46, 0x00bc],
        controls: [0x0002, 0x0166, 0x370b, 0xf9c8, 0x0e3a],
        angles: [0x03bd, 0xa024, 0xf000],
        motion: [0x0000524d, 0x36594ef0],
        factor: 0x0004,
        sine: 0xfbcc,
        expect_rates: [0x0000, 0xf9be, 0x0000, 0x003c],
        expect_controls: [0x0000, 0x010b, 0x27b6, 0x085e, 0x0eba],
        expect_out: [0x0000, 0xf9be, 0x0000, 0x003c],
        expect_angles: [0x736d, 0xa022, 0xf000],
    },
    FAnalogCase {
        pad: [
            0x87c09d19, 0x0000000c, 0x00001000, 0x000000fb, 0x80000000, 0xfffffb28,
        ],
        rates: [0x7690, 0x13bf, 0x0ab6, 0x5f11],
        controls: [0x0005, 0xfffc, 0x44e4, 0xfca0, 0x075b],
        angles: [0xd2f1, 0xf36e, 0xc08b],
        motion: [0x836b7be0, 0xffff83fb],
        factor: 0x1932,
        sine: 0xf118,
        expect_rates: [0x0000, 0xf000, 0x0413, 0x1000],
        expect_controls: [0xcaed, 0x0000, 0x2fa9, 0x3375, 0x06db],
        expect_out: [0x0000, 0xf000, 0x0413, 0x1000],
        expect_angles: [0xd2f1, 0x4e64, 0xc08b],
    },
    FAnalogCase {
        pad: [
            0x5e000000, 0x00000003, 0xfffff338, 0x000003f8, 0x00003dfa, 0x00003105,
        ],
        rates: [0xf828, 0x00af, 0xf9a4, 0x094e],
        controls: [0x0560, 0x03bf, 0x0441, 0x1ddb, 0x03eb],
        angles: [0x05aa, 0x5ea5, 0xf800],
        motion: [0x000165b3, 0x0001a9a6],
        factor: 0xfffe,
        sine: 0x0c20,
        expect_rates: [0xf68f, 0x04af, 0xfc1a, 0x0881],
        expect_controls: [0x0564, 0xff9c, 0x0732, 0x0c40, 0x036b],
        expect_out: [0xf68f, 0x04af, 0xfc1a, 0x0881],
        expect_angles: [0x059c, 0x5e91, 0xf800],
    },
    FAnalogCase {
        pad: [
            0x61800000, 0x00000004, 0x000009ff, 0x000008ef, 0x000004b0, 0xfffff800,
        ],
        rates: [0xf0b9, 0x48cd, 0x2832, 0x0ed6],
        controls: [0xed83, 0x0004, 0xf70d, 0xf8b0, 0x0456],
        angles: [0x04cc, 0x9885, 0xf03f],
        motion: [0x000094a7, 0x0000e982],
        factor: 0xe811,
        sine: 0x00b1,
        expect_rates: [0x0000, 0x04b0, 0x0f0e, 0x0d90],
        expect_controls: [0xf5e7, 0x0000, 0xfa77, 0x0ec8, 0x03d6],
        expect_out: [0x0000, 0x04b0, 0x0f0e, 0x0d90],
        expect_angles: [0x04cc, 0x9105, 0xf03f],
    },
    FAnalogCase {
        pad: [
            0x03e51a0e, 0x00000003, 0xfffff7b2, 0xfffff630, 0xfffffefe, 0xfffff600,
        ],
        rates: [0x0a0d, 0x7a1c, 0x1576, 0x132e],
        controls: [0x1725, 0xdcef, 0x0caf, 0xf821, 0xe7fe],
        angles: [0xf7c3, 0x7587, 0x5dbc],
        motion: [0x000147f0, 0x00009023],
        factor: 0xec29,
        sine: 0xf728,
        expect_rates: [0xfff6, 0x1000, 0x1462, 0x1000],
        expect_controls: [0x08bc, 0xd185, 0x0982, 0x10c4, 0xe87e],
        expect_out: [0xfff6, 0x1000, 0x1462, 0x1000],
        expect_angles: [0x277f, 0xa6e9, 0x5dbc],
    },
    FAnalogCase {
        pad: [
            0x25f89fda, 0x00000004, 0xfffff6f0, 0xfffff77c, 0xfffffe68, 0x0000073f,
        ],
        rates: [0xf5a0, 0x573d, 0x48a6, 0xfd3b],
        controls: [0x0214, 0x0005, 0xf9e3, 0xb6e9, 0xf0ce],
        angles: [0x0000, 0x2999, 0x0ffe],
        motion: [0x0000745b, 0xffffe1d0],
        factor: 0xea9e,
        sine: 0x0fd7,
        expect_rates: [0x0000, 0x0198, 0x05fe, 0xfd78],
        expect_controls: [0x000e, 0x0000, 0xfb06, 0x06bc, 0xf14e],
        expect_out: [0x0000, 0x0198, 0x05fe, 0xfd78],
        expect_angles: [0x0000, 0xdcb1, 0x0ffe],
    },
    FAnalogCase {
        pad: [
            0x01800000, 0x0000000e, 0xfffff834, 0xfffffe94, 0x000048f9, 0x00002f45,
        ],
        rates: [0x07b5, 0xfb50, 0xef0c, 0xf84e],
        controls: [0xd3f1, 0x0001, 0x0870, 0xfffb, 0x0004],
        angles: [0x011e, 0x21f9, 0xeddc],
        motion: [0x000122db, 0xfad8a54d],
        factor: 0xa1f9,
        sine: 0xeb77,
        expect_rates: [0x0000, 0xff50, 0xefbc, 0xf9c8],
        expect_controls: [0xe479, 0x0000, 0x0a56, 0x0427, 0xff84],
        expect_out: [0x0000, 0xff50, 0xefbc, 0xf9c8],
        expect_angles: [0x011e, 0x6b71, 0xeddc],
    },
    FAnalogCase {
        pad: [
            0x723d7b4a, 0x0000000d, 0xfffff594, 0x00007bf4, 0xffffda6f, 0xffff8eff,
        ],
        rates: [0x0008, 0xf1bf, 0xfffa, 0xf4bc],
        controls: [0x89a2, 0x0005, 0x5fc7, 0x0d83, 0x111c],
        angles: [0xa4af, 0xa266, 0x552b],
        motion: [0x0b58d5b2, 0xe998aea7],
        factor: 0xe7c5,
        sine: 0xfffd,
        expect_rates: [0xf4c4, 0xf000, 0x0328, 0xf133],
        expect_controls: [0xc0ad, 0xe97b, 0x43d4, 0x025e, 0x119c],
        expect_out: [0xf4c4, 0xf000, 0x0328, 0xf133],
        expect_angles: [0x9a5f, 0x64b4, 0x552b],
    },
    FAnalogCase {
        pad: [
            0x14000000, 0x00000001, 0x000002f9, 0xfffff704, 0x00000501, 0x00001000,
        ],
        rates: [0x0b4d, 0xfced, 0x092c, 0x1416],
        controls: [0x0007, 0x11f5, 0x05ee, 0x0006, 0x0007],
        angles: [0x0cdc, 0xaadf, 0x07fc],
        motion: [0x000034b7, 0xfffe3391],
        factor: 0x10a2,
        sine: 0x3624,
        expect_rates: [0x02f9, 0xfe09, 0x0099, 0x1000],
        expect_controls: [0x002d, 0x0e34, 0x0471, 0x112e, 0x0087],
        expect_out: [0x02f9, 0xfe09, 0x0099, 0x1000],
        expect_angles: [0xc576, 0x0795, 0x07fc],
    },
    FAnalogCase {
        pad: [
            0xac94b334, 0x0000001e, 0xfffffe1c, 0x80000000, 0x00000aeb, 0xfffff330,
        ],
        rates: [0xa3e4, 0xf88d, 0x7f93, 0xfcb0],
        controls: [0x523a, 0xff69, 0xf4c6, 0x020d, 0x06c2],
        angles: [0x7ffe, 0x0705, 0xed60],
        motion: [0x68686784, 0xfffe6403],
        factor: 0xfe49,
        sine: 0x0d3e,
        expect_rates: [0xfac0, 0xfb47, 0x0000, 0xfc49],
        expect_controls: [0x3daa, 0x00e0, 0xfa51, 0x0658, 0x0742],
        expect_out: [0xfac0, 0xfb47, 0x0000, 0xfc49],
        expect_angles: [0x1ecc, 0x81fd, 0xed60],
    },
    FAnalogCase {
        pad: [
            0x02000000, 0x00000009, 0xffff896e, 0xa9df3ebc, 0xfffffcf7, 0xfffffefe,
        ],
        rates: [0xfa69, 0x0b7c, 0x0695, 0x0cb0],
        controls: [0x0007, 0x5d3c, 0xedbf, 0x03fd, 0x0738],
        angles: [0x0402, 0x2faf, 0xeffe],
        motion: [0xfffe4527, 0x10d9521e],
        factor: 0xe6ff,
        sine: 0x0354,
        expect_rates: [0xffe3, 0x0759, 0x05ae, 0x0b90],
        expect_controls: [0xf8cd, 0x45e4, 0xf251, 0x0dcc, 0x06b8],
        expect_out: [0xffe3, 0x0759, 0x05ae, 0x0b90],
        expect_angles: [0x95d6, 0x77a7, 0xeffe],
    },
    FAnalogCase {
        pad: [
            0x2a000000, 0x0000000c, 0xfffff043, 0x0000255e, 0xfffff02b, 0xfffff7ff,
        ],
        rates: [0xfde6, 0xf9e7, 0xfd2d, 0xf58c],
        controls: [0x0008, 0x1202, 0xfa87, 0x0004, 0x00a1],
        angles: [0x8002, 0x1537, 0x77b6],
        motion: [0xa20a1de2, 0x85e8e69d],
        factor: 0xe50c,
        sine: 0x0c2f,
        expect_rates: [0x0000, 0xf5f1, 0x142c, 0xf631],
        expect_controls: [0xb803, 0x0d80, 0xf7ee, 0x0339, 0x0021],
        expect_out: [0x0000, 0xf5f1, 0x142c, 0xf631],
        expect_angles: [0x8002, 0x087b, 0x77b6],
    },
    FAnalogCase {
        pad: [
            0x27800000, 0x00000002, 0x337820f1, 0x00000a2d, 0xfffff429, 0xb5f668f2,
        ],
        rates: [0xfffb, 0xd9f4, 0xf4c5, 0xee8f],
        controls: [0x0006, 0xd01d, 0x1317, 0xa937, 0xa244],
        angles: [0xbf90, 0x52c6, 0xf8e4],
        motion: [0xbcbdb833, 0xfffe8c61],
        factor: 0xfffa,
        sine: 0x0263,
        expect_rates: [0x0000, 0xf000, 0xf567, 0xf00e],
        expect_controls: [0x051d, 0xdc17, 0x0a50, 0x0007, 0xa2c4],
        expect_out: [0x0000, 0xf000, 0xf567, 0xf00e],
        expect_angles: [0xbf90, 0x52d0, 0xf8e4],
    },
    FAnalogCase {
        pad: [
            0x00b3278b, 0x00000003, 0x00000594, 0x00000c63, 0xffffc5a2, 0xfffff100,
        ],
        rates: [0xfd14, 0xb65a, 0x4c62, 0xff11],
        controls: [0xfffa, 0x0e49, 0xeedd, 0x0a47, 0xef2a],
        angles: [0x0a00, 0x70dd, 0xd34c],
        motion: [0xa647b130, 0xfffee2b4],
        factor: 0x0003,
        sine: 0x71f1,
        expect_rates: [0xff94, 0xf000, 0x1cee, 0xff26],
        expect_controls: [0x0739, 0x0fb9, 0xf327, 0x0793, 0xefaa],
        expect_out: [0xff94, 0xf000, 0x1cee, 0xff26],
        expect_angles: [0x0a0b, 0x7073, 0xd34c],
    },
    FAnalogCase {
        pad: [
            0x1944bd5b, 0x0000000b, 0x00000a00, 0xfffffd9c, 0xfffff3d5, 0x80000000,
        ],
        rates: [0x0911, 0x39da, 0x4e91, 0x131d],
        controls: [0xf38d, 0x0003, 0xc4b2, 0xa93b, 0xfef3],
        angles: [0x09ff, 0xe908, 0x03fe],
        motion: [0xffff955d, 0xfffe2dbc],
        factor: 0xa404,
        sine: 0xfffd,
        expect_rates: [0x0a00, 0x1000, 0x03b6, 0x1000],
        expect_controls: [0xf5be, 0x0284, 0xd387, 0x10bc, 0xfe73],
        expect_out: [0x0a00, 0x1000, 0x03b6, 0x1000],
        expect_angles: [0xe171, 0xe7bc, 0x03fe],
    },
    FAnalogCase {
        pad: [
            0x476bfd27, 0x0000001e, 0xfffff649, 0x364bc29b, 0xfffff7a9, 0xfffff371,
        ],
        rates: [0x8c4b, 0x0001, 0x0ba9, 0x8e2e],
        controls: [0x33c7, 0x1170, 0x119d, 0x01c9, 0x0ce4],
        angles: [0x95ed, 0x7b72, 0x0a01],
        motion: [0xfffe110d, 0xffff1cc0],
        factor: 0xfc9e,
        sine: 0xf479,
        expect_rates: [0xf402, 0xfdeb, 0x0c5d, 0xf000],
        expect_controls: [0x2c78, 0x0a13, 0x0b1e, 0xd3f8, 0x0c64],
        expect_out: [0xf402, 0xfdeb, 0x0c5d, 0xf000],
        expect_angles: [0xd46e, 0x55be, 0x0a01],
    },
    FAnalogCase {
        pad: [
            0x62000000, 0x00000000, 0xfffff24a, 0xffffcf3e, 0x388d1da7, 0xfffff600,
        ],
        rates: [0xfb25, 0x0000, 0xed95, 0x0e77],
        controls: [0xfffe, 0x02d3, 0x126c, 0x0810, 0xfc8d],
        angles: [0xf3ad, 0xe256, 0x03ff],
        motion: [0x000016e6, 0x0001766c],
        factor: 0xe338,
        sine: 0xb8c9,
        expect_rates: [0x0000, 0x0000, 0xec95, 0x0d39],
        expect_controls: [0xf7fd, 0x021d, 0x0dcf, 0x0e9c, 0xfd0d],
        expect_out: [0x0000, 0x0000, 0xec95, 0x0d39],
        expect_angles: [0xf3ad, 0x2560, 0x03ff],
    },
    FAnalogCase {
        pad: [
            0x46000000, 0x00000000, 0x0000021b, 0x00000a78, 0xfffffb48, 0x000002a7,
        ],
        rates: [0x0001, 0xf092, 0xf48f, 0x0d66],
        controls: [0x0ddb, 0x8d88, 0xfb91, 0x8179, 0xfa49],
        angles: [0x460f, 0x02a1, 0xf2e4],
        motion: [0xfffe9715, 0x9f673b6e],
        factor: 0xfbd5,
        sine: 0xf585,
        expect_rates: [0x0000, 0xf492, 0xf2d8, 0x0c40],
        expect_controls: [0xfcb0, 0xaa28, 0x00ae, 0x0e20, 0xfac9],
        expect_out: [0x0000, 0xf492, 0xf2d8, 0x0c40],
        expect_angles: [0x460f, 0x6f05, 0xf2e4],
    },
    FAnalogCase {
        pad: [
            0x22800000, 0x00000007, 0x0000097b, 0x000005da, 0x0000075c, 0x00000df8,
        ],
        rates: [0x12ca, 0xfb6c, 0xfcf6, 0x06c3],
        controls: [0xfff9, 0x0a86, 0x0005, 0x0003, 0xefe8],
        angles: [0x7f53, 0xf94e, 0x7fff],
        motion: [0xc5f34e37, 0x000160e4],
        factor: 0x6fe0,
        sine: 0x0350,
        expect_rates: [0x2361, 0xfd43, 0x0c1e, 0x062e],
        expect_controls: [0x7940, 0x2914, 0x01dc, 0x0b17, 0xf068],
        expect_out: [0x2361, 0xfd43, 0x0c1e, 0x062e],
        expect_angles: [0x7df0, 0xcb24, 0x7fff],
    },
];

const F_SCRIPTED_CASES: [FScriptedCase; 25] = [
    FScriptedCase {
        phase: [0x00000002, 0x0000004c],
        rates: [0xf58b, 0x007e, 0xb5e4, 0xed00],
        controls: [0xffff, 0xfffc, 0xf785, 0xf331, 0x0007],
        angles: [0x11bd, 0xba09, 0x071c],
        expect_phase: [0x00000002, 0x0000004d],
        expect_rates: [0x0000, 0x0000, 0x0000, 0xf000],
        expect_controls: [0x0000, 0x0000, 0xf9a5, 0xff50, 0xff87],
        expect_out: [0x0000, 0x0000, 0x0000, 0xf000],
    },
    FScriptedCase {
        phase: [0x00000001, 0x00000079],
        rates: [0x0837, 0xf214, 0xf944, 0x12b1],
        controls: [0xef5c, 0x0005, 0x66dc, 0x0349, 0x88f8],
        angles: [0x0e8b, 0x5751, 0xc55e],
        expect_phase: [0x00000002, 0x00000000],
        expect_rates: [0x0000, 0x0000, 0x0000, 0x1000],
        expect_controls: [0xf387, 0x0000, 0x4d23, 0x108b, 0x8978],
        expect_out: [0x0000, 0x0000, 0x0000, 0x1000],
    },
    FScriptedCase {
        phase: [0x00000003, 0x0000004b],
        rates: [0xfedc, 0xfe74, 0x6dba, 0x0ec7],
        controls: [0x0af8, 0x0005, 0xfffd, 0x0008, 0x0147],
        angles: [0x0475, 0x0d87, 0x5f3d],
        expect_phase: [0x00000003, 0x0000004b],
        expect_rates: [0xfcdc, 0x0000, 0x1000, 0x0d82],
        expect_controls: [0x0438, 0xfc02, 0x0000, 0x0ec1, 0x00c7],
        expect_out: [0xfcdc, 0x0000, 0x1000, 0x0d82],
    },
    FScriptedCase {
        phase: [0x00000000, 0x0000004b],
        rates: [0x012c, 0x00e5, 0xfff8, 0x5c95],
        controls: [0xfffd, 0x067c, 0xf28e, 0xa1a0, 0x486a],
        angles: [0xfc00, 0x9324, 0xf600],
        expect_phase: [0x00000001, 0x00000000],
        expect_rates: [0x0000, 0x0000, 0x0000, 0x0000],
        expect_controls: [0x0000, 0x04db, 0xf5ec, 0x0800, 0x47ea],
        expect_out: [0x0000, 0x0000, 0x0000, 0x0000],
    },
    FScriptedCase {
        phase: [0x00000001, 0x00000049],
        rates: [0x738d, 0xfd9b, 0xed09, 0x6ed2],
        controls: [0xfffe, 0xb565, 0x024e, 0xeef8, 0x0e19],
        angles: [0xe000, 0x3660, 0x693c],
        expect_phase: [0x00000001, 0x0000004a],
        expect_rates: [0x0000, 0x0000, 0x0000, 0x1000],
        expect_controls: [0x0000, 0xc80d, 0x01b9, 0x3aa9, 0x0d99],
        expect_out: [0x0000, 0x0000, 0x0000, 0x1000],
    },
    FScriptedCase {
        phase: [0x00000004, 0x00000001],
        rates: [0x0bb9, 0xff60, 0xf037, 0xf9bf],
        controls: [0xf5a6, 0xf218, 0xfc1c, 0x09f1, 0x61f8],
        angles: [0x39b3, 0x27ba, 0x8000],
        expect_phase: [0x00000004, 0x00000001],
        expect_rates: [0x0000, 0x0000, 0x0000, 0xfa49],
        expect_controls: [0xf83e, 0xf594, 0xfd17, 0x0524, 0x6178],
        expect_out: [0x0000, 0x0000, 0x0000, 0xfa49],
    },
    FScriptedCase {
        phase: [0xffffffff, 0x00000078],
        rates: [0x1049, 0xfe16, 0xee14, 0x06e3],
        controls: [0x0008, 0x0e54, 0x0000, 0x0004, 0xf38c],
        angles: [0x4000, 0x77b2, 0x0817],
        expect_phase: [0xffffffff, 0x00000078],
        expect_rates: [0x0000, 0x0000, 0x0000, 0x064b],
        expect_controls: [0x0004, 0x0abd, 0x0000, 0x0b25, 0xf40c],
        expect_out: [0x0000, 0x0000, 0x0000, 0x064b],
    },
    FScriptedCase {
        phase: [0x00000003, 0x00000000],
        rates: [0xfb70, 0xf6ed, 0xf78c, 0x7b28],
        controls: [0x0a40, 0x0e7e, 0x10d3, 0xf60b, 0xf939],
        angles: [0xf9d1, 0x29d5, 0xf534],
        expect_phase: [0x00000003, 0x00000000],
        expect_rates: [0xfd70, 0x0000, 0xf80c, 0x1000],
        expect_controls: [0x0bb2, 0x0ee0, 0x0c9d, 0x404c, 0xf9b9],
        expect_out: [0xfd70, 0x0000, 0xf80c, 0x1000],
    },
    FScriptedCase {
        phase: [0x00000002, 0x00000079],
        rates: [0x067a, 0x08b2, 0xfff1, 0xf4c5],
        controls: [0x30a8, 0xf141, 0xf22e, 0xec64, 0xf3c0],
        angles: [0x28f6, 0x7af6, 0x0000],
        expect_phase: [0x00000003, 0x00000000],
        expect_rates: [0x0000, 0x0000, 0x0062, 0xf57c],
        expect_controls: [0x280a, 0xf4f2, 0xf5a4, 0x02de, 0xf440],
        expect_out: [0x0000, 0x0000, 0x0062, 0xf57c],
    },
    FScriptedCase {
        phase: [0x00000001, 0x0000004c],
        rates: [0x0070, 0x1121, 0x0932, 0xf8c8],
        controls: [0x0263, 0xf0ca, 0xf312, 0x9039, 0x12d3],
        angles: [0x4cc6, 0x0d74, 0xeeb8],
        expect_phase: [0x00000002, 0x00000000],
        expect_rates: [0x0000, 0x0000, 0x0000, 0xf888],
        expect_controls: [0x01c9, 0xf499, 0xf64f, 0x0464, 0x1253],
        expect_out: [0x0000, 0x0000, 0x0000, 0xf888],
    },
    FScriptedCase {
        phase: [0x00000001, 0x0000004d],
        rates: [0xf0ad, 0xf26e, 0x0852, 0xfffe],
        controls: [0x1481, 0xf5a2, 0x0dc2, 0x0577, 0xfffc],
        angles: [0x0a01, 0xc3bb, 0x0001],
        expect_phase: [0x00000002, 0x00000000],
        expect_rates: [0x0000, 0x0000, 0x0000, 0xffbe],
        expect_controls: [0x0f5f, 0xf83b, 0x0a50, 0x07ff, 0xfffc],
        expect_out: [0x0000, 0x0000, 0x0000, 0xffbe],
    },
    FScriptedCase {
        phase: [0x3206cdf2, 0x00000001],
        rates: [0x0fea, 0x0001, 0x0565, 0x0301],
        controls: [0xee42, 0x87f7, 0xfab5, 0x11a2, 0x0de1],
        angles: [0xefff, 0xd2d4, 0x0804],
        expect_phase: [0x3206cdf2, 0x00000001],
        expect_rates: [0x0000, 0x0000, 0x0000, 0x02bf],
        expect_controls: [0xf2b3, 0xa5fb, 0xfc09, 0x095f, 0x0d61],
        expect_out: [0x0000, 0x0000, 0x0000, 0x02bf],
    },
    FScriptedCase {
        phase: [0x00000001, 0x0000004d],
        rates: [0x0890, 0xf7ec, 0x124e, 0x00db],
        controls: [0x326d, 0xecaf, 0xfd5a, 0x9c2a, 0xf58b],
        angles: [0xf093, 0x174b, 0xf7fe],
        expect_phase: [0x00000002, 0x00000000],
        expect_rates: [0x0000, 0x0000, 0x0000, 0x009b],
        expect_controls: [0x25d0, 0xf185, 0xfe05, 0x086d, 0xf60b],
        expect_out: [0x0000, 0x0000, 0x0000, 0x009b],
    },
    FScriptedCase {
        phase: [0x00000001, 0x0000007a],
        rates: [0x8f03, 0x0954, 0xf12e, 0x0192],
        controls: [0xf5ef, 0xfea3, 0xd755, 0xfc2c, 0x0e63],
        angles: [0xc000, 0xce55, 0xec8e],
        expect_phase: [0x00000002, 0x00000000],
        expect_rates: [0x0000, 0x0000, 0x0000, 0x0152],
        expect_controls: [0xf875, 0xfefc, 0xe181, 0x08c9, 0x0de3],
        expect_out: [0x0000, 0x0000, 0x0000, 0x0152],
    },
    FScriptedCase {
        phase: [0x00000001, 0x00000075],
        rates: [0xf6de, 0x0107, 0x463d, 0xecb4],
        controls: [0x1110, 0xffff, 0x0666, 0x0840, 0xfffe],
        angles: [0xeffe, 0xaa52, 0xf5ff],
        expect_phase: [0x00000002, 0x00000000],
        expect_rates: [0x0000, 0x0000, 0x0000, 0xf000],
        expect_controls: [0x0cca, 0x0000, 0x04cb, 0xff2e, 0xfffe],
        expect_out: [0x0000, 0x0000, 0x0000, 0xf000],
    },
    FScriptedCase {
        phase: [0x00000004, 0x80000000],
        rates: [0xff20, 0x6e5d, 0x063e, 0xfffd],
        controls: [0xfaac, 0x9eaf, 0xfb53, 0x0454, 0x06b8],
        angles: [0x08e3, 0x1cb3, 0x107c],
        expect_phase: [0x00000004, 0x80000000],
        expect_rates: [0x0000, 0x0000, 0x0000, 0xfffe],
        expect_controls: [0xfc03, 0xb705, 0xfc80, 0x07ff, 0x0638],
        expect_out: [0x0000, 0x0000, 0x0000, 0xfffe],
    },
    FScriptedCase {
        phase: [0x00000003, 0x00000001],
        rates: [0xae97, 0x0d8d, 0xf300, 0x0eea],
        controls: [0x002d, 0xfbaf, 0xf3bc, 0x0001, 0x0000],
        angles: [0x196b, 0x20e6, 0x0177],
        expect_phase: [0x00000003, 0x00000001],
        expect_rates: [0xf000, 0x0000, 0xfd12, 0x0da2],
        expect_controls: [0xff65, 0xf8c2, 0xf6cf, 0x0ed1, 0x0000],
        expect_out: [0xf000, 0x0000, 0xfd12, 0x0da2],
    },
    FScriptedCase {
        phase: [0x00000000, 0x00000078],
        rates: [0x0236, 0x742d, 0x03f0, 0x0f0e],
        controls: [0xab1b, 0xf035, 0x35fb, 0xfffc, 0x1040],
        angles: [0x0765, 0x529b, 0xf1d0],
        expect_phase: [0x00000001, 0x00000000],
        expect_rates: [0x0000, 0x0000, 0x0000, 0x0000],
        expect_controls: [0xc056, 0xf429, 0x287b, 0x0800, 0x0fc0],
        expect_out: [0x0000, 0x0000, 0x0000, 0x0000],
    },
    FScriptedCase {
        phase: [0x00000002, 0x0000007a],
        rates: [0xfff8, 0xd7ef, 0xf4af, 0xf82c],
        controls: [0x11a4, 0xfffa, 0xa96c, 0xbf45, 0xeae6],
        angles: [0x1219, 0xa066, 0x06f7],
        expect_phase: [0x00000003, 0x00000000],
        expect_rates: [0x0000, 0x0000, 0xffb6, 0xf7ec],
        expect_controls: [0x0d4c, 0xfffd, 0xbf13, 0x0416, 0xeb66],
        expect_out: [0x0000, 0x0000, 0xffb6, 0xf7ec],
    },
    FScriptedCase {
        phase: [0x00000003, 0x00000079],
        rates: [0x5f4e, 0xfffd, 0x337c, 0xef61],
        controls: [0x3b9b, 0xfffe, 0xfe0f, 0x0008, 0x1c43],
        angles: [0x0684, 0x3ef7, 0x318e],
        expect_phase: [0x00000003, 0x00000079],
        expect_rates: [0x1000, 0x0000, 0x1000, 0xf0ce],
        expect_controls: [0x28b3, 0xfbfd, 0xfe8d, 0x0067, 0x1bc3],
        expect_out: [0x1000, 0x0000, 0x1000, 0xf0ce],
    },
    FScriptedCase {
        phase: [0x00000003, 0x00000079],
        rates: [0x0cb9, 0x2a54, 0xf9a0, 0x110a],
        controls: [0xeefb, 0xfea0, 0x6213, 0x54ca, 0xef1c],
        angles: [0xecfc, 0x66d3, 0xf27c],
        expect_phase: [0x00000003, 0x00000079],
        expect_rates: [0x0eb9, 0x0000, 0xfa20, 0x0f94],
        expect_controls: [0xf73e, 0x02fa, 0x498d, 0x0fca, 0xef9c],
        expect_out: [0x0eb9, 0x0000, 0xfa20, 0x0f94],
    },
    FScriptedCase {
        phase: [0x6472ce38, 0x00000079],
        rates: [0x0007, 0xf351, 0xf415, 0xfff9],
        controls: [0x7e81, 0xec2e, 0x2fd4, 0x0e2d, 0x0d1d],
        angles: [0x08a4, 0xb5d7, 0x13c0],
        expect_phase: [0x6472ce38, 0x00000079],
        expect_rates: [0x0000, 0x0000, 0x0000, 0xfffa],
        expect_controls: [0x5edf, 0xf124, 0x23dd, 0x07fd, 0x0c9d],
        expect_out: [0x0000, 0x0000, 0x0000, 0xfffa],
    },
    FScriptedCase {
        phase: [0x00000002, 0x0000004c],
        rates: [0x5b98, 0xb471, 0x58b3, 0xf1ef],
        controls: [0x1029, 0xf685, 0x0e01, 0xfb6a, 0x06e4],
        angles: [0x0000, 0xd2fd, 0x1000],
        expect_phase: [0x00000002, 0x0000004d],
        expect_rates: [0x0000, 0x0000, 0x1000, 0xf2e4],
        expect_controls: [0x081d, 0xf8e5, 0x0a7f, 0x0192, 0x0664],
        expect_out: [0x0000, 0x0000, 0x1000, 0xf2e4],
    },
    FScriptedCase {
        phase: [0x00000000, 0x00000000],
        rates: [0xf5c9, 0xe424, 0x07f4, 0xfa1a],
        controls: [0x0001, 0xfffe, 0xfb67, 0x02bf, 0x129d],
        angles: [0xf5ff, 0xee3e, 0x7ffe],
        expect_phase: [0x00000001, 0x00000000],
        expect_rates: [0x0000, 0x0000, 0x0000, 0x0000],
        expect_controls: [0x0000, 0x0000, 0xfc8f, 0x0800, 0x121d],
        expect_out: [0x0000, 0x0000, 0x0000, 0x0000],
    },
    FScriptedCase {
        phase: [0x00000001, 0x00000079],
        rates: [0x31af, 0x00e8, 0xa1f6, 0xfff9],
        controls: [0xfff8, 0xda4a, 0x094d, 0xf9ec, 0x1237],
        angles: [0x8000, 0x55b6, 0xa112],
        expect_phase: [0x00000002, 0x00000000],
        expect_rates: [0x0000, 0x0000, 0x0000, 0xffb9],
        expect_controls: [0xfffc, 0xe3b9, 0x06f8, 0x07fc, 0x11b7],
        expect_out: [0x0000, 0x0000, 0x0000, 0xffb9],
    },
];
// ==== end of f ====

// ==== b2 ====

/// 80031164 on the render captures (return address 800766E8, output
/// SVECTOR in the caller's frame): the six output bytes on the stack must
/// match the return snapshot and differ from the stale entry bytes; the
/// GTE registers are compared by `check`.
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/render-002, render-003, render-006"]
fn turn_orientation_80031164() {
    let mut outputs = 0;
    for (name, count) in [("render-002", 3), ("render-003", 2), ("render-006", 2)] {
        for occurrence in 0..count {
            let output = RefCell::new((0, Vec::new(), Vec::new()));
            check(name, 0x8003_1164, occurrence..occurrence + 1, |m, e| {
                let out = e.gpr(6);
                let before = m.read_bytes(out, 6);
                player::turn_orientation_80031164(m, e.gpr(4), e.gpr(5), out);
                *output.borrow_mut() = (out, m.read_bytes(out, 6), before);
                None
            });
            let (out, produced, before) = output.into_inner();
            check_stack_output(name, 0x8003_1164, occurrence, &produced, &before, out);
            outputs += 1;
        }
    }
    assert_eq!(outputs, 7);
}

/// Group B terrain and engine-sound routines on the crash dive player-015
/// (flight frames 501/503/505): the ray tests near the ground, the vertical
/// ray hit at 503 (contact bit 2, height lifted) that the round-1 captures
/// never reached, and the ground height under the player. 80070AF4 #1
/// (frame 503) reacts to the contact through 80081564 (effects module) and
/// is left out until that is merged.
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-015"]
fn b2_group_b_crash_dive() {
    let name = "player-015";
    let changed = check(name, 0x8006_f670, 0..3, |m, e| {
        player::world_collision_8006f670(m, e.gpr(4), e.gpr(5));
        None
    });
    assert!(changed >= 3 * 40, "8006F670: {changed}");
    let changed = check(name, 0x8006_fb78, 0..3, |m, e| {
        let v0 = player::ground_height_8006fb78(m, e.gpr(4), e.gpr(5), e.gpr(6));
        assert_ne!(v0, 0);
        Some(v0)
    });
    assert!(changed > 0, "8006FB78: {changed}");
    let changed = check(name, 0x8002_ffac, 0..2, |m, e| {
        player::engine_voices_8002ffac(m, e.gpr(4));
        None
    }) + check(name, 0x8007_0a90, 0..2, |m, _| {
        player::engine_sound_80070a90(m);
        None
    });
    assert!(changed > 0, "8002FFAC/80070A90: {changed}");
    let changed: usize = [0, 2]
        .into_iter()
        .map(|occurrence| {
            check(name, 0x8007_0af4, occurrence..occurrence + 1, |m, e| {
                player::player_effects_80070af4(m, e.gpr(4));
                None
            })
        })
        .sum();
    assert!(changed > 0, "80070AF4: {changed}");
}

/// Voice entry `voice` of the SPU voice table (800C35F8, 20 bytes).
fn b2_voice(voice: u32) -> u32 {
    0x800c_35f8 + voice * 20
}

/// Synthetic checks of the positioned-sound voice routines, which no
/// snapshot capture reaches yet: values worked out by hand from the
/// instructions, and the 0xFF (no voice) early outs leaving RAM untouched.
#[test]
fn b2_sound_voice_arithmetic() {
    let sound = 0x8010_0000;
    let voice = 1;
    let entry = b2_voice(voice);

    // 0xFF: no voice, nothing written, v0 = 0xFF.
    let mut m = Machine::default();
    let before = m.ram().to_vec();
    assert_eq!(player::restore_voice_8002e260(&mut m, 0xff), 0xff);
    assert_eq!(
        player::distance_volume_8002e430(&mut m, 0xff, sound, 2),
        0xff
    );
    assert_eq!(player::pan_volume_8002e4c0(&mut m, 0xff, sound), 0xff);
    assert_eq!(
        player::surround_side_8002e608(&mut m, 0xff, 0x8000_0000),
        0xff
    );
    assert_eq!(player::doppler_pitch_8002e82c(&mut m, 0xff, sound), 0xff);
    assert_eq!(player::detune_8002ecb8(&mut m, 0xff, 0x100), 0xff);
    assert!(m.ram() == &before[..]);
    assert!(m.take_fault().is_ok());

    // 8002E430: level 0x3FFF - (0x100 << 2) = 0x3BFF, fade 0x3BFF^2 >> 14 =
    // 14398, volume 0x3000 * 14398 >> 14 = 10798 into both sides.
    let mut m = Machine::default();
    m.sh(entry + 4, 0x3000);
    m.sw(sound, 0x100);
    assert_eq!(
        player::distance_volume_8002e430(&mut m, voice, sound, 2),
        voice
    );
    assert_eq!(m.lhu(entry + 4), 10798);
    assert_eq!(m.lhu(entry + 6), 10798);
    assert_eq!(m.lbu(entry + 1), 0x10);
    // Beyond range the level is 0; a negative distance saturates at 0x4000.
    m.sh(entry + 4, 0x3000);
    m.sw(sound, 0x1_0000);
    player::distance_volume_8002e430(&mut m, voice, sound, 0);
    assert_eq!(m.lhu(entry + 4), 0);
    m.sh(entry + 4, 0x3000);
    m.sw(sound, 0xffff_0000);
    player::distance_volume_8002e430(&mut m, voice, sound, 0);
    assert_eq!(m.lhu(entry + 4), 0x3000);

    // 8002E4C0: bearing 0 gives sin 0 / cos 0x1000 (table set here), so
    // left 0 and right 0x2000 * 0x1000 >> 14 = 0x800; distance 0x80 gives
    // the floor 0x2000 * (0x4000 - 0x2000) >> 14 = 0x1000 on both sides.
    let mut m = Machine::default();
    m.sh(0x8001_0498 + 0x2000, 0x1000);
    m.sh(entry + 4, 0x2000);
    m.sw(sound, 0x80);
    assert_eq!(player::pan_volume_8002e4c0(&mut m, voice, sound), voice);
    assert_eq!((m.lhu(entry + 4), m.lhu(entry + 6)), (0x1000, 0x1000));
    // Far away (no floor) with a negative product: absolute value, capped.
    m.sh(entry + 4, 0x8000);
    m.sh(0x8001_0498 + 0x2000, 0x1000);
    m.sw(sound, 0x1000);
    player::pan_volume_8002e4c0(&mut m, voice, sound);
    assert_eq!((m.lhu(entry + 4), m.lhu(entry + 6)), (0, 0x2000));

    // 8002E608: a left source (negative side) negates the left volume only.
    let mut m = Machine::default();
    m.sh(entry + 4, 0x1234);
    m.sh(entry + 6, 0x0567);
    player::surround_side_8002e608(&mut m, voice, 1);
    assert_eq!(m.lhu(entry + 4), 0x1234);
    player::surround_side_8002e608(&mut m, voice, 0xffff_fc00);
    assert_eq!((m.lhu(entry + 4), m.lhu(entry + 6)), (0xedcc, 0x0567));

    // 8002E260: volumes from +8, pitch from +0xE.
    let mut m = Machine::default();
    m.sw(entry + 8, 0x0aaa_0bbb);
    m.sh(entry + 0xe, 0x3c00);
    assert_eq!(player::restore_voice_8002e260(&mut m, voice), voice);
    assert_eq!(m.lw(entry + 4), 0x0aaa_0bbb);
    assert_eq!(m.lhu(entry + 0xc), 0x3c00);

    // 8002E82C: no distance change is index (0x470 << 9) / 0x8E0 = 0x100,
    // the table centre; a large approach rate clamps to 0x1FF.
    let mut m = Machine::default();
    m.sh(entry + 0xe, 0x3c00);
    m.sh(0x800a_e920 + 0x100 * 2, 5);
    m.sh(0x800a_e920 + 0x1ff * 2, 0x7f);
    assert_eq!(player::doppler_pitch_8002e82c(&mut m, voice, sound), voice);
    assert_eq!(m.lhu(entry + 0xc), 0x3c05);
    m.sw(sound + 4, 0x1000);
    player::doppler_pitch_8002e82c(&mut m, voice, sound);
    assert_eq!(m.lhu(entry + 0xc), 0x3c7f);

    // 8002ECB8: -0x200 fine steps (a signed halfword) from note 0x3C.
    let mut m = Machine::default();
    m.sh(entry + 0xc, 0x3c00);
    assert_eq!(player::detune_8002ecb8(&mut m, voice, 0xffff_fe00), voice);
    assert_eq!(m.lhu(entry + 0xc), 0x3800);
    assert_eq!(m.lbu(entry + 1), 0x10);
    assert!(m.take_fault().is_ok());
}

// ==== end of b2 ====

// ==== g ====
// Crash capture player-015 (the full-throttle dive of player-012, flight
// frames 501..509): 80072EDC #0 (frame 505: phase 0 -> 2 with immediate
// ground contact -> phase 0x10), #1 and #2 (507, 509: phase 0x10); the
// vibration leaves 8002921C/80029254/80029298 (505); 80071358 #2 (505,
// mode 0x100000). 80072EDC calls the effect routines 8008094C, 80080BA4
// and 80081124 (the player agent's effects module): the full checks need
// them claimed; `crash_outside_effects` checks everything else now.

const G_CRASH: &str = "player-015";
const G_EFFECTS: [u32; 3] = [0x8008_094c, 0x8008_0ba4, 0x8008_1124];

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-015"]
fn vibrate_presets_8002921c_80029298() {
    let impact = check(G_CRASH, 0x8002_921c, 0..1, |m, _| {
        player::vibrate_preset_8002921c(m);
        None
    });
    let start = check(G_CRASH, 0x8002_9298, 0..1, |m, _| {
        player::vibrate_preset_80029298(m);
        None
    });
    assert!(impact > 0 && start > 0, "{impact} {start}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-015"]
fn vibrate_random_80029254() {
    let changed = check(G_CRASH, 0x8002_9254, 0..1, |m, _| {
        player::vibrate_random_80029254(m);
        None
    });
    // Slot bytes 0..2 and the rand state.
    assert!(changed > 2, "{changed}");
}

/// Needs 8008094C, 80080BA4 and 80081124 claimed (effects module); fails
/// with `Fault::Untranslated` until then.
#[test]
#[ignore = "requires AC3_DATA_DIR, analysis/flight-runtime/player-015 and the effects module"]
fn crash_80072edc() {
    let changed = check(G_CRASH, 0x8007_2edc, 0..3, |m, e| {
        player::crash_80072edc(m, e.gpr(4), e.gpr(5));
        None
    });
    assert!(changed > 0);
}

/// Needs the effects module: 80071358 in crash mode 0x100000 (frame 505).
#[test]
#[ignore = "requires AC3_DATA_DIR, analysis/flight-runtime/player-015 and the effects module"]
fn update_player_80071358_crash() {
    let changed = check(G_CRASH, 0x8007_1358, 2..3, |m, _| {
        player::update_player_80071358(m);
        None
    });
    assert!(changed > 0);
}

/// Run `run` on entry `occurrence` of `callee` with the routines `nested`
/// left untranslated, and compare with the paired return snapshot except
/// for the bytes the bracketed `nested` calls inside that call changed
/// (their own entry/return pairs) and, when those calls changed GTE
/// registers, the GTE; `also` lists further excused addresses (values
/// derived from state the nested calls changed). The only fault allowed is
/// `Untranslated` for a `nested` address. Returns the bytes the original
/// changed outside the excused set and the number of nested calls.
fn g_check_outside(
    name: &str,
    callee: u32,
    occurrence: usize,
    nested: &[u32],
    also: &[u32],
    run: impl FnOnce(&mut Machine, &Snapshot),
) -> (usize, usize) {
    let index_file = capture(name);
    let directory = index_file.parent().expect("directory");
    let index = snapshot::read_index(&index_file).expect("index");
    let paired = |entry: &snapshot::SnapshotInfo| {
        index
            .iter()
            .find(|s| {
                s.callee == entry.callee
                    && s.stage == "return"
                    && s.sequence > entry.sequence
                    && s.ra == entry.ra
                    && s.registers.gpr[29] == entry.registers.gpr[29]
            })
            .expect("return")
    };
    let entry = index
        .iter()
        .filter(|s| s.callee == callee && s.stage == "entry")
        .nth(occurrence)
        .expect("entry");
    let exit = paired(entry);
    let mut excused: std::collections::HashSet<u32> = also.iter().copied().collect();
    let mut gte_excused = false;
    let mut calls = 0;
    for inner in index.iter().filter(|s| {
        nested.contains(&s.callee)
            && s.stage == "entry"
            && s.sequence > entry.sequence
            && s.sequence < exit.sequence
    }) {
        let before = Snapshot::load(directory, inner).expect("snapshot");
        let after = Snapshot::load(directory, paired(inner)).expect("snapshot");
        let changes = snapshot::compare(&before.machine(), &after);
        excused.extend(changes.ram);
        excused.extend(changes.scratch);
        gte_excused |= !changes.gte_data.is_empty() || !changes.gte_control.is_empty();
        calls += 1;
    }
    let start = Snapshot::load(directory, entry).expect("snapshot");
    let end = Snapshot::load(directory, exit).expect("snapshot");
    let mut m = start.machine();
    run(&mut m, &start);
    match m.take_fault() {
        Ok(()) => {}
        Err(ac3_flight::Fault::Untranslated { address }) if nested.contains(&address) => {}
        Err(fault) => panic!("{name} #{occurrence}: {fault}"),
    }
    let differences = snapshot::compare(&m, &end);
    let wrong: Vec<u32> = differences
        .ram
        .iter()
        .chain(&differences.scratch)
        .filter(|a| !excused.contains(*a))
        .copied()
        .collect();
    assert!(
        wrong.is_empty(),
        "{name} #{occurrence}: {} bytes differ outside the nested calls: {:08x?}",
        wrong.len(),
        &wrong[..wrong.len().min(16)]
    );
    if !gte_excused {
        assert!(
            differences.gte_data.is_empty() && differences.gte_control.is_empty(),
            "{name} #{occurrence}: GTE {:?} {:?}",
            differences.gte_data,
            differences.gte_control
        );
    }
    let original = snapshot::compare(&start.machine(), &end);
    let changed = original
        .ram
        .iter()
        .chain(&original.scratch)
        .filter(|a| !excused.contains(*a))
        .count();
    (changed, calls)
}

/// Byte 2 of the vibration slot 80029254 claimed (priority 0xA0) in the
/// return snapshot of `callee` #`occurrence`, if any: its `rand()` value
/// follows the rand state 80080BA4 advanced first (80029254 itself is
/// checked on its own entry by `vibrate_random_80029254`).
fn g_random_vibration_byte(callee: u32, occurrence: usize) -> Vec<u32> {
    let exit = exit_snapshot(G_CRASH, callee, occurrence);
    let word = |a: u32| {
        let at = (a & 0x1f_ffff) as usize;
        u32::from_le_bytes(exit.ram[at..at + 4].try_into().unwrap())
    };
    let slots = word(ac3_flight::GP + 0x30);
    (0..6)
        .map(|i| slots + i * 4)
        .filter(|&slot| word(slot) & 0xffff == 0xa001)
        .map(|slot| slot + 2)
        .collect()
}

/// 80072EDC and 80071358 #2 on player-015 with the effect calls left
/// untranslated: everything outside the bytes the bracketed effect calls
/// changed matches the original return (phase, counter, roll rate, entity
/// velocity and flags, vibration slots, rand state, sounds).
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-015"]
fn crash_outside_effects() {
    // Only #0 (frame 505, phases 2 -> 0x10) runs 80080BA4 and 80029254.
    for occurrence in 0..3 {
        let also = if occurrence == 0 {
            g_random_vibration_byte(0x8007_2edc, 0)
        } else {
            Vec::new()
        };
        assert_eq!(also.len(), usize::from(occurrence == 0));
        let (changed, calls) = g_check_outside(
            G_CRASH,
            0x8007_2edc,
            occurrence,
            &G_EFFECTS,
            &also,
            |m, e| player::crash_80072edc(m, e.gpr(4), e.gpr(5)),
        );
        assert!(changed > 0 && calls > 0, "#{occurrence}: {changed} {calls}");
    }
    let also = g_random_vibration_byte(0x8007_1358, 2);
    assert_eq!(also.len(), 1);
    let (changed, calls) = g_check_outside(G_CRASH, 0x8007_1358, 2, &G_EFFECTS, &also, |m, _| {
        player::update_player_80071358(m);
    });
    assert!(changed > 0 && calls == 3, "{changed} {calls}");
}

// ---- g: synthetic paths no capture reaches (modes 0x10, 0x20/0x40, 0x200) ----

const G_ENTITY: u32 = 0x8010_0000;
const G_STATE: u32 = 0x8010_0200;
const G_CONTROLS: u32 = 0x8010_0300;
const G_VIEW: u32 = 0x8010_0400;
const G_GAMEPLAY: u32 = 0x8011_0000;
const G_CAMERA: u32 = 0x8011_1000;
const G_TABLE: u32 = 0x8011_2000;
const G_CONTEXT: u32 = 0x8011_3000;
/// Resident sine table (1.14: 0x4000 = 1.0; sine at entry a, cosine at a +
/// 0x1000).
const G_SINE: u32 = 0x8001_0498;
/// rsin's quarter-wave table (800A45C8).
const G_QUARTER: u32 = 0x800b_4444;

fn g_machine() -> Machine {
    let mut m = Machine::default();
    m.set_gw(0x2d0, G_GAMEPLAY);
    m.sw(G_GAMEPLAY + 0x28, G_CAMERA);
    m.set_gw(0x2d8, G_TABLE);
    m.sw(G_TABLE, G_ENTITY);
    m.set_gw(0x2dc, G_CONTEXT);
    // cos(0) = 1.0: angle 0 gives the identity rotation (80030534).
    m.sh(G_SINE + 0x2000, 0x4000);
    m
}

fn g_roll(m: &mut Machine, braking: u32) -> [i32; 6] {
    player::ground_roll_800731f4(m, G_ENTITY, G_STATE, braking);
    assert!(m.take_fault().is_ok());
    [
        m.lw(G_ENTITY + 0x38) as i32,
        m.lw(G_ENTITY + 0x3c) as i32,
        m.lw(G_ENTITY + 0x40) as i32,
        i32::from(m.lh(G_ENTITY + 0x18)),
        m.lw(G_STATE + 8) as i32,
        m.lw(G_STATE + 0xc) as i32,
    ]
}

#[test]
fn ground_roll_800731f4_phases() {
    let mut m = g_machine();
    let velocity = |m: &mut Machine, vx: i32, vy: i32, vz: i32| {
        m.sw(G_ENTITY + 0x38, vx as u32);
        m.sw(G_ENTITY + 0x3c, vy as u32);
        m.sw(G_ENTITY + 0x40, vz as u32);
    };
    // Airborne: friction 0xFF4 (0xF60 braking) toward zero, gravity 0x29D.
    velocity(&mut m, 0x10000, 0x100, -0x10001);
    assert_eq!(&g_roll(&mut m, 0)[..3], [0xff40, 0x39d, -0xff40]);
    velocity(&mut m, 0x10000, 0x100, 0);
    assert_eq!(g_roll(&mut m, 1)[0], 0xf600);
    // Ground, phase 0, pitch 0xB5: phase 3 from (0xB5 << 10) / 182 + 0x1000
    // = 0x13FA, then pitch = rsin(0x3FA) * 0xB6 / 4096 = 0xB5 and 0x13BA.
    m.sw(G_STATE + 0x10, 1);
    m.sh(G_QUARTER + 0x3fa * 2, 0xffe);
    velocity(&mut m, 0, 0x500, 0);
    m.sh(G_ENTITY + 0x18, 0xb5);
    m.sh(G_ENTITY + 0x1c, 0x700);
    assert_eq!(g_roll(&mut m, 0)[1..], [0x500, 0, 0xb5, 3, 0x13ba]);
    // |roll| >= 0x600: damping 0xF00.
    assert_eq!(m.lh(G_ENTITY + 0x1c), 0x690);
    // Phase 1 (lift 0: roll damping 0xEC0): pitch += (0xB6 - pitch) / 2.
    velocity(&mut m, 0, -5, 0);
    m.sw(G_STATE + 8, 1);
    m.sh(G_ENTITY + 0x18, 0x10);
    m.sh(G_ENTITY + 0x1c, 0x100);
    assert_eq!(g_roll(&mut m, 0)[3..5], [0x63, 1]);
    assert_eq!(m.lh(G_ENTITY + 0x1c), 0xec);
    // From 0xB6: pitch 0xB6, phase 3 at 0x1400 (run from the next call).
    m.sh(G_ENTITY + 0x18, 0xb6);
    assert_eq!(g_roll(&mut m, 0)[3..], [0xb6, 3, 0x1400]);
    // Phase 2 (lift 0x400): pitch -= (pitch - 0xB6) * 0x800 / 4096, at least 8.
    velocity(&mut m, 0, 0x400, 0);
    m.sw(G_STATE + 8, 2);
    m.sh(G_ENTITY + 0x18, 0x200);
    assert_eq!(g_roll(&mut m, 0)[3..5], [0x15b, 2]);
    m.sh(G_ENTITY + 0x18, 0xb7);
    assert_eq!(g_roll(&mut m, 0)[3..5], [0xaf, 2]);
    // Phase 3 end (braking: -0x80): 80077F58(1) and phase 4.
    m.sw(G_STATE + 8, 3);
    m.sw(G_STATE + 0xc, -0x1c0i32 as u32);
    assert_eq!(g_roll(&mut m, 1)[3..], [0, 4, -0x240]);
    assert_eq!(m.lw(G_GAMEPLAY) & 1, 1, "80035248(0) sets bit 0");
    assert_eq!(m.lw(G_CAMERA + 0x50), 0x100, "80077D54 mode 0x100");
    assert_eq!(m.lw(G_CAMERA + 0x34), G_ENTITY + 8);
}

#[test]
fn scripted_fly_in_80073524_phases() {
    let mut m = g_machine();
    let run = |m: &mut Machine| {
        for offset in (0..0x10).step_by(2) {
            m.sh(G_CONTROLS + offset, 0xffff);
        }
        let v0 = player::scripted_fly_in_80073524(m, G_ENTITY, G_STATE, G_CONTROLS, G_VIEW);
        assert!(m.take_fault().is_ok());
        let controls: Vec<u16> = (0..8).map(|i| m.lhu(G_CONTROLS + i * 2)).collect();
        assert_eq!(controls, [0, 0, 0, 0x800, 0, 0, 0, 0]);
        v0
    };
    // Phase 0, view yaw 0 (sin 0, cos 0x4000 >> 2), [gp+0x28C] != 0:
    // z = 0x300000 + 0x1000 * -0x780.
    m.set_gw(0x28c, 1);
    for (i, value) in [0x10_0000, 0x20_0000, 0x30_0000].into_iter().enumerate() {
        m.sw(G_VIEW + i as u32 * 4, value);
    }
    m.sh(G_QUARTER + 0x13e * 2, 0x7a0);
    assert_eq!(run(&mut m), 0);
    let words: Vec<u32> = [8, 0xc, 0x10, 0x38, 0x3c, 0x40]
        .iter()
        .map(|&o| m.lw(G_ENTITY + o))
        .collect();
    assert_eq!(
        words,
        [0x10_0000, 0xfff2_6000, 0xffb8_0000, 0, 0x7a00, 0x1_0000]
    );
    assert_eq!(m.lhu(G_ENTITY + 0x18), 0xfeef);
    assert_eq!(
        [m.lw(G_STATE + 0x48), m.lw(G_STATE + 0xc), m.lw(G_STATE + 8)],
        [0x1_0000, 0, 1]
    );
    // Phase 1: phase 2 after the 0x1F-th frame.
    m.sw(G_STATE + 0xc, 0x1d);
    run(&mut m);
    assert_eq!([m.lw(G_STATE + 8), m.lw(G_STATE + 0xc)], [1, 0x1e]);
    run(&mut m);
    assert_eq!([m.lw(G_STATE + 8), m.lw(G_STATE + 0xc)], [2, 0]);
    // Phase 2 at 0x10: (0x10 << 12) / 75 = 873; rsin(0x5B4) = table
    // [0x24C] = 0x200; k = 0x900: pitch = -409k / 4096 + 0x88 = -94.
    m.sh(G_QUARTER + 0x24c * 2, 0x200);
    m.sw(G_STATE + 0xc, 0x10);
    assert_eq!(run(&mut m), 0);
    assert_eq!(m.lh(G_ENTITY + 0x18), -94);
    assert_eq!(m.lw(G_STATE + 0xc), 0x11);
    // Ground contact (+0x10 & 9) or counter 0x79: done (v0 = 1, no count).
    m.sw(G_STATE + 0x10, 8);
    assert_eq!(run(&mut m), 1);
    assert_eq!(m.lw(G_STATE + 0xc), 0x11);
    m.sw(G_STATE + 0x10, 0);
    m.sw(G_STATE + 0xc, 0x79);
    assert_eq!(run(&mut m), 1);
    // Counter >= 0x4C: progress 0x1000, rsin(0xC00) = 0 here: pitch -68.
    assert_eq!(m.lh(G_ENTITY + 0x18), -68);
}

#[test]
fn follow_entity_800737e0_offsets() {
    let mut m = g_machine();
    let leader = 0x8012_0000;
    let target = 0x8012_0100;
    m.sw(G_TABLE + 3 * 4, leader);
    m.sw(target, 3);
    for (i, value) in [0x1000, 0x2000, 0x7_0000].into_iter().enumerate() {
        m.sw(target + 4 + i as u32 * 4, value);
    }
    for (i, value) in [0x1_0000, 0x2_0000, 0x3_0000].into_iter().enumerate() {
        m.sw(leader + 8 + i as u32 * 4, value);
    }
    // Leader speed 0x40000 (velocity >> 6 = 0x1000; root table entry 0x1000).
    m.sw(leader + 0x38, 0x4_0000);
    m.sh(0x800b_4c54, 0x1000);
    m.sh(G_ENTITY + 0x1a, 0x1234);
    player::follow_entity_800737e0(&mut m, G_ENTITY, G_STATE, G_CONTROLS, target);
    assert!(m.take_fault().is_ok());
    // Identity orientation: offset (0x40, 0x80, (0x70000 - 0x64000) >> 6)
    // << 6 plus the leader position; forward (0, 0, 0x40000 >> 5) << 5.
    let words: Vec<u32> = [8, 0xc, 0x10, 0x38, 0x3c, 0x40]
        .iter()
        .map(|&o| m.lw(G_ENTITY + o))
        .collect();
    assert_eq!(words, [0x1_1000, 0x2_2000, 0x3_c000, 0, 0, 0x4_0000]);
    assert_eq!(
        [m.lw(G_STATE + 0x48), u32::from(m.lhu(G_STATE + 0x4c))],
        [0x4_0000, 0x1000]
    );
    assert_eq!(m.lhu(G_ENTITY + 0x1a), 0);
    assert_eq!(m.lhu(G_CONTROLS + 6), 0x800);
}
// ==== end of g ====

// ==== h ====
// 80071358 mode handlers. Only the crash check of player-015 (full-throttle
// dive into the ground, frame 503) reaches this group in the captures:
// 80074D5C and 80077EF4. The other handlers run in modes the first-mission
// checkpoints do not reach (see the report).

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-015"]
fn enter_crash_80074d5c() {
    let changed = check("player-015", 0x8007_4d5c, 0..1, |m, _| {
        player::enter_crash_80074d5c(m);
        None
    });
    assert!(changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-015"]
fn crash_camera_80077ef4() {
    let changed = check("player-015", 0x8007_7ef4, 0..1, |m, e| {
        // 80071358 passes 1 (state +0x10 & 0x2F set) on this frame.
        assert_eq!(e.gpr(4), 1);
        player::crash_camera_80077ef4(m, e.gpr(4));
        None
    });
    assert!(changed > 0);
}
// ==== end of h ====

// ==== j ====
// 80050C84 (ground-unit motion) and 8005173C (direction) run only through
// 80050428 for ground units in play; no capture reaches them (the captured
// missions have no ground units in play), so these are synthetic fixtures.
// Tests marked `ignore` load the resident executable for the sine and
// square-root tables.

const J_RECORDS: u32 = 0x8010_0000;
const J_UNITS: u32 = 0x8010_1000;
const J_TABLE: u32 = 0x8010_2000;
const J_PATH: u32 = 0x8011_0000;
const J_ENTITY: u32 = 0x8012_0000;
const J_DEFINITION: u32 = 0x8013_0000;
const J_PLAYER: u32 = 0x8014_0000;
const J_GAMEPLAY: u32 = 0x8015_0000;
const J_MISSION: u32 = 0x8015_1000;
const J_SCRIPTS: u32 = 0x8016_0000;
/// Two empty scripts (a zero byte each) for the event-script paths.
const J_SCRIPT_148: u32 = 0x8016_1000;
const J_SCRIPT_14C: u32 = 0x8016_1010;
const J_STAGE: u32 = 0x800b_aeb4;
/// 800C4D98 slot 0x20: the state word of ground unit 0.
const J_STATE: u32 = 0x800c_4e18;
const J_ANCHORS: u32 = 0x800b_01b4;

/// Ground unit `unit` with motion record kind `kind`: unit records, unit
/// and entity tables (player entity in slot 0), gameplay, mission and
/// script blocks, stage 0.
fn j_unit(m: &mut Machine, unit: u32, kind: u8) {
    m.set_gw(0x378, J_RECORDS);
    m.sw(J_RECORDS + unit * 0x28 + 8, J_DEFINITION);
    m.sw(J_RECORDS + unit * 0x28 + 0x14, J_PATH);
    m.set_gw(0x2c8, J_UNITS);
    m.sw(J_UNITS + unit * 4, J_ENTITY);
    m.set_gw(0x2d8, J_TABLE);
    m.sw(J_TABLE, J_PLAYER);
    m.set_gw(0x2d0, J_GAMEPLAY);
    m.set_gw(0x330, J_MISSION);
    m.set_gw(0x328, J_SCRIPTS);
    m.sw(J_SCRIPTS + 0x148, J_SCRIPT_148);
    m.sw(J_SCRIPTS + 0x14c, J_SCRIPT_14C);
    m.sb(J_SCRIPT_148, 0);
    m.sb(J_SCRIPT_14C, 0);
    m.sb(J_STAGE, 0);
    m.sb(J_PATH, kind);
}

fn j_words(m: &mut Machine, address: u32, words: &[i32]) {
    for (i, &word) in (0u32..).zip(words) {
        m.sw(address + i * 4, word as u32);
    }
}

fn j_vector(m: &mut Machine, address: u32) -> [i32; 3] {
    std::array::from_fn(|i| m.lw(address + i as u32 * 4) as i32)
}

fn j_velocity(m: &mut Machine) -> [i32; 3] {
    j_vector(m, J_ENTITY + 0x38)
}

/// Machine holding the original resident executable at its load address.
fn j_resident() -> Machine {
    let path = PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR"))
        .join("extracted/programs/ACE.unpacked.exe");
    let exe = std::fs::read(path).expect("ACE.unpacked.exe");
    let word = |at: usize| u32::from_le_bytes(exe[at..at + 4].try_into().unwrap());
    let (address, size) = (word(0x18), word(0x1c) as usize);
    let mut m = Machine::default();
    m.write_bytes(address, &exe[0x800..0x800 + size]);
    m
}

/// The expected 8005173C result for `delta`, by its formula with the
/// translated SquareRoot0: (direction, distance).
fn j_expected_direction(delta: [i32; 3]) -> ([i32; 3], i32) {
    let mut size = delta.map(i32::wrapping_abs);
    let mut shift = 0;
    while size.iter().any(|&s| s >= 0x4000) {
        shift += 1;
        size = size.map(|s| s >> 1);
    }
    let squares: i32 = size.iter().map(|s| s * s).sum();
    let mut tables = j_resident();
    let length = ac3_flight::sdk::square_root0(&mut tables, squares as u32) as i32;
    assert!(length > 0);
    (
        delta.map(|d| ((d >> shift) << 14) / length),
        length << shift,
    )
}

fn j_scale(speed: i32, component: i32) -> i32 {
    speed.wrapping_mul(component) >> 14
}

#[test]
fn direction_8005173c_short_and_trap() {
    let mut m = Machine::default();
    let (from, to, out) = (0x8010_0000, 0x8010_0010, 0x8010_0020);
    j_words(&mut m, from, &[0x100, 0x200, -0x300]);
    // |d| = 0xFFF, 0xFFF, 0: all below 0x1000, so out = 0 and v0 = 0.
    j_words(&mut m, to, &[0x100 + 0xfff, 0x200 - 0xfff, -0x300]);
    j_words(&mut m, out, &[0x5555, 0x5555, 0x5555]);
    assert_eq!(player::direction_8005173c(&mut m, from, to, out), 0);
    assert_eq!(j_vector(&mut m, out), [0, 0, 0]);
    // d.x = -0x80000000: `subu zero` keeps it negative, below 0x1000.
    j_words(&mut m, from, &[0, 0x200, -0x300]);
    j_words(&mut m, to, &[i32::MIN, 0x200, -0x300]);
    j_words(&mut m, out, &[0x5555, 0x5555, 0x5555]);
    assert_eq!(player::direction_8005173c(&mut m, from, to, out), 0);
    assert_eq!(j_vector(&mut m, out), [0, 0, 0]);
    assert!(m.take_fault().is_ok());
    // |d.x| = 0x1000 takes the long path; with no square-root table (zero
    // RAM) the length is 0 and the first `div` reaches `break 7`: LO is -1
    // for x and z (dividend >= 0) and 1 for y (negative dividend).
    j_words(&mut m, from, &[0x100, 0x200, -0x300]);
    j_words(&mut m, to, &[0x1100, 0x200 - 0xfff, -0x300]);
    assert_eq!(player::direction_8005173c(&mut m, from, to, out), 0);
    assert_eq!(j_vector(&mut m, out), [-1, 1, -1]);
    assert_eq!(
        m.take_fault(),
        Err(ac3_flight::Fault::Trap {
            address: 0x8005_18a0,
            code: 7
        })
    );
}

#[test]
#[ignore = "requires AC3_DATA_DIR and extracted/programs/ACE.unpacked.exe"]
fn direction_8005173c_scaled() {
    let mut m = j_resident();
    let (from, to, out) = (0x8010_0000, 0x8010_0010, 0x8010_0020);
    // (0x3000, -0x4000, 0): one halving; a 3-4-5 triangle.
    // (-0x12345, 0x5000, 0x70): three halvings; the negative odd x shifts
    // toward minus infinity (-0x2469) while its magnitude gives 0x2468.
    // Wrapping difference: 0x7FFF0000 - (-0x7FFF0000).
    for (a, b) in [
        ([0x1000, 0x2000, 0x3000], [0x4000, -0x2000, 0x3000]),
        ([0x10_0000, 0, -0x10], [0x10_0000 - 0x1_2345, 0x5000, 0x60]),
        ([-0x7fff_0000, 0, 0], [0x7fff_0000, 0x10_0000, 0]),
    ] {
        j_words(&mut m, from, &a);
        j_words(&mut m, to, &b);
        let delta: [i32; 3] = std::array::from_fn(|i| b[i].wrapping_sub(a[i]));
        let (direction, distance) = j_expected_direction(delta);
        let v0 = player::direction_8005173c(&mut m, from, to, out);
        assert_eq!(v0 as i32, distance, "{delta:x?}");
        assert_eq!(j_vector(&mut m, out), direction, "{delta:x?}");
        assert!(m.take_fault().is_ok());
    }
    // The 3-4-5 case: length 0x2800 << 1, direction (0.6, -0.8, 0) * 0x4000.
    j_words(&mut m, from, &[0x1000, 0x2000, 0x3000]);
    j_words(&mut m, to, &[0x4000, -0x2000, 0x3000]);
    let v0 = player::direction_8005173c(&mut m, from, to, out) as i32;
    assert!((0x4ffc..=0x5004).contains(&v0), "{v0:#x}");
    let [x, y, z] = j_vector(&mut m, out);
    assert!((9825..=9835).contains(&x) && (-13112..=-13102).contains(&y) && z == 0);
}

#[test]
fn unit_motion_80050c84_simple_kinds() {
    // Kind 0: velocity 0. A frozen slot (0x400000) or kind >= 10: nothing.
    let mut m = Machine::default();
    j_unit(&mut m, 0, 0);
    j_words(&mut m, J_ENTITY + 0x38, &[1, 2, 3]);
    player::unit_motion_80050c84(&mut m, 0);
    assert_eq!(j_velocity(&mut m), [0, 0, 0]);
    let mut m = Machine::default();
    j_unit(&mut m, 5, 0);
    m.sw(J_STATE + 5 * 4, 0x40_0001);
    j_words(&mut m, J_ENTITY + 0x38, &[1, 2, 3]);
    player::unit_motion_80050c84(&mut m, 5);
    assert_eq!(j_velocity(&mut m), [1, 2, 3]);
    m.sw(J_STATE + 5 * 4, 1);
    m.sb(J_PATH, 10);
    player::unit_motion_80050c84(&mut m, 5);
    assert_eq!(j_velocity(&mut m), [1, 2, 3]);

    // Kind 2: vertical velocity P+4.
    let mut m = Machine::default();
    j_unit(&mut m, 1, 2);
    m.sw(J_PATH + 4, -0x345i32 as u32);
    j_words(&mut m, J_ENTITY + 0x38, &[1, 2, 3]);
    player::unit_motion_80050c84(&mut m, 1);
    assert_eq!(j_velocity(&mut m), [0, -0x345, 0]);

    // Kind 3: -0x100 in phases 0 and 3, +0x100 otherwise; the phase
    // advances when the counter reaches 0x40 and wraps from 4 to 0.
    let mut m = Machine::default();
    j_unit(&mut m, 0, 3);
    m.sw(J_ENTITY + 0x5c, 0x3e);
    j_words(&mut m, J_ENTITY + 0x38, &[7, 0x500, 7]);
    let step = |m: &mut Machine| {
        player::unit_motion_80050c84(m, 0);
        (
            m.lw(J_ENTITY + 0x3c) as i32,
            m.lw(J_ENTITY + 0x5c),
            m.lbu(J_ENTITY + 0x58),
        )
    };
    assert_eq!(step(&mut m), (0x400, 0x3f, 0));
    assert_eq!(j_velocity(&mut m), [0, 0x400, 0]);
    assert_eq!(step(&mut m), (0x300, 0, 1));
    assert_eq!(step(&mut m), (0x400, 1, 1));
    m.sb(J_ENTITY + 0x58, 3);
    m.sw(J_ENTITY + 0x5c, 0x3f);
    assert_eq!(step(&mut m), (0x300, 0, 0));
    m.sb(J_ENTITY + 0x58, 0xff);
    m.sw(J_ENTITY + 0x5c, 0x3f);
    assert_eq!(step(&mut m), (0x400, 0, 0));

    // Kind 6 phase 0: vertical velocity P+4, angle 0x3000 within 0x100000
    // below P+0x10, then the stop at P+0x10 (angle 0x1000, phase 1).
    let mut m = Machine::default();
    j_unit(&mut m, 0, 6);
    m.sw(J_PATH + 4, -0x800i32 as u32);
    m.sw(J_PATH + 0x10, 0x18_0000);
    m.sb(J_ENTITY + 5, 0x10);
    m.sh(J_ENTITY + 6, 0x1234);
    m.sw(J_ENTITY + 0xc, 0);
    m.sw(J_ENTITY + 0x60, 0x99);
    j_words(&mut m, J_ENTITY + 0x38, &[1, 2, 3]);
    player::unit_motion_80050c84(&mut m, 0);
    assert_eq!(j_velocity(&mut m), [0, -0x800, 0]);
    assert_eq!((m.lbu(J_ENTITY + 5), m.lhu(J_ENTITY + 6)), (0x14, 0x1234));
    m.sw(J_ENTITY + 0xc, 0x10_0000);
    player::unit_motion_80050c84(&mut m, 0);
    assert_eq!(m.lhu(J_ENTITY + 6), 0x3000);
    assert_eq!(
        (m.lbu(J_ENTITY + 0x58), m.lw(J_ENTITY + 0xc)),
        (0, 0x10_0000)
    );
    m.sw(J_ENTITY + 0xc, 0x18_0000);
    player::unit_motion_80050c84(&mut m, 0);
    assert_eq!(m.lhu(J_ENTITY + 6), 0x1000);
    assert_eq!(m.lw(J_ENTITY + 0xc), 0x18_0000);
    assert_eq!((m.lbu(J_ENTITY + 0x58), m.lw(J_ENTITY + 0x60)), (1, 0));
    // Phase 1: angle 0x2000 below 0x20 counts, 0x3000 below 0x40, then
    // phase 2 with step 0x200.
    m.sw(J_ENTITY + 0x60, 0x1f);
    player::unit_motion_80050c84(&mut m, 0);
    assert_eq!((m.lhu(J_ENTITY + 6), m.lw(J_ENTITY + 0x60)), (0x2000, 0x20));
    player::unit_motion_80050c84(&mut m, 0);
    assert_eq!((m.lhu(J_ENTITY + 6), m.lw(J_ENTITY + 0x60)), (0x3000, 0x21));
    m.sw(J_ENTITY + 0x60, 0x40);
    player::unit_motion_80050c84(&mut m, 0);
    assert_eq!((m.lhu(J_ENTITY + 6), m.lw(J_ENTITY + 0x60)), (0x3000, 0x41));
    assert_eq!((m.lbu(J_ENTITY + 0x58), m.lw(J_ENTITY + 0x68)), (2, 0x200));
    // Other phases only set E+5 bit 0x4.
    m.sb(J_ENTITY + 0x58, 3);
    m.sb(J_ENTITY + 5, 0);
    player::unit_motion_80050c84(&mut m, 0);
    assert_eq!((m.lbu(J_ENTITY + 5), m.lw(J_ENTITY + 0x60)), (4, 0x41));

    // Kind 7: player position >> 12 inside the box marks the unit
    // (state 0x20000) unless E+5 has 0x4.
    let mut m = Machine::default();
    j_unit(&mut m, 3, 7);
    for (x, y, z, marked) in [
        (-0x4fff, -0x3ff, -0x57ff, true),
        (-0x4801, 0x7ff, -0x1801, true),
        (-0x4800, 0, -0x2000, false),
        (-0x5000, 0, -0x2000, false),
        (-0x4900, -0x400, -0x2000, false),
        (-0x4900, 0, -0x1800, false),
        (-0x4900, 0, -0x5800, false),
    ] {
        j_words(&mut m, J_PLAYER + 8, &[x << 12, y << 12, z << 12]);
        m.sw(J_STATE + 3 * 4, 1);
        player::unit_motion_80050c84(&mut m, 3);
        let expected = if marked { 0x2_0001 } else { 1 };
        assert_eq!(m.lw(J_STATE + 3 * 4), expected, "{x:x} {y:x} {z:x}");
    }
    m.sb(J_ENTITY + 5, 4);
    m.sw(J_STATE + 3 * 4, 1);
    j_words(&mut m, J_PLAYER + 8, &[-0x4900 << 12, 0, -0x2000 << 12]);
    player::unit_motion_80050c84(&mut m, 3);
    assert_eq!(m.lw(J_STATE + 3 * 4), 1);

    // Kind 8: marked while E.z - player.z < P+0xC (signed).
    let mut m = Machine::default();
    j_unit(&mut m, 2, 8);
    for (unit_z, player_z, limit, marked) in [
        (0x5000, 0x2000, 0x3001, true),
        (0x5000, 0x2000, 0x3000, false),
        (0, 0x1_0000, 0, true),
        (0, -0x1_0000, 0x1_0000, false),
    ] {
        m.sw(J_STATE + 2 * 4, 0);
        m.sw(J_ENTITY + 0x10, unit_z as u32);
        m.sw(J_PLAYER + 0x10, player_z as u32);
        m.sw(J_PATH + 0xc, limit as u32);
        player::unit_motion_80050c84(&mut m, 2);
        let expected = if marked { 0x2_0000 } else { 0 };
        assert_eq!(m.lw(J_STATE + 2 * 4), expected, "{unit_z:x} {player_z:x}");
    }

    // Kind 9, units other than 0: placed at the nearest anchor ([gp+0x48C])
    // with +-0x800000 offsets; model byte from the anchor timer's top bits.
    let mut m = Machine::default();
    for (unit, timer, model, x, z) in [
        (1, 0x2abc, 0x12, 0x90_0000, 0xb0_0000),
        (2, 0x2abc, 0x12, -0x70_0000, -0x50_0000),
        (3, 0xc123, 0x1e, 0x90_0000, -0x50_0000),
    ] {
        j_unit(&mut m, unit, 9);
        m.sb(J_DEFINITION + 2, 0x10);
        m.sh(ac3_flight::GP + 0x48c, 2);
        m.sh(ac3_flight::GP + 0x480, 0x1111);
        m.sh(ac3_flight::GP + 0x484, timer);
        j_words(&mut m, J_ANCHORS + 0x20, &[0x10_0000, -0x2000, 0x30_0000]);
        m.sb(J_ENTITY + 5, 0x40);
        player::unit_motion_80050c84(&mut m, unit);
        assert_eq!(m.lbu(J_ENTITY + 4), model, "unit {unit}");
        assert_eq!(m.lbu(J_ENTITY + 5), 0x44);
        assert_eq!(m.lhu(J_ENTITY + 6), timer & 0xfff);
        assert_eq!(
            j_vector(&mut m, J_ENTITY + 8),
            [x, -0x2000, z],
            "unit {unit}"
        );
        assert_eq!(m.lhu(ac3_flight::GP + 0x480), 0x1111, "timers untouched");
    }
    assert!(m.take_fault().is_ok());
}

#[test]
#[ignore = "requires AC3_DATA_DIR and extracted/programs/ACE.unpacked.exe"]
fn unit_motion_80050c84_heading_and_waypoints() {
    // Kind 1: velocity speed * (sin, 0, cos) of P+2 & 0x3FFF, >> 14.
    let mut m = j_resident();
    j_unit(&mut m, 0, 1);
    m.sh(J_PATH + 2, 0xc123);
    m.sw(J_PATH + 8, 0x1_2345);
    j_words(&mut m, J_ENTITY + 0x38, &[1, 2, 3]);
    let sine = i32::from(m.lh(0x8001_0498 + 0x123 * 2));
    let cosine = i32::from(m.lh(0x8001_0498 + 0x1123 * 2));
    assert!(sine > 0 && cosine > sine);
    player::unit_motion_80050c84(&mut m, 0);
    assert_eq!(
        j_velocity(&mut m),
        [j_scale(0x1_2345, sine), 0, j_scale(0x1_2345, cosine)]
    );
    assert!(m.take_fault().is_ok());

    // Kind 4, phase 0 -> 1 once the distance to waypoint 1 is below the
    // speed; phase 1 heads for waypoint 2.
    let mut m = j_resident();
    j_unit(&mut m, 0, 4);
    let speed = 0x5800;
    m.sw(J_PATH + 8, speed as u32);
    j_words(&mut m, J_ENTITY + 8, &[0x1000, 0x2000, 0x3000, 0x77]);
    j_words(&mut m, J_PATH + 0xc, &[0x4000, 0x2000, 0x7000]);
    j_words(&mut m, J_PATH + 0x18, &[0x10_0000, 0x2000, 0x3000]);
    j_words(&mut m, J_PATH + 0x24, &[0x9999, 0x9999, 0x9999]);
    player::unit_motion_80050c84(&mut m, 0);
    let (direction, distance) = j_expected_direction([0x3000, 0, 0x4000]);
    assert!(distance < speed);
    assert_eq!(m.lbu(J_ENTITY + 0x58), 1);
    assert_eq!(j_velocity(&mut m), direction.map(|c| j_scale(speed, c)));
    player::unit_motion_80050c84(&mut m, 0);
    let (direction, distance) = j_expected_direction([0x10_0000 - 0x1000, 0, 0]);
    assert!(distance >= speed);
    assert_eq!(m.lbu(J_ENTITY + 0x58), 1);
    assert_eq!(j_velocity(&mut m), direction.map(|c| j_scale(speed, c)));
    // Phase 2 on stage 0x2B: from the unit to waypoint 3; arriving there
    // moves to phase 3 and runs script +0x148 once (E flag 0x40).
    m.sb(J_STAGE, 0x2b);
    m.sb(J_ENTITY + 0x58, 2);
    m.sw(J_ENTITY, 0xc0);
    m.set_gw(0x488, 0x100);
    j_words(&mut m, J_PATH + 0x24, &[0x1000, 0x2000, 0x3000]);
    player::unit_motion_80050c84(&mut m, 0);
    assert_eq!(m.lbu(J_ENTITY + 0x58), 3);
    assert_eq!(m.gw(0x488), 0x140);
    assert_eq!(j_velocity(&mut m), [0, 0, 0]);
    // Phase 3 on stage 0x2B starts at waypoint 2 (the unit position no
    // longer matters); at distance 0 the second script runs once (0x80).
    j_words(&mut m, J_ENTITY + 8, &[0x9000, 0, 0]);
    j_words(&mut m, J_PATH + 0x18, &[0x5_0000, 0x1000, 0]);
    j_words(&mut m, J_PATH + 0x24, &[0x5_0000, 0x1000, 0]);
    player::unit_motion_80050c84(&mut m, 0);
    assert_eq!(m.gw(0x488), 0x1c0);
    assert_eq!(j_velocity(&mut m), [0, 0, 0]);
    player::unit_motion_80050c84(&mut m, 0);
    assert_eq!(m.gw(0x488), 0x1c0);
    // Off stage 0x2B, phase 3 heads from the unit to waypoint 3.
    m.sb(J_STAGE, 0);
    player::unit_motion_80050c84(&mut m, 0);
    let (direction, _) = j_expected_direction([0x5_0000 - 0x9000, 0x1000, 0]);
    assert_eq!(j_velocity(&mut m), direction.map(|c| j_scale(speed, c)));
    assert_eq!(m.lbu(J_ENTITY + 0x58), 3);
    assert!(m.take_fault().is_ok());

    // Kind 5: patrol; a distance below the speed toggles the phase.
    let mut m = j_resident();
    j_unit(&mut m, 0, 5);
    let speed = 0x2100;
    m.sw(J_PATH + 8, speed as u32);
    j_words(&mut m, J_ENTITY + 8, &[0, 0, 0]);
    j_words(&mut m, J_PATH + 0xc, &[0x2000, 0, 0]);
    j_words(&mut m, J_PATH + 0x18, &[0, -0x8_0000, 0]);
    player::unit_motion_80050c84(&mut m, 0);
    let (direction, distance) = j_expected_direction([0x2000, 0, 0]);
    assert!(distance < speed);
    assert_eq!(m.lbu(J_ENTITY + 0x58), 1);
    assert_eq!(j_velocity(&mut m), direction.map(|c| j_scale(speed, c)));
    player::unit_motion_80050c84(&mut m, 0);
    let (direction, _) = j_expected_direction([0, -0x8_0000, 0]);
    assert_eq!(m.lbu(J_ENTITY + 0x58), 1);
    assert_eq!(j_velocity(&mut m), direction.map(|c| j_scale(speed, c)));
    j_words(&mut m, J_PATH + 0x18, &[0, -0x1000, 0]);
    player::unit_motion_80050c84(&mut m, 0);
    assert_eq!(m.lbu(J_ENTITY + 0x58), 0);
    assert!(m.take_fault().is_ok());

    // Kind 6 phase 2: E.y limited to P+0x10, run to waypoint 1, script
    // +0x148 once at distance 0 (mission word bit 0x40), angle steps.
    let mut m = j_resident();
    j_unit(&mut m, 0, 6);
    let speed = 0x300;
    m.sw(J_PATH + 8, speed as u32);
    m.sw(J_PATH + 0x10, 0x4000);
    j_words(&mut m, J_PATH + 0xc, &[0x1000, 0x4000, 0x2000]);
    j_words(&mut m, J_ENTITY + 8, &[0x1000, 0x5000, 0x2000]);
    m.sb(J_ENTITY + 0x58, 2);
    m.sh(J_ENTITY + 6, 0x4000);
    m.sw(J_MISSION, 1);
    j_words(&mut m, J_ENTITY + 0x38, &[1, 2, 3]);
    player::unit_motion_80050c84(&mut m, 0);
    assert_eq!(m.lw(J_ENTITY + 0xc), 0x4000);
    assert_eq!(m.lw(J_MISSION), 0x41);
    assert_eq!(j_velocity(&mut m), [0, 0, 0]);
    assert_eq!(m.lw(J_ENTITY + 0x68) as i32, -0x200);
    assert_eq!((m.lhu(J_ENTITY + 6), m.lbu(J_ENTITY + 5)), (0x3e00, 4));
    m.sw(J_MISSION, 0x40);
    m.sw(J_ENTITY + 0xc, 0x3000);
    player::unit_motion_80050c84(&mut m, 0);
    let (direction, _) = j_expected_direction([0, 0x1000, 0]);
    assert_eq!(j_velocity(&mut m), direction.map(|c| j_scale(speed, c)));
    assert_eq!(m.lw(J_ENTITY + 0xc), 0x3000);
    assert_eq!(m.lw(J_MISSION), 0x40);
    assert_eq!(m.lhu(J_ENTITY + 6), 0x3c00);
    m.sh(J_ENTITY + 6, 0x2000);
    player::unit_motion_80050c84(&mut m, 0);
    assert_eq!(
        (m.lhu(J_ENTITY + 6), m.lw(J_ENTITY + 0x68)),
        (0x2200, 0x200)
    );
    assert!(m.take_fault().is_ok());
}

#[test]
#[ignore = "requires AC3_DATA_DIR and extracted/programs/ACE.unpacked.exe"]
fn unit_motion_80050c84_anchors() {
    // Kind 9, unit 0: nearest anchor (800827B4 of position >> 12), anchor
    // timers counting down on odd frames, -0x10 more within 0x4000 and
    // stopping at 0; outside gameplay mode 4 no overlay.
    let setup = |player_x: i32, timers: [u16; 3], frame: u32, mode: u32| {
        let mut m = j_resident();
        j_unit(&mut m, 0, 9);
        m.sb(J_DEFINITION + 2, 0x20);
        for (i, x) in (0u32..).zip([0, 0x1000_0000, 0x2000_0000]) {
            j_words(&mut m, J_ANCHORS + i * 0x10, &[x, 0, 0]);
            m.sh(ac3_flight::GP + 0x480 + i * 2, timers[i as usize]);
        }
        m.sh(ac3_flight::GP + 0x48c, 0);
        j_words(&mut m, J_PLAYER + 8, &[player_x, 0, 0]);
        m.sw(J_GAMEPLAY + 4, mode);
        m.sw(J_GAMEPLAY + 0xc, frame);
        m
    };
    let timers = |m: &mut Machine| -> [u16; 3] {
        std::array::from_fn(|i| m.lhu(ac3_flight::GP + 0x480 + i as u32 * 2))
    };
    // Distance 0x4800 to anchor 2 (0xB800 to anchor 1), odd frame. (800827B4
    // truncates reduced components above 0x7FFF on its unscaled path, so
    // the positions keep clear of it.)
    let mut m = setup(0x1b80_0000, [0x3000, 0, 0x2000], 1, 2);
    player::unit_motion_80050c84(&mut m, 0);
    assert_eq!(m.lh(ac3_flight::GP + 0x48c), 2);
    assert_eq!(timers(&mut m), [0x2fff, 0, 0x1fff]);
    assert_eq!(m.lbu(J_ENTITY + 4), 0x24);
    assert_eq!(m.lhu(J_ENTITY + 6), 0xfff);
    assert_eq!(
        j_vector(&mut m, J_ENTITY + 8),
        [0x2000_0000 - 0x80_0000, 0, 0x80_0000]
    );
    assert!(m.take_fault().is_ok());
    // Distance 0x1000, even frame: the nearest timer loses 0x10 and stops
    // at 0 (model byte + 6, angle 0).
    let mut m = setup(0x2100_0000, [0x3000, 0, 5], 2, 2);
    player::unit_motion_80050c84(&mut m, 0);
    assert_eq!(m.lh(ac3_flight::GP + 0x48c), 2);
    assert_eq!(timers(&mut m), [0x3000, 0, 0]);
    assert_eq!((m.lbu(J_ENTITY + 4), m.lhu(J_ENTITY + 6)), (0x26, 0));
    // Anchor 0 nearest: index 0 written on the first comparison.
    let mut m = setup(0x100_0000, [0x10, 0x10, 0x10], 2, 2);
    m.sh(ac3_flight::GP + 0x48c, 1);
    player::unit_motion_80050c84(&mut m, 0);
    assert_eq!(m.lh(ac3_flight::GP + 0x48c), 0);
    assert_eq!(timers(&mut m), [0, 0x10, 0x10]);
    assert!(m.take_fault().is_ok());
    // Gameplay mode 4 at 0x3000 or more: the overlay level falls by 0x40
    // and stops at 0 without drawing (8008E42C runs only while it stays
    // positive); mission bit 0x8 clears. Frame 0 resets the level first.
    for (frame, level) in [(2, 0x30), (0, 0x100)] {
        let mut m = setup(0x1b80_0000, [0x3000, 0, 0x2000], frame, 4);
        m.set_gw(0x47c, level);
        m.sw(J_MISSION, 0xf);
        m.sw(J_GAMEPLAY + 0x14, 0x1234);
        player::unit_motion_80050c84(&mut m, 0);
        assert_eq!(m.gw(0x47c), 0, "frame {frame}");
        assert_eq!(m.lw(J_MISSION), 7);
        assert_eq!(m.lw(J_GAMEPLAY + 0x14), 0x1234);
        assert!(m.take_fault().is_ok());
    }
}
// ==== end of j ====

// ==== a2 ====
// Ground units (the 80050428 subtree, 80038010, 800458E0, 80044664) are not
// reached by any capture (no ground units, wrecks or lock releases through
// 800458E0 in missions 006/007): they are translated only. This synthetic
// test (not snapshot-validated) walks the motion-script stepper 80050578
// through its forward, restart, turn-around, stop and backward-wreck paths
// on a hand-built unit 0 (slot 0x20).

const A2_UNITS: u32 = 0x8010_0000;
const A2_RECORDS: u32 = 0x8010_1000;
const A2_ENTITY: u32 = 0x8010_2000;
const A2_DESCRIPTOR: u32 = 0x8010_3000;
const A2_STATE: u32 = 0x800c_4d98 + 0x20 * 4;

/// Unit 0 with a 3-step script (durations 0x100, 0x200, 0x100), end `mode`
/// and `rate`; slot state: in play, script running.
fn a2_unit_machine(mode: u8, rate: u32) -> Machine {
    let mut m = Machine::default();
    m.set_gw(0x2c8, A2_UNITS);
    m.set_gw(0x378, A2_RECORDS);
    m.sw(A2_UNITS, A2_ENTITY);
    m.sw(A2_RECORDS + 0x18, A2_DESCRIPTOR);
    m.sb(A2_DESCRIPTOR + 2, mode);
    m.sb(A2_DESCRIPTOR + 3, 3);
    for (i, duration) in [0x100u16, 0x200, 0x100].into_iter().enumerate() {
        m.sh(A2_DESCRIPTOR + 4 + i as u32 * 2, duration);
    }
    m.sw(A2_ENTITY + 0x68, rate);
    m.sw(A2_STATE, 0x0102_0001);
    m
}

#[test]
fn a2_unit_script_synthetic() {
    let steps = |m: &mut Machine, frames: usize| -> Vec<u16> {
        (0..frames)
            .map(|_| {
                player::step_unit_script_80050578(m, 0);
                m.lhu(A2_ENTITY + 6)
            })
            .collect()
    };
    // Rate 0x80: step 0 ends after frame 3, step 1 after frame 7 (the last
    // step); mode 3 restarts.
    let mut m = a2_unit_machine(3, 0x80);
    let progress = steps(&mut m, 7);
    assert_eq!(progress, [0, 0x800, 0x1000, 0x1400, 0x1800, 0x1c00, 0x2000]);
    assert_eq!((m.lb(A2_ENTITY + 0x59), m.lw(A2_ENTITY + 0x60)), (0, 0));
    assert_eq!(m.lbu(A2_ENTITY + 5), 4);
    // Mode 0 turns around at step count - 2 with its duration and rate -1.
    let mut m = a2_unit_machine(0, 0x80);
    steps(&mut m, 7);
    assert_eq!(
        (
            m.lb(A2_ENTITY + 0x59),
            m.lw(A2_ENTITY + 0x60),
            m.lw(A2_ENTITY + 0x68)
        ),
        (1, 0x200, u32::MAX)
    );
    // Mode 1 stops the script (state bit 0x20000) on the last step.
    let mut m = a2_unit_machine(1, 0x80);
    steps(&mut m, 7);
    assert_eq!((m.lb(A2_ENTITY + 0x59), m.lw(A2_STATE)), (2, 0x0100_0001));
    // Running back past step 0 with mode 5 stops and wrecks (80050958).
    let mut m = a2_unit_machine(5, (-0x20i32) as u32);
    m.sw(A2_ENTITY + 0x60, 0x10);
    m.sw(A2_ENTITY, 0x0001_c400);
    assert_eq!(steps(&mut m, 1), [0x100]);
    assert_eq!(m.lb(A2_ENTITY + 0x59), -1);
    assert_eq!(m.lw(A2_STATE), 0x0100_0004);
    assert_eq!(m.lw(A2_ENTITY), 0x4000);
    assert_eq!(m.lbu(0x800c_4e98 + 0x20), 0x96);
    assert!(m.take_fault().is_ok());
}

// ==== end of a2 ====

// ==== c2 ====
// No capture reaches 80098DB0, 80097404, 800972B0, 800999FC or 80098EF4
// (the call trees player-006/player-014 have none); the tests below check
// them against the captured siblings 80097EDC/80098B80.

/// Entry snapshot `occurrence` of `callee` in capture `name`.
fn c2_entry(name: &str, callee: u32, occurrence: usize) -> Snapshot {
    let index_file = capture(name);
    let index = snapshot::read_index(&index_file).expect("index");
    let entry = index
        .iter()
        .filter(|s| s.callee == callee && s.stage == "entry")
        .nth(occurrence)
        .expect("entry");
    Snapshot::load(index_file.parent().expect("directory"), entry).expect("snapshot")
}

/// RAM word of a snapshot.
fn c2_word(snapshot: &Snapshot, address: u32) -> u32 {
    let at = (address & 0x1f_ffff) as usize;
    u32::from_le_bytes(snapshot.ram[at..at + 4].try_into().expect("word"))
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004"]
fn object_hit_80098ef4_as_80097edc() {
    // 80098EF4 is 80097EDC's code for one block given directly. On
    // 80097EDC's captured calls (one-entry lists) it must reproduce the
    // return snapshots exactly: RAM, scratchpad, GTE, v0, the hit point.
    let (changed, stack_changed) = check_with_stack_output(0x8009_7edc, 5, 12, |m, e| {
        let list = stack_arg(m, e, 0);
        assert_ne!(m.lw(list.wrapping_add(0x14)) & 0x20_0000, 0, "one entry");
        let index = m.lw(list.wrapping_add(8)) & 0x1ff;
        let block = m.lw(e.gpr(7).wrapping_add(index * 4));
        let vertices = m.lw(block.wrapping_add(4));
        Some(player::object_hit_80098ef4(
            m,
            e.gpr(4),
            e.gpr(5),
            e.gpr(6),
            block,
            vertices,
            list,
        ))
    });
    assert!(changed > 0 && stack_changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-001, player-004"]
fn ground_ray_cells_80098db0_near_80098b80() {
    // Consistency, not a snapshot validation: on 80098B80's captured rays
    // (vertical, one cell) the cell-list variant must hit the same face
    // ([gp+0x554]/[gp+0x558]) at the same distance up to the rounding of
    // its >> 12 coordinates (list y = -(from.y >> 12) against
    // (-from.y) >> 12, ray (to >> 12) - (from >> 12)).
    for name in ["player-001", "player-004"] {
        for occurrence in 0..2 {
            let entry = c2_entry(name, 0x8009_8b80, occurrence);
            let exit = exit_snapshot(name, 0x8009_8b80, occurrence);
            let mut m = entry.machine();
            let args = [entry.gpr(4), entry.gpr(5), entry.gpr(6), entry.gpr(7)];
            let v0 = player::ground_ray_cells_80098db0(&mut m, args[0], args[1], args[2], args[3]);
            assert_eq!(m.take_fault(), Ok(()), "{name} #{occurrence}");
            let expected = exit.gpr(2);
            assert_ne!(expected, 0x7fff_ffff);
            assert!(
                v0.abs_diff(expected) <= 1,
                "{name} #{occurrence}: {v0:#x} vs {expected:#x}"
            );
            for address in [0x800b_af48, 0x800b_af4c] {
                assert_eq!(
                    m.lw(address),
                    c2_word(&exit, address),
                    "{name} #{occurrence}"
                );
            }
        }
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-015"]
fn crash_collisions_8006f670_8006f3fc() {
    // The crash dive (frames 501/503/505): terrain contacts through 80098B80
    // with the direct 80098DB0/800999FC call sites in place.
    let changed = check("player-015", 0x8006_f670, 0..3, |m, e| {
        player::world_collision_8006f670(m, e.gpr(4), e.gpr(5));
        None
    });
    check("player-015", 0x8006_f3fc, 0..3, |m, e| {
        player::collide_entities_8006f3fc(m, e.gpr(4), e.gpr(5));
        None
    });
    assert!(changed > 0, "{changed}");
}

// ==== end of c2 ====
