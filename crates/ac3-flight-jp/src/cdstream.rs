//! CD reads and XA/data streaming on the machine model: the CD request
//! handlers 2..5 the main-loop pump (8002C4DC/8002C45C) runs, the drive
//! driver they start (8002A79C) with its command state machines, interrupt
//! callbacks and the data-stream ring. The host side, [`CdDrive`] (a drive
//! model answering the libcd host protocol from the original disc), is the
//! version-independent `ac3_machine::cd`, re-exported here; [`CLOCK`] is
//! where it reads the PAL VBlank count.
//!
//! | Routine | Rust |
//! | --- | --- |
//! | 8002BE14 (request 2: file read) | [`read_file_request`] |
//! | 8002BEE4 (request 3: play a stream, fade in) | [`play_stream_request`] |
//! | 8002C1D4 (request 4: read a stream file) | [`read_stream_request`] |
//! | 8002C2E4 (request 5: fade out and stop) | [`fade_out_request`] |
//! | 8002A79C (start a read or stream) | [`start_read`] |
//! | 80029DFC (seek to the start sector) | [`seek_start`] |
//! | 80029E74 / 80029FE4 (data / stream command states) | [`data_states`] / [`stream_states`] |
//! | 8002A200 (CdSyncCallback) | [`sync_callback`] |
//! | 80029A44 (CdReadyCallback of data reads) | [`data_ready`] |
//! | 80029D70 / 80029BD8 (CdReadyCallback of streams) | [`stream_ready`] / [`stream_sector`] |
//! | 8002A498 (stream service, the CD monitor's hook) | [`stream_service`] |
//! | 8002B760 / 8002B610 / 8002B6D0 (data-stream ring) | [`ring_sector`] / [`ring_init`] / [`ring_clear`] |
//! | 8002B588 / 8002BDB0 | [`driver_flags`] / [`read_complete`] |
//!
//! Routines owned elsewhere are called by address: 8002995C (file table
//! record, `loading.rs`) and 800299D8 (CD volume, `boot.rs`); the CD mix
//! switch 8002999C and the stop 8002AC3C are `mainloop.rs`'s. libcd is
//! [`pending_sdk`]'s RAM-exact translation over the CD host protocol.
//!
//! # Driver state `[gp+0x38]`
//!
//! +0 flags (bit 31 open, 30 paused, 25 resume armed, 24 a sector arrived,
//! 15/14 sub-data modes, 13 ring stream, 12 channel filter, 10 XA audio, 7
//! failed, 6 pause/resume request, 5 restart, 4 waiting for the first
//! sector); +4 service hook (8002A498 for streams); +8 completion callback;
//! +0xC command state handler; +0x10 start sector; +0x14 words (data) or
//! sectors (streams) left; +0x18 destination; +0x1C/+0x1D Setfilter file
//! and channel; +0x20 state; +0x24 last sector read; +0x28 estimated
//! position; +0x2C last sector; +0x30 first-sector timeout; +0x34 error;
//! +0x3C end time; +0x40 elapsed time while paused; +0x44 time of the last
//! data sector. Times are the VBlank count at `[[gp+0x34]+0x20]`.
//!
//! # Host contract (the CD interrupt)
//!
//! The drive raises its interrupt when a command response or a sector is
//! ready; the host then runs libcd's handler,
//! [`pending_sdk::cd_interrupt_handler`] (or [`cd_service`]), which polls
//! the drive and runs the callbacks this module installs: 8002A200 on
//! command completion (2) and errors (5), 80029A44 (data reads) or
//! 80029D70 (streams) on data ready (1). Blocking libcd calls deliver the
//! interrupts they wait for themselves. A host without a drive interrupt
//! line calls [`cd_service`] at least once per main-loop pass and in every
//! loop that waits for a CD request (such as boot's 8002C580 after its
//! `VSync(0)`); [`CdDrive`] paces sectors by the VBlank count (0x800B6174,
//! or VSync's 0x800B5048 when later), so calling it more often changes
//! nothing but latency. Without a drive, the first CD command faults
//! (`Fault::Untranslated { address: 0x800A3F64 }`). libcd's state must
//! come from its own initialisation ([`pending_sdk::cd_init`], CdInit
//! 800A2D58): a CdSync status byte left at 0 makes every read wait.
use crate::{
    dispatch,
    machine::{Machine, SmallData},
    mainloop::{self, pending_sdk},
    sdk,
};
pub use ac3_machine::cd::{
    Archives, CdAudio, CdDrive, CdDriveState, Disc, RawImage, SECTOR_BYTES, VBlankClock,
};

/// `[gp+0x34]`: CD monitor (+0x20 pointer to the VBlank counter, +0x24 MDEC
/// check, +0x28/+0x2C last callback result).
const CD_MONITOR: u32 = 0x34;
/// `[gp+0x38]`: the driver state above.
const CD_DRIVER: u32 = 0x38;
/// `[gp+0x3C]`: sub-data buffer (CdGetSector2 target; word 0 is cleared at
/// each start).
const SUB_DATA: u32 = 0x3c;
/// `[gp+0x40]`: data-stream ring state.
const RING: u32 = 0x40;
/// `[gp+0x48]`: CD request queue (+0x160 stereo, +0x164 volume, +0x168
/// done, +0x16C status, +0x170 fade start, +0x174 current file, +0x178
/// pause request).
const CD_QUEUE: u32 = 0x48;
/// `[gp+0x2EC]`: timer block (+0x20 frame count of the fades).
const TIMER: u32 = 0x2ec;
/// `[gp+0x30C]`: a stream sector deferred while the MDEC was busy.
const DEFERRED_SECTOR: u32 = 0x30c;

pub const READ_COMPLETE: u32 = 0x8002_bdb0;
pub const SYNC_CALLBACK: u32 = 0x8002_a200;
pub const DATA_READY: u32 = 0x8002_9a44;
pub const STREAM_READY: u32 = 0x8002_9d70;
pub const DATA_STATES: u32 = 0x8002_9e74;
pub const STREAM_STATES: u32 = 0x8002_9fe4;
pub const STREAM_SERVICE: u32 = 0x8002_a498;
const FILE_ENTRY: u32 = 0x8002_995c;
const SET_CD_VOLUME: u32 = 0x8002_99d8;

const fn at(base: u32, offset: u32) -> u32 {
    base.wrapping_add(offset)
}
fn sext16(value: i16) -> u32 {
    value as i32 as u32
}
fn driver(m: &mut Machine) -> u32 {
    m.gw(CD_DRIVER)
}
fn framed<R>(m: &mut Machine, size: u32, body: impl FnOnce(&mut Machine, u32) -> R) -> R {
    let frame = m.push_frame(size);
    let result = body(m, frame);
    m.pop_frame(size);
    result
}
/// Store the last callback result words (`*result`, `result[1]`) in the
/// monitor (+0x28/+0x2C), as every CD callback here does first.
fn keep_result(m: &mut Machine, result: u32) {
    let monitor = m.gw(CD_MONITOR);
    let low = m.lw(result);
    m.sw(at(monitor, 0x28), low);
    let high = m.lw(at(result, 4));
    m.sw(at(monitor, 0x2c), high);
}

// ---- request handlers 2..5 (table 0x800B0390) --------------------------------------

/// 8002BE14: request 2, read file `entry+2` (8002995C record) to `entry+4`.
/// Phase 0 starts the read (8002A79C, completion 8002BDB0) and moves to
/// phase 1 once started; phase 1 waits for the completion: done (1, current
/// file -1) unless it reported -2 (restart from phase 0). Returns 1 when
/// done.
pub fn read_file_request(m: &mut Machine, entry: u32) -> u32 {
    framed(m, 0x20, |m, frame| match m.lbu(at(entry, 1)) {
        0 => {
            let queue = m.gw(CD_QUEUE);
            let file = sext16(m.lh(at(entry, 2)));
            m.sw(at(queue, 0x174), file);
            m.sw(at(queue, 0x168), 0);
            let file = sext16(m.lh(at(entry, 2)));
            let record = dispatch::call(m, FILE_ENTRY, [file, 0, 0, 0]);
            m.sw(at(frame, 0x10), 0);
            let destination = m.lw(at(entry, 4));
            if start_read(m, record, destination, 0, READ_COMPLETE, 0) == 1 {
                m.sb(at(entry, 1), 1);
            }
            0
        }
        1 => wait_read(m, entry),
        _ => 0,
    })
}

/// Phase 1 of requests 2 and 4.
fn wait_read(m: &mut Machine, entry: u32) -> u32 {
    let queue = m.gw(CD_QUEUE);
    if m.lw(at(queue, 0x168)) == 0 {
        return 0;
    }
    if m.lw(at(queue, 0x16c)) == -2i32 as u32 {
        m.sb(at(entry, 1), 0);
        return 0;
    }
    m.sw(at(queue, 0x174), u32::MAX);
    1
}

/// 8002BEE4: request 3, play the stream chain at `entry+4` (records: +0
/// stream file, +2 fade-in frames, +4 fade divisor, +6 start offset, +8
/// next record). Nothing happens while the queue's pause request +0x178 is
/// set. Phase 0 sets the volume (0xFF without fade-in, else 0), starts the
/// stream from its offset (a copy of the file record at frame +0x18 with
/// the sector advanced and the length reduced; `entry+8` zero drops the
/// sub-data bit 0x1000) and moves to phase 1 (fade in) or 2. Phase 1 raises
/// the volume with the frames since the first sector; phase 2 waits for the
/// end and continues with the next record, or is done (1).
pub fn play_stream_request(m: &mut Machine, entry: u32) -> u32 {
    framed(m, 0x30, |m, frame| play_stream_body(m, entry, frame))
}

fn play_stream_body(m: &mut Machine, entry: u32, frame: u32) -> u32 {
    let queue = m.gw(CD_QUEUE);
    if m.lw(at(queue, 0x178)) != 0 {
        return 0;
    }
    let phase = m.lbu(at(entry, 1));
    let stream = m.lw(at(entry, 4));
    match phase {
        0 => {
            let volume = if m.lh(at(stream, 2)) == 0 { 0xff } else { 0 };
            m.sw(at(queue, 0x164), volume);
            let queue = m.gw(CD_QUEUE);
            let volume = m.lw(at(queue, 0x164));
            let stereo = m.lw(at(queue, 0x160));
            dispatch::call(m, SET_CD_VOLUME, [volume, stereo, 0, 0]);
            let keep = m.lw(at(entry, 8));
            let mut file = sext16(m.lh(stream));
            if keep == 0 {
                file &= !0x1000;
            }
            let queue = m.gw(CD_QUEUE);
            m.sw(at(queue, 0x174), file);
            let record = dispatch::call(m, FILE_ENTRY, [file, 0, 0, 0]);
            let size = m.lw(record);
            let sector = m.lw(at(record, 4));
            m.sw(at(frame, 0x18), size);
            m.sw(at(frame, 0x1c), sector);
            let offset = sext16(m.lh(at(stream, 6)));
            let size = m.lw(at(frame, 0x18));
            let sector = m.lw(at(frame, 0x1c));
            m.sw(at(frame, 0x1c), sector.wrapping_add(offset));
            let shift = u32::from(m.lbu(at(record, 3))) & 7;
            let units = (i32::from(m.lh(at(stream, 6))) >> shift) as u32;
            let length = (size & 0x7_ffff).wrapping_sub(units) & 0x7_ffff;
            let word = (size & 0xfff8_0000) | length;
            m.sw(at(frame, 0x18), word);
            let queue = m.gw(CD_QUEUE);
            m.sw(at(queue, 0x168), 0);
            let flags = if file & 0x1000 != 0 { 0x1000 } else { 0 };
            m.sw(at(frame, 0x10), flags);
            if start_read(m, at(frame, 0x18), 0, 0, READ_COMPLETE, flags) != 1 {
                return 0;
            }
            if m.lh(at(stream, 2)) == 0 {
                m.sb(at(entry, 1), 2);
                return 0;
            }
            m.sb(at(entry, 1), 1);
            let timer = m.gw(TIMER);
            let start = m.lw(at(timer, 0x20));
            let queue = m.gw(CD_QUEUE);
            m.sw(at(queue, 0x170), start);
            0
        }
        1 => {
            if driver_flags(m) & 0x0100_0000 != 0 {
                let timer = m.gw(TIMER);
                let clock = m.lw(at(timer, 0x20));
                let queue = m.gw(CD_QUEUE);
                let start = m.lw(at(queue, 0x170));
                let divisor = i32::from(m.lh(at(stream, 4)));
                let elapsed = clock.wrapping_sub(start);
                let scaled = (elapsed << 8).wrapping_sub(elapsed) as i32;
                let mut volume = if divisor != 0 {
                    mainloop::checked_div(m, scaled, divisor, 0x8002_c0c0, 0x8002_c0d8)
                } else {
                    scaled >> 7
                };
                if volume < 0 {
                    volume = 0;
                }
                if volume >= 0x100 {
                    volume = 0xff;
                    m.sb(at(entry, 1), 2);
                }
                let queue = m.gw(CD_QUEUE);
                let stereo = m.lw(at(queue, 0x160));
                m.sw(at(queue, 0x164), volume as u32);
                dispatch::call(m, SET_CD_VOLUME, [volume as u32, stereo, 0, 0]);
            } else {
                let timer = m.gw(TIMER);
                let clock = m.lw(at(timer, 0x20));
                let queue = m.gw(CD_QUEUE);
                m.sw(at(queue, 0x170), clock);
            }
            let queue = m.gw(CD_QUEUE);
            if m.lw(at(queue, 0x168)) != 0 {
                m.sb(at(entry, 1), 2);
            }
            0
        }
        2 => {
            if m.lw(at(queue, 0x168)) == 0 {
                return 0;
            }
            if m.lw(at(queue, 0x16c)) == -2i32 as u32 {
                m.sb(at(entry, 1), 0);
                return 0;
            }
            if m.lw(at(stream, 8)) == 0 {
                m.sw(at(queue, 0x174), u32::MAX);
                return 1;
            }
            m.sb(at(entry, 1), 0);
            let next = m.lw(at(stream, 8));
            m.sw(at(entry, 4), next);
            0
        }
        _ => 0,
    }
}

/// 8002C1D4: request 4, read stream file `entry+2` into `entry+4` with
/// buffer size `entry+8` and start flags `entry+0xC` (bit 0x1000 only when
/// the file has it) at volume 0xFF; phase 1 as request 2.
pub fn read_stream_request(m: &mut Machine, entry: u32) -> u32 {
    framed(m, 0x28, |m, frame| match m.lbu(at(entry, 1)) {
        0 => {
            let queue = m.gw(CD_QUEUE);
            let stereo = m.lw(at(queue, 0x160));
            m.sw(at(queue, 0x164), 0xff);
            dispatch::call(m, SET_CD_VOLUME, [0xff, stereo, 0, 0]);
            let file = sext16(m.lh(at(entry, 2)));
            let mut flags = m.lw(at(entry, 0xc));
            if file & 0x1000 == 0 {
                flags &= !0x1000;
            }
            let queue = m.gw(CD_QUEUE);
            m.sw(at(queue, 0x168), 0);
            let file = sext16(m.lh(at(entry, 2)));
            m.sw(at(queue, 0x174), file);
            let file = sext16(m.lh(at(entry, 2)));
            let record = dispatch::call(m, FILE_ENTRY, [file, 0, 0, 0]);
            m.sw(at(frame, 0x10), flags);
            let destination = m.lw(at(entry, 4));
            let size = m.lw(at(entry, 8));
            if start_read(m, record, destination, size, READ_COMPLETE, flags) == 1 {
                m.sb(at(entry, 1), 1);
            }
            0
        }
        1 => wait_read(m, entry),
        _ => 0,
    })
}

/// 8002C2E4: request 5, fade the stream out over 64 frames of `[gp+0x2EC]
/// +0x20` from the queue's volume (+0x164; below 5 it becomes 0 and phase
/// 2 follows), then stop (8002AC3C(0)); once the stop is issued the pause
/// request +0x178 is cleared and the request is done (1). Nothing happens
/// while the pause request is set.
pub fn fade_out_request(m: &mut Machine, entry: u32) -> u32 {
    framed(m, 0x18, |m, _| {
        let queue = m.gw(CD_QUEUE);
        if m.lw(at(queue, 0x178)) != 0 {
            return 0;
        }
        let phase = m.lbu(at(entry, 1));
        match phase {
            0 | 1 => {
                if phase == 0 {
                    let timer = m.gw(TIMER);
                    let start = m.lw(at(timer, 0x20));
                    m.sw(at(queue, 0x170), start);
                    m.sb(at(entry, 1), 1);
                }
                let timer = m.gw(TIMER);
                let clock = m.lw(at(timer, 0x20));
                let queue = m.gw(CD_QUEUE);
                let start = m.lw(at(queue, 0x170));
                let elapsed = (clock.wrapping_sub(start) << 12) as i32 >> 6;
                let mut factor = 0x1000i32.wrapping_sub(elapsed);
                if factor >= 0x1001 {
                    factor = 0x1000;
                }
                if factor < 0 {
                    factor = 0;
                }
                let volume = m.lw(at(queue, 0x164)) as i32;
                let mut level = volume.wrapping_mul(factor) >> 12;
                if level < 5 {
                    m.sb(at(entry, 1), 2);
                    level = 0;
                    let queue = m.gw(CD_QUEUE);
                    m.sw(at(queue, 0x164), 0);
                }
                let queue = m.gw(CD_QUEUE);
                let stereo = m.lw(at(queue, 0x160));
                dispatch::call(m, SET_CD_VOLUME, [level as u32, stereo, 0, 0]);
                0
            }
            2 => {
                let issued = mainloop::cd_stop(m, 0);
                if issued != 0 {
                    let queue = m.gw(CD_QUEUE);
                    m.sw(at(queue, 0x178), 0);
                }
                issued
            }
            _ => 0,
        }
    })
}

// ---- 8002A79C: starting a read or stream -------------------------------------------

// JP-REVIEW(R0038): near function: PAL 8002A34C -> JP 8002A79C, del:4 imm:4 ins:13 op:8 reg:5, 34 instructions differ (whole routine)
/// 8002A79C(record, destination, size, callback, flags at sp+0x10): start
/// reading the file `record` (size/flags word: bits 27..28 kind, 24..26
/// length shift, 19..23 channel, 0..18 length; sector word). Returns -1
/// while the CD monitor is not ready (+0), 0 while CdSync(1) reports a
/// command still running; a record without bit 31 calls `callback(-3)` and
/// returns 1. Otherwise it resets the driver (start sector, completion
/// `callback`) and sets up by kind: 0 a data read of the word count to
/// `destination` (states 80029E74); 1 a data stream into the ring at
/// `destination` of `size` bytes (2328-byte slots with `flags & 0x2000`),
/// 2 an XA audio stream (`flags & 0x1000`: sub-data to `[gp+0x3C]`), 3 an
/// interleaved stream (sub-data modes 0x1000/0x4000), all with the stream
/// states 80029FE4 and service hook 8002A498 and an end time now + 2 +
/// sectors / 3; `flags & 0x800` asks for the MDEC check. Then it seeks
/// (80029DFC) and returns 1.
// JP-PATCH: replaced by `crate::jp::res_p3_cdstream::start_read` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn start_read(
    m: &mut Machine,
    record: u32,
    destination: u32,
    size: u32,
    callback: u32,
    flags: u32,
) -> u32 {
    crate::jp::res_p3_cdstream::start_read(m, record, destination, size, callback, flags)
}

/// 80029DFC: (re)start at the start sector +0x10: clear the deferred
/// sector `[gp+0x30C]` and the state, mark the driver open (clearing bits
/// 25, 24, 12 and 4), install 8002A200 as CdSyncCallback, clear the
/// monitor's result and the sub-data word, and CdControlF(Setloc).
pub fn seek_start(m: &mut Machine) {
    framed(m, 0x20, |m, frame| {
        let d = driver(m);
        let start = m.lw(at(d, 0x10));
        let word = m.lw(d);
        m.set_gw(DEFERRED_SECTOR, 0);
        m.sw(at(d, 0x20), 0);
        m.sw(d, (word | 0x8000_0000) & 0xfcff_efef);
        pending_sdk::cd_int_to_pos(m, start, at(frame, 0x10));
        pending_sdk::cd_sync_callback(m, SYNC_CALLBACK);
        let monitor = m.gw(CD_MONITOR);
        m.sw(at(monitor, 0x28), 0);
        m.sw(at(monitor, 0x2c), 0);
        let sub = m.gw(SUB_DATA);
        m.sw(sub, 0);
        pending_sdk::cd_control_f(m, 2, at(frame, 0x10));
    });
}

// ---- command state machines (CdSyncCallback) ----------------------------------------

/// 8002A200 (CdSyncCallback): keep the result; on an error (5) of a driver
/// not marked failed (bit 7), record error 1 and request a restart from the
/// sector after the last one read; on completion (2) or that error, run
/// the state handler +0xC with `result`.
pub fn sync_callback(m: &mut Machine, status: u32, result: u32) {
    framed(m, 0x18, |m, _| {
        keep_result(m, result);
        let status = status & 0xff;
        let run = if status == 5 {
            let d = driver(m);
            if m.lw(d) & 0x80 != 0 {
                false
            } else {
                m.sw(at(d, 0x20), 0xff);
                m.sw(at(d, 0x34), 1);
                let word = m.lw(d);
                let last = m.lw(at(d, 0x24));
                m.sw(d, (word | 0x20) & !0x10);
                if last != 0 {
                    m.sw(at(d, 0x10), last.wrapping_add(1));
                    m.sw(at(d, 0x24), 0);
                }
                true
            }
        } else {
            status == 2
        };
        if run {
            let d = driver(m);
            let handler = m.lw(at(d, 0xc));
            if handler != 0 {
                dispatch::call(m, handler, [result, 0, 0, 0]);
            }
        }
    });
}

/// The failure path shared by both state machines (80029F74/8002A188): a
/// requested restart seeks again; otherwise the driver is marked failed,
/// CdSyncCallback is removed and the completion callback gets -2.
fn restart_or_fail(m: &mut Machine, stream: bool) {
    let d = driver(m);
    let word = m.lw(d);
    if word & 0x20 != 0 {
        m.sw(d, word & !0x20);
        seek_start(m);
        return;
    }
    m.sw(d, word | 0x80);
    pending_sdk::cd_sync_callback(m, 0);
    finish(m, stream, -2i32 as u32);
}

/// Remove the service hook and the sub-data word (streams), then call the
/// completion callback +8 with `status`.
fn finish(m: &mut Machine, stream: bool, status: u32) {
    let d = driver(m);
    if stream {
        let sub = m.gw(SUB_DATA);
        m.sw(at(d, 4), 0);
        m.sw(sub, 0);
    }
    let callback = m.lw(at(d, 8));
    if callback != 0 {
        dispatch::call(m, callback, [status, 0, 0, 0]);
    }
}

/// 80029E74: data-read states on each completed command: 0 Setmode 0xA0
/// (double speed, whole sectors); 1 install 80029A44 and ReadN; 2 (ReadN
/// acknowledged) arm the first-sector timeout (300) and wait; 3 once no
/// words are left, close the driver and call the completion callback with
/// 0. Other states restart or fail.
pub fn data_states(m: &mut Machine) {
    framed(m, 0x20, |m, frame| {
        let d = driver(m);
        match m.lw(at(d, 0x20)) {
            0 => {
                m.sb(at(frame, 0x10), 0xa0);
                m.sw(at(d, 0x20), 1);
                pending_sdk::cd_control_f(m, 0xe, at(frame, 0x10));
            }
            1 => {
                m.sw(at(d, 0x24), 0);
                pending_sdk::cd_ready_callback(m, DATA_READY);
                let d = driver(m);
                m.sw(at(d, 0x20), 2);
                pending_sdk::cd_control_f(m, 6, 0);
            }
            2 => {
                m.sw(at(d, 0x30), 0x12c);
                let word = m.lw(d);
                m.sw(at(d, 0x20), 3);
                m.sw(d, word | 0x10);
            }
            3 => {
                if (m.lw(at(d, 0x14)) as i32) <= 0 {
                    let word = m.lw(d) & 0x7eff_ffff;
                    m.sw(d, word);
                    pending_sdk::cd_sync_callback(m, 0);
                    finish(m, false, 0);
                }
            }
            _ => restart_or_fail(m, false),
        }
    });
}

/// 80029FE4: stream states on each completed command: 0 Setfilter (file,
/// channel at +0x1C); 0x10 Setmode 0xE8 (double speed, XA-ADPCM, whole
/// sectors, filter) with the channel filter bit 12; 0x20 install 80029D70
/// and ReadS; 0x30 (ReadS acknowledged) CD audio on (8002999C(0)), arm the
/// first-sector timeout and resume; 0x40 when no sectors are left close the
/// driver (completion 0), else a pause takes two completions before ReadS
/// resumes. Other states restart or fail.
pub fn stream_states(m: &mut Machine) {
    framed(m, 0x20, |m, frame| {
        let d = driver(m);
        match m.lw(at(d, 0x20)) {
            0 => {
                m.sw(at(d, 0x20), 0x10);
                pending_sdk::cd_control_f(m, 0xd, at(d, 0x1c));
            }
            0x10 => {
                m.sb(at(frame, 0x10), 0xe8);
                let word = m.lw(d);
                m.sw(at(d, 0x20), 0x20);
                m.sw(d, word | 0x1000);
                pending_sdk::cd_control_f(m, 0xe, at(frame, 0x10));
            }
            0x20 => {
                m.sw(at(d, 0x24), 0);
                pending_sdk::cd_ready_callback(m, STREAM_READY);
                let d = driver(m);
                m.sw(at(d, 0x20), 0x30);
                pending_sdk::cd_control_f(m, 0x1b, 0);
            }
            0x30 => {
                mainloop::set_cd_mix(m, 0);
                let d = driver(m);
                m.sw(at(d, 0x30), 0x12c);
                let word = m.lw(d);
                m.sw(at(d, 0x20), 0x40);
                m.sw(d, (word & 0xbfff_ffff) | 0x10);
            }
            0x40 => {
                if (m.lw(at(d, 0x14)) as i32) <= 0 {
                    let word = m.lw(d) & 0x7eff_ffff;
                    m.sw(d, word);
                    pending_sdk::cd_sync_callback(m, 0);
                    finish(m, true, 0);
                    return;
                }
                let word = m.lw(d);
                if word & 0x4000_0000 == 0 {
                    return;
                }
                if word & 0x0200_0000 == 0 {
                    m.sw(d, word | 0x0200_0000);
                    return;
                }
                m.sw(d, word & 0xfcff_ffff);
                m.sw(at(d, 0x20), 0x30);
                pending_sdk::cd_control_f(m, 0x1b, 0);
            }
            _ => restart_or_fail(m, true),
        }
    });
}

// ---- data ready (CdReadyCallback) ------------------------------------------------------

/// Mark a sector arrival (bit 24) and stop the first-sector timeout (bit 4).
fn sector_arrived(m: &mut Machine) {
    let d = driver(m);
    let word = m.lw(d) | 0x0100_0000;
    m.sw(d, word);
    if word & 0x10 != 0 {
        m.sw(d, word & !0x10);
    }
}

/// 80029A44 (CdReadyCallback of data reads): keep the result; on data
/// ready (1) read the sector header (CdGetSector 3 words to frame +0x10)
/// and check its position (the start sector first, then consecutive: errors
/// 3 and 4), transfer up to 0x200 words to the destination and advance it;
/// when no words are left remove the callback and pause. Any other status
/// (error 2) or a bad position marks a restart from the sector after the
/// last one read, removes the callback and pauses.
pub fn data_ready(m: &mut Machine, status: u32, result: u32) {
    framed(m, 0x30, |m, frame| {
        keep_result(m, result);
        let mut error = 0;
        if status & 0xff != 1 {
            error = 2;
        } else {
            sector_arrived(m);
            pending_sdk::cd_get_sector(m, at(frame, 0x10), 3);
            let position = sdk::cd_pos_to_int(m, at(frame, 0x10));
            let d = driver(m);
            let last = m.lw(at(d, 0x24));
            if last == 0 {
                if position != m.lw(at(d, 0x10)) {
                    error = 3;
                }
            } else if position != last.wrapping_add(1) {
                error = 4;
            }
            let d = driver(m);
            let left = m.lw(at(d, 0x14));
            m.sw(at(d, 0x24), position);
            let words = if (left as i32) < 0x200 { left } else { 0x200 };
            let destination = m.lw(at(d, 0x18));
            pending_sdk::cd_get_sector(m, destination, words);
            let d = driver(m);
            let destination = m.lw(at(d, 0x18)).wrapping_add(words << 2);
            let left = m.lw(at(d, 0x14)).wrapping_sub(words);
            m.sw(at(d, 0x18), destination);
            m.sw(at(d, 0x14), left);
            if (left as i32) <= 0 {
                pending_sdk::cd_ready_callback(m, 0);
                pending_sdk::cd_control_f(m, 9, 0);
            }
        }
        if error != 0 {
            let d = driver(m);
            let word = m.lw(d);
            let last = m.lw(at(d, 0x24));
            m.sw(at(d, 0x20), 0xff);
            m.sw(at(d, 0x34), error);
            m.sw(d, (word | 0x20) & !0x10);
            if last != 0 {
                m.sw(at(d, 0x10), last.wrapping_add(1));
                m.sw(at(d, 0x24), 0);
            }
            pending_sdk::cd_ready_callback(m, 0);
            pending_sdk::cd_control_f(m, 9, 0);
        }
    });
}

/// 80029D70 (CdReadyCallback of streams): keep the result; a status other
/// than data ready records error 5; with the MDEC check (+0x24) and the
/// MDEC busy (DecDCTinSync(1)) the sector is deferred (`[gp+0x30C]` = 1),
/// else handled (80029BD8).
pub fn stream_ready(m: &mut Machine, status: u32, result: u32) {
    framed(m, 0x18, |m, _| {
        let monitor = m.gw(CD_MONITOR);
        keep_result(m, result);
        let status = status & 0xff;
        if status != 1 {
            let d = driver(m);
            m.sw(at(d, 0x34), 5);
        } else if m.lw(at(monitor, 0x24)) != 0 && pending_sdk::dec_dct_in_sync(m, 1) != 0 {
            m.set_gw(DEFERRED_SECTOR, status);
        } else {
            stream_sector(m);
        }
    });
}

// JP-REVIEW(R0043): near function: PAL 800297B8 -> JP 80029BD8, del:2 imm:1 ins:7 op:3 reg:7, 20 instructions differ (whole routine)
/// 80029BD8: one stream sector: read its header and subheader (3 words);
/// a data sector (submode bit 3) updates the position: sectors left = last
/// sector - position, the time of this sector and the end time (now + 2 +
/// left / 3). Its data goes to `[gp+0x3C]` (0x81 words, CdGetSector2) for
/// channel 0 with sub-data mode 0x4000, else to the ring (8002B760) unless
/// the stream is XA audio or filtered to another channel. With no sectors
/// left, CD audio off, the callback removed and a pause.
// JP-PATCH: replaced by `crate::jp::res_p3_cdstream::stream_sector` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn stream_sector(m: &mut Machine) {
    crate::jp::res_p3_cdstream::stream_sector(m)
}

// ---- 8002A498: stream service -------------------------------------------------------------

// JP-REVIEW(R0044): changed function: PAL 8002A064 -> JP 8002A498, del:14 imm:5 ins:21 op:1 reg:22, 63 instructions differ (whole routine)
/// 8002A498(status): the CD monitor's per-pass stream hook with the
/// CdSync(1) status. After a sector arrived: estimate the position (last
/// sector + 3 per frame since it) unless paused, stop the first-sector
/// timeout, and once no sectors are left or the end time passed (with the
/// last command complete and not paused) turn CD audio off and pause (1);
/// a pause/resume request (bit 6) pauses (1); while not paused keep the
/// elapsed time +0x40 (0). Before the first sector, a stream past its end
/// time or whose first-sector timeout ran out (or whose drive motor
/// stopped) is closed: CD audio off, callbacks removed, completion -1, and
/// CdControl(Pause) (CdReset(0) when refused) (1).
// JP-PATCH: replaced by `crate::jp::res_p3_cdstream::stream_service` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn stream_service(m: &mut Machine, status: u32) -> u32 {
    crate::jp::res_p3_cdstream::stream_service(m, status)
}

// ---- data-stream ring ----------------------------------------------------------------------

/// 8002B588: the driver flags word.
pub fn driver_flags(m: &mut Machine) -> u32 {
    let d = driver(m);
    m.lw(d)
}

/// 8002BDB0 (completion callback of queued reads): queue +0x168 = 1 (done),
/// +0x16C = `status`.
pub fn read_complete(m: &mut Machine, status: u32) {
    let queue = m.gw(CD_QUEUE);
    m.sw(at(queue, 0x168), 1);
    m.sw(at(queue, 0x16c), status);
}

/// 8002B610(buffer, size, form2): ring `[gp+0x40]` of `size / slot` slots
/// (slot 0x918 bytes with `form2`, else 0x800): 32-byte headers at
/// `buffer`, data after them; +0 header words (8), +4 data words per slot,
/// +8 form2, +0xC slots, +0x10/+0x14 header and data bases, +0x18 slot
/// index, +0x1C/+0x20 frame start, +0x24 frame number (-1), +0x28 sector in
/// frame, +0x2C/+0x30 write position, +0x34 frames, +0x38, +0x3C/+0x40 read
/// position. Returns `buffer`.
pub fn ring_init(m: &mut Machine, buffer: u32, size: u32, form2: u32) -> u32 {
    let slot: i32 = if form2 != 0 { 0x918 } else { 0x800 };
    let count = (size as i32) / slot;
    let ring = m.gw(RING);
    m.sw(ring, 8);
    m.sw(at(ring, 0x10), buffer);
    m.sw(at(ring, 8), form2);
    m.sw(at(ring, 0x18), 0);
    m.sw(at(ring, 0x28), 0);
    m.sw(at(ring, 0x34), 0);
    m.sw(at(ring, 0x38), 0);
    m.sw(at(ring, 0xc), count as u32);
    m.sw(
        at(ring, 0x14),
        buffer.wrapping_add((count as u32).wrapping_mul(32)),
    );
    m.sw(at(ring, 0x1c), buffer);
    let data = m.lw(at(ring, 0x14));
    m.sw(at(ring, 4), ((slot - 0x20) >> 2) as u32);
    m.sw(at(ring, 0x20), data);
    let headers = m.lw(at(ring, 0x1c));
    let data = m.lw(at(ring, 0x20));
    m.sw(at(ring, 0x24), u32::MAX);
    m.sw(at(ring, 0x2c), headers);
    m.sw(at(ring, 0x30), data);
    m.sw(at(ring, 0x3c), headers);
    m.sw(at(ring, 0x40), data);
    buffer
}

/// 8002B6D0: clear the status byte of every slot header.
pub fn ring_clear(m: &mut Machine) {
    let ring = m.gw(RING);
    let mut header = m.lw(at(ring, 0x10));
    let count = m.lw(at(ring, 0xc));
    let stride = m.lw(ring) << 2;
    if (count as i32) <= 0 {
        return;
    }
    let mut index = 0i32;
    loop {
        m.sb(header, 0);
        let ring = m.gw(RING);
        let count = m.lw(at(ring, 0xc));
        index += 1;
        header = header.wrapping_add(stride);
        if index >= count as i32 {
            break;
        }
    }
}

/// 8002B760: one data-stream sector into the ring: its 32-byte header
/// (status 1, sector in frame +1, sectors per frame +2, frame number +6)
/// goes to the write slot unless that slot is full (2) or wrapped (4). A
/// new frame starts at the frame start slot (wrapping to the ring start,
/// slot status 4, when it would not fit); the header is copied there.
/// Consecutive sectors of the frame append their data; the frame's last
/// sector marks its slots full (2), counts the frame and moves the frame
/// start. An out-of-order sector or a bad header (not 1, cleared) is
/// dropped.
pub fn ring_sector(m: &mut Machine) {
    framed(m, 0x20, |m, _| ring_sector_body(m));
}

fn ring_sector_body(m: &mut Machine) {
    let ring = m.gw(RING);
    let mut header = m.lw(at(ring, 0x2c));
    let status = m.lbu(header);
    if status == 2 || status == 4 {
        return;
    }
    let words = m.lw(ring);
    pending_sdk::cd_get_sector(m, header, words);
    if m.lbu(header) != 1 {
        m.sb(header, 0);
        return;
    }
    let ring = m.gw(RING);
    let frame = u32::from(m.lhu(at(header, 6)));
    if m.lw(at(ring, 0x24)) == frame {
        let expected = m.lw(at(ring, 0x28)).wrapping_add(1);
        if expected != u32::from(m.lbu(at(header, 1))) {
            return;
        }
    } else {
        if m.lbu(at(header, 1)) != 0 {
            return;
        }
        let start = m.lw(at(ring, 0x1c));
        m.sw(at(ring, 0x2c), start);
        let mut target = start;
        let data = m.lw(at(ring, 0x20));
        m.sw(at(ring, 0x30), data);
        let sectors = u32::from(m.lbu(at(header, 2)));
        let next = m.lw(at(ring, 0x18)).wrapping_add(sectors);
        if (m.lw(at(ring, 0xc)) as i32) < next as i32 {
            m.sb(header, 4);
            let ring = m.gw(RING);
            let base = m.lw(at(ring, 0x10));
            m.sw(at(ring, 0x1c), base);
            let data = m.lw(at(ring, 0x14));
            let start = m.lw(at(ring, 0x1c));
            m.sw(at(ring, 0x18), 0);
            m.sw(at(ring, 0x2c), start);
            target = start;
            m.sw(at(ring, 0x20), data);
            m.sw(at(ring, 0x30), data);
            if m.lbu(target) == 2 {
                return;
            }
        }
        let ring = m.gw(RING);
        let frame = u32::from(m.lhu(at(header, 6)));
        m.sw(at(ring, 0x24), frame);
        if header != target {
            let source = header;
            let count = m.lw(ring);
            header = m.lw(at(ring, 0x1c));
            let mut i = 0i32;
            if (count as i32) > 0 {
                loop {
                    let word = m.lw(at(source, (i as u32) << 2));
                    m.sw(at(target, (i as u32) << 2), word);
                    let ring = m.gw(RING);
                    let count = m.lw(ring);
                    i += 1;
                    if i >= count as i32 {
                        break;
                    }
                }
            }
        }
    }
    let ring = m.gw(RING);
    let index = u32::from(m.lbu(at(header, 1)));
    let data = m.lw(at(ring, 0x30));
    let data_words = m.lw(at(ring, 4));
    let header_words = m.lw(ring);
    m.sw(at(ring, 0x28), index);
    pending_sdk::cd_get_sector(m, data, data_words);
    let ring = m.gw(RING);
    let done = m.lw(at(ring, 0x28)).wrapping_add(1);
    if done != u32::from(m.lbu(at(header, 2))) {
        let position = m.lw(at(ring, 0x2c)).wrapping_add(header_words << 2);
        m.sw(at(ring, 0x2c), position);
        let data = m.lw(at(ring, 0x30)).wrapping_add(data_words << 2);
        m.sw(at(ring, 0x30), data);
        return;
    }
    let mut slot = m.lw(at(ring, 0x1c));
    let mut data = m.lw(at(ring, 0x20));
    let mut i = 0i32;
    if (done as i32) > 0 {
        loop {
            m.sb(slot, 2);
            slot = slot.wrapping_add(header_words << 2);
            let sectors = i32::from(m.lbu(at(header, 2)));
            i += 1;
            data = data.wrapping_add(data_words << 2);
            if i >= sectors {
                break;
            }
        }
    }
    let ring = m.gw(RING);
    let frames = m.lw(at(ring, 0x34)).wrapping_add(1);
    m.sw(at(ring, 0x34), frames);
    let sectors = u32::from(m.lbu(at(header, 2)));
    let next = m.lw(at(ring, 0x18)).wrapping_add(sectors);
    let slots = m.lw(at(ring, 0xc));
    m.sw(at(ring, 0x18), next);
    if (next as i32) < slots as i32 {
        m.sw(at(ring, 0x1c), slot);
        m.sw(at(ring, 0x20), data);
    } else {
        let base = m.lw(at(ring, 0x10));
        let data = m.lw(at(ring, 0x14));
        m.sw(at(ring, 0x18), 0);
        m.sw(at(ring, 0x1c), base);
        m.sw(at(ring, 0x20), data);
    }
    let ring = m.gw(RING);
    let start = m.lw(at(ring, 0x1c));
    let data = m.lw(at(ring, 0x20));
    m.sw(at(ring, 0x2c), start);
    m.sw(at(ring, 0x30), data);
}

// ---- host entry --------------------------------------------------------------------------

/// The host's CD interrupt: libcd's handler 800A4B48 over the attached
/// drive (see the module documentation). Returns the interrupts handled.
pub fn cd_service(m: &mut Machine) -> u32 {
    pending_sdk::cd_interrupt_handler(m)
}

// ---- calls by original address ----------------------------------------------------------

/// Routines translated here, by original address (a0..a3 in, v0 out;
/// routines without a meaningful v0 return 0). 8002A79C reads its fifth
/// argument from `m.sp + 0x10`, the caller's outgoing argument area.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let [a0, a1, a2, a3] = args;
    Some(match target {
        0x8002_be14 => read_file_request(m, a0),
        0x8002_bee4 => play_stream_request(m, a0),
        0x8002_c1d4 => read_stream_request(m, a0),
        0x8002_c2e4 => fade_out_request(m, a0),
        0x8002_a79c => {
            let flags = m.lw(at(m.sp, 0x10));
            start_read(m, a0, a1, a2, a3, flags)
        }
        0x8002_9dfc => {
            seek_start(m);
            0
        }
        0x8002_9e74 => {
            data_states(m);
            0
        }
        0x8002_9fe4 => {
            stream_states(m);
            0
        }
        0x8002_a200 => {
            sync_callback(m, a0, a1);
            0
        }
        0x8002_9a44 => {
            data_ready(m, a0, a1);
            0
        }
        0x8002_9d70 => {
            stream_ready(m, a0, a1);
            0
        }
        0x8002_9bd8 => {
            stream_sector(m);
            0
        }
        0x8002_a498 => stream_service(m, a0),
        0x8002_b588 => driver_flags(m),
        0x8002_bdb0 => {
            read_complete(m, a0);
            0
        }
        0x8002_b610 => ring_init(m, a0, a1, a2),
        0x8002_b6d0 => {
            ring_clear(m);
            0
        }
        0x8002_b760 => {
            ring_sector(m);
            0
        }
        _ => return None,
    })
}

// ---- host drive model ----------------------------------------------------------------------

/// The PAL RAM words [`CdDrive`] paces sectors by: libetc's VBlank counter
/// 0x800B6174 and VSync's record of the VBlank its last wait reached
/// 0x800B5048.
pub const CLOCK: VBlankClock = VBlankClock {
    counter: sdk::VSYNC_COUNTER,
    vsync_record: sdk::VSYNC_LAST_COUNT,
};
