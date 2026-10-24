//! Resident main loop `800261A8` on the machine model: one pass of its loop
//! body (`800261D0`..`8002634C`) and every routine it runs around the state
//! callback. See `docs/FLIGHT_MACHINE.md`.
//!
//! Loop body, in original order: display requests 80025BC8, buffer swap
//! 80026050, pad polling 80026E84, pad input mapping 80028C24, sound
//! 8002E1F4, state dispatch 80026350 (the state callback), CD queue 8002C4DC,
//! cursor sprites 800278A8, `GetRCnt` / `DrawSync(0)` / `GetRCnt` /
//! `VSync(n)` / `VSync(-1)` / `ResetRCnt`, frame end 800260C0 (environment
//! swap and `DrawOTag`), then the context flag handling.
//!
//! # Host contract (one call of [`main_loop_iteration`])
//!
//! * `Machine::sp` is main's frame pointer (`0x801FFFE8` in the original:
//!   `800261A8` reserves 0x18 bytes below `0x80200000`).
//! * Interrupt-maintained RAM the SDK routines read: the VBlank counter
//!   0x800B6174 (libetc's VBlank handler; `VSync(-1)` is stored at context
//!   +0x20 and feeds the GPU/CD timeouts), libpad's port records, receive
//!   buffers and new-data flag ([`libpad::vblank`], libpad's VBlank handler
//!   translated; [`write_pad_buffers`] for tests that only need the buffers),
//!   and libcd's completion status byte 0x800B6538 / result bytes 0x800C7EC0
//!   (CdSync). The host updates them as the hardware interrupts would, e.g.
//!   from a [`Device`](crate::machine::Device) when `VSync(n)` waits.
//! * `Machine::host_results` (shared queue, call order; documented defaults
//!   when empty): ClearOTagR's DMA poll count; when a pad needs
//!   (re)detection, 8002685C's `PadGetState` / `PadInfoMode` / `PadInfoAct`
//!   results; 24 `SpuGetAllKeysStatus` voice states (0 off, 1 on, 2 off with
//!   a sounding envelope, 3 on with a silent one); the state callback's own
//!   results; `GetRCnt` (drawing start), DrawSync's poll count, `GetRCnt`
//!   (drawing end), `VSync(n)`'s root counter at entry, VBlank counter after
//!   the wait and root counter after it; the two SetIntrMask results of
//!   PutDrawEnv and of DrawOTag; DrawSync's poll count again when an arena
//!   reset requested it.
//! * `Machine::host_calls` then holds the GPU/SPU/CD/pad/timer requests in
//!   original order (SPU attribute, environment and pad actuator structures
//!   are guest pointers, as in the original ABI).
//! * CD: the request pump runs [`cdstream`](crate::cdstream)'s handlers,
//!   whose reads and streams install libcd callbacks (CdSyncCallback
//!   8002A200, CdReadyCallback 80029A44/80029D70); the CD monitor here
//!   (8002AC3C) removes them. libcd ([`pending_sdk`]) talks to the drive
//!   through the CD host protocol; the host runs libcd's interrupt handler
//!   when the drive interrupts (see `cdstream`'s host contract). Blocking
//!   libcd calls (CdControl/CdControlB) deliver the interrupts they wait
//!   for themselves.
//!
//! PsyQ routines that `sdk.rs` does not provide yet live in [`pending_sdk`]
//! with the original ABI; libpad's VBlank-time side is [`libpad`], which the
//! host runs at each delivered VBlank.

#[path = "libpad.rs"]
pub mod libpad;

use crate::{
    common::CONTEXT,
    dispatch,
    machine::{Fault, Machine, SmallData},
    script, sdk, shared,
};

/// A state-callback invoker: original address and a0..a3, returns v0.
/// Production code uses [`dispatch::call`]; snapshot tests substitute the
/// original's recorded callback effect.
pub type CallbackInvoker<'a> = dyn FnMut(&mut Machine, u32, [u32; 4]) -> u32 + 'a;

// ---- globals -------------------------------------------------------------------

/// `[gp+0x10]`: pad block. +0x00/+0x22 libpad receive buffers of ports 1/2,
/// +0x44 eight 0x10-byte controller records (port byte, setup phase,
/// state, pad state pointer +4, actuator alignment +8..+0xD, redetect +0xE),
/// +0xC4/+0xC8 mode table count/pointer, +0xCC vibration enable.
pub const PAD_BLOCK: u32 = 0x10;
/// `[gp+0x24]`: queued cursor sprites (count, then {xy, uv/clut} pairs).
const SPRITE_QUEUE: u32 = 0x24;
/// `[gp+0x30]`: six vibration slots {frames, kind, level, -}.
const VIBRATION_SLOTS: u32 = 0x30;
/// `[gp+0x34]`: CD monitor (+0 state, +4 step, +0xC error, +0x28 CdSync result).
const CD_MONITOR: u32 = 0x34;
/// `[gp+0x38]`: CD stream state (+0 flags, +4 sync hook, +0x30 retry count).
const CD_STREAM: u32 = 0x38;
/// `[gp+0x48]`: CD command queue (+0 read, +4 write, sixteen 0x14-byte
/// entries from +8, pending flag +0x148 with entry +0x14C, completion
/// argument +0x16C, current track +0x174).
const CD_QUEUE: u32 = 0x48;
/// `[gp+0x2D4]`: display request block (PAL 0x800BB0A8).
const DISPLAY_REQUESTS: u32 = 0x2d4;
/// `[gp+0x2E4]`: per-buffer table, stride 0x1014: +0 DRAWENV*, +4 DISPENV*,
/// +8..+0x10 three-entry ordering table, +0x14 primitive area.
const DRAW_BUFFERS: u32 = 0x2e4;
/// `gp+0x2F4`: last texture page word of the cursor sprites.
const SPRITE_TPAGE: u32 = 0x2f4;
/// `gp+0x324` / `gp+0x325`: sound sequence position / stopped position.
const SEQUENCE_POSITION: u32 = 0x324;
const SEQUENCE_STOPPED: u32 = 0x325;

/// Built-in environments: DRAWENV at +0, +0x5C, +0xB8; DISPENV at +0x114,
/// +0x128, +0x13C.
const ENVIRONMENTS: u32 = 0x800b_d0d0;
const BUFFER_STRIDE: u32 = 0x1014;
/// `GetRCnt`/`ResetRCnt` argument: root counter 1 (horizontal blanks).
const ROOT_COUNTER_1: u32 = 0xf200_0001;
/// 16-byte state records {callback table, module, load address, arena limit}.
const STATE_TABLE: u32 = 0x800b_0134;
/// Callbacks of the loading path (substate 0: 8009B6B8, 1: 8009B7CC).
const LOADING_CALLBACKS: u32 = 0x800b_4674;
/// CD command handlers indexed by entry byte 0.
const CD_COMMANDS: u32 = 0x800b_0390;
/// Two pad mode records {result, mode string} for 80026628.
const PAD_MODES: u32 = 0x800b_01c4;
/// Three actuator records {result, function, sub-function, size} for 800267F0.
const ACTUATOR_KINDS: u32 = 0x800b_01d4;
/// SpuCommonAttr used by 8002D80C/8002D844.
const SPU_COMMON: u32 = 0x800c_5800;
/// 24 voice records of 0x14 bytes: +0 sound, +1 request bits (1 key on,
/// 2 key off, 4 free, 0x10 volume update), +2 release delay, +3 in use,
/// +4/+6 volume, +8/+0xA initial volume, +0xC note, +0x10/+0x11 fade
/// target/level, +0x12 fade frames.
const VOICES: u32 = 0x800c_5830;
const VOICE_STRIDE: u32 = 0x14;
/// Eight delayed sounds {sound, -, frames}.
const DELAYED_SOUNDS: u32 = 0x800c_6550;
/// 32-step sound sequence {sound, voice, frames}.
const SOUND_SEQUENCE: u32 = 0x800c_6570;
const NO_VOICE: u32 = 0xff;

/// Context word 0 bits used by the loop.
pub mod flags {
    /// 800260C0: skip DrawOTag.
    pub const NO_DRAW: u32 = 4;
    /// DrawSync(0) after the frame (set by an arena reset).
    pub const DRAW_SYNC: u32 = 8;
    /// 800260C0: do not count this frame at +0x28 (cleared there).
    pub const SKIP_COUNT: u32 = 0x0200_0000;
    pub const PAUSE_REQUEST: u32 = 0x0400_0000;
    pub const RESUME_REQUEST: u32 = 0x0800_0000;
    /// Pause requests are discarded and the pause state cleared every frame.
    pub const PAUSE_LOCK: u32 = 0x1000_0000;
    pub const PAUSED: u32 = 0x2000_0000;
}

/// State callback result bits (80026350).
pub mod transition {
    pub const LOAD_COMPLETE: u32 = 0x0200_0000;
    pub const NEXT_SUBSTATE: u32 = 0x0400_0000;
    pub const NEXT_STATE: u32 = 0x0800_0000;
    pub const SET_SUBSTATE: u32 = 0x1000_0000;
    pub const SET_STATE: u32 = 0x2000_0000;
    pub const RESET_ARENA: u32 = 0x4000_0000;
}

/// Display request bits in word 0 of `[gp+0x2D4]` (80025BC8).
pub mod request {
    /// Drawing environments by phase (+4); requests SELECT_DISPLAY next frame.
    pub const SELECT_DRAW: u32 = 1;
    /// Screen x/y (+8/+0xC) into the three built-in DISPENVs.
    pub const SCREEN_OFFSET: u32 = 2;
    pub const CLEAR_OFF: u32 = 4;
    /// Background clear with RGB +0x10..+0x12.
    pub const CLEAR_ON: u32 = 8;
    /// Both buffers display the DISPENV at +0x18; then SELECT_DRAW.
    pub const CUSTOM_DISPLAY: u32 = 0x10;
    /// Both buffers draw with the DRAWENV at +0x2C; then SELECT_DRAW.
    pub const CUSTOM_DRAW: u32 = 0x20;
    /// Display environments by phase (+4).
    pub const SELECT_DISPLAY: u32 = 0x40;
}

fn voice(index: u32) -> u32 {
    VOICES.wrapping_add(index.wrapping_mul(VOICE_STRIDE))
}

/// Signed `div` quotient behind the compiler's `break 7` (zero divisor, at
/// `zero_break`) and `break 6` (`0x80000000 / -1`, at `overflow_break`)
/// checks: reaching a trap records `Fault::Trap` and yields the MIPS result.
pub(crate) fn checked_div(
    m: &mut Machine,
    n: i32,
    d: i32,
    zero_break: u32,
    overflow_break: u32,
) -> i32 {
    if d == 0 {
        m.raise(Fault::Trap {
            address: zero_break,
            code: 7,
        });
        return if n < 0 { 1 } else { -1 };
    }
    if d == -1 && n == i32::MIN {
        m.raise(Fault::Trap {
            address: overflow_break,
            code: 6,
        });
        return i32::MIN;
    }
    n / d
}

// ---- SDK routines missing from sdk.rs ---------------------------------------------

/// PsyQ routines this module and [`cdstream`](crate::cdstream) call that
/// `sdk.rs` does not provide yet (pending consolidation there), with the
/// original ABI.
///
/// * libpad (`PadGetState` .. `PadSetAct`): RAM-exact over libpad's port
///   records (0x800C7CB8, +0xF0 for the second port), reached through
///   libpad's function pointers 0x800B4698 (port record) and 0x800B46A0
///   (busy test) as the original does.
/// * libcd (`CdInit`, `CdReset`, callbacks, `CdControl`/`F`/`B`, `CD_cw`, the
///   interrupt parser and handler, `CdGetSector`/`2`, `CdIntToPos`): RAM-exact over
///   libcd's driver state (callbacks 0x800B625C/0x800B6260, drive status
///   0x800B6268/0x800B626C, last position/mode/command 0x800B6274..F9,
///   status bytes 0x800B6538..BA and results 0x800C7EC0/A0/A8, command
///   tables 0x800B61D4..0x800B64A0), with the drive behind the CD host
///   protocol below. `CdSearchFile`, `CdPosToInt`, `printf` and `rsin` are
///   `sdk.rs`'s.
/// * The MDEC status `DecDCTinSync(1)` is a host call.
///
/// # CD host protocol
///
/// The drive is the machine's [`Device`](crate::machine::Device) (for
/// example [`cdstream::CdDrive`](crate::cdstream::CdDrive)). It answers:
///
/// * [`CD_COMMAND`] `[command, parameter count, parameter bytes
///   (little-endian), async]`: the write of a command to the drive (CD_cw
///   800A3F64). Result: the status polls a blocking command (async 0) makes
///   before its interrupt arrives (0x800C7EDC keeps the count; at least 1).
///   A machine without a drive answers nothing: the command then raises
///   `Fault::Untranslated { address: 0x800A3F64 }` (no device behind libcd)
///   instead of waiting forever for an interrupt that never comes.
/// * [`CD_INTERRUPT`] `[site, frame, 0, 0]`: the reads of the interrupt
///   flag and response FIFO (800A34B8). The device writes the next
///   interrupt into 800A34B8's stack frame, where the original keeps what it
///   read: its kind at `frame + 0x10` (1 data ready, 2 complete, 3
///   acknowledge, 4 data end, 5 error; the translation clears it first, so
///   no device or no pending interrupt reads as 0), the response byte count
///   (at most 8) at `frame + 0x11` and the response bytes at `frame + 0x18`.
///   `site` is the polling loop ([`SITE_HANDLER`], [`SITE_SYNC`],
///   [`SITE_COMMAND`]). The poll never takes `Machine::host_results`, so a
///   host may poll where no drive is attached.
/// * [`CD_GET_SECTOR`] / [`CD_GET_SECTOR2`] `[destination, words]`: the
///   transfer of the next `words` words of the sector buffer; the device
///   writes them to RAM.
///
/// The host raises the CD interrupt by calling [`cd_interrupt_handler`]
/// (800A4B48, which libetc's dispatcher runs): it polls [`CD_INTERRUPT`]
/// until no interrupt is left, running the ready callback (0x800B6260) for
/// data ready, data end and errors and the sync callback (0x800B625C) for
/// completions and errors. The drive must report a sector it has ready and
/// a command response it has due at the next poll. Blocking libcd waits
/// (CD_cw without async; CdSync before CdControlB returns) call the handler
/// themselves at the point where the original's interrupt arrives during the
/// busy-wait. libetc's dispatcher bookkeeping (its in-callback flag
/// 0x800B50AE, kernel interrupt context) is not reproduced.
pub mod pending_sdk {
    use crate::{
        dispatch,
        machine::{Fault, Machine},
        sdk,
    };

    pub const PAD_GET_STATE: u32 = 0x8009_cdbc;
    pub const PAD_INFO_MODE: u32 = 0x8009_ce88;
    pub const PAD_INFO_ACT: u32 = 0x8009_cf80;
    pub const PAD_SET_ACT_ALIGN: u32 = 0x8009_d0fc;
    pub const PAD_SET_MAIN_MODE: u32 = 0x8009_d134;
    pub const PAD_SET_ACT: u32 = 0x8009_d17c;
    pub const CD_RESET: u32 = 0x800a_2ee4;
    pub const CD_SYNC_CALLBACK: u32 = 0x800a_14fc;
    pub const CD_READY_CALLBACK: u32 = 0x800a_1510;
    pub const CD_CONTROL: u32 = 0x800a_3054;
    pub const CD_CONTROL_F: u32 = 0x800a_3190;
    pub const CD_CONTROL_B: u32 = 0x800a_1794;
    pub const CD_MIX: u32 = 0x800a_18e0;
    pub const CD_GET_SECTOR_CALL: u32 = 0x800a_3430;
    pub const CD_GET_SECTOR2_CALL: u32 = 0x800a_3450;
    pub const CD_DATA_CALLBACK: u32 = 0x800a_1940;
    pub const CD_INTERRUPT_HANDLER: u32 = 0x800a_4b48;
    pub const CD_INT_TO_POS: u32 = 0x800a_5f68;
    pub const DEC_DCT_IN_SYNC: u32 = 0x800a_af08;

    // CD host protocol (host-call function codes: the original routines
    // whose device accesses they stand for).
    /// CD_cw's command write (800A3F64).
    pub const CD_COMMAND: u32 = 0x800a_2434;
    /// The interrupt flag read of 800A34B8.
    pub const CD_INTERRUPT: u32 = 0x800a_1988;
    /// CD_getsector's DMA (800A4950).
    pub const CD_GET_SECTOR: u32 = 0x800a_2e20;
    /// CD_getsector2's DMA (800A4A50).
    pub const CD_GET_SECTOR2: u32 = 0x800a_2f20;
    /// [`CD_INTERRUPT`] sites: the interrupt handler 800A4B48, CD_sync
    /// inside an interrupt callback (800A3B84), CD_cw waiting inside one
    /// (800A4260).
    pub const SITE_HANDLER: u32 = 0x800a_4b84;
    pub const SITE_SYNC: u32 = 0x800a_3b8c;
    pub const SITE_COMMAND: u32 = 0x800a_4268;

    // libcd RAM.
    pub const CD_SYNC_CALLBACK_SLOT: u32 = 0x800b_625c;
    pub const CD_READY_CALLBACK_SLOT: u32 = 0x800b_6260;
    /// libcd debug level (CdSetDebug).
    pub const CD_DEBUG: u32 = 0x800b_6264;
    /// Drive status byte of the last response (word) and its second byte.
    pub const CD_STATUS: u32 = 0x800b_6268;
    pub const CD_STATUS2: u32 = 0x800b_626c;
    /// Count of status responses that newly reported an open shell.
    pub const CD_SHELL_OPENED: u32 = 0x800b_6270;
    /// Last Setloc position (4 bytes), Setmode byte and command byte.
    pub const CD_LAST_POSITION: u32 = 0x800b_6274;
    pub const CD_LAST_MODE: u32 = 0x800b_6278;
    pub const CD_LAST_COMMAND: u32 = 0x800b_6279;
    const COMMAND_NAMES: u32 = 0x800b_6280;
    /// Per-command words: issue Setloc first when given a parameter
    /// (CdControl*), a second response follows the acknowledge, the command
    /// clears the ready status, the acknowledge updates the drive status,
    /// parameter byte count.
    const POSITION_COMMANDS: u32 = 0x800b_61d4;
    const SECOND_RESPONSE: u32 = 0x800b_6320;
    const ACK_STATUS: u32 = 0x800b_6420;
    /// Status bytes: command (2 complete, 3 acknowledged, 5 error; 0 sent),
    /// ready (1 data ready, 4 data end, 5 error) and data end.
    pub const CD_SYNC_STATUS: u32 = sdk::CD_SYNC_STATUS;
    pub const CD_READY_STATUS: u32 = 0x800b_6539;
    pub const CD_END_STATUS: u32 = 0x800b_653a;
    /// Response bytes (8 each) for the sync and ready callbacks and data end.
    pub const CD_SYNC_RESULT: u32 = sdk::CD_SYNC_RESULT;
    pub const CD_READY_RESULT: u32 = 0x800c_7ec8;
    pub const CD_END_RESULT: u32 = 0x800c_7ed0;
    /// libpad: port record function pointer, busy test pointer, records.
    const PAD_PORT_FUNCTION: u32 = 0x800b_4698;
    const PAD_BUSY_FUNCTION: u32 = 0x800b_46a0;
    const PAD_PORT_RECORD: u32 = 0x8009_e40c;
    const PAD_BUSY: u32 = 0x8009_e8b8;
    pub const PAD_PORTS: u32 = 0x800c_7cb8;

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

    // ---- libpad ---------------------------------------------------------------

    /// 8009E40C (libpad, through `[0x800B4698]`): the record of `port`
    /// (0x800C7CB8, or +0xF0 when `port & 0xF0`).
    pub fn pad_port_record(port: u32) -> u32 {
        if port & 0xf0 != 0 {
            PAD_PORTS + 0xf0
        } else {
            PAD_PORTS
        }
    }

    /// 8009E8B8 (through `[0x800B46A0]`): 0 when the port takes a request
    /// (`+0xE6` nonzero and `+0x46` = 0xFF), else 1.
    pub fn pad_busy(m: &mut Machine, record: u32) -> u32 {
        if m.lhu(at(record, 0xe6)) == 0 {
            return 1;
        }
        u32::from(m.lbu(at(record, 0x46)) != 0xff)
    }

    /// libpad's port-record routine through `[0x800B4698]`. A null pointer
    /// (a machine whose PadInitDirect 8009DDD0 ran as a host call, so the
    /// pointers it installs are missing) uses the routine PadInitDirect
    /// installs; the same holds for the busy test.
    fn port_record(m: &mut Machine, port: u32) -> u32 {
        let function = m.lw(PAD_PORT_FUNCTION);
        if function == PAD_PORT_RECORD || function == 0 {
            pad_port_record(port)
        } else {
            dispatch::call(m, function, [port, 0, 0, 0])
        }
    }

    fn port_busy(m: &mut Machine, record: u32) -> u32 {
        let function = m.lw(PAD_BUSY_FUNCTION);
        if function == PAD_BUSY || function == 0 {
            pad_busy(m, record)
        } else {
            dispatch::call(m, function, [record, 0, 0, 0])
        }
    }

    /// 8009CDBC PadGetState(port): the record's state `+0x49`, with 2 and 3
    /// reported as 1 and 6 as 4 while a request is in flight (`+0x37`,
    /// `+0x38`, `+0x39` for a sub-port, or `[+0x30]` nonzero).
    pub fn pad_get_state(m: &mut Machine, port: u32) -> u32 {
        framed(m, 0x18, |m, _| {
            let record = port_record(m, port);
            let idle = m.lbu(at(record, 0x37)) == 0
                && m.lbu(at(record, 0x38)) == 0
                && (m.lw(at(record, 0x10)) == record || m.lbu(at(record, 0x39)) == 0)
                && {
                    let pending = m.lw(at(record, 0x30));
                    m.lbu(pending) == 0
                };
            if !idle {
                match m.lbu(at(record, 0x49)) {
                    2 | 3 => return 1,
                    6 => return 4,
                    _ => {}
                }
            }
            u32::from(m.lbu(at(record, 0x49)))
        })
    }

    /// 8009CE88 PadInfoMode(port, term, offset): 1 controller ID `+0xE8`, 2
    /// extended ID `+0xE6`, 3 current mode `+0xE4`, 4 mode count `+0xE3`
    /// (`offset` < 0) or mode `offset` of the table at `[+0]`, 0x64 the word
    /// `+0x4C`; else 0.
    pub fn pad_info_mode(m: &mut Machine, port: u32, term: u32, offset: u32) -> u32 {
        framed(m, 0x20, |m, _| {
            let record = port_record(m, port);
            match term {
                1 => u32::from(m.lbu(at(record, 0xe8))),
                2 => u32::from(m.lhu(at(record, 0xe6))),
                3 => u32::from(m.lbu(at(record, 0xe4))),
                4 => {
                    let count = u32::from(m.lbu(at(record, 0xe3)));
                    if (offset as i32) < 0 {
                        count
                    } else if (offset as i32) < count as i32 {
                        let table = m.lw(record);
                        u32::from(m.lhu(at(table, offset << 1)))
                    } else {
                        0
                    }
                }
                0x64 => m.lw(at(record, 0x4c)),
                _ => 0,
            }
        })
    }

    /// 8009CF80 PadInfoAct(port, actuator, term): the actuator count `+0xE9`
    /// (`actuator` < 0), else byte `term - 1` (terms 1..=5) of the 5-byte
    /// record `actuator` in the table at `[+4]`; else 0.
    pub fn pad_info_act(m: &mut Machine, port: u32, actuator: u32, term: u32) -> u32 {
        framed(m, 0x20, |m, _| {
            let record = port_record(m, port);
            let count = u32::from(m.lbu(at(record, 0xe9)));
            if (actuator as i32) < 0 {
                return count;
            }
            if (actuator as i32) >= count as i32 {
                return 0;
            }
            let table = m.lw(at(record, 4));
            let entry = at(table, (actuator << 2).wrapping_add(actuator));
            let index = term.wrapping_sub(1);
            if index < 5 {
                u32::from(m.lbu(at(entry, index)))
            } else {
                0
            }
        })
    }

    /// 8009D0FC PadSetActAlign(port, data*) through 8009DA8C: when the port
    /// takes a request, `+0x46` = 1, request handlers 8009DAF4/8009DB10 at
    /// `+0x14`/`+0x18` and `+0x20` = `data`; returns 1, else 0.
    pub fn pad_set_act_align(m: &mut Machine, port: u32, data: u32) -> u32 {
        framed(m, 0x18, |m, _| {
            let record = port_record(m, port);
            framed(m, 0x20, |m, _| {
                if port_busy(m, record) != 0 {
                    return 0;
                }
                m.sb(at(record, 0x46), 1);
                m.sw(at(record, 0x14), 0x8009_daf4);
                m.sw(at(record, 0x20), data);
                m.sw(at(record, 0x18), 0x8009_db10);
                1
            })
        })
    }

    /// 8009D134 PadSetMainMode(port, offset, lock) through 8009DBD8: when
    /// the port takes a request, `+0x46` = 1, handlers 8009DC70/8009DCC4,
    /// `+0x51` = offset, `+0x52` = lock, `+0x53` = (offset == current mode
    /// `+0xE4`); returns 1, else 0.
    pub fn pad_set_main_mode(m: &mut Machine, port: u32, offset: u32, lock: u32) -> u32 {
        framed(m, 0x20, |m, _| {
            let record = port_record(m, port);
            let (offset, lock) = (offset & 0xff, lock & 0xff);
            framed(m, 0x28, |m, _| {
                if port_busy(m, record) != 0 {
                    return 0;
                }
                let current = u32::from(m.lbu(at(record, 0xe4)));
                m.sb(at(record, 0x46), 1);
                m.sw(at(record, 0x14), 0x8009_dc70);
                m.sw(at(record, 0x18), 0x8009_dcc4);
                m.sb(at(record, 0x51), offset as u8);
                m.sb(at(record, 0x52), lock as u8);
                m.sb(at(record, 0x53), u8::from(offset == current));
                1
            })
        })
    }

    /// 8009D17C PadSetAct(port, data*, length) through 8009D1D0: `+0x28` =
    /// data, `+0x34` = length (byte). Returns the record (the original v0).
    pub fn pad_set_act(m: &mut Machine, port: u32, data: u32, length: u32) -> u32 {
        framed(m, 0x20, |m, _| {
            let record = port_record(m, port);
            m.sw(at(record, 0x28), data);
            m.sb(at(record, 0x34), length as u8);
            record
        })
    }

    // ---- libcd: callbacks and conversions ---------------------------------------

    /// 800A302C CdSyncCallback(func) -> previous (0x800B625C).
    pub fn cd_sync_callback(m: &mut Machine, function: u32) -> u32 {
        let previous = m.lw(CD_SYNC_CALLBACK_SLOT);
        m.sw(CD_SYNC_CALLBACK_SLOT, function);
        previous
    }

    /// 800A3040 CdReadyCallback(func) -> previous (0x800B6260).
    pub fn cd_ready_callback(m: &mut Machine, function: u32) -> u32 {
        let previous = m.lw(CD_READY_CALLBACK_SLOT);
        m.sw(CD_READY_CALLBACK_SLOT, function);
        previous
    }

    /// 800A3470 CdDataCallback(func) -> previous: `DMACallback(3, func)`.
    pub fn cd_data_callback(m: &mut Machine, function: u32) -> u32 {
        framed(m, 0x18, |m, _| sdk::dma_callback(m, 3, function))
    }

    /// 800A5F68 CdIntToPos(i, p): BCD minute, second and sector of logical
    /// sector `i` (+150) at `p..p+3`; returns `p`.
    pub fn cd_int_to_pos(m: &mut Machine, sector: u32, position: u32) -> u32 {
        let bcd = |value: i32| ((value / 10) << 4).wrapping_add(value % 10) as u8;
        let absolute = (sector as i32).wrapping_add(150);
        let seconds = absolute / 75;
        let frames = absolute - seconds * 75;
        let minutes = seconds / 60;
        let seconds = seconds - minutes * 60;
        m.sb(at(position, 2), bcd(frames));
        m.sb(at(position, 1), bcd(seconds));
        m.sb(position, bcd(minutes));
        position
    }

    /// BIOS puts (800AF1E8) and DeliverEvent (800AF318): host calls (the
    /// kernel's events are the host's). libetc's ResetCallback and
    /// InterruptCallback are `sdk.rs`'s (RAM effects and host call).
    const PUTS: u32 = 0x800a_d618;
    const DELIVER_EVENT: u32 = 0x800a_d748;
    /// CD_initvol's device writes (SPU CD volume, CD audio mix 0x80/0/0x80/0).
    pub const CD_INIT_VOLUME: u32 = 0x800a_299c;

    /// 800A2D58 CdInit(): up to five attempts of the drive initialisation
    /// (800A2DE8: CD_init 800A4608, then CD_initvol 800A44CC). On success it
    /// installs the default callbacks (sync 800A2E24, ready 800A2E4C,
    /// 0x800B61C4 = 800A2E74 with 0x800B61C8 = 0) and returns 1; else prints
    /// and returns 0.
    pub fn cd_init(m: &mut Machine) -> u32 {
        framed(m, 0x18, |m, _| {
            for _ in 0..5 {
                let ready = framed(m, 0x18, |m, _| {
                    u32::from(cd_drive_init(m) == 0 && cd_init_volume(m) == 0)
                });
                if ready == 1 {
                    m.sw(CD_SYNC_CALLBACK_SLOT, 0x800a_2e24);
                    m.sw(CD_READY_CALLBACK_SLOT, 0x800a_2e4c);
                    m.sw(0x800b_61c4, 0x800a_2e74);
                    m.sw(0x800b_61c8, 0);
                    return 1;
                }
                if m.fault().is_some() {
                    return 0;
                }
            }
            sdk::printf(m, [0x8002_4b94, 0, 0, 0]);
            0
        })
    }

    /// 800A4608 CD_init: print, clear the last command and mode, both
    /// callbacks and the status words, ResetCallback and
    /// InterruptCallback(2, 800A4B48), drain the drive's interrupts (device
    /// only), status bytes ready/end 0 and sync 2, then Getstat (twice with
    /// the shell reported open), Init and Demute, each blocking, and
    /// CD_sync(0). Returns 0, or -1 on a failed command.
    pub fn cd_drive_init(m: &mut Machine) -> u32 {
        framed(m, 0x18, |m, _| {
            m.host_call(PUTS, [0x8002_4df4, 0, 0, 0]);
            sdk::printf(m, [0x8002_4e00, 0x800b_653c, 0, 0]);
            m.sb(CD_LAST_COMMAND, 0);
            m.sb(CD_LAST_MODE, 0);
            m.sw(CD_READY_CALLBACK_SLOT, 0);
            m.sw(CD_SYNC_CALLBACK_SLOT, 0);
            m.sw(CD_STATUS2, 0);
            m.sw(CD_STATUS, 0);
            sdk::reset_callback(m);
            sdk::interrupt_callback(m, 2, CD_INTERRUPT_HANDLER);
            m.sb(CD_END_STATUS, 0);
            let end = m.lbu(CD_END_STATUS);
            m.sb(CD_READY_STATUS, end);
            m.sb(CD_SYNC_STATUS, 2);
            cd_command(m, 1, 0, 0, 0);
            if m.lw(CD_STATUS) & 0x10 != 0 {
                cd_command(m, 1, 0, 0, 0);
            }
            if cd_command(m, 0xa, 0, 0, 0) != 0 || cd_command(m, 0xc, 0, 0, 0) != 0 {
                return u32::MAX;
            }
            await_completion(m);
            if sdk::cd_sync(m, 0, 0) == 2 {
                0
            } else {
                u32::MAX
            }
        })
    }

    /// 800A44CC CD_initvol: SPU CD volume and CD audio mix registers
    /// ([`CD_INIT_VOLUME`], device only). Returns 0.
    pub fn cd_init_volume(m: &mut Machine) -> u32 {
        m.host_call(CD_INIT_VOLUME, [0; 4]);
        0
    }

    /// 800A2E24 / 800A2E4C / 800A2E74: CdInit's default callbacks,
    /// DeliverEvent(0xF0000003, 0x20 / 0x40 / 0x40) (host calls).
    pub fn cd_default_callback(m: &mut Machine, event: u32) -> u32 {
        framed(m, 0x18, |m, _| {
            m.host_call(DELIVER_EVENT, [0xf000_0003, event, 0, 0]);
            0
        })
    }

    /// 800A2EE4 CdReset(mode): mode 2 reinstalls the interrupt handler
    /// (CD_initintr 800A45BC: clears both callbacks and the status words,
    /// ResetCallback and InterruptCallback(2, 800A4B48)) and returns 1;
    /// otherwise CD_init (0 on failure) and, for mode 1, CD_initvol; 1.
    pub fn cd_reset(m: &mut Machine, mode: u32) -> u32 {
        framed(m, 0x18, |m, _| {
            if mode == 2 {
                framed(m, 0x18, |m, _| {
                    m.sw(CD_READY_CALLBACK_SLOT, 0);
                    m.sw(CD_SYNC_CALLBACK_SLOT, 0);
                    m.sw(CD_STATUS2, 0);
                    m.sw(CD_STATUS, 0);
                    sdk::reset_callback(m);
                    sdk::interrupt_callback(m, 2, CD_INTERRUPT_HANDLER);
                });
                return 1;
            }
            if cd_drive_init(m) != 0 {
                return 0;
            }
            if mode == 1 && cd_init_volume(m) != 0 {
                return 0;
            }
            1
        })
    }

    /// 800A3430 CdGetSector(p, words) through CD_getsector 800A4950: the
    /// next `words` words of the sector buffer to `p` ([`CD_GET_SECTOR`];
    /// the device writes RAM). Returns 1.
    pub fn cd_get_sector(m: &mut Machine, destination: u32, words: u32) -> u32 {
        framed(m, 0x18, |m, _| {
            m.host_call(CD_GET_SECTOR, [destination, words, 0, 0]);
            1
        })
    }

    /// 800A3450 CdGetSector2(p, words) through CD_getsector2 800A4A50
    /// ([`CD_GET_SECTOR2`]). Returns 1.
    pub fn cd_get_sector2(m: &mut Machine, destination: u32, words: u32) -> u32 {
        framed(m, 0x18, |m, _| {
            m.host_call(CD_GET_SECTOR2, [destination, words, 0, 0]);
            1
        })
    }

    // ---- libcd: interrupts --------------------------------------------------------

    fn copy_response(m: &mut Machine, response: &[u8; 8], target: u32) {
        for (i, &byte) in response.iter().enumerate() {
            m.sb(at(target, i as u32), byte);
        }
    }

    /// 800A34B8: read and parse one CD interrupt ([`CD_INTERRUPT`]).
    /// Returns 0 when none is pending, else
    /// bit 2 = the ready callback is due, bit 1 = the sync callback is due
    /// (1: an acknowledge whose command has a second response).
    ///
    /// The status byte (first response byte) updates 0x800B6268/EC (not
    /// for the acknowledge of GetlocL/GetlocP) and counts a newly open shell;
    /// its error bits (0x1D) make the result an error. Acknowledge: sync
    /// status 3 when a second response follows, else 2 (5 on error);
    /// complete: 2 (5); data ready: ready status 1 (5, unless only the
    /// status byte came); data end: ready and end status 4; error: sync and
    /// ready status 5. Sync results go to 0x800C7EC0, ready results to
    /// 0x800C7EC8 (data end also to 0x800C7ED0). Kinds 6 and 7 print.
    pub fn cd_get_interrupt(m: &mut Machine, site: u32) -> u32 {
        framed(m, 0x30, |m, frame| get_interrupt(m, site, frame))
    }

    fn get_interrupt(m: &mut Machine, site: u32, frame: u32) -> u32 {
        m.sb(at(frame, 0x10), 0);
        m.sb(at(frame, 0x11), 0);
        m.host_call(CD_INTERRUPT, [site, frame, 0, 0]);
        let kind = u32::from(m.lbu(at(frame, 0x10)) & 7);
        if kind == 0 {
            return 0;
        }
        let count = u32::from(m.lbu(at(frame, 0x11))).min(8);
        // Bytes past the response are cleared, as 800A3578 does.
        for i in count..8 {
            m.sb(at(frame, 0x18 + i), 0);
        }
        let response: [u8; 8] = std::array::from_fn(|i| m.lbu(at(frame, 0x18 + i as u32)));
        let mut error = 0u8;
        let last = u32::from(m.lbu(CD_LAST_COMMAND));
        if kind != 3 || m.lw(at(ACK_STATUS, last << 2)) != 0 {
            if m.lw(CD_STATUS) & 0x10 == 0 && response[0] & 0x10 != 0 {
                let opened = m.lw(CD_SHELL_OPENED);
                m.sw(CD_SHELL_OPENED, opened.wrapping_add(1));
            }
            error = response[0] & 0x1d;
            m.sw(CD_STATUS, u32::from(response[0]));
            m.sw(CD_STATUS2, u32::from(response[1]));
        }
        if kind == 5 && (m.lw(CD_DEBUG) as i32) >= 3 {
            sdk::printf(m, [0x8002_4d2c, 0, 0, 0]);
            if (m.lw(CD_DEBUG) as i32) >= 3 {
                let last = u32::from(m.lbu(CD_LAST_COMMAND));
                let name = m.lw(at(COMMAND_NAMES, last << 2));
                let status = m.lw(CD_STATUS);
                let second = m.lw(CD_STATUS2);
                sdk::printf(m, [0x8002_4d38, name, status, second]);
            }
        }
        match kind {
            3 => {
                let last = u32::from(m.lbu(CD_LAST_COMMAND));
                if error != 0 {
                    m.sb(CD_SYNC_STATUS, 5);
                    copy_response(m, &response, CD_SYNC_RESULT);
                    2
                } else if m.lw(at(SECOND_RESPONSE, last << 2)) != 0 {
                    m.sb(CD_SYNC_STATUS, 3);
                    copy_response(m, &response, CD_SYNC_RESULT);
                    1
                } else {
                    m.sb(CD_SYNC_STATUS, 2);
                    copy_response(m, &response, CD_SYNC_RESULT);
                    2
                }
            }
            2 => {
                m.sb(CD_SYNC_STATUS, if error != 0 { 5 } else { 2 });
                copy_response(m, &response, CD_SYNC_RESULT);
                2
            }
            1 => {
                if error != 0 && count == 1 {
                    error = 0;
                }
                m.sb(CD_READY_STATUS, if error != 0 { 5 } else { 1 });
                copy_response(m, &response, CD_READY_RESULT);
                4
            }
            4 => {
                m.sb(CD_END_STATUS, 4);
                let end = m.lbu(CD_END_STATUS);
                m.sb(CD_READY_STATUS, end);
                copy_response(m, &response, CD_END_RESULT);
                copy_response(m, &response, CD_READY_RESULT);
                4
            }
            5 => {
                m.sb(CD_READY_STATUS, 5);
                let ready = m.lbu(CD_READY_STATUS);
                m.sb(CD_SYNC_STATUS, ready);
                copy_response(m, &response, CD_SYNC_RESULT);
                copy_response(m, &response, CD_READY_RESULT);
                6
            }
            _ => {
                // 800A39E8: puts and printf of the unknown interrupt.
                m.host_call(0x800a_d618, [0x8002_4d54, 0, 0, 0]);
                sdk::printf(m, [0x8002_4d68, kind, 0, 0]);
                0
            }
        }
    }

    /// The polling loop of 800A4B48 (also CD_sync's at 800A3B70 and CD_cw's
    /// at 800A4248 inside an interrupt callback): parse interrupts until
    /// none is left, calling the ready callback with (ready status,
    /// 0x800C7EC8) and the sync callback with (sync status, 0x800C7EC0) as
    /// they become due. Returns the number of interrupts handled.
    pub fn cd_poll_interrupts(m: &mut Machine, site: u32) -> u32 {
        let mut handled = 0;
        loop {
            let due = cd_get_interrupt(m, site);
            if due == 0 || m.fault().is_some() {
                return handled;
            }
            handled += 1;
            if due & 4 != 0 {
                let callback = m.lw(CD_READY_CALLBACK_SLOT);
                if callback != 0 {
                    let status = u32::from(m.lbu(CD_READY_STATUS));
                    dispatch::call(m, callback, [status, CD_READY_RESULT, 0, 0]);
                }
            }
            if due & 2 != 0 {
                let callback = m.lw(CD_SYNC_CALLBACK_SLOT);
                if callback != 0 {
                    let status = u32::from(m.lbu(CD_SYNC_STATUS));
                    dispatch::call(m, callback, [status, CD_SYNC_RESULT, 0, 0]);
                }
            }
        }
    }

    /// 800A4B48: libcd's CD interrupt handler (installed with
    /// InterruptCallback(2, ...)). The host calls it when the drive raises
    /// its interrupt (see the protocol above). Returns the number of
    /// interrupts handled (the original's v0 is meaningless).
    pub fn cd_interrupt_handler(m: &mut Machine) -> u32 {
        framed(m, 0x28, |m, _| cd_poll_interrupts(m, SITE_HANDLER))
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

    // ---- libcd: commands ------------------------------------------------------------

    /// 800A3F64 CD_cw(command, param*, result*, async): send `command` with
    /// its parameter bytes (count from 0x800B64A0; a command that needs them
    /// without `param` returns -2) after the previous command completed
    /// (CD_sync(0, 0)). Setloc's position and Setmode's byte are recorded;
    /// the sync status (and, for reads, the ready status) restarts at 0. With
    /// `async` it returns 0 at once; otherwise it polls (deadline 0x3C0
    /// VBlanks after VSync(-1), count at 0x800C7EDC) until the acknowledge,
    /// copies the 8 sync result bytes to `result` and returns -1 on error,
    /// else 0. The timeout path (800A41C4: print, CD_flush) is not
    /// translated.
    // JP-PATCH: replaced by `crate::jp::res_p2_mainloop::cd_command` (src/jp/mod.rs REPLACES, PATCHES.md)
    pub fn cd_command(
        m: &mut Machine,
        command: u32,
        param: u32,
        result: u32,
        asynchronous: u32,
    ) -> u32 {
        crate::jp::res_p2_mainloop::cd_command(m, command, param, result, asynchronous)
    }

    /// The shared retry loop of CdControl/CdControlF/CdControlB: up to four
    /// attempts with the sync callback disabled, a Getstat first when the
    /// shell was reported open (except for Getstat itself), Setloc first for
    /// commands that take a position, then the command with the callback
    /// restored. Returns whether the command was accepted.
    fn control(m: &mut Machine, command: u32, param: u32, result: u32, asynchronous: u32) -> bool {
        let code = command & 0xff;
        let saved = m.lw(CD_SYNC_CALLBACK_SLOT);
        let positional = at(POSITION_COMMANDS, code << 2);
        let setloc_result = if asynchronous != 0 { 0 } else { result };
        let command_result = if asynchronous != 0 { 0 } else { result };
        for _ in 0..4 {
            m.sw(CD_SYNC_CALLBACK_SLOT, 0);
            if code != 1 && m.lbu(CD_STATUS) & 0x10 != 0 {
                cd_command(m, 1, 0, 0, 0);
            }
            if param != 0 && m.lw(positional) != 0 && cd_command(m, 2, param, setloc_result, 0) != 0
            {
                continue;
            }
            m.sw(CD_SYNC_CALLBACK_SLOT, saved);
            if cd_command(m, code, param, command_result, asynchronous) == 0 {
                return true;
            }
            if m.fault().is_some() {
                break;
            }
        }
        m.sw(CD_SYNC_CALLBACK_SLOT, saved);
        false
    }

    /// 800A3054 CdControl(command, param*, result*): blocking command (waits
    /// for the acknowledge). Returns 1 when accepted, else 0.
    pub fn cd_control(m: &mut Machine, command: u32, param: u32, result: u32) -> u32 {
        framed(m, 0x38, |m, _| {
            u32::from(control(m, command, param, result, 0))
        })
    }

    /// 800A3190 CdControlF(command, param*): the command is sent without
    /// waiting (CD_cw async; a preceding Setloc still waits). Returns 1, or 0.
    pub fn cd_control_f(m: &mut Machine, command: u32, param: u32) -> u32 {
        framed(m, 0x38, |m, _| u32::from(control(m, command, param, 0, 1)))
    }

    /// 800A32C4 CdControlB(command, param*, result*): CdControl, then
    /// CD_sync(0, result) for the command's completion. Returns 1 when it
    /// completed (status 2), else 0.
    pub fn cd_control_b(m: &mut Machine, command: u32, param: u32, result: u32) -> u32 {
        framed(m, 0x38, |m, _| {
            if !control(m, command, param, result, 0) {
                return 0;
            }
            await_completion(m);
            u32::from(sdk::cd_sync(m, 0, result) == 2)
        })
    }

    // ---- device-level and other SDK routines ------------------------------------------

    /// 800ACAC8 DecDCTinSync(mode): mode 1 reads the MDEC busy bit (bit 24
    /// of the channel register `[0x800CBC80]`): host result (default 0,
    /// idle). Mode 0 waits (800ACDF8, not translated).
    pub fn dec_dct_in_sync(m: &mut Machine, mode: u32) -> u32 {
        framed(m, 0x18, |m, _| {
            if mode == 0 {
                m.raise(Fault::Untranslated {
                    address: 0x800a_cdf8,
                });
                return 0;
            }
            m.host_call_result(DEC_DCT_IN_SYNC, [mode, 0, 0, 0], 0) & 1
        })
    }

    /// Claims of the routines above by original address (a0..a3, v0).
    pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
        let [a0, a1, a2, a3] = args;
        Some(match target {
            0x8009_cdbc => pad_get_state(m, a0),
            0x8009_ce88 => pad_info_mode(m, a0, a1, a2),
            0x8009_cf80 => pad_info_act(m, a0, a1, a2),
            0x8009_d0fc => pad_set_act_align(m, a0, a1),
            0x8009_d134 => pad_set_main_mode(m, a0, a1, a2),
            0x8009_d17c => pad_set_act(m, a0, a1, a2),
            0x8009_e40c => pad_port_record(a0),
            0x8009_e8b8 => pad_busy(m, a0),
            0x800a_2ee4 => cd_reset(m, a0),
            0x800a_2d58 => cd_init(m),
            0x800a_4608 => cd_drive_init(m),
            0x800a_44cc => cd_init_volume(m),
            0x800a_2e24 => cd_default_callback(m, 0x20),
            0x800a_2e4c | 0x800a_2e74 => cd_default_callback(m, 0x40),
            0x800a_302c => cd_sync_callback(m, a0),
            0x800a_3040 => cd_ready_callback(m, a0),
            0x800a_3054 => cd_control(m, a0, a1, a2),
            0x800a_3190 => cd_control_f(m, a0, a1),
            0x800a_32c4 => cd_control_b(m, a0, a1, a2),
            0x800a_3430 => cd_get_sector(m, a0, a1),
            0x800a_3450 => cd_get_sector2(m, a0, a1),
            0x800a_3470 => cd_data_callback(m, a0),
            0x800a_34b8 => cd_get_interrupt(m, SITE_HANDLER),
            0x800a_3f64 => cd_command(m, a0, a1, a2, a3),
            0x800a_4b48 => {
                cd_interrupt_handler(m);
                0
            }
            0x800a_5f68 => cd_int_to_pos(m, a0, a1),
            0x800a_cac8 => dec_dct_in_sync(m, a0),
            _ => return None,
        })
    }
}

// ---- 80025BC8: display requests ------------------------------------------------------

/// 80025BC8: frame-start processing of the display request word at
/// `[gp+0x2D4]` (+4 phase, +8/+0xC screen x/y, +0x10..+0x12 clear RGB,
/// +0x18 custom DISPENV, +0x2C custom DRAWENV). Environment pointers go to
/// buffer 0 (`[gp+0x2E4]`) and buffer 1 (+0x1014): +0 DRAWENV*, +4 DISPENV*.
/// Phases other than 0/1 change no pointers.
pub fn display_requests(m: &mut Machine) {
    let status = m.gw(DISPLAY_REQUESTS);
    let flags = m.lw(status);
    if flags == 0 {
        return;
    }
    let envs = ENVIRONMENTS;
    let buffers = m.gw(DRAW_BUFFERS);
    if flags & request::SELECT_DISPLAY != 0 {
        let phase = m.lw(status + 4);
        m.sw(status, flags & !request::SELECT_DISPLAY);
        let pair = match phase {
            0 => Some((envs + 0x114, envs + 0x128)),
            1 => Some((envs + 0x13c, envs + 0x13c)),
            _ => None,
        };
        if let Some((first, second)) = pair {
            m.sw(buffers + 4, first);
            m.sw(buffers + BUFFER_STRIDE + 4, second);
        }
    }
    let flags = m.lw(status);
    if flags & request::SELECT_DRAW != 0 {
        let phase = m.lw(status + 4);
        m.sw(
            status,
            (flags & !request::SELECT_DRAW) | request::SELECT_DISPLAY,
        );
        let pair = match phase {
            0 => Some((envs, envs + 0x5c)),
            1 => Some((envs + 0xb8, envs + 0xb8)),
            _ => None,
        };
        if let Some((first, second)) = pair {
            m.sw(buffers, first);
            m.sw(buffers + BUFFER_STRIDE, second);
        }
    }
    let flags = m.lw(status);
    if flags & request::CUSTOM_DISPLAY != 0 {
        m.sw(status, flags & !request::CUSTOM_DISPLAY);
        m.sw(buffers + 4, status + 0x18);
        m.sw(buffers + BUFFER_STRIDE + 4, status + 0x18);
        let flags = m.lw(status);
        m.sw(status, flags | request::SELECT_DRAW);
    }
    let flags = m.lw(status);
    if flags & request::CUSTOM_DRAW != 0 {
        m.sw(status, flags & !request::CUSTOM_DRAW);
        m.sw(buffers, status + 0x2c);
        m.sw(buffers + BUFFER_STRIDE, status + 0x2c);
        let flags = m.lw(status);
        m.sw(status, flags | request::SELECT_DRAW);
    }
    let flags = m.lw(status);
    if flags & request::SCREEN_OFFSET != 0 {
        let x = m.lhu(status + 8);
        m.sw(status, flags & !request::SCREEN_OFFSET);
        m.sh(envs + 0x11c, x);
        let y = m.lhu(status + 0xc);
        m.sh(envs + 0x11e, y);
        for display in [envs + 0x128, envs + 0x13c] {
            let x = m.lhu(status + 8);
            m.sh(display + 8, x);
            let y = m.lhu(status + 0xc);
            m.sh(display + 0xa, y);
        }
    }
    let mut flags = m.lw(status);
    if flags & request::CLEAR_OFF != 0 {
        m.sw(status, flags & !request::CLEAR_OFF);
        for draw in [envs + 0xb8, envs + 0x5c, envs] {
            m.sb(draw + 0x18, 0);
        }
        flags = m.lw(status);
    }
    if flags & request::CLEAR_ON != 0 {
        let r = m.lbu(status + 0x10);
        let g = m.lbu(status + 0x11);
        m.sw(status, flags & !request::CLEAR_ON);
        let b = m.lbu(status + 0x12);
        for draw in [envs + 0xb8, envs + 0x5c, envs] {
            m.sb(draw + 0x18, 1);
        }
        for draw in [envs, envs + 0x5c, envs + 0xb8] {
            m.sb(draw + 0x19, r);
            m.sb(draw + 0x1a, g);
            m.sb(draw + 0x1b, b);
        }
    }
}

// ---- 80026050 / 800260C0: buffers ---------------------------------------------------

/// 80026050: flip the buffer index (context +4), point context +8 / +0xC /
/// +0x10 at entries 1 / 0 / 2 of the new buffer's three-entry ordering table
/// and +0x14 at its primitive area, then `ClearOTagR(table, 3)`; returns its
/// result.
pub fn swap_buffers(m: &mut Machine) -> u32 {
    let context = m.gw(CONTEXT);
    let index = m.lw(context + 4) ^ 1;
    m.sw(context + 4, index);
    let buffer = m
        .gw(DRAW_BUFFERS)
        .wrapping_add(index.wrapping_mul(BUFFER_STRIDE));
    m.sw(context + 8, buffer.wrapping_add(0xc));
    m.sw(context + 0xc, buffer.wrapping_add(8));
    m.sw(context + 0x10, buffer.wrapping_add(0x10));
    m.sw(context + 0x14, buffer.wrapping_add(0x14));
    sdk::clear_otag_r(m, buffer.wrapping_add(8), 3)
}

/// 800260C0: frame end. PutDispEnv/PutDrawEnv of the current buffer, then
/// DrawOTag of its table (from entry 2) unless context bit 2 is set. Frame
/// counters: +0x28 while not paused (a set SKIP_COUNT skips one and is
/// cleared), +0x24 always. Returns the new +0x24.
pub fn frame_end(m: &mut Machine) -> u32 {
    let context = m.gw(CONTEXT);
    let index = m.lw(context + 4);
    let buffer = m
        .gw(DRAW_BUFFERS)
        .wrapping_add(index.wrapping_mul(BUFFER_STRIDE));
    let display = m.lw(buffer + 4);
    sdk::put_disp_env(m, display);
    let draw = m.lw(buffer);
    sdk::put_draw_env(m, draw);
    let context = m.gw(CONTEXT);
    if m.lw(context) & flags::NO_DRAW == 0 {
        // Widescreen (host-side, off by default): the overlay squeezed for
        // the GPU, then put back as the game wrote it.
        let undo = crate::widescreen::before_draw(m, buffer.wrapping_add(0x10), draw, index);
        sdk::draw_otag(m, buffer.wrapping_add(0x10));
        crate::widescreen::after_draw(m, undo);
    }
    let mut context = m.gw(CONTEXT);
    let word = m.lw(context);
    if word & flags::PAUSED == 0 {
        if word & flags::SKIP_COUNT == 0 {
            let counted = m.lw(context + 0x28);
            m.sw(context + 0x28, counted.wrapping_add(1));
        } else {
            m.sw(context, word & !flags::SKIP_COUNT);
        }
        context = m.gw(CONTEXT);
    }
    let frames = m.lw(context + 0x24).wrapping_add(1);
    m.sw(context + 0x24, frames);
    frames
}

// ---- 800278A8: cursor sprites -----------------------------------------------------------

/// 800278A8: link the queued cursor sprites (`[gp+0x24]`) as 6x6 SPRT
/// packets (code 0x64, colour 0x808080) at `packet` into ordering-table
/// entry `ot`, then a DR_TPAGE (E1000015); clears the queue. Returns the
/// next free primitive address.
pub fn link_sprites(m: &mut Machine, ot: u32, packet: u32) -> u32 {
    let queue = m.gw(SPRITE_QUEUE);
    let count = m.lw(queue) as i32;
    let mut entry = queue.wrapping_add(4);
    let mut packet = packet;
    for _ in 0..count {
        m.sw(packet.wrapping_add(4), 0x6480_8080);
        let xy = m.lw(entry);
        m.sw(packet.wrapping_add(8), xy);
        let uv = m.lw(entry.wrapping_add(4));
        entry = entry.wrapping_add(8);
        m.sw(packet.wrapping_add(0x10), 0x0006_0006);
        m.sw(packet.wrapping_add(0xc), uv);
        let link = m.lw(ot);
        m.sw(packet, link | 0x0400_0000);
        m.sw(ot, (packet << 8) >> 8);
        packet = packet.wrapping_add(0x14);
    }
    m.sb(packet.wrapping_add(3), 1);
    m.sw(packet.wrapping_add(4), 0xe100_0015);
    let tag = m.lw(packet);
    let link = m.lw(ot);
    m.set_gw(SPRITE_TPAGE, 0x0010_0010);
    m.sw(packet, (tag & 0xff00_0000) | (link & 0x00ff_ffff));
    let link = m.lw(ot);
    let queue = m.gw(SPRITE_QUEUE);
    m.sw(ot, (link & 0xff00_0000) | (packet & 0x00ff_ffff));
    m.sw(queue, 0);
    packet.wrapping_add(8)
}

// ---- 80026E84: pad polling -----------------------------------------------------------------

/// Bytes of one libpad receive buffer.
pub const PAD_BUFFER_BYTES: usize = 0x22;
/// libpad's new-data flag, set by its VSync handler (PADMAIN 8009BAFC),
/// read and cleared by PadChkVsync.
pub const PAD_VSYNC_FLAG: u32 = 0x800b_46ec;

/// Host side of libpad's VSync receive: store both ports' raw receive
/// buffers (byte 0 status, 0 = valid / 0xFF = no controller; byte 1 type
/// and halfword count, e.g. 0x41 digital, 0x73 DualShock analog, 0x80 in
/// byte 1 for a multitap; then the data, buttons active low) and set the
/// new-data flag.
pub fn write_pad_buffers(
    m: &mut Machine,
    port1: &[u8; PAD_BUFFER_BYTES],
    port2: &[u8; PAD_BUFFER_BYTES],
) {
    let block = m.gw(PAD_BLOCK);
    m.write_bytes(block, port1);
    m.write_bytes(block + PAD_BUFFER_BYTES as u32, port2);
    m.sw(PAD_VSYNC_FLAG, 1);
}

/// Receive buffer of a digital pad (type 0x41) with `buttons` held
/// (active-high libpad bit order: bit 0 SELECT .. bit 15 SQUARE).
pub fn digital_pad(buttons: u16) -> [u8; PAD_BUFFER_BYTES] {
    let mut buffer = [0; PAD_BUFFER_BYTES];
    buffer[1] = 0x41;
    buffer[2..4].copy_from_slice(&(!buttons).to_le_bytes());
    buffer
}

/// Receive buffer of an empty port.
pub fn no_pad() -> [u8; PAD_BUFFER_BYTES] {
    let mut buffer = [0; PAD_BUFFER_BYTES];
    buffer[0] = 0xff;
    buffer
}

/// 80026E84: when libpad has new data, update the eight controller records
/// from the two receive buffers (four sub-buffers each; a single pad marks
/// sub-buffers 2..4 empty with 0xFFFF).
pub fn poll_pads(m: &mut Machine) {
    if sdk::pad_chk_vsync(m) == 0 {
        return;
    }
    let block = m.gw(PAD_BLOCK);
    poll_port(m, block, 0);
    let block = m.gw(PAD_BLOCK);
    poll_port(m, block + PAD_BUFFER_BYTES as u32, 4);
}

fn poll_port(m: &mut Machine, buffer: u32, first: u32) {
    let head = m.lhu(buffer);
    let multitap = head & 0xff == 0 && head >> 8 == 0x80;
    let start = if multitap { buffer + 2 } else { buffer };
    for i in 0..4 {
        let raw = start + i * 8;
        if !multitap && i > 0 {
            m.sh(raw, 0xffff);
        }
        let record = m.gw(PAD_BLOCK) + 0x44 + (first + i) * 0x10;
        pad_update(m, raw, record);
    }
}

/// 80026BA4: update one controller record from its receive sub-buffer
/// `raw`. Pad state (`[record+4]`): +0..+7 actuator bytes, +0xA mode id,
/// +0xC type, +0xD event (1 lost, 2 detecting, 4 mode changed, 8 removed),
/// +0xE held buttons, +0x10 newly pressed, +0x12..+0x18 analog/pointer.
pub fn pad_update(m: &mut Machine, raw: u32, record: u32) {
    let state = m.lw(record + 4);
    let mut head = u32::from(m.lhu(raw));
    let kind = m.lbu(state + 0xc);
    let data = raw + 2;
    m.sb(state + 0xd, 0);
    if kind == 0 && head & 0xff == 0 {
        if pad_setup(m, record) == 0 {
            head = 0;
            m.sb(state + 0xd, 2);
        }
    } else if m.lbu(record + 0xe) != 0 {
        if pad_setup(m, record) != 0 {
            m.sb(record + 0xe, 0);
        } else {
            m.sb(state + 0xd, 2);
        }
    }
    let head = head & 0xffff;
    if head & 0xff != 0 {
        let kind = m.lbu(state + 0xc);
        m.sb(state + 0xd, 1);
        if kind != 0 {
            m.sb(state + 0xd, 8);
        }
        m.sb(record + 1, 0);
        m.sb(state, 0);
        m.sb(state + 1, 0);
        for offset in [7, 6, 5, 4, 3, 2] {
            m.sb(state + offset, 0);
        }
        m.sh(state + 8, 0);
        m.sh(state + 0xa, 0);
        m.sb(state + 0xc, 0);
        m.sh(state + 0x10, 0);
        m.sh(state + 0xe, 0);
        clear_pad_axes(m, state);
        return;
    }
    let mut kind = head >> 8;
    if m.lbu(state + 0xc) != 0 && m.lhu(state + 0xa) != 0 {
        let port = u32::from(m.lbu(record));
        pending_sdk::pad_get_state(m, port);
        let port = u32::from(m.lbu(record));
        let mode = pending_sdk::pad_info_mode(m, port, 2, 0);
        if mode != u32::from(m.lhu(state + 0xa)) {
            kind = 0;
            m.sb(state + 0xd, 4);
            m.sb(record + 1, 1);
            m.sb(record + 0xe, 1);
        }
    }
    m.sb(state + 0xc, kind as u8);
    let buttons = !u32::from(m.lhu(data));
    let released = !u32::from(m.lhu(state + 0xe));
    m.sh(state + 0xe, buttons as u16);
    m.sh(state + 0x10, (buttons & released) as u16);
    let data = data + 2;
    match kind & 0xffff {
        0x23 | 0x53 | 0x73 => {
            let first = m.lhu(data);
            let second = m.lhu(data + 2);
            m.sh(state + 0x12, first >> 8);
            m.sh(state + 0x14, first & 0xff);
            m.sh(state + 0x16, second >> 8);
            m.sh(state + 0x18, second & 0xff);
        }
        0x12 => {
            // Mouse: signed byte deltas, accumulated at +0x16/+0x18.
            let motion = m.lhu(data);
            let dx = (motion >> 8) as u8 as i8;
            let dy = motion as u8 as i8;
            m.sh(state + 0x12, dx as i16 as u16);
            let x = m.lhu(state + 0x12);
            let total_y = m.lhu(state + 0x18);
            m.sh(state + 0x14, dy as i16 as u16);
            let total_x = m.lhu(state + 0x16);
            let y = m.lhu(state + 0x14);
            m.sh(state + 0x16, total_x.wrapping_add(x));
            m.sh(state + 0x18, total_y.wrapping_add(y));
        }
        0x63 => {
            let x = m.lhu(data);
            let y = m.lhu(data + 2);
            m.sh(state + 0x18, 0);
            m.sh(state + 0x16, 0);
            m.sh(state + 0x12, x);
            m.sh(state + 0x14, y);
        }
        0xe3 => {
            let position = m.lhu(data);
            let second = m.lhu(data + 2);
            let previous = m.lhu(state + 0x12);
            m.sh(state + 0x18, 0);
            m.sh(state + 0x16, position.wrapping_sub(previous));
            m.sh(state + 0x12, position);
            m.sh(state + 0x14, second);
        }
        _ => clear_pad_axes(m, state),
    }
}

fn clear_pad_axes(m: &mut Machine, state: u32) {
    for offset in [0x18, 0x16, 0x14, 0x12] {
        m.sh(state + offset, 0);
    }
}

/// 8002685C: libpad detection and actuator setup of one controller record,
/// stepped by its phase byte (+1): 0 select the main mode (or, for a pad
/// found without extended modes, send its actuator bytes), 1 read the
/// actuator table and align it, 2 start the actuators. Returns the pad's
/// current id once set up, else 0.
pub fn pad_setup(m: &mut Machine, record: u32) -> u32 {
    let frame = m.push_frame(0x48);
    let result = pad_setup_frame(m, record, frame);
    m.pop_frame(0x48);
    result
}

fn pad_setup_frame(m: &mut Machine, record: u32, frame: u32) -> u32 {
    let port = u32::from(m.lbu(record));
    let state = pending_sdk::pad_get_state(m, port);
    let id = pending_sdk::pad_info_mode(m, port, 1, 0);
    let extended = pending_sdk::pad_info_mode(m, port, 2, 0);
    m.sb(record + 2, state as u8);
    if state == 0 {
        let pad = m.lw(record + 4);
        m.sb(pad, 0);
        let pad = m.lw(record + 4);
        m.sb(pad + 1, 0);
        let pad = m.lw(record + 4);
        m.sh(pad + 0xa, 0);
        m.sb(record + 1, 0);
        return 0;
    }
    match m.lbu(record + 1) {
        0 => {
            if state == 2 {
                let mode = pad_mode_index(id);
                let pad = m.lw(record + 4);
                m.sb(pad, mode as u8);
                let block = m.gw(PAD_BLOCK);
                let vibration = m.lbu(block + 0xcc) & 1;
                let pad = m.lw(record + 4);
                m.sb(pad + 1, vibration);
                let pad = m.lw(record + 4);
                pending_sdk::pad_set_act(m, port, pad + 2, 2);
                let pad = m.lw(record + 4);
                m.sh(pad + 0xa, extended as u16);
                return id;
            }
            if state != 6 {
                return 0;
            }
            let count = pending_sdk::pad_info_mode(m, port, 4, 0xffff_ffff);
            let table = frame + 0x10;
            for i in (0..8).rev() {
                m.sh(table + i * 2, 0);
            }
            let mut i = 0;
            while (i as i32) < count as i32 && i < 7 {
                let mode = pending_sdk::pad_info_mode(m, port, 4, i);
                m.sh(table + i * 2, mode as u16);
                i += 1;
            }
            let selected = pad_mode_lookup(m, table);
            let offset = pad_mode_offset(m, selected);
            let lock = pad_mode_lock(m, selected);
            let pad = m.lw(record + 4);
            m.sb(pad, selected as u8);
            pending_sdk::pad_set_main_mode(m, port, offset, lock);
            let pad = m.lw(record + 4);
            m.sb(pad + 1, 0);
            m.sb(record + 1, 1);
            0
        }
        1 => {
            if state != 6 {
                return 0;
            }
            for offset in [0xd, 0xc, 0xb, 0xa, 9, 8] {
                m.sb(record + offset, 0xff);
            }
            let count = pending_sdk::pad_info_act(m, port, 0xffff_ffff, 0) & 0xff;
            let info = frame + 0x20;
            let mut aligned: u32 = 0;
            let mut actuator = 0;
            while actuator < 6 && actuator < count {
                for term in 1..4 {
                    let value = pending_sdk::pad_info_act(m, port, actuator, term);
                    m.sb(info + term - 1, value as u8);
                }
                let kind = actuator_kind(m, info);
                if kind != 0 {
                    m.sb(record + (aligned & 0xff) + 8, actuator as u8);
                    aligned = aligned.wrapping_add(1);
                }
                let pad = m.lw(record + 4);
                let mask = u32::from(m.lbu(pad + 1));
                m.sb(pad + 1, (mask | kind) as u8);
                actuator += 1;
            }
            pending_sdk::pad_set_act_align(m, port, record + 8);
            m.sb(record + 1, 2);
            0
        }
        2 => {
            if state != 6 {
                return 0;
            }
            let pad = m.lw(record + 4);
            let mask = u32::from(m.lbu(pad + 1));
            let length = (0..6).filter(|bit| (mask >> bit) & 1 != 0).count() as u32;
            let pad = m.lw(record + 4);
            pending_sdk::pad_set_act(m, port, pad + 2, length);
            let pad = m.lw(record + 4);
            m.sh(pad + 0xa, extended as u16);
            id
        }
        _ => {
            m.sb(record + 1, 0);
            0
        }
    }
}

/// 80026694: pad mode index for libpad current id `id` (1..7), else -1.
pub fn pad_mode_index(id: u32) -> u32 {
    // Jump table 0x80010000 (800266C4..800266F4).
    match id.wrapping_sub(1) {
        0 => 3,
        1 => 2,
        2 => 5,
        3 => 1,
        4 => 4,
        5 => 7,
        6 => 6,
        _ => 0xffff_ffff,
    }
}

/// 800265F0 (thunk) / 8002660C: compare zero-terminated halfword strings;
/// returns the signed difference of the first unequal halfwords, or 0.
pub fn compare_halfwords(m: &mut Machine, a: u32, b: u32) -> u32 {
    let (mut a, mut b) = (a, b);
    loop {
        let x = m.lh(a);
        let y = m.lh(b);
        let end = m.lhu(a);
        if x != y {
            return (i32::from(x) - i32::from(y)) as u32;
        }
        a = a.wrapping_add(2);
        if end == 0 {
            return 0;
        }
        b = b.wrapping_add(2);
    }
}

/// 80026628: result of the first of the two mode records at 0x800B01C4
/// whose mode string equals `table`, else -2.
pub fn pad_mode_lookup(m: &mut Machine, table: u32) -> u32 {
    for i in 0..2 {
        let entry = PAD_MODES + i * 8;
        let modes = m.lw(entry + 4);
        if compare_halfwords(m, table, modes) == 0 {
            return m.lw(entry);
        }
    }
    0xffff_fffe
}

fn pad_mode_field(m: &mut Machine, mode: u32, field: u32) -> u32 {
    let block = m.gw(PAD_BLOCK);
    let count = m.lw(block + 0xc4) as i32;
    if count <= 0 {
        return 0;
    }
    let mut entry = m.lw(block + 0xc8);
    for _ in 0..count {
        if u32::from(m.lbu(entry)) == mode {
            return u32::from(m.lbu(entry + field));
        }
        entry = entry.wrapping_add(3);
    }
    0
}

/// 80026700: byte 1 (mode offset) of the `[gp+0x10]+0xC8` record for `mode`.
pub fn pad_mode_offset(m: &mut Machine, mode: u32) -> u32 {
    pad_mode_field(m, mode, 1)
}

/// 80026754: byte 2 (mode lock) of the `[gp+0x10]+0xC8` record for `mode`.
pub fn pad_mode_lock(m: &mut Machine, mode: u32) -> u32 {
    pad_mode_field(m, mode, 2)
}

/// 800267A8: 1 if the three bytes at `a` and `b` differ, else 0.
pub fn bytes3_differ(m: &mut Machine, a: u32, b: u32) -> u32 {
    if m.lbu(a) != m.lbu(b) || m.lbu(a + 1) != m.lbu(b + 1) {
        return 1;
    }
    u32::from(m.lbu(a + 2) != m.lbu(b + 2))
}

/// 800267F0: actuator bit for the {function, sub-function, size} bytes at
/// `info` from the three records at 0x800B01D4, else 0.
pub fn actuator_kind(m: &mut Machine, info: u32) -> u32 {
    for i in 0..3 {
        let entry = ACTUATOR_KINDS + i * 4;
        if bytes3_differ(m, info, entry + 1) == 0 {
            return u32::from(m.lbu(entry));
        }
    }
    0
}

// ---- 80028C24: pad input mapping ------------------------------------------------------------

// JP-REVIEW(R0250): near function: PAL 80028784 -> JP 80028C24, del:32 imm:1, 33 instructions differ (whole routine)
/// 80028C24: map the pad state `pad` through the button configuration
/// `config` (0x5C-byte tables: digital +0, DualShock +0x5C/+0xB8, analog
/// joystick +0x114, neGcon +0x170; each 32 button masks, calibration +0x40,
/// flags +0x50, axis map +0x52, repeat flags +0x58) into the input block
/// `input` (+4 device class, +8 held, +0xC pressed, +0x10 pressed with
/// repeat, +0x14 released, +0x18 axis mask, +0x1C six axes, +0x34 motor
/// levels, +0x36 events, +0x38..+0x3B counters), and write the vibration
/// bytes for the pad (+2..+7). Context +0x50/+0x120/+0x15C in the main loop.
// JP-PATCH: replaced by `crate::jp::res_p3_mainloop::map_pad_input` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn map_pad_input(m: &mut Machine, pad: u32, input: u32, config: u32) {
    crate::jp::res_p3_mainloop::map_pad_input(m, pad, input, config)
}

/// 8002895C: clamp four analog halfwords to their calibration {low,
/// centre, high, dead zone} bytes and scale them to about ±0x1000 outside
/// the dead zone.
pub fn calibrate_axes(m: &mut Machine, axes: u32, calibration: u32) {
    for i in 0..4 {
        let axis = axes + i * 2;
        let cal = calibration + i * 4;
        let low = i32::from(m.lbu(cal));
        let value = i32::from(m.lh(axis));
        let high = i32::from(m.lbu(cal + 2));
        if value < low {
            m.sh(axis, low as u16);
        }
        let value = i32::from(m.lh(axis));
        if high < value {
            m.sh(axis, high as u16);
        }
        let centre = i32::from(m.lbu(cal + 1));
        let raw = i32::from(m.lhu(axis));
        let dead = i32::from(m.lbu(cal + 3));
        let offset = i32::from(raw.wrapping_sub(centre) as u16 as i16);
        let result = if dead < offset {
            checked_div(
                m,
                (offset - dead) << 13,
                (high - centre - dead) << 1,
                0x8002_89d8,
                0x8002_89f0,
            )
            .wrapping_sub(1)
        } else if offset < -dead {
            checked_div(
                m,
                (offset + dead) << 13,
                -((low - centre + dead) << 1),
                0x8002_8a30,
                0x8002_8a48,
            )
        } else {
            0
        };
        m.sh(axis, result as u16);
    }
}

/// 80028A7C (`negate` false) / 80028B1C (`negate` true): add
/// `7 * rsin(±value/2) / 24` to `value` and clamp to -0x1000..0xFFF.
pub fn axis_curve(m: &mut Machine, value: u32, negate: bool) -> u32 {
    let x = i32::from(value as u16 as i16);
    let half = (x + ((x as u32) >> 31) as i32) >> 1;
    let angle = if negate { half.wrapping_neg() } else { half } as u32 & 0xfff;
    let sine = sdk::rsin(m, angle) as i32;
    let seven = sine.wrapping_mul(7);
    let high = ((i64::from(seven) * 0x2aaa_aaab) >> 32) as i32;
    let sum = value.wrapping_add(((high >> 2) - (seven >> 31)) as u32);
    let mut result = sum;
    if i32::from(sum as u16 as i16) < -0x1000 {
        result = (-0x1000_i32) as u32;
    }
    if i32::from(result as u16 as i16) >= 0x1000 {
        result = 0xfff;
    }
    i32::from(result as u16 as i16) as u32
}

/// 80029734: mix the six vibration slots at `[gp+0x30]` into the two motor
/// levels at `out` (+0 small, +1 large). Each kind feeds one or both
/// motors and decays its level; a slot clears its kind when its frame
/// count reaches zero.
pub fn mix_vibration(m: &mut Machine, out: u32) {
    let mut small: u32 = 0;
    let mut large: u32 = 0;
    let slots = m.gw(VIBRATION_SLOTS);
    for i in 0..6 {
        let slot = slots + i * 4;
        let frames = m.lbu(slot);
        if frames == 0 {
            continue;
        }
        let kind = m.lbu(slot + 1);
        let level = u32::from(m.lbu(slot + 2));
        let mut decay = None;
        match kind {
            1..=3 | 9 => {
                if m.lbu(slot + 1) & 1 != 0 {
                    small = small.max(level);
                }
                if m.lbu(slot + 1) & 2 != 0 {
                    large = large.max(level);
                    decay = Some(2);
                }
            }
            0x10 => {
                large = large.max(level);
                decay = Some(3);
            }
            0x14 => {
                small = small.max(level);
                large = large.max(level >> 1);
                decay = Some(4);
            }
            0x1e => {
                small = small.max(level >> 1);
                large = large.max(level);
                decay = Some(5);
            }
            0xb4 => {
                small = small.max(level);
                large = large.max(level);
                if frames < 6 {
                    decay = Some(4);
                }
            }
            0xfa => {
                small = small.max(level);
                large = large.max(level);
                if frames < 0x39 {
                    decay = Some(5);
                }
            }
            0xa1 => {
                small = small.max(level);
                large = large.max(level);
            }
            5 | 0xa0 => large = large.max(level),
            _ => {}
        }
        if let Some(shift) = decay {
            m.sb(slot + 2, (level - (level >> shift)) as u8);
        }
        let frames = frames - 1;
        m.sb(slot, frames);
        if frames == 0 {
            m.sb(slot + 1, 0);
        }
    }
    m.sb(out, small.min(0xff) as u8);
    m.sb(out + 1, large.min(0xff) as u8);
}

// ---- 8002E1F4: sound ----------------------------------------------------------------------

/// 8002D844: CD audio volume `volume << 6` (SpuSetCommonAttr, mask 0xC0).
pub fn set_cd_volume(m: &mut Machine, volume: u32) {
    m.sw(SPU_COMMON, 0xc0);
    let level = (volume << 6) as u16;
    m.sh(SPU_COMMON + 0x12, level);
    m.sh(SPU_COMMON + 0x10, level);
    sdk::spu_set_common_attr(m, SPU_COMMON);
}

/// 8002D80C: master volume (SpuSetCommonAttr, mask 3).
pub fn set_master_volume(m: &mut Machine, volume: u32) {
    m.sw(SPU_COMMON, 3);
    m.sh(SPU_COMMON + 4, volume as u16);
    m.sh(SPU_COMMON + 6, volume as u16);
    sdk::spu_set_common_attr(m, SPU_COMMON);
}

/// 8002999C: CD audio mixing on (`mute` 0) or off (SpuSetCommonAttr mask
/// 0x200 on a stack attribute).
pub fn set_cd_mix(m: &mut Machine, mute: u32) {
    let frame = m.push_frame(0x40);
    m.sw(frame + 0x10, 0x200);
    m.sw(frame + 0x28, u32::from(mute == 0));
    sdk::spu_set_common_attr(m, frame + 0x10);
    m.pop_frame(0x40);
}

// JP-REVIEW(R0254): near function: PAL 8002DDCC -> JP 8002E1F4, del:3 imm:2 op:4 reg:1, 10 instructions differ (whole routine)
/// 8002E1F4: per-frame sound update. CD and master volume, delayed sounds
/// and the sound sequence, then for each of the 24 voices key-on setup,
/// volume fades and volume updates, key on, release of voices whose
/// envelope ended, freeing, and key off.
// JP-PATCH: replaced by `crate::jp::res_p3_mainloop::update_sound` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn update_sound(m: &mut Machine) {
    crate::jp::res_p3_mainloop::update_sound(m)
}

// JP-REVIEW(R0256): near function: PAL 8002DB70 -> JP 8002DF98, imm:2, 2 instructions differ (whole routine)
/// 8002DF98: SpuSetVoiceAttr volume/note update of voice `index` (mask
/// 0x23) from its volume (+4/+6), the effects volume (context +0x39C), the
/// fade level (+0x11) and the mono flag (context +0x39E bit 0).
// JP-PATCH: replaced by `crate::jp::res_p3_mainloop::update_voice_volume` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn update_voice_volume(m: &mut Machine, index: u32) {
    crate::jp::res_p3_mainloop::update_voice_volume(m, index)
}

// JP-REVIEW(R0257): near function: PAL 8002DCA8 -> JP 8002E0D0, imm:2, 2 instructions differ (whole routine)
/// 8002E0D0: SpuSetVoiceAttr key-on setup of voice `index` (mask 0x600E3:
/// volume, note, sample note, start address, ADSR) from its sound
/// definition.
// JP-PATCH: replaced by `crate::jp::res_p3_mainloop::key_on_voice` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn key_on_voice(m: &mut Machine, index: u32) {
    crate::jp::res_p3_mainloop::key_on_voice(m, index)
}

/// 8002F1E0: count down the eight delayed sounds; play each when it expires.
pub fn update_delayed_sounds(m: &mut Machine) {
    for index in 0..8 {
        let slot = DELAYED_SOUNDS + index * 4;
        let frames = m.lh(slot + 2);
        if frames < 0 {
            continue;
        }
        let frames = m.lhu(slot + 2).wrapping_sub(1);
        m.sh(slot + 2, frames);
        if frames == 0 {
            let id = u32::from(m.lbu(slot));
            script::play_sound_8002d724(m, id);
            shared::reset_channel_group(m, index);
        }
    }
}

/// 8002F2CC: step the 32-entry sound sequence: wait for the playing step's
/// voice to end, then advance; a pending step plays after its delay
/// (not while paused); a step with frames -1 marks the end (`gp+0x325`).
pub fn update_sequence(m: &mut Machine) {
    let position = u32::from(m.lbu(crate::GP + SEQUENCE_POSITION));
    let step = SOUND_SEQUENCE + position * 4;
    let playing = u32::from(m.lbu(step + 1));
    if playing != NO_VOICE {
        if m.lbu(voice(playing) + 3) != 0 {
            return;
        }
        m.sb(step + 1, 0xff);
        m.sb(step, 0xff);
        m.sh(step + 2, 0xffff);
        let position = m.lbu(crate::GP + SEQUENCE_POSITION);
        m.sb(
            crate::GP + SEQUENCE_POSITION,
            position.wrapping_add(1) & 0x1f,
        );
        return;
    }
    let frames = m.lh(step + 2);
    if frames == -1 {
        m.sb(crate::GP + SEQUENCE_STOPPED, position as u8);
        return;
    }
    let context = m.gw(CONTEXT);
    if m.lw(context) & flags::PAUSED != 0 {
        return;
    }
    if frames == 0 {
        let id = u32::from(m.lbu(step));
        let started = script::play_sound_8002d724(m, id);
        m.sb(step + 1, started as u8);
    }
    let frames = m.lhu(step + 2);
    m.sh(step + 2, frames.wrapping_sub(1));
}

// ---- 8002C4DC: CD queue and monitor ---------------------------------------------------------

/// 8002C4DC: run the CD monitor, then the pending command (+0x14C) or the
/// next queued one; a finished command is removed.
pub fn process_cd_queue(m: &mut Machine) {
    cd_monitor(m);
    let queue = m.gw(CD_QUEUE);
    if m.lw(queue + 0x148) != 0 {
        if run_cd_command(m, queue + 0x14c) != 0 {
            let queue = m.gw(CD_QUEUE);
            m.sw(queue + 0x148, 0);
        }
        return;
    }
    let read = m.lw(queue);
    let write = m.lw(queue + 4);
    if (read as i32) < (write as i32) {
        let entry = queue + (read & 0xf) * 0x14 + 8;
        if run_cd_command(m, entry) != 0 {
            let queue = m.gw(CD_QUEUE);
            let read = m.lw(queue);
            m.sw(queue, read.wrapping_add(1));
        }
    }
}

/// 8002C45C: call the handler of command entry `entry` (table 0x800B0390
/// by byte 0); when it reports completion, call the entry's completion
/// routine (+0x10) with `[gp+0x48]+0x16C`. Returns the handler's result.
pub fn run_cd_command(m: &mut Machine, entry: u32) -> u32 {
    let command = u32::from(m.lbu(entry));
    let handler = m.lw(CD_COMMANDS + command * 4);
    let done = dispatch::call(m, handler, [entry, 0, 0, 0]);
    if done != 0 {
        let completion = m.lw(entry + 0x10);
        if completion != 0 {
            let queue = m.gw(CD_QUEUE);
            let argument = m.lw(queue + 0x16c);
            dispatch::call(m, completion, [argument, 0, 0, 0]);
        }
    }
    done
}

// ---- 8002BD28..8002BDB0: CD audio volume settings (no functions.tsv records) -------

/// 8002BD28: the CD audio stereo flag `[gp+0x48]` +0x160 = `stereo`.
/// Returns `v0` = the queue pointer.
pub fn set_cd_stereo(m: &mut Machine, stereo: u32) -> u32 {
    let queue = m.gw(CD_QUEUE);
    m.sw(queue + 0x160, stereo);
    queue
}

/// 8002BD34: the CD audio volume `[gp+0x48]` +0x164 = `volume` clamped to
/// 0..=0xFF (signed). Returns `v0` = the queue pointer.
pub fn set_cd_audio_volume(m: &mut Machine, volume: u32) -> u32 {
    let volume = (volume as i32).clamp(0, 0xff) as u32;
    let queue = m.gw(CD_QUEUE);
    m.sw(queue + 0x164, volume);
    queue
}

/// 8002BD5C: applies the stored settings: 800299D8 (CdMix through
/// `boot::set_cd_volume`) with the volume +0x164 and stereo flag +0x160.
/// Returns its `v0`.
pub fn apply_cd_audio_volume(m: &mut Machine) -> u32 {
    let queue = m.gw(CD_QUEUE);
    m.push_frame(0x18);
    let volume = m.lw(queue + 0x164);
    let stereo = m.lw(queue + 0x160);
    let v0 = dispatch::call(m, 0x8002_99d8, [volume, stereo, 0, 0]);
    m.pop_frame(0x18);
    v0
}

/// 8002BD88 / 8002BD9C: the stored stereo flag (+0x160) / volume (+0x164).
pub fn cd_audio_setting(m: &mut Machine, offset: u32) -> u32 {
    let queue = m.gw(CD_QUEUE);
    m.lw(queue + offset)
}

/// 8002BDC4: CD command 0 (no operation); always done.
pub fn cd_command_nop() -> u32 {
    1
}

/// 8002BDCC: CD command 1: clear the current track (+0x174 = -1) and stop
/// streaming (8002AC3C with the entry's mode +4); once the stop is issued
/// the paused track (+0x178) is cleared. Returns 8002AC3C's result.
pub fn cd_command_stop(m: &mut Machine, entry: u32) -> u32 {
    let queue = m.gw(CD_QUEUE);
    m.sw(queue + 0x174, 0xffff_ffff);
    let mode = m.lw(entry + 4);
    let issued = cd_stop(m, mode);
    if issued != 0 {
        let queue = m.gw(CD_QUEUE);
        m.sw(queue + 0x178, 0);
    }
    issued
}

// JP-REVIEW(R0258): exact function, calls differ: PAL 8002BFF4 -> JP 8002C41C: instructions equal once addresses are masked, but only 0/1 calls go to the PAL callee's counterpart
/// 8002C41C: CD command 6: pause (entry +4 nonzero) or resume the stream
/// (PAL 8002A76C) and record the request at +0x178. Always done.
// JP-PATCH: replaced by `crate::jp::res_p3_mainloop::cd_command_pause` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn cd_command_pause(m: &mut Machine, entry: u32) -> u32 {
    crate::jp::res_p3_mainloop::cd_command_pause(m, entry)
}

fn set_monitor_state(m: &mut Machine, state: u32) {
    let monitor = m.gw(CD_MONITOR);
    m.sw(monitor, state);
    m.sw(monitor + 4, 0);
}

/// 8002A2C0: CD monitor state machine (`[gp+0x34]`; jump table 0x80010474).
/// 0 normal: poll CdSync into +0x28, call the stream's sync hook, and pause
/// a stream whose retries ran out; an open lid starts recovery (4). 2/3
/// wait for the drive and re-identify the disc, 4 re-reads the disc header.
pub fn cd_monitor(m: &mut Machine) {
    let monitor = m.gw(CD_MONITOR);
    match m.lw(monitor) {
        0 => {
            let monitor = m.gw(CD_MONITOR);
            if m.lbu(monitor + 0x28) & 0x10 != 0 {
                m.sw(monitor + 0xc, 4);
                cd_lid_opened(m);
                return;
            }
            let status = sdk::cd_sync(m, 1, monitor + 0x28);
            let stream = m.gw(CD_STREAM);
            let hook = m.lw(stream + 4);
            let busy = if hook != 0 {
                dispatch::call(m, hook, [status, 0, 0, 0])
            } else {
                0
            };
            let stream = m.gw(CD_STREAM);
            if m.lw(stream) & 0x10 == 0 || busy != 0 {
                return;
            }
            let retries = m.lw(stream + 0x30);
            m.sw(stream + 0x30, retries.wrapping_sub(1));
            if retries as i32 > 0 {
                return;
            }
            let word = m.lw(stream);
            m.sw(stream + 0x20, 0xff);
            m.sw(stream, (word | 0x20) & !0x10);
            pending_sdk::cd_control_f(m, 9, 0);
        }
        2 => {
            let monitor = m.gw(CD_MONITOR);
            if cd_wait_ready(m, monitor + 0x28) == 1 {
                set_monitor_state(m, 3);
            }
        }
        3 => {
            let monitor = m.gw(CD_MONITOR);
            let result = cd_identify(m, monitor + 0x28) as i32;
            if result < 0 {
                set_monitor_state(m, 2);
            } else if result == 1 {
                set_monitor_state(m, 4);
            }
        }
        4 => {
            let monitor = m.gw(CD_MONITOR);
            if m.lbu(monitor + 0x28) & 0x10 != 0 {
                cd_lid_opened(m);
                return;
            }
            let result = crate::boot::open_resource_files(m) as i32;
            if result == 1 {
                set_monitor_state(m, 0);
            } else if result < 0 {
                set_monitor_state(m, 2);
            }
        }
        _ => {}
    }
}

/// 8002B428: in state 0, stop streaming (8002AC3C(1)) and enter state 2.
pub fn cd_lid_opened(m: &mut Machine) {
    let monitor = m.gw(CD_MONITOR);
    if m.lw(monitor) != 0 {
        return;
    }
    cd_stop(m, 1);
    set_monitor_state(m, 2);
}

// JP-REVIEW(R0260): near function: PAL 8002A80C -> JP 8002AC3C, call-:1 call~:1 del:3 imm:1 ins:1, 6 instructions differ (whole routine)
/// 8002AC3C: remove the CD callbacks, clear the stream state, mute CD audio
/// (8002999C(1)) and, if CdSync reports complete, issue CdControl 9 (pause,
/// `mode` 0), 7 (standby, `mode` 1) or 8 (stop). Returns 1 if issued.
// JP-PATCH: replaced by `crate::jp::res_p3_mainloop::cd_stop` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn cd_stop(m: &mut Machine, mode: u32) -> u32 {
    crate::jp::res_p3_mainloop::cd_stop(m, mode)
}

/// 8002B094: wait for a closed lid and a complete CdSync, then step
/// (+4) through CdControlB 0x0E (setmode, `gp+0x2FC` = 0), CdControl 8
/// (stop) and CdControlB 1 (nop). Returns 1 while the lid is open.
pub fn cd_wait_ready(m: &mut Machine, result: u32) -> u32 {
    if m.lbu(result) & 0x10 != 0 {
        return 1;
    }
    if sdk::cd_sync(m, 1, result) != 2 {
        return 0;
    }
    let monitor = m.gw(CD_MONITOR);
    match m.lw(monitor + 4) {
        0 => {
            m.sb(crate::GP + 0x2fc, 0);
            if pending_sdk::cd_control_b(m, 0xe, crate::GP + 0x2fc, result) != 0 {
                let monitor = m.gw(CD_MONITOR);
                m.sw(monitor + 4, 1);
            }
        }
        1 => {
            if pending_sdk::cd_control(m, 8, 0, result) != 0 {
                let monitor = m.gw(CD_MONITOR);
                m.sw(monitor + 4, 2);
            }
        }
        2 => {
            pending_sdk::cd_control_b(m, 1, 0, result);
        }
        _ => {}
    }
    0
}

/// 8002B194: drive re-identification after a lid close (steps +4 0..7 via
/// jump table 0x8001048c). Returns -1 on a drive error (+0xC = 5), 1 when
/// the drive is ready, -2 on timeout, else 0.
pub fn cd_identify(m: &mut Machine, result: u32) -> u32 {
    let status = m.lbu(result);
    let error = (status & 1 != 0 && m.lbu(result + 1) & 0x40 != 0)
        || (status & 4 != 0 && m.lbu(result + 1) & 4 != 0);
    if error {
        let monitor = m.gw(CD_MONITOR);
        m.sw(monitor + 0xc, 5);
        return 0xffff_ffff;
    }
    if m.lbu(result) & 0x10 != 0 {
        let monitor = m.gw(CD_MONITOR);
        m.sw(monitor + 4, 0);
        m.sw(monitor + 0xc, 4);
    }
    if sdk::cd_sync(m, 1, result) != 2 {
        return 0;
    }
    let monitor = m.gw(CD_MONITOR);
    let advance = |m: &mut Machine, step: u32| {
        let monitor = m.gw(CD_MONITOR);
        m.sw(monitor + 4, step);
    };
    match m.lw(monitor + 4) {
        0 => {
            if m.lbu(result) & 0x10 == 0 {
                let monitor = m.gw(CD_MONITOR);
                let clock = m.lw(monitor + 0x20);
                m.sw(monitor + 4, 1);
                m.sw(monitor + 0xc, 0);
                let now = m.lw(clock);
                m.sw(monitor + 8, now);
            }
            pending_sdk::cd_control_b(m, 1, 0, result);
            0
        }
        1 => {
            let monitor = m.gw(CD_MONITOR);
            let clock = m.lw(monitor + 0x20);
            let start = m.lw(monitor + 8);
            let now = m.lw(clock);
            if (start.wrapping_add(0xb4) as i32) < now as i32 {
                m.sw(monitor + 0xc, 4);
                return 0xffff_fffe;
            }
            if m.lbu(result) & 2 != 0 {
                m.sw(monitor + 4, 2);
            }
            pending_sdk::cd_control_b(m, 1, 0, result);
            0
        }
        2 => {
            if pending_sdk::cd_control_b(m, 0x13, 0, result) != 0 {
                advance(m, 3);
            }
            0
        }
        3 => {
            let parameter = crate::GP + 0x300;
            m.sb(parameter, 0);
            m.sb(crate::GP + 0x301, 2);
            m.sb(crate::GP + 0x302, 0);
            m.sb(crate::GP + 0x303, 0);
            if pending_sdk::cd_control(m, 2, parameter, result) != 0 {
                advance(m, 4);
            }
            0
        }
        4 => {
            if pending_sdk::cd_control(m, 0x15, 0, result) != 0 {
                advance(m, 5);
            }
            0
        }
        5 => {
            if pending_sdk::cd_control(m, 1, 0, result) != 0 {
                advance(m, 6);
            }
            0
        }
        6 => {
            m.sb(crate::GP + 0x2fd, 0x80);
            if pending_sdk::cd_control(m, 0xe, crate::GP + 0x2fd, result) != 0 {
                let monitor = m.gw(CD_MONITOR);
                let clock = m.lw(monitor + 0x20);
                m.sw(monitor + 4, 7);
                let now = m.lw(clock);
                m.sw(monitor + 8, now);
            }
            0
        }
        7 => {
            let monitor = m.gw(CD_MONITOR);
            let clock = m.lw(monitor + 0x20);
            let start = m.lw(monitor + 8);
            let now = m.lw(clock);
            u32::from((start.wrapping_add(3) as i32) < now as i32)
        }
        _ => 0,
    }
}

// ---- 80026350: state dispatch ---------------------------------------------------------------

/// 80026044: the initial state's callback (state 0 of the table 0x800B0134):
/// set the state to 4 with an arena reset (`0x60000004`).
pub fn initial_state_callback() -> u32 {
    transition::RESET_ARENA | transition::SET_STATE | 4
}

/// 80026350: call the current state callback and apply its result.
pub fn dispatch_state(m: &mut Machine) -> u32 {
    let mut call = dispatch::call;
    dispatch_state_with(m, &mut call)
}

/// 80026350 with an explicit callback invoker. With the loaded module
/// (+0x38) equal to the requested one (+0x34) the callback is
/// `table[substate]` of state record `+0x2C` (a0 = module, a1 = context);
/// otherwise the loading callback of the substate (a0 = module,
/// a1 = load address), whose LOAD_COMPLETE result marks the module loaded.
/// Then SET_STATE / NEXT_STATE / SET_SUBSTATE / NEXT_SUBSTATE (first set bit
/// wins), a new state's record fields, and RESET_ARENA (also requests
/// DrawSync). Returns 0.
pub fn dispatch_state_with(m: &mut Machine, call: &mut CallbackInvoker) -> u32 {
    use transition::*;
    let context = m.gw(CONTEXT);
    let requested = m.lw(context + 0x34);
    let loaded = m.lw(context + 0x38);
    let result = if requested == loaded {
        let state = m.lw(context + 0x2c);
        let record = STATE_TABLE.wrapping_add(state << 4);
        let substate = m.lw(context + 0x30);
        let table = m.lw(record);
        let callback = m.lw(table.wrapping_add(substate << 2));
        call(m, callback, [requested, context, 0, 0])
    } else {
        let substate = m.lw(context + 0x30);
        let address = m.lw(context + 0x3c);
        let callback = m.lw(LOADING_CALLBACKS.wrapping_add(substate << 2));
        let result = call(m, callback, [requested, address, 0, 0]);
        if result & LOAD_COMPLETE != 0 {
            let context = m.gw(CONTEXT);
            let requested = m.lw(context + 0x34);
            m.sw(context + 0x30, 0);
            m.sw(context + 0x38, requested);
        }
        result
    };
    if result == 0 {
        return 0;
    }
    if result & SET_STATE != 0 {
        let context = m.gw(CONTEXT);
        m.sw(context + 0x2c, result & 0xff);
        m.sw(context + 0x30, 0);
    } else if result & NEXT_STATE != 0 {
        let context = m.gw(CONTEXT);
        let state = m.lw(context + 0x2c);
        m.sw(context + 0x30, 0);
        m.sw(context + 0x2c, state.wrapping_add(1));
    } else if result & SET_SUBSTATE != 0 {
        let context = m.gw(CONTEXT);
        m.sw(context + 0x30, result & 0xff);
    } else if result & NEXT_SUBSTATE != 0 {
        let context = m.gw(CONTEXT);
        let substate = m.lw(context + 0x30);
        m.sw(context + 0x30, substate.wrapping_add(1));
    }
    if result & (SET_STATE | NEXT_STATE) != 0 {
        let context = m.gw(CONTEXT);
        let record = |m: &mut Machine| {
            let state = m.lw(context + 0x2c);
            STATE_TABLE.wrapping_add(state << 4)
        };
        let entry = record(m);
        let module = m.lw(entry + 4);
        let entry = record(m);
        m.sw(context + 0x34, module);
        let address = m.lw(entry + 8);
        let entry = record(m);
        m.sw(context + 0x3c, address);
        let limit = m.lw(entry + 0xc);
        let module = m.lw(context + 0x34);
        m.sw(context + 0x40, limit);
        if (module as i32) < 0 {
            m.sw(context + 0x38, 0xffff_ffff);
        }
    }
    if result & RESET_ARENA != 0 {
        let context = m.gw(CONTEXT);
        let limit = m.lw(context + 0x40);
        crate::boot::set_arena(m, limit);
        let context = m.gw(CONTEXT);
        let word = m.lw(context);
        m.sw(context, word | flags::DRAW_SYNC);
    }
    0
}

// ---- 800261A8: loop body --------------------------------------------------------------------

/// One pass of the resident main loop body (`800261D0` until it branches
/// back), with the state callback called through [`dispatch::call`].
pub fn main_loop_iteration(m: &mut Machine) {
    let mut call = dispatch::call;
    main_loop_iteration_with(m, &mut call);
}

/// [`main_loop_iteration`] with an explicit state-callback invoker.
pub fn main_loop_iteration_with(m: &mut Machine, call: &mut CallbackInvoker) {
    // Host-side: whether this pass is a widescreen flight pass.
    crate::widescreen::begin_pass(m);
    display_requests(m);
    swap_buffers(m);
    poll_pads(m);
    let context = m.gw(CONTEXT);
    map_pad_input(m, context + 0x50, context + 0x120, context + 0x15c);
    update_sound(m);
    dispatch_state_with(m, call);
    process_cd_queue(m);
    let context = m.gw(CONTEXT);
    let ot = m.lw(context + 0xc);
    let packet = m.lw(context + 0x14);
    let packet = link_sprites(m, ot, packet);
    let context = m.gw(CONTEXT);
    m.sw(context + 0x14, packet);
    let drawing_start = sdk::get_rcnt(m, ROOT_COUNTER_1);
    let context = m.gw(CONTEXT);
    m.sw(context + 0x48, drawing_start);
    sdk::draw_sync(m, 0);
    let drawing_end = sdk::get_rcnt(m, ROOT_COUNTER_1);
    let context = m.gw(CONTEXT);
    let display = m.gw(DISPLAY_REQUESTS);
    m.sw(context + 0x4c, drawing_end);
    let interval = m.lw(display + 0x14);
    sdk::vsync(m, interval);
    let vblanks = sdk::vsync(m, 0xffff_ffff);
    let context = m.gw(CONTEXT);
    m.sw(context + 0x20, vblanks);
    sdk::reset_rcnt(m, ROOT_COUNTER_1);
    frame_end(m);
    let context = m.gw(CONTEXT);
    let word = m.lw(context);
    if word & flags::DRAW_SYNC != 0 {
        m.sw(context, word & !flags::DRAW_SYNC);
        sdk::draw_sync(m, 0);
    }
    let context = m.gw(CONTEXT);
    let word = m.lw(context);
    if word & flags::PAUSE_LOCK != 0 {
        m.sw(
            context,
            word & !(flags::PAUSED | flags::RESUME_REQUEST | flags::PAUSE_REQUEST),
        );
        return;
    }
    if word & flags::PAUSE_REQUEST != 0 {
        m.sw(context, (word & !flags::PAUSE_REQUEST) | flags::PAUSED);
    }
    let context = m.gw(CONTEXT);
    let word = m.lw(context);
    if word & flags::RESUME_REQUEST != 0 {
        m.sw(context, word & !flags::RESUME_REQUEST & !flags::PAUSED);
    }
}

// ---- dispatch ---------------------------------------------------------------------------

/// Native targets owned by this module, by original address (a0..a3 in,
/// v0 out; routines without a meaningful v0 return 0).
#[allow(unused_variables)]
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let [a0, a1, a2, _] = args;
    Some(match target {
        0x8002_5bc8 => {
            display_requests(m);
            0
        }
        0x8002_6044 => initial_state_callback(),
        0x8002_6050 => swap_buffers(m),
        0x8002_60c0 => frame_end(m),
        0x8002_6350 => dispatch_state(m),
        0x8002_65f0 | 0x8002_660c => compare_halfwords(m, a0, a1),
        0x8002_6628 => pad_mode_lookup(m, a0),
        0x8002_6694 => pad_mode_index(a0),
        0x8002_6700 => pad_mode_offset(m, a0),
        0x8002_6754 => pad_mode_lock(m, a0),
        0x8002_67a8 => bytes3_differ(m, a0, a1),
        0x8002_67f0 => actuator_kind(m, a0),
        0x8002_685c => pad_setup(m, a0),
        0x8002_6ba4 => {
            pad_update(m, a0, a1);
            0
        }
        0x8002_6e84 => {
            poll_pads(m);
            0
        }
        0x8002_78a8 => link_sprites(m, a0, a1),
        0x8002_895c => {
            calibrate_axes(m, a0, a1);
            0
        }
        0x8002_8a7c => axis_curve(m, a0, false),
        0x8002_8b1c => axis_curve(m, a0, true),
        0x8002_8c24 => {
            map_pad_input(m, a0, a1, a2);
            0
        }
        0x8002_9734 => {
            mix_vibration(m, a0);
            0
        }
        0x8002_999c => {
            set_cd_mix(m, a0);
            0
        }
        0x8002_a2c0 => {
            cd_monitor(m);
            0
        }
        0x8002_ac3c => cd_stop(m, a0),
        0x8002_b094 => cd_wait_ready(m, a0),
        0x8002_b194 => cd_identify(m, a0),
        0x8002_bd28 => set_cd_stereo(m, a0),
        0x8002_bd34 => set_cd_audio_volume(m, a0),
        0x8002_bd5c => apply_cd_audio_volume(m),
        0x8002_bd88 => cd_audio_setting(m, 0x160),
        0x8002_bd9c => cd_audio_setting(m, 0x164),
        0x8002_bdc4 => cd_command_nop(),
        0x8002_bdcc => cd_command_stop(m, a0),
        0x8002_c41c => cd_command_pause(m, a0),
        0x8002_b428 => {
            cd_lid_opened(m);
            0
        }
        0x8002_c45c => run_cd_command(m, a0),
        0x8002_c4dc => {
            process_cd_queue(m);
            0
        }
        0x8002_d80c => {
            set_master_volume(m, a0);
            0
        }
        0x8002_d844 => {
            set_cd_volume(m, a0);
            0
        }
        0x8002_df98 => {
            update_voice_volume(m, a0);
            0
        }
        0x8002_e0d0 => {
            key_on_voice(m, a0);
            0
        }
        0x8002_e1f4 => {
            update_sound(m);
            0
        }
        0x8002_f1e0 => {
            update_delayed_sounds(m);
            0
        }
        0x8002_f2cc => {
            update_sequence(m);
            0
        }
        _ => return pending_sdk::dispatch(m, target, args),
    })
}
