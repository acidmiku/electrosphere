//! JP overlay 4, part 1: the unreferenced music player (800CD734,
//! 800CD780; PAL overlay 4's earlier one, 800CC4D4 and 800CC520, which PAL
//! does not translate), translated from the JP instructions. No call, jump
//! or code pointer reaches it in the resident program or any overlay.
//!
//! Its block `[0x800D1518]` (0x18 bytes): +0 flags (1 playing, 2 paused),
//! +4 the playing track, +8 the chosen track (0..0x21), +0xC the volume,
//! +0x10 the CD audio volume, +0x14 mono. Its screen is the resident debug
//! print 80027AE0 (untranslated: the development fallback interprets it).
use super::ov3_p1::{call, ctx, framed, hi_s, lbu, lh, lhu, slt, sra};
use super::ov4_p1::MODULE;
use crate::common::loaded_module;
use crate::machine::Machine;

/// `[0x800D1518]`: the music player's block.
const PLAYER: u32 = 0x800d_1518;
/// 0x800CF2B0: the 34 stream requests (12 bytes each); 0x800CF448 the
/// track names.
const REQUESTS: u32 = 0x800c_f2b0;
const NAMES: u32 = 0x800c_f448;
/// The strings of the screen.
const TITLE: u32 = 0x800c_c0fc;
const PAUSE: u32 = 0x800c_c118;
const PLAY: u32 = 0x800c_c124;
const SEARCH: u32 = 0x800c_c130;
const BLANK: u32 = 0x800c_c13c;
const STOPPED: u32 = 0x800c_c148;
const TRACK: u32 = 0x800c_c154;
const NEWLINE: u32 = 0x800c_c170;
const VOLUME: u32 = 0x800c_c174;
const AT_VOLUME: u32 = 0x800c_c18c;
const MONO: u32 = 0x800c_c1a8;
const STEREO: u32 = 0x800c_c1b0;
const TRANSFER: u32 = 0x800c_c1b8;

/// Resident routines (JP addresses): the arena, the debug print, the CD
/// audio settings (volume, mono) and their application, the resource
/// wait, the sound volume, the stream status and play time, the stream
/// request queue, pause, stop, the transfer record.
const ALLOC: u32 = 0x8002_7410;
const PRINT: u32 = 0x8002_7ae0;
const CD_VOLUME: u32 = 0x8002_bd9c;
const CD_MONO: u32 = 0x8002_bd88;
const SET_CD_VOLUME: u32 = 0x8002_bd34;
const SET_CD_MONO: u32 = 0x8002_bd28;
const APPLY_CD_AUDIO: u32 = 0x8002_bd5c;
const WAIT_RESOURCES: u32 = 0x8002_c580;
const SET_VOLUME: u32 = 0x8002_d844;
const DRIVER_FLAGS: u32 = 0x8002_b588;
const PLAY_TIME: u32 = 0x8002_b59c;
const TRANSFER_RECORD: u32 = 0x8002_b5b0;
const QUEUE_REQUEST: u32 = 0x8002_c8d8;
const PAUSE_SOUND: u32 = 0x8002_c6f4;
const STOP_SOUND: u32 = 0x8002_c71c;

/// 800CD734 (JP; PAL 800CC4D4, exact; no caller): allocate the player's
/// block: stopped, track 0, volume 0x3FFF, CD volume 0xFF, stereo. Returns
/// it.
pub fn music_start(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let b = call(m, ALLOC, [0x18, 0, 0, 0]);
        m.sw(b.wrapping_add(0xc), 0x3fff);
        m.sw(b.wrapping_add(4), 0);
        m.sw(b.wrapping_add(8), 0);
        m.sw(b, 0);
        m.sw(b.wrapping_add(0x10), 0xff);
        m.sw(b.wrapping_add(0x14), 0);
        m.sw(PLAYER, b);
        b
    })
}

/// The player's block.
fn player(m: &mut Machine) -> u32 {
    m.lw(PLAYER)
}

/// Queue track +8's stream request (12-byte records at 0x800CF2B0).
fn queue_track(m: &mut Machine) {
    let b = player(m);
    let track = m.lw(b.wrapping_add(8));
    let request = (track << 1).wrapping_add(track) << 2;
    call(m, QUEUE_REQUEST, [request.wrapping_add(REQUESTS), 1, 0, 0]);
}

/// 800CD780 (JP; PAL 800CC520, near: the time display for 60 fields a
/// second; no caller): one frame of the music player. Pad: up/down
/// (+0x130 bits 4/8) choose the track (0..0x21, wrapping), left/right
/// (+0x128 bits 1/2) the volume by 0x80 (0..0x7FFF), bits 0x100/0x200 the
/// CD volume (0..0xFF), 0x800 toggles mono; START (0x10) plays (or pauses
/// and resumes), SELECT (0x80) stops. A changed track starts playing when
/// not held. Prints the title, the state, the track and time, the volumes
/// and the transfer record through 80027AE0. Returns the last print's
/// result (0 without a transfer record).
///
/// The prints of fixed strings pass the registers' leftover values as
/// a1..a3; the print stores them only into this frame's argument area
/// (the stack) and reads none of them, so they are passed as 0 here.
pub fn music_frame(m: &mut Machine) -> u32 {
    framed(m, 0x50, |m, sp| {
        let mut request = 0u32;
        for (bit, step) in [(4u32, u32::MAX), (8, 1)] {
            let context = ctx(m);
            if m.lw(context.wrapping_add(0x130)) & bit != 0 {
                let b = player(m);
                let track = m.lw(b.wrapping_add(8)).wrapping_add(step);
                m.sw(b.wrapping_add(8), track);
            }
        }
        let context = ctx(m);
        let b = player(m);
        let held = m.lw(context.wrapping_add(0x128));
        let mut volume = m.lw(b.wrapping_add(0xc));
        if held & 1 != 0 {
            volume = volume.wrapping_add(0x80);
        }
        if held & 2 != 0 {
            volume = volume.wrapping_sub(0x80);
        }
        let mut cd_volume = call(m, CD_VOLUME, [0; 4]);
        let context = ctx(m);
        let held = m.lw(context.wrapping_add(0x128));
        if held & 0x100 != 0 {
            cd_volume = cd_volume.wrapping_sub(1);
        }
        if held & 0x200 != 0 {
            cd_volume = cd_volume.wrapping_add(1);
        }
        let mut mono = call(m, CD_MONO, [0; 4]);
        let context = ctx(m);
        let pressed = m.lw(context.wrapping_add(0x12c));
        if pressed & 0x800 != 0 {
            mono ^= 1;
        }
        if pressed & 0x10 != 0 {
            let b = player(m);
            let flags = m.lw(b);
            request = 1;
            if flags & 1 != 0 {
                request = 2;
            }
        }
        let context = ctx(m);
        if m.lw(context.wrapping_add(0x12c)) & 0x80 != 0 {
            let b = player(m);
            if m.lw(b) & 1 != 0 {
                request = 3;
            }
        }
        let busy = call(m, WAIT_RESOURCES, [1, 0, 0, 0]);
        if busy == 0 {
            let b = player(m);
            if m.lw(b) & 1 != 0 {
                let track = m.lw(b.wrapping_add(8)).wrapping_add(1);
                m.sw(b.wrapping_add(8), track);
            }
        }
        let mut b = player(m);
        let track = m.lw(b.wrapping_add(8));
        let below = if slt(track, 0) {
            m.sw(b.wrapping_add(8), 0x21);
            b = player(m);
            slt(m.lw(b.wrapping_add(8)), 0x22)
        } else {
            slt(track, 0x22)
        };
        if !below {
            m.sw(b.wrapping_add(8), 0);
        }
        if slt(volume, 0) {
            volume = 0;
        }
        if slt(0x7fff, volume) {
            volume = 0x7fff;
        }
        let b = player(m);
        if volume != m.lw(b.wrapping_add(0xc)) {
            call(m, SET_VOLUME, [volume, 0, 0, 0]);
            let b = player(m);
            m.sw(b.wrapping_add(0xc), volume);
        }
        let b = player(m);
        if m.lw(b.wrapping_add(0x10)) != cd_volume {
            if slt(cd_volume, 0) {
                cd_volume = 0;
            }
            if !slt(cd_volume, 0x100) {
                cd_volume = 0xff;
            }
            call(m, SET_CD_VOLUME, [cd_volume, 0, 0, 0]);
            call(m, APPLY_CD_AUDIO, [0; 4]);
            let b = player(m);
            m.sw(b.wrapping_add(0x10), cd_volume);
        }
        let b = player(m);
        if m.lw(b.wrapping_add(0x14)) != mono {
            call(m, SET_CD_MONO, [mono, 0, 0, 0]);
            call(m, APPLY_CD_AUDIO, [0; 4]);
            let b = player(m);
            m.sw(b.wrapping_add(0x14), mono);
        }
        let status = call(m, DRIVER_FLAGS, [0; 4]) & 0x4000_0000;
        let b = player(m);
        let flags = m.lw(b);
        m.sw(b, if status != 0 { flags | 2 } else { flags & !2 });
        match request {
            2 => {
                let b = player(m);
                let resume = u32::from(m.lw(b) & 2 == 0);
                call(m, PAUSE_SOUND, [resume, 0, 0, 0]);
            }
            1 if busy == 0 => {
                queue_track(m);
                let b = player(m);
                let flags = m.lw(b) | 1;
                let track = m.lw(b.wrapping_add(8));
                m.sw(b.wrapping_add(4), track);
                m.sw(b, flags);
            }
            3 => {
                call(m, STOP_SOUND, [0, 0, 0, 0]);
                let b = player(m);
                let flags = m.lw(b) & !1 & !2;
                m.sw(b, flags);
            }
            _ => {}
        }
        let b = player(m);
        let playing = m.lw(b.wrapping_add(4));
        let chosen = m.lw(b.wrapping_add(8));
        if playing != chosen && slt(busy, 2) {
            let context = ctx(m);
            if m.lw(context.wrapping_add(0x128)) & 0xc == 0 && request == 0 {
                call(m, STOP_SOUND, [0, 0, 0, 0]);
                queue_track(m);
                let b = player(m);
                let flags = m.lw(b);
                let track = m.lw(b.wrapping_add(8));
                m.sw(b.wrapping_add(4), track);
                m.sw(b, (flags | 1) & !2);
            }
        }
        call(m, PRINT, [TITLE, 0, 0, 0]);
        let b = player(m);
        let flags = m.lw(b);
        let label = if flags & 1 == 0 {
            STOPPED
        } else if flags & 2 != 0 {
            PAUSE
        } else if m.lw(b.wrapping_add(4)) == m.lw(b.wrapping_add(8)) {
            PLAY
        } else {
            let context = ctx(m);
            if m.lw(context.wrapping_add(0x20)) & 4 != 0 {
                SEARCH
            } else {
                BLANK
            }
        };
        call(m, PRINT, [label, 0, 0, 0]);
        let b = player(m);
        let paused = m.lw(b) & 2 != 0;
        let show = !paused || {
            let context = ctx(m);
            m.lw(context.wrapping_add(0x20)) & 0x10 != 0
        };
        if show {
            let time = call(m, PLAY_TIME, [0; 4]);
            let hours = hi_s(time, 0x91a2_b3c5);
            let b = player(m);
            let playing = m.lw(b.wrapping_add(4));
            let name = m.lw((playing << 2).wrapping_add(NAMES));
            let seconds = hi_s(time, 0x8888_8889);
            m.sw(sp.wrapping_add(0x10), name);
            let sign = sra(time, 31);
            let track = m.lw(b.wrapping_add(8));
            let minutes = sra(hours.wrapping_add(time), 11).wrapping_sub(sign);
            let whole = ((minutes << 4).wrapping_sub(minutes)) << 2;
            let rest = sra(seconds.wrapping_add(time), 5)
                .wrapping_sub(sign)
                .wrapping_sub(whole);
            call(m, PRINT, [TRACK, track, minutes, rest]);
        }
        call(m, PRINT, [NEWLINE, 0, 0, 0]);
        let b = player(m);
        let volume = m.lw(b.wrapping_add(0xc));
        let scaled = ((((volume << 1).wrapping_add(volume)) << 3).wrapping_add(volume)) << 2;
        let percent =
            sra(hi_s(scaled, 0x8001_0003).wrapping_add(scaled), 14).wrapping_sub(sra(scaled, 31));
        call(m, PRINT, [VOLUME, percent, volume, 0]);
        let b = player(m);
        let cd_volume = m.lw(b.wrapping_add(0x10));
        let scaled =
            ((((cd_volume << 1).wrapping_add(cd_volume)) << 3).wrapping_add(cd_volume)) << 2;
        let mono = m.lw(b.wrapping_add(0x14));
        let percent =
            sra(hi_s(scaled, 0x8080_8081).wrapping_add(scaled), 7).wrapping_sub(sra(scaled, 31));
        let mode = if mono == 0 { STEREO } else { MONO };
        let cd_volume = m.lw(b.wrapping_add(0x10));
        call(m, PRINT, [AT_VOLUME, percent, cd_volume, mode]);
        let record = call(m, TRANSFER_RECORD, [0; 4]);
        if record == 0 {
            return 0;
        }
        let words = lh(m, record.wrapping_add(8));
        let first = lhu(m, record);
        let second = lh(m, record.wrapping_add(2));
        m.sw(sp.wrapping_add(0x10), words);
        for (i, offset) in (0xa..0x10u32).enumerate() {
            let value = lbu(m, record.wrapping_add(offset));
            m.sw(sp.wrapping_add(0x14 + 4 * i as u32), value);
        }
        let third = m.lw(record.wrapping_add(4));
        call(m, PRINT, [TRANSFER, first, second, third])
    })
}

/// The routines of this file by JP address, while module 4 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, _args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    Some(match target {
        0x800c_d734 => music_start(m),
        0x800c_d780 => music_frame(m),
        _ => return None,
    })
}
