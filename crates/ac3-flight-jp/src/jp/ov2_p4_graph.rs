//! JP overlay 2: the mission replay graph (800F4C04..800F5833), the flight
//! path and event markers of the debriefing's replay, translated from the
//! JP instructions (class `new`, except 800F5018 `changed`: PAL 800D1BA4 is
//! a different routine now). No capture reaches them (the debriefing needs
//! a won mission).
//!
//! Block `[0x80112E04]` (the replay state): +1 the next event, +2 the clock
//! (4 ticks per path point); +0x10 + 4 n the event markers (+0 x, +1 y,
//! +2 colour kind, +3 shown); the kill list at +0x110 (names) / +0x138 (+0
//! kind, +1 count) and the second list at +0x270 / +0x298, 0x2C bytes per
//! entry, their counts at +0x3D0 / +0x3D1. Block `[0x80112E08]` (the
//! recorded mission): +8 the path length, +0xA the event count, +0xC the
//! path points (x, y bytes), +0x390 + 8 n the events (+0 kind, +1 colour,
//! +2 time, +4 x, +5 y).
use super::ov2_p4::{At, call, lh, lt, sra};
use super::ov2_p4_text as text;
use crate::common::loaded_module;
use crate::machine::Machine;

/// `[0x80112E04]` / `[0x80112E08]`: the replay state and the record.
pub const STATE: u32 = 0x8011_2e04;
pub const RECORD: u32 = 0x8011_2e08;
/// `[0x80112DC4]`: the hub block (+8 its ordering-table entry).
const HUB: u32 = 0x8011_2dc4;
/// libgpu SetDrawMode; libc strcpy; the sound request.
const SET_DRAW_MODE: u32 = 0x800a_c7f8;
const STRCPY: u32 = 0x800a_dea8;
const PLAY_SOUND: u32 = 0x8002_db4c;
/// Overlay 2 (other parts): 800F6CBC a flat quad, 800F6DA4 a line.
const QUAD: u32 = 0x800f_6cbc;
const LINE: u32 = 0x800f_6da4;
/// The names of kill kinds (below 0x30) and of kinds 0x70..0x74.
const KILL_NAMES: u32 = 0x8010_e560;
const OTHER_NAMES: u32 = 0x8010_e620;
/// "%2d\n".
const COUNT_FORMAT: u32 = 0x800c_d14c;

/// The routines of this file by JP address, while module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != 2 {
        return None;
    }
    let [a0, ..] = args;
    Some(match target {
        0x800f_4c04 => cursor_f4c04(m, a0),
        0x800f_4df0 => trail_f4df0(m, a0),
        0x800f_5018 => events_f5018(m),
        0x800f_5388 => markers_f5388(m, a0),
        0x800f_56d8 => lists_f56d8(m),
        _ => return None,
    })
}

/// The path point of the clock (`[RECORD] + 0xC + 2 * (clock / 4)`).
fn point(m: &mut Machine) -> u32 {
    let state = m.lw(STATE);
    let clock = lh(m, state.at(2));
    let record = m.lw(RECORD);
    let clock = if lt(clock, 0) {
        clock.wrapping_add(3)
    } else {
        clock
    };
    record.wrapping_add(sra(clock, 2) << 1)
}

/// AddPrim into the hub's table entry (`[[HUB] + 8]`, reloaded as the
/// original does).
fn add_prim(m: &mut Machine, hub: u32, packet: u32) {
    let table = m.lw(hub.at(8));
    let tag = m.lw(packet);
    let head = m.lw(table);
    m.sw(packet, (tag & 0xff00_0000) | (head & 0x00ff_ffff));
    let table = m.lw(hub.at(8));
    let head = m.lw(table);
    m.sw(table, (head & 0xff00_0000) | (packet & 0x00ff_ffff));
}

/// 800F4C04: the aircraft cursor at the clock's path point: two POLY_FT4
/// (tpage 0x1F, clut 0x603C, grey) of 256x128 split at y, linked into the
/// hub's table from `packets`. Returns the next free packet.
pub fn cursor_f4c04(m: &mut Machine, packets: u32) -> u32 {
    let at = point(m);
    let x = u32::from(m.lbu(at.at(0xc)));
    let first = packets;
    let second = packets.at(0x28);
    let left = 0x54u32.wrapping_sub(x);
    let right = 0x154u32.wrapping_sub(x);
    let y = u32::from(m.lbu(at.at(0xd)));
    for (offset, value) in [
        (3u32, 9u8),
        (7, 0x2c),
        (0xc, 0),
        (0xd, 0),
        (0x14, 0x80),
        (0x15, 0),
        (0x1c, 0),
        (0x1d, 0x40),
        (0x24, 0x80),
        (0x25, 0x40),
        (4, 0x80),
        (5, 0x80),
        (6, 0x80),
    ] {
        m.sb(first.at(offset), value);
    }
    let hub = m.lw(HUB);
    m.sh(first.at(0x16), 0x1f);
    m.sh(first.at(0xe), 0x603c);
    m.sh(first.at(8), left as u16);
    m.sh(first.at(0x10), right as u16);
    m.sh(first.at(0x18), left as u16);
    m.sh(first.at(0x20), right as u16);
    let rise = 0xffu32.wrapping_sub(y) << 1;
    let top = 0xbeu32.wrapping_sub(rise);
    let middle = 0x1beu32.wrapping_sub(rise);
    m.sh(first.at(0xa), top as u16);
    m.sh(first.at(0x12), top as u16);
    m.sh(first.at(0x1a), middle as u16);
    m.sh(first.at(0x22), middle as u16);
    add_prim(m, hub, first);
    for (offset, value) in [
        (3u32, 9u8),
        (7, 0x2c),
        (0xc, 0),
        (0xd, 0x40),
        (0x14, 0x80),
        (0x15, 0x40),
        (0x1c, 0),
        (0x1d, 0x80),
        (0x24, 0x80),
        (0x25, 0x80),
        (4, 0x80),
        (5, 0x80),
        (6, 0x80),
    ] {
        m.sb(second.at(offset), value);
    }
    let hub = m.lw(HUB);
    let bottom = 0x2beu32.wrapping_sub(rise);
    m.sh(second.at(0x16), 0x1f);
    m.sh(second.at(0xe), 0x603c);
    m.sh(second.at(8), left as u16);
    m.sh(second.at(0xa), middle as u16);
    m.sh(second.at(0x10), right as u16);
    m.sh(second.at(0x12), middle as u16);
    m.sh(second.at(0x18), left as u16);
    m.sh(second.at(0x1a), bottom as u16);
    m.sh(second.at(0x20), right as u16);
    m.sh(second.at(0x22), bottom as u16);
    add_prim(m, hub, second);
    second.at(0x28)
}

/// 800F4DF0: the flight path's trail: up to 40 semi-transparent LINE_G2
/// (0x14 bytes) from the clock's point backwards, fading from 0x80 by 2
/// per segment, relative to the clock's point at (0x53, 0xC0); then a
/// DR_MODE (tpage 0x20). Returns the next free packet.
pub fn trail_f4df0(m: &mut Machine, packets: u32) -> u32 {
    let sp = m.push_frame(0x30);
    let at = point(m);
    let x0 = u32::from(m.lbu(at.at(0xc)));
    let y0 = u32::from(m.lbu(at.at(0xd)));
    let mut packet = packets;
    let mut line = packets.wrapping_sub(0x14);
    let (mut fade, mut colour) = (2u32, 0x80u32);
    for k in 0..40u32 {
        let state = m.lw(STATE);
        let clock = lh(m, state.at(2));
        let clock = if lt(clock, 0) {
            clock.wrapping_add(3)
        } else {
            clock
        };
        let index = sra(clock, 2).wrapping_sub(k);
        if lt(0, index) {
            let record = m.lw(RECORD);
            let count = lh(m, record.at(8));
            if lt(index, count) {
                let p = record.wrapping_add(index << 1);
                let x1 = u32::from(m.lbu(p.at(0xc)))
                    .wrapping_sub(x0)
                    .wrapping_add(0x53);
                let y1 = (y0.wrapping_sub(u32::from(m.lbu(p.at(0xd)))) << 1).wrapping_add(0xc0);
                let q = record.wrapping_add(index.wrapping_sub(1) << 1);
                let x2 = u32::from(m.lbu(q.at(0xc)))
                    .wrapping_sub(x0)
                    .wrapping_add(0x53);
                line = line.at(0x14);
                let tail = 0x80u32.wrapping_sub(fade);
                let y2 = u32::from(m.lbu(q.at(0xd)));
                m.sb(line.at(3), 4);
                m.sb(line.at(7), 0x52);
                for offset in [4u32, 5, 6] {
                    m.sb(line.at(offset), colour as u8);
                }
                for offset in [0xcu32, 0xd, 0xe] {
                    m.sb(line.at(offset), tail as u8);
                }
                let hub = m.lw(HUB);
                m.sh(line.at(8), x1 as u16);
                m.sh(line.at(0xa), y1 as u16);
                m.sh(line.at(0x10), x2 as u16);
                let y2 = (y0.wrapping_sub(y2) << 1).wrapping_add(0xc0);
                m.sh(line.at(0x12), y2 as u16);
                add_prim(m, hub, packet);
                packet = packet.at(0x14);
            }
        }
        fade = fade.wrapping_add(2);
        colour = colour.wrapping_sub(2);
    }
    let mode = packet;
    m.sw(sp.at(0x10), 0);
    call(m, SET_DRAW_MODE, [mode, 0, 0, 0x20]);
    let hub = m.lw(HUB);
    add_prim(m, hub, mode);
    m.pop_frame(0x30);
    mode.at(0xc)
}

/// 800F5018 (JP; PAL 800D1BA4, changed): show every recorded event whose
/// time has come: its marker (+0x10 + 4 n), and for kill kinds (below 0x30)
/// or kinds 0x70..0x74 the count of its entry in the kill / second list
/// (a new entry takes the kind's name, sound 0x2E / 0x2F). Returns 1, or 0
/// when every event was shown.
pub fn events_f5018(m: &mut Machine) -> u32 {
    m.push_frame(0x28);
    let v = events(m);
    m.pop_frame(0x28);
    v
}

/// The event `n` of the record: (record, marker, class 1 kill / 2 other /
/// 0).
fn event(m: &mut Machine, state: u32, record: u32) -> (u32, u32, u32) {
    let n = u32::from(m.lbu(state.at(1)));
    let entry = record.wrapping_add((n << 3).wrapping_add(0x390));
    let kind = u32::from(m.lbu(entry));
    let marker = state.wrapping_add((n << 2).wrapping_add(0x10));
    let class = if kind < 0x30 {
        1
    } else {
        u32::from(kind.wrapping_sub(0x70) < 5) << 1
    };
    (entry, marker, class)
}

fn events(m: &mut Machine) -> u32 {
    let state = m.lw(STATE);
    let record = m.lw(RECORD);
    if m.lbu(state.at(1)) >= m.lbu(record.at(0xa)) {
        return 0;
    }
    let (mut entry, mut marker, mut class) = event(m, state, record);
    let state = m.lw(STATE);
    if lt(lh(m, state.at(2)), lh(m, entry.at(2))) {
        return 1;
    }
    loop {
        let state = m.lw(STATE);
        let record = m.lw(RECORD);
        if m.lbu(state.at(1)) >= m.lbu(record.at(0xa)) {
            return 1;
        }
        m.sb(marker.at(3), 1);
        let x = m.lbu(entry.at(4));
        m.sb(marker, x);
        let y = m.lbu(entry.at(5));
        m.sb(marker.at(1), y);
        let colour = m.lbu(entry.at(1));
        m.sb(marker.at(2), colour);
        let state = m.lw(STATE);
        let n = m.lbu(state.at(1)).wrapping_add(1);
        m.sb(state.at(1), n);
        match class {
            1 => {
                count_entry(m, entry, 0x3d0, 0x138, 0x110, KILL_NAMES, 0);
                call(m, PLAY_SOUND, [0x2e, 0, 0, 0]);
            }
            2 => {
                count_entry(m, entry, 0x3d1, 0x298, 0x270, OTHER_NAMES, 0x70);
                call(m, PLAY_SOUND, [0x2f, 0, 0, 0]);
            }
            _ => {}
        }
        let state = m.lw(STATE);
        let record = m.lw(RECORD);
        if m.lbu(state.at(1)) == m.lbu(record.at(0xa)) {
            return 1;
        }
        (entry, marker, class) = event(m, state, record);
        let state = m.lw(STATE);
        if lt(lh(m, state.at(2)), lh(m, entry.at(2))) {
            return 1;
        }
    }
}

/// Count event `entry`'s kind in the list at `list` (counter at `count`):
/// an existing entry's count +1, else a new entry (+0 kind, +1 count 1, the
/// name from `names[kind - base]` at `name` + 0x2C n).
fn count_entry(
    m: &mut Machine,
    entry: u32,
    count: u32,
    list: u32,
    name: u32,
    names: u32,
    base: u32,
) {
    let state = m.lw(STATE);
    let n = u32::from(m.lbu(state.at(count)));
    let (mut i, mut found) = (0u32, false);
    if n != 0 {
        let kind = m.lbu(entry);
        let mut at = state;
        loop {
            if m.lbu(at.at(list)) == kind {
                let c = m.lbu(at.at(list + 1)).wrapping_add(1);
                found = true;
                m.sb(at.at(list + 1), c);
                break;
            }
            i = i.wrapping_add(1);
            at = at.at(0x2c);
            if !lt(i, n) {
                break;
            }
        }
    }
    if found {
        return;
    }
    let offset = i.wrapping_mul(0x2c);
    let state = m.lw(STATE);
    let kind = m.lbu(entry);
    m.sb(state.wrapping_add(offset).at(list), kind);
    let kind = u32::from(m.lbu(entry));
    let text = m.lw(names.wrapping_add(kind.wrapping_sub(base) << 2));
    let state = m.lw(STATE);
    call(
        m,
        STRCPY,
        [state.wrapping_add(offset.wrapping_add(name)), text, 0, 0],
    );
    let state = m.lw(STATE);
    m.sb(state.wrapping_add(offset).at(list + 1), 1);
    let state = m.lw(STATE);
    let c = m.lbu(state.at(count)).wrapping_add(1);
    m.sb(state.at(count), c);
}

/// 800F5388: the shown event markers (up to 64) relative to the clock's
/// point: each a flat box (800F6CBC) and a coloured 3D-edged frame of six
/// lines (800F6DA4; colour by kind: 0 white, 1 blue, 2 black, 3 yellow, 4
/// red), all into the hub's table from `packets`. The original keeps the
/// two parameter blocks (frame + 0x10, + 0x20) and its spilled locals in
/// its frame. Returns the next free packet.
pub fn markers_f5388(m: &mut Machine, packets: u32) -> u32 {
    let sp = m.push_frame(0x68);
    let at = point(m);
    let x = m.lbu(at.at(0xc));
    m.sh(sp.at(0x30), u16::from(x));
    m.sw(sp.at(0x28), 0);
    m.sw(sp.at(0x3c), sp.at(0x20));
    let y = u32::from(m.lbu(at.at(0xd)));
    m.sw(sp.at(0x38), 0xffu32.wrapping_sub(0xffu32.wrapping_sub(y)));
    let mut packet = packets;
    loop {
        let i = m.lw(sp.at(0x28));
        let state = m.lw(STATE);
        let marker = state.wrapping_add((i << 2).wrapping_add(0x10));
        if m.lbu(marker.at(3)) == 0 {
            break;
        }
        let quad = m.lw(sp.at(0x3c));
        let mx = u32::from(m.lbu(marker));
        let my = u32::from(m.lbu(marker.at(1)));
        let hub = m.lw(HUB);
        let x0 = u32::from(m.lhu(sp.at(0x30)));
        for offset in [0x20u32, 0x21, 0x22, 0x23] {
            m.sb(sp.at(offset), 0);
        }
        m.sh(sp.at(0x24), 0);
        m.sh(sp.at(0x26), 0);
        let dx = mx.wrapping_sub(x0);
        let base = m.lw(sp.at(0x38));
        let table = m.lw(hub.at(8));
        let dy = base.wrapping_sub(my) << 1;
        packet = call(m, QUAD, [quad, packet, table, 0]);
        let kind = m.lbu(marker.at(2));
        let rgb = match kind {
            0 => Some([0xff, 0xff, 0xff]),
            _ => None,
        };
        if let Some([r, g, b]) = rgb {
            m.sb(sp.at(0x18), r);
            m.sb(sp.at(0x19), g);
            m.sb(sp.at(0x1a), b);
        }
        if m.lbu(marker.at(2)) == 1 {
            m.sb(sp.at(0x18), 0);
            m.sb(sp.at(0x19), 0);
            m.sb(sp.at(0x1a), 0xff);
        }
        let mut kind = m.lbu(marker.at(2));
        if kind == 2 {
            m.sb(sp.at(0x18), 0);
            m.sb(sp.at(0x19), 0);
            m.sb(sp.at(0x1a), 0);
            kind = m.lbu(marker.at(2));
        }
        if kind == 3 {
            m.sb(sp.at(0x18), 0xff);
            m.sb(sp.at(0x19), 0xff);
            m.sb(sp.at(0x1a), 0);
        }
        if m.lbu(marker.at(2)) == 4 {
            m.sb(sp.at(0x18), 0xff);
            m.sb(sp.at(0x19), 0);
            m.sb(sp.at(0x1a), 0);
        }
        let params = sp.at(0x10);
        let (x50, y_top, x58, y_bottom) = (
            dx.wrapping_add(0x50),
            dy.wrapping_add(0xb8),
            dx.wrapping_add(0x58),
            dy.wrapping_add(0xc8),
        );
        let hub = m.lw(HUB);
        m.sb(sp.at(0x1b), 1);
        let i = m.lw(sp.at(0x28));
        m.sh(params, x50 as u16);
        m.sh(params.at(2), y_top as u16);
        m.sh(params.at(4), x58 as u16);
        m.sh(params.at(6), y_bottom as u16);
        let table = m.lw(hub.at(8));
        m.sw(sp.at(0x28), i.wrapping_add(1));
        packet = call(m, LINE, [params, packet, table, 0]);
        let (x4e, x56) = (dx.wrapping_add(0x4e), dx.wrapping_add(0x56));
        let hub = m.lw(HUB);
        m.sh(params, x4e as u16);
        m.sh(params.at(4), x56 as u16);
        let table = m.lw(hub.at(8));
        packet = call(m, LINE, [params, packet, table, 0]);
        let (x4f, x57) = (dx.wrapping_add(0x4f), dx.wrapping_add(0x57));
        let r = m.lbu(sp.at(0x18));
        let g = m.lbu(sp.at(0x19));
        m.sh(params, x4f as u16);
        m.sh(params.at(4), x57 as u16);
        m.sb(sp.at(0x18), r << 1);
        let b = m.lbu(sp.at(0x1a));
        m.sb(sp.at(0x19), g << 1);
        let hub = m.lw(HUB);
        m.sb(sp.at(0x1a), b << 1);
        let table = m.lw(hub.at(8));
        packet = call(m, LINE, [params, packet, table, 0]);
        let r = m.lbu(sp.at(0x18));
        let g = m.lbu(sp.at(0x19));
        m.sb(sp.at(0x1b), 1);
        m.sh(params, x50 as u16);
        m.sh(params.at(2), y_bottom as u16);
        m.sh(params.at(4), x58 as u16);
        m.sh(params.at(6), y_top as u16);
        m.sb(sp.at(0x18), r >> 1);
        let b = m.lbu(sp.at(0x1a));
        m.sb(sp.at(0x19), g >> 1);
        let hub = m.lw(HUB);
        m.sb(sp.at(0x1a), b >> 1);
        let table = m.lw(hub.at(8));
        packet = call(m, LINE, [params, packet, table, 0]);
        let hub = m.lw(HUB);
        m.sh(params, x4e as u16);
        m.sh(params.at(4), x56 as u16);
        let table = m.lw(hub.at(8));
        packet = call(m, LINE, [params, packet, table, 0]);
        let r = m.lbu(sp.at(0x18));
        let g = m.lbu(sp.at(0x19));
        m.sb(sp.at(0x1b), 1);
        m.sh(params, x4f as u16);
        m.sh(params.at(4), x57 as u16);
        m.sb(sp.at(0x18), r << 1);
        let b = m.lbu(sp.at(0x1a));
        m.sb(sp.at(0x19), g << 1);
        let hub = m.lw(HUB);
        m.sb(sp.at(0x1a), b << 1);
        let table = m.lw(hub.at(8));
        packet = call(m, LINE, [params, packet, table, 0]);
        let hub = m.lw(HUB);
        let quad = m.lw(sp.at(0x3c));
        m.sb(sp.at(0x23), 0);
        let table = m.lw(hub.at(8));
        packet = call(m, QUAD, [quad, packet, table, 0]);
        let i = m.lw(sp.at(0x28));
        if !lt(i, 0x40) {
            break;
        }
    }
    m.pop_frame(0x68);
    packet
}

/// 800F56D8: the two lists: up to 8 kill entries (name at (0xB2, 0x36 + 0x16
/// n), count "%2d" with zeros at (0x10E, 0x3B + 0x16 n)) and up to 8
/// second-list entries from y 0xD8 / 0xDF. Returns 0.
pub fn lists_f56d8(m: &mut Machine) -> u32 {
    m.push_frame(0x30);
    for (count, name, y_name, y_count) in [
        (0x139u32, 0x110u32, 0x36u32, 0x3bu32),
        (0x299, 0x270, 0xd8, 0xdf),
    ] {
        let (mut y0, mut y1) = (y_name, y_count);
        for n in 0..8u32 {
            let state = m.lw(STATE);
            let offset = n * 0x2c;
            if m.lbu(state.wrapping_add(offset).at(count)) == 0 {
                break;
            }
            text::set_pen_f33a8(m, 0xb2, y0);
            let state = m.lw(STATE);
            // the leftover a1 of 800F33A8 (y << 16) is the unused second
            // argument (stored in the caller's frame)
            text::printf_f4ae0(
                m,
                [
                    state.wrapping_add(offset.wrapping_add(name)),
                    y0 << 16,
                    0,
                    0,
                ],
            );
            y0 = y0.wrapping_add(0x16);
            text::set_pen_f33a8(m, 0x10e, y1);
            y1 = y1.wrapping_add(0x16);
            let state = m.lw(STATE);
            let c = u32::from(m.lbu(state.wrapping_add(offset).at(count)));
            text::printf_zeros_f4b40(m, [COUNT_FORMAT, c, 0, 0]);
        }
    }
    m.pop_frame(0x30);
    0
}
