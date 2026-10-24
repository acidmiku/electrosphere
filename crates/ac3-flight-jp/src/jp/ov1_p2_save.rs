//! JP overlay 1, part 2: the save block, the account records, the card
//! events, the card probe and the card file (`800D4D54`..`800D75E0`).
//!
//! The save block `[0x800DDA48]` (0x360 bytes, [`alloc_blocks`]): +0x4..+0x1C
//! seven BIOS event handles (SW_CARD done/error/timeout/new, HW_CARD
//! done/error/timeout), +0x28 card changed flag, +0x2C/+0x30 card and load
//! status per slot (halfwords), +0x3C file handle, +0x4C..+0x52 halfwords
//! (+0x50 the record being transferred), +0x54 free blocks and +0x58 file
//! count per slot, +0x5C 15 file names (20 bytes) per slot (0x12C apart),
//! +0x2B4 the two card file names (26 bytes apart), +0x2E8 the account
//! flags (0x80 bytes: +0 checksum, +4 version 0x00990426, +8 account bits,
//! +0xC save count), +0x2EC three account records and +0x2F8 the current
//! one (0x480 bytes: +0 checksum, +4, +8 name, +0x14 record index, +0x16
//! record bits), +0x2FC six records and +0x314 the current one (0x180
//! bytes), +0x318 probe timeout, +0x320, +0x334/+0x336 (+0x336 transfer
//! started), +0x338 "empty account", +0x348 card present, +0x34A file
//! present, +0x34C probe step, +0x34E last event, +0x350 retries (bytes
//! per slot), +0x354 the slot being probed, +0x358 open retries, +0x35A
//! transfer active.
//!
//! The card file (`bu00:BISLPSP02020AC3ES`, two blocks): the head (0x280
//! bytes from `[0x800DDA44]`) at 0, the account flags at 0x280, account n
//! at 0x300 + 0x480 n, record k of account n at 0x1080 + 0x900 n + 0x180 k.
//! Records carry an XOR checksum (seed 0x41434533) of their words after the
//! first.
//!
//! The card-event routines of the first half of the module (800D3CE8
//! reset, 800D3D60 ready, 800D3EA8 / 800D4060 test the card / file events,
//! 800D3F88 / 800D3FF4 acknowledge them) and 800CCE38 are reached through
//! `dispatch`.
use super::ov1_p2::{
    HEAD, SAVE, STRCMP, STRCPY, call, context, lb, lbu, lh, lhu, load_unaligned, loaded, off,
    sext16, slt, sra, store_unaligned,
};
use crate::{arena, bios, machine::Machine, sdk};

/// 800D3CE8 (first half): reset the card probe state.
const RESET_PROBE: u32 = 0x800d_3ce8;
/// 800D3D60 (first half): clear the changed flag; nonzero when idle.
const CARD_READY: u32 = 0x800d_3d60;
/// 800D3EA8 (first half): test the card events (slot, step).
const TEST_CARD_EVENTS: u32 = 0x800d_3ea8;
/// 800D3F88 (first half): acknowledge the card events.
const DRAIN_CARD_EVENTS: u32 = 0x800d_3f88;
/// 800D3FF4 (first half): acknowledge the file events.
const DRAIN_FILE_EVENTS: u32 = 0x800d_3ff4;
/// 800D4060 (first half): test the file events (slot, step).
const TEST_FILE_EVENTS: u32 = 0x800d_4060;
/// 800CCE38 (first half): the head's icon images (count; pointers out).
const HEAD_IMAGES: u32 = 0x800c_ce38;

/// 0x41434533: the record checksum seed.
const CHECKSUM_SEED: u32 = 0x4143_4533;
/// 0x00990426: the account flags' version word.
const FLAGS_VERSION: u32 = 0x0099_0426;
/// 0x800CC488 "SC", 0x800CC48C the 39-byte title, 0x800CC4B4 "CRD0".
const HEAD_MAGIC: u32 = 0x800c_c488;
const HEAD_TITLE: u32 = 0x800c_c48c;
const HEAD_CLUT_NAME: u32 = 0x800c_c4b4;
/// 0x800CC4BC "Nemo": the default account name.
const DEFAULT_NAME: u32 = 0x800c_c4bc;
/// 0x800CC4C4 "empty account", 0x800CC4D4 / 0x800CC4EC the file names.
const EMPTY_ACCOUNT: u32 = 0x800c_c4c4;
const FILE_NAME_0: u32 = 0x800c_c4d4;
const FILE_NAME_1: u32 = 0x800c_c4ec;
/// 0x800CC504: the empty string.
const EMPTY: u32 = 0x800c_c504;
/// 0x800CC52C "%3d:%2d:%2d" and its output buffer 0x800DDA38.
const TIME_FORMAT: u32 = 0x800c_c52c;
const TIME_TEXT: u32 = 0x800d_da38;
/// 0x800CC538: the probe step jump table (nine steps).
const PROBE_STEPS: u32 = 9;
/// 0x800CC55C "bu%1d0:*.*".
const DIRECTORY_PATTERN: u32 = 0x800c_c55c;
/// 0x800DD6A4 mission names, 0x800DD798 / 0x800DD7B0 the faction names and
/// the mission's faction, 0x800DD7F0 difficulty names, 0x800DD800 two
/// more names.
const MISSION_NAMES: u32 = 0x800d_d6a4;
const FACTION_NAMES: u32 = 0x800d_d798;
const MISSION_FACTION: u32 = 0x800d_d7b0;
const DIFFICULTY_NAMES: u32 = 0x800d_d7f0;
const MODE_NAMES: u32 = 0x800d_d800;
/// `[0x800DD9D0]`: the menu state block of the first half.
const MENU_STATE: u32 = 0x800d_d9d0;
/// 0x066FF2FF: the largest play time (frames) the time text shows.
const TIME_LIMIT: u32 = 0x066f_f2ff;

fn save(m: &mut Machine) -> u32 {
    m.lw(SAVE)
}
/// The card file name of `slot` (26 bytes apart from +0x2B4).
fn file_name(block: u32, slot: u32) -> u32 {
    let n = (slot << 1).wrapping_add(slot);
    let n = (n << 2).wrapping_add(slot);
    off(block, (n << 1).wrapping_add(0x2b4))
}
/// XOR of the seed and the `words` words after the first at `base`.
fn checksum(m: &mut Machine, base: u32, words: u32) -> u32 {
    let mut value = CHECKSUM_SEED;
    for i in 1..=words {
        value ^= m.lw(off(base, 4 * i));
    }
    value
}
/// The common start of a transfer: mark it active and clear the open
/// retries the first time.
fn begin_transfer(m: &mut Machine) -> u32 {
    let block = save(m);
    if m.lh(off(block, 0x35a)) == 0 {
        m.sh(off(block, 0x35a), 1);
        m.sh(off(block, 0x358), 0);
    }
    save(m)
}
/// Count an open retry: true (and +0x358 incremented) while fewer than 3.
fn retry(m: &mut Machine, block: u32) -> bool {
    let tries = lh(m, off(block, 0x358));
    let count = lhu(m, off(block, 0x358));
    if slt(tries, 3) {
        m.sh(off(block, 0x358), count.wrapping_add(1) as u16);
        true
    } else {
        false
    }
}

// ---- blocks -------------------------------------------------------------------------

/// 800D4D54 (unreferenced): clear the account bits (flags +8).
pub fn clear_account_bits(m: &mut Machine) {
    let block = save(m);
    let flags = m.lw(off(block, 0x2e8));
    m.sb(off(flags, 8), 0);
}

/// 800D4D6C (unreferenced): clear the record bits of account `index` (-1:
/// the current one; 3 and more: nothing) and copy the context's name
/// (+0x948) to its name.
pub fn reset_account_name(m: &mut Machine, index: u32) {
    m.push_frame(0x18);
    let record = if index == u32::MAX {
        let block = save(m);
        let record = m.lw(off(block, 0x2f8));
        m.sh(off(record, 0x16), 0);
        Some(m.lw(off(block, 0x2f8)))
    } else if slt(index, 3) {
        let entry = off(save(m), index << 2);
        let record = m.lw(off(entry, 0x2ec));
        m.sh(off(record, 0x16), 0);
        Some(m.lw(off(entry, 0x2ec)))
    } else {
        None
    };
    if let Some(record) = record {
        let name = off(context(m), 0x948);
        call(m, STRCPY, [off(record, 8), name, 0, 0]);
    }
    m.pop_frame(0x18);
}

/// 800D4DF4: load record `index` (byte) into the context: its index at
/// +0x95D, play time +0x114 to +0x102C, the mission +0x113 / difficulty
/// +0x112 to +0x38C / +0x394, 60 halfwords from +4 to +0xBA4, 60 bytes
/// +0x7C.. as nibbles to +0xD0C.., 60 words from +0x9A and +0xD6, and the
/// option bytes +0x118..+0x11B.
pub fn load_record(m: &mut Machine, index: u32) {
    let entry = off(save(m), (index & 0xff) << 2);
    let record = m.lw(off(entry, 0x2fc));
    let base = off(context(m), 0x95c);
    m.sb(off(base, 1), index as u8);
    let time = m.lw(off(record, 0x114));
    let ctx = context(m);
    m.sw(off(base, 0x6d0), time);
    let mission = lbu(m, off(record, 0x113));
    m.sh(off(ctx, 0x38c), mission as u16);
    let difficulty = lbu(m, off(record, 0x112));
    m.sw(off(ctx, 0x394), difficulty);
    for i in 0..0x3c {
        let half = lhu(m, off(record, 4 + 2 * i));
        let at = off(record, i);
        m.sh(off(base, 0x248 + 2 * i), half as u16);
        let byte = lbu(m, off(at, 0x7c));
        m.sb(off(base, 0x3b0 + i), byte as u8);
        let value = lbu(m, off(at, 0x9a));
        m.sw(off(base, 0x4f0 + 4 * i), value);
        let value = lbu(m, off(at, 0xd6));
        let ctx = context(m);
        m.sw(off(base, 0x5e0 + 4 * i), value);
        let option = lbu(m, off(record, 0x118));
        m.sh(off(ctx, 0x38e), option as u16);
        let option = lbu(m, off(record, 0x119));
        m.sb(off(ctx, 0x3a9), option as u8);
        let ctx = context(m);
        let option = lbu(m, off(record, 0x11a));
        m.sb(off(ctx, 0x3aa), option as u8);
        let ctx = context(m);
        let option = lbu(m, off(record, 0x11b));
        m.sb(off(ctx, 0x3ab), option as u8);
    }
    for i in 0..0x1e {
        let at = off(record, i);
        let byte = lbu(m, off(at, 0x7c));
        m.sb(off(base, 0x3b0 + 2 * i), (byte & 0xf) as u8);
        let byte = lbu(m, off(at, 0x7c));
        m.sb(off(base, 0x3b1 + 2 * i), (byte >> 4) as u8);
    }
}

/// 800D4F0C: allocate the save block, the account flags (0x80), the four
/// account records (0x480) and the seven records (0x180).
pub fn alloc_blocks(m: &mut Machine) {
    m.push_frame(0x20);
    let block = arena::alloc(m, 0x360);
    m.sw(SAVE, block);
    let flags = arena::alloc(m, 0x80);
    let block = save(m);
    m.sw(off(block, 0x2e8), flags);
    for i in 0..4u32 {
        let record = arena::alloc(m, 0x480);
        let block = save(m);
        if i < 3 {
            m.sw(off(off(block, i << 2), 0x2ec), record);
        } else {
            m.sw(off(block, 0x2f8), record);
        }
    }
    for i in 0..7u32 {
        let record = arena::alloc(m, 0x180);
        let block = save(m);
        if i < 6 {
            m.sw(off(off(block, i << 2), 0x2fc), record);
        } else {
            m.sw(off(block, 0x314), record);
        }
    }
    m.pop_frame(0x20);
}

/// 800D4FF0: build the card file head (0x280 bytes at `[0x800DDA44]`):
/// "SC", icon type 0x13, two blocks, the title, "CRD0", the CLUT of the
/// first icon image and the icon images of `images` (800CCE38), then the
/// 128 bytes at `extra`.
pub fn build_head(m: &mut Machine, images: u32, extra: u32) {
    let frame = m.push_frame(0x30);
    let head = arena::alloc(m, 0x280);
    m.sw(HEAD, head);
    let magic = lh(m, HEAD_MAGIC);
    let magic2 = lb(m, off(HEAD_MAGIC, 2));
    m.sh(head, magic as u16);
    m.sb(off(head, 2), magic2 as u8);
    let head = m.lw(HEAD);
    m.sb(off(head, 2), 0x13);
    let head = m.lw(HEAD);
    m.sb(off(head, 3), 2);
    let head = m.lw(HEAD);
    for i in 0..8 {
        let word = m.lw(off(HEAD_TITLE, 4 * i));
        m.sw(off(head, 4 + 4 * i), word);
    }
    let word = m.lw(off(HEAD_TITLE, 0x20));
    let half = lh(m, off(HEAD_TITLE, 0x24));
    let byte = lb(m, off(HEAD_TITLE, 0x26));
    m.sw(off(head, 0x24), word);
    m.sh(off(head, 0x28), half as u16);
    m.sb(off(head, 0x2a), byte as u8);
    for offset in [0x4c, 0x48, 0x44] {
        m.sw(off(head, offset), 0);
    }
    let head = m.lw(HEAD);
    m.sh(off(head, 0x50), 1);
    let name = load_unaligned(m, HEAD_CLUT_NAME);
    let byte = lb(m, off(HEAD_CLUT_NAME, 4));
    store_unaligned(m, off(head, 0x52), name);
    m.sb(off(head, 0x56), byte as u8);
    m.sb(off(head, 0x56), 0);
    let head = m.lw(HEAD);
    m.sb(off(head, 0x57), 0);
    let head = m.lw(HEAD);
    m.sw(off(head, 0x58), 0);
    m.sw(off(head, 0x5c), 0);
    let count = call(m, HEAD_IMAGES, [images, off(frame, 0x10), 0, 0]);
    let head = m.lw(HEAD);
    m.sb(off(head, 2), count.wrapping_add(0x10) as u8);
    for i in 0..8 {
        let first = m.lw(off(frame, 0x10));
        let word = m.lw(off(first, 0x14 + 4 * i));
        let head = m.lw(HEAD);
        m.sw(off(head, 0x60 + 4 * i), word);
    }
    let mut k = 0u32;
    while slt(k, count) {
        for i in 0..0x20 {
            let head = m.lw(HEAD);
            let image = m.lw(off(frame, 0x10 + 4 * k));
            let word = m.lw(off(image, 0x40 + 4 * i));
            m.sw(off(off(head, k << 7), 0x80 + 4 * i), word);
        }
        k = k.wrapping_add(1);
    }
    let head = m.lw(HEAD);
    for i in 0..0x20 {
        let word = m.lw(off(extra, 4 * i));
        m.sw(off(head, 0x200 + 4 * i), word);
    }
    m.pop_frame(0x30);
}

/// 800D51FC: reset the account flags: version 0x00990426, no accounts, the
/// 0x70 bytes from +0x10 cleared, save count 0.
pub fn reset_flags(m: &mut Machine) {
    let block = save(m);
    let flags = m.lw(off(block, 0x2e8));
    m.sw(off(flags, 4), FLAGS_VERSION);
    let flags = m.lw(off(block, 0x2e8));
    m.sb(off(flags, 8), 0);
    let block = save(m);
    let flags = m.lw(off(block, 0x2e8));
    m.sb(off(flags, 9), 0);
    for i in 0..0x70 {
        let block = save(m);
        let flags = m.lw(off(block, 0x2e8));
        m.sb(off(off(flags, i), 0x10), 0);
    }
    let block = save(m);
    let flags = m.lw(off(block, 0x2e8));
    m.sw(off(flags, 0xc), 0);
}

/// 800D5270 (unreferenced): [`reset_flags`].
pub fn reset_flags_call(m: &mut Machine) {
    m.push_frame(0x18);
    reset_flags(m);
    m.pop_frame(0x18);
}

/// 800D5290: reset the four account records: name "Nemo", no records, the
/// tables cleared, the six slot entries from the context (+0x16C..,
/// +0x19C..+0x1B7, 0x5C apart), colours, options from the context's
/// settings.
pub fn reset_accounts(m: &mut Machine) {
    for account in 0..4u32 {
        let block = save(m);
        let record = if slt(account, 3) {
            m.lw(off(off(block, account << 2), 0x2ec))
        } else {
            m.lw(off(block, 0x2f8))
        };
        let name = m.lw(DEFAULT_NAME);
        let tail = lb(m, off(DEFAULT_NAME, 4));
        m.sw(off(record, 8), name);
        m.sb(off(record, 0xc), tail as u8);
        m.sh(off(record, 0x14), 0);
        m.sh(off(record, 0x16), 0);
        for i in 0..0x3c {
            let at = off(record, 2 * i);
            m.sh(off(at, 0x18a), 0);
            m.sh(off(at, 0x112), 0);
            m.sh(off(at, 0x9a), 0);
            m.sh(off(at, 0x22), 0);
            let at = off(record, i);
            m.sb(off(at, 0x202), 0);
            m.sb(off(at, 0x220), 0);
            m.sb(off(at, 0x23e), 0);
            m.sb(off(at, 0x25c), 0);
        }
        m.sh(off(record, 0x2c0), 0);
        m.sh(off(record, 0x2c2), 0);
        for slot in 0..6u32 {
            let source = slot.wrapping_mul(0x5c);
            let entry = off(record, 0x2c4 + 0x44 * slot);
            let ctx = context(m);
            for k in 0..0x14 {
                let half = lhu(m, off(ctx, 0x15c + 0x16 + 2 * k + source));
                m.sh(off(entry, 2 * k), half as u16);
            }
            let ctx = context(m);
            let at = off(ctx, source);
            // four words, then two (the original's register groups)
            for (first, count) in [(0, 4), (4, 2)] {
                let words: Vec<u32> = (first..first + count)
                    .map(|k| load_unaligned(m, off(at, 0x19c + 4 * k)))
                    .collect();
                for (k, word) in (first..).zip(words) {
                    store_unaligned(m, off(entry, 0x28 + 4 * k), word);
                }
            }
            let word = m.lw(off(at, 0x1b4));
            m.sw(off(entry, 0x40), word);
        }
        m.sb(off(record, 0x45c), 0);
        m.sb(off(record, 0x45d), 0xcc);
        m.sb(off(record, 0x45e), 0xcc);
        let ctx = context(m);
        m.sh(off(record, 0x462), 0);
        m.sh(off(record, 0x460), 0);
        m.sh(off(record, 0x16), 0);
        let settings = m.lw(off(ctx, 0x120));
        m.sb(off(record, 0x464), u8::from(settings & 4 == 0));
        let ctx = context(m);
        m.sw(off(record, 0x468), 0);
        if lbu(m, off(ctx, 0x39c)) & 1 != 0 {
            m.sw(off(record, 0x468), 1);
        }
        let ctx = context(m);
        if m.lw(off(ctx, 0x120)) & 1 != 0 {
            let flags = m.lw(off(record, 0x468));
            m.sw(off(record, 0x468), flags | 2);
        }
        let ctx = context(m);
        if m.lw(off(ctx, 0x120)) & 2 != 0 {
            let flags = m.lw(off(record, 0x468));
            m.sw(off(record, 0x468), flags | 4);
        }
        let ctx = context(m);
        if lbu(m, off(ctx, 0x95a)) == 1 {
            let flags = m.lw(off(record, 0x468));
            m.sw(off(record, 0x468), flags | 8);
        }
        for i in (0..=0x10).rev() {
            m.sb(off(record, 0x46c + i), 0);
        }
    }
}

/// 800D5528 (unreferenced): [`reset_accounts`].
pub fn reset_accounts_call(m: &mut Machine) {
    m.push_frame(0x18);
    reset_accounts(m);
    m.pop_frame(0x18);
}

/// 800D5548: an empty routine.
pub fn nothing() {}

/// 800D5550: reset the save state: transfer counters, flags, accounts, the
/// probe (800D3CE8), the status words, "empty account", the two card file
/// names and the card status of both slots (2).
pub fn reset_save_state(m: &mut Machine) {
    m.push_frame(0x18);
    let block = save(m);
    m.sh(off(block, 0x358), 0);
    m.sh(off(block, 0x35a), 0);
    m.sh(off(block, 0x35c), 0);
    reset_flags(m);
    reset_accounts(m);
    nothing();
    call(m, RESET_PROBE, [0; 4]);
    let block = save(m);
    m.sb(off(block, 0x336), 0);
    let block2 = save(m);
    m.sw(off(block, 0x320), 0);
    for offset in [0x4c, 0x4e, 0x50, 0x52] {
        m.sh(off(block, offset), 0);
    }
    m.sb(off(block2, 0x334), 0);
    let block = save(m);
    for i in 0..3 {
        let word = m.lw(off(EMPTY_ACCOUNT, 4 * i));
        m.sw(off(block, 0x338 + 4 * i), word);
    }
    let half = lh(m, off(EMPTY_ACCOUNT, 0xc));
    m.sh(off(block, 0x344), half as u16);
    let target = save(m);
    for i in 0..5 {
        let word = m.lw(off(FILE_NAME_0, 4 * i));
        m.sw(off(target, 0x2b4 + 4 * i), word);
    }
    let half = lh(m, off(FILE_NAME_0, 0x14));
    m.sh(off(target, 0x2c8), half as u16);
    let byte = lb(m, off(FILE_NAME_0, 0x16));
    m.sb(off(target, 0x2ca), byte as u8);
    let target = save(m);
    for i in 0..5 {
        let word = load_unaligned(m, off(FILE_NAME_1, 4 * i));
        store_unaligned(m, off(target, 0x2ce + 4 * i), word);
    }
    let half = lh(m, off(FILE_NAME_1, 0x14));
    m.sh(off(target, 0x2e2), half as u16);
    let byte = lb(m, off(FILE_NAME_1, 0x16));
    m.sb(off(target, 0x2e4), byte as u8);
    for slot in 0..2u32 {
        let at = off(block, sext16(slot) << 1);
        m.sh(off(at, 0x2c), 2);
        m.sh(off(at, 0x30), 2);
    }
    m.pop_frame(0x18);
}

// ---- card events ---------------------------------------------------------------------

/// 800D56E0 (JP; PAL 800CC46C, changed): open and enable the seven card
/// events (SW_CARD done, error, timeout, new card; HW_CARD done, error,
/// timeout) in a critical section. Returns ExitCriticalSection's `v0`.
pub fn open_events(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    bios::enter_critical_section(m);
    let specs = [
        (bios::SW_CARD, bios::EV_IOE),
        (bios::SW_CARD, bios::EV_ERROR),
        (bios::SW_CARD, bios::EV_TIMEOUT),
        (bios::SW_CARD, bios::EV_NEW),
        (bios::HW_CARD, bios::EV_IOE),
        (bios::HW_CARD, bios::EV_ERROR),
        (bios::HW_CARD, bios::EV_TIMEOUT),
    ];
    for (i, (class, spec)) in specs.into_iter().enumerate() {
        let handle = bios::open_event(m, class, spec, bios::EV_MODE_NO_INTERRUPT, 0);
        let block = save(m);
        m.sw(off(block, 4 + 4 * i as u32), handle);
    }
    let mut v0 = 0;
    for i in 0..7 {
        let block = save(m);
        let handle = m.lw(off(block, 4 + 4 * i));
        v0 = bios::enable_event(m, handle);
    }
    let v0 = bios::exit_critical_section(m, v0);
    m.pop_frame(0x18);
    v0
}

/// 800D5870 (JP; PAL 800CC62C, changed; unreferenced): clear the changed
/// flag.
pub fn clear_changed(m: &mut Machine) {
    let block = save(m);
    m.sw(off(block, 0x28), 0);
}

/// 800D5880 (JP; PAL 800CB5B4, changed; unreferenced): allocate the blocks,
/// open the events, reset the save state.
pub fn setup(m: &mut Machine) {
    m.push_frame(0x18);
    alloc_blocks(m);
    open_events(m);
    reset_save_state(m);
    m.pop_frame(0x18);
}

/// 800D58B0 (JP; PAL 800CC6F4, near): close the seven card events in a
/// critical section. Returns ExitCriticalSection's `v0`.
pub fn close_events(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    bios::enter_critical_section(m);
    let mut v0 = 0;
    for i in 0..7 {
        let block = save(m);
        let handle = m.lw(off(block, 4 + 4 * i));
        v0 = bios::close_event(m, handle);
    }
    let v0 = bios::exit_critical_section(m, v0);
    m.pop_frame(0x18);
    v0
}

// ---- getters and setters -------------------------------------------------------------

/// 800D5970: the name of account `index` (0 the current one, 1..3 the
/// saved ones), else the empty string.
pub fn account_name(m: &mut Machine, index: u32) -> u32 {
    let block = save(m);
    if index == 0 {
        return off(m.lw(off(block, 0x2f8)), 8);
    }
    if slt(index, 4) {
        let entry = off(block, index.wrapping_sub(1) << 2);
        return off(m.lw(off(entry, 0x2ec)), 8);
    }
    EMPTY
}

/// 800D59C0: bit `bit` of the record bits of account `index` (-1: the
/// current one).
pub fn record_bit(m: &mut Machine, index: u32, bit: u32) -> u32 {
    let block = save(m);
    let record = if index == u32::MAX {
        m.lw(off(block, 0x2f8))
    } else {
        m.lw(off(off(block, index << 2), 0x2ec))
    };
    sra(lh(m, off(record, 0x16)), bit) & 1
}

/// 800D5A20 (JP; PAL 800CCD50, near; unreferenced): status halfword +0x4E.
pub fn status_4e(m: &mut Machine) -> u32 {
    let block = save(m);
    lh(m, off(block, 0x4e))
}

/// 800D5A38: bit `index` of the account bits (0..2), else -1.
pub fn account_bit(m: &mut Machine, index: u32) -> u32 {
    if index >= 3 {
        return u32::MAX;
    }
    let block = save(m);
    let flags = m.lw(off(block, 0x2e8));
    sra(lbu(m, off(flags, 8)), index) & 1
}

fn record_of(m: &mut Machine, index: u32) -> u32 {
    let block = save(m);
    m.lw(off(off(block, (index & 0xff) << 2), 0x2fc))
}

/// 800D5A70: the difficulty name of record `index`.
pub fn record_difficulty_name(m: &mut Machine, index: u32) -> u32 {
    let record = record_of(m, index);
    let difficulty = lbu(m, off(record, 0x112));
    m.lw(off(DIFFICULTY_NAMES, difficulty << 2))
}

/// 800D5AA8 (JP; PAL 800CCD68, changed; unreferenced): the difficulty of
/// the current record.
pub fn current_difficulty(m: &mut Machine) -> u32 {
    let block = save(m);
    let record = m.lw(off(block, 0x314));
    lbu(m, off(record, 0x112))
}

/// 800D5AC8: the play time of record `index` as "hhh:mm:ss" (frames at
/// 30 per second, at most 0x066FF2FF; spaces become zeros) in the buffer
/// 0x800DDA38, which it returns.
pub fn record_time_text(m: &mut Machine, index: u32) -> u32 {
    let frame = m.push_frame(0x20);
    let record = record_of(m, index);
    let mut time = m.lw(off(record, 0x114));
    if slt(TIME_LIMIT, time) {
        time = TIME_LIMIT;
    }
    if slt(time, 0) {
        time = 0;
    }
    let high = |a: u32, b: u32| ((i64::from(a as i32) * i64::from(b as i32)) >> 32) as u32;
    let sign = sra(time, 31);
    let minutes = sra(high(time, 0x91a2_b3c5).wrapping_add(time), 10).wrapping_sub(sign);
    let seconds = sra(high(time, 0x8888_8889).wrapping_add(time), 4).wrapping_sub(sign);
    let seconds = seconds.wrapping_sub(minutes.wrapping_mul(60));
    m.sw(off(frame, 0x10), seconds);
    let hours = sra(high(time, 0x9b58_3739).wrapping_add(time), 16).wrapping_sub(sign);
    let minutes = minutes.wrapping_sub(hours.wrapping_mul(60));
    sdk::sprintf(m, TIME_TEXT, TIME_FORMAT, hours, minutes);
    for i in 0..12 {
        if lbu(m, off(TIME_TEXT, i)) == 0x20 {
            m.sb(off(TIME_TEXT, i), 0x30);
        }
    }
    m.pop_frame(0x20);
    TIME_TEXT
}

/// 800D5BF4: the mission name of record `index`.
pub fn record_mission_name(m: &mut Machine, index: u32) -> u32 {
    let record = record_of(m, index);
    let mission = lbu(m, off(record, 0x113));
    m.lw(off(MISSION_NAMES, mission << 2))
}

/// 800D5C2C: the faction name of record `index`'s mission.
pub fn record_faction_name(m: &mut Machine, index: u32) -> u32 {
    let record = record_of(m, index);
    let mission = lbu(m, off(record, 0x113));
    let faction = lbu(m, off(MISSION_FACTION, mission));
    m.lw(off(FACTION_NAMES, faction << 2))
}

/// 800D5C74 (unreferenced): the empty string.
pub fn empty_string() -> u32 {
    EMPTY
}

/// 800D5C80 (JP; PAL 800CB70C, changed): the account bits.
pub fn account_bits(m: &mut Machine) -> u32 {
    let block = save(m);
    let flags = m.lw(off(block, 0x2e8));
    lbu(m, off(flags, 8))
}

/// 800D5CA0: the record bits of the current account.
pub fn current_record_bits(m: &mut Machine) -> u32 {
    let block = save(m);
    let record = m.lw(off(block, 0x2f8));
    lh(m, off(record, 0x16))
}

/// 800D5CC0: the mode name of account `index` (+0x464).
pub fn account_mode_name(m: &mut Machine, index: u32) -> u32 {
    let block = save(m);
    let record = m.lw(off(off(block, index << 2), 0x2ec));
    let mode = lbu(m, off(record, 0x464));
    m.lw(off(MODE_NAMES, mode << 2))
}

/// 800D5CF4 / 800D5D0C / 800D5D24 / 800D5D3C (unreferenced): the address of
/// the current account's byte +0x45C / +0x45D / +0x45E / halfword +0x460.
pub fn current_field(m: &mut Machine, offset: u32) -> u32 {
    let block = save(m);
    off(m.lw(off(block, 0x2f8)), offset)
}

/// 800D5D54 (unreferenced): set the current record's difficulty.
pub fn set_current_difficulty(m: &mut Machine, value: u32) {
    let block = save(m);
    let record = m.lw(off(block, 0x314));
    m.sb(off(record, 0x112), value as u8);
}

/// 800D5D6C / 800D5D84 / 800D5D9C (unreferenced): set the current account's
/// byte +0x45C / +0x45D / +0x45E.
pub fn set_current_byte(m: &mut Machine, offset: u32, value: u32) {
    let block = save(m);
    let record = m.lw(off(block, 0x2f8));
    m.sb(off(record, offset), value as u8);
}

/// 800D5DB4 (unreferenced): the saved account (0..2) whose name equals the
/// context's (+0x948) and whose +4 equals context +0x968, else -1.
pub fn find_account(m: &mut Machine) -> u32 {
    m.push_frame(0x28);
    let mut result = u32::MAX;
    for index in 0..3u32 {
        let block = save(m);
        let flags = m.lw(off(block, 0x2e8));
        if sra(lbu(m, off(flags, 8)), index) & 1 == 0 {
            continue;
        }
        let record = m.lw(off(off(block, index << 2), 0x2ec));
        let name = off(context(m), 0x948);
        if call(m, STRCMP, [off(record, 8), name, 0, 0]) != 0 {
            continue;
        }
        let block = save(m);
        let record = m.lw(off(off(block, index << 2), 0x2ec));
        let ctx = context(m);
        let id = m.lw(off(record, 4));
        if id == m.lw(off(ctx, 0x968)) {
            result = index;
            break;
        }
    }
    m.pop_frame(0x28);
    result
}

// ---- card operations -----------------------------------------------------------------

/// 800D5E74 (JP; PAL 800CD918, exact; unreferenced): format the card in
/// `slot` when the probe is idle: 1 on success, -1 on failure, 0 busy.
pub fn format_card(m: &mut Machine, slot: u32) -> u32 {
    m.push_frame(0x18);
    let result = if call(m, CARD_READY, [0; 4]) == 0 {
        0
    } else {
        let ok = bios::card_format(m, slot << 4);
        call(m, RESET_PROBE, [0; 4]);
        if ok == 1 { 1 } else { u32::MAX }
    };
    m.pop_frame(0x18);
    result
}

/// 800D5EC4 (JP; PAL 800CDAA8, near): create the card file of `slot` (two
/// blocks): 1 created, -1 failed, 0 busy.
pub fn create_file(m: &mut Machine, slot: u32) -> u32 {
    m.push_frame(0x20);
    let result = if call(m, CARD_READY, [0; 4]) == 0 {
        0
    } else {
        let block = save(m);
        let fd = bios::open(m, file_name(block, slot), 0x0002_0200);
        let block = save(m);
        m.sw(off(block, 0x3c), fd);
        if fd == u32::MAX {
            bios::close(m, u32::MAX);
            call(m, RESET_PROBE, [0; 4]);
            u32::MAX
        } else {
            bios::close(m, fd);
            call(m, RESET_PROBE, [0; 4]);
            1
        }
    };
    m.pop_frame(0x20);
    result
}

fn probe_step(m: &mut Machine, slot: u32, step: u8) {
    let block = save(m);
    m.sb(off(off(block, slot), 0x34c), step);
}
fn toggle_slot(m: &mut Machine, block: u32) {
    let slot = m.lw(off(block, 0x354));
    m.sw(off(block, 0x354), 1u32.wrapping_sub(slot));
}
/// Count a card-present retry: up to 0x11 the present flag stays clear and
/// the count grows, then the card counts as present (`present`).
fn retry_present(m: &mut Machine, entry: u32, slot: u32, present: u8) {
    if lbu(m, off(entry, 0x350)) < 0x11 {
        m.sb(off(entry, 0x348), 0);
        let entry = off(save(m), slot);
        let count = lbu(m, off(entry, 0x350));
        m.sb(off(entry, 0x350), count.wrapping_add(1) as u8);
    } else {
        m.sb(off(entry, 0x348), present);
    }
}

/// 800D5F6C (JP; PAL 800CBDAC, changed): one step of the card probe of the
/// slot +0x354: 0 switch slot, 1 `_card_info`, 2 wait for its event
/// (timeout 300), 3 evaluate it, 4 wait for the `_card_clear` event, 5
/// `_card_load`, 6 wait for its event, 7 evaluate it, 8 test the card file.
pub fn probe(m: &mut Machine) {
    m.push_frame(0x20);
    let block = save(m);
    let slot = m.lw(off(block, 0x354));
    let step = lbu(m, off(off(block, slot), 0x34c));
    if step < PROBE_STEPS {
        match step {
            0 => {
                let block = save(m);
                toggle_slot(m, block);
            }
            1 => {
                call(m, DRAIN_CARD_EVENTS, [0; 4]);
                bios::card_info(m, slot << 4);
                probe_step(m, slot, 2);
                let block = save(m);
                m.sw(off(block, 0x318), 300);
            }
            2 => {
                let event = call(m, TEST_CARD_EVENTS, [slot, 3, 0, 0]);
                let block = save(m);
                m.sb(off(off(block, slot), 0x34e), event as u8);
                let block = save(m);
                let left = m.lw(off(block, 0x318));
                if (left as i32) < 0 {
                    m.sb(off(off(block, slot), 0x34c), 1);
                } else {
                    m.sw(off(block, 0x318), left.wrapping_sub(1));
                }
            }
            3 => evaluate_info(m, slot),
            4 => loop {
                let event = call(m, TEST_FILE_EVENTS, [slot, 5, 0, 0]);
                let block = save(m);
                m.sb(off(off(block, slot), 0x34e), event as u8);
                let block = save(m);
                if lbu(m, off(off(block, slot), 0x34e)) != 0xff {
                    break;
                }
            },
            5 => {
                let block = save(m);
                let event = lbu(m, off(off(block, slot), 0x34e));
                m.sh(off(off(block, slot << 1), 0x2c), event as u16);
                call(m, DRAIN_CARD_EVENTS, [0; 4]);
                bios::card_load(m, slot << 4);
                probe_step(m, slot, 6);
            }
            6 => {
                let event = call(m, TEST_CARD_EVENTS, [slot, 7, 0, 0]);
                let block = save(m);
                m.sb(off(off(block, slot), 0x34e), event as u8);
            }
            7 => evaluate_load(m, slot),
            _ => {
                let block = save(m);
                let fd = bios::open(m, file_name(block, slot), 0x8001);
                let block = save(m);
                m.sw(off(block, 0x3c), fd);
                if fd != u32::MAX {
                    m.sb(off(off(block, slot), 0x34a), 1);
                }
                let block = save(m);
                let fd = m.lw(off(block, 0x3c));
                bios::close(m, fd);
                probe_step(m, slot, 1);
                let block = save(m);
                m.sb(off(off(block, slot), 0x348), 1);
                let block = save(m);
                toggle_slot(m, block);
            }
        }
    }
    m.pop_frame(0x20);
}

/// Step 3 of [`probe`] (800D6058): the `_card_info` event.
fn evaluate_info(m: &mut Machine, slot: u32) {
    let block = save(m);
    let entry = off(block, slot);
    let event = lbu(m, off(entry, 0x34e));
    let status = off(block, slot << 1);
    m.sh(off(status, 0x2c), event as u16);
    match event {
        0 => {
            if lbu(m, off(entry, 0x348)) != 1 {
                call(m, DRAIN_FILE_EVENTS, [0; 4]);
                bios::card_clear(m, slot << 4);
                probe_step(m, slot, 4);
            } else {
                let changed = m.lw(off(block, 0x28));
                m.sb(off(entry, 0x34c), u8::from(changed != 0));
                let block = save(m);
                toggle_slot(m, block);
            }
        }
        1 => {
            retry_present(m, entry, slot, 1);
            let block = save(m);
            let changed = m.lw(off(block, 0x28));
            m.sb(off(off(block, slot), 0x34c), u8::from(changed == 1));
            let block = save(m);
            toggle_slot(m, block);
        }
        2 => {
            retry_present(m, entry, slot, 1);
            let block = save(m);
            let changed = m.lw(off(block, 0x28));
            m.sb(off(off(block, slot), 0x34c), u8::from(changed != 0));
            let block = save(m);
            toggle_slot(m, block);
            m.sb(off(off(block, slot), 0x34a), 0);
        }
        3 => {
            m.sh(off(status, 0x2c), 0);
            call(m, DRAIN_FILE_EVENTS, [0; 4]);
            bios::card_clear(m, slot << 4);
            probe_step(m, slot, 4);
            let block = save(m);
            m.sb(off(off(block, slot), 0x348), 0);
            let block = save(m);
            m.sb(off(off(block, slot), 0x350), 0);
        }
        _ => {
            let block = save(m);
            m.sb(off(off(block, slot), 0x348), 1);
            let block = save(m);
            m.sb(off(off(block, slot), 0x34c), 1);
            let block = save(m);
            toggle_slot(m, block);
        }
    }
}

/// Step 7 of [`probe`] (800D6324): the `_card_load` event.
fn evaluate_load(m: &mut Machine, slot: u32) {
    let block = save(m);
    let entry = off(block, slot);
    let event = lbu(m, off(entry, 0x34e));
    m.sh(off(off(block, slot << 1), 0x30), event as u16);
    m.sb(off(entry, 0x348), 1);
    let block = save(m);
    let status = lh(m, off(off(block, slot << 1), 0x30));
    let entry = off(block, slot);
    match status as i32 {
        0 => m.sb(off(entry, 0x34c), 8),
        1 | 2 => {
            retry_present(m, entry, slot, 1);
            probe_step(m, slot, 1);
        }
        3 => {
            m.sb(off(entry, 0x348), 1);
            probe_step(m, slot, 1);
            let block = save(m);
            toggle_slot(m, block);
        }
        _ => {}
    }
}

/// 800D64C4 (JP; PAL 800CD968, near): list the files of the card in
/// `slot` (pattern "bu<slot>0:*.*"): their names (20 bytes from +0x5C +
/// 0x12C slot), their count (+0x58), and the free blocks (+0x54, from 15
/// minus the files' 8 KiB blocks), which it returns; -1 busy.
pub fn list_files(m: &mut Machine, slot: u32) -> u32 {
    let frame = m.push_frame(0x68);
    let result = if call(m, CARD_READY, [0; 4]) == 0 {
        u32::MAX
    } else {
        let block = save(m);
        m.sh(off(off(block, slot << 1), 0x54), 15);
        sdk::sprintf(m, off(frame, 0x10), DIRECTORY_PATTERN, slot, 0);
        let entry = off(frame, 0x20);
        let mut count = 0u32;
        if bios::first_file(m, off(frame, 0x10), entry) != 0 {
            let names = slot.wrapping_mul(300).wrapping_add(0x5c);
            let mut name = 0u32;
            loop {
                let block = save(m);
                call(m, STRCPY, [off(off(block, names), name), entry, 0, 0]);
                let size = m.lw(off(frame, 0x38));
                let block = save(m);
                let mut rounded = size.wrapping_add(0x1fff);
                let blocks = off(block, slot << 1);
                if (rounded as i32) < 0 {
                    rounded = size.wrapping_add(0x3ffe);
                }
                name = name.wrapping_add(0x14);
                count = count.wrapping_add(1);
                let free = lhu(m, off(blocks, 0x54)).wrapping_sub(sra(rounded, 13));
                m.sh(off(blocks, 0x54), free as u16);
                if bios::next_file(m, entry) == 0 {
                    break;
                }
            }
        }
        let block = save(m);
        m.sh(off(off(block, slot << 1), 0x58), count as u16);
        call(m, RESET_PROBE, [0; 4]);
        let block = save(m);
        lh(m, off(off(block, slot << 1), 0x54))
    };
    m.pop_frame(0x68);
    result
}

/// The end of a failed open or transfer once the retries are used up.
fn give_up(m: &mut Machine, record: bool) -> u32 {
    call(m, RESET_PROBE, [0; 4]);
    let block = save(m);
    m.sh(off(block, 0x35a), 0);
    if record {
        m.sh(off(block, 0x50), 0);
    }
    u32::MAX
}

/// Open the card file of `slot` with `mode` into +0x3C: the descriptor.
fn open_file(m: &mut Machine, block: u32, slot: u32, mode: u32) -> u32 {
    let fd = bios::open(m, file_name(block, slot), mode);
    let block = save(m);
    m.sw(off(block, 0x3c), fd);
    fd
}

/// Wait for the transfer's event (800D3EA8, step 0): `None` while none
/// fired, else the file closed and the event (0 done).
fn finish_transfer(m: &mut Machine, slot: u32) -> Option<u32> {
    let event = call(m, TEST_CARD_EVENTS, [slot, 0, 0, 0]);
    if event == u32::MAX {
        return None;
    }
    let block = save(m);
    let fd = m.lw(off(block, 0x3c));
    bios::close(m, fd);
    let block = save(m);
    m.sb(off(block, 0x336), 0);
    Some(event)
}

/// 800D6604: read the account flags (0x80 bytes at 0x280) of the card in
/// `slot`, one step per call: 0 in progress (or busy), 1 done and valid
/// (`reset` 1: the probe is reset too), 2 bad checksum or version, -1
/// failed.
pub fn read_flags(m: &mut Machine, slot: u32, reset: u32) -> u32 {
    m.push_frame(0x20);
    let result = read_flags_body(m, slot, reset);
    m.pop_frame(0x20);
    result
}

fn read_flags_body(m: &mut Machine, slot: u32, reset: u32) -> u32 {
    if call(m, CARD_READY, [0; 4]) == 0 {
        return 0;
    }
    let block = begin_transfer(m);
    if lbu(m, off(block, 0x336)) == 0 {
        let fd = open_file(m, block, slot, 0x8001);
        if fd == u32::MAX {
            let block = save(m);
            if retry(m, block) {
                return 0;
            }
            return give_up(m, false);
        }
        bios::lseek(m, fd, 0x280, bios::SEEK_SET);
        let block = save(m);
        let fd = m.lw(off(block, 0x3c));
        let flags = m.lw(off(block, 0x2e8));
        bios::read(m, fd, flags, 0x80);
        let block = save(m);
        m.sb(off(block, 0x336), 1);
    }
    let Some(event) = finish_transfer(m, slot) else {
        return 0;
    };
    let block = save(m);
    let flags = m.lw(off(block, 0x2e8));
    let sum = checksum(m, flags, 0x1f);
    let block = save(m);
    let flags = m.lw(off(block, 0x2e8));
    if m.lw(flags) != sum || m.lw(off(flags, 4)) != FLAGS_VERSION {
        call(m, RESET_PROBE, [0; 4]);
        let block = save(m);
        m.sh(off(block, 0x35a), 0);
        return 2;
    }
    if event != 0 {
        if retry(m, block) {
            return 0;
        }
        m.sh(off(block, 0x35a), 0);
        call(m, RESET_PROBE, [0; 4]);
        return u32::MAX;
    }
    m.sh(off(block, 0x35a), 0);
    if reset == 1 {
        call(m, RESET_PROBE, [0; 4]);
    }
    1
}

/// 800D6814: read the account records (0x480 bytes at 0x300 + 0x480 n) of
/// the accounts present, n = +0x50 = 0..2, one step per call: 0 in
/// progress, 1 all read (`reset` 1: the probe is reset too), 2 bad
/// checksum, -1 failed.
pub fn read_accounts(m: &mut Machine, slot: u32, reset: u32) -> u32 {
    m.push_frame(0x20);
    let result = read_accounts_body(m, slot, reset);
    m.pop_frame(0x20);
    result
}

fn read_accounts_body(m: &mut Machine, slot: u32, reset: u32) -> u32 {
    if call(m, CARD_READY, [0; 4]) == 0 {
        return 0;
    }
    let block = save(m);
    let flags = m.lw(off(block, 0x2e8));
    let index = lh(m, off(block, 0x50));
    let present = sra(lbu(m, off(flags, 8)), index) & 1 != 0;
    if present {
        if call(m, CARD_READY, [0; 4]) == 0 {
            return 0;
        }
        let block = begin_transfer(m);
        if lbu(m, off(block, 0x336)) == 0 {
            let fd = open_file(m, block, slot, 0x8001);
            let block = save(m);
            if fd == u32::MAX {
                if retry(m, block) {
                    return 0;
                }
                return give_up(m, true);
            }
            let index = lh(m, off(block, 0x50));
            let offset = (index << 3).wrapping_add(index) << 7;
            bios::lseek(m, fd, offset.wrapping_add(0x300), bios::SEEK_SET);
            let block = save(m);
            let index = lh(m, off(block, 0x50));
            let fd = m.lw(off(block, 0x3c));
            let record = m.lw(off(off(block, index << 2), 0x2ec));
            bios::read(m, fd, record, 0x480);
            let block = save(m);
            m.sb(off(block, 0x336), 1);
        }
        let Some(event) = finish_transfer(m, slot) else {
            return 0;
        };
        if event != 0 {
            let block = save(m);
            if retry(m, block) {
                return 0;
            }
            return give_up(m, true);
        }
        let block = save(m);
        let index = lh(m, off(block, 0x50));
        let record = m.lw(off(off(block, index << 2), 0x2ec));
        let sum = checksum(m, record, 0x11f);
        let block = save(m);
        let index = lh(m, off(block, 0x50));
        let record = m.lw(off(off(block, index << 2), 0x2ec));
        if m.lw(record) != sum {
            call(m, RESET_PROBE, [0; 4]);
            let block = save(m);
            m.sh(off(block, 0x35a), 0);
            m.sh(off(block, 0x50), 0);
            return 2;
        }
    }
    let block = save(m);
    let index = lh(m, off(block, 0x50));
    let next = lhu(m, off(block, 0x50)).wrapping_add(1);
    if slt(index, 2) {
        m.sh(off(block, 0x50), next as u16);
        return 0;
    }
    m.sh(off(block, 0x50), 0);
    if reset == 1 {
        call(m, RESET_PROBE, [0; 4]);
    }
    1
}

/// 800D6AB4: read the six records (0x180 bytes at 0x1080 + 0x900 account +
/// 0x180 k) of `account` whose record bit is set, k = +0x50 = 0..5, one
/// step per call: as [`read_accounts`].
pub fn read_records(m: &mut Machine, slot: u32, account: u32, reset: u32) -> u32 {
    m.push_frame(0x28);
    let result = read_records_body(m, slot, account, reset);
    m.pop_frame(0x28);
    result
}

fn read_records_body(m: &mut Machine, slot: u32, account: u32, reset: u32) -> u32 {
    if call(m, CARD_READY, [0; 4]) == 0 {
        return 0;
    }
    let block = save(m);
    let record = m.lw(off(off(block, account << 2), 0x2ec));
    let index = lh(m, off(block, 0x50));
    let present = sra(lh(m, off(record, 0x16)), index) & 1 != 0;
    if present {
        if call(m, CARD_READY, [0; 4]) == 0 {
            return 0;
        }
        let block = begin_transfer(m);
        if lbu(m, off(block, 0x336)) == 0 {
            let fd = bios::open(m, file_name(block, slot), 0x8001);
            let block = save(m);
            m.sw(off(block, 0x3c), fd);
            if fd == u32::MAX {
                if retry(m, block) {
                    return 0;
                }
                m.sh(off(block, 0x35a), 0);
                call(m, RESET_PROBE, [0; 4]);
                let block = save(m);
                m.sh(off(block, 0x50), 0);
                return u32::MAX;
            }
            let index = lh(m, off(block, 0x50));
            let base = ((account << 3).wrapping_add(account) << 8).wrapping_add(0x1080);
            let offset = ((index << 1).wrapping_add(index) << 7).wrapping_add(base);
            bios::lseek(m, fd, offset, bios::SEEK_SET);
            let block = save(m);
            let index = lh(m, off(block, 0x50));
            let fd = m.lw(off(block, 0x3c));
            let record = m.lw(off(off(block, index << 2), 0x2fc));
            bios::read(m, fd, record, 0x180);
            let block = save(m);
            m.sb(off(block, 0x336), 1);
        }
        let Some(event) = finish_transfer(m, slot) else {
            return 0;
        };
        if event != 0 {
            let block = save(m);
            if retry(m, block) {
                return 0;
            }
            m.sh(off(block, 0x35a), 0);
            call(m, RESET_PROBE, [0; 4]);
            let block = save(m);
            m.sh(off(block, 0x50), 0);
            return u32::MAX;
        }
        let block = save(m);
        let index = lh(m, off(block, 0x50));
        let record = m.lw(off(off(block, index << 2), 0x2fc));
        let sum = checksum(m, record, 0x5f);
        let block = save(m);
        let index = lh(m, off(block, 0x50));
        let record = m.lw(off(off(block, index << 2), 0x2fc));
        if m.lw(record) != sum {
            call(m, RESET_PROBE, [0; 4]);
            let block = save(m);
            m.sh(off(block, 0x35a), 0);
            m.sh(off(block, 0x50), 0);
            return 2;
        }
    }
    let block = save(m);
    let index = lh(m, off(block, 0x50));
    let next = lhu(m, off(block, 0x50)).wrapping_add(1);
    if slt(index, 6) {
        m.sh(off(block, 0x50), next as u16);
        return 0;
    }
    if reset == 1 {
        call(m, RESET_PROBE, [0; 4]);
    }
    let block = save(m);
    m.sh(off(block, 0x35a), 0);
    m.sh(off(block, 0x50), 0);
    1
}

/// A write's last steps (the common tail of 800D6D78, 800D6F04, 800D713C,
/// 800D73B8 from the transfer's event on): 0 in progress, 1 done, -1
/// failed (`reset_after`: whether +0x35A is cleared after the probe reset,
/// as the open failure does, or before it).
fn finish_write(m: &mut Machine, slot: u32, reset: u32, reset_after: bool) -> u32 {
    let Some(event) = finish_transfer(m, slot) else {
        return 0;
    };
    if event != 0 {
        let block = save(m);
        if retry(m, block) {
            return 0;
        }
        if reset_after {
            return give_up(m, false);
        }
        m.sh(off(block, 0x35a), 0);
        call(m, RESET_PROBE, [0; 4]);
        return u32::MAX;
    }
    if reset == 1 {
        call(m, RESET_PROBE, [0; 4]);
    }
    1
}

/// 800D6D78: write the card file head (0x280 bytes from `[0x800DDA44]`) to
/// the start of the file of `slot`, one step per call: 0 in progress, 1
/// done (`reset` 1: the probe is reset too), -1 failed.
pub fn write_head(m: &mut Machine, slot: u32, reset: u32) -> u32 {
    m.push_frame(0x20);
    let result = write_head_body(m, slot, reset);
    m.pop_frame(0x20);
    result
}

fn write_head_body(m: &mut Machine, slot: u32, reset: u32) -> u32 {
    if call(m, CARD_READY, [0; 4]) == 0 {
        return 0;
    }
    let block = begin_transfer(m);
    if lbu(m, off(block, 0x336)) == 0 {
        let fd = open_file(m, block, slot, 0x8002);
        if fd == u32::MAX {
            let block = save(m);
            if retry(m, block) {
                return 0;
            }
            return give_up(m, false);
        }
        let head = m.lw(HEAD);
        bios::write(m, fd, head, 0x280);
        let block = save(m);
        m.sb(off(block, 0x336), 1);
    }
    finish_write(m, slot, reset, false)
}

/// 800D6F04: write the account flags (account bit `account` set and the
/// save count incremented, or for a negative `account` both cleared; the
/// checksum updated) at 0x280 of the file of `slot`: as [`write_head`].
pub fn write_flags(m: &mut Machine, slot: u32, account: u32, reset: u32) -> u32 {
    m.push_frame(0x20);
    let result = write_flags_body(m, slot, account, reset);
    m.pop_frame(0x20);
    result
}

fn write_flags_body(m: &mut Machine, slot: u32, account: u32, reset: u32) -> u32 {
    if call(m, CARD_READY, [0; 4]) == 0 {
        return 0;
    }
    let block = begin_transfer(m);
    if lbu(m, off(block, 0x336)) == 0 {
        if (account as i32) < 0 {
            let flags = m.lw(off(block, 0x2e8));
            m.sb(off(flags, 8), 0);
            let block = save(m);
            let flags = m.lw(off(block, 0x2e8));
            m.sw(off(flags, 0xc), 0);
        } else {
            let flags = m.lw(off(block, 0x2e8));
            let bits = lbu(m, off(flags, 8)) | (1 << (account & 31));
            m.sb(off(flags, 8), bits as u8);
            let block = save(m);
            let flags = m.lw(off(block, 0x2e8));
            let count = m.lw(off(flags, 0xc));
            m.sw(off(flags, 0xc), count.wrapping_add(1));
        }
        let block = save(m);
        let flags = m.lw(off(block, 0x2e8));
        let sum = checksum(m, flags, 0x1f);
        let block = save(m);
        let flags = m.lw(off(block, 0x2e8));
        m.sw(flags, sum);
        let fd = open_file(m, block, slot, 0x8002);
        if fd == u32::MAX {
            let block = save(m);
            if retry(m, block) {
                return 0;
            }
            return give_up(m, false);
        }
        bios::lseek(m, fd, 0x280, bios::SEEK_SET);
        let block = save(m);
        let fd = m.lw(off(block, 0x3c));
        let flags = m.lw(off(block, 0x2e8));
        bios::write(m, fd, flags, 0x80);
        let block = save(m);
        m.sb(off(block, 0x336), 1);
    }
    finish_write(m, slot, reset, true)
}

/// 800D713C: write the current account record (+0x2F8: record index
/// `record` and its bit set over the bits of account `account`; `record` 6
/// clears both) at 0x300 + 0x480 `account` of the file of `slot`, with its
/// checksum: as [`write_head`].
pub fn write_account(m: &mut Machine, slot: u32, account: u32, record: u32, reset: u32) -> u32 {
    m.push_frame(0x28);
    let result = write_account_body(m, slot, account, record, reset);
    m.pop_frame(0x28);
    result
}

fn write_account_body(m: &mut Machine, slot: u32, account: u32, record: u32, reset: u32) -> u32 {
    if call(m, CARD_READY, [0; 4]) == 0 {
        return 0;
    }
    begin_transfer(m);
    if record < 6 {
        let block = save(m);
        let current = m.lw(off(block, 0x2f8));
        m.sh(off(current, 0x14), record as u16);
        let saved = m.lw(off(off(block, account << 2), 0x2ec));
        let current = m.lw(off(block, 0x2f8));
        let bits = lhu(m, off(saved, 0x16));
        m.sh(off(current, 0x16), bits as u16);
        let current = m.lw(off(block, 0x2f8));
        let bits = lhu(m, off(current, 0x16)) | (1 << (record & 31));
        m.sh(off(current, 0x16), bits as u16);
    } else if record == 6 {
        let block = save(m);
        let current = m.lw(off(block, 0x2f8));
        m.sh(off(current, 0x14), 0);
        let current = m.lw(off(block, 0x2f8));
        m.sh(off(current, 0x16), 0);
    }
    let block = save(m);
    if lbu(m, off(block, 0x336)) == 0 {
        let current = m.lw(off(block, 0x2f8));
        let sum = checksum(m, current, 0x11f);
        let block = save(m);
        let current = m.lw(off(block, 0x2f8));
        m.sw(current, sum);
        let fd = open_file(m, block, slot, 0x8002);
        if fd == u32::MAX {
            let block = save(m);
            if retry(m, block) {
                return 0;
            }
            return give_up(m, false);
        }
        let offset = ((account << 3).wrapping_add(account) << 7).wrapping_add(0x300);
        bios::lseek(m, fd, offset, bios::SEEK_SET);
        let block = save(m);
        let fd = m.lw(off(block, 0x3c));
        let current = m.lw(off(block, 0x2f8));
        bios::write(m, fd, current, 0x480);
        let block = save(m);
        m.sb(off(block, 0x336), 1);
    }
    finish_write(m, slot, reset, false)
}

/// 800D73B8 (unreferenced): write the current record (+0x314, with its
/// checksum) as record `record` of `account` (0x1080 + 0x900 account +
/// 0x180 record) of the file of `slot`: as [`write_head`].
pub fn write_record(m: &mut Machine, slot: u32, account: u32, record: u32, reset: u32) -> u32 {
    m.push_frame(0x28);
    let result = write_record_body(m, slot, account, record, reset);
    m.pop_frame(0x28);
    result
}

fn write_record_body(m: &mut Machine, slot: u32, account: u32, record: u32, reset: u32) -> u32 {
    if call(m, CARD_READY, [0; 4]) == 0 {
        return 0;
    }
    let block = begin_transfer(m);
    if lbu(m, off(block, 0x336)) == 0 {
        let current = m.lw(off(block, 0x314));
        let sum = checksum(m, current, 0x5f);
        let block = save(m);
        let current = m.lw(off(block, 0x314));
        m.sw(current, sum);
        let fd = open_file(m, block, slot, 0x8002);
        if fd == u32::MAX {
            let block = save(m);
            if retry(m, block) {
                return 0;
            }
            return give_up(m, false);
        }
        let offset = ((record << 1).wrapping_add(record) << 7)
            .wrapping_add(((account << 3).wrapping_add(account) << 8).wrapping_add(0x1080));
        bios::lseek(m, fd, offset, bios::SEEK_SET);
        let block = save(m);
        let fd = m.lw(off(block, 0x3c));
        let current = m.lw(off(block, 0x314));
        bios::write(m, fd, current, 0x180);
        let block = save(m);
        m.sb(off(block, 0x336), 1);
    }
    finish_write(m, slot, reset, true)
}

/// 800D75B4 (JP; PAL 800D3254, changed; unreferenced): copy `name` to the
/// current account's name.
pub fn set_current_name(m: &mut Machine, name: u32) {
    m.push_frame(0x18);
    let block = save(m);
    let record = m.lw(off(block, 0x2f8));
    call(m, STRCPY, [off(record, 8), name, 0, 0]);
    m.pop_frame(0x18);
}

/// 800D75E4 (table 0x800DD808): reset the menu state's counters (+0x48,
/// +0x10; +0xE = 1).
pub fn reset_menu_counters(m: &mut Machine) {
    let state = m.lw(MENU_STATE);
    m.sw(off(state, 0x48), 0);
    m.sh(off(state, 0x10), 0);
    m.sh(off(state, 0xe), 1);
}

/// 800D7600 (table 0x800DD80C): when the probe is idle, close the card
/// events and mark the menu state (+0x18 = 0, +0x58 = 1): 1, else 0.
pub fn release_card(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let result = if call(m, CARD_READY, [0; 4]) == 0 {
        0
    } else {
        close_events(m);
        let state = m.lw(MENU_STATE);
        m.sw(off(state, 0x18), 0);
        m.sb(off(state, 0x58), 1);
        1
    };
    m.pop_frame(0x18);
    result
}

/// The routines of this file by JP address, while module 1 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if !loaded(m) {
        return None;
    }
    let [a0, a1, a2, a3] = args;
    Some(match target {
        0x800d_4d54 => {
            clear_account_bits(m);
            0
        }
        0x800d_4d6c => {
            reset_account_name(m, a0);
            0
        }
        0x800d_4df4 => {
            load_record(m, a0);
            0
        }
        0x800d_4f0c => {
            alloc_blocks(m);
            0
        }
        0x800d_4ff0 => {
            build_head(m, a0, a1);
            0
        }
        0x800d_51fc => {
            reset_flags(m);
            0
        }
        0x800d_5270 => {
            reset_flags_call(m);
            0
        }
        0x800d_5290 => {
            reset_accounts(m);
            0
        }
        0x800d_5528 => {
            reset_accounts_call(m);
            0
        }
        0x800d_5548 => {
            nothing();
            0
        }
        0x800d_5550 => {
            reset_save_state(m);
            0
        }
        0x800d_56e0 => open_events(m),
        0x800d_5870 => {
            clear_changed(m);
            0
        }
        0x800d_5880 => {
            setup(m);
            0
        }
        0x800d_58b0 => close_events(m),
        0x800d_5970 => account_name(m, a0),
        0x800d_59c0 => record_bit(m, a0, a1),
        0x800d_5a20 => status_4e(m),
        0x800d_5a38 => account_bit(m, a0),
        0x800d_5a70 => record_difficulty_name(m, a0),
        0x800d_5aa8 => current_difficulty(m),
        0x800d_5ac8 => record_time_text(m, a0),
        0x800d_5bf4 => record_mission_name(m, a0),
        0x800d_5c2c => record_faction_name(m, a0),
        0x800d_5c74 => empty_string(),
        0x800d_5c80 => account_bits(m),
        0x800d_5ca0 => current_record_bits(m),
        0x800d_5cc0 => account_mode_name(m, a0),
        0x800d_5cf4 => current_field(m, 0x45c),
        0x800d_5d0c => current_field(m, 0x45d),
        0x800d_5d24 => current_field(m, 0x45e),
        0x800d_5d3c => current_field(m, 0x460),
        0x800d_5d54 => {
            set_current_difficulty(m, a0);
            0
        }
        0x800d_5d6c => {
            set_current_byte(m, 0x45c, a0);
            0
        }
        0x800d_5d84 => {
            set_current_byte(m, 0x45d, a0);
            0
        }
        0x800d_5d9c => {
            set_current_byte(m, 0x45e, a0);
            0
        }
        0x800d_5db4 => find_account(m),
        0x800d_5e74 => format_card(m, a0),
        0x800d_5ec4 => create_file(m, a0),
        0x800d_5f6c => {
            probe(m);
            0
        }
        0x800d_64c4 => list_files(m, a0),
        0x800d_6604 => read_flags(m, a0, a1),
        0x800d_6814 => read_accounts(m, a0, a1),
        0x800d_6ab4 => read_records(m, a0, a1, a2),
        0x800d_6d78 => write_head(m, a0, a1),
        0x800d_6f04 => write_flags(m, a0, a1, a2),
        0x800d_713c => write_account(m, a0, a1, a2, a3),
        0x800d_73b8 => write_record(m, a0, a1, a2, a3),
        0x800d_75b4 => {
            set_current_name(m, a0);
            0
        }
        0x800d_75e4 => {
            reset_menu_counters(m);
            0
        }
        0x800d_7600 => release_card(m),
        _ => return None,
    })
}
