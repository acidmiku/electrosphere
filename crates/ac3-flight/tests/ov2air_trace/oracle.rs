//! TEST-ONLY differential oracle for ov2air's routines on paths no capture
//! reaches. Like the shared `tests/support/oracle.rs`, it interprets the
//! ORIGINAL instruction bytes from machine RAM; unlike it, it interprets
//! the whole call tree (any routine in RAM) and lets a policy take over at
//! device-facing calls, so a routine and everything it calls can be run in
//! original code next to the native translation:
//!
//! - reference run: the routine and all its callees interpreted;
//! - native run: ov2air's routines run natively wherever they are called
//!   (the routine itself, or inside an interpreted caller such as a state's
//!   update callback), everything else interpreted;
//!
//! with the same [`HOST`] routines (GPU, SPU/sound, BIOS vectors, timers)
//! replaced in both by a stub that logs `(target, arguments)` and answers
//! through a [`Host`] policy (0 by default; a test may model a memory card
//! or the SPU). The runs must agree on all RAM outside the stack and the
//! interrupt ranges, the scratchpad, the GTE registers, `v0` and the stub
//! log (the arguments each call reads). [`updates`] chains such runs over
//! scripted frames from the original's previous state; the reference runs
//! record the instructions interpreted, so [`coverage`] reports what the
//! original code of each routine executed. The port itself never
//! interprets MIPS code (FLIGHT_MACHINE.md).
use super::{STACK, all_own, compared, own};
use ac3_flight::{Fault, Machine, ov2_aircraft::Calls};
use std::collections::{BTreeSet, HashSet};
use std::sync::OnceLock;

/// A return address no routine uses: reaching it ends a run.
const RETURN: u32 = 0x800d_ead4;

/// Device-facing or BIOS routines stubbed in both runs (they touch I/O
/// registers or BIOS code outside RAM): TIM upload, LoadImage, StoreImage,
/// ClearImage2, DrawSync, the sound requests, GetODE, the SPU transfer
/// helpers, 8002B900/8002B934, the BIOS file calls (open, read, write,
/// close, nextfile, firstfile) and any other BIOS vector call (pc below
/// 0x80000000: 0xA0/0xB0/0xC0).
pub const HOST: [u32; 19] = [
    0x8002_6be4,
    0x8002_6d00,
    0x800a_6910,
    0x800a_6970,
    0x800a_6878,
    0x800a_6664,
    0x8002_d724,
    0x8002_db18,
    0x800a_73c0,
    0x8009_f358,
    0x8009_f7d8,
    0x8002_b900,
    0x8002_b934,
    0x800a_d808,
    0x800a_d818,
    0x800a_d828,
    0x800a_d838,
    0x800a_d848,
    0x800a_da08,
];

fn is_host(target: u32) -> bool {
    HOST.contains(&target) || !(0x8000_0000..0xa000_0000).contains(&target)
}

/// The stub log: `(target or BIOS vector | t1 << 8, a0..a3)`.
pub type Log = Vec<(u32, [u32; 4])>;

/// The stubbed host calls' results: `(machine, key, arguments) -> v0`, with
/// the keys of the [`Log`]. It may write the machine (a BIOS `read` filling
/// its buffer), but must depend only on its inputs: both runs call it.
pub type Host<'a> = &'a dyn Fn(&mut Machine, u32, [u32; 4]) -> u32;

/// Every host call returns 0.
pub fn zero(_: &mut Machine, _: u32, _: [u32; 4]) -> u32 {
    0
}

/// One run's call policy and record.
pub struct Run<'a> {
    pub log: Log,
    /// The native run: ov2air's routines run natively wherever they are
    /// called, interpreted code included.
    pub native: bool,
    pub host: Host<'a>,
    /// The instruction addresses interpreted (reference runs).
    pub coverage: Option<BTreeSet<u32>>,
}

/// ov2air's routines (all modules).
fn owned() -> &'static HashSet<u32> {
    static OWNED: OnceLock<HashSet<u32>> = OnceLock::new();
    OWNED.get_or_init(|| all_own().into_iter().collect())
}

/// Interpret the original code at `entry` with arguments `args` (stack
/// arguments stay at `m.sp + 0x10..`), stubbing [`HOST`] calls into `log`
/// (results 0). Returns `v0`.
pub fn interpret(m: &mut Machine, entry: u32, args: [u32; 4], log: &mut Log) -> u32 {
    let mut run = Run {
        log: std::mem::take(log),
        native: false,
        host: &zero,
        coverage: None,
    };
    let v0 = execute(m, entry, args, &mut run);
    *log = run.log;
    v0
}

/// [`interpret`] under the policy `run`.
pub fn execute(m: &mut Machine, entry: u32, args: [u32; 4], run: &mut Run) -> u32 {
    let mut r = [0u32; 32];
    let (mut hi, mut lo) = (0u32, 0u32);
    r[4..8].copy_from_slice(&args);
    r[28] = ac3_flight::GP;
    r[29] = m.sp;
    r[31] = RETURN;
    let (mut pc, mut next) = (entry, entry + 4);
    let mut steps = 0u32;
    loop {
        if pc == RETURN {
            m.sp = r[29];
            return r[2];
        }
        // A host routine can only be entered at its start, by a call (or a
        // BIOS trampoline's `jr t2`): stub it and return to the caller.
        if is_host(pc) {
            let key = if pc < 0x8000_0000 {
                pc | (r[9] << 8)
            } else {
                pc
            };
            let a = [r[4], r[5], r[6], r[7]];
            run.log.push((key, a));
            r[2] = (run.host)(m, key, a);
            pc = r[31];
            next = pc.wrapping_add(4);
            continue;
        }
        // Likewise one of ov2air's routines, in the native run.
        if run.native && steps > 0 && owned().contains(&pc) {
            m.sp = r[29];
            let a = [r[4], r[5], r[6], r[7]];
            let v0 = own(m, &mut OracleCalls { run: &mut *run }, pc, a).expect("claimed");
            assert_eq!(m.sp, r[29], "{pc:08x} left the stack unbalanced");
            r[2] = v0;
            pc = r[31];
            next = pc.wrapping_add(4);
            continue;
        }
        if m.fault().is_some() {
            m.sp = r[29];
            return r[2];
        }
        steps += 1;
        assert!(steps < 50_000_000, "runaway at {pc:08x}");
        if let Some(coverage) = run.coverage.as_mut() {
            coverage.insert(pc);
        }
        let w = m.lw(pc);
        let cur = pc;
        pc = next;
        next = next.wrapping_add(4);
        let op = w >> 26;
        let (rs, rt, rd) = (
            ((w >> 21) & 31) as usize,
            ((w >> 16) & 31) as usize,
            ((w >> 11) & 31) as usize,
        );
        let sa = (w >> 6) & 31;
        let simm = (w & 0xffff) as u16 as i16 as i32 as u32;
        let zimm = w & 0xffff;
        let bt = cur.wrapping_add(4).wrapping_add(simm << 2);
        let jt = (cur.wrapping_add(4) & 0xf000_0000) | ((w & 0x03ff_ffff) << 2);
        let (a, b) = (r[rs], r[rt]);
        let mut out: Option<(usize, u32)> = None;
        match op {
            0 => match w & 63 {
                0x00 => out = Some((rd, b << sa)),
                0x02 => out = Some((rd, b >> sa)),
                0x03 => out = Some((rd, ((b as i32) >> sa) as u32)),
                0x04 => out = Some((rd, b << (a & 31))),
                0x06 => out = Some((rd, b >> (a & 31))),
                0x07 => out = Some((rd, ((b as i32) >> (a & 31)) as u32)),
                0x08 => next = a,
                0x09 => {
                    out = Some((rd, cur + 8));
                    next = a;
                }
                0x0c => {
                    // syscall (BIOS critical sections): stubbed.
                    run.log.push((0xffff_0000 | r[4], [r[4], r[5], r[6], r[7]]));
                }
                0x0d => {
                    let code = ((w >> 6) & 0xf_ffff) >> 10;
                    m.raise(Fault::Trap { address: cur, code });
                    m.sp = r[29];
                    return r[2];
                }
                0x10 => out = Some((rd, hi)),
                0x11 => hi = a,
                0x12 => out = Some((rd, lo)),
                0x13 => lo = a,
                0x18 => {
                    let p = i64::from(a as i32) * i64::from(b as i32);
                    lo = p as u32;
                    hi = (p >> 32) as u32;
                }
                0x19 => {
                    let p = u64::from(a) * u64::from(b);
                    lo = p as u32;
                    hi = (p >> 32) as u32;
                }
                0x1a => {
                    let (n, d) = (a as i32, b as i32);
                    if d == 0 {
                        lo = if n < 0 { 1 } else { u32::MAX };
                        hi = a;
                    } else {
                        lo = n.wrapping_div(d) as u32;
                        hi = n.wrapping_rem(d) as u32;
                    }
                }
                0x1b => {
                    lo = a.checked_div(b).unwrap_or(u32::MAX);
                    hi = a.checked_rem(b).unwrap_or(a);
                }
                0x20 | 0x21 => out = Some((rd, a.wrapping_add(b))),
                0x22 | 0x23 => out = Some((rd, a.wrapping_sub(b))),
                0x24 => out = Some((rd, a & b)),
                0x25 => out = Some((rd, a | b)),
                0x26 => out = Some((rd, a ^ b)),
                0x27 => out = Some((rd, !(a | b))),
                0x2a => out = Some((rd, u32::from((a as i32) < (b as i32)))),
                0x2b => out = Some((rd, u32::from(a < b))),
                f => panic!("special {f:#x} at {cur:08x}"),
            },
            1 => {
                let take = if rt & 1 == 0 {
                    (a as i32) < 0
                } else {
                    (a as i32) >= 0
                };
                if rt & 0x10 != 0 {
                    r[31] = cur + 8;
                }
                if take {
                    next = bt;
                }
            }
            2 => next = jt,
            3 => {
                r[31] = cur + 8;
                next = jt;
            }
            4 if a == b => next = bt,
            5 if a != b => next = bt,
            6 if (a as i32) <= 0 => next = bt,
            7 if (a as i32) > 0 => next = bt,
            4..=7 => {}
            8 | 9 => out = Some((rt, a.wrapping_add(simm))),
            10 => out = Some((rt, u32::from((a as i32) < (simm as i32)))),
            11 => out = Some((rt, u32::from(a < simm))),
            12 => out = Some((rt, a & zimm)),
            13 => out = Some((rt, a | zimm)),
            14 => out = Some((rt, a ^ zimm)),
            15 => out = Some((rt, zimm << 16)),
            16 => {}
            18 => {
                if w & (1 << 25) != 0 {
                    m.gte.command(w & 0x1ff_ffff).unwrap();
                } else {
                    match rs {
                        0 => out = Some((rt, m.gte.mfc2(rd))),
                        2 => out = Some((rt, m.gte.cfc2(rd))),
                        4 => m.gte.mtc2(rd, b),
                        6 => m.gte.ctc2(rd, b),
                        x => panic!("cop2 {x} at {cur:08x}"),
                    }
                }
            }
            0x20 => out = Some((rt, m.lb(a.wrapping_add(simm)) as i32 as u32)),
            0x21 => out = Some((rt, m.lh(a.wrapping_add(simm)) as i32 as u32)),
            0x23 => out = Some((rt, m.lw(a.wrapping_add(simm)))),
            0x24 => out = Some((rt, u32::from(m.lbu(a.wrapping_add(simm))))),
            0x25 => out = Some((rt, u32::from(m.lhu(a.wrapping_add(simm))))),
            0x28 => m.sb(a.wrapping_add(simm), b as u8),
            0x29 => m.sh(a.wrapping_add(simm), b as u16),
            0x2b => m.sw(a.wrapping_add(simm), b),
            0x22 | 0x26 | 0x2a | 0x2e => {
                let addr = a.wrapping_add(simm);
                let aligned = addr & !3;
                let shift = (addr & 3) as usize;
                let mem = m.lw(aligned);
                match op {
                    0x22 => {
                        const MASK: [u32; 4] = [0x00ff_ffff, 0x0000_ffff, 0x0000_00ff, 0];
                        const SH: [u32; 4] = [24, 16, 8, 0];
                        out = Some((rt, (b & MASK[shift]) | (mem << SH[shift])));
                    }
                    0x26 => {
                        const MASK: [u32; 4] = [0, 0xff00_0000, 0xffff_0000, 0xffff_ff00];
                        const SH: [u32; 4] = [0, 8, 16, 24];
                        out = Some((rt, (b & MASK[shift]) | (mem >> SH[shift])));
                    }
                    0x2a => {
                        const MASK: [u32; 4] = [0xffff_ff00, 0xffff_0000, 0xff00_0000, 0];
                        const SH: [u32; 4] = [24, 16, 8, 0];
                        m.sw(aligned, (mem & MASK[shift]) | (b >> SH[shift]));
                    }
                    _ => {
                        const MASK: [u32; 4] = [0, 0x0000_00ff, 0x0000_ffff, 0x00ff_ffff];
                        const SH: [u32; 4] = [0, 8, 16, 24];
                        m.sw(aligned, (mem & MASK[shift]) | (b << SH[shift]));
                    }
                }
            }
            0x32 => {
                let value = m.lw(a.wrapping_add(simm));
                m.gte.mtc2(rt, value);
            }
            0x3a => {
                let value = m.gte.mfc2(rt);
                m.sw(a.wrapping_add(simm), value);
            }
            x => panic!("opcode {x:#x} at {cur:08x}"),
        }
        if let Some((reg, value)) = out
            && reg != 0
        {
            r[reg] = value;
        }
    }
}

/// The native run's [`Calls`]: ov2air's routines natively, [`HOST`]
/// routines stubbed into the run's log, everything else interpreted under
/// the run's policy.
pub struct OracleCalls<'r, 'a> {
    pub run: &'r mut Run<'a>,
}

impl Calls for OracleCalls<'_, '_> {
    fn call(&mut self, m: &mut Machine, target: u32, args: [u32; 4]) -> u32 {
        if let Some(v0) = own(m, self, target, args) {
            return v0;
        }
        if is_host(target) {
            self.run.log.push((target, args));
            return (self.run.host)(m, target, args);
        }
        execute(m, target, args, self.run)
    }
}

/// The argument registers a BIOS vector call (key `vector | function << 8`)
/// or a syscall (key 0xFFFF0000 | a0) reads; the rest hold whatever the
/// caller left there. Unlisted calls compare all four.
fn vector_arguments(key: u32) -> u8 {
    match key {
        // EnterCriticalSection/ExitCriticalSection: the code in a0.
        k if k & 0xffff_0000 == 0xffff_0000 => 0b0001,
        // B(0x09..0x0D): CloseEvent, WaitEvent, TestEvent, EnableEvent,
        // DisableEvent (the event).
        0x09b0 | 0x0ab0 | 0x0bb0 | 0x0cb0 | 0x0db0 => 0b0001,
        // B(0x4E)/B(0x4F) _card_write/_card_read (port, sector, buffer).
        0x4eb0 | 0x4fb0 => 0b0111,
        // B(0x50) _new_card.
        0x50b0 => 0,
        _ => 0b1111,
    }
}

/// A run both ways that agreed.
pub struct Outcome {
    /// Bytes the original changed (outside the stack).
    pub changed: usize,
    /// `v0` of both runs.
    pub v0: u32,
    /// The original's machine afterwards.
    pub reference: Machine,
    /// The fault both runs ended on.
    pub fault: Option<Fault>,
    /// The instructions the original interpreted.
    pub coverage: BTreeSet<u32>,
}

/// Run `routine` both ways from `m` and require identical results (`v0`
/// only when `returns`): the reference run interprets everything; the
/// native run runs ov2air's routines natively wherever they are called,
/// starting with `routine` itself when it is one of them (otherwise it is
/// interpreted, a caller such as an update callback). `host` gives the
/// stubbed host calls' results.
pub fn compare(
    m: &Machine,
    routine: u32,
    args: [u32; 4],
    returns: bool,
    host: Host,
) -> Result<Outcome, String> {
    let mut reference = m.clone();
    let mut run = Run {
        log: Log::new(),
        native: false,
        host,
        coverage: Some(BTreeSet::new()),
    };
    let v0 = execute(&mut reference, routine, args, &mut run);
    let reference_fault = reference.take_fault().err();
    let mut native = m.clone();
    let mut native_run = Run {
        log: Log::new(),
        native: true,
        host,
        coverage: None,
    };
    let native_v0 = if owned().contains(&routine) {
        own(
            &mut native,
            &mut OracleCalls {
                run: &mut native_run,
            },
            routine,
            args,
        )
        .ok_or_else(|| format!("{routine:08x} not claimed"))?
    } else {
        execute(&mut native, routine, args, &mut native_run)
    };
    let native_fault = native.take_fault().err();
    if reference_fault != native_fault {
        return Err(format!(
            "{routine:08x}: faults differ: original {reference_fault:?}, native {native_fault:?}"
        ));
    }
    let ranges = compared();
    let ram: Vec<usize> = ranges
        .iter()
        .flat_map(|&(a, b)| a..b)
        .filter(|&i| native.ram()[i] != reference.ram()[i])
        .collect();
    let scratch: Vec<usize> = (0..native.scratch().len())
        .filter(|&i| native.scratch()[i] != reference.scratch()[i])
        .collect();
    if !ram.is_empty()
        || !scratch.is_empty()
        || native.gte.data != reference.gte.data
        || native.gte.control != reference.gte.control
    {
        let list = |v: &[usize], base: u32, ours: &[u8], theirs: &[u8]| {
            v.iter()
                .take(24)
                .map(|&a| format!("{:08x}:{:02x}/{:02x}", base + a as u32, ours[a], theirs[a]))
                .collect::<Vec<_>>()
                .join(" ")
        };
        return Err(format!(
            "{routine:08x}: ram {} [{}] scratch {} [{}] (native/original)",
            ram.len(),
            list(&ram, 0x8000_0000, native.ram(), reference.ram()),
            scratch.len(),
            list(&scratch, 0x1f80_0000, native.scratch(), reference.scratch()),
        ));
    }
    if returns && native_v0 != v0 {
        return Err(format!(
            "{routine:08x}: v0 {native_v0:#x}, original {v0:#x}"
        ));
    }
    let masked = |log: &Log| -> Vec<(u32, Vec<u32>)> {
        log.iter()
            .map(|&(target, args)| {
                let mask = super::args::ARGS
                    .iter()
                    .find(|e| e.0 == target)
                    .map_or_else(|| vector_arguments(target), |e| e.1);
                let used = (0..4)
                    .filter(|i| mask & (1 << i) != 0)
                    .map(|i| args[i])
                    .collect();
                (target, used)
            })
            .collect()
    };
    if masked(&native_run.log) != masked(&run.log) {
        return Err(format!(
            "{routine:08x}: device calls differ:\n native {:08x?}\n original {:08x?}",
            native_run.log, run.log
        ));
    }
    let changed = ranges
        .iter()
        .flat_map(|&(a, b)| a..b)
        .filter(|&i| m.ram()[i] != reference.ram()[i])
        .count();
    let _ = STACK;
    Ok(Outcome {
        changed,
        v0,
        reference,
        fault: reference_fault,
        coverage: run.coverage.unwrap_or_default(),
    })
}

/// [`compare`] with every host call returning 0; the bytes changed.
pub fn differential(
    m: &Machine,
    routine: u32,
    args: [u32; 4],
    returns: bool,
) -> Result<usize, String> {
    compare(m, routine, args, returns, &zero).map(|outcome| outcome.changed)
}

/// A scripted run of consecutive updates that all agreed.
pub struct Session {
    /// The original's machine after the last update.
    pub machine: Machine,
    /// The updates run, and those that changed state.
    pub updates: usize,
    pub effective: usize,
    /// Each update's `v0`.
    pub results: Vec<u32>,
    /// The fault the run ended on, if any.
    pub fault: Option<Fault>,
    pub coverage: BTreeSet<u32>,
}

/// Run `routine` (an update, called once per frame) `count` times from
/// `m`, each time both ways from the original's previous result, after
/// `prepare(machine, update)` (inputs, card events). Stops at a fault both
/// runs share.
pub fn updates(
    m: &Machine,
    routine: u32,
    args: [u32; 4],
    count: usize,
    host: Host,
    prepare: &mut dyn FnMut(&mut Machine, usize),
) -> Result<Session, String> {
    let mut session = Session {
        machine: m.clone(),
        updates: 0,
        effective: 0,
        results: Vec::new(),
        fault: None,
        coverage: BTreeSet::new(),
    };
    for update in 0..count {
        prepare(&mut session.machine, update);
        let outcome = compare(&session.machine, routine, args, true, host)
            .map_err(|error| format!("update {update}: {error}"))?;
        session.updates += 1;
        session.effective += usize::from(outcome.changed > 0);
        session.results.push(outcome.v0);
        session.coverage.extend(outcome.coverage);
        session.machine = outcome.reference;
        if outcome.fault.is_some() {
            session.fault = outcome.fault;
            break;
        }
    }
    Ok(session)
}

/// The end of the routine at `start` (exclusive): the first `jr ra` (and
/// its delay slot) past every branch target and forward `j` seen so far.
pub fn extent(m: &mut Machine, start: u32) -> u32 {
    let mut reach = start;
    let mut p = start;
    loop {
        let w = m.lw(p);
        let op = w >> 26;
        let simm = (w & 0xffff) as u16 as i16 as i32 as u32;
        let target = match op {
            1 | 4..=7 | 0x14..=0x17 => Some(p.wrapping_add(4).wrapping_add(simm << 2)),
            2 => Some((p.wrapping_add(4) & 0xf000_0000) | ((w & 0x03ff_ffff) << 2))
                .filter(|&t| t > p && t < p + 0x2000),
            _ => None,
        };
        if let Some(t) = target {
            reach = reach.max(t);
        }
        if w == 0x03e0_0008 && reach <= p + 4 {
            return p + 8;
        }
        p += 4;
        assert!(p < start + 0x4000, "{start:08x}: no end");
    }
}

/// Per routine: `(start, instructions interpreted, instructions of its
/// extent, the addresses never interpreted)`.
pub type Covered = (u32, usize, usize, Vec<u32>);

/// [`Covered`] for each of `routines` (extents from `m`'s code).
pub fn coverage(m: &Machine, routines: &[u32], covered: &BTreeSet<u32>) -> Vec<Covered> {
    let mut m = m.clone();
    routines
        .iter()
        .map(|&start| {
            let end = extent(&mut m, start);
            let all: Vec<u32> = (start..end).step_by(4).collect();
            let missed: Vec<u32> = all
                .iter()
                .copied()
                .filter(|a| !covered.contains(a))
                .collect();
            (start, all.len() - missed.len(), all.len(), missed)
        })
        .collect()
}

/// `missed` as compact address ranges.
pub fn ranges(missed: &[u32]) -> String {
    let mut out = Vec::new();
    let mut i = 0;
    while i < missed.len() {
        let mut j = i;
        while j + 1 < missed.len() && missed[j + 1] == missed[j] + 4 {
            j += 1;
        }
        out.push(if i == j {
            format!("{:08x}", missed[i])
        } else {
            format!("{:08x}..{:08x}", missed[i], missed[j])
        });
        i = j + 1;
    }
    out.join(" ")
}
