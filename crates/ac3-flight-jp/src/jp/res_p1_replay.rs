//! Patches of `replay` routines whose JP instructions differ from PAL
//! (REVIEW.md §1-§3b, resident part 1): the mission replay. Each is listed
//! in [`REPLACES`](super::REPLACES). Private helpers of the generated
//! `replay.rs` that the patches need are copied here unchanged.
//!
//! JP context layout: the replay mode PAL keeps at +0x3A9 is at +0x3A5 and
//! the mission halfword PAL keeps at +0x390 is at +0x38C.
use crate::replay::{
    camera_follow_player, camera_replay_view, camera_set_eye, camera_set_target, camera_view_apply,
    course_targets_reset, emitters_clear, entities_clear_hidden, music_none, music_select,
    pool_alloc, record_play, replay_rewind, replay_skip_frames, sequence_commands,
    sequence_restore, sound_channel_volume, sound_listener_reset, sound_stop_all,
    stream_entry_negative, stream_queue,
};
use crate::{
    common::{CONTEXT, ENTITIES, GAMEPLAY, request_transition},
    dispatch,
    machine::{Fault, GP, Machine, SmallData},
    sdk,
};
use ac3_gte::{ctrl, reg};

/// 8007F808 (JP; PAL 8007E1A8, near, `imm:1`: the replay mode at context
/// +0x3A5): end the replay (phase 0x200): flush the sequencer, rewind,
/// replay the effects forward, clear the buffer counters, allow pausing,
/// restore the entities and slots, then either block pausing again (replay
/// mode) or run the mission step 80037E50.
pub fn replay_finish(m: &mut Machine) {
    call(m, 0x8002_c71c, [0; 4]);
    replay_rewind(m);
    replay_skip_frames(m, 1);
    let buffer = replay_buffer(m);
    let context = m.gw(CONTEXT);
    m.sw(buffer, 0);
    m.sw(buffer + 0x1018, 0);
    let flags = m.lw(context);
    m.sw(context, flags & 0xefff_ffff);
    restore_entities(m);
    call(m, 0x8006_c530, [0; 4]);
    let context = m.gw(CONTEXT);
    if m.lbu(context + 0x3a5) != 0 {
        let gameplay = m.gw(GAMEPLAY);
        let flags = m.lw(gameplay);
        m.sw(gameplay, flags | 0x1000_0000);
    } else {
        call(m, 0x8003_7e50, [0; 4]);
    }
}

fn call(m: &mut Machine, address: u32, args: [u32; 4]) -> u32 {
    dispatch::call(m, address, args)
}

/// Restore the 64 entities from the replay buffer (+0x1850) with 8002759C.
fn restore_entities(m: &mut Machine) {
    for i in 0..0x40u32 {
        let buffer = replay_buffer(m);
        let target = entity(m, i);
        copy_words(m, target, buffer + 0x1850 + i * 0x20, 8);
    }
}

/// 8002759C (shared): copy `words` words from `src` to `dst`.
fn copy_words(m: &mut Machine, dst: u32, src: u32, words: u32) {
    call(m, 0x8002_759c, [dst, src, words, 0]);
}

fn entity(m: &mut Machine, index: u32) -> u32 {
    let table = m.gw(ENTITIES);
    m.lw((index << 2).wrapping_add(table))
}

fn replay_buffer(m: &mut Machine) -> u32 {
    m.gw(REPLAY)
}

/// `[gp+0x544]`: replay buffer. +0 record count, +4/+8 ring start/end,
/// +0x10 first record, +0x14 frame-done flag, +0x18 entity snapshots (64 x
/// 0x20), +0x1018 frame, +0x101C cursor, +0x1020 entity copies (64 x 0x20),
/// +0x1820 camera offset, +0x1830 target offset, +0x1840..0x1848 camera
/// target state, +0x1850 saved entities (64 x 0x20).
const REPLAY: u32 = 0x544;

/// 8007F5B4 (JP; PAL 8007DF54, near, `del:2 imm:2 ins:2`: a followed
/// entity of kind 0x23, 0x24, 0x2B or 0x3D is a ground target, PAL 0x32,
/// 0x33, 0x3D or 0x4D): play one replay frame; returns 1 once every recorded frame
/// was played (else 0).
pub fn replay_frame(m: &mut Machine) -> u32 {
    let buffer = replay_buffer(m);
    let played = m.lw(buffer + 0x1018);
    if (played as i32) >= (m.lw(buffer) as i32) {
        return 1;
    }
    let target = m.lw(buffer + 0x1840);
    let mut cursor = m.lw(buffer + 0x101c);
    m.sw(buffer + 0x1018, played.wrapping_add(1));
    m.sw(buffer + 0x14, 0);
    loop {
        let record = wrap_cursor(m, cursor);
        cursor = record_play(m, record);
        let buffer = replay_buffer(m);
        if m.lw(buffer + 0x14) != 0 {
            break;
        }
    }
    let buffer = replay_buffer(m);
    let current = m.lw(buffer + 0x1840);
    m.sw(buffer + 0x101c, cursor);
    if target != current {
        m.sw(buffer + 0x1848, 0x96);
        if target == u32::MAX {
            let current = m.lw(buffer + 0x1840);
            m.sw(buffer + 0x1844, current);
        }
    }
    let buffer = replay_buffer(m);
    let delay = m.lw(buffer + 0x1848);
    if delay != 0 {
        m.sw(buffer + 0x1848, delay.wrapping_sub(1));
    } else {
        let current = m.lw(buffer + 0x1840);
        m.sw(buffer + 0x1844, current);
    }
    // Camera offset: the player's recorded copy minus the live player.
    let table = m.gw(ENTITIES);
    let player = m.lw(table);
    let buffer = replay_buffer(m);
    let px = m.lw(player + 8);
    let py = m.lw(player + 0xc);
    let pz = m.lw(player + 0x10);
    let copy = buffer + 0x1020;
    let cx = m.lw(copy + 8);
    let cy = m.lw(copy + 0xc);
    let cz = m.lw(copy + 0x10);
    let followed = m.lw(buffer + 0x1844);
    m.sw(buffer + 0x1820, cx.wrapping_sub(px));
    m.sw(buffer + 0x1824, cy.wrapping_sub(py));
    m.sw(buffer + 0x1828, cz.wrapping_sub(pz));
    if (followed as i32) >= 0 {
        let target = m.lw((followed << 2).wrapping_add(table));
        let tx = m.lw(target + 8);
        let ty = m.lw(target + 0xc);
        let tz = m.lw(target + 0x10);
        let copy = buffer.wrapping_add((followed << 5).wrapping_add(0x1020));
        let cx = m.lw(copy + 8);
        let cz = m.lw(copy + 0x10);
        m.sw(buffer + 0x1830, cx.wrapping_sub(tx));
        m.sw(buffer + 0x1838, cz.wrapping_sub(tz));
        let cy = m.lw(copy + 0xc);
        m.sw(buffer + 0x1834, cy.wrapping_sub(ty));
    }
    for i in 0..0x40u32 {
        let buffer = replay_buffer(m);
        let target = entity(m, i);
        copy_words(m, target, buffer + 0x1020 + i * 0x20, 8);
    }
    let player = entity(m, 0);
    let buffer = replay_buffer(m);
    camera_set_eye(m, player.wrapping_add(8), buffer + 0x1820);
    let buffer = replay_buffer(m);
    let followed = m.lw(buffer + 0x1844);
    let (source, mode) = if (followed as i32) < 0 {
        (0, 0)
    } else {
        let target = entity(m, followed);
        let source = target.wrapping_add(8);
        let mode = if (followed as i32) < 0x20 {
            1
        } else {
            match m.lh(target + 0x48) {
                0x23 | 0x24 | 0x2b | 0x3d => 1,
                _ => 2,
            }
        };
        (source, mode)
    };
    let buffer = replay_buffer(m);
    camera_set_target(m, source, buffer + 0x1830, mode);
    0
}

/// Wrap a record cursor at the ring end (+8) back to the ring start (+4).
fn wrap_cursor(m: &mut Machine, cursor: u32) -> u32 {
    let buffer = replay_buffer(m);
    let end = m.lw(buffer + 8);
    if (cursor as i32) < (end as i32) {
        cursor
    } else {
        m.lw(buffer + 4)
    }
}

/// 8007F448 (JP; PAL 8007DDCC, near, `del:7 imm:4 op:1`): start the replay
/// (phase 0x80): clear the status bytes, block pausing, queue the replay
/// music (the mission's stream when its entry is not negative, else
/// sequence 800B4334 + 12 * (context +0x38C mod 6 + 1); PAL +0x390, and
/// only in replay modes below 3),
/// reset slots, set the replay camera, save the entities, rewind, skip the
/// effects of every frame, leave bit 0x10000000 of gameplay and request
/// transition 0x100.
pub fn replay_start(m: &mut Machine) {
    // 8002937C (boot.rs): clear the six slot bytes at `[gp+0x30]`.
    call(m, 0x8002_937c, [0; 4]);
    let context = m.gw(CONTEXT);
    let flags = m.lw(context);
    let mode = m.lbu(context + 0x3a5);
    m.sw(context, flags | 0x1000_0000);
    if mode == 0 {
        call(m, 0x8002_c8d8, [REPLAY_MUSIC, 0, 0, 0]);
    } else {
        let stage = m.lh(context + 0x38c) as i32 as u32;
        let stream = call(m, 0x8005_6964, [stage, 0, 0, 0]);
        let mut sequence = m.lw((stream << 2).wrapping_add(STREAMS));
        let id = m.lh(sequence) as i32 as u32;
        if stream_entry_negative(m, id) == 0 {
            let context = m.gw(CONTEXT);
            let value = i32::from(m.lh(context + 0x38c));
            let high = ((i64::from(value) * 0x2aaa_aaab) >> 32) as i32;
            let quotient = high.wrapping_sub(value >> 31);
            let remainder = value.wrapping_sub(quotient.wrapping_mul(6));
            let index = i32::from(remainder as i16) + 1;
            sequence = (index.wrapping_mul(12) as u32).wrapping_add(REPLAY_MUSIC);
        }
        call(m, 0x8002_c8d8, [sequence, 0, 0, 0]);
    }
    call(m, 0x8006_c530, [0; 4]);
    camera_replay_view(m);
    save_entities(m);
    replay_rewind(m);
    replay_skip_frames(m, 0);
    let gameplay = m.gw(GAMEPLAY);
    let flags = m.lw(gameplay);
    let stage = m.lw(gameplay + 8);
    m.sw(gameplay, flags & 0xefff_ffff);
    request_transition(m, 0x100, stage);
}

/// Save the 64 entities into the replay buffer (+0x1850) with 8002759C.
fn save_entities(m: &mut Machine) {
    for i in 0..0x40u32 {
        let buffer = replay_buffer(m);
        let source = entity(m, i);
        copy_words(m, buffer + 0x1850 + i * 0x20, source, 8);
    }
}

const REPLAY_MUSIC: u32 = 0x800b_4334;

/// 800B2334: stream table; 800B4334: replay music sequences (12 bytes each).
const STREAMS: u32 = 0x800b_2334;

/// 80093D1C (JP; PAL 80092540, near, `imm:3`: spark life 0xC / 0x1E and
/// small size 0x640 for PAL 9 / 0x14 and 0x4B0): sparks at (`x`, `y`, `z`): on odd `phase` a spark of the pool
/// at effects +0x600 (3 masks, 7 words), then a random-chance ember and a
/// smoke puff of the pool at +0x1098 (10 masks, 7 words), all with random
/// spread, life and size.
pub fn sequence_sparks(m: &mut Machine, x: u32, y: u32, z: u32, phase: u32) {
    if phase & 1 != 0 {
        let pool = m.gw(0x41c).wrapping_add(0x600);
        let spark = pool_alloc(m, pool, 3, 7);
        if spark != 0 {
            let large = sdk::rand(m) & 1 != 0;
            m.sh(spark, 0xffff);
            let value = sdk::rand(m);
            let life = if large {
                (value & 7) + 0xc
            } else {
                (value & 7) + 0x1e
            };
            m.sh(spark + 2, life as u16);
            let spread = random_spread(m);
            m.sw(spark + 4, x.wrapping_add(spread));
            m.sw(spark + 8, y);
            let spread = random_spread(m);
            m.sw(spark + 0xc, z.wrapping_add(spread));
            m.sh(spark + 0x16, 0);
            m.sh(spark + 0x18, if large { 0x100 } else { 0x210 });
            let value = sdk::rand(m);
            let size = if large {
                (value & 0x1ff) + 0x3e8
            } else {
                (value & 0xff) + 0x640
            };
            m.sh(spark + 0x1a, size as u16);
        }
    }
    if sdk::rand(m) & 1 != 0 {
        let pool = m.gw(0x41c).wrapping_add(0x1098);
        let ember = pool_alloc(m, pool, 10, 7);
        if ember != 0 {
            m.sh(ember, 0);
            let value = sdk::rand(m);
            m.sh(ember + 2, ((value & 0xf) + 0x3c) as u16);
            let spread = random_spread(m);
            m.sw(ember + 4, x.wrapping_add(spread));
            m.sw(ember + 8, y.wrapping_add(0xffce_0000));
            let spread = random_spread(m);
            m.sw(ember + 0xc, z.wrapping_add(spread));
            m.sh(ember + 0x16, 0x98);
            let value = sdk::rand(m);
            m.sh(ember + 0x10, (value & 0xff).wrapping_sub(0x80) as u16);
            let value = sdk::rand(m);
            m.sh(ember + 0x14, (value & 0xff).wrapping_sub(0x80) as u16);
            m.sh(ember + 0x12, 0);
            m.sh(ember + 0x18, 0);
            let value = sdk::rand(m);
            m.sh(ember + 0x1a, ((value & 0xff) + 0xf0) as u16);
        }
    }
    let pool = m.gw(0x41c).wrapping_add(0x1098);
    let smoke = pool_alloc(m, pool, 10, 7);
    if smoke == 0 {
        return;
    }
    m.sh(smoke, 0x1e);
    let value = sdk::rand(m);
    m.sh(smoke + 2, ((value & 7) + 0x50) as u16);
    let spread = sra(random_spread(m), 1);
    m.sw(smoke + 4, x.wrapping_add(spread));
    m.sw(smoke + 8, y.wrapping_add(0xfff3_8000));
    let spread = sra(random_spread(m), 1);
    m.sw(smoke + 0xc, z.wrapping_add(spread));
    m.sh(smoke + 0x16, 0x98);
    let value = sdk::rand(m);
    m.sh(smoke + 0x10, (value & 0x7f).wrapping_sub(0x40) as u16);
    let value = sdk::rand(m);
    m.sh(smoke + 0x14, (value & 0x7f).wrapping_sub(0x40) as u16);
    m.sh(smoke + 0x12, 0);
    m.sh(smoke + 0x18, 0x1e04);
    let value = sdk::rand(m);
    m.sh(smoke + 0x1a, ((value & 0x3ff) + 0x7d0) as u16);
}

/// `(rand() - 0x4000) << 9`: a random offset of +-0x800000.
fn random_spread(m: &mut Machine) -> u32 {
    let value = sdk::rand(m);
    value.wrapping_sub(0x4000) << 9
}

fn sra(value: u32, shift: u32) -> u32 {
    ((value as i32) >> shift) as u32
}

/// 80093C90 (JP; PAL 800924B4, near, `imm:2`: the rectangle (-0xA0, -0x78)
/// 320x240 for PAL (-0xA0, -0x80) 320x256): red tint (far colour 0x30/0/0, RGB 0x6203135F, IR0 =
/// 0x1000 - `level`, DPCS) as a full-screen semi-transparent rectangle at
/// ordering-table entry `ot`; returns the next free packet (nothing when
/// `level` is 0).
pub fn tint_rectangle(m: &mut Machine, ot: u32, packets: u32, level: u32) -> u32 {
    if level == 0 {
        return packets;
    }
    m.gte.ctc2(ctrl::RFC, 0x30);
    m.gte.ctc2(ctrl::GFC, 0);
    m.gte.ctc2(ctrl::BFC, 0);
    m.gte.mtc2(reg::RGBC, 0x6203_135f);
    m.gte.mtc2(reg::IR0, 0x1000u32.wrapping_sub(level));
    let packet = packets;
    m.gte.dpcs(true, false);
    let head = m.lw(ot);
    m.sw(packet, head | 0x0300_0000);
    m.sw(ot, (packet << 8) >> 8);
    m.sw(packet + 8, 0xff88_ff60);
    m.sw(packet + 0xc, 0x00f0_0140);
    let rgb = m.gte.mfc2(reg::RGB2);
    m.sw(packet + 4, rgb);
    packet.wrapping_add(0x10)
}

/// 80093BC4 (JP; PAL 800923E8, near, `imm:1`: the rectangle 320x240 for
/// PAL 320x256): fade (`level` < 0x1000): a full-screen rectangle of strength
/// 0x1000 - `level` (negated when `additive` is 0) drawn by 80093B2C, then a
/// mask-bit packet (E6000001); returns the next free packet.
pub fn screen_fade(m: &mut Machine, ot: u32, packets: u32, additive: u32, level: u32) -> u32 {
    if (level as i32) >= 0x1000 {
        return packets;
    }
    let mut amount = 0x1000u32.wrapping_sub(level);
    m.sw(0x1f80_0000, 0);
    m.sw(0x1f80_0004, 0x00f0_0140);
    if additive == 0 {
        amount = amount.wrapping_neg();
    }
    let effects = m.gw(0x41c);
    m.sw(effects + 0x5c8, 1);
    let packet = call(m, 0x8009_3b2c, [ot, packets, 0x1f80_0000, amount]);
    m.sw(packet, 0x0100_0000);
    m.sw(packet + 4, 0xe600_0001);
    let head = m.lw(ot);
    m.sw(packet, 0x0100_0000 | (head & 0x00ff_ffff));
    let head = m.lw(ot);
    m.sw(ot, (head & 0xff00_0000) | (packet & 0x00ff_ffff));
    packet.wrapping_add(8)
}

/// 800573E8 (JP; PAL 80055AF0, near, `del:1 imm:7 ins:24 op:4 reg:3`):
/// phase 0x4000 (movie start). Movie entries 7, 0x10 and 0xC (context
/// +0x3A8; PAL: entry 2 at +0x3AC) go to the picture screen (transition
/// 0x20000). Otherwise the music flag, context bit 0x10000000, arena reset
/// to gameplay +0x2B8, the 0x44-byte player block `[gp+0x4D0]` with two
/// 0x400-byte buffers and a 320x240 frame (PAL 320x256), both VRAM halves
/// cleared (320x240), music 7, then transition 0x10000 (movie end) for
/// entries 0xC / 0x1B while the context halfword +0x3AC is set, or for
/// entries 0x1D / 0x1E while it is 0; else 0x8000.
pub fn movie_start(m: &mut Machine) {
    sdk::draw_sync(m, 0);
    let context = m.gw(CONTEXT);
    let entry = m.lbu(context + 0x3a8);
    if entry == 7 || entry == 0x10 || entry == 0xc {
        let gameplay = m.gw(GAMEPLAY);
        let stage = m.lw(gameplay + 8);
        request_transition(m, 0x2_0000, stage);
        return;
    }
    let table = m.gw(0x220);
    let flag = screen_music_flag(m, table);
    let screen = m.gw(0x218);
    m.sw(screen + 4, flag);
    let context = m.gw(CONTEXT);
    let status = m.gw(0x2d4);
    let flags = m.lw(context);
    let gameplay = m.gw(GAMEPLAY);
    m.sw(context, flags | 0x1000_0000);
    m.sw(status + 0x14, 0);
    let base = m.lw(gameplay + 0x2b8);
    crate::arena::reset(m, base);
    let block = crate::arena::alloc(m, 0x44);
    m.set_gw(0x4d0, block);
    let first = crate::arena::alloc(m, 0x400);
    let block = m.gw(0x4d0);
    m.sw(block + 4, first);
    let second = crate::arena::alloc(m, 0x400);
    let block = m.gw(0x4d0);
    let context = m.gw(CONTEXT);
    m.sw(block + 8, second);
    m.sw(block, 0);
    for (offset, value) in [
        (0x24, 0),
        (0x26, 0),
        (0x28, 0),
        (0x2a, 0x100),
        (0x2c, 0x140),
        (0x2e, 0xf0),
        (0x30, 0),
        (0x32, 0x20),
    ] {
        m.sh(block + offset, value);
    }
    let entry = u32::from(m.lbu(context + 0x3a8));
    let table = m.gw(0x220);
    let file = m.lh((entry << 3).wrapping_add(table)) as i32 as u32;
    m.sw(block + 0x38, 0);
    m.sw(block + 0x3c, 0);
    m.sw(block + 0x40, 0x78);
    m.sw(block + 0x34, file);
    clear_both_buffers(m);
    let screen = m.gw(0x218);
    if m.lw(screen + 4) != 0 {
        music_select(m, 7, 0, 0);
    }
    let context = m.gw(CONTEXT);
    let entry = m.lbu(context + 0x3a8);
    let end = if m.lh(context + 0x3ac) != 0 {
        entry == 0xc || entry == 0x1b
    } else {
        entry.wrapping_sub(0x1d) < 2
    };
    let next = if end { 0x1_0000 } else { 0x8000 };
    let gameplay = m.gw(GAMEPLAY);
    let stage = m.lw(gameplay + 8);
    request_transition(m, next, stage);
}

/// Music-on flag of an event screen: the context's music option (JP
/// +0x39C & +0x3A7; PAL +0x3A0 & +0x3AB) and the entry (context +0x3A8,
/// PAL +0x3AC) +2 halfword being 1.
fn screen_music_flag(m: &mut Machine, table: u32) -> u32 {
    let context = m.gw(CONTEXT);
    let enabled = m.lbu(context + 0x39c) & m.lbu(context + 0x3a7);
    if enabled == 0 {
        return 0;
    }
    let entry = u32::from(m.lbu(context + 0x3a8));
    u32::from(m.lh((entry << 3).wrapping_add(table) + 2) == 1)
}

/// Clear the two 320x240 halves of VRAM at y 0 and 0x100 (gp+0x4C4
/// rectangle, colour 1/1/1; PAL 320x256).
fn clear_both_buffers(m: &mut Machine) {
    let rect = GP + 0x4c4;
    m.sh(GP + 0x4c8, 0x140);
    m.sh(GP + 0x4c4, 0);
    m.sh(GP + 0x4c6, 0);
    m.sh(GP + 0x4ca, 0xf0);
    call(m, CLEAR_IMAGE, [rect, 1, 1, 1]);
    m.sh(GP + 0x4c4, 0);
    m.sh(GP + 0x4c6, 0x100);
    call(m, CLEAR_IMAGE, [rect, 1, 1, 1]);
}

/// 800A8318 / 800A84A0: libgpu ClearImage / StoreImage.
const CLEAR_IMAGE: u32 = 0x800a_8318;
const STORE_IMAGE: u32 = 0x800a_84a0;

/// 80057640 (JP; PAL 80055CEC, changed: the VRAM halves cleared are
/// 320x240, PAL 320x256): phase 0x10000 (movie end): context bit 0x10000000
/// cleared, status +0x14 = 2, music off, arena and resident reload
/// (800323F8) from gameplay +0x2B8, both VRAM halves cleared, next event
/// (80037E50).
pub fn movie_end(m: &mut Machine) {
    let context = m.gw(CONTEXT);
    let flags = m.lw(context);
    let status = m.gw(0x2d4);
    m.sw(context, flags & 0xefff_ffff);
    let screen = m.gw(0x218);
    m.sw(status + 0x14, 2);
    if m.lw(screen + 4) != 0 {
        music_select(m, 0xff, 0, 0);
    }
    let gameplay = m.gw(GAMEPLAY);
    let base = m.lw(gameplay + 0x2b8);
    crate::arena::reset(m, base);
    let gameplay = m.gw(GAMEPLAY);
    let base = m.lw(gameplay + 0x2b8);
    call(m, 0x8003_23f8, [base, 0, 0, 0]);
    clear_both_buffers(m);
    call(m, 0x8003_7e50, [0; 4]);
}

/// 8005AE8C (JP; PAL 800594E4, near, `imm:4 ins:24 op:1`): phase 0x20000
/// (picture start): context bit 0x10000000, status +0x14 = 0, arena reset,
/// status flags (+4 = 1, +0x10..+0x12 = 0, word 0 | 9), the 0x64-byte block
/// `[gp+0x4DC]` with two 0x2000-byte buffers and a 320x480 frame, VRAM saved
/// (rectangle 800BCDAC into 0x2800 bytes and 768,0 256x284 into 0x23800
/// bytes), the screen cleared, music 10 (entry at context +0x3A8, music
/// option +0x39C & +0x3A7). JP then ends at once for entries 0xC / 0x1B
/// while the context halfword +0x3AC is set (status +0x14 = 2, +4 = 0, word
/// 0 | 5, transition 0x80000); otherwise transition 0x40000.
pub fn picture_start(m: &mut Machine) {
    let frame = m.push_frame(0x30);
    let context = m.gw(CONTEXT);
    let status = m.gw(0x2d4);
    let flags = m.lw(context);
    let gameplay = m.gw(GAMEPLAY);
    m.sw(context, flags | 0x1000_0000);
    m.sw(status + 0x14, 0);
    let base = m.lw(gameplay + 0x2b8);
    crate::arena::reset(m, base);
    let status = m.gw(0x2d4);
    m.sb(status + 0x10, 0);
    m.sw(status + 4, 1);
    m.sb(status + 0x11, 0);
    m.sb(status + 0x12, 0);
    let flags = m.lw(status);
    m.sw(status, flags | 9);
    let block = crate::arena::alloc(m, 0x64);
    m.set_gw(0x4dc, block);
    let first = crate::arena::alloc(m, 0x2000);
    let block = m.gw(0x4dc);
    m.sw(block + 4, first);
    let second = crate::arena::alloc(m, 0x2000);
    let block = m.gw(0x4dc);
    let context = m.gw(CONTEXT);
    m.sw(block + 8, second);
    m.sh(block + 0x4e, 0x1e0);
    m.sw(block, 0);
    m.sh(block + 0x4c, 0x140);
    m.sw(block + 0x60, 0x78);
    let entry = u32::from(m.lbu(context + 0x3a8));
    let table = m.gw(0x220);
    let file = m.lh((entry << 3).wrapping_add(table)) as i32 as u32;
    m.sw(block + 0x54, 0);
    m.sw(block + 0x50, file);
    let rect = frame + 0x10;
    for i in 0..2 {
        let word = m.lw(0x800b_cdac + 4 * i);
        m.sw(rect + 4 * i, word);
    }
    let pixels = crate::arena::alloc(m, 0x2800);
    let block = m.gw(0x4dc);
    m.sw(block + 0x58, pixels);
    call(m, STORE_IMAGE, [rect, pixels, 0, 0]);
    m.sh(rect, 0x300);
    m.sh(rect + 4, 0x100);
    m.sh(rect + 2, 0);
    m.sh(rect + 6, 0x11c);
    let pixels = crate::arena::alloc(m, 0x2_3800);
    let block = m.gw(0x4dc);
    m.sw(block + 0x5c, pixels);
    call(m, STORE_IMAGE, [rect, pixels, 0, 0]);
    let clear = frame + 0x18;
    m.sh(clear, 0);
    m.sh(clear + 2, 0);
    m.sh(clear + 4, 0x140);
    m.sh(clear + 6, 0x1f0);
    call(m, CLEAR_IMAGE, [clear, 0, 0, 0]);
    sdk::draw_sync(m, 0);
    let table = m.lw(0x800b_cd94);
    let flag = screen_music_flag(m, table);
    let screen = m.gw(0x22c);
    m.sw(screen + 4, flag);
    let screen = m.gw(0x22c);
    if m.lw(screen + 4) != 0 {
        music_select(m, 0xa, 0, 0);
    }
    let context = m.gw(CONTEXT);
    let mut next = 0x4_0000;
    if m.lh(context + 0x3ac) != 0 {
        let entry = m.lbu(context + 0x3a8);
        if entry == 0xc || entry == 0x1b {
            let status = m.gw(0x2d4);
            m.sw(status + 0x14, 2);
            let flags = m.lw(status);
            m.sw(status + 4, 0);
            m.sw(status, flags | 5);
            next = 0x8_0000;
        }
    }
    let gameplay = m.gw(GAMEPLAY);
    let stage = m.lw(gameplay + 8);
    request_transition(m, next, stage);
    m.pop_frame(0x30);
}

/// 800395D4 (JP; PAL 8003926C, changed): end the sequence: event queue
/// reset, its final-state commands (object +4), music off, sounds stopped,
/// context bit 0x10000000 cleared and 80038D1C(`flags_only`): JP restores
/// only the entity flag words for sequence modes 2, 4, 0x14, 0x1E, 0x44 and
/// 0x55 (PAL always restores the whole snapshots).
pub fn sequence_end(m: &mut Machine) -> u32 {
    call(m, 0x8002_c6a8, [0; 4]);
    let object = sequence_object(m);
    m.set_gw(0x3ec, object.wrapping_add(4));
    emitters_clear(m);
    entities_clear_hidden(m);
    let record = m.gw(0x3ec);
    sequence_commands(m, record);
    music_select(m, 0xff, 0, 0);
    sound_stop_all(m);
    let context = m.gw(CONTEXT);
    let flags = m.lw(context);
    m.sw(context, flags & 0xefff_ffff);
    let mode = m.lw(context + SEQUENCE_MODE) as i32;
    let flags_only = u32::from(matches!(mode, 2 | 4 | 0x14 | 0x1e | 0x44 | 0x55));
    sequence_restore(m, flags_only)
}

/// The sequence object of the current mode: word 0 is its frame count, the
/// commands of its final state follow at +4.
fn sequence_object(m: &mut Machine) -> u32 {
    let context = m.gw(CONTEXT);
    let mode = m.lw(context + SEQUENCE_MODE);
    let gameplay = m.gw(GAMEPLAY);
    let slot = u32::from(m.lbu(mode.wrapping_add(SEQUENCE_OBJECTS)));
    let table = m.lw(gameplay + 0x26c);
    m.lw((slot << 2).wrapping_add(table))
}

/// 800B0FF4: object slot of each sequence mode (byte per mode).
const SEQUENCE_OBJECTS: u32 = 0x800b_0ff4;

/// Context +0x1038 (PAL +0xF7C): the sequence mode (word).
const SEQUENCE_MODE: u32 = 0x1038;

/// 80039430 (JP; PAL 80039138, near, `call~:1 imm:2 ins:28 op:1`; the mode
/// at context +0x1038): start the sequence of the current mode: frame count from its
/// object, camera on the player, gameplay bit 0 and context bit 0x10000000,
/// event queue reset, a 0x48C0-byte stream buffer for its stream file
/// (8002C928 with callback 80039420), sound and effect resets, music 1 and
/// transition 0x20 (fade state gp+0x3F0 = 0), or, JP, for the modes of the
/// jump table 80023A64 while context +0x3AC is set, transition 0x40 with
/// fade state 5 (returns 5).
pub fn sequence_start(m: &mut Machine) -> u32 {
    let frame = m.push_frame(0x20);
    m.set_gw(0x3ec, 0x8020_1000);
    let object = sequence_object(m);
    let frames = m.lw(object).wrapping_sub(1);
    m.set_gw(0x3f4, frames);
    let left = m.lhu(GP + 0x3f4);
    m.set_gw(0x3fc, 0);
    m.sh(GP + 0x400, left);
    camera_follow_player(m);
    let gameplay = m.gw(GAMEPLAY);
    let flags = m.lw(gameplay);
    m.sw(gameplay, flags | 1);
    let context = m.gw(CONTEXT);
    let flags = m.lw(context);
    m.sw(context, flags | 0x1000_0000);
    call(m, 0x8002_c6a8, [0; 4]);
    let buffer = crate::arena::alloc(m, 0x48c0);
    let context = m.gw(CONTEXT);
    let mode = m.lw(context + SEQUENCE_MODE);
    m.set_gw(0x404, 0);
    let file = m.lh((mode << 1).wrapping_add(SEQUENCE_STREAMS)) as i32 as u32;
    m.sw(frame + 0x10, 0x8003_9420);
    stream_queue(m, file, buffer, 0x48c0, 0x3000, 0x8003_9420);
    sound_listener_reset(m);
    call(m, 0x8008_49ec, [0; 4]);
    call(m, 0x8007_da1c, [0; 4]);
    call(m, 0x8006_c530, [0; 4]);
    music_select(m, 1, 0, 0);
    let context = m.gw(CONTEXT);
    let pending = m.lh(context + 0x3ac);
    m.set_gw(0x3f8, 0);
    // The jump table at 80023A64 (modes 5..0x5D): these modes skip the
    // sequence (transition 0x40, fade state 5 = done) while the context
    // halfword +0x3AC is set.
    let skip = pending != 0
        && matches!(
            m.lw(context + SEQUENCE_MODE),
            0x5 | 0xf
                | 0x13
                | 0x15
                | 0x25
                | 0x27
                | 0x28
                | 0x2e
                | 0x36
                | 0x3c
                | 0x48
                | 0x4b
                | 0x4c
                | 0x54
                | 0x58
                | 0x59
                | 0x5a
                | 0x5c
                | 0x5d
        );
    let gameplay = m.gw(GAMEPLAY);
    let stage = m.lw(gameplay + 8);
    let v0 = if skip {
        request_transition(m, 0x40, stage);
        m.set_gw(0x3f0, 5);
        5
    } else {
        let v0 = request_transition_v0(m, 0x20, stage);
        m.set_gw(0x3f0, 0);
        v0
    };
    m.pop_frame(0x20);
    v0
}

/// [`request_transition`] and its return value (gameplay word 0).
fn request_transition_v0(m: &mut Machine, first: u32, second: u32) -> u32 {
    request_transition(m, first, second);
    let gameplay = m.gw(GAMEPLAY);
    m.lw(gameplay)
}

/// 800B0F34: stream file of each sequence mode (halfword per mode).
const SEQUENCE_STREAMS: u32 = 0x800b_0f34;

/// 800399D4 (JP; PAL 80039598, changed): sequence fade, letterbox and title
/// overlays. gp+0x3F0 is the fade state (1/3 fading in, 2 shown, 4 fading
/// out, 5 done), gp+0x9C the fade level, gp+0xA0 the letterbox level and
/// gp+0xA4 the overlay frame counter; `elapsed` = gp+0x3F4 - gp+0x400 + 2.
/// Pad bit 0x40 skips (state 1 -> 3, 2 -> 4); fading out ends with
/// transition 0x40. The fade (80093BC4) is additive in states 4/5 for the
/// sequence modes (context +0x1038) of the jump table 80023BC8 and in
/// states 0/1/3 for mode 0x2A; then the letterbox (800939D4, at the
/// letterbox level less 0x800). JP's jump table 80023D18 then picks the
/// overlays by mode:
/// 80084474 (bit 0, at ordering-table entry +8) and 8008FC9C (bit 1) at a
/// position from gp+0xA4 over the mode's frame window, each advancing
/// gp+0xA4 (80084474 also sets bit 0 of the effects word +0x5C8). The
/// original's 0x20-byte frame is kept for its callees.
pub fn sequence_fade(m: &mut Machine) {
    const FRAME: u32 = 0x20;
    m.push_frame(FRAME);
    fade_and_overlays(m);
    m.pop_frame(FRAME);
}

fn fade_and_overlays(m: &mut Machine) {
    let left = u32::from(m.lhu(GP + 0x400));
    let frames = m.gw(0x3f4);
    let context = m.gw(CONTEXT);
    let elapsed = frames.wrapping_sub(left).wrapping_add(2) as i32;
    if m.lw(context + 0x12c) & 0x40 != 0 {
        let state = m.gw(0x3f0);
        m.set_gw(0x3fc, 1);
        if state == 1 {
            m.set_gw(0x3f0, 3);
        } else if state == 2 {
            fade_out(m);
        }
    }
    let state = m.gw(0x3f0);
    if state.wrapping_sub(1) < 3 {
        if (m.gw(0x3f4) as i32) <= 0 {
            m.set_gw(0x9c, 0);
            m.set_gw(0xa0, 0);
        } else {
            let step = elapsed.wrapping_sub(1) as u32;
            m.set_gw(0x9c, step << 7);
            m.set_gw(0xa0, step << 8);
        }
        if (m.gw(0x9c) as i32) < 0 {
            m.set_gw(0x9c, 0);
        }
        if (m.gw(0xa0) as i32) < 0 {
            m.set_gw(0xa0, 0);
        }
        if (m.gw(0x9c) as i32) >= 0x1800 {
            let state = m.gw(0x3f0);
            if state == 1 {
                m.set_gw(0x3f0, 2);
            } else if state == 3 {
                fade_out(m);
            }
        }
        if m.lhu(GP + 0x400) < 0x31 {
            fade_out(m);
        }
    } else if state == 4 {
        let level = m.gw(0x9c).wrapping_sub(0x80);
        m.set_gw(0x9c, level);
        if (level as i32) <= 0 {
            let gameplay = m.gw(GAMEPLAY);
            m.set_gw(0x9c, 0);
            let stage = m.lw(gameplay + 8);
            m.set_gw(0x3f0, 5);
            request_transition(m, 0x40, stage);
        }
        if m.gw(0x3fc) == 0 && m.lhu(GP + 0x400) < 0x19 {
            let level = m.gw(0xa0).wrapping_sub(0x100);
            m.set_gw(0xa0, level);
        }
        if (m.gw(0xa0) as i32) <= 0 {
            m.set_gw(0xa0, 0);
        }
    } else if state == 0 || state == 5 {
        m.set_gw(0x9c, 0);
        m.set_gw(0xa0, 0);
        m.set_gw(0xa4, 0);
    }
    let state = m.gw(0x3f0);
    let additive = if state.wrapping_sub(4) < 2 {
        let context = m.gw(CONTEXT);
        let mode = m.lw(context + SEQUENCE_MODE);
        u32::from(matches!(
            mode,
            9 | 0xc | 0x1a | 0x1c | 0x30 | 0x3a | 0x3d | 0x3e | 0x40 | 0x42 | 0x43 | 0x5b
        ))
    } else if state == 1 || state == 3 || state == 0 {
        let context = m.gw(CONTEXT);
        u32::from(m.lw(context + SEQUENCE_MODE) == 0x2a)
    } else {
        0
    };
    let context = m.gw(CONTEXT);
    let gameplay = m.gw(GAMEPLAY);
    let level = m.gw(0x9c);
    let ot = m.lw(context + 0xc);
    let packets = m.lw(gameplay + 0x14);
    let packets = screen_fade(m, ot, packets, additive, level);
    let gameplay = m.gw(GAMEPLAY);
    let letterbox_level = m.gw(0xa0).wrapping_sub(0x800);
    let ot = m.lw(gameplay + 0x10);
    m.sw(gameplay + 0x14, packets);
    let packets = call(m, 0x8009_39d4, [ot, packets, letterbox_level, 0]);
    let gameplay = m.gw(GAMEPLAY);
    let context = m.gw(CONTEXT);
    m.sw(gameplay + 0x14, packets);
    let mode = m.lw(context + SEQUENCE_MODE);
    // (overlay bits, position): bit 0 80084474, bit 1 8008FC9C.
    let (mut overlays, mut position) = (0u32, 0u32);
    match mode {
        0x27 | 0x34 => {
            if let Some(frames) = overlay_window(m, elapsed, 0x97) {
                overlays |= 1;
                position = overlay_share(m.gw(0xa4) << 12, frames.wrapping_sub(0x96));
            }
        }
        0x59 => {
            if let Some(frames) = overlay_window(m, elapsed, 0x23b) {
                overlays |= 1;
                position = overlay_share(m.gw(0xa4) << 12, frames.wrapping_sub(0x23a));
            }
        }
        0x28 | 0x5a => {
            if (elapsed.wrapping_sub(1) as u32) < 0x5a {
                overlays |= 1;
                position = 0x1000u32.wrapping_sub(div_90(m.gw(0xa4) << 12));
            }
        }
        0x35 => {
            if (elapsed.wrapping_sub(1) as u32) < 0x5a {
                overlays = 1;
                position |= 0x1000u32.wrapping_sub(div_90(m.gw(0xa4) << 12));
            }
        }
        0x3a => {
            if let Some(frames) = overlay_window(m, elapsed, 0x1ff) {
                overlays |= 2;
                position = overlay_share(m.gw(0xa4) << 12, frames.wrapping_sub(0x1fe));
            }
        }
        0x5b => {
            if let Some(frames) = overlay_window(m, elapsed, 0x14b) {
                position = overlay_share(m.gw(0xa4) << 11, frames.wrapping_sub(0x14a));
                overlays |= 2;
            }
        }
        0x2a => {
            if (elapsed.wrapping_sub(1) as u32) < 0x70 {
                let scaled = m.gw(0xa4).wrapping_mul(5) << 9;
                // (scaled * 0x8FB823EF) >> 32, + scaled, >> 6, minus the sign.
                let high = (i64::from(scaled as i32) * i64::from(0x8fb8_23efu32 as i32)) >> 32;
                set_effects_bit(m);
                overlays |= 2;
                let quotient = ((high as u32).wrapping_add(scaled) as i32 >> 6)
                    .wrapping_sub(scaled as i32 >> 31);
                position = 0xa00u32.wrapping_sub(quotient as u32);
            }
            if (elapsed.wrapping_sub(0x71) as u32) < 0x10 {
                set_effects_bit(m);
                overlays |= 1;
                position = 0x400;
            }
        }
        _ => {}
    }
    if overlays & 1 != 0 {
        let gameplay = m.gw(GAMEPLAY);
        let ot = m.lw(gameplay + 0x10);
        let packets = m.lw(gameplay + 0x14);
        let next = call(m, 0x8008_4474, [ot.wrapping_add(8), packets, position, 0]);
        let gameplay = m.gw(GAMEPLAY);
        let effects = m.gw(0x41c);
        m.sw(gameplay + 0x14, next);
        let shown = m.gw(0xa4).wrapping_add(1);
        let bits = m.lw(effects + 0x5c8);
        m.set_gw(0xa4, shown);
        m.sw(effects + 0x5c8, bits | 1);
    }
    if overlays & 2 != 0 {
        let gameplay = m.gw(GAMEPLAY);
        let ot = m.lw(gameplay + 0x10);
        let packets = m.lw(gameplay + 0x14);
        let next = call(m, 0x8008_fc9c, [ot, packets, position, 0]);
        let shown = m.gw(0xa4).wrapping_add(1);
        let gameplay = m.gw(GAMEPLAY);
        m.sw(gameplay + 0x14, next);
        m.set_gw(0xa4, shown);
    }
}

/// Fade state 4 (fading out) from full level.
fn fade_out(m: &mut Machine) {
    m.set_gw(0x3f0, 4);
    m.set_gw(0x9c, 0x1680);
    m.set_gw(0xa0, 0x1800);
}

/// An overlay's frame window of 800399D4: from `first` frames on, while
/// `elapsed` is within the sequence's frames (gp+0x3F4, signed); returns
/// those frames.
fn overlay_window(m: &mut Machine, elapsed: i32, first: i32) -> Option<i32> {
    if elapsed < first {
        return None;
    }
    let frames = m.gw(0x3f4) as i32;
    (frames >= elapsed).then_some(frames)
}

/// The overlay position `shown / span` (MIPS `div`). Inside its window the
/// span is at least 1, so the original's `break` checks never fire.
fn overlay_share(shown: u32, span: i32) -> u32 {
    (shown as i32).wrapping_div(span) as u32
}

/// `value / 90` as 800399D4 computes it: the reciprocal 0xB60B60B7, HI +
/// value, >> 6, minus the value's sign.
fn div_90(value: u32) -> u32 {
    let high = (i64::from(value as i32) * i64::from(0xb60b_60b7u32 as i32)) >> 32;
    ((high as u32).wrapping_add(value) as i32 >> 6).wrapping_sub(value as i32 >> 31) as u32
}

/// Bit 0 of the effects word `[gp+0x41C]` +0x5C8.
fn set_effects_bit(m: &mut Machine) {
    let effects = m.gw(0x41c);
    let bits = m.lw(effects + 0x5c8);
    m.sw(effects + 0x5c8, bits | 1);
}

/// 8005BE0C (JP; PAL 8005A410, exact but for the message routine 8004CEB0,
/// REVIEW §3b): phase 0x2000 (course end) by result gameplay +8: 2 (cleared)
/// marks the lesson at its end frame and leaves after the lesson's window
/// (lesson 0x8000 always waits for pad 0x200); 0x20 (failed) leaves from
/// frame 0x78; another nonzero result restarts the course (transition
/// 0x1000) from frame 0x78; 0 leaves from frame 0x96. Pad 0x7000 skips.
/// The lesson's result message (8004CEB0, PAL 8004B760) shows on frame 0.
pub fn course_end(m: &mut Machine) {
    let mut pressed = call(m, 0x8006_cb30, [0; 4]);
    let gameplay = m.gw(GAMEPLAY);
    let result = m.lw(gameplay + 8);
    let mut message = 0xff;
    if result & 2 != 0 {
        if pressed & 0x7000 != 0 {
            course_skip(m);
            return;
        }
        let flags = m.gw(0x4e4);
        // (message, window end, lesson end frame); flags outside the course
        // set leave the original's s0/s1 to its caller: no window here.
        let lesson: Option<(u32, u32, u32)> = if flags & 0x300 != 0 {
            if m.lw(gameplay + 0xc) == 0 {
                sound_channel_volume(m, 0, 0, 0x96);
                sound_channel_volume(m, 1, 0x1e, 0x96);
                sound_channel_volume(m, 2, 0x14, 0x96);
            }
            Some((0xc, 0x10e, 0x10e))
        } else if flags & 0x400 != 0 {
            Some((0x14, 0x168, 0x168))
        } else if flags & 0x800 != 0 {
            Some((0x2b, 0x168, 0x168))
        } else if flags & 0x6000 != 0 {
            Some((0x31, 0x78, u32::MAX))
        } else if flags & 0x1000 != 0 {
            Some((0x2f, 0xb4, 0xb4))
        } else if flags & 0x8000 != 0 {
            pressed = 0x200;
            Some((0xff, 0x5a, 0x5a))
        } else {
            None
        };
        if let Some((text, window, end)) = lesson {
            message = text;
            let gameplay = m.gw(GAMEPLAY);
            if m.lw(gameplay + 0xc) == end {
                course_cleared(m);
            }
            let gameplay = m.gw(GAMEPLAY);
            if pressed & 0x200 != 0 && (window as i32) < (m.lw(gameplay + 0xc) as i32) {
                course_leave(m);
            }
        }
    } else if result & 0x20 != 0 {
        if pressed & 0x7000 != 0 {
            course_skip(m);
        }
        let gameplay = m.gw(GAMEPLAY);
        if m.lw(gameplay + 0xc) == 0x78 {
            course_cleared(m);
        }
        let gameplay = m.gw(GAMEPLAY);
        if (m.lw(gameplay + 0xc) as i32) >= 0x78 {
            course_leave(m);
        }
    } else if result != 0 {
        let flags = m.gw(0x4e4);
        if flags & 0x300 != 0 {
            message = 0x29;
        } else if flags & 0x400 != 0 {
            message = 0x2a;
        } else if flags & 0x800 != 0 {
            message = 0x2c;
        } else if flags & 0x1000 != 0 {
            message = 0x30;
        }
        let gameplay = m.gw(GAMEPLAY);
        if m.lw(gameplay + 0xc) == 0 {
            call(m, 0x8005_6518, [0; 4]);
        }
        let gameplay = m.gw(GAMEPLAY);
        if (m.lw(gameplay + 0xc) as i32) >= 0x78 {
            course_targets_reset(m);
            call(m, 0x8003_048c, [0; 4]);
            call(m, 0x8005_6518, [0; 4]);
            let progress = m.gw(0x340);
            let next = m.lbu(progress + 0x28);
            m.sb(progress + 0x25, next);
            let progress = m.gw(0x340);
            let next = u32::from(m.lbu(progress + 0x28));
            request_transition(m, 0x1000, next);
            call(m, 0x8007_9924, [0; 4]);
        }
    } else if (m.lw(gameplay + 0xc) as i32) >= 0x96 && call(m, 0x8005_650c, [0; 4]) == 0 {
        course_targets_reset(m);
        leave_to_next_event(m);
    }
    let gameplay = m.gw(GAMEPLAY);
    if m.lw(gameplay + 0xc) == 0 && message != 0xff {
        call(m, SHOW_MESSAGE, [message, 0, 0, 0]);
    }
}

fn leave_to_next_event(m: &mut Machine) {
    let progress = m.gw(0x340);
    if m.lbu(progress + 0x29) == 0 {
        music_none();
    } else {
        call(m, 0x8003_048c, [0; 4]);
    }
    call(m, 0x8003_7e50, [0; 4]);
}

/// Leave the course once no radio message plays and the lesson is not
/// marked: reset its targets, music (800304EC / 8003048C by the next event
/// type) and choose the next event.
fn course_leave(m: &mut Machine) {
    if call(m, 0x8005_650c, [0; 4]) != 0 {
        return;
    }
    let progress = m.gw(0x340);
    if m.lbu(progress + 0x30) != 0 {
        return;
    }
    course_targets_reset(m);
    leave_to_next_event(m);
}

/// Lesson cleared: progress +0x31 = 2, +0x30 = 1, +0x32 = 0x1E, +0x1A = 0.
fn course_cleared(m: &mut Machine) {
    let progress = m.gw(0x340);
    m.sb(progress + 0x31, 2);
    let progress = m.gw(0x340);
    m.sb(progress + 0x30, 1);
    let progress = m.gw(0x340);
    m.sh(progress + 0x32, 0x1e);
    m.sh(progress + 0x1a, 0);
}

/// Course end request: radio off, progress +0x25 = +0x28 and gameplay +8 =
/// +0x28, transition 0x2000 (1), context bit 0x10000000.
fn course_skip(m: &mut Machine) {
    call(m, 0x8005_6518, [0; 4]);
    let progress = m.gw(0x340);
    let next = m.lbu(progress + 0x28);
    m.sb(progress + 0x25, next);
    let progress = m.gw(0x340);
    let gameplay = m.gw(GAMEPLAY);
    let next = u32::from(m.lbu(progress + 0x28));
    m.sw(gameplay + 8, next);
    request_transition(m, 0x2000, 1);
    let context = m.gw(CONTEXT);
    let flags = m.lw(context);
    m.sw(context, flags | 0x1000_0000);
}

/// 8005BC64 (JP; PAL 8005A268, exact but for the message routine 8004CEB0,
/// REVIEW §3b): phase 0x400 (course intro): a fade-in for lessons 0x6000 with
/// progress +0x3F = 1; after 0x3D frames and no radio message, the course
/// camera (800762D0 for 0x8000, else 80076F04), the lesson's first radio
/// message (8004CEB0, PAL 8004B760) and transition 0x800. Any of pad 0x7000 skips the
/// course (transition 0x2000).
pub fn course_start(m: &mut Machine) {
    let progress = m.gw(0x340);
    let lesson_flag = m.lbu(progress + 0x3f);
    let flags = m.gw(0x4e4);
    if lesson_flag == 1 && flags & 0x6000 != 0 {
        let gameplay = m.gw(GAMEPLAY);
        let frame = m.lw(gameplay + 0xc);
        if (frame as i32) < 0x10 {
            let level = 0x1000u32.wrapping_sub(frame << 8);
            let ot = m.lw(gameplay + 0x10).wrapping_add(8);
            let packets = m.lw(gameplay + 0x14);
            let next = call(m, 0x8008_4560, [ot, packets, level, 0]);
            let gameplay = m.gw(GAMEPLAY);
            m.sw(gameplay + 0x14, next);
        }
    }
    let gameplay = m.gw(GAMEPLAY);
    if (m.lw(gameplay + 0xc) as i32) >= 0x3d && call(m, 0x8005_650c, [0; 4]) == 0 {
        if flags & 0x8000 != 0 {
            call(m, 0x8007_62d0, [0; 4]);
        } else {
            camera_view_apply(m);
        }
        // Flags outside the course set leave the original's message register
        // to its caller; the native port uses message 0 then.
        let message = [
            (0x300, 0xb),
            (0x400, 0x13),
            (0x800, 0x1d),
            (0x2000, 0xf),
            (0x4000, 0x1b),
            (0x1000, 0x27),
            (0x8000, 0x2d),
        ]
        .into_iter()
        .find(|&(bits, _)| flags & bits != 0)
        .map_or(0, |(_, message)| message);
        call(m, SHOW_MESSAGE, [message, 0, 0, 0]);
        let gameplay = m.gw(GAMEPLAY);
        let stage = m.lw(gameplay + 8);
        request_transition(m, 0x800, stage);
    }
    let pressed = call(m, 0x8006_cb30, [0; 4]);
    if pressed & 0x7000 != 0 {
        call(m, 0x8005_6518, [0; 4]);
        let progress = m.gw(0x340);
        let next = m.lbu(progress + 0x28);
        m.sb(progress + 0x25, next);
        request_transition(m, 0x2000, 1);
        let context = m.gw(CONTEXT);
        let flags = m.lw(context);
        m.sw(context, flags | 0x1000_0000);
    }
}

/// 8004CEB0 (JP-only, resident part 3): the on-screen message that
/// replaces PAL 8004B760 (message id).
const SHOW_MESSAGE: u32 = 0x8004_ceb0;

/// 80039230 (JP; exact, REVIEW §1 R0328: the jump table is at JP 800239E4,
/// which no data map pairs with PAL 8002398C): command 5, effect: sub-command (header >> 8) & 0x1F through the
/// jump table 800239E4 to the 8009xxxx effect routines.
pub fn command_effect(m: &mut Machine, record: u32) -> u32 {
    let header = m.lw(record);
    let case = (header >> 8) & 0x1f;
    let target = m.lw((case << 2).wrapping_add(EFFECT_CASES));
    let half = m.lh(record + 8) as i32 as u32;
    match target {
        0x8003_9278 => {
            call(m, 0x8009_351c, [record, case, 0, 0]);
        }
        0x8003_9288 => {
            call(m, 0x8009_3484, [record, case.wrapping_sub(8), 0, 0]);
        }
        0x8003_929c => {
            call(m, 0x8009_35ac, [record, half, 0, 0]);
        }
        0x8003_92b4 => {
            call(m, 0x8009_35ac, [record, half, 1, 0]);
        }
        0x8003_92cc => {
            call(m, 0x8009_35ac, [record, half, 2, 0]);
        }
        0x8003_92e4 => {
            call(m, 0x8009_2b80, [record, case, 0, 0]);
        }
        0x8003_92f4 => {
            call(m, 0x8009_2d90, [record, case, 0, 0]);
        }
        0x8003_9304 => {
            call(m, 0x8009_3040, [record, case, 0, 0]);
        }
        0x8003_9314 => {
            call(m, 0x8009_319c, [record, case, 0, 0]);
        }
        0x8003_9324 => {
            call(m, 0x8009_3324, [record, 0, 0, 0]);
        }
        0x8003_9338 => {
            call(m, 0x8009_3324, [record, 1, 0, 0]);
        }
        0x8003_934c => {
            call(m, 0x8009_3324, [record, 2, 0, 0]);
        }
        0x8003_9360 => {
            call(m, 0x8009_37d8, [record, 4, 0, 0]);
        }
        0x8003_936c => {}
        _ => m.raise(Fault::Untranslated { address: target }),
    }
    record.wrapping_add(0x18)
}

/// 800239E4 (PAL 8002398C): jump table of command 5 (80039230), by
/// (header >> 8) & 0x1F (JP 80039230: `lui 0x8002; addiu 0x39E4`).
const EFFECT_CASES: u32 = 0x8002_39e4;
