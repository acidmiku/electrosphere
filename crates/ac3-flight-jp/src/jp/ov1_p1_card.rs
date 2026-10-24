//! JP overlay 1, part 1: the memory-card layer's probe state, its BIOS
//! events and the save record (800D3CE8..800D4808).
//!
//! The card block is `[0x800DDA48]` ([`CARD`]): +4..+0x10 the four card
//! events (IOE, ERROR, TIMEOUT, NEW CARD) and +0x14..+0x20 the four
//! file events, +0x28 the changed flag, +0x2C/+0x30 the probe and file
//! steps of the two slots (halfwords), +0x2EC the per-slot save buffers,
//! +0x2F8 the record buffer, +0x314 the second record,
//! +0x348/+0x34A/+0x34C/+0x350 per-slot flags (bytes), +0x354 a word.
//! The save record ([`pack_record`], [`unpack_record`]) is 0x480 bytes:
//! +4 the context's +0x968, +8 the account name, +0x14 the account
//! byte, +0x18.. bit sets, +0x22.. four 60-halfword tables, +0x202..
//! nibble pairs, +0x27A.. 2-bit sets, +0x2C4 six 0x44-byte mission
//! records, +0x45C.. settings, +0x464 and +0x468 flags.
use super::ov1_p1::{MODULE, call, context, ctx_lw, display, framed, lbu, lh, lhu, off};
use crate::{common::loaded_module, machine::Machine};

/// `[0x800DDA48]`: the memory-card block.
pub const CARD: u32 = 0x800d_da48;
/// TestEvent (JP 800AF358, PAL 800AD788).
const TEST_EVENT: u32 = 0x800a_f358;
/// strcpy (JP 800ADEA8).
const STRCPY: u32 = 0x800a_dea8;

fn card(m: &mut Machine) -> u32 {
    m.lw(CARD)
}
fn card_lw(m: &mut Machine, offset: u32) -> u32 {
    let block = card(m);
    m.lw(off(block, offset))
}
/// An unaligned word copy (`lwl/lwr` + `swl/swr`), `words` words read
/// before any is written, as the original's register groups.
fn copy_words(m: &mut Machine, source: u32, destination: u32, words: usize) {
    let bytes = m.read_bytes(source, 4 * words);
    m.write_bytes(destination, &bytes);
}

/// 800D3CE8: resets the probe state of both slots: flags +0x34C/+0x34D
/// = 1 (probe pending), +0x348.., +0x34A.., +0x350.. cleared, the file
/// steps +0x30/+0x32 = 0, +0x354 = 0 and the changed flag +0x28 = 1.
/// Returns 1.
pub fn probe_reset(m: &mut Machine) -> u32 {
    let block = card(m);
    m.sb(off(block, 0x34c), 1);
    let v0 = card(m);
    m.sw(off(block, 0x28), 1);
    m.sb(off(v0, 0x348), 0);
    let v0 = card(m);
    m.sb(off(v0, 0x34a), 0);
    let v0 = card(m);
    m.sb(off(v0, 0x350), 0);
    let block = card(m);
    m.sh(off(v0, 0x30), 0);
    m.sb(off(block, 0x34d), 1);
    let v0 = card(m);
    m.sb(off(v0, 0x349), 0);
    let v0 = card(m);
    m.sb(off(v0, 0x34b), 0);
    let v0 = card(m);
    m.sb(off(v0, 0x351), 0);
    let v1 = card(m);
    m.sh(off(v0, 0x32), 0);
    m.sw(off(v1, 0x354), 0);
    1
}

/// 800D3D60: clears the changed flag; 1 when neither slot has a probe
/// pending (+0x34C, +0x34D), else 0.
pub fn probes_done(m: &mut Machine) -> u32 {
    let block = card(m);
    let first = lbu(m, off(block, 0x34c));
    m.sw(off(block, 0x28), 0);
    if first != 0 {
        return 0;
    }
    u32::from(lbu(m, off(block, 0x34d)) == 0)
}

/// 800D3D90: the state of `slot`: without a probed card (+0x348), 6 when
/// its probe step (+0x2C) is 2 (no card) else 0; with one, probe steps
/// 1 and 2 give 1 and 2; steps 0 and 3 continue with the file step
/// (+0x30): 1, 2 and 3 as such, and step 0 gives 5 or 4 by the slot's
/// format flag (+0x34A: 1 or 0; other values 1); other steps -1.
pub fn slot_state(m: &mut Machine, slot: u32) -> u32 {
    let block = card(m);
    if lbu(m, off(block, slot.wrapping_add(0x348))) == 0 {
        let probe = lh(m, off(block, (slot << 1).wrapping_add(0x2c)));
        return if probe == 2 { 6 } else { 0 };
    }
    let probe = lh(m, off(block, (slot << 1).wrapping_add(0x2c)));
    match probe {
        1 => return 1,
        2 => return 2,
        0 | 3 => {}
        _ => return u32::MAX,
    }
    let block = card(m);
    let file = lh(m, off(block, (slot << 1).wrapping_add(0x30)));
    match file {
        1 => 1,
        2 => 2,
        3 => 3,
        0 => match lbu(m, off(block, slot.wrapping_add(0x34a))) {
            1 => 5,
            0 => 4,
            _ => 1,
        },
        _ => u32::MAX,
    }
}

/// The first of four events (card block offsets `first`..) that has
/// fired (TestEvent 1): its index, with `value` stored to the slot's
/// +0x34C; -1 when none fired (all four are tested until one fires).
fn first_event(m: &mut Machine, first: u32, slot: u32, value: u32) -> u32 {
    framed(m, 0x28, |m, _| {
        for k in 0..4u32 {
            let event = card_lw(m, first + 4 * k);
            if call(m, TEST_EVENT, [event, 0, 0, 0]) == 1 {
                let at = card(m).wrapping_add(slot);
                m.sb(off(at, 0x34c), value as u8);
                return k;
            }
        }
        u32::MAX
    })
}

/// 800D3EA8: [`first_event`] of the four card events (+4..+0x10).
pub fn card_event(m: &mut Machine, slot: u32, value: u32) -> u32 {
    first_event(m, 4, slot, value)
}

/// 800D4060: [`first_event`] of the four file events (+0x14..+0x20).
pub fn file_event(m: &mut Machine, slot: u32, value: u32) -> u32 {
    first_event(m, 0x14, slot, value)
}

/// Tests (acknowledges) the four events from card block offset `first`.
fn clear_events(m: &mut Machine, first: u32) {
    framed(m, 0x18, |m, _| {
        for k in 0..4u32 {
            let event = card_lw(m, first + 4 * k);
            call(m, TEST_EVENT, [event, 0, 0, 0]);
        }
    });
}

/// 800D3F88: tests (acknowledges) the four card events.
pub fn clear_card_events(m: &mut Machine) {
    clear_events(m, 4);
}

/// 800D3FF4: tests (acknowledges) the four file events.
pub fn clear_file_events(m: &mut Machine) {
    clear_events(m, 0x14);
}

/// 800D4140: the context's frame counter (+0x20).
pub fn frame_counter(m: &mut Machine) -> u32 {
    ctx_lw(m, 0x20)
}

/// 800D4158: the account name from the record (+8) to the context
/// (+0x948). Returns strcpy's result.
pub fn name_from_record(m: &mut Machine) -> u32 {
    let record = card_lw(m, 0x2f8);
    let ctx = context(m);
    framed(m, 0x18, |m, _| {
        call(m, STRCPY, [off(ctx, 0x948), off(record, 8), 0, 0])
    })
}

/// 800D4190: the account name from the context (+0x948) to the record
/// (+8). Returns strcpy's result.
pub fn name_to_record(m: &mut Machine) -> u32 {
    let record = card_lw(m, 0x2f8);
    let ctx = context(m);
    framed(m, 0x18, |m, _| {
        call(m, STRCPY, [off(record, 8), off(ctx, 0x948), 0, 0])
    })
}

/// 800D41C8: packs the context's progress into the record buffer
/// (`[CARD]+0x2F8`; inverse of [`unpack_record`]).
pub fn pack_record(m: &mut Machine) {
    framed(m, 8, |m, _| {
        let ctx = context(m);
        let record = card_lw(m, 0x2f8);
        let word = m.lw(off(ctx, 0x968));
        let progress = off(ctx, 0x95c);
        m.sw(off(record, 4), word);
        let (mut source, mut group) = (0u32, 0x2c4u32);
        for _ in 0..6 {
            let to = record.wrapping_add(group);
            let ctx = context(m);
            for k in 0..20u32 {
                let v = lhu(m, ctx.wrapping_add(source + 0x16 + 2 * k + 0x15c));
                m.sh(to.wrapping_add(2 * k), v as u16);
            }
            let ctx = context(m);
            let from = ctx.wrapping_add(source);
            copy_words(m, off(from, 0x19c), off(to, 0x28), 4);
            copy_words(m, off(from, 0x1ac), off(to, 0x38), 2);
            let v = m.lw(off(from, 0x1b4));
            m.sw(off(to, 0x40), v);
            let flag = if m.lw(off(ctx, 0x120)) & 4 != 0 { 0 } else { 1 };
            m.sb(off(record, 0x464), flag);
            source += 0x5c;
            group += 0x44;
        }
        for (count, to, from) in [(8u32, 0x1au32, 0x18u32), (2, 0x18, 0x58)] {
            for i in 0..count {
                let at = off(record, i);
                m.sb(off(at, to), 0);
                for j in 0..8u32 {
                    let bit = lbu(m, off(progress, i * 8 + j + from)) & 1;
                    let v = lbu(m, off(at, to)) | (bit << j);
                    m.sb(off(at, to), v as u8);
                }
            }
        }
        for k in 0..0x3cu32 {
            let (from, to) = (off(progress, 2 * k), off(record, 2 * k));
            for (src, dst) in [(0x68, 0x22), (0xe0, 0x9a), (0x158, 0x112), (0x1d0, 0x18a)] {
                let v = lhu(m, off(from, src));
                m.sh(off(to, dst), v as u16);
            }
        }
        for k in 0..0x1eu32 {
            let (low, to) = (off(progress, 2 * k), off(record, k));
            for (src, dst) in [
                (0x2c0, 0x202),
                (0x2fc, 0x220),
                (0x338, 0x23e),
                (0x374, 0x25c),
            ] {
                let a = lbu(m, off(low, src)) & 0xf;
                let b = lbu(m, off(low, src + 1)) & 0xf;
                m.sb(off(to, dst), (a | (b << 4)) as u8);
            }
        }
        for (count, to, from) in [(0x14u32, 0x27au32, 0x3ecu32), (0x2d, 0x28e, 0x43c)] {
            for i in 0..count {
                let at = off(record, i);
                m.sb(off(at, to), 0);
                for j in 0..4u32 {
                    let pair = lbu(m, off(progress, i * 4 + j + from)) & 3;
                    let v = lbu(m, off(at, to)) | (pair << (j * 2));
                    m.sb(off(at, to), v as u8);
                }
            }
        }
        let request = display(m);
        let v = lhu(m, off(request, 8));
        m.sh(off(record, 0x460), v as u16);
        let v = lhu(m, off(request, 0xc));
        m.sh(off(record, 0x462), v as u16);
        for (src, dst) in [(0x398, 0x45d), (0x399, 0x45e), (0x39a, 0x45c)] {
            let ctx = context(m);
            let v = lbu(m, off(ctx, src));
            m.sb(off(record, dst), v as u8);
        }
        let v = lhu(m, off(progress, 0x6d4));
        let ctx = context(m);
        m.sh(off(record, 0x2c0), v as u16);
        let v = lhu(m, off(progress, 0x6d8));
        m.sh(off(record, 0x2c2), v as u16);
        let v = lbu(m, off(progress, 1));
        m.sw(off(record, 0x468), 0);
        m.sh(off(record, 0x14), v as u16);
        if lbu(m, off(ctx, 0x39c)) & 1 != 0 {
            m.sw(off(record, 0x468), 1);
        }
        for (bit, flag) in [(1, 2), (2, 4)] {
            if ctx_lw(m, 0x120) & bit != 0 {
                let v = m.lw(off(record, 0x468)) | flag;
                m.sw(off(record, 0x468), v);
            }
        }
        let ctx = context(m);
        if lbu(m, off(ctx, 0x95a)) == 1 {
            let v = m.lw(off(record, 0x468)) | 8;
            m.sw(off(record, 0x468), v);
        }
        for k in (0..=0x10u32).rev() {
            m.sb(off(record, 0x46c + k), 0);
        }
    });
}

/// 800D4658 (no caller in the captures): prints the second record's
/// address ("pMcrd->my_data : %8x") and packs the context's progress
/// into it (`[CARD]+0x314`).
pub fn pack_second_record(m: &mut Machine) {
    framed(m, 0x20, |m, _| {
        let progress = off(context(m), 0x95c);
        let record = card_lw(m, 0x314);
        call(m, 0x800a_dfb8, [0x800c_c470, record, 0, 0]);
        let v = m.lw(off(progress, 0x6d0));
        let ctx = context(m);
        m.sw(off(record, 0x114), v);
        let v = lbu(m, off(ctx, 0x38c));
        m.sb(off(record, 0x113), v as u8);
        let ctx = context(m);
        let v = lbu(m, off(ctx, 0x394));
        m.sb(off(record, 0x112), v as u8);
        for k in 0..0x3cu32 {
            let v = lhu(m, off(progress, 0x248 + 2 * k));
            m.sh(off(record, 4 + 2 * k), v as u16);
            let v = lbu(m, off(progress, 0x4f0 + 4 * k));
            m.sb(off(record, 0x9a + k), v as u8);
            let v = lbu(m, off(progress, 0x5e0 + 4 * k));
            m.sb(off(record, 0xd6 + k), v as u8);
            for (src, dst) in [
                (0x38e, 0x118),
                (0x3a9, 0x119),
                (0x3aa, 0x11a),
                (0x3ab, 0x11b),
            ] {
                let ctx = context(m);
                let v = lbu(m, off(ctx, src));
                m.sb(off(record, dst), v as u8);
            }
        }
        for k in 0..0x1eu32 {
            let a = lbu(m, off(progress, 0x3b0 + 2 * k)) & 0xf;
            let b = lbu(m, off(progress, 0x3b0 + 2 * k + 1)) & 0xf;
            m.sb(off(record, 0x7c + k), (a | (b << 4)) as u8);
        }
        for k in (0..=0x63u32).rev() {
            m.sb(off(record, 0x11c + k), 0);
        }
    });
}

/// 800D47B8: copies the save buffer of `slot` (`[CARD]+0x2EC+4*slot`,
/// 0x480 bytes, 16 at a time) into the record buffer (+0x2F8).
pub fn record_from_slot(m: &mut Machine, slot: u32) {
    let block = card(m);
    let mut from = m.lw(block.wrapping_add(slot << 2).wrapping_add(0x2ec));
    let mut to = m.lw(off(block, 0x2f8));
    let end = off(from, 0x480);
    loop {
        let words = [
            m.lw(from),
            m.lw(off(from, 4)),
            m.lw(off(from, 8)),
            m.lw(off(from, 0xc)),
        ];
        for (k, w) in words.into_iter().enumerate() {
            m.sw(off(to, 4 * k as u32), w);
        }
        from = off(from, 0x10);
        to = off(to, 0x10);
        if from == end {
            break;
        }
    }
}

/// 800D4808: unpacks the record buffer into the context (inverse of
/// [`pack_record`]); `out` takes the account byte (+0x14). Applies the
/// stored settings: screen offset (display +8/+0xC), sound settings
/// (context +0x398..+0x39A; CD stereo by bit 0 of +0x39A, 8002BD28 and
/// 8002BD5C), flags (+0x120 bits 0..2, +0x39C bit 0, +0x95A); display
/// flag 2.
pub fn unpack_record(m: &mut Machine, out: u32) {
    framed(m, 0x28, |m, _| {
        let record = card_lw(m, 0x2f8);
        let ctx = context(m);
        let account = lbu(m, off(record, 0x14));
        let progress = off(ctx, 0x95c);
        m.sb(out, account as u8);
        let source = card_lw(m, 0x2f8);
        let ctx = context(m);
        call(m, STRCPY, [off(ctx, 0x948), off(source, 8), 0, 0]);
        let source = card_lw(m, 0x2f8);
        let ctx = context(m);
        let word = m.lw(off(source, 4));
        m.sw(off(ctx, 0x968), word);
        for k in 0..0x3cu32 {
            let (from, to) = (off(record, 2 * k), off(progress, 2 * k));
            for (src, dst) in [(0x22, 0x68), (0x9a, 0xe0), (0x112, 0x158), (0x18a, 0x1d0)] {
                let v = lhu(m, off(from, src));
                m.sh(off(to, dst), v as u16);
            }
        }
        for k in 0..0x1eu32 {
            let from = off(record, k);
            for n in 0..2u32 {
                let to = off(progress, 2 * k + n);
                for (src, dst) in [
                    (0x202, 0x2c0),
                    (0x220, 0x2fc),
                    (0x23e, 0x338),
                    (0x25c, 0x374),
                ] {
                    let v = (lbu(m, off(from, src)) >> (4 * n)) & 0xf;
                    m.sb(off(to, dst), v as u8);
                }
            }
        }
        let v = lh(m, off(record, 0x2c0));
        m.sw(off(progress, 0x6d4), v);
        let v = lh(m, off(record, 0x2c2));
        m.sw(off(progress, 0x6d8), v);
        let v = lbu(m, off(record, 0x14));
        m.sb(off(progress, 1), v as u8);
        let (mut target, mut group) = (0u32, 0x2c4u32);
        for _ in 0..6 {
            let from = record.wrapping_add(group);
            let ctx = context(m);
            for k in 0..20u32 {
                let v = lhu(m, off(from, 2 * k));
                m.sh(ctx.wrapping_add(target + 0x16 + 2 * k + 0x15c), v as u16);
            }
            let ctx = context(m);
            let to = ctx.wrapping_add(target);
            copy_words(m, off(from, 0x28), off(to, 0x19c), 4);
            copy_words(m, off(from, 0x38), off(to, 0x1ac), 2);
            let v = m.lw(off(from, 0x40));
            m.sw(off(to, 0x1b4), v);
            target += 0x5c;
            group += 0x44;
        }
        let ctx = context(m);
        let flags = m.lw(off(ctx, 0x120));
        if lbu(m, off(record, 0x464)) != 0 {
            m.sw(off(ctx, 0x120), flags & !4);
        } else {
            m.sw(off(ctx, 0x120), flags | 4);
        }
        let block = card(m);
        let source = m.lw(off(block, 0x2f8));
        let request = display(m);
        let v = lh(m, off(source, 0x460));
        m.sw(off(request, 8), v);
        let source = m.lw(off(block, 0x2f8));
        let v = lh(m, off(source, 0x462));
        m.sw(off(request, 0xc), v);
        for (src, dst) in [(0x45d, 0x398), (0x45e, 0x399), (0x45c, 0x39a)] {
            let ctx = context(m);
            let v = lbu(m, off(record, src));
            m.sb(off(ctx, dst), v as u8);
        }
        let ctx = context(m);
        let stereo = u32::from(lbu(m, off(ctx, 0x39a)) & 1 != 0);
        call(m, 0x8002_bd28, [stereo, 0, 0, 0]);
        call(m, 0x8002_bd5c, [0, 0, 0, 0]);
        for (count, from, to) in [(8u32, 0x1au32, 0x18u32), (2, 0x18, 0x58)] {
            for i in 0..count {
                for j in 0..8u32 {
                    let v = (lbu(m, off(record, i + from)) >> j) & 1;
                    m.sb(off(progress, i * 8 + j + to), v as u8);
                }
            }
        }
        for (count, from, to) in [(0x14u32, 0x27au32, 0x3ecu32), (0x2d, 0x28e, 0x43c)] {
            for i in 0..count {
                for j in 0..4u32 {
                    let v = (lbu(m, off(record, i + from)) >> (j * 2)) & 3;
                    m.sb(off(progress, i * 4 + j + to), v as u8);
                }
            }
        }
        let bits = m.lw(off(record, 0x468));
        let ctx = context(m);
        let v = lbu(m, off(ctx, 0x39c));
        let v = if bits & 1 != 0 { v | 1 } else { v & 0xfe };
        m.sb(off(ctx, 0x39c), v as u8);
        for (bit, flag) in [(2u32, 1u32), (4, 2)] {
            let bits = m.lw(off(record, 0x468));
            let ctx = context(m);
            let v = m.lw(off(ctx, 0x120));
            let v = if bits & bit != 0 { v | flag } else { v & !flag };
            m.sw(off(ctx, 0x120), v);
        }
        let bits = m.lw(off(record, 0x468));
        let ctx = context(m);
        m.sb(off(ctx, 0x95a), u8::from(bits & 8 != 0));
        let request = display(m);
        let v = m.lw(request);
        m.sw(request, v | 2);
    });
}

/// The routines of this file by JP address, while module 1 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    let [a0, a1, ..] = args;
    Some(match target {
        0x800d_3ce8 => probe_reset(m),
        0x800d_3d60 => probes_done(m),
        0x800d_3d90 => slot_state(m, a0),
        0x800d_3ea8 => card_event(m, a0, a1),
        0x800d_3f88 => {
            clear_card_events(m);
            0
        }
        0x800d_3ff4 => {
            clear_file_events(m);
            0
        }
        0x800d_4060 => file_event(m, a0, a1),
        0x800d_4140 => frame_counter(m),
        0x800d_4158 => name_from_record(m),
        0x800d_4190 => name_to_record(m),
        0x800d_41c8 => {
            pack_record(m);
            0
        }
        0x800d_4658 => {
            pack_second_record(m);
            0
        }
        0x800d_47b8 => {
            record_from_slot(m, a0);
            0
        }
        0x800d_4808 => {
            unpack_record(m, a0);
            0
        }
        _ => return None,
    })
}
