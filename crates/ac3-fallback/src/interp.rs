//! The R3000A interpreter: one routine's original instructions over the
//! guest machine.
//!
//! Semantics are those of the test oracle (`ac3-flight/tests/support/
//! oracle.rs`), which was checked against Redux captures, plus Redux's COP2
//! write-to-use timing:
//! * branch and jump delay slots; no load delay slots (compiled code never
//!   reads a loaded register in the next instruction);
//! * `add`/`addi`/`sub` wrap like `addu` (compiled code never relies on the
//!   overflow trap), `div`/`divu` by zero and `-2^31 / -1` as the CPU;
//! * `break` raises the native trap convention and ends the run;
//! * GTE through `ac3-gte`, with the pipeline of `docs/GTE.md`: a
//!   `mtc2`/`ctc2`/`lwc2` lands two instructions after it issues, a read
//!   sees landed writes only, a command runs through
//!   `Gte::command_after_writes`, and a busy GTE stalls until every write
//!   lands. Compiled code never trips these rules (it separates COP2 writes
//!   from use); the report counts any case that does.
//!
//! Control leaving the routine's extent:
//! * `jal`/`jalr` (and `bltzal`/`bgezal`) are calls: through the call-out,
//!   then on at the link address;
//! * any other way out (a branch, a fall-through past the end, `j`/`jr`)
//!   into a routine above the device boundary goes on inline in the extent
//!   it reached. The PsyQ signatures split many library routines in two
//!   (`memmove` jumps into `MEMMOVE_OBJ_64` with `v0` already set), and
//!   GCC 2.x emits no sibling calls, so this is the original's own
//!   control flow; for a genuine tail jump inline is the same thing.
//!   (Calling there would lose the registers, and a loop across a split
//!   would nest without end);
//! * into a device routine or an address no extent knows, it is a tail
//!   call: the routine returns what the target returns.
//!
//! A run started by native code gets a fresh register file: arguments, `$gp`,
//! `$sp` and a sentinel `$ra`. A run called from interpreted code (the call
//! went out through `dispatch::call` and came back to the fallback) takes
//! over the caller's registers and hands its own back on return, as one CPU
//! would: hand-written SDK helpers (libgte's `MSC02_OBJ_100` under
//! `VectorNormalSS`, libgcc's `__udivmoddi4`) take and return values in
//! registers outside the C convention, and `v1` comes back too. Native code
//! in between cannot see the difference: it passes `a0..a3` only.
use crate::{Call, Fallback, Guest};
use ac3_gte::pipeline::{PendingWrite, still_busy, write_need};

/// The register file handed between an interpreted caller and an
/// interpreted callee.
#[derive(Clone, Copy, Default)]
pub(crate) struct Registers {
    pub r: [u32; 32],
    pub hi: u32,
    pub lo: u32,
}

/// A load of the VBlank counter within this many instructions of the last
/// one continues a spin; from this many in a row, the guest busy-waits.
const SPIN_GAP: u64 = 32;
const SPIN_POLLS: u32 = 256;

/// A return address that is never code: reaching it ends the run.
pub const RETURN: u32 = 0xffff_fff0;

/// Why a run stopped early.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// `break` (the trap is raised on the guest).
    Trap,
    /// An instruction the interpreter does not run (COP0, `syscall`, an
    /// unknown opcode or GTE command).
    Unsupported,
    /// The step limit: a loop that never leaves (e.g. waiting for an
    /// interrupt the host only delivers in VSync waits).
    Runaway,
    /// An inner run stopped for one of the reasons above (except a trap).
    Aborted,
}

pub struct Outcome {
    pub v0: u32,
    pub v1: u32,
    pub steps: u64,
    pub stop: Option<Stop>,
    /// Commands or reads that met an in-flight COP2 write.
    pub hazards: u64,
    /// The first address where the run went on inline into another extent.
    pub crossed: Option<u32>,
}

/// How control reaches the next instruction.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Transfer {
    /// A taken branch (no link).
    Branch,
    /// `j` or `jr`.
    Jump,
    /// `jal`, `jalr`, `bltzal`, `bgezal`: on at this address afterwards.
    Link(u32),
}

/// COP2 writes in flight (issue index, write) and the last command.
#[derive(Default)]
struct Cop2 {
    pending: [(u64, Option<PendingWrite>); 3],
    len: usize,
    last: Option<(u64, u32)>,
    hazards: u64,
}

impl Cop2 {
    /// Land the writes issued at least three instructions before `n`.
    #[inline]
    fn land_due<G: Guest>(&mut self, g: &mut G, n: u64) {
        while self.len > 0 && n >= self.pending[0].0 + 3 {
            if let Some(write) = self.pending[0].1.take() {
                g.gte().apply_write(&write);
            }
            self.pending.rotate_left(1);
            self.len -= 1;
        }
    }
    fn land_all<G: Guest>(&mut self, g: &mut G) {
        for slot in &mut self.pending[..self.len] {
            if let Some(write) = slot.1.take() {
                g.gte().apply_write(&write);
            }
        }
        self.len = 0;
    }
    fn busy(&self, n: u64) -> bool {
        self.last
            .is_some_and(|(at, code)| still_busy(code, (n - at) as u32))
    }
    fn write<G: Guest>(&mut self, g: &mut G, n: u64, control: bool, index: usize, value: u32) {
        self.land_due(g, n);
        let write = PendingWrite {
            control,
            index,
            value,
            distance: 0,
        };
        self.pending[self.len] = (n, Some(write));
        self.len += 1;
    }
    /// Before `mfc2`/`cfc2`/`swc2` of `register` (data 0-31, control 32-63):
    /// a busy GTE stalls until every write lands; otherwise the read sees
    /// the landed value.
    fn read<G: Guest>(&mut self, g: &mut G, n: u64, register: usize) {
        self.land_due(g, n);
        if self.len == 0 {
            return;
        }
        if self.busy(n) {
            self.land_all(g);
        } else if self.pending[..self.len]
            .iter()
            .any(|(_, w)| w.is_some_and(|w| w.register() == register))
        {
            self.hazards += 1;
        }
    }
    fn command<G: Guest>(&mut self, g: &mut G, n: u64, code: u32) -> bool {
        self.land_due(g, n);
        let result = if self.len == 0 {
            g.gte().command(code)
        } else if self.busy(n) {
            self.land_all(g);
            g.gte().command(code)
        } else {
            let mut writes = [PendingWrite {
                control: false,
                index: 0,
                value: 0,
                distance: 0,
            }; 3];
            let mut count = 0;
            for (issued, write) in &mut self.pending[..self.len] {
                let write = write.take().expect("pending write");
                let distance = (n - *issued - 1) as u32;
                if distance < write_need(code, write.register()) {
                    self.hazards += 1;
                }
                writes[count] = PendingWrite { distance, ..write };
                count += 1;
            }
            self.len = 0;
            g.gte().command_after_writes(code, &writes[..count])
        };
        self.last = Some((n, code));
        result.is_ok()
    }
}

/// Run the original instructions of `start..end` from `entry` (a fifth and
/// later argument stay in the caller's frame at `$sp + 0x10..`).
pub fn exec<G: Guest>(
    fallback: &Fallback,
    g: &mut G,
    (start, end): (u32, u32),
    entry: u32,
    args: [u32; 4],
    call: &mut Call<'_, G>,
) -> Outcome {
    // Called from interpreted code: its registers (see `Registers`).
    let inherited = fallback.handoff.take();
    let Registers {
        mut r,
        mut hi,
        mut lo,
    } = inherited.unwrap_or_default();
    r[4..8].copy_from_slice(&args);
    r[28] = fallback.tables.gp;
    r[29] = g.sp();
    r[31] = RETURN;
    let (mut pc, mut next) = (entry, entry.wrapping_add(4));
    // The last control transfer and its target.
    let (mut transfer, mut target) = (Transfer::Branch, entry);
    let mut steps: u64 = 0;
    let mut cop2 = Cop2::default();
    let limit = fallback.max_steps;
    let (mut start, mut size) = (start, end.wrapping_sub(start));
    let mut stop = None;
    let mut crossed = None;
    // Polls of the VBlank counter in a tight loop.
    let counter = fallback.tables.vblank_counter & 0x1fff_ffff;
    let (mut spin, mut last_poll) = (0u32, 0u64);
    loop {
        if pc.wrapping_sub(start) >= size {
            if pc == RETURN {
                break;
            }
            let how = if pc == target { Some(transfer) } else { None };
            if !matches!(how, Some(Transfer::Link(_)))
                && let Some(owner) = fallback.owner(g, pc).filter(|o| o.extent.interpretable())
            {
                // Not a call: one routine split in two (or a tail jump,
                // which inline is the same): on in the extent it reached.
                start = owner.extent.start;
                size = owner.extent.size;
                crossed.get_or_insert(pc);
                fallback.trace(|| format!("on into {} at {pc:08x}", owner.extent.name));
                continue;
            }
            cop2.land_all(g);
            spin = 0;
            cop2.last = None;
            g.set_sp(r[29]);
            fallback.handoff.set(Some(Registers { r, hi, lo }));
            let (v0, v1) = fallback.callout(g, pc, [r[4], r[5], r[6], r[7]], call);
            if let Some(back) = fallback.handoff.take() {
                // An interpreted callee left these (`$ra` and `$sp` stay ours).
                let (ra, sp) = (r[31], r[29]);
                Registers { r, hi, lo } = back;
                (r[31], r[29]) = (ra, sp);
            }
            if let Some(v0) = v0 {
                r[2] = v0;
            }
            if let Some(v1) = v1 {
                r[3] = v1;
            }
            if fallback.aborted() {
                stop = Some(Stop::Aborted);
                break;
            }
            if let Some(Transfer::Link(back)) = how {
                pc = back;
                next = back.wrapping_add(4);
                target = RETURN;
                continue;
            }
            // A tail call: the target returned to our caller.
            r[29] = g.sp();
            break;
        }
        let n = steps;
        steps += 1;
        if steps > limit {
            g.raise_runaway(pc);
            stop = Some(Stop::Runaway);
            break;
        }
        let w = g.fetch(pc);
        let cur = pc;
        pc = next;
        next = next.wrapping_add(4);
        let op = w >> 26;
        let rs = ((w >> 21) & 31) as usize;
        let rt = ((w >> 16) & 31) as usize;
        let rd = ((w >> 11) & 31) as usize;
        let simm = (w & 0xffff) as u16 as i16 as i32 as u32;
        let (a, b) = (r[rs], r[rt]);
        match op {
            0 => {
                let sa = (w >> 6) & 31;
                match w & 63 {
                    0x00 => r[rd] = b << sa,
                    0x02 => r[rd] = b >> sa,
                    0x03 => r[rd] = ((b as i32) >> sa) as u32,
                    0x04 => r[rd] = b << (a & 31),
                    0x06 => r[rd] = b >> (a & 31),
                    0x07 => r[rd] = ((b as i32) >> (a & 31)) as u32,
                    0x08 => {
                        next = a;
                        (transfer, target) = (Transfer::Jump, a);
                    }
                    0x09 => {
                        r[rd] = cur.wrapping_add(8);
                        next = a;
                        (transfer, target) = (Transfer::Link(cur.wrapping_add(8)), a);
                    }
                    0x0d => {
                        // break code: bits 6..25; the compiler's traps put
                        // the 10-bit code in the upper half (0x1C00 -> 7).
                        let code = ((w >> 6) & 0xf_ffff) >> 10;
                        g.raise_trap(cur, code);
                        stop = Some(Stop::Trap);
                        break;
                    }
                    0x10 => r[rd] = hi,
                    0x11 => hi = a,
                    0x12 => r[rd] = lo,
                    0x13 => lo = a,
                    0x18 => {
                        let p = i64::from(a as i32) * i64::from(b as i32);
                        (lo, hi) = (p as u32, (p >> 32) as u32);
                    }
                    0x19 => {
                        let p = u64::from(a) * u64::from(b);
                        (lo, hi) = (p as u32, (p >> 32) as u32);
                    }
                    0x1a => {
                        let (x, y) = (a as i32, b as i32);
                        if y == 0 {
                            lo = if x < 0 { 1 } else { u32::MAX };
                            hi = a;
                        } else {
                            lo = x.wrapping_div(y) as u32;
                            hi = x.wrapping_rem(y) as u32;
                        }
                    }
                    0x1b => {
                        lo = a.checked_div(b).unwrap_or(u32::MAX);
                        hi = a.checked_rem(b).unwrap_or(a);
                    }
                    0x20 | 0x21 => r[rd] = a.wrapping_add(b),
                    0x22 | 0x23 => r[rd] = a.wrapping_sub(b),
                    0x24 => r[rd] = a & b,
                    0x25 => r[rd] = a | b,
                    0x26 => r[rd] = a ^ b,
                    0x27 => r[rd] = !(a | b),
                    0x2a => r[rd] = u32::from((a as i32) < (b as i32)),
                    0x2b => r[rd] = u32::from(a < b),
                    _ => {
                        g.raise_unsupported(cur, w);
                        stop = Some(Stop::Unsupported);
                        break;
                    }
                }
            }
            1 => {
                let take = if rt & 1 == 0 {
                    (a as i32) < 0
                } else {
                    (a as i32) >= 0
                };
                if rt & 0x10 != 0 {
                    r[31] = cur.wrapping_add(8);
                }
                if take {
                    next = cur.wrapping_add(4).wrapping_add(simm << 2);
                    transfer = if rt & 0x10 != 0 {
                        Transfer::Link(cur.wrapping_add(8))
                    } else {
                        Transfer::Branch
                    };
                    target = next;
                }
            }
            2 | 3 => {
                next = (cur.wrapping_add(4) & 0xf000_0000) | ((w & 0x03ff_ffff) << 2);
                target = next;
                transfer = if op == 3 {
                    r[31] = cur.wrapping_add(8);
                    Transfer::Link(cur.wrapping_add(8))
                } else {
                    Transfer::Jump
                };
            }
            4..=7 => {
                let take = match op {
                    4 => a == b,
                    5 => a != b,
                    6 => (a as i32) <= 0,
                    _ => (a as i32) > 0,
                };
                if take {
                    next = cur.wrapping_add(4).wrapping_add(simm << 2);
                    (transfer, target) = (Transfer::Branch, next);
                }
            }
            8 | 9 => r[rt] = a.wrapping_add(simm),
            0xa => r[rt] = u32::from((a as i32) < (simm as i32)),
            0xb => r[rt] = u32::from(a < simm),
            0xc => r[rt] = a & (w & 0xffff),
            0xd => r[rt] = a | (w & 0xffff),
            0xe => r[rt] = a ^ (w & 0xffff),
            0xf => r[rt] = w << 16,
            0x12 => {
                let ok = if w & (1 << 25) != 0 {
                    cop2.command(g, n, w & 0x1ff_ffff)
                } else {
                    match rs {
                        0 => {
                            cop2.read(g, n, rd);
                            r[rt] = g.gte().mfc2(rd);
                            true
                        }
                        2 => {
                            cop2.read(g, n, 32 + rd);
                            r[rt] = g.gte().cfc2(rd);
                            true
                        }
                        4 => {
                            cop2.write(g, n, false, rd, b);
                            true
                        }
                        6 => {
                            cop2.write(g, n, true, rd, b);
                            true
                        }
                        _ => false,
                    }
                };
                if !ok {
                    g.raise_unsupported(cur, w);
                    stop = Some(Stop::Unsupported);
                    break;
                }
            }
            0x20 => r[rt] = g.lb(a.wrapping_add(simm)) as i32 as u32,
            0x21 => r[rt] = g.lh(a.wrapping_add(simm)) as i32 as u32,
            0x23 => {
                let address = a.wrapping_add(simm);
                if address & 0x1fff_ffff == counter {
                    // A tight loop on the VBlank counter: from SPIN_POLLS on,
                    // each iteration lets its guest time pass (about two
                    // cycles per instruction, as Redux counts), so the VBlank
                    // the loop waits for arrives.
                    let gap = n - last_poll;
                    spin = if gap <= SPIN_GAP {
                        spin.saturating_add(1)
                    } else {
                        0
                    };
                    last_poll = n;
                    if spin >= SPIN_POLLS && g.busy_wait(cur, 2 * gap as u32) {
                        fallback.busy_waited();
                    }
                }
                r[rt] = g.lw(address);
            }
            0x24 => r[rt] = u32::from(g.lbu(a.wrapping_add(simm))),
            0x25 => r[rt] = u32::from(g.lhu(a.wrapping_add(simm))),
            0x28 => g.sb(a.wrapping_add(simm), b as u8),
            0x29 => g.sh(a.wrapping_add(simm), b as u16),
            0x2b => g.sw(a.wrapping_add(simm), b),
            0x22 | 0x26 | 0x2a | 0x2e => {
                let address = a.wrapping_add(simm);
                let aligned = address & !3;
                let shift = (address & 3) as usize;
                let mem = g.lw(aligned);
                match op {
                    0x22 => {
                        const MASK: [u32; 4] = [0x00ff_ffff, 0x0000_ffff, 0x0000_00ff, 0];
                        const SH: [u32; 4] = [24, 16, 8, 0];
                        r[rt] = (b & MASK[shift]) | (mem << SH[shift]);
                    }
                    0x26 => {
                        const MASK: [u32; 4] = [0, 0xff00_0000, 0xffff_0000, 0xffff_ff00];
                        const SH: [u32; 4] = [0, 8, 16, 24];
                        r[rt] = (b & MASK[shift]) | (mem >> SH[shift]);
                    }
                    0x2a => {
                        const MASK: [u32; 4] = [0xffff_ff00, 0xffff_0000, 0xff00_0000, 0];
                        const SH: [u32; 4] = [24, 16, 8, 0];
                        g.sw(aligned, (mem & MASK[shift]) | (b >> SH[shift]));
                    }
                    _ => {
                        const MASK: [u32; 4] = [0, 0x0000_00ff, 0x0000_ffff, 0x00ff_ffff];
                        const SH: [u32; 4] = [0, 8, 16, 24];
                        g.sw(aligned, (mem & MASK[shift]) | (b << SH[shift]));
                    }
                }
            }
            0x32 => {
                let value = g.lw(a.wrapping_add(simm));
                cop2.write(g, n, false, rt, value);
            }
            0x3a => {
                cop2.read(g, n, rt);
                let value = g.gte().mfc2(rt);
                g.sw(a.wrapping_add(simm), value);
            }
            _ => {
                g.raise_unsupported(cur, w);
                stop = Some(Stop::Unsupported);
                break;
            }
        }
        r[0] = 0;
    }
    cop2.land_all(g);
    g.set_sp(r[29]);
    if inherited.is_some() {
        fallback.handoff.set(Some(Registers { r, hi, lo }));
    }
    Outcome {
        v0: r[2],
        v1: r[3],
        steps,
        stop,
        hazards: cop2.hazards,
        crossed,
    }
}
