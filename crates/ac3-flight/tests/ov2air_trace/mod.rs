//! Shared trace harness of the ov2air tests (`tests/ov2_aircraft.rs`,
//! `tests/ov2_misc.rs`): chronological call traces recorded by
//! `.ov2air/air-trace.lua` (captures `analysis/flight-runtime/ov2air-NNN`:
//! a full RAM/scratchpad image at the first event, then per event the
//! RAM/scratchpad words changed since the previous event, with the CPU
//! registers and, when they change, the GTE registers).
//!
//! Every occurrence of a routine is run from the original state at its
//! entry and must reproduce the original state at its return: all RAM
//! outside the stack (`0x801FC000..`) and the interrupt-owned ranges (see
//! [`INTERRUPT_OWNED`]), the scratchpad, every GTE register and `v0` (for
//! routines that return one), like `snapshot::check_call`. Calls to
//! routines ov2air does not own are spliced (see [`spliced`]): at the call
//! the machine must equal the original state at that callee's entry (same
//! comparison, plus `$sp`, the argument registers and stack argument words
//! the callee reads, and strings and buffers it reads through pointer
//! arguments), then the callee's original writes, GTE registers and `v0`
//! are applied. The original's calls must all be matched, in order.
//! ov2air's routines run natively; the few translated resident routines in
//! [`NATIVE`] run through `dispatch::call`.
#![allow(dead_code)]
mod args;
pub mod oracle;

use ac3_flight::{
    Machine, dispatch,
    machine::{RAM_BYTES, SCRATCH_BYTES},
    ov2_aircraft::{self as air, Calls},
    ov2_misc,
};
use serde_json::Value;
use std::{collections::BTreeMap, path::PathBuf};

/// RAM excluded from comparisons: the guest stack.
pub const STACK: usize = 0x1f_c000;
/// RAM only interrupt handlers write while these routines run, excluded
/// from comparisons: an interrupt between two trace events (VBlank, the CD
/// interrupt of the background stream) leaves the BIOS's saved registers
/// and the SDK handlers' state there. The SDK ranges are the ones
/// `tests/boot.rs` shows hold no game state; each was observed in these
/// traces. No translated routine here writes any of them. At each spliced
/// call the test copies the original's bytes there and counts how often
/// they differed (`interrupts`).
pub const INTERRUPT_OWNED: [(usize, usize, &str); 5] = [
    (
        0x0_0000,
        0x1_0000,
        "kernel RAM: thread control block register save (0x8000E1xx), handler state (0x8000859C..)",
    ),
    (
        0xb_2f2c,
        0xb_4034,
        "libetc interrupt and callback manager (incl. its setjmp buffer), VSync counters, DMA callbacks",
    ),
    (0xb_4038, 0xb_4100, "libcd state (callbacks, command bytes)"),
    (0xb_43b8, 0xb_4440, "libcd completion status and read state"),
    (0xc_5c98, 0xc_5ca0, "libcd last result bytes"),
];

/// The compared RAM byte ranges: everything below the stack outside
/// [`INTERRUPT_OWNED`].
fn compared() -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut at = 0;
    for (start, end, _) in INTERRUPT_OWNED {
        if start > at {
            out.push((at, start));
        }
        at = end;
    }
    out.push((at, STACK));
    out
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Entry,
    Return,
}

pub struct Event {
    kind: Kind,
    f: u32,
    depth: u64,
    frame: u64,
    ram: (usize, usize),
    scratch: (usize, usize),
    gpr: [u32; 32],
    gte: Option<([u32; 32], [u32; 32])>,
}

pub struct Trace {
    name: String,
    ram: Vec<u8>,
    scratch: Vec<u8>,
    bin: Vec<u8>,
    events: Vec<Event>,
}

fn words(v: &Value) -> [u32; 32] {
    let list = v.as_array().expect("register list");
    std::array::from_fn(|i| list[i].as_u64().expect("register") as u32)
}

fn pair(v: &Value) -> (usize, usize) {
    (
        v[0].as_u64().expect("offset") as usize,
        v[1].as_u64().expect("count") as usize,
    )
}

impl Trace {
    pub fn load(capture: &str) -> Trace {
        let directory = PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR"))
            .join("analysis/flight-runtime")
            .join(capture);
        let read = |name: &str| std::fs::read(directory.join(name)).expect(name);
        let text = String::from_utf8(read("air-trace.jsonl")).expect("utf-8");
        let bin = read("air-trace.bin");
        // A capture whose game crashed ends inside its last event: keep the
        // complete events before it (and only those whose words were written).
        let lines: Vec<&str> = text.lines().collect();
        let parsed: Vec<Value> = lines
            .iter()
            .enumerate()
            .map_while(|(i, line)| match serde_json::from_str::<Value>(line) {
                Ok(v) => Some(v),
                Err(error) => {
                    assert_eq!(i + 1, lines.len(), "event {i}: {error}");
                    None
                }
            })
            .take_while(|v| {
                let end = |p: &Value| (pair(p).0 + pair(p).1 * 8) <= bin.len();
                end(&v["ram"]) && end(&v["scratch"])
            })
            .collect();
        let events = parsed
            .iter()
            .map(|v| {
                let kind = match v["kind"].as_str() {
                    Some("open" | "entry") => Kind::Entry,
                    Some("close" | "return") => Kind::Return,
                    other => panic!("event kind {other:?}"),
                };
                Event {
                    kind,
                    f: v["f"].as_u64().expect("f") as u32,
                    depth: v["depth"].as_u64().expect("depth"),
                    frame: v["frame"].as_u64().expect("frame"),
                    ram: pair(&v["ram"]),
                    scratch: pair(&v["scratch"]),
                    gpr: words(&v["gpr"]),
                    gte: v.get("cp2d").map(|data| (words(data), words(&v["cp2c"]))),
                }
            })
            .collect();
        let trace = Trace {
            name: capture.to_owned(),
            ram: read("air-trace-base.ram.bin"),
            scratch: read("air-trace-base.scratch.bin"),
            bin,
            events,
        };
        assert_eq!(trace.ram.len(), RAM_BYTES);
        assert_eq!(trace.scratch.len(), SCRATCH_BYTES);
        trace
    }

    /// The (byte offset, word) pairs event `index` changed in RAM and the
    /// scratchpad.
    fn changes(&self, index: usize) -> impl Iterator<Item = (bool, usize, u32)> + '_ {
        let event = &self.events[index];
        let pairs = move |(at, count): (usize, usize), scratch: bool| {
            (0..count).map(move |i| {
                let p = at + i * 8;
                let word = |o: usize| u32::from_le_bytes(self.bin[o..o + 4].try_into().unwrap());
                (scratch, word(p) as usize, word(p + 4))
            })
        };
        pairs(event.ram, false).chain(pairs(event.scratch, true))
    }

    /// The return event matching the entry `index`, if the trace has it
    /// (a capture can end inside a call).
    fn try_matching_return(&self, index: usize) -> Option<usize> {
        let entry = &self.events[index];
        (index + 1..self.events.len()).find(|&i| {
            let e = &self.events[i];
            e.kind == Kind::Return && e.f == entry.f && e.depth == entry.depth
        })
    }

    /// The return event matching the entry `index`.
    fn matching_return(&self, index: usize) -> usize {
        let entry = &self.events[index];
        (index + 1..self.events.len())
            .find(|&i| {
                let e = &self.events[i];
                e.kind == Kind::Return && e.f == entry.f && e.depth == entry.depth
            })
            .unwrap_or_else(|| panic!("{}: no return for event {index}", self.name))
    }
}

/// Original machine state after a prefix of the trace's events.
#[derive(Clone)]
pub struct Replay<'a> {
    trace: &'a Trace,
    ram: Vec<u8>,
    scratch: Vec<u8>,
    data: [u32; 32],
    control: [u32; 32],
    /// Events applied so far (the state is the one at event `next - 1`).
    next: usize,
}

impl<'a> Replay<'a> {
    fn new(trace: &'a Trace) -> Self {
        Replay {
            trace,
            ram: trace.ram.clone(),
            scratch: trace.scratch.clone(),
            data: [0; 32],
            control: [0; 32],
            next: 0,
        }
    }

    /// Apply events up to and including `index`; `machine` (if any) takes
    /// the same writes.
    fn advance(&mut self, index: usize, mut machine: Option<&mut Machine>) {
        while self.next <= index {
            for (scratch, offset, word) in self.trace.changes(self.next) {
                let bytes = word.to_le_bytes();
                if scratch {
                    self.scratch[offset..offset + 4].copy_from_slice(&bytes);
                } else {
                    self.ram[offset..offset + 4].copy_from_slice(&bytes);
                }
                if let Some(m) = machine.as_deref_mut() {
                    let base = if scratch { 0x1f80_0000 } else { 0x8000_0000 };
                    m.sw(base + offset as u32, word);
                }
            }
            if let Some((data, control)) = self.trace.events[self.next].gte {
                self.data = data;
                self.control = control;
            }
            self.next += 1;
        }
    }

    fn machine(&self, sp: u32) -> Machine {
        let mut m = Machine::from_images(&self.ram, &self.scratch).expect("image sizes");
        m.gte.data = self.data;
        m.gte.control = self.control;
        m.sp = sp;
        m
    }

    /// Differences between `m` and this state outside the stack.
    fn differences(&self, m: &Machine) -> Option<String> {
        let ranges = compared();
        if ranges.iter().all(|&(a, b)| m.ram()[a..b] == self.ram[a..b])
            && m.scratch() == &self.scratch[..]
            && m.gte.data == self.data
            && m.gte.control == self.control
        {
            return None;
        }
        let ram: Vec<usize> = ranges
            .iter()
            .flat_map(|&(a, b)| a..b)
            .filter(|&i| m.ram()[i] != self.ram[i])
            .collect();
        let scratch: Vec<usize> = (0..SCRATCH_BYTES)
            .filter(|&i| m.scratch()[i] != self.scratch[i])
            .collect();
        let data: Vec<usize> = (0..32).filter(|&i| m.gte.data[i] != self.data[i]).collect();
        let control: Vec<usize> = (0..32)
            .filter(|&i| m.gte.control[i] != self.control[i])
            .collect();
        if ram.is_empty() && scratch.is_empty() && data.is_empty() && control.is_empty() {
            return None;
        }
        let list = |v: &[usize], base: u32, image: &[u8], ours: &[u8]| {
            v.iter()
                .take(24)
                .map(|&a| format!("{:08x}:{:02x}/{:02x}", base + a as u32, ours[a], image[a]))
                .collect::<Vec<_>>()
                .join(" ")
        };
        Some(format!(
            "ram {} [{}] scratch {} [{}] gte data {data:?} control {control:?} (ours/original)",
            ram.len(),
            list(&ram, 0x8000_0000, &self.ram, m.ram()),
            scratch.len(),
            list(&scratch, 0x1f80_0000, &self.scratch, m.scratch()),
        ))
    }

    fn byte(&self, address: u32) -> u8 {
        self.ram[(address & 0x1f_ffff) as usize]
    }

    fn word(&self, address: u32) -> u32 {
        let o = (address & 0x1f_ffff) as usize;
        u32::from_le_bytes(self.ram[o..o + 4].try_into().unwrap())
    }
}

/// 800ACD28 (`sprintf`) reads its third and fourth argument registers and
/// its stack words only for the conversions of its format.
pub const SPRINTF: u32 = 0x800a_cd28;
/// Resident routines that run natively (translated, no device or interrupt
/// work): sound requests, memset, SquareRoot0, ratan2, and the arena
/// (80026F70 alloc, 80027098 reset, which ov2air calls through
/// `ac3_flight::arena` directly).
pub const NATIVE: [u32; 7] = [
    0x8002_d724,
    0x8002_db18,
    0x800a_c148,
    0x800a_47c8,
    0x800a_6088,
    0x8002_6f70,
    0x8002_7098,
];
/// Routines that read NUL-terminated strings through pointer arguments
/// (argument indexes): text 800D04C8, strlen, strcat, strcpy, BIOS
/// open/firstfile.
pub const STRINGS: [(u32, &[usize]); 6] = [
    (0x800d_04c8, &[1]),
    (0x800a_c328, &[0]),
    (0x800a_c1b8, &[0, 1]),
    (0x800a_c2d8, &[1]),
    (0x800a_d808, &[0]),
    (0x800a_da08, &[0]),
];
/// Routines that read fixed-size buffers through pointer arguments:
/// (address, argument index, bytes).
pub const BUFFERS: [(u32, usize, u32); 2] = [(0x800f_17c0, 1, 4), (0x800f_17c0, 2, 4)];

/// Whether a call out of ov2air's routines is spliced, and then the
/// argument registers (mask a0..a3) and stack words the callee reads
/// (`args::ARGS`, from `.ov2air/argscan.py`; unknown routines: all four
/// registers). Everything except ov2air's own routines and [`NATIVE`] is
/// spliced: overlay-2 routines other agents own, and resident routines that
/// are untranslated or do device and interrupt work (TIM upload,
/// LoadImage/StoreImage, DrawSync, ULZ expansion, BIOS file calls,
/// sprintf, ...).
pub fn spliced(target: u32) -> Option<(u8, u32)> {
    if NATIVE.contains(&target) {
        return None;
    }
    Some(
        args::ARGS
            .iter()
            .find(|e| e.0 == target)
            .map_or((0b1111, 0), |e| (e.1, u32::from(e.2))),
    )
}

/// Whether the trace's calls of `f` are spliced (not ov2air's, not native).
fn is_spliced(f: u32) -> bool {
    !OWN.with(|own| own.borrow().contains(&f)) && spliced(f).is_some()
}

thread_local! {
    /// ov2air's routines (set by [`check_capture`]).
    static OWN: std::cell::RefCell<Vec<u32>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Every routine of ov2air's modules.
pub fn all_own() -> Vec<u32> {
    let mut all = air::ROUTINES.to_vec();
    all.extend(ov2_misc::ROUTINES);
    all
}

/// Run one of ov2air's routines natively (any of its modules).
pub fn own(m: &mut Machine, c: &mut dyn Calls, target: u32, args: [u32; 4]) -> Option<u32> {
    air::routines::dispatch(m, c, target, args)
        .or_else(|| ov2_misc::routines::dispatch(m, c, target, args))
}

/// A format string's conversion count (`%%` excluded).
fn conversions(m: &mut Machine, mut at: u32) -> u32 {
    let mut count = 0;
    loop {
        let byte = m.lbu(at);
        if byte == 0 {
            return count;
        }
        if byte == b'%' {
            if m.lbu(at + 1) == b'%' {
                at += 1;
            } else {
                count += 1;
            }
        }
        at += 1;
    }
}

/// The [`Calls`] of a traced run: splices [`spliced`] routines from the
/// trace, runs this module's routines natively and everything else through
/// `dispatch::call`.
struct Splicer<'a> {
    replay: Replay<'a>,
    /// Return event of the routine under test.
    end: usize,
    calls: usize,
    /// Spliced calls whose entry found interrupt writes in kernel RAM.
    interrupts: usize,
    error: Option<String>,
}

impl Splicer<'_> {
    fn splice(&mut self, m: &mut Machine, target: u32, args: [u32; 4], used: (u8, u32)) -> u32 {
        let trace = self.replay.trace;
        let start = self.replay.next;
        let Some(entry) = (start..self.end).find(|&i| {
            let e = &trace.events[i];
            e.kind == Kind::Entry && is_spliced(e.f)
        }) else {
            self.error = Some(format!(
                "{target:08x}: called, but the original makes no further call"
            ));
            return 0;
        };
        let event = &trace.events[entry];
        if event.f != target {
            self.error = Some(format!(
                "{target:08x} called where the original calls {:08x} (event {entry})",
                event.f
            ));
            return 0;
        }
        self.replay.advance(entry, None);
        let mut interrupted = false;
        for (start, end, _) in INTERRUPT_OWNED {
            if m.ram()[start..end] != self.replay.ram[start..end] {
                interrupted = true;
                for i in (start..end).step_by(4) {
                    let word = u32::from_le_bytes(self.replay.ram[i..i + 4].try_into().unwrap());
                    if m.lw(0x8000_0000 + i as u32) != word {
                        m.sw(0x8000_0000 + i as u32, word);
                    }
                }
            }
        }
        self.interrupts += usize::from(interrupted);
        if let Some(difference) = self.replay.differences(m) {
            self.error = Some(format!("{target:08x} entry (event {entry}): {difference}"));
            return 0;
        }
        let (mut mask, mut stack) = used;
        if target == SPRINTF {
            let n = conversions(m, args[1]);
            mask = match n {
                0 => 0b0011,
                1 => 0b0111,
                _ => 0b1111,
            };
            stack = n.saturating_sub(2);
        }
        let differs = (0..4).any(|i| mask & (1 << i) != 0 && args[i] != event.gpr[4 + i]);
        if m.sp != event.gpr[29] || differs {
            self.error = Some(format!(
                "{target:08x} entry (event {entry}): sp {:08x} args {args:08x?} (mask {mask:04b}), original sp {:08x} args {:08x?}",
                m.sp,
                event.gpr[29],
                &event.gpr[4..8]
            ));
            return 0;
        }
        for k in 0..stack {
            let at = m.sp + 0x10 + 4 * k;
            if m.lw(at) != self.replay.word(at) {
                self.error = Some(format!("{target:08x}: stack argument {k} differs"));
                return 0;
            }
        }
        for (address, strings) in STRINGS {
            if address != target {
                continue;
            }
            for &i in strings {
                let mut at = args[i];
                loop {
                    let byte = m.lbu(at);
                    if byte != self.replay.byte(at) {
                        self.error = Some(format!("{target:08x}: string byte at {at:08x} differs"));
                        return 0;
                    }
                    if byte == 0 {
                        break;
                    }
                    at += 1;
                }
            }
        }
        for (address, i, bytes) in BUFFERS {
            if address != target {
                continue;
            }
            for k in 0..bytes {
                let at = args[i] + k;
                if m.lbu(at) != self.replay.byte(at) {
                    self.error = Some(format!("{target:08x}: buffer byte at {at:08x} differs"));
                    return 0;
                }
            }
        }
        let exit = trace.matching_return(entry);
        self.replay.advance(exit, Some(m));
        m.gte.data = self.replay.data;
        m.gte.control = self.replay.control;
        self.calls += 1;
        trace.events[exit].gpr[2]
    }
}

impl Calls for Splicer<'_> {
    fn call(&mut self, m: &mut Machine, target: u32, args: [u32; 4]) -> u32 {
        if self.error.is_some() {
            return 0;
        }
        if let Some(v0) = own(m, self, target, args) {
            return v0;
        }
        if let Some(used) = spliced(target) {
            return self.splice(m, target, args, used);
        }
        dispatch::call(m, target, args)
    }
}

/// Bytes outside the stack that differ between two states.
pub fn changed(a: &Replay, b: &Replay) -> usize {
    let count = |x: &[u8], y: &[u8]| {
        x.chunks(4096)
            .zip(y.chunks(4096))
            .filter(|(p, q)| p != q)
            .map(|(p, q)| p.iter().zip(q.iter()).filter(|(u, v)| u != v).count())
            .sum::<usize>()
    };
    compared()
        .into_iter()
        .map(|(x, y)| count(&a.ram[x..y], &b.ram[x..y]))
        .sum::<usize>()
        + count(&a.scratch, &b.scratch)
}

#[derive(Clone, Copy, Default, Debug)]
pub struct Stats {
    /// Occurrences reproduced exactly.
    pub passed: usize,
    /// Of those, occurrences that changed machine state.
    pub effective: usize,
    /// Bytes the original changed outside the stack, summed.
    pub changed: usize,
    /// Outside calls spliced from the trace.
    pub spliced: usize,
    /// Spliced calls that found interrupt writes (see [`INTERRUPT_OWNED`]).
    pub interrupts: usize,
}

/// Run every routine of `ov2_aircraft` on every traced occurrence and
/// require the original result; per routine statistics.
pub fn check_capture(trace: &Trace, routines: &[(u32, bool)]) -> BTreeMap<u32, Stats> {
    let returns: BTreeMap<u32, bool> = routines.iter().copied().collect();
    OWN.with(|own| *own.borrow_mut() = all_own());
    let mut stats: BTreeMap<u32, Stats> = BTreeMap::new();
    let mut replay = Replay::new(trace);
    for index in 0..trace.events.len() {
        replay.advance(index, None);
        let event = &trace.events[index];
        let routine = event.f;
        let Some(&returns_value) = returns.get(&routine) else {
            continue;
        };
        if event.kind != Kind::Entry {
            continue;
        }
        let Some(exit) = trace.try_matching_return(index) else {
            continue;
        };
        let mut m = replay.machine(event.gpr[29]);
        let mut splicer = Splicer {
            replay: replay.clone(),
            end: exit,
            calls: 0,
            interrupts: 0,
            error: None,
        };
        let args = [event.gpr[4], event.gpr[5], event.gpr[6], event.gpr[7]];
        let v0 = own(&mut m, &mut splicer, routine, args)
            .unwrap_or_else(|| panic!("{routine:08x} not claimed"));
        let context = format!(
            "{routine:08x} {} event {index} (frame {})",
            trace.name, event.frame
        );
        if let Err(fault) = m.take_fault() {
            panic!("{context}: {fault}");
        }
        if let Some(error) = splicer.error {
            panic!("{context}: {error}");
        }
        let start = splicer.replay.next;
        if let Some(missed) = (start..exit).find(|&i| {
            let e = &trace.events[i];
            e.kind == Kind::Entry && is_spliced(e.f)
        }) {
            panic!(
                "{context}: the original also calls {:08x} (event {missed})",
                trace.events[missed].f
            );
        }
        let mut original = splicer.replay;
        original.advance(exit, None);
        if let Some(difference) = original.differences(&m) {
            panic!("{context}: {difference}");
        }
        if returns_value {
            assert_eq!(v0, trace.events[exit].gpr[2], "{context}: v0");
        }
        let count = changed(&replay, &original);
        let entry = stats.entry(routine).or_default();
        entry.passed += 1;
        entry.effective += usize::from(count > 0);
        entry.changed += count;
        entry.spliced += splicer.calls;
        entry.interrupts += splicer.interrupts;
    }
    for (routine, s) in &stats {
        eprintln!(
            "{routine:08x} {}: {} exact ({} changing state), {} bytes changed, {} calls spliced, {} interrupts",
            trace.name, s.passed, s.effective, s.changed, s.spliced, s.interrupts
        );
    }
    stats
}

/// The occurrence number of `f`'s first entry at or after `frame`.
pub fn occurrence_at(trace: &Trace, f: u32, frame: u64) -> usize {
    trace
        .events
        .iter()
        .filter(|e| e.kind == Kind::Entry && e.f == f)
        .position(|e| e.frame >= frame)
        .unwrap_or_else(|| panic!("{}: no entry of {f:08x} from frame {frame}", trace.name))
}

/// The original machine at the `occurrence`th entry of `f` in `trace`,
/// with that entry's argument registers.
pub fn machine_at(trace: &Trace, f: u32, occurrence: usize) -> (Machine, [u32; 4]) {
    let index = trace
        .events
        .iter()
        .enumerate()
        .filter(|(_, e)| e.kind == Kind::Entry && e.f == f)
        .nth(occurrence)
        .unwrap_or_else(|| panic!("{}: no entry #{occurrence} of {f:08x}", trace.name))
        .0;
    let mut replay = Replay::new(trace);
    replay.advance(index, None);
    let event = &trace.events[index];
    (
        replay.machine(event.gpr[29]),
        [event.gpr[4], event.gpr[5], event.gpr[6], event.gpr[7]],
    )
}

/// The original machine at the return matching the `occurrence`th entry of
/// `f` in `trace`, with its `v0`.
pub fn return_state(trace: &Trace, f: u32, occurrence: usize) -> (Machine, u32) {
    let index = trace
        .events
        .iter()
        .enumerate()
        .filter(|(_, e)| e.kind == Kind::Entry && e.f == f)
        .nth(occurrence)
        .unwrap_or_else(|| panic!("{}: no entry #{occurrence} of {f:08x}", trace.name))
        .0;
    let exit = trace.matching_return(index);
    let mut replay = Replay::new(trace);
    replay.advance(exit, None);
    let event = &trace.events[exit];
    (replay.machine(event.gpr[29]), event.gpr[2])
}

/// Differences between two machines outside the stack and the interrupt
/// ranges (RAM, scratchpad, GTE).
pub fn machine_differences(a: &Machine, b: &Machine) -> usize {
    compared()
        .into_iter()
        .flat_map(|(x, y)| x..y)
        .filter(|&i| a.ram()[i] != b.ram()[i])
        .count()
        + (0..SCRATCH_BYTES)
            .filter(|&i| a.scratch()[i] != b.scratch()[i])
            .count()
        + usize::from(a.gte.data != b.gte.data || a.gte.control != b.gte.control)
}
