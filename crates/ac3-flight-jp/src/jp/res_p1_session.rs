//! Patches of `session` routines whose JP instructions differ from PAL
//! (REVIEW.md §1-§3b, resident part 1). Each is listed in
//! [`REPLACES`](super::REPLACES). Private helpers of the generated
//! `session.rs` that the patches need are copied here unchanged.
//!
//! JP context layout: the fields PAL keeps at +0x3A1..+0x3A9 are 4 lower.
use crate::session::{
    camera_74c04, camera_74c20, camera_74c40, camera_74c60, camera_74c80, camera_follow_player,
    indicator, indicator_close, indicator_done, indicator_on, music_80030288, pad_buttons,
    player_hit_75934, sound_queue_80055184, spawn_group,
};
use crate::{
    common::{CONTEXT, ENTITIES, GAMEPLAY, request_transition},
    dispatch,
    machine::{GP, Machine, SmallData},
};
use ac3_gte::reg;

/// 800343C8 (JP; PAL 800340D8, near, `imm:2`: the corner sprites use CLUT
/// 0x5DD6 / 0x5D56, PAL 0x57D4 / 0x5754): menu box at the guest RECT `rect` (x, y, w, h): four 8x8 corner
/// sprites, four edge tiles and three fill tiles in style `style` (1, 2 or
/// other), added to OT slot `slot`; returns `packet + 0xB0`.
pub fn draw_box(m: &mut Machine, slot: u32, packet: u32, rect: u32, style: u32) -> u32 {
    let x = m.lh(rect) as i32 as u32;
    let y = m.lh(rect.wrapping_add(2)) as i32 as u32;
    let w = m.lh(rect.wrapping_add(4)) as i32 as u32;
    let h = m.lh(rect.wrapping_add(6)) as i32 as u32;
    let (clut, corner, edge, fill) = match style {
        1 => (0x5dd6, 0x7680_8080, 0x6200_0000, 0x6248_4848),
        2 => (0x5d56, 0x7680_8080, 0x6200_0098, 0x6200_30f8),
        _ => (0x5d56, 0x7480_8080, 0x6000_0098, 0x6000_30f8),
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

/// Add the 0x10-byte primitive `prim` (len 3) to OT slot `slot`.
fn add_prim3(m: &mut Machine, slot: u32, prim: u32) {
    let link = m.lw(slot);
    m.sw(prim, link | 0x0300_0000);
    m.sw(slot, (prim << 8) >> 8);
}

/// 80034718 (JP; PAL 80034428, near, `imm:5`: the options at context +0x39D
/// / +0x39E, PAL +0x3A1 / +0x3A2): pause menu input: move the cursor (0..4, wrapping), toggle the
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
            let value = m.lbu(context + 0x39d);
            m.sb(context + 0x39d, u8::from(value == 0));
        }
    }
    if m.lh(cursor) == 2 {
        let context = m.gw(CONTEXT);
        if m.lw(context + 0x130) & 0xc != 0 {
            let value = m.lbu(context + 0x39e);
            m.sb(context + 0x39e, u8::from(value == 0));
            let context = m.gw(CONTEXT);
            let request = u32::from(m.lbu(context + 0x39e) == 0);
            call(m, 0x8003_5740, [request, 0, 0, 0]);
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

fn call(m: &mut Machine, address: u32, args: [u32; 4]) -> u32 {
    dispatch::call(m, address, args)
}

/// 8007953C (JP; PAL 80077ECC, near, `imm:1`): [`camera_follow_player`]
/// with context byte +0x39F (PAL +0x3A3).
pub fn camera_follow_default(m: &mut Machine) {
    let context = m.gw(CONTEXT);
    let outside = u32::from(m.lbu(context + 0x39f));
    camera_follow_player(m, outside);
}

/// 80084474 (JP; PAL 80082E2C, near, `imm:2`: sprites over 320x240 from
/// (-0xA0, -0x78), PAL 320x256 from (-0xA0, -0x80)): screen fade toward `level` (0: nothing; negative: subtractive
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
    let end = fade_sprites(m, first, 0x46d6, 0xff88_ff60, 0x00f0_0140);
    fade_close(m, slot, packet, end)
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

/// The packets' `lwl/swl` splice at +2: `head` takes the link of `slot`, and
/// `slot` then links to `first`. OT slots and packet pointers are word
/// aligned, so the register byte `lwl` preserves is never stored.
fn splice(m: &mut Machine, slot: u32, head: u32, first: u32) {
    let link = lwl(m, slot.wrapping_add(2), 0);
    swl(m, head.wrapping_add(2), link);
    swl(m, slot.wrapping_add(2), first << 8);
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

/// Little-endian `lwl rt, address` with the register's previous value.
fn lwl(m: &mut Machine, address: u32, rt: u32) -> u32 {
    let k = address & 3;
    let word = m.lw(address & !3);
    let shift = 8 * (3 - k);
    let keep = if k == 3 { 0 } else { u32::MAX >> (8 * (k + 1)) };
    (rt & keep) | (word << shift)
}

/// 80084318 (depth-cued noise sprites; RGBC/IR0 set by the caller).
fn fade_sprites(m: &mut Machine, packet: u32, clut: u32, xy: u32, wh: u32) -> u32 {
    call(m, 0x8008_4318, [packet, clut, xy, wh])
}

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

/// 80084560 (JP; PAL 80082F18, near, `imm:2`: sprites over 320x240, as
/// 80084474): intro fade (level `level`, 0x1000 = black): a mask packet, a
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
    let end = fade_sprites(m, first, 0x401d, 0xff88_ff60, 0x00f0_0140);
    fade_close(m, slot, packet, end)
}

/// 8007F8E4 (JP; PAL 8007E284, near, `del:8 imm:4 ins:2`): phase-0x80
/// (replay) control: 8007F448 in phase 0x80, 8007F808 in 0x200, otherwise
/// 8007F5B4 and the replay exit checks. JP: any replay mode (context
/// +0x3A5) leaves on confirm or at the end (PAL: modes 1 and 2; mode 3 kept
/// playing on START); mode 0 leaves on START.
pub fn update_8007e284(m: &mut Machine) {
    let gameplay = m.gw(GAMEPLAY);
    let phase = m.lw(gameplay + 4);
    if phase == 0x80 {
        call(m, 0x8007_f448, [0; 4]);
        return;
    }
    if phase == 0x200 {
        call(m, 0x8007_f808, [0; 4]);
        return;
    }
    let result = call(m, 0x8007_f5b4, [0; 4]);
    let context = m.gw(CONTEXT);
    let replay = m.lbu(context + 0x3a5);
    let leave = if replay != 0 {
        m.lw(context + 0x12c) & 0x10 != 0 || result == 1
    } else if m.lw(context + 0x12c) & 0x40 != 0 {
        true
    } else {
        if result == 1 {
            call(m, 0x8007_9680, [0; 4]);
            call(m, 0x8007_f298, [0; 4]);
            call(m, 0x8007_ec54, [0; 4]);
        }
        false
    };
    if leave {
        let gameplay = m.gw(GAMEPLAY);
        let stage = m.lw(gameplay + 8);
        request_transition(m, 0x200, stage);
    }
}

// ---- the JP radio messages (8005628C, 80056518) -------------------------------------

/// `[gp+0x4AC]`: the radio queue (+0 the playing message or 0xFF, +4/+0xC
/// four wait counters and their targets, +0x14 four queued messages).
const RADIO: u32 = 0x4ac;
/// `[gp+0x4C0]`: the message table, 4 bytes per message: +0 kind (0: a
/// voice clip, else a sequence), +1 clip, +2 sequence index.
const RADIO_MESSAGES: u32 = 0x4c0;
/// `[gp+0x4B8]`: the sequence table (words) of the messages of kind 1.
const RADIO_SEQUENCES: u32 = 0x4b8;
/// `gp+0x4B4`: the radio state (0 idle, 1 clip, 2 sequence, 3 waiting for
/// the indicator).
const RADIO_STATE: u32 = 0x4b4;
/// `[gp+0x4BC]`: the sequence player block (+7 its voice).
const RADIO_PLAYER: u32 = 0x4bc;
/// 800BCE8E: set while the radio indicator is open (JP).
const RADIO_INDICATOR_OPEN: u32 = 0x800b_ce8e;
/// JP-only radio routines (resident part 3): start a clip, start a
/// sequence, step the clip, step the sequence.
const RADIO_START_CLIP: u32 = 0x8005_65e8;
const RADIO_START_SEQUENCE: u32 = 0x8005_671c;
const RADIO_STEP_CLIP: u32 = 0x8005_6634;
const RADIO_STEP_SEQUENCE: u32 = 0x8005_6778;
/// 8002D544 (exact, no owner yet): whether the sound request `[gp+0x314]`
/// +4 is busy.
const SOUND_REQUEST_BUSY: u32 = 0x8002_d544;

/// `~[context] & 0x20000000`: false while the game is paused.
fn running(m: &mut Machine) -> bool {
    let context = m.gw(CONTEXT);
    !m.lw(context) & 0x2000_0000 != 0
}

/// 8005628C (JP; PAL 80054E10, changed, `call+:2 del:6 imm:8 ins:28 op:10
/// reg:16`): start queued radio messages whose wait elapsed (with context
/// +0x39C = 1 only once the indicator is open; PAL: only then), each a
/// clip (800565E8) or a sequence (8005671C) from the message table; step
/// the clip and the sequence (80056634, 80056778; PAL's sequence player
/// 80054C8C is gone); when neither plays and the sound request is idle,
/// settle the radio state; close the indicator once the queue is empty
/// (with 800BCE8E set) and keep it open while a message plays.
pub fn radio_update(m: &mut Machine) {
    m.push_frame(0x20);
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
            if m.lbu(context + 0x39c) == 1 {
                indicator_on(m);
                if indicator(m) != 2 {
                    m.set_gw(RADIO_STATE, 3);
                    continue;
                }
            }
            let table = m.gw(RADIO_MESSAGES);
            let entry = (u32::from(message) << 2).wrapping_add(table);
            if m.lbu(entry) == 0 {
                let clip = u32::from(m.lbu(entry + 1));
                call(m, RADIO_START_CLIP, [clip, 0, 0, 0]);
            } else {
                let index = m.lh(entry + 2) as i32 as u32;
                let sequences = m.gw(RADIO_SEQUENCES);
                call(
                    m,
                    RADIO_START_SEQUENCE,
                    [sequences.wrapping_add(index << 2), 0, 0, 0],
                );
            }
            let context = m.gw(CONTEXT);
            let flags = m.lw(context);
            let queue = m.gw(RADIO);
            m.sw(context, flags | 0x1000_0000);
            m.sw(queue, u32::from(message));
            m.sb(queue.wrapping_add(slot) + 0x14, 0xff);
        }
    }
    let clip = call(m, RADIO_STEP_CLIP, [0; 4]);
    let sequence = call(m, RADIO_STEP_SEQUENCE, [0; 4]);
    if clip == 0 && sequence == 0 && call(m, SOUND_REQUEST_BUSY, [0; 4]) == 0 {
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
    if m.lw(queue) == 0xff && m.lbu(RADIO_INDICATOR_OPEN) != 0 {
        m.sb(RADIO_INDICATOR_OPEN, 0);
        indicator_close(m, 1);
    }
    let context = m.gw(CONTEXT);
    if m.lbu(context + 0x39c) == 1 {
        let queue = m.gw(RADIO);
        if m.lw(queue) != 0xff {
            indicator_on(m);
        } else {
            indicator_done(m);
        }
    }
    m.pop_frame(0x20);
}

/// 80056518 (JP; PAL 80055044, changed, `call+:3 imm:1 ins:15 op:5`): stop
/// the radio by its state: a clip flushes the sound sequencer (8002C71C),
/// a sequence stops its voice (8002DDA8) and restarts the player empty
/// (8005671C(0)), a wait for the indicator ends (state 0, 8004167C); then
/// the indicator closes (8004197C(0)), no message plays and all four slots
/// are free. Returns 0.
pub fn radio_reset(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    match m.gw(RADIO_STATE) {
        1 => {
            call(m, 0x8002_c71c, [0; 4]);
        }
        2 => {
            let player = m.gw(RADIO_PLAYER);
            let voice = m.lbu(player + 7);
            if voice != 0xff {
                call(m, 0x8002_dda8, [u32::from(voice), 0, 0, 0]);
            }
            call(m, RADIO_START_SEQUENCE, [0; 4]);
        }
        3 => {
            m.set_gw(RADIO_STATE, 0);
            indicator_done(m);
        }
        _ => {}
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
    m.pop_frame(0x18);
    0
}

/// 800540E0 (JP; PAL 80052AA0, near, `call+:3 del:30 imm:16 ins:124 op:25
/// ref:1 reg:16`): per-frame mission logic: phase-2 intro spawns, phase-4
/// timers, scripted triggers of the individual missions, the phase-0x800
/// clock and the radio update. JP: the mission number is context +0x38C,
/// phase 2 no longer runs the clock, mission 0x1E has a flickering screen
/// fade (frames 0x4B0..0xF3C, [`mission_1e`]) and on-screen messages are
/// 8004CEB0.
pub fn mission_logic(m: &mut Machine) {
    m.push_frame(0x28);
    let gameplay = m.gw(GAMEPLAY);
    let phase = m.lw(gameplay + 4);
    let stage = m.lw(gameplay + 8);
    if phase == 2 {
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
    m.pop_frame(0x28);
}

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

fn progress_block(m: &mut Machine) -> u32 {
    m.gw(PROGRESS)
}

/// `[gp+0x340]`: mission progress block.
const PROGRESS: u32 = 0x340;

/// Phase-4 part of 800540E0 (80054208..80054D04).
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
                call(m, 0x8005_4ebc, [script, 0, 0, 0]);
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
            let target = m.lw(0x800c_6ed8);
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

/// Mission triggers of 800540E0 (PAL 800530BC..80054D04).
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
                        call(m, 0x8005_4ebc, [script, 0, 0, 0]);
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
            let count = m.gw(0x49c);
            m.set_gw(0x49c, count.wrapping_add(1));
        } else {
            m.set_gw(0x49c, 0);
        }
        if m.gw(0x49c) == 0x4650 {
            run_script(m, 0x148);
        }
    }
    if mission_number(m) == 0x2c {
        mission_2c(m);
    }
    if mission_number(m) == 0x1e {
        mission_1e(m);
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
        if m.lw(context + 0x394) == 0 {
            if (m.lw(player + 0x10) as i32) > 0x11d2_8000 {
                run_script(m, 0x14c);
            }
        } else if (m.lw(player + 0x10) as i32) > 0x343f_0000 {
            run_script(m, 0x148);
        }
    }
}

/// `interpret` (80054EBC, script module) on script table word `offset`.
fn run_script(m: &mut Machine, offset: u32) {
    let scripts = m.gw(SCRIPTS);
    let script = m.lw(scripts.wrapping_add(offset));
    call(m, 0x8005_4ebc, [script, 0, 0, 0]);
}

/// `[gp+0x338]`: mission script table.
const SCRIPTS: u32 = 0x338;

fn player_entity(m: &mut Machine) -> u32 {
    let entities = m.gw(ENTITIES);
    m.lw(entities)
}

fn mission_number(m: &mut Machine) -> i32 {
    let context = m.gw(CONTEXT);
    i32::from(m.lh(context + 0x38c))
}

/// `gp+0x4A0`: the screen fade level of mission 0x1E.
const FLICKER: u32 = 0x4a0;

/// Mission 0x1E (JP only, 800549C0..80054B28), stage 0, frames 0x4B0 to
/// 0xF3C: each frame `rand()` and context bit 0x10000000; at 0x4B0 the
/// sound queue 80056A10 and level 0; the level then rises by 0x12 (to
/// 0x528), is 0x800 (to 0x5A0), `0x300 + (rand & 0xFF)` (to 0xC4E), rises
/// by 8 (to 0xD7A) and by 0x10 (to 0xDD4), is -0x800 (to 0xE88) and rises
/// by 0x10 back to 0 (to 0xF3C; the rises stop at 0x800); the screen fade
/// 80084474 of that level is drawn every frame.
fn mission_1e(m: &mut Machine) {
    let gameplay = m.gw(GAMEPLAY);
    if m.lw(gameplay + 8) != 0 {
        return;
    }
    if m.lw(gameplay + 0xc).wrapping_sub(0x4b0) >= 0xa8d {
        return;
    }
    let random = crate::sdk::rand(m);
    set_context_flags(m, 0x1000_0000);
    let gameplay = m.gw(GAMEPLAY);
    let frame = m.lw(gameplay + 0xc) as i32;
    let rise = |m: &mut Machine, step: u32| {
        let level = m.gw(FLICKER);
        if (level as i32) < 0x800 {
            m.set_gw(FLICKER, level.wrapping_add(step));
        }
    };
    if frame == 0x4b0 {
        sound_queue_80055184(m);
        m.set_gw(FLICKER, 0);
    } else if frame < 0x528 {
        rise(m, 0x12);
    } else if frame < 0x5a0 {
        m.set_gw(FLICKER, 0x800);
    } else if frame < 0xc4e {
        m.set_gw(FLICKER, (random & 0xff).wrapping_add(0x300));
    } else if frame < 0xd7a {
        rise(m, 8);
    } else if frame < 0xdd4 {
        rise(m, 0x10);
    } else if frame < 0xe88 {
        m.set_gw(FLICKER, -0x800i32 as u32);
    } else if frame < 0xf3c {
        let level = m.gw(FLICKER);
        if (level as i32) < 0 {
            m.set_gw(FLICKER, level.wrapping_add(0x10));
        }
    }
    let gameplay = m.gw(GAMEPLAY);
    let level = m.gw(FLICKER);
    let slot = m.lw(gameplay + 0x10).wrapping_add(4);
    let packet = m.lw(gameplay + 0x14);
    let next = screen_fade(m, slot, packet, level);
    let gameplay = m.gw(GAMEPLAY);
    m.sw(gameplay + 0x14, next);
}

/// Mission 0x2C trigger of 800540E0 (800548AC..800549BC).
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

/// 8004287C (raise priority message `id`; `hud.rs`).
fn priority_message(m: &mut Machine, id: u32) {
    call(m, 0x8004_287c, [id, 0, 0, 0]);
}

/// The 0xE/0x35 countdown of 800540E0 (80054484..800546E0).
fn mission_countdown(m: &mut Machine, number: i32) {
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

/// 8004CEB0 (JP-only, resident part 3; PAL 8004B760): show the on-screen
/// message `id`.
fn show_message(m: &mut Machine, id: u32) {
    call(m, 0x8004_ceb0, [id, 0, 0, 0]);
}

/// `[gp+0x33C]`: per-mission limits (phase table).
const LIMITS: u32 = 0x33c;

/// 800371C4 (JP; PAL 80036E54, near, `call-:2 del:43 imm:53 ins:26 op:13`):
/// per-frame session step (frame tail 80033DC0, unless replaying). JP: the
/// mission number is context +0x38C, the camera choice context +0x39F (PAL
/// +0x3A3), on-screen messages 8004CEB0; the result abort no longer
/// flushes the sound, and a cleared result uses one timing for all
/// missions but 6 (PAL: also 0x37, and a sound cue at half the message
/// frame).
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

/// The remaining phases of 800371C4: 0x10 (mission 0x2E start) and 0x100000
/// (ending); every other bit returns.
fn session_other(m: &mut Machine, gameplay: u32, phase: u32, frame: u32) {
    if phase & 0x10 != 0 {
        if m.lw(gameplay + 0xc) == 0 && mission_number(m) == 0x2e && m.lw(gameplay + 8) == 2 {
            call(m, 0x8004_8ad4, [0; 4]);
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
            call(m, 0x8007_2780, [start, 0, 0, 0]);
        }
        camera_74c80(m, 0);
        let context = m.gw(CONTEXT);
        let outside = u32::from(m.lbu(context + 0x39f));
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
    call(m, 0x8003_7e50, [0; 4]);
    music_80030248(m);
}

fn music_80030248(m: &mut Machine) {
    call(m, 0x8003_048c, [0; 4]);
}

const ENDING_STEP: u32 = GP + 0x369;

/// 8005650C (the radio state word `gp+0x4B4`; `hud.rs`).
fn radio_state(m: &mut Machine) -> u32 {
    call(m, 0x8005_650c, [0; 4])
}

/// 80056234 (queue a radio message).
fn radio_queue(m: &mut Machine, message: u32, frames: u32) {
    call(m, 0x8005_6234, [message, frames, 0, 0]);
}

/// 80030464 / 8003048C (sound channel resets).
fn music_80030220(m: &mut Machine) {
    call(m, 0x8003_0464, [0; 4]);
}

/// Phase 8 (result) of 800371C4.
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
            let progress = progress_block(m);
            m.sw(context, flags | 0x1000_0000);
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

/// Phase 8 with a cleared stage (800377F0..80037A30).
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
    let end = if mission_number(m) == 6 {
        m.set_gw(FADE_FRAME, 0x3c);
        m.set_gw(MESSAGE_FRAME, 0);
        0x5a
    } else {
        m.set_gw(MESSAGE_FRAME, 0x3c);
        m.set_gw(FADE_FRAME, 0xb4);
        0xd2
    };
    m.set_gw(END_FRAME, end);
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
    call(m, 0x8004_2524, [0; 4]); // message slot reset (hud.rs)
    call(m, 0x8003_7e50, [0; 4]);
}

const FADE_STARTED: u32 = GP + 0x368;

/// Result-countdown frames of 800371C4: `[gp+0x35C]` end, `[gp+0x360]`
/// fade start, `[gp+0x364]` message frame, `[gp+0x368]`/`[gp+0x369]` flags.
const END_FRAME: u32 = 0x35c;

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

const FADE_FRAME: u32 = 0x360;

const MESSAGE_FRAME: u32 = 0x364;

/// Phase 8 with stage 0 (failed) (80037A34..80037BA8).
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
        call(m, 0x8008_49ec, [0; 4]);
        call(m, 0x8004_596c, [0; 4]);
    }
    call(m, 0x8004_2524, [0; 4]); // message slot reset (hud.rs)
    radio_reset(m);
    call(m, 0x8003_7e50, [0; 4]);
}

/// 8002C71C (sound sequencer flush; called with a0 = 0).
fn sound_flush(m: &mut Machine) {
    call(m, 0x8002_c71c, [0; 4]);
}

fn set_context_flags(m: &mut Machine, bits: u32) {
    let context = m.gw(CONTEXT);
    let flags = m.lw(context);
    m.sw(context, flags | bits);
}

/// Phase 4 (mission) of 800371C4: skip, time-out, area and end checks, then
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

/// Remaining-time sounds of 800371C4 phase 4 (80037644..80037738).
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

/// 8002DB4C (play sound effect).
fn sound_effect(m: &mut Machine, effect: u32) {
    call(m, 0x8002_db4c, [effect, 0, 0, 0]);
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

/// The area check of 800371C4 phase 4 (true: outside).
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

/// Phase 2 (intro) of 800371C4.
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
    let outside = u32::from(m.lbu(context + 0x39f));
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

/// 800396E8 (JP; PAL 8003931C, near, `call+:2 imm:6 ins:28 op:1 reg:3`):
/// phase-0x10 sequence player (gp+0x3EC..0x418 state). Phase 0x10 starts
/// the sequence (80039430, 800399D4), phase 0x40 ends it (800399D4,
/// 800395D4). Otherwise JP first stops the music (8002CC44 0xFF) while
/// fewer than 0x17 frames are left (gp+0x400), then the fade (800399D4) and
/// the stream status (8002BCA4) drive the steps ([`sequence_step`]), the
/// wait for the stream (gp+0x3F8 = 1, fade state 1) and the 0x96-frame
/// timeout ([`sequence_finish`]). The original's 0x18-byte frame is kept
/// (its callees' frames, and the stack words they read uninitialized, sit
/// below it).
pub fn update_8003931c(m: &mut Machine) {
    const FRAME: u32 = 0x18;
    m.push_frame(FRAME);
    sequence_player(m);
    m.pop_frame(FRAME);
}

fn sequence_player(m: &mut Machine) {
    let gameplay = m.gw(GAMEPLAY);
    let phase = m.lw(gameplay + 4);
    if phase == 0x10 {
        call(m, 0x8003_9430, [0; 4]);
        call(m, 0x8003_99d4, [0; 4]);
        m.sh(GP + 0x416, 0);
        m.set_gw(0x418, 0);
        return;
    }
    if phase == 0x40 {
        call(m, 0x8003_99d4, [0; 4]);
        call(m, 0x8003_95d4, [0; 4]);
        m.set_gw(0x3f0, 0);
        return;
    }
    if m.lhu(GP + 0x400) < 0x17 {
        call(m, 0x8002_cc44, [0xff, 0, 0, 0]);
    }
    call(m, 0x8003_99d4, [0; 4]);
    let status = call(m, 0x8002_bca4, [0; 4]);
    if m.gw(0x3f8) != 0 {
        if (status as i32) >= 2 || m.gw(0x404) == 1 {
            sequence_step(m);
        } else {
            m.set_gw(0x3f8, 0);
        }
        return;
    }
    if (status as i32) < 4 && m.gw(0x404) != 1 {
        let count = m.gw(0x418).wrapping_add(1);
        m.set_gw(0x418, count);
        if (count as i32) < 0x96 {
            return;
        }
    } else {
        let previous = m.gw(0x3f0);
        m.set_gw(0x3f8, 1);
        m.sh(GP + 0x416, 0);
        m.set_gw(0x418, 0);
        if previous == 0 {
            m.set_gw(0x3f0, 1);
        }
        return;
    }
    sequence_finish(m);
}

/// 80039900: end of the phase-0x10 sequence: state 5 and transition 0x40.
fn sequence_finish(m: &mut Machine) {
    let gameplay = m.gw(GAMEPLAY);
    let stage = m.lw(gameplay + 8);
    m.set_gw(0x3f0, 5);
    request_transition(m, 0x40, stage);
}

/// 800397B4..800399C0 (JP): one step of the phase-0x10 sequence. The
/// sequence mode is context +0x1038 (PAL +0xF7C): mode 0x26 runs 80093FC0
/// and mode 0x48 80094380 (PAL: mode 0x16, 800927E4) with the frames left;
/// modes 0x4B and 0x25 skip the 80030528 call of the step's tail record.
fn sequence_step(m: &mut Machine) {
    if m.lh(GP + 0x416) == 0 {
        let found = call(m, 0x8002_b9ec, [GP + 0x40c, GP + 0x410, 0, 0]);
        if found != 0 {
            let done = m.gw(0x404);
            m.set_gw(0x3f8, 0);
            if done != 0 {
                sequence_finish(m);
            }
            return;
        }
        let header = m.gw(0x410);
        let count = m.lbu(header + 8);
        m.sh(GP + 0x414, u16::from(count));
    }
    let index = m.lh(GP + 0x416) as i32 as u32;
    let base = m.gw(0x40c);
    let record = base.wrapping_add(index.wrapping_mul(0x8f).wrapping_mul(4));
    let header = m.gw(0x410);
    m.set_gw(0x3ec, record);
    let step = m.lhu(GP + 0x416);
    let total = m.lhu(header + 6);
    let context = m.gw(CONTEXT);
    let mode = m.lw(context + 0x1038);
    m.sh(GP + 0x400, total.wrapping_sub(step));
    let effect = match mode {
        0x26 => Some(0x8009_3fc0),
        0x48 => Some(0x8009_4380),
        _ => None,
    };
    if let Some(effect) = effect {
        let remaining = u32::from(m.lhu(GP + 0x400));
        let base = m.gw(0x3f4);
        call(
            m,
            effect,
            [base.wrapping_sub(remaining).wrapping_add(2), 0, 0, 0],
        );
    }
    call(m, 0x8003_8e48, [0; 4]);
    call(m, 0x8003_8e58, [0; 4]);
    let record = m.gw(0x3ec);
    let size = m.lw(record);
    call(m, 0x8009_2ab8, [0; 4]);
    let record = m.gw(0x3ec);
    call(m, 0x8003_93b8, [record.wrapping_add(4), 0, 0, 0]);
    call(m, 0x8008_50d8, [0; 4]);
    let context = m.gw(CONTEXT);
    let mode = m.lw(context + 0x1038);
    if mode != 0x4b && mode != 0x25 {
        let record = m.gw(0x3ec);
        call(
            m,
            0x8003_0528,
            [record.wrapping_add((size >> 2) << 2), 0, 0, 0],
        );
    }
    let step = m.lhu(GP + 0x416).wrapping_add(1);
    let total = m.lh(GP + 0x414);
    m.sh(GP + 0x416, step);
    if (step as i16) < total {
        return;
    }
    let header = m.gw(0x410);
    call(m, 0x8002_bbe0, [header, 0, 0, 0]);
    m.sh(GP + 0x416, 0);
}

/// 8005C270 (JP; PAL 8005A874, changed): phase-0x400 control (the course
/// table entry `[gp+0x334]` of 800B1FC4): 8005BC64 / 8005BE0C in phases
/// 0x400 / 0x2000, nothing in 0x1000; otherwise pad shortcuts, the help
/// texts and the exit conditions (transition 0x2000). JP draws the help
/// texts (gameplay +0xC bit 0x10) as two 8003563C sprites, (0x50, 0xD0)
/// image 80024548 and (0x60, 0xD8) image 8002455C, both level 0x8300 (PAL:
/// blinking per-language texts); the 0xE10 message is 8004CEB0's.
pub fn update_8005a874(m: &mut Machine) {
    const FRAME: u32 = 0x28;
    let sp = m.push_frame(FRAME);
    course_control(m, sp);
    m.pop_frame(FRAME);
}

/// 8003563C (`loading`): a bar/text sprite (ot, prim, x, y; stack +0x10
/// level, +0x14 image) -> next primitive.
const SPRITE: u32 = 0x8003_563c;

fn course_control(m: &mut Machine, sp: u32) {
    let index = m.gw(0x334);
    let course = (index << 2).wrapping_add(0x800b_1fc4);
    let gameplay = m.gw(GAMEPLAY);
    let phase = m.lw(gameplay + 4);
    match phase {
        0x400 => {
            call(m, 0x8005_bc64, [0; 4]);
            return;
        }
        0x2000 => {
            call(m, 0x8005_be0c, [0; 4]);
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
            call(m, 0x8007_3a30, [0; 4]);
            call(m, 0x8004_291c, [9, 0, 0, 0]);
            call(m, 0x8002_937c, [0; 4]);
            let progress = progress_block(m);
            m.sw(progress + 0x14, 0);
            m.sb(progress + 0x1c, 0xff);
        }
        let gameplay = m.gw(GAMEPLAY);
        if m.lw(gameplay + 0xc) & 0x10 != 0 {
            for (x, y, image) in [(0x50, 0xd0, 0x8002_4548), (0x60, 0xd8, 0x8002_455c)] {
                let gameplay = m.gw(GAMEPLAY);
                m.sw(sp + 0x10, 0x8300);
                m.sw(sp + 0x14, image);
                let ot = m.lw(gameplay + 0x10);
                let packets = m.lw(gameplay + 0x14);
                let next = call(m, SPRITE, [ot, packets, x, y]);
                let gameplay = m.gw(GAMEPLAY);
                m.sw(gameplay + 0x14, next);
            }
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
