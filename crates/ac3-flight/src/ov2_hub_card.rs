//! The memory-card layer of the save screen (inner state 9): the per-frame
//! card monitor (800CBFAC) with its event polls, card status (800CC554),
//! the save image (header, progress data, checksums), directory listing,
//! save/load/format, and the BIOS / libcard entry points it uses as host
//! calls ([`bios`]).
//!
//! Card block `[0x800F1E44]` (0x800F69D8, overlay data): +0 the save file
//! image written and read as one 0x11FC-byte file (+0 header, +0x60 icon
//! CLUT, +0x80 icon, +0x180 progress data with its checksum, +0x5A4 six
//! 0x12C-byte mission records), +0xCAC a copy of the progress data, +0x10D0
//! a copy of the current record, +0x11FC..+0x1208 the four software card
//! events (I/O done, error, timeout, new card), +0x120C..+0x1214 the three
//! hardware card events, +0x121C monitor enable, +0x1220/+0x1224 per-port
//! card_info / card_load results (halfwords), +0x122C open file, +0x1230 /
//! +0x1234 per-port free blocks and file count, +0x1238 per-port 15
//! directory names (0x14 bytes), +0x1490 per-port save file names (26
//! bytes), +0x14C4 timeout counter, +0x14C8.. per-port bytes (+0x14C8 card
//! ready, +0x14CA save file present, +0x14CC monitor state, +0x14CE event
//! result, +0x14D0 retry count), +0x14D4 polled port, +0x14D8 per-port
//! transfer flags, +0x14E0 open retries.
use super::{CONTEXT, call};
use crate::{
    bios,
    machine::{Machine, SmallData},
};

/// `[0x800F1E44]`: the card block.
pub const CARD: u32 = 0x800f_1e44;
/// TestEvent (resident, owned by `sdk`).
pub const TEST_EVENT: u32 = 0x800a_d788;
/// libc routines (resident, owned by `sdk`): sprintf, strcpy, memset.
pub const SPRINTF: u32 = 0x800a_cd28;
pub const STRCPY: u32 = 0x800a_c2d8;
/// 800CC9AC: copy the icon CLUT and bitmaps into the header (`ov2core`).
pub const ICON_COPY: u32 = 0x800c_c9ac;
/// 800C9DE8: the header template ("SC", SJIS title); 800C9E20 "CRD0";
/// 800C9E28 / 800C9E40 the two save file names; 800C9E58 "bu%1d0:*.*".
const HEADER_MAGIC: u32 = 0x800c_9de8;
const HEADER_TITLE: u32 = 0x800c_9dec;
const HEADER_CRD0: u32 = 0x800c_9e20;
const NAME_PORT_0: u32 = 0x800c_9e28;
const NAME_PORT_1: u32 = 0x800c_9e40;
const DIRECTORY_PATTERN: u32 = 0x800c_9e58;
/// Checksum seed "3ECA" (0x41434533).
const SEED: u32 = 0x4143_4533;

fn card(m: &mut Machine) -> u32 {
    m.lw(CARD)
}

fn context(m: &mut Machine) -> u32 {
    m.gw(CONTEXT)
}

fn test_event(m: &mut Machine, event: u32) -> u32 {
    call(m, TEST_EVENT, [event, 0, 0, 0])
}

/// Copy `n` bytes as the compiled `lwl/lwr` + `swl/swr` (or `lb/sb`)
/// sequences do (no overlap in any use).
fn copy_bytes(m: &mut Machine, dst: u32, src: u32, n: u32) {
    for i in 0..n {
        let b = m.lbu(src.wrapping_add(i));
        m.sb(dst.wrapping_add(i), b);
    }
}

/// Copy `n` (a multiple of 4) bytes word by word.
fn copy_words(m: &mut Machine, dst: u32, src: u32, n: u32) {
    for i in (0..n).step_by(4) {
        let w = m.lw(src.wrapping_add(i));
        m.sw(dst.wrapping_add(i), w);
    }
}

/// 800CBCA8: reset the monitor of both ports (state 1, not ready, no save
/// file, no retries, info result 0) and select port 0. Returns 1.
pub fn reset_ports_cbca8(m: &mut Machine) -> u32 {
    let c = card(m);
    m.sb(c + 0x14cc, 1);
    m.sw(c + 0x121c, 1);
    m.sb(c + 0x14c8, 0);
    m.sb(c + 0x14ca, 0);
    m.sb(c + 0x14d0, 0);
    m.sh(c + 0x1224, 0);
    m.sb(c + 0x14cd, 1);
    m.sb(c + 0x14c9, 0);
    m.sb(c + 0x14cb, 0);
    m.sb(c + 0x14d1, 0);
    m.sh(c + 0x1226, 0);
    m.sw(c + 0x14d4, 0);
    1
}

/// 800CBD20: stop the monitor; 1 when both ports are idle (state 0).
pub fn stop_monitor_cbd20(m: &mut Machine) -> u32 {
    let c = card(m);
    let state = m.lbu(c + 0x14cc);
    m.sw(c + 0x121c, 0);
    if state != 0 {
        return 0;
    }
    u32::from(m.lbu(c + 0x14cd) == 0)
}

/// The event polls of 800CBD50 / 800CBEF4: the first event (in order) that
/// TestEvent reports as occurred gives its index and sets the port's
/// monitor state to `next`; none gives -1.
fn poll_events(m: &mut Machine, events: &[u32], port: u32, next: u32) -> u32 {
    for (index, &offset) in events.iter().enumerate() {
        let c = card(m);
        let event = m.lw(c + offset);
        if test_event(m, event) == 1 {
            let c = card(m);
            m.sb(c.wrapping_add(port) + 0x14cc, next as u8);
            return index as u32;
        }
    }
    u32::MAX
}

/// 800CBD50: poll the software card events (0 done, 1 error, 2 timeout,
/// 3 new card, -1 none).
pub fn poll_sw_cbd50(m: &mut Machine, port: u32, next: u32) -> u32 {
    m.push_frame(0x28);
    let result = poll_events(m, &[0x11fc, 0x1200, 0x1204, 0x1208], port, next);
    m.pop_frame(0x28);
    result
}

/// 800CBE30: clear the four software card events (TestEvent each).
pub fn clear_sw_cbe30(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let mut result = 0;
    for offset in [0x11fc, 0x1200, 0x1204, 0x1208] {
        let c = card(m);
        let event = m.lw(c + offset);
        result = test_event(m, event);
    }
    m.pop_frame(0x18);
    result
}

/// 800CBE9C: clear the three hardware card events.
pub fn clear_hw_cbe9c(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let mut result = 0;
    for offset in [0x120c, 0x1210, 0x1214] {
        let c = card(m);
        let event = m.lw(c + offset);
        result = test_event(m, event);
    }
    m.pop_frame(0x18);
    result
}

/// 800CBEF4: poll the hardware card events (0 done, 1 error, 2 timeout,
/// -1 none).
pub fn poll_hw_cbef4(m: &mut Machine, port: u32, next: u32) -> u32 {
    m.push_frame(0x28);
    let result = poll_events(m, &[0x120c, 0x1210, 0x1214], port, next);
    m.pop_frame(0x28);
    result
}

/// Count a failed check: below 0x11 retries the port stays not ready and
/// the retry count grows, after that it is marked ready (the
/// `sltiu 0x11` blocks of 800CBFAC).
fn retry(m: &mut Machine, at: u32) {
    if m.lbu(at + 0x14d0) < 0x11 {
        m.sb(at + 0x14c8, 0);
        let count = m.lbu(at + 0x14d0);
        m.sb(at + 0x14d0, count.wrapping_add(1));
    } else {
        m.sb(at + 0x14c8, 1);
    }
}

/// Switch the monitor to the other port.
fn other_port(m: &mut Machine) {
    let c = card(m);
    let port = m.lw(c + 0x14d4);
    m.sw(c + 0x14d4, 1u32.wrapping_sub(port));
}

/// 800CBFAC: one step of the card monitor of the polled port
/// (`[+0x14D4]`), by its state `[+0x14CC + port]`: 0 idle (next port), 1
/// start _card_info, 2 wait for it (0x12C ticks), 3 evaluate (ok: clear
/// the card; error/timeout: retry; new card: clear), 4 wait for the clear
/// (0x3E8 ticks, busy-polled), 5 start _card_load, 6 wait, 7 evaluate, 8
/// look for the save file (open/close).
pub fn monitor_cbfac(m: &mut Machine) {
    m.push_frame(0x20);
    let c = card(m);
    let port = m.lw(c + 0x14d4);
    let state = m.lbu(c.wrapping_add(port) + 0x14cc);
    let at = |m: &mut Machine| card(m).wrapping_add(port);
    let half = |m: &mut Machine| card(m).wrapping_add(port << 1);
    match state {
        0 => other_port(m),
        1 => {
            clear_sw_cbe30(m);
            bios::card_info(m, port << 4);
            let p = at(m);
            m.sb(p + 0x14cc, 2);
            let c = card(m);
            m.sw(c + 0x14c4, 0x12c);
        }
        2 => {
            let result = poll_sw_cbd50(m, port, 3);
            let p = at(m);
            m.sb(p + 0x14ce, result as u8);
            let c = card(m);
            let timer = m.lw(c + 0x14c4);
            if (timer as i32) < 0 {
                m.sb(p + 0x14cc, 1);
            } else {
                m.sw(c + 0x14c4, timer.wrapping_sub(1));
            }
        }
        3 => {
            let p = at(m);
            let result = m.lbu(p + 0x14ce);
            let h = half(m);
            m.sh(h + 0x1220, u16::from(result));
            match result {
                0 => {
                    if m.lbu(p + 0x14c8) != 1 {
                        clear_hw_cbe9c(m);
                        bios::card_clear(m, port << 4);
                        m.sb(p + 0x14cc, 4);
                        m.sh(h + 0x1220, 0);
                        m.sb(p + 0x14c8, 0);
                        m.sb(p + 0x14d0, 0);
                        let c = card(m);
                        m.sw(c + 0x14c4, 0x3e8);
                    } else {
                        let c = card(m);
                        let enabled = m.lw(c + 0x121c);
                        m.sb(p + 0x14cc, u8::from(enabled != 0));
                        other_port(m);
                    }
                }
                1 => {
                    retry(m, p);
                    let c = card(m);
                    let enabled = m.lw(c + 0x121c);
                    m.sb(p + 0x14cc, if enabled == 1 { 1 } else { 0 });
                    other_port(m);
                }
                2 => {
                    retry(m, p);
                    let c = card(m);
                    let enabled = m.lw(c + 0x121c);
                    m.sb(p + 0x14cc, u8::from(enabled != 0));
                    let port_now = m.lw(c + 0x14d4);
                    m.sw(c + 0x14d4, 1u32.wrapping_sub(port_now));
                    m.sb(p + 0x14ca, 0);
                }
                3 => {
                    m.sh(h + 0x1220, 0);
                    clear_hw_cbe9c(m);
                    bios::card_clear(m, port << 4);
                    m.sb(p + 0x14cc, 4);
                    m.sb(p + 0x14c8, 0);
                    m.sb(p + 0x14d0, 0);
                }
                _ => {
                    m.sb(p + 0x14c8, 1);
                    m.sb(p + 0x14cc, 1);
                    other_port(m);
                }
            }
        }
        4 => loop {
            let result = poll_hw_cbef4(m, port, 5);
            let p = at(m);
            m.sb(p + 0x14ce, result as u8);
            let c = card(m);
            let timer = m.lw(c + 0x14c4);
            if (timer as i32) < 0 {
                m.sb(p + 0x14cc, 1);
                break;
            }
            m.sw(c + 0x14c4, timer.wrapping_sub(1));
            if m.lbu(p + 0x14ce) != 0xff {
                break;
            }
        },
        5 => {
            let p = at(m);
            let result = m.lbu(p + 0x14ce);
            let h = half(m);
            m.sh(h + 0x1220, u16::from(result));
            clear_sw_cbe30(m);
            bios::card_load(m, port << 4);
            let p = at(m);
            m.sb(p + 0x14cc, 6);
        }
        6 => {
            let result = poll_sw_cbd50(m, port, 7);
            let p = at(m);
            m.sb(p + 0x14ce, result as u8);
        }
        7 => {
            let p = at(m);
            let result = m.lbu(p + 0x14ce);
            let h = half(m);
            m.sh(h + 0x1224, u16::from(result));
            m.sb(p + 0x14c8, 1);
            let loaded = m.lh(h + 0x1224);
            match loaded {
                0 => m.sb(p + 0x14cc, 8),
                1 | 2 => {
                    retry(m, p);
                    m.sb(p + 0x14cc, 1);
                }
                3 => {
                    m.sb(p + 0x14c8, 1);
                    m.sb(p + 0x14cc, 1);
                    other_port(m);
                }
                _ => {}
            }
        }
        8 => {
            let c = card(m);
            let name = c.wrapping_add(port.wrapping_mul(26)) + 0x1490;
            let fd = bios::open(m, name, 0x8001);
            let c = card(m);
            m.sw(c + 0x122c, fd);
            let p = at(m);
            if fd != u32::MAX {
                m.sb(p + 0x14ca, 1);
            }
            let fd = m.lw(c + 0x122c);
            bios::close(m, fd);
            m.sb(p + 0x14cc, 1);
            m.sb(p + 0x14c8, 1);
            other_port(m);
        }
        _ => {}
    }
    m.pop_frame(0x20);
}

/// 800CC554: card status bits of `port` for the save screen (bit 2: a
/// card is usable; 0x10/0x08 save file present / absent; 0x20 unknown;
/// 1 busy, 6/0xC/0x1C/4/0/5 as combined below).
pub fn status_cc554(m: &mut Machine, port: u32) -> u32 {
    let c = card(m);
    let h = c.wrapping_add(port << 1);
    if m.lbu(c.wrapping_add(port) + 0x14c8) == 0 {
        return if m.lh(h + 0x1220) != 2 { 5 } else { 1 };
    }
    match m.lh(h + 0x1220) {
        1 => 6,
        2 => 0,
        0 | 3 => match m.lh(h + 0x1224) {
            1 => 6,
            2 => 0,
            3 => 4,
            0 => match m.lbu(c.wrapping_add(port) + 0x14ca) {
                1 => 0x1c,
                0 => 0xc,
                _ => 6,
            },
            _ => 0x20,
        },
        _ => 0x20,
    }
}

/// 800CC66C: create the seven card events (software class 0xF4000001:
/// done, error, timeout, new card; hardware class 0xF0000011: done, error,
/// timeout; mode 0x2000, no handler) inside a critical section.
pub fn open_events_cc66c(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    bios::enter_critical_section(m);
    let mut event = 0;
    for (class, spec, offset) in [
        (0xf400_0001, 4, 0x11fc),
        (0xf400_0001, 0x8000, 0x1200),
        (0xf400_0001, 0x100, 0x1204),
        (0xf400_0001, 0x2000, 0x1208),
        (0xf000_0011, 4, 0x120c),
        (0xf000_0011, 0x8000, 0x1210),
        (0xf000_0011, 0x100, 0x1214),
    ] {
        event = bios::open_event(m, class, spec, 0x2000, 0);
        let c = card(m);
        m.sw(c + offset, event);
    }
    // The kernel leaves v0 alone: the last OpenEvent's descriptor.
    let result = bios::exit_critical_section(m, event);
    m.pop_frame(0x18);
    result
}

const EVENTS: [u32; 7] = [0x11fc, 0x1200, 0x1204, 0x1208, 0x120c, 0x1210, 0x1214];

/// 800CC774: enable the seven card events.
pub fn enable_events_cc774(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    bios::enter_critical_section(m);
    let mut v0 = 0;
    for offset in EVENTS {
        let c = card(m);
        let event = m.lw(c + offset);
        v0 = bios::enable_event(m, event);
    }
    let result = bios::exit_critical_section(m, v0);
    m.pop_frame(0x18);
    result
}

/// 800CC8F4: close the seven card events.
pub fn close_events_cc8f4(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    bios::enter_critical_section(m);
    let mut v0 = 0;
    for offset in EVENTS {
        let c = card(m);
        let event = m.lw(c + offset);
        v0 = bios::close_event(m, event);
    }
    let result = bios::exit_critical_section(m, v0);
    m.pop_frame(0x18);
    result
}

/// 800CCAC8: build the file header at `header`: "SC", icon flag 0x11, one
/// block, the 0x31-byte SJIS title, zeroed +0x44..+0x50, +0x50 = 1, "CRD0"
/// at +0x52 (its fifth byte then cleared with +0x57), +0x58/+0x5C = 0; then
/// 800CC9AC(`clut`, `icons`) copies the icon data.
pub fn header_ccac8(m: &mut Machine, header: u32, clut: u32, icons: u32) -> u32 {
    m.push_frame(0x18);
    let magic = m.lh(HEADER_MAGIC) as u16;
    let third = m.lb(HEADER_MAGIC + 2) as u8;
    m.sh(header, magic);
    m.sb(header + 2, third);
    m.sb(header + 2, 0x11);
    m.sb(header + 3, 1);
    copy_words(m, header + 4, HEADER_TITLE, 0x30);
    let last = m.lb(HEADER_TITLE + 0x30) as u8;
    m.sb(header + 0x34, last);
    for offset in [0x4c, 0x48, 0x44] {
        m.sw(header + offset, 0);
    }
    m.sh(header + 0x50, 1);
    let fifth = m.lb(HEADER_CRD0 + 4) as u8;
    copy_bytes(m, header + 0x52, HEADER_CRD0, 4);
    m.sb(header + 0x56, fifth);
    m.sb(header + 0x56, 0);
    m.sb(header + 0x57, 0);
    m.sw(header + 0x58, 0);
    m.sw(header + 0x5c, 0);
    let result = call(m, ICON_COPY, [clut, icons, 0, 0]);
    m.pop_frame(0x18);
    result
}

/// The six aircraft records the progress data keeps (800CCBAC, 800CD580):
/// 20 halfwords from context +0x172 + 0x5C*k to `data` + 0x280 + 0x44*k,
/// then the 24 bytes at context +0x19C + 0x5C*k and the word at +0x1B4.
fn copy_aircraft(m: &mut Machine, data: u32, k: u32) {
    let target = data + 0x280 + k * 0x44;
    for i in 0..0x14u32 {
        let ctx = context(m);
        let value = m.lhu(ctx.wrapping_add(k * 0x5c) + 0x172 + i * 2);
        m.sh(target + i * 2, value);
    }
    let ctx = context(m);
    let source = ctx.wrapping_add(k * 0x5c);
    copy_bytes(m, target + 0x28, source + 0x19c, 0x18);
    let word = m.lw(source + 0x1b4);
    m.sw(target + 0x40, word);
}

/// The option bits of +0x420 shared by 800CCBAC and 800CD580: context
/// +0x3A0 bit 0 -> 1, +0x120 bit 0 -> 2, bit 1 -> 4, +0x95E == 1 -> 8.
fn option_bits(m: &mut Machine, data: u32) {
    let ctx = context(m);
    if m.lbu(ctx + 0x3a0) & 1 != 0 {
        let v = m.lw(data + 0x420);
        m.sw(data + 0x420, v | 1);
    }
    let ctx = context(m);
    if m.lw(ctx + 0x120) & 1 != 0 {
        let v = m.lw(data + 0x420);
        m.sw(data + 0x420, v | 2);
    }
    let ctx = context(m);
    if m.lw(ctx + 0x120) & 2 != 0 {
        let v = m.lw(data + 0x420);
        m.sw(data + 0x420, v | 4);
    }
    let ctx = context(m);
    if m.lbu(ctx + 0x95e) == 1 {
        let v = m.lw(data + 0x420);
        m.sw(data + 0x420, v | 8);
    }
}

/// 800CCBAC: initial progress data at `data` (card +0x180): cleared
/// mission tables, the six aircraft records from the context, colours
/// (0, 0xCC, 0xCC), cleared display offsets and the option bits.
pub fn fresh_data_ccbac(m: &mut Machine, data: u32) {
    m.sh(data + 0x10, 0);
    for i in 0..0x3cu32 {
        let at = data + i * 2;
        m.sh(at + 0x184, 0);
        m.sh(at + 0x10c, 0);
        m.sh(at + 0x94, 0);
        m.sh(at + 0x1c, 0);
        let b = data + i;
        m.sb(b + 0x1fc, 0);
        m.sb(b + 0x21a, 0);
        m.sb(b + 0x238, 0);
        m.sb(b + 0x256, 0);
    }
    m.sw(data + 0x278, 0);
    m.sw(data + 0x27c, 0);
    for k in 0..6 {
        copy_aircraft(m, data, k);
    }
    m.sb(data + 0x418, 0);
    m.sb(data + 0x419, 0xcc);
    m.sb(data + 0x41a, 0xcc);
    m.sh(data + 0x41e, 0);
    m.sh(data + 0x41c, 0);
    m.sw(data + 0x420, 0);
    let ctx = context(m);
    if m.lw(ctx + 0x120) & 4 != 0 {
        m.sw(data + 0x420, 0x10);
    }
    option_bits(m, data);
}

/// 800CCDC0: mark a mission record unused (+4 = 0).
pub fn clear_record_ccdc0(m: &mut Machine, record: u32) {
    m.sw(record + 4, 0);
}

/// 800CCDC8: reset the retry counters, copy the two save file names
/// ("bu00:BESCESP02066AC3PAL", "bu10:...") and set both ports' results to
/// 2 (unknown) with no transfer in progress.
pub fn names_ccdc8(m: &mut Machine) {
    let c = card(m);
    m.sh(c + 0x14e0, 0);
    m.sh(c + 0x14e2, 0);
    m.sh(c + 0x14e4, 0);
    copy_words(m, c + 0x1490, NAME_PORT_0, 0x18);
    let c = card(m);
    copy_bytes(m, c + 0x14aa, NAME_PORT_1, 0x18);
    for port in 0..2u32 {
        m.sw(c + port * 4 + 0x14d8, 0);
        m.sh(c + port * 2 + 0x1220, 2);
        m.sh(c + port * 2 + 0x1224, 2);
    }
}

/// 800CCEB8: prepare the card layer: names, header (icon data from
/// `clut`/`icons`), fresh progress data, six unused records, the events,
/// and the monitor reset. Returns 1.
pub fn prepare_cceb8(m: &mut Machine, clut: u32, icons: u32) -> u32 {
    m.push_frame(0x28);
    names_ccdc8(m);
    let c = card(m);
    header_ccac8(m, c, clut, icons);
    let c = card(m);
    fresh_data_ccbac(m, c + 0x180);
    for k in 0..6u32 {
        let c = card(m);
        clear_record_ccdc0(m, c + 0x5a4 + k * 0x12c);
    }
    open_events_cc66c(m);
    let result = reset_ports_cbca8(m);
    m.pop_frame(0x28);
    result
}

/// 800CCF50: the current mission record index (card +0x190, signed).
pub fn current_record_ccf50(m: &mut Machine) -> u32 {
    let c = card(m);
    m.lh(c + 0x190) as i32 as u32
}

/// Eight bits from eight bytes' bit 0 (the bit-packing loops of 800CD580
/// and 800CD98C): byte `at` = OR of (`src[k] & 1) << k`, built in place.
fn pack_bits(m: &mut Machine, at: u32, src: u32) {
    m.sb(at, 0);
    for k in 0..8u32 {
        let bit = m.lbu(src + k) & 1;
        let v = m.lbu(at);
        m.sb(at, v | (bit << k));
    }
}

/// Two nibbles from two bytes: `lo & 0xF | (hi & 0xF) << 4`.
fn pack_nibbles(m: &mut Machine, lo: u32, hi: u32) -> u8 {
    let a = m.lbu(lo) & 0xf;
    let b = m.lbu(hi) & 0xf;
    a | (b << 4)
}

/// 800CD580: pack the context's progress (block +0x960) into the progress
/// data (card +0x180) for mission record `slot`.
pub fn pack_progress_cd580(m: &mut Machine, slot: u32) {
    m.push_frame(8);
    let ctx = context(m);
    let c = card(m);
    let block = ctx + 0x960;
    let data = c + 0x180;
    let v = m.lw(block + 0x14);
    m.sw(data + 4, v);
    let v = m.lbu(block + 1);
    m.sh(data + 0x10, u16::from(v));
    let v = m.lw(ctx + 0x38c);
    m.sw(data + 0x420, 0);
    m.sw(data + 0xc, v);
    let v = m.lw(ctx + 0x96c);
    m.sw(data + 8, v);
    let v = m.lw(ctx + 0xf78);
    m.sw(data + 0x274, v);
    for k in 0..6u32 {
        copy_aircraft(m, data, k);
        let ctx = context(m);
        let v = m.lw(data + 0x420);
        let v = if m.lw(ctx + 0x120) & 4 != 0 {
            v | 0x10
        } else {
            v & !0x10
        };
        m.sw(data + 0x420, v);
    }
    for row in 0..8u32 {
        pack_bits(m, data + row + 0x14, block + row * 8 + 0x18);
    }
    for row in 0..2u32 {
        pack_bits(m, data + row + 0x12, block + row * 8 + 0x98);
    }
    for i in 0..0x3cu32 {
        let s = block + i * 2;
        let d = data + i * 2;
        let v = m.lhu(s + 0xa8);
        m.sh(d + 0x1c, v);
        let v = m.lhu(s + 0x120);
        m.sh(d + 0x94, v);
        let v = m.lhu(s + 0x198);
        m.sh(d + 0x10c, v);
        let v = m.lhu(s + 0x210);
        m.sh(d + 0x184, v);
    }
    for i in 0..0x1eu32 {
        let s = block + i * 2;
        let d = data + i;
        for (from, to) in [
            (0x300, 0x1fc),
            (0x33c, 0x21a),
            (0x378, 0x238),
            (0x3b4, 0x256),
        ] {
            let v = pack_nibbles(m, s + from, s + 1 + from);
            m.sb(d + to, v);
        }
    }
    let display = m.lw(0x800b_acb8);
    let v = m.lhu(display + 8);
    m.sh(data + 0x41c, v);
    let v = m.lhu(display + 0xc);
    let ctx = context(m);
    m.sh(data + 0x41e, v);
    let v = m.lbu(ctx + 0x39c);
    m.sb(data + 0x419, v);
    let ctx = context(m);
    let v = m.lbu(ctx + 0x39d);
    m.sb(data + 0x41a, v);
    let ctx = context(m);
    let v = m.lbu(ctx + 0x39e);
    m.sb(data + 0x418, v);
    let v = m.lw(block + 0x610);
    m.sw(data + 0x278, v);
    let v = m.lw(block + 0x614);
    m.sh(data + 0x10, slot as u16);
    m.sw(data + 0x27c, v);
    option_bits(m, data);
    m.pop_frame(8);
}

/// 800CD98C: fill mission record `k` (card +0x5A4 + 0x12C*k) from the
/// context and mark it used.
pub fn pack_record_cd98c(m: &mut Machine, k: u32) {
    let c = card(m);
    let record = c.wrapping_add(k.wrapping_mul(0x12c)).wrapping_add(0x5a4);
    let ctx = context(m);
    let flags = m.lw(record + 4);
    let block = ctx + 0x960;
    m.sw(record + 4, flags | 1);
    let v = m.lw(block + 0x60c);
    m.sw(record + 0x124, v);
    let v = m.lbu(ctx + 0x390);
    m.sb(record + 0x129, v);
    let ctx = context(m);
    let v = m.lbu(ctx + 0x398);
    m.sb(record + 0x128, v);
    for i in 0..0x3cu32 {
        let v = m.lhu(block + i * 2 + 0x288);
        m.sh(record + i * 2 + 8, v);
        let v = m.lbu(block + i * 4 + 0x42c);
        m.sb(record + i + 0x9e, v);
        let v = m.lbu(block + i * 4 + 0x51c);
        m.sb(record + i + 0xda, v);
    }
    for (from, to) in [
        (0x392, 0x11e),
        (0x3ad, 0x11f),
        (0x3ae, 0x120),
        (0x3af, 0x121),
    ] {
        let ctx = context(m);
        let v = m.lbu(ctx + from);
        m.sb(record + to, v);
    }
    for row in 0..8u32 {
        pack_bits(m, record + row + 0x116, block + row * 8 + 0x58);
    }
    for i in 0..0x1eu32 {
        let v = pack_nibbles(m, block + i * 2 + 0x3f0, block + i * 2 + 1 + 0x3f0);
        m.sb(record + i + 0x80, v);
    }
}

/// 800CDB18: format the card in `port` once both ports are idle. 0 while
/// busy, 1 on success, -1 on failure.
pub fn format_cdb18(m: &mut Machine, port: u32) -> u32 {
    m.push_frame(0x18);
    let result = if stop_monitor_cbd20(m) == 0 {
        0
    } else {
        let formatted = bios::card_format(m, port << 4);
        reset_ports_cbca8(m);
        if formatted == 1 { 1 } else { u32::MAX }
    };
    m.pop_frame(0x18);
    result
}

/// 800CDB68: list the card in `port` ("bu%1d0:*.*"): copy each file name
/// to the port's directory table, subtract each file's blocks from 15 free
/// blocks, store the file count; returns the free blocks (signed), or -1
/// while the monitor is busy.
pub fn directory_cdb68(m: &mut Machine, port: u32) -> u32 {
    let frame = m.push_frame(0x68);
    let result = if stop_monitor_cbd20(m) == 0 {
        u32::MAX
    } else {
        let pattern = frame + 0x10;
        let entry = frame + 0x20;
        let c = card(m);
        m.sh(c.wrapping_add(port << 1) + 0x1230, 0xf);
        call(m, SPRINTF, [pattern, DIRECTORY_PATTERN, port, 0]);
        let mut count = 0u32;
        if bios::first_file(m, pattern, entry) != 0 {
            let table = port.wrapping_mul(0x12c).wrapping_add(0x1238);
            let mut name = 0u32;
            loop {
                let c = card(m);
                call(
                    m,
                    STRCPY,
                    [c.wrapping_add(table).wrapping_add(name), entry, 0, 0],
                );
                let size = m.lw(frame + 0x38) as i32;
                let c = card(m);
                let rounded = size.wrapping_add(0x1fff);
                let rounded = if rounded < 0 {
                    size.wrapping_add(0x3ffe)
                } else {
                    rounded
                };
                let at = c.wrapping_add(port << 1);
                name = name.wrapping_add(0x14);
                count = count.wrapping_add(1);
                let free = m.lhu(at + 0x1230);
                m.sh(at + 0x1230, free.wrapping_sub((rounded >> 13) as u16));
                if bios::next_file(m, entry) == 0 {
                    break;
                }
            }
        }
        let c = card(m);
        let at = c.wrapping_add(port << 1);
        m.sh(at + 0x1234, count as u16);
        reset_ports_cbca8(m);
        let c = card(m);
        m.lh(c.wrapping_add(port << 1) + 0x1230) as i32 as u32
    };
    m.pop_frame(0x68);
    result
}

/// The port's save file name (card +0x1490 + 26*port).
fn file_name(m: &mut Machine, port: u32) -> u32 {
    let c = card(m);
    c.wrapping_add(port.wrapping_mul(26)).wrapping_add(0x1490)
}

/// 800CDCA8: create the save file (one block) in `port`: 0 while busy, 1
/// created, -1 failed.
pub fn create_cdca8(m: &mut Machine, port: u32) -> u32 {
    m.push_frame(0x20);
    let result = if stop_monitor_cbd20(m) == 0 {
        0
    } else {
        let name = file_name(m, port);
        let fd = bios::open(m, name, 0x1_0200);
        let c = card(m);
        m.sw(c + 0x122c, fd);
        if fd != u32::MAX {
            bios::close(m, fd);
            reset_ports_cbca8(m);
            1
        } else {
            bios::close(m, u32::MAX);
            reset_ports_cbca8(m);
            u32::MAX
        }
    };
    m.pop_frame(0x20);
    result
}

/// XOR of the `words` words from `at` seeded with "3ECA" (the compiled
/// loops run `size / 4 - 1` words after the stored checksum).
fn checksum(m: &mut Machine, at: u32, words: u32) -> u32 {
    let mut sum = SEED;
    for i in 0..words {
        sum ^= m.lw(at + i * 4);
    }
    sum
}

/// 800CDD50: checksum the progress data (card +0x184.., 264 words) into
/// +0x180 and copy the 0x424 bytes to the backup at +0xCAC.
pub fn seal_data_cdd50(m: &mut Machine) {
    let c = card(m);
    let sum = checksum(m, c + 0x184, 0x424 / 4 - 1);
    let c = card(m);
    m.sw(c + 0x180, sum);
    copy_words(m, c + 0xcac, c + 0x180, 0x424);
}

/// 800CDDF0: checksum the six mission records; the current one
/// (`[card+0x190]`) is also copied to the backup at +0x10D0.
pub fn seal_records_cddf0(m: &mut Machine) {
    for k in 0..6u32 {
        let c = card(m);
        let record = c + 0x5a4 + k * 0x12c;
        let sum = checksum(m, record + 4, 0x12c / 4 - 1);
        let c = card(m);
        let record = c + 0x5a4 + k * 0x12c;
        m.sw(record, sum);
        if k as i32 == i32::from(m.lh(c + 0x190)) {
            copy_words(m, c + 0x10d0, record, 0x12c);
        }
    }
}

/// Bump the open retry counter (card +0x14E0) while below 3; true when
/// it was bumped (the caller then returns 0, "busy").
fn bump_retry(m: &mut Machine) -> bool {
    let c = card(m);
    let count = m.lh(c + 0x14e0);
    let raw = m.lhu(c + 0x14e0);
    if count < 3 {
        m.sh(c + 0x14e0, raw.wrapping_add(1));
        true
    } else {
        false
    }
}

/// Clear the port's transfer flag (card +0x14D8 + 4*port, bit 0).
fn end_transfer(m: &mut Machine, port: u32) {
    let c = card(m);
    let at = c.wrapping_add(port << 2) + 0x14d8;
    let v = m.lw(at);
    m.sw(at, v & !1);
}

/// 800CDED4: save to `port`: once idle, stamp the session (context +0x96C
/// and data +8 = frame clock), seal data and records, open the file for
/// writing and start the asynchronous write of the 0x11FC-byte image;
/// then poll the software events. 0 while busy, 1 done (`finish` == 1 also
/// resets the monitor), -1 failed after three retries.
pub fn save_cded4(m: &mut Machine, port: u32, finish: u32) -> u32 {
    m.push_frame(0x28);
    let result = save_steps(m, port, finish);
    m.pop_frame(0x28);
    result
}

fn save_steps(m: &mut Machine, port: u32, finish: u32) -> u32 {
    if stop_monitor_cbd20(m) == 0 {
        return 0;
    }
    let c = card(m);
    if m.lw(c.wrapping_add(port << 2) + 0x14d8) & 1 == 0 {
        let ctx = context(m);
        let clock = m.lw(ctx + 0x20);
        m.sw(ctx + 0x96c, clock);
        m.sw(c + 0x188, clock);
        seal_data_cdd50(m);
        seal_records_cddf0(m);
        let name = file_name(m, port);
        let fd = bios::open(m, name, 0x8002);
        let c = card(m);
        m.sw(c + 0x122c, fd);
        if fd == u32::MAX {
            if bump_retry(m) {
                return 0;
            }
            reset_ports_cbca8(m);
            end_transfer(m, port);
            return u32::MAX;
        }
        bios::write(m, fd, c, 0x11fc);
        let c = card(m);
        let at = c.wrapping_add(port << 2) + 0x14d8;
        let v = m.lw(at);
        m.sw(at, v | 1);
    }
    let event = poll_sw_cbd50(m, port, 0);
    if event == u32::MAX {
        return 0;
    }
    let c = card(m);
    let fd = m.lw(c + 0x122c);
    bios::close(m, fd);
    end_transfer(m, port);
    if event == 0 {
        if finish == 1 {
            reset_ports_cbca8(m);
        }
        return 1;
    }
    if bump_retry(m) {
        return 0;
    }
    end_transfer(m, port);
    reset_ports_cbca8(m);
    u32::MAX
}

/// 800CE09C: check the loaded progress data: 1 when its checksum holds, 2
/// when only the backup's does (the backup is restored), 0 otherwise.
pub fn check_data_ce09c(m: &mut Machine) -> u32 {
    let c = card(m);
    let stored = m.lw(c + 0x180);
    if checksum(m, c + 0x184, 0x424 / 4 - 1) == stored {
        return 1;
    }
    let c = card(m);
    let stored = m.lw(c + 0xcac);
    if checksum(m, c + 0xcb0, 0x424 / 4 - 1) != stored {
        return 0;
    }
    let c = card(m);
    copy_words(m, c + 0x180, c + 0xcac, 0x424);
    2
}

/// 800CE1A4: check the six loaded mission records; a bad current record
/// is restored from the backup when that holds, any other bad record is
/// marked unused.
pub fn check_records_ce1a4(m: &mut Machine) {
    for k in 0..6u32 {
        let c = card(m);
        let record = c + 0x5a4 + k * 0x12c;
        let stored = m.lw(record);
        if checksum(m, record + 4, 0x12c / 4 - 1) == stored {
            continue;
        }
        let c = card(m);
        if k as i32 == i32::from(m.lh(c + 0x190)) {
            let stored = m.lw(c + 0x10d0);
            if checksum(m, c + 0x10d4, 0x12c / 4 - 1) == stored {
                let c = card(m);
                copy_words(m, c + 0x5a4 + k * 0x12c, c + 0x10d0, 0x12c);
                continue;
            }
            let c = card(m);
            m.sw(c + k * 0x12c + 0x5a8, 0);
        } else {
            m.sw(c + k * 0x12c + 0x5a8, 0);
        }
    }
}

/// 800CE2FC: load from `port`: once idle, open the file and start the
/// asynchronous read into the card block; then poll. 0 while busy, 1
/// loaded (`finish` == 1 resets the monitor), 2 data checksum failed, 3
/// saved in another session, -1 failed after three retries.
pub fn load_ce2fc(m: &mut Machine, port: u32, finish: u32) -> u32 {
    m.push_frame(0x28);
    let result = load_steps(m, port, finish);
    m.pop_frame(0x28);
    result
}

fn load_steps(m: &mut Machine, port: u32, finish: u32) -> u32 {
    if stop_monitor_cbd20(m) == 0 {
        return 0;
    }
    let c = card(m);
    if m.lw(c.wrapping_add(port << 2) + 0x14d8) & 1 == 0 {
        let name = file_name(m, port);
        let fd = bios::open(m, name, 0x8001);
        let c = card(m);
        m.sw(c + 0x122c, fd);
        if fd == u32::MAX {
            if bump_retry(m) {
                return 0;
            }
            reset_ports_cbca8(m);
            end_transfer(m, port);
            return u32::MAX;
        }
        bios::read(m, fd, c, 0x11fc);
        let c = card(m);
        let at = c.wrapping_add(port << 2) + 0x14d8;
        let v = m.lw(at);
        m.sw(at, v | 1);
    }
    let event = poll_sw_cbd50(m, port, 0);
    if event == u32::MAX {
        return 0;
    }
    let c = card(m);
    let fd = m.lw(c + 0x122c);
    bios::close(m, fd);
    end_transfer(m, port);
    if check_data_ce09c(m) == 0 {
        return 2;
    }
    check_records_ce1a4(m);
    let c = card(m);
    let ctx = context(m);
    if m.lw(c + 0x188) != m.lw(ctx + 0x96c) {
        return 3;
    }
    if event == 0 {
        end_transfer(m, port);
        if finish == 1 {
            reset_ports_cbca8(m);
        }
        return 1;
    }
    if bump_retry(m) {
        return 0;
    }
    end_transfer(m, port);
    reset_ports_cbca8(m);
    u32::MAX
}

/// 800CE500: mission record `k` when it is in use (bit 0 of +4), else 0
/// (also for `k` >= 6).
pub fn record_ce500(m: &mut Machine, k: u32) -> u32 {
    let c = card(m);
    let record = c.wrapping_add(k.wrapping_mul(0x12c)).wrapping_add(0x5a4);
    if (k as i32) >= 6 {
        return 0;
    }
    if m.lw(record + 4) & 1 != 0 { record } else { 0 }
}

/// 800CE554: the better of two ranks for entry `i`: nibble `a[i] & 0xF`
/// against nibble `i` of the packed `b` (low nibble for even `i`); the
/// lower non-zero one wins.
pub fn best_rank_ce554(m: &mut Machine, a: u32, b: u32, i: u32) -> u32 {
    let own = u32::from(m.lbu(a.wrapping_add(i))) & 0xf;
    let half = ((i as i32).wrapping_add((i >> 31) as i32) >> 1) as u32;
    let byte = u32::from(m.lbu(b.wrapping_add(half)));
    let other = if i & 1 == 0 { byte & 0xf } else { byte >> 4 };
    let own8 = own & 0xff;
    let other8 = other & 0xff;
    let best = if own8 == 0 || (other8 != 0 && other8 < own8) {
        other
    } else {
        own
    };
    best & 0xff
}

/// 800CE5C8: merge the loaded progress data into the context: OR the
/// flags and completion bits, keep the better (lower) times and ranks,
/// the higher totals and the lower +0x614.
pub fn merge_ce5c8(m: &mut Machine) {
    m.push_frame(0x28);
    let c = card(m);
    let ctx = context(m);
    let data = c + 0x180;
    let block = ctx + 0x960;
    let v = m.lw(block + 0x14) | m.lw(data + 4);
    m.sw(block + 0x14, v);
    for row in 0..2u32 {
        for k in 0..8u32 {
            let bits = m.lbu(data + row + 0x12);
            if (u32::from(bits) >> k) & 1 != 0 {
                m.sb(block + row * 8 + k + 0x98, 1);
            }
        }
    }
    for row in 0..8u32 {
        for k in 0..8u32 {
            let bits = m.lbu(data + row + 0x14);
            if (u32::from(bits) >> k) & 1 != 0 {
                m.sb(block + row * 8 + k + 0x18, 1);
            }
        }
    }
    for i in 0..0x3cu32 {
        let d = data + i * 2;
        let b = block + i * 2;
        for (from, to) in [(0x1c, 0xa8), (0x94, 0x120), (0x10c, 0x198)] {
            let mine = m.lhu(d + from);
            if mine < m.lhu(b + to) {
                m.sh(b + to, mine);
            }
        }
    }
    for i in 0..0x3cu32 {
        let first = best_rank_ce554(m, block + 0x300, data + 0x1fc, i);
        let at = block + i;
        m.sb(at + 0x300, first as u8);
        let second = best_rank_ce554(m, block + 0x33c, data + 0x21a, i);
        m.sb(at + 0x33c, second as u8);
        let third = best_rank_ce554(m, block + 0x378, data + 0x238, i);
        m.sb(at + 0x378, third as u8);
    }
    let theirs = m.lw(data + 0x274);
    if m.lw(block + 0x618) < theirs {
        m.sw(block + 0x618, theirs);
    }
    let theirs = m.lw(data + 0x278);
    if m.lw(block + 0x610) < theirs {
        m.sw(block + 0x610, theirs);
    }
    let theirs = m.lw(data + 0x27c);
    if theirs < m.lw(block + 0x614) {
        m.sw(block + 0x614, theirs);
    }
    m.pop_frame(0x28);
}

/// Claims of this part by original address, asked by `ov2_hub::dispatch`
/// (nested so the crate-wide claim scan sees them).
pub mod claims {
    use super::*;
    use crate::{common::loaded_module, ov2_hub::MODULE};

    /// The claims, only while module 2 is loaded, including the resident
    /// BIOS / libcard stubs the overlay calls ([`bios`], as host calls).
    /// Routines without a result return 0 (no caller reads their `v0`).
    pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
        if loaded_module(m) != MODULE {
            return None;
        }
        let [a0, a1, a2, _] = args;
        Some(match target {
            0x800c_bca8 => reset_ports_cbca8(m),
            0x800c_bd20 => stop_monitor_cbd20(m),
            0x800c_bd50 => poll_sw_cbd50(m, a0, a1),
            0x800c_be30 => clear_sw_cbe30(m),
            0x800c_be9c => clear_hw_cbe9c(m),
            0x800c_bef4 => poll_hw_cbef4(m, a0, a1),
            0x800c_bfac => {
                monitor_cbfac(m);
                0
            }
            0x800c_c554 => status_cc554(m, a0),
            0x800c_c66c => open_events_cc66c(m),
            0x800c_c774 => enable_events_cc774(m),
            0x800c_c8f4 => close_events_cc8f4(m),
            0x800c_cac8 => header_ccac8(m, a0, a1, a2),
            0x800c_cbac => {
                fresh_data_ccbac(m, a0);
                0
            }
            0x800c_cdc0 => {
                clear_record_ccdc0(m, a0);
                0
            }
            0x800c_cdc8 => {
                names_ccdc8(m);
                0
            }
            0x800c_ceb8 => prepare_cceb8(m, a0, a1),
            0x800c_cf50 => current_record_ccf50(m),
            0x800c_d580 => {
                pack_progress_cd580(m, a0);
                0
            }
            0x800c_d98c => {
                pack_record_cd98c(m, a0);
                0
            }
            0x800c_db18 => format_cdb18(m, a0),
            0x800c_db68 => directory_cdb68(m, a0),
            0x800c_dca8 => create_cdca8(m, a0),
            0x800c_dd50 => {
                seal_data_cdd50(m);
                0
            }
            0x800c_ddf0 => {
                seal_records_cddf0(m);
                0
            }
            0x800c_ded4 => save_cded4(m, a0, a1),
            0x800c_e09c => check_data_ce09c(m),
            0x800c_e1a4 => {
                check_records_ce1a4(m);
                0
            }
            0x800c_e2fc => load_ce2fc(m, a0, a1),
            0x800c_e500 => record_ce500(m, a0),
            0x800c_e554 => best_rank_ce554(m, a0, a1, a2),
            0x800c_e5c8 => {
                merge_ce5c8(m);
                0
            }
            _ => return None,
        })
    }
}
