//! Overlay module 3 (game state 7): the ending staff credits. Loaded at
//! [`OVERLAY_BASE`](crate::common::OVERLAY_BASE) from `ACE.BIN` file 3
//! (85,800 bytes: images, the 197 credit lines at 800D9B2C, two fonts,
//! code from 800D6414; arena from 0x800DECE8). State 5 goes here when
//! context +0x3AC is 2 (the ending); the credits return to context +0x3A4.
//!
//! Over a streamed picture sequence (movie table entry 2's file, the same
//! chunk format as the resident picture player but 40x256 texture tiles)
//! the credits draw "Produced by" / the Namco logo / the copyright lines,
//! the scrolling list of headings and names (two fonts, scrolling 2 lines
//! every other frame), and a moving reticle with a fading trail.
//!
//! | State 7 substate | Routine |
//! | --- | --- |
//! | 0 | 800D67D4 [`start`] |
//! | 1 | 800D94A0 [`frame`] |
//!
//! The block `[gp+0x4E4]` (0x3DDC bytes) and the decoder `[gp+0x4E8]` are
//! the resident picture player's pointers (see `states.rs`).
use crate::{
    arena,
    common::{self, CONTEXT},
    dispatch,
    machine::{Fault, Machine, SmallData},
    mainloop::transition::{NEXT_SUBSTATE, RESET_ARENA, SET_STATE},
    sdk,
    states::{self, Invoker, picture as pic},
};

/// Overlay module number.
pub const MODULE: u32 = 3;
/// `[gp+0x4E4]`: the credits block: +0 phase, +4/+8 two 0x4000-byte packet
/// areas, +0xC/+0x24 two 6-entry ordering tables, +0x3C next packet, +0x40
/// current table, +0x44..+0x52 layout, +0x54/+0x56 320x240, +0x58 stream
/// file, +0x5C mode, +0x60/+0x64 saved VRAM, +0x68 flag, +0x6C the reticle
/// ([`reticle`]), +0x354 the credit list ([`credits`]), +0x3DD4 scroll
/// time, +0x3DD8 the frame counter at the start.
const BLOCK: u32 = 0x4e4;
/// `[gp+0x4E8]`: the picture decoder.
const DECODER: u32 = 0x4e8;
/// `gp+0x2C4`: display request block.
const DISPLAY: u32 = 0x2c4;
/// `[gp+0x20C]`: the movie/picture table (entry 2's file +0x10).
const MOVIE_TABLE: u32 = 0x20c;

/// The reticle record (block +0x6C): +0/+2 position, +4/+6 size (16x24),
/// +0xC colour word, +0x10 key index, +0x14/+0xDC trail x/y (100
/// halfwords), +0x1A4 trail spread (100 halfwords), +0x26C trail
/// brightness (100 bytes), +0x2D0 trail count, +0x2D4 trail points shown,
/// +0x2D8 fading, +0x2DC fade start, +0x2E4 time.
pub mod reticle {
    pub const KEY: u32 = 0x10;
    pub const TRAIL_X: u32 = 0x14;
    pub const TRAIL_Y: u32 = 0xdc;
    pub const SPREAD: u32 = 0x1a4;
    pub const BRIGHTNESS: u32 = 0x26c;
    pub const COUNT: u32 = 0x2d0;
    pub const SHOWN: u32 = 0x2d4;
    pub const FADING: u32 = 0x2d8;
    pub const FADE_START: u32 = 0x2dc;
    pub const TIME: u32 = 0x2e4;
}
use reticle as ret;

/// The credit list (block +0x354): +4i line kind (0 heading, 1 name, 2..5
/// logos), +0x314 + 0x40i text, +0x3454 + 4i x, +0x3768 + 4i y, +0x3A7C /
/// +0x3A7E first / last line on screen (halfwords), +0x3A80 scroll time.
pub mod credits {
    pub const TEXT: u32 = 0x314;
    pub const X: u32 = 0x3454;
    pub const Y: u32 = 0x3768;
    pub const FIRST: u32 = 0x3a7c;
    pub const LAST: u32 = 0x3a7e;
    pub const TIME: u32 = 0x3a80;
    pub const LINES: u32 = 0xc5;
}
use credits as cr;

/// 800D9B2C: the 197 credit records (0x48 bytes: kind, text, x, y).
const CREDIT_RECORDS: u32 = 0x800d_9b2c;
/// 800DE9F4: font records (12 bytes: u, v, -, line height, tpage, CLUT,
/// glyph table); 800DEB14: character map.
const FONTS: u32 = 0x800d_e9f4;
const CHARACTER_MAP: u32 = 0x800d_eb14;
/// 800D5D64 (0x5B0 bytes) / 800D6314 (0x100 bytes): the glyph table and
/// character map 800D98CC copies.
const WIDTH_GLYPHS: u32 = 0x800d_5d64;
const WIDTH_MAP: u32 = 0x800d_6314;
/// 800DEC6C / 800DECBC: texture-tile rectangles and sources of the stream.
const TILE_RECTS: u32 = 0x800d_ec6c;
const TILE_SOURCES: u32 = 0x800d_ecbc;
/// 800D6EF8: completion routine of the credits stream.
pub const STREAM_DONE: u32 = 0x800d_6ef8;

const RESET_CD_QUEUE: u32 = 0x8002_c280;
const QUEUE_STREAM: u32 = 0x8002_c500;
const PROCESS_CD_QUEUE: u32 = 0x8002_c0b4;
const NEXT_CHUNK: u32 = 0x8002_b5c4;
const RELEASE_CHUNK: u32 = 0x8002_b7b8;
const CLEAR_IMAGE: u32 = 0x800a_67e8;
const STRCPY: u32 = 0x800a_c2d8;
const STRLEN: u32 = 0x800a_c328;

fn block(m: &mut Machine) -> u32 {
    m.gw(BLOCK)
}
fn decoder(m: &mut Machine) -> u32 {
    m.gw(DECODER)
}
fn sext16(value: u32) -> i32 {
    i32::from(value as u16 as i16)
}
/// `(v + sign) >> 1` of a signed halfword (the compiler's `/ 2`).
fn half(value: u16) -> u32 {
    let v = i32::from(value as i16);
    ((v + i32::from(v < 0)) >> 1) as u32
}
fn gray(level: u32) -> u32 {
    let b = level & 0xff;
    (b << 16) | (b << 8) | b
}
/// Signed `div` quotient at `site` (the `div` instruction) behind the
/// compiler's checks: `break 7` at `site + 0xC` for a zero divisor and
/// `break 6` at `site + 0x24` for `0x80000000 / -1`. A reached trap records
/// `Fault::Trap` and yields the MIPS result.
fn quotient(m: &mut Machine, n: i32, d: i32, site: u32) -> i32 {
    if d == 0 {
        m.raise(Fault::Trap {
            address: site + 0xc,
            code: 7,
        });
        return if n < 0 { 1 } else { -1 };
    }
    if d == -1 && n == i32::MIN {
        m.raise(Fault::Trap {
            address: site + 0x24,
            code: 6,
        });
        return i32::MIN;
    }
    n / d
}
/// Signed `div` remainder at `site`, with the same checks as [`quotient`].
fn remainder(m: &mut Machine, n: i32, d: i32, site: u32) -> i32 {
    if d == 0 {
        m.raise(Fault::Trap {
            address: site + 0xc,
            code: 7,
        });
        return n;
    }
    if d == -1 && n == i32::MIN {
        m.raise(Fault::Trap {
            address: site + 0x24,
            code: 6,
        });
        return 0;
    }
    n % d
}
/// `x / 100` by the compiler's reciprocal (0x51EB851F, shift `shift`).
fn div_magic(x: i32, shift: u32) -> i32 {
    let hi = ((i64::from(x) * 0x51eb_851f) >> 32) as i32;
    (hi >> shift).wrapping_sub(x >> 31)
}
fn link_or(m: &mut Machine, ot: u32, packet: u32, words: u32) {
    let link = m.lw(ot);
    m.sw(packet, link | (words << 24));
    m.sw(ot, (packet << 8) >> 8);
}
fn link_masked(m: &mut Machine, ot: u32, packet: u32) {
    let tag = m.lw(packet);
    let link = m.lw(ot);
    m.sw(packet, (tag & 0xff00_0000) | (link & 0x00ff_ffff));
    let link = m.lw(ot);
    m.sw(ot, (link & 0xff00_0000) | (packet & 0x00ff_ffff));
}

// ---- state 7 callbacks ------------------------------------------------------------------

/// 800D653C: upload the credits' images (LoadImage of ten overlay images
/// with rectangles from 800D9AAC.., built in 800DEC14) with a DrawSync after
/// each group.
pub fn upload_images(m: &mut Machine) {
    m.push_frame(0x28);
    const RECT: u32 = 0x800d_ec14;
    // (rectangle offset, x/y/w/h sources (0 = CLUT: 16x1), pixels).
    let groups: [&[(u32, [u32; 4], u32)]; 3] = [
        &[
            (
                0,
                [0x800d_9b1c, 0x800d_9b20, 0x800d_9b14, 0x800d_9b18],
                0x800c_dc94,
            ),
            (8, [0x800d_9b24, 0x800d_9b28, 0, 0], 0x800c_dc74),
        ],
        &[
            (
                0,
                [0x800d_9ab4, 0x800d_9ab8, 0x800d_9aac, 0x800d_9ab0],
                0x800c_9de4,
            ),
            (
                8,
                [0x800d_9acc, 0x800d_9ad0, 0x800d_9ac4, 0x800d_9ac8],
                0x800c_c114,
            ),
            (
                0x10,
                [0x800d_9ae4, 0x800d_9ae8, 0x800d_9adc, 0x800d_9ae0],
                0x800c_cb54,
            ),
            (
                0x18,
                [0x800d_9afc, 0x800d_9b00, 0x800d_9af4, 0x800d_9af8],
                0x800c_d474,
            ),
            (0x20, [0x800d_9abc, 0x800d_9ac0, 0, 0], 0x800c_9dc4),
            (0x28, [0x800d_9ad4, 0x800d_9ad8, 0, 0], 0x800c_c0f4),
        ],
        &[
            (0x30, [0x800d_9aec, 0x800d_9af0, 0, 0], 0x800c_cb34),
            (0x38, [0x800d_9b04, 0x800d_9b08, 0, 0], 0x800c_d454),
        ],
    ];
    for group in groups {
        for &(offset, [x, y, w, h], pixels) in group {
            let rect = RECT + offset;
            let x = m.lhu(x);
            let y = m.lhu(y);
            if w == 0 {
                m.sh(rect + 4, 0x10);
                m.sh(rect + 6, 1);
                m.sh(rect, x);
                m.sh(rect + 2, y);
            } else {
                let w = m.lhu(w);
                let h = m.lhu(h);
                m.sh(rect, x);
                m.sh(rect + 2, y);
                m.sh(rect + 4, w);
                m.sh(rect + 6, h);
            }
            sdk::load_image(m, rect, pixels);
        }
        sdk::draw_sync(m, 0);
    }
    m.pop_frame(0x28);
}

/// 800D6414(reticle): reset the reticle: 16x24 at 0,0, colour 0x60808080,
/// key 0, no trail.
pub fn reset_reticle(m: &mut Machine, c: u32) {
    m.sh(c + 4, 0x10);
    m.sh(c + 6, 0x18);
    m.sb(c + 0xc, 0x80);
    m.sb(c + 0xd, 0x80);
    m.sb(c + 0xe, 0x80);
    m.sh(c, 0);
    m.sh(c + 2, 0);
    m.sb(c + 0xf, 0x60);
    m.sw(c + ret::KEY, 0);
    m.sw(c + ret::COUNT, 0);
    m.sw(c + ret::SHOWN, 0);
    m.sb(c + ret::FADING, 0);
    m.sw(c + ret::FADE_START, 0);
    m.sw(c + 0x2e0, 0);
    for i in 0..100 {
        m.sh(c + ret::SPREAD + 2 * i, 0);
        m.sb(c + ret::BRIGHTNESS + i, 0);
    }
    m.sw(c + ret::TIME, 0);
}

/// 800D6488(list): copy the 197 credit records (kind, text (strcpy), x, y
/// + 680) into the list; nothing on screen yet.
pub fn load_credits(m: &mut Machine, c: &mut Invoker, list: u32) {
    m.push_frame(0x38);
    m.sh(list + cr::FIRST, 0);
    m.sh(list + cr::LAST, 0);
    for i in 0..cr::LINES {
        let record = CREDIT_RECORDS + i * 0x48;
        let kind = m.lw(record);
        m.sw(list + 4 * i, kind);
        c(m, STRCPY, [list + cr::TEXT + 0x40 * i, record + 4, 0, 0]);
        let x = m.lh(record + 0x44) as i32 as u32;
        m.sw(list + cr::X + 4 * i, x);
        let y = (m.lh(record + 0x46) as i32 as u32).wrapping_add(0x2a8);
        m.sw(list + cr::Y + 4 * i, y);
    }
    m.sw(list + cr::TIME, 0);
    m.pop_frame(0x38);
}

/// 800D67D4 (state 7, substate 0): on buffer 1, display phase 1 (requests
/// 1|8, colour 0, interval 0), context bit 0x10000000; the 0x3DDC-byte
/// block (two 0x4000-byte packet areas, 320x240, file of movie entry 2),
/// two VRAM areas saved (800D5C94's rectangle into 0x28000 bytes, 480,256
/// 96x256 into 0xC000), the screen cleared, the images uploaded, the
/// reticle and credit list set up. Returns NEXT_SUBSTATE (0 on buffer 0).
pub fn start(m: &mut Machine, c: &mut Invoker) -> u32 {
    let frame = m.push_frame(0x40);
    let context = m.gw(CONTEXT);
    let result = if m.lw(context + 4) == 0 {
        0
    } else {
        let display = m.gw(DISPLAY);
        m.sb(display + 0x10, 0);
        let display2 = m.gw(DISPLAY);
        m.sw(display + 4, 1);
        m.sw(display + 0x14, 0);
        m.sb(display2 + 0x11, 0);
        let display = m.gw(DISPLAY);
        m.sb(display + 0x12, 0);
        let display = m.gw(DISPLAY);
        let flags = m.lw(display);
        let context = m.gw(CONTEXT);
        m.sw(display, flags | 9);
        let word = m.lw(context);
        m.sw(context, word | 0x1000_0000);
        let b = arena::alloc(m, 0x3ddc);
        m.set_gw(BLOCK, b);
        let first = arena::alloc(m, 0x4000);
        let b = block(m);
        m.sw(b + 4, first);
        let second = arena::alloc(m, 0x4000);
        let b = block(m);
        m.sw(b + 8, second);
        m.sh(b + 0x54, 0x140);
        let table = m.gw(MOVIE_TABLE);
        m.sw(b, 0);
        m.sh(b + 0x56, 0xf0);
        let file = m.lh(table + 0x10) as i32 as u32;
        m.sw(b + 0x5c, 0);
        m.sw(b + 0x58, file);
        let rect = frame + 0x10;
        for i in 0..2 {
            let word = m.lw(0x800d_5c94 + 4 * i);
            m.sw(rect + 4 * i, word);
        }
        let pixels = arena::alloc(m, 0x2_8000);
        let b = block(m);
        m.sw(b + 0x60, pixels);
        sdk::store_image(m, rect, pixels);
        m.sh(rect, 0x1e0);
        m.sh(rect + 2, 0x100);
        m.sh(rect + 4, 0x60);
        m.sh(rect + 6, 0x100);
        let pixels = arena::alloc(m, 0xc000);
        let b = block(m);
        m.sw(b + 0x64, pixels);
        sdk::store_image(m, rect, pixels);
        let clear = frame + 0x18;
        m.sh(clear, 0);
        m.sh(clear + 2, 0);
        m.sh(clear + 4, 0x1e0);
        m.sh(clear + 6, 0xf0);
        c(m, CLEAR_IMAGE, [clear, 0, 0, 0]);
        m.sh(clear, 0);
        m.sh(clear + 2, 0x100);
        c(m, CLEAR_IMAGE, [clear, 0, 0, 0]);
        sdk::draw_sync(m, 0);
        upload_images(m);
        let context = m.gw(CONTEXT);
        let b = block(m);
        let time = m.lw(context + 0x24);
        m.sw(b + 0x68, 1);
        m.sw(b + 0x3dd8, time);
        reset_reticle(m, b + 0x6c);
        let b = block(m);
        load_credits(m, c, b + 0x354);
        NEXT_SUBSTATE
    };
    m.pop_frame(0x40);
    result
}

/// 800D9558: end: reset the CD queue and restore the two saved VRAM areas
/// (rectangles 800DD324 / 800DD32C). Returns 1.
pub fn finish(m: &mut Machine, c: &mut Invoker) -> u32 {
    m.push_frame(0x18);
    c(m, RESET_CD_QUEUE, [0; 4]);
    let b = block(m);
    let pixels = m.lw(b + 0x60);
    sdk::load_image(m, 0x800d_d324, pixels);
    let b = block(m);
    let pixels = m.lw(b + 0x64);
    sdk::load_image(m, 0x800d_d32c, pixels);
    m.pop_frame(0x18);
    1
}

/// 800D94A0 (state 7, substate 1): one frame: the buffer's 6-entry table
/// (`ClearOTagR`) and packet area, the player; when it ends, [`finish`]
/// and SET_STATE to context +0x3A4 with an arena reset, otherwise
/// `DrawOTag(table + 0x14)`; returns 0.
pub fn frame(m: &mut Machine, c: &mut Invoker) -> u32 {
    m.push_frame(0x20);
    let context = m.gw(CONTEXT);
    let buffer = m.lw(context + 4);
    let b = block(m);
    let ot = b.wrapping_add(buffer.wrapping_mul(0x18).wrapping_add(0xc));
    m.sw(b + 0x40, ot);
    let packets = m.lw(b.wrapping_add(buffer << 2) + 4);
    m.sw(b + 0x3c, packets);
    sdk::clear_otag_r(m, ot, 6);
    let result = if player(m, c) == 0 {
        let b = block(m);
        let ot = m.lw(b + 0x40);
        sdk::draw_otag(m, ot.wrapping_add(0x14));
        0
    } else {
        finish(m, c);
        let context = m.gw(CONTEXT);
        m.lw(context + 0x3a4) | RESET_ARENA | SET_STATE
    };
    m.pop_frame(0x20);
    result
}

/// 800D69EC: the credits player (block phase +0): 0 lay out the decoder
/// (ring 0xD740 bytes) and start the stream; 1 on buffer 0 wait for the
/// first chunk (END ends); 2 show frames (START ends); 3/4 clear the
/// screen; 5 restore the display (phase 0, interval 2, requests 1|4,
/// context bit 0x10000000 cleared) and return 1.
pub fn player(m: &mut Machine, c: &mut Invoker) -> u32 {
    let frame = m.push_frame(0x38);
    let b = block(m);
    let phase = m.lw(b);
    let set_phase = |m: &mut Machine, phase: u32| {
        let b = block(m);
        m.sw(b, phase);
    };
    let result = match phase {
        0 => {
            let buffer = arena::peek(m, 0);
            let b = block(m);
            let mut layout = [0u32; 8];
            for (i, slot) in layout.iter_mut().enumerate() {
                *slot = m.lh(b + 0x44 + 2 * i as u32) as i32 as u32;
                m.sw(frame + 0x10 + 4 * i as u32, *slot);
            }
            let file = m.lw(b + 0x58);
            let mode = m.lw(b + 0x5c);
            // 800D6CB4: 80056C64's instructions (its callee 800D6D58 differs
            // from 80056D04 only by a repeated store of +0x10).
            states::picture_decoder_init(m, c, file, mode, buffer, 0xd740, layout);
            stream_start(m, c);
            set_phase(m, 1);
            0
        }
        1 => {
            let context = m.gw(CONTEXT);
            if m.lw(context + 4) == 0 {
                m.sw(context + 4, 0);
                if wait_frame(m, c, 1) != 0 {
                    set_phase(m, 2);
                } else {
                    let d = decoder(m);
                    if m.lbu(d + pic::END) == 1 {
                        set_phase(m, 3);
                    }
                }
            }
            let d = decoder(m);
            let count = m.lw(d + pic::FRAME).wrapping_add(1);
            m.sw(d + pic::FRAME, count);
            0
        }
        2 => {
            let context = m.gw(CONTEXT);
            if m.lw(context + 0x12c) & 0x40 != 0 {
                let d = decoder(m);
                m.sb(d + pic::END, 1);
            }
            if credits_frame(m, c) != 0 {
                set_phase(m, 3);
            }
            0
        }
        3 | 4 => {
            if phase == 3 {
                states::picture_stop(m, c);
            }
            states::picture_bank_zero(m);
            let rect = if phase == 3 { 0x800d_ec5c } else { 0x800d_ec64 };
            m.sh(rect, 0);
            m.sh(rect + 4, 0x140);
            m.sh(rect + 2, 0);
            m.sh(rect + 6, 0xf0);
            c(m, CLEAR_IMAGE, [rect, 0, 0, 0]);
            if phase == 4 {
                set_phase(m, 5);
            } else {
                let context = m.gw(CONTEXT);
                if m.lw(context + 4) == 0 {
                    set_phase(m, 4);
                }
            }
            0
        }
        5 => {
            let context = m.gw(CONTEXT);
            let word = m.lw(context);
            let display = m.gw(DISPLAY);
            m.sw(context, word & 0xefff_ffff);
            let flags = m.lw(display);
            m.sw(display + 4, 0);
            m.sw(display + 0x14, 2);
            m.sw(display, flags | 5);
            1
        }
        _ => 0,
    };
    m.pop_frame(0x38);
    result
}

/// 800D6EB8: start streaming: 8002C500(file, ring, ring size, 0x2000,
/// completion 800D6EF8).
pub fn stream_start(m: &mut Machine, c: &mut Invoker) {
    let frame = m.push_frame(0x20);
    let d = decoder(m);
    m.sw(frame + 0x10, STREAM_DONE);
    let file = m.lw(d + pic::FILE);
    let ring = m.lw(d + pic::RING);
    let size = m.lw(d + pic::RING_SIZE);
    c(m, QUEUE_STREAM, [file, ring, size, 0x2000]);
    m.pop_frame(0x20);
}

/// 800D6F48: [`next_chunk`]; with `poll` 0 retry for up to 121 frames (CD
/// queue, `VSync(0)`), then END. Returns 1 once a chunk was taken or the
/// wait gave up.
pub fn wait_frame(m: &mut Machine, c: &mut Invoker, poll: u32) -> u32 {
    let frame = m.push_frame(0x28);
    let result = if poll != 0 {
        let d = decoder(m);
        u32::from(next_chunk(m, c, d) != 0)
    } else {
        m.sw(frame + 0x10, 0x78);
        loop {
            let d = decoder(m);
            if next_chunk(m, c, d) != 0 {
                break 1;
            }
            c(m, PROCESS_CD_QUEUE, [0; 4]);
            crate::states::vsync_wait(m);
            let count = m.lw(frame + 0x10).wrapping_sub(1);
            m.sw(frame + 0x10, count);
            if (m.lw(frame + 0x10) as i32) < 0 {
                let d = decoder(m);
                m.sb(d + pic::END, 1);
                break 1;
            }
        }
    };
    m.pop_frame(0x28);
    result
}

fn slot(d: u32, bank: u32, slot: u32) -> u32 {
    d.wrapping_add(bank.wrapping_mul(pic::BANK_SIZE))
        .wrapping_add(slot.wrapping_mul(pic::SLOT_SIZE))
}

/// 800D6FF4: take the next credits chunk (the resident picture format,
/// see `states::picture_next_chunk`), with these differences: only tile 0
/// is a CLUT; other tiles drop every 574th word (0x1426 words compacted);
/// a picture starts at tile (+0x13 & 0xF) + 7; the uploads are 40x256 tiles
/// (x from 0x140 or 0x200 per bank) and the CLUT row is 256x1 at 768,1/2.
pub fn next_chunk(m: &mut Machine, c: &mut Invoker, d: u32) -> u32 {
    let frame = m.push_frame(0x28);
    let result = next_chunk_body(m, c, d, frame);
    m.pop_frame(0x28);
    result
}

fn next_chunk_body(m: &mut Machine, c: &mut Invoker, d: u32, frame: u32) -> u32 {
    if c(m, NEXT_CHUNK, [frame + 0x10, frame + 0x14, 0, 0]) != 0 {
        return 0;
    }
    let header = m.lw(frame + 0x14);
    if m.lhu(header + 6) == 0 {
        m.sb(d + pic::END, 1);
    }
    let header = m.lw(frame + 0x14);
    let kinds = m.lbu(header + 0xc);
    let selector = u32::from(m.lbu(header + 0x13) & 0xf);
    let page = u32::from(m.lbu(header + 0xf));
    let restart = m.lbu(header + 0xd);
    let kind = kinds & 0xf;
    let step = kinds >> 4;
    if page == 0 {
        let mut entry = m.lw(frame + 0x10).wrapping_add(0x14);
        for _ in 0..0x100 {
            let value = m.lhu(entry);
            if value != 0 {
                m.sh(entry, value | 0x8000);
            }
            entry = entry.wrapping_add(2);
        }
    } else {
        let data = m.lw(frame + 0x10);
        let mut out = data;
        for i in 0..0x1426i32 {
            let hi = ((i64::from(i) * 0x725f_9bed) >> 32) as i32;
            let skip = sext16(((hi >> 8).wrapping_sub(i >> 31)) as u32);
            let word = m.lw(data.wrapping_add((i.wrapping_add(skip) as u32) << 2));
            m.sw(out, word);
            out = out.wrapping_add(4);
        }
    }
    if page == selector.wrapping_add(7) {
        let bank = m.lbu(d + pic::BANK);
        m.sb(d + pic::BANK, u8::from(bank == 0));
        if restart == 1 {
            m.sb(d + pic::BANK, 0);
        }
        for s in 0..5u32 {
            let bank = u32::from(m.lbu(d + pic::BANK));
            m.sb(slot(d, bank, s) + pic::SLOTS, 0);
            let bank = u32::from(m.lbu(d + pic::BANK));
            m.sh(slot(d, bank, s) + 0x14, 0xffff);
            let bank = u32::from(m.lbu(d + pic::BANK));
            m.sh(slot(d, bank, s) + 0x16, 0xffff);
            for i in 0..4u32 {
                let bank = u32::from(m.lbu(d + pic::BANK));
                m.sh(slot(d, bank, s) + 0x18 + 2 * i, 0);
            }
        }
    }
    if (2..18).contains(&page) {
        let first = pic::TILE_PAGES + 17 - page;
        let offsets: &[u32] = if page < 6 {
            &[first, first + 4]
        } else {
            &[first]
        };
        for &offset in offsets {
            let bank = u32::from(m.lbu(d + pic::BANK));
            let flags = d.wrapping_add(bank * 0x14);
            let header = m.lw(frame + 0x14);
            let value = m.lbu(header + 0x13) >> 4;
            m.sb(flags + offset, value);
        }
    }
    let header = m.lw(frame + 0x14);
    let record = m.lbu(header + 0x10);
    if record & 0xf == 1 {
        let s = u32::from(record >> 4);
        let bank = u32::from(m.lbu(d + pic::BANK));
        let effect = m.lbu(header + 0x11) & 0xf;
        m.sb(slot(d, bank, s) + pic::SLOTS, effect);
        let header = m.lw(frame + 0x14);
        let bank = u32::from(m.lbu(d + pic::BANK));
        let start = m.lhu(header + 0x14);
        m.sh(slot(d, bank, s) + 0x14, start);
        let bank = u32::from(m.lbu(d + pic::BANK));
        let end = m.lhu(header + 0x16);
        m.sh(slot(d, bank, s) + 0x16, end);
        for i in 0..4u32 {
            let value = m.lhu(header + 0x18 + 2 * i);
            let bank = u32::from(m.lbu(d + pic::BANK));
            m.sh(slot(d, bank, s) + 0x18 + 2 * i, value);
        }
    }
    if kind == 0 {
        let rect = TILE_RECTS + page * 8;
        if page != 0 {
            let x0: i32 = if m.lbu(d + pic::BANK) != 0 {
                0x200
            } else {
                0x140
            };
            let tile = page as i32 - 1;
            let row = tile >> 2;
            let column = tile - (row << 2) - 3;
            m.sh(rect, (x0 - column * 40) as u16);
            m.sh(rect + 2, (0 - ((row - 1) << 8)) as u16);
            m.sh(rect + 4, 0x28);
            m.sh(rect + 6, 0x100);
        } else {
            m.sh(TILE_RECTS, 0x300);
            m.sh(TILE_RECTS + 4, 0x100);
            m.sh(TILE_RECTS + 6, 1);
            let y = if m.lbu(d + pic::BANK) != 0 { 2 } else { 1 };
            m.sh(TILE_RECTS + 2, y);
        }
        let source = m.lw(frame + 0x10).wrapping_add(0x14);
        m.sw(TILE_SOURCES + page * 4, source);
        sdk::load_image(m, rect, source);
    }
    if step == 1 {
        m.sb(d + pic::STEP, if restart == 1 { 2 } else { 1 });
    }
    let header = m.lw(frame + 0x14);
    c(m, RELEASE_CHUNK, [header, 0, 0, 0]);
    1
}

// ---- the credits frame ------------------------------------------------------------------

/// Copy `bytes` of the overlay's string `source` to `buffer` and clear
/// `fill` bytes after it (memset), as the frame's local text buffers.
fn text_buffer(m: &mut Machine, buffer: u32, source: u32, bytes: u32, fill: u32) {
    for i in 0..bytes {
        let byte = m.lbu(source + i);
        m.sb(buffer + i, byte);
    }
    sdk::memset(m, buffer + bytes, 0, fill);
}

/// Centered text at y `y` in font 0x15 with colour `color` (800D98CC,
/// 800D95B0) appended to the block's packets.
fn centered_text(m: &mut Machine, c: &mut Invoker, frame: u32, text: u32, y: u32, color: u32) {
    let width = text_width(m, c, 0x15, text);
    let x = 0xa0u32.wrapping_sub(((width as i32 + (width >> 31) as i32) >> 1) as u32);
    let b = block(m);
    m.sw(frame + 0x10, x);
    m.sw(frame + 0x14, y);
    m.sw(frame + 0x18, color);
    let ot = m.lw(b + 0x40);
    let packets = m.lw(b + 0x3c);
    let next = draw_text(m, c, 0x15, text, ot, packets, [x, y, color]);
    let b = block(m);
    m.sw(b + 0x3c, next);
}

/// 800D77B4: one credits frame. A chunk step flips the shown bank (1) or
/// starts the layered sequence (2: the frame counter restarts at context
/// +0x24). Layered: the background tiles ([`layers`]), the reticle
/// ([`draw_reticle`]), then the scrolling list ([`draw_credits`]) once the
/// last line's y is 190 or more; before that "Produced by" with the Namco
/// logo (first 19 pictures, fading after frame 128) or the two copyright
/// lines (fading in and out over frames 200..963). The title "ACECOMBAT3
/// electrosphere" fades over the first pictures, and before the layered
/// sequence it fades in over frames 150..277. Then the next chunk; returns
/// 1 at END.
pub fn credits_frame(m: &mut Machine, c: &mut Invoker) -> u32 {
    let frame = m.push_frame(0x438);
    let d = decoder(m);
    let step = m.lbu(d + pic::STEP);
    if step != 0 {
        if step == 1 {
            let shown = m.lbu(d + pic::SHOWN_BANK);
            m.sb(d + pic::SHOWN_BANK, shown ^ 1);
            let d = decoder(m);
            let count = m.lhu(d + pic::SHOWN).wrapping_add(1);
            m.sh(d + pic::SHOWN, count);
        } else {
            m.sb(d + pic::SHOWN_BANK, 0);
            let d = decoder(m);
            m.sh(d + pic::SHOWN, 0);
            m.sb(d + pic::LAYERED, 1);
            let context = m.gw(CONTEXT);
            let b = block(m);
            let time = m.lw(context + 0x24);
            m.sw(b + 0x3dd8, time);
        }
        let d = decoder(m);
        m.sb(d + pic::STEP, 0);
        let d2 = decoder(m);
        m.sw(d + pic::FRAME, 0);
        m.sb(d2 + pic::SLOT, 0);
        let d = decoder(m);
        m.sb(d + pic::EFFECT, 0);
    }
    let d = decoder(m);
    let layered = m.lbu(d + pic::LAYERED);
    if layered == 1 {
        let context = m.gw(CONTEXT);
        let b = block(m);
        let start = m.lw(b + 0x3dd8);
        let packets = m.lw(b + 0x3c);
        let now = m.lw(context + 0x24);
        m.sw(b + 0x350, now.wrapping_sub(start));
        let now = m.lw(context + 0x24);
        let ot = m.lw(b + 0x40);
        m.sw(b + 0x3dd4, now.wrapping_sub(start));
        let next = layers(m, d, packets, ot);
        let b = block(m);
        let ot = m.lw(b + 0x40);
        m.sw(b + 0x3c, next);
        let next = draw_reticle(m, b + 0x6c, ot, next);
        let b = block(m);
        let last_y = m.lw(b + 0x3dc8);
        m.sw(b + 0x3c, next);
        if (last_y as i32) >= 0xbe {
            let ot = m.lw(b + 0x40);
            let next = draw_credits(m, c, b + 0x354, ot, next);
            let b = block(m);
            m.sw(b + 0x3c, next);
        } else {
            let d = decoder(m);
            if m.lh(d + pic::SHOWN) < 0x13 || (m.lw(d + pic::FRAME) as i32) < 0x80 {
                let early = m.lh(d + pic::SHOWN) < 0x13;
                let (text, color) = if early {
                    let text = frame + 0x20;
                    text_buffer(m, text, 0x800d_5cf4, 12, 0xf4);
                    (text, 0x0080_8080)
                } else {
                    let text = frame + 0x120;
                    text_buffer(m, text, 0x800d_5cf4, 12, 0xf4);
                    let d = decoder(m);
                    let level = 0xffff_ff80u32.wrapping_sub(u32::from(m.lbu(d + pic::FRAME)));
                    (text, gray(level))
                };
                let width = text_width(m, c, 0x15, text);
                let x = 0xa0u32.wrapping_sub(((width as i32 + (width >> 31) as i32) >> 1) as u32);
                let b = block(m);
                m.sw(frame + 0x10, x);
                m.sw(frame + 0x14, 0xbe);
                m.sw(frame + 0x18, color);
                let ot = m.lw(b + 0x40);
                let packets = m.lw(b + 0x3c);
                let logo = draw_text(m, c, 0x15, text, ot, packets, [x, 0xbe, color]);
                let b = block(m);
                m.sw(b + 0x3c, logo);
                let code = if early {
                    0x6680_8080
                } else {
                    color | 0x6600_0000
                };
                m.sw(logo + 4, code);
                m.sw(logo + 8, 0x00fa_0060);
                m.sw(logo + 0xc, 0x00f3_4880);
                m.sw(logo + 0x10, 0x0020_0080);
                let ot = m.lw(b + 0x40);
                link_or(m, ot, logo, 4);
                let page = logo.wrapping_add(0x14);
                m.sb(page + 3, layered);
                m.sw(page + 4, 0xe100_023e);
                link_masked(m, ot, page);
                let b = block(m);
                m.sw(b + 0x3c, logo.wrapping_add(0x1c));
            } else {
                let first = frame + 0x220;
                text_buffer(m, first, 0x800d_5d00, 24, 0xe8);
                let second = frame + 0x320;
                text_buffer(m, second, 0x800d_5d18, 20, 0xec);
                let d = decoder(m);
                let time = m.lw(d + pic::FRAME);
                let level = if time.wrapping_sub(0xc8) < 0x40 {
                    time.wrapping_sub(0xc8) << 1
                } else if time.wrapping_sub(0x108) < 0x27c {
                    0x80
                } else {
                    let out = time.wrapping_sub(0x384);
                    if out < 0x40 {
                        0xffff_ff80u32.wrapping_sub(out << 1)
                    } else {
                        0
                    }
                };
                let color = gray(level);
                text_width(m, c, 0x15, first);
                let b = block(m);
                m.sw(frame + 0x10, 0x50);
                m.sw(frame + 0x14, 0xf0);
                m.sw(frame + 0x18, color);
                let ot = m.lw(b + 0x40);
                let packets = m.lw(b + 0x3c);
                let next = draw_text(m, c, 0x15, first, ot, packets, [0x50, 0xf0, color]);
                let b = block(m);
                m.sw(b + 0x3c, next);
                text_width(m, c, 0x15, second);
                let b = block(m);
                m.sw(frame + 0x10, 0x5e);
                m.sw(frame + 0x14, 0x114);
                m.sw(frame + 0x18, color);
                let ot = m.lw(b + 0x40);
                let packets = m.lw(b + 0x3c);
                let next = draw_text(m, c, 0x15, second, ot, packets, [0x5e, 0x114, color]);
                let b = block(m);
                m.sw(b + 0x3c, next);
            }
        }
        let d = decoder(m);
        if m.lh(d + pic::SHOWN) == 0 && (m.lw(d + pic::FRAME) as i32) < 0x80 {
            let text = frame + 0x20;
            text_buffer(m, text, 0x800d_5d2c, 25, 0xe7);
            let d = decoder(m);
            let level = 0xffff_ff80u32.wrapping_sub(u32::from(m.lbu(d + pic::FRAME)));
            centered_text(m, c, frame, text, 0x100, gray(level));
        }
    } else {
        let text = frame + 0x20;
        text_buffer(m, text, 0x800d_5d2c, 25, 0xe7);
        let d = decoder(m);
        let time = m.lw(d + pic::FRAME);
        if time.wrapping_sub(0x96) < 0x80 {
            let level = u32::from(m.lbu(d + pic::FRAME)).wrapping_add(0x6a);
            centered_text(m, c, frame, text, 0x100, gray(level));
        } else if (time as i32) >= 0x116 {
            centered_text(m, c, frame, text, 0x100, 0x0080_8080);
        } else {
            centered_text(m, c, frame, text, 0x100, 0);
        }
    }
    let d = decoder(m);
    next_chunk(m, c, d);
    let d = decoder(m);
    let end = m.lbu(d + pic::END);
    let count = m.lw(d + pic::FRAME).wrapping_add(1);
    m.sw(d + pic::FRAME, count);
    m.pop_frame(0x438);
    u32::from(end ^ 1 == 0)
}

/// 800D80A4(decoder, packets, ot): the effect slots like the resident
/// picture layers; effects 0..5 draw [`background`] (timed for 1..5).
/// Returns the next packet (unchanged for other effects).
pub fn layers(m: &mut Machine, d: u32, packets: u32, ot: u32) -> u32 {
    m.push_frame(0x18);
    let current = |m: &mut Machine| {
        let s = u32::from(m.lbu(d + pic::SLOT));
        let bank = u32::from(m.lbu(d + pic::SHOWN_BANK));
        slot(d, bank, s)
    };
    let s = current(m);
    let end = i32::from(m.lh(s + 0x16)).wrapping_add(i32::from(m.lh(s + 0x1e)));
    if m.lw(d + pic::FRAME) == end as u32 {
        let next = m.lbu(d + pic::SLOT);
        m.sb(d + pic::EFFECT, 0);
        m.sw(d + pic::EFFECT_TIME, 0);
        m.sb(d + pic::SLOT, next.wrapping_add(1));
    }
    let s = current(m);
    if m.lw(d + pic::FRAME) == m.lh(s + 0x14) as i32 as u32 {
        let effect = m.lbu(s + pic::SLOTS);
        m.sw(d + pic::EFFECT_TIME, 0);
        m.sb(d + pic::EFFECT, effect);
    }
    let effect = m.lbu(d + pic::EFFECT);
    let result = match effect {
        0 => background(m, d, packets, ot),
        1..=5 => {
            let next = background(m, d, packets, ot);
            let time = m.lw(d + pic::EFFECT_TIME).wrapping_add(1);
            m.sw(d + pic::EFFECT_TIME, time);
            next
        }
        _ => packets,
    };
    m.pop_frame(0x18);
    result
}

/// 800D81CC(decoder, packets, ot): the shown bank's picture as four SPRT
/// (256x256 + 64x256 over two rows) at a brightness fading out (effect 4)
/// or in (5) over the slot, else 0x80; in table entry 2 for the 20th
/// picture, else 4. Returns the next packet.
pub fn background(m: &mut Machine, d: u32, packets: u32, ot: u32) -> u32 {
    let effect = m.lbu(d + pic::EFFECT);
    let mut level: u32 = 0x80;
    if effect == 4 || effect == 5 {
        let s = {
            let sl = u32::from(m.lbu(d + pic::SLOT));
            let bank = u32::from(m.lbu(d + pic::SHOWN_BANK));
            slot(d, bank, sl)
        };
        let span = i32::from(m.lh(s + 0x16)).wrapping_sub(i32::from(m.lh(s + 0x14)));
        let time = m.lw(d + pic::EFFECT_TIME) << 7;
        level = if effect == 4 {
            let q = quotient(m, (time.wrapping_neg()) as i32, span, 0x800d_8224);
            q.wrapping_add(0x80) as u32
        } else {
            quotient(m, time as i32, span, 0x800d_82a4) as u32
        };
        if sext16(level) > 0x80 {
            level = 0x80;
        }
        if sext16(level) < 0 {
            level = 0;
        }
    }
    let b = sext16(level) as u32;
    let color = (b << 16) | (b << 8) | b;
    let mut p = packets;
    let bank = m.lbu(d + pic::SHOWN_BANK);
    for i in 0..4u32 {
        let (w, h, x, y) = match i {
            0 => (0x100u32, 0x100u32, 0u32, 0u32),
            1 => (0x40, 0x100, 0x100, 0),
            2 => (0x100, 0x100, 0, 0x100),
            _ => (0x40, 0x100, 0x100, 0x100),
        };
        let pages = [[0x85, 0x87, 0x95, 0x97], [0x88, 0x8a, 0x98, 0x9a]];
        let (page, clut) = if m.lbu(d + pic::SHOWN_BANK) == 0 {
            (pages[0][i as usize], 0x70)
        } else {
            (pages[1][i as usize], 0xb0)
        };
        let _ = bank;
        m.sw(p + 4, page);
        m.sw(p + 0xc, (y << 16) | (sext16(x) as u32));
        m.sw(p + 0x14, w | ((sext16(h) as u32) << 16));
        m.sw(p + 8, color);
        m.sb(p + 0xb, 0x64);
        m.sw(p + 0x10, clut << 16);
        m.sb(p + 7, 0xe1);
        m.sb(p + 6, 0);
        let table = if m.lh(d + pic::SHOWN) == 0x13 {
            ot + 8
        } else {
            ot + 0x10
        };
        link_or(m, table, p, 5);
        p = p.wrapping_add(0x18);
    }
    p
}

/// A reticle point's centre (`base` + 2i: trail x/y; or the reticle
/// itself) as an xy word, offset by (`dx`, `dy`).
fn centre(m: &mut Machine, c: u32, x: u32, y: u32, dx: i32, dy: i32) -> u32 {
    let w = m.lhu(c + 4);
    let h = m.lhu(c + 6);
    let px = u32::from(m.lhu(x))
        .wrapping_add(half(w))
        .wrapping_add(dx as u32)
        & 0xffff;
    let py = u32::from(m.lhu(y))
        .wrapping_add(half(h))
        .wrapping_add(dy as u32);
    (py << 16) | px
}

/// 800D8530(reticle, ot, packets): the reticle moving from (310,180) to
/// (-32,80) over times 12540..15040 (+0x2E4), a TILE at its position,
/// trail points every 100 frames (and at the key ends), the trail fading
/// (brightness 64 down to 3, spread up to 3) once the move ended (or at
/// elapsed 1050), drawn as gouraud quads from the reticle to the newest
/// point and between points (table entry 3), then a DR_TPAGE. Returns the
/// next packet.
pub fn draw_reticle(m: &mut Machine, c: u32, ot: u32, packets: u32) -> u32 {
    let keys = m.push_frame(0x20);
    m.sw(keys, 0x30fc);
    m.sw(keys + 4, 0x3ac0);
    m.sh(keys + 8, 0x136);
    m.sh(keys + 0xa, 0xffe0);
    m.sh(keys + 0x10, 0xb4);
    m.sh(keys + 0x12, 0x50);
    let mut p = packets;
    let key = m.lw(c + ret::KEY);
    let time = m.lw(c + ret::TIME) as i32;
    let range = keys.wrapping_add(key << 3);
    if time >= m.lw(range) as i32 && time < m.lw(range + 4) as i32 {
        let xs = keys.wrapping_add(8).wrapping_add(key << 2);
        let ys = keys.wrapping_add(0x10).wrapping_add(key << 2);
        let start = m.lhu(range);
        let duration = m.lhu(range + 4).wrapping_sub(start);
        let dx = m.lhu(xs + 2).wrapping_sub(m.lhu(xs));
        let dy = m.lhu(ys + 2).wrapping_sub(m.lhu(ys));
        let elapsed = m.lhu(c + ret::TIME).wrapping_sub(start);
        let t = i32::from(elapsed as i16);
        if t == 0x41a {
            let now = m.lw(c + ret::TIME);
            m.sb(c + ret::FADING, 1);
            m.sw(c + ret::FADE_START, now);
        }
        let span = i32::from(duration as i16);
        let ox = quotient(m, i32::from(dx as i16).wrapping_mul(t), span, 0x800d_8628);
        let oy = quotient(m, i32::from(dy as i16).wrapping_mul(t), span, 0x800d_866c);
        let key = m.lw(c + ret::KEY);
        let x = m.lhu(keys.wrapping_add(8).wrapping_add(key << 2));
        m.sh(c, x.wrapping_add(ox as u16));
        let y = m.lhu(keys.wrapping_add(0x10).wrapping_add(key << 2));
        m.sh(c + 2, y.wrapping_add(oy as u16));
        let color = m.lw(c + 0xc);
        m.sw(p + 4, color);
        let xy = ((m.lh(c + 2) as i32 as u32) << 16) | u32::from(m.lhu(c));
        m.sw(p + 8, xy);
        let wh = ((m.lh(c + 6) as i32 as u32) << 16) | (m.lh(c + 4) as i32 as u32);
        m.sw(p + 0xc, wh);
        p = p.wrapping_add(0x10);
        let rest = t.wrapping_sub(div_magic(t, 5).wrapping_mul(100));
        if rest & 0xffff == 0 && t != 0 {
            add_trail_point(m, c, None);
        }
    }
    let key = m.lw(c + ret::KEY);
    if m.lw(c + ret::TIME) == m.lw(keys.wrapping_add(key << 3)) {
        m.sw(c + ret::COUNT, 0);
        m.sw(c + ret::SHOWN, 1);
        m.sb(c + ret::FADING, 0);
        m.sw(c + ret::FADE_START, 0);
        let key = m.lw(c + ret::KEY);
        let x = m.lhu(keys.wrapping_add(8).wrapping_add(key << 2));
        m.sh(c + ret::TRAIL_X, x);
        let count = m.lw(c + ret::COUNT);
        let key = m.lw(c + ret::KEY);
        let y = m.lhu(keys.wrapping_add(0x10).wrapping_add(key << 2));
        m.sh(c.wrapping_add(count << 1) + ret::TRAIL_Y, y);
        let count = m.lw(c + ret::COUNT);
        m.sb(c.wrapping_add(count) + ret::BRIGHTNESS, 0x40);
        let count = m.lw(c + ret::COUNT);
        m.sh(c.wrapping_add(count << 1) + ret::SPREAD, 1);
    }
    let base = p;
    let key = m.lw(c + ret::KEY);
    if m.lw(c + ret::TIME) == m.lw(keys.wrapping_add(key << 3) + 4) {
        m.sw(c + ret::KEY, key.wrapping_add(1));
        if m.lbu(c + ret::FADING) == 0 {
            let now = m.lw(c + ret::TIME);
            m.sb(c + ret::FADING, 1);
            m.sw(c + ret::FADE_START, now);
        }
        add_trail_point(m, c, None);
    }
    if m.lbu(c + ret::FADING) == 1 {
        let since = m.lw(c + ret::TIME).wrapping_sub(m.lw(c + ret::FADE_START)) as i32;
        let mut age = since;
        let mut i = 0u32;
        while (i as i32) < m.lw(c + ret::SHOWN) as i32 {
            let product = age.wrapping_mul(0x3d);
            let fade = 0x40i32.wrapping_sub(div_magic(product, 6)) as u32;
            let brightness = if sext16(fade) < 3 { 3 } else { fade };
            m.sb(c + i + ret::BRIGHTNESS, brightness as u8);
            if age >= 100 {
                let grown =
                    div_magic(age.wrapping_sub(100).wrapping_mul(2), 5).wrapping_add(1) as u32;
                let spread = if sext16(grown) >= 4 { 3 } else { grown };
                m.sh(c + 2 * i + ret::SPREAD, spread as u16);
            }
            i += 1;
            age = age.wrapping_sub(100);
        }
        let hundredths = div_magic(since, 5);
        if since == hundredths.wrapping_mul(100)
            && since != 0
            && (m.lw(c + ret::COUNT) as i32) >= m.lw(c + ret::SHOWN) as i32
        {
            let shown = m.lw(c + ret::SHOWN).wrapping_add(1);
            m.sw(c + ret::SHOWN, shown);
        }
    }
    let mut p = base;
    let key = m.lw(c + ret::KEY);
    let range = keys.wrapping_add(key << 3);
    let time = m.lw(c + ret::TIME) as i32;
    if time >= m.lw(range) as i32 && m.lw(range + 4) as i32 >= time {
        for side in 0..2 {
            let q = p.wrapping_add(0x20);
            m.sw(q - 0x1c, 0x3a40_4040);
            let count = m.lw(c + ret::COUNT);
            let b = u32::from(m.lbu(c.wrapping_add(count) + ret::BRIGHTNESS));
            m.sw(q - 0x14, gray(b));
            m.sw(q - 0xc, 0x0003_0303);
            m.sw(q - 4, 0x0003_0303);
            let xy0 = centre(m, c, c, c + 2, 0, 0);
            m.sw(q - 0x18, xy0);
            let count = m.lw(c + ret::COUNT);
            let point = c.wrapping_add(count << 1);
            let xy1 = centre(m, c, point + ret::TRAIL_X, point + ret::TRAIL_Y, 0, 0);
            m.sw(q - 0x10, xy1);
            let sign = if side == 0 { 1 } else { -1 };
            let xy2 = centre(m, c, c, c + 2, sign, -sign);
            m.sw(q - 8, xy2);
            let count = m.lw(c + ret::COUNT);
            let point = c.wrapping_add(count << 1);
            let s = i32::from(m.lhu(point + ret::SPREAD));
            let xy3 = centre(
                m,
                c,
                point + ret::TRAIL_X,
                point + ret::TRAIL_Y,
                sign * s,
                -sign * s,
            );
            m.sw(q, xy3);
            link_or(m, ot + 0xc, p, 8);
            p = p.wrapping_add(0x24);
        }
    }
    let mut i = 0u32;
    while (i as i32) < m.lw(c + ret::COUNT) as i32 {
        let newer = c.wrapping_add((i + 1) << 1);
        let older = c.wrapping_add(i << 1);
        for side in 0..2 {
            let q = p.wrapping_add(0x20);
            let b = u32::from(m.lbu(c.wrapping_add(i + 1) + ret::BRIGHTNESS));
            m.sw(q - 0x1c, gray(b) | 0x3a00_0000);
            let b = u32::from(m.lbu(c.wrapping_add(i) + ret::BRIGHTNESS));
            m.sw(q - 0x14, gray(b));
            m.sw(q - 0xc, 0x0003_0303);
            m.sw(q - 4, 0x0003_0303);
            let xy0 = centre(m, c, newer + ret::TRAIL_X, newer + ret::TRAIL_Y, 0, 0);
            m.sw(q - 0x18, xy0);
            let xy1 = centre(m, c, older + ret::TRAIL_X, older + ret::TRAIL_Y, 0, 0);
            m.sw(q - 0x10, xy1);
            let sign = if side == 0 { 1 } else { -1 };
            let s1 = i32::from(m.lhu(newer + ret::SPREAD));
            let xy2 = centre(
                m,
                c,
                newer + ret::TRAIL_X,
                newer + ret::TRAIL_Y,
                sign * s1,
                -sign * s1,
            );
            m.sw(q - 8, xy2);
            let s0 = i32::from(m.lhu(older + ret::SPREAD));
            let xy3 = centre(
                m,
                c,
                older + ret::TRAIL_X,
                older + ret::TRAIL_Y,
                sign * s0,
                -sign * s0,
            );
            m.sw(q, xy3);
            link_or(m, ot + 0xc, p, 8);
            p = p.wrapping_add(0x24);
        }
        i += 1;
    }
    if (m.lw(c + ret::COUNT) as i32) < i32::from(m.lbu(c + ret::FADING)) {
        m.sb(c + ret::FADING, 0);
        m.sw(c + ret::FADE_START, 0);
    }
    m.sb(p + 3, 1);
    m.sw(p + 4, 0xe100_0020);
    link_masked(m, ot + 0xc, p);
    m.pop_frame(0x20);
    p.wrapping_add(8)
}

/// Append the reticle's position as a trail point (brightness 64, spread
/// 1).
fn add_trail_point(m: &mut Machine, c: u32, _unused: Option<()>) {
    let count = m.lw(c + ret::COUNT).wrapping_add(1);
    m.sw(c + ret::COUNT, count);
    let x = m.lhu(c);
    m.sh(c.wrapping_add(count << 1) + ret::TRAIL_X, x);
    let count = m.lw(c + ret::COUNT);
    let y = m.lhu(c + 2);
    m.sh(c.wrapping_add(count << 1) + ret::TRAIL_Y, y);
    let count = m.lw(c + ret::COUNT);
    m.sb(c.wrapping_add(count) + ret::BRIGHTNESS, 0x40);
    let count = m.lw(c + ret::COUNT);
    m.sh(c.wrapping_add(count << 1) + ret::SPREAD, 1);
}

/// 800D90B0(list, ot, packets): the scrolling credit list: every other
/// scroll frame all lines from the first on screen move up 2; the lines
/// from the first with y < 521 are on screen, those above y -211 leave;
/// headings (font 0x15 at x 48; the last one centred) and names (font 0x14
/// at x 64) as text, kinds 2..5 as logo sprites with their DR_TPAGE.
/// Returns the next packet.
pub fn draw_credits(m: &mut Machine, c: &mut Invoker, list: u32, ot: u32, packets: u32) -> u32 {
    let frame = m.push_frame(0x50);
    let mut p = packets;
    let mut first = m.lh(list + cr::FIRST) as i32;
    m.sw(0x800d_ece4, 0x202);
    let step = u32::from(m.lbu(0x800d_ece4));
    if first < cr::LINES as i32 {
        let mut i = first;
        while i < cr::LINES as i32 {
            let time = m.lw(list + cr::TIME) as i32;
            if remainder(m, time, 2, 0x800d_911c) == 0 {
                let at = list.wrapping_add((i as u32) << 2) + cr::Y;
                let y = m.lw(at).wrapping_sub(step);
                m.sw(at, y);
            }
            i += 1;
        }
    }
    first = m.lh(list + cr::FIRST) as i32;
    let mut i = first;
    while i < cr::LINES as i32 {
        if (m.lw(list.wrapping_add((i as u32) << 2) + cr::Y) as i32) >= 0x209 {
            break;
        }
        m.sh(list + cr::LAST, i as u16);
        i += 1;
    }
    let last = m.lh(list + cr::LAST) as i32;
    let mut i = 0i32;
    while i < last {
        if (m.lw(list.wrapping_add((i as u32) << 2) + cr::Y) as i32) >= -0xd3 {
            break;
        }
        m.sh(list + cr::FIRST, (i + 1) as u16);
        i += 1;
    }
    let mut i = m.lh(list + cr::FIRST) as i32;
    if m.lh(list + cr::LAST) as i32 >= i {
        let white = 0x0080_8080u32;
        let final_text = list + 0x33d4;
        let mut line = list.wrapping_add((i as u32) << 2);
        let mut text = (i as u32).wrapping_shl(6).wrapping_add(cr::TEXT);
        loop {
            let kind = m.lw(line);
            match kind {
                0 | 1 => {
                    let (font, source, x, y) = if kind == 1 {
                        (0x14, list.wrapping_add(text), 0x40, m.lw(line + cr::Y))
                    } else if i == 0xc3 {
                        let width = text_width(m, c, 0x15, final_text);
                        let x = 0xa0u32
                            .wrapping_sub(((width as i32 + (width >> 31) as i32) >> 1) as u32);
                        (0x15, final_text, x, m.lw(list + 0x3a74))
                    } else {
                        (0x15, list.wrapping_add(text), 0x30, m.lw(line + cr::Y))
                    };
                    m.sw(frame + 0x10, x);
                    m.sw(frame + 0x18, white);
                    m.sw(frame + 0x14, y);
                    p = draw_text(m, c, font, source, ot, p, [x, y, white]);
                }
                2..=5 => {
                    let y = u32::from(m.lhu(line + cr::Y)) << 16;
                    let (uv, size, x, mode) = match kind {
                        2 => (0x00f0_0000, 0x00cc_0058, 0x74, 0xe100_023e),
                        3 => (0x00f3_0080, 0x0048_0048, 0x7c, 0xe100_023e),
                        4 => (0x00f2_0040, 0x0018_00c0, 0x40, 0xe100_023d),
                        _ => (0x00f3_4880, 0x0020_0080, 0x60, 0xe100_023e),
                    };
                    m.sw(p + 4, 0x6680_8080);
                    m.sw(p + 0xc, uv);
                    m.sw(p + 0x10, size);
                    m.sw(p + 8, y | x);
                    link_or(m, ot, p, 4);
                    p = p.wrapping_add(0x14);
                    m.sb(p + 3, 1);
                    m.sw(p + 4, mode);
                    link_masked(m, ot, p);
                    p = p.wrapping_add(8);
                }
                _ => {}
            }
            line = line.wrapping_add(4);
            i += 1;
            text = text.wrapping_add(0x40);
            if (m.lh(list + cr::LAST) as i32) < i {
                break;
            }
        }
    }
    m.pop_frame(0x50);
    p
}

/// 800D95B0(font, text, ot, packets, x, y, colour): one SPRT per character
/// (glyphs of the font record at 800DE9F4 + 12·font through the character
/// map 800DEB14; newline: next line, CR skipped; fonts 15/16 draw codes
/// 1..6 as 12x20 icons), then the font's DR_TPAGE. Returns the next packet.
#[allow(clippy::too_many_arguments)]
pub fn draw_text(
    m: &mut Machine,
    c: &mut Invoker,
    font: u32,
    text: u32,
    ot: u32,
    packets: u32,
    stack: [u32; 3],
) -> u32 {
    let [x0, y0, color] = stack;
    m.push_frame(0x38);
    let record = FONTS.wrapping_add(font.wrapping_mul(12));
    let length = c(m, STRLEN, [text, 0, 0, 0]) as i32;
    let mut cursor = text;
    let mut x = x0;
    let mut y = y0;
    let mut p = packets;
    let icons = font.wrapping_sub(0xf) < 2;
    let code = color | 0x6600_0000;
    for _ in 0..length.max(0) {
        let ch = u32::from(m.lbu(cursor));
        cursor = cursor.wrapping_add(1);
        if ch == 10 {
            x = x0;
            y = y.wrapping_add(1).wrapping_add(u32::from(m.lbu(record + 3)));
            continue;
        }
        if ch == 13 {
            continue;
        }
        let index = u32::from(m.lbu(CHARACTER_MAP.wrapping_add(ch)));
        let glyph = m.lw(record + 8).wrapping_add(index << 2);
        let u = u32::from(m.lbu(record)).wrapping_add(u32::from(m.lbu(glyph)));
        let v = u32::from(m.lbu(record + 1)).wrapping_add(u32::from(m.lbu(glyph + 1)));
        let width = u32::from(m.lbu(glyph + 2));
        let offset = m.lb(glyph + 3) as i32 as u32;
        m.sw(p + 4, code);
        let clut = u32::from(m.lhu(record + 6)) << 16;
        m.sw(p + 0xc, u | (v << 8) | clut);
        let size = if icons && ch.wrapping_sub(1) < 6 {
            0x0014_000c
        } else {
            width | (u32::from(m.lbu(record + 3)) << 16)
        };
        m.sw(p + 0x10, size);
        m.sh(p + 8, x as u16);
        m.sh(p + 0xa, y.wrapping_add(offset) as u16);
        x = x.wrapping_add(1).wrapping_add(width);
        link_or(m, ot, p, 4);
        p = p.wrapping_add(0x14);
    }
    m.sb(p + 3, 1);
    let page = u32::from(m.lhu(record + 4)) & 0x9ff;
    m.sw(p + 4, page | 0xe100_0000);
    link_masked(m, ot, p);
    m.pop_frame(0x38);
    p.wrapping_add(8)
}

/// 800D98CC(font, text): the width of `text` (glyph widths +2 of a stack
/// copy of 800D5D64 through the copy of the map 800D6314; fonts other than
/// 0x14/0x15 read the glyph table at address 0).
pub fn text_width(m: &mut Machine, c: &mut Invoker, font: u32, text: u32) -> u32 {
    let frame = m.push_frame(0x6d0);
    let glyphs = frame + 0x10;
    for i in 0..0x5b0 / 4 {
        let word = m.lw(WIDTH_GLYPHS + 4 * i);
        m.sw(glyphs + 4 * i, word);
    }
    let length = c(m, STRLEN, [text, 0, 0, 0]) as i32;
    let map = frame + 0x5c0;
    for i in 0..0x100 / 4 {
        let word = m.lw(WIDTH_MAP + 4 * i);
        m.sw(map + 4 * i, word);
    }
    let table = if font == 0x14 || font == 0x15 {
        glyphs
    } else {
        0
    };
    let mut width = 0u32;
    let mut cursor = text;
    for _ in 0..length.max(0) {
        let ch = u32::from(m.lbu(cursor));
        let index = u32::from(m.lbu(map.wrapping_add(ch)));
        width = width.wrapping_add(u32::from(m.lbu(table.wrapping_add(index << 2) + 2)));
        cursor = cursor.wrapping_add(1);
    }
    m.pop_frame(0x6d0);
    width
}

/// 800D97C8(index, table) (no caller): language-indexed text table
/// (context +0x38C): `table + off[lang]`, then its entry `index`.
pub fn text_entry(m: &mut Machine, index: u32, table: u32) -> u32 {
    let context = m.gw(CONTEXT);
    let lang = m.lw(context + 0x38c);
    let section = table.wrapping_add(m.lw(table.wrapping_add(lang << 2) + 4) & !3);
    section.wrapping_add(m.lw(section.wrapping_add(index << 2) + 4) & !3)
}

/// 800D9814(first, second, table) (no caller): two levels of
/// [`text_entry`].
pub fn text_entry2(m: &mut Machine, first: u32, second: u32, table: u32) -> u32 {
    let entry = text_entry(m, first, table);
    entry.wrapping_add(m.lw(entry.wrapping_add(second << 2) + 4) & !3)
}

/// 800D9878(index, table) (no caller): the word at [`text_entry`].
pub fn text_word(m: &mut Machine, index: u32, table: u32) -> u32 {
    let entry = text_entry(m, index, table);
    m.lw(entry)
}

/// Native targets of module 3 (only while it is loaded).
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if !(0x800c_9dc0..0x800d_ece8).contains(&target) || common::loaded_module(m) != MODULE {
        return None;
    }
    let [a0, a1, a2, a3] = args;
    let mut call = dispatch::call;
    let c: &mut Invoker = &mut call;
    Some(match target {
        0x800d_6414 => {
            reset_reticle(m, a0);
            0
        }
        0x800d_6488 => {
            load_credits(m, c, a0);
            0
        }
        0x800d_653c => {
            upload_images(m);
            0
        }
        0x800d_67d4 => start(m, c),
        0x800d_69ec => player(m, c),
        0x800d_6cb4 => {
            let sp = m.sp;
            let layout = std::array::from_fn(|i| m.lw(sp.wrapping_add(0x10 + 4 * i as u32)));
            states::picture_decoder_init(m, c, a0, a1, a2, a3, layout)
        }
        0x800d_6d58 => {
            states::picture_decoder_setup(m, a0);
            0
        }
        0x800d_6eb8 => {
            stream_start(m, c);
            0
        }
        0x800d_6ef8 => {
            states::picture_stream_done(m);
            0
        }
        0x800d_6f0c => states::picture_stop(m, c),
        0x800d_6f30 => states::picture_bank_zero(m),
        0x800d_6f48 => wait_frame(m, c, a0),
        0x800d_6ff4 => next_chunk(m, c, a0),
        0x800d_77b4 => credits_frame(m, c),
        0x800d_80a4 => layers(m, a0, a1, a2),
        0x800d_81cc => background(m, a0, a1, a2),
        0x800d_8530 => draw_reticle(m, a0, a1, a2),
        0x800d_90b0 => draw_credits(m, c, a0, a1, a2),
        0x800d_94a0 => frame(m, c),
        0x800d_9558 => finish(m, c),
        0x800d_95b0 => {
            let sp = m.sp;
            let stack = std::array::from_fn(|i| m.lw(sp.wrapping_add(0x10 + 4 * i as u32)));
            draw_text(m, c, a0, a1, a2, a3, stack)
        }
        0x800d_97c8 => text_entry(m, a0, a1),
        0x800d_9814 => text_entry2(m, a0, a1, a2),
        0x800d_9878 => text_word(m, a0, a1),
        0x800d_98cc => text_width(m, c, a0, a1),
        _ => return None,
    })
}
