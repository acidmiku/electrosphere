//! Patches of `cdstream` routines whose JP instructions differ from PAL
//! (REVIEW.md §2-§3), part 3 of the resident wave. Each is listed in
//! [`REPLACES`](super::REPLACES).
//!
//! The JP drive is paced for 60 fields per second: every PAL `sectors / 3`
//! (frames of 25 Hz at 75 sectors per second) becomes `6 * sectors / 15`
//! ([`two_fifths`]) and the position estimate `3 * fields` becomes
//! `5 * fields / 2`.
use crate::{
    cdstream::{self, DATA_STATES, STREAM_SERVICE, STREAM_STATES},
    dispatch,
    machine::{Machine, SmallData},
    mainloop::{self, pending_sdk},
    sdk,
};

/// `[gp+0x34]`: CD monitor (+0x20 pointer to the VBlank counter, +0x24 MDEC
/// check, +0x28/+0x2C last callback result).
const CD_MONITOR: u32 = 0x34;
/// `[gp+0x38]`: the driver state (`cdstream` module documentation).
const CD_DRIVER: u32 = 0x38;
/// `[gp+0x3C]`: sub-data buffer (CdGetSector2 target).
const SUB_DATA: u32 = 0x3c;

const fn at(base: u32, offset: u32) -> u32 {
    base.wrapping_add(offset)
}
fn driver(m: &mut Machine) -> u32 {
    m.gw(CD_DRIVER)
}
/// `[[gp+0x34]+0x20]`: the VBlank count.
fn now(m: &mut Machine) -> u32 {
    let monitor = m.gw(CD_MONITOR);
    let clock = m.lw(at(monitor, 0x20));
    m.lw(clock)
}
fn framed<R>(m: &mut Machine, size: u32, body: impl FnOnce(&mut Machine, u32) -> R) -> R {
    let frame = m.push_frame(size);
    let result = body(m, frame);
    m.pop_frame(size);
    result
}

/// JP `x * 6` (`sll 1; addu; sll 1`, wrapping), then `mult 0x88888889;
/// mfhi; + x6; sra 3; - (x6 >> 31)`: `6x / 15` truncated toward zero (PAL
/// `x / 3`).
fn two_fifths(x: u32) -> u32 {
    let x6 = x.wrapping_mul(6) as i32;
    let high = ((i64::from(x6) * i64::from(0x8888_8889u32 as i32)) >> 32) as i32;
    (high.wrapping_add(x6) >> 3).wrapping_sub(x6 >> 31) as u32
}

/// Mark a sector arrival (bit 24) and stop the first-sector timeout (bit 4).
fn sector_arrived(m: &mut Machine) {
    let d = driver(m);
    let word = m.lw(d) | 0x0100_0000;
    m.sw(d, word);
    if word & 0x10 != 0 {
        m.sw(d, word & !0x10);
    }
}

/// Remove the service hook and the sub-data word (streams), then call the
/// completion callback +8 with `status`.
fn finish_stream(m: &mut Machine, status: u32) {
    let d = driver(m);
    let sub = m.gw(SUB_DATA);
    m.sw(at(d, 4), 0);
    m.sw(sub, 0);
    let callback = m.lw(at(d, 8));
    if callback != 0 {
        dispatch::call(m, callback, [status, 0, 0, 0]);
    }
}

// ---- 8002A79C: starting a read or stream -------------------------------------------

/// 8002A79C (JP; PAL 8002A34C, near, `del:4 imm:4 ins:13 op:8 reg:5`):
/// start reading the file `record` (record, destination, size, callback,
/// flags at sp+0x10), as PAL, except that the end time of the stream kinds
/// 1..3 is now + 2 + `6 * sectors / 15` (PAL `sectors / 3`). Returns -1
/// while the CD monitor is not ready, 0 while CdSync(1) reports a command
/// still running, 1 otherwise.
pub fn start_read(
    m: &mut Machine,
    record: u32,
    destination: u32,
    size: u32,
    callback: u32,
    flags: u32,
) -> u32 {
    framed(m, 0x30, |m, _| {
        start_read_body(m, record, destination, size, callback, flags)
    })
}

fn stream_length(m: &mut Machine, record: u32) -> u32 {
    let word = m.lw(record);
    (word & 0x7_ffff).wrapping_shl((word >> 24) & 7)
}

fn set_channel(m: &mut Machine, record: u32) {
    let word = m.lw(record);
    let d = driver(m);
    m.sb(at(d, 0x1d), ((word >> 19) & 0x1f) as u8);
}

fn start_read_body(
    m: &mut Machine,
    record: u32,
    destination: u32,
    size: u32,
    callback: u32,
    flags: u32,
) -> u32 {
    let monitor = m.gw(CD_MONITOR);
    if m.lw(monitor) != 0 {
        return u32::MAX;
    }
    if sdk::cd_sync(m, 1, at(monitor, 0x28)) != 2 {
        return 0;
    }
    if (m.lw(record) as i32) >= 0 {
        let sub = m.gw(SUB_DATA);
        m.sw(sub, 0);
        dispatch::call(m, callback, [-3i32 as u32, 0, 0, 0]);
        return 1;
    }
    let monitor = m.gw(CD_MONITOR);
    let d = driver(m);
    m.sw(at(monitor, 0x24), 0);
    m.sw(d, 0);
    let start = m.lw(at(record, 4));
    m.sw(at(d, 0x40), 0);
    m.sw(at(d, 0x34), 0);
    m.sw(at(d, 8), callback);
    m.sw(at(d, 0x10), start);
    let kind = (m.lw(record) >> 27) & 3;
    let clock = {
        let pointer = m.lw(at(monitor, 0x20));
        m.lw(pointer)
    };
    match kind {
        0 => {
            m.sw(at(d, 0xc), DATA_STATES);
            m.sw(at(d, 4), 0);
            m.sw(at(d, 0x18), destination);
            let words = m.lw(record) & 0x7_ffff;
            m.sw(at(d, 0x14), words);
            let words = m.lw(record) & 0x7_ffff;
            let mut last = words.wrapping_sub(1) as i32;
            if last < 0 {
                last = words.wrapping_add(0x1fe) as i32;
            }
            let start = m.lw(at(d, 0x10));
            m.sw(at(d, 0x2c), start.wrapping_add((last >> 9) as u32));
        }
        1 => {
            m.sw(at(d, 0xc), STREAM_STATES);
            let word = m.lw(d);
            m.sw(at(d, 4), STREAM_SERVICE);
            m.sw(at(d, 0x18), destination);
            m.sb(at(d, 0x1c), 1);
            m.sw(d, word | 0x2000);
            set_channel(m, record);
            let length = stream_length(m, record);
            let d = driver(m);
            let start = m.lw(at(d, 0x10));
            m.sw(at(d, 0x14), length);
            m.sw(at(d, 0x2c), start.wrapping_add(length).wrapping_sub(1));
            let form2 = u32::from(flags & 0x2000 != 0);
            if flags & 0x800 != 0 {
                let monitor = m.gw(CD_MONITOR);
                m.sw(at(monitor, 0x24), 1);
            }
            start_ring(m, destination, size, form2, clock);
        }
        2 => {
            mainloop::set_cd_mix(m, 1);
            if flags & 0x1000 != 0 {
                let d = driver(m);
                let word = m.lw(d);
                m.sw(d, word | 0x4000);
            }
            let d = driver(m);
            m.sw(at(d, 0xc), STREAM_STATES);
            let word = m.lw(d);
            m.sw(at(d, 4), STREAM_SERVICE);
            m.sb(at(d, 0x1c), 1);
            m.sw(d, word | 0x400);
            set_channel(m, record);
            let length = stream_length(m, record);
            let d = driver(m);
            m.sw(at(d, 0x14), length);
            let start = m.lw(at(d, 0x10));
            let end = clock.wrapping_add(2);
            m.sw(at(d, 0x3c), end);
            m.sw(at(d, 0x2c), start.wrapping_add(length).wrapping_sub(1));
            m.sw(at(d, 0x3c), end.wrapping_add(two_fifths(length)));
        }
        _ => {
            mainloop::set_cd_mix(m, 1);
            if flags & 0x1000 != 0 {
                let d = driver(m);
                let word = m.lw(d);
                m.sw(d, word | 0x4000);
            } else if flags & 0x4000 != 0 {
                let d = driver(m);
                let word = m.lw(d);
                m.sw(d, word | 0x8000);
            }
            if flags & 0x800 != 0 {
                let monitor = m.gw(CD_MONITOR);
                m.sw(at(monitor, 0x24), 1);
            }
            let d = driver(m);
            m.sw(at(d, 0xc), STREAM_STATES);
            m.sw(at(d, 4), STREAM_SERVICE);
            let word = m.lw(d);
            let form2 = u32::from(flags & 0x2000 != 0);
            m.sb(at(d, 0x1c), 1);
            m.sw(at(d, 0x18), destination);
            m.sw(d, word | 0x2000);
            set_channel(m, record);
            let length = stream_length(m, record);
            let d = driver(m);
            let start = m.lw(at(d, 0x10));
            m.sw(at(d, 0x14), length);
            m.sw(at(d, 0x2c), start.wrapping_add(length).wrapping_sub(1));
            start_ring(m, destination, size, form2, clock);
        }
    }
    cdstream::seek_start(m);
    1
}

/// 8002AB5C..8002ABAC: ring set-up of kinds 1 and 3 and their end time,
/// now + 2 + `6 * sectors / 15`.
fn start_ring(m: &mut Machine, buffer: u32, size: u32, form2: u32, clock: u32) {
    cdstream::ring_init(m, buffer, size, form2);
    cdstream::ring_clear(m);
    let d = driver(m);
    let length = m.lw(at(d, 0x14));
    let end = clock.wrapping_add(2);
    m.sw(at(d, 0x3c), end);
    m.sw(at(d, 0x3c), end.wrapping_add(two_fifths(length)));
}

// ---- 80029BD8: one stream sector ---------------------------------------------------

/// 80029BD8 (JP; PAL 800297B8, near, `del:2 imm:1 ins:7 op:3 reg:7`): one
/// stream sector, as PAL, except that the end time of a data sector is
/// now + 2 + `6 * left / 15` (PAL `left / 3`): read its header and subheader (3
/// words); a data sector (submode bit 3) updates the position (sectors
/// left, the time of this sector, the end time); its data goes to
/// `[gp+0x3C]` (0x81 words) for channel 0 with sub-data mode 0x4000, else to
/// the ring (8002B760) unless the stream is XA audio or filtered to another
/// channel. With no sectors left, CD audio off, the callback removed and a
/// pause.
pub fn stream_sector(m: &mut Machine) {
    framed(m, 0x28, |m, frame| {
        sector_arrived(m);
        pending_sdk::cd_get_sector(m, at(frame, 0x10), 3);
        if m.lbu(at(frame, 0x16)) & 8 == 0 {
            return;
        }
        let position = sdk::cd_pos_to_int(m, at(frame, 0x10));
        let d = driver(m);
        let channel = if m.lw(d) & 0x1000 != 0 {
            u32::from(m.lbu(at(frame, 0x15)))
        } else {
            0x100
        };
        let left = m.lw(at(d, 0x2c)).wrapping_sub(position);
        m.sw(at(d, 0x14), left);
        m.sw(at(d, 0x24), position);
        let clock = now(m);
        m.sw(at(d, 0x44), clock);
        let end = clock.wrapping_add(2);
        m.sw(at(d, 0x3c), end);
        m.sw(at(d, 0x3c), end.wrapping_add(two_fifths(left)));
        if channel == 0 && m.lw(d) & 0x4000 != 0 {
            let buffer = m.gw(SUB_DATA);
            pending_sdk::cd_get_sector2(m, buffer, 0x81);
        } else {
            let d = driver(m);
            let word = m.lw(d);
            if word & 0x400 == 0 && (word & 0x2000 == 0 || channel == u32::from(m.lbu(at(d, 0x1d))))
            {
                cdstream::ring_sector(m);
            }
        }
        let d = driver(m);
        if (m.lw(at(d, 0x14)) as i32) <= 0 {
            mainloop::set_cd_mix(m, 1);
            pending_sdk::cd_ready_callback(m, 0);
            pending_sdk::cd_control_f(m, 9, 0);
        }
    });
}

// ---- 8002A498: stream service ------------------------------------------------------------

/// 8002A498 (JP; PAL 8002A064, changed, `del:14 imm:5 ins:21 op:1 reg:22`):
/// the CD monitor's per-pass stream hook with the CdSync(1) status, as PAL
/// except for the 60 Hz pacing. After a sector arrived: estimate the
/// position, last sector + `5 * fields / 2` since it (PAL `3 * frames`),
/// unless paused; stop the first-sector timeout; once no sectors are left
/// or the end time passed (with the last command complete and not paused)
/// turn CD audio off and pause (1); a pause/resume request (bit 6) pauses
/// (1); while not paused keep the elapsed time +0x40 = `6 * (position -
/// start) / 15` + now - the last sector's time (0). Before the first
/// sector, a stream past its end time or whose first-sector timeout ran out
/// (or whose drive motor stopped) is closed: CD audio off, callbacks
/// removed, completion -1, and CdControl(Pause) (CdReset(0) when refused)
/// (1).
pub fn stream_service(m: &mut Machine, status: u32) -> u32 {
    framed(m, 0x18, |m, _| stream_service_body(m, status))
}

fn stream_service_body(m: &mut Machine, status: u32) -> u32 {
    let d = driver(m);
    let word = m.lw(d);
    if word & 0x0100_0000 != 0 {
        if word & 0x4000_0000 == 0 {
            let clock = now(m);
            let mark = m.lw(at(d, 0x44));
            let elapsed = clock.wrapping_sub(mark);
            let last = m.lw(at(d, 0x24));
            let advance = (elapsed.wrapping_mul(5) as i32) >> 1;
            m.sw(at(d, 0x28), last.wrapping_add(advance as u32));
        }
        let d = driver(m);
        let word = m.lw(d);
        if word & 0x10 != 0 {
            m.sw(d, word & !0x10);
        }
        let d = driver(m);
        let running = (m.lw(at(d, 0x14)) as i32) > 0 && {
            let clock = now(m);
            (clock as i32) < (m.lw(at(d, 0x3c)) as i32)
        };
        if !running && status == 2 && m.lw(d) & 0x4000_0000 == 0 {
            mainloop::set_cd_mix(m, 1);
            let d = driver(m);
            m.sw(at(d, 0x14), 0);
            pending_sdk::cd_ready_callback(m, 0);
            pending_sdk::cd_control_f(m, 9, 0);
            return 1;
        }
        let d = driver(m);
        let word = m.lw(d);
        if word & 0x40 != 0 && status == 2 {
            let word = word & !0x40;
            m.sw(d, word);
            if word & 0x4000_0000 == 0 {
                m.sw(d, word | 0x4000_0000);
            }
            pending_sdk::cd_control_f(m, 9, 0);
            return 1;
        }
        let d = driver(m);
        if m.lw(d) & 0x4000_0000 != 0 {
            return 0;
        }
        let span = m.lw(at(d, 0x24)).wrapping_sub(m.lw(at(d, 0x10)));
        let clock = now(m);
        let mark = m.lw(at(d, 0x44));
        m.sw(
            at(d, 0x40),
            two_fifths(span).wrapping_add(clock).wrapping_sub(mark),
        );
        return 0;
    }
    let mut close = false;
    if (word as i32) < 0 {
        let inverted = !word;
        if inverted & 0x4000_0000 != 0 {
            let clock = now(m);
            close = (clock as i32) >= (m.lw(at(d, 0x3c)) as i32);
        }
        if inverted & 0x4000_0010 == 0x4000_0010 {
            let timeout = m.lw(at(d, 0x30)).wrapping_sub(1);
            m.sw(at(d, 0x30), timeout);
            let monitor = m.gw(CD_MONITOR);
            if m.lbu(at(monitor, 0x28)) & 2 == 0 || (timeout as i32) <= 0 {
                close = true;
            }
        }
    }
    if !close {
        return 0;
    }
    mainloop::set_cd_mix(m, 1);
    let d = driver(m);
    m.sw(at(d, 0x14), 0);
    pending_sdk::cd_ready_callback(m, 0);
    let d = driver(m);
    let word = m.lw(d) & 0x7fff_ffff;
    m.sw(d, word);
    pending_sdk::cd_sync_callback(m, 0);
    finish_stream(m, u32::MAX);
    let monitor = m.gw(CD_MONITOR);
    if pending_sdk::cd_control(m, 9, 0, at(monitor, 0x28)) == 0 {
        pending_sdk::cd_reset(m, 0);
    }
    1
}

#[cfg(test)]
mod tests {
    use super::two_fifths;

    #[test]
    fn two_fifths_is_the_truncated_quotient() {
        for x in [
            0u32,
            1,
            2,
            3,
            5,
            7,
            15,
            1000,
            0x7_ffff << 7,
            u32::MAX,
            0x8000_0000,
        ] {
            let x6 = x.wrapping_mul(6) as i32;
            assert_eq!(two_fifths(x) as i32, x6.wrapping_div(15), "{x:#x}");
        }
    }
}
