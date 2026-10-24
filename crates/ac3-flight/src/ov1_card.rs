//! Overlay 1 (front-end menus): memory-card state and save data on the
//! address-preserving machine (`800CBAA8`..`800CE354`).
//!
//! The card block `[0x800D640C]` (static, 0x800D8970): +0 the save file
//! image (0x11FC bytes: header with "SC" magic, icon, +0x180 the current
//! game record (+0x180 checksum, +0x184.. data), +0x5A4 six 0x12C-byte
//! mission saves, +0xCAC the backup record), +0x11FC..+0x1214 seven BIOS
//! event handles (four card events, three file events), +0x121C card
//! changed flag, +0x1220/+0x1224 card and load status per slot
//! (halfwords), +0x122C file handle, +0x1490/+0x14AA the two file names,
//! +0x14C4 timeout, +0x14C8 card present, +0x14CA file present, +0x14CC
//! probe step, +0x14CE last event, +0x14D0 retry count (bytes per slot),
//! +0x14D4 the slot being probed, +0x14D8 file-open flags per slot,
//! +0x14E0 open retries.
//!
//! The BIOS routines (events, critical sections, files, `_card_*`) are
//! device-facing SDK routines reached through [`dispatch::call`]; TestEvent
//! is `sdk::test_event` (a host call returning the event state).
use crate::{
    dispatch,
    machine::{Machine, SmallData},
    ov1::{CARD, context, lbu, lh, lhu, off},
};

/// 0x41434533 ("3ECA"): the save-record checksum seed.
const CHECKSUM_SEED: u32 = 0x4143_4533;
/// 0x800C9DE8 "SC", 0x800C9DEC the 48-byte title, 0x800C9E1C its last
/// byte, 0x800C9E20 "CRD0", 0x800C9E28 / 0x800C9E40 the two file names.
const HEADER_MAGIC: u32 = 0x800c_9de8;
const HEADER_TITLE: u32 = 0x800c_9dec;
const HEADER_CLUT: u32 = 0x800c_9e20;
const FILE_NAMES: [u32; 2] = [0x800c_9e28, 0x800c_9e40];

pub const ENTER_CRITICAL_SECTION: u32 = 0x800a_d7e8;
pub const EXIT_CRITICAL_SECTION: u32 = 0x800a_d7f8;
pub const OPEN_EVENT: u32 = 0x800a_d758;
pub const CLOSE_EVENT: u32 = 0x800a_d768;
pub const TEST_EVENT: u32 = 0x800a_d788;
pub const ENABLE_EVENT: u32 = 0x800a_d798;
pub const DISABLE_EVENT: u32 = 0x800a_d7a8;
pub const OPEN: u32 = 0x800a_d808;
pub const READ: u32 = 0x800a_d818;
pub const CLOSE: u32 = 0x800a_d838;
pub const CARD_INFO: u32 = 0x800a_b7e8;
pub const CARD_LOAD: u32 = 0x800a_b7f8;
pub const CARD_CLEAR: u32 = 0x800a_b808;

fn card(m: &mut Machine) -> u32 {
    m.lw(CARD)
}
fn bios(m: &mut Machine, routine: u32, args: [u32; 4]) -> u32 {
    dispatch::call(m, routine, args)
}
fn test_event(m: &mut Machine, handle_offset: u32) -> u32 {
    let card = card(m);
    let handle = m.lw(off(card, handle_offset));
    bios(m, TEST_EVENT, [handle, 0, 0, 0])
}
/// The 26-byte-spaced file name of `slot`.
fn file_name(card: u32, slot: u32) -> u32 {
    off(card, slot.wrapping_mul(26).wrapping_add(0x1490))
}
/// Mission save `slot` (0x12C bytes from +0x5A4).
fn record(card: u32, slot: u32) -> u32 {
    off(card, slot.wrapping_mul(0x12c).wrapping_add(0x5a4))
}

/// XOR of the seed and the `bytes / 4 - 1` words after the first at `base`;
/// the first word holds the expected value.
fn xor_words(m: &mut Machine, base: u32, bytes: u32) -> (u32, u32) {
    let expected = m.lw(base);
    let mut value = CHECKSUM_SEED;
    let count = (bytes as i32 / 4) as u32;
    for i in 1..count {
        value ^= m.lw(off(base, 4 * i));
    }
    (value, expected)
}

// ---- state ------------------------------------------------------------------------

/// 800CBAA8: reset the probe state of both slots (step 1, changed flag
/// set, nothing present). Returns 1 (also left in `a1`).
pub fn reset_state(m: &mut Machine) -> u32 {
    let c = card(m);
    m.sb(off(c, 0x14cc), 1);
    m.sw(off(c, 0x121c), 1);
    for (offset, value) in [(0x14c8, 0), (0x14ca, 0), (0x14d0, 0)] {
        let c = card(m);
        m.sb(off(c, offset), value);
    }
    let c2 = card(m);
    m.sh(off(c, 0x1224), 0);
    m.sb(off(c2, 0x14cd), 1);
    for offset in [0x14c9, 0x14cb, 0x14d1] {
        let c = card(m);
        m.sb(off(c, offset), 0);
    }
    let c3 = card(m);
    m.sh(off(c2, 0x1226), 0);
    m.sw(off(c3, 0x14d4), 0);
    1
}

/// 800CBB20: clear the changed flag; 1 when both slots are idle (step 0
/// for slot 0 and step 0 for slot 1), else 0.
pub fn card_ready(m: &mut Machine) -> u32 {
    let c = card(m);
    let step = lbu(m, off(c, 0x14cc));
    m.sw(off(c, 0x121c), 0);
    if step != 0 {
        0
    } else {
        u32::from(lbu(m, off(c, 0x14cd)) == 0)
    }
}

/// 800CBB50: test the four card events (+0x11FC..+0x1208); the first
/// that fired (0..3) is returned and moves `slot` to `step`; -1 if none.
pub fn test_card_events(m: &mut Machine, slot: u32, step: u32) -> u32 {
    m.push_frame(0x28);
    let mut result = 0xffff_ffff;
    for (index, handle) in [0x11fc, 0x1200, 0x1204, 0x1208].into_iter().enumerate() {
        if test_event(m, handle) == 1 {
            let c = card(m);
            m.sb(off(off(c, slot), 0x14cc), step as u8);
            result = index as u32;
            break;
        }
    }
    m.pop_frame(0x28);
    result
}

/// 800CBC30: test (acknowledge) the four card events.
pub fn drain_card_events(m: &mut Machine) {
    m.push_frame(0x18);
    for handle in [0x11fc, 0x1200, 0x1204, 0x1208] {
        test_event(m, handle);
    }
    m.pop_frame(0x18);
}

/// 800CBC9C: test (acknowledge) the three file events.
pub fn drain_file_events(m: &mut Machine) {
    m.push_frame(0x18);
    for handle in [0x120c, 0x1210, 0x1214] {
        test_event(m, handle);
    }
    m.pop_frame(0x18);
}

/// 800CBCF4: test the three file events (+0x120C..+0x1214), as
/// [`test_card_events`].
pub fn test_file_events(m: &mut Machine, slot: u32, step: u32) -> u32 {
    m.push_frame(0x28);
    let mut result = 0xffff_ffff;
    for (index, handle) in [0x120c, 0x1210, 0x1214].into_iter().enumerate() {
        if test_event(m, handle) == 1 {
            let c = card(m);
            m.sb(off(off(c, slot), 0x14cc), step as u8);
            result = index as u32;
            break;
        }
    }
    m.pop_frame(0x28);
    result
}

/// Toggle the probed slot (+0x14D4 = 1 - itself).
fn next_slot(m: &mut Machine) {
    let c = card(m);
    let slot = m.lw(off(c, 0x14d4));
    m.sw(off(c, 0x14d4), 1u32.wrapping_sub(slot));
}

/// Step byte of `slot` (+0x14CC).
fn set_step(m: &mut Machine, slot: u32, step: u8) {
    let c = card(m);
    m.sb(off(off(c, slot), 0x14cc), step);
}

/// Count a card-present retry: up to 0x11 the present flag stays clear
/// and the count grows, then the card counts as present.
fn retry_present(m: &mut Machine, slot: u32, present: u8) {
    let c = card(m);
    let entry = off(c, slot);
    if lbu(m, off(entry, 0x14d0)) < 0x11 {
        m.sb(off(entry, 0x14c8), 0);
        let c = card(m);
        let entry = off(c, slot);
        let count = lbu(m, off(entry, 0x14d0));
        m.sb(off(entry, 0x14d0), count.wrapping_add(1) as u8);
    } else {
        m.sb(off(entry, 0x14c8), present);
    }
}

/// 800CBDAC: one step of the probe of the current slot (+0x14D4): 0 idle
/// (switch slot), 1 `_card_info`, 2 wait for its event (timeout 300),
/// 3 evaluate (none → `_card_clear` unless present; 1/2 retry, 3 new
/// card → `_card_clear`, error → present), 4 wait for the clear (timeout
/// 1000), 5 `_card_load`, 6 wait for its event, 7 evaluate (0 → step 8,
/// 1/2 retry, 3 present), 8 test the save file with `open`/`close`.
pub fn poll(m: &mut Machine) {
    m.push_frame(0x20);
    let c = card(m);
    let slot = m.lw(off(c, 0x14d4));
    let step = lbu(m, off(off(c, slot), 0x14cc));
    match step {
        0 => next_slot(m),
        1 => {
            drain_card_events(m);
            bios(m, CARD_INFO, [slot << 4, 0, 0, 0]);
            set_step(m, slot, 2);
            let c = card(m);
            m.sw(off(c, 0x14c4), 300);
        }
        2 => {
            let event = test_card_events(m, slot, 3);
            let c = card(m);
            m.sb(off(off(c, slot), 0x14ce), event as u8);
            timeout(m, slot);
        }
        3 => evaluate_info(m, slot),
        4 => loop {
            let event = test_file_events(m, slot, 5);
            let c = card(m);
            m.sb(off(off(c, slot), 0x14ce), event as u8);
            let c = card(m);
            let left = m.lw(off(c, 0x14c4));
            if (left as i32) < 0 {
                set_step(m, slot, 1);
                break;
            }
            m.sw(off(c, 0x14c4), left.wrapping_sub(1));
            if lbu(m, off(off(c, slot), 0x14ce)) != 0xff {
                break;
            }
        },
        5 => {
            let c = card(m);
            let event = lbu(m, off(off(c, slot), 0x14ce));
            m.sh(off(c, (slot << 1).wrapping_add(0x1220)), event as u16);
            drain_card_events(m);
            bios(m, CARD_LOAD, [slot << 4, 0, 0, 0]);
            set_step(m, slot, 6);
        }
        6 => {
            let event = test_card_events(m, slot, 7);
            let c = card(m);
            m.sb(off(off(c, slot), 0x14ce), event as u8);
        }
        7 => evaluate_load(m, slot),
        8 => {
            let c = card(m);
            let handle = bios(m, OPEN, [file_name(c, slot), 0x8001, 0, 0]);
            let c = card(m);
            m.sw(off(c, 0x122c), handle);
            if handle != 0xffff_ffff {
                m.sb(off(off(c, slot), 0x14ca), 1);
            }
            let c = card(m);
            let handle = m.lw(off(c, 0x122c));
            bios(m, CLOSE, [handle, 0, 0, 0]);
            set_step(m, slot, 1);
            let c = card(m);
            m.sb(off(off(c, slot), 0x14c8), 1);
            next_slot(m);
        }
        _ => {}
    }
    m.pop_frame(0x20);
}

/// Steps 2: count the timeout down; past zero restart at step 1.
fn timeout(m: &mut Machine, slot: u32) {
    let c = card(m);
    let left = m.lw(off(c, 0x14c4));
    if (left as i32) < 0 {
        m.sb(off(off(c, slot), 0x14cc), 1);
    } else {
        m.sw(off(c, 0x14c4), left.wrapping_sub(1));
    }
}

/// Step 3 of [`poll`] (`0x800CBE88`).
fn evaluate_info(m: &mut Machine, slot: u32) {
    let c = card(m);
    let entry = off(c, slot);
    let event = lbu(m, off(entry, 0x14ce));
    let status = off(c, slot << 1);
    m.sh(off(status, 0x1220), event as u16);
    match event {
        0 => {
            if lbu(m, off(entry, 0x14c8)) != 1 {
                drain_file_events(m);
                bios(m, CARD_CLEAR, [slot << 4, 0, 0, 0]);
                set_step(m, slot, 4);
                let c = card(m);
                m.sh(off(c, (slot << 1).wrapping_add(0x1220)), 0);
                m.sb(off(off(c, slot), 0x14c8), 0);
                let c = card(m);
                m.sb(off(off(c, slot), 0x14d0), 0);
                let c = card(m);
                m.sw(off(c, 0x14c4), 1000);
            } else {
                let changed = m.lw(off(c, 0x121c));
                m.sb(off(entry, 0x14cc), u8::from(changed != 0));
                next_slot(m);
            }
        }
        1 => {
            retry_present(m, slot, 1);
            let c = card(m);
            let changed = m.lw(off(c, 0x121c));
            m.sb(off(off(c, slot), 0x14cc), u8::from(changed == 1));
            next_slot(m);
        }
        2 => {
            retry_present(m, slot, 1);
            let c = card(m);
            let changed = m.lw(off(c, 0x121c));
            m.sb(off(off(c, slot), 0x14cc), u8::from(changed != 0));
            let c = card(m);
            let probed = m.lw(off(c, 0x14d4));
            m.sw(off(c, 0x14d4), 1u32.wrapping_sub(probed));
            m.sb(off(off(c, slot), 0x14ca), 0);
        }
        3 => {
            m.sh(off(status, 0x1220), 0);
            drain_file_events(m);
            bios(m, CARD_CLEAR, [slot << 4, 0, 0, 0]);
            set_step(m, slot, 4);
            let c = card(m);
            m.sb(off(off(c, slot), 0x14c8), 0);
            let c = card(m);
            m.sb(off(off(c, slot), 0x14d0), 0);
        }
        _ => {
            let c = card(m);
            m.sb(off(off(c, slot), 0x14c8), 1);
            let c = card(m);
            m.sb(off(off(c, slot), 0x14cc), 1);
            next_slot(m);
        }
    }
}

/// Step 7 of [`poll`] (`0x800CC1A4`).
fn evaluate_load(m: &mut Machine, slot: u32) {
    let c = card(m);
    let entry = off(c, slot);
    let event = lbu(m, off(entry, 0x14ce));
    m.sh(off(c, (slot << 1).wrapping_add(0x1224)), event as u16);
    m.sb(off(entry, 0x14c8), 1);
    let c = card(m);
    let status = lh(m, off(c, (slot << 1).wrapping_add(0x1224))) as i32;
    match status {
        0 => m.sb(off(off(c, slot), 0x14cc), 8),
        1 | 2 => {
            retry_present(m, slot, 1);
            set_step(m, slot, 1);
        }
        3 => {
            m.sb(off(off(c, slot), 0x14c8), 1);
            let c = card(m);
            m.sb(off(off(c, slot), 0x14cc), 1);
            next_slot(m);
        }
        _ => {}
    }
}

/// 800CC354: the status of `slot` as load-screen bits (0x20 unknown card
/// state, 6 unformatted, 5/1 absent, 4 ..., 0xC no save file, 0x1C save
/// file present, 0 busy), and the original's exit `a1` (the card block or
/// the status halfword last read), which the load screen's phase callback
/// receives.
pub fn slot_state(m: &mut Machine, slot: u32) -> (u32, u32) {
    let c = card(m);
    if lbu(m, off(off(c, slot), 0x14c8)) == 0 {
        let status = lh(m, off(c, (slot << 1).wrapping_add(0x1220)));
        return (if status != 2 { 5 } else { 1 }, c);
    }
    let a1 = lh(m, off(c, (slot << 1).wrapping_add(0x1220)));
    match a1 as i32 {
        1 => return (6, a1),
        2 => return (0, a1),
        0 | 3 => {}
        _ => return (0x20, a1),
    }
    let c = card(m);
    let a1 = lh(m, off(c, (slot << 1).wrapping_add(0x1224)));
    let value = match a1 as i32 {
        1 => 6,
        2 => 0,
        3 => 4,
        0 => match lbu(m, off(off(c, slot), 0x14ca)) {
            1 => 0x1c,
            0 => 0xc,
            _ => 6,
        },
        _ => 0x20,
    };
    (value, a1)
}

// ---- BIOS events ------------------------------------------------------------------

/// 800CC46C: open the seven events (card: class 0xF4000001, file: class
/// 0xF0000011; specs 4, 0x8000, 0x100 and 0x2000; mode 0x2000) inside a
/// critical section.
pub fn open_events(m: &mut Machine) {
    m.push_frame(0x18);
    bios(m, ENTER_CRITICAL_SECTION, [0; 4]);
    let events = [
        (0xf400_0001, 4, 0x11fc),
        (0xf400_0001, 0x8000, 0x1200),
        (0xf400_0001, 0x100, 0x1204),
        (0xf400_0001, 0x2000, 0x1208),
        (0xf000_0011, 4, 0x120c),
        (0xf000_0011, 0x8000, 0x1210),
        (0xf000_0011, 0x100, 0x1214),
    ];
    for (class, spec, offset) in events {
        let handle = bios(m, OPEN_EVENT, [class, spec, 0x2000, 0]);
        let c = card(m);
        m.sw(off(c, offset), handle);
    }
    bios(m, EXIT_CRITICAL_SECTION, [0; 4]);
    m.pop_frame(0x18);
}

/// Apply `routine` to the seven event handles inside a critical section.
fn each_event(m: &mut Machine, routine: u32) {
    m.push_frame(0x18);
    bios(m, ENTER_CRITICAL_SECTION, [0; 4]);
    for offset in [0x11fc, 0x1200, 0x1204, 0x1208, 0x120c, 0x1210, 0x1214] {
        let c = card(m);
        let handle = m.lw(off(c, offset));
        bios(m, routine, [handle, 0, 0, 0]);
    }
    bios(m, EXIT_CRITICAL_SECTION, [0; 4]);
    m.pop_frame(0x18);
}

/// 800CC574: EnableEvent on the seven events.
pub fn enable_events(m: &mut Machine) {
    each_event(m, ENABLE_EVENT);
}

/// 800CC62C (unreferenced): clear the changed flag (+0x121C).
pub fn clear_changed(m: &mut Machine) {
    let c = card(m);
    m.sw(off(c, 0x121c), 0);
}

/// 800CC63C: DisableEvent on the seven events.
pub fn disable_events(m: &mut Machine) {
    each_event(m, DISABLE_EVENT);
}

/// 800CC6F4: CloseEvent on the seven events.
pub fn close_events(m: &mut Machine) {
    each_event(m, CLOSE_EVENT);
}

// ---- save file image ------------------------------------------------------------------

/// 800CC7AC: icon data for the header: from a relocated table `table` (a
/// RAM pointer below 0x80200000: count, then image entries) the eight
/// words +0x14 of its first entry (+0x60) and 32 words +0x40 of each entry
/// (+0x80, 0x80 apart); from `palette` (same range) 32 words (+0x100).
pub fn load_icons(m: &mut Machine, table: u32, palette: u32) {
    m.push_frame(0x20);
    let c = card(m);
    let in_ram = |p: u32| (p as i32) < 0 && p <= 0x801f_ffff;
    if in_ram(table) {
        let count = m.lw(table);
        let entries = dispatch::call(m, crate::ov1_script::RELOCATE_DIRECTORY, [table, 0, 0, 0]);
        m.sb(off(c, 2), count.wrapping_add(0x10) as u8);
        let first = m.lw(entries);
        for k in 0..8 {
            let v = m.lw(off(first, 0x14 + 4 * k));
            m.sw(off(c, 0x60 + 4 * k), v);
        }
        for j in 0..count as i32 {
            let entry = m.lw(off(entries, 4 * j as u32));
            for k in 0..0x20 {
                let v = m.lw(off(entry, 0x40 + 4 * k));
                m.sw(off(c, 0x80 + 0x80 * j as u32 + 4 * k), v);
            }
        }
    }
    if in_ram(palette) {
        for k in 0..0x20 {
            let v = m.lw(off(palette, 4 * k));
            m.sw(off(c, 0x100 + 4 * k), v);
        }
    }
    m.pop_frame(0x20);
}

/// 800CC8C8: the save file header at `buffer`: "SC", icon flag 0x11 and one
/// block, the title (0x800C9DEC), cleared reserved words, "CRD0" at +0x52
/// (its terminator cleared), then the icon (800CC7AC with `table`,
/// `palette`).
pub fn init_header(m: &mut Machine, buffer: u32, table: u32, palette: u32) {
    m.push_frame(0x18);
    let magic = m.read_bytes(HEADER_MAGIC, 3);
    m.write_bytes(buffer, &magic);
    m.sb(off(buffer, 2), 0x11);
    m.sb(off(buffer, 3), 1);
    for k in 0..12 {
        let v = m.lw(off(HEADER_TITLE, 4 * k));
        m.sw(off(buffer, 4 + 4 * k), v);
    }
    let last = m.lbu(off(HEADER_TITLE, 0x30));
    m.sb(off(buffer, 0x34), last);
    for k in [0x4c, 0x48, 0x44] {
        m.sw(off(buffer, k), 0);
    }
    m.sh(off(buffer, 0x50), 1);
    let clut = m.read_bytes(HEADER_CLUT, 5);
    m.write_bytes(off(buffer, 0x52), &clut);
    m.sb(off(buffer, 0x56), 0);
    m.sb(off(buffer, 0x57), 0);
    m.sw(off(buffer, 0x58), 0);
    m.sw(off(buffer, 0x5c), 0);
    load_icons(m, table, palette);
    m.pop_frame(0x18);
}

/// 800CC9AC: the current game record at `record` from the context:
/// cleared mission tables, six 0x44-byte wingman entries (20 halfwords
/// from +0x172 and 0x1C bytes from +0x19C per 0x5C-byte context entry),
/// options (+0x418..+0x420) and the option flags from context +0x120,
/// +0x3A0 and +0x95E.
pub fn init_record(m: &mut Machine, record: u32) {
    m.sh(off(record, 0x10), 0);
    for i in 0..0x3c {
        let half = off(record, 2 * i);
        for offset in [0x184, 0x10c, 0x94, 0x1c] {
            m.sh(off(half, offset), 0);
        }
        let byte = off(record, i);
        for offset in [0x1fc, 0x21a, 0x238, 0x256] {
            m.sb(off(byte, offset), 0);
        }
    }
    m.sw(off(record, 0x278), 0);
    m.sw(off(record, 0x27c), 0);
    for k in 0..6u32 {
        let source = 0x5c * k;
        let entry = off(record, 0x280 + 0x44 * k);
        let context = context(m);
        for i in 0..0x14 {
            let v = lhu(m, off(context, source + 0x172 + 2 * i));
            m.sh(off(entry, 2 * i), v as u16);
        }
        let context = self::context(m);
        let bytes = m.read_bytes(off(context, source + 0x19c), 0x18);
        m.write_bytes(off(entry, 0x28), &bytes);
        let v = m.lw(off(context, source + 0x1b4));
        m.sw(off(entry, 0x40), v);
    }
    m.sb(off(record, 0x418), 0);
    m.sb(off(record, 0x419), 0xcc);
    m.sb(off(record, 0x41a), 0xcc);
    let context = context(m);
    m.sh(off(record, 0x41e), 0);
    m.sh(off(record, 0x41c), 0);
    m.sw(off(record, 0x420), 0);
    if m.lw(off(context, 0x120)) & 4 != 0 {
        m.sw(off(record, 0x420), 0x10);
    }
    let set = |m: &mut Machine, bit: u32| {
        let v = m.lw(off(record, 0x420));
        m.sw(off(record, 0x420), v | bit);
    };
    let context = self::context(m);
    if lbu(m, off(context, 0x3a0)) & 1 != 0 {
        set(m, 1);
    }
    let context = self::context(m);
    if m.lw(off(context, 0x120)) & 1 != 0 {
        set(m, 2);
    }
    let context = self::context(m);
    if m.lw(off(context, 0x120)) & 2 != 0 {
        set(m, 4);
    }
    let context = self::context(m);
    if lbu(m, off(context, 0x95e)) == 1 {
        set(m, 8);
    }
}

/// 800CCBC0: mark the save at `record` empty (+4 = 0).
pub fn clear_record(m: &mut Machine, record: u32) {
    m.sw(off(record, 4), 0);
}

/// 800CCBC8: retry counts 0, the two file names (0x800C9E28 aligned,
/// 0x800C9E40 to +0x14AA), slots closed with card and load status 2.
pub fn init_names(m: &mut Machine) {
    let c = card(m);
    for offset in [0x14e0, 0x14e2, 0x14e4] {
        m.sh(off(c, offset), 0);
    }
    for k in 0..6 {
        let v = m.lw(off(FILE_NAMES[0], 4 * k));
        m.sw(off(c, 0x1490 + 4 * k), v);
    }
    let c = card(m);
    let name = m.read_bytes(FILE_NAMES[1], 0x18);
    m.write_bytes(off(c, 0x14aa), &name);
    for slot in 0..2 {
        m.sw(off(c, 0x14d8 + 4 * slot), 0);
        m.sh(off(c, 0x1220 + 2 * slot), 2);
        m.sh(off(c, 0x1224 + 2 * slot), 2);
    }
}

/// 800CCCB8: initialize the card block: names, header (icon `table`,
/// `palette`), the current record from the context, six empty saves, the
/// events, the probe state.
pub fn initialize(m: &mut Machine, table: u32, palette: u32) {
    m.push_frame(0x28);
    init_names(m);
    let c = card(m);
    init_header(m, c, table, palette);
    let c = card(m);
    init_record(m, off(c, 0x180));
    for k in 0..6 {
        let c = card(m);
        clear_record(m, off(c, 0x5a4 + 0x12c * k));
    }
    open_events(m);
    reset_state(m);
    m.pop_frame(0x28);
}

/// 800CCD50: the last load-screen choice saved on the card (+0x190).
pub fn last_choice(m: &mut Machine) -> u32 {
    let c = card(m);
    lh(m, off(c, 0x190))
}

/// 800CCD68: before the language is chosen (+0x970 bit 0 clear), restore
/// it from the card (+0x18C).
pub fn restore_language(m: &mut Machine) {
    let context = context(m);
    if m.lw(off(context, 0x970)) & 1 == 0 {
        let c = card(m);
        let language = m.lw(off(c, 0x18c));
        m.sw(off(context, 0x38c), language);
    }
}

/// 800CD224: load mission save `slot` into the context: play time
/// (+0xF6C), mission (+0x390), difficulty (+0x398), 60 scores, ranks and
/// counters, aircraft (+0x392), wingman bytes (+0x3AD..+0x3AF), the 64
/// unlock bits (+0x9B8) and the 60 emblem nibbles (+0xD50).
pub fn apply_save(m: &mut Machine, slot: u32) {
    let c = card(m);
    let save = record(c, slot);
    let context = context(m);
    let base = off(context, 0x960);
    let time = m.lw(off(save, 0x124));
    m.sw(off(base, 0x60c), time);
    let mission = lbu(m, off(save, 0x129));
    m.sh(off(context, 0x390), mission as u16);
    let difficulty = lbu(m, off(save, 0x128));
    m.sw(off(context, 0x398), difficulty);
    for i in 0..0x3c {
        let score = lhu(m, off(save, 8 + 2 * i));
        m.sh(off(base, 0x288 + 2 * i), score as u16);
        let rank = lbu(m, off(save, 0x9e + i));
        m.sw(off(base, 0x42c + 4 * i), rank);
        let count = lbu(m, off(save, 0xda + i));
        m.sw(off(base, 0x51c + 4 * i), count);
    }
    let context = self::context(m);
    let aircraft = lbu(m, off(save, 0x11e));
    m.sh(off(context, 0x392), aircraft as u16);
    let v = lbu(m, off(save, 0x11f));
    m.sb(off(context, 0x3ad), v as u8);
    for (from, to) in [(0x120, 0x3ae), (0x121, 0x3af)] {
        let context = self::context(m);
        let v = lbu(m, off(save, from));
        m.sb(off(context, to), v as u8);
    }
    for byte in 0..8 {
        let bits = lbu(m, off(save, 0x116 + byte));
        for bit in 0..8 {
            m.sb(off(base, 0x58 + 8 * byte + bit), ((bits >> bit) & 1) as u8);
        }
    }
    for i in 0..0x1e {
        let v = lbu(m, off(save, 0x80 + i));
        m.sb(off(base, 0x3f0 + 2 * i), (v & 0xf) as u8);
        let v = lbu(m, off(save, 0x80 + i));
        m.sb(off(base, 0x3f1 + 2 * i), (v >> 4) as u8);
    }
}

/// 800CDE9C: check the current record (+0x180) and, failing that, the
/// backup (+0xCAC, copied over the current one when valid). Returns 1
/// (current valid), 2 (restored) or 0, and the original's exit `a1` (the
/// last checksum, or 0x41430000 after a valid current record).
pub fn check_record(m: &mut Machine) -> (u32, u32) {
    let c = card(m);
    let (value, expected) = xor_words(m, off(c, 0x180), 0x424);
    if value == expected {
        return (1, 0x4143_0000);
    }
    let c = card(m);
    let (value, expected) = xor_words(m, off(c, 0xcac), 0x424);
    if value != expected {
        return (0, value);
    }
    let c = card(m);
    let bytes = m.read_bytes(off(c, 0xcac), 0x424);
    m.write_bytes(off(c, 0x180), &bytes);
    (2, value)
}

/// 800CDFA4: check the six mission saves; a bad one is replaced by the
/// backup save (+0x10D0) when it is the one the backup belongs to (+0x190)
/// and the backup is valid, otherwise marked empty. Returns the exit `a1`
/// (the last checksum computed).
pub fn check_saves(m: &mut Machine) -> u32 {
    let mut a1 = 0;
    for slot in 0..6u32 {
        let c = card(m);
        let save = off(c, 0x5a4 + 0x12c * slot);
        let (value, expected) = xor_words(m, save, 0x12c);
        a1 = value;
        if value == expected {
            continue;
        }
        let c = card(m);
        let owner = lh(m, off(c, 0x190));
        if slot != owner {
            m.sw(off(off(c, 0x12c * slot), 0x5a8), 0);
            continue;
        }
        let (value, expected) = xor_words(m, off(c, 0x10d0), 0x12c);
        a1 = value;
        if value != expected {
            let c = card(m);
            m.sw(off(off(c, 0x12c * slot), 0x5a8), 0);
            continue;
        }
        let c = card(m);
        let bytes = m.read_bytes(off(c, 0x10d0), 0x12c);
        m.write_bytes(off(off(c, 0x12c * slot), 0x5a4), &bytes);
    }
    a1
}

/// 800CE0FC: read the save file of `slot` (`mode` 1: reset the probe
/// after a good read): open (three retries, then -1 with the probe
/// reset), read 0x11FC bytes into the card block, wait for a card event
/// (the read's completion, `SW_CARD`), close, then check the records (0: 2), the saves, the game ID (+0x188
/// against context +0x96C: 3). Returns 0 (busy), 1, 2, 3 or -1, and the
/// original's exit `a1` (the load screen stores its low byte as a
/// visibility flag after -1 and 2).
pub fn read_save(m: &mut Machine, slot: u32, mode: u32) -> (u32, u32) {
    m.push_frame(0x28);
    let result = read_save_body(m, slot, mode);
    m.pop_frame(0x28);
    result
}

fn read_save_body(m: &mut Machine, slot: u32, mode: u32) -> (u32, u32) {
    if card_ready(m) == 0 {
        return (0, mode);
    }
    let c = card(m);
    let flags = off(c, (slot << 2).wrapping_add(0x14d8));
    if m.lw(flags) & 1 == 0 {
        let handle = bios(m, OPEN, [file_name(c, slot), 0x8001, 0, 0]);
        let c = card(m);
        m.sw(off(c, 0x122c), handle);
        if handle == 0xffff_ffff {
            let retries = lh(m, off(c, 0x14e0));
            let count = lhu(m, off(c, 0x14e0));
            if (retries as i32) < 3 {
                m.sh(off(c, 0x14e0), count.wrapping_add(1) as u16);
                return (0, c);
            }
            reset_state(m);
            let c = card(m);
            let flags = off(c, (slot << 2).wrapping_add(0x14d8));
            let v = m.lw(flags);
            m.sw(flags, v & 0xffff_fffe);
            return (0xffff_ffff, 0xffff_fffe);
        }
        bios(m, READ, [handle, c, 0x11fc, 0]);
        let c = card(m);
        let flags = off(c, (slot << 2).wrapping_add(0x14d8));
        let v = m.lw(flags);
        m.sw(flags, v | 1);
    }
    let event = test_card_events(m, slot, 0);
    if event == 0xffff_ffff {
        return (0, 0);
    }
    let c = card(m);
    let handle = m.lw(off(c, 0x122c));
    bios(m, CLOSE, [handle, 0, 0, 0]);
    let c = card(m);
    let flags = off(c, (slot << 2).wrapping_add(0x14d8));
    let v = m.lw(flags);
    m.sw(flags, v & 0xffff_fffe);
    let (valid, a1) = check_record(m);
    if valid == 0 {
        return (2, a1);
    }
    let a1 = check_saves(m);
    let c = card(m);
    let context = context(m);
    let id = m.lw(off(c, 0x188));
    if id != m.lw(off(context, 0x96c)) {
        return (3, a1);
    }
    if event != 0 {
        let retries = lh(m, off(c, 0x14e0));
        let count = lhu(m, off(c, 0x14e0));
        if (retries as i32) < 3 {
            m.sh(off(c, 0x14e0), count.wrapping_add(1) as u16);
            return (0, a1);
        }
        let flags = off(c, (slot << 2).wrapping_add(0x14d8));
        let v = m.lw(flags);
        m.sw(flags, v & 0xffff_fffe);
        reset_state(m);
        return (0xffff_ffff, 1);
    }
    let flags = off(c, (slot << 2).wrapping_add(0x14d8));
    let v = m.lw(flags);
    m.sw(flags, v & 0xffff_fffe);
    if mode != 1 {
        return (1, a1);
    }
    reset_state(m);
    (1, 1)
}

/// 800CE300: mission save `slot` when it is below 6 and marked used (+4
/// bit 0), else 0.
pub fn save_record(m: &mut Machine, slot: u32) -> u32 {
    let c = card(m);
    let save = record(c, slot);
    if (slot as i32) >= 6 {
        return 0;
    }
    if m.lw(off(save, 4)) & 1 != 0 { save } else { 0 }
}

/// 800CCDA4: apply the current game record (+0x180) to the context:
/// unlock flags (+0x974), +0x961, game ID (+0x96C), +0xF78, the bit tables
/// (+0x978, +0x9F8), the four 60-entry mission tables, the emblem nibbles
/// (+0xC60..), +0xF70/+0xF74, the six wingman entries, the options (pad
/// bits of +0x120, the screen offset in the display block +8/+0xC, sound
/// settings +0x39C..+0x39E with the CD mix (8002B900 / 8002B934), +0x3A0,
/// +0x95E) and display flag 2.
pub fn apply_options(m: &mut Machine) {
    m.push_frame(0x18);
    let c = card(m);
    let record = off(c, 0x180);
    let context = context(m);
    let base = off(context, 0x960);
    let v = m.lw(off(record, 4));
    m.sw(off(base, 0x14), v);
    let v = lbu(m, off(record, 0x10));
    m.sb(off(base, 1), v as u8);
    let v = m.lw(off(record, 8));
    m.sw(off(base, 0xc), v);
    let v = m.lw(off(record, 0x274));
    m.sw(off(base, 0x618), v);
    for byte in 0..8 {
        for bit in 0..8 {
            let bits = lbu(m, off(record, 0x14 + byte));
            m.sb(off(base, 0x18 + 8 * byte + bit), ((bits >> bit) & 1) as u8);
        }
    }
    for byte in 0..2 {
        for bit in 0..8 {
            let bits = lbu(m, off(record, 0x12 + byte));
            m.sb(off(base, 0x98 + 8 * byte + bit), ((bits >> bit) & 1) as u8);
        }
    }
    for i in 0..0x3c {
        for (from, to) in [(0x1c, 0xa8), (0x94, 0x120), (0x10c, 0x198), (0x184, 0x210)] {
            let v = lhu(m, off(record, from + 2 * i));
            m.sh(off(base, to + 2 * i), v as u16);
        }
    }
    for i in 0..0x1e {
        for nibble in 0..2 {
            let target = off(base, 2 * i + nibble);
            for (from, to) in [
                (0x1fc, 0x300),
                (0x21a, 0x33c),
                (0x238, 0x378),
                (0x256, 0x3b4),
            ] {
                let v = lbu(m, off(record, from + i));
                m.sb(off(target, to), ((v >> (nibble << 2)) & 0xf) as u8);
            }
        }
    }
    let v = m.lw(off(record, 0x278));
    m.sw(off(base, 0x610), v);
    let v = m.lw(off(record, 0x27c));
    m.sw(off(base, 0x614), v);
    for k in 0..6u32 {
        let entry = off(record, 0x280 + 0x44 * k);
        let target = 0x5c * k;
        let context = self::context(m);
        for i in 0..0x14 {
            let v = lhu(m, off(entry, 2 * i));
            m.sh(off(context, target + 0x172 + 2 * i), v as u16);
        }
        let context = self::context(m);
        let bytes = m.read_bytes(off(entry, 0x28), 0x18);
        m.write_bytes(off(context, target + 0x19c), &bytes);
        let v = m.lw(off(entry, 0x40));
        m.sw(off(context, target + 0x1b4), v);
    }
    let options = m.lw(off(record, 0x420));
    let context = self::context(m);
    let pad = m.lw(off(context, 0x120));
    m.sw(
        off(context, 0x120),
        if options & 0x10 != 0 {
            pad | 4
        } else {
            pad & !4
        },
    );
    let display = m.gw(crate::ov1::DISPLAY);
    let x = lh(m, off(record, 0x41c));
    m.sw(off(display, 8), x);
    let y = lh(m, off(record, 0x41e));
    m.sw(off(display, 0xc), y);
    for (from, to) in [(0x419, 0x39c), (0x41a, 0x39d), (0x418, 0x39e)] {
        let context = self::context(m);
        let v = lbu(m, off(record, from));
        m.sb(off(context, to), v as u8);
    }
    let context = self::context(m);
    let stereo = u32::from(lbu(m, off(context, 0x39e)) & 1 != 0);
    dispatch::call(m, SET_CD_MIX, [stereo, 0, 0, 0]);
    dispatch::call(m, APPLY_CD_MIX, [0; 4]);
    let options = m.lw(off(record, 0x420));
    let context = self::context(m);
    let flags = lbu(m, off(context, 0x3a0));
    let flags = if options & 1 != 0 {
        flags | 1
    } else {
        flags & 0xfe
    };
    m.sb(off(context, 0x3a0), flags as u8);
    for (bit, pad_bit) in [(2, 1), (4, 2)] {
        let options = m.lw(off(record, 0x420));
        let context = self::context(m);
        let pad = m.lw(off(context, 0x120));
        let pad = if options & bit != 0 {
            pad | pad_bit
        } else {
            pad & !pad_bit
        };
        m.sw(off(context, 0x120), pad);
    }
    let options = m.lw(off(record, 0x420));
    let context = self::context(m);
    m.sb(off(context, 0x95e), u8::from(options & 8 != 0));
    let display = m.gw(crate::ov1::DISPLAY);
    let flags = m.lw(display);
    m.sw(display, flags | 2);
    m.pop_frame(0x18);
}

/// 8002B900 / 8002B934 (resident CD queue, not translated yet): store the
/// CD mix mode (`[gp+0x48]` +0x160) and apply it (800295B8).
pub const SET_CD_MIX: u32 = 0x8002_b900;
pub const APPLY_CD_MIX: u32 = 0x8002_b934;

/// 800ABC38 `_card_format` (BIOS, `sdk.rs`).
pub const CARD_FORMAT: u32 = 0x800a_bc38;

/// 800CD918 (unreferenced): with both slots idle, `_card_format(slot << 4)`
/// and reset the probe; 1 when formatted, -1 when not, 0 when busy.
pub fn format_card(m: &mut Machine, slot: u32) -> u32 {
    m.push_frame(0x18);
    let result = if card_ready(m) == 0 {
        0
    } else {
        let formatted = bios(m, CARD_FORMAT, [slot << 4, 0, 0, 0]);
        reset_state(m);
        if formatted == 1 { 1 } else { 0xffff_ffff }
    };
    m.pop_frame(0x18);
    result
}
