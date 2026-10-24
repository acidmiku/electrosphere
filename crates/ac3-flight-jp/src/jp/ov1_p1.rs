//! JP overlay 1 (module 1: title, accounts, saves, the disc prompt), part 1:
//! the routines 800CC5D8..800D4808 (`jp-ov1a`), translated from the JP
//! instructions of BPB entry 1 (loaded at 0x800CBFE8 in state 4).
//!
//! This file holds the state-4 callbacks (callback table 0x800DCB00:
//! [`load_begin`] substate 0, [`load_wait`] substate 1, [`setup`]
//! substate 2, [`frame`] substate 3), their helpers, the sprite pools of
//! [`sprite_pools`] and the small allocator at 0x800DDA20. The menu
//! screens are in [`ov1_p1_screens`](super::ov1_p1_screens) and
//! [`ov1_p1_files`](super::ov1_p1_files); the memory-card layer in
//! [`ov1_p1_card`](super::ov1_p1_card).
//!
//! Overlay 1's other half (800D4D54..800DCAA4) is `jp-ov1b`'s: it is
//! called through [`dispatch::call`], like every resident routine.
//! The parts' dispatchers answer only while module 1 is loaded.
use crate::{
    arena,
    common::loaded_module,
    dispatch,
    machine::{Fault, Machine},
};

/// The module number the dispatchers answer for.
pub const MODULE: u32 = 1;
/// `[0x800BCE60]` (gp+0x2EC): the resident game context.
pub const CONTEXT: u32 = 0x800b_ce60;
/// `[0x800BCE48]` (gp+0x2D4): the display-request block (flags +0,
/// phase +4, background colour +0x10..+0x12, VSync interval +0x14).
pub const DISPLAY: u32 = 0x800b_ce48;
/// `[0x800BCE50]` (gp+0x2DC): the resident resource block.
pub const RESOURCES: u32 = 0x800b_ce50;
/// `[0x800DD9D0]`: the overlay's menu block (0xE4 bytes, [`setup`]): +0/+4
/// the two 0x2000-byte packet areas, +8 the packet pointer, +0xC/+0xE
/// the screen and its step (halfwords), +0x18 the card layer's state,
/// +0x44 frame counter, +0x48 a countdown, +0x58 the exit flag, +0x78
/// the idle counter.
pub const MENU: u32 = 0x800d_d9d0;
/// `[0x800DD9DC]`: the resource directory record of the loader.
pub const DIRECTORY: u32 = 0x800d_d9dc;
/// `0x800DCB10`: the screen table: per screen a table of step routines.
pub const SCREENS: u32 = 0x800d_cb10;
/// `[0x800DDA18]`: the two sprite pools of [`sprite_pools`].
pub const POOLS: u32 = 0x800d_da18;
/// `0x800DDA20` / `0x800DDA24` / `0x800DDA28` / `0x800DDA2C`: the small
/// bump allocator of [`heap_reset`] (pointer, last block, base, total).
pub const HEAP: u32 = 0x800d_da20;
/// The main loop's "next substate" request.
pub const NEXT_SUBSTATE: u32 = 0x0400_0000;

// ---- helpers shared by the part's files ----------------------------------------

pub(crate) const fn off(base: u32, offset: u32) -> u32 {
    base.wrapping_add(offset)
}
/// `lh`, sign-extended to a register value.
pub(crate) fn lh(m: &mut Machine, address: u32) -> u32 {
    m.lh(address) as u32
}
pub(crate) fn lhu(m: &mut Machine, address: u32) -> u32 {
    u32::from(m.lhu(address))
}
pub(crate) fn lbu(m: &mut Machine, address: u32) -> u32 {
    u32::from(m.lbu(address))
}
/// The 16-bit sign extension of `sll 16; sra 16`.
pub(crate) const fn sext16(value: u32) -> u32 {
    value as u16 as i16 as i32 as u32
}
/// `slt`: signed less-than.
pub(crate) const fn lt(a: u32, b: u32) -> bool {
    (a as i32) < (b as i32)
}
/// `sra`.
pub(crate) const fn sra(value: u32, shift: u32) -> u32 {
    ((value as i32) >> shift) as u32
}
/// HI of a signed `mult`.
pub(crate) const fn mulhi(a: u32, b: u32) -> u32 {
    ((a as i32 as i64 * b as i32 as i64) >> 32) as u32
}
/// A call through [`dispatch::call`] (a routine of another file, part or
/// module, or the resident).
pub(crate) fn call(m: &mut Machine, target: u32, args: [u32; 4]) -> u32 {
    dispatch::call(m, target, args)
}
/// Run `body` in an original stack frame of `size` bytes (`addiu sp,
/// sp, -size`); `body` gets the frame base.
pub(crate) fn framed<T>(
    m: &mut Machine,
    size: u32,
    body: impl FnOnce(&mut Machine, u32) -> T,
) -> T {
    let sp = m.push_frame(size);
    let out = body(m, sp);
    m.pop_frame(size);
    out
}
pub(crate) fn context(m: &mut Machine) -> u32 {
    m.lw(CONTEXT)
}
pub(crate) fn menu(m: &mut Machine) -> u32 {
    m.lw(MENU)
}
/// `lw` of the context's field at `offset`.
pub(crate) fn ctx_lw(m: &mut Machine, offset: u32) -> u32 {
    let ctx = context(m);
    m.lw(off(ctx, offset))
}
/// `lw` of the menu block's field at `offset`.
pub(crate) fn menu_lw(m: &mut Machine, offset: u32) -> u32 {
    let block = menu(m);
    m.lw(off(block, offset))
}
/// `lbu` of the menu block's byte at `offset`.
pub(crate) fn menu_lbu(m: &mut Machine, offset: u32) -> u32 {
    let block = menu(m);
    lbu(m, off(block, offset))
}
/// `lh` of the menu block's halfword at `offset`.
pub(crate) fn menu_lh(m: &mut Machine, offset: u32) -> u32 {
    let block = menu(m);
    lh(m, off(block, offset))
}
pub(crate) fn display(m: &mut Machine) -> u32 {
    m.lw(DISPLAY)
}
/// The addPrim idiom on words: the packet takes the 24-bit link of the
/// ordering-table entry, which then links to the packet (both keep their
/// top bytes): `p = p & 0xFF000000 | ot & 0xFFFFFF; ot = ot & 0xFF000000 |
/// packet & 0xFFFFFF`, with the reads and writes in the original order.
pub(crate) fn add_prim(m: &mut Machine, ot: u32, packet: u32) {
    let link = m.lw(packet) & 0xff00_0000;
    let entry = m.lw(ot) & 0x00ff_ffff;
    m.sw(packet, link | entry);
    let entry = m.lw(ot) & 0xff00_0000;
    m.sw(ot, entry | (packet & 0x00ff_ffff));
}

// ---- the state-4 callbacks ----------------------------------------------------

/// 800CCE98 (state 4, substate 0): display request 9 with phase 1, a
/// black background and VSync interval 0; resets the CD queue
/// (8002C6A8), runs 800DC810, relocates the directory 0x800DCE68 into
/// `[DIRECTORY]+0x24`, starts the text layer (800DA828: 1 when the
/// context's log-out flag +0x96C bit 2 is set) and 800D80C0; then queues
/// the overlay's file list (resource +0x60 = 0x70, one file, into an
/// arena block of its total size, callback 0x8009B904), sets the
/// context's loading flag +0x44 and the menu countdown +0x48 = 0x78.
/// Returns NEXT_SUBSTATE.
pub fn load_begin(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let block = display(m);
        m.sw(off(block, 4), 1);
        let flags = m.lw(block);
        m.sb(off(block, 0x10), 0);
        let block2 = display(m);
        m.sw(off(block, 0x14), 0);
        m.sw(block, flags | 9);
        m.sb(off(block2, 0x11), 0);
        let block3 = display(m);
        m.sb(off(block3, 0x12), 0);
        call(m, 0x8002_c6a8, [0, 0, block2, 0]);
        call(m, 0x800d_c810, [0, 0, 0, 0]);
        let relocated = call(m, 0x8002_7338, [0x800d_ce68, 0, 0, 0]);
        let record = m.lw(DIRECTORY);
        let ctx = context(m);
        m.sw(off(record, 0x24), relocated);
        let logout = u32::from(m.lw(off(ctx, 0x96c)) & 2 != 0);
        call(m, 0x800d_a828, [logout, 0, 0, 0]);
        call(m, 0x800d_80c0, [0, 0, 0, 0]);
        let resources = m.lw(RESOURCES);
        m.sw(off(resources, 0x60), 0x70);
        let size = call(m, 0x8002_c63c, [off(resources, 0x60), 1, 0, 0]);
        let destination = arena::alloc(m, size);
        let ctx = context(m);
        let list = off(m.lw(RESOURCES), 0x60);
        m.sw(off(ctx, 0x44), 1);
        call(m, 0x8002_c82c, [list, 1, destination, 0x8009_b904]);
        let block = menu(m);
        m.sw(off(block, 0x48), 0x78);
        NEXT_SUBSTATE
    })
}

/// 800CCFA4: the overlay's files are in: relocates the loaded directory
/// (resource +0x60's block), installs its sound bank (+0x20, 8002D96C)
/// and runs 8002DA94; the directory record takes the first entry's
/// first word and the relocated entry table (+0x24); then 800D4FF0
/// (+4, +8), 800D8BA8 (+0x10), 800D8CEC (+0x18), the arena reset to
/// +0x10 and 800DA418 (+0xC); clears the context's log-out flag (+0x96C
/// bit 2). Returns NEXT_SUBSTATE.
pub fn load_done(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        let resources = m.lw(RESOURCES);
        let block = m.lw(off(resources, 0x60));
        let directory = call(m, 0x8002_7338, [block, 0, 0, 0]);
        let bank = m.lw(off(directory, 0x20));
        call(m, 0x8002_d96c, [bank, 0, 0, 0]);
        call(m, 0x8002_da94, [bank, 0, 0, 0]);
        let first = m.lw(directory);
        let word = m.lw(first);
        let record = m.lw(DIRECTORY);
        m.sw(record, word);
        let first = m.lw(directory);
        let relocated = call(m, 0x8002_7338, [first, 0, 0, 0]);
        let record = m.lw(DIRECTORY);
        m.sw(off(record, 0x24), relocated);
        let a1 = m.lw(off(directory, 8));
        call(m, 0x800d_4ff0, [off(directory, 4), a1, 0, 0]);
        let a0 = m.lw(off(directory, 0x10));
        call(m, 0x800d_8ba8, [a0, 0, 0, 0]);
        let a0 = m.lw(off(directory, 0x18));
        call(m, 0x800d_8cec, [a0, 0, 0, 0]);
        let a0 = m.lw(off(directory, 0x10));
        arena::reset(m, a0);
        let a0 = m.lw(off(directory, 0xc));
        call(m, 0x800d_a418, [a0, 0, 0, 0]);
        let ctx = context(m);
        let flags = m.lw(off(ctx, 0x96c));
        m.sw(off(ctx, 0x96c), flags & !2);
        NEXT_SUBSTATE
    })
}

/// 800CD07C (state 4, substate 1): counts the menu countdown +0x48 down;
/// while the context's loading flag (+0x44) is set, or in log-out mode
/// (+0x96C bit 2, pad +0x12C bit 0x10 clear) before the countdown runs
/// out, draws the loading screen (and "LOG OUT" in log-out mode) and
/// returns 0; otherwise finishes with [`load_done`] (NEXT_SUBSTATE).
pub fn load_wait(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let block = menu(m);
        let count = m.lw(off(block, 0x48)).wrapping_sub(1);
        let ctx = context(m);
        m.sw(off(block, 0x48), count);
        let draw = m.lw(off(ctx, 0x44)) != 0
            || (m.lw(off(ctx, 0x96c)) & 2 != 0
                && m.lw(off(ctx, 0x12c)) & 0x10 == 0
                && !lt(count, 0));
        if !draw {
            return load_done(m);
        }
        let ot = ctx_lw(m, 8);
        call(m, 0x800d_81f0, [ot, 0, 0, 0]);
        call(m, 0x800d_7d08, [7, 0, 0, 0]);
        call(m, 0x800d_7c88, [u32::MAX, 0, 0, 0]);
        call(m, 0x800d_7ce4, [1, 1, 1, 0]);
        call(m, 0x800d_7cd4, [1, 1, 1, 0]);
        call(m, 0x800d_7c60, [0x18, 0xb8, 0xd0, 0]);
        if ctx_lw(m, 0x96c) & 2 != 0 {
            call(m, 0x800d_7c40, [0xe9, 0x91, 0xd0, 0]);
            call(m, 0x800d_8aa0, [0x800c_bfec, 0x91, 0xd0, 0]);
        }
        let ot = ctx_lw(m, 8);
        call(m, 0x800d_c9ec, [ot, 0, 0, 0]);
        call(m, 0x800d_caa4, [0, 0, 0, 0]);
        0
    })
}

/// 800CC5D8 (state 4, substate 2): clears the context's +0x95E/+0x95F
/// and flag +0x96C bit 8, copies the pair +0x390 to +0x38C; allocates
/// the menu block (0xE4 bytes) and its two 0x2000-byte packet areas;
/// runs 800DA25C; resets the block (screen 1 step 0, counters, flags;
/// +0x64 = 0x800DCB30, +0x68 = 0x800DCCC8, +0xD1 = 1) and the context's
/// +0x971; then 800D4F0C, 800D5550, 800D80C0 (with the card state +0x18
/// cleared) and [`sprite_pools`]. Returns NEXT_SUBSTATE.
pub fn setup(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        let ctx = context(m);
        m.sb(off(ctx, 0x95e), 0);
        let ctx = context(m);
        m.sb(off(ctx, 0x95f), 0);
        let ctx = context(m);
        let flags = m.lw(off(ctx, 0x96c));
        let a2 = lhu(m, off(ctx, 0x390));
        let a3 = lhu(m, off(ctx, 0x392));
        m.sw(off(ctx, 0x96c), flags & !8);
        m.sh(off(ctx, 0x38c), a2 as u16);
        m.sh(off(ctx, 0x38e), a3 as u16);
        let block = arena::alloc(m, 0xe4);
        m.sw(MENU, block);
        let mut a1 = 0;
        for k in 0..2u32 {
            let area = arena::alloc(m, 0x2000);
            a1 = off(menu(m), k * 4);
            m.sw(a1, area);
        }
        call(m, 0x800d_a25c, [0, a1, a2, a3]);
        let block = menu(m);
        m.sb(off(block, 0x58), 0);
        let block2 = menu(m);
        m.sh(off(block, 0xc), 1);
        m.sh(off(block, 0xe), 0);
        m.sw(off(block, 0x44), 0);
        m.sh(off(block, 0x12), 0);
        m.sw(off(block, 0x40), 0);
        m.sw(off(block, 0x78), 0);
        m.sb(off(block2, 0x59), 0);
        for offset in [0x5a, 0x5d, 0x5f, 0x60, 0x5e] {
            let block = menu(m);
            m.sb(off(block, offset), 0);
        }
        let block = menu(m);
        m.sw(off(block, 0x64), 0x800d_cb30);
        m.sb(off(block, 0x5c), 0);
        let block2 = menu(m);
        m.sw(off(block, 0x68), 0x800d_ccc8);
        m.sb(off(block2, 0x5b), 0);
        let block = menu(m);
        m.sb(off(block, 0xd0), 0);
        let block = menu(m);
        m.sb(off(block, 0xd1), 1);
        let ctx = context(m);
        m.sb(off(ctx, 0x971), 0);
        call(m, 0x800d_4f0c, [block2, 1, a2, a3]);
        call(m, 0x800d_5550, [0, 0, 0, 0]);
        let block = menu(m);
        m.sw(off(block, 0x18), 0);
        call(m, 0x800d_80c0, [0, 0, 0, 0]);
        sprite_pools(m);
        NEXT_SUBSTATE
    })
}

/// 800CC764 (state 4, substate 3): one menu frame. Counts +0x44, takes
/// the packet area of the context's buffer index (+4) as the packet
/// pointer, clears the ordering table (800DC9EC), draws the backdrop
/// (800DA6FC, 800D81F0, 800D7C40, 800D7C60), runs the card layer
/// (800D5F6C) while it is active and 800DCAA4, then the current screen
/// step (the table [`SCREENS`], through `jalr` with a0 the menu block).
/// After 601 idle frames (+0x78) it leaves for state 5 (the attract
/// movie: context +0x3A0 = 4, +0x3A8 = 0); on the exit flag (+0x58) it
/// resets the CD queue and leaves for state 3 (the story hub; context
/// +0x954 = 2, or 0 in log-out mode, display flag 4). Returns the
/// request, or 0.
pub fn frame(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        let block = menu(m);
        let ctx = context(m);
        let count = m.lw(off(block, 0x44)).wrapping_add(1);
        let ot = m.lw(off(ctx, 8));
        m.sw(off(block, 0x44), count);
        let index = m.lw(off(ctx, 4)) << 2;
        let area = m.lw(block.wrapping_add(index));
        m.sw(off(block, 8), area);
        call(m, 0x800d_c9ec, [off(ot, 4), 0, 0, 0]);
        let v0 = call(m, 0x800d_a6fc, [0, 0, 0, 0]);
        call(m, 0x800d_81f0, [off(v0, 4), 0, 0, 0]);
        call(m, 0x800d_7c40, [0x32, 0x12c, 0, 0]);
        call(m, 0x800d_7c60, [0x40, 0x40, 0x40, 0]);
        if menu_lw(m, 0x18) != 0 {
            call(m, 0x800d_5f6c, [0, 0, 0, 0]);
        }
        call(m, 0x800d_caa4, [0, 0, 0, 0]);
        let block = menu(m);
        let screen = lh(m, off(block, 0xc)) << 2;
        let steps = m.lw(screen.wrapping_add(SCREENS));
        if steps != 0 {
            let step = lh(m, off(block, 0xe)) << 2;
            let routine = m.lw(step.wrapping_add(steps));
            call(m, routine, [block, 0, 0, 0]);
        }
        let block = menu(m);
        if !lt(m.lw(off(block, 0x78)), 0x259) {
            if m.lw(off(block, 0x18)) != 0 {
                call(m, 0x800d_58b0, [0, 0, 0, 0]);
            }
            let ctx = context(m);
            m.sw(off(ctx, 0x3a0), 4);
            m.sb(off(ctx, 0x3a8), 0);
            return 0x6000_0005;
        }
        if m.lbu(off(block, 0x58)) == 0 {
            return 0;
        }
        call(m, 0x8002_c6a8, [0, 0, 0, 0]);
        if menu_lw(m, 0x18) != 0 {
            call(m, 0x800d_58b0, [0, 0, 0, 0]);
        }
        let ctx = context(m);
        let mode = if m.lw(off(ctx, 0x96c)) & 0x10 != 0 {
            0
        } else {
            2
        };
        m.sh(off(ctx, 0x954), mode);
        let block = display(m);
        let flags = m.lw(block);
        m.sw(block, flags | 4);
        0x6000_0003
    })
}

// ---- title helpers ------------------------------------------------------------

/// 800CC938: the 52-entry rank table at `out`: over the 55 records
/// 1..0x37 of the context (bytes +0xC1C/+0xC58/+0xC94; records 0x13,
/// 0x31 and 0x36 left out) the best non-zero of the three (5 when the
/// first is zero) is counted (on the stack, as the original: a rank
/// above 5 counts into the caller's frame); then `out` takes ranks 1..5
/// as often as counted, and 5 up to entry 0x34. Returns `out`.
pub fn rank_table(m: &mut Machine, out: u32) -> u32 {
    framed(m, 0x18, |m, sp| {
        for k in (0..6).rev() {
            m.sw(off(sp, 4 * k), 0);
        }
        let ctx = context(m);
        for record in 1..0x38u32 {
            if matches!(record, 0x13 | 0x31 | 0x36) {
                continue;
            }
            let at = off(ctx, record);
            let first = lbu(m, off(at, 0xc1c));
            let mut rank = if first == 0 { 5 } else { first };
            let second = lbu(m, off(at, 0xc58));
            if second != 0 && lt(second, rank) {
                rank = second;
            }
            let third = lbu(m, off(at, 0xc94));
            if third != 0 && lt(third, rank) {
                rank = third;
            }
            let slot = off(sp, rank << 2);
            let n = m.lw(slot).wrapping_add(1);
            m.sw(slot, n);
        }
        let mut at = 0u32;
        for rank in 1..=5u32 {
            let slot = off(sp, rank * 4);
            if lt(0, m.lw(slot)) {
                let mut n = 0u32;
                loop {
                    n = n.wrapping_add(1);
                    m.sb(off(out, at), rank as u8);
                    let count = m.lw(slot);
                    at = at.wrapping_add(1);
                    if !lt(n, count) {
                        break;
                    }
                }
            }
        }
        while lt(at, 0x34) {
            m.sb(off(out, at), 5);
            at = at.wrapping_add(1);
        }
        out
    })
}

/// 800CCB38: an 8x16 TILE packet at (110, 229) at `packet` whose colour
/// pulses with the context's frame counter (+0x20 modulo 120: the
/// distance from 60 scales (0xE5, 0xE4, 0xDE) down), linked into the
/// ordering-table entry `ot`. Returns the next packet.
pub fn pulse_tile(m: &mut Machine, ot: u32, packet: u32) -> u32 {
    const MAGIC: u32 = 0x8888_8889;
    m.sb(off(packet, 3), 3);
    m.sb(off(packet, 7), 0x60);
    m.sh(off(packet, 8), 0x6e);
    m.sh(off(packet, 0xa), 0xe5);
    m.sh(off(packet, 0xc), 8);
    let ctx = context(m);
    m.sh(off(packet, 0xe), 0x10);
    let counter = m.lw(off(ctx, 0x20));
    let q = sra(mulhi(counter, MAGIC).wrapping_add(counter), 6).wrapping_sub(sra(counter, 31));
    let mut phase = counter
        .wrapping_sub(q.wrapping_mul(15).wrapping_mul(8))
        .wrapping_sub(0x3c);
    if lt(phase, 0) {
        phase = 0u32.wrapping_sub(phase);
    }
    let scale = |m: u32| {
        let v = 0u32.wrapping_sub(phase.wrapping_mul(m));
        sra(mulhi(v, MAGIC).wrapping_add(v), 5).wrapping_sub(sra(v, 31))
    };
    let r = scale(29).wrapping_add(0xe5);
    m.sb(off(packet, 4), r as u8);
    let g = scale(132).wrapping_add(0xe4);
    m.sb(off(packet, 5), g as u8);
    let link = m.lw(packet);
    let b = scale(198).wrapping_add(0xde);
    m.sb(off(packet, 6), b as u8);
    let entry = m.lw(ot);
    m.sw(packet, (link & 0xff00_0000) | (entry & 0x00ff_ffff));
    let entry = m.lw(ot);
    m.sw(ot, (entry & 0xff00_0000) | (packet & 0x00ff_ffff));
    off(packet, 0x10)
}

/// 800CCC90 (title screen, step 0): step 1; the text layer in mode 1;
/// display request 8 with the background colour (0xE5, 0xE4, 0xDE); the
/// rank table at menu +0x94, whose 52 ranks go to the record icons
/// 1..0x34 (800DA9A8).
pub fn title_open(m: &mut Machine) {
    framed(m, 0x20, |m, _| {
        let block = menu(m);
        m.sw(off(block, 0x48), 0);
        m.sh(off(block, 0x10), 0);
        m.sh(off(block, 0xe), 1);
        call(m, 0x800d_a828, [1, 0, 0, 0]);
        let request = display(m);
        let flags = m.lw(request);
        m.sb(off(request, 0x10), 0xe5);
        let request2 = display(m);
        m.sw(request, flags | 8);
        m.sb(off(request2, 0x11), 0xe4);
        let request = display(m);
        m.sb(off(request, 0x12), 0xde);
        let block = menu(m);
        rank_table(m, off(block, 0x94));
        for k in 0..0x34u32 {
            let block = menu(m);
            let rank = lbu(m, off(block, k + 0x94));
            call(m, 0x800d_a9a8, [k + 1, rank, 0, 0]);
        }
    });
}

/// 800CCD48 (title screen, step 1): the pulsing tile (its next packet is
/// not kept); START (pad +0x12C bit 0x40) goes to screen 2 with display
/// request 8 and the background (0, 0x70, 0x78), sound 0x58, hides the
/// record icons 1..0x37 and clears the idle counter; otherwise counts
/// the idle counter +0x78.
pub fn title_wait(m: &mut Machine) {
    framed(m, 0x20, |m, _| {
        let ctx = context(m);
        let block = menu(m);
        let ot = m.lw(off(ctx, 8));
        let packet = m.lw(off(block, 8));
        pulse_tile(m, ot, packet);
        let ctx = context(m);
        if m.lw(off(ctx, 0x12c)) & 0x40 == 0 {
            let block = menu(m);
            let idle = m.lw(off(block, 0x78)).wrapping_add(1);
            m.sw(off(block, 0x78), idle);
            return;
        }
        let block = menu(m);
        let request = display(m);
        m.sh(off(block, 0xc), 2);
        m.sh(off(block, 0xe), 0);
        m.sh(off(block, 0x10), 0);
        let flags = m.lw(request);
        m.sb(off(request, 0x10), 0);
        let request2 = display(m);
        m.sw(request, flags | 8);
        m.sb(off(request2, 0x11), 0x70);
        let request2 = display(m);
        m.sb(off(request2, 0x12), 0x78);
        call(m, 0x8002_db4c, [0x58, request, 0x800c_0000, 0]);
        for k in 1..0x38u32 {
            call(m, 0x800d_a9a8, [k, 0, 0, 0]);
        }
        let block = menu(m);
        m.sw(off(block, 0x78), 0);
    });
}

/// 800CCE38: a relocated offset table: `table = [record]` holds a count
/// and that many byte offsets (rounded toward zero to words); `out`
/// takes `table + offset` for each. Returns the count.
pub fn offsets(m: &mut Machine, record: u32, out: u32) -> u32 {
    let table = m.lw(record);
    let count = m.lw(table);
    if !lt(0, count) {
        return count;
    }
    let mut out = out;
    let mut entry = table;
    let mut k = 0u32;
    loop {
        let mut offset = m.lw(off(entry, 4));
        if lt(offset, 0) {
            offset = offset.wrapping_add(3);
        }
        let word = sra(offset, 2) << 2;
        m.sw(out, table.wrapping_add(word));
        out = off(out, 4);
        let n = m.lw(table);
        k = k.wrapping_add(1);
        entry = off(entry, 4);
        if !lt(k, n) {
            return count;
        }
    }
}

// ---- sprite pools and the small allocator -----------------------------------

/// 800CD1A0: the sprite pools: a 0x24-byte record at [`POOLS`] (+0x18,
/// +0x1C, +0x20 cleared) with two pools, each 1000 SPRT packets (0x14
/// bytes: length 4, code 0x64, CLUT 0x41A8, 8x14) at +0 and 100 DR_MODE
/// packets (SetDrawMode with texture page 0x1B) at +0xC.
pub fn sprite_pools(m: &mut Machine) {
    framed(m, 0x38, |m, sp| {
        let record = arena::alloc(m, 0x24);
        m.sw(POOLS, record);
        m.sw(off(record, 0x18), 0);
        m.sw(off(record, 0x1c), 0);
        m.sw(off(record, 0x20), 0);
        for pool in 0..2u32 {
            let sprites = arena::alloc(m, 0x4e20);
            let slot = off(m.lw(POOLS), pool * 4);
            m.sw(slot, sprites);
            let modes = arena::alloc(m, 0x4b0);
            let slot = off(m.lw(POOLS), pool * 4);
            let sprites = m.lw(slot);
            m.sw(off(slot, 0xc), modes);
            let mut at = off(sprites, 0xe);
            for _ in 0..1000 {
                m.sb(off(at, 0xffff_fff5), 4);
                m.sb(off(at, 0xffff_fff9), 0x64);
                m.sh(off(at, 2), 8);
                m.sh(off(at, 4), 0xe);
                m.sh(at, 0x41a8);
                at = off(at, 0x14);
            }
            let slot = off(m.lw(POOLS), pool * 4);
            let mut mode = m.lw(off(slot, 0xc));
            for _ in 0..100 {
                m.sw(off(sp, 0x10), 0);
                call(m, 0x800a_c7f8, [mode, 0, 0, 0x1b]);
                mode = off(mode, 0xc);
            }
        }
    });
}

/// The remainder (HI) of a signed `div` with the compiler's `break 7`
/// (zero divisor, at `zero_break`) and `break 6` (`0x80000000 / -1`, at
/// `overflow_break`) checks: a reached trap records [`Fault::Trap`] and
/// yields the MIPS result.
pub(crate) fn rem_trap(
    m: &mut Machine,
    numerator: u32,
    divisor: u32,
    zero_break: u32,
    overflow_break: u32,
) -> u32 {
    let (n, d) = (numerator as i32, divisor as i32);
    if d == 0 {
        m.raise(Fault::Trap {
            address: zero_break,
            code: 7,
        });
        return numerator;
    }
    if d == -1 && n == i32::MIN {
        m.raise(Fault::Trap {
            address: overflow_break,
            code: 6,
        });
        return 0;
    }
    (n % d) as u32
}

/// `0x800DD288`: the proportional widths of the characters 0x21.. of
/// [`draw_text`].
const GLYPH_WIDTHS: u32 = 0x800d_d288;

/// One colour component of [`draw_text`]'s pulse (flag 2): the pool's
/// frame counter (+0x20) modulo 120, its distance from 60 scaling the
/// component `c` to `c + c * d / 60 - c / 2`.
fn pulse_component(m: &mut Machine, pools: u32, c: u32) -> u32 {
    const MAGIC: u32 = 0x8888_8889;
    let counter = m.lw(off(pools, 0x20));
    let q = sra(mulhi(counter, MAGIC).wrapping_add(counter), 6).wrapping_sub(sra(counter, 31));
    let mut d = counter
        .wrapping_sub(q.wrapping_mul(15).wrapping_mul(8))
        .wrapping_sub(0x3c);
    if lt(d, 0) {
        d = 0u32.wrapping_sub(d);
    }
    let v = d.wrapping_mul(c);
    let scaled = sra(mulhi(v, MAGIC).wrapping_add(v), 5).wrapping_sub(sra(v, 31));
    c.wrapping_add(scaled).wrapping_sub(c >> 1)
}

/// 800CD2F0 (no capture reaches it): draws `text` at (`x`, `y`) as SPRT
/// glyphs of the sprite pool ([`select_pool`]) into the ordering-table
/// entry `ot`, then a draw mode. Stack arguments (the caller's +0x10):
/// the colour (3 bytes), then the flags word (+0x14): bit 0 proportional
/// widths ([`GLYPH_WIDTHS`], else 6; spaces 4 or 6), bit 1 a pulse, bits
/// 2 and 3 a sweep by character (brighter before / at the counter's
/// position, modulo the length + 4). Characters outside 0x21..0x7A are
/// made spaces in the text; a newline restarts the line one pixel lower.
pub fn draw_text(m: &mut Machine, x: u32, y: u32, text: u32, ot: u32) {
    framed(m, 0x38, |m, sp| {
        let flags = m.lw(off(sp, 0x4c));
        let length = call(m, 0x800a_def8, [text, 0, 0, 0]);
        let mut at = text;
        let (mut column, mut line, mut index) = (0u32, 0u32, 0u32);
        let proportional = flags & 1;
        let period = sext16(length).wrapping_add(4);
        while lbu(m, at) != 0 {
            let c = lbu(m, at);
            if c == 0xa {
                column = 0;
                line = line.wrapping_add(1);
                at = off(at, 1);
                continue;
            }
            if c.wrapping_sub(0x21) >= 0x5a {
                m.sb(at, 0x20);
            }
            if lbu(m, at) == 0x20 {
                column = column.wrapping_add(if proportional == 0 { 6 } else { 4 });
                at = off(at, 1);
                index = index.wrapping_add(1);
                continue;
            }
            for k in 0..3 {
                let v = lbu(m, off(sp, 0x48 + k));
                m.sb(off(sp, 0x10 + k), v as u8);
            }
            if flags & 2 != 0 {
                let pools = m.lw(POOLS);
                for k in 0..3 {
                    let c = lbu(m, off(sp, 0x48 + k));
                    let v = pulse_component(m, pools, c);
                    m.sb(off(sp, 0x10 + k), v as u8);
                }
            }
            if flags & 4 != 0 {
                let pools = m.lw(POOLS);
                let mut counter = m.lw(off(pools, 0x20));
                if lt(counter, 0) {
                    counter = counter.wrapping_add(3);
                }
                let position = rem_trap(m, sra(counter, 2), period, 0x800c_d5a8, 0x800c_d5c0);
                let bright = lt(sext16(index), position);
                for k in 0..3 {
                    let c = lbu(m, off(sp, 0x48 + k));
                    let v = if bright { c << 1 } else { c >> 2 };
                    m.sb(off(sp, 0x10 + k), v as u8);
                }
            }
            if flags & 8 != 0 {
                let pools = m.lw(POOLS);
                let counter = m.lw(off(pools, 0x20));
                let fifth = sra(mulhi(counter, 0x6666_6667), 1).wrapping_sub(sra(counter, 31));
                let position = rem_trap(m, fifth, period, 0x800c_d670, 0x800c_d688);
                let dim = sext16(index) == position;
                for k in 0..3 {
                    let c = lbu(m, off(sp, 0x48 + k));
                    let v = if dim { c >> 2 } else { c << 1 };
                    m.sb(off(sp, 0x10 + k), v as u8);
                }
            }
            for k in 0..3 {
                let pools = m.lw(POOLS);
                let sprite = m.lw(off(pools, 8));
                let v = lbu(m, off(sp, 0x10 + k));
                m.sb(off(sprite, 4 + k), v as u8);
            }
            let pools = m.lw(POOLS);
            let sprite = m.lw(off(pools, 8));
            m.sh(off(sprite, 8), (column & 0xff).wrapping_add(x) as u16);
            let sprite = m.lw(off(pools, 8));
            m.sh(off(sprite, 0xa), (line & 0xff).wrapping_add(y) as u16);
            let c = lbu(m, at);
            let sprite = m.lw(off(pools, 8));
            let cell = c.wrapping_sub(0x41);
            let rounded = if lt(cell, 0) {
                c.wrapping_sub(0x22)
            } else {
                cell
            };
            let u = cell.wrapping_sub(sra(rounded, 5) << 5) << 3;
            m.sb(off(sprite, 0xc), u as u8);
            let c = lbu(m, at);
            let pools = m.lw(POOLS);
            let sprite = m.lw(off(pools, 8));
            let row = c.wrapping_sub(0x21);
            let row = if lt(row, 0) {
                sra(c.wrapping_sub(2), 5)
            } else {
                sra(row, 5)
            };
            m.sb(off(sprite, 0xd), (row.wrapping_mul(7) << 1) as u8);
            let pools = m.lw(POOLS);
            let sprite = m.lw(off(pools, 8));
            add_prim(m, ot, sprite);
            let next = m.lw(off(pools, 8)).wrapping_add(0x14);
            m.sw(off(pools, 8), next);
            column = if proportional == 0 {
                column.wrapping_add(6)
            } else {
                let c = lbu(m, at);
                let width = lbu(m, c.wrapping_add(GLYPH_WIDTHS).wrapping_sub(0x21));
                column.wrapping_add(width)
            };
            at = off(at, 1);
            index = index.wrapping_add(1);
        }
        let pools = m.lw(POOLS);
        let mode = m.lw(off(pools, 0x14));
        add_prim(m, ot, mode);
        let next = m.lw(off(pools, 0x14)).wrapping_add(0xc);
        m.sw(off(pools, 0x14), next);
    });
}

/// 800CD8C8: selects the sprite pool of the context's buffer index (+4):
/// pool record +8 = its sprites, +0x14 = its draw modes; counts +0x20.
pub fn select_pool(m: &mut Machine) {
    let ctx = context(m);
    let index = m.lw(off(ctx, 4)) << 2;
    let record = m.lw(POOLS);
    let sprites = m.lw(record.wrapping_add(index));
    m.sw(off(record, 8), sprites);
    let index = m.lw(off(ctx, 4)) << 2;
    let count = m.lw(off(record, 0x20));
    let modes = m.lw(off(record.wrapping_add(index), 0xc));
    m.sw(off(record, 0x20), count.wrapping_add(1));
    m.sw(off(record, 0x14), modes);
}

/// 800CD914: the small allocator starts at `base` (pointer and base),
/// total 0.
pub fn heap_reset(m: &mut Machine, base: u32) {
    m.sw(HEAP, base);
    m.sw(off(HEAP, 8), base);
    m.sw(off(HEAP, 0xc), 0);
}

/// 800CD930: `bytes` (rounded up to words) from the small allocator:
/// returns the block (also kept at +4), advances the pointer and the
/// total.
pub fn heap_alloc(m: &mut Machine, bytes: u32) -> u32 {
    let size = (bytes.wrapping_add(3) >> 2) << 2;
    let block = m.lw(HEAP);
    m.sw(off(HEAP, 4), block);
    let total = m.lw(off(HEAP, 0xc));
    m.sw(HEAP, block.wrapping_add(size));
    m.sw(off(HEAP, 0xc), total.wrapping_add(size));
    block
}

/// 800CD968: the small allocator's total.
pub fn heap_total(m: &mut Machine) -> u32 {
    m.lw(off(HEAP, 0xc))
}

/// 800CD978: gives `bytes` (rounded up to words) back to the small
/// allocator. Returns the new total.
pub fn heap_free(m: &mut Machine, bytes: u32) -> u32 {
    let size = (bytes.wrapping_add(3) >> 2) << 2;
    let total = m.lw(off(HEAP, 0xc)).wrapping_sub(size);
    let pointer = m.lw(HEAP).wrapping_sub(size);
    m.sw(off(HEAP, 0xc), total);
    m.sw(HEAP, pointer);
    total
}

/// The routines of this file by JP address, while module 1 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    let [a0, a1, a2, a3] = args;
    Some(match target {
        0x800c_c5d8 => setup(m),
        0x800c_c764 => frame(m),
        0x800c_c938 => rank_table(m, a0),
        0x800c_cb38 => pulse_tile(m, a0, a1),
        0x800c_cc90 => {
            title_open(m);
            0
        }
        0x800c_cd48 => {
            title_wait(m);
            0
        }
        0x800c_ce38 => offsets(m, a0, a1),
        0x800c_ce98 => load_begin(m),
        0x800c_cfa4 => load_done(m),
        0x800c_d07c => load_wait(m),
        0x800c_d1a0 => {
            sprite_pools(m);
            0
        }
        0x800c_d2f0 => {
            draw_text(m, a0, a1, a2, a3);
            0
        }
        0x800c_d8c8 => {
            select_pool(m);
            0
        }
        0x800c_d914 => {
            heap_reset(m, a0);
            0
        }
        0x800c_d930 => heap_alloc(m, a0),
        0x800c_d968 => heap_total(m),
        0x800c_d978 => heap_free(m, a0),
        _ => return None,
    })
}
