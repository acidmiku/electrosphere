//! Resident game routines that no wave part owns and the patch layer
//! calls, translated from the JP instructions (`ACE3.BIN`). All are exact
//! matches of PAL routines our PAL crate never translated (no PAL Rust),
//! except 8003A07C, whose PAL counterpart 80039AF8 the PAL crate only names
//! (its caller dispatches to it; nothing claims it).
//!
//! Every routine is `pub`, so patch code can call it natively; [`dispatch`]
//! claims exactly these addresses. Calls to translated routines are native.
use crate::machine::{Machine, SmallData};
use crate::mainloop::pending_sdk::cd_control;
use crate::{cdstream, loading, sdk, session, states};

fn at(base: u32, offset: u32) -> u32 {
    base.wrapping_add(offset)
}

/// `lwl rt, 3(address)` then `lwr rt, 0(address)`: the four bytes at a
/// possibly unaligned `address`, read as the two aligned words those
/// instructions load (in that order, so a bad address faults as in the
/// original).
fn load_unaligned(m: &mut Machine, address: u32) -> u32 {
    let high = m.lw(at(address, 3) & !3);
    let low = m.lw(address & !3);
    match address & 3 {
        0 => low,
        shift => (low >> (8 * shift)) | (high << (8 * (4 - shift))),
    }
}

// ---- pads and TIM images --------------------------------------------------------------

/// 80027064 (JP; PAL 80026BC4, exact): stop the pads: PadStopCom
/// ([`res_sdk::pad_stop_com`](super::res_sdk::pad_stop_com), the host call
/// `PAD_STOP_COM`). Its `v0` is PadStopCom's (the kernel's).
pub fn stop_pads(m: &mut Machine) {
    m.push_frame(0x18);
    super::res_sdk::pad_stop_com(m);
    m.pop_frame(0x18);
}

/// 80010030 / 80010048: "Can't open TIM [%08x]\n" and "Can't read TIM
/// [%08x]\n".
const OPEN_TIM_FAILED: u32 = 0x8001_0030;
const READ_TIM_FAILED: u32 = 0x8001_0048;

/// 80027084 (JP; PAL 80026BE4, exact): upload the TIM at `tim` with its
/// image at `x`, `y` and its CLUT at `clut_x`, `clut_y` (the fifth argument,
/// the caller's frame +0x10) instead of the positions the TIM gives:
/// OpenTIM, ReadTIM into the frame's TIM_IMAGE (+0x10), then for the CLUT
/// (when the TIM has one) and the image (when it has one) a copy of the
/// TIM's RECT in the frame (+0x28) with the new position, which the
/// TIM_IMAGE then points at, and LoadImage. A failed OpenTIM or ReadTIM
/// prints the TIM's first word (printf host calls with the register
/// arguments the original passes: y and clut_x stay in a2/a3, which
/// OpenTIM and a failing ReadTIM leave alone). Returns the image LoadImage's result,
/// or 0 when the TIM has no image.
pub fn upload_tim_at(m: &mut Machine, tim: u32, x: u32, y: u32, clut_x: u32, clut_y: u32) -> u32 {
    let frame = m.push_frame(0x48);
    if sdk::open_tim(m, tim) != 0 {
        let id = m.lw(tim);
        sdk::printf(m, [OPEN_TIM_FAILED, id, y, clut_x]);
    }
    let image = at(frame, 0x10);
    if sdk::read_tim(m, image) == 0 {
        let id = m.lw(tim);
        sdk::printf(m, [READ_TIM_FAILED, id, y, clut_x]);
    }
    let rect = at(frame, 0x28);
    // (TIM_IMAGE field, position) of the CLUT, then of the image: +4/+8
    // CLUT RECT and data, +0xC/+0x10 image RECT and data.
    let mut v0 = 0;
    for (field, (px, py)) in [(4, (clut_x, clut_y)), (0xc, (x, y))] {
        let data = m.lw(at(image, field + 4));
        v0 = data;
        if data == 0 {
            continue;
        }
        let source = m.lw(at(image, field));
        let xy = load_unaligned(m, source);
        let wh = load_unaligned(m, at(source, 4));
        m.sw(rect, xy);
        m.sw(at(rect, 4), wh);
        let pixels = m.lw(at(image, field + 4));
        m.sh(rect, px as u16);
        m.sh(at(rect, 2), py as u16);
        m.sw(at(image, field), rect);
        v0 = sdk::load_image(m, rect, pixels);
    }
    m.pop_frame(0x48);
    v0
}

// ---- the CD: disc swaps and streams ---------------------------------------------------

/// `[gp+0x34]`: the CD monitor (8002A2C0: +0 state, +4 step, +0xC error,
/// +0x28 CdSync result).
const CD_MONITOR: u32 = 0x34;
/// `[gp+0x48]`: the CD command queue (+0 read, +4 write, +0x148 pending
/// flag, +0x174 current track).
const CD_QUEUE: u32 = 0x48;

/// 8002B46C (JP; PAL 8002B044, exact): after a disc swap, start the drive
/// again: with the shell closed (CdSync result bit 4 clear), error 1 or 2
/// and the monitor in state 2 at step 2, CdControl(7 = Standby, none, the
/// result bytes +0x28); once accepted, the monitor's state and step 0, and
/// 1. Otherwise 0.
pub fn restart_drive(m: &mut Machine) -> u32 {
    let monitor = m.gw(CD_MONITOR);
    m.push_frame(0x18);
    let v0 = 'done: {
        if m.lbu(at(monitor, 0x28)) & 0x10 != 0 {
            break 'done 0;
        }
        if m.lw(at(monitor, 0xc)).wrapping_sub(1) >= 2 {
            break 'done 0;
        }
        let state = m.lw(monitor);
        if state != 2 || m.lw(at(monitor, 4)) != state {
            break 'done 0;
        }
        if cd_control(m, 7, 0, at(monitor, 0x28)) == 0 {
            break 'done 0;
        }
        let monitor = m.gw(CD_MONITOR);
        m.sw(monitor, 0);
        m.sw(at(monitor, 4), 0);
        1
    };
    m.pop_frame(0x18);
    v0
}

/// 8002B528 (JP; PAL 8002B100, exact): the CD monitor's disc-swap state
/// for the "insert disc" screens. State 2 (waiting for the drive): 2 once
/// the step is 2, else 1; state 3 (identifying the disc): 3 while the step
/// is 0, else 1; state 4 (re-reading the disc header): 1; any other state
/// (0, normal): 0.
pub fn disc_state(m: &mut Machine) -> u32 {
    let monitor = m.gw(CD_MONITOR);
    let state = m.lw(monitor);
    match state {
        2 => {
            if m.lw(at(monitor, 4)) == state {
                2
            } else {
                1
            }
        }
        3 => {
            if m.lw(at(monitor, 4)) == 0 {
                3
            } else {
                1
            }
        }
        _ => u32::from(state == 4),
    }
}

/// 8002CAE4 (JP; PAL 8002C6BC, exact): the time of the stream of `track`:
/// while it is the queue's current track, the queue holds requests (read
/// index below the write index) and none is pending, and the stream
/// driver's flags (8002B588) have bits 31 and 24 set, the driver's elapsed
/// time (8002B59C); otherwise -1.
pub fn stream_time(m: &mut Machine, track: u32) -> u32 {
    let queue = m.gw(CD_QUEUE);
    m.push_frame(0x18);
    let v0 = 'done: {
        if m.lw(at(queue, 0x174)) != track {
            break 'done u32::MAX;
        }
        let read = m.lw(queue) as i32;
        let write = m.lw(at(queue, 4)) as i32;
        if read >= write || m.lw(at(queue, 0x148)) != 0 {
            break 'done u32::MAX;
        }
        if cdstream::driver_flags(m) & 0x8100_0000 != 0x8100_0000 {
            break 'done u32::MAX;
        }
        states::stream_pause_time(m)
    };
    m.pop_frame(0x18);
    v0
}

/// 8002CBA4 (JP; PAL 8002C77C, exact): the kind of file `file`: bits 29..30
/// of its file-table record's first word (8002995C).
pub fn stream_kind(m: &mut Machine, file: u32) -> u32 {
    m.push_frame(0x18);
    let entry = loading::file_entry(m, file);
    let word = m.lw(entry);
    m.pop_frame(0x18);
    (word >> 29) & 3
}

// ---- sound ----------------------------------------------------------------------------

/// `[gp+0x314]`: the sound request block.
const SOUND_REQUEST: u32 = 0x314;
/// 800C65F0: the SpuReverbAttr of the sound set-up (mask, mode, depths).
const REVERB_ATTR: u32 = 0x800c_65f0;
/// 800C5830: the 24 voice records of 0x14 bytes (+3 in use).
const VOICES: u32 = 0x800c_5830;
/// 800C5A10: the sound definitions of 0x14 bytes (+8 pitch).
const SOUND_DEFINITIONS: u32 = 0x800c_5a10;
/// 800C6550: eight delayed sounds of 4 bytes {sound, -, frames (-1: free)}.
const DELAYED_SOUNDS: u32 = 0x800c_6550;

/// 8002D544 (JP; PAL 8002D11C, exact): whether the sound request block's
/// word +4 has any of its low 24 bits set.
pub fn sound_request_busy(m: &mut Machine) -> u32 {
    let block = m.gw(SOUND_REQUEST);
    u32::from(m.lw(at(block, 4)) & 0x00ff_ffff != 0)
}

/// 8002DA94 (JP; PAL 8002D66C, exact): turn the reverb on:
/// SpuClearReverbWorkArea(the attribute's mode, +4), the attribute's mask
/// 6 (both depths) and SpuSetReverbDepth, SpuSetReverb(1),
/// SpuSetReverbVoice(1, all 24 voices). Returns the last one's result.
pub fn reverb_on(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let mode = m.lw(at(REVERB_ATTR, 4));
    sdk::spu_clear_reverb_work_area(m, mode);
    m.sw(REVERB_ATTR, 6);
    sdk::spu_set_reverb_depth(m, REVERB_ATTR);
    sdk::spu_set_reverb(m, 1);
    let v0 = sdk::spu_set_reverb_voice(m, 1, 0x00ff_ffff);
    m.pop_frame(0x20);
    v0
}

/// 8002DD7C (JP; PAL 8002D954, exact): voice `voice`'s in-use byte (+3),
/// or 0xFF for the voice 0xFF (none).
pub fn voice_in_use(m: &mut Machine, voice: u32) -> u32 {
    if voice == 0xff {
        return 0xff;
    }
    u32::from(m.lbu(at(at(VOICES, voice.wrapping_mul(0x14)), 3)))
}

/// 8002DE68 (JP; PAL 8002DA40, exact): sound `sound`'s pitch, the signed
/// halfword +8 of its definition.
pub fn sound_pitch(m: &mut Machine, sound: u32) -> u32 {
    m.lh(at(at(SOUND_DEFINITIONS, sound.wrapping_mul(0x14)), 8)) as i32 as u32
}

/// 8002F19C (JP; PAL 8002ED80, exact): queue sound `sound` to start in
/// `frames` frames in the first free delayed-sound slot (frames -1): its
/// index, or -1 when all eight are taken.
pub fn delay_sound(m: &mut Machine, sound: u32, frames: u32) -> u32 {
    for slot in 0..8u32 {
        let entry = at(DELAYED_SOUNDS, slot << 2);
        if m.lh(at(entry, 2)) == -1 {
            m.sb(entry, sound as u8);
            m.sh(at(entry, 2), frames as u16);
            return slot;
        }
    }
    u32::MAX
}

// ---- flight -----------------------------------------------------------------------------

/// 800C6D60: the lock-on block (+0xE the locked entity).
const LOCK: u32 = 0x800c_6d60;
/// 800C7170: the radio indicator block (+0 timer, +2 shown, +3 portrait,
/// +8/+0xA position).
const INDICATOR: u32 = 0x800c_7170;
/// `[gp+0x4A8]`: set when a queued radio message has played.
const MESSAGE_DONE: u32 = 0x4a8;

/// 8003A07C (JP; PAL 80039AF8, exact): reset the lock-on block: +9 0xFF,
/// +0xC 0x83, the halfword +6 and the bytes +8, +0xA, +0xB, +0xD, +0xF and
/// +0x11 zero. Returns the block's address.
pub fn reset_lock(m: &mut Machine) -> u32 {
    m.sb(at(LOCK, 9), 0xff);
    m.sb(at(LOCK, 0xf), 0);
    m.sh(at(LOCK, 6), 0);
    m.sb(at(LOCK, 8), 0);
    m.sb(at(LOCK, 0xa), 0);
    m.sb(at(LOCK, 0xb), 0);
    m.sb(at(LOCK, 0xc), 0x83);
    m.sb(at(LOCK, 0xd), 0);
    m.sb(at(LOCK, 0x11), 0);
    LOCK
}

/// 80041920 (JP; PAL 80040F70, exact): show radio portrait `portrait` at
/// `x`, `y`. Portrait 0 closes the indicator (8004197C(1); that routine's
/// translation is void: 0). The portrait already shown (`portrait` - 1 at
/// +3) changes nothing and returns it; otherwise the indicator is shown
/// (+2 1) with its timer cleared, the position and `portrait` - 1 stored,
/// and 1 returned.
pub fn show_portrait(m: &mut Machine, portrait: u32, x: u32, y: u32) -> u32 {
    m.push_frame(0x18);
    let v0 = if portrait == 0 {
        session::indicator_close(m, 1);
        0
    } else {
        let index = portrait.wrapping_sub(1);
        let shown = u32::from(m.lbu(at(INDICATOR, 3)));
        if shown == index {
            shown
        } else {
            m.sb(at(INDICATOR, 2), 1);
            m.sh(INDICATOR, 0);
            m.sh(at(INDICATOR, 8), x as u16);
            m.sh(at(INDICATOR, 0xa), y as u16);
            m.sb(at(INDICATOR, 3), index as u8);
            1
        }
    };
    m.pop_frame(0x18);
    v0
}

/// 80056A44 (JP; PAL 800551B8, exact): the completion routine of a queued
/// radio message: `[gp+0x4A8]` = 1; returns 1.
pub fn message_done(m: &mut Machine) -> u32 {
    m.set_gw(MESSAGE_DONE, 1);
    1
}

/// 80094380 (JP; PAL 80092BA4, exact): the effect of session mode 0x48 at
/// step `step`: at step 500, spawn object group 0 (8009423C; its
/// translation is void: 0); any other step returns 500.
pub fn spawn_at_500(m: &mut Machine, step: u32) -> u32 {
    m.push_frame(0x18);
    let v0 = if step == 0x1f4 {
        session::spawn_group(m, 0);
        0
    } else {
        0x1f4
    };
    m.pop_frame(0x18);
    v0
}

/// The routines of this file by JP address.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let [a0, a1, a2, a3] = args;
    Some(match target {
        0x8002_7064 => {
            stop_pads(m);
            0
        }
        0x8002_7084 => {
            let clut_y = m.lw(at(m.sp, 0x10));
            upload_tim_at(m, a0, a1, a2, a3, clut_y)
        }
        0x8002_b46c => restart_drive(m),
        0x8002_b528 => disc_state(m),
        0x8002_cae4 => stream_time(m, a0),
        0x8002_cba4 => stream_kind(m, a0),
        0x8002_d544 => sound_request_busy(m),
        0x8002_da94 => reverb_on(m),
        0x8002_dd7c => voice_in_use(m, a0),
        0x8002_de68 => sound_pitch(m, a0),
        0x8002_f19c => delay_sound(m, a0, a1),
        0x8003_a07c => reset_lock(m),
        0x8004_1920 => show_portrait(m, a0, a1, a2),
        0x8005_6a44 => message_done(m),
        0x8009_4380 => spawn_at_500(m, a0),
        _ => return None,
    })
}
