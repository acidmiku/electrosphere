//! Frame-tail session step 80036E54 (called by 80033B28) and its tree, the
//! pause path of 80033C58 (80034428, 80034640, 8002C2CC) and the alternate
//! flight update routines that the other flight states hand to 80033748.
//!
//! Translated from the original instructions in their original order.
//! Routines owned by other modules (script, tail, loading, shared, SDK) are
//! called by original address through [`dispatch::call`]; every routine
//! translated here is claimed by [`dispatch`]. Stack-passed arguments of
//! such calls are written to the caller's guest frame (`[sp+0x10]`, ...).
use crate::{
    common::{CONTEXT, ENTITIES, GAMEPLAY, request_transition},
    dispatch,
    machine::{Fault, GP, Machine, SmallData},
    sdk,
};
use ac3_gte::reg;

/// `[gp+0x4C0]` (800BAEB4): current mission number.
const MISSION: u32 = 0x800b_aeb4;
/// `[gp+0x330]`: mission progress block.
const PROGRESS: u32 = 0x330;
/// `[gp+0x32C]`: per-mission limits (phase table).
const LIMITS: u32 = 0x32c;
/// `[gp+0x328]`: mission script table.
const SCRIPTS: u32 = 0x328;
/// `[gp+0x48]`: sound sequencer control block.
const SOUND: u32 = 0x48;
/// `[gp+0x528]`: pad state (word 0 = buttons).
const PAD: u32 = 0x528;
/// Radio message queue: `[gp+0x4A8]` slots, sequence index/table/frames,
/// cursor, step, state and timer words.
const RADIO: u32 = 0x4a8;
const RADIO_SEQUENCE: u32 = 0x49c;
const RADIO_TABLE: u32 = 0x4a0;
const RADIO_STEP: u32 = 0x4ac;
const RADIO_FRAMES: u32 = 0x4b0;
const RADIO_CURSOR: u32 = 0x4b8;
const RADIO_STATE: u32 = 0x4bc;
const RADIO_TIMER: u32 = 0x4c4;
/// `[gp+0x450]`: radio indicator state byte.
const INDICATOR: u32 = GP + 0x450;
/// 800C4F38: radio indicator block (80040F50 resets it).
const INDICATOR_BLOCK: u32 = 0x800c_4f38;
/// `[0x800BAF1C]`: camera request block.
const CAMERA_REQUEST: u32 = 0x800b_af1c;
/// `[0x800BAF20]`: flight state block.
const FLIGHT_STATE: u32 = 0x800b_af20;
/// 800AF720: pause menu row offsets (one word per row).
const MENU_ROWS: u32 = 0x800a_f720;
/// 800BAB20/800BAB28: pause menu boxes (x, y, w, h halfwords).
const MENU_BOX_OUTER: u32 = 0x800b_ab20;
const MENU_BOX_ROWS: u32 = 0x800b_ab28;
/// 800243D0: jump table of 80092A60 (five cases).
const SPAWN_CASES: u32 = 0x8002_43d0;

fn call(m: &mut Machine, address: u32, args: [u32; 4]) -> u32 {
    dispatch::call(m, address, args)
}
fn mission_number(m: &mut Machine) -> u32 {
    u32::from(m.lbu(MISSION))
}
fn progress_block(m: &mut Machine) -> u32 {
    m.gw(PROGRESS)
}
/// `~[context] & 0x20000000`: false while the game is paused.
fn running(m: &mut Machine) -> bool {
    let context = m.gw(CONTEXT);
    !m.lw(context) & 0x2000_0000 != 0
}
fn set_context_flags(m: &mut Machine, bits: u32) {
    let context = m.gw(CONTEXT);
    let flags = m.lw(context);
    m.sw(context, flags | bits);
}
/// `interpret` (80053704, script module) on script table word `offset`.
fn run_script(m: &mut Machine, offset: u32) {
    let scripts = m.gw(SCRIPTS);
    let script = m.lw(scripts.wrapping_add(offset));
    call(m, 0x8005_3704, [script, 0, 0, 0]);
}
fn player_entity(m: &mut Machine) -> u32 {
    let entities = m.gw(ENTITIES);
    m.lw(entities)
}
/// Little-endian `lwl rt, address` with the register's previous value.
fn lwl(m: &mut Machine, address: u32, rt: u32) -> u32 {
    let k = address & 3;
    let word = m.lw(address & !3);
    let shift = 8 * (3 - k);
    let keep = if k == 3 { 0 } else { u32::MAX >> (8 * (k + 1)) };
    (rt & keep) | (word << shift)
}
/// Little-endian `swl rt, address`.
fn swl(m: &mut Machine, address: u32, rt: u32) {
    let k = address & 3;
    let aligned = address & !3;
    for i in 0..=k {
        let byte = (rt >> (8 * (3 - k + i))) as u8;
        m.sb(aligned + i, byte);
    }
}
/// The packets' `lwl/swl` splice at +2: `head` takes the link of `slot`, and
/// `slot` then links to `first`. OT slots and packet pointers are word
/// aligned, so the register byte `lwl` preserves is never stored.
fn splice(m: &mut Machine, slot: u32, head: u32, first: u32) {
    let link = lwl(m, slot.wrapping_add(2), 0);
    swl(m, head.wrapping_add(2), link);
    swl(m, slot.wrapping_add(2), first << 8);
}

// ---- routines owned by other modules (called by original address) ----------

/// 8002C2F4 (sound sequencer flush; called with a0 = 0).
fn sound_flush(m: &mut Machine) {
    call(m, 0x8002_c2f4, [0; 4]);
}
/// 80041D4C (raise priority message `id`; `hud.rs`).
fn priority_message(m: &mut Machine, id: u32) {
    call(m, 0x8004_1d4c, [id, 0, 0, 0]);
}
/// 80055038 (the radio state word `gp+0x4BC`; `hud.rs`).
fn radio_state(m: &mut Machine) -> u32 {
    call(m, 0x8005_5038, [0; 4])
}
/// 8002D724 (play sound effect).
fn sound_effect(m: &mut Machine, effect: u32) {
    call(m, 0x8002_d724, [effect, 0, 0, 0]);
}
/// 8004B760 (show on-screen message).
fn show_message(m: &mut Machine, id: u32) {
    call(m, 0x8004_b760, [id, 0, 0, 0]);
}
/// 80054C20 (queue a radio message).
fn radio_queue(m: &mut Machine, message: u32, frames: u32) {
    call(m, 0x8005_4c20, [message, frames, 0, 0]);
}
/// 80030220 / 80030248 (sound channel resets).
fn music_80030220(m: &mut Machine) {
    call(m, 0x8003_0220, [0; 4]);
}
fn music_80030248(m: &mut Machine) {
    call(m, 0x8003_0248, [0; 4]);
}
/// 80077D54 (camera target).
fn camera_target(m: &mut Machine, camera: u32, mode: u32, target: u32) {
    call(m, 0x8007_7d54, [camera, mode, target, 0]);
}
/// 80082CD0 (depth-cued noise sprites; RGBC/IR0 set by the caller).
fn fade_sprites(m: &mut Machine, packet: u32, clut: u32, xy: u32, wh: u32) -> u32 {
    call(m, 0x8008_2cd0, [packet, clut, xy, wh])
}
/// 800343D0 (DR_TPAGE added to an OT slot; returns the next packet).
fn add_tpage(m: &mut Machine, slot: u32, packet: u32, tpage: u32) -> u32 {
    call(m, 0x8003_43d0, [slot, packet, tpage, 0])
}
/// 800353D8 (localized menu text): the three stack arguments go to the
/// caller's frame at `[sp+0x10..0x18]`, as the original passes them.
#[allow(clippy::too_many_arguments)]
fn draw_text(
    m: &mut Machine,
    slot: u32,
    packet: u32,
    x: u32,
    y: u32,
    color: u32,
    align: u32,
    id: u32,
) -> u32 {
    let sp = m.sp;
    m.sw(sp + 0x10, color);
    m.sw(sp + 0x14, align);
    m.sw(sp + 0x18, id);
    call(m, 0x8003_53d8, [slot, packet, x, y])
}

// ---- radio messages and indicators ------------------------------------------

/// 8006B360: current pad buttons `[[gp+0x528]]`.
pub fn pad_buttons(m: &mut Machine) -> u32 {
    let pad = m.gw(PAD);
    m.lw(pad)
}

/// 80054DFC: 1 while a radio sequence is selected (`[gp+0x49C]` != -1).
pub fn radio_busy(m: &mut Machine) -> u32 {
    u32::from(m.gw(RADIO_SEQUENCE) != u32::MAX)
}

/// 80040CA4: indicator byte = 1 unless it is 2.
pub fn indicator_on(m: &mut Machine) {
    if m.lbu(INDICATOR) != 2 {
        m.sb(INDICATOR, 1);
    }
}

/// 80040CC0: indicator byte.
pub fn indicator(m: &mut Machine) -> u32 {
    u32::from(m.lbu(INDICATOR))
}

/// 80040CCC: indicator byte 2 becomes 3.
pub fn indicator_done(m: &mut Machine) {
    if m.lbu(INDICATOR) == 2 {
        m.sb(INDICATOR, 3);
    }
}

/// 80040FCC: clear the 800C4F38 halfword; if its byte +2 is set, set it to 3
/// (`mode` 1) or 0 and byte +3 to 0xFF.
pub fn indicator_close(m: &mut Machine, mode: u32) {
    let active = m.lbu(INDICATOR_BLOCK + 2);
    m.sh(INDICATOR_BLOCK, 0);
    if active == 0 {
        return;
    }
    m.sb(INDICATOR_BLOCK + 2, if mode == 1 { 3 } else { 0 });
    m.sb(INDICATOR_BLOCK + 3, 0xff);
}

/// 80054C78: select radio sequence `sequence` from its start.
pub fn radio_select(m: &mut Machine, sequence: u32) {
    m.set_gw(RADIO_SEQUENCE, sequence);
    m.set_gw(RADIO_STEP, 0);
    m.set_gw(RADIO_TIMER, 0);
}

/// 80054C8C: advance the selected radio sequence by one frame; returns 0.
pub fn radio_tick(m: &mut Machine) -> u32 {
    let sequence = m.gw(RADIO_SEQUENCE);
    if sequence == u32::MAX {
        return 0;
    }
    let table = m.gw(RADIO_TABLE);
    let step = m.gw(RADIO_STEP);
    let entry = (sequence << 2).wrapping_add(table);
    let cursor = (m.lh(entry) as i32 as u32).wrapping_add(step);
    let frames = m.gw(RADIO_FRAMES);
    let duration = m.lh((cursor << 2).wrapping_add(frames).wrapping_add(2)) as i32 as u32;
    let timer = m.gw(RADIO_TIMER);
    m.set_gw(RADIO_CURSOR, cursor);
    if timer != duration {
        m.set_gw(RADIO_TIMER, timer.wrapping_add(1));
        return 0;
    }
    let length = i32::from(m.lh(entry.wrapping_add(2)));
    let next = step.wrapping_add(1);
    m.set_gw(RADIO_TIMER, 0);
    m.set_gw(RADIO_STEP, next);
    if (next as i32) < length {
        return 0;
    }
    m.set_gw(RADIO_CURSOR, 0);
    m.set_gw(RADIO_STEP, 0);
    m.set_gw(RADIO_SEQUENCE, u32::MAX);
    0
}

/// 80054E10: start queued radio messages whose wait elapsed, tick the active
/// sequence and settle the radio state and indicator.
pub fn radio_update(m: &mut Machine) {
    if running(m) {
        for slot in 0..4u32 {
            let queue = m.gw(RADIO);
            let message = m.lbu(queue.wrapping_add(slot) + 0x14);
            if message == 0xff {
                continue;
            }
            let timing = queue.wrapping_add(slot << 1);
            let wait = m.lh(timing + 4);
            let target = m.lh(timing + 0xc);
            let counter = m.lhu(timing + 4);
            if wait != target {
                if running(m) {
                    m.sh(timing + 4, counter.wrapping_add(1));
                }
                continue;
            }
            if m.lw(queue) != 0xff {
                continue;
            }
            let context = m.gw(CONTEXT);
            let mode = m.lbu(context + 0x3a0);
            if mode == 1 {
                indicator_on(m);
                if indicator(m) != 2 {
                    m.set_gw(RADIO_STATE, 3);
                    continue;
                }
                radio_select(m, u32::from(message));
                let context = m.gw(CONTEXT);
                let queue = m.gw(RADIO);
                let flags = m.lw(context);
                m.set_gw(RADIO_STATE, u32::from(mode));
                m.sw(context, flags | 0x1000_0000);
                m.sw(queue, u32::from(message));
            }
            let queue = m.gw(RADIO);
            m.sb(queue.wrapping_add(slot) + 0x14, 0xff);
        }
    }
    radio_tick(m);
    if radio_busy(m) == 0 {
        let state = m.gw(RADIO_STATE);
        if state != 3 {
            if state != 0 {
                let gameplay = m.gw(GAMEPLAY);
                if m.lw(gameplay + 4) & 4 != 0 {
                    let context = m.gw(CONTEXT);
                    let flags = m.lw(context);
                    m.sw(context, flags & 0xefff_ffff);
                }
            }
            let queue = m.gw(RADIO);
            m.set_gw(RADIO_STATE, 0);
            m.sw(queue, 0xff);
        }
    }
    let queue = m.gw(RADIO);
    if m.lw(queue) == 0xff {
        indicator_close(m, 1);
    }
    let context = m.gw(CONTEXT);
    if m.lbu(context + 0x3a0) == 1 {
        let queue = m.gw(RADIO);
        if m.lw(queue) != 0xff {
            indicator_on(m);
        } else {
            indicator_done(m);
        }
    }
}

/// 80055044: stop the radio: no sequence, all four slots free, indicator
/// closed. Returns 0.
pub fn radio_reset(m: &mut Machine) -> u32 {
    let state = m.gw(RADIO_STATE);
    m.set_gw(RADIO_SEQUENCE, u32::MAX);
    m.set_gw(RADIO_CURSOR, 0);
    m.set_gw(RADIO_STEP, 0);
    m.set_gw(RADIO_TIMER, 0);
    if state == 3 {
        m.set_gw(RADIO_STATE, 0);
        indicator_done(m);
    }
    indicator_close(m, 0);
    let queue = m.gw(RADIO);
    m.sw(queue, 0xff);
    for slot in 0..4u32 {
        let queue = m.gw(RADIO);
        m.sb(queue.wrapping_add(slot) + 0x14, 0xff);
        let queue = m.gw(RADIO);
        let timing = queue.wrapping_add(slot << 1);
        m.sh(timing + 0xc, 0);
        m.sh(timing + 4, 0);
    }
    0
}

// ---- sound sequencer commands -------------------------------------------------

/// 8002C2CC: sequencer command 6 with argument `pause` (pause on/off);
/// returns 1.
pub fn sound_pause(m: &mut Machine, pause: u32) -> u32 {
    let sound = m.gw(SOUND);
    m.sb(sound + 0x14c, 6);
    let current = m.gw(SOUND);
    m.sw(sound + 0x148, 1);
    m.sw(current + 0x150, pause);
    m.sw(current + 0x15c, 0);
    m.sb(current + 0x14d, 0);
    1
}

/// 80055184: flush the sequencer and queue 800B0490.
pub fn sound_queue_80055184(m: &mut Machine) {
    sound_flush(m);
    call(m, 0x8002_c4b0, [0x800b_0490, 0, 0, 0]);
}

/// 80030288: 8002F204; returns 0.
pub fn music_80030288(m: &mut Machine) -> u32 {
    call(m, 0x8002_f204, [0; 4]);
    0
}

// ---- camera requests ------------------------------------------------------------

fn camera_request(m: &mut Machine, mode: u32, argument: u32) {
    let request = m.lw(CAMERA_REQUEST);
    m.sw(request, mode);
    m.sw(request + 4, argument);
    m.sw(request + 8, 0);
}

/// 80074C04: camera request (1, 1).
pub fn camera_74c04(m: &mut Machine) {
    camera_request(m, 1, 1);
}
/// 80074C20: camera request (1, 0x1000000).
pub fn camera_74c20(m: &mut Machine) {
    camera_request(m, 1, 0x0100_0000);
}
/// 80074C40: camera request (1, 0x2000000).
pub fn camera_74c40(m: &mut Machine) {
    camera_request(m, 1, 0x0200_0000);
}
/// 80074C60: camera request (0x800, 4).
pub fn camera_74c60(m: &mut Machine) {
    camera_request(m, 0x800, 4);
}
/// 80074C80: keep the camera mode with bit 0x40 and argument 2 (`near` != 0)
/// or 0x400000.
pub fn camera_74c80(m: &mut Machine, near: u32) {
    let request = m.lw(CAMERA_REQUEST);
    let mode = m.lw(request);
    m.sw(request + 4, if near != 0 { 2 } else { 0x40_0000 });
    m.sw(request + 8, 0);
    m.sw(request, mode | 0x40);
}

/// 80077E68: unless the camera is locked (+0x50 bit 0x800), target the
/// player (mode 1, or 2 for `outside` != 0) and run 80035248(1).
pub fn camera_follow_player(m: &mut Machine, outside: u32) {
    let gameplay = m.gw(GAMEPLAY);
    let camera = m.lw(gameplay + 0x28);
    if m.lw(camera + 0x50) & 0x800 != 0 {
        return;
    }
    let mode = if outside == 0 { 1 } else { 2 };
    let target = player_entity(m);
    camera_target(m, camera, mode, target);
    call(m, 0x8003_5248, [1, 0, 0, 0]);
}

/// 80077ECC: [`camera_follow_player`] with context byte +0x3A3.
pub fn camera_follow_default(m: &mut Machine) {
    let context = m.gw(CONTEXT);
    let outside = u32::from(m.lbu(context + 0x3a3));
    camera_follow_player(m, outside);
}

// ---- on-screen messages ---------------------------------------------------------

// ---- fade overlays (GTE depth-cued sprites) ------------------------------------

/// Mode packet (len 3: E1 `tpage`, E2 0x0C4318 window, NOP) at `at`, linked
/// to the following packet.
fn fade_mode(m: &mut Machine, at: u32, tpage: u32) -> u32 {
    let next = at.wrapping_add(0x10);
    m.sw(at, ((next << 8) >> 8) | 0x0300_0000);
    m.sw(at.wrapping_add(4), tpage);
    m.sw(at.wrapping_add(8), 0xe20c_4318);
    m.sw(at.wrapping_add(0xc), 0);
    next
}

/// Closing mode packet after the sprites and the splice into `slot`.
fn fade_close(m: &mut Machine, slot: u32, packet: u32, end: u32) -> u32 {
    m.sw(end.wrapping_add(4), 0xe100_0220);
    m.sw(end, 0x0300_0000);
    m.sw(end.wrapping_add(8), 0xe200_0000);
    m.sw(end.wrapping_add(0xc), 0);
    splice(m, slot, end, packet);
    end.wrapping_add(0x10)
}

/// 80082F18: intro fade (level `level`, 0x1000 = black): a mask packet, a
/// 16-pixel CLUT upload whose last `level>>8` entries are 0x8000, then the faded
/// screen sprites; spliced in at OT slot `slot`. Returns the next packet.
pub fn intro_fade(m: &mut Machine, slot: u32, packet: u32, level: u32) -> u32 {
    let shade = (level as i32) >> 8;
    if shade <= 0 {
        return packet;
    }
    let sprites = packet.wrapping_add(0x3c);
    let lit = 0x10 - shade;
    let upload = packet.wrapping_add(8);
    m.sw(packet, ((upload << 8) >> 8) | 0x0100_0000);
    m.sw(packet.wrapping_add(4), 0xe600_0000);
    m.sw(packet.wrapping_add(8), ((sprites << 8) >> 8) | 0x0c00_0000);
    m.sw(upload.wrapping_add(4), 0xa000_0000);
    m.sw(upload.wrapping_add(8), 0x0100_01d0);
    m.sw(upload.wrapping_add(0xc), 0x0001_0010);
    m.sw(upload.wrapping_add(0x30), 0x0100_0000);
    for i in 0..0x10i32 {
        let pixel = if i < lit { 0 } else { 0x8000 };
        m.sh(upload.wrapping_add(0x10 + 2 * i as u32), pixel);
    }
    let first = fade_mode(m, sprites, 0xe100_0238);
    m.gte.mtc2(reg::RGBC, 0x6483_8383);
    m.gte.mtc2(reg::IR0, 0);
    let end = fade_sprites(m, first, 0x401d, 0xff80_ff60, 0x0100_0140);
    fade_close(m, slot, packet, end)
}

/// 80082E2C: screen fade toward `level` (0: nothing; negative: subtractive
/// blend) spliced in at OT slot `slot`. Returns the next packet.
pub fn screen_fade(m: &mut Machine, slot: u32, packet: u32, level: u32) -> u32 {
    if level == 0 {
        return packet;
    }
    let mut blend = 1u32;
    let mut level = level;
    if (level as i32) < 0 {
        blend = 2;
        level = 0u32.wrapping_sub(level);
    }
    let first = fade_mode(m, packet, (blend << 5) | 0xe100_0218);
    m.gte.mtc2(reg::RGBC, 0x6683_8383);
    m.gte.mtc2(reg::IR0, 0x1000u32.wrapping_sub(level));
    let end = fade_sprites(m, first, 0x46d6, 0xff80_ff60, 0x0100_0140);
    fade_close(m, slot, packet, end)
}

// ---- mission objects ------------------------------------------------------------

/// 80092A60: spawn the 80088568 objects of group `group` (0..4, from the
/// 800243D0 jump table) through 8007E76C, each with a random flag.
pub fn spawn_group(m: &mut Machine, group: u32) {
    if group >= 5 {
        return;
    }
    let target = m.lw((group << 2).wrapping_add(SPAWN_CASES));
    let (mut source, count) = match target {
        0x8009_2aac => (0x800b_22dc, 7),
        0x8009_2abc => (0x800b_22dc, 5),
        0x8009_2acc => (0x800b_2330, 5),
        0x8009_2adc => (0x800b_236c, 12),
        0x8009_2aec => (0x800b_23fc, 10),
        _ => {
            m.raise(Fault::Untranslated { address: target });
            return;
        }
    };
    for index in 0..count {
        let pool = m.gw(0x40c).wrapping_add(0x36c4);
        let object = call(m, 0x8007_e76c, [pool, 1, 5, 0]);
        if object == 0 {
            return;
        }
        let random = sdk::rand(m);
        m.sh(object, ((index << 1) + (random & 1)) as u16);
        m.sh(object.wrapping_add(2), 0x7fff);
        let a = m.lw(source);
        m.sw(object.wrapping_add(4), a);
        let b = m.lw(source.wrapping_add(4));
        m.sw(object.wrapping_add(8), b);
        let c = m.lw(source.wrapping_add(8));
        m.sw(object.wrapping_add(0x10), 0x8008_8568);
        m.sw(object.wrapping_add(0xc), c);
        source = source.wrapping_add(0xc);
    }
}

/// 80075934: unless the camera request has bits 0x7000, decrement player
/// halfword +0x4E, then 80029168 and 8006B374(2).
pub fn player_hit_75934(m: &mut Machine) {
    let request = m.lw(CAMERA_REQUEST);
    if m.lw(request) & 0x7000 != 0 {
        return;
    }
    let player = player_entity(m);
    let value = m.lhu(player + 0x4e).wrapping_sub(1);
    m.sh(player + 0x4e, value);
    call(m, 0x8002_9168, [0; 4]);
    call(m, 0x8006_b374, [2, 0, 0, 0]);
}

// ---- mission logic (80052AA0) -----------------------------------------------------

/// Copy the time-left word: `[progress+0x14]` from +0xC (mode 0), counted
/// down to 0 (mode 1), or `other`.
fn mission_clock(m: &mut Machine, zero_other: bool) {
    let progress = progress_block(m);
    let mode = m.lbu(progress + 0x1c);
    if mode == 0 {
        let time = m.lw(progress + 0xc);
        m.sw(progress + 0x14, time);
    } else if mode == 1 {
        let left = m.lw(progress + 0x14);
        m.sw(
            progress + 0x14,
            if (left as i32) > 0 {
                left.wrapping_sub(1)
            } else {
                0
            },
        );
    } else if zero_other {
        m.sw(progress + 0x14, 0);
    }
}

/// 80052AA0: per-frame mission logic: phase-2 intro spawns, phase-4 timers,
/// scripted triggers of the individual missions, the phase-0x800 clock and
/// the radio update.
pub fn mission_logic(m: &mut Machine) {
    let gameplay = m.gw(GAMEPLAY);
    let phase = m.lw(gameplay + 4);
    let stage = m.lw(gameplay + 8);
    if phase == 2 {
        let progress = progress_block(m);
        if m.lbu(progress + 0x1c) == 0 {
            let time = m.lw(progress + 0xc);
            m.sw(progress + 0x14, time);
        } else if (m.lw(progress + 0x14) as i32) < 0 {
            m.sw(progress + 0x14, 0);
        }
        for (number, group) in [(0xf, 3), (0x23, 4)] {
            if mission_number(m) == number {
                let gameplay = m.gw(GAMEPLAY);
                if m.lw(gameplay + 8) == 0 && m.lw(gameplay + 0xc) == 0 {
                    spawn_group(m, group);
                }
            }
        }
        if mission_number(m) == 0x2e {
            let gameplay = m.gw(GAMEPLAY);
            if m.lw(gameplay + 0xc) == 0 {
                if m.lw(gameplay + 8) == 0 {
                    spawn_group(m, 1);
                }
                let gameplay = m.gw(GAMEPLAY);
                if m.lw(gameplay + 8) == 1 {
                    spawn_group(m, 2);
                }
            }
        }
    }
    if phase == 4 {
        mission_phase4(m, stage);
    }
    if phase == 0x800 && running(m) {
        mission_clock(m, true);
    }
    let gameplay = m.gw(GAMEPLAY);
    if m.lw(gameplay + 4) & 0x0010_2c0e != 0 {
        radio_update(m);
    }
}

/// Phase-4 part of 80052AA0 (80052BF8..8005354C).
fn mission_phase4(m: &mut Machine, stage: u32) {
    if running(m) {
        let mut times = stage << 5;
        let mut scripts = stage << 6;
        for _ in 0..16 {
            let table = m.gw(SCRIPTS);
            let progress = progress_block(m);
            let when = u32::from(m.lhu(table.wrapping_add(times) + 0x158));
            if m.lw(progress + 0x10) == when {
                let script = m.lw(table.wrapping_add(scripts) + 0x48);
                call(m, 0x8005_3704, [script, 0, 0, 0]);
            }
            scripts = scripts.wrapping_add(4);
            times = times.wrapping_add(2);
        }
        for (count, target, script) in [(0x1e, 0x20, 0x28), (0x1f, 0x21, 0x38)] {
            let progress = progress_block(m);
            let value = m.lbu(progress + count);
            if value != 0 && m.lbu(progress + target) == value {
                run_script(m, (stage << 2).wrapping_add(script));
            }
        }
        let gameplay = m.gw(GAMEPLAY);
        if mission_number(m) != 9 || m.lw(gameplay + 8) == 0 {
            let progress = progress_block(m);
            let elapsed = m.lw(progress + 0xc);
            m.sw(progress + 0xc, elapsed.wrapping_add(1));
        }
        let progress = progress_block(m);
        let frames = m.lw(progress + 0x10);
        m.sw(progress + 0x10, frames.wrapping_add(1));
        mission_clock(m, false);
    }
    let progress = progress_block(m);
    let flags = m.lw(progress);
    m.sw(progress, flags & 0xffff_ffef);
    if mission_number(m) == 3 {
        let gameplay = m.gw(GAMEPLAY);
        if m.lw(gameplay + 8) == 0 {
            let paused = !running(m);
            let target = m.lw(0x800c_4ca0);
            if paused {
                return;
            }
            let distance = m.lw(target + 4) as i32;
            if distance < 0x1000 {
                let progress = progress_block(m);
                let bonus = if distance < 0x100 {
                    3
                } else if distance < 0x400 {
                    2
                } else {
                    1
                };
                let score = m.lw(progress + 4);
                m.sw(progress + 4, score.wrapping_add(bonus));
                let progress = progress_block(m);
                let flags = m.lw(progress);
                m.sw(progress, flags | 0x10);
            }
        }
    }
    if !running(m) {
        return;
    }
    let number = mission_number(m);
    if number == 0xe || number == 0x35 {
        mission_countdown(m, number);
    }
    mission_triggers(m);
}

/// The 0xE/0x35 countdown of 80052AA0 (80052E74..800530B8).
fn mission_countdown(m: &mut Machine, number: u32) {
    let gameplay = m.gw(GAMEPLAY);
    if m.lw(gameplay + 8) != 0 {
        return;
    }
    if m.lw(gameplay + 0xc) == 0x3c {
        if number == 0xe {
            let limits = m.gw(LIMITS);
            let start = m.lhu(limits + 0x20).wrapping_sub(0x97);
            let progress = progress_block(m);
            m.sh(progress + 0x18, start);
        }
        if mission_number(m) == 0x35 {
            let limits = m.gw(LIMITS);
            let start = m.lhu(limits + 0x20).wrapping_sub(0x79);
            let progress = progress_block(m);
            m.sh(progress + 0x18, start);
        }
        let progress = progress_block(m);
        let flags = m.lw(progress);
        m.sw(progress, flags & 0xffff_fffd);
    }
    let progress = progress_block(m);
    let count = m.lhu(progress + 0x18);
    let flags = m.lw(progress);
    m.sh(progress + 0x18, count.wrapping_add(1));
    if flags & 2 != 0 {
        m.sb(progress + 0x1c, 1);
        let limits = m.gw(LIMITS);
        let progress = progress_block(m);
        let left = i32::from(m.lh(limits + 0x22)).wrapping_sub(i32::from(m.lh(progress + 0x18)));
        m.sw(progress + 0x14, left as u32);
        if mission_number(m) == 0xe {
            let warning = i32::from(m.lh(limits + 0x22)).wrapping_sub(0x258);
            if i32::from(m.lh(progress + 0x18)) == warning {
                run_script(m, 0x150);
            }
            let limits = m.gw(LIMITS);
            let progress = progress_block(m);
            let warning = i32::from(m.lh(limits + 0x22)).wrapping_sub(0x258);
            if i32::from(m.lh(progress + 0x18)) >= warning {
                let player = player_entity(m);
                if (m.lw(player + 0xc) as i32) > (0xfb1e_0000u32 as i32) {
                    priority_message(m, 6);
                }
            }
        }
        let progress = progress_block(m);
        let limits = m.gw(LIMITS);
        if m.lh(progress + 0x18) != m.lh(limits + 0x22) {
            return;
        }
        if mission_number(m) == 0x35 {
            run_script(m, 0x150);
        }
        let progress = progress_block(m);
        let flags = m.lw(progress);
        m.sh(progress + 0x18, 0);
        m.sw(progress, flags & 0xffff_fffd);
    } else {
        m.sb(progress + 0x1c, 0);
        let limits = m.gw(LIMITS);
        let progress = progress_block(m);
        let warning = i32::from(m.lh(limits + 0x20)).wrapping_sub(0x96);
        if i32::from(m.lh(progress + 0x18)) == warning {
            run_script(m, 0x148);
        }
        let progress = progress_block(m);
        let limits = m.gw(LIMITS);
        if m.lh(progress + 0x18) != m.lh(limits + 0x20) {
            return;
        }
        run_script(m, 0x14c);
        let progress = progress_block(m);
        let flags = m.lw(progress);
        m.sh(progress + 0x18, 0);
        m.sw(progress, flags | 2);
        let message = if mission_number(m) == 0x35 {
            0x20
        } else {
            0x11
        };
        show_message(m, message);
    }
}

/// Mission triggers of 80052AA0 (800530BC..8005354C).
fn mission_triggers(m: &mut Machine) {
    if mission_number(m) == 0xe {
        let gameplay = m.gw(GAMEPLAY);
        if m.lw(gameplay + 8) == 0 {
            let progress = progress_block(m);
            let flags = m.lw(progress);
            if flags & 1 == 0 {
                let mut over = flags & 2 != 0;
                if !over {
                    let player = player_entity(m);
                    if (m.lw(player + 0xc) as i32) > (0xfb1e_0000u32 as i32) {
                        let scripts = m.gw(SCRIPTS);
                        m.sw(progress, flags | 1);
                        let script = m.lw(scripts + 0x154);
                        call(m, 0x8005_3704, [script, 0, 0, 0]);
                    }
                    let progress = progress_block(m);
                    over = m.lw(progress) & 2 != 0;
                }
                if over {
                    let player = player_entity(m);
                    if (m.lw(player + 0xc) as i32) > (0xfb1e_0000u32 as i32) {
                        let gameplay = m.gw(GAMEPLAY);
                        let slot = m.lw(gameplay + 0x10).wrapping_add(4);
                        let packet = m.lw(gameplay + 0x14);
                        let next = screen_fade(m, slot, packet, 0x2fd);
                        let gameplay = m.gw(GAMEPLAY);
                        m.sw(gameplay + 0x14, next);
                    }
                }
            }
        }
    }
    if mission_number(m) == 6 {
        let player = player_entity(m);
        if (m.lw(player + 0xc) as i32) < (0xff60_0000u32 as i32) {
            radio_reset(m);
            run_script(m, 0x148);
        }
    }
    if mission_number(m) == 0xd {
        let progress = progress_block(m);
        if m.lw(progress) & 0x20 != 0 {
            let count = m.gw(0x494);
            m.set_gw(0x494, count.wrapping_add(1));
        } else {
            m.set_gw(0x494, 0);
        }
        if m.gw(0x494) == 0x4650 {
            run_script(m, 0x148);
        }
    }
    if mission_number(m) == 0x2c {
        mission_2c(m);
    }
    if mission_number(m) == 0x2a {
        let player = player_entity(m);
        let gameplay = m.gw(GAMEPLAY);
        let frame = m.lw(gameplay + 0xc);
        let height = m.lw(player + 0xc) as i32;
        let tick = frame & 0x1f;
        if height >= 0 && tick == 0 {
            player_hit_75934(m);
        }
        if height > 0x0138_7fff && tick == 0x10 {
            player_hit_75934(m);
        }
        if height > 0x01f3_ffff && (tick == 8 || tick == 0x18) {
            player_hit_75934(m);
        }
    }
    let progress = progress_block(m);
    let cleared = m.lw(progress) & 0xffff_fffb;
    m.sw(progress, cleared);
    if mission_number(m) == 0x17 {
        let gameplay = m.gw(GAMEPLAY);
        if m.lw(gameplay + 8) == 0 {
            let position = player_entity(m).wrapping_add(8);
            let limit = 0x0946_ffff;
            if (m.lw(position) as i32).wrapping_abs() <= limit
                && (m.lw(position + 8) as i32).wrapping_abs() <= limit
                && (m.lw(position + 4) as i32) > (0xfc40_0000u32 as i32)
            {
                m.sw(progress, cleared | 4);
            }
        }
    }
    if mission_number(m) == 0x37 {
        let context = m.gw(CONTEXT);
        let player = player_entity(m);
        if m.lw(context + 0x398) == 0 {
            if (m.lw(player + 0x10) as i32) > 0x11d2_8000 {
                run_script(m, 0x14c);
            }
        } else if (m.lw(player + 0x10) as i32) > 0x343f_0000 {
            run_script(m, 0x148);
        }
    }
}

/// Mission 0x2C trigger of 80052AA0 (8005326C..8005337C).
fn mission_2c(m: &mut Machine) {
    let gameplay = m.gw(GAMEPLAY);
    if m.lw(gameplay + 8) != 0 {
        return;
    }
    let progress = progress_block(m);
    if m.lw(progress) & 1 != 0 {
        return;
    }
    let player = player_entity(m);
    let position = player + 8;
    if (m.lw(position + 4) as i32) < (0xffa0_0000u32 as i32) {
        priority_message(m, 4);
    }
    if (m.lw(position + 4) as i32) < (0xff80_0000u32 as i32) {
        let progress = progress_block(m);
        let flags = m.lw(progress);
        m.sw(progress, flags | 1);
        run_script(m, 0x148);
    }
    let x = m.lw(player + 8).wrapping_add(0x01f3_ffff);
    if 0x0714_7ffe < x {
        return;
    }
    let z = m.lw(position + 8) as i32;
    if z > (0xfb9b_0000u32 as i32) && z <= 0x0138_7fff {
        let progress = progress_block(m);
        let flags = m.lw(progress);
        m.sw(progress, flags | 1);
        run_script(m, 0x14c);
    }
}

// ---- session step (80036E54) ------------------------------------------------------

/// Result-countdown frames of 80036E54: `[gp+0x34C]` end, `[gp+0x350]`
/// fade start, `[gp+0x354]` message frame, `[gp+0x358]`/`[gp+0x359]` flags.
const END_FRAME: u32 = 0x34c;
const FADE_FRAME: u32 = 0x350;
const MESSAGE_FRAME: u32 = 0x354;
const FADE_STARTED: u32 = GP + 0x358;
const ENDING_STEP: u32 = GP + 0x359;

/// Start the result fade: progress +0x31 = 2, +0x30 = 1, +0x32 = 30,
/// +0x1A = 0 (with `flag` stored between, as in the original).
fn start_result_fade(m: &mut Machine, flag: Option<(u32, u8)>) {
    let progress = progress_block(m);
    m.sb(progress + 0x31, 2);
    let progress = progress_block(m);
    m.sb(progress + 0x30, 1);
    let progress = progress_block(m);
    if let Some((address, value)) = flag {
        m.sb(address, value);
    }
    m.sh(progress + 0x32, 0x1e);
    m.sh(progress + 0x1a, 0);
}

/// Abort to phase change `8, reason`: progress +0x2A = 5 / +0x29 = 9 in the
/// given order, then the transition and context bit 0x10000000.
fn abort_mission(m: &mut Machine, reason: u32, fade_first: bool) {
    let progress = progress_block(m);
    if fade_first {
        m.sb(progress + 0x2a, 5);
        let progress = progress_block(m);
        m.sb(progress + 0x29, 9);
    } else {
        m.sb(progress + 0x29, 9);
        let progress = progress_block(m);
        m.sb(progress + 0x2a, 5);
    }
    request_transition(m, 8, reason);
    set_context_flags(m, 0x1000_0000);
}

/// 80036E54: per-frame session step (frame tail 80033B28, unless replaying).
/// Runs the mission logic, then the phase-specific session handling:
/// intro (2), mission (4), result (8) and the 0x10/0x100000 endings.
/// Returns 0.
pub fn session_step(m: &mut Machine) -> u32 {
    let frame = m.push_frame(0x38);
    let player = player_entity(m);
    mission_logic(m);
    let gameplay = m.gw(GAMEPLAY);
    let phase = m.lw(gameplay + 4);
    if phase & 2 != 0 {
        session_intro(m, gameplay);
    } else if phase & 4 != 0 {
        session_mission(m, player);
    } else if phase & 8 != 0 {
        session_result(m, gameplay);
    } else {
        session_other(m, gameplay, phase, frame);
    }
    m.pop_frame(0x38);
    0
}

/// Phase 2 (intro) of 80036E54.
fn session_intro(m: &mut Machine, gameplay: u32) {
    set_context_flags(m, 0x1000_0000);
    let progress = progress_block(m);
    if m.lbu(progress + 0x3f) == 1 && m.lw(gameplay + 8) == 0 {
        let frame = m.lw(gameplay + 0xc);
        if (frame as i32) < 0x10 {
            let slot = m.lw(gameplay + 0x10).wrapping_add(4);
            let packet = m.lw(gameplay + 0x14);
            let level = 0x1000u32.wrapping_sub(frame << 8);
            let next = intro_fade(m, slot, packet, level);
            let gameplay = m.gw(GAMEPLAY);
            m.sw(gameplay + 0x14, next);
        }
    }
    let gameplay = m.gw(GAMEPLAY);
    if m.lw(gameplay + 0xc) == 0 {
        let number = mission_number(m);
        if number == 0x2a {
            camera_74c40(m);
        } else if m.lw(gameplay + 8) == 0 && (number == 6 || number == 0xe || number == 0x2c) {
            camera_74c80(m, 0);
        } else {
            camera_74c60(m);
        }
        camera_follow_default(m);
    }
    if pad_buttons(m) & 0x7000 != 0 {
        abort_mission(m, 1, false);
        radio_reset(m);
        sound_flush(m);
        return;
    }
    let gameplay = m.gw(GAMEPLAY);
    if (m.lw(gameplay + 0xc) as i32) < 0x3d || radio_state(m) != 0 {
        return;
    }
    if mission_number(m) == 0x2a {
        camera_74c20(m);
    } else {
        camera_74c04(m);
    }
    let context = m.gw(CONTEXT);
    let outside = u32::from(m.lbu(context + 0x3a3));
    camera_follow_player(m, outside);
    let gameplay = m.gw(GAMEPLAY);
    let stage = m.lw(gameplay + 8);
    request_transition(m, 4, stage);
    let context = m.gw(CONTEXT);
    let flags = m.lw(context);
    let gameplay = m.gw(GAMEPLAY);
    m.sw(context, flags & 0xefff_ffff);
    let limits = m.gw(LIMITS);
    let stage = m.lw(gameplay + 8);
    let message = u32::from(m.lbu(limits.wrapping_add(stage) + 0x48));
    show_message(m, message);
}

/// Phase 4 (mission) of 80036E54: skip, time-out, area and end checks, then
/// the countdown sounds.
fn session_mission(m: &mut Machine, player: u32) {
    let mut stop = false;
    if pad_buttons(m) & 0x7000 != 0 {
        abort_mission(m, 1, true);
        stop = true;
    } else {
        let gameplay = m.gw(GAMEPLAY);
        let limits = m.gw(LIMITS);
        let progress = progress_block(m);
        let stage = m.lw(gameplay + 8);
        let frames = m.lw(progress + 0x10);
        let limit = m.lw(limits.wrapping_add(stage << 2) + 0x24);
        if frames == limit {
            abort_mission(m, 4, true);
            stop = true;
        } else if outside_area(m, player, limits) {
            let number = mission_number(m);
            if number == 0x20 || number == 0x2f || number == 0x35 {
            } else {
                abort_mission(m, 8, true);
                stop = true;
            }
        } else {
            let progress = progress_block(m);
            if m.lbu(progress + 0x29) == 9 {
                let reason = u32::from(m.lbu(progress + 0x27));
                request_transition(m, 8, reason);
                let context = m.gw(CONTEXT);
                let flags = m.lw(context);
                let number = mission_number(m);
                m.sw(context, flags | 0x1000_0000);
                if number == 6 {
                    radio_reset(m);
                }
                sound_flush(m);
            }
        }
    }
    if stop {
        radio_reset(m);
        sound_flush(m);
    }
    session_countdown(m);
}

/// The area check of 80036E54 phase 4 (true: outside).
fn outside_area(m: &mut Machine, player: u32, limits: u32) -> bool {
    let x = m.lw(player + 8) as i32;
    if x < m.lw(limits + 0x10) as i32 || (m.lw(limits + 0x14) as i32) < x {
        return true;
    }
    let z = m.lw(player + 0x10) as i32;
    if z < m.lw(limits + 0x18) as i32 || (m.lw(limits + 0x1c) as i32) < z {
        return true;
    }
    let number = mission_number(m);
    if number == 0x3a && (0xffd0_0000u32 as i32) < m.lw(player + 0xc) as i32 {
        return true;
    }
    if number == 0x2a {
        let height = m.lw(player + 0xc).wrapping_add(0x0ea6_0000);
        return 0x124f_8000 < height;
    }
    false
}

/// Remaining-time sounds of 80036E54 phase 4 (800372C8..800373BC).
fn session_countdown(m: &mut Machine) {
    let gameplay = m.gw(GAMEPLAY);
    let stage = m.lw(gameplay + 8);
    let limits = m.gw(LIMITS);
    let progress = progress_block(m);
    let limit = m.lw(limits.wrapping_add(stage << 2) + 0x24);
    let frames = m.lw(progress + 0x10);
    let mut left = limit.wrapping_sub(frames) as i32;
    if left == 0x708 {
        show_message(m, 0xa);
        let progress = progress_block(m);
        m.sb(progress + 0x1c, 1);
        let progress = progress_block(m);
        m.sw(progress + 0x14, left as u32);
    }
    if left >= 0x708 {
        let progress = progress_block(m);
        if m.lbu(progress + 0x1c) == 1 {
            let shown = m.lw(progress + 0x14) as i32;
            if shown < 0x708 {
                left = shown;
            }
        }
        if left >= 0x708 {
            return;
        }
    }
    // Signed division by 30 as compiled: high word of left * 0x88888889.
    let high = ((i64::from(left) * i64::from(0x8888_8889u32 as i32)) >> 32) as i32;
    let quotient = (high.wrapping_add(left) >> 4).wrapping_sub(left >> 31);
    if left != quotient.wrapping_mul(30) {
        return;
    }
    sound_effect(m, if left < 0x12d { 0x39 } else { 0x38 });
}

/// Phase 8 (result) of 80036E54.
fn session_result(m: &mut Machine, gameplay: u32) {
    set_context_flags(m, 0x1000_0000);
    let stage = m.lw(gameplay + 8);
    if stage & 0x62 != 0 || stage == 0 {
        if pad_buttons(m) & 0x7000 != 0 {
            let progress = progress_block(m);
            m.sb(progress + 0x29, 9);
            request_transition(m, 8, 1);
            let context = m.gw(CONTEXT);
            let flags = m.lw(context);
            m.sw(context, flags | 0x1000_0000);
            sound_flush(m);
            let progress = progress_block(m);
            m.sb(progress + 0x30, 0);
            radio_reset(m);
            let progress = progress_block(m);
            m.sb(progress + 0x2a, 5);
            return;
        }
        let gameplay = m.gw(GAMEPLAY);
        if m.lw(gameplay + 8) == 0 {
            session_result_failed(m, gameplay);
            return;
        }
    }
    session_result_cleared(m);
}

/// Phase 8 with a cleared stage (8003747C..80037710).
fn session_result_cleared(m: &mut Machine) {
    let gameplay = m.gw(GAMEPLAY);
    let stage = m.lw(gameplay + 8);
    let message = if stage & 2 != 0 {
        0
    } else if stage & 0x20 != 0 {
        1
    } else if stage & 0x40 == 0 {
        2
    } else {
        0x32
    };
    let gameplay = m.gw(GAMEPLAY);
    if m.lw(gameplay + 0xc) == 0 {
        if mission_number(m) == 0x3a && m.lw(gameplay + 8) & 8 != 0 {
            camera_74c80(m, 0);
        } else {
            let gameplay = m.gw(GAMEPLAY);
            if m.lw(gameplay + 8) & 1 == 0 {
                if mission_number(m) == 0x2a {
                    camera_74c40(m);
                } else {
                    camera_74c60(m);
                }
            }
        }
        m.sb(FADE_STARTED, 0);
    }
    let end = match mission_number(m) {
        6 => {
            m.set_gw(FADE_FRAME, 0x3c);
            m.set_gw(MESSAGE_FRAME, 0);
            0x5a
        }
        0x37 => {
            m.set_gw(MESSAGE_FRAME, 0x3c);
            m.set_gw(FADE_FRAME, 0xb4);
            0xd2
        }
        _ => {
            m.set_gw(MESSAGE_FRAME, 0x32);
            m.set_gw(FADE_FRAME, 0x118);
            0x136
        }
    };
    m.set_gw(END_FRAME, end);
    let at = m.gw(MESSAGE_FRAME) as i32;
    let half = (at.wrapping_add(((at as u32) >> 31) as i32)) >> 1;
    let gameplay = m.gw(GAMEPLAY);
    if m.lw(gameplay + 0xc) as i32 == half && m.lw(gameplay + 8) & 0x22 != 0 {
        sound_queue_80055184(m);
    }
    let gameplay = m.gw(GAMEPLAY);
    if m.lw(gameplay + 0xc) == m.gw(MESSAGE_FRAME) {
        show_message(m, message);
    }
    let gameplay = m.gw(GAMEPLAY);
    let fade = m.gw(FADE_FRAME) as i32;
    if (m.lw(gameplay + 0xc) as i32) >= fade && m.lbu(FADE_STARTED) == 0 && radio_state(m) == 0 {
        start_result_fade(m, Some((FADE_STARTED, 1)));
    }
    let gameplay = m.gw(GAMEPLAY);
    if (m.lw(gameplay + 0xc) as i32) < m.gw(END_FRAME) as i32
        || m.lbu(FADE_STARTED) != 1
        || radio_state(m) != 0
    {
        return;
    }
    let progress = progress_block(m);
    if m.lbu(progress + 0x30) != 0 {
        return;
    }
    if m.lbu(progress + 0x29) == 0 {
        music_80030288(m);
    } else {
        music_80030248(m);
    }
    radio_reset(m);
    call(m, 0x8004_1a48, [0; 4]); // message slot reset (hud.rs)
    call(m, 0x8003_7b24, [0; 4]);
}

/// Phase 8 with stage 0 (failed) (80037714..80037884).
fn session_result_failed(m: &mut Machine, gameplay: u32) {
    if m.lw(gameplay + 0xc) == 0 {
        let progress = progress_block(m);
        let (fade, end) = if m.lbu(progress + 0x29) == 0 {
            (u32::MAX, 0x5a)
        } else {
            let number = mission_number(m);
            camera_74c60(m);
            if number == 6 {
                let progress = progress_block(m);
                if m.lbu(progress + 0x2a) & 2 != 0 {
                    (0, 0x1e)
                } else {
                    (0x3c, 0x5a)
                }
            } else if number == 0x17 {
                (0x78, 0x96)
            } else {
                (0x3c, 0x5a)
            }
        };
        m.set_gw(FADE_FRAME, fade);
        m.set_gw(END_FRAME, end);
    }
    let gameplay = m.gw(GAMEPLAY);
    if m.lw(gameplay + 0xc) == m.gw(FADE_FRAME) {
        start_result_fade(m, None);
    }
    let gameplay = m.gw(GAMEPLAY);
    if (m.lw(gameplay + 0xc) as i32) < m.gw(END_FRAME) as i32 || radio_state(m) != 0 {
        return;
    }
    let progress = progress_block(m);
    if m.lbu(progress + 0x30) != 0 {
        return;
    }
    if m.lbu(progress + 0x29) == 0 {
        music_80030288(m);
    } else {
        music_80030248(m);
        call(m, 0x8008_33a4, [0; 4]);
        call(m, 0x8004_4e24, [0; 4]);
    }
    call(m, 0x8004_1a48, [0; 4]); // message slot reset (hud.rs)
    radio_reset(m);
    call(m, 0x8003_7b24, [0; 4]);
}

/// The remaining phases of 80036E54: 0x10 (mission 0x2E start) and 0x100000
/// (ending); every other bit returns.
fn session_other(m: &mut Machine, gameplay: u32, phase: u32, frame: u32) {
    if phase & 0x10 != 0 {
        if m.lw(gameplay + 0xc) == 0 && mission_number(m) == 0x2e && m.lw(gameplay + 8) == 2 {
            call(m, 0x8004_7dcc, [0; 4]);
        }
        return;
    }
    let stops = [
        0x20, 0x40, 0x400, 0x800, 0x2000, 0x1000, 0x80, 0x100, 0x200, 0x4000, 0x8000, 0x1_0000,
        0x20_0000,
    ];
    if stops.iter().any(|&bit| phase & bit != 0) || phase & 0x10_0000 == 0 {
        return;
    }
    if m.lw(gameplay + 0xc) == 0 {
        let number = mission_number(m);
        m.sb(ENDING_STEP, 0);
        let start = frame + 0x10;
        let view = match number {
            6 => Some((0xc7c0_0000u32, 0xff44_8000u32, 0xd7c0_0000u32, 0xe000u16)),
            9 => Some((0, 0xfe2b_4000, 0, 0)),
            _ => None,
        };
        if let Some((x, y, z, pitch)) = view {
            m.sw(start, x);
            m.sw(start + 8, z);
            m.sw(start + 4, y);
            m.sh(start + 0x10, 0);
            m.sh(start + 0x12, pitch);
            m.sh(start + 0x14, 0);
            call(m, 0x8007_10fc, [start, 0, 0, 0]);
        }
        camera_74c80(m, 0);
        let context = m.gw(CONTEXT);
        let outside = u32::from(m.lbu(context + 0x3a3));
        camera_follow_player(m, outside);
        music_80030220(m);
    }
    let gameplay = m.gw(GAMEPLAY);
    if m.lw(gameplay + 0xc) == 0x1e {
        let progress = progress_block(m);
        let message = u32::from(m.lbu(progress + 0x3e));
        radio_queue(m, message, 0);
    }
    if m.lbu(ENDING_STEP) == 0 {
        let gameplay = m.gw(GAMEPLAY);
        if (m.lw(gameplay + 0xc) as i32) >= 0x97 && radio_state(m) == 0 {
            m.sb(ENDING_STEP, 1);
        }
    }
    if m.lbu(ENDING_STEP) == 1 {
        let progress = progress_block(m);
        m.sb(progress + 0x31, 2);
        let progress = progress_block(m);
        m.sb(progress + 0x30, 1);
        m.sb(ENDING_STEP, 2);
        let progress = progress_block(m);
        m.sh(progress + 0x32, 0x1e);
        m.sh(progress + 0x1a, 0);
    }
    if m.lbu(ENDING_STEP) != 2 {
        return;
    }
    let progress = progress_block(m);
    if m.lbu(progress + 0x30) != 0 {
        return;
    }
    call(m, 0x8003_7b24, [0; 4]);
    music_80030248(m);
}

// ---- pause menu ---------------------------------------------------------------------

/// Add the 0x10-byte primitive `prim` (len 3) to OT slot `slot`.
fn add_prim3(m: &mut Machine, slot: u32, prim: u32) {
    let link = m.lw(slot);
    m.sw(prim, link | 0x0300_0000);
    m.sw(slot, (prim << 8) >> 8);
}

/// 800340D8: menu box at the guest RECT `rect` (x, y, w, h): four 8x8 corner
/// sprites, four edge tiles and three fill tiles in style `style` (1, 2 or
/// other), added to OT slot `slot`; returns `packet + 0xB0`.
pub fn draw_box(m: &mut Machine, slot: u32, packet: u32, rect: u32, style: u32) -> u32 {
    let x = m.lh(rect) as i32 as u32;
    let y = m.lh(rect.wrapping_add(2)) as i32 as u32;
    let w = m.lh(rect.wrapping_add(4)) as i32 as u32;
    let h = m.lh(rect.wrapping_add(6)) as i32 as u32;
    let (clut, corner, edge, fill) = match style {
        1 => (0x57d4, 0x7680_8080, 0x6200_0000, 0x6248_4848),
        2 => (0x5754, 0x7680_8080, 0x6200_0098, 0x6200_30f8),
        _ => (0x5754, 0x7480_8080, 0x6000_0098, 0x6000_30f8),
    };
    let clut = clut << 16;
    let right = x.wrapping_add(w);
    let bottom = y.wrapping_add(h);
    let inner_h = h.wrapping_sub(0x10);
    let inner_w = w.wrapping_sub(0x10);
    let corners = [
        (x, y, 0x40e8),
        (right.wrapping_sub(8), y, 0x40f8),
        (x, bottom.wrapping_sub(8), 0x50e8),
        (right.wrapping_sub(8), bottom.wrapping_sub(8), 0x50f8),
    ];
    let mut prim = packet;
    for (px, py, uv) in corners {
        m.sw(prim.wrapping_add(4), corner);
        m.sh(prim.wrapping_add(8), px as u16);
        m.sh(prim.wrapping_add(0xa), py as u16);
        m.sw(prim.wrapping_add(0xc), clut | uv);
        add_prim3(m, slot, prim);
        prim = prim.wrapping_add(0x10);
    }
    let tiles = [
        (edge, x, y.wrapping_add(8), 2, inner_h),
        (edge, right.wrapping_sub(2), y.wrapping_add(8), 2, inner_h),
        (edge, x.wrapping_add(8), y, inner_w, 2),
        (edge, x.wrapping_add(8), bottom.wrapping_sub(2), inner_w, 2),
        (fill, x.wrapping_add(8), y.wrapping_add(2), inner_w, 6),
        (
            fill,
            x.wrapping_add(2),
            y.wrapping_add(8),
            w.wrapping_sub(4),
            inner_h,
        ),
        (fill, x.wrapping_add(8), bottom.wrapping_sub(8), inner_w, 6),
    ];
    for (color, tx, ty, tw, th) in tiles {
        m.sw(prim.wrapping_add(4), color);
        m.sh(prim.wrapping_add(8), tx as u16);
        m.sh(prim.wrapping_add(0xa), ty as u16);
        m.sh(prim.wrapping_add(0xc), tw as u16);
        m.sh(prim.wrapping_add(0xe), th as u16);
        add_prim3(m, slot, prim);
        prim = prim.wrapping_add(0x10);
    }
    prim
}

/// 80034640: draw the pause menu (five rows, the two option values, the
/// cursor box, the frame and the blinking arrows) at OT slot `slot` from
/// `packet`; returns the next packet.
pub fn draw_pause_menu(m: &mut Machine, slot: u32, packet: u32) -> u32 {
    let frame = m.push_frame(0x48);
    let outer = frame + 0x20;
    let rows = frame + 0x28;
    for i in 0..8 {
        let byte = m.lbu(MENU_BOX_OUTER + i);
        m.sb(outer + i, byte);
    }
    for i in 0..8 {
        let byte = m.lbu(MENU_BOX_ROWS + i);
        m.sb(rows + i, byte);
    }
    let white = 0x0080_8080;
    let gameplay = m.gw(GAMEPLAY);
    let cursor = gameplay + 0x29c;
    let row_y = |m: &mut Machine, row: u32| {
        let top = m.lh(rows + 2) as i32 as u32;
        m.lw(MENU_ROWS + row * 4).wrapping_add(2).wrapping_add(top)
    };
    let left = |m: &mut Machine| (m.lh(rows) as i32 as u32).wrapping_add(8);
    let right = |m: &mut Machine| {
        let x = m.lh(rows) as i32 as u32;
        let w = m.lh(rows + 4) as i32 as u32;
        x.wrapping_add(w).wrapping_sub(6)
    };
    let mut prim = packet;
    // Row 0 (text 5).
    let (x, y) = (left(m), row_y(m, 0));
    prim = draw_text(m, slot, prim, x, y, white, 0, 5);
    let tpage = if m.lh(cursor) != 0 { 0x55 } else { 0x35 };
    prim = add_tpage(m, slot, prim, tpage);
    // Row 1 (text 6) and its value (7 when context +0x3A1 is set, else 8).
    let (x, y) = (left(m), row_y(m, 1));
    prim = draw_text(m, slot, prim, x, y, white, 0, 6);
    let context = m.gw(CONTEXT);
    let value = if m.lbu(context + 0x3a1) != 0 { 7 } else { 8 };
    let (x, y) = (right(m), row_y(m, 1));
    prim = draw_text(m, slot, prim, x, y, white, 2, value);
    let tpage = if m.lh(cursor) == 1 { 0x35 } else { 0x55 };
    prim = add_tpage(m, slot, prim, tpage);
    // Row 2 (text 0xC) and its value (8 when context +0x3A2 is set, else 7).
    let (x, y) = (left(m), row_y(m, 2));
    prim = draw_text(m, slot, prim, x, y, white, 0, 0xc);
    let context = m.gw(CONTEXT);
    let value = if m.lbu(context + 0x3a2) != 0 { 8 } else { 7 };
    let (x, y) = (right(m), row_y(m, 2));
    prim = draw_text(m, slot, prim, x, y, white, 2, value);
    let tpage = if m.lh(cursor) == 2 { 0x35 } else { 0x55 };
    prim = add_tpage(m, slot, prim, tpage);
    // Rows 3 and 4 (texts 9, 0xA).
    for (row, id) in [(3u32, 9u32), (4, 0xa)] {
        let (x, y) = (left(m), row_y(m, row));
        prim = draw_text(m, slot, prim, x, y, white, 0, id);
        let tpage = if m.lh(cursor) as i32 == row as i32 {
            0x35
        } else {
            0x55
        };
        prim = add_tpage(m, slot, prim, tpage);
    }
    // Cursor box on the selected row, then the outer frame.
    let selected = m.lh(cursor) as i32 as u32;
    let offset = m.lhu((selected << 2).wrapping_add(MENU_ROWS));
    let top = m.lhu(rows + 2).wrapping_add(offset);
    m.sh(rows + 2, top);
    prim = draw_box(m, slot, prim, rows, 1);
    prim = add_tpage(m, slot, prim, 0x55);
    prim = draw_box(m, slot, prim, outer, 2);
    // Blinking arrows (text 1) in a triangle-wave grey, then a blue copy.
    let context = m.gw(CONTEXT);
    let mut level = (m.lw(context + 0x24) << 3) & 0x1ff;
    if level >= 0x100 {
        level = 0x1ff - level;
    }
    let grey = level | (level << 8) | (level << 16);
    prim = draw_text(m, slot, prim, 0xa0, 0x48, grey, 1, 1);
    prim = draw_text(m, slot, prim, 0xa1, 0x49, grey & 0x00ff_0000, 1, 1);
    prim = add_tpage(m, slot, prim, 0x15);
    m.pop_frame(0x48);
    prim
}

/// 80034428: pause menu input: move the cursor (0..4, wrapping), toggle the
/// two options, and on confirm resume (0), retry (3) or quit (4).
pub fn pause_input(m: &mut Machine) {
    let gameplay = m.gw(GAMEPLAY);
    let flags = m.lw(gameplay);
    let context = m.gw(CONTEXT);
    m.sw(gameplay, flags | 0x40);
    let cursor = gameplay + 0x29c;
    if m.lw(context + 0x130) & 1 != 0 {
        let value = m.lhu(cursor).wrapping_sub(1);
        m.sh(cursor, value);
    }
    let context = m.gw(CONTEXT);
    if m.lw(context + 0x130) & 2 != 0 {
        let value = m.lhu(cursor).wrapping_add(1);
        m.sh(cursor, value);
    }
    let mut row = m.lh(cursor);
    if row < 0 {
        m.sh(cursor, 4);
        row = m.lh(cursor);
    }
    if row >= 5 {
        m.sh(cursor, 0);
    }
    if m.lh(cursor) == 1 {
        let context = m.gw(CONTEXT);
        if m.lw(context + 0x130) & 0xc != 0 {
            let value = m.lbu(context + 0x3a1);
            m.sb(context + 0x3a1, u8::from(value == 0));
        }
    }
    if m.lh(cursor) == 2 {
        let context = m.gw(CONTEXT);
        if m.lw(context + 0x130) & 0xc != 0 {
            let value = m.lbu(context + 0x3a2);
            m.sb(context + 0x3a2, u8::from(value == 0));
            let context = m.gw(CONTEXT);
            let request = u32::from(m.lbu(context + 0x3a2) == 0);
            call(m, 0x8003_5248, [request, 0, 0, 0]);
        }
    }
    let context = m.gw(CONTEXT);
    if m.lw(context + 0x12c) & 0x40 != 0 {
        m.sh(cursor, 0);
    }
    let context = m.gw(CONTEXT);
    if m.lw(context + 0x12c) & 0x10 == 0 {
        return;
    }
    let extra = match m.lh(cursor) {
        0 => None,
        3 => Some(0x4000_0000),
        4 => Some(0x8000_0000),
        _ => return,
    };
    let flags = m.lw(context);
    m.sw(context, flags | 0x0800_0000);
    if let Some(bit) = extra {
        let gameplay = m.gw(GAMEPLAY);
        let flags = m.lw(gameplay);
        m.sw(gameplay, flags | bit);
    }
}

// ---- alternate flight update routines --------------------------------------------

/// 8006B340: 8006B1E8.
pub fn pad_8006b340(m: &mut Machine) {
    call(m, 0x8006_b1e8, [0; 4]);
}

/// 80032DE4: phase-0x10 update (state 80033D68): 8003931C, then 8006B340
/// and 8007C44C unless paused.
pub fn update_32de4(m: &mut Machine) {
    update_8003931c(m);
    if running(m) {
        pad_8006b340(m);
        call(m, 0x8007_c44c, [0; 4]);
    }
}

/// 80032E34: phase-0x80 update (state 80033DA8, replay): 8007E284, 8006B340,
/// 8007C44C, 80083A64.
pub fn update_32e34(m: &mut Machine) {
    update_8007e284(m);
    pad_8006b340(m);
    call(m, 0x8007_c44c, [0; 4]);
    call(m, 0x8008_3a64, [0; 4]);
}

/// The flight subsystem sequence shared with the regular update 80032CA4:
/// eight updates, then 80039D08 on a stack copy of the flight state's
/// target, whose results go to camera +0xBC and flight state +0x18.
fn flight_subsystems(m: &mut Machine, frame: u32) {
    let gameplay = m.gw(GAMEPLAY);
    let camera = m.lw(gameplay + 0x28);
    for routine in [
        0x8007_1358,
        0x8004_4d50,
        0x8003_8728,
        0x8005_01c8,
        0x8004_5950,
        0x8006_ade4,
        0x8007_c44c,
        0x8008_3a64,
    ] {
        call(m, routine, [0; 4]);
    }
    let state = m.lw(FLIGHT_STATE);
    let target = m.lw(state + 0x1c);
    m.sw(frame + 0x10, target);
    call(m, 0x8003_9d08, [frame + 0x10, 0, 0, 0]);
    let value = m.lw(frame + 0x20);
    let state = m.lw(FLIGHT_STATE);
    m.sw(camera + 0xbc, value);
    let target = m.lw(frame + 0x24);
    m.sw(state + 0x18, target);
}

/// 80032E6C: phase-0x400 update (state 80033ED8): 8005A874, then the flight
/// subsystems unless paused or gameplay +4 has bit 0x1000.
pub fn update_32e6c(m: &mut Machine) {
    let frame = m.push_frame(0x30);
    update_8005a874(m);
    if running(m) {
        let gameplay = m.gw(GAMEPLAY);
        if !m.lw(gameplay + 4) & 0x1000 != 0 {
            flight_subsystems(m, frame);
        }
    }
    m.pop_frame(0x30);
}

/// 80032F48: phase-0x100000 update (state 80034024): the flight subsystems
/// unless paused.
pub fn update_32f48(m: &mut Machine) {
    let frame = m.push_frame(0x30);
    if running(m) {
        flight_subsystems(m, frame);
    }
    m.pop_frame(0x30);
}

/// 80033004: phase-0x200000 update (state 80034064): nothing.
pub fn update_33004(_m: &mut Machine) {}

/// 8003300C: 800562B4 (called directly by state 80033FB4, phase 0x4000).
pub fn update_3300c(m: &mut Machine) {
    update_800562b4(m);
}

/// 8003302C: 8005971C (called directly by state 80033FEC, phase 0x20000).
pub fn update_3302c(m: &mut Machine) {
    update_8005971c(m);
}

/// 8003931C: phase-0x10 sequence player (gp+0x3DC..0x408 state).
pub fn update_8003931c(m: &mut Machine) {
    let gameplay = m.gw(GAMEPLAY);
    let phase = m.lw(gameplay + 4);
    if phase == 0x10 {
        call(m, 0x8003_9138, [0; 4]);
        call(m, 0x8003_9598, [0; 4]);
        m.sh(GP + 0x406, 0);
        m.set_gw(0x408, 0);
        return;
    }
    if phase == 0x40 {
        call(m, 0x8003_9598, [0; 4]);
        call(m, 0x8003_926c, [0; 4]);
        m.set_gw(0x3e0, 0);
        return;
    }
    call(m, 0x8003_9598, [0; 4]);
    let status = call(m, 0x8002_b87c, [0; 4]);
    if m.gw(0x3e8) != 0 {
        if (status as i32) >= 2 || m.gw(0x3f4) == 1 {
            sequence_step(m);
        } else {
            m.set_gw(0x3e8, 0);
        }
        return;
    }
    if (status as i32) < 4 && m.gw(0x3f4) != 1 {
        let count = m.gw(0x408).wrapping_add(1);
        m.set_gw(0x408, count);
        if (count as i32) < 0x96 {
            return;
        }
    } else {
        let previous = m.gw(0x3e0);
        m.set_gw(0x3e8, 1);
        m.sh(GP + 0x406, 0);
        m.set_gw(0x408, 0);
        if previous == 0 {
            m.set_gw(0x3e0, 1);
        }
        return;
    }
    sequence_finish(m);
}

/// 800394EC: end of the phase-0x10 sequence: state 5 and transition 0x40.
fn sequence_finish(m: &mut Machine) {
    let gameplay = m.gw(GAMEPLAY);
    let stage = m.lw(gameplay + 8);
    m.set_gw(0x3e0, 5);
    request_transition(m, 0x40, stage);
}

/// 800393C8..80039584: one step of the phase-0x10 sequence.
fn sequence_step(m: &mut Machine) {
    if m.lh(GP + 0x406) == 0 {
        let found = call(m, 0x8002_b5c4, [GP + 0x3fc, GP + 0x400, 0, 0]);
        if found != 0 {
            let done = m.gw(0x3f4);
            m.set_gw(0x3e8, 0);
            if done != 0 {
                sequence_finish(m);
            }
            return;
        }
        let header = m.gw(0x400);
        let count = m.lbu(header + 8);
        m.sh(GP + 0x404, u16::from(count));
    }
    let index = m.lh(GP + 0x406) as i32 as u32;
    let base = m.gw(0x3fc);
    let record = base.wrapping_add(index.wrapping_mul(0x8f).wrapping_mul(4));
    let header = m.gw(0x400);
    m.set_gw(0x3dc, record);
    let step = m.lhu(GP + 0x406);
    let total = m.lhu(header + 6);
    let context = m.gw(CONTEXT);
    let mode = m.lw(context + 0xf7c);
    m.sh(GP + 0x3f0, total.wrapping_sub(step));
    if mode == 0x16 {
        let remaining = u32::from(m.lhu(GP + 0x3f0));
        let base = m.gw(0x3e4);
        call(
            m,
            0x8009_27e4,
            [base.wrapping_sub(remaining).wrapping_add(2), 0, 0, 0],
        );
    }
    call(m, 0x8003_8b50, [0; 4]);
    call(m, 0x8003_8b60, [0; 4]);
    let record = m.gw(0x3dc);
    let size = m.lw(record) >> 2;
    call(m, 0x8009_1248, [0; 4]);
    let record = m.gw(0x3dc);
    call(m, 0x8003_90c0, [record.wrapping_add(4), 0, 0, 0]);
    let size = size << 2;
    call(m, 0x8008_3a64, [0; 4]);
    let record = m.gw(0x3dc);
    call(m, 0x8003_02e4, [record.wrapping_add(size), 0, 0, 0]);
    let step = m.lhu(GP + 0x406).wrapping_add(1);
    let total = m.lh(GP + 0x404);
    m.sh(GP + 0x406, step);
    if (step as i16) < total {
        return;
    }
    let header = m.gw(0x400);
    call(m, 0x8002_b7b8, [header, 0, 0, 0]);
    m.sh(GP + 0x406, 0);
}

/// 8007E284: phase-0x80 (replay) control: 8007DDCC in phase 0x80, 8007E1A8
/// in 0x200, otherwise 8007DF54 and the replay exit checks.
pub fn update_8007e284(m: &mut Machine) {
    let gameplay = m.gw(GAMEPLAY);
    let phase = m.lw(gameplay + 4);
    if phase == 0x80 {
        call(m, 0x8007_ddcc, [0; 4]);
        return;
    }
    if phase == 0x200 {
        call(m, 0x8007_e1a8, [0; 4]);
        return;
    }
    let result = call(m, 0x8007_df54, [0; 4]);
    let context = m.gw(CONTEXT);
    let replay = m.lbu(context + 0x3a9);
    let leave = if u32::from(replay).wrapping_sub(1) < 2 {
        m.lw(context + 0x12c) & 0x10 != 0 || result == 1
    } else if m.lw(context + 0x12c) & 0x40 != 0 && replay != 3 {
        true
    } else {
        if result == 1 {
            call(m, 0x8007_8010, [0; 4]);
            call(m, 0x8007_dc1c, [0; 4]);
            call(m, 0x8007_d5d8, [0; 4]);
        }
        false
    };
    if leave {
        let gameplay = m.gw(GAMEPLAY);
        let stage = m.lw(gameplay + 8);
        request_transition(m, 0x200, stage);
    }
}

/// 800562B4: phase-0x4000 drawing: 80055AF0 / 80055CEC in phases 0x4000 /
/// 0x10000, otherwise clear and fill its own ordering table and draw it.
pub fn update_800562b4(m: &mut Machine) {
    let gameplay = m.gw(GAMEPLAY);
    let phase = m.lw(gameplay + 4);
    if phase == 0x4000 {
        call(m, 0x8005_5af0, [0; 4]);
        return;
    }
    if phase == 0x1_0000 {
        call(m, 0x8005_5cec, [0; 4]);
        return;
    }
    let context = m.gw(CONTEXT);
    let buffer = m.lw(context + 4);
    let block = m.gw(0x4d8);
    let ot = block.wrapping_add((buffer << 3).wrapping_add(0xc));
    m.sw(block + 0x20, ot);
    let packets = m.lw(block.wrapping_add(buffer << 2) + 4);
    m.sw(block + 0x1c, packets);
    sdk::clear_otag_r(m, ot, 2);
    if call(m, 0x8005_5dcc, [0; 4]) != 0 {
        let gameplay = m.gw(GAMEPLAY);
        let stage = m.lw(gameplay + 8);
        request_transition(m, 0x1_0000, stage);
        return;
    }
    let screen = m.gw(0x204);
    if m.lw(screen + 4) != 0 {
        let block = m.gw(0x4d8);
        let ot = m.lw(block + 0x20);
        let packets = m.lw(block + 0x1c);
        let next = call(m, 0x8002_cfcc, [ot, packets, 0, 0]);
        let block = m.gw(0x4d8);
        m.sw(block + 0x1c, next);
    }
    let block = m.gw(0x4d8);
    let ot = m.lw(block + 0x20);
    sdk::draw_otag(m, ot.wrapping_add(4));
}

/// 8005971C: phase-0x20000 drawing: 800594E4 / 8005982C in phases 0x20000 /
/// 0x80000, otherwise clear and fill its own ordering table and draw it.
pub fn update_8005971c(m: &mut Machine) {
    let gameplay = m.gw(GAMEPLAY);
    let phase = m.lw(gameplay + 4);
    if phase == 0x2_0000 {
        call(m, 0x8005_94e4, [0; 4]);
        return;
    }
    if phase == 0x8_0000 {
        call(m, 0x8005_982c, [0; 4]);
        return;
    }
    let context = m.gw(CONTEXT);
    let buffer = m.lw(context + 4);
    let block = m.gw(0x4e4);
    let ot = block.wrapping_add(buffer.wrapping_mul(0x14).wrapping_add(0xc));
    m.sw(block + 0x38, ot);
    let packets = m.lw(block.wrapping_add(buffer << 2) + 4);
    m.sw(block + 0x34, packets);
    sdk::clear_otag_r(m, ot, 5);
    if call(m, 0x8005_6950, [0; 4]) != 0 {
        let gameplay = m.gw(GAMEPLAY);
        let stage = m.lw(gameplay + 8);
        request_transition(m, 0x8_0000, stage);
        return;
    }
    let screen = m.gw(0x218);
    if m.lw(screen + 4) != 0 {
        let block = m.gw(0x4e4);
        let ot = m.lw(block + 0x38);
        let packets = m.lw(block + 0x34);
        let next = call(m, 0x8002_cfcc, [ot, packets, 0, 0]);
        let block = m.gw(0x4e4);
        m.sw(block + 0x34, next);
    }
    let block = m.gw(0x4e4);
    let ot = m.lw(block + 0x38);
    sdk::draw_otag(m, ot.wrapping_add(0x10));
}

/// 8005A874: phase-0x400 control: 8005A268 / 8005A410 in phases 0x400 /
/// 0x2000, nothing in 0x1000; otherwise pad shortcuts, the help overlay and
/// the exit conditions (transition 0x2000).
pub fn update_8005a874(m: &mut Machine) {
    let index = m.gw(0x324);
    let course = (index << 2).wrapping_add(0x800b_0100);
    let gameplay = m.gw(GAMEPLAY);
    let phase = m.lw(gameplay + 4);
    match phase {
        0x400 => {
            call(m, 0x8005_a268, [0; 4]);
            return;
        }
        0x2000 => {
            call(m, 0x8005_a410, [0; 4]);
            return;
        }
        0x1000 => return,
        _ => {}
    }
    let buttons = pad_buttons(m);
    let kind = u32::from(m.lbu(course));
    let allowed = kind != 7
        && !buttons & 0x200 != 0
        && (kind.wrapping_sub(2) < 2 || {
            (kind < 2 || kind == 5 || kind == 6 || kind == 4) && {
                let gameplay = m.gw(GAMEPLAY);
                (m.lw(gameplay + 0xc) as i32) < 0x12c
            }
        })
        && !buttons & 0x40 != 0;
    if allowed {
        let context = m.gw(CONTEXT);
        if m.lw(context + 0x12c) & 0x40 != 0 {
            call(m, 0x8007_23ac, [0; 4]);
            call(m, 0x8004_1dec, [9, 0, 0, 0]);
            call(m, 0x8002_8f5c, [0; 4]);
            let progress = progress_block(m);
            m.sw(progress + 0x14, 0);
            m.sb(progress + 0x1c, 0xff);
        }
        let gameplay = m.gw(GAMEPLAY);
        if m.lw(gameplay + 0xc) & 0x10 != 0 {
            help_overlay(m);
        }
    }
    let gameplay = m.gw(GAMEPLAY);
    if m.lw(gameplay + 0xc) == 0xe10 {
        show_message(m, 0xa);
    }
    let exit = if buttons & 0x7000 != 0 {
        Some(1)
    } else {
        let gameplay = m.gw(GAMEPLAY);
        if (m.lw(gameplay + 0xc) as i32) >= 0x1518 {
            Some(4)
        } else if buttons & 0x80 != 0 {
            Some(2)
        } else if buttons & 0x100 != 0 {
            Some(0x10)
        } else if buttons & 0x200 != 0 {
            Some(0)
        } else {
            None
        }
    };
    if let Some(reason) = exit {
        request_transition(m, 0x2000, reason);
        return;
    }
    if m.lbu(course) != 7 {
        return;
    }
    let progress = progress_block(m);
    if m.lh(progress + 8) >= 0x186 {
        request_transition(m, 0x2000, 2);
        let progress = progress_block(m);
        m.sb(progress + 0x2f, 1);
    }
    let progress = progress_block(m);
    if m.lh(progress + 8) < 0 {
        request_transition(m, 0x2000, 0x20);
        let progress = progress_block(m);
        m.sb(progress + 0x2f, 0);
    }
    let gameplay = m.gw(GAMEPLAY);
    if m.lw(gameplay + 0xc) != 0x384 {
        return;
    }
    let progress = progress_block(m);
    let result = m.lbu(progress + 0xa);
    if result == 1 {
        request_transition(m, 0x2000, 2);
        let progress = progress_block(m);
        m.sb(progress + 0x2f, result);
    } else {
        request_transition(m, 0x2000, 0x20);
        let progress = progress_block(m);
        m.sb(progress + 0x2f, 0);
    }
}

/// 8005A9C8..8005AA84: the blinking help texts of 8005A874.
fn help_overlay(m: &mut Machine) {
    let context = m.gw(CONTEXT);
    let language = m.lw(context + 0x38c) << 2;
    let first = m.lw(language.wrapping_add(0x800b_05b0));
    let second = m.lw(language.wrapping_add(0x800b_05c4));
    call(m, 0x8005_ac6c, [0, 0x83, 0, 0]);
    for (table, y, text) in [(0x800b_05d8, 0xd6, first), (0x800b_05e0, 0xe4, second)] {
        let context = m.gw(CONTEXT);
        let language = m.lw(context + 0x38c);
        let x = u32::from(m.lbu(language.wrapping_add(table)));
        call(m, 0x8005_ac58, [x, y, 0, 0]);
        let gameplay = m.gw(GAMEPLAY);
        let ot = m.lw(gameplay + 0x10);
        let packets = m.lw(gameplay + 0x14);
        let next = call(m, 0x8005_ac84, [ot, packets, text, 0]);
        let gameplay = m.gw(GAMEPLAY);
        m.sw(gameplay + 0x14, next);
    }
}

/// Native targets owned by this module, by original address.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let [a0, a1, a2, a3] = args;
    let v0 = match target {
        0x8003_6e54 => session_step(m),
        0x8005_2aa0 => {
            mission_logic(m);
            0
        }
        0x8005_4e10 => {
            radio_update(m);
            0
        }
        0x8005_4c8c => radio_tick(m),
        0x8005_4dfc => radio_busy(m),
        0x8005_4c78 => {
            radio_select(m, a0);
            0
        }
        0x8005_5044 => radio_reset(m),
        0x8004_0ca4 => {
            indicator_on(m);
            0
        }
        0x8004_0cc0 => indicator(m),
        0x8004_0ccc => {
            indicator_done(m);
            0
        }
        0x8004_0fcc => {
            indicator_close(m, a0);
            0
        }
        0x8006_b360 => pad_buttons(m),
        0x8008_2f18 => intro_fade(m, a0, a1, a2),
        0x8008_2e2c => screen_fade(m, a0, a1, a2),
        0x8007_4c04 => {
            camera_74c04(m);
            0
        }
        0x8007_4c20 => {
            camera_74c20(m);
            0
        }
        0x8007_4c40 => {
            camera_74c40(m);
            0
        }
        0x8007_4c60 => {
            camera_74c60(m);
            0
        }
        0x8007_4c80 => {
            camera_74c80(m, a0);
            0
        }
        0x8007_7e68 => {
            camera_follow_player(m, a0);
            0
        }
        0x8007_7ecc => {
            camera_follow_default(m);
            0
        }
        0x8009_2a60 => {
            spawn_group(m, a0);
            0
        }
        0x8007_5934 => {
            player_hit_75934(m);
            0
        }
        0x8002_c2cc => sound_pause(m, a0),
        0x8005_5184 => {
            sound_queue_80055184(m);
            0
        }
        0x8003_0288 => music_80030288(m),
        0x8003_4428 => {
            pause_input(m);
            0
        }
        0x8003_4640 => draw_pause_menu(m, a0, a1),
        0x8003_40d8 => draw_box(m, a0, a1, a2, a3),
        0x8006_b340 => {
            pad_8006b340(m);
            0
        }
        0x8003_2de4 => {
            update_32de4(m);
            0
        }
        0x8003_2e34 => {
            update_32e34(m);
            0
        }
        0x8003_2e6c => {
            update_32e6c(m);
            0
        }
        0x8003_2f48 => {
            update_32f48(m);
            0
        }
        0x8003_3004 => {
            update_33004(m);
            0
        }
        0x8003_300c => {
            update_3300c(m);
            0
        }
        0x8003_302c => {
            update_3302c(m);
            0
        }
        0x8003_931c => {
            update_8003931c(m);
            0
        }
        0x8007_e284 => {
            update_8007e284(m);
            0
        }
        0x8005_62b4 => {
            update_800562b4(m);
            0
        }
        0x8005_971c => {
            update_8005971c(m);
            0
        }
        0x8005_a874 => {
            update_8005a874(m);
            0
        }
        _ => return None,
    };
    Some(v0)
}
