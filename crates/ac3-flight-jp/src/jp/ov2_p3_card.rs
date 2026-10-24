//! JP overlay 2, part 3: the memory-card layer of the JP save screens.
//!
//! The JP counterpart of PAL's `ov2_hub_card` (800CBCA8..), rewritten for
//! the JP card file: `BISLPSP02020AC3ES`, two blocks, read and written in
//! records with `lseek` (JP_PORT_PLAN §6): a 0x280-byte head written from
//! the start, a 0x80 record at 0x280, 0x480 records at 0x300 + 0x480 n and
//! 0x180 records at 0x1080 + 0x900 account + 0x180 n. Every record is
//! guarded by the "3ECA" checksum (word 0 = 0x41434533 XOR the other words).
//!
//! Card block `[0x80112DF0]` (0x360 bytes, 800E37F8): +4..+0x10 the four
//! software card events (I/O done, error, timeout, new card), +0x14..+0x20
//! the hardware card events (three opened; +0x20 is polled too), +0x28
//! monitor enable, +0x2C / +0x30 per-port `_card_info` / `_card_load`
//! results (halfwords), +0x3C the open file, +0x50 the record index of the
//! multi-record reads, +0x54 / +0x58 per-port free blocks and file count,
//! +0x5C per-port 15 directory names (0x14 bytes), +0x2B4 per-port save
//! file names (26 bytes), +0x2E8 the 0x80 record, +0x2EC.. three 0x480
//! records, +0x2F8 the current 0x480 record, +0x2FC.. six 0x180 records,
//! +0x314 the current 0x180 record, +0x318 timeout counter, +0x336
//! transfer started, +0x348.. per-port bytes (+0x348 card ready, +0x34A
//! save file present, +0x34C monitor state, +0x34E event result, +0x350
//! retry count), +0x354 polled port, +0x358 transfer retries, +0x35A
//! transfer busy.
use super::ov2_p3::MODULE;
use crate::common::{CONTEXT, loaded_module};
use crate::dispatch::call;
use crate::machine::SmallData;
use crate::{bios, machine::Machine, sdk};

/// `[0x80112DF0]`: the card block.
pub const CARD: u32 = 0x8011_2df0;
/// `[0x80112DEC]`: the 0x280-byte card file head (800E38DC).
pub const HEAD: u32 = 0x8011_2dec;
/// The checksum seed "3ECA".
pub const SEED: u32 = 0x4143_4533;
/// The 0x80 record's version word.
pub const VERSION: u32 = 0x0099_0426;
/// libc (resident, JP addresses): strcpy, sprintf.
pub const STRCPY: u32 = 0x800a_dea8;
pub const SPRINTF: u32 = 0x800a_e8f8;
/// StoreImage2 (resident, JP).
const STORE_IMAGE2: u32 = 0x800a_a9cc;
/// 0x800CCE10 "bu%1d0:*.*".
const DIRECTORY_PATTERN: u32 = 0x800c_ce10;
/// 0x800CCE1C: the RECT of the debug screen dump; 0x800CCE24 its file.
const DUMP_RECT: u32 = 0x800c_ce1c;
const DUMP_FILE: u32 = 0x800c_ce24;

pub fn card(m: &mut Machine) -> u32 {
    m.lw(CARD)
}

fn context(m: &mut Machine) -> u32 {
    m.gw(CONTEXT)
}

fn test_event(m: &mut Machine, event: u32) -> u32 {
    sdk::test_event(m, event)
}

/// `port * 26 + 0x2B4`: the port's save file name in the card block.
fn name_of(port: u32) -> u32 {
    port.wrapping_mul(26).wrapping_add(0x2b4)
}

/// The "3ECA" checksum of `words` words after the first at `record`.
pub fn checksum(m: &mut Machine, record: u32, words: u32) -> u32 {
    let mut sum = SEED;
    let mut at = record.wrapping_add(4);
    for _ in 0..words {
        let w = m.lw(at);
        at = at.wrapping_add(4);
        sum ^= w;
    }
    sum
}

/// 800E25D4: reset the monitor of both ports (state 1, not ready, no
/// save file, no retries, load result 0) and select port 0. Returns 1.
pub fn reset_ports_e25d4(m: &mut Machine) -> u32 {
    let c = card(m);
    m.sb(c.wrapping_add(0x34c), 1);
    let v = card(m);
    m.sw(c.wrapping_add(0x28), 1);
    m.sb(v.wrapping_add(0x348), 0);
    let v = card(m);
    m.sb(v.wrapping_add(0x34a), 0);
    let v = card(m);
    m.sb(v.wrapping_add(0x350), 0);
    let c = card(m);
    m.sh(v.wrapping_add(0x30), 0);
    m.sb(c.wrapping_add(0x34d), 1);
    let v = card(m);
    m.sb(v.wrapping_add(0x349), 0);
    let v = card(m);
    m.sb(v.wrapping_add(0x34b), 0);
    let v = card(m);
    m.sb(v.wrapping_add(0x351), 0);
    let c = card(m);
    m.sh(v.wrapping_add(0x32), 0);
    m.sw(c.wrapping_add(0x354), 0);
    1
}

/// 800E264C: stop the monitor; 1 when both ports are idle (state 0).
pub fn stop_monitor_e264c(m: &mut Machine) -> u32 {
    let c = card(m);
    let state = m.lbu(c.wrapping_add(0x34c));
    m.sw(c.wrapping_add(0x28), 0);
    if state != 0 {
        return 0;
    }
    u32::from(m.lbu(c.wrapping_add(0x34d)) == 0)
}

/// 800E267C: the card status of `port`: 0 no card, 6 not ready with
/// info result 2; else by the info result (+0x2C) 1 / 2, and for 0 or 3
/// by the load result (+0x30): 1 / 2 / 3, for 0 a save file 5 or none
/// 4; -1 otherwise.
pub fn status_e267c(m: &mut Machine, port: u32) -> u32 {
    let c = card(m);
    if m.lbu(c.wrapping_add(port).wrapping_add(0x348)) == 0 {
        let info = m.lh(c.wrapping_add(port << 1).wrapping_add(0x2c));
        return if info == 2 { 6 } else { 0 };
    }
    match m.lh(c.wrapping_add(port << 1).wrapping_add(0x2c)) {
        1 => return 1,
        2 => return 2,
        0 | 3 => {}
        _ => return u32::MAX,
    }
    let c = card(m);
    match m.lh(c.wrapping_add(port << 1).wrapping_add(0x30)) {
        1 => 1,
        2 => 2,
        3 => 3,
        0 => match m.lbu(c.wrapping_add(port).wrapping_add(0x34a)) {
            1 => 5,
            0 => 4,
            _ => 1,
        },
        _ => u32::MAX,
    }
}

/// The event polls of 800E2794 / 800E294C: the first event (in order)
/// that TestEvent reports as occurred gives its index and sets the
/// port's monitor state to `next`; none gives -1.
fn poll_events(m: &mut Machine, events: [u32; 4], port: u32, next: u32) -> u32 {
    m.push_frame(0x28);
    let mut result = u32::MAX;
    for (index, offset) in events.into_iter().enumerate() {
        let c = card(m);
        let event = m.lw(c.wrapping_add(offset));
        if test_event(m, event) == 1 {
            let c = card(m);
            m.sb(c.wrapping_add(port).wrapping_add(0x34c), next as u8);
            result = index as u32;
            break;
        }
    }
    m.pop_frame(0x28);
    result
}

/// 800E2794: poll the software card events (0 done, 1 error, 2 timeout,
/// 3 new card, -1 none).
pub fn poll_sw_e2794(m: &mut Machine, port: u32, next: u32) -> u32 {
    poll_events(m, [4, 8, 0xc, 0x10], port, next)
}

/// 800E294C: poll the hardware card events (+0x14..+0x20).
pub fn poll_hw_e294c(m: &mut Machine, port: u32, next: u32) -> u32 {
    poll_events(m, [0x14, 0x18, 0x1c, 0x20], port, next)
}

fn clear_events(m: &mut Machine, events: [u32; 4]) -> u32 {
    m.push_frame(0x18);
    let mut result = 0;
    for offset in events {
        let c = card(m);
        let event = m.lw(c.wrapping_add(offset));
        result = test_event(m, event);
    }
    m.pop_frame(0x18);
    result
}

/// 800E2874: clear the four software card events (TestEvent each).
pub fn clear_sw_e2874(m: &mut Machine) -> u32 {
    clear_events(m, [4, 8, 0xc, 0x10])
}

/// 800E28E0: clear the hardware card events (+0x14..+0x20).
pub fn clear_hw_e28e0(m: &mut Machine) -> u32 {
    clear_events(m, [0x14, 0x18, 0x1c, 0x20])
}

/// 800E2A2C: context +0x20.
pub fn context_word_e2a2c(m: &mut Machine) -> u32 {
    let c = context(m);
    m.lw(c.wrapping_add(0x20))
}

/// 800E2A44: copy the current record's name (+8) to the context's
/// (+0x948). Returns strcpy's result.
pub fn name_to_context_e2a44(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let c = card(m);
    let ctx = context(m);
    let record = m.lw(c.wrapping_add(0x2f8));
    let v0 = call(
        m,
        STRCPY,
        [ctx.wrapping_add(0x948), record.wrapping_add(8), 0, 0],
    );
    m.pop_frame(0x18);
    v0
}

/// 800E2A7C: copy the context's name (+0x948) to the current record's.
pub fn name_from_context_e2a7c(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let c = card(m);
    let ctx = context(m);
    let record = m.lw(c.wrapping_add(0x2f8));
    let v0 = call(
        m,
        STRCPY,
        [record.wrapping_add(8), ctx.wrapping_add(0x948), 0, 0],
    );
    m.pop_frame(0x18);
    v0
}

/// The seven card events: (class, spec, card block offset).
const EVENTS: [(u32, u32, u32); 7] = [
    (bios::SW_CARD, bios::EV_IOE, 4),
    (bios::SW_CARD, bios::EV_ERROR, 8),
    (bios::SW_CARD, bios::EV_TIMEOUT, 0xc),
    (bios::SW_CARD, bios::EV_NEW, 0x10),
    (bios::HW_CARD, bios::EV_IOE, 0x14),
    (bios::HW_CARD, bios::EV_ERROR, 0x18),
    (bios::HW_CARD, bios::EV_TIMEOUT, 0x1c),
];

/// 800E3FCC: open the seven card events (no-interrupt mode, no handler)
/// and enable them, in a critical section. Returns the last EnableEvent's
/// result (ExitCriticalSection leaves `v0`).
pub fn open_events_e3fcc(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    bios::enter_critical_section(m);
    for (class, spec, offset) in EVENTS {
        let event = bios::open_event(m, class, spec, bios::EV_MODE_NO_INTERRUPT, 0);
        let c = card(m);
        m.sw(c.wrapping_add(offset), event);
    }
    let mut v0 = 0;
    for (_, _, offset) in EVENTS {
        let c = card(m);
        let event = m.lw(c.wrapping_add(offset));
        v0 = bios::enable_event(m, event);
    }
    let result = bios::exit_critical_section(m, v0);
    m.pop_frame(0x18);
    result
}

/// 800E415C: monitor enable off.
pub fn monitor_off_e415c(m: &mut Machine) {
    let c = card(m);
    m.sw(c.wrapping_add(0x28), 0);
}

/// 800E419C: close the seven card events, in a critical section.
pub fn close_events_e419c(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    bios::enter_critical_section(m);
    let mut v0 = 0;
    for (_, _, offset) in EVENTS {
        let c = card(m);
        let event = m.lw(c.wrapping_add(offset));
        v0 = bios::close_event(m, event);
    }
    let result = bios::exit_critical_section(m, v0);
    m.pop_frame(0x18);
    result
}

/// 800E4760: format the card in `port` (synchronous `_card_format`) once
/// the monitor is stopped: 0 busy, 1 formatted, -1 failed.
pub fn format_e4760(m: &mut Machine, port: u32) -> u32 {
    m.push_frame(0x18);
    let result = if stop_monitor_e264c(m) == 0 {
        0
    } else {
        let formatted = bios::card_format(m, port << 4);
        reset_ports_e25d4(m);
        if formatted == 1 { 1 } else { u32::MAX }
    };
    m.pop_frame(0x18);
    result
}

/// 800E47B0: whether the save file exists in `port` (open with mode
/// 0x20200, close): 0 busy, 1 present, -1 absent.
pub fn file_exists_e47b0(m: &mut Machine, port: u32) -> u32 {
    m.push_frame(0x20);
    let result = if stop_monitor_e264c(m) == 0 {
        0
    } else {
        let c = card(m);
        let fd = bios::open(m, c.wrapping_add(name_of(port)), 0x2_0200);
        let c = card(m);
        m.sw(c.wrapping_add(0x3c), fd);
        if fd == u32::MAX {
            bios::close(m, u32::MAX);
            reset_ports_e25d4(m);
            u32::MAX
        } else {
            bios::close(m, fd);
            reset_ports_e25d4(m);
            1
        }
    };
    m.pop_frame(0x20);
    result
}

/// A failed check of the card monitor (`at` = card block + `port`):
/// below 0x11 retries the port stays not ready and the retry count grows,
/// after that it is marked ready.
fn retry_check(m: &mut Machine, at: u32, port: u32) {
    if m.lbu(at.wrapping_add(0x350)) < 0x11 {
        m.sb(at.wrapping_add(0x348), 0);
        let at = card(m).wrapping_add(port);
        let count = m.lbu(at.wrapping_add(0x350));
        m.sb(at.wrapping_add(0x350), count.wrapping_add(1));
    } else {
        m.sb(at.wrapping_add(0x348), 1);
    }
}

/// Switch the monitor to the other port.
fn other_port(m: &mut Machine) {
    let c = card(m);
    let port = m.lw(c.wrapping_add(0x354));
    m.sw(c.wrapping_add(0x354), 1u32.wrapping_sub(port));
}

/// 800E4858: one step of the card monitor of the polled port
/// (`[+0x354]`), by its state `[+0x34C + port]` (table 0x800CCDEC): 0
/// idle (next port), 1 start `_card_info`, 2 wait for it (0x12C ticks),
/// 3 evaluate (done: clear a new card or settle; timeout or error: retry;
/// new card: clear), 4 wait for the clear (busy-polled), 5 start
/// `_card_load`, 6 wait, 7 evaluate, 8 look for the save file.
pub fn monitor_e4858(m: &mut Machine) {
    m.push_frame(0x20);
    let c = card(m);
    let port = m.lw(c.wrapping_add(0x354));
    let state = m.lbu(c.wrapping_add(port).wrapping_add(0x34c));
    let at = |m: &mut Machine| card(m).wrapping_add(port);
    match state {
        0 => other_port(m),
        1 => {
            clear_sw_e2874(m);
            bios::card_info(m, port << 4);
            let a = at(m);
            m.sb(a.wrapping_add(0x34c), 2);
            let c = card(m);
            m.sw(c.wrapping_add(0x318), 0x12c);
        }
        2 => {
            let result = poll_sw_e2794(m, port, 3);
            let a = at(m);
            m.sb(a.wrapping_add(0x34e), result as u8);
            let c = card(m);
            let ticks = m.lw(c.wrapping_add(0x318));
            if (ticks as i32) < 0 {
                m.sb(c.wrapping_add(port).wrapping_add(0x34c), 1);
            } else {
                m.sw(c.wrapping_add(0x318), ticks.wrapping_sub(1));
            }
        }
        3 => {
            let c = card(m);
            let a = c.wrapping_add(port);
            let result = m.lbu(a.wrapping_add(0x34e));
            let half = c.wrapping_add(port << 1);
            m.sh(half.wrapping_add(0x2c), u16::from(result));
            match result {
                0 => {
                    if m.lbu(a.wrapping_add(0x348)) == 1 {
                        let enabled = m.lw(c.wrapping_add(0x28));
                        m.sb(a.wrapping_add(0x34c), u8::from(enabled != 0));
                        other_port(m);
                    } else {
                        clear_hw_e28e0(m);
                        bios::card_clear(m, port << 4);
                        let a = at(m);
                        m.sb(a.wrapping_add(0x34c), 4);
                    }
                }
                2 => {
                    retry_check(m, a, port);
                    let c = card(m);
                    let enabled = m.lw(c.wrapping_add(0x28));
                    m.sb(
                        c.wrapping_add(port).wrapping_add(0x34c),
                        u8::from(enabled != 0),
                    );
                    let c = card(m);
                    let p = m.lw(c.wrapping_add(0x354));
                    m.sw(c.wrapping_add(0x354), 1u32.wrapping_sub(p));
                    m.sb(c.wrapping_add(port).wrapping_add(0x34a), 0);
                }
                3 => {
                    m.sh(half.wrapping_add(0x2c), 0);
                    clear_hw_e28e0(m);
                    bios::card_clear(m, port << 4);
                    let a = at(m);
                    m.sb(a.wrapping_add(0x34c), 4);
                    let a = at(m);
                    m.sb(a.wrapping_add(0x348), 0);
                    let a = at(m);
                    m.sb(a.wrapping_add(0x350), 0);
                }
                1 => {
                    retry_check(m, a, port);
                    let c = card(m);
                    let enabled = m.lw(c.wrapping_add(0x28));
                    m.sb(
                        c.wrapping_add(port).wrapping_add(0x34c),
                        u8::from(enabled == 1),
                    );
                    other_port(m);
                }
                _ => {
                    let a = at(m);
                    m.sb(a.wrapping_add(0x348), 1);
                    let a = at(m);
                    m.sb(a.wrapping_add(0x34c), 1);
                    other_port(m);
                }
            }
        }
        4 => loop {
            let result = poll_hw_e294c(m, port, 5);
            let a = at(m);
            m.sb(a.wrapping_add(0x34e), result as u8);
            let a = at(m);
            if m.lbu(a.wrapping_add(0x34e)) != 0xff {
                break;
            }
        },
        5 => {
            let c = card(m);
            let result = m.lbu(c.wrapping_add(port).wrapping_add(0x34e));
            m.sh(
                c.wrapping_add(port << 1).wrapping_add(0x2c),
                u16::from(result),
            );
            clear_sw_e2874(m);
            bios::card_load(m, port << 4);
            let a = at(m);
            m.sb(a.wrapping_add(0x34c), 6);
        }
        6 => {
            let result = poll_sw_e2794(m, port, 7);
            let a = at(m);
            m.sb(a.wrapping_add(0x34e), result as u8);
        }
        7 => {
            let c = card(m);
            let a = c.wrapping_add(port);
            let result = m.lbu(a.wrapping_add(0x34e));
            m.sh(
                c.wrapping_add(port << 1).wrapping_add(0x30),
                u16::from(result),
            );
            m.sb(a.wrapping_add(0x348), 1);
            let c = card(m);
            let load = m.lh(c.wrapping_add(port << 1).wrapping_add(0x30));
            let a = c.wrapping_add(port);
            match load {
                0 => m.sb(a.wrapping_add(0x34c), 8),
                1 | 2 => {
                    retry_check(m, a, port);
                    let a = at(m);
                    m.sb(a.wrapping_add(0x34c), 1);
                }
                3 => {
                    m.sb(a.wrapping_add(0x348), 1);
                    let a = at(m);
                    m.sb(a.wrapping_add(0x34c), 1);
                    other_port(m);
                }
                _ => {}
            }
        }
        8 => {
            let c = card(m);
            let fd = bios::open(m, c.wrapping_add(name_of(port)), 0x8001);
            let c = card(m);
            m.sw(c.wrapping_add(0x3c), fd);
            if fd != u32::MAX {
                m.sb(c.wrapping_add(port).wrapping_add(0x34a), 1);
            }
            let c = card(m);
            let fd = m.lw(c.wrapping_add(0x3c));
            bios::close(m, fd);
            let a = at(m);
            m.sb(a.wrapping_add(0x34c), 1);
            let a = at(m);
            m.sb(a.wrapping_add(0x348), 1);
            other_port(m);
        }
        _ => {}
    }
    m.pop_frame(0x20);
}

/// 800E4DB0: list the files of the card in `port` ("bu%1d0:*.*"): the
/// names into the port's 15 directory slots, its free blocks (15 minus
/// the blocks of every file, +0x54) and file count (+0x58). Returns the
/// free blocks, or -1 while the monitor runs.
pub fn directory_e4db0(m: &mut Machine, port: u32) -> u32 {
    let frame = m.push_frame(0x68);
    let result = if stop_monitor_e264c(m) == 0 {
        u32::MAX
    } else {
        let pattern = frame + 0x10;
        let entry = frame + 0x20;
        let c = card(m);
        m.sh(c.wrapping_add(port << 1).wrapping_add(0x54), 0xf);
        call(m, SPRINTF, [pattern, DIRECTORY_PATTERN, port, 0]);
        let mut files = 0u32;
        if bios::first_file(m, pattern, entry) != 0 {
            let names = port.wrapping_mul(300).wrapping_add(0x5c);
            let mut slot = 0u32;
            loop {
                let c = card(m);
                call(
                    m,
                    STRCPY,
                    [c.wrapping_add(names).wrapping_add(slot), entry, 0, 0],
                );
                let size = m.lw(frame.wrapping_add(0x38));
                let c = card(m);
                let mut rounded = size.wrapping_add(0x1fff);
                if (rounded as i32) < 0 {
                    rounded = size.wrapping_add(0x3ffe);
                }
                let half = c.wrapping_add(port << 1);
                slot = slot.wrapping_add(0x14);
                files = files.wrapping_add(1);
                let free = m.lhu(half.wrapping_add(0x54));
                let blocks = ((rounded as i32) >> 13) as u32;
                m.sh(
                    half.wrapping_add(0x54),
                    (u32::from(free).wrapping_sub(blocks)) as u16,
                );
                if bios::next_file(m, entry) == 0 {
                    break;
                }
            }
        }
        let c = card(m);
        m.sh(c.wrapping_add(port << 1).wrapping_add(0x58), files as u16);
        reset_ports_e25d4(m);
        let c = card(m);
        m.lh(c.wrapping_add(port << 1).wrapping_add(0x54)) as i32 as u32
    };
    m.pop_frame(0x68);
    result
}

/// The start of a record transfer: the busy flag (+0x35A) set with the
/// retries (+0x358) cleared on the first call. Returns `[CARD]`.
fn begin_transfer(m: &mut Machine) -> u32 {
    let c = card(m);
    if m.lh(c.wrapping_add(0x35a)) == 0 {
        m.sh(c.wrapping_add(0x35a), 1);
        m.sh(c.wrapping_add(0x358), 0);
        return card(m);
    }
    c
}

/// A failed open or transfer: below three retries count one and report
/// "busy" (true); else false.
fn retry(m: &mut Machine, c: u32) -> bool {
    if m.lh(c.wrapping_add(0x358)) < 3 {
        let count = m.lhu(c.wrapping_add(0x358));
        m.sh(c.wrapping_add(0x358), count.wrapping_add(1));
        return true;
    }
    false
}

/// Open the save file of `port` with `mode` into +0x3C; the descriptor.
fn open_file(m: &mut Machine, c: u32, port: u32, mode: u32) -> u32 {
    let fd = bios::open(m, c.wrapping_add(name_of(port)), mode);
    let c = card(m);
    m.sw(c.wrapping_add(0x3c), fd);
    fd
}

/// Close the open file and clear the transfer flag.
fn close_file(m: &mut Machine) {
    let c = card(m);
    let fd = m.lw(c.wrapping_add(0x3c));
    bios::close(m, fd);
    let c = card(m);
    m.sb(c.wrapping_add(0x336), 0);
}

/// 800E4EF0: read the 0x80 record (at 0x280) of `port` into +0x2E8 and
/// check it ("3ECA" checksum and version 0x00990426). 0 busy, 1 read, 2
/// bad record, -1 failed; `finish` 1 resets the monitor after a read.
pub fn read_info_e4ef0(m: &mut Machine, port: u32, finish: u32) -> u32 {
    m.push_frame(0x20);
    let result = read_info(m, port, finish);
    m.pop_frame(0x20);
    result
}

fn read_info(m: &mut Machine, port: u32, finish: u32) -> u32 {
    if stop_monitor_e264c(m) == 0 {
        return 0;
    }
    let c = begin_transfer(m);
    if m.lbu(c.wrapping_add(0x336)) == 0 {
        let fd = open_file(m, c, port, 0x8001);
        if fd == u32::MAX {
            let c = card(m);
            if retry(m, c) {
                return 0;
            }
            reset_ports_e25d4(m);
            let c = card(m);
            m.sh(c.wrapping_add(0x35a), 0);
            return u32::MAX;
        }
        bios::lseek(m, fd, 0x280, 0);
        let c = card(m);
        let fd = m.lw(c.wrapping_add(0x3c));
        let buffer = m.lw(c.wrapping_add(0x2e8));
        bios::read(m, fd, buffer, 0x80);
        let c = card(m);
        m.sb(c.wrapping_add(0x336), 1);
    }
    let polled = poll_sw_e2794(m, port, 0);
    if polled == u32::MAX {
        return 0;
    }
    close_file(m);
    let c = card(m);
    let record = m.lw(c.wrapping_add(0x2e8));
    let sum = checksum(m, record, 0x1f);
    let c = card(m);
    let record = m.lw(c.wrapping_add(0x2e8));
    if m.lw(record) != sum || m.lw(record.wrapping_add(4)) != VERSION {
        reset_ports_e25d4(m);
        let c = card(m);
        m.sh(c.wrapping_add(0x35a), 0);
        return 2;
    }
    if polled != 0 {
        if retry(m, c) {
            return 0;
        }
        m.sh(c.wrapping_add(0x35a), 0);
        reset_ports_e25d4(m);
        return u32::MAX;
    }
    m.sh(c.wrapping_add(0x35a), 0);
    if finish == 1 {
        reset_ports_e25d4(m);
    }
    1
}

/// 800E5100: read the 0x480 records (at 0x300 + 0x480 n) of `port` that
/// the 0x80 record marks present, one per call by the index +0x50 (0..2)
/// into +0x2EC.. ("3ECA"-checked). 0 busy or next, 1 all read, 2 bad
/// record, -1 failed; `finish` 1 resets the monitor at the end.
pub fn read_slots_e5100(m: &mut Machine, port: u32, finish: u32) -> u32 {
    m.push_frame(0x20);
    let result = read_slots(m, port, finish);
    m.pop_frame(0x20);
    result
}

fn read_slots(m: &mut Machine, port: u32, finish: u32) -> u32 {
    if stop_monitor_e264c(m) == 0 {
        return 0;
    }
    let c = card(m);
    let info = m.lw(c.wrapping_add(0x2e8));
    let index = m.lh(c.wrapping_add(0x50)) as i32 as u32;
    let flags = u32::from(m.lbu(info.wrapping_add(8)));
    if ((flags as i32) >> (index & 31)) & 1 != 0 {
        if stop_monitor_e264c(m) == 0 {
            return 0;
        }
        let c = begin_transfer(m);
        if m.lbu(c.wrapping_add(0x336)) == 0 {
            let fd = open_file(m, c, port, 0x8001);
            let c = card(m);
            if fd == u32::MAX {
                if retry(m, c) {
                    return 0;
                }
                reset_ports_e25d4(m);
                return slots_failed(m, u32::MAX);
            }
            let index = m.lh(c.wrapping_add(0x50)) as i32 as u32;
            bios::lseek(m, fd, index.wrapping_mul(0x480).wrapping_add(0x300), 0);
            let c = card(m);
            let index = m.lh(c.wrapping_add(0x50)) as i32 as u32;
            let fd = m.lw(c.wrapping_add(0x3c));
            let buffer = m.lw(c.wrapping_add(index << 2).wrapping_add(0x2ec));
            bios::read(m, fd, buffer, 0x480);
            let c = card(m);
            m.sb(c.wrapping_add(0x336), 1);
        }
        let polled = poll_sw_e2794(m, port, 0);
        if polled == u32::MAX {
            return 0;
        }
        close_file(m);
        if polled != 0 {
            let c = card(m);
            if retry(m, c) {
                return 0;
            }
            reset_ports_e25d4(m);
            return slots_failed(m, u32::MAX);
        }
        let c = card(m);
        let index = m.lh(c.wrapping_add(0x50)) as i32 as u32;
        let record = m.lw(c.wrapping_add(index << 2).wrapping_add(0x2ec));
        let sum = checksum(m, record, 0x11f);
        let c = card(m);
        let index = m.lh(c.wrapping_add(0x50)) as i32 as u32;
        let record = m.lw(c.wrapping_add(index << 2).wrapping_add(0x2ec));
        if m.lw(record) != sum {
            reset_ports_e25d4(m);
            return slots_failed(m, 2);
        }
    }
    let c = card(m);
    let index = m.lh(c.wrapping_add(0x50));
    let raw = m.lhu(c.wrapping_add(0x50));
    if index < 2 {
        m.sh(c.wrapping_add(0x50), raw.wrapping_add(1));
        return 0;
    }
    m.sh(c.wrapping_add(0x50), 0);
    if finish == 1 {
        reset_ports_e25d4(m);
    }
    1
}

/// The end of a failed multi-record transfer: busy flag and index cleared.
fn slots_failed(m: &mut Machine, result: u32) -> u32 {
    let c = card(m);
    m.sh(c.wrapping_add(0x35a), 0);
    m.sh(c.wrapping_add(0x50), 0);
    result
}

/// 800E53A0: read the 0x180 records of `account` (at 0x1080 + 0x900
/// account + 0x180 n) of `port` that the account's 0x480 record marks
/// present (+0x16), one per call by the index +0x50 (0..6) into +0x2FC..
/// ("3ECA"-checked). Results as 800E5100.
pub fn read_records_e53a0(m: &mut Machine, port: u32, account: u32, finish: u32) -> u32 {
    m.push_frame(0x28);
    let result = read_records(m, port, account, finish);
    m.pop_frame(0x28);
    result
}

fn read_records(m: &mut Machine, port: u32, account: u32, finish: u32) -> u32 {
    if stop_monitor_e264c(m) == 0 {
        return 0;
    }
    let c = card(m);
    let slot = m.lw(c.wrapping_add(account << 2).wrapping_add(0x2ec));
    let index = m.lh(c.wrapping_add(0x50)) as i32 as u32;
    let flags = m.lh(slot.wrapping_add(0x16)) as i32;
    if (flags >> (index & 31)) & 1 != 0 {
        if stop_monitor_e264c(m) == 0 {
            return 0;
        }
        let c = begin_transfer(m);
        if m.lbu(c.wrapping_add(0x336)) == 0 {
            let fd = open_file(m, c, port, 0x8001);
            let c = card(m);
            if fd == u32::MAX {
                if retry(m, c) {
                    return 0;
                }
                m.sh(c.wrapping_add(0x35a), 0);
                reset_ports_e25d4(m);
                let c = card(m);
                m.sh(c.wrapping_add(0x50), 0);
                return u32::MAX;
            }
            let index = m.lh(c.wrapping_add(0x50)) as i32 as u32;
            let offset = index
                .wrapping_mul(0x180)
                .wrapping_add(account.wrapping_mul(0x900).wrapping_add(0x1080));
            bios::lseek(m, fd, offset, 0);
            let c = card(m);
            let index = m.lh(c.wrapping_add(0x50)) as i32 as u32;
            let fd = m.lw(c.wrapping_add(0x3c));
            let buffer = m.lw(c.wrapping_add(index << 2).wrapping_add(0x2fc));
            bios::read(m, fd, buffer, 0x180);
            let c = card(m);
            m.sb(c.wrapping_add(0x336), 1);
        }
        let polled = poll_sw_e2794(m, port, 0);
        if polled == u32::MAX {
            return 0;
        }
        close_file(m);
        if polled != 0 {
            let c = card(m);
            if retry(m, c) {
                return 0;
            }
            m.sh(c.wrapping_add(0x35a), 0);
            reset_ports_e25d4(m);
            let c = card(m);
            m.sh(c.wrapping_add(0x50), 0);
            return u32::MAX;
        }
        let c = card(m);
        let index = m.lh(c.wrapping_add(0x50)) as i32 as u32;
        let record = m.lw(c.wrapping_add(index << 2).wrapping_add(0x2fc));
        let sum = checksum(m, record, 0x5f);
        let c = card(m);
        let index = m.lh(c.wrapping_add(0x50)) as i32 as u32;
        let record = m.lw(c.wrapping_add(index << 2).wrapping_add(0x2fc));
        if m.lw(record) != sum {
            reset_ports_e25d4(m);
            return slots_failed(m, 2);
        }
    }
    let c = card(m);
    let index = m.lh(c.wrapping_add(0x50));
    let raw = m.lhu(c.wrapping_add(0x50));
    if index < 6 {
        m.sh(c.wrapping_add(0x50), raw.wrapping_add(1));
        return 0;
    }
    if finish == 1 {
        reset_ports_e25d4(m);
    }
    slots_failed(m, 1)
}

/// The end of a write: poll, close, then 0 busy, -1 failed (after three
/// retries) or 1 (`finish` 1 resets the monitor).
fn finish_write(m: &mut Machine, port: u32, finish: u32) -> u32 {
    let polled = poll_sw_e2794(m, port, 0);
    if polled == u32::MAX {
        return 0;
    }
    close_file(m);
    if polled != 0 {
        let c = card(m);
        if retry(m, c) {
            return 0;
        }
        m.sh(c.wrapping_add(0x35a), 0);
        reset_ports_e25d4(m);
        return u32::MAX;
    }
    if finish == 1 {
        reset_ports_e25d4(m);
    }
    1
}

/// 800E5664: write the 0x280-byte file head `[0x80112DEC]` from the start
/// of the save file of `port`. 0 busy, 1 written, -1 failed.
pub fn write_head_e5664(m: &mut Machine, port: u32, finish: u32) -> u32 {
    m.push_frame(0x20);
    let result = write_head(m, port, finish);
    m.pop_frame(0x20);
    result
}

fn write_head(m: &mut Machine, port: u32, finish: u32) -> u32 {
    if stop_monitor_e264c(m) == 0 {
        return 0;
    }
    let c = begin_transfer(m);
    if m.lbu(c.wrapping_add(0x336)) == 0 {
        let fd = open_file(m, c, port, 0x8002);
        if fd == u32::MAX {
            let c = card(m);
            if retry(m, c) {
                return 0;
            }
            reset_ports_e25d4(m);
            let c = card(m);
            m.sh(c.wrapping_add(0x35a), 0);
            return u32::MAX;
        }
        let head = m.lw(HEAD);
        bios::write(m, fd, head, 0x280);
        let c = card(m);
        m.sb(c.wrapping_add(0x336), 1);
    }
    finish_write(m, port, finish)
}

/// The failure of 800E57F0 / 800E5A28 / 800E5CA4 after their retries.
fn write_failed(m: &mut Machine) -> u32 {
    reset_ports_e25d4(m);
    let c = card(m);
    m.sh(c.wrapping_add(0x35a), 0);
    u32::MAX
}

/// The end of the writes of 800E57F0 / 800E5A28 / 800E5CA4.
fn finish_record_write(m: &mut Machine, port: u32, finish: u32) -> u32 {
    let polled = poll_sw_e2794(m, port, 0);
    if polled == u32::MAX {
        return 0;
    }
    close_file(m);
    if polled != 0 {
        let c = card(m);
        if retry(m, c) {
            return 0;
        }
        return write_failed(m);
    }
    if finish == 1 {
        reset_ports_e25d4(m);
    }
    1
}

/// 800E57F0: write the 0x80 record (at 0x280) to `port`, first marking
/// `slot` present (bit, count +0xC) or, for a negative slot, clearing
/// every mark; with its "3ECA" checksum. Results as 800E5664.
pub fn write_info_e57f0(m: &mut Machine, port: u32, slot: u32, finish: u32) -> u32 {
    m.push_frame(0x20);
    let result = write_info(m, port, slot, finish);
    m.pop_frame(0x20);
    result
}

fn write_info(m: &mut Machine, port: u32, slot: u32, finish: u32) -> u32 {
    if stop_monitor_e264c(m) == 0 {
        return 0;
    }
    let c = begin_transfer(m);
    if m.lbu(c.wrapping_add(0x336)) == 0 {
        if (slot as i32) < 0 {
            let info = m.lw(c.wrapping_add(0x2e8));
            m.sb(info.wrapping_add(8), 0);
            let c = card(m);
            let info = m.lw(c.wrapping_add(0x2e8));
            m.sw(info.wrapping_add(0xc), 0);
        } else {
            let info = m.lw(c.wrapping_add(0x2e8));
            let flags = m.lbu(info.wrapping_add(8));
            m.sb(info.wrapping_add(8), flags | (1u32 << (slot & 31)) as u8);
            let c = card(m);
            let info = m.lw(c.wrapping_add(0x2e8));
            let count = m.lw(info.wrapping_add(0xc));
            m.sw(info.wrapping_add(0xc), count.wrapping_add(1));
        }
        let c = card(m);
        let info = m.lw(c.wrapping_add(0x2e8));
        let sum = checksum(m, info, 0x1f);
        let c = card(m);
        let info = m.lw(c.wrapping_add(0x2e8));
        m.sw(info, sum);
        let fd = open_file(m, c, port, 0x8002);
        if fd == u32::MAX {
            let c = card(m);
            if retry(m, c) {
                return 0;
            }
            return write_failed(m);
        }
        bios::lseek(m, fd, 0x280, 0);
        let c = card(m);
        let fd = m.lw(c.wrapping_add(0x3c));
        let info = m.lw(c.wrapping_add(0x2e8));
        bios::write(m, fd, info, 0x80);
        let c = card(m);
        m.sb(c.wrapping_add(0x336), 1);
    }
    finish_record_write(m, port, finish)
}

/// 800E5A28: write the current 0x480 record (+0x2F8) as record `index`
/// (at 0x300 + 0x480 index) of `port`: first its slot field (+0x14) and
/// present marks (+0x16, from record `index` plus `slot` for a slot below
/// 6; none for slot 6), with its "3ECA" checksum. Results as 800E5664.
pub fn write_slot_e5a28(m: &mut Machine, port: u32, index: u32, slot: u32, finish: u32) -> u32 {
    m.push_frame(0x28);
    let result = write_slot(m, port, index, slot, finish);
    m.pop_frame(0x28);
    result
}

fn write_slot(m: &mut Machine, port: u32, index: u32, slot: u32, finish: u32) -> u32 {
    if stop_monitor_e264c(m) == 0 {
        return 0;
    }
    begin_transfer(m);
    if slot < 6 {
        let c = card(m);
        let current = m.lw(c.wrapping_add(0x2f8));
        m.sh(current.wrapping_add(0x14), slot as u16);
        let source = m.lw(c.wrapping_add(index << 2).wrapping_add(0x2ec));
        let current = m.lw(c.wrapping_add(0x2f8));
        let marks = m.lhu(source.wrapping_add(0x16));
        m.sh(current.wrapping_add(0x16), marks);
        let current = m.lw(c.wrapping_add(0x2f8));
        let marks = m.lhu(current.wrapping_add(0x16));
        m.sh(
            current.wrapping_add(0x16),
            marks | (1u32 << (slot & 31)) as u16,
        );
    } else if slot == 6 {
        let c = card(m);
        let current = m.lw(c.wrapping_add(0x2f8));
        m.sh(current.wrapping_add(0x14), 0);
        let current = m.lw(c.wrapping_add(0x2f8));
        m.sh(current.wrapping_add(0x16), 0);
    }
    let c = card(m);
    if m.lbu(c.wrapping_add(0x336)) == 0 {
        let current = m.lw(c.wrapping_add(0x2f8));
        let sum = checksum(m, current, 0x11f);
        let c = card(m);
        let current = m.lw(c.wrapping_add(0x2f8));
        m.sw(current, sum);
        let fd = open_file(m, c, port, 0x8002);
        if fd == u32::MAX {
            let c = card(m);
            if retry(m, c) {
                return 0;
            }
            return write_failed(m);
        }
        bios::lseek(m, fd, index.wrapping_mul(0x480).wrapping_add(0x300), 0);
        let c = card(m);
        let fd = m.lw(c.wrapping_add(0x3c));
        let current = m.lw(c.wrapping_add(0x2f8));
        bios::write(m, fd, current, 0x480);
        let c = card(m);
        m.sb(c.wrapping_add(0x336), 1);
    }
    finish_write(m, port, finish)
}

/// 800E5CA4: write the current 0x180 record (+0x314) as record `index` of
/// `account` (at 0x1080 + 0x900 account + 0x180 index) of `port`, with its
/// "3ECA" checksum. Results as 800E5664.
pub fn write_record_e5ca4(
    m: &mut Machine,
    port: u32,
    account: u32,
    index: u32,
    finish: u32,
) -> u32 {
    m.push_frame(0x28);
    let result = write_record(m, port, account, index, finish);
    m.pop_frame(0x28);
    result
}

fn write_record(m: &mut Machine, port: u32, account: u32, index: u32, finish: u32) -> u32 {
    if stop_monitor_e264c(m) == 0 {
        return 0;
    }
    let c = begin_transfer(m);
    if m.lbu(c.wrapping_add(0x336)) == 0 {
        let current = m.lw(c.wrapping_add(0x314));
        let sum = checksum(m, current, 0x5f);
        let c = card(m);
        let current = m.lw(c.wrapping_add(0x314));
        m.sw(current, sum);
        let fd = open_file(m, c, port, 0x8002);
        if fd == u32::MAX {
            let c = card(m);
            if retry(m, c) {
                return 0;
            }
            return write_failed(m);
        }
        let offset = index
            .wrapping_mul(0x180)
            .wrapping_add(account.wrapping_mul(0x900).wrapping_add(0x1080));
        bios::lseek(m, fd, offset, 0);
        let c = card(m);
        let fd = m.lw(c.wrapping_add(0x3c));
        let current = m.lw(c.wrapping_add(0x314));
        bios::write(m, fd, current, 0x180);
        let c = card(m);
        m.sb(c.wrapping_add(0x336), 1);
    }
    finish_record_write(m, port, finish)
}

/// 800E5EA0: copy the name `name` to the current record's (+8).
pub fn set_name_e5ea0(m: &mut Machine, name: u32) -> u32 {
    m.push_frame(0x18);
    let c = card(m);
    let current = m.lw(c.wrapping_add(0x2f8));
    let v0 = call(m, STRCPY, [current.wrapping_add(8), name, 0, 0]);
    m.pop_frame(0x18);
    v0
}

/// 800E5ED0: the debug screen dump: StoreImage2 of the 320x480 frame
/// buffer to 0x80200014 (a 0x14-byte TIM header before it: id 0x10, flag
/// 2, size 0x2580C, the rectangle), written to "sim:\usr\src\display.tim".
pub fn dump_screen_e5ed0(m: &mut Machine) {
    let frame = m.push_frame(0x20);
    let rect = frame + 0x10;
    for i in 0..8 {
        let b = m.lbu(DUMP_RECT.wrapping_add(i));
        m.sb(rect.wrapping_add(i), b);
    }
    let header = 0x8020_0000;
    m.sw(header, 0x10);
    m.sw(header.wrapping_add(4), 2);
    m.sw(header.wrapping_add(8), 0x2_580c);
    m.sw(header.wrapping_add(0xc), 0);
    m.sw(header.wrapping_add(0x10), 0x01e0_0140);
    m.sh(rect.wrapping_add(2), 0);
    call(m, STORE_IMAGE2, [rect, 0x8020_0014, 0x2_580c, 0x01e0_0140]);
    let fd = bios::open(m, DUMP_FILE, 0x202);
    bios::write(m, fd, 0x8020_0000, 0x4_b014);
    bios::close(m, fd);
    m.pop_frame(0x20);
}

/// The routines of this file, while module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    let [a0, a1, a2, a3] = args;
    Some(match target {
        0x800e_25d4 => reset_ports_e25d4(m),
        0x800e_264c => stop_monitor_e264c(m),
        0x800e_267c => status_e267c(m, a0),
        0x800e_2794 => poll_sw_e2794(m, a0, a1),
        0x800e_2874 => clear_sw_e2874(m),
        0x800e_28e0 => clear_hw_e28e0(m),
        0x800e_294c => poll_hw_e294c(m, a0, a1),
        0x800e_2a2c => context_word_e2a2c(m),
        0x800e_2a44 => name_to_context_e2a44(m),
        0x800e_2a7c => name_from_context_e2a7c(m),
        0x800e_3fcc => open_events_e3fcc(m),
        0x800e_415c => {
            monitor_off_e415c(m);
            0
        }
        0x800e_419c => close_events_e419c(m),
        0x800e_4760 => format_e4760(m, a0),
        0x800e_47b0 => file_exists_e47b0(m, a0),
        0x800e_4858 => {
            monitor_e4858(m);
            0
        }
        0x800e_4db0 => directory_e4db0(m, a0),
        0x800e_4ef0 => read_info_e4ef0(m, a0, a1),
        0x800e_5100 => read_slots_e5100(m, a0, a1),
        0x800e_53a0 => read_records_e53a0(m, a0, a1, a2),
        0x800e_5664 => write_head_e5664(m, a0, a1),
        0x800e_57f0 => write_info_e57f0(m, a0, a1, a2),
        0x800e_5a28 => write_slot_e5a28(m, a0, a1, a2, a3),
        0x800e_5ca4 => write_record_e5ca4(m, a0, a1, a2, a3),
        0x800e_5ea0 => set_name_e5ea0(m, a0),
        0x800e_5ed0 => {
            dump_screen_e5ed0(m);
            0
        }
        _ => return None,
    })
}
