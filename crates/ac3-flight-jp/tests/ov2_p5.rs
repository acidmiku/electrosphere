//! JP overlay 2, part 5 (800F5834..800FF95C, `src/jp/ov2_p5*.rs`) against
//! the JP originals.
//!
//! 1. **Captures.** Every routine of the part with a JP Redux capture
//!    (`story-002/snapshots-module2.jsonl`, the first call of each reached
//!    routine) runs natively from its entry snapshot and must reproduce the
//!    return snapshot (`snapshot::check_call`: RAM outside the stack,
//!    scratchpad, GTE, and `v0` unless the claim is void).
//! 2. **Oracle.** Every routine of the part runs from module-2 RAM states
//!    twice, as the original instructions (a copy of the test-only oracle
//!    `support::oracle::run` that also records which instructions ran) and
//!    as the translation (`dispatch::call`); RAM outside the stack window,
//!    scratchpad, GTE, `sp`, the first fault and `v0` must agree. States:
//!    the routine's own entry snapshot and eight module-2 states of the
//!    story, save and flight runs, each with randomised variants:
//!    arguments (the captured ones, pointers of the gp area and of the
//!    overlay blocks, small numbers) and fields (words, halfwords and bytes
//!    at the offsets the routine's own loads use, in the blocks of the
//!    global pointers it reads, of the context and of pointer arguments).
//!    The report lists, per routine, the cases, the share of its
//!    instructions the original executed over all cases, and the first
//!    failure.
//!
//! Calls out of a routine run natively in both runs (`dispatch::call`); the
//! untranslated ones (other parts) through the development fallback, which
//! each worker installs: feature `dev-fallback`. Results:
//! `$CARGO_TARGET_TMPDIR/jp-ov2-p5.tsv`.
#![cfg(feature = "dev-fallback")]
mod support;

use ac3_fallback::{Fallback, Version};
use ac3_flight_jp::snapshot::{self, Snapshot, SnapshotInfo};
use ac3_flight_jp::{Device, Fault, HostCall, Machine};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};
use support::scan;

/// The part: overlay-2 function starts in this range.
const PART: (u32, u32) = (0x800f_5834, 0x800f_f95c);
/// Variants per state.
const VARIANTS: u32 = 32;
/// Host calls a case may make before it counts as a runaway.
const HOST_CALLS: usize = 20_000;
/// The stack window (not compared).
const STACK: std::ops::Range<u32> = 0x1f_c000..0x20_0000;
/// Per-routine time limit (a case that does not end: a wait loop on a
/// random state).
const TIMEOUT: Duration = Duration::from_secs(300);
/// Instruction limits: of the routine under test (the oracle stops with
/// a panic: the case is skipped) and of each interpreted callee (the
/// fallback raises its RUNAWAY trap, in both runs alike).
const ORACLE_STEPS: u32 = 5_000_000;
const FALLBACK_STEPS: u64 = 3_000_000;
/// No new case starts after this long (slow routines with interpreted
/// callees run fewer cases; the report says so).
const BUDGET: Duration = Duration::from_secs(150);
const WORKERS: usize = 6;
const WORKER: &str = "ov2-p5-worker";
/// Module-2 RAM states (under `AC3_JP_DIR/analysis/redux`): hub, archive,
/// options, briefing, aircraft select, save, after a mission.
const STATES: [&str; 8] = [
    "story-001/ram-017800",
    "story-001/ram-030300",
    "story-001/ram-038710",
    "story-001/ram-040610",
    "story-001/ram-046300",
    "save-001/ram-054250",
    "save-001/ram-055790",
    "flight-001/ram-053900",
];

fn redux() -> PathBuf {
    std::env::var_os("AC3_JP_CAPTURES")
        .map(PathBuf::from)
        .unwrap_or_else(|| scan::jp_dir().join("analysis/redux"))
}

/// The part's routines: (start, end, class).
fn part() -> Vec<(u32, u32, String)> {
    scan::jp_map(&scan::jp_dir())
        .into_iter()
        .filter(|r| r.region == "ov2" && (PART.0..PART.1).contains(&r.jp))
        .map(|r| (r.jp, r.jp + r.size, r.class))
        .collect()
}

/// The claims of this part's files (`jp/ov2_p5*.rs`) and the void ones
/// (no `v0`).
fn claims() -> (BTreeSet<u32>, BTreeSet<u32>) {
    let (claims, void) =
        scan::claims_and_void(&scan::crate_dir(), &|name| !name.starts_with("jp/ov2_p5"));
    (claims.into_keys().collect(), void)
}

/// A device that answers nothing and panics after `limit` calls.
struct Limit(usize, usize);
impl Device for Limit {
    fn call(&mut self, _: &mut [u8], _: &mut [u8], _: HostCall) -> Option<u32> {
        self.0 += 1;
        assert!(self.0 <= self.1, "runaway: {} host calls", self.1);
        None
    }
}

const RETURN: u32 = 0xffff_fff0;

/// `support::oracle::run`, recording the executed instructions of
/// `start..end` in `seen` (one flag per word).
fn run(m: &mut Machine, start: u32, end: u32, args: [u32; 4], seen: &mut [bool]) -> u32 {
    let mut r = [0u32; 32];
    let (mut hi, mut lo) = (0u32, 0u32);
    r[4..8].copy_from_slice(&args);
    r[28] = ac3_flight_jp::GP;
    r[29] = m.sp;
    r[31] = RETURN;
    let (mut pc, mut next) = (start, start + 4);
    let mut steps = 0u32;
    loop {
        if pc == RETURN {
            m.sp = r[29];
            return r[2];
        }
        if pc < start || pc >= end {
            m.sp = r[29];
            let wide = |hi: u32, lo: u32| (u64::from(hi) << 32) | u64::from(lo);
            match pc {
                0x8009_ea48 => {
                    let q = ac3_flight_jp::sdk::udivdi3(m, wide(r[5], r[4]), wide(r[7], r[6]));
                    (r[2], r[3]) = (q as u32, (q >> 32) as u32);
                }
                0x8009_e9b0 => {
                    let q = ac3_flight_jp::sdk::divdi3(
                        m,
                        wide(r[5], r[4]) as i64,
                        wide(r[7], r[6]) as i64,
                    ) as u64;
                    (r[2], r[3]) = (q as u32, (q >> 32) as u32);
                }
                _ => r[2] = ac3_flight_jp::dispatch::call(m, pc, [r[4], r[5], r[6], r[7]]),
            }
            pc = r[31];
            next = pc + 4;
            continue;
        }
        seen[((pc - start) / 4) as usize] = true;
        steps += 1;
        assert!(steps < ORACLE_STEPS, "runaway at {pc:08x}");
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
            0xa => out = Some((rt, u32::from((a as i32) < (simm as i32)))),
            0xb => out = Some((rt, u32::from(a < simm))),
            0xc => out = Some((rt, a & zimm)),
            0xd => out = Some((rt, a | zimm)),
            0xe => out = Some((rt, a ^ zimm)),
            0xf => out = Some((rt, zimm << 16)),
            0x12 => {
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
                let v = m.lw(a.wrapping_add(simm));
                m.gte.mtc2(rt, v);
            }
            0x3a => {
                let v = m.gte.mfc2(rt);
                m.sw(a.wrapping_add(simm), v);
            }
            o => panic!("op {o:#x} at {cur:08x}"),
        }
        if let Some((i, v)) = out
            && i != 0
        {
            r[i] = v;
        }
    }
}

/// What a routine's code reads: the global pointer words it loads (a `lui`
/// of 0x800B..0x8011 and a `lw` of that page), and the immediate offsets
/// and widths of its loads.
struct Shape {
    globals: Vec<u32>,
    offsets: Vec<(u32, u8)>,
}

fn shape(m: &mut Machine, start: u32, end: u32) -> Shape {
    let mut upper = [None::<u32>; 32];
    let (mut globals, mut offsets) = (BTreeSet::new(), BTreeSet::new());
    for pc in (start..end).step_by(4) {
        let w = m.lw(pc);
        let op = w >> 26;
        let (rs, rt) = (((w >> 21) & 31) as usize, ((w >> 16) & 31) as usize);
        let simm = (w & 0xffff) as u16 as i16 as i32 as u32;
        if op == 0xf {
            let hi = (w & 0xffff) << 16;
            upper[rt] = (0x800b_0000..0x8012_0000).contains(&hi).then_some(hi);
            continue;
        }
        let width = match op {
            0x20 | 0x24 => 1,
            0x21 | 0x25 => 2,
            0x23 => 4,
            _ => 0,
        };
        if width == 4
            && let Some(hi) = upper[rs]
        {
            globals.insert(hi.wrapping_add(simm));
        } else if width != 0 && rs != 29 && rs != 28 && simm < 0x2000 {
            offsets.insert((simm, width));
        }
        if width == 4 && rs == 28 {
            globals.insert(ac3_flight_jp::GP.wrapping_add(simm));
        }
    }
    globals.insert(ac3_flight_jp::GP + ac3_flight_jp::common::CONTEXT);
    Shape {
        globals: globals.into_iter().collect(),
        offsets: offsets.into_iter().collect(),
    }
}

struct Rng(u32);
impl Rng {
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(1_103_515_245).wrapping_add(12345);
        self.0 >> 8
    }
    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.next() as usize % items.len()]
    }
}

/// An interesting value for a field of `width` bytes.
fn value(rng: &mut Rng, width: u8, old: u32) -> u32 {
    match rng.next() % 10 {
        0 => 0,
        1 => 1,
        2 => 2,
        3 => 3,
        4 => rng.next() % 8,
        5 => old ^ (1 << (rng.next() % (8 * u32::from(width)))),
        6 => u32::MAX,
        7 => old.wrapping_add(1),
        8 => old.wrapping_sub(1),
        _ => rng.next(),
    }
}

fn valid(p: u32) -> bool {
    (0x8001_0000..0x8020_0000).contains(&p)
}

/// Where a knob's field is.
#[derive(Clone, Copy)]
enum Base {
    /// The runtime context (`[gp+0x2EC]`).
    Ctx,
    /// The block a global pointer word points at.
    Ptr(u32),
    /// The block a pointer field of a global's block points at:
    /// `[[global] + field]`.
    Ptr2(u32, u32),
    /// An absolute address (the offset).
    Abs,
    /// Argument register a0..a3 (the offset is ignored).
    Arg(usize),
}

/// A field (or one of `count` fields `stride` apart) that selects a path
/// of a routine, and the values that matter.
struct Knob {
    base: Base,
    offset: u32,
    width: u8,
    count: u32,
    stride: u32,
    values: &'static [u32],
}

const fn k(base: Base, offset: u32, width: u8, values: &'static [u32]) -> Knob {
    Knob {
        base,
        offset,
        width,
        count: 1,
        stride: 0,
        values,
    }
}
/// `count` fields of `width` bytes from `offset`, `stride` apart.
const fn ks(
    base: Base,
    offset: u32,
    width: u8,
    count: u32,
    stride: u32,
    values: &'static [u32],
) -> Knob {
    Knob {
        base,
        offset,
        width,
        count,
        stride,
        values,
    }
}

const HUB: u32 = 0x8011_2dc4;
const DEBRIEF: u32 = 0x8011_2e04;
const RECORD: u32 = 0x8011_2dc0;
const NET: u32 = 0x8011_2e10;
const DISC: u32 = 0x8011_2e1c;
const CHECK: u32 = 0x8011_278c;

/// The path-selecting fields of the routines whose branches random fields
/// rarely reach (from their instructions).
fn knobs(start: u32) -> Vec<Knob> {
    use Base::*;
    const BOOL: &[u32] = &[0, 1];
    let mut out = Vec::new();
    match start {
        0x800f_5834 | 0x800f_5a24 | 0x800f_59b4 => {
            out.push(k(Ptr(DEBRIEF), 0, 1, &[0, 1, 2]));
            out.push(k(Ptr(DEBRIEF), 2, 2, &[0, 1, 30, 100, 0x7fff, 0x8000]));
            out.push(k(Ptr(DEBRIEF), 0x3d4, 2, &[0, 1, 0x1e, 0x100]));
            out.push(k(Ctx, 0x3ba, 2, &[0, 29, 30, 3000, 0xffff]));
        }
        0x800f_5b04 => {
            out.push(k(Ctx, 0x954, 2, &[1, 1, 1, 0]));
            for o in [
                0x2e2u32, 0x31e, 0x35a, 0x396, 0x2e7, 0x323, 0x35f, 0x39b, 0x2d2, 0x30e, 0x34a,
                0x386, 0x2f5, 0x331, 0x36d, 0x3a9, 0x2f0, 0x32c, 0x368, 0x3a4,
            ] {
                out.push(k(Ctx, 0x95c + o, 1, &[0, 0, 0, 1]));
            }
            out.push(k(Arg(0), 0, 4, &[0x22, 0x27, 0x12, 0x35, 0x30, 5]));
        }
        0x800f_5ca4 => {
            out.push(k(Ctx, 0x394, 4, &[0, 1, 2, 3, 4]));
            out.push(k(Ctx, 0x3ba, 2, &[0, 5, 0x100, 0x8000]));
            out.push(k(Ctx, 0x3bc, 1, &[0, 3, 0x20]));
            out.push(k(Ctx, 0x3c3, 1, &[0, 1, 2, 3, 4, 5, 6, 0x80]));
        }
        0x800f_60f0 => {
            out.push(k(Ctx, 0x954, 2, &[0, 1, 1, 2, 3, 4]));
            out.push(k(
                Ctx,
                0x3bc,
                1,
                &[0x12, 0x22, 0x27, 0x30, 0x35, 0x36, 0x3a, 5],
            ));
            out.push(k(Ctx, 0x3bd, 1, &[0x38, 0x39, 3]));
            out.push(k(Ctx, 0x957, 1, BOOL));
            out.push(k(Ctx, 0x95f, 1, BOOL));
            out.push(k(Ctx, 0x96c, 4, &[0, 1, 8, 0x10, 0x20, 0x28]));
        }
        0x800f_6848 => {
            out.push(k(Ctx, 0x96c, 4, &[0, 0, 1, 8, 0x10]));
            out.push(k(Ctx, 0x95f, 1, BOOL));
            out.push(k(Ctx, 0x954, 2, &[1, 1, 3, 0]));
            out.push(k(Ctx, 0x957, 1, &[0, 0, 1]));
            out.push(k(Ptr(HUB), 0x1c6, 2, &[0, 1, 2, 3, 4, 5, 6, 7]));
            out.push(k(Ptr(HUB), 0x268, 4, &[0, 1, 2]));
            out.push(k(Ctx, 0x12c, 4, BOOL));
            out.push(k(Ctx, 0x128, 4, &[0, 0x10, 1]));
            out.push(k(Ctx, 0x38c, 2, &[0, 1, 2, 3]));
            out.push(k(Ctx, 0x956, 1, &[0, 1, 2]));
        }
        0x800f_6774 => out.push(k(Arg(1), 0, 4, &[0, 1, 2])),
        0x800f_770c => out.push(k(Arg(0), 0, 4, &[0, 1, 2])),
        0x800f_7c24 => {
            // the CLUT rows (8 in the game; the stack table holds 16)
            out.push(k(Ptr(HUB), 0xde, 2, &[0, 1, 2, 8, 16]));
            out.push(k(Arg(0), 0, 4, &[0, 1, 0xa, 0x19, 0x20, 0x64]));
        }
        0x800f_78b0 => {
            for offset in [0xc1d, 0xc59, 0xc95] {
                out.push(ks(Ctx, offset, 1, 0x37, 1, &[1, 2, 3, 4]));
                out.push(ks(Ctx, offset, 1, 0x37, 1, &[1, 2, 3, 4]));
            }
        }
        0x800f_8c0c | 0x800f_8fa8 | 0x800f_9ec8 | 0x800f_a4e4 | 0x800f_aa48 | 0x800f_a860 => {
            // the faders 0..8 (800DEE84) and the net block's phase and step
            for fader in 0..9 {
                out.push(k(Ptr2(RECORD, 0x58), 2 * fader, 2, &[0, 0, 1, 2, 3]));
            }
            out.push(k(Ptr(NET), 0, 2, &[0, 1, 2, 3, 4, 5]));
            out.push(k(Ptr(NET), 2, 2, &[0, 1, 1, 2, 3]));
            out.push(ks(Ptr(NET), 0x14, 1, 4, 1, &[0, 1, 2, 3]));
            out.push(k(Ptr(NET), 0x118, 1, &[0, 1, 2]));
            out.push(k(Ctx, 0x96c, 4, &[0, 4, 8, 0x10, 0x14, 0x18, 0x1c]));
            out.push(k(Ctx, 0x12c, 4, &[0, 0x10, 0x20, 0x40, 0x1000, 0x4000]));
            out.push(k(Ctx, 0x128, 4, &[0, 0, 0x10, 0x5f]));
            out.push(k(Ptr(HUB), 0x6a, 2, &[0, 1, 2]));
        }
        0x800f_d9cc | 0x800f_de78 | 0x800f_e0d4 | 0x800f_e584 => {
            // the disc-prompt block: state, step, cursor, list size
            out.push(k(Ptr(DISC), 0, 1, &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]));
            out.push(k(Ptr(DISC), 1, 1, &[0, 1, 2]));
            out.push(k(Ptr(DISC), 2, 2, &[0, 1, 2, 3]));
            out.push(k(Ptr(DISC), 4, 1, &[0, 1, 2, 3, 4, 5]));
            out.push(k(Ptr(DISC), 5, 1, &[0, 1, 2, 3]));
            out.push(k(Ptr(DISC), 0x42, 2, &[1, 3, 4, 5, 8, 12]));
            out.push(k(Abs, 0x8011_2dd4, 4, &[0, 0, 1]));
            out.push(k(Ctx, 0x130, 4, &[0, 1, 2, 4, 8, 0x10, 0x20, 0x40]));
            out.push(k(Ctx, 0x12c, 4, &[0, 0x20, 0x40]));
            out.push(k(Ctx, 0x128, 4, &[0, 0, 0x5f]));
            out.push(k(Ptr(HUB), 0x264, 4, &[0, 1, 2, 3]));
        }
        0x800f_e1e8 | 0x800f_ef24 | 0x800f_f0bc | 0x800f_eee0 | 0x800f_f098 => {
            // the disc check: the disc number ([gp+0x34]+0xC, 8002B504), the
            // check block's message, retry flag and mission index, the step
            out.push(k(Ptr(0x800b_cba8), 0xc, 4, &[0, 1, 2, 3, 4, 5]));
            out.push(k(Ptr(CHECK), 0, 4, &[0, 1, 2, 3, 4, 5, 6, 7]));
            out.push(k(Ptr(CHECK), 4, 4, &[0, 1]));
            out.push(k(Ptr(CHECK), 8, 4, &[0, 1, 0x10, 0x20, 0x30, 0x3b]));
            out.push(k(Ptr(DISC), 1, 1, &[0, 1, 2]));
            out.push(k(Ptr(HUB), 0x264, 4, &[0, 1, 2, 3]));
            out.push(k(Ctx, 0x12c, 4, &[0, 0x20, 0x40]));
            out.push(k(Ctx, 0x128, 4, &[0, 0, 0x5f]));
        }
        0x800f_d028 | 0x800f_d408 => {
            // the player state [gp+0x4D0] (step 0..4), the playback block's
            // end and decoded flags, the hub's movie phase
            out.push(k(Ptr(0x800b_d044), 0, 4, &[0, 1, 2, 3, 4, 5]));
            out.push(k(Ptr(0x8011_2e18), 3, 1, &[0, 1]));
            out.push(k(Ptr(0x8011_2e18), 4, 1, &[0, 1]));
            out.push(k(Ptr(HUB), 0x114, 4, &[0, 0x4000, 0x1_0000, 1]));
            out.push(k(Ctx, 0x12c, 4, &[0, 0x10, 0x20, 0x40, 0x800, 0x1000]));
        }
        _ => {}
    }
    out
}

/// Fields many routines of the part branch on (the context's pad words,
/// flags and mission, the faders of `[RECORD]`, the net, disc-prompt and
/// disc-check blocks, the hub's phase words, the arguments): each case
/// sets one to five of them. Not the movie decode state (hub +0x118): a
/// mid-frame state over a stream that is not there sends the resident VLC
/// decoder into an endless loop, in the original as in the translation.
fn common_knobs() -> Vec<Knob> {
    use Base::*;
    const PAD: &[u32] = &[
        0, 1, 2, 4, 8, 0x10, 0x20, 0x40, 0x80, 0x5f, 0x1000, 0x2000, 0x4000, 0x8000,
    ];
    const MISSIONS: &[u32] = &[
        0, 1, 0xd, 0xe, 0x13, 0x14, 0x1c, 0x1d, 0x20, 0x22, 0x23, 0x27, 0x2a, 0x2b, 0x2c, 0x2f,
        0x30, 0x31, 0x35, 0x36, 0x37, 0x3a, 0x3b,
    ];
    const SMALL: &[u32] = &[0, 1, 2, 3];
    const STEPS: &[u32] = &[0, 1, 2, 3, 4, 5, 6, 7, 8];
    vec![
        k(Ctx, 0x96c, 4, &[0, 1, 8, 0x10, 0x11, 0x18, 0x20]),
        k(Ctx, 0x128, 4, PAD),
        k(Ctx, 0x12c, 4, PAD),
        k(Ctx, 0x130, 4, PAD),
        k(Ctx, 0x38c, 2, MISSIONS),
        k(Ctx, 0x956, 1, SMALL),
        k(Ctx, 0x394, 4, SMALL),
        k(Ctx, 0x38e, 2, &[0, 1, 2, 3, 4, 5, 6, 7, 8]),
        ks(Ctx, 0x974, 1, 0x26, 1, &[0, 1]),
        ks(Ctx, 0xc1c, 1, 0x38, 1, &[0, 1, 2, 3, 4, 5]),
        ks(Ctx, 0xc58, 1, 0x38, 1, &[0, 1, 2, 3, 4, 5]),
        ks(Ctx, 0xc94, 1, 0x38, 1, &[0, 1, 2, 3, 4, 5]),
        ks(Ptr2(RECORD, 0x58), 0, 2, 16, 2, SMALL),
        ks(Ptr2(RECORD, 0x50), 0, 2, 16, 2, &[0, 1, 0x10, 0x100]),
        ks(Ptr2(RECORD, 0x5c), 0, 1, 0x40, 1, &[0, 1, 2]),
        k(Ptr(NET), 0, 2, STEPS),
        k(Ptr(NET), 2, 2, &[0, 1, 2, 3, 4]),
        k(Ptr(NET), 0x10, 4, &[0, 1, 2, 3, 4, 8]),
        ks(Ptr(NET), 0x14, 1, 4, 1, SMALL),
        ks(Ptr(NET), 0x118, 1, 3, 1, &[0, 1, 2]),
        ks(Ptr(DISC), 0, 1, 7, 1, &[0, 1, 2, 3, 4, 5, 6]),
        k(Ptr(DISC), 0x42, 2, &[0, 1, 3, 4, 5, 8, 12]),
        k(Ptr(DISC), 2, 2, &[0, 1, 2, 3]),
        k(Ptr(CHECK), 4, 4, &[0, 1, 2]),
        k(Ptr2(CHECK, 8), 0, 1, &[0, 1, 2, 5]),
        k(Ptr(0x800b_cba8), 0xc, 4, &[1, 2, 5]),
        k(Ptr(HUB), 0x264, 4, SMALL),
        k(Ptr(HUB), 0x26c, 4, &[0, 8]),
        k(Ptr(HUB), 0x86, 2, SMALL),
        k(Ptr(HUB), 0x80, 4, SMALL),
        k(Ptr(HUB), 0xc0, 4, SMALL),
        k(Ptr(HUB), 0x1c6, 2, STEPS),
        k(Ptr(HUB), 0x7d, 1, STEPS),
        k(Arg(0), 0, 4, MISSIONS),
        k(
            Arg(1),
            0,
            4,
            &[0, 1, 2, 3, 4, 9, 0xa, 0x12, 0x1a, 0x1f, 0x24, 0x25],
        ),
        k(Arg(2), 0, 4, &[0, 1, 2]),
        k(Arg(3), 0, 4, &[0, 1]),
    ]
}

/// The address of a knob's field in `m` (None: not in RAM).
fn knob_address(m: &mut Machine, knob: &Knob, rng: &mut Rng) -> Option<u32> {
    let base = match knob.base {
        Base::Arg(_) => return None,
        Base::Ctx => m.lw(ac3_flight_jp::GP + ac3_flight_jp::common::CONTEXT),
        Base::Ptr(g) => m.lw(g),
        Base::Abs => 0,
        Base::Ptr2(g, field) => {
            let block = m.lw(g);
            m.lw(block.wrapping_add(field))
        }
    };
    let index = if knob.count > 1 {
        rng.next() % knob.count
    } else {
        0
    };
    let at = base
        .wrapping_add(knob.offset)
        .wrapping_add(index * knob.stride);
    (valid(at) && at % u32::from(knob.width) == 0).then_some(at)
}

fn set_knob(m: &mut Machine, knob: &Knob, args: &mut [u32; 4], rng: &mut Rng) {
    let v = rng.pick(knob.values);
    if let Base::Arg(n) = knob.base {
        args[n] = v;
        return;
    }
    if let Some(at) = knob_address(m, knob, rng) {
        match knob.width {
            1 => m.sb(at, v as u8),
            2 => m.sh(at, v as u16),
            _ => m.sw(at, v),
        }
    }
}

/// Apply each of the routine's knobs with probability 1/2, and one to five
/// of the common ones.
fn turn(m: &mut Machine, knobs: &[Knob], common: &[Knob], args: &mut [u32; 4], rng: &mut Rng) {
    for knob in knobs {
        if rng.next().is_multiple_of(2) {
            set_knob(m, knob, args, rng);
        }
    }
    for _ in 0..1 + rng.next() % 5 {
        let knob = &common[rng.next() as usize % common.len()];
        set_knob(m, knob, args, rng);
    }
}

/// Routines that only make sense in their own captured state: in the
/// other RAM states (or with random fields) they reach resident or SDK
/// callees that never return (the MDEC VLC decoder on a garbage stream,
/// list walks through cycles), in the original as in the translation.
/// They run on their capture only, with more variants.
fn own_only(start: u32) -> bool {
    matches!(
        start,
        0x800f_6848
            | 0x800f_7c24
            | 0x800f_ba30
            | 0x800f_c89c
            | 0x800f_c970
            | 0x800f_cc7c
            | 0x800f_d698
            | 0x800f_c764
            | 0x800f_a4e4
            | 0x800f_aa48
            | 0x800f_d028
            | 0x800f_d408
    )
}
/// Own-state routines whose blocks get only their knobs, no random fields
/// (the playback block feeds the VLC decoder).
fn plain_fields(start: u32) -> bool {
    matches!(start, 0x800f_7c24 | 0x800f_c89c | 0x800f_c970 | 0x800f_cc7c)
}
fn variants(start: u32) -> u32 {
    if own_only(start) {
        4 * VARIANTS
    } else {
        VARIANTS
    }
}

/// The disc-prompt block `[0x80112E1C]` exists only while its screens
/// run (none of the RAM states has it): the routines of those screens get
/// a block of 0x80 cleared bytes at 0x801E0000 (an unused RAM area of the
/// states) for their fields to vary in. Both runs start from it.
fn prepare(m: &mut Machine, start: u32) {
    if (0x800f_d770..0x800f_e668).contains(&start) && !valid(m.lw(DISC)) {
        m.sw(DISC, 0x801e_0000);
        for i in (0..0x80).step_by(4) {
            m.sw(0x801e_0000 + i, 0);
        }
    }
}

/// A RAM state: (name, RAM, scratchpad).
type RamState = (String, Vec<u8>, Vec<u8>);

/// One state to start from.
struct State {
    name: String,
    machine: Machine,
    args: Option<[u32; 4]>,
}

/// One case: Ok((RAM bytes the original changed, whether both runs
/// raised the same fault)) or Err(detail). Two runs that raise the same
/// first fault (a `break` trap of a random divisor, a callee no module
/// claims, an address outside RAM) agree: the machine is dead from there
/// (the original crashes), and the state after the fault is not compared
/// (the oracle stops at its own `break`, the translation runs on).
fn compare(
    base: &Machine,
    (start, end): (u32, u32),
    args: [u32; 4],
    void: bool,
    seen: &mut [bool],
) -> Result<(usize, bool), String> {
    let mut original = base.clone();
    original.device = Some(Box::new(Limit(0, HOST_CALLS)));
    let mut local = vec![false; seen.len()];
    let v0 = panic::catch_unwind(AssertUnwindSafe(|| {
        run(&mut original, start, end, args, &mut local)
    }))
    .map_err(|e| format!("skip: oracle stopped: {}", panic_text(&*e)))?;
    for (s, l) in seen.iter_mut().zip(&local) {
        *s |= *l;
    }
    if std::env::var_os("VERBOSE").is_some() {
        eprintln!("{start:08x}   original done, v0 {v0:#x}");
    }
    let mut native = base.clone();
    native.device = Some(Box::new(Limit(0, HOST_CALLS)));
    let nv0 = panic::catch_unwind(AssertUnwindSafe(|| {
        ac3_flight_jp::dispatch::call(&mut native, start, args)
    }))
    .map_err(|e| format!("native panic: {}", panic_text(&*e)))?;
    let (fo, fnat) = (original.take_fault(), native.take_fault());
    if let (Err(Fault::Untranslated { address }), Ok(())) = (&fo, &fnat)
        && (PART.0..PART.1).contains(address)
    {
        // the oracle calls even the part's own routines through the
        // module-gated dispatch: a case whose random fields made the
        // routine overwrite the loaded-module word stops the oracle at its
        // next call, where the original's `jal` (and the translation's
        // direct call) goes on
        return Err("skip: the case overwrote the loaded module".into());
    }
    if fo != fnat {
        return Err(format!("faults differ: original {fo:?}, native {fnat:?}"));
    }
    let faulted = fo.is_err();
    let ram: Vec<u32> = (0..0x20_0000u32)
        .filter(|i| !STACK.contains(i) && original.ram()[*i as usize] != native.ram()[*i as usize])
        .map(|i| 0x8000_0000 + i)
        .collect();
    let scratch = (0..0x400)
        .filter(|&i| original.scratch()[i] != native.scratch()[i])
        .count();
    let gte = original.gte == native.gte;
    if !ram.is_empty() || scratch != 0 || !gte || original.sp != native.sp {
        if faulted {
            return Ok((0, true));
        }
        return Err(format!(
            "state differs: ram {:08x?} ({} bytes) scratch {scratch} gte equal {gte} sp {:x}/{:x}",
            &ram[..ram.len().min(8)],
            ram.len(),
            original.sp,
            native.sp
        ));
    }
    if !void && v0 != nv0 && !faulted {
        return Err(format!("v0 differs: original {v0:#x}, native {nv0:#x}"));
    }
    Ok((
        (0..0x20_0000usize)
            .filter(|&i| !STACK.contains(&(i as u32)) && base.ram()[i] != original.ram()[i])
            .count(),
        false,
    ))
}

fn panic_text(e: &(dyn std::any::Any + Send)) -> String {
    e.downcast_ref::<String>()
        .cloned()
        .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default()
}

/// A routine's result.
#[derive(Clone, Debug, Default)]
struct Outcome {
    cases: usize,
    faulted: usize,
    budget: bool,
    skipped: usize,
    changed: usize,
    covered: usize,
    words: usize,
    failure: Option<String>,
    /// The instructions the original never executed (ranges).
    missed: Vec<(u32, u32)>,
}

/// Every case of one routine.
fn routine(
    start: u32,
    end: u32,
    void: bool,
    own: Option<Snapshot>,
    common: &[RamState],
) -> Outcome {
    let mut states = Vec::new();
    if let Some(s) = own {
        let mut m = s.machine();
        m.sp = run_sp(s.gpr(29));
        states.push(State {
            name: "capture".into(),
            machine: m,
            args: Some([s.gpr(4), s.gpr(5), s.gpr(6), s.gpr(7)]),
        });
    }
    let common = if own_only(start) && !states.is_empty() {
        &common[..0]
    } else {
        common
    };
    for (name, ram, scratch) in common {
        let mut m = Machine::from_images(ram, scratch).expect("images");
        m.sp = 0x801f_ff00;
        states.push(State {
            name: name.clone(),
            machine: m,
            args: None,
        });
    }
    let words = ((end - start) / 4) as usize;
    let mut seen = vec![false; words];
    let mut out = Outcome {
        words,
        ..Outcome::default()
    };
    let shape = shape(&mut states[0].machine, start, end);
    let knobs = knobs(start);
    let common_knobs = common_knobs();
    let captured = states[0].args;
    let verbose = std::env::var_os("VERBOSE").is_some();
    let started = Instant::now();
    'states: for state in &mut states {
        let mut rng = Rng(start ^ (state.name.len() as u32).wrapping_mul(0x9e37_79b9));
        // pointers: the gp area, the overlay globals the routine reads
        let mut pool: Vec<u32> = (ac3_flight_jp::GP - 0x400..ac3_flight_jp::GP + 0x800)
            .step_by(4)
            .map(|a| state.machine.lw(a))
            .filter(|&v| valid(v) && v % 4 == 0)
            .collect();
        for &g in &shape.globals {
            let p = state.machine.lw(g);
            if valid(p) && p % 4 == 0 {
                pool.push(p);
            }
        }
        pool.sort_unstable();
        pool.dedup();
        for variant in 0..variants(start) {
            if started.elapsed() > BUDGET {
                out.budget = true;
                break 'states;
            }
            let mut base = state.machine.clone();
            // the context pointer and the loaded module stay (the
            // translation's direct calls do not go through the module gate)
            let context_at = ac3_flight_jp::GP + ac3_flight_jp::common::CONTEXT;
            let context = base.lw(context_at);
            let module = base.lw(context.wrapping_add(0x38));
            prepare(&mut base, start);
            let mut args: [u32; 4] = match (variant, state.args.or(captured)) {
                (0, Some(a)) => a,
                (v, Some(a)) if v % 3 == 1 || own_only(start) => {
                    let mut a = a;
                    let k = rng.next() as usize % 4;
                    a[k] = match rng.next() % 3 {
                        0 => rng.next() % 8,
                        1 => a[k].wrapping_add(1),
                        _ => a[k] ^ 1,
                    };
                    a
                }
                _ => std::array::from_fn(|_| match rng.next() % 8 {
                    0..=2 if !pool.is_empty() => rng.pick(&pool),
                    3 | 4 => rng.next() % 4,
                    5 => 0,
                    6 => rng.next() % 0x40,
                    _ => rng.next() % 0x200,
                }),
            };
            if variant > 0 && !shape.offsets.is_empty() && !plain_fields(start) {
                // fields of the blocks the routine reads
                let mut blocks: Vec<u32> = shape
                    .globals
                    .iter()
                    .map(|&g| base.lw(g))
                    .filter(|&p| valid(p))
                    .collect();
                blocks.extend(args.iter().copied().filter(|&p| valid(p)));
                let n = 1 + rng.next() % 6;
                for _ in 0..n {
                    if blocks.is_empty() {
                        break;
                    }
                    let block = rng.pick(&blocks);
                    let (offset, width) = rng.pick(&shape.offsets);
                    let at = block.wrapping_add(offset);
                    if !valid(at) || at % u32::from(width) != 0 {
                        continue;
                    }
                    match width {
                        1 => {
                            let old = u32::from(base.lbu(at));
                            let v = value(&mut rng, 1, old);
                            base.sb(at, v as u8);
                        }
                        2 => {
                            let old = u32::from(base.lhu(at));
                            let v = value(&mut rng, 2, old);
                            base.sh(at, v as u16);
                        }
                        _ => {
                            let old = base.lw(at);
                            let v = value(&mut rng, 4, old);
                            // keep pointer fields pointing into RAM
                            let v = if valid(old) && rng.next().is_multiple_of(2) {
                                rng.pick(&pool)
                            } else {
                                v
                            };
                            base.sw(at, v);
                        }
                    }
                }
                // the global words that are not pointers (flags, counters)
                let plain: Vec<u32> = shape
                    .globals
                    .iter()
                    .copied()
                    .filter(|&g| !valid(base.lw(g)))
                    .collect();
                if !plain.is_empty() && rng.next().is_multiple_of(2) {
                    let g = rng.pick(&plain);
                    let old = base.lw(g);
                    let v = value(&mut rng, 4, old);
                    base.sw(g, v);
                }
                let _ = base.take_fault();
            }
            if variant > 0 && variant % 4 != 3 {
                turn(&mut base, &knobs, &common_knobs, &mut args, &mut rng);
                let _ = base.take_fault();
            }
            base.sw(context_at, context);
            base.sw(context.wrapping_add(0x38), module);
            let _ = base.take_fault();
            if let Ok(case) = std::env::var("CASE")
                && case != format!("{}:{variant}", state.name)
            {
                continue;
            }
            if verbose {
                eprintln!("{start:08x} {} v{variant} args {args:08x?}", state.name);
            }
            match compare(&base, (start, end), args, void, &mut seen) {
                Ok((n, faulted)) => {
                    out.cases += 1;
                    out.faulted += usize::from(faulted);
                    out.changed += n;
                }
                Err(e) if e.starts_with("skip") => out.skipped += 1,
                Err(e) => {
                    out.failure = Some(format!("{} v{variant} args {args:08x?}: {e}", state.name));
                    out.covered = seen.iter().filter(|s| **s).count();
                    return out;
                }
            }
        }
    }
    out.covered = seen.iter().filter(|s| **s).count();
    out.missed = missed(start, &seen);
    out
}

/// The address ranges of the instructions never executed.
fn missed(start: u32, seen: &[bool]) -> Vec<(u32, u32)> {
    let mut out: Vec<(u32, u32)> = Vec::new();
    for (i, s) in seen.iter().enumerate() {
        if *s {
            continue;
        }
        let a = start + 4 * i as u32;
        match out.last_mut() {
            Some(last) if last.1 == a => last.1 = a + 4,
            _ => out.push((a, a + 4)),
        }
    }
    out
}

/// The `sp` a case runs with: the captured one, unless the capture ran on
/// libetc's interrupt stack (an interrupt callback; below the machine's
/// stack region), where the native frames would fault: then the main
/// stack, as the host runs interrupt work (PAL `tests/cdstream.rs`
/// HOST_IRQ_SP).
fn run_sp(sp: u32) -> u32 {
    if sp < ac3_flight_jp::machine::STACK_LIMIT {
        0x801f_f000
    } else {
        sp
    }
}

/// RAM that interrupt handlers, the BIOS and the devices own: the original
/// writes it while a captured call waits in DrawSync/VSync/CD routines and
/// the VBlank/CD/SPU/DMA interrupts run, and the port does not run the BIOS
/// or the handlers' register saves (the PAL `tests/ov3.rs` OWNED list,
/// moved to JP by the data runs of JP_PORT_PLAN §5: +0x2180 in the gp area,
/// +0x2238 and +0x2228 in the BSS; the addresses the JP crate names are
/// cited).
const OWNED: &[(u32, u32, &str)] = &[
    (0x8000_0000, 0x8001_0000, "BIOS kernel RAM"),
    (
        0x800b_46ec,
        0x800b_46f0,
        "VSync flag (libpad PAD_VSYNC_FLAG)",
    ),
    (
        0x800b_5044,
        0x800b_5048,
        "VSync root-counter reading (sdk VSYNC_LAST_HCOUNT)",
    ),
    (
        0x800b_50ac,
        0x800b_60c4,
        "libetc interrupt context and stack (setjmp sp 0x800B60C4)",
    ),
    (
        0x800b_6174,
        0x800b_6178,
        "VSync counter (sdk VSYNC_COUNTER)",
    ),
    (
        0x800b_6190,
        0x800b_6194,
        "GPU DMA callback slot (sdk DMA_CALLBACKS + 8)",
    ),
    (
        0x800b_6268,
        0x800b_626c,
        "libcd drive status byte (CD_STATUS)",
    ),
    (
        0x800b_6538,
        0x800b_653c,
        "libcd read state (CD_SYNC_STATUS)",
    ),
    (
        0x800b_ba8c,
        0x800b_ba94,
        "libgpu queue head/tail (GPU_QUEUE_WRITE/READ)",
    ),
    (
        0x800b_ba94,
        0x800b_ba98,
        "libgpu saved interrupt mask (GPU_SAVED_MASK, a host value)",
    ),
    (
        0x800b_baa0,
        0x800b_baa8,
        "libgpu timeout and DMA poll counter",
    ),
    (0x800c_1388, 0x800c_1408, "CD monitor and driver state"),
    (0x800c_5408, 0x800c_5658, "CD ring state and sector headers"),
    (0x800c_7df4, 0x800c_7df6, "sound driver tick state"),
    (
        0x800c_7e98,
        0x800c_7e9a,
        "sound driver tick state (libpad WAIT_START)",
    ),
    (
        0x800c_7ec0,
        0x800c_7ec8,
        "libcd last result bytes (CD_SYNC_RESULT)",
    ),
    (
        0x800c_a310,
        0x800c_a314,
        "ClearImage draw mode (GPU status; GPU_CLEAR_PACKET + 8)",
    ),
    (
        0x800c_a31c,
        0x800c_a320,
        "ClearImage draw mode (GPU status; GPU_CLEAR_PACKET + 0x14)",
    ),
    (
        0x800c_a334,
        0x800c_a340,
        "ClearImage drawing area (GPU status; GPU_CLEAR_RESTORE + 4)",
    ),
    (0x800c_a458, 0x800c_bc58, "libgpu queue entries (GPU_QUEUE)"),
];

fn owned(address: u32) -> bool {
    OWNED.iter().any(|(a, b, _)| (*a..*b).contains(&address))
}

/// The device of a capture replay: it records the RAM ranges whose content
/// is a device value, from each call's arguments at the time of the call:
/// StoreImage (0x800A6970, `[rect, pixels]`: the VRAM rectangle's pixels)
/// and the MDEC output DMA (0x800AB118, `[buffer, words]`: decoded pixels).
/// Without the capture's VRAM and MDEC the translation leaves those bytes
/// alone. With `pending`, the MDEC output DMA is not complete when
/// DecDCTout returns: the MDEC callback 800FCB44 starts the next slice's
/// DMA, whose completion interrupt calls it again only after it returned
/// (the host model completes it at once unless the device says otherwise).
struct Recorder {
    pending: bool,
    ranges: Rc<RefCell<Vec<(u32, u32)>>>,
}
impl Device for Recorder {
    fn call(&mut self, ram: &mut [u8], _: &mut [u8], call: HostCall) -> Option<u32> {
        let half = |a: u32| {
            let i = (a & 0x1f_ffff) as usize;
            u32::from(u16::from_le_bytes([ram[i], ram[(i + 1) & 0x1f_ffff]]))
        };
        match call.function {
            0x800a_6970 => {
                let [rect, pixels, ..] = call.args;
                let (w, h) = (half(rect.wrapping_add(4)), half(rect.wrapping_add(6)));
                self.ranges
                    .borrow_mut()
                    .push((pixels, pixels.wrapping_add(w * h * 2)));
                None
            }
            0x800a_b118 => {
                let [buffer, words, ..] = call.args;
                self.ranges
                    .borrow_mut()
                    .push((buffer, buffer.wrapping_add(words.wrapping_mul(4))));
                self.pending.then_some(0)
            }
            _ => None,
        }
    }
}

/// Captures that could not match while a callee outside the part was not
/// the JP routine. 80077880 is now patched (jp-res2) and 800AD398
/// translated (jp-sdk); 800FC970 and 800FABE8 pass. 800FC488 still differs
/// in `v0` only: `scene_camera` returns SetGeomScreen's `v0`, a leftover
/// register in the original and a host-call 0 natively. The oracle
/// validates these routines meanwhile.
const BLOCKED: &[(u32, &str)] = &[
    (0x800f_abe8, "800FC488 -> 80077880 (now patched)"),
    (0x800f_c488, "v0 after SetGeomScreen (leftover register)"),
];

/// The part's captures: callee -> (directory, entry, exit).
fn captures() -> BTreeMap<u32, (PathBuf, SnapshotInfo, SnapshotInfo)> {
    let mut out = BTreeMap::new();
    let Ok(runs) = std::fs::read_dir(redux()) else {
        return out;
    };
    let mut runs: Vec<PathBuf> = runs.flatten().map(|r| r.path()).collect();
    runs.sort();
    for run in runs {
        let index = run.join("snapshots-module2.jsonl");
        let Ok(list) = snapshot::read_index(&index) else {
            continue;
        };
        for entry in list.iter().filter(|s| s.stage == "entry") {
            if !(PART.0..PART.1).contains(&entry.callee) || out.contains_key(&entry.callee) {
                continue;
            }
            let exit = list.iter().find(|s| {
                s.callee == entry.callee
                    && s.stage == "return"
                    && s.sequence > entry.sequence
                    && s.ra == entry.ra
                    && s.registers.gpr[29] == entry.registers.gpr[29]
            });
            if let Some(exit) = exit {
                out.insert(entry.callee, (run.clone(), entry.clone(), exit.clone()));
            }
        }
    }
    out
}

#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn part5_captures_replay() {
    let (claimed, void) = claims();
    let caps = captures();
    let _fallback = ac3_fallback::install(Fallback::new(Version::Jp));
    let hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let (mut pass, mut fail, mut blocked) = (Vec::new(), Vec::new(), Vec::new());
    for (callee, (dir, entry, exit)) in &caps {
        if !claimed.contains(callee) {
            continue;
        }
        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            let entry = Snapshot::load(dir, entry).map_err(|e| e.to_string())?;
            let exit = Snapshot::load(dir, exit).map_err(|e| e.to_string())?;
            let mut m = entry.machine();
            m.sp = run_sp(entry.gpr(29));
            let ranges = Rc::new(RefCell::new(Vec::new()));
            m.device = Some(Box::new(Recorder {
                pending: *callee == 0x800f_cb44,
                ranges: ranges.clone(),
            }));
            let args = [entry.gpr(4), entry.gpr(5), entry.gpr(6), entry.gpr(7)];
            let v0 = ac3_flight_jp::dispatch::call(&mut m, *callee, args);
            m.take_fault().map_err(|f| format!("fault {f}"))?;
            let devices = ranges.borrow().clone();
            let mut d = snapshot::compare(&m, &exit);
            let excluded = |a: u32| owned(a) || devices.iter().any(|(s, e)| (*s..*e).contains(&a));
            let kernel = d.ram.iter().filter(|&&a| excluded(a)).count();
            d.ram.retain(|&a| !excluded(a));
            if !d.is_empty() {
                let detail: Vec<String> = d
                    .ram
                    .iter()
                    .take(64)
                    .map(|&a| {
                        let i = (a & 0x1f_ffff) as usize;
                        format!(
                            "{a:08x}:{:02x}/{:02x}/{:02x}",
                            entry.ram[i],
                            m.ram()[i],
                            exit.ram[i]
                        )
                    })
                    .collect();
                return Err(format!(
                    "state differs: {} (address:entry/native/original {})",
                    d.summary(),
                    detail.join(" ")
                ));
            }
            if !void.contains(callee) && v0 != exit.gpr(2) {
                return Err(format!("v0 {v0:#x}, original {:#x}", exit.gpr(2)));
            }
            Ok((snapshot::changed_bytes(&entry, &exit), kernel))
        }))
        .unwrap_or_else(|e| Err(format!("panic: {}", panic_text(&*e))));
        if let Some((_, why)) = BLOCKED.iter().find(|(a, _)| a == callee) {
            match &result {
                Ok(_) => pass.push(format!("{callee:08x} (passes; listed as blocked: {why})")),
                Err(e) => blocked.push(format!("{callee:08x}: {why}: {e}")),
            }
            continue;
        }
        match result {
            Ok((n, 0)) => pass.push(format!("{callee:08x} ({n} bytes changed)")),
            Ok((n, kernel)) => pass.push(format!(
                "{callee:08x} ({n} bytes changed; {kernel} handler/device bytes excluded)"
            )),
            Err(e) => fail.push(format!("{callee:08x}: {e}")),
        }
    }
    panic::set_hook(hook);
    println!(
        "part 5 captures: {} pass, {} fail, {} blocked by a callee outside the part",
        pass.len(),
        fail.len(),
        blocked.len()
    );
    for p in &pass {
        println!("  pass {p}");
    }
    for b in &blocked {
        println!("  BLOCKED {b}");
    }
    for f in &fail {
        println!("  FAIL {f}");
    }
    assert!(!pass.is_empty(), "no capture replayed");
    assert!(fail.is_empty(), "captures missed: {fail:#?}");
}

#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux RAM states)"]
fn part5_matches_the_original() {
    let (claimed, void) = claims();
    let only: Option<BTreeSet<u32>> =
        std::env::var("ONLY")
            .ok()
            .filter(|s| !s.is_empty())
            .map(|s| {
                s.split(',')
                    .filter_map(|x| u32::from_str_radix(x.trim().trim_start_matches("0x"), 16).ok())
                    .collect()
            });
    let routines: Vec<(u32, u32, String)> = part()
        .into_iter()
        .filter(|(a, _, _)| claimed.contains(a))
        .filter(|(a, _, _)| only.as_ref().is_none_or(|o| o.contains(a)))
        .collect();
    let caps = Arc::new(captures());
    let root = redux();
    let common: Arc<Vec<RamState>> = Arc::new(
        STATES
            .iter()
            .filter_map(|stem| {
                let ram = std::fs::read(root.join(format!("{stem}.ram.bin"))).ok()?;
                let scratch = std::fs::read(root.join(format!("{stem}.scratch.bin")))
                    .unwrap_or(vec![0; 0x400]);
                Some((stem.to_string(), ram, scratch))
            })
            .collect(),
    );
    assert!(!common.is_empty(), "no RAM states under {}", root.display());
    let hook = Arc::new(panic::take_hook());
    {
        let hook = hook.clone();
        panic::set_hook(Box::new(move |info| {
            if std::thread::current().name() != Some(WORKER) {
                hook(info);
            }
        }));
    }
    let (tx, rx) = mpsc::channel::<(usize, Outcome)>();
    let mut results: Vec<Option<Outcome>> = vec![None; routines.len()];
    let mut active: BTreeMap<usize, Instant> = BTreeMap::new();
    let (mut next, mut done) = (0, 0);
    while done < routines.len() {
        while active.len() < WORKERS && next < routines.len() {
            let (start, end, _) = routines[next].clone();
            let (tx, caps, common) = (tx.clone(), caps.clone(), common.clone());
            let is_void = void.contains(&start);
            let index = next;
            std::thread::Builder::new()
                .name(WORKER.into())
                .stack_size(64 << 20)
                .spawn(move || {
                    // a lower step limit: interpreted callees that wait forever on a
                    // foreign state stop with the RUNAWAY trap in both runs
                    let mut fallback = Fallback::new(Version::Jp);
                    fallback.set_max_steps(FALLBACK_STEPS);
                    let _fallback = ac3_fallback::install(fallback);
                    let own = caps
                        .get(&start)
                        .and_then(|(dir, entry, _)| Snapshot::load(dir, entry).ok());
                    let outcome = routine(start, end, is_void, own, &common);
                    let _ = tx.send((index, outcome));
                })
                .expect("worker");
            active.insert(next, Instant::now());
            next += 1;
        }
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok((index, outcome)) => {
                if active.remove(&index).is_some() {
                    results[index] = Some(outcome);
                    done += 1;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let late: Vec<usize> = active
                    .iter()
                    .filter(|(_, t)| t.elapsed() > TIMEOUT)
                    .map(|(i, _)| *i)
                    .collect();
                for index in late {
                    active.remove(&index);
                    results[index] = Some(Outcome {
                        failure: Some("timeout".into()),
                        ..Outcome::default()
                    });
                    done += 1;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let _ = panic::take_hook();
    let mut report = String::from(
        "jp_address\tclass\tcases\tfaulted\tskipped\tcovered\twords\tram_bytes\tresult\n",
    );
    let (mut pass, mut failures, mut cases) = (0, Vec::new(), 0);
    for ((start, _, class), outcome) in routines.iter().zip(&results) {
        let o = outcome.clone().unwrap_or_default();
        cases += o.cases;
        let result = match &o.failure {
            None if o.budget => {
                pass += 1;
                "pass (time budget reached)".to_string()
            }
            None => {
                pass += 1;
                "pass".to_string()
            }
            Some(f) => {
                failures.push(format!("{start:08x}: {f}"));
                f.clone()
            }
        };
        let _ = writeln!(
            report,
            "{start:08x}\t{class}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            o.cases,
            o.faulted,
            o.skipped,
            o.covered,
            o.words,
            o.changed,
            result.replace(['\t', '\n'], " ")
        );
        println!(
            "{start:08x} {class:8} {:4} cases ({:3} faulted alike, {:3} skipped), {:4}/{:4} instructions ({:3}%), {:8} RAM bytes: {}",
            o.cases,
            o.faulted,
            o.skipped,
            o.covered,
            o.words,
            (100 * o.covered).checked_div(o.words).unwrap_or(0),
            o.changed,
            result
        );
        if !o.missed.is_empty() && std::env::var_os("MISSED").is_some() {
            let ranges: Vec<String> = o
                .missed
                .iter()
                .map(|(a, b)| format!("{a:08x}..{b:08x}"))
                .collect();
            println!("    not executed: {}", ranges.join(" "));
        }
    }
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join("jp-ov2-p5.tsv");
    std::fs::write(&path, &report).expect("report");
    println!(
        "part 5 oracle: {} routines, {pass} pass, {} fail, {cases} cases; per-routine results in {}",
        routines.len(),
        failures.len(),
        path.display()
    );
    assert!(failures.is_empty(), "{failures:#?}");
}
