//! Patches of `mainloop` routines whose JP instructions differ from PAL
//! (REVIEW.md §2, §3b), part 3 of the resident wave. Each is listed in
//! [`REPLACES`](super::REPLACES).
//!
//! The JP context's sound bytes moved down by 4 (context +0x398 effects
//! volume, +0x399 CD volume of the music tracks, +0x39A mono flag; PAL
//! +0x39C/+0x39D/+0x39E).
use crate::{
    common::CONTEXT,
    machine::{Machine, SmallData},
    mainloop::{self, axis_curve, calibrate_axes, checked_div, flags, mix_vibration, pending_sdk},
    sdk, shared,
};

/// `[gp+0x34]`: CD monitor (+0x28 CdSync result).
const CD_MONITOR: u32 = 0x34;
/// `[gp+0x38]`: CD stream state (+0 flags, +4 sync hook).
const CD_STREAM: u32 = 0x38;
/// `[gp+0x48]`: CD command queue (+0x174 current track, +0x178 pause request).
const CD_QUEUE: u32 = 0x48;
/// Sixteen 32-frame on/off patterns for the small vibration motor.
const VIBRATION_PATTERNS: u32 = 0x800b_0340;
/// 24 voice records of 0x14 bytes (`mainloop` documentation).
const VOICES: u32 = 0x800c_5830;
const VOICE_STRIDE: u32 = 0x14;
const VOICE_COUNT: u32 = 24;
/// Sound definitions, 0x14 bytes.
const SOUNDS: u32 = 0x800c_5a10;

/// JP context +0x398: effects volume (PAL +0x39C).
const EFFECTS_VOLUME: u32 = 0x398;
/// JP context +0x399: CD volume of the music tracks (PAL +0x39D).
const MUSIC_VOLUME: u32 = 0x399;
/// JP context +0x39A bit 0: mono (PAL +0x39E).
const MONO: u32 = 0x39a;

fn voice(index: u32) -> u32 {
    VOICES.wrapping_add(index.wrapping_mul(VOICE_STRIDE))
}

fn sound(id: u32) -> u32 {
    SOUNDS.wrapping_add(id.wrapping_mul(0x14))
}

// ---- 80028C24: pad input mapping ------------------------------------------------------------

/// 80028C24 (JP; PAL 80028784, near, `del:32 imm:1`): map the pad state
/// `pad` through the button configuration `config` into the input block
/// `input`, as PAL, except that JP has one tap/hold detector on the repeat
/// flag 0x10 (button bit 0x200000, counter +0x39, hold bit 0x2000): PAL's
/// second one (bit 0x400000, counter +0x3B) is gone.
pub fn map_pad_input(m: &mut Machine, pad: u32, input: u32, config: u32) {
    let frame = m.push_frame(0x38);
    map_pad_input_frame(m, pad, input, config, frame);
    m.pop_frame(0x38);
}

fn map_pad_input_frame(m: &mut Machine, pad: u32, input: u32, config: u32, frame: u32) {
    let axes = frame + 0x10;
    let mut buttons = u32::from(m.lhu(pad + 0xe));
    m.sw(input + 0x18, 0);
    m.sh(input + 0x36, 0);
    let mut held: u32 = 0;
    let mut config = config;
    match m.lbu(pad + 0xc) {
        0x41 | 0x43 => m.sw(input + 4, 1),
        0x53 | 0x73 => {
            let (offset, class) = if m.lbu(pad + 0xc) == 0x73 {
                if m.lhu(pad + 0xa) == 7 {
                    (0xb8, 4)
                } else {
                    (0x5c, 2)
                }
            } else {
                (0x114, 8)
            };
            config = config.wrapping_add(offset);
            m.sw(input + 4, class);
            map_axes(m, pad, axes, input, config);
            let (first, second, limit) = if m.lbu(pad + 0xc) == 0x53 {
                (axes, axes + 2, 0x600)
            } else {
                (axes + 4, axes + 6, 0xeaa)
            };
            let x = i32::from(m.lh(first));
            if x < -limit {
                held |= 1;
            }
            if limit < x {
                held |= 2;
            }
            let y = i32::from(m.lh(second));
            if y < -limit {
                held |= 4;
            }
            if limit < y {
                held |= 8;
            }
        }
        0x23 => {
            m.sw(input + 4, 0x10);
            config = config.wrapping_add(0x170);
            map_axes(m, pad, axes, input, config);
            if m.lh(axes) > 0x600 {
                buttons |= 0x4000;
            }
            if m.lh(axes + 6) > 0x600 {
                buttons |= 0x8000;
            }
            if m.lh(axes + 4) > 0x600 {
                buttons |= 0x400;
            }
            if buttons & 0x400 != 0 {
                buttons |= 0x100;
            }
            if buttons & 0x800 != 0 {
                buttons |= 0x200;
            }
        }
        kind => {
            buttons = 0;
            m.sw(input + 4, if kind == 0 { 0 } else { 0x20 });
            for offset in [0x10, 0xc, 8, 0x18, 0x24, 0x20, 0x1c, 0x30, 0x2c, 0x28] {
                m.sw(input + offset, 0);
            }
            for offset in [0x38, 0x3b, 0x3a, 0x39] {
                m.sb(input + offset, 0);
            }
        }
    }
    let event = m.lbu(pad + 0xd);
    let bit = if event & 1 != 0 {
        2
    } else if event & 2 != 0 {
        8
    } else if event & 8 != 0 {
        1
    } else {
        0
    };
    if bit != 0 {
        let events = m.lhu(input + 0x36);
        m.sh(input + 0x36, events | bit);
    }
    for index in 0..32 {
        let mask = m.lh(config + index * 2) as i32 as u32;
        if mask != 0 && buttons & mask != 0 {
            held |= 1 << index;
        }
    }
    if m.lw(config + 0x58) & 0x10 != 0 {
        held = tap_or_hold(m, held, 0x0020_0000, input + 0x39, 7, 0x2000, |_, count| {
            count.wrapping_sub(1) < 6
        });
    }
    if m.lw(config + 0x58) & 8 != 0 {
        held = tap_or_hold(
            m,
            held,
            0x4000,
            input + 0x3a,
            10,
            0x8000_0000,
            |_, count| count.wrapping_sub(1) < 9,
        );
    }
    let previous = m.lw(input + 8);
    let directions = held & 0xf;
    m.sw(input + 8, held);
    let pressed = !previous & held;
    m.sw(input + 0xc, pressed);
    m.sw(input + 0x10, pressed);
    m.sw(input + 0x14, previous & !held);
    if directions != 0 && directions == previous & 0xf {
        let count = m.lbu(input + 0x38);
        m.sb(input + 0x38, count.wrapping_add(1));
    } else {
        m.sb(input + 0x38, 0);
    }
    let count = m.lbu(input + 0x38);
    if count >= 0x17 && count & 3 == 0 {
        let repeated = m.lw(input + 0x10);
        m.sw(input + 0x10, repeated | directions);
    }
    for (bit, offset) in [(4, 0x1c), (8, 0x28), (0x10, 0x2c)] {
        if m.lhu(config + 0x50) & bit != 0 {
            let value = m.lw(input + offset);
            m.sw(input + offset, value.wrapping_neg());
        }
    }
    mix_vibration(m, input + 0x34);
    let context = m.gw(CONTEXT);
    if m.lw(input) & 1 == 0 || m.lw(context) & flags::PAUSED != 0 {
        for offset in 2..8 {
            m.sb(pad + offset, 0);
        }
        return;
    }
    let mut small = u32::from(m.lbu(input + 0x34));
    let present = u32::from(m.lbu(pad + 1));
    let mut large = u32::from(m.lbu(input + 0x35));
    let missing = !present;
    if missing & 1 != 0 {
        large = (large + small) >> 1;
    }
    if missing & 2 != 0 {
        small = (small + large) >> 1;
    }
    let mut out = pad + 2;
    let pattern = m.lw(VIBRATION_PATTERNS + ((small >> 2) & 0x3c)) as i32;
    let frames = m.lw(context + 0x24);
    let mode = m.lhu(pad + 0xa);
    let on = ((pattern >> (frames & 0x1f)) & 1) as u8;
    if mode == 0 && m.lbu(pad + 0xc) != 0 {
        m.sb(pad + 2, 0x40);
        out = pad + 3;
    }
    if m.lbu(pad + 1) & 1 != 0 {
        m.sb(out, on);
        out += 1;
    }
    if m.lbu(pad + 1) & 2 != 0 {
        m.sb(out, large as u8);
    }
}

/// One tap/hold detector of 80028C24: while `bit` is held its counter
/// counts to `limit`, then `hold` is reported instead of `bit`; on release
/// `bit` is reported once when `tap(count)` holds.
fn tap_or_hold(
    m: &mut Machine,
    held: u32,
    bit: u32,
    counter: u32,
    limit: u8,
    hold: u32,
    tap: impl FnOnce(&mut Machine, u8) -> bool,
) -> u32 {
    let mut held = held;
    if held & bit != 0 {
        held &= !bit;
        let count = m.lbu(counter);
        if count < limit {
            let count = count.wrapping_add(1);
            m.sb(counter, count);
            if count >= limit {
                held |= hold;
            }
        } else {
            held |= hold;
        }
    } else {
        let count = m.lbu(counter);
        if tap(m, count) {
            held |= bit;
        }
        m.sb(counter, 0);
    }
    held
}

/// The analog part of 80028C24: copy the pad's four analog halfwords to
/// the stack, calibrate them (8002895C) and map them to the six input axes.
fn map_axes(m: &mut Machine, pad: u32, axes: u32, input: u32, config: u32) {
    for i in 0..4 {
        let value = m.lhu(pad + 0x12 + i * 2);
        m.sh(axes + i * 2, value);
    }
    calibrate_axes(m, axes, config + 0x40);
    for index in 0..6 {
        let source = u32::from(m.lbu(config + index + 0x52));
        let slot = input + index * 4 + 0x1c;
        if source == 0xff {
            m.sw(slot, 0);
            continue;
        }
        let mask = m.lw(input + 0x18);
        let value = m.lh(axes.wrapping_add(source * 2)) as i32 as u32;
        m.sw(input + 0x18, mask | 1 << index);
        let curve = m.lhu(config + 0x50);
        let value = if curve & 1 != 0 {
            axis_curve(m, value, false)
        } else if curve & 2 != 0 {
            axis_curve(m, value, true)
        } else {
            value
        };
        m.sw(slot, value);
    }
}

// ---- 8002AC3C / 8002ABE0 users: CD stop and pause ---------------------------------------

/// 8002AC3C (JP; PAL 8002A80C, near, `call-:1 call~:1 del:3 imm:1
/// ins:1`): remove the CD callbacks, clear the stream state, mute CD audio
/// (8002999C(1)) and, if CdSync reports complete, issue CdControl 9 (pause,
/// `mode` 0), 7 (standby, `mode` 1) or 8 (stop). Returns 1 if issued. JP
/// no longer clears the stream's paused time (+0x40) before the command.
pub fn cd_stop(m: &mut Machine, mode: u32) -> u32 {
    pending_sdk::cd_ready_callback(m, 0);
    pending_sdk::cd_sync_callback(m, 0);
    let stream = m.gw(CD_STREAM);
    let reader = m.gw(0x3c);
    m.sw(stream, 0);
    m.sw(stream + 4, 0);
    m.sw(stream + 0x20, 0);
    m.sw(reader, 0);
    mainloop::set_cd_mix(m, 1);
    let monitor = m.gw(CD_MONITOR);
    if sdk::cd_sync(m, 1, monitor + 0x28) != 2 {
        return 0;
    }
    let command = match mode {
        0 => 9,
        1 => 7,
        _ => 8,
    };
    let monitor = m.gw(CD_MONITOR);
    let issued = pending_sdk::cd_control(m, command, 0, monitor + 0x28);
    u32::from(issued != 0)
}

/// 8002C41C (JP; PAL 8002BFF4, exact, calls differ, REVIEW.md §3b): CD
/// command 6: pause (entry +4 nonzero) or resume the stream through the JP
/// stream pause 8002ABE0 (PAL's 8002A76C has no JP counterpart) and record
/// the request at +0x178. Always done.
pub fn cd_command_pause(m: &mut Machine, entry: u32) -> u32 {
    let pause = m.lw(entry + 4);
    super::res_p3_resident::stream_pause(m, pause);
    let queue = m.gw(CD_QUEUE);
    let pause = m.lw(entry + 4);
    m.sw(queue + 0x178, pause);
    1
}

// ---- 8002E1F4: per-frame sound update ------------------------------------------------------

/// 8002E1F4 (JP; PAL 8002DDCC, near, `del:3 imm:2 op:4 reg:1`): per-frame
/// sound update, as PAL except for the CD volume: the music tracks (CD file
/// 0x1E6 and 0x23..=0x46, PAL below 0x2A and 0x44/0x46/0x47) take context
/// +0x399, the others +0x398 (PAL +0x39D/+0x39C). Then the master volume,
/// delayed sounds and the sound sequence, and for each of the 24 voices
/// key-on setup, volume fades and volume updates, key on, release of voices
/// whose envelope ended, freeing, and key off.
pub fn update_sound(m: &mut Machine) {
    let frame = m.push_frame(0x38);
    update_sound_frame(m, frame);
    m.pop_frame(0x38);
}

fn update_sound_frame(m: &mut Machine, frame: u32) {
    let track = shared::current_cd_file(m) & 0x7ff;
    let context = m.gw(CONTEXT);
    let volume = if track == 0x1e6 || track.wrapping_sub(0x23) < 0x24 {
        m.lbu(context + MUSIC_VOLUME)
    } else {
        m.lbu(context + EFFECTS_VOLUME)
    };
    mainloop::set_cd_volume(m, u32::from(volume));
    let context = m.gw(CONTEXT);
    let master = if m.lw(context) & flags::PAUSED != 0 {
        0
    } else {
        0x3fff
    };
    mainloop::set_master_volume(m, master);
    let context = m.gw(CONTEXT);
    if m.lw(context) & flags::PAUSED == 0 {
        mainloop::update_delayed_sounds(m);
    }
    mainloop::update_sequence(m);
    let mut keys_on = 0;
    for index in 0..VOICE_COUNT {
        let v = voice(index);
        let requests = m.lbu(v + 1);
        if requests & 1 != 0 {
            keys_on |= 1 << index;
            m.sb(v + 1, requests & 0xfe);
            key_on_voice(m, index);
            let requests = m.lbu(v + 1);
            if requests & 0x10 != 0 {
                m.sb(v + 1, requests & 0xef);
            }
        }
        let frames = m.lh(v + 0x12);
        if frames > 0 {
            let target = i32::from(m.lbu(v + 0x10));
            let level = i32::from(m.lbu(v + 0x11));
            let step = checked_div(
                m,
                target - level,
                i32::from(frames),
                0x8002_e324,
                0x8002_e33c,
            );
            let level = level.wrapping_add(step);
            let frames = m.lh(v + 0x12);
            m.sb(v + 0x11, level as u8);
            let raw = m.lhu(v + 0x12);
            if frames > 0 {
                m.sh(v + 0x12, raw.wrapping_sub(1));
            }
        } else {
            let target = m.lbu(v + 0x10);
            m.sb(v + 0x11, target);
        }
        if m.lbu(v + 0x11) != 0x80 {
            let requests = m.lbu(v + 1);
            m.sb(v + 1, requests | 0x10);
        }
        if m.lbu(v + 1) & 0x10 != 0 {
            update_voice_volume(m, index);
            let requests = m.lbu(v + 1);
            m.sb(v + 1, requests & 0xef);
        }
    }
    sdk::spu_set_key(m, 1, keys_on);
    let status = frame + 0x10;
    sdk::spu_get_all_keys_status(m, status);
    for index in 0..VOICE_COUNT {
        let v = voice(index);
        match m.lbu(status + index) {
            1 => m.sb(v + 2, 8),
            2 => {}
            0 => {
                if m.lbu(v + 3) != 0 {
                    m.sb(v + 1, 4);
                }
            }
            _ => {
                let delay = m.lbu(v + 2);
                if delay == 0 {
                    shared::release_channel(m, index);
                } else {
                    let delay = delay - 1;
                    m.sb(v + 2, delay);
                    if delay == 0 {
                        shared::release_channel(m, index);
                    }
                }
            }
        }
    }
    for index in 0..VOICE_COUNT {
        if m.lbu(voice(index) + 1) & 4 != 0 {
            shared::reset_channel(m, index);
        }
    }
    let mut keys_off = 0;
    for index in 0..VOICE_COUNT {
        if m.lbu(voice(index) + 1) & 2 != 0 {
            keys_off |= 1 << index;
        }
    }
    sdk::spu_set_key(m, 0, keys_off);
}

/// 8002DF98 (JP; PAL 8002DB70, near, `imm:2`): SpuSetVoiceAttr volume/note
/// update of voice `index` (mask 0x23) from its volume (+4/+6), the effects
/// volume (context +0x398, PAL +0x39C), the fade level (+0x11) and the mono
/// flag (context +0x39A bit 0, PAL +0x39E).
pub fn update_voice_volume(m: &mut Machine, index: u32) {
    let frame = m.push_frame(0x58);
    let attr = frame + 0x10;
    let context = m.gw(CONTEXT);
    let effects = i32::from(m.lbu(context + EFFECTS_VOLUME));
    m.sw(attr, 1 << (index & 0x1f));
    m.sw(attr + 4, 0x23);
    let v = voice(index);
    if m.lbu(context + MONO) & 1 != 0 {
        let left = i32::from(m.lh(v + 4));
        let right = i32::from(m.lh(v + 6));
        let sum = left.wrapping_abs().wrapping_add(right.wrapping_abs());
        let scaled = sum.wrapping_mul(effects) >> 9;
        let fade = i32::from(m.lbu(v + 0x11));
        let level = scaled.wrapping_mul(fade) >> 7;
        m.sh(attr + 8, level as u16);
        m.sh(attr + 0xa, level as u16);
    } else {
        for (source, target) in [(4, 8), (6, 0xa)] {
            let volume = i32::from(m.lh(v + source));
            let scaled = volume.wrapping_mul(effects) >> 8;
            let level = if m.lbu(v + 0x11) == 0x80 {
                scaled
            } else {
                scaled.wrapping_mul(i32::from(m.lbu(v + 0x11))) >> 7
            };
            m.sh(attr + target, level as u16);
        }
    }
    let note = m.lhu(v + 0xc);
    m.sh(attr + 0x16, note);
    sdk::spu_set_voice_attr(m, attr);
    m.pop_frame(0x58);
}

/// 8002E0D0 (JP; PAL 8002DCA8, near, `imm:2`): SpuSetVoiceAttr key-on
/// setup of voice `index` (mask 0x600E3: volume, note, sample note, start
/// address, ADSR) from its sound definition, with the effects volume at
/// context +0x398 and the mono flag at +0x39A (PAL +0x39C/+0x39E).
pub fn key_on_voice(m: &mut Machine, index: u32) {
    let frame = m.push_frame(0x58);
    let attr = frame + 0x10;
    let v = voice(index);
    let definition = sound(u32::from(m.lbu(v)));
    let context = m.gw(CONTEXT);
    let effects = i32::from(m.lbu(context + EFFECTS_VOLUME));
    m.sw(attr, 1 << (index & 0x1f));
    m.sw(attr + 4, 0x0006_00e3);
    if m.lbu(context + MONO) & 1 != 0 {
        let left = i32::from(m.lh(v + 4));
        let right = i32::from(m.lh(v + 6));
        let sum = left.wrapping_abs().wrapping_add(right.wrapping_abs());
        let level = effects.wrapping_mul(sum) >> 9;
        m.sh(attr + 8, level as u16);
        m.sh(attr + 0xa, level as u16);
    } else {
        let left = i32::from(m.lh(v + 4));
        m.sh(attr + 8, (left.wrapping_mul(effects) >> 8) as u16);
        let right = i32::from(m.lh(v + 6));
        m.sh(attr + 0xa, (right.wrapping_mul(effects) >> 8) as u16);
    }
    let note = m.lhu(v + 0xc);
    m.sh(attr + 0x16, note);
    let sample_note = m.lhu(definition + 0xa);
    m.sh(attr + 0x18, sample_note);
    let address = m.lw(definition + 0x10);
    m.sw(attr + 0x1c, address);
    let adsr1 = m.lhu(definition + 0xc);
    m.sh(attr + 0x3a, adsr1);
    let adsr2 = m.lhu(definition + 0xe);
    m.sh(attr + 0x3c, adsr2);
    sdk::spu_set_voice_attr(m, attr);
    m.pop_frame(0x58);
}
