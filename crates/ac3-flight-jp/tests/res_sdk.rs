//! The JP resident routines of `src/jp/res_sdk*.rs` and the overlay-2
//! stubs of `src/jp/ov2_stubs.rs` against the JP originals.
//!
//! 1. **Captures.** GetClut (800AC708) and SetDrawLoad (800AC788) have a JP
//!    Redux capture (`story-002/snapshots-module0.jsonl`); each runs natively
//!    from its entry snapshot and must reproduce the return snapshot
//!    (`snapshot::check_call`: RAM outside the stack, scratchpad, GTE, `v0`).
//!    The global `tests/captures.rs` replays them too. No capture reaches the
//!    others.
//! 2. **Oracle.** Every routine but the two BIOS-level ones runs from JP RAM
//!    states (boot, story, save and flight runs) twice: as the original
//!    instructions (a copy of the test-only oracle `support::oracle::run`
//!    that interprets a set of ranges and records which instructions ran)
//!    and as the translation (`dispatch::call`). RAM outside the stack
//!    window, scratchpad, GTE, `sp`, the first fault, the host calls and
//!    `v0` (unless the routine leaves it) must agree. vsprintf and the
//!    formatter run with the formatter's helpers 80027C24/80027C3C/80027E80
//!    interpreted too, and CdFlush with CD_flush, so the generated helpers
//!    are checked with them. Arguments and the data they point at are
//!    randomised per case: TIM images and the stack words a failed ReadTIM
//!    leaves, formats from a grammar of every flag, width, precision, size
//!    and conversion with fitting arguments, string pairs, RECTs and
//!    packets, the CD monitor, queue and driver words, sound slots, the
//!    radio indicator, libgpu's debug level and DMA callback slot. The
//!    report lists, per routine, the cases, the cases where both runs raised
//!    the same fault, and the original's instructions executed over all
//!    cases; the test fails when an instruction outside the documented
//!    unreachable ones never ran.
//! 3. **BIOS level.** Krom2RawAdd (800AF438, `jr 0xB0`) and PadStopCom
//!    (8009CD50, kernel calls) cannot run in the oracle: their translations
//!    are checked directly.
//!
//! Calls out of the interpreted ranges run natively in both runs
//! (`dispatch::call`). No case needs the development fallback.
mod support;

use ac3_flight_jp::cdstream::{CLOCK, CdDrive, Disc, SECTOR_BYTES};
use ac3_flight_jp::common::loaded_module;
use ac3_flight_jp::jp::{res_sdk, res_sdk_format, res_sdk_game};
use ac3_flight_jp::{Device, Fault, HostCall, Machine, host_ids, snapshot};
use std::collections::BTreeSet;
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use support::scan;

/// Guest `sp` at entry.
const SP: u32 = 0x801f_ff00;
/// The stack window (not compared: natives keep locals in Rust).
const STACK: std::ops::Range<u32> = 0x1f_c000..0x20_0000;
/// A RAM area the cases build their data in (strings, TIMs, RECTs).
const DATA: u32 = 0x801e_0000;
/// Variants per RAM state.
const VARIANTS: u32 = 48;
/// Instructions the oracle may run (the routine and its interpreted
/// ranges).
const ORACLE_STEPS: u32 = 5_000_000;
/// RAM states under `AC3_JP_DIR/analysis/redux`: title (module 1), story
/// hub and briefing (module 2), save, flight.
const STATES: [&str; 7] = [
    "boot-001/ram-005110",
    "boot-001/ram-010310",
    "story-001/ram-017800",
    "story-001/ram-040610",
    "save-001/ram-054250",
    "flight-001/ram-050200",
    "flight-001/ram-053900",
];

/// 800BB966: libgpu's debug level byte.
const GPU_DEBUG: u32 = 0x800b_b966;
/// 800B6190: the DMA channel 2 callback slot (DMACallback's table + 8).
const DMA2_CALLBACK: u32 = 0x800b_6190;
/// 800BBA78 / 800BBA6C: libgpu's pointers to the DMA channel 2 control
/// register and to GPUSTAT, which LoadImage2's wait polls.
const DMA2_CHCR_POINTER: u32 = 0x800b_ba78;
const GPUSTAT_POINTER: u32 = 0x800b_ba6c;
/// 800B6520 / 800B6528 / 800B652C / 800B6530: libcd's pointers to the CD
/// index, interrupt-enable and interrupt-flag registers and COM_DELAY,
/// which CD_flush writes and polls.
const CD_REGISTER_POINTERS: [u32; 4] = [0x800b_6520, 0x800b_6528, 0x800b_652c, 0x800b_6530];
/// RAM words standing for those device registers in the oracle (excluded
/// from the comparison: the translation leaves device registers alone).
const DEVICE_WORDS: (u32, u32) = (DATA + 0x1f00, DATA + 0x1f20);

fn redux() -> PathBuf {
    std::env::var_os("AC3_JP_CAPTURES")
        .map(PathBuf::from)
        .unwrap_or_else(|| scan::jp_dir().join("analysis/redux"))
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
    fn chance(&mut self, n: u32) -> bool {
        self.next().is_multiple_of(n)
    }
}

const RETURN: u32 = 0xffff_fff0;

/// `support::oracle::run` over several ranges: the original instructions
/// of every range in `ranges` are interpreted from `entry` (calls between
/// them stay interpreted), calls anywhere else run natively; the executed
/// addresses go to `seen`.
fn run(
    m: &mut Machine,
    entry: u32,
    ranges: &[(u32, u32)],
    args: [u32; 4],
    seen: &mut BTreeSet<u32>,
) -> u32 {
    let inside = |pc: u32| ranges.iter().any(|&(s, e)| (s..e).contains(&pc));
    let mut r = [0u32; 32];
    let (mut hi, mut lo) = (0u32, 0u32);
    r[4..8].copy_from_slice(&args);
    r[28] = ac3_flight_jp::GP;
    r[29] = m.sp;
    r[31] = RETURN;
    let (mut pc, mut next) = (entry, entry + 4);
    let mut steps = 0u32;
    loop {
        if pc == RETURN {
            m.sp = r[29];
            return r[2];
        }
        if !inside(pc) {
            m.sp = r[29];
            let wide = |hi: u32, lo: u32| (u64::from(hi) << 32) | u64::from(lo);
            match pc {
                // libgcc's 64-bit divisions return v1:v0; `dispatch` carries
                // only v0, so run them natively with both result words.
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
        seen.insert(pc);
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
            o => panic!("op {o:#x} at {cur:08x}"),
        }
        if let Some((i, v)) = out
            && i != 0
        {
            r[i] = v;
        }
    }
}

fn panic_text(e: &(dyn std::any::Any + Send)) -> String {
    e.downcast_ref::<String>()
        .cloned()
        .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default()
}

fn calls(m: &Machine) -> Vec<(u32, [u32; 4])> {
    m.host_calls
        .iter()
        .map(|&HostCall { function, args }| (function, args))
        .collect()
}

/// A disc without sectors, for the drive model.
struct NoDisc;
impl Disc for NoDisc {
    fn sector(&mut self, _: u32) -> Option<[u8; SECTOR_BYTES]> {
        None
    }
}

/// A routine under test.
struct Routine {
    entry: u32,
    /// 0 for a resident routine, else its overlay (the cases run in the RAM
    /// states that have it loaded).
    module: u32,
    name: &'static str,
    /// Map routines that are pieces of this one (the JP map splits
    /// LoadImage2 at its wait loop's join and strcmp before its shared
    /// `jr ra`): interpreted and covered with it.
    parts: &'static [u32],
    /// Callees the oracle interprets too (map routines by start).
    with: &'static [u32],
    /// The routine leaves the caller's `v0` (not compared).
    void: bool,
    /// The cases need the host CD drive (CdControl).
    drive: bool,
    /// Instructions the cases cannot reach, with the reason.
    unreachable: &'static [(u32, u32)],
    why: &'static str,
}

/// One case from `base`: Ok((RAM bytes the original changed, whether both
/// runs raised the same fault)) or Err(detail). Two runs that raise the
/// same first fault agree; the state after it is not compared (the oracle
/// stops at its own `break`, the translation runs on).
fn compare(
    base: &Machine,
    routine: &Routine,
    ranges: &[(u32, u32)],
    args: [u32; 4],
    seen: &mut BTreeSet<u32>,
) -> Result<(usize, bool), String> {
    let device = || -> Option<Box<dyn Device>> {
        routine
            .drive
            .then(|| Box::new(CdDrive::new(NoDisc, CLOCK)) as Box<dyn Device>)
    };
    let mut original = base.clone();
    original.device = device();
    let v0 = panic::catch_unwind(AssertUnwindSafe(|| {
        run(&mut original, routine.entry, ranges, args, seen)
    }))
    .map_err(|e| format!("oracle stopped: {}", panic_text(&*e)))?;
    let mut native = base.clone();
    native.device = device();
    let nv0 = panic::catch_unwind(AssertUnwindSafe(|| {
        ac3_flight_jp::dispatch::call(&mut native, routine.entry, args)
    }))
    .map_err(|e| format!("native panic: {}", panic_text(&*e)))?;
    let (fo, fnat) = (original.take_fault(), native.take_fault());
    if fo != fnat {
        return Err(format!("faults differ: original {fo:?}, native {fnat:?}"));
    }
    if fo.is_err() {
        return Ok((0, true));
    }
    let compared = |i: u32| {
        !STACK.contains(&i)
            && !(DEVICE_WORDS.0 & 0x1f_ffff..DEVICE_WORDS.1 & 0x1f_ffff).contains(&i)
    };
    let ram: Vec<u32> = (0..0x20_0000u32)
        .filter(|&i| compared(i) && original.ram()[i as usize] != native.ram()[i as usize])
        .map(|i| 0x8000_0000 + i)
        .collect();
    let scratch = (0..0x400)
        .filter(|&i| original.scratch()[i] != native.scratch()[i])
        .count();
    let gte = original.gte == native.gte;
    if !ram.is_empty() || scratch != 0 || !gte || original.sp != native.sp {
        return Err(format!(
            "state differs: ram {:08x?} ({} bytes) scratch {scratch} gte equal {gte} sp {:x}/{:x}",
            &ram[..ram.len().min(8)],
            ram.len(),
            original.sp,
            native.sp
        ));
    }
    if calls(&original) != calls(&native) {
        return Err(format!(
            "host calls differ: original {:08x?}, native {:08x?}",
            calls(&original),
            calls(&native)
        ));
    }
    if !routine.void && v0 != nv0 {
        return Err(format!("v0 differs: original {v0:#x}, native {nv0:#x}"));
    }
    Ok((
        (0..0x20_0000u32)
            .filter(|&i| compared(i) && base.ram()[i as usize] != original.ram()[i as usize])
            .count(),
        false,
    ))
}

/// A RAM state: (name, machine with `sp` = [`SP`]).
fn states() -> Vec<(String, Machine)> {
    let root = redux();
    let out: Vec<(String, Machine)> = STATES
        .iter()
        .filter_map(|stem| {
            let ram = std::fs::read(root.join(format!("{stem}.ram.bin"))).ok()?;
            let scratch =
                std::fs::read(root.join(format!("{stem}.scratch.bin"))).unwrap_or(vec![0; 0x400]);
            let mut m = Machine::from_images(&ram, &scratch)?;
            m.sp = SP;
            Some((stem.to_string(), m))
        })
        .collect();
    assert!(!out.is_empty(), "no RAM states under {}", root.display());
    out
}

/// A routine's extent from the JP map (start, end): resident (module 0)
/// or of overlay `module`.
fn extent(rows: &[scan::Row], module: u32, start: u32) -> (u32, u32) {
    let region = format!("ov{module}");
    let row = rows
        .iter()
        .find(|r| {
            r.jp == start
                && if module == 0 {
                    r.region.starts_with("resident")
                } else {
                    r.region == region
                }
        })
        .unwrap_or_else(|| panic!("{start:08x} is not a JP function of module {module}"));
    (start, start + row.size)
}

/// Bytes into RAM (no fault: `at` is inside RAM).
fn put(m: &mut Machine, at: u32, bytes: &[u8]) {
    for (i, &b) in bytes.iter().enumerate() {
        m.sb(at + i as u32, b);
    }
}

/// A random NUL-terminated string of up to `max` characters at `at`.
fn put_string(m: &mut Machine, rng: &mut Rng, at: u32, max: u32) -> Vec<u8> {
    let length = rng.next() % (max + 1);
    let mut text: Vec<u8> = (0..length)
        .map(|_| match rng.next() % 4 {
            0 => 0x80 | (rng.next() as u8 & 0x7f),
            _ => b'a' + (rng.next() % 26) as u8,
        })
        .collect();
    text.push(0);
    put(m, at, &text);
    text
}

/// A pointer for a field: often into the data area, sometimes into the
/// state's gp area pointers, unaligned, null or outside RAM.
fn pointer(rng: &mut Rng, m: &mut Machine) -> u32 {
    match rng.next() % 10 {
        0 => 0,
        1 => 0x1f80_1000 + (rng.next() % 0x100) * 4,
        2 => DATA + rng.next() % 0x1000,
        3 => {
            let at = ac3_flight_jp::GP - 0x400 + (rng.next() % 0x400) * 4;
            m.lw(at)
        }
        _ => DATA + (rng.next() % 0x400) * 4,
    }
}

/// A RECT size halfword: small, at the packet limits, negative, huge.
fn size_half(rng: &mut Rng) -> u16 {
    rng.pick(&[
        0u16, 1, 2, 3, 4, 5, 6, 7, 8, 23, 24, 25, 26, 0x10, 0x40, 0x100, 0x3ff, 0x400, 0x401,
        0xffff, 0xfffe, 0x7fff, 0x8000,
    ])
}

/// Random words for a case's register arguments.
fn random_args(rng: &mut Rng) -> [u32; 4] {
    std::array::from_fn(|_| match rng.next() % 4 {
        0 => rng.next() % 0x400,
        1 => 0,
        _ => rng.next().wrapping_mul(0x9e37_79b9),
    })
}

/// A value for a word field: small, at a boundary, a single bit, random.
fn word(rng: &mut Rng) -> u32 {
    match rng.next() % 8 {
        0 => 0,
        1 => 1,
        2 => u32::MAX,
        3 => rng.next() % 16,
        4 => 1 << (rng.next() % 32),
        _ => rng.next().wrapping_mul(0x9e37_79b9),
    }
}

/// A format of literal text and conversions from every flag, width,
/// precision, size and conversion character the formatter knows (and some
/// it does not), at `format`, with arguments that fit it at `list`
/// (strings for `%s`, counters for `%n`, two words for `L`). Widths and
/// precisions stay small.
fn put_format(m: &mut Machine, rng: &mut Rng, format: u32, list: u32) {
    let strings = DATA + 0x1000;
    let counters = DATA + 0x1c00;
    let mut text = Vec::new();
    let mut arguments: Vec<u32> = Vec::new();
    let small = |rng: &mut Rng| match rng.next() % 6 {
        0 => 0,
        1 => 0u32.wrapping_sub(1 + rng.next() % 12),
        _ => rng.next() % 24,
    };
    for piece in 0..1 + rng.next() % 5 {
        for _ in 0..rng.next() % 4 {
            text.push(rng.pick(b"Can't 12 [%]\t:-"));
            if text.last() == Some(&b'%') {
                text.push(b'%');
            }
        }
        text.push(b'%');
        for _ in 0..rng.next() % 4 {
            match rng.next() % 12 {
                0 => text.push(b' '),
                1 => text.push(b'#'),
                2 => text.push(b'+'),
                3 => text.push(b'-'),
                4 => text.push(b'0'),
                5 => {
                    text.push(b'*');
                    arguments.push(small(rng));
                }
                6 => {
                    let width = 1 + rng.next() % 19;
                    text.extend_from_slice(width.to_string().as_bytes());
                }
                7 => text.push(b'.'),
                8 => {
                    text.extend_from_slice(b".*");
                    arguments.push(small(rng));
                }
                9 => {
                    let precision = rng.next() % 20;
                    text.push(b'.');
                    text.extend_from_slice(precision.to_string().as_bytes());
                }
                _ => {}
            }
        }
        if rng.chance(3) {
            text.push(rng.pick(b"hlLL"));
        }
        let wide = text.last() == Some(&b'L');
        let conversion = rng.pick(b"diDIuUZxXXpobcsnsdxyq!");
        text.push(conversion);
        let value = |rng: &mut Rng| match rng.next() % 5 {
            0 => 0,
            1 => rng.next() % 1000,
            2 => 0u32.wrapping_sub(rng.next() % 1000),
            3 => 0x8000_0000,
            _ => rng.next().wrapping_mul(0x9e37_79b9),
        };
        match conversion {
            b's' => {
                let at = strings + piece * 0x40;
                put_string(m, rng, at, 0x20);
                arguments.push(if rng.chance(6) { 0 } else { at });
            }
            b'n' => {
                let at = counters + piece * 4 + if rng.chance(4) { 2 } else { 0 };
                arguments.push(at);
            }
            b'c' => arguments.push(rng.next()),
            b'y' | b'q' | b'!' => {}
            _ => {
                arguments.push(value(rng));
                if wide {
                    arguments.push(value(rng));
                }
            }
        }
    }
    if rng.chance(8) {
        text.push(b'%');
    }
    text.push(0);
    put(m, format, &text);
    for (i, a) in arguments.iter().enumerate() {
        m.sw(list + 4 * i as u32, *a);
    }
}

/// Set up one case of `routine` in `m`; returns its arguments.
fn prepare(routine: &Routine, m: &mut Machine, rng: &mut Rng, variant: u32) -> [u32; 4] {
    let mut args = random_args(rng);
    let gp = ac3_flight_jp::GP;
    let block = |m: &mut Machine, offset: u32| {
        let p = m.lw(gp + offset);
        ((0x8000_0000..0x801f_fe00).contains(&p) && p.is_multiple_of(4)).then_some(p)
    };
    match routine.entry {
        0x8002_7084 => {
            // a TIM at an aligned data address (sometimes not a TIM, or
            // not aligned), the TIM_IMAGE stack words a failed ReadTIM
            // leaves, the CLUT y on the stack, libgpu's debug level
            let tim = if rng.chance(8) {
                pointer(rng, m)
            } else {
                DATA + (rng.next() % 0x100) * 4
            };
            if (0x8000_0000..0x8020_0000).contains(&tim) && tim.is_multiple_of(4) {
                let clut = rng.chance(2);
                let id = if rng.chance(6) {
                    rng.next() % 0x20
                } else {
                    0x10
                };
                let mut words = vec![id, (u32::from(clut) << 3) | (rng.next() % 4)];
                if clut {
                    let n = 1 + rng.next() % 8;
                    words.push(12 + 4 * n);
                    words.push(rng.next());
                    words.push(rng.next());
                    words.extend((0..n).map(|_| rng.next()));
                }
                let n = 1 + rng.next() % 16;
                words.push(12 + 4 * n);
                words.push(rng.next());
                words.push((u32::from(size_half(rng)) << 16) | u32::from(size_half(rng)));
                words.extend((0..n).map(|_| rng.next()));
                for (i, w) in words.iter().enumerate() {
                    if (tim + 4 * i as u32) < 0x8020_0000 - 4 {
                        m.sw(tim + 4 * i as u32, *w);
                    }
                }
            }
            let frame = SP - 0x48;
            for field in (0x10..0x28).step_by(4) {
                let word = if rng.chance(3) { 0 } else { pointer(rng, m) };
                m.sw(frame + field, word);
            }
            m.sw(SP + 0x10, rng.next() % 0x200);
            m.sb(GPU_DEBUG, rng.pick(&[0u8, 0, 0, 1, 2]));
            args = [
                tim,
                rng.next() % 0x400,
                rng.next() % 0x200,
                rng.next() % 0x400,
            ];
        }
        0x8002_877c | 0x8002_8074 => {
            let format = DATA;
            let list = DATA + 0x400;
            let buffer = DATA + 0x800 + rng.next() % 4;
            put_format(m, rng, format, list);
            let format = if variant % 16 == 15 {
                // a format the image holds
                rng.pick(&[0x8001_0030, 0x8001_0048, 0x8002_5160, 0x8002_5154])
            } else {
                format
            };
            args = if routine.entry == 0x8002_877c {
                [buffer, format, list, rng.next()]
            } else {
                let cursor = DATA + 0x1e00;
                m.sw(cursor, buffer);
                [format, list, cursor, rng.next()]
            };
        }
        0x8002_b46c | 0x8002_b528 => {
            // the CD monitor's state, step, error and CdSync result
            if let Some(monitor) = block(m, 0x34) {
                m.sw(monitor, rng.pick(&[0, 1, 2, 2, 2, 2, 3, 3, 4, 5, u32::MAX]));
                m.sw(monitor + 4, rng.pick(&[0, 0, 1, 2, 2, 2, 3, u32::MAX]));
                m.sw(monitor + 0xc, rng.pick(&[0, 1, 1, 2, 2, 3, 5, u32::MAX]));
                let result = m.lbu(monitor + 0x28);
                m.sb(monitor + 0x28, result ^ rng.pick(&[0u8, 0, 0x10]));
            }
        }
        0x8002_cae4 => {
            // the queue's track, indices and pending flag, the driver flags
            if let Some(queue) = block(m, 0x48) {
                let track = m.lw(queue + 0x174);
                args[0] = if rng.chance(4) { word(rng) } else { track };
                m.sw(queue, rng.pick(&[0, 0, 1, 2, 0x10, u32::MAX]));
                m.sw(queue + 4, rng.pick(&[0, 1, 1, 2, 3, 0x10, u32::MAX]));
                m.sw(queue + 0x148, rng.pick(&[0, 0, 0, 1]));
            }
            if let Some(driver) = block(m, 0x38) {
                let flags = m.lw(driver);
                let bits = rng.pick(&[0, 0x8100_0000, 0x8100_0000, 0x8000_0000, 0x0100_0000]);
                m.sw(driver, (flags & 0x7eff_ffff) | bits);
                if rng.chance(2) {
                    m.sw(driver + 0x40, word(rng));
                }
            }
        }
        0x8002_cba4 => {
            args[0] = (rng.next() % 0x40) | if rng.chance(2) { 0x800 } else { 0 };
        }
        0x8002_d544 => {
            if let Some(request) = block(m, 0x314) {
                m.sw(request + 4, word(rng));
            }
        }
        0x8002_da94 => {
            m.sw(
                0x800c_65f4,
                rng.pick(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]),
            );
        }
        0x8002_dd7c => {
            let w = word(rng);
            args[0] = rng.pick(&[0, 1, 5, 23, 24, 0xff, 0xff, 0x100, w]);
        }
        0x8002_de68 => {
            let w = word(rng);
            args[0] = rng.pick(&[0, 1, 5, 0x20, 0x60, 0xff, w]);
        }
        0x8002_f19c => {
            for slot in 0..8 {
                let frames = if rng.chance(3) {
                    0xffff
                } else {
                    rng.next() % 0x40
                };
                m.sh(0x800c_6552 + 4 * slot, frames as u16);
            }
            args[1] = word(rng);
        }
        0x8004_1920 => {
            let w = word(rng);
            args = [
                rng.pick(&[0, 0, 1, 2, 3, 4, 5, 0x100, w]),
                rng.next() % 0x200,
                rng.next() % 0x100,
                rng.next(),
            ];
            if rng.chance(2) {
                m.sb(0x800c_7173, args[0].wrapping_sub(1) as u8);
            }
            m.sb(0x800c_7172, rng.pick(&[0u8, 1, 2, 3]));
        }
        0x8009_4380 => {
            let w = word(rng);
            args[0] = rng.pick(&[0x1f4, 0x1f4, 0, 0x1f3, 0x1f5, w]);
        }
        0x800a_de38 => {
            // two strings, often sharing a prefix (or equal), sometimes
            // the same pointer or a null one
            let a = DATA + rng.next() % 0x100;
            let b = DATA + 0x200 + rng.next() % 0x100;
            let left = put_string(m, rng, a, 12);
            let mut right = if rng.chance(2) {
                left.clone()
            } else {
                put_string(m, rng, b, 12)
            };
            if rng.chance(2) && right.len() > 1 {
                let k = rng.next() as usize % (right.len() - 1);
                right[k] = rng.pick(&[0u8, 0x7f, 0x80, 0xff, b'a', b'z']);
            }
            put(m, b, &right);
            args = match rng.next() % 12 {
                0 => [0, b, 0, 0],
                1 => [a, 0, 0, 0],
                2 => [0, 0, 0, 0],
                3 => [a, a, 0, 0],
                4 => [a, a + 1, 0, 0],
                _ => [a, b, rng.next(), rng.next()],
            };
        }
        0x800a_a8e0 => {
            // libgpu's device pointers aimed at RAM words that say DMA
            // channel 2 idle and the GPU ready (the host GPU's state); the
            // RECT, the debug level, the DMA callback slot
            m.sw(DEVICE_WORDS.0, 0);
            m.sw(DEVICE_WORDS.0 + 4, 0x1400_0000 | (rng.next() & 0x00ff_ffff));
            m.sw(DMA2_CHCR_POINTER, DEVICE_WORDS.0);
            m.sw(GPUSTAT_POINTER, DEVICE_WORDS.0 + 4);
            let rect = DATA + (rng.next() % 0x40) * 4;
            for (offset, half) in [
                (0, (rng.next() % 0x500) as u16),
                (2, (rng.next() % 0x240) as u16),
                (4, size_half(rng)),
                (6, size_half(rng)),
            ] {
                m.sh(rect + offset, half);
            }
            m.sb(GPU_DEBUG, rng.pick(&[0u8, 0, 0, 1, 1, 2]));
            if rng.chance(2) {
                m.sw(
                    DMA2_CALLBACK,
                    rng.pick(&[0, 0x800a_acf8, 0x800a_acf8, 0x800a_9f2c]),
                );
            }
            args = [rect, pointer(rng, m), rng.next(), rng.next()];
        }
        0x800a_c788 => {
            // a packet and a RECT (sometimes overlapping it)
            let packet = DATA + (rng.next() % 0x40) * 4 + u32::from(rng.chance(16));
            let rect = match rng.next() % 6 {
                0 => packet.wrapping_add(4),
                1 => packet.wrapping_add(8),
                _ => DATA + 0x400 + (rng.next() % 0x40) * 4,
            };
            if (DATA..DATA + 0x800).contains(&rect) {
                let w = size_half(rng);
                let h = size_half(rng);
                m.sw(rect, rng.next());
                m.sh(rect + 4, w);
                m.sh(rect + 6, h);
            }
            args = [packet, rect, rng.next(), rng.next()];
        }
        0x800a_d398 => {
            args[0] = if rng.chance(3) {
                DATA + rng.next() % 0x100
            } else {
                pointer(rng, m)
            };
        }
        0x800a_2f50 | 0x800a_43f8 => {
            // libcd's register pointers aimed at RAM words (excluded from
            // the comparison), the interrupt flag with no interrupt pending
            for (i, &p) in CD_REGISTER_POINTERS.iter().enumerate() {
                m.sw(p, DEVICE_WORDS.0 + 0x10 + 4 * i as u32);
                m.sw(DEVICE_WORDS.0 + 0x10 + 4 * i as u32, rng.next() & !7);
            }
            for status in 0x800b_6538..0x800b_653b {
                m.sb(status, rng.next() as u8);
            }
        }
        _ => {}
    }
    let _ = m.take_fault();
    args
}

/// Every case of a routine: (cases, faulted alike, RAM bytes changed,
/// covered words, words, failures, never-run ranges).
struct Outcome {
    cases: usize,
    faulted: usize,
    changed: usize,
    covered: usize,
    words: usize,
    failures: Vec<String>,
    missed: Vec<(u32, u32)>,
}

fn outcome(routine: &Routine) -> Outcome {
    let rows = scan::jp_map(&scan::jp_dir());
    let (start, mut end) = extent(&rows, routine.module, routine.entry);
    for &part in routine.parts {
        let (from, to) = extent(&rows, routine.module, part);
        assert_eq!(
            from, end,
            "{part:08x} does not continue {:08x}",
            routine.entry
        );
        end = to;
    }
    let mut ranges = vec![(start, end)];
    ranges.extend(
        routine
            .with
            .iter()
            .map(|&s| extent(&rows, routine.module, s)),
    );
    let words = ((end - start) / 4) as usize;
    let mut seen = BTreeSet::new();
    let mut out = Outcome {
        cases: 0,
        faulted: 0,
        changed: 0,
        covered: 0,
        words,
        failures: Vec::new(),
        missed: Vec::new(),
    };
    for (name, mut state) in states() {
        if routine.module != 0 && loaded_module(&mut state) != routine.module {
            continue;
        }
        let mut rng = Rng(start
            ^ (name.len() as u32).wrapping_mul(0x9e37_79b9)
            ^ u32::from(name.as_bytes()[17]));
        for variant in 0..VARIANTS {
            let mut base = state.clone();
            let args = prepare(routine, &mut base, &mut rng, variant);
            match compare(&base, routine, &ranges, args, &mut seen) {
                Ok((changed, faulted)) => {
                    out.cases += 1;
                    out.faulted += usize::from(faulted);
                    out.changed += changed;
                }
                Err(e) => out
                    .failures
                    .push(format!("{name} v{variant} args {args:08x?}: {e}")),
            }
        }
    }
    for a in (start..end).step_by(4) {
        if seen.contains(&a) {
            out.covered += 1;
            continue;
        }
        match out.missed.last_mut() {
            Some(last) if last.1 == a => last.1 = a + 4,
            _ => out.missed.push((a, a + 4)),
        }
    }
    out
}

fn check(routine: Routine) {
    let o = outcome(&routine);
    let missed: Vec<String> = o
        .missed
        .iter()
        .map(|(a, b)| format!("{a:08x}..{b:08x}"))
        .collect();
    println!(
        "{:08x} {}: {} cases ({} faulted alike), {}/{} instructions ran, {} RAM bytes changed{}",
        routine.entry,
        routine.name,
        o.cases,
        o.faulted,
        o.covered,
        o.words,
        o.changed,
        if missed.is_empty() {
            String::new()
        } else {
            format!("; not run: {} ({})", missed.join(" "), routine.why)
        }
    );
    for f in o.failures.iter().take(8) {
        println!("  FAIL {f}");
    }
    assert!(
        o.failures.is_empty(),
        "{:08x}: {} cases differ",
        routine.entry,
        o.failures.len()
    );
    let stray: Vec<&(u32, u32)> = o
        .missed
        .iter()
        .filter(|(a, b)| !routine.unreachable.iter().any(|(s, e)| s <= a && b <= e))
        .collect();
    assert!(
        stray.is_empty(),
        "{:08x}: instructions never ran: {stray:08x?}",
        routine.entry
    );
    assert!(
        o.cases > o.faulted,
        "{:08x}: every case faulted",
        routine.entry
    );
}

const fn plain(entry: u32, name: &'static str) -> Routine {
    Routine {
        entry,
        module: 0,
        name,
        parts: &[],
        with: &[],
        void: false,
        drive: false,
        unreachable: &[],
        why: "",
    }
}

/// The formatter's helpers 80027C24, 80027C3C and 80027E80.
const FORMAT_HELPERS: &[u32] = &[0x8002_7c24, 0x8002_7c3c, 0x8002_7e80];

macro_rules! oracle_tests {
    ($($test:ident: $routine:expr;)*) => {
        $(
            #[test]
            #[ignore = "requires AC3_JP_DIR (the JP Redux RAM states)"]
            fn $test() {
                check($routine);
            }
        )*
    };
}

oracle_tests! {
    stop_pads: Routine { void: true, ..plain(0x8002_7064, "stop_pads") };
    upload_tim_at: Routine {
        unreachable: &[(0x8002_70c0, 0x8002_70cc)],
        why: "the failed-OpenTIM print: OpenTIM (800AAD48) always returns 0",
        ..plain(0x8002_7084, "upload_tim_at")
    };
    format: Routine { with: FORMAT_HELPERS, ..plain(0x8002_8074, "format") };
    vsprintf: Routine {
        with: &[0x8002_7c24, 0x8002_7c3c, 0x8002_7e80, 0x8002_8074],
        ..plain(0x8002_877c, "vsprintf")
    };
    restart_drive: Routine { drive: true, ..plain(0x8002_b46c, "restart_drive") };
    disc_state: plain(0x8002_b528, "disc_state");
    stream_time: plain(0x8002_cae4, "stream_time");
    stream_kind: plain(0x8002_cba4, "stream_kind");
    sound_request_busy: plain(0x8002_d544, "sound_request_busy");
    reverb_on: plain(0x8002_da94, "reverb_on");
    voice_in_use: plain(0x8002_dd7c, "voice_in_use");
    sound_pitch: plain(0x8002_de68, "sound_pitch");
    delay_sound: plain(0x8002_f19c, "delay_sound");
    reset_lock: plain(0x8003_a07c, "reset_lock");
    show_portrait: plain(0x8004_1920, "show_portrait");
    message_done: plain(0x8005_6a44, "message_done");
    spawn_at_500: plain(0x8009_4380, "spawn_at_500");
    strcmp: Routine { parts: &[0x800a_de94], ..plain(0x800a_de38, "strcmp") };
    load_image2: Routine {
        unreachable: &[(0x800a_a938, 0x800a_a95c)],
        why: "the wait loop's poll and its timeout exit: they run only while DMA channel 2 is \
              busy or the GPU takes no commands (device states; the host GPU is ready at once, \
              as for StoreImage2)",
        parts: &[0x800a_a95c],
        ..plain(0x800a_a8e0, "load_image2")
    };
    get_clut: plain(0x800a_c708, "get_clut");
    set_draw_load: plain(0x800a_c788, "set_draw_load");
    dec_dct_buf_size: plain(0x800a_d398, "dec_dct_buf_size");
    // `jr ra; nop`: the caller's v0 stays (not compared)
    reset_rcnt_epilogue: Routine { void: true, ..plain(0x800a_f5ec, "reset_rcnt_epilogue") };
    // overlay 2's jr ra; nop handlers (src/jp/ov2_stubs.rs)
    ov2_empty_handler_11e4: Routine { module: 2, void: true, ..plain(0x800e_11e4, "ov2 empty handler") };
    ov2_empty_handler_11ec: Routine { module: 2, void: true, ..plain(0x800e_11ec, "ov2 empty handler") };
    cd_flush: Routine { with: &[0x800a_43f8], ..plain(0x800a_2f50, "cd_flush") };
    cd_flush_drive: Routine {
        unreachable: &[(0x800a_4428, 0x800a_447c)],
        why: "the acknowledge loop: it runs only while the drive has an interrupt pending \
              (a device state; the host drive delivers its interrupts through libcd's handler)",
        ..plain(0x800a_43f8, "cd_flush_drive")
    };
}

/// The captured calls of GetClut and SetDrawLoad (story-002, module 0).
#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn captures_replay() {
    let index = redux().join("story-002/snapshots-module0.jsonl");
    let mut clut = 0;
    let changed = snapshot::check_call(&index, 0x800a_c708, 0, |_, entry| {
        clut = res_sdk::get_clut(entry.gpr(4), entry.gpr(5));
        Some(clut)
    })
    .unwrap();
    println!("800ac708 GetClut: capture reproduced, v0 {clut:#06x}, {changed} bytes changed");
    assert_ne!(clut, 0, "GetClut's capture is not vacuous");
    let mut end = 0;
    let changed = snapshot::check_call(&index, 0x800a_c788, 0, |m, entry| {
        end = res_sdk::set_draw_load(m, entry.gpr(4), entry.gpr(5));
        Some(end)
    })
    .unwrap();
    println!("800ac788 SetDrawLoad: capture reproduced, v0 {end:#010x}, {changed} bytes changed");
    assert!(changed > 0, "SetDrawLoad's capture is not vacuous");
}

/// Krom2RawAdd: no kanji ROM, so every code is one the ROM lacks; nothing
/// else happens (no RAM, no host call, no fault).
#[test]
fn krom2_raw_add_has_no_glyphs() {
    let mut m = Machine::default();
    m.sp = SP;
    for sjis in [
        0u32,
        0x8140,
        0x82a0,
        0x889f,
        0x9872,
        0xeaa4,
        0xffff,
        u32::MAX,
    ] {
        assert_eq!(res_sdk::krom2_raw_add(sjis), u32::MAX);
        assert_eq!(
            ac3_flight_jp::dispatch::call(&mut m, 0x800a_f438, [sjis, 0, 0, 0]),
            u32::MAX
        );
    }
    assert!(m.host_calls.is_empty());
    assert!(m.ram().iter().all(|&b| b == 0));
    assert_eq!(m.take_fault(), Ok(()));
}

/// PadStopCom (and 80027064 around it) is the one host call PAD_STOP_COM
/// and writes no RAM.
#[test]
fn pad_stop_com_is_the_host_call() {
    let mut m = Machine::default();
    m.sp = SP;
    ac3_flight_jp::dispatch::call(&mut m, 0x8009_cd50, [0; 4]);
    res_sdk_game::stop_pads(&mut m);
    assert_eq!(
        calls(&m),
        vec![
            (host_ids::PAD_STOP_COM, [0; 4]),
            (host_ids::PAD_STOP_COM, [0; 4])
        ]
    );
    assert!(m.ram().iter().all(|&b| b == 0));
    assert_eq!(m.sp, SP);
    assert_eq!(m.take_fault(), Ok(()));
}

/// Edge paths by value: GetClut's masks, SetDrawLoad's length limit and
/// its terminator over the tag, strcmp's null pointers and signed bytes,
/// the disc state table, a format with every conversion.
#[test]
fn edge_paths() {
    assert_eq!(res_sdk::get_clut(0x3f0, 0x1ff), 0x7fff);
    assert_eq!(res_sdk::get_clut(0xffff_fff0, 0x401), 0x007f);
    let mut m = Machine::default();
    m.sp = SP;
    let (packet, rect) = (0x8010_0000, 0x8010_0100);
    m.sw(rect, 0x0010_0020);
    m.sh(rect + 4, 5);
    m.sh(rect + 6, 5);
    // 25 pixels: 13 words, too many: length 0 and the terminator over the tag
    assert_eq!(res_sdk::set_draw_load(&mut m, packet, rect), packet);
    assert_eq!(m.lw(packet), 0x0100_0000);
    m.sh(rect + 6, 4);
    // 20 pixels: 10 words, length 14
    assert_eq!(
        res_sdk::set_draw_load(&mut m, packet, rect),
        packet + 14 * 4
    );
    assert_eq!(m.lbu(packet + 3), 14);
    assert_eq!(m.lw(packet + 4), 0xa000_0000);
    assert_eq!(m.lw(packet + 8), 0x0010_0020);
    assert_eq!(m.lw(packet + 0xc), 0x0004_0005);
    assert_eq!(m.lw(packet + 14 * 4), 0x0100_0000);
    let (a, b) = (0x8010_0200, 0x8010_0300);
    put(&mut m, a, b"ab\x80\0");
    put(&mut m, b, b"ab\x01\0");
    assert_eq!(res_sdk::strcmp(&mut m, a, b), (-0x80i32 - 1) as u32);
    assert_eq!(res_sdk::strcmp(&mut m, b, a), 0x81);
    assert_eq!(res_sdk::strcmp(&mut m, a, a), 0);
    assert_eq!(res_sdk::strcmp(&mut m, 0, a), u32::MAX);
    assert_eq!(res_sdk::strcmp(&mut m, a, 0), 1);
    assert_eq!(res_sdk::strcmp(&mut m, 0, 0), 0);
    let monitor = 0x8010_0400;
    m.sw(ac3_flight_jp::GP + 0x34, monitor);
    for (state, step, expected) in [
        (0, 0, 0),
        (1, 2, 0),
        (2, 2, 2),
        (2, 0, 1),
        (3, 0, 3),
        (3, 1, 1),
        (4, 0, 1),
        (5, 0, 0),
    ] {
        m.sw(monitor, state);
        m.sw(monitor + 4, step);
        assert_eq!(
            res_sdk_game::disc_state(&mut m),
            expected,
            "state {state} step {step}"
        );
    }
    // the digit tables and character classes the formatter reads
    put(&mut m, 0x8001_0134, b"0123456789ABCDEF\0\0\0\0");
    put(&mut m, 0x8001_0148, b"0123456789abcdef\0\0\0\0");
    for c in b'0'..=b'9' {
        m.sb(0x800b_ca55 + u32::from(c), 4);
    }
    put(&mut m, 0x800b_cb9c, b"(null)\0");
    let (format, list, buffer) = (0x8010_0500, 0x8010_0600, 0x8010_0700);
    put(&mut m, format, b"[%5d|%-4x|%04X|%s|%c|%.2s|%u%%]\0");
    for (i, v) in [
        0xffff_ff85u32,
        0xab,
        0x1f,
        0,
        u32::from(b'q'),
        0x8010_0800,
        7,
    ]
    .iter()
    .enumerate()
    {
        m.sw(list + 4 * i as u32, *v);
    }
    put(&mut m, 0x8010_0800, b"xyz\0");
    let length = res_sdk_format::vsprintf(&mut m, buffer, format, list);
    let text: Vec<u8> = (0..=length).map(|i| m.lbu(buffer + i)).collect();
    assert_eq!(&text[..], b"[ -123|ab  |001F|(null)|q|xy|7%]\0");
    assert_eq!(m.take_fault(), Ok(()));
}
