//! The libcd command routine CD_cw (800A3F64,
//! `mainloop::pending_sdk::cd_command`), listed in
//! [`REPLACES`](super::REPLACES) for its timeout path (800A41C4), which the
//! generated translation left as `Fault::Untranslated`: a drive that does
//! not acknowledge a blocking command within 0x3C0 VBlanks (or 0x3C0000
//! polls) makes the original print the command and its status and flush
//! the drive (CD_flush, 800A43F8), and CD_cw returns -1 (its callers, as
//! CdInit's five attempts, retry). Part `resident-part2` (80025B00's
//! CdInit). The rest is the generated function's, with the private items
//! of `pending_sdk` it uses copied.
use crate::mainloop::pending_sdk::{
    CD_COMMAND, CD_DEBUG, CD_LAST_COMMAND, CD_LAST_MODE, CD_LAST_POSITION, CD_READY_STATUS,
    CD_SYNC_RESULT, CD_SYNC_STATUS, SITE_COMMAND, cd_interrupt_handler, cd_poll_interrupts,
};
use crate::{
    dispatch,
    machine::{Fault, Machine},
    sdk,
};

/// The [`CD_COMMAND`] result when no drive answers.
const NO_DRIVE: u32 = u32::MAX;
const COMMAND_NAMES: u32 = 0x800b_6280;
/// 800B6300: the names of the status bytes (4 bytes per status).
const STATUS_NAMES: u32 = 0x800b_6300;
const CLEARS_READY: u32 = 0x800b_63a0;
const PARAMETER_COUNTS: u32 = 0x800b_64a0;
const CD_TIMEOUT_NAME: u32 = 0x800c_7ee0;
/// BIOS puts (800AF1E8): a host call.
const PUTS: u32 = 0x800a_d618;
/// 800A43F8 CD_flush (`jp-sdk`).
const CD_FLUSH: u32 = 0x800a_43f8;

fn at(base: u32, offset: u32) -> u32 {
    base.wrapping_add(offset)
}

/// Run `body` in an original stack frame of `size` bytes (its base).
fn framed<R>(m: &mut Machine, size: u32, body: impl FnOnce(&mut Machine, u32) -> R) -> R {
    let frame = m.push_frame(size);
    let result = body(m, frame);
    m.pop_frame(size);
    result
}

/// 800A41C4, CD_cw's timeout (in its frame `sp`): puts(0x80024D00), then
/// printf(0x80024D10, the waiting routine's name ([800C7EE0]), the last
/// command's name, the sync status's name, and on the stack at `sp + 0x10`
/// the ready status's name), and CD_flush (800A43F8); CD_cw then returns
/// -1.
fn timeout(m: &mut Machine, sp: u32) {
    m.host_call(PUTS, [0x8002_4d00, 0, 0, 0]);
    let sync = u32::from(m.lbu(CD_SYNC_STATUS));
    let ready = u32::from(m.lbu(CD_SYNC_STATUS + 1));
    let name = m.lw(CD_TIMEOUT_NAME);
    let ready_name = m.lw(at(STATUS_NAMES, ready << 2));
    let command = u32::from(m.lbu(CD_LAST_COMMAND));
    let command_name = m.lw(at(COMMAND_NAMES, command << 2));
    m.sw(sp + 0x10, ready_name);
    let sync_name = m.lw(at(STATUS_NAMES, sync << 2));
    sdk::printf(m, [0x8002_4d10, name, command_name, sync_name]);
    dispatch::call(m, CD_FLUSH, [0; 4]);
}

/// While a blocking CD_sync (800A3A1C) waits for the current command in
/// the main program, the drive's interrupts arrive: run the handler
/// until the command status is complete (2) or error (5), or the drive
/// reports nothing. Inside a callback CD_sync polls the drive itself.
fn await_completion(m: &mut Machine) {
    while !matches!(m.lbu(CD_SYNC_STATUS), 2 | 5)
        && sdk::check_callback(m) == 0
        && m.fault().is_none()
    {
        if cd_interrupt_handler(m) == 0 {
            break;
        }
    }
}

/// 800A3F64 CD_cw(command, param*, result*, async): send `command` with
/// its parameter bytes (count from 0x800B64A0; a command that needs them
/// without `param` returns -2) after the previous command completed
/// (CD_sync(0, 0)). Setloc's position and Setmode's byte are recorded;
/// the sync status (and, for reads, the ready status) restarts at 0. With
/// `async` it returns 0 at once; otherwise it polls (deadline 0x3C0
/// VBlanks after VSync(-1), count at 0x800C7EDC) until the acknowledge,
/// copies the 8 sync result bytes to `result` and returns -1 on error,
/// else 0. A drive that does not acknowledge takes the timeout path
/// ([`timeout`], 800A41C4) and CD_cw returns -1.
///
/// JP (patched): the generated function raised `Fault::Untranslated` at
/// 800A41C4 there, which a release build would hit whenever the drive does
/// not answer.
pub fn cd_command(
    m: &mut Machine,
    command: u32,
    param: u32,
    result: u32,
    asynchronous: u32,
) -> u32 {
    framed(m, 0x38, |m, sp| {
        command_body(m, sp, command, param, result, asynchronous)
    })
}

fn command_body(
    m: &mut Machine,
    sp: u32,
    command: u32,
    param: u32,
    result: u32,
    asynchronous: u32,
) -> u32 {
    let code = command & 0xff;
    if (m.lw(CD_DEBUG) as i32) >= 2 {
        let name = m.lw(at(COMMAND_NAMES, code << 2));
        sdk::printf(m, [0x8002_4d9c, name, result, asynchronous]);
    }
    if m.lw(at(PARAMETER_COUNTS, code << 2)) != 0 && param == 0 {
        if (m.lw(CD_DEBUG) as i32) > 0 {
            let name = m.lw(at(COMMAND_NAMES, code << 2));
            sdk::printf(m, [0x8002_4da4, name, result, asynchronous]);
        }
        return -2i32 as u32;
    }
    await_completion(m);
    sdk::cd_sync(m, 0, 0);
    if code == 2 {
        for i in 0..4 {
            let byte = m.lbu(at(param, i));
            m.sb(at(CD_LAST_POSITION, i), byte);
        }
    }
    if code == 0xe {
        let mode = m.lbu(param);
        m.sb(CD_LAST_MODE, mode);
    }
    m.sb(CD_SYNC_STATUS, 0);
    if m.lw(at(CLEARS_READY, code << 2)) != 0 {
        m.sb(CD_READY_STATUS, 0);
    }
    let count = m.lw(at(PARAMETER_COUNTS, code << 2));
    let mut bytes = 0u32;
    for i in 0..(count as i32).max(0) as u32 {
        let byte = u32::from(m.lbu(at(param, i)));
        if i < 4 {
            bytes |= byte << (i * 8);
        }
    }
    m.sb(CD_LAST_COMMAND, code as u8);
    let polls = m.host_call_result(CD_COMMAND, [code, count, bytes, asynchronous], NO_DRIVE);
    if polls == NO_DRIVE {
        m.raise(Fault::Untranslated {
            address: /* JP CD_COMMAND: */ 0x800a_3f64,
        });
        return -1i32 as u32;
    }
    if asynchronous != 0 {
        return 0;
    }
    let now = sdk::vsync(m, -1i32 as u32);
    m.sw(sdk::CD_TIMEOUT_DEADLINE, now.wrapping_add(0x3c0));
    m.sw(sdk::CD_TIMEOUT_POLLS, 0);
    m.sw(CD_TIMEOUT_NAME, 0x8002_4db4);
    if m.lbu(CD_SYNC_STATUS) == 0 {
        let mut poll = 0u32;
        loop {
            let now = sdk::vsync(m, -1i32 as u32);
            let polled = m.lw(sdk::CD_TIMEOUT_POLLS);
            if (m.lw(sdk::CD_TIMEOUT_DEADLINE) as i32) < (now as i32) || {
                m.sw(sdk::CD_TIMEOUT_POLLS, polled.wrapping_add(1));
                (polled as i32) > 0x3c_0000
            } {
                timeout(m, sp);
                return -1i32 as u32;
            }
            poll += 1;
            let handled = if sdk::check_callback(m) != 0 {
                cd_poll_interrupts(m, SITE_COMMAND)
            } else if poll >= polls {
                cd_interrupt_handler(m)
            } else {
                1
            };
            if m.lbu(CD_SYNC_STATUS) != 0 {
                break;
            }
            if m.fault().is_some() {
                return -1i32 as u32;
            }
            if handled == 0 {
                // The drive never acknowledges: the original polls on until
                // the deadline or the poll limit and times out.
                timeout(m, sp);
                return -1i32 as u32;
            }
        }
    }
    if result != 0 {
        for i in 0..8 {
            let byte = m.lbu(at(CD_SYNC_RESULT, i));
            m.sb(at(result, i), byte);
        }
    }
    if m.lbu(CD_SYNC_STATUS) == 5 {
        -1i32 as u32
    } else {
        0
    }
}
