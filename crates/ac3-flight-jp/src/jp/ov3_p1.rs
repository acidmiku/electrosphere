//! JP overlay 3, part 1: the ending staff credits (state 7), the routines
//! 8011B328..8011E853 (20 functions, `jpwave/ov3-part1.tsv`), translated
//! from the JP instructions.
//!
//! Module 3 is BPB entry 3 (338,656 bytes at 0x800CBFE8: images, the 339
//! credit records at 0x80119FCC, code from 0x8011B328, the state's callback
//! table 0x8011E854). The resident movie state 5 enters state 7 after the
//! ending movies, or for movie 0x1E (`res_p1_states::movie_setup` and
//! `movie_frame`); state 7 returns to context +0x3A0.
//!
//! Over a streamed picture sequence (movie table entry 0x1E's file, 40x240
//! texture tiles) the credits draw the title, the "Produced by" and
//! copyright pictures, a moving reticle with a fading trail, and the
//! scrolling list of credit pictures (one sprite per record: no text is
//! rendered in JP, every line is an image of the overlay).
//!
//! | State 7 substate | Callback | Rust |
//! | ---: | --- | --- |
//! | 0 | `8011BB7C` | [`start`] |
//! | 1 | `8011E744` | [`frame`] |
//!
//! Files: this one (helpers, the callbacks, the credit list and the image
//! uploads), [`super::ov3_p1_player`] (the picture player and the stream
//! chunks) and [`super::ov3_p1_draw`] (the credits frame, the background
//! layers, the reticle and the list). Each file's `dispatch` answers only
//! while module 3 is loaded.
//!
//! Memory helpers take addresses computed with wrapping arithmetic, as the
//! CPU does; halfword and byte loads are extended as the instruction does.
use super::ov3_p1_player as player;
use crate::common::{CONTEXT, loaded_module};
use crate::dispatch;
use crate::machine::{Fault, Machine, SmallData};

/// The module number (context +0x38 while it is loaded).
pub const MODULE: u32 = 3;

/// `[gp+0x2D4]` (0x800BCE48): the display request block.
pub const DISPLAY: u32 = 0x800b_ce48;
/// `[gp+0x4DC]` (0x800BD050): the credits block (0x1894 bytes): +0 player
/// phase, +4/+8 two 0x2000-byte packet areas, +0xC/+0x24 two 6-entry
/// ordering tables, +0x3C next packet, +0x40 current table, +0x44..+0x52
/// the decoder layout, +0x54/+0x56 320x240, +0x58 stream file, +0x5C mode,
/// +0x60/+0x64 saved VRAM, +0x68 flag, +0x6C the reticle, +0x350 time,
/// +0x354 the credit list, +0x1890 the frame counter at the start.
pub const BLOCK: u32 = 0x800b_d050;
/// `[gp+0x4E0]` (0x800BD054): the picture decoder block (+0 step, +1
/// layered, +4 frame, +8 pictures shown, +0xA effect, +0xC effect time,
/// +0x10 slot, +0x12 + 0x46 bank + 0xE slot: the effect slots, +0x9E bank,
/// +0x9F shown bank, +0xA0 end, +0xA1 mode, +0xA2 + 0x14 bank: tile
/// flags, +0xCC file, +0xD0 ring size, +0xD4 ring).
pub const DECODER: u32 = 0x800b_d054;
/// `[gp+0x220]` (0x800BCD94): the movie table (entry 0x1E's file at +0xF0).
pub const MOVIE_TABLE: u32 = 0x800b_cd94;

/// Resident and SDK routines (JP addresses).
pub const ALLOC: u32 = 0x8002_7410;
pub const ARENA_PEEK: u32 = 0x8002_7478;
pub const RESET_CD_QUEUE: u32 = 0x8002_c6a8;
pub const QUEUE_STREAM: u32 = 0x8002_c928;
pub const PROCESS_CD_QUEUE: u32 = 0x8002_c4dc;
pub const NEXT_CHUNK: u32 = 0x8002_b9ec;
pub const RELEASE_CHUNK: u32 = 0x8002_bbe0;
pub const VSYNC: u32 = 0x800a_2098;
pub const DRAW_SYNC: u32 = 0x800a_8194;
pub const CLEAR_IMAGE: u32 = 0x800a_8318;
pub const LOAD_IMAGE: u32 = 0x800a_8440;
pub const STORE_IMAGE: u32 = 0x800a_84a0;
pub const CLEAR_OTAG_R: u32 = 0x800a_8680;
pub const DRAW_OTAG: u32 = 0x800a_8788;

/// NEXT_SUBSTATE, and RESET_ARENA | SET_STATE (`lui 0x6000`).
const NEXT_SUBSTATE: u32 = 0x0400_0000;
const RESET_AND_SET_STATE: u32 = 0x6000_0000;

// ---- helpers ----------------------------------------------------------------------

/// A call by JP address (the resident, or another routine of the module).
pub(crate) fn call(m: &mut Machine, target: u32, args: [u32; 4]) -> u32 {
    dispatch::call(m, target, args)
}
/// Run `body` inside the original stack frame of `size` bytes (its base is
/// the routine's `sp`), so callee frames and stack arguments sit at the
/// original addresses.
pub(crate) fn framed<R>(
    m: &mut Machine,
    size: u32,
    body: impl FnOnce(&mut Machine, u32) -> R,
) -> R {
    let sp = m.push_frame(size);
    let result = body(m, sp);
    m.pop_frame(size);
    result
}
/// `lh` (sign-extended).
pub(crate) fn lh(m: &mut Machine, address: u32) -> u32 {
    m.lh(address) as i32 as u32
}
/// `lhu`.
pub(crate) fn lhu(m: &mut Machine, address: u32) -> u32 {
    u32::from(m.lhu(address))
}
/// `lbu`.
pub(crate) fn lbu(m: &mut Machine, address: u32) -> u32 {
    u32::from(m.lbu(address))
}
/// `sh` (the low half of `value`).
pub(crate) fn sh(m: &mut Machine, address: u32, value: u32) {
    m.sh(address, value as u16);
}
/// `sb` (the low byte of `value`).
pub(crate) fn sb(m: &mut Machine, address: u32, value: u32) {
    m.sb(address, value as u8);
}
/// `sll 16; sra 16`.
pub(crate) fn sx16(value: u32) -> u32 {
    value as u16 as i16 as u32
}
/// `sra`.
pub(crate) fn sra(value: u32, shift: u32) -> u32 {
    ((value as i32) >> shift) as u32
}
/// `slt`.
pub(crate) fn slt(a: u32, b: u32) -> bool {
    (a as i32) < (b as i32)
}
/// `mult a, b; mflo`.
pub(crate) fn lo_s(a: u32, b: u32) -> u32 {
    (i64::from(a as i32) * i64::from(b as i32)) as u32
}
/// `mult a, b; mfhi`.
pub(crate) fn hi_s(a: u32, b: u32) -> u32 {
    ((i64::from(a as i32) * i64::from(b as i32)) >> 32) as u32
}
/// `div n, d` behind the compiler's checks: `break 7` at `zero` for a zero
/// divisor and `break 6` at `overflow` for `0x80000000 / -1`. A reached
/// trap records [`Fault::Trap`]. Returns (LO, HI) as the CPU leaves them.
pub(crate) fn div(m: &mut Machine, n: u32, d: u32, zero: u32, overflow: u32) -> (u32, u32) {
    let (a, b) = (n as i32, d as i32);
    if b == 0 {
        m.raise(Fault::Trap {
            address: zero,
            code: 7,
        });
        return (if a < 0 { 1 } else { u32::MAX }, n);
    }
    if a == i32::MIN && b == -1 {
        m.raise(Fault::Trap {
            address: overflow,
            code: 6,
        });
        return (n, 0);
    }
    (a.wrapping_div(b) as u32, a.wrapping_rem(b) as u32)
}
/// The libgpu `addPrim` sequence: the packet's link first (its own length
/// byte kept), then the table word (its top byte kept), each re-read.
pub(crate) fn add_prim(m: &mut Machine, ot: u32, packet: u32) {
    let link = (m.lw(packet) & 0xff00_0000) | (m.lw(ot) & 0x00ff_ffff);
    m.sw(packet, link);
    let head = (m.lw(ot) & 0xff00_0000) | (packet & 0x00ff_ffff);
    m.sw(ot, head);
}
/// `lwl/lwr` + `swl/swr` (and `lh`/`sh`) copies: the bytes, whatever the
/// alignment.
pub(crate) fn copy_bytes(m: &mut Machine, dst: u32, src: u32, bytes: u32) {
    for i in 0..bytes {
        let byte = m.lbu(src.wrapping_add(i));
        m.sb(dst.wrapping_add(i), byte);
    }
}
/// The runtime context (`[gp+0x2EC]`).
pub(crate) fn ctx(m: &mut Machine) -> u32 {
    m.gw(CONTEXT)
}
/// `[BLOCK]`.
pub(crate) fn block(m: &mut Machine) -> u32 {
    m.lw(BLOCK)
}
/// `[DECODER]`.
pub(crate) fn decoder(m: &mut Machine) -> u32 {
    m.lw(DECODER)
}

// ---- the reticle and the credit list ------------------------------------------------

/// 8011B328 (JP; PAL 800D6414, exact): reset the reticle record `c`
/// (block +0x6C): 16x24 at 0,0, colour 0x60808080, key 0, no trail (the
/// 100 spreads and brightnesses cleared), time 0. Returns 0.
pub fn reset_reticle(m: &mut Machine, c: u32) -> u32 {
    sh(m, c.wrapping_add(4), 0x10);
    sh(m, c.wrapping_add(6), 0x18);
    for offset in [0xc, 0xd, 0xe] {
        sb(m, c.wrapping_add(offset), 0x80);
    }
    sh(m, c, 0);
    sh(m, c.wrapping_add(2), 0);
    sb(m, c.wrapping_add(0xf), 0x60);
    m.sw(c.wrapping_add(0x10), 0);
    m.sw(c.wrapping_add(0x2d0), 0);
    m.sw(c.wrapping_add(0x2d4), 0);
    sb(m, c.wrapping_add(0x2d8), 0);
    m.sw(c.wrapping_add(0x2dc), 0);
    m.sw(c.wrapping_add(0x2e0), 0);
    for i in 0..100u32 {
        sh(m, c.wrapping_add(2 * i).wrapping_add(0x1a4), 0);
        sb(m, c.wrapping_add(i).wrapping_add(0x26c), 0);
    }
    m.sw(c.wrapping_add(0x2e4), 0);
    0
}

/// 0x80119FCC: the 339 credit records (14 bytes: +0 x, +2 y, +4 w, +6 h,
/// +8 the tpage case, +0xA u, +0xC v).
const CREDIT_RECORDS: u32 = 0x8011_9fcc;
/// 0x8011B258 / 0x8011B274: two 26-byte tables the routine copies into its
/// frame and does not use.
const UNUSED_TABLES: [(u32, u32); 2] = [(0x8011_b258, 0x1290), (0x8011_b274, 0x12b0)];
/// 0x8011E8F4 / 0x8011E8F8: the CLUT position (x, y) of the credit images.
const CLUT_X: u32 = 0x8011_e8f4;
const CLUT_Y: u32 = 0x8011_e8f8;
/// The tpage codes of the record's case 0..12 (the switch table 0x8011B294).
const TPAGES: [u32; 13] = [
    0x27, 0x2a, 0x2b, 0x2c, 0x3b, 0x37, 0x3a, 0x2d, 0x2e, 0x3c, 0x2f, 0x3d, 0x3e,
];

/// 8011B39C (JP, new): load the credit list `list` (block +0x354) from the
/// 339 records (copied into the frame first): +4i x, +0x54C + 4i y + 680,
/// +0xA98 + 2i w, +0xD3E + 2i h, +0xFE4 + i u, +0x1137 + i v, +0x128A + 2i
/// the tpage code of case +8 (cases 13 and up keep the old code), +0x1530
/// the CLUT word; nothing on screen (+0x1532 first, +0x1534 last), scroll
/// time +0x1538 0. Returns 0.
pub fn load_credits(m: &mut Machine, list: u32) -> u32 {
    framed(m, 0x12d0, |m, sp| {
        // the aligned branch of the copy (the source is a constant, word
        // aligned), then the 10-byte tail and the two unused tables
        for i in (0..0x1280u32).step_by(4) {
            let word = m.lw(CREDIT_RECORDS + i);
            m.sw(sp.wrapping_add(i), word);
        }
        copy_bytes(m, sp.wrapping_add(0x1280), CREDIT_RECORDS + 0x1280, 10);
        for (source, at) in UNUSED_TABLES {
            copy_bytes(m, sp.wrapping_add(at), source, 0x1a);
        }
        sh(m, list.wrapping_add(0x1532), 0);
        sh(m, list.wrapping_add(0x1534), 0);
        for i in 0..0x153u32 {
            let record = sp.wrapping_add(14 * i);
            let words = list.wrapping_add(4 * i);
            let halves = list.wrapping_add(2 * i);
            let bytes = list.wrapping_add(i);
            let x = lh(m, record);
            m.sw(words, x);
            let y = lh(m, record.wrapping_add(2)).wrapping_add(0x2a8);
            m.sw(words.wrapping_add(0x54c), y);
            let w = lhu(m, record.wrapping_add(4));
            sh(m, halves.wrapping_add(0xa98), w);
            let h = lhu(m, record.wrapping_add(6));
            sh(m, halves.wrapping_add(0xd3e), h);
            let u = lbu(m, record.wrapping_add(0xa));
            sb(m, bytes.wrapping_add(0xfe4), u);
            let v = lbu(m, record.wrapping_add(0xc));
            sb(m, bytes.wrapping_add(0x1137), v);
            let clut_y = lhu(m, CLUT_Y);
            let clut_x = m.lw(CLUT_X);
            sh(
                m,
                list.wrapping_add(0x1530),
                (clut_y << 6) | (sra(clut_x, 4) & 0x3f),
            );
            let case = lh(m, record.wrapping_add(8));
            if let Some(&code) = TPAGES.get(case as usize) {
                sh(m, halves.wrapping_add(0x128a), code);
            }
        }
        m.sw(list.wrapping_add(0x1538), 0);
        0
    })
}

/// 0x8011E99C: the upload rectangles (19 RECTs).
const UPLOAD_RECTS: u32 = 0x8011_e99c;
/// The images 8011B690 uploads: (rectangle offset, the words whose low
/// halves give x, y, w, h, the pixels). The data words are 0x8011E85C..
/// (each a 16-byte w, h, x, y record; the record at 0x8011E8CC is unused).
const UPLOADS: [(u32, [u32; 4], u32); 18] = [
    (
        0x00,
        [0x8011_e864, 0x8011_e868, 0x8011_e85c, 0x8011_e860],
        0x800c_bfec,
    ),
    (
        0x08,
        [0x8011_e874, 0x8011_e878, 0x8011_e86c, 0x8011_e870],
        0x800c_fbec,
    ),
    (
        0x10,
        [0x8011_e884, 0x8011_e888, 0x8011_e87c, 0x8011_e880],
        0x800d_37ec,
    ),
    (
        0x18,
        [0x8011_e894, 0x8011_e898, 0x8011_e88c, 0x8011_e890],
        0x800d_73ec,
    ),
    (
        0x20,
        [0x8011_e8a4, 0x8011_e8a8, 0x8011_e89c, 0x8011_e8a0],
        0x800d_afec,
    ),
    (
        0x28,
        [0x8011_e8b4, 0x8011_e8b8, 0x8011_e8ac, 0x8011_e8b0],
        0x800e_27ec,
    ),
    (
        0x30,
        [0x8011_e8c4, 0x8011_e8c8, 0x8011_e8bc, 0x8011_e8c0],
        0x800e_a00c,
    ),
    (
        0x38,
        [0x8011_e8dc, 0x8011_e8e0, 0x8011_e8d4, 0x8011_e8d8],
        0x800f_180c,
    ),
    (
        0x40,
        [0x8011_e8ec, 0x8011_e8f0, 0x8011_e8e4, 0x8011_e8e8],
        0x800f_902c,
    ),
    (
        0x48,
        [0x8011_e904, 0x8011_e908, 0x8011_e8fc, 0x8011_e900],
        0x8010_082c,
    ),
    (
        0x50,
        [0x8011_e914, 0x8011_e918, 0x8011_e90c, 0x8011_e910],
        0x8010_802c,
    ),
    (
        0x58,
        [0x8011_e924, 0x8011_e928, 0x8011_e91c, 0x8011_e920],
        0x8010_f82c,
    ),
    (
        0x60,
        [0x8011_e934, 0x8011_e938, 0x8011_e92c, 0x8011_e930],
        0x8011_702c,
    ),
    (
        0x68,
        [0x8011_e944, 0x8011_e948, 0x8011_e93c, 0x8011_e940],
        0x8011_7a4c,
    ),
    (
        0x70,
        [0x8011_e954, 0x8011_e958, 0x8011_e94c, 0x8011_e950],
        0x8011_81cc,
    ),
    (
        0x78,
        [0x8011_e964, 0x8011_e968, 0x8011_e95c, 0x8011_e960],
        0x8011_89cc,
    ),
    (
        0x80,
        [0x8011_e974, 0x8011_e978, 0x8011_e96c, 0x8011_e970],
        0x8011_91cc,
    ),
    (
        0x88,
        [0x8011_e984, 0x8011_e988, 0x8011_e97c, 0x8011_e980],
        0x8011_95cc,
    ),
];
/// The CLUT of the credit images (16x1 at CLUT_X, CLUT_Y).
const CLUT_PIXELS: u32 = 0x800f_900c;

/// 8011B690 (JP, new): upload the credits' images: LoadImage of 18 overlay
/// images (12, DrawSync, 6) and the 16x1 CLUT, then DrawSync. Returns
/// DrawSync's result.
pub fn upload_images(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        for (index, &(offset, [x, y, w, h], pixels)) in UPLOADS.iter().enumerate() {
            if index == 12 {
                call(m, DRAW_SYNC, [0; 4]);
            }
            let rect = UPLOAD_RECTS + offset;
            let (x, y, w, h) = (lhu(m, x), lhu(m, y), lhu(m, w), lhu(m, h));
            sh(m, rect, x);
            sh(m, rect + 2, y);
            sh(m, rect + 4, w);
            sh(m, rect + 6, h);
            call(m, LOAD_IMAGE, [rect, pixels, 0, 0]);
        }
        let rect = UPLOAD_RECTS + 0x90;
        sh(m, rect + 4, 0x10);
        let x = lhu(m, CLUT_X);
        let y = lhu(m, CLUT_Y);
        sh(m, rect + 6, 1);
        sh(m, rect, x);
        sh(m, rect + 2, y);
        call(m, LOAD_IMAGE, [rect, CLUT_PIXELS, 0, 0]);
        call(m, DRAW_SYNC, [0; 4])
    })
}

// ---- state 7 callbacks ------------------------------------------------------------

/// 0x8011B2C8: the rectangle of the first saved VRAM area (320,0 160x512).
const SAVED_RECT: u32 = 0x8011_b2c8;

/// 8011BB7C (JP; PAL 800D67D4, near: block 0x1894 bytes, packet areas
/// 0x2000, movie entry +0xF0; state 7, substate 0): on buffer 1, display
/// phase 1 (requests 1|8, colour 0, interval 0), context bit 0x10000000;
/// the block (two packet areas, 320x240, the file of movie table entry
/// 0x1E); the VRAM areas 0x8011B2C8 (into 0x28000 bytes) and 480,256 96x256
/// (into 0xC000) saved; both 480x240 screens cleared; the images uploaded;
/// the reticle and the credit list set up. Returns NEXT_SUBSTATE (0 on
/// buffer 0).
pub fn start(m: &mut Machine) -> u32 {
    framed(m, 0x40, |m, sp| {
        let context = ctx(m);
        if m.lw(context.wrapping_add(4)) == 0 {
            return 0;
        }
        let display = m.lw(DISPLAY);
        sb(m, display.wrapping_add(0x10), 0);
        let display2 = m.lw(DISPLAY);
        m.sw(display.wrapping_add(4), 1);
        m.sw(display.wrapping_add(0x14), 0);
        sb(m, display2.wrapping_add(0x11), 0);
        let display = m.lw(DISPLAY);
        sb(m, display.wrapping_add(0x12), 0);
        let display = m.lw(DISPLAY);
        let flags = m.lw(display) | 9;
        let context = ctx(m);
        m.sw(display, flags);
        let word = m.lw(context) | 0x1000_0000;
        m.sw(context, word);
        let b = call(m, ALLOC, [0x1894, 0, 0, 0]);
        m.sw(BLOCK, b);
        let first = call(m, ALLOC, [0x2000, 0, 0, 0]);
        let b = block(m);
        m.sw(b.wrapping_add(4), first);
        let second = call(m, ALLOC, [0x2000, 0, 0, 0]);
        let b = block(m);
        m.sw(b.wrapping_add(8), second);
        sh(m, b.wrapping_add(0x54), 0x140);
        let table = m.lw(MOVIE_TABLE);
        m.sw(b, 0);
        sh(m, b.wrapping_add(0x56), 0xf0);
        let file = lh(m, table.wrapping_add(0xf0));
        m.sw(b.wrapping_add(0x5c), 0);
        m.sw(b.wrapping_add(0x58), file);
        let rect = sp.wrapping_add(0x10);
        copy_bytes(m, rect, SAVED_RECT, 8);
        let pixels = call(m, ALLOC, [0x2_8000, 0, 0, 0]);
        let b = block(m);
        m.sw(b.wrapping_add(0x60), pixels);
        call(m, STORE_IMAGE, [rect, pixels, 0, 0]);
        sh(m, rect, 0x1e0);
        sh(m, rect.wrapping_add(2), 0x100);
        sh(m, rect.wrapping_add(4), 0x60);
        sh(m, rect.wrapping_add(6), 0x100);
        let pixels = call(m, ALLOC, [0xc000, 0, 0, 0]);
        let b = block(m);
        m.sw(b.wrapping_add(0x64), pixels);
        call(m, STORE_IMAGE, [rect, pixels, 0, 0]);
        let clear = sp.wrapping_add(0x18);
        sh(m, clear, 0);
        sh(m, clear.wrapping_add(2), 0);
        sh(m, clear.wrapping_add(4), 0x1e0);
        sh(m, clear.wrapping_add(6), 0xf0);
        call(m, CLEAR_IMAGE, [clear, 0, 0, 0]);
        sh(m, clear, 0);
        sh(m, clear.wrapping_add(2), 0x100);
        call(m, CLEAR_IMAGE, [clear, 0, 0, 0]);
        call(m, DRAW_SYNC, [0; 4]);
        upload_images(m);
        let context = ctx(m);
        let b = block(m);
        let time = m.lw(context.wrapping_add(0x24));
        m.sw(b.wrapping_add(0x68), 1);
        m.sw(b.wrapping_add(0x1890), time);
        reset_reticle(m, b.wrapping_add(0x6c));
        let b = block(m);
        load_credits(m, b.wrapping_add(0x354));
        NEXT_SUBSTATE
    })
}

/// 8011E744 (JP; PAL 800D94A0, near: returns to context +0x3A0, PAL
/// +0x3A4; state 7, substate 1): one frame: the buffer's 6-entry table
/// (`ClearOTagR`) and packet area, the player
/// ([`player::player`]); when it ends, [`finish`] and SET_STATE to context
/// +0x3A0 with an arena reset, otherwise `DrawOTag(table + 0x14)` and 0.
pub fn frame(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        let context = ctx(m);
        let buffer = m.lw(context.wrapping_add(4));
        let b = block(m);
        let ot = b.wrapping_add(buffer.wrapping_mul(0x18).wrapping_add(0xc));
        m.sw(b.wrapping_add(0x40), ot);
        let packets = m.lw(b.wrapping_add(buffer << 2).wrapping_add(4));
        m.sw(b.wrapping_add(0x3c), packets);
        call(m, CLEAR_OTAG_R, [ot, 6, 0, 0]);
        if player::player(m) == 0 {
            let b = block(m);
            let ot = m.lw(b.wrapping_add(0x40));
            call(m, DRAW_OTAG, [ot.wrapping_add(0x14), 0, 0, 0]);
            0
        } else {
            finish(m);
            let context = ctx(m);
            m.lw(context.wrapping_add(0x3a0)) | RESET_AND_SET_STATE
        }
    })
}

/// 8011E7FC (JP; PAL 800D9558, exact): the end: reset the CD queue and
/// restore the two saved VRAM areas (rectangles 0x8011E98C / 0x8011E994).
/// Returns 1.
pub fn finish(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        call(m, RESET_CD_QUEUE, [0, 0, 0, 0]);
        let b = block(m);
        let pixels = m.lw(b.wrapping_add(0x60));
        call(m, LOAD_IMAGE, [0x8011_e98c, pixels, 0, 0]);
        let b = block(m);
        let pixels = m.lw(b.wrapping_add(0x64));
        call(m, LOAD_IMAGE, [0x8011_e994, pixels, 0, 0]);
        1
    })
}

/// The routines of this file by JP address, while module 3 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    let [a0, ..] = args;
    Some(match target {
        0x8011_b328 => reset_reticle(m, a0),
        0x8011_b39c => load_credits(m, a0),
        0x8011_b690 => upload_images(m),
        0x8011_bb7c => start(m),
        0x8011_e744 => frame(m),
        0x8011_e7fc => finish(m),
        _ => return None,
    })
}
