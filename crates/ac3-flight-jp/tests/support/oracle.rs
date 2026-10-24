//! TEST-ONLY differential oracle for routines no capture reaches (other game
//! modes, missions, weapons). It interprets the ORIGINAL instruction bytes of
//! one routine from snapshot RAM with a small MIPS interpreter and compares
//! the result with the native translation started from the same machine
//! state. Calls leaving the interpreted range run natively through
//! `dispatch::call` in both runs, so only the routine under test differs.
//!
//! This is a validation tool, like the PCSX-Redux captures: the port itself
//! never interprets MIPS code (FLIGHT_MACHINE.md). Limits: no GTE
//! write-to-use hazard and no load delay slots (compiled code respects
//! both), no COP0. `break` raises `Fault::Trap` (the native convention) and
//! stops the run.
//!
//! The JP crate's copy (`tools/jp/templates/support/oracle.rs`, in place of
//! the re-addressed PAL `crates/ac3-flight/tests/support/oracle.rs`; keep the
//! two in step): a call out of the range to a `jr ra; nop` stub ([`STUB`])
//! returns with `v0` unchanged, as the stub does on the hardware, instead
//! of the value its dispatch arm returns (found by `jp-ov2b`: the claimed
//! stub 800E03CC returns 0 through `dispatch`, its `v0` is the caller's),
//! and so does a call of ExitCriticalSection 800AF3C8, which the kernel
//! returns from with the caller's `v0` (found by `jp-res2`: 80025B00's
//! `v0` is init_pads' 3 left over the call; the `bios` arm, reached
//! through `dispatch`, cannot carry it in and returns 0).
#![allow(dead_code)]
use ac3_flight_jp::{
    Fault, Machine,
    snapshot::{self, Snapshot},
};
use std::path::PathBuf;

/// `analysis/flight-runtime/<name>/snapshots.jsonl`.
pub fn capture(name: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR"))
        .join("analysis/flight-runtime")
        .join(name)
        .join("snapshots.jsonl")
}

/// Every entry snapshot of `callee` in capture `name`.
pub fn entries(name: &str, callee: u32) -> Vec<Snapshot> {
    let index_file = capture(name);
    let index = snapshot::read_index(&index_file).unwrap();
    index
        .iter()
        .filter(|s| s.callee == callee && s.stage == "entry")
        .map(|s| Snapshot::load(index_file.parent().unwrap(), s).unwrap())
        .collect()
}

/// A return address that is never code (not even in an overlay): reaching
/// it ends the run.
const RETURN: u32 = 0xffff_fff0;

/// `jr ra; nop`: a stub that returns with every register as the caller
/// left it.
pub const STUB: [u32; 2] = [0x03e0_0008, 0];

/// Interpret the original instructions in `start..end` over `m` with
/// arguments `args` (a fifth and later argument stay in the caller's frame at
/// `m.sp + 0x10..`). Returns `v0`.
pub fn run(m: &mut Machine, start: u32, end: u32, args: [u32; 4]) -> u32 {
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
                // ExitCriticalSection: the kernel leaves `v0` as it was
                0x800a_f3c8 => r[2] = ac3_flight_jp::bios::exit_critical_section(m, r[2]),
                // a `jr ra; nop` stub leaves `v0` as it was
                _ if [m.peek(pc), m.peek(pc.wrapping_add(4))] == STUB => {}
                _ => r[2] = ac3_flight_jp::dispatch::call(m, pc, [r[4], r[5], r[6], r[7]]),
            }
            pc = r[31];
            next = pc + 4;
            continue;
        }
        steps += 1;
        assert!(steps < 10_000_000, "runaway at {pc:08x}");
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
                    // break code: bits 6..25; the compiler's traps put the
                    // 10-bit code in the upper half (0x1C00 -> 7).
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
                    // divu by zero: quotient 0xFFFFFFFF, remainder = dividend.
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

/// Run the oracle and `native` from clones of `base` and require the same
/// result: RAM outside the guest stack below the entry `sp`, scratchpad,
/// GTE, `sp`, the first fault, and `v0` when `native` returns one. Returns
/// the number of RAM bytes the oracle changed (to show a case is not
/// vacuous).
pub fn diff(
    label: &str,
    base: &Machine,
    range: (u32, u32),
    args: [u32; 4],
    native: impl FnOnce(&mut Machine) -> Option<u32>,
) -> usize {
    let sp = base.sp;
    let mut oracle = base.clone();
    let v0 = run(&mut oracle, range.0, range.1, args);
    let mut nat = base.clone();
    let nv0 = native(&mut nat);
    let (fo, fnat) = (oracle.take_fault(), nat.take_fault());
    assert_eq!(fo, fnat, "{label}: faults");
    let low = snapshot::STACK_START;
    let high = (sp & 0x1f_ffff) as usize;
    let dead = |i: usize| (low..high).contains(&i);
    let ram: Vec<u32> = (0..0x20_0000usize)
        .filter(|&i| !dead(i) && oracle.ram()[i] != nat.ram()[i])
        .map(|i| 0x8000_0000 + i as u32)
        .collect();
    let scratch = (0..0x400)
        .filter(|&i| oracle.scratch()[i] != nat.scratch()[i])
        .count();
    assert!(
        ram.is_empty() && scratch == 0 && oracle.gte == nat.gte && oracle.sp == nat.sp,
        "{label}: ram {:08x?} scratch {scratch} gte equal {} sp {:x}/{:x} fault {fo:?}",
        &ram[..ram.len().min(16)],
        oracle.gte == nat.gte,
        oracle.sp,
        nat.sp
    );
    if let Some(n) = nv0 {
        assert_eq!(n, v0, "{label}: v0");
    }
    (0..0x20_0000usize)
        .filter(|&i| !dead(i) && base.ram()[i] != oracle.ram()[i])
        .count()
}

/// Deterministic variant generator for oracle cases.
pub struct Rng(pub u32);
impl Rng {
    pub fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(1_103_515_245).wrapping_add(12345);
        self.0 >> 8
    }
}
