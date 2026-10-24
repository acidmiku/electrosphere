//! The other inner states reached from the hub menu: the save screen of
//! state 9 with its memory-card layer (`ov2_hub::save`, `ov2_hub::card`),
//! the aircraft unlock screen of state 12 (`ov2_hub::unlock`) and the
//! "CONNECTION LOST" reload of state 13 (`ov2_hub::lost`).
//!
//! Captures (PCSX-Redux, `.ov2hub/ov2-snapshots.lua` with the save-screen
//! window key and the BIOS log; checkpoint `menu-runtime/mission-002/
//! final.sav`, hub choice 2 at frame 21, "Save game?" confirmed at 160):
//! - `ov2hub-008`: the profile's new formatted card: "File not found on
//!   this MEMORY CARD. Save to new file?" yes, slot 1, "Save to file 1?"
//!   yes, the write, "saved" and back to the hub.
//! - `ov2hub-009`: the card ov2hub-008 wrote (its `profile/memcard1.mcd`):
//!   the load of the existing file, the other-session question (yes), slot
//!   1 overwritten after the record merge, then the hub and state 9 again.
//! - `ov2hub-010`: that card with one progress byte changed: the
//!   other-session question answered "no", "Save game?" cancelled.
//! - `ov2hub-011`: an all-zero (unformatted) card: the format question,
//!   the format, then the slot question answered "no" twice.
//! - `ov2hub-012`: the card of ov2hub-008 with its file renamed to another
//!   product code (directory checksum updated): the card listing finds one
//!   foreign file (firstfile fills its DIRENTRY), then "Save to new file?"
//!   (only the listing and the file questions bracketed).
//!
//! Each capture's `bios.jsonl` lists every BIOS / libcard stub call (and
//! TestEvent) the overlay made, with the snapshot sequence before it, its
//! argument registers, `v0` and the DIRENTRY bytes firstfile / nextfile
//! filled. The test device replays them in order for the calls inside the
//! bracketed routine, checking the argument registers each stub reads, and
//! every original call must be made. Comparison: all RAM outside the stack,
//! scratchpad, GTE and `v0` (not for the routines without a result), except
//! the RAM the BIOS and the device own: the interrupt context (kernel RAM,
//! libetc's interrupt manager and stack, the pad / SPU / CD driver state:
//! VSync and card interrupts run during any routine) and, when the stub was
//! called, libc's firstfile statics (0x800C9D60..0x800C9D80) and libcard's
//! format buffers (0x800C9A70..0x800C9D30).
//!
//! Occurrences that reach a shared routine still untranslated in this
//! workspace (libc sprintf 800ACD28, strlen 800AC328 inside 800D04C8,
//! strcat 800AC1B8, strcpy 800AC2D8) cannot match the return snapshot; they
//! are compared instead with the instruction oracle (`support::oracle::run`)
//! from the same entry state, both runs replaying the original stub calls:
//! both take the same callee results, and their host calls must agree.
//! They become full snapshot comparisons once libc is translated.
//!
//! Paths no capture reaches (no card, a full card, failed transfers, bad
//! data, the unlock screen after a mission, the CONNECTION LOST reload) are
//! compared with the oracle on variants of a captured save-screen state.
use crate::support::oracle::{self, Rng};
use ac3_flight::{
    Device, Fault, HostCall, Machine, ov2_hub,
    snapshot::{self, Snapshot},
};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::rc::Rc;

pub const SAVE_CAPTURES: [&str; 5] = [
    "ov2hub-008",
    "ov2hub-009",
    "ov2hub-010",
    "ov2hub-011",
    "ov2hub-012",
];

/// The BIOS / libcard stubs the overlay calls (host calls) and TestEvent.
const STUBS: [u32; 16] = [
    0x800a_b7e8,
    0x800a_b7f8,
    0x800a_b808,
    0x800a_bc38,
    0x800a_d758,
    0x800a_d768,
    0x800a_d788,
    0x800a_d798,
    0x800a_d7e8,
    0x800a_d7f8,
    0x800a_d808,
    0x800a_d818,
    0x800a_d828,
    0x800a_d838,
    0x800a_d848,
    0x800a_da08,
];

/// Shared libc routines still untranslated: sprintf, strlen, strcat,
/// strcpy.
const LIBC: [u32; 4] = [0x800a_cd28, 0x800a_c328, 0x800a_c1b8, 0x800a_c2d8];

/// Routines whose `v0` no caller reads: those without a result, and
/// 800CCAC8 / 800D6634, whose `v0` is the one their last callee leaves
/// (`ov2core`'s 800CC9AC and 800D0EE4, which the `ov2core` dispatch
/// returns as 0).
const VOID: [u32; 12] = [
    0x800c_bfac,
    0x800c_cac8,
    0x800d_6634,
    0x800c_cbac,
    0x800c_cdc0,
    0x800c_cdc8,
    0x800c_d580,
    0x800c_d98c,
    0x800c_dd50,
    0x800c_ddf0,
    0x800c_e1a4,
    0x800c_e5c8,
];

/// The original instructions of every routine of the four parts: (first,
/// end), from `.ov2hub/reach.py`.
const EXTENTS: [(u32, u32); 112] = [
    (0x800c_bca8, 0x800c_bd20),
    (0x800c_bd20, 0x800c_bd50),
    (0x800c_bd50, 0x800c_be30),
    (0x800c_be30, 0x800c_be9c),
    (0x800c_be9c, 0x800c_bef4),
    (0x800c_bef4, 0x800c_bfac),
    (0x800c_bfac, 0x800c_c554),
    (0x800c_c554, 0x800c_c66c),
    (0x800c_c66c, 0x800c_c774),
    (0x800c_c774, 0x800c_c82c),
    (0x800c_c8f4, 0x800c_c9ac),
    (0x800c_cac8, 0x800c_cbac),
    (0x800c_cbac, 0x800c_cdc0),
    (0x800c_cdc0, 0x800c_cdc8),
    (0x800c_cdc8, 0x800c_ceb8),
    (0x800c_ceb8, 0x800c_cf50),
    (0x800c_cf50, 0x800c_cf68),
    (0x800c_d580, 0x800c_d98c),
    (0x800c_d98c, 0x800c_db18),
    (0x800c_db18, 0x800c_db68),
    (0x800c_db68, 0x800c_dca8),
    (0x800c_dca8, 0x800c_dd50),
    (0x800c_dd50, 0x800c_ddf0),
    (0x800c_ddf0, 0x800c_ded4),
    (0x800c_ded4, 0x800c_e09c),
    (0x800c_e09c, 0x800c_e1a4),
    (0x800c_e1a4, 0x800c_e2fc),
    (0x800c_e2fc, 0x800c_e500),
    (0x800c_e500, 0x800c_e554),
    (0x800c_e554, 0x800c_e5c8),
    (0x800c_e5c8, 0x800c_e7e4),
    (0x800d_0efc, 0x800d_0f20),
    (0x800d_0f50, 0x800d_0f60),
    (0x800d_17ac, 0x800d_17d0),
    (0x800d_1828, 0x800d_186c),
    (0x800d_3730, 0x800d_3794),
    (0x800d_3794, 0x800d_3908),
    (0x800d_4c8c, 0x800d_4d08),
    (0x800d_4d08, 0x800d_4de8),
    (0x800d_4de8, 0x800d_4e48),
    (0x800d_4e48, 0x800d_4ed4),
    (0x800d_4ed4, 0x800d_4f5c),
    (0x800d_4f5c, 0x800d_50e8),
    (0x800d_50e8, 0x800d_5170),
    (0x800d_5170, 0x800d_5210),
    (0x800d_5210, 0x800d_53a0),
    (0x800d_53a0, 0x800d_5460),
    (0x800d_5460, 0x800d_560c),
    (0x800d_560c, 0x800d_56c4),
    (0x800d_56c4, 0x800d_5880),
    (0x800d_5880, 0x800d_5918),
    (0x800d_5918, 0x800d_594c),
    (0x800d_594c, 0x800d_59c0),
    (0x800d_59c0, 0x800d_5a30),
    (0x800d_5a30, 0x800d_5ad0),
    (0x800d_5ad0, 0x800d_5c7c),
    (0x800d_5c7c, 0x800d_5d50),
    (0x800d_5d50, 0x800d_5f10),
    (0x800d_5f10, 0x800d_5f80),
    (0x800d_5f80, 0x800d_6034),
    (0x800d_6034, 0x800d_60a8),
    (0x800d_60a8, 0x800d_6178),
    (0x800d_6178, 0x800d_6304),
    (0x800d_6304, 0x800d_63c4),
    (0x800d_63c4, 0x800d_6560),
    (0x800d_6560, 0x800d_6634),
    (0x800d_6634, 0x800d_6890),
    (0x800d_6890, 0x800d_6988),
    (0x800d_6988, 0x800d_6ad0),
    (0x800d_6ad0, 0x800d_6be8),
    (0x800d_6be8, 0x800d_6c50),
    (0x800d_6c50, 0x800d_6fb4),
    (0x800d_6fb4, 0x800d_7018),
    (0x800d_7018, 0x800d_70ac),
    (0x800d_70ac, 0x800d_7118),
    (0x800d_7118, 0x800d_718c),
    (0x800d_718c, 0x800d_71e0),
    (0x800d_71e0, 0x800d_7230),
    (0x800d_7230, 0x800d_725c),
    (0x800d_725c, 0x800d_72a0),
    (0x800d_72a0, 0x800d_730c),
    (0x800d_730c, 0x800d_734c),
    (0x800d_734c, 0x800d_7384),
    (0x800d_7384, 0x800d_73b8),
    (0x800d_73b8, 0x800d_7400),
    (0x800d_7400, 0x800d_7468),
    (0x800d_7468, 0x800d_74c0),
    (0x800d_74c0, 0x800d_75a4),
    (0x800d_75a4, 0x800d_7640),
    (0x800d_7640, 0x800d_77e4),
    (0x800d_b980, 0x800d_ba04),
    (0x800d_ba04, 0x800d_bc54),
    (0x800d_bc54, 0x800d_bd1c),
    (0x800d_bd1c, 0x800d_be2c),
    (0x800d_be2c, 0x800d_bf2c),
    (0x800d_bf2c, 0x800d_bff8),
    (0x800d_bff8, 0x800d_c074),
    (0x800d_c138, 0x800d_c1d4),
    (0x800d_c1d4, 0x800d_c22c),
    (0x800d_c22c, 0x800d_c254),
    (0x800d_c254, 0x800d_c288),
    (0x800d_c288, 0x800d_c2d4),
    (0x800d_c2d4, 0x800d_c470),
    (0x800d_c470, 0x800d_c5bc),
    (0x800d_c5bc, 0x800d_c638),
    (0x800d_c638, 0x800d_c7b0),
    (0x800d_c7b0, 0x800d_c868),
    (0x800d_c868, 0x800d_c8f4),
    (0x800e_a008, 0x800e_a020),
    (0x800e_a020, 0x800e_a038),
    (0x800f_17c0, 0x800f_18ec),
    (0x800f_1954, 0x800f_196c),
];

pub fn extent(callee: u32) -> (u32, u32) {
    *EXTENTS
        .iter()
        .find(|(first, _)| *first == callee)
        .unwrap_or_else(|| panic!("{callee:08x}: no extent"))
}

fn index(capture: &str) -> PathBuf {
    oracle::capture(capture)
}

/// One original stub call (`bios.jsonl`).
#[derive(Clone, Debug)]
struct StubCall {
    after: u64,
    function: u32,
    args: [u32; 4],
    v0: u32,
    bytes: Vec<u8>,
}

fn stub_calls(directory: &Path) -> Vec<StubCall> {
    let Ok(text) = std::fs::read_to_string(directory.join("bios.jsonl")) else {
        return Vec::new();
    };
    text.lines()
        .map(|line| {
            let v: serde_json::Value = serde_json::from_str(line).expect("bios.jsonl line");
            let int = |x: &serde_json::Value| x.as_u64().expect("number") as u32;
            let hex = v["bytes"].as_str().unwrap_or_default();
            StubCall {
                after: v["after"].as_u64().expect("after"),
                function: int(&v["function"]),
                args: std::array::from_fn(|i| int(&v["args"][i])),
                v0: int(&v["v0"]),
                bytes: (0..hex.len() / 2)
                    .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap())
                    .collect(),
            }
        })
        .collect()
}

/// Argument registers each stub reads.
fn arity(function: u32) -> usize {
    match function {
        0x800a_d7e8 | 0x800a_d7f8 => 0,
        0x800a_d758 => 4,
        0x800a_d808 | 0x800a_da08 => 2,
        0x800a_d818 | 0x800a_d828 => 3,
        _ => 1,
    }
}

#[derive(Default)]
struct ReplayLog {
    calls: VecDeque<StubCall>,
    errors: Vec<String>,
}

/// The test memory-card device: replays the original stub calls.
struct Replay(Rc<RefCell<ReplayLog>>);

impl Device for Replay {
    fn call(&mut self, ram: &mut [u8], _scratch: &mut [u8], call: HostCall) -> Option<u32> {
        if !STUBS.contains(&call.function) {
            return None;
        }
        let mut log = self.0.borrow_mut();
        let Some(next) = log.calls.pop_front() else {
            log.errors
                .push(format!("extra call {:08x} {:x?}", call.function, call.args));
            return None;
        };
        let n = arity(next.function);
        if next.function != call.function || next.args[..n] != call.args[..n] {
            log.errors.push(format!(
                "call {:08x} {:x?}, original {:08x} {:x?}",
                call.function,
                call.args,
                next.function,
                &next.args[..n]
            ));
        }
        if !next.bytes.is_empty() {
            let at = (next.v0 & 0x1f_ffff) as usize;
            ram[at..at + next.bytes.len()].copy_from_slice(&next.bytes);
        }
        Some(next.v0)
    }
}

/// RAM written in interrupt context, which runs during any routine (as
/// `tests/boot.rs` and `tests/cdstream.rs` exclude it): BIOS kernel RAM,
/// libpad's driver state, libetc's interrupt and callback manager with the
/// interrupt stack, the libpad / libspu / libcd BSS and the pad receive
/// buffers.
const INTERRUPT_CONTEXT: [(u32, u32); 5] = [
    (0x8000_0000, 0x8001_0000),
    (0x800b_2504, 0x800b_258c),
    (0x800b_2f2c, 0x800b_4034),
    (0x800c_59d0, 0x800c_5cbc),
    (0x800b_e1e0, 0x800b_e224),
];

/// RAM the BIOS and the memory-card device own: the interrupt context and
/// the libc / libcard statics of the stubs called.
fn device_owned(calls: &[StubCall], address: u32) -> bool {
    let called = |f: u32| calls.iter().any(|c| c.function == f);
    INTERRUPT_CONTEXT
        .iter()
        .any(|(start, end)| (*start..*end).contains(&address))
        || (called(0x800a_da08) && (0x800c_9d60..0x800c_9d80).contains(&address))
        || (called(0x800a_bc38) && (0x800c_9a70..0x800c_9d30).contains(&address))
}

#[derive(Debug)]
pub enum Outcome {
    /// Full comparison passed; bytes the original changed.
    Exact(usize),
    /// An untranslated libc routine was reached; the oracle comparison from
    /// the entry state passed (bytes the original instructions changed).
    Oracle(u32, usize),
}

fn native_result(m: &mut Machine, callee: u32, args: [u32; 4]) -> Option<u32> {
    let v0 = ov2_hub::dispatch(m, callee, args);
    if VOID.contains(&callee) { None } else { v0 }
}

/// Run `callee` on its `occurrence`th entry snapshot in `capture` with the
/// replay device and compare with the matching return.
pub fn run_case(capture: &str, callee: u32, occurrence: usize) -> Result<Outcome, String> {
    let index = index(capture);
    let directory = index.parent().unwrap();
    let infos = snapshot::read_index(&index).map_err(|e| e.to_string())?;
    let entry = infos
        .iter()
        .filter(|s| s.callee == callee && s.stage == "entry")
        .nth(occurrence)
        .ok_or("no entry")?;
    let exit = infos
        .iter()
        .find(|s| {
            s.callee == callee
                && s.stage == "return"
                && s.sequence > entry.sequence
                && s.ra == entry.ra
                && s.registers.gpr[29] == entry.registers.gpr[29]
        })
        .ok_or("no return")?;
    let calls: Vec<StubCall> = stub_calls(directory)
        .into_iter()
        .filter(|c| c.after >= entry.sequence && c.after < exit.sequence)
        .collect();
    let entry = Snapshot::load(directory, entry).map_err(|e| e.to_string())?;
    let exit = Snapshot::load(directory, exit).map_err(|e| e.to_string())?;
    let args = [entry.gpr(4), entry.gpr(5), entry.gpr(6), entry.gpr(7)];
    let tag = format!("{callee:08x} {capture} #{occurrence}");
    let log = Rc::new(RefCell::new(ReplayLog {
        calls: calls.iter().cloned().collect(),
        errors: Vec::new(),
    }));
    let mut m = entry.machine();
    m.device = Some(Box::new(Replay(log.clone())));
    let v0 = ov2_hub::dispatch(&mut m, callee, args).ok_or("not claimed")?;
    match m.take_fault() {
        Ok(()) => {
            let log = log.borrow();
            if !log.errors.is_empty() {
                return Err(format!("{tag}: {}", log.errors.join("; ")));
            }
            if let Some(next) = log.calls.front() {
                return Err(format!(
                    "{tag}: {} original stub calls not made, next {:08x} {:x?}",
                    log.calls.len(),
                    next.function,
                    next.args
                ));
            }
            let differences = snapshot::compare(&m, &exit);
            let ram: Vec<u32> = differences
                .ram
                .iter()
                .copied()
                .filter(|a| !device_owned(&calls, *a))
                .collect();
            if !ram.is_empty()
                || !differences.scratch.is_empty()
                || !differences.gte_data.is_empty()
                || !differences.gte_control.is_empty()
            {
                return Err(format!(
                    "{tag}: ram {} {:08x?} {}",
                    ram.len(),
                    &ram[..ram.len().min(16)],
                    differences.summary()
                ));
            }
            if !VOID.contains(&callee) && v0 != exit.gpr(2) {
                return Err(format!("{tag}: v0 {v0:#x}, original {:#x}", exit.gpr(2)));
            }
            Ok(Outcome::Exact(snapshot::changed_bytes(&entry, &exit)))
        }
        Err(Fault::Untranslated { address }) if LIBC.contains(&address) => {
            let changed = replayed_oracle(&tag, &entry.machine(), &calls, callee, args)?;
            Ok(Outcome::Oracle(address, changed))
        }
        Err(fault) => Err(format!("{tag}: {fault}")),
    }
}

/// `base` with a replay device over `calls`.
fn replaying(base: &Machine, calls: &[StubCall]) -> Machine {
    let mut m = base.clone();
    m.device = Some(Box::new(Replay(Rc::new(RefCell::new(ReplayLog {
        calls: calls.iter().cloned().collect(),
        errors: Vec::new(),
    })))));
    m
}

/// The instruction oracle and the translation of `callee` from `base`,
/// each with its own replay of the original stub calls (results and
/// DIRENTRY fills): the same faults, host calls (functions and argument
/// registers, in order), RAM outside the stack below the entry `sp`,
/// scratchpad, GTE, `sp` and `v0`. Returns the bytes the original
/// instructions changed.
fn replayed_oracle(
    tag: &str,
    base: &Machine,
    calls: &[StubCall],
    callee: u32,
    args: [u32; 4],
) -> Result<usize, String> {
    let (start, end) = extent(callee);
    let mut original = replaying(base, calls);
    let v0 = oracle::run(&mut original, start, end, args);
    let mut native = replaying(base, calls);
    let native_v0 = native_result(&mut native, callee, args);
    let (fo, fnat) = (original.take_fault(), native.take_fault());
    if fo != fnat {
        return Err(format!("{tag}: faults {fo:?} / {fnat:?}"));
    }
    if original.host_calls != native.host_calls {
        return Err(format!(
            "{tag}: host calls {:x?} / {:x?}",
            original.host_calls, native.host_calls
        ));
    }
    let dead = snapshot::STACK_START..(base.sp & 0x1f_ffff) as usize;
    let ram: Vec<u32> = (0..0x20_0000usize)
        .filter(|i| !dead.contains(i) && original.ram()[*i] != native.ram()[*i])
        .map(|i| 0x8000_0000 + i as u32)
        .collect();
    if !ram.is_empty()
        || original.scratch() != native.scratch()
        || original.gte != native.gte
        || original.sp != native.sp
    {
        return Err(format!(
            "{tag}: oracle ram {:08x?} scratch equal {} gte equal {} sp {:x}/{:x}",
            &ram[..ram.len().min(16)],
            original.scratch() == native.scratch(),
            original.gte == native.gte,
            original.sp,
            native.sp
        ));
    }
    if let Some(n) = native_v0
        && n != v0
    {
        return Err(format!("{tag}: oracle v0 {v0:#x}, translation {n:#x}"));
    }
    Ok((0..0x20_0000usize)
        .filter(|i| !dead.contains(i) && base.ram()[*i] != original.ram()[*i])
        .count())
}

#[derive(Debug, Default)]
pub struct Tally {
    pub exact: usize,
    pub oracle: usize,
    pub changed: usize,
}

/// Every captured occurrence of `callee` in the save captures; none may
/// fail. Returns the tally (possibly empty for routines no capture
/// reaches).
pub fn check(callee: u32) -> Tally {
    let (tally, errors) = survey(callee);
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    tally
}

/// As [`check`], collecting the failures.
pub fn survey(callee: u32) -> (Tally, Vec<String>) {
    let mut tally = Tally::default();
    let mut errors = Vec::new();
    for capture in SAVE_CAPTURES {
        let count = snapshot::read_index(&index(capture))
            .unwrap()
            .iter()
            .filter(|s| s.callee == callee && s.stage == "entry")
            .count();
        for occurrence in 0..count {
            match run_case(capture, callee, occurrence) {
                Ok(Outcome::Exact(changed)) => {
                    eprintln!("{callee:08x} {capture} #{occurrence}: exact, {changed} changed");
                    tally.exact += 1;
                    tally.changed += changed;
                }
                Ok(Outcome::Oracle(address, changed)) => {
                    eprintln!(
                        "{callee:08x} {capture} #{occurrence}: {address:08x} untranslated, \
                         oracle equal, {changed} changed"
                    );
                    tally.oracle += 1;
                    tally.changed += changed;
                }
                Err(error) => errors.push(error),
            }
        }
    }
    eprintln!("{callee:08x}: {tally:?}, {} failed", errors.len());
    (tally, errors)
}

/// Captured routines: at least one occurrence.
pub fn captured(callee: u32) {
    let tally = check(callee);
    assert!(
        tally.exact + tally.oracle > 0,
        "{callee:08x}: no captured occurrence"
    );
}

// ---- oracle variants ----------------------------------------------------------------------------

/// Guest addresses of the state blocks.
const CONTEXT: u32 = 0x800b_acd0;
const SAVE: u32 = 0x800f_5eb4;
const CARD: u32 = 0x800f_1e44;
const UNLOCK: u32 = 0x800f_6048;
const LOST: u32 = 0x800f_5e8c;

/// The first save-screen tick of `ov2hub-008` (module 2 loaded, the save
/// screen and card layer set up).
pub fn base() -> Machine {
    let index = index("ov2hub-008");
    let infos = snapshot::read_index(&index).unwrap();
    let info = infos
        .iter()
        .find(|s| s.callee == 0x800d_7640 && s.stage == "entry")
        .expect("tick entry");
    Snapshot::load(index.parent().unwrap(), info)
        .unwrap()
        .machine()
}

fn pick(rng: &mut Rng, values: &[u32]) -> u32 {
    values[rng.next() as usize % values.len()]
}

fn bits(rng: &mut Rng, values: &[u32]) -> u32 {
    values
        .iter()
        .filter(|_| rng.next() & 1 != 0)
        .fold(0, |a, b| a | b)
}

fn fill(m: &mut Machine, rng: &mut Rng, at: u32, len: u32, modulo: u32) {
    for i in 0..len {
        m.sb(at + i, (rng.next() % modulo) as u8);
    }
}

/// Queue `n` stub / host results drawn from `values`.
fn results(m: &mut Machine, rng: &mut Rng, n: usize, values: &[u32]) {
    m.host_results = (0..n).map(|_| pick(rng, values)).collect();
}

/// Randomize the save screen state, the pressed buttons and the card
/// monitor bytes.
fn save_variant(m: &mut Machine, rng: &mut Rng) {
    let s = m.lw(SAVE);
    let ctx = m.lw(CONTEXT);
    let c = m.lw(CARD);
    m.sw(s, bits(rng, &[1, 2, 4, 0x10, 0x20]));
    m.sw(s + 4, rng.next() & 1);
    m.sw(s + 0xc, rng.next() & 0x3f);
    m.sw(s + 0x10, rng.next() & 0x3f);
    m.sw(s + 0x14, rng.next() & 1);
    m.sw(s + 0x18, rng.next() % 7);
    for at in [0x1c, 0x20, 0x24] {
        m.sw(
            s + at,
            pick(rng, &[0, 1, 9, 10, 11, 24, 25, 26, 49, 50, 51, 100]),
        );
    }
    m.sw(s + 0x28, rng.next() & 1);
    m.sw(
        ctx + 0x12c,
        pick(rng, &[0, 0, 1, 2, 4, 8, 0x10, 0x20, 0x30, 0x11, 0x12]),
    );
    m.sw(ctx + 0x970, bits(rng, &[8, 0x10, 0x40]));
    for port in 0..2 {
        m.sb(c + 0x14c8 + port, (rng.next() % 3) as u8);
        m.sb(c + 0x14ca + port, (rng.next() % 3) as u8);
        m.sb(c + 0x14cc + port, (rng.next() % 9) as u8);
        m.sb(c + 0x14ce + port, (rng.next() % 5) as u8);
        m.sb(c + 0x14d0 + port, (rng.next() % 5) as u8);
        m.sh(c + 0x1220 + 2 * port, (rng.next() % 5) as u16);
        m.sh(c + 0x1224 + 2 * port, (rng.next() % 5) as u16);
        m.sw(c + 0x1230 + 4 * port, rng.next() % 17);
        m.sw(c + 0x1234 + 4 * port, rng.next() % 16);
        m.sb(c + 0x14d8 + port, (rng.next() % 4) as u8);
    }
    m.sw(c + 0x14d4, rng.next() & 1);
    m.sw(c + 0x14c4, pick(rng, &[0, 1, 2, 0x12c, u32::MAX]));
    m.sw(c + 0x14e0, rng.next() % 5);
    m.sw(c + 0x122c, pick(rng, &[0, 2, u32::MAX]));
    m.sw(c + 0x121c, rng.next() & 1);
    // Record flags (+4 bit 0 used) of the six slots.
    for k in 0..6 {
        let record = c + 0x5a4 + k * 0x12c;
        let flags = m.lw(record + 4);
        m.sw(record + 4, (flags & !1) | (rng.next() & 1));
    }
    results(
        m,
        rng,
        64,
        &[0, 1, 1, 2, u32::MAX, 0x800f_6f54, 0xf100_0001],
    );
}

/// Every routine of `routines` on `rounds` variants per routine; returns
/// the number of variants whose original run changed RAM.
fn variants(
    label: &str,
    routines: &[u32],
    rounds: u32,
    seed: u32,
    vary: impl Fn(&mut Machine, &mut Rng, u32) -> [u32; 4],
) -> usize {
    let base = base();
    let mut rng = Rng(seed);
    let mut changed = 0;
    for &routine in routines {
        let mut routine_changed = 0;
        for round in 0..rounds {
            let mut m = base.clone();
            let args = vary(&mut m, &mut rng, routine);
            let tag = format!("{label} {routine:08x} #{round}");
            let bytes = oracle::diff(&tag, &m, extent(routine), args, |m| {
                native_result(m, routine, args)
            });
            if bytes > 0 {
                routine_changed += 1;
            }
        }
        eprintln!("{label} {routine:08x}: {rounds} variants, {routine_changed} changing RAM");
        changed += routine_changed;
    }
    changed
}

/// The state-9 phase callbacks and helpers.
pub const SAVE_ROUTINES: [u32; 58] = [
    0x800d_0efc,
    0x800d_0f50,
    0x800e_a008,
    0x800e_a020,
    0x800f_1954,
    0x800d_4c8c,
    0x800d_4d08,
    0x800d_4de8,
    0x800d_4e48,
    0x800d_4ed4,
    0x800d_4f5c,
    0x800d_50e8,
    0x800d_5170,
    0x800d_5210,
    0x800d_53a0,
    0x800d_5460,
    0x800d_560c,
    0x800d_56c4,
    0x800d_5880,
    0x800d_5918,
    0x800d_594c,
    0x800d_59c0,
    0x800d_5a30,
    0x800d_5ad0,
    0x800d_5c7c,
    0x800d_5d50,
    0x800d_5f10,
    0x800d_5f80,
    0x800d_6034,
    0x800d_60a8,
    0x800d_6178,
    0x800d_6304,
    0x800d_63c4,
    0x800d_6560,
    0x800d_6634,
    0x800d_6890,
    0x800d_6988,
    0x800d_6ad0,
    0x800d_6be8,
    0x800d_6c50,
    0x800d_6fb4,
    0x800d_7018,
    0x800d_70ac,
    0x800d_7118,
    0x800d_718c,
    0x800d_71e0,
    0x800d_7230,
    0x800d_725c,
    0x800d_72a0,
    0x800d_730c,
    0x800d_734c,
    0x800d_7384,
    0x800d_73b8,
    0x800d_7400,
    0x800d_7468,
    0x800d_74c0,
    0x800d_75a4,
    0x800d_7640,
];

pub fn save_variants() -> usize {
    variants(
        "save",
        &SAVE_ROUTINES,
        24,
        0x5a7e_0009,
        |m, rng, routine| {
            save_variant(m, rng);
            if routine == 0x800d_7640 {
                // Keep the phase table index in range.
                let s = m.lw(SAVE);
                m.sh(s + 8, (rng.next() % 20) as u16);
                m.sh(s + 0xa, 0);
            }
            [rng.next() % 7, rng.next() % 4, rng.next() & 0xff, 0]
        },
    )
}

/// The card layer (31 routines).
pub const CARD_ROUTINES: [u32; 31] = [
    0x800c_bca8,
    0x800c_bd20,
    0x800c_bd50,
    0x800c_be30,
    0x800c_be9c,
    0x800c_bef4,
    0x800c_bfac,
    0x800c_c554,
    0x800c_c66c,
    0x800c_c774,
    0x800c_c8f4,
    0x800c_cac8,
    0x800c_cbac,
    0x800c_cdc0,
    0x800c_cdc8,
    0x800c_ceb8,
    0x800c_cf50,
    0x800c_d580,
    0x800c_d98c,
    0x800c_db18,
    0x800c_db68,
    0x800c_dca8,
    0x800c_dd50,
    0x800c_ddf0,
    0x800c_ded4,
    0x800c_e09c,
    0x800c_e1a4,
    0x800c_e2fc,
    0x800c_e500,
    0x800c_e554,
    0x800c_e5c8,
];

pub fn card_variants() -> usize {
    variants(
        "card",
        &CARD_ROUTINES,
        32,
        0xca7d_0001,
        |m, rng, routine| {
            save_variant(m, rng);
            let c = m.lw(CARD);
            let ctx = m.lw(CONTEXT);
            match routine {
                // Checksums, record checks and the merge: random image bytes
                // with the stored checksums kept or broken.
                0x800c_e09c | 0x800c_e1a4 | 0x800c_e5c8 | 0x800c_dd50 | 0x800c_ddf0 => {
                    fill(m, rng, c + 0x180, 0x40, 4);
                    fill(m, rng, c + 0x5a4, 0x40, 16);
                    fill(m, rng, c + 0xcac, 0x40, 4);
                    fill(m, rng, c + 0x10d0, 0x40, 16);
                    m.sh(c + 0x190, (rng.next() % 7) as u16);
                    fill(m, rng, ctx + 0x960 + 0x300, 0x100, 5);
                }
                0x800c_d580 | 0x800c_d98c => {
                    fill(m, rng, ctx + 0x960, 0x100, 3);
                    fill(m, rng, ctx + 0x960 + 0x300, 0x100, 5);
                }
                _ => {}
            }
            [
                pick(rng, &[0, 1, 0x10]),
                pick(rng, &[0, 1, 2, 3]),
                rng.next() % 37,
                0,
            ]
        },
    )
}

/// The unlock screen (19 routines).
pub const UNLOCK_ROUTINES: [u32; 19] = [
    0x800d_17ac,
    0x800d_b980,
    0x800d_ba04,
    0x800d_bc54,
    0x800d_bd1c,
    0x800d_be2c,
    0x800d_bf2c,
    0x800d_bff8,
    0x800d_c138,
    0x800d_c1d4,
    0x800d_c22c,
    0x800d_c254,
    0x800d_c288,
    0x800d_c2d4,
    0x800d_c470,
    0x800d_c5bc,
    0x800d_c638,
    0x800d_c7b0,
    0x800d_c868,
];

/// Random progress (ranks mostly 1 to reach the special unlocks), unlock
/// state and options.
fn unlock_variant(m: &mut Machine, rng: &mut Rng) {
    let ctx = m.lw(CONTEXT);
    let p = ctx + 0x960;
    // A third of the variants rank (nearly) every mission 1, half of those
    // exactly every one (the all-first unlocks and flag +0xA3).
    let all_first = rng.next().is_multiple_of(3);
    let every = all_first && rng.next() & 1 == 0;
    for pass in [0x300, 0x33c, 0x378, 0x3f0] {
        for n in 0..0x26 {
            let rank = if every || (all_first && !rng.next().is_multiple_of(8)) {
                1
            } else {
                rng.next() % 5
            };
            m.sb(p + pass + n, rank as u8);
        }
    }
    fill(m, rng, p + 0x18, 0x18, 2);
    fill(m, rng, p + 0x58, 0x18, 2);
    for flag in [0x2b, 0x2c, 0x2d, 0xa3, 0xa5] {
        m.sb(p + flag, u8::from(rng.next().is_multiple_of(3)));
    }
    m.sw(ctx + 0x970, bits(rng, &[8, 0x10, 0x18]));
    m.sw(ctx + 0x12c, pick(rng, &[0, 4, 8, 0x10, 0x20, 0x0c]));
    m.sh(ctx + 0x390, (rng.next() % 41) as u16);
    let block = m.lw(0x800f_69d0);
    m.sw(block + 0xc, rng.next() % 41);
    let s = m.lw(UNLOCK);
    m.sw(s, rng.next() % 3);
    m.sw(s + 4, bits(rng, &[1, 2, 4]));
    m.sw(s + 8, pick(rng, &[0, 5, 9, 10, 11]));
    m.sw(s + 0xc, pick(rng, &[0, 1, 12, 24, 25, 26]));
    let count = rng.next() % 9;
    m.sh(s + 0x10, count as u16);
    m.sh(s + 0x12, (rng.next() % (count + 1)) as u16);
    for i in 0..8 {
        m.sb(s + 0x14 + i, (rng.next() % 21) as u8);
    }
}

pub fn unlock_variants() -> usize {
    variants(
        "unlock",
        &UNLOCK_ROUTINES,
        32,
        0x0c12_0012,
        |m, rng, routine| {
            unlock_variant(m, rng);
            if routine == 0x800d_bc54 && rng.next() & 1 != 0 {
                return [0x24, 0, 0, 0];
            }
            let a0 = match routine {
                0x800d_c22c | 0x800d_c254 => pick(rng, &[0x12, 0x11, 3]),
                _ => rng.next() % 0x26,
            };
            // a2 is the stale offset 800DC2D4 passes on to 800F18EC.
            [a0, rng.next() % 0x26, pick(rng, &[0, 0xff00_0000]), 0]
        },
    )
}

/// State 13 (4 routines).
pub const LOST_ROUTINES: [u32; 4] = [0x800d_1828, 0x800d_3730, 0x800d_3794, 0x800f_17c0];

pub fn lost_variants() -> usize {
    variants(
        "lost",
        &LOST_ROUTINES,
        32,
        0x1057_0013,
        |m, rng, routine| {
            let ctx = m.lw(CONTEXT);
            let s = m.lw(LOST);
            m.sw(ctx + 0x20, rng.next());
            m.sh(ctx + 0x390, (rng.next() % 41) as u16);
            // Loader callbacks with every result kind: 800CED30 (1), 800CED5C
            // (2), 800CF2F4 (0 or 2 by its fade), 800CF350 (4).
            let (phase, substate) = [(0, 0), (0, 1), (3, 4), (4, 0)][rng.next() as usize % 4];
            m.sh(s, phase);
            m.sh(s + 2, substate);
            match routine {
                0x800f_17c0 => {
                    let from = 0x800f_9e00;
                    let to = 0x800f_9e04;
                    fill(m, rng, from, 8, 256);
                    [rng.next() % 0x80, from, to, 0]
                }
                _ => [rng.next() % 0x28, 0, 0, 0],
            }
        },
    )
}
