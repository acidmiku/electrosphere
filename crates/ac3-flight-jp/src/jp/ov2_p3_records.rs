//! JP overlay 2, part 3: the save records of the JP card file and their
//! accessors.
//!
//! The card block (`[0x80112DF0]`, see `ov2_p3_card`) holds the records:
//! +0x2E8 the 0x80 record (+4 version 0x00990426, +8 present-account bits,
//! +0xC account count), +0x2EC.. three 0x480 account records and +0x2F8 the
//! current one (+4 the context word +0x968, +8 the name, +0x14 / +0x16
//! slot and present-record bits, +0x18.. the packed progress of context
//! +0x95C: bit fields, nibbles and 2-bit fields, +0x2C4 six 0x44-byte
//! mission entries, +0x45C.. settings, +0x468 flags), +0x2FC.. six 0x180
//! records and +0x314 the current one (+0x112 / +0x113 / +0x114 the
//! record's unit, level and time, +0x7C.. its packed scores). The context
//! is `[gp+0x2EC]`; `[gp+0x2D4]` (0x800BCE48) is the resident settings
//! block.
use super::ov2_p3::{MODULE, call5};
use super::ov2_p3_card::{CARD, HEAD, SPRINTF, STRCPY, VERSION, card};
use crate::common::{CONTEXT, loaded_module};
use crate::dispatch::call;
use crate::machine::{Machine, SmallData};

/// 0x800BCE48 (`[gp+0x2D4]`): the resident settings block.
const SETTINGS: u32 = 0x800b_ce48;
/// printf and strcmp (resident, JP).
const PRINTF: u32 = 0x800a_dfb8;
const STRCMP: u32 = 0x800a_de38;
/// Resident routines (`mainloop`): 8002BD28 set the CD audio stereo mode,
/// 8002BD5C apply the CD audio volume.
const SET_CD_STEREO: u32 = 0x8002_bd28;
const APPLY_CD_VOLUME: u32 = 0x8002_bd5c;
/// Another part of overlay 2: 800ECD4C (the record tables of a mission).
const MISSION_TABLES: u32 = 0x800e_cd4c;
/// 0x800CCD1C "pNetMcrd->my_data : %8x\n".
const DEBUG_FORMAT: u32 = 0x800c_cd1c;
/// 0x800CCD38 "SC", 0x800CCD3C the 32-byte head template, 0x800CCD5C its
/// 7-byte tail, 0x800CCD64 "CRD0": the card file head.
const HEAD_MAGIC: u32 = 0x800c_cd38;
const HEAD_TEMPLATE: u32 = 0x800c_cd3c;
const HEAD_CRD0: u32 = 0x800c_cd64;
/// 0x800CCD6C "Nemo": the default account name; 0x800CCD74 "empty
/// account"; 0x800CCD84 / 0x800CCD9C the two save file names; 0x800CCDB4
/// the empty string; 0x800CCDDC "%3d:%2d:%2d".
const DEFAULT_NAME: u32 = 0x800c_cd6c;
const EMPTY_ACCOUNT: u32 = 0x800c_cd74;
const NAME_PORT_0: u32 = 0x800c_cd84;
const NAME_PORT_1: u32 = 0x800c_cd9c;
const EMPTY: u32 = 0x800c_cdb4;
const TIME_FORMAT: u32 = 0x800c_cddc;
/// 0x80112DE0: the time text buffer.
const TIME_TEXT: u32 = 0x8011_2de0;
/// Overlay data tables of the record texts: units (0x8010B1B8), names
/// (0x8010B06C), levels (by 0x8010B178, texts 0x8010B160), settings
/// (0x8010B1C8).
const UNIT_TEXTS: u32 = 0x8010_b1b8;
const NAME_TEXTS: u32 = 0x8010_b06c;
const LEVEL_OF: u32 = 0x8010_b178;
const LEVEL_TEXTS: u32 = 0x8010_b160;
const SETTING_TEXTS: u32 = 0x8010_b1c8;

fn context(m: &mut Machine) -> u32 {
    m.gw(CONTEXT)
}

/// Copy `n` bytes byte by byte (the `lwl/lwr` + `swl/swr` groups of the
/// original; source and destination never overlap).
fn copy_bytes(m: &mut Machine, dst: u32, src: u32, n: u32) {
    for i in 0..n {
        let b = m.lbu(src.wrapping_add(i));
        m.sb(dst.wrapping_add(i), b);
    }
}

/// 800E2AB4: pack the context's progress into the current 0x480 record.
pub fn pack_slot_e2ab4(m: &mut Machine) {
    m.push_frame(8);
    let c = card(m);
    let ctx = context(m);
    let record = m.lw(c.wrapping_add(0x2f8));
    let word = m.lw(ctx.wrapping_add(0x968));
    let progress = ctx.wrapping_add(0x95c);
    m.sw(record.wrapping_add(4), word);
    let mut source = 0u32;
    let mut entry = 0x2c4u32;
    for _ in 0..6 {
        let to = record.wrapping_add(entry);
        let ctx = context(m);
        let mut from = source.wrapping_add(0x16);
        let mut out = 0u32;
        for _ in 0xb..0x1f {
            let v = m.lhu(ctx.wrapping_add(from).wrapping_add(0x15c));
            from = from.wrapping_add(2);
            m.sh(to.wrapping_add(out), v);
            out = out.wrapping_add(2);
        }
        let ctx = context(m);
        let base = ctx.wrapping_add(source);
        copy_bytes(m, to.wrapping_add(0x28), base.wrapping_add(0x19c), 0x10);
        copy_bytes(m, to.wrapping_add(0x38), base.wrapping_add(0x1ac), 8);
        let v = m.lw(base.wrapping_add(0x1b4));
        m.sw(to.wrapping_add(0x40), v);
        if m.lw(ctx.wrapping_add(0x120)) & 4 != 0 {
            m.sb(record.wrapping_add(0x464), 0);
        } else {
            m.sb(record.wrapping_add(0x464), 1);
        }
        source = source.wrapping_add(0x5c);
        entry = entry.wrapping_add(0x44);
    }
    pack_bits(m, record.wrapping_add(0x1a), progress.wrapping_add(0x18), 8);
    pack_bits(m, record.wrapping_add(0x18), progress.wrapping_add(0x58), 2);
    for k in 0..0x3cu32 {
        let at = progress + 2 * k;
        let to = record + 2 * k;
        let v = m.lhu(at.wrapping_add(0x68));
        m.sh(to.wrapping_add(0x22), v);
        let v = m.lhu(at.wrapping_add(0xe0));
        m.sh(to.wrapping_add(0x9a), v);
        let v = m.lhu(at.wrapping_add(0x158));
        m.sh(to.wrapping_add(0x112), v);
        let v = m.lhu(at.wrapping_add(0x1d0));
        m.sh(to.wrapping_add(0x18a), v);
    }
    for k in 0..0x1eu32 {
        let to = record + k;
        for (from, into) in [
            (0x2c0, 0x202),
            (0x2fc, 0x220),
            (0x338, 0x23e),
            (0x374, 0x25c),
        ] {
            let low = m.lbu(progress.wrapping_add(2 * k).wrapping_add(from)) & 0xf;
            let high = m.lbu(
                progress
                    .wrapping_add(2 * k)
                    .wrapping_add(1)
                    .wrapping_add(from),
            ) & 0xf;
            m.sb(to.wrapping_add(into), low | (high << 4));
        }
    }
    pack_pairs(
        m,
        record.wrapping_add(0x27a),
        progress.wrapping_add(0x3ec),
        0x14,
    );
    pack_pairs(
        m,
        record.wrapping_add(0x28e),
        progress.wrapping_add(0x43c),
        0x2d,
    );
    let settings = m.lw(SETTINGS);
    let v = m.lhu(settings.wrapping_add(8));
    m.sh(record.wrapping_add(0x460), v);
    let v = m.lhu(settings.wrapping_add(0xc));
    let ctx = context(m);
    m.sh(record.wrapping_add(0x462), v);
    let b = m.lbu(ctx.wrapping_add(0x398));
    m.sb(record.wrapping_add(0x45d), b);
    let ctx = context(m);
    let b = m.lbu(ctx.wrapping_add(0x399));
    m.sb(record.wrapping_add(0x45e), b);
    let ctx = context(m);
    let b = m.lbu(ctx.wrapping_add(0x39a));
    m.sb(record.wrapping_add(0x45c), b);
    let v = m.lhu(progress.wrapping_add(0x6d4));
    let ctx = context(m);
    m.sh(record.wrapping_add(0x2c0), v);
    let v = m.lhu(progress.wrapping_add(0x6d8));
    m.sh(record.wrapping_add(0x2c2), v);
    let b = m.lbu(progress.wrapping_add(1));
    m.sw(record.wrapping_add(0x468), 0);
    m.sh(record.wrapping_add(0x14), u16::from(b));
    if m.lbu(ctx.wrapping_add(0x39c)) & 1 != 0 {
        m.sw(record.wrapping_add(0x468), 1);
    }
    let ctx = context(m);
    if m.lw(ctx.wrapping_add(0x120)) & 1 != 0 {
        let f = m.lw(record.wrapping_add(0x468));
        m.sw(record.wrapping_add(0x468), f | 2);
    }
    let ctx = context(m);
    if m.lw(ctx.wrapping_add(0x120)) & 2 != 0 {
        let f = m.lw(record.wrapping_add(0x468));
        m.sw(record.wrapping_add(0x468), f | 4);
    }
    let ctx = context(m);
    if m.lbu(ctx.wrapping_add(0x95a)) == 1 {
        let f = m.lw(record.wrapping_add(0x468));
        m.sw(record.wrapping_add(0x468), f | 8);
    }
    for i in (0..=0x10u32).rev() {
        m.sb(record.wrapping_add(i).wrapping_add(0x46c), 0);
    }
    m.pop_frame(8);
}

/// Pack `bytes` groups of eight 1-bit flags (`from + 8 j + k`, bit 0) into
/// the bytes `into + j` (cleared first, OR-ed bit by bit).
fn pack_bits(m: &mut Machine, into: u32, from: u32, bytes: u32) {
    for j in 0..bytes {
        m.sb(into.wrapping_add(j), 0);
        for k in 0..8 {
            let bit = m.lbu(from.wrapping_add(j * 8).wrapping_add(k)) & 1;
            let v = m.lbu(into.wrapping_add(j));
            m.sb(into.wrapping_add(j), v | (bit << k));
        }
    }
}

/// Pack `bytes` groups of four 2-bit fields (`from + 4 j + k`) into the
/// bytes `into + j`.
fn pack_pairs(m: &mut Machine, into: u32, from: u32, bytes: u32) {
    for j in 0..bytes {
        m.sb(into.wrapping_add(j), 0);
        for k in 0..4 {
            let field = m.lbu(from.wrapping_add(j * 4).wrapping_add(k)) & 3;
            let v = m.lbu(into.wrapping_add(j));
            m.sb(into.wrapping_add(j), v | (field << (k * 2)));
        }
    }
}

/// 800E2F44: pack the context's current mission record into the current
/// 0x180 record (+0x314), after printing its address (debug).
pub fn pack_record_e2f44(m: &mut Machine) {
    m.push_frame(0x20);
    let ctx = context(m);
    let progress = ctx.wrapping_add(0x95c);
    let c = card(m);
    let record = m.lw(c.wrapping_add(0x314));
    call(m, PRINTF, [DEBUG_FORMAT, record, 0, 0]);
    let v = m.lw(progress.wrapping_add(0x6d0));
    let ctx = context(m);
    m.sw(record.wrapping_add(0x114), v);
    let b = m.lbu(ctx.wrapping_add(0x38c));
    m.sb(record.wrapping_add(0x113), b);
    let ctx = context(m);
    let b = m.lbu(ctx.wrapping_add(0x394));
    m.sb(record.wrapping_add(0x112), b);
    for k in 0..0x3cu32 {
        let v = m.lhu(progress.wrapping_add(2 * k).wrapping_add(0x248));
        m.sh(record.wrapping_add(2 * k).wrapping_add(4), v);
        let b = m.lbu(progress.wrapping_add(4 * k).wrapping_add(0x4f0));
        m.sb(record.wrapping_add(k).wrapping_add(0x9a), b);
        let b = m.lbu(progress.wrapping_add(4 * k).wrapping_add(0x5e0));
        m.sb(record.wrapping_add(k).wrapping_add(0xd6), b);
        for (from, into) in [
            (0x38e, 0x118),
            (0x3a9, 0x119),
            (0x3aa, 0x11a),
            (0x3ab, 0x11b),
        ] {
            let ctx = context(m);
            let b = m.lbu(ctx.wrapping_add(from));
            m.sb(record.wrapping_add(into), b);
        }
    }
    for k in 0..0x1eu32 {
        let low = m.lbu(progress.wrapping_add(2 * k).wrapping_add(0x3b0)) & 0xf;
        let high = m.lbu(
            progress
                .wrapping_add(2 * k)
                .wrapping_add(1)
                .wrapping_add(0x3b0),
        ) & 0xf;
        m.sb(record.wrapping_add(k).wrapping_add(0x7c), low | (high << 4));
    }
    for i in (0..=0x63u32).rev() {
        m.sb(record.wrapping_add(i).wrapping_add(0x11c), 0);
    }
    m.pop_frame(0x20);
}

/// 800E30A4: copy account record `account` (0x480 bytes) into the current
/// one.
pub fn select_slot_e30a4(m: &mut Machine, account: u32) {
    let c = card(m);
    let mut from = m.lw(c.wrapping_add(account << 2).wrapping_add(0x2ec));
    let mut to = m.lw(c.wrapping_add(0x2f8));
    let end = from.wrapping_add(0x480);
    loop {
        let w: [u32; 4] = std::array::from_fn(|i| m.lw(from.wrapping_add(4 * i as u32)));
        for (i, w) in w.into_iter().enumerate() {
            m.sw(to.wrapping_add(4 * i as u32), w);
        }
        from = from.wrapping_add(0x10);
        to = to.wrapping_add(0x10);
        if from == end {
            break;
        }
    }
}

/// 800E30F4: unpack the current 0x480 record into the context (the inverse
/// of 800E2AB4), store its slot byte (+0x14) at `out`, and apply its
/// settings (resident 8002BD28 / 8002BD5C).
pub fn unpack_slot_e30f4(m: &mut Machine, out: u32) {
    m.push_frame(0x28);
    let c = card(m);
    let record = m.lw(c.wrapping_add(0x2f8));
    let ctx = context(m);
    let b = m.lbu(record.wrapping_add(0x14));
    let progress = ctx.wrapping_add(0x95c);
    m.sb(out, b);
    let c = card(m);
    let ctx = context(m);
    let current = m.lw(c.wrapping_add(0x2f8));
    call(
        m,
        STRCPY,
        [ctx.wrapping_add(0x948), current.wrapping_add(8), 0, 0],
    );
    let c = card(m);
    let current = m.lw(c.wrapping_add(0x2f8));
    let ctx = context(m);
    let word = m.lw(current.wrapping_add(4));
    m.sw(ctx.wrapping_add(0x968), word);
    for k in 0..0x3cu32 {
        let at = record + 2 * k;
        let to = progress + 2 * k;
        let v = m.lhu(at.wrapping_add(0x22));
        m.sh(to.wrapping_add(0x68), v);
        let v = m.lhu(at.wrapping_add(0x9a));
        m.sh(to.wrapping_add(0xe0), v);
        let v = m.lhu(at.wrapping_add(0x112));
        m.sh(to.wrapping_add(0x158), v);
        let v = m.lhu(at.wrapping_add(0x18a));
        m.sh(to.wrapping_add(0x1d0), v);
    }
    for k in 0..0x1eu32 {
        for n in 0..2u32 {
            let to = progress + 2 * k + n;
            for (from, into) in [
                (0x202, 0x2c0),
                (0x220, 0x2fc),
                (0x23e, 0x338),
                (0x25c, 0x374),
            ] {
                let b = u32::from(m.lbu(record.wrapping_add(k).wrapping_add(from)));
                m.sb(to.wrapping_add(into), ((b >> (n * 4)) & 0xf) as u8);
            }
        }
    }
    let v = m.lh(record.wrapping_add(0x2c0)) as i32 as u32;
    m.sw(progress.wrapping_add(0x6d4), v);
    let v = m.lh(record.wrapping_add(0x2c2)) as i32 as u32;
    m.sw(progress.wrapping_add(0x6d8), v);
    let b = m.lbu(record.wrapping_add(0x14));
    m.sb(progress.wrapping_add(1), b);
    let mut source = 0u32;
    let mut entry = 0x2c4u32;
    for _ in 0..6 {
        let from = record.wrapping_add(entry);
        let ctx = context(m);
        let mut to = source.wrapping_add(0x16);
        let mut at = 0u32;
        for _ in 0xb..0x1f {
            let v = m.lhu(from.wrapping_add(at));
            at = at.wrapping_add(2);
            m.sh(ctx.wrapping_add(to).wrapping_add(0x15c), v);
            to = to.wrapping_add(2);
        }
        entry = entry.wrapping_add(0x44);
        let ctx = context(m);
        let base = ctx.wrapping_add(source);
        copy_bytes(m, base.wrapping_add(0x19c), from.wrapping_add(0x28), 0x10);
        copy_bytes(m, base.wrapping_add(0x1ac), from.wrapping_add(0x38), 8);
        let v = m.lw(from.wrapping_add(0x40));
        m.sw(base.wrapping_add(0x1b4), v);
        source = source.wrapping_add(0x5c);
    }
    let ctx = context(m);
    let flags = m.lw(ctx.wrapping_add(0x120));
    if m.lbu(record.wrapping_add(0x464)) == 0 {
        m.sw(ctx.wrapping_add(0x120), flags | 4);
    } else {
        m.sw(ctx.wrapping_add(0x120), flags & !4);
    }
    let c = card(m);
    let current = m.lw(c.wrapping_add(0x2f8));
    let settings = m.lw(SETTINGS);
    let v = m.lh(current.wrapping_add(0x460)) as i32 as u32;
    m.sw(settings.wrapping_add(8), v);
    let current = m.lw(c.wrapping_add(0x2f8));
    let v = m.lh(current.wrapping_add(0x462)) as i32 as u32;
    m.sw(settings.wrapping_add(0xc), v);
    for (from, into) in [(0x45d, 0x398), (0x45e, 0x399), (0x45c, 0x39a)] {
        let ctx = context(m);
        let b = m.lbu(record.wrapping_add(from));
        m.sb(ctx.wrapping_add(into), b);
    }
    let ctx = context(m);
    let stereo = u32::from(m.lbu(ctx.wrapping_add(0x39a)) & 1 != 0);
    call(m, SET_CD_STEREO, [stereo, 0, 0, 0]);
    call(m, APPLY_CD_VOLUME, [0, 0, 0, 0]);
    for j in 0..8u32 {
        for k in 0..8u32 {
            let b = u32::from(m.lbu(record.wrapping_add(j).wrapping_add(0x1a)));
            m.sb(
                progress
                    .wrapping_add(j * 8)
                    .wrapping_add(k)
                    .wrapping_add(0x18),
                ((b >> k) & 1) as u8,
            );
        }
    }
    for j in 0..2u32 {
        for k in 0..8u32 {
            let b = u32::from(m.lbu(record.wrapping_add(j).wrapping_add(0x18)));
            m.sb(
                progress
                    .wrapping_add(j * 8)
                    .wrapping_add(k)
                    .wrapping_add(0x58),
                ((b >> k) & 1) as u8,
            );
        }
    }
    for j in 0..0x14u32 {
        for k in 0..4u32 {
            let b = u32::from(m.lbu(record.wrapping_add(j).wrapping_add(0x27a)));
            m.sb(
                progress
                    .wrapping_add(j * 4)
                    .wrapping_add(k)
                    .wrapping_add(0x3ec),
                ((b >> (k * 2)) & 3) as u8,
            );
        }
    }
    for j in 0..0x2du32 {
        for k in 0..4u32 {
            let b = u32::from(m.lbu(record.wrapping_add(j).wrapping_add(0x28e)));
            m.sb(
                progress
                    .wrapping_add(j * 4)
                    .wrapping_add(k)
                    .wrapping_add(0x43c),
                ((b >> (k * 2)) & 3) as u8,
            );
        }
    }
    let ctx = context(m);
    let b = m.lbu(ctx.wrapping_add(0x39c));
    if m.lw(record.wrapping_add(0x468)) & 1 != 0 {
        m.sb(ctx.wrapping_add(0x39c), b | 1);
    } else {
        m.sb(ctx.wrapping_add(0x39c), b & 0xfe);
    }
    for (bit, mask) in [(2u32, 1u32), (4, 2)] {
        let set = m.lw(record.wrapping_add(0x468)) & bit != 0;
        let ctx = context(m);
        let flags = m.lw(ctx.wrapping_add(0x120));
        m.sw(
            ctx.wrapping_add(0x120),
            if set { flags | mask } else { flags & !mask },
        );
    }
    let set = m.lw(record.wrapping_add(0x468)) & 8 != 0;
    let ctx = context(m);
    m.sb(ctx.wrapping_add(0x95a), u8::from(set));
    let settings = m.lw(SETTINGS);
    let flags = m.lw(settings);
    m.sw(settings, flags | 2);
    m.pop_frame(0x28);
}

/// 800E3640: clear the present-account bits of the 0x80 record.
pub fn clear_accounts_e3640(m: &mut Machine) {
    let c = card(m);
    let info = m.lw(c.wrapping_add(0x2e8));
    m.sb(info.wrapping_add(8), 0);
}

/// 800E3658: clear the present-record bits of account record `account`
/// (-1 the current one; 3 and above nothing) and give it the context's
/// name. Returns strcpy's result (0x80110000 for none).
pub fn reset_account_e3658(m: &mut Machine, account: u32) -> u32 {
    m.push_frame(0x18);
    let result = if account == u32::MAX {
        let c = card(m);
        let current = m.lw(c.wrapping_add(0x2f8));
        m.sh(current.wrapping_add(0x16), 0);
        let ctx = context(m);
        let current = m.lw(c.wrapping_add(0x2f8));
        call(
            m,
            STRCPY,
            [current.wrapping_add(8), ctx.wrapping_add(0x948), 0, 0],
        )
    } else if (account as i32) < 3 {
        let at = card(m).wrapping_add(account << 2);
        let record = m.lw(at.wrapping_add(0x2ec));
        m.sh(record.wrapping_add(0x16), 0);
        let ctx = context(m);
        let record = m.lw(at.wrapping_add(0x2ec));
        call(
            m,
            STRCPY,
            [record.wrapping_add(8), ctx.wrapping_add(0x948), 0, 0],
        )
    } else {
        // the `lui v0, 0x8011` in the branch's delay slot
        0x8011_0000
    };
    m.pop_frame(0x18);
    result
}

/// 800E36E0: unpack mission record `index` (0x180 bytes) into the context
/// (the inverse of 800E2F44), with the index at progress +1.
pub fn unpack_record_e36e0(m: &mut Machine, index: u32) {
    let c = card(m);
    let at = c.wrapping_add((index & 0xff) << 2);
    let ctx = context(m);
    let record = m.lw(at.wrapping_add(0x2fc));
    let progress = ctx.wrapping_add(0x95c);
    m.sb(progress.wrapping_add(1), index as u8);
    let v = m.lw(record.wrapping_add(0x114));
    let ctx = context(m);
    m.sw(progress.wrapping_add(0x6d0), v);
    let b = m.lbu(record.wrapping_add(0x113));
    m.sh(ctx.wrapping_add(0x38c), u16::from(b));
    let b = m.lbu(record.wrapping_add(0x112));
    m.sw(ctx.wrapping_add(0x394), u32::from(b));
    for k in 0..0x3cu32 {
        let v = m.lhu(record.wrapping_add(2 * k).wrapping_add(4));
        let at = record.wrapping_add(k);
        m.sh(progress.wrapping_add(2 * k).wrapping_add(0x248), v);
        let b = m.lbu(at.wrapping_add(0x7c));
        m.sb(progress.wrapping_add(k).wrapping_add(0x3b0), b);
        let b = m.lbu(at.wrapping_add(0x9a));
        m.sw(
            progress.wrapping_add(4 * k).wrapping_add(0x4f0),
            u32::from(b),
        );
        let b = m.lbu(at.wrapping_add(0xd6));
        let ctx = context(m);
        m.sw(
            progress.wrapping_add(4 * k).wrapping_add(0x5e0),
            u32::from(b),
        );
        let b = m.lbu(record.wrapping_add(0x118));
        m.sh(ctx.wrapping_add(0x38e), u16::from(b));
        let b = m.lbu(record.wrapping_add(0x119));
        m.sb(ctx.wrapping_add(0x3a9), b);
        let ctx = context(m);
        let b = m.lbu(record.wrapping_add(0x11a));
        m.sb(ctx.wrapping_add(0x3aa), b);
        let ctx = context(m);
        let b = m.lbu(record.wrapping_add(0x11b));
        m.sb(ctx.wrapping_add(0x3ab), b);
    }
    for k in 0..0x1eu32 {
        let b = m.lbu(record.wrapping_add(k).wrapping_add(0x7c));
        m.sb(progress.wrapping_add(2 * k).wrapping_add(0x3b0), b & 0xf);
        let b = m.lbu(record.wrapping_add(k).wrapping_add(0x7c));
        m.sb(
            progress
                .wrapping_add(2 * k)
                .wrapping_add(1)
                .wrapping_add(0x3b0),
            b >> 4,
        );
    }
}

/// 800E37F8: allocate the card block (0x360) and its records: the 0x80
/// record, three 0x480 account records and the current one, six 0x180
/// records and the current one.
pub fn allocate_e37f8(m: &mut Machine) {
    m.push_frame(0x20);
    let block = crate::arena::alloc(m, 0x360);
    m.sw(CARD, block);
    let info = crate::arena::alloc(m, 0x80);
    let c = card(m);
    m.sw(c.wrapping_add(0x2e8), info);
    for i in 0..4u32 {
        let record = crate::arena::alloc(m, 0x480);
        let c = card(m);
        if i < 3 {
            m.sw(c.wrapping_add(i << 2).wrapping_add(0x2ec), record);
        } else {
            m.sw(c.wrapping_add(0x2f8), record);
        }
    }
    for i in 0..7u32 {
        let record = crate::arena::alloc(m, 0x180);
        let c = card(m);
        if i < 6 {
            m.sw(c.wrapping_add(i << 2).wrapping_add(0x2fc), record);
        } else {
            m.sw(c.wrapping_add(0x314), record);
        }
    }
    m.pop_frame(0x20);
}

/// 800E38DC: build the 0x280-byte card file head `[0x80112DEC]` ("SC",
/// icon flag 0x13 and block count 2, the title template, "CRD0") with
/// the icon of mission `mission` (800ECD4C) and the 32-word palette at
/// `palette`.
pub fn build_head_e38dc(m: &mut Machine, mission: u32, palette: u32) {
    let frame = m.push_frame(0x30);
    let head = crate::arena::alloc(m, 0x280);
    m.sw(HEAD, head);
    let magic = m.lh(HEAD_MAGIC) as u16;
    let third = m.lb(HEAD_MAGIC.wrapping_add(2)) as u8;
    m.sh(head, magic);
    m.sb(head.wrapping_add(2), third);
    let h = m.lw(HEAD);
    m.sb(h.wrapping_add(2), 0x13);
    let h = m.lw(HEAD);
    m.sb(h.wrapping_add(3), 2);
    let h = m.lw(HEAD);
    let mut from = HEAD_TEMPLATE;
    let mut to = h + 4;
    while from != HEAD_TEMPLATE + 0x20 {
        let w: [u32; 4] = std::array::from_fn(|i| m.lw(from + 4 * i as u32));
        for (i, w) in w.into_iter().enumerate() {
            m.sw(to + 4 * i as u32, w);
        }
        from += 0x10;
        to += 0x10;
    }
    let h = m.lw(HEAD);
    let w = m.lw(from);
    let half = m.lh(from.wrapping_add(4)) as u16;
    let byte = m.lb(from.wrapping_add(6)) as u8;
    m.sw(to, w);
    m.sh(to.wrapping_add(4), half);
    m.sb(to.wrapping_add(6), byte);
    for i in (0..3u32).rev() {
        m.sw(h.wrapping_add(0x44).wrapping_add(i * 4), 0);
    }
    let h = m.lw(HEAD);
    m.sh(h.wrapping_add(0x50), 1);
    copy_bytes(m, h.wrapping_add(0x52), HEAD_CRD0, 4);
    let b = m.lb(HEAD_CRD0.wrapping_add(4)) as u8;
    m.sb(h.wrapping_add(0x56), b);
    m.sb(h.wrapping_add(0x56), 0);
    let h = m.lw(HEAD);
    m.sb(h.wrapping_add(0x57), 0);
    let h = m.lw(HEAD);
    m.sw(h.wrapping_add(0x58), 0);
    m.sw(h.wrapping_add(0x5c), 0);
    let tables = frame + 0x10;
    let count = call(m, MISSION_TABLES, [mission, tables, 0, 0]);
    let h = m.lw(HEAD);
    m.sb(h.wrapping_add(2), count.wrapping_add(0x10) as u8);
    for k in 0..8u32 {
        let first = m.lw(tables);
        let h = m.lw(HEAD);
        let v = m.lw(first.wrapping_add(k * 4).wrapping_add(0x14));
        m.sw(h.wrapping_add(k * 4).wrapping_add(0x60), v);
    }
    let mut table = tables;
    let mut frame_index = 0u32;
    while (frame_index as i32) < (count as i32) {
        for k in 0..0x20u32 {
            let h = m.lw(HEAD);
            let source = m.lw(table);
            let v = m.lw(source.wrapping_add(k * 4).wrapping_add(0x40));
            m.sw(
                h.wrapping_add(frame_index << 7)
                    .wrapping_add(k * 4)
                    .wrapping_add(0x80),
                v,
            );
        }
        frame_index = frame_index.wrapping_add(1);
        table = table.wrapping_add(4);
    }
    let h = m.lw(HEAD);
    for k in 0..0x20u32 {
        let v = m.lw(palette.wrapping_add(k * 4));
        m.sw(h.wrapping_add(0x200).wrapping_add(k * 4), v);
    }
    m.pop_frame(0x30);
}

/// 800E3AE8: reset the 0x80 record: version 0x00990426, no accounts.
pub fn reset_info_e3ae8(m: &mut Machine) {
    let c = card(m);
    let info = m.lw(c.wrapping_add(0x2e8));
    m.sw(info.wrapping_add(4), VERSION);
    let info = m.lw(c.wrapping_add(0x2e8));
    m.sb(info.wrapping_add(8), 0);
    let c = card(m);
    let info = m.lw(c.wrapping_add(0x2e8));
    m.sb(info.wrapping_add(9), 0);
    for i in 0..0x70u32 {
        let c = card(m);
        let info = m.lw(c.wrapping_add(0x2e8));
        m.sb(info.wrapping_add(i).wrapping_add(0x10), 0);
    }
    let c = card(m);
    let info = m.lw(c.wrapping_add(0x2e8));
    m.sw(info.wrapping_add(0xc), 0);
}

/// 800E3B5C: 800E3AE8.
pub fn reset_info_e3b5c(m: &mut Machine) {
    m.push_frame(0x18);
    reset_info_e3ae8(m);
    m.pop_frame(0x18);
}

/// 800E3B7C: reset the four account records ("Nemo", no progress, the
/// context's mission entries and settings).
pub fn reset_slots_e3b7c(m: &mut Machine) {
    m.push_frame(8);
    for i in 0..4u32 {
        let c = card(m);
        let record = if i < 3 {
            m.lw(c.wrapping_add(i << 2).wrapping_add(0x2ec))
        } else {
            m.lw(c.wrapping_add(0x2f8))
        };
        let name = m.lw(DEFAULT_NAME);
        let fifth = m.lb(DEFAULT_NAME.wrapping_add(4)) as u8;
        m.sw(record.wrapping_add(8), name);
        m.sb(record.wrapping_add(0xc), fifth);
        m.sh(record.wrapping_add(0x14), 0);
        m.sh(record.wrapping_add(0x16), 0);
        for k in 0..0x3cu32 {
            let half = record + 2 * k;
            m.sh(half.wrapping_add(0x18a), 0);
            m.sh(half.wrapping_add(0x112), 0);
            m.sh(half.wrapping_add(0x9a), 0);
            m.sh(half.wrapping_add(0x22), 0);
            let byte = record + k;
            m.sb(byte.wrapping_add(0x202), 0);
            m.sb(byte.wrapping_add(0x220), 0);
            m.sb(byte.wrapping_add(0x23e), 0);
            m.sb(byte.wrapping_add(0x25c), 0);
        }
        m.sh(record.wrapping_add(0x2c0), 0);
        m.sh(record.wrapping_add(0x2c2), 0);
        let mut source = 0u32;
        let mut entry = 0x2c4u32;
        for _ in 0..6 {
            let ctx = context(m);
            let to = record.wrapping_add(entry);
            let mut from = 0x16u32;
            for k in 0..0x14u32 {
                let v = m.lhu(
                    ctx.wrapping_add(from.wrapping_add(source))
                        .wrapping_add(0x15c),
                );
                from = from.wrapping_add(2);
                m.sh(to.wrapping_add(2 * k), v);
            }
            entry = entry.wrapping_add(0x44);
            let ctx = context(m);
            let base = ctx.wrapping_add(source);
            copy_bytes(m, to.wrapping_add(0x28), base.wrapping_add(0x19c), 0x10);
            copy_bytes(m, to.wrapping_add(0x38), base.wrapping_add(0x1ac), 8);
            let v = m.lw(base.wrapping_add(0x1b4));
            m.sw(to.wrapping_add(0x40), v);
            source = source.wrapping_add(0x5c);
        }
        m.sb(record.wrapping_add(0x45c), 0);
        m.sb(record.wrapping_add(0x45d), 0xcc);
        m.sb(record.wrapping_add(0x45e), 0xcc);
        let ctx = context(m);
        m.sh(record.wrapping_add(0x462), 0);
        m.sh(record.wrapping_add(0x460), 0);
        m.sh(record.wrapping_add(0x16), 0);
        if m.lw(ctx.wrapping_add(0x120)) & 4 != 0 {
            m.sb(record.wrapping_add(0x464), 0);
        } else {
            m.sb(record.wrapping_add(0x464), 1);
        }
        let ctx = context(m);
        m.sw(record.wrapping_add(0x468), 0);
        if m.lbu(ctx.wrapping_add(0x39c)) & 1 != 0 {
            m.sw(record.wrapping_add(0x468), 1);
        }
        let ctx = context(m);
        if m.lw(ctx.wrapping_add(0x120)) & 1 != 0 {
            let f = m.lw(record.wrapping_add(0x468));
            m.sw(record.wrapping_add(0x468), f | 2);
        }
        let ctx = context(m);
        if m.lw(ctx.wrapping_add(0x120)) & 2 != 0 {
            let f = m.lw(record.wrapping_add(0x468));
            m.sw(record.wrapping_add(0x468), f | 4);
        }
        let ctx = context(m);
        if m.lbu(ctx.wrapping_add(0x95a)) == 1 {
            let f = m.lw(record.wrapping_add(0x468));
            m.sw(record.wrapping_add(0x468), f | 8);
        }
        for k in (0..=0x10u32).rev() {
            m.sb(record.wrapping_add(k).wrapping_add(0x46c), 0);
        }
    }
    m.pop_frame(8);
}

/// 800E3E14: 800E3B7C.
pub fn reset_slots_e3e14(m: &mut Machine) {
    m.push_frame(0x18);
    reset_slots_e3b7c(m);
    m.pop_frame(0x18);
}

/// 800E3E34: an empty routine (PAL 80054C78's place).
pub fn nothing_e3e34(_m: &mut Machine) {}

/// 800E3E3C: reset the card layer: counters, records, the monitor
/// (800E25D4), the empty-account name, the two save file names and both
/// ports' results (2, no card).
pub fn reset_e3e3c(m: &mut Machine) {
    m.push_frame(0x18);
    let c = card(m);
    m.sh(c.wrapping_add(0x358), 0);
    m.sh(c.wrapping_add(0x35a), 0);
    m.sh(c.wrapping_add(0x35c), 0);
    reset_info_e3ae8(m);
    reset_slots_e3b7c(m);
    nothing_e3e34(m);
    super::ov2_p3_card::reset_ports_e25d4(m);
    let c = card(m);
    m.sb(c.wrapping_add(0x336), 0);
    let v1 = card(m);
    m.sw(c.wrapping_add(0x320), 0);
    m.sh(c.wrapping_add(0x4c), 0);
    m.sh(c.wrapping_add(0x4e), 0);
    m.sh(c.wrapping_add(0x50), 0);
    m.sh(c.wrapping_add(0x52), 0);
    m.sb(v1.wrapping_add(0x334), 0);
    let block = card(m);
    for i in 0..3u32 {
        let w = m.lw(EMPTY_ACCOUNT.wrapping_add(4 * i));
        m.sw(block.wrapping_add(0x338).wrapping_add(4 * i), w);
    }
    let half = m.lh(EMPTY_ACCOUNT.wrapping_add(0xc)) as u16;
    m.sh(block.wrapping_add(0x344), half);
    let c = card(m);
    for i in 0..5u32 {
        let w = m.lw(NAME_PORT_0.wrapping_add(4 * i));
        m.sw(c.wrapping_add(0x2b4).wrapping_add(4 * i), w);
    }
    let half = m.lh(NAME_PORT_0.wrapping_add(0x14)) as u16;
    m.sh(c.wrapping_add(0x2c8), half);
    let b = m.lb(NAME_PORT_0.wrapping_add(0x16)) as u8;
    m.sb(c.wrapping_add(0x2ca), b);
    let c = card(m);
    copy_bytes(m, c.wrapping_add(0x2ce), NAME_PORT_1, 0x14);
    let half = m.lh(NAME_PORT_1.wrapping_add(0x14)) as u16;
    m.sh(c.wrapping_add(0x2e2), half);
    let b = m.lb(NAME_PORT_1.wrapping_add(0x16)) as u8;
    m.sb(c.wrapping_add(0x2e4), b);
    for port in 0..2u32 {
        let at = block + port * 2;
        m.sh(at.wrapping_add(0x2c), 2);
        m.sh(at.wrapping_add(0x30), 2);
    }
    m.pop_frame(0x18);
}

/// 800E416C: set the card layer up: allocate (800E37F8), open the events
/// (800E3FCC), reset (800E3E3C).
pub fn setup_e416c(m: &mut Machine) {
    m.push_frame(0x18);
    allocate_e37f8(m);
    super::ov2_p3_card::open_events_e3fcc(m);
    reset_e3e3c(m);
    m.pop_frame(0x18);
}

/// 800E425C: the name of account `n` (0 the current one, 1..3 the account
/// records; else the empty string).
pub fn account_name_e425c(m: &mut Machine, n: u32) -> u32 {
    if n == 0 {
        let c = card(m);
        return m.lw(c.wrapping_add(0x2f8)).wrapping_add(8);
    }
    if (n as i32) < 4 {
        let c = card(m);
        let record = m.lw(c.wrapping_add(n.wrapping_sub(1) << 2).wrapping_add(0x2ec));
        return record.wrapping_add(8);
    }
    EMPTY
}

/// 800E42AC: whether record `slot` of account `account` (-1 the current
/// one) is present (bit of +0x16).
pub fn present_e42ac(m: &mut Machine, account: u32, slot: u32) -> u32 {
    let c = card(m);
    let record = if account == u32::MAX {
        m.lw(c.wrapping_add(0x2f8))
    } else {
        m.lw(c.wrapping_add(account << 2).wrapping_add(0x2ec))
    };
    let marks = m.lh(record.wrapping_add(0x16)) as i32;
    ((marks >> (slot & 31)) & 1) as u32
}

/// 800E430C: card block +0x4E.
pub fn word_4e_e430c(m: &mut Machine) -> u32 {
    let c = card(m);
    m.lh(c.wrapping_add(0x4e)) as i32 as u32
}

/// 800E4324: whether account `account` (0..2) is present, else -1.
pub fn account_present_e4324(m: &mut Machine, account: u32) -> u32 {
    if account >= 3 {
        return u32::MAX;
    }
    let c = card(m);
    let info = m.lw(c.wrapping_add(0x2e8));
    (u32::from(m.lbu(info.wrapping_add(8))) >> account) & 1
}

/// 800E435C: the unit text of mission record `slot`.
pub fn value_e435c(m: &mut Machine, slot: u32) -> u32 {
    let c = card(m);
    let record = m.lw(c.wrapping_add((slot & 0xff) << 2).wrapping_add(0x2fc));
    let unit = u32::from(m.lbu(record.wrapping_add(0x112)));
    m.lw((unit << 2).wrapping_add(UNIT_TEXTS))
}

/// 800E4394: the unit of the current mission record.
pub fn current_unit_e4394(m: &mut Machine) -> u32 {
    let c = card(m);
    let record = m.lw(c.wrapping_add(0x314));
    u32::from(m.lbu(record.wrapping_add(0x112)))
}

/// 800E43B4: the time of mission record `slot` as "%3d:%2d:%2d" (frames at
/// 30 per second, at most 0x066FF2FF, blanks as zeros) in 0x80112DE0.
pub fn value_e43b4(m: &mut Machine, slot: u32) -> u32 {
    let frame = m.push_frame(0x20);
    let c = card(m);
    let record = m.lw(c.wrapping_add((slot & 0xff) << 2).wrapping_add(0x2fc));
    // `slt` against 0x066FF2FF, then `bgez`: the signed clamp
    let time = (m.lw(record.wrapping_add(0x114)) as i32).clamp(0, 0x066f_f2ff) as u32;
    let sign = ((time as i32) >> 31) as u32;
    let minutes = ((super::ov2_p3::mult_hi(time, 0x91a2_b3c5).wrapping_add(time) as i32 >> 10)
        as u32)
        .wrapping_sub(sign);
    let seconds = ((super::ov2_p3::mult_hi(time, 0x8888_8889).wrapping_add(time) as i32 >> 4)
        as u32)
        .wrapping_sub(sign);
    let seconds = seconds.wrapping_sub(minutes.wrapping_mul(60));
    let hours = ((super::ov2_p3::mult_hi(time, 0x9b58_3739).wrapping_add(time) as i32 >> 16)
        as u32)
        .wrapping_sub(sign);
    let minutes = minutes.wrapping_sub(hours.wrapping_mul(60));
    call5(
        m,
        frame,
        SPRINTF,
        [TIME_TEXT, TIME_FORMAT, hours, minutes],
        seconds,
    );
    for i in 0..0xcu32 {
        if m.lbu(TIME_TEXT.wrapping_add(i)) == 0x20 {
            m.sb(TIME_TEXT.wrapping_add(i), 0x30);
        }
    }
    m.pop_frame(0x20);
    TIME_TEXT
}

/// 800E44E0: the name text of mission record `slot`'s level.
pub fn name_e44e0(m: &mut Machine, slot: u32) -> u32 {
    let c = card(m);
    let record = m.lw(c.wrapping_add((slot & 0xff) << 2).wrapping_add(0x2fc));
    let level = u32::from(m.lbu(record.wrapping_add(0x113)));
    m.lw((level << 2).wrapping_add(NAME_TEXTS))
}

/// 800E4518: the level text of mission record `slot`.
pub fn value_e4518(m: &mut Machine, slot: u32) -> u32 {
    let c = card(m);
    let record = m.lw(c.wrapping_add((slot & 0xff) << 2).wrapping_add(0x2fc));
    let level = u32::from(m.lbu(record.wrapping_add(0x113)));
    let class = u32::from(m.lbu(level.wrapping_add(LEVEL_OF)));
    m.lw((class << 2).wrapping_add(LEVEL_TEXTS))
}

/// 800E4560: the empty string.
pub fn empty_e4560() -> u32 {
    EMPTY
}

/// 800E456C: the present-account bits.
pub fn accounts_e456c(m: &mut Machine) -> u32 {
    let c = card(m);
    let info = m.lw(c.wrapping_add(0x2e8));
    u32::from(m.lbu(info.wrapping_add(8)))
}

/// 800E458C: the present-record bits of the current account.
pub fn records_e458c(m: &mut Machine) -> u32 {
    let c = card(m);
    let current = m.lw(c.wrapping_add(0x2f8));
    m.lh(current.wrapping_add(0x16)) as i32 as u32
}

/// 800E45AC: the setting text of account record `account` (+0x464).
pub fn value_e45ac(m: &mut Machine, account: u32) -> u32 {
    let c = card(m);
    let record = m.lw(c.wrapping_add(account << 2).wrapping_add(0x2ec));
    let setting = u32::from(m.lbu(record.wrapping_add(0x464)));
    m.lw((setting << 2).wrapping_add(SETTING_TEXTS))
}

/// 800E45E0 / 800E45F8 / 800E4610 / 800E4628: the address of a setting of
/// the current account record (+0x45C / +0x45D / +0x45E / +0x460).
pub fn setting_at(m: &mut Machine, offset: u32) -> u32 {
    let c = card(m);
    m.lw(c.wrapping_add(0x2f8)).wrapping_add(offset)
}

/// 800E4640: set the current mission record's unit (+0x112).
pub fn set_unit_e4640(m: &mut Machine, unit: u32) {
    let c = card(m);
    let record = m.lw(c.wrapping_add(0x314));
    m.sb(record.wrapping_add(0x112), unit as u8);
}

/// 800E4658 / 800E4670 / 800E4688: set a setting byte of the current
/// account record (+0x45C / +0x45D / +0x45E).
pub fn set_setting(m: &mut Machine, offset: u32, value: u32) {
    let c = card(m);
    let current = m.lw(c.wrapping_add(0x2f8));
    m.sb(current.wrapping_add(offset), value as u8);
}

/// 800E46A0: the present account (0..2) whose name and context word match
/// the context's, else -1.
pub fn find_account_e46a0(m: &mut Machine) -> u32 {
    m.push_frame(0x28);
    let mut result = u32::MAX;
    for account in 0..3u32 {
        let c = card(m);
        let info = m.lw(c.wrapping_add(0x2e8));
        if (u32::from(m.lbu(info.wrapping_add(8))) >> account) & 1 == 0 {
            continue;
        }
        let record = m.lw(c.wrapping_add(account << 2).wrapping_add(0x2ec));
        let ctx = context(m);
        if call(
            m,
            STRCMP,
            [record.wrapping_add(8), ctx.wrapping_add(0x948), 0, 0],
        ) != 0
        {
            continue;
        }
        let c = card(m);
        let record = m.lw(c.wrapping_add(account << 2).wrapping_add(0x2ec));
        let ctx = context(m);
        let word = m.lw(record.wrapping_add(4));
        if word == m.lw(ctx.wrapping_add(0x968)) {
            result = account;
            break;
        }
    }
    m.pop_frame(0x28);
    result
}

/// The routines of this file, while module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    let [a0, a1, _, _] = args;
    Some(match target {
        0x800e_2ab4 => {
            pack_slot_e2ab4(m);
            0
        }
        0x800e_2f44 => {
            pack_record_e2f44(m);
            0
        }
        0x800e_30a4 => {
            select_slot_e30a4(m, a0);
            0
        }
        0x800e_30f4 => {
            unpack_slot_e30f4(m, a0);
            0
        }
        0x800e_3640 => {
            clear_accounts_e3640(m);
            0
        }
        0x800e_3658 => reset_account_e3658(m, a0),
        0x800e_36e0 => {
            unpack_record_e36e0(m, a0);
            0
        }
        0x800e_37f8 => {
            allocate_e37f8(m);
            0
        }
        0x800e_38dc => {
            build_head_e38dc(m, a0, a1);
            0
        }
        0x800e_3ae8 => {
            reset_info_e3ae8(m);
            0
        }
        0x800e_3b5c => {
            reset_info_e3b5c(m);
            0
        }
        0x800e_3b7c => {
            reset_slots_e3b7c(m);
            0
        }
        0x800e_3e14 => {
            reset_slots_e3e14(m);
            0
        }
        0x800e_3e34 => {
            nothing_e3e34(m);
            0
        }
        0x800e_3e3c => {
            reset_e3e3c(m);
            0
        }
        0x800e_416c => {
            setup_e416c(m);
            0
        }
        0x800e_425c => account_name_e425c(m, a0),
        0x800e_42ac => present_e42ac(m, a0, a1),
        0x800e_430c => word_4e_e430c(m),
        0x800e_4324 => account_present_e4324(m, a0),
        0x800e_435c => value_e435c(m, a0),
        0x800e_4394 => current_unit_e4394(m),
        0x800e_43b4 => value_e43b4(m, a0),
        0x800e_44e0 => name_e44e0(m, a0),
        0x800e_4518 => value_e4518(m, a0),
        0x800e_4560 => empty_e4560(),
        0x800e_456c => accounts_e456c(m),
        0x800e_458c => records_e458c(m),
        0x800e_45ac => value_e45ac(m, a0),
        0x800e_45e0 => setting_at(m, 0x45c),
        0x800e_45f8 => setting_at(m, 0x45d),
        0x800e_4610 => setting_at(m, 0x45e),
        0x800e_4628 => setting_at(m, 0x460),
        0x800e_4640 => {
            set_unit_e4640(m, a0);
            0
        }
        0x800e_4658 => {
            set_setting(m, 0x45c, a0);
            0
        }
        0x800e_4670 => {
            set_setting(m, 0x45d, a0);
            0
        }
        0x800e_4688 => {
            set_setting(m, 0x45e, a0);
            0
        }
        0x800e_46a0 => find_account_e46a0(m),
        _ => return None,
    })
}
