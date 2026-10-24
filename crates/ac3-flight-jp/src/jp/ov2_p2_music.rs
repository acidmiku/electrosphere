//! JP overlay 2, part 2: the hub's music page (track choice, spectrum
//! display, play time) and its neighbours, translated from the JP
//! instructions.
//!
//! The music page block (`[0x80112DD8]`, 0x23BC bytes from the arena)
//! holds per display buffer 108 + 108 sprites (0x14 bytes, the spectrum
//! columns at +0 and +0x10E0 + 0x870 per buffer), draw modes at +0x21C0 /
//! +0x21D8 / +0x2380 (+ 0xC per buffer), five dot sprites at +0x21F0 +
//! 0x64 per buffer, the dots' orbit (+0x2398/+0x23A2 radii, +0x23AC speeds),
//! +0x23B6 the phase, +0x23B8 the cursor (0..7) and +0x23B9 the play mode.
//! The music driver's record is `[0x80112E0C]` (+0 flags, +8 the track).
use super::ov2_p2::{MODULE, framed, lbu, less, lh, lhu, mul, quotient, sext16, visual, work};
use super::ov2_p2_hub::STATE;
use crate::common::{CONTEXT, RESOURCES, loaded_module};
use crate::machine::{Machine, SmallData};
use crate::{arena, dispatch, sdk};

/// Pointer to the music page block.
pub const PAGE: u32 = 0x8011_2dd8;
/// Pointer to the music driver's record.
pub const PLAYER: u32 = 0x8011_2e0c;
/// Track names (pointers by track).
pub const TRACK_NAMES: u32 = 0x8010_a568;

/// 8002DB4C (resident, `script`): start sound effect `a0`; returns the
/// channel.
const SOUND: u32 = 0x8002_db4c;
/// 8002DDA8 (resident, `shared`): stop sound channel `a0`.
const STOP: u32 = 0x8002_dda8;
/// 8002C6A8 (resident, `shared`; a0, a1), 8002C580 (resident, `boot`;
/// a0), 8002B588 (resident, `cdstream`: status word), 8002B59C (resident,
/// `states`: play time).
const MUSIC_STOP: u32 = 0x8002_c6a8;
const MUSIC_POLL: u32 = 0x8002_c580;
const STREAM_STATUS: u32 = 0x8002_b588;
const PLAY_TIME: u32 = 0x8002_b59c;
/// 80027338 (resident, `loading`), 80027538 (resident, `arena`).
const RESOURCE: u32 = 0x8002_7338;
const ARENA_RESET: u32 = 0x8002_7538;
/// Overlay 2, part 4: release image `a0`; text routines.
const RELEASE: u32 = 0x800e_cda8;
const TEXT_AT: u32 = 0x800f_33a8;
const TEXT_COLOR: u32 = 0x800f_33d0;
const TEXT_ALIGN: u32 = 0x800f_33f8;
const TEXT_FONT: u32 = 0x800f_35b8;
const PRINT: u32 = 0x800f_4ae0;
const PRINT3: u32 = 0x800f_4b40;
/// Overlay 2, part 5: help text (a0), page title (a0, a1), spectrum of
/// channel `a0` (halfword samples), output-mode value.
const HELP: u32 = 0x800f_7514;
const TITLE: u32 = 0x800f_7524;
const SPECTRUM: u32 = 0x800f_770c;
const MODE_VALUE: u32 = 0x800f_77a4;
const FADE_DONE: u32 = 0x800f_f2bc;

fn state(m: &mut Machine) -> u32 {
    m.lw(STATE)
}
fn page(m: &mut Machine) -> u32 {
    m.lw(PAGE)
}
fn player(m: &mut Machine) -> u32 {
    m.lw(PLAYER)
}
fn call(m: &mut Machine, routine: u32, args: [u32; 4]) -> u32 {
    dispatch::call(m, routine, args)
}
fn sound(m: &mut Machine, id: u32) -> u32 {
    call(m, SOUND, [id, 0, 0, 0])
}
fn press(m: &mut Machine, bit: u32, held: u32) -> bool {
    let context = m.gw(CONTEXT);
    m.lw(context.wrapping_add(0x12c)) & bit != 0 && m.lw(context.wrapping_add(0x128)) & held == 0
}

/// `n / d` as compiled for a constant (magic multiply, shift, sign).
fn divide(n: u32, magic: u32, shift: u32) -> u32 {
    let high = ((i64::from(n as i32) * i64::from(magic as i32)) >> 32) as u32;
    ((high.wrapping_add(n) as i32 >> shift) as u32).wrapping_sub((n as i32 >> 31) as u32)
}

/// 800DCC80: the fade color of [gp+0x2D4] (+0x10..+0x12) from fade slot
/// 1's level (143/29, 135/29 and 83/29 of it); once slot 1 has finished:
/// cell preset 1, mirror cell 0xF of list 0, 800FF2BC, step and fade slot
/// 1 in to 0x19. Returns the fade start's value, or the new flags of
/// [gp+0x2D4].
pub fn fade_color(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let level = super::ov2_p2::fade_level(m, 1);
        if super::ov2_p2::fade_mode(m, 1) == 2 {
            super::ov2_p2::set_cell_preset(m, 0, 1);
            super::ov2_p2::mirror_cell(m, 0, 0xf);
            call(m, FADE_DONE, [0; 4]);
            let record = state(m);
            let step = lbu(m, record.wrapping_add(1)).wrapping_add(1);
            m.sb(record.wrapping_add(1), step as u8);
            return super::ov2_p2::set_fade(m, 1, 1, 0, 0x19);
        }
        let red = divide(mul(level, 143), 0x8d3d_cb09, 4);
        let green = divide(mul(level, 135), 0x8d3d_cb09, 4);
        let blue = divide(mul(level, 83), 0x8d3d_cb09, 4);
        for (offset, value) in [(0x10, red), (0x11, green), (0x12, blue)] {
            let fade = m.gw(0x2d4);
            m.sb(fade.wrapping_add(offset), value as u8);
        }
        let fade = m.gw(0x2d4);
        let flags = m.lw(fade) | 8;
        m.sw(fade, flags);
        flags
    })
}

/// 800DCDC4: once fade slot 1 has finished, return to page 0. Returns the
/// fade mode, or the state record once finished.
pub fn wait_fade(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let mode = super::ov2_p2::fade_mode(m, 1);
        if mode != 2 {
            return mode;
        }
        let record = state(m);
        m.sb(record, 0);
        let record = state(m);
        m.sb(record.wrapping_add(1), 0);
        record
    })
}

/// 800DCE0C: open the music page: allocate and fill its block (cursor 2,
/// phase and mode 0; the spectrum sprites and draw modes, the dot sprites
/// and their orbits), help text 0x8010A5EC, cells and slots, release
/// image [visual +0xA8], stop the five channels, step the state. Returns
/// the new step.
pub fn open_music(m: &mut Machine) -> u32 {
    framed(m, 0x40, |m, sp| {
        let block = arena::alloc(m, 0x23bc);
        m.sw(PAGE, block);
        m.sb(block.wrapping_add(0x23b8), 2);
        let current = page(m);
        m.sh(block.wrapping_add(0x23b6), 0);
        m.sb(current.wrapping_add(0x23b9), 0);
        call(m, HELP, [0x8010_a5ec, 0, 0, 0]);
        for buffer in 0..2u32 {
            let block = page(m);
            let mut columns = block.wrapping_add(0x870 * buffer);
            let mut caps = block.wrapping_add(0x10e0 + 0x870 * buffer);
            for column in 0..0x12u32 {
                let x = 0x9c + 8 * column;
                for row in 0..6u32 {
                    let y = 0x15e - 0x10 * row;
                    let low = row < 3;
                    for (sprite, (u, v, clut)) in [
                        (
                            columns,
                            if low {
                                (0xdc, 0x24, 0xedb)
                            } else {
                                (0xe4, 0x14, 0xf1b)
                            },
                        ),
                        (caps, (0xec, 0x18, 0xe9b)),
                    ] {
                        let first = sprite == caps;
                        if first {
                            m.sb(sprite.wrapping_add(0xc), u);
                            m.sb(sprite.wrapping_add(0xd), v);
                        }
                        m.sb(sprite.wrapping_add(3), 4);
                        m.sb(sprite.wrapping_add(7), 0x66);
                        for at in 4..7 {
                            m.sb(sprite.wrapping_add(at), 0x80);
                        }
                        m.sh(sprite.wrapping_add(8), x as u16);
                        m.sh(sprite.wrapping_add(0xa), y as u16);
                        m.sh(sprite.wrapping_add(0x10), 8);
                        m.sh(sprite.wrapping_add(0x12), 0x10);
                        if !first {
                            m.sb(sprite.wrapping_add(0xc), u);
                            m.sb(sprite.wrapping_add(0xd), v);
                        }
                        m.sh(sprite.wrapping_add(0xe), clut);
                    }
                    columns = columns.wrapping_add(0x14);
                    caps = caps.wrapping_add(0x14);
                }
            }
            for (at, page_bits) in [(0x21c0, 0x1d), (0x21d8, 0x5d)] {
                let block = page(m);
                m.sw(sp + 0x10, 0);
                sdk::set_draw_mode(m, block.wrapping_add(at + 0xc * buffer), 0, 0, page_bits, 0);
            }
        }
        for buffer in 0..2u32 {
            let block = page(m);
            let mut dot = block.wrapping_add(0x21f0 + 0x64 * buffer);
            for _ in 0..5 {
                m.sb(dot.wrapping_add(3), 4);
                m.sb(dot.wrapping_add(7), 0x66);
                for at in 4..7 {
                    m.sb(dot.wrapping_add(at), 0x80);
                }
                m.sh(dot.wrapping_add(0x10), 0x4c);
                m.sh(dot.wrapping_add(0x12), 0x54);
                m.sb(dot.wrapping_add(0xc), 0);
                m.sb(dot.wrapping_add(0xd), 0);
                m.sh(dot.wrapping_add(0xe), 0xd5b);
                dot = dot.wrapping_add(0x14);
            }
            let block = page(m);
            m.sw(sp + 0x10, 0);
            sdk::set_draw_mode(m, block.wrapping_add(0x2380 + 0xc * buffer), 0, 0, 0x5d, 0);
        }
        let block = page(m);
        for (offset, value) in [
            (0x23ac, 0x20u32),
            (0x23a2, 4),
            (0x2398, 4),
            (0x23a4, 4),
            (0x239a, 4),
            (0x23ae, (-0x30_i32) as u32),
            (0x23b0, 0x38),
            (0x23b2, (-0x18_i32) as u32),
            (0x23aa, 8),
            (0x23a0, 8),
            (0x23a6, 0xc),
            (0x239c, 0xc),
            (0x23a8, 0xc),
            (0x239e, 0xc),
            (0x23b4, 0x40),
        ] {
            m.sh(block.wrapping_add(offset), value as u16);
        }
        super::ov2_p2::reset_cell(m, 2, 0x13);
        super::ov2_p2::set_word_178(m, 2, 0x8_0000);
        super::ov2_p2::reset_cell(m, 0, 0xe);
        super::ov2_p2::set_word_178(m, 0, 0x4001);
        let vis = visual(m);
        let image = m.lw(vis.wrapping_add(0xa8));
        call(m, RELEASE, [image, 0, 0, 0]);
        for offset in [0x130, 0x134, 0x138, 0x13c, 0x140] {
            let vis = visual(m);
            let channel = m.lw(vis.wrapping_add(offset));
            call(m, STOP, [channel, 0, 0, 0]);
        }
        let record = state(m);
        let step = lbu(m, record.wrapping_add(1)).wrapping_add(1);
        m.sb(record.wrapping_add(1), step as u8);
        step
    })
}

/// `addPrim` of `prim` into ordering-table slot 1 of [visual + 8] (both
/// upper bytes kept), the table re-read for each access.
fn link(m: &mut Machine, vis: u32, prim: u32) {
    let ot = m.lw(vis.wrapping_add(8));
    let tag = m.lw(prim);
    let head = m.lw(ot.wrapping_add(4));
    m.sw(prim, (tag & 0xff00_0000) | (head & 0x00ff_ffff));
    let ot = m.lw(vis.wrapping_add(8));
    let head = m.lw(ot.wrapping_add(4));
    m.sw(
        ot.wrapping_add(4),
        (head & 0xff00_0000) | (prim & 0x00ff_ffff),
    );
}

/// Link a draw mode of the music block (+`at` + 0xC * buffer): the
/// original's sequence, the context's buffer re-read for the slot.
fn link_mode(m: &mut Machine, at: u32) {
    let context = m.gw(CONTEXT);
    let buffer = m.lw(context.wrapping_add(4));
    let block = page(m);
    let mode = mul(buffer, 12).wrapping_add(block);
    let vis = visual(m);
    let ot = m.lw(vis.wrapping_add(8));
    let tag = m.lw(mode.wrapping_add(at));
    let head = m.lw(ot.wrapping_add(4));
    m.sw(
        mode.wrapping_add(at),
        (tag & 0xff00_0000) | (head & 0x00ff_ffff),
    );
    let buffer = m.lw(context.wrapping_add(4));
    let ot = m.lw(vis.wrapping_add(8));
    let prim = block.wrapping_add(mul(buffer, 12).wrapping_add(at));
    let head = m.lw(ot.wrapping_add(4));
    m.sw(
        ot.wrapping_add(4),
        (head & 0xff00_0000) | (prim & 0x00ff_ffff),
    );
}

/// Print `frames` (60 per second) with "%2d:%2d.%2d" as minutes, seconds
/// and hundredths.
fn print_time(m: &mut Machine, frames: u32) {
    let seconds = divide(frames, 0x8888_8889, 5);
    let left = frames.wrapping_sub(mul(seconds, 60));
    let hundredths = mul(left, 100);
    let minutes = divide(frames, 0x91a2_b3c5, 11);
    let seconds = seconds.wrapping_sub(mul(minutes, 60));
    let hundredths = divide(hundredths, 0x8888_8889, 5);
    call(m, PRINT3, [0x800c_c838, minutes, seconds, hundredths]);
}

/// The track number of the player clamped as the original does (negative
/// to 0x20, above 0x20 to 0).
fn clamp_track(m: &mut Machine) {
    let record = player(m);
    let mut fits = less(m.lw(record.wrapping_add(8)), 0x21);
    if less(m.lw(record.wrapping_add(8)), 0) {
        m.sw(record.wrapping_add(8), 0x20);
        let record = player(m);
        fits = less(m.lw(record.wrapping_add(8)), 0x21);
    }
    if !fits {
        let record = player(m);
        m.sw(record.wrapping_add(8), 0);
    }
}

/// A random track (frame counter context +0x20 mod 32) for play mode 1.
fn random_track(m: &mut Machine) {
    let context = m.gw(CONTEXT);
    let frame = m.lw(context.wrapping_add(0x20));
    let record = player(m);
    let biased = if (frame as i32) < 0 {
        frame.wrapping_add(0x1f)
    } else {
        frame
    };
    m.sw(
        record.wrapping_add(8),
        frame.wrapping_sub((biased as i32 >> 5 << 5) as u32),
    );
}

/// Restart sounds `ids` into visual +0x130.. (the channels). Returns the
/// last channel.
fn restart(m: &mut Machine, ids: &[u32]) -> u32 {
    let mut channel = sound(m, ids[0]);
    for (n, &id) in ids.iter().enumerate().skip(1) {
        let vis = visual(m);
        m.sw(vis.wrapping_add(0x130 + 4 * (n as u32 - 1)), channel);
        channel = sound(m, id);
    }
    let vis = visual(m);
    let last = 0x130 + 4 * (ids.len() as u32 - 1);
    if ids.len() == 3 {
        let context = m.gw(CONTEXT);
        m.sw(vis.wrapping_add(last), channel);
        m.sw(context.wrapping_add(0x12c), 0);
    } else {
        m.sw(vis.wrapping_add(last), channel);
    }
    channel
}

/// Leave the music page: cell preset 1, state page 0, stop the music
/// (8002C6A8 with 0, 0) and restart the hub sounds. Returns the last
/// channel.
fn leave(m: &mut Machine, ids: &[u32]) -> u32 {
    super::ov2_p2::set_cell_preset(m, 0, 1);
    let record = state(m);
    m.sb(record, 0);
    let record = state(m);
    m.sb(record.wrapping_add(1), 0);
    call(m, MUSIC_STOP, [0, 0, 0, 0]);
    restart(m, ids)
}

/// 800DD1D0: one frame of the music page: the spectrum (18 columns from
/// both channels' samples, 800F770C) and its caps, the five orbiting dots
/// (rcos/rsin of phase x speed), the cursor (pad 8 / 4 / 1-2 by the
/// original's tables 0x800CC844 and 0x800CC864) and its actions (previous
/// / next track, play modes, the title's speaker icons, leave), the
/// player's track and flags, the title (800F7524), the track number and
/// name, the play time (blinking while paused) and the phase step; cancel
/// leaves the page. Returns the cancel test's value (0, the held bits &
/// 0x5F) or, after leaving, the last sound channel.
pub fn music_page(m: &mut Machine) -> u32 {
    framed(m, 0x90, |m, sp| {
        m.sw(sp + 0x60, 0);
        for n in 0..0x12u32 {
            m.sw(sp + 0x54 - 4 * n, 0);
        }
        let mut left = call(m, SPECTRUM, [0, 0, 0, 0]);
        let mut right = call(m, SPECTRUM, [1, 0, 0, 0]);
        let context = m.gw(CONTEXT);
        let buffer = m.lw(context.wrapping_add(4));
        let offset = mul(buffer, 0x870);
        let block = page(m);
        let columns = block.wrapping_add(offset);
        let caps = block.wrapping_add(offset.wrapping_add(0x10e0));
        let mut band = sp + 0x10;
        for sample in 0..100u32 {
            let column = sample / 5;
            let phase = sample % 5;
            let a = lh(m, left);
            left = left.wrapping_add(2);
            let sum = m
                .lw(band)
                .wrapping_add(if less(a, 0) { 0u32.wrapping_sub(a) } else { a });
            m.sw(band, sum);
            let b = lh(m, right);
            right = right.wrapping_add(2);
            let sum = sum.wrapping_add(if less(b, 0) { 0u32.wrapping_sub(b) } else { b });
            m.sw(band, sum);
            if phase == 4 {
                let mean = quotient(m, sum, 10, 0x800d_d2f4);
                m.sw(band, mean);
                let scaled = mul(mean, 7);
                let scaled = if less(scaled, 0) {
                    scaled.wrapping_add(0x7fff)
                } else {
                    scaled
                };
                m.sw(band, (scaled as i32 >> 15) as u32);
                if column < 0x12 {
                    band += 4;
                }
            }
        }
        let vis = visual(m);
        for (sprites, link_at) in [(columns, None), (caps, Some(0x21c0))] {
            if let Some(at) = link_at {
                link_mode(m, at);
            }
            let vis = if link_at.is_some() { visual(m) } else { vis };
            for column in 0..0x12u32 {
                let height = m.lw(sp + 0x10 + 4 * column);
                if (height as i32) <= 0 {
                    continue;
                }
                let mut row = 0u32;
                loop {
                    let sprite = mul(6 * column + row, 0x14).wrapping_add(sprites);
                    link(m, vis, sprite);
                    row += 1;
                    if !less(row, m.lw(sp + 0x10 + 4 * column)) {
                        break;
                    }
                }
            }
        }
        link_mode(m, 0x21d8);
        let context = m.gw(CONTEXT);
        let buffer = m.lw(context.wrapping_add(4));
        let block = page(m);
        call(m, MODE_VALUE, [0; 4]);
        let mut dot = block.wrapping_add(mul(buffer, 0x64).wrapping_add(0x21f0));
        let mut xy = dot.wrapping_add(0xa);
        for n in 0..5u32 {
            let block = page(m);
            let orbit = block.wrapping_add(n << 1);
            let phase = lh(m, block.wrapping_add(0x23b6));
            let speed = lh(m, orbit.wrapping_add(0x23ac));
            let radius_x = sext16(lhu(m, orbit.wrapping_add(0x2398)));
            let radius_y = lhu(m, orbit.wrapping_add(0x23a2));
            let cos = sdk::rcos(m, mul(phase, speed));
            let x = quotient(m, mul(cos, 76), radius_x, 0x800d_d63c);
            let x = if less(x, 0) { x.wrapping_add(0xfff) } else { x };
            let block = page(m);
            m.sh(
                xy.wrapping_sub(2),
                ((x as i32 >> 12) as u32).wrapping_add(0x25) as u16,
            );
            let phase = lh(m, block.wrapping_add(0x23b6));
            let sin = sdk::rsin(m, mul(phase, speed));
            let y = quotient(m, mul(sin, 84), sext16(radius_y), 0x800d_d6bc);
            let y = if less(y, 0) { y.wrapping_add(0xfff) } else { y };
            m.sh(xy, ((y as i32 >> 12) as u32).wrapping_add(0x12e) as u16);
            xy = xy.wrapping_add(0x14);
            let vis = visual(m);
            link(m, vis, dot);
            dot = dot.wrapping_add(0x14);
        }
        link_mode(m, 0x2380);
        cursor(m, sp);
        let block = work(m);
        let flags = m.lw(block.wrapping_add(0x5c));
        let page_block = page(m);
        let cursor = lbu(m, page_block.wrapping_add(0x23b8));
        m.sb(flags.wrapping_add(1), cursor as u8);
        if call(m, MUSIC_POLL, [1, flags, 0, 0]) == 0 {
            let record = player(m);
            if m.lw(record) & 1 != 0 {
                let block = page(m);
                if lbu(m, block.wrapping_add(0x23b9)) == 0 {
                    let track = m.lw(record.wrapping_add(8)).wrapping_add(1);
                    m.sw(record.wrapping_add(8), track);
                }
                let block = page(m);
                if lbu(m, block.wrapping_add(0x23b9)) == 1 {
                    random_track(m);
                }
            }
        }
        clamp_track(m);
        let streaming = call(m, STREAM_STATUS, [0; 4]) & 0x4000_0000 != 0;
        let record = player(m);
        let flags = m.lw(record);
        m.sw(record, if streaming { flags | 2 } else { flags & !2 });
        let record = player(m);
        let speakers = m.lw(sp + 0x60);
        let track = m.lw(record.wrapping_add(8));
        call(m, TITLE, [speakers, track, 0, 0]);
        let time = call(m, PLAY_TIME, [0; 4]);
        call(m, TEXT_FONT, [3, 0, 0, 0]);
        call(m, TEXT_ALIGN, [2, 0, 0, 0]);
        call(m, TEXT_COLOR, [0x80, 0x80, 0x80, 0]);
        call(m, TEXT_AT, [0x66, 0x90, 0x80, 0]);
        let record = player(m);
        let track = m.lw(record.wrapping_add(8));
        call(m, PRINT3, [0x800c_c834, track, 0x80, 0]);
        call(m, TEXT_AT, [0x7c, 0x90, 0x80, 0]);
        let record = player(m);
        let track = m.lw(record.wrapping_add(8));
        let name = m.lw((track << 2).wrapping_add(TRACK_NAMES));
        call(m, PRINT, [name, 0x90, 0x80, 0]);
        call(m, TEXT_FONT, [4, 0x90, 0x80, 0]);
        call(m, TEXT_ALIGN, [2, 0x90, 0x80, 0]);
        call(m, TEXT_AT, [0xc8, 0x18a, 0x80, 0]);
        if call(m, STREAM_STATUS, [0; 4]) & 0x4000_0000 != 0 {
            let context = m.gw(CONTEXT);
            let frame = m.lw(context.wrapping_add(0x20));
            let second = frame.wrapping_sub(mul(divide(frame, 0x8888_8889, 5), 60));
            let half = ((((i64::from(second as i32) * i64::from(0x8888_8889_u32 as i32)) >> 32)
                as u32)
                .wrapping_add(second) as i32
                >> 4) as u32;
            if half != (second as i32 >> 31) as u32 {
                print_time(m, time);
            }
        } else {
            print_time(m, time);
        }
        let paused = call(m, STREAM_STATUS, [0; 4]) & 0x0200_0000 != 0;
        let step = !paused || call(m, STREAM_STATUS, [0; 4]) & 0x4000_0000 != 0;
        if step {
            let block = page(m);
            let phase = lhu(m, block.wrapping_add(0x23b6)).wrapping_add(1);
            m.sh(block.wrapping_add(0x23b6), phase as u16);
        }
        let context = m.gw(CONTEXT);
        let pressed = m.lw(context.wrapping_add(0x12c)) & 0x20;
        if pressed == 0 {
            return 0;
        }
        let held = m.lw(context.wrapping_add(0x128)) & 0x5f;
        if held != 0 {
            return held;
        }
        leave(m, &[0x5d, 0x5e, 0x5f])
    })
}

/// The cursor and actions of [`music_page`].
fn cursor(m: &mut Machine, sp: u32) {
    let context = m.gw(CONTEXT);
    let block = page(m);
    if m.lw(context.wrapping_add(0x130)) & 8 != 0 {
        let at = lbu(m, block.wrapping_add(0x23b8));
        match at {
            1 => m.sb(block.wrapping_add(0x23b8), 0),
            7 => m.sb(block.wrapping_add(0x23b8), 2),
            _ => {
                let block = page(m);
                let at = lbu(m, block.wrapping_add(0x23b8)).wrapping_add(1);
                m.sb(block.wrapping_add(0x23b8), at as u8);
            }
        }
    }
    let context = m.gw(CONTEXT);
    if m.lw(context.wrapping_add(0x130)) & 4 != 0 {
        let block = page(m);
        let at = lbu(m, block.wrapping_add(0x23b8));
        let (target, next) = match at {
            0 => (block, 1),
            2 => (block, 7),
            _ => {
                let block = page(m);
                (block, lbu(m, block.wrapping_add(0x23b8)).wrapping_sub(1))
            }
        };
        m.sb(target.wrapping_add(0x23b8), next as u8);
    }
    let context = m.gw(CONTEXT);
    if m.lw(context.wrapping_add(0x130)) & 3 != 0 {
        let block = page(m);
        let at = lbu(m, block.wrapping_add(0x23b8));
        if at < 8 {
            let next = match at {
                0 => Some(2),
                1 => Some(4),
                2 | 3 => Some(0),
                _ => Some(1),
            };
            if let Some(next) = next {
                let block = page(m);
                m.sb(block.wrapping_add(0x23b8), next);
            }
        }
    }
    let context = m.gw(CONTEXT);
    if m.lw(context.wrapping_add(0x130)) & 0x10 == 0
        || m.lw(context.wrapping_add(0x128)) & 0x20 != 0
    {
        return;
    }
    let block = page(m);
    let at = lbu(m, block.wrapping_add(0x23b8));
    match at {
        0 | 1 => {
            let block = page(m);
            if lbu(m, block.wrapping_add(0x23b9)) == 0 {
                let record = player(m);
                let track = m.lw(record.wrapping_add(8));
                let track = if at == 0 {
                    track.wrapping_sub(1)
                } else {
                    track.wrapping_add(1)
                };
                m.sw(record.wrapping_add(8), track);
            }
            let block = page(m);
            if lbu(m, block.wrapping_add(0x23b9)) == 1 {
                random_track(m);
            }
        }
        2 => {
            let block = page(m);
            m.sb(block.wrapping_add(0x23b9), 0);
        }
        3 => {
            let block = page(m);
            m.sb(block.wrapping_add(0x23b9), 1);
        }
        4 => m.sw(sp + 0x60, 1),
        5 => m.sw(sp + 0x60, 3),
        6 => m.sw(sp + 0x60, 2),
        7 => {
            leave(m, &[0x5d, 0x5e, 0x5f, 0x60, 0x61]);
        }
        _ => {}
    }
}

/// 800DDFA4: open the music menu: reset cell 0x16 of list 2, word
/// 0x400000 into slot 2; unless context +0x44 is 1, reload resource
/// [resources +0x60] (its entries +0/+4 into visual +0x1D4/+0x1DC) and step
/// the state. Returns 1 (context +0x44 is 1) or the new step.
pub fn open_music_menu(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        super::ov2_p2::reset_cell(m, 2, 0x16);
        super::ov2_p2::set_word_178(m, 2, 0x40_0000);
        let context = m.gw(CONTEXT);
        if m.lw(context.wrapping_add(0x44)) == 1 {
            return 1;
        }
        let resources = m.gw(RESOURCES);
        let id = m.lw(resources.wrapping_add(0x60));
        let vis = visual(m);
        m.sw(vis.wrapping_add(0x28), id);
        let resource = call(m, RESOURCE, [id, 0, 0, 0]);
        let image = m.lw(resource.wrapping_add(8));
        call(m, RELEASE, [image, 0, 0, 0]);
        let pointer = m.lw(resource.wrapping_add(8));
        call(m, ARENA_RESET, [pointer, 0, 0, 0]);
        let id = m.lw(resource);
        let first = call(m, RESOURCE, [id, 0, 0, 0]);
        let vis = visual(m);
        m.sw(vis.wrapping_add(0x1d4), first);
        let id = m.lw(resource.wrapping_add(4));
        let second = call(m, RESOURCE, [id, 0, 0, 0]);
        let vis = visual(m);
        let record = state(m);
        m.sw(vis.wrapping_add(0x1dc), second);
        let step = lbu(m, record.wrapping_add(1)).wrapping_add(1);
        m.sb(record.wrapping_add(1), step as u8);
        step
    })
}

/// 800DE078: confirm (pad 0x10) asks for the ending (visual +0x11C = 1,
/// context +0x95F = 1, +0x3A8 = byte 0x8010A3A8); cancel (0x20) returns to
/// page 0 (cell preset 1, visual +0x84/+0x86 and context +0x95F cleared,
/// arena reset to [visual +0x28]); gameplay word +4 = 0. Returns the
/// gameplay pointer ([gp+0x2E0]).
pub fn ending_choice(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let context = m.gw(CONTEXT);
        if m.lw(context.wrapping_add(0x12c)) & 0x10 != 0
            && m.lw(context.wrapping_add(0x128)) & 0x2f == 0
        {
            let vis = visual(m);
            m.sw(vis.wrapping_add(0x11c), 1);
            m.sb(context.wrapping_add(0x95f), 1);
            let context = m.gw(CONTEXT);
            let value = m.lbu(0x8010_a3a8);
            m.sb(context.wrapping_add(0x3a8), value);
        }
        if press(m, 0x20, 0x5f) {
            super::ov2_p2::set_cell_preset(m, 0, 1);
            let record = state(m);
            m.sb(record, 0);
            let record = state(m);
            m.sb(record.wrapping_add(1), 0);
            let vis = visual(m);
            let context = m.gw(CONTEXT);
            m.sh(vis.wrapping_add(0x84), 0);
            m.sh(vis.wrapping_add(0x86), 0);
            m.sb(context.wrapping_add(0x95f), 0);
            let vis = visual(m);
            let pointer = m.lw(vis.wrapping_add(0x28));
            call(m, ARENA_RESET, [pointer, 0, 0, 0]);
            let context = m.gw(CONTEXT);
            m.sw(context.wrapping_add(0x12c), 0);
        }
        let gameplay = m.gw(crate::common::GAMEPLAY);
        m.sw(gameplay.wrapping_add(4), 0);
        gameplay
    })
}

// ---- dispatch -------------------------------------------------------------------------------------

/// The routines of this file by JP address, while module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    let _ = args;
    Some(match target {
        0x800d_cc80 => fade_color(m),
        0x800d_cdc4 => wait_fade(m),
        0x800d_ce0c => open_music(m),
        0x800d_d1d0 => music_page(m),
        0x800d_dfa4 => open_music_menu(m),
        0x800d_e078 => ending_choice(m),
        _ => return None,
    })
}
