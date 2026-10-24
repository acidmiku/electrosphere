//! Overlay 1 (front-end menus, `ov1*.rs`) on original function snapshots.
//!
//! Every snapshot entry of a capture runs through `ov1::dispatch` with its
//! original `a0..a3` and must reproduce the matching return snapshot: all
//! RAM outside the stack, the scratchpad, every GTE register and, for
//! routines with a result, `v0`. Captures (module 1 loaded at every entry,
//! taken with the private `.ov1/snapshots.lua` observer, which also logs
//! the device routines' results in `logs.jsonl`):
//!
//! - `ov1-010`: cold start from the state-4 entry checkpoint
//!   (`server-runs/.../overlay-entry.sav`, frames 0..400): initialization,
//!   menu state 0 (card probe, resource 0x45 and its textures), the
//!   language screen with down/down/up/confirm, the title fade;
//! - `ov1-011`: from the language screen (`menu-runtime/uploads-001`):
//!   confirm, title, START, main menu, New Game, the difficulty screen
//!   (frames 0..900);
//! - `ov1-012`: as ov1-011 to the main menu, then the load screen without
//!   a save (card probe, "no data"), cancel back to the main menu and to
//!   the title (frames 0..700);
//! - `ov1-013`: from the difficulty screen (`menu-runtime/continuation-001`):
//!   down, confirm, the new-game commit and the exit to the hub state
//!   (frames 0..300);
//! - `ov1-014`: the title left idle until its timeout (attract mode, frame
//!   1057);
//! - `ov1-018`: the cold start with context +0x970 = 1 and return flag
//!   +0x964 = 4 written at the first entry of 800CB2BC (a documented RAM
//!   setup by `.ov1/patch-return.lua`, copied into the capture): menu
//!   state 8 (the return to the menus) and the resource loading under it;
//! - `ov1-019`: the cold start with a memory card holding two saves (built
//!   by `.ov1/mkcard.py` from the overlay's own card block, placed as the
//!   profile's `memcard1.mcd` by `.ov1/capture.py`): the save read during
//!   boot, then language, title, main menu, the load screen listing the
//!   saves, and loading one (frames 0..800);
//! - `ov1-022`, `ov1-023`, `ov1-024`: oracle vectors (see
//!   [`check_vectors`]) for the paths no capture reaches. `ov1-022` and
//!   `ov1-023` supersede `ov1-020` and `ov1-021`, generated before the
//!   oracle started from the entry registers (it ran with gp = 0 there).
//!
//! Device results: the device-facing routines ([`REPLAYED`]: TestEvent,
//! `sdk::test_event`, and the BIOS event, critical-section, file and card
//! calls of `bios.rs`) are answered in call order with the original's
//! logged results by a test [`Device`], which also requires every logged
//! call, made with the argument registers the original passed.
//! Allowances (all differences outside them fail):
//! - the upper halfword of VZ0..VZ2 ([`VZ`], as in `tests/frame.rs`);
//! - `0x800B9924` (`sdk::GPU_TIMEOUT_POLLS`, as in `tests/frame.rs`):
//!   libgpu's DMA poll count in ClearOTagR / DrawSync, which counts
//!   hardware time (the native transfer completes at once);
//! - `0x80000000..0x80010000`: the BIOS kernel's RAM (event control blocks,
//!   the exception handler's register save area and its dispatch state).
//!   The BIOS writes it in the event, file and card calls (host calls here)
//!   and in the exception handler of the VBlank, DMA and CD interrupts
//!   that arrive inside a window; no translated routine writes there.
//!
//! In a window where interrupts demonstrably ran (libetc's interrupt
//! manager, which holds the VBlank counter, or libgpu's queue indices
//! differ between the original's entry and return), the driver state
//! those handlers and the queued GPU transfers own is excluded as well
//! ([`INTERRUPTS`]: the ranges of `tests/boot.rs` and the CD stream state
//! the resident CD callbacks update while the menu music streams): the
//! native LoadImage is a synchronous host call and interrupts are the
//! host's.
//!
//! A routine owned elsewhere that is not translated yet raises
//! `Fault::Untranslated`; such an occurrence is reported as BLOCKED, not
//! failed, and becomes a full comparison when the routine lands (none
//! left: libc, libgte, the CD mix and the BIOS layer have landed). Any
//! other fault or difference fails.
use ac3_flight::{
    Device, Fault, HostCall, Machine, ov1, ov1_script, ov1_text, sdk,
    snapshot::{self, Snapshot, SnapshotInfo},
};
use serde_json::Value;
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, VecDeque},
    fs,
    path::{Path, PathBuf},
    rc::Rc,
};

mod support;
use support::oracle;

fn capture(name: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR"))
        .join("analysis/flight-runtime")
        .join(name)
}

/// One `logs.jsonl` record of the observer (`a` the argument registers at
/// that stage).
#[derive(Clone, Debug)]
struct Log {
    sequence: u64,
    stage: String,
    callee: u32,
    a: [u32; 4],
    v0: u32,
}

fn read_logs(directory: &Path) -> Vec<Log> {
    let Ok(text) = fs::read_to_string(directory.join("logs.jsonl")) else {
        return Vec::new();
    };
    text.lines()
        .map(|line| {
            let v: Value = serde_json::from_str(line).expect("log line");
            let int = |name: &str| v[name].as_u64().expect(name);
            Log {
                sequence: int("sequence"),
                stage: v["stage"].as_str().unwrap_or_default().to_owned(),
                callee: int("callee") as u32,
                a: ["a0", "a1", "a2", "a3"].map(|r| int(r) as u32),
                v0: int("v0") as u32,
            }
        })
        .collect()
}

/// Queued results per function: the original's arguments when logged
/// (checked), and its `v0`.
type Queues = HashMap<u32, VecDeque<(Option<[u32; 4]>, u32)>>;

/// What a [`Replay`] has left and found.
#[derive(Default)]
struct ReplayState {
    queues: Queues,
    mismatches: Vec<String>,
}
impl ReplayState {
    /// Mismatched or extra calls, then logged calls never made.
    fn problems(&self) -> Vec<String> {
        let mut problems = self.mismatches.clone();
        for (function, queue) in &self.queues {
            if !queue.is_empty() {
                problems.push(format!(
                    "{function:08x}: {} logged calls not made",
                    queue.len()
                ));
            }
        }
        problems
    }
}

/// Answers host calls of the queued functions with the original's results
/// in call order. Where the original's arguments were logged, the call must
/// match them ([`compared_arguments`]); a mismatch or a call beyond the
/// queue is recorded ([`ReplayState::problems`]).
struct Replay(Rc<RefCell<ReplayState>>);
impl Replay {
    fn new(queues: Queues) -> (Self, Rc<RefCell<ReplayState>>) {
        let state = Rc::new(RefCell::new(ReplayState {
            queues,
            mismatches: Vec::new(),
        }));
        (Self(state.clone()), state)
    }
    /// Results without logged arguments (oracle vectors).
    fn results(results: HashMap<u32, VecDeque<u32>>) -> Self {
        let queues = results
            .into_iter()
            .map(|(f, values)| (f, values.into_iter().map(|v| (None, v)).collect()))
            .collect();
        Self::new(queues).0
    }
}
impl Device for Replay {
    fn call(&mut self, _ram: &mut [u8], _scratch: &mut [u8], call: HostCall) -> Option<u32> {
        let mut state = self.0.borrow_mut();
        let next = state.queues.get_mut(&call.function)?.pop_front();
        let Some((args, v0)) = next else {
            state.mismatches.push(format!(
                "{:08x}{:x?}: not in the original",
                call.function, call.args
            ));
            return None;
        };
        let n = compared_arguments(call.function);
        if let Some(args) = args
            && call.args[..n] != args[..n]
        {
            state.mismatches.push(format!(
                "{:08x}{:x?}: the original passed {:x?}",
                call.function,
                &call.args[..n],
                &args[..n]
            ));
        }
        Some(v0)
    }
}

/// Arguments compared per replayed routine: none for the critical-section
/// syscalls (their stubs load `a0` after the logged entry), all four for
/// OpenEvent, three for read, two for open, else the first.
fn compared_arguments(function: u32) -> usize {
    match function {
        0x800a_d7e8 | 0x800a_d7f8 => 0,
        0x800a_d758 => 4,
        0x800a_d818 => 3,
        0x800a_d808 => 2,
        _ => 1,
    }
}

/// Device routines replayed from the logs (host calls by these function
/// addresses, `bios.rs` and `sdk::test_event`): TestEvent, OpenEvent,
/// EnableEvent, DisableEvent, CloseEvent, Enter/ExitCriticalSection, open,
/// read, close, `_card_info`, `_card_load`, `_card_clear`.
const REPLAYED: [u32; 13] = [
    0x800a_d788,
    0x800a_d758,
    0x800a_d798,
    0x800a_d7a8,
    0x800a_d768,
    0x800a_d7e8,
    0x800a_d7f8,
    0x800a_d808,
    0x800a_d818,
    0x800a_d838,
    0x800a_b7e8,
    0x800a_b7f8,
    0x800a_b808,
];

/// Overlay-1 routines whose `v0` is part of their contract.
const RESULTS: &[u32] = &[
    0x800c_afc0,
    0x800c_b2bc,
    0x800c_b4c8,
    0x800c_b5b4,
    0x800c_b5e4,
    0x800c_b690,
    0x800c_b6ec,
    0x800c_b70c,
    0x800c_b738,
    0x800c_b7ac,
    0x800c_b7cc,
    0x800c_b828,
    0x800c_b884,
    0x800c_b8b0,
    0x800c_b8dc,
    0x800c_b94c,
    0x800c_b964,
    0x800c_ba04,
    0x800c_baa8,
    0x800c_bb20,
    0x800c_bb50,
    0x800c_bcf4,
    0x800c_c354,
    0x800c_cd50,
    0x800c_d918,
    0x800c_de9c,
    0x800c_e0fc,
    0x800c_e300,
    0x800c_e644,
    0x800c_eaac,
    0x800c_faf0,
    0x800d_0150,
    0x800d_019c,
    0x800d_0200,
    0x800d_05e8,
    0x800d_097c,
    0x800d_09b4,
    0x800d_0b74,
    0x800d_0bf4,
    0x800d_0c84,
    0x800d_0d04,
    0x800d_0d38,
    0x800d_0e78,
    0x800d_0fa4,
    0x800d_103c,
    0x800d_11b4,
    0x800d_13e4,
    0x800d_14e0,
    0x800d_1578,
    0x800d_1794,
    0x800d_18b0,
    0x800d_1900,
    0x800d_1b58,
    0x800d_1c1c,
    0x800d_1d80,
    0x800d_1eac,
    0x800d_1ef4,
    0x800d_1f68,
    0x800d_1ff0,
    0x800d_204c,
    0x800d_2098,
    0x800d_2108,
    0x800d_2178,
    0x800d_21fc,
    0x800d_2264,
    0x800d_22d8,
    0x800d_234c,
    0x800d_23c0,
    0x800d_2488,
    0x800d_251c,
    0x800d_25a0,
    0x800d_25c0,
    0x800d_26cc,
    0x800d_2b24,
    0x800d_2ba4,
    0x800d_2c3c,
    0x800d_2d88,
    0x800d_33c8,
    0x800d_570c,
    0x800d_5724,
];

#[derive(Debug)]
enum Outcome {
    /// Bytes (RAM outside the stack and scratchpad) the original changed.
    Exact(usize),
    Blocked(u32),
    Failed(String),
}

/// The BIOS kernel's RAM (see the module documentation).
const KERNEL: std::ops::Range<u32> = 0x8000_0000..0x8001_0000;

/// GTE data registers VZ0, VZ1, VZ2: their upper halfword is the stale
/// stack pad of an SVECTOR (`lwc2`) that the GTE never uses; in a composite
/// run (the rotated sprites of 800D33C8 inside a whole menu frame) it is
/// whatever the original's callers left on the stack, which native callers
/// keep in Rust (FLIGHT_MACHINE.md rule 5). The low halfword must match.
const VZ: [usize; 3] = [1, 3, 5];

/// Driver state written by interrupt handlers and queued GPU transfers
/// (from `tests/boot.rs`), excluded only where interrupts ran.
const INTERRUPTS: &[(u32, u32, &str)] = &[
    (0x800b_2504, 0x800b_258c, "libpad driver state"),
    (0x800b_25cc, 0x800b_26b0, "libspu driver state"),
    (0x800b_2ec4, 0x800b_2ecc, "libetc VSync state"),
    (
        0x800b_2f2c,
        0x800b_4034,
        "libetc interrupt and callback manager, VSync counters, libcd command state, DMA callbacks",
    ),
    (0x800b_97ec, 0x800b_97f0, "libgpu submission flag"),
    (
        0x800b_990c,
        0x800b_9928,
        "libgpu queue indices, saved interrupt masks and DMA timeout",
    ),
    (
        0x800b_f150,
        0x800b_f200,
        "CD monitor [gp+0x34] and stream [gp+0x38] state, written by the resident CdSync/CdReady callbacks of the menu music stream",
    ),
    (0x800c_59d0, 0x800c_5cbc, "libpad, libspu and libcd BSS"),
    (0x800c_8128, 0x800c_9a30, "libgpu command queue"),
];

/// Whether interrupts ran in the original's window: libetc's interrupt
/// and callback manager (0x800B2F2C..0x800B4034, which holds the VBlank
/// counter) or libgpu's queue indices changed.
fn interrupted(entry: &Snapshot, exit: &Snapshot) -> bool {
    (0xb_2f2c..0xb_4034)
        .chain(0xb_990c..0xb_9914)
        .any(|i| entry.ram[i] != exit.ram[i])
}

fn overlay_code(address: u32) -> bool {
    (0x800c_afc0..0x800d_5940).contains(&address)
}

/// Run `ov1::dispatch` on one entry snapshot and compare with its return.
fn run(
    directory: &Path,
    index: &[SnapshotInfo],
    logs: &[Log],
    entry: &SnapshotInfo,
) -> Option<Outcome> {
    let exit = index.iter().find(|s| {
        s.callee == entry.callee
            && s.stage == "return"
            && s.sequence > entry.sequence
            && s.ra == entry.ra
            && s.registers.gpr[29] == entry.registers.gpr[29]
    })?;
    let entry_snapshot = Snapshot::load(directory, entry).expect("entry images");
    let exit_snapshot = Snapshot::load(directory, exit).expect("return images");
    let mut m = entry_snapshot.machine();
    let mut queues: Queues = HashMap::new();
    let mut arguments: HashMap<u32, [u32; 4]> = HashMap::new();
    for log in logs.iter().filter(|l| {
        l.sequence > entry.sequence && l.sequence < exit.sequence && REPLAYED.contains(&l.callee)
    }) {
        if log.stage == "entry" {
            arguments.insert(log.callee, log.a);
        } else {
            let args = arguments.remove(&log.callee);
            queues
                .entry(log.callee)
                .or_default()
                .push_back((args, log.v0));
        }
    }
    let (replay, replayed) = Replay::new(queues);
    m.device = Some(Box::new(replay));
    let args = [
        entry_snapshot.gpr(4),
        entry_snapshot.gpr(5),
        entry_snapshot.gpr(6),
        entry_snapshot.gpr(7),
    ];
    let v0 = ov1::dispatch(&mut m, entry.callee, args).expect("claimed by ov1::dispatch");
    match m.take_fault() {
        Ok(()) => {}
        Err(Fault::Untranslated { address }) if !overlay_code(address) => {
            return Some(Outcome::Blocked(address));
        }
        Err(fault) => return Some(Outcome::Failed(fault.to_string())),
    }
    // The original's device calls, all of them, with their arguments.
    let problems = replayed.borrow().problems();
    if !problems.is_empty() {
        return Some(Outcome::Failed(problems.join("; ")));
    }
    let mut differences = snapshot::compare(&m, &exit_snapshot);
    let polls = sdk::GPU_TIMEOUT_POLLS;
    let interrupts = interrupted(&entry_snapshot, &exit_snapshot);
    let allowed = |a: u32| {
        (polls..polls + 4).contains(&a)
            || KERNEL.contains(&a)
            || (interrupts && INTERRUPTS.iter().any(|&(s, e, _)| (s..e).contains(&a)))
    };
    differences.ram.retain(|&a| !allowed(a));
    differences.gte_data.retain(|&r| {
        !(VZ.contains(&r)
            && m.gte.data[r] & 0xffff == exit_snapshot.info.registers.cp2d[r] & 0xffff)
    });
    if !differences.is_empty() {
        return Some(Outcome::Failed(differences.summary()));
    }
    if RESULTS.contains(&entry.callee) && v0 != exit_snapshot.gpr(2) {
        return Some(Outcome::Failed(format!(
            "v0 {v0:#010x}, original {:#010x}",
            exit_snapshot.gpr(2)
        )));
    }
    Some(Outcome::Exact(snapshot::changed_bytes(
        &entry_snapshot,
        &exit_snapshot,
    )))
}

/// Per-routine results of one capture.
#[derive(Default, Debug)]
struct Tally {
    exact: usize,
    changed: usize,
    blocked: BTreeMap<u32, usize>,
    failed: Vec<String>,
}

fn check_capture(name: &str) -> BTreeMap<u32, Tally> {
    let directory = capture(name);
    let index = snapshot::read_index(&directory.join("snapshots.jsonl")).expect("index");
    let logs = read_logs(&directory);
    let mut tallies: BTreeMap<u32, Tally> = BTreeMap::new();
    for entry in index.iter().filter(|s| s.stage == "entry") {
        let Some(outcome) = run(&directory, &index, &logs, entry) else {
            continue;
        };
        let tally = tallies.entry(entry.callee).or_default();
        match outcome {
            Outcome::Exact(changed) => {
                tally.exact += 1;
                tally.changed += changed;
            }
            Outcome::Blocked(address) => *tally.blocked.entry(address).or_default() += 1,
            Outcome::Failed(error) => tally.failed.push(format!(
                "{:08x} frame {} seq {}: {error}",
                entry.callee, entry.frame, entry.sequence
            )),
        }
    }
    for (callee, tally) in &tallies {
        let blocked: Vec<String> = tally
            .blocked
            .iter()
            .map(|(a, n)| format!("{a:08x} x{n}"))
            .collect();
        eprintln!(
            "{callee:08x}: exact {} ({} bytes changed){}{}",
            tally.exact,
            tally.changed,
            if blocked.is_empty() {
                String::new()
            } else {
                format!(", BLOCKED by {}", blocked.join(" "))
            },
            if tally.failed.is_empty() {
                String::new()
            } else {
                format!(", FAILED {}", tally.failed.len())
            }
        );
    }
    tallies
}

/// Require no failure and the listed routines exact on at least one
/// occurrence with an effect (bytes changed or a compared `v0`): the lists
/// are the routines exact today; occurrences BLOCKED by a routine owned
/// elsewhere join them when it lands.
fn require(tallies: &BTreeMap<u32, Tally>, exact: &[u32]) {
    let failures: Vec<&String> = tallies.values().flat_map(|t| &t.failed).collect();
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures
            .iter()
            .take(40)
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
    for callee in exact {
        let tally = tallies
            .get(callee)
            .unwrap_or_else(|| panic!("{callee:08x}: no snapshot"));
        assert!(tally.exact > 0, "{callee:08x}: never exact ({tally:?})");
        assert!(
            tally.changed > 0 || RESULTS.contains(callee),
            "{callee:08x}: nothing changed"
        );
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and ov1-010"]
fn cold_start_ov1_010() {
    let tallies = check_capture("ov1-010");
    require(
        &tallies,
        &[
            0x800c_afc0,
            0x800c_b3f8,
            0x800c_b4c8,
            0x800c_b648,
            0x800c_b6ec,
            0x800c_b70c,
            0x800c_b828,
            0x800c_b884,
            0x800c_b8b0,
            0x800c_b8dc,
            0x800c_b94c,
            0x800c_b964,
            0x800c_ba04,
            0x800c_ba5c,
            0x800c_ba6c,
            0x800c_baa8,
            0x800c_bb50,
            0x800c_bcf4,
            0x800c_bdac,
            0x800c_c354,
            0x800c_c8c8,
            0x800c_c9ac,
            0x800c_cbc8,
            0x800c_e5e4,
            0x800c_e644,
            0x800c_eaac,
            0x800d_0150,
            0x800d_04f4,
            0x800d_050c,
            0x800d_0548,
            0x800d_05e8,
            0x800d_0914,
            0x800d_097c,
            0x800d_0b74,
            0x800d_0bf4,
            0x800d_13e4,
            0x800d_1578,
            0x800d_1794,
            0x800d_1800,
            0x800d_18b0,
            0x800d_1900,
            0x800d_2d88,
            0x800d_363c,
            0x800d_3824,
            0x800d_4058,
            0x800d_47f4,
            0x800d_4808,
            0x800d_4844,
            0x800d_4858,
            0x800d_4878,
            0x800d_48ac,
            0x800d_4988,
            0x800d_49d0,
            0x800d_4a60,
            0x800d_4ab0,
            0x800d_4bbc,
            0x800d_51e0,
            0x800d_532c,
            0x800d_5458,
            0x800d_54c0,
            0x800d_54e8,
            0x800d_5608,
            0x800d_566c,
            0x800d_5724,
        ],
    );
}

#[test]
#[ignore = "requires AC3_DATA_DIR and ov1-011"]
fn title_main_difficulty_ov1_011() {
    let tallies = check_capture("ov1-011");
    require(
        &tallies,
        &[
            0x800c_afc0,
            0x800c_b4c8,
            0x800d_0280,
            0x800d_0290,
            0x800d_050c,
            0x800d_0548,
            0x800d_05e8,
            0x800d_1578,
            0x800d_1794,
            0x800d_1800,
            0x800d_18b0,
            0x800d_1900,
            0x800d_19e4,
            0x800d_1b58,
            0x800d_1c1c,
            0x800d_1d80,
            0x800d_1ef4,
            0x800d_1f68,
            0x800d_363c,
            0x800d_3824,
            0x800d_3ae0,
            0x800d_4a9c,
            0x800d_4ab0,
            0x800d_51e0,
            0x800d_532c,
            0x800d_54d8,
            0x800d_5724,
        ],
    );
}

#[test]
#[ignore = "requires AC3_DATA_DIR and ov1-012"]
fn load_screen_ov1_012() {
    let tallies = check_capture("ov1-012");
    require(
        &tallies,
        &[
            0x800c_b4c8,
            0x800c_baa8,
            0x800c_bb50,
            0x800c_bcf4,
            0x800c_bdac,
            0x800c_c354,
            0x800d_02b0,
            0x800d_1c1c,
            0x800d_1ff0,
            0x800d_204c,
            0x800d_2108,
            0x800d_21fc,
            0x800d_22d8,
            0x800d_234c,
        ],
    );
}

#[test]
#[ignore = "requires AC3_DATA_DIR and ov1-013"]
fn new_game_exit_ov1_013() {
    let tallies = check_capture("ov1-013");
    require(
        &tallies,
        &[
            0x800c_afc0,
            0x800c_b4c8,
            0x800c_b5b4,
            0x800c_b5e4,
            0x800d_02b0,
            0x800d_1d0c,
            0x800d_1d80,
            0x800d_1eac,
            0x800d_1f68,
        ],
    );
}

#[test]
#[ignore = "requires AC3_DATA_DIR and ov1-014"]
fn title_timeout_ov1_014() {
    let tallies = check_capture("ov1-014");
    require(&tallies, &[0x800d_1794, 0x800d_1900]);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and ov1-018"]
fn return_to_menus_ov1_018() {
    let tallies = check_capture("ov1-018");
    require(
        &tallies,
        &[
            0x800c_afc0,
            0x800c_b4c8,
            0x800c_b884,
            0x800c_b8b0,
            0x800c_b8dc,
            0x800c_b94c,
            0x800c_b964,
            0x800c_ba04,
            0x800c_ba6c,
            0x800d_0c84,
            0x800d_0d04,
            0x800d_18b0,
            0x800d_4608,
        ],
    );
}

#[test]
#[ignore = "requires AC3_DATA_DIR and ov1-019"]
fn saved_game_ov1_019() {
    let tallies = check_capture("ov1-019");
    require(
        &tallies,
        &[
            0x800c_afc0,
            0x800c_b4c8,
            0x800c_b5b4,
            0x800c_b5e4,
            0x800c_b648,
            0x800c_b738,
            0x800c_bb20,
            0x800c_bcf4,
            0x800c_bdac,
            0x800c_c354,
            0x800c_cd50,
            0x800c_d224,
            0x800c_de9c,
            0x800c_e0fc,
            0x800c_e300,
            0x800d_0150,
            0x800d_05e8,
            0x800d_0bf4,
            0x800d_1ff0,
            0x800d_204c,
            0x800d_2108,
            0x800d_21fc,
            0x800d_22d8,
            0x800d_23c0,
            0x800d_25c0,
            0x800d_2b24,
            0x800d_2c3c,
            0x800d_3ae0,
        ],
    );
}

/// One oracle vector (see [`check_vectors`]): a patched entry state, its
/// call and the original's result.
struct Vector {
    name: String,
    callee: u32,
    args: [u32; 4],
    /// The entry snapshot with the vector's `sp` and patches applied.
    base: Machine,
    /// Scripted device results by function, in call order.
    host: HashMap<u32, VecDeque<u32>>,
    v0: u32,
    /// The bytes the original changed (RAM below the stack, scratchpad).
    changes: BTreeMap<u32, u8>,
}

/// Calls `f` with every vector of `analysis/flight-runtime/<name>/
/// vectors.json`; returns their number.
fn each_vector(name: &str, mut f: impl FnMut(Vector)) -> usize {
    let text = fs::read_to_string(capture(name).join("vectors.json")).expect("vectors");
    let all: Value = serde_json::from_str(&text).expect("json");
    let vectors = all["vectors"].as_array().expect("vectors");
    let int = |v: &Value| v.as_u64().expect("number") as u32;
    let mut entries: HashMap<String, Machine> = HashMap::new();
    for vector in vectors {
        let source = vector["capture"].as_str().expect("capture");
        let stem = vector["stem"].as_str().expect("stem");
        let entry = entries
            .entry(format!("{source}/{stem}"))
            .or_insert_with(|| {
                let directory = capture(source);
                let index =
                    snapshot::read_index(&directory.join("snapshots.jsonl")).expect("index");
                let info = index.iter().find(|s| s.stem == stem).expect("entry");
                Snapshot::load(&directory, info).expect("images").machine()
            });
        let mut base = entry.clone();
        if let Some(sp) = vector.get("sp") {
            base.sp = int(sp);
        }
        for patch in vector["patch"].as_array().expect("patch") {
            let hex = patch[1].as_str().unwrap();
            let bytes: Vec<u8> = (0..hex.len() / 2)
                .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap())
                .collect();
            base.write_bytes(int(&patch[0]), &bytes);
        }
        let mut host: HashMap<u32, VecDeque<u32>> = HashMap::new();
        if let Some(scripted) = vector.get("host").and_then(Value::as_object) {
            for (function, values) in scripted {
                let function: u32 = function.parse().expect("host function");
                host.insert(
                    function,
                    values.as_array().unwrap().iter().map(int).collect(),
                );
            }
        }
        let args: Vec<u32> = vector["args"].as_array().unwrap().iter().map(int).collect();
        f(Vector {
            name: vector["name"].as_str().unwrap_or_default().to_string(),
            callee: int(&vector["callee"]),
            args: [args[0], args[1], args[2], args[3]],
            base,
            host,
            v0: int(&vector["v0"]),
            changes: vector["changes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| (int(&c[0]), int(&c[1]) as u8))
                .collect(),
        });
    }
    vectors.len()
}

/// Per-routine and per-blocker tallies of a vector run.
#[derive(Default)]
struct VectorTally {
    exact: usize,
    blocked: usize,
    changed: usize,
    routines: BTreeMap<u32, (usize, usize)>,
    blockers: BTreeMap<u32, usize>,
    failures: Vec<String>,
}

impl VectorTally {
    fn exact(&mut self, callee: u32, changed: usize) {
        self.exact += 1;
        self.changed += changed;
        self.routines.entry(callee).or_default().0 += 1;
    }

    /// Counts a BLOCKED native run; false for any other outcome.
    fn blocked(&mut self, callee: u32, fault: &Result<(), Fault>) -> bool {
        match fault {
            Err(Fault::Untranslated { address }) if !overlay_code(*address) => {
                self.blocked += 1;
                *self.blockers.entry(*address).or_default() += 1;
                self.routines.entry(callee).or_default().1 += 1;
                true
            }
            _ => false,
        }
    }

    fn finish(self, label: &str, count: usize) -> (usize, usize, usize) {
        for (callee, (exact, blocked)) in &self.routines {
            eprintln!("{callee:08x}: {exact} states exact, {blocked} BLOCKED");
        }
        let blockers: Vec<String> = self
            .blockers
            .iter()
            .map(|(a, n)| format!("{a:08x} x{n}"))
            .collect();
        eprintln!(
            "{label}: {} states exact ({} bytes changed), {} BLOCKED by {}",
            self.exact,
            self.changed,
            self.blocked,
            blockers.join(" ")
        );
        assert!(self.failures.is_empty(), "{}", self.failures.join("\n"));
        assert_eq!(self.exact + self.blocked, count);
        (self.exact, self.blocked, self.changed)
    }
}

/// Oracle vectors: synthetic commands and states written into captured
/// entry snapshots and run through the original instructions by
/// `.ov1/emu2.py` (an R3000 integer interpreter over the snapshot RAM,
/// starting from the entry snapshot's registers, used only as an oracle;
/// it reproduces the captured returns of the routines it was checked on).
/// Device and BIOS routines are scripted host calls (`host`: results in
/// call order, replayed by [`Replay`]). Each vector: the same patches and
/// call (with its `sp`), exactly the same changed bytes (RAM outside the
/// stack, scratchpad) and, for routines with a result, `v0`. A vector
/// reaching a routine owned elsewhere that is not translated yet is
/// BLOCKED. Returns (exact, blocked, changed bytes).
fn check_vectors(name: &str) -> (usize, usize, usize) {
    let mut tally = VectorTally::default();
    let count = each_vector(name, |vector| {
        let Vector {
            name,
            callee,
            args,
            base: mut m,
            host,
            v0: original,
            changes: expected,
        } = vector;
        m.device = Some(Box::new(Replay::results(host)));
        let base_ram = m.ram().to_vec();
        let base_scratch = m.scratch().to_vec();
        let v0 = ov1::dispatch(&mut m, callee, args).expect("claimed by ov1::dispatch");
        let fault = m.take_fault();
        if tally.blocked(callee, &fault) {
            return;
        }
        if let Err(fault) = fault {
            tally.failures.push(format!("{name}: {fault}"));
            return;
        }
        let mut actual: BTreeMap<u32, u8> = BTreeMap::new();
        for (i, (&now, &before)) in m.ram()[..snapshot::STACK_START]
            .iter()
            .zip(&base_ram)
            .enumerate()
        {
            if now != before {
                actual.insert(0x8000_0000 + i as u32, now);
            }
        }
        for (i, (&now, &before)) in m.scratch().iter().zip(&base_scratch).enumerate() {
            if now != before {
                actual.insert(0x1f80_0000 + i as u32, now);
            }
        }
        if actual != expected {
            let differing: Vec<String> = expected
                .keys()
                .chain(actual.keys())
                .filter(|a| expected.get(a) != actual.get(a))
                .take(12)
                .map(|a| format!("{a:08x}: {:?} vs {:?}", actual.get(a), expected.get(a)))
                .collect();
            tally
                .failures
                .push(format!("{name}: {}", differing.join(", ")));
            return;
        }
        if RESULTS.contains(&callee) && v0 != original {
            tally
                .failures
                .push(format!("{name}: v0 {v0:#x}, original {original:#x}"));
            return;
        }
        tally.exact(callee, expected.len());
    });
    tally.finish(name, count)
}

/// Interpreted ranges `[start, end)` (the next routine's address) of the
/// routines the vector sets call, for the shared oracle.
const ROUTINES: [(u32, u32); 41] = [
    (0x800c_afc0, 0x800c_b2bc),
    (0x800c_b3f0, 0x800c_b3f8),
    (0x800c_bdac, 0x800c_c354),
    (0x800c_c354, 0x800c_c46c),
    (0x800c_c62c, 0x800c_c63c),
    (0x800c_d224, 0x800c_d380),
    (0x800c_de9c, 0x800c_dfa4),
    (0x800c_dfa4, 0x800c_e0fc),
    (0x800c_e300, 0x800c_e354),
    (0x800c_faf0, 0x800d_0150),
    (0x800d_0150, 0x800d_019c),
    (0x800d_019c, 0x800d_0200),
    (0x800d_0200, 0x800d_0254),
    (0x800d_0578, 0x800d_0588),
    (0x800d_0588, 0x800d_05e8),
    (0x800d_0e78, 0x800d_0fa4),
    (0x800d_1c1c, 0x800d_1d0c),
    (0x800d_2098, 0x800d_2108),
    (0x800d_2178, 0x800d_21fc),
    (0x800d_2264, 0x800d_22d8),
    (0x800d_2488, 0x800d_250c),
    (0x800d_250c, 0x800d_251c),
    (0x800d_251c, 0x800d_25a0),
    (0x800d_2d88, 0x800d_33c8),
    (0x800d_33c8, 0x800d_3634),
    (0x800d_3634, 0x800d_363c),
    (0x800d_40cc, 0x800d_42b4),
    (0x800d_42b4, 0x800d_4444),
    (0x800d_4444, 0x800d_4608),
    (0x800d_4608, 0x800d_47f4),
    (0x800d_48dc, 0x800d_48f0),
    (0x800d_48f0, 0x800d_4920),
    (0x800d_4920, 0x800d_4940),
    (0x800d_4940, 0x800d_4988),
    (0x800d_499c, 0x800d_49d0),
    (0x800d_4a3c, 0x800d_4a60),
    (0x800d_4f9c, 0x800d_51e0),
    (0x800d_52f4, 0x800d_532c),
    (0x800d_532c, 0x800d_5458),
    (0x800d_54a8, 0x800d_54c0),
    (0x800d_570c, 0x800d_5724),
];

/// The vector states once more through the shared differential oracle
/// (`tests/support/oracle.rs`, FLIGHT_MACHINE.md "Validation"): the
/// original instructions of the routine under test interpreted over the
/// patched snapshot state, every routine it calls native
/// (`dispatch::call`) in both runs, the same scripted device results in
/// each, and the same RAM (outside the dead stack below `sp`), scratchpad,
/// GTE, `sp`, fault and, for routines with a result, `v0`. A state whose
/// native run reaches a routine owned elsewhere that is not translated
/// yet is BLOCKED. Returns (exact, blocked, bytes the original changed).
fn differential(name: &str) -> (usize, usize, usize) {
    let mut tally = VectorTally::default();
    let count = each_vector(name, |vector| {
        let Vector {
            name,
            callee,
            args,
            base,
            host,
            ..
        } = vector;
        let &(start, end) = ROUTINES
            .iter()
            .find(|r| r.0 == callee)
            .expect("interpreted range");
        let mut native = base.clone();
        native.device = Some(Box::new(Replay::results(host.clone())));
        let native_v0 = ov1::dispatch(&mut native, callee, args).expect("claimed");
        let native_fault = native.take_fault();
        if tally.blocked(callee, &native_fault) {
            return;
        }
        let mut original = base.clone();
        original.device = Some(Box::new(Replay::results(host)));
        let v0 = oracle::run(&mut original, start, end, args);
        let fault = original.take_fault();
        let stack = snapshot::STACK_START..(base.sp & 0x1f_ffff) as usize;
        let ram: Vec<u32> = (0..0x20_0000usize)
            .filter(|&i| !stack.contains(&i) && original.ram()[i] != native.ram()[i])
            .map(|i| 0x8000_0000 + i as u32)
            .collect();
        let scratch = (0..0x400)
            .filter(|&i| original.scratch()[i] != native.scratch()[i])
            .count();
        if fault != native_fault
            || !ram.is_empty()
            || scratch != 0
            || original.gte != native.gte
            || original.sp != native.sp
            || (RESULTS.contains(&callee) && v0 != native_v0)
        {
            tally.failures.push(format!(
                "{name}: faults {fault:?}/{native_fault:?} ram {:08x?} scratch {scratch} \
                 gte equal {} sp {:x}/{:x} v0 {v0:#x}/{native_v0:#x}",
                &ram[..ram.len().min(12)],
                original.gte == native.gte,
                original.sp,
                native.sp
            ));
            return;
        }
        let changed = (0..0x20_0000usize)
            .filter(|&i| !stack.contains(&i) && base.ram()[i] != original.ram()[i])
            .count();
        tally.exact(callee, changed);
    });
    tally.finish(name, count)
}

/// Paths no capture reaches: the unused script commands 0, 10, 14, 17, 18,
/// 21..25, 27, the clipping cases of command 23, the animation gates and
/// the load-screen waits for a card (`ov1-022`).
#[test]
#[ignore = "requires AC3_DATA_DIR and ov1-022"]
fn oracle_vectors_ov1_022() {
    let (exact, _, changed) = check_vectors("ov1-022");
    assert!(exact >= 54 && changed > 0);
}

/// Branch coverage of partly captured routines (`ov1-023`): the small
/// unreferenced helpers, 160 random animation records (800D2D88), the
/// axis-aligned quadrilateral (800D33C8), the colour pulse over frame
/// counters, the glyph renderer in every font with the spacing triggers,
/// line feed and carriage return, the language rows, every card-status
/// branch (800CC354) and probe step (800CBDAC), corrupted records and saves
/// (800CDE9C, 800CDFA4), every save slot (800CE300, 800CD224), and the
/// menu dispatcher's routes through the main-menu, difficulty and title
/// results and every menu state's initialization.
#[test]
#[ignore = "requires AC3_DATA_DIR and ov1-023"]
fn oracle_vectors_ov1_023() {
    let (exact, _, changed) = check_vectors("ov1-023");
    assert!(exact > 300 && changed > 0);
}

/// The unreferenced string-bank helpers 800D019C and 800D0200 (and the
/// live 800D0150) on synthetic bank sets for every language, and the
/// unreferenced card flag clear 800CC62C (`ov1-024`).
#[test]
#[ignore = "requires AC3_DATA_DIR and ov1-024"]
fn oracle_vectors_ov1_024() {
    let (exact, blocked, changed) = check_vectors("ov1-024");
    assert!(exact == 86 && blocked == 0 && changed > 0);
}

/// The states of `ov1-022`..`ov1-024` through the shared oracle
/// ([`differential`]).
#[test]
#[ignore = "requires AC3_DATA_DIR and ov1-022..ov1-024"]
fn shared_oracle_ov1_022_to_024() {
    for name in ["ov1-022", "ov1-023", "ov1-024"] {
        let (exact, _, changed) = differential(name);
        assert!(exact > 0 && changed > 0, "{name}");
    }
}

// ---- unit tests --------------------------------------------------------------------

/// 64-bit FNV-1a over little-endian words.
fn fnv(values: impl Iterator<Item = u32>) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for value in values {
        for byte in value.to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    hash
}

/// 800CEAAC on every font 0..24 and byte pair, against the FNV-1a hash of
/// the original instructions' results (`.ov1/emu.py`, a small R3000
/// interpreter run on the overlay image as an oracle: 1,638,400 calls,
/// 23,550 x -1, 196 x -2, 3,559 x 1, 2 x 2).
#[test]
fn kerning_matches_original_on_every_input() {
    let values = (0..25u32).flat_map(|font| {
        (0..256u32).flat_map(move |left| {
            (0..256u32).map(move |right| ov1_text::kerning(font, left, right))
        })
    });
    assert_eq!(fnv(values), 0xb4db_cc25_2acc_c7ac);
    assert_eq!(
        ov1_text::kerning(5, u32::from(b'r'), u32::from(b'e')),
        -2i32 as u32
    );
    assert_eq!(ov1_text::kerning(16, u32::from(b'W'), u32::from(b'i')), 2);
    assert_eq!(ov1_text::kerning(23, u32::from(b'r'), u32::from(b'e')), 0);
    assert_eq!(ov1_text::kerning(5, 0x100, 0), 0, "left outside the tables");
}

fn machine_with_module() -> Machine {
    let mut m = Machine::default();
    m.sw(ac3_flight::GP + 0x2dc, 0x800b_d158);
    m.sw(0x800b_d158 + 0x38, 1);
    m
}

/// The dispatcher answers only while module 1 is loaded.
#[test]
fn dispatch_requires_module_1() {
    let mut m = machine_with_module();
    assert_eq!(ov1::dispatch(&mut m, 0x800d_1d78, [0; 4]), Some(1));
    m.sw(0x800b_d158 + 0x38, 2);
    assert_eq!(ov1::dispatch(&mut m, 0x800d_1d78, [0; 4]), None);
    m.sw(0x800b_d158 + 0x38, 0xffff_ffff);
    assert_eq!(ov1::dispatch(&mut m, 0x800c_b4c8, [0; 4]), None);
}

/// 800D2D88: every animation type on a synthetic record, including the
/// `break 7` of a zero-length key interval.
#[test]
fn animation_types() {
    let mut m = machine_with_module();
    let out = 0x8010_0000;
    let anim = 0x8010_0010;
    let set = |m: &mut Machine, rect: [i16; 4], fields: [i16; 8]| {
        for (i, v) in rect.iter().enumerate() {
            m.sh(out + 2 * i as u32, *v as u16);
        }
        for (i, v) in fields.iter().enumerate() {
            m.sh(anim + 2 * i as u32, *v as u16);
        }
    };
    let rect = |m: &mut Machine| -> [i16; 4] { std::array::from_fn(|i| m.lh(out + 2 * i as u32)) };
    // Type 0 and out-of-range types draw unchanged.
    set(&mut m, [10, 20, 30, 40], [0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(ov1_script::animate(&mut m, out, anim), 1);
    set(&mut m, [10, 20, 30, 40], [0, -1, 0, 0, 0, 0, 0, 0]);
    assert_eq!(ov1_script::animate(&mut m, out, anim), 1);
    assert_eq!(rect(&mut m), [10, 20, 30, 40]);
    // Type 1 before its first key: hidden.
    set(&mut m, [100, 50, 64, 32], [0, 1, 0, 0, 10, 20, 30, 5]);
    assert_eq!(ov1_script::animate(&mut m, out, anim), 0);
    // Type 1 grows from the origin: x = 0 + (100-0)*(15-10)/(20-10) = 50,
    // y moves one step toward the origin side (sign of 50 - 0 = +1 from 0),
    // width scaled to 32, height 2.
    set(&mut m, [100, 50, 64, 32], [0, 1, 0, 0, 10, 20, 30, 15]);
    assert_eq!(ov1_script::animate(&mut m, out, anim), 1);
    assert_eq!(rect(&mut m), [50, 1, 32, 2]);
    // Type 1 second phase: y and height scale over (20, 30].
    set(&mut m, [100, 50, 64, 32], [0, 1, 0, 0, 10, 20, 30, 25]);
    assert_eq!(ov1_script::animate(&mut m, out, anim), 1);
    assert_eq!(rect(&mut m), [100, 25, 64, 16]);
    // Type 1 done.
    set(&mut m, [100, 50, 64, 32], [0, 1, 0, 0, 10, 20, 30, 30]);
    assert_eq!(ov1_script::animate(&mut m, out, anim), 1);
    assert_eq!(rect(&mut m), [100, 50, 64, 32]);
    // Type 2 first phase: x steps by the sign, y and height scale, w = 1.
    set(&mut m, [100, 50, 64, 32], [0, 2, 10, 0, 10, 20, 30, 15]);
    assert_eq!(ov1_script::animate(&mut m, out, anim), 1);
    assert_eq!(rect(&mut m), [11, 25, 1, 16]);
    // Type 3 zooms all four.
    set(&mut m, [100, 50, 64, 32], [0, 3, 0, 0, 10, 20, 30, 15]);
    assert_eq!(ov1_script::animate(&mut m, out, anim), 1);
    assert_eq!(rect(&mut m), [50, 25, 16, 16]);
    // Type 4: shown from the last key on.
    set(&mut m, [1, 2, 3, 4], [0, 4, 0, 0, 0, 0, 30, 29]);
    assert_eq!(ov1_script::animate(&mut m, out, anim), 0);
    set(&mut m, [1, 2, 3, 4], [0, 4, 0, 0, 0, 0, 30, 30]);
    assert_eq!(ov1_script::animate(&mut m, out, anim), 1);
    assert!(m.take_fault().is_ok());
    // A zero-length interval reaches the compiler's divide-by-zero break.
    set(&mut m, [100, 50, 64, 32], [0, 1, 0, 0, 10, 10, 30, 15]);
    ov1_script::animate(&mut m, out, anim);
    assert!(m.take_fault().is_ok(), "second phase divides by t2 - t1");
    set(&mut m, [100, 50, 64, 32], [0, 3, 0, 0, 10, 20, 10, 15]);
    ov1_script::animate(&mut m, out, anim);
    assert_eq!(
        m.take_fault(),
        Err(Fault::Trap {
            address: 0x800d_3328,
            code: 7
        })
    );
}
