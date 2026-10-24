//! JP overlay 3, part 1 (the credits, 8011B328..8011E854,
//! `src/jp/ov3_p1*.rs`) against the JP originals, in two tests:
//!
//! * [`captured_calls_reproduce`]: every captured call of the part
//!   (`snapshots-module3.jsonl`: the Redux ends run `ends7-002`, a cold boot
//!   whose first state selection is substituted by state 7, CAPTURES.md
//!   section 11; the first two calls of each routine and later calls
//!   sampled over the whole credits, to their end) runs natively from its
//!   entry snapshot and must reproduce the return snapshot, or pass as
//!   device work with the proof of `support::proof`;
//! * [`part1_matches_the_original`]: the oracle, below.
//!
//! The oracle: every routine of the part runs from overlay-3 machine states twice, as
//! the original instructions (a copy of the test-only oracle
//! `support::oracle::run` that also records which instructions ran) and as
//! the translation (`dispatch::call`); RAM outside the stack window,
//! scratchpad, GTE, `sp`, the first fault, the host-call log and `v0` must
//! agree. A case whose two runs raise the same first fault (a `break` of a
//! random divisor, an address outside RAM) is counted as faulting alike;
//! the state after the fault is not compared (the oracle stops there).
//!
//! **States.** The RAM dump of the run that entered state 7 (`run1/probe-3`:
//! overlay 3 loaded, the first callback about to run), two dumps of the
//! credits running in the Redux ends run (`ends7-001`: the layered sequence
//! starting, field 2100, and its middle, field 9000), and three module-2
//! RAM dumps (hub, save, after a mission) with the overlay image
//! (`extracted/disc1/archives/bpb_0003.bin`) written at 0x800CBFE8 and the
//! loaded-module word (context +0x38) set to 3. Each in four stages, made
//! by running the translation (the stages are inputs only; every routine
//! is compared on all of them): `raw`; `started` (state 7 substate 0,
//! 8011BB7C, on buffer 1: the credits block); `playing` (the player's
//! phase 0: the decoder block and the stream request); `streaming` (a
//! synthetic CD stream at 0x801C0000 whose next chunk has random header
//! bytes and data, so that 8002B9EC hands the chunk over, and the layered
//! sequence on).
//!
//! **Variants.** Arguments (the routine's own pointers from the blocks,
//! pointers of the gp area, small numbers) and fields: words, halfwords and
//! bytes at the offsets the routine's own loads use in the blocks it reads,
//! and per routine the fields that select its paths (phases, frame and
//! picture counters, effects, the reticle's keys and trail, the list's
//! first/last lines and scroll time, the chunk header).
//!
//! The report lists per routine the cases, the instructions of the
//! original executed over all cases, and the ones never executed (with
//! `MISSED=1`). Results: `$CARGO_TARGET_TMPDIR/jp-ov3-p1.tsv`.
//!
//! `cargo test --release -p ac3-flight-jp --features dev-fallback --test
//! ov3_p1 -- --include-ignored --nocapture`. Data: `AC3_JP_DIR` (default
//! `~/ac3-jp`).
#![cfg(feature = "dev-fallback")]
mod support;

use ac3_fallback::{Fallback, Version};
use ac3_flight_jp::snapshot::{self, Snapshot, SnapshotInfo};
use ac3_flight_jp::{Device, Fault, HostCall, Machine};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};
use support::{proof, scan};

/// The overlay and its image.
const MODULE: u32 = 3;
const REGION: &str = "ov3";
const OVERLAY_BASE: u32 = 0x800c_bfe8;
const IMAGE: &str = "extracted/disc1/archives/bpb_0003.bin";
const REPORT: &str = "jp-ov3-p1.tsv";
/// The part's files.
const FILES: &str = "jp/ov3_p1";
/// Variants per state.
const VARIANTS: u32 = 24;
/// Host calls a case may make before it counts as a runaway.
const HOST_CALLS: usize = 20_000;
/// The stack window (not compared).
const STACK: std::ops::Range<u32> = 0x1f_c000..0x20_0000;
/// Per-routine time limit and the budget after which no case starts.
const TIMEOUT: Duration = Duration::from_secs(400);
const BUDGET: Duration = Duration::from_secs(200);
/// Instruction limits of the routine under test and of interpreted callees.
const ORACLE_STEPS: u32 = 5_000_000;
const FALLBACK_STEPS: u64 = 3_000_000;
const WORKERS: usize = 6;
const WORKER: &str = "ov3-p1-worker";
/// Guest `sp` of a case.
const SP: u32 = 0x801f_ff00;
/// The RAM dumps (under `AC3_JP_DIR/analysis/redux`): state 7 entered,
/// the credits running (the ends run), the hub, the save screen, the hub
/// after a mission.
const DUMPS: [&str; 6] = [
    "run1/probe-3/ram",
    "ends7-001/ram-002100",
    "ends7-001/ram-009000",
    "story-001/ram-030300",
    "save-001/ram-054250",
    "flight-001/ram-053900",
];

/// The overlay's globals: the block pointers whose fields vary.
const BLOCK: u32 = 0x800b_d050;
const DECODER: u32 = 0x800b_d054;
const GLOBALS: [u32; 2] = [BLOCK, DECODER];
/// `[gp+0x2EC]`, the context pointer.
const CONTEXT_AT: u32 = 0x800b_ce60;
/// The synthetic stream: the stream state `[gp+0x38]`, the stream
/// `[gp+0x40]`, the chunk header and its data.
const STREAM_STATE: u32 = 0x801c_0000;
const STREAM: u32 = 0x801c_0100;
const CHUNK: u32 = 0x801c_0200;
const DATA: u32 = 0x801c_1000;
const DATA_BYTES: u32 = 0x5400;

fn redux() -> PathBuf {
    scan::jp_dir().join("analysis/redux")
}

/// The part's routines: (start, end, class).
fn part() -> Vec<(u32, u32, String)> {
    scan::jp_map(&scan::jp_dir())
        .into_iter()
        .filter(|r| r.region == REGION)
        .map(|r| (r.jp, r.jp + r.size, r.class))
        .collect()
}

/// The claims of the part's files and the void ones.
fn claims() -> (BTreeSet<u32>, BTreeSet<u32>) {
    let (claims, void) =
        scan::claims_and_void(&scan::crate_dir(), &|name| !name.starts_with(FILES));
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

/// The immediate offsets and widths of a routine's loads (not from `sp`
/// or `gp`).
fn load_offsets(m: &mut Machine, start: u32, end: u32) -> Vec<(u32, u8)> {
    let mut offsets = BTreeSet::new();
    for pc in (start..end).step_by(4) {
        let w = m.lw(pc);
        let rs = (w >> 21) & 31;
        let simm = (w & 0xffff) as u16 as i16 as i32 as u32;
        let width = match w >> 26 {
            0x20 | 0x24 => 1,
            0x21 | 0x25 => 2,
            0x23 => 4,
            _ => 0,
        };
        if width != 0 && rs != 29 && rs != 28 && simm < 0x2000 {
            offsets.insert((simm, width));
        }
    }
    offsets.into_iter().collect()
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
    /// The runtime context.
    Ctx,
    /// The block a global pointer word points at.
    Ptr(u32),
    /// An absolute address (the offset).
    Abs,
    /// Argument register a0..a3 (the offset is ignored).
    Arg(usize),
}

/// A field (or one of `count` fields `stride` apart) and its values.
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

const BOOL: &[u32] = &[0, 1];
const SMALL: &[u32] = &[0, 1, 2, 3];
/// The decoder's frame counter: the thresholds of the credits frame.
const FRAMES: &[u32] = &[
    0,
    1,
    0x7f,
    0x80,
    0xb3,
    0xb4,
    0x133,
    0x134,
    0xc7,
    0xc8,
    0xc9,
    0x107,
    0x108,
    0x3cf,
    0x3d0,
    0x3d1,
    0x40f,
    0x410,
    0xffff_ffff,
    0x8000_0000,
];
/// The reticle's times: before, at and inside the key range, the trail
/// points, the fade start and the end.
const RETICLE_TIMES: &[u32] = &[
    0x30fb, 0x30fc, 0x30fd, 0x3160, 0x31c4, 0x3516, 0x3517, 0x357a, 0x3abf, 0x3ac0, 0x3ac1, 0x3b24,
    0x3b88,
];

/// The path-selecting fields of a routine.
fn knobs(start: u32) -> Vec<Knob> {
    use Base::*;
    let reticle = |offset: u32| offset + 0x6c;
    let list = |offset: u32| offset + 0x354;
    let mut out = Vec::new();
    match start {
        0x8011_b39c => {
            // the records' tpage cases (the overlay's data the routine reads)
            out.push(ks(
                Abs,
                0x8011_9fcc + 8,
                2,
                0x153,
                14,
                &[0, 1, 5, 9, 12, 13, 0xffff],
            ));
            out.push(ks(
                Abs,
                0x8011_9fcc + 8,
                2,
                0x153,
                14,
                &[0, 3, 6, 7, 11, 0x8000],
            ));
        }
        0x8011_bb7c => out.push(k(Ctx, 4, 4, &[0, 1, 1])),
        0x8011_e744 | 0x8011_bd94 => {
            out.push(k(Ptr(BLOCK), 0, 4, &[0, 1, 2, 3, 4, 5, 6, 7]));
            out.push(k(Ctx, 4, 4, &[0, 1, 1]));
            out.push(k(Ctx, 0x12c, 4, &[0, 0x40]));
            out.push(k(Ptr(DECODER), 0xa0, 1, BOOL));
        }
        0x8011_c2f0 => out.push(k(Arg(0), 0, 4, &[0, 1, 1])),
        0x8011_cb5c => {
            out.push(k(Ptr(DECODER), 0, 1, &[0, 1, 2, 2]));
            out.push(k(Ptr(DECODER), 1, 1, &[0, 1, 1]));
            out.push(k(Ptr(DECODER), 4, 4, FRAMES));
            // the copyright picture's windows (frames 0xC8.. and after 0x40F)
            out.push(k(
                Ptr(DECODER),
                4,
                4,
                &[
                    0xc8,
                    0xd0,
                    0x107,
                    0x108,
                    0x3d0,
                    0x40f,
                    0x410,
                    0x500,
                    0xffff_ffff,
                ],
            ));
            out.push(k(
                Ptr(DECODER),
                8,
                2,
                &[0, 0, 1, 0x16, 0x17, 0x18, 0x17, 0x18],
            ));
            out.push(k(Ptr(BLOCK), 0xde0, 4, &[0, 0xbd, 0xbd, 0xbe, 0x200]));
        }
        0x8011_d474 | 0x8011_d59c => {
            out.push(k(Ptr(DECODER), 0xa, 1, &[0, 1, 4, 4, 5, 5, 6]));
            out.push(k(Ptr(DECODER), 4, 4, &[0, 1, 2, 0x10, 0x20]));
            out.push(k(
                Ptr(DECODER),
                0xc,
                4,
                &[0, 1, 0x10, 0x20, 0x100, 0x100_0000, 0x100_0000, 0xff00_0000],
            ));
            out.push(k(Ptr(DECODER), 0x10, 1, &[0, 1, 4]));
            out.push(k(Ptr(DECODER), 0x9f, 1, BOOL));
            out.push(k(Ptr(DECODER), 8, 2, &[0, 0x17, 0x17]));
            // the effect slots (+0x12 + 0x46 bank + 0xE slot): start, end,
            // and the fourth word
            for offset in [0x14u32, 0x16, 0x1e] {
                out.push(ks(
                    Ptr(DECODER),
                    offset,
                    2,
                    10,
                    0xe,
                    &[0, 1, 2, 0x10, 0x20, 0xffff, 0xffff, 0x8000],
                ));
            }
            out.push(ks(Ptr(DECODER), 0x12, 1, 10, 0xe, &[0, 1, 4, 5, 7]));
            // bank 0's slot 0: a span of -1 (with the time 0x1000000 the
            // division overflows)
            out.push(k(Ptr(DECODER), 0x14, 2, &[0, 0x10]));
            out.push(k(Ptr(DECODER), 0x16, 2, &[0xffff, 0xf, 0x20]));
            out.push(k(Ptr(DECODER), 0xc, 4, &[0x100_0000, 0x300_0000]));
        }
        0x8011_d8f4 => {
            out.push(k(Ptr(BLOCK), reticle(0x10), 4, &[0, 0, 1, 2]));
            out.push(k(Ptr(BLOCK), reticle(0x2e4), 4, RETICLE_TIMES));
            out.push(k(Ptr(BLOCK), reticle(0x2e4), 4, RETICLE_TIMES));
            out.push(k(Ptr(BLOCK), reticle(0x2d8), 1, &[0, 1, 1, 2]));
            out.push(k(Ptr(BLOCK), reticle(0x2d0), 4, &[0, 1, 2, 3, 5, 9]));
            out.push(k(Ptr(BLOCK), reticle(0x2d4), 4, &[0, 1, 2, 4, 6]));
            out.push(k(
                Ptr(BLOCK),
                reticle(0x2dc),
                4,
                &[0, 0x3000, 0x3500, 0x3a00],
            ));
            out.push(k(Ptr(BLOCK), reticle(4), 2, &[0x10, 0xfff0, 0x11]));
            out.push(k(Ptr(BLOCK), reticle(6), 2, &[0x18, 0xffe7]));
            out.push(k(Ptr(BLOCK), reticle(0x2dc), 4, &[0x3098, 0x30fc, 0x2f40]));
            // keys 1 and 2 read the frame's stale words (the frame is at
            // SP - 0x20 when the case calls the routine): +0xC (key 1's
            // end and x), +0x14 (key 1's y, key 2's end), +0x18 (key 2's y)
            out.push(k(
                Abs,
                SP - 0x20 + 0xc,
                4,
                &[
                    0x0136,
                    0x0135,
                    0x4000_0136,
                    0x7fff_7fff,
                    0x0000_3b00,
                    0x8000_0136,
                ],
            ));
            out.push(k(
                Abs,
                SP - 0x20 + 0x14,
                4,
                &[0x00b4, 0x00b3, 0x7fff_0000, 0x3b00],
            ));
            out.push(k(Abs, SP - 0x20 + 0x18, 4, &[0, 0x8000_7fff, 0x1234_5678]));
        }
        0x8011_e474 => {
            out.push(k(
                Ptr(BLOCK),
                list(0x1532),
                2,
                &[0, 0, 1, 5, 0x150, 0x151, 0x152, 0xffff],
            ));
            out.push(k(
                Ptr(BLOCK),
                list(0x1534),
                2,
                &[0, 3, 0x20, 0x151, 0x152, 0xffff],
            ));
            out.push(k(Ptr(BLOCK), list(0x1538), 4, &[0, 1, 2, 0xffff_ffff]));
            out.push(ks(
                Ptr(BLOCK),
                list(0x54c),
                4,
                0x153,
                4,
                &[
                    0xffff_ff00,
                    0xffff_ffcf,
                    0xffff_ffd0,
                    0,
                    0x1f4,
                    0x1f5,
                    0x300,
                ],
            ));
            out.push(ks(
                Ptr(BLOCK),
                list(0x54c),
                4,
                8,
                4,
                &[0xffff_ff00, 0xffff_ffce, 0x100, 0x1f4, 0x1f5],
            ));
        }
        _ => {}
    }
    out
}

/// Fields many routines branch on.
fn common_knobs() -> Vec<Knob> {
    use Base::*;
    vec![
        k(Ctx, 4, 4, BOOL),
        k(Ctx, 0x12c, 4, &[0, 0x40, 0x10]),
        k(Ptr(BLOCK), 0, 4, &[0, 1, 2, 3, 4, 5]),
        k(Ptr(DECODER), 0, 1, &[0, 1, 2]),
        k(Ptr(DECODER), 1, 1, BOOL),
        k(Ptr(DECODER), 4, 4, FRAMES),
        k(Ptr(DECODER), 8, 2, &[0, 0x16, 0x17]),
        k(Ptr(DECODER), 0xa, 1, &[0, 1, 4, 5, 6]),
        k(Ptr(DECODER), 0x9e, 1, BOOL),
        k(Ptr(DECODER), 0x9f, 1, BOOL),
        k(Ptr(DECODER), 0xa0, 1, BOOL),
        k(Ptr(BLOCK), 0xde0, 4, &[0, 0xbe]),
        k(Arg(0), 0, 4, SMALL),
    ]
}

/// The address of a knob's field in `m` (None: not in RAM).
fn knob_address(m: &mut Machine, knob: &Knob, rng: &mut Rng) -> Option<u32> {
    let base = match knob.base {
        Base::Arg(_) => return None,
        Base::Ctx => m.lw(CONTEXT_AT),
        Base::Ptr(g) => m.lw(g),
        Base::Abs => 0,
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

/// Each routine knob with probability 1/2 (list knobs several times), and
/// one to three common ones.
fn turn(m: &mut Machine, knobs: &[Knob], common: &[Knob], args: &mut [u32; 4], rng: &mut Rng) {
    for knob in knobs {
        let times = if knob.count > 1 {
            1 + rng.next() % 24
        } else {
            1
        };
        for _ in 0..times {
            if rng.next().is_multiple_of(2) {
                set_knob(m, knob, args, rng);
            }
        }
    }
    for _ in 0..1 + rng.next() % 3 {
        let knob = &common[rng.next() as usize % common.len()];
        set_knob(m, knob, args, rng);
    }
}

/// The routine's own arguments in a state (from the blocks).
fn own_args(m: &mut Machine, start: u32) -> [u32; 4] {
    let b = m.lw(BLOCK);
    let d = m.lw(DECODER);
    let (packets, ot) = if valid(b) {
        (m.lw(b + 0x3c), m.lw(b + 0x40))
    } else {
        (0, 0)
    };
    match start {
        0x8011_b328 => [b.wrapping_add(0x6c), 0, 0, 0],
        0x8011_b39c => [b.wrapping_add(0x354), 0, 0, 0],
        0x8011_c05c => {
            let file = if valid(b) { m.lw(b + 0x58) } else { 0 };
            [file, 0, 0x8015_0000, 0xd740]
        }
        0x8011_c100 => [0x801c_0300, 0, 0, 0],
        0x8011_c2f0 => [1, 0, 0, 0],
        0x8011_c39c => [d, 0, 0, 0],
        0x8011_d474 | 0x8011_d59c => [d, packets, ot, 0],
        0x8011_d8f4 => [b.wrapping_add(0x6c), ot, packets, 0],
        0x8011_e474 => [b.wrapping_add(0x354), ot, packets, 0],
        _ => [0; 4],
    }
}

/// Install the synthetic stream: its next chunk (kind 2, one part) has a
/// random header and random data.
fn install_stream(m: &mut Machine, rng: &mut Rng) {
    m.sw(ac3_flight_jp::GP + 0x38, STREAM_STATE);
    m.sw(ac3_flight_jp::GP + 0x40, STREAM);
    for i in (0..0x40).step_by(4) {
        m.sw(STREAM_STATE + i, 0);
    }
    m.sw(STREAM_STATE + 0x20, 0x40);
    let words = [
        (0x00, 0x10),
        (0x04, 0x400),
        (0x0c, 0x40),
        (0x10, CHUNK),
        (0x14, DATA),
        (0x34, 1 + rng.next() % 3),
        (0x38, 0),
        (0x3c, CHUNK),
        (0x40, DATA),
    ];
    for (offset, value) in words {
        m.sw(STREAM + offset, value);
    }
    header(m, rng);
    for i in (0..DATA_BYTES).step_by(4) {
        let v = rng.next() ^ (rng.next() << 8);
        m.sw(DATA + i, v);
    }
}

/// A random chunk header at CHUNK (+0 kind 2, +2 one part).
fn header(m: &mut Machine, rng: &mut Rng) {
    for i in (0..0x40).step_by(4) {
        let v = rng.next() ^ (rng.next() << 8);
        m.sw(CHUNK + i, v);
    }
    m.sb(CHUNK, 2);
    m.sb(CHUNK + 2, 1);
    let first = rng.next() % 12;
    let page = match rng.next() % 6 {
        0 => 0,
        1 => first + 7,
        2 => 2 + rng.next() % 16,
        3 => 1 + rng.next() % 20,
        4 => 0xff,
        _ => rng.next() % 0x20,
    };
    m.sb(CHUNK + 0xf, page as u8);
    let flags = rng.next() % 16;
    m.sb(CHUNK + 0x13, ((flags << 4) | first) as u8);
    let kind = rng.pick(&[0u32, 0, 1, 3]);
    let step = rng.pick(&[0u32, 1, 1, 2]);
    m.sb(CHUNK + 0xc, ((step << 4) | kind) as u8);
    m.sb(CHUNK + 0xd, rng.pick(&[0u32, 1, 1, 2]) as u8);
    let slot = rng.next() % 5;
    let record = if rng.next().is_multiple_of(2) { 1 } else { 0 };
    m.sb(CHUNK + 0x10, ((slot << 4) | record) as u8);
    m.sh(CHUNK + 6, rng.pick(&[0u32, 1, 5]) as u16);
}

/// Arguments the routine takes as an index: none.
fn fix_args(_: u32, _: &mut [u32; 4]) {}

/// Per variant: a new chunk header in the streaming stage.
fn refresh(m: &mut Machine, state: &str, rng: &mut Rng) {
    if state.ends_with(":streaming") {
        header(m, rng);
    }
}

/// A RAM image: (name, RAM, scratchpad).
type RamState = (String, Vec<u8>, Vec<u8>);

/// One state to start from (images: machines are not `Send`).
#[derive(Clone)]
struct State {
    name: String,
    ram: Vec<u8>,
    scratch: Vec<u8>,
}
impl State {
    fn new(name: String, m: &Machine) -> Self {
        Self {
            name,
            ram: m.ram().to_vec(),
            scratch: m.scratch().to_vec(),
        }
    }
    fn machine(&self) -> Machine {
        let mut m = Machine::from_images(&self.ram, &self.scratch).expect("images");
        m.sp = SP;
        m
    }
}

fn context_module(m: &mut Machine) -> (u32, u32) {
    let context = m.lw(CONTEXT_AT);
    let module = m.lw(context.wrapping_add(0x38));
    (context, module)
}

/// The four stages of a RAM image (see the module doc).
fn stages(name: &str, ram: &[u8], scratch: &[u8], image: &[u8]) -> Vec<State> {
    let mut m = Machine::from_images(ram, scratch).expect("images");
    m.sp = SP;
    m.write_bytes(OVERLAY_BASE, image);
    let context = m.lw(CONTEXT_AT);
    m.sw(context + 0x38, MODULE);
    let mut out = vec![State::new(format!("{name}:raw"), &m)];
    m.device = Some(Box::new(Limit(0, HOST_CALLS)));
    m.sw(context + 4, 1);
    ac3_flight_jp::dispatch::call(&mut m, 0x8011_bb7c, [0; 4]);
    let _ = m.take_fault();
    m.host_calls.clear();
    m.sp = SP;
    out.push(State::new(format!("{name}:started"), &m));
    m.device = Some(Box::new(Limit(0, HOST_CALLS)));
    ac3_flight_jp::dispatch::call(&mut m, 0x8011_bd94, [0; 4]);
    let _ = m.take_fault();
    m.host_calls.clear();
    m.sp = SP;
    out.push(State::new(format!("{name}:playing"), &m));
    let mut rng = Rng(name.len() as u32 * 0x9e37);
    install_stream(&mut m, &mut rng);
    let d = m.lw(DECODER);
    if valid(d) {
        m.sb(d + 1, 1);
    }
    out.push(State::new(format!("{name}:streaming"), &m));
    out
}

/// One case: Ok((RAM bytes the original changed, whether both runs
/// faulted alike)) or Err(detail).
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
    let code = (start & 0x1f_ffff) as usize..(end & 0x1f_ffff) as usize;
    if original.ram()[code.clone()] != base.ram()[code] {
        // random pointers made the original store into its own code: the
        // oracle runs the changed instructions, the translation cannot
        return Err("skip: the case overwrote the routine's code".into());
    }
    for (s, l) in seen.iter_mut().zip(&local) {
        *s |= *l;
    }
    let mut native = base.clone();
    native.device = Some(Box::new(Limit(0, HOST_CALLS)));
    let nv0 = panic::catch_unwind(AssertUnwindSafe(|| {
        ac3_flight_jp::dispatch::call(&mut native, start, args)
    }))
    .map_err(|e| format!("native panic: {}", panic_text(&*e)))?;
    let (fo, fnat) = (original.take_fault(), native.take_fault());
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
    let hosts = original.host_calls == native.host_calls;
    if !ram.is_empty() || scratch != 0 || !gte || original.sp != native.sp || !hosts {
        if faulted {
            return Ok((0, true));
        }
        let detail: Vec<String> = ram
            .iter()
            .take(8)
            .map(|&a| {
                let i = (a & 0x1f_ffff) as usize;
                format!(
                    "{a:08x}:{:02x}/{:02x}/{:02x}",
                    base.ram()[i],
                    original.ram()[i],
                    native.ram()[i]
                )
            })
            .collect();
        return Err(format!(
            "state differs: ram (address:before/original/native) {} ({} bytes) scratch {scratch} gte equal {gte} sp {:x}/{:x} host calls equal {hosts} ({}/{})",
            detail.join(" "),
            ram.len(),
            original.sp,
            native.sp,
            original.host_calls.len(),
            native.host_calls.len()
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
    missed: Vec<(u32, u32)>,
    /// The first skipped case's reason.
    skip: Option<String>,
}

/// Variants per state of a routine: more for the routines with many
/// paths.
fn variants(start: u32) -> u32 {
    match start {
        0x8011_c39c | 0x8011_cb5c | 0x8011_d474 | 0x8011_d59c | 0x8011_d8f4 | 0x8011_e474 => {
            6 * VARIANTS
        }
        _ => VARIANTS,
    }
}

/// Every case of one routine.
fn routine(start: u32, end: u32, void: bool, states: &[State]) -> Outcome {
    let words = ((end - start) / 4) as usize;
    let mut seen = vec![false; words];
    let mut out = Outcome {
        words,
        ..Outcome::default()
    };
    let mut probe = states[0].machine();
    let offsets = load_offsets(&mut probe, start, end);
    let knobs = knobs(start);
    let common = common_knobs();
    let started = Instant::now();
    'states: for state in states {
        let machine = state.machine();
        let mut rng = Rng(start ^ (state.name.len() as u32).wrapping_mul(0x9e37_79b9));
        let mut pool: Vec<u32> = (ac3_flight_jp::GP - 0x400..ac3_flight_jp::GP + 0x800)
            .step_by(4)
            .map(|a| machine.peek(a))
            .filter(|&v| valid(v) && v % 4 == 0)
            .collect();
        for g in GLOBALS {
            let p = machine.peek(g);
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
            let mut base = machine.clone();
            let (context, module) = context_module(&mut base);
            let mut args = own_args(&mut base, start);
            if variant % 4 == 3 {
                args = std::array::from_fn(|_| match rng.next() % 8 {
                    0..=2 if !pool.is_empty() => rng.pick(&pool),
                    3 | 4 => rng.next() % 4,
                    5 => 0,
                    6 => rng.next() % 0x40,
                    _ => rng.next() % 0x200,
                });
            }
            if variant > 0 && !offsets.is_empty() {
                let mut blocks: Vec<u32> = GLOBALS
                    .iter()
                    .map(|&g| base.lw(g))
                    .filter(|&p| valid(p))
                    .collect();
                blocks.extend(args.iter().copied().filter(|&p| valid(p)));
                for _ in 0..1 + rng.next() % 6 {
                    if blocks.is_empty() {
                        break;
                    }
                    let block = rng.pick(&blocks);
                    let (offset, width) = rng.pick(&offsets);
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
                            let v = if valid(old) && rng.next().is_multiple_of(2) {
                                rng.pick(&pool)
                            } else {
                                v
                            };
                            base.sw(at, v);
                        }
                    }
                }
                let _ = base.take_fault();
            }
            if variant > 0 && variant % 4 != 2 {
                turn(&mut base, &knobs, &common, &mut args, &mut rng);
                let _ = base.take_fault();
            }
            if variant > 0 {
                refresh(&mut base, &state.name, &mut rng);
            }
            fix_args(start, &mut args);
            base.sw(CONTEXT_AT, context);
            base.sw(context.wrapping_add(0x38), module);
            let _ = base.take_fault();
            base.host_calls.clear();
            base.sp = SP;
            if let Ok(case) = std::env::var("CASE")
                && case != format!("{}:{variant}", state.name)
            {
                continue;
            }
            if std::env::var_os("VERBOSE").is_some() {
                eprintln!("{start:08x} {} v{variant} args {args:08x?}", state.name);
            }
            match compare(&base, (start, end), args, void, &mut seen) {
                Ok((n, faulted)) => {
                    out.cases += 1;
                    out.faulted += usize::from(faulted);
                    out.changed += n;
                }
                Err(e) if e.starts_with("skip") => {
                    out.skipped += 1;
                    if out.skip.is_none() {
                        out.skip = Some(format!("{} v{variant}: {e}", state.name));
                    }
                }
                Err(e) => {
                    out.failure = Some(format!("{} v{variant} args {args:08x?}: {e}", state.name));
                    out.covered = seen.iter().filter(|s| **s).count();
                    out.missed = missed(start, &seen);
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

fn load_dumps() -> Vec<RamState> {
    let root = redux();
    DUMPS
        .iter()
        .filter_map(|stem| {
            let ram = std::fs::read(root.join(format!("{stem}.ram.bin")))
                .or_else(|_| std::fs::read(root.join(format!("{stem}.bin"))))
                .ok()?;
            let scratch =
                std::fs::read(root.join(format!("{stem}.scratch.bin"))).unwrap_or(vec![0; 0x400]);
            Some((stem.to_string(), ram, scratch))
        })
        .collect()
}

#[test]
#[ignore = "requires AC3_JP_DIR (the JP RAM dumps and the overlay image)"]
fn part1_matches_the_original() {
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
    let all = part();
    let unclaimed: Vec<String> = all
        .iter()
        .filter(|(a, _, _)| !claimed.contains(a))
        .map(|(a, _, _)| format!("{a:08x}"))
        .collect();
    assert!(unclaimed.is_empty(), "routines not claimed: {unclaimed:?}");
    let routines: Vec<(u32, u32, String)> = all
        .into_iter()
        .filter(|(a, _, _)| only.as_ref().is_none_or(|o| o.contains(a)))
        .collect();
    let image = std::fs::read(scan::jp_dir().join(IMAGE)).expect("overlay image");
    let dumps = load_dumps();
    assert!(
        !dumps.is_empty(),
        "no RAM dumps under {}",
        redux().display()
    );
    let hook = Arc::new(panic::take_hook());
    {
        let hook = hook.clone();
        panic::set_hook(Box::new(move |info| {
            if std::thread::current().name() != Some(WORKER) {
                hook(info);
            }
        }));
    }
    // the stages are made on a worker thread too (they run translations
    // that may call the fallback)
    let states: Arc<Vec<State>> = {
        let (dumps, image) = (dumps.clone(), image.clone());
        Arc::new(
            std::thread::Builder::new()
                .name(WORKER.into())
                .stack_size(64 << 20)
                .spawn(move || {
                    let _fallback = ac3_fallback::install(Fallback::new(Version::Jp));
                    dumps
                        .iter()
                        .flat_map(|(name, ram, scratch)| stages(name, ram, scratch, &image))
                        .collect::<Vec<State>>()
                })
                .expect("stages")
                .join()
                .expect("stages"),
        )
    };
    let (tx, rx) = mpsc::channel::<(usize, Outcome)>();
    let mut results: Vec<Option<Outcome>> = vec![None; routines.len()];
    let mut active: BTreeMap<usize, Instant> = BTreeMap::new();
    let (mut next, mut done) = (0, 0);
    while done < routines.len() {
        while active.len() < WORKERS && next < routines.len() {
            let (start, end, _) = routines[next].clone();
            let (tx, states) = (tx.clone(), states.clone());
            let is_void = void.contains(&start);
            let index = next;
            std::thread::Builder::new()
                .name(WORKER.into())
                .stack_size(64 << 20)
                .spawn(move || {
                    let mut fallback = Fallback::new(Version::Jp);
                    fallback.set_max_steps(FALLBACK_STEPS);
                    let _fallback = ac3_fallback::install(fallback);
                    let outcome = routine(start, end, is_void, &states);
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
    report(&routines, &results);
}

fn report(routines: &[(u32, u32, String)], results: &[Option<Outcome>]) {
    let mut text = String::from(
        "jp_address\tclass\tcases\tfaulted\tskipped\tcovered\twords\tram_bytes\tresult\tnot_executed\n",
    );
    let (mut pass, mut failures, mut cases) = (0, Vec::new(), 0);
    for ((start, _, class), outcome) in routines.iter().zip(results) {
        let o = outcome.clone().unwrap_or_default();
        cases += o.cases;
        let result = match &o.failure {
            None if o.cases == 0 => {
                failures.push(format!("{start:08x}: no case ran"));
                "no case ran".to_string()
            }
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
        let ranges: Vec<String> = o
            .missed
            .iter()
            .map(|(a, b)| format!("{a:08x}..{b:08x}"))
            .collect();
        let _ = writeln!(
            text,
            "{start:08x}\t{class}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            o.cases,
            o.faulted,
            o.skipped,
            o.covered,
            o.words,
            o.changed,
            result.replace(['\t', '\n'], " "),
            ranges.join(" ")
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
        if !ranges.is_empty() && std::env::var_os("MISSED").is_some() {
            println!("    not executed: {}", ranges.join(" "));
        }
        if let Some(skip) = &o.skip {
            println!("    first skip: {skip}");
        }
    }
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(REPORT);
    std::fs::write(&path, &text).expect("report");
    println!(
        "overlay {MODULE} part 1 oracle: {} routines, {pass} pass, {} fail, {cases} cases; per-routine results in {}",
        routines.len(),
        failures.len(),
        path.display()
    );
    assert!(failures.is_empty(), "{failures:#?}");
}

// ---- the captured calls ------------------------------------------------------------

/// The capture root (`AC3_JP_CAPTURES`, else `AC3_JP_DIR/analysis/redux`).
fn capture_root() -> PathBuf {
    std::env::var_os("AC3_JP_CAPTURES")
        .map(PathBuf::from)
        .unwrap_or_else(redux)
}

/// The return snapshot of a captured entry (the first later return of the
/// same callee with the same `ra` and `sp`, as `snapshot::check_call`).
fn exit_of<'a>(index: &'a [SnapshotInfo], entry: &SnapshotInfo) -> Option<&'a SnapshotInfo> {
    index.iter().find(|s| {
        s.callee == entry.callee
            && s.stage == "return"
            && s.sequence > entry.sequence
            && s.ra == entry.ra
            && s.registers.gpr[29] == entry.registers.gpr[29]
    })
}

/// Replay one captured call natively from its entry snapshot
/// (`proof::replay_machine`): Ok(bytes the original changed) or Err(cause).
/// RAM the BIOS, the handlers and the devices own (`proof::OWNED`) and the
/// destinations of device transfers (`proof::device_ranges`) are left out,
/// as in the global `tests/captures.rs`.
fn replay(
    directory: &Path,
    entry: &SnapshotInfo,
    exit: &SnapshotInfo,
    void: bool,
) -> Result<usize, String> {
    let entry = Snapshot::load(directory, entry).map_err(|e| format!("load: {e}"))?;
    let exit = Snapshot::load(directory, exit).map_err(|e| format!("load: {e}"))?;
    let mut m = proof::replay_machine(&entry, &exit, MODULE);
    let args = [entry.gpr(4), entry.gpr(5), entry.gpr(6), entry.gpr(7)];
    let v0 = panic::catch_unwind(AssertUnwindSafe(|| {
        ac3_flight_jp::dispatch::call(&mut m, entry.info.callee, args)
    }))
    .map_err(|e| format!("panic: {}", panic_text(&*e)))?;
    if let Err(fault) = m.take_fault() {
        return Err(format!("fault: {fault}"));
    }
    let mut d = snapshot::compare(&m, &exit);
    let stored = proof::device_ranges(&m);
    d.ram
        .retain(|&a| !proof::owned(a) && !stored.iter().any(|r| r.contains(&(a & 0x1f_ffff))));
    if !d.is_empty() {
        let mut ranges: Vec<(u32, u32)> = Vec::new();
        for &a in &d.ram {
            match ranges.last_mut() {
                Some((_, end)) if a <= *end + 16 => *end = a,
                _ => ranges.push((a, a)),
            }
        }
        let text: Vec<String> = ranges
            .iter()
            .take(24)
            .map(|(a, b)| format!("{a:08x}-{b:08x}"))
            .collect();
        return Err(format!(
            "state differs: {} ranges {}",
            d.summary(),
            text.join(" ")
        ));
    }
    if !void && v0 != exit.gpr(2) {
        return Err(format!(
            "v0 differs: {v0:#010x}, original {:#010x}",
            exit.gpr(2)
        ));
    }
    Ok(snapshot::changed_bytes(&entry, &exit))
}

/// The device-work proof (`proof::device_work`) of a call its replay does
/// not reproduce: from the same entry and device the translation equals
/// the original instructions, it reached a device, and `proof::DEVICE_WORK`
/// or `proof::OWNED` explains the difference.
fn device_work(
    directory: &Path,
    entry: &SnapshotInfo,
    exit: &SnapshotInfo,
    end: u32,
    void: bool,
) -> Result<String, String> {
    let entry = Snapshot::load(directory, entry).map_err(|e| format!("load: {e}"))?;
    let exit = Snapshot::load(directory, exit).map_err(|e| format!("load: {e}"))?;
    let base = proof::replay_machine(&entry, &exit, MODULE);
    let call = proof::Call {
        entry: &entry,
        exit: &exit,
        base: &base,
        module: MODULE,
        range: (entry.info.callee, end),
        void,
    };
    proof::device_work(&call, &|| {
        Box::new(proof::Replay::new(&entry, &exit, MODULE))
    })
}

/// Every captured call of the part's routines (`snapshots-module<MODULE>.jsonl`
/// of every run under the capture root: the Redux ends runs, CAPTURES.md
/// section 11) runs natively from its entry snapshot and must reproduce the
/// return snapshot: RAM outside the stack, scratchpad, GTE, and `v0` unless
/// the translation models none; or pass as device work with the proof of
/// `support::proof`. Untranslated callees would run interpreted (the
/// development fallback; none is left). `ONLY=<address,...>` limits it.
#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn captured_calls_reproduce() {
    let (claimed, void) = claims();
    assert!(!claimed.is_empty(), "no claims in {FILES}*");
    let ends: BTreeMap<u32, u32> = part().into_iter().map(|(a, e, _)| (a, e)).collect();
    let _fallback = ac3_fallback::install(Fallback::new(Version::Jp));
    let mut runs: Vec<PathBuf> = std::fs::read_dir(capture_root())
        .expect("capture root")
        .flatten()
        .map(|e| e.path().join(format!("snapshots-module{MODULE}.jsonl")))
        .filter(|p| p.is_file())
        .collect();
    runs.sort();
    assert!(
        !runs.is_empty(),
        "no snapshots-module{MODULE}.jsonl under {}",
        capture_root().display()
    );
    let only: Option<BTreeSet<u32>> =
        std::env::var("ONLY")
            .ok()
            .filter(|s| !s.is_empty())
            .map(|s| {
                s.split(',')
                    .filter_map(|x| u32::from_str_radix(x.trim().trim_start_matches("0x"), 16).ok())
                    .collect()
            });
    let hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let mut passed: BTreeMap<u32, usize> = BTreeMap::new();
    let (mut failed, mut open, mut bytes, mut device) = (Vec::new(), 0usize, 0usize, 0usize);
    for path in &runs {
        let directory = path.parent().unwrap();
        let run = directory
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let index = snapshot::read_index(path).unwrap();
        for entry in index.iter().filter(|s| s.stage == "entry") {
            let callee = entry.callee;
            if !claimed.contains(&callee) || only.as_ref().is_some_and(|o| !o.contains(&callee)) {
                continue;
            }
            let Some(exit) = exit_of(&index, entry) else {
                open += 1;
                continue;
            };
            let is_void = void.contains(&callee);
            let e = match replay(directory, entry, exit, is_void) {
                Ok(n) => {
                    bytes += n;
                    *passed.entry(callee).or_default() += 1;
                    println!(
                        "pass {run} #{} {callee:08x} (field {}): {n} bytes changed",
                        entry.sequence, entry.frame
                    );
                    continue;
                }
                Err(e) => e,
            };
            let proof = if e.starts_with("state differs") || e.starts_with("v0 differs") {
                device_work(directory, entry, exit, ends[&callee], is_void)
            } else {
                Err("not a state or v0 difference".into())
            };
            match proof {
                Ok(why) => {
                    device += 1;
                    *passed.entry(callee).or_default() += 1;
                    println!(
                        "pass (device work) {run} #{} {callee:08x}: {e}; {why}",
                        entry.sequence
                    );
                }
                Err(not) => {
                    let text = format!(
                        "{run} #{} {callee:08x} (field {}): {e}; not device work: {not}",
                        entry.sequence, entry.frame
                    );
                    println!("FAIL {text}");
                    failed.push(text);
                }
            }
        }
    }
    panic::set_hook(hook);
    let uncaptured: Vec<String> = claimed
        .iter()
        .filter(|a| only.as_ref().is_none_or(|o| o.contains(a)))
        .filter(|a| {
            !passed.contains_key(a) && !failed.iter().any(|f| f.contains(&format!("{a:08x}")))
        })
        .map(|a| format!("{a:08x}"))
        .collect();
    let calls: usize = passed.values().sum();
    println!(
        "overlay {MODULE} part 1 captures: {calls} calls of {} routines pass ({bytes} original \
         bytes changed; {device} as device work), {} failures, {open} entries without a return; \
         {} claimed routines have no capture: {}",
        passed.len(),
        failed.len(),
        uncaptured.len(),
        uncaptured.join(" ")
    );
    assert!(failed.is_empty(), "{failed:#?}");
    assert!(calls > 0, "no captured call ran");
}
