//! Overlay module 5 (state 9, the music player, `ac3_flight::ov5`) against
//! original entry/return snapshots.
//!
//! Capture `ovmisc-014`: from the title (`overlay-entry.sav`, CROSS at
//! 301) the documented one-register state selection (`.ovmisc/
//! snapshots.lua`: the first state-callback result 0 on or after frame 450
//! becomes 0x60000009) enters state 9; module 5 loads (frames 451..460)
//! and the player runs its background flight. Pad: START at 1100 (play
//! track 1), RIGHT at 1300 (next), START at 1400 (pause), TRIANGLE at 1500
//! (stop) and 1600 (eject). Snapshots are taken only while module 5 is
//! loaded.
//!
//! 8002B160 (CD driver flags, `cdstream.rs`, not in this workspace yet) is
//! replayed from its own snapshots in the same call. CD-interrupt state
//! (monitor/driver blocks, ring state, XA stream) is interrupt-owned.
use ac3_flight::{
    Machine, dispatch, ov5,
    snapshot::{self, Snapshot},
};
use std::path::{Path, PathBuf};

fn capture(name: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR"))
        .join("analysis/flight-runtime")
        .join(name)
        .join("snapshots.jsonl")
}

const OWNED: &[(u32, u32, &str)] = &[
    (0x8000_0000, 0x8001_0000, "BIOS kernel RAM"),
    (0x800b_256c, 0x800b_2570, "sound interrupt flag"),
    (0x800b_2ec4, 0x800b_2ec8, "VSync root-counter reading"),
    (
        0x800b_2f2c,
        0x800b_3f44,
        "libetc interrupt context and stack",
    ),
    (0x800b_3ff4, 0x800b_3ff8, "VSync counter"),
    (0x800b_4010, 0x800b_4014, "GPU DMA callback slot"),
    (0x800b_990c, 0x800b_9914, "libgpu queue head/tail"),
    (0x800b_9914, 0x800b_9918, "libgpu saved interrupt mask"),
    (0x800b_9920, 0x800b_9928, "libgpu timeout"),
    (0x800b_f150, 0x800b_f1d0, "CD monitor and driver state"),
    (0x800c_31d0, 0x800c_3420, "CD ring state and sector headers"),
    (0x800c_5bcc, 0x800c_5bce, "sound driver tick state"),
    (0x800c_5c70, 0x800c_5c72, "sound driver tick state"),
    (0x800c_8230, 0x800c_9a30, "libgpu queue entries"),
];
fn owner(address: u32) -> Option<&'static str> {
    OWNED
        .iter()
        .find(|(s, e, _)| (*s..*e).contains(&address))
        .map(|(_, _, n)| *n)
}

struct Pair {
    entry: Snapshot,
    exit: Snapshot,
}
fn pair(index: &Path, callee: u32, occurrence: usize) -> Pair {
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
    Pair {
        entry: Snapshot::load(directory, entry).unwrap(),
        exit: Snapshot::load(directory, exit).unwrap(),
    }
}
fn count(index: &Path, callee: u32) -> usize {
    snapshot::read_index(index)
        .unwrap()
        .iter()
        .filter(|s| s.callee == callee && s.stage == "entry")
        .count()
}

/// A recorded call: callee, RAM and scratchpad bytes it changed, GTE
/// registers at its return, `v0`, used.
type Recording = (u32, Vec<(usize, u8)>, Vec<(usize, u8)>, Gte, u32, bool);
type Gte = ([u32; 32], [u32; 32]);

/// Recorded calls of `pending` inside one call.
fn recorded(index: &Path, p: &Pair, pending: &[u32]) -> Vec<Recording> {
    let directory = index.parent().unwrap();
    let all = snapshot::read_index(index).unwrap();
    let window = p.entry.info.sequence..p.exit.info.sequence;
    let mut out = Vec::new();
    for e in all.iter().filter(|s| {
        s.stage == "entry" && pending.contains(&s.callee) && window.contains(&s.sequence)
    }) {
        let Some(x) = all.iter().find(|s| {
            s.callee == e.callee
                && s.stage == "return"
                && s.sequence > e.sequence
                && s.ra == e.ra
                && s.registers.gpr[29] == e.registers.gpr[29]
        }) else {
            continue;
        };
        let a = Snapshot::load(directory, e).unwrap();
        let b = Snapshot::load(directory, x).unwrap();
        let diff = (0..snapshot::STACK_START)
            .filter(|&i| a.ram[i] != b.ram[i])
            .map(|i| (i, b.ram[i]))
            .collect();
        let scratch = (0..a.scratch.len())
            .filter(|&i| a.scratch[i] != b.scratch[i])
            .map(|i| (i, b.scratch[i]))
            .collect();
        let gte = (b.info.registers.cp2d, b.info.registers.cp2c);
        out.push((e.callee, diff, scratch, gte, b.gpr(2), false));
    }
    out
}

/// Run `native` (with the pending routines replayed) and compare.
fn check(
    index: &Path,
    callee: u32,
    occurrence: usize,
    pending: &[u32],
    native: impl FnOnce(&mut Machine, &Snapshot, &mut ov5_invoker::Call) -> Option<u32>,
) -> usize {
    let p = pair(index, callee, occurrence);
    let mut calls = recorded(index, &p, pending);
    let mut m = p.entry.machine();
    let v0 = {
        let mut c = |m: &mut Machine, t: u32, a: [u32; 4]| -> u32 {
            if !pending.contains(&t) {
                return dispatch::call(m, t, a);
            }
            let call = calls
                .iter_mut()
                .find(|c| c.0 == t && !c.5)
                .unwrap_or_else(|| panic!("{t:08x}: no recording"));
            call.5 = true;
            for &(i, b) in &call.1 {
                let address = 0x8000_0000 + i as u32;
                if owner(address).is_none() {
                    m.sb(address, b);
                }
            }
            for &(i, b) in &call.2 {
                m.sb(0x1f80_0000 + i as u32, b);
            }
            (m.gte.data, m.gte.control) = call.3;
            call.4
        };
        native(&mut m, &p.entry, &mut c)
    };
    m.take_fault()
        .unwrap_or_else(|f| panic!("{callee:08x}#{occurrence}: {f}"));
    let d = snapshot::compare(&m, &p.exit);
    assert!(
        d.scratch.is_empty() && d.gte_data.is_empty() && d.gte_control.is_empty(),
        "{callee:08x}#{occurrence}: {}",
        d.summary()
    );
    let foreign: Vec<u32> = d
        .ram
        .iter()
        .copied()
        .filter(|&a| owner(a).is_none())
        .collect();
    assert!(
        foreign.is_empty(),
        "{callee:08x}#{occurrence}: {} bytes differ: {:08x?}",
        foreign.len(),
        &foreign[..foreign.len().min(24)]
    );
    if let Some(v0) = v0 {
        assert_eq!(v0, p.exit.gpr(2), "{callee:08x}#{occurrence}: v0");
    }
    let changed = snapshot::changed_bytes(&p.entry, &p.exit);
    assert!(changed > 0 || v0.is_some(), "{callee:08x}: vacuous");
    changed
}

mod ov5_invoker {
    pub type Call<'a> = dyn FnMut(&mut ac3_flight::Machine, u32, [u32; 4]) -> u32 + 'a;
}

/// 80033DA8: state 2's demo-flight callback (substate 9), not in this
/// workspace: replayed from its snapshots in `ovmisc-015`.
const DEMO_FLIGHT: u32 = 0x8003_3da8;

#[test]
fn claims_only_while_loaded() {
    let mut m = Machine::default();
    m.sw(0x800b_a9f4 + 0x2dc, 0x800b_d158);
    m.sw(0x800b_d158 + 0x38, 1);
    assert_eq!(ov5::dispatch(&mut m, 0x800c_a6c8, [0; 4]), None);
    m.sw(0x800b_d158 + 0x38, 5);
    m.sw(0x8010_0000, 0x0012_3456);
    let next = ov5::dispatch(&mut m, 0x800c_a6c8, [0x8010_0000, 0x8010_0100, 0x0fff, 0]);
    assert_eq!(next, Some(0x8010_0108));
    assert_eq!(m.lw(0x8010_0104), 0xe100_09ff);
    assert_eq!(m.lw(0x8010_0100), 0x0112_3456);
    assert_eq!(m.lw(0x8010_0000), 0x0010_0100);
    assert!(m.take_fault().is_ok());
}

#[test]
#[ignore = "needs analysis/flight-runtime/ovmisc-014"]
fn music_player_matches_original() {
    let index = capture("ovmisc-014");
    let changed = check(&index, 0x800c_a154, 0, &[], |m, _, c| {
        ov5::setup_player(m, c);
        None
    });
    println!("800CA154: {changed} bytes");
    let changed = check(&index, 0x800c_a0a0, 0, &[], |m, _, c| {
        Some(ov5::start(m, c))
    });
    println!("800CA0A0: {changed} bytes");
    for occurrence in 0..count(&index, 0x800c_a720) {
        let changed = check(&index, 0x800c_a720, occurrence, &[], |m, e, c| {
            Some(ov5::draw(m, c, e.gpr(4), e.gpr(5)))
        });
        println!("800CA720 #{occurrence}: {changed} bytes");
    }
    for occurrence in 0..count(&index, 0x800c_a1a8) {
        let changed = check(&index, 0x800c_a1a8, occurrence, &[], |m, _, c| {
            Some(ov5::update(m, c))
        });
        println!("800CA1A8 #{occurrence}: {changed} bytes");
    }
}

#[test]
#[ignore = "needs analysis/flight-runtime/ovmisc-014"]
fn debug_font_text_matches_original() {
    let index = capture("ovmisc-014");
    for occurrence in 0..count(&index, 0x8002_7168) {
        let changed = check(&index, 0x8002_7168, occurrence, &[], |m, e, _| {
            Some(dispatch::call(
                m,
                0x8002_7168,
                [e.gpr(4), e.gpr(5), e.gpr(6), 0],
            ))
        });
        println!("80027168 #{occurrence}: {changed} bytes");
    }
    for occurrence in 0..count(&index, 0x8002_7228) {
        let changed = check(&index, 0x8002_7228, occurrence, &[], |m, e, _| {
            Some(dispatch::call(
                m,
                0x8002_7228,
                [e.gpr(4), e.gpr(5), e.gpr(6), e.gpr(7)],
            ))
        });
        println!("80027228 #{occurrence}: {changed} bytes");
    }
}

/// `ovmisc-015` (same inputs as `ovmisc-014`): whole player frames at the
/// first START (frames 1100, 1102) and during the eject countdown (1618..
/// 1622), with the background demo flight (80033DA8) replayed.
#[test]
#[ignore = "needs analysis/flight-runtime/ovmisc-015"]
fn music_player_frames_match_original() {
    let index = capture("ovmisc-015");
    for occurrence in 0..count(&index, 0x800c_9fec) {
        let changed = check(
            &index,
            0x800c_9fec,
            occurrence,
            &[DEMO_FLIGHT],
            |m, e, c| Some(ov5::run_flight(m, c, e.gpr(4), e.gpr(5))),
        );
        println!("800C9FEC #{occurrence}: {changed} bytes");
    }
    for occurrence in 0..count(&index, 0x800c_a0dc) {
        let changed = check(
            &index,
            0x800c_a0dc,
            occurrence,
            &[DEMO_FLIGHT],
            |m, e, c| Some(ov5::frame(m, c, e.gpr(4), e.gpr(5))),
        );
        println!("800CA0DC #{occurrence}: {changed} bytes");
    }
}
