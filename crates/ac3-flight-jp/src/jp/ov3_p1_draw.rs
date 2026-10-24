//! JP overlay 3, part 1: the credits' drawing (8011CB5C..8011E743): one
//! credits frame, the background layers of the decoded pictures, the
//! reticle with its trail and the scrolling list of credit pictures,
//! translated from the JP instructions.
//!
//! The credit list (block +0x354, [`super::ov3_p1::load_credits`]) holds
//! per record x (+4i), y (+0x54C + 4i), w (+0xA98 + 2i), h (+0xD3E + 2i), u
//! (+0xFE4 + i), v (+0x1137 + i) and the tpage code (+0x128A + 2i). Records
//! 0..0x151 scroll; records 0x150..0x152 are also the "Produced by" and
//! copyright pictures of the frame before the list.
use super::ov3_p1::{
    MODULE, add_prim, block, ctx, decoder, div, framed, hi_s, lbu, lh, lhu, lo_s, sb, sh, slt, sra,
    sx16,
};
use super::ov3_p1_player::next_chunk;
use crate::common::loaded_module;
use crate::machine::Machine;

/// `(b << 16) | (b << 8) | b` of the low byte of `level`.
fn gray(level: u32) -> u32 {
    let b = level & 0xff;
    (b << 16) | (b << 8) | b
}
/// The effect slot `slot` of bank `bank` of decoder `d`.
fn slot_at(d: u32, bank: u32, slot: u32) -> u32 {
    d.wrapping_add(bank.wrapping_mul(0x46))
        .wrapping_add(slot.wrapping_mul(0xe))
}
/// The decoder's current slot: `+0x10` of the shown bank `+0x9F`.
fn current_slot(m: &mut Machine, d: u32) -> u32 {
    let slot = lbu(m, d.wrapping_add(0x10));
    let bank = lbu(m, d.wrapping_add(0x9f));
    slot_at(d, bank, slot)
}
/// Link a 4-word packet (SPRT) at `packet` into `[ot]` (`[[block]+0x40]`,
/// re-read): `[ot] | 0x04000000`, then the table gets the packet.
fn link_sprite(m: &mut Machine, b: u32, packet: u32) {
    let ot = m.lw(b.wrapping_add(0x40));
    let tag = m.lw(ot) | 0x0400_0000;
    m.sw(packet, tag);
    let ot = m.lw(b.wrapping_add(0x40));
    m.sw(ot, (packet << 8) >> 8);
}
/// A DR_TPAGE at `packet` (+3 length 1, +4 `code`) linked into
/// `[[BLOCK]+0x40]` (`addPrim`, the block read first, the table pointer
/// re-read for each half). Returns the block pointer it read.
fn tpage(m: &mut Machine, packet: u32, code: u32) -> u32 {
    sb(m, packet.wrapping_add(3), 1);
    let b = block(m);
    m.sw(packet.wrapping_add(4), code);
    let ot = m.lw(b.wrapping_add(0x40));
    let word = (m.lw(packet) & 0xff00_0000) | (m.lw(ot) & 0x00ff_ffff);
    m.sw(packet, word);
    let ot = m.lw(b.wrapping_add(0x40));
    let head = (m.lw(ot) & 0xff00_0000) | (packet & 0x00ff_ffff);
    m.sw(ot, head);
    b
}

/// The fields of credit record `i` of the block `b` (`b + 0x354` is the
/// list): x, y, w, h, u, v, tpage code (halfwords and bytes as loaded).
fn picture(m: &mut Machine, b: u32, i: u32) -> [u32; 7] {
    let list = b.wrapping_add(0x354);
    [
        lhu(m, list.wrapping_add(4 * i)),
        lhu(m, list.wrapping_add(0x54c + 4 * i)),
        lhu(m, list.wrapping_add(0xa98 + 2 * i)),
        lhu(m, list.wrapping_add(0xd3e + 2 * i)),
        lbu(m, list.wrapping_add(0xfe4 + i)),
        lbu(m, list.wrapping_add(0x1137 + i)),
        lhu(m, list.wrapping_add(0x128a + 2 * i)),
    ]
}
/// The SPRT words of a credit picture at `packet` (+8 xy, +0xC uv and
/// CLUT 0x7814, +0x10 wh) and its colour code word +4.
fn sprite_words(m: &mut Machine, packet: u32, [x, y, w, h, u, v, _]: [u32; 7], color: u32) {
    m.sw(packet.wrapping_add(4), color);
    m.sw(packet.wrapping_add(8), (y << 16) | sx16(x));
    m.sw(packet.wrapping_add(0xc), 0x7814_0000 | (v << 8) | u);
    m.sw(packet.wrapping_add(0x10), (h << 16) | sx16(w));
}
/// Records 0x150 and 0x151 as two sprites with their DR_TPAGEs from
/// `packet` in colour `color`; the block's next packet becomes `packet +
/// 0x38`.
fn two_pictures(m: &mut Machine, b: u32, packet: u32, color: u32) {
    let first = picture(m, b, 0x150);
    sprite_words(m, packet, first, color);
    link_sprite(m, b, packet);
    let b = tpage(
        m,
        packet.wrapping_add(0x14),
        0xe100_0200 | (first[6] & 0x9ff),
    );
    let second = picture(m, b, 0x151);
    let packet2 = packet.wrapping_add(0x1c);
    sprite_words(m, packet2, second, color);
    link_sprite(m, b, packet2);
    let b = tpage(
        m,
        packet.wrapping_add(0x30),
        0xe100_0200 | (second[6] & 0x9ff),
    );
    m.sw(b.wrapping_add(0x3c), packet.wrapping_add(0x38));
}
/// The title picture (0,0x14 256x20 at 0x20,0xC8 in CLUT 0x7814, tpage
/// 0x2F) in colour code `color` at `packet`; the block's next packet
/// becomes `packet + 0x1C`. The sprite is linked through `b` when the
/// caller read the block before the stores, else through the block read
/// after them.
fn title(m: &mut Machine, packet: u32, color: u32, b: Option<u32>) {
    m.sw(packet.wrapping_add(4), color);
    m.sw(packet.wrapping_add(8), 0x00c8_0020);
    m.sw(packet.wrapping_add(0xc), 0x7814_1400);
    m.sw(packet.wrapping_add(0x10), 0x0014_0100);
    let b = match b {
        Some(b) => b,
        None => block(m),
    };
    link_sprite(m, b, packet);
    let page = packet.wrapping_add(0x14);
    let b = tpage(m, page, 0xe100_002f);
    m.sw(b.wrapping_add(0x3c), page.wrapping_add(8));
}

/// 8011CB5C (JP, new; PAL 800D77B4's role): one credits frame. A chunk
/// step flips the shown bank (1) or starts the layered sequence (2: the
/// frame counter restarts at context +0x24). Layered: the background
/// layers ([`layers`]), the reticle ([`draw_reticle`]), then the scrolling
/// list ([`draw_credits`]) once record 0x150's y is 190 or more; before
/// that records 0x150/0x151 (the first 23 pictures; then fading out over
/// frames 0..127) or record 0x152 at y 400 (fading in and out over frames
/// 200..1039). The title fades out over the first picture's first 128
/// frames, and before the layered sequence it fades in over frames
/// 180..307. Then the next chunk; returns 1 at END.
pub fn credits_frame(m: &mut Machine) -> u32 {
    framed(m, 0x28, |m, _| {
        let d = decoder(m);
        let step = lbu(m, d);
        if step != 0 {
            if step == 1 {
                let shown = lbu(m, d.wrapping_add(0x9f)) ^ 1;
                sb(m, d.wrapping_add(0x9f), shown);
                let d = decoder(m);
                let count = lhu(m, d.wrapping_add(8)).wrapping_add(1);
                sh(m, d.wrapping_add(8), count);
            } else {
                sb(m, d.wrapping_add(0x9f), 0);
                let d = decoder(m);
                sh(m, d.wrapping_add(8), 0);
                sb(m, d.wrapping_add(1), 1);
                let context = ctx(m);
                let b = block(m);
                let time = m.lw(context.wrapping_add(0x24));
                m.sw(b.wrapping_add(0x1890), time);
            }
            let d = decoder(m);
            sb(m, d, 0);
            let d2 = decoder(m);
            m.sw(d.wrapping_add(4), 0);
            sb(m, d2.wrapping_add(0x10), 0);
            let d = decoder(m);
            sb(m, d.wrapping_add(0xa), 0);
        }
        let d = decoder(m);
        let layered = lbu(m, d.wrapping_add(1));
        if layered == 1 {
            layered_frame(m, d);
        } else {
            let b = block(m);
            let time = m.lw(d.wrapping_add(4));
            let packet = m.lw(b.wrapping_add(0x3c));
            let color = if time.wrapping_sub(0xb4) < 0x80 {
                let level = lbu(m, d.wrapping_add(4)).wrapping_add(0x4c) & 0xff;
                (level << 16) | (level << 8) | 0x6600_0000 | level
            } else if slt(time, 0x134) {
                0x6600_0000
            } else {
                0x6680_8080
            };
            title(m, packet, color, None);
        }
        let d = decoder(m);
        next_chunk(m, d);
        let d = decoder(m);
        let end = lbu(m, d.wrapping_add(0xa0)) ^ 1;
        let count = m.lw(d.wrapping_add(4)).wrapping_add(1);
        m.sw(d.wrapping_add(4), count);
        u32::from(end == 0)
    })
}

/// The layered part of [`credits_frame`] (8011CC34..8011D427).
fn layered_frame(m: &mut Machine, d: u32) {
    let context = ctx(m);
    let b = block(m);
    let start = m.lw(b.wrapping_add(0x1890));
    let packets = m.lw(b.wrapping_add(0x3c));
    let now = m.lw(context.wrapping_add(0x24));
    m.sw(b.wrapping_add(0x350), now.wrapping_sub(start));
    let now = m.lw(context.wrapping_add(0x24));
    let ot = m.lw(b.wrapping_add(0x40));
    m.sw(b.wrapping_add(0x188c), now.wrapping_sub(start));
    let next = layers(m, d, packets, ot);
    let b = block(m);
    let ot = m.lw(b.wrapping_add(0x40));
    m.sw(b.wrapping_add(0x3c), next);
    let next = draw_reticle(m, b.wrapping_add(0x6c), ot, next, 0x800c_0000);
    let b = block(m);
    let last_y = m.lw(b.wrapping_add(0xde0));
    m.sw(b.wrapping_add(0x3c), next);
    if !slt(last_y, 0xbe) {
        let ot = m.lw(b.wrapping_add(0x40));
        let next = draw_credits(m, b.wrapping_add(0x354), ot, next);
        let b = block(m);
        m.sw(b.wrapping_add(0x3c), next);
    } else {
        let d = decoder(m);
        if slt(lh(m, d.wrapping_add(8)), 0x17) {
            two_pictures(m, b, next, 0x6680_8080);
        } else {
            let time = m.lw(d.wrapping_add(4));
            if slt(time, 0x80) {
                let level = 0xffff_ff80u32.wrapping_sub(lbu(m, d.wrapping_add(4))) & 0xff;
                let color = (level << 16) | (level << 8) | 0x6600_0000 | level;
                two_pictures(m, b, next, color);
            } else {
                let level = if time.wrapping_sub(0xc8) < 0x40 {
                    time.wrapping_sub(0xc8) << 1
                } else if time.wrapping_sub(0x108) < 0x2c8 {
                    0x80
                } else if time.wrapping_sub(0x3d0) < 0x40 {
                    0xffff_ff80u32.wrapping_sub(time.wrapping_sub(0x3d0) << 1)
                } else {
                    0
                };
                let color = gray(level) | 0x6600_0000;
                // record 0x152 (its y is not read: the picture is at y 400)
                let b = block(m);
                let list = b.wrapping_add(0x354);
                let x = lhu(m, list.wrapping_add(4 * 0x152));
                let w = lhu(m, list.wrapping_add(0xa98 + 2 * 0x152));
                let h = lhu(m, list.wrapping_add(0xd3e + 2 * 0x152));
                let u = lbu(m, list.wrapping_add(0xfe4 + 0x152));
                let v = lbu(m, list.wrapping_add(0x1137 + 0x152));
                let code = lhu(m, list.wrapping_add(0x128a + 2 * 0x152));
                m.sw(next.wrapping_add(4), color);
                m.sw(next.wrapping_add(8), sx16(x) | 0x0190_0000);
                m.sw(next.wrapping_add(0xc), 0x7814_0000 | (v << 8) | u);
                m.sw(next.wrapping_add(0x10), (h << 16) | sx16(w));
                link_sprite(m, b, next);
                let page = next.wrapping_add(0x14);
                let b = tpage(m, page, (code & 0x9ff) | 0xe100_0200);
                m.sw(b.wrapping_add(0x3c), page.wrapping_add(8));
            }
        }
    }
    let d = decoder(m);
    if lh(m, d.wrapping_add(8)) == 0 {
        let time = m.lw(d.wrapping_add(4));
        let b = block(m);
        let packet = m.lw(b.wrapping_add(0x3c));
        if slt(time, 0x80) {
            let level = 0xffff_ff80u32.wrapping_sub(lbu(m, d.wrapping_add(4))) & 0xff;
            let color = (level << 16) | (level << 8) | 0x6600_0000 | level;
            title(m, packet, color, Some(b));
        }
    }
}

/// 8011D474 (JP; PAL 800D80A4, exact; `d`, `packets`, `ot`): the effect
/// slots: at the current slot's end (+0x16 + its fourth word) the next
/// slot, at its start (+0x14) its effect (+0x12); effects 0..5 draw the
/// background ([`background`]; 1..5 count the effect time +0xC). Returns
/// the next packet (unchanged for other effects).
pub fn layers(m: &mut Machine, d: u32, packets: u32, ot: u32) -> u32 {
    framed(m, 0x18, |m, _| {
        let s = current_slot(m, d);
        let end = lh(m, s.wrapping_add(0x16)).wrapping_add(lh(m, s.wrapping_add(0x1e)));
        if m.lw(d.wrapping_add(4)) == end {
            let next = lbu(m, d.wrapping_add(0x10));
            sb(m, d.wrapping_add(0xa), 0);
            m.sw(d.wrapping_add(0xc), 0);
            sb(m, d.wrapping_add(0x10), next.wrapping_add(1));
        }
        let s = current_slot(m, d);
        let start = lh(m, s.wrapping_add(0x14));
        if m.lw(d.wrapping_add(4)) == start {
            let effect = lbu(m, s.wrapping_add(0x12));
            m.sw(d.wrapping_add(0xc), 0);
            sb(m, d.wrapping_add(0xa), effect);
        }
        match lbu(m, d.wrapping_add(0xa)) {
            0 => background(m, d, packets, ot),
            1..=5 => {
                let next = background(m, d, packets, ot);
                let time = m.lw(d.wrapping_add(0xc)).wrapping_add(1);
                m.sw(d.wrapping_add(0xc), time);
                next
            }
            _ => packets,
        }
    })
}

/// 8011D59C (JP; PAL 800D81CC, changed: the second row 224 lines tall,
/// CLUT rows 510/511, table entry 2 for the 24th picture; `d`, `packets`,
/// `ot`): the shown bank's picture as four SPRT (256 + 64 wide, 256 + 224
/// tall) at
/// a brightness fading out (effect 4) or in (5) over the current slot,
/// else 0x80, linked into table entry 2 (picture 0x17) or 4. Returns the
/// next packet.
pub fn background(m: &mut Machine, d: u32, packets: u32, ot: u32) -> u32 {
    framed(m, 8, |m, _| {
        let effect = lbu(m, d.wrapping_add(0xa));
        let level = if effect == 4 || effect == 5 {
            let s = current_slot(m, d);
            let span = lh(m, s.wrapping_add(0x16)).wrapping_sub(lh(m, s.wrapping_add(0x14)));
            let time = m.lw(d.wrapping_add(0xc)) << 7;
            let level = if effect == 4 {
                div(m, time.wrapping_neg(), span, 0x8011_d60c, 0x8011_d624)
                    .0
                    .wrapping_add(0x80)
            } else {
                div(m, time, span, 0x8011_d68c, 0x8011_d6a4).0
            };
            let level = if slt(sx16(level), 0x81) { level } else { 0x80 };
            if ((level << 16) as i32) < 0 { 0 } else { level }
        } else {
            0x80
        };
        let level = sx16(level);
        let color = (level << 16) | (level << 8) | level;
        let mut p = packets;
        let mut tail = packets << 8;
        for i in 0..4u32 {
            // (w, h, x, y) and the two banks' tpages
            let (w, h, x, y, pages) = match i {
                0 => (0x100u32, 0x100u32, 0u32, 0u32, (0x85u32, 0x88u32)),
                1 => (0x40, 0x100, 0x100, 0, (0x87, 0x8a)),
                2 => (0x100, 0xe0, 0, 0x100, (0x95, 0x98)),
                _ => (0x40, 0xe0, 0x100, 0x100, (0x97, 0x9a)),
            };
            let (page, clut) = if lbu(m, d.wrapping_add(0x9f)) == 0 {
                (pages.0, 0x7f80u32)
            } else {
                (pages.1, 0x7fc0)
            };
            m.sw(p.wrapping_add(4), page & 0xffff);
            m.sw(p.wrapping_add(0xc), (y << 16) | sx16(x));
            m.sw(p.wrapping_add(0x10), clut << 16);
            m.sw(p.wrapping_add(8), color);
            sb(m, p.wrapping_add(0xb), 0x64);
            m.sw(p.wrapping_add(0x14), (h << 16) | sx16(w));
            sb(m, p.wrapping_add(7), 0xe1);
            sb(m, p.wrapping_add(6), 0);
            let table = if lh(m, d.wrapping_add(8)) == 0x17 {
                ot.wrapping_add(8)
            } else {
                ot.wrapping_add(0x10)
            };
            let tag = m.lw(table) | 0x0500_0000;
            m.sw(p, tag);
            m.sw(table, tail >> 8);
            tail = tail.wrapping_add(0x1800);
            p = p.wrapping_add(0x18);
        }
        p
    })
}

/// `(sx16(v) + (v as u16 >> 15)) >> 1`: the compiler's signed halfword
/// halving of `lhu`/`sll 16`/`sra 16`/`srl 31`/`addu`/`sra 1`.
fn half(value: u32) -> u32 {
    let shifted = value << 16;
    sra(sra(shifted, 16).wrapping_add(shifted >> 31), 1)
}
/// The centre word of a point: `((y + h/2 + dy) << 16) | ((x + w/2 + dx) &
/// 0xFFFF)` with the reticle's size `c + 4`/`c + 6` (loaded first).
fn centre(m: &mut Machine, c: u32, at_x: u32, at_y: u32, dx: u32, dy: u32) -> u32 {
    let w = lhu(m, c.wrapping_add(4));
    let x = lhu(m, at_x);
    let px = x.wrapping_add(half(w)).wrapping_add(dx) & 0xffff;
    let h = lhu(m, c.wrapping_add(6));
    let y = lhu(m, at_y);
    let py = y.wrapping_add(half(h)).wrapping_add(dy);
    (py << 16) | px
}
/// The centre word of trail point `point` (`c + 2n`) moved by its spread
/// `s` (+0x1A4): side 0 `(x + s, y - s)`, side 1 `(x - s, s + y)` (loads
/// in the original order: w, x, s, h, y).
fn spread_centre(m: &mut Machine, c: u32, point: u32, side: u32) -> u32 {
    let w = lhu(m, c.wrapping_add(4));
    let x = lhu(m, point.wrapping_add(0x14));
    let s = lhu(m, point.wrapping_add(0x1a4));
    let h = lhu(m, c.wrapping_add(6));
    let y = lhu(m, point.wrapping_add(0xdc));
    let x = x.wrapping_add(half(w));
    let y = y.wrapping_add(half(h));
    if side == 0 {
        (y.wrapping_sub(s) << 16) | (s.wrapping_add(x) & 0xffff)
    } else {
        (s.wrapping_add(y) << 16) | (x.wrapping_sub(s) & 0xffff)
    }
}
/// The trail point `count` of reticle `c` takes the reticle's position:
/// brightness 0x40, spread 1.
fn add_point(m: &mut Machine, c: u32) {
    let count = m.lw(c.wrapping_add(0x2d0)).wrapping_add(1);
    let x = lhu(m, c);
    m.sw(c.wrapping_add(0x2d0), count);
    sh(m, c.wrapping_add(count << 1).wrapping_add(0x14), x);
    let count = m.lw(c.wrapping_add(0x2d0));
    let y = lhu(m, c.wrapping_add(2));
    sh(m, c.wrapping_add(count << 1).wrapping_add(0xdc), y);
    let count = m.lw(c.wrapping_add(0x2d0));
    sb(m, c.wrapping_add(count).wrapping_add(0x26c), 0x40);
    let count = m.lw(c.wrapping_add(0x2d0));
    sh(m, c.wrapping_add(count << 1).wrapping_add(0x1a4), 1);
}
/// Link a gouraud quad (8 words) at `packet` into `ot + 0xC`.
fn link_quad(m: &mut Machine, ot: u32, packet: u32) {
    let tag = m.lw(ot.wrapping_add(0xc)) | 0x0800_0000;
    m.sw(packet, tag);
    m.sw(ot.wrapping_add(0xc), (packet << 8) >> 8);
}

/// 8011D8F4 (JP; PAL 800D8530, exact; `c` the reticle, `ot`, `packets`):
/// the reticle moving from (310,180) to (-32,80) over times 12540..15040
/// (+0x2E4; the key table in the frame, a later key reads the frame's
/// stale words), a TILE at its position, trail points every 100 frames
/// (and at the key ends), the trail fading (brightness 64 down to 3,
/// spread up to 3) once the move ended (or at elapsed 1050), drawn as
/// gouraud quads from the reticle to the newest point and between points
/// (table entry 3), then a DR_TPAGE. Returns the next packet.
///
/// The frame's word +0x18 is where the original saves `s0`, and keys from
/// 2 read it: `caller_s0` is the caller's `s0` ([`credits_frame`] holds
/// 0x800C0000 there; through `dispatch` the caller is unknown and the
/// value is the oracle's, 0).
pub fn draw_reticle(m: &mut Machine, c: u32, ot: u32, packets: u32, caller_s0: u32) -> u32 {
    framed(m, 0x20, |m, keys| {
        m.sw(keys.wrapping_add(0x18), caller_s0);
        m.sw(keys, 0x30fc);
        m.sw(keys.wrapping_add(4), 0x3ac0);
        sh(m, keys.wrapping_add(8), 0x136);
        sh(m, keys.wrapping_add(0xa), 0xffe0);
        sh(m, keys.wrapping_add(0x10), 0xb4);
        sh(m, keys.wrapping_add(0x12), 0x50);
        let mut p = packets;
        let key = m.lw(c.wrapping_add(0x10));
        let time = m.lw(c.wrapping_add(0x2e4));
        let range = keys.wrapping_add(key << 3);
        if !slt(time, m.lw(range)) && slt(time, m.lw(range.wrapping_add(4))) {
            let xs = keys.wrapping_add(8).wrapping_add(key << 2);
            let ys = keys.wrapping_add(0x10).wrapping_add(key << 2);
            let end = lhu(m, range.wrapping_add(4));
            let start = lhu(m, range);
            let x1 = lhu(m, xs.wrapping_add(2));
            let x0 = lhu(m, xs);
            let duration = end.wrapping_sub(start);
            let dx = x1.wrapping_sub(x0);
            let y1 = lhu(m, ys.wrapping_add(2));
            let y0 = lhu(m, ys);
            let elapsed = lhu(m, c.wrapping_add(0x2e4)).wrapping_sub(start) << 16;
            let t = sra(elapsed, 16);
            let dy = y1.wrapping_sub(y0);
            if t == 0x41a {
                let now = m.lw(c.wrapping_add(0x2e4));
                sb(m, c.wrapping_add(0x2d8), 1);
                m.sw(c.wrapping_add(0x2dc), now);
            }
            let span = sx16(duration);
            let ox = div(m, lo_s(sx16(dx), t), span, 0x8011_d9f8, 0x8011_da10).0;
            let oy = div(m, lo_s(sx16(dy), t), span, 0x8011_da3c, 0x8011_da54).0;
            let hundredths = hi_s(t, 0x51eb_851f);
            let key = m.lw(c.wrapping_add(0x10));
            let x = lhu(m, keys.wrapping_add(8).wrapping_add(key << 2));
            sh(m, c, x.wrapping_add(ox));
            let key = m.lw(c.wrapping_add(0x10));
            let y = lhu(m, keys.wrapping_add(0x10).wrapping_add(key << 2));
            let color = m.lw(c.wrapping_add(0xc));
            sh(m, c.wrapping_add(2), y.wrapping_add(oy));
            m.sw(p.wrapping_add(4), color);
            let xy = (lh(m, c.wrapping_add(2)) << 16) | lhu(m, c);
            m.sw(p.wrapping_add(8), xy);
            let wh = (lh(m, c.wrapping_add(6)) << 16) | lh(m, c.wrapping_add(4));
            m.sw(p.wrapping_add(0xc), wh);
            let q = sra(hundredths, 5).wrapping_sub(sra(elapsed, 31));
            let rest = t.wrapping_sub(q.wrapping_mul(100)) << 16;
            p = p.wrapping_add(0x10);
            if rest == 0 && t != 0 {
                add_point(m, c);
            }
        }
        let key = m.lw(c.wrapping_add(0x10));
        let time = m.lw(c.wrapping_add(0x2e4));
        let base = p;
        if time == m.lw(keys.wrapping_add(key << 3)) {
            m.sw(c.wrapping_add(0x2d0), 0);
            let count = m.lw(c.wrapping_add(0x2d0));
            let key = m.lw(c.wrapping_add(0x10));
            m.sw(c.wrapping_add(0x2d4), 1);
            sb(m, c.wrapping_add(0x2d8), 0);
            m.sw(c.wrapping_add(0x2dc), 0);
            let x = lhu(m, keys.wrapping_add(key << 2).wrapping_add(8));
            sh(m, c.wrapping_add(count << 1).wrapping_add(0x14), x);
            let count = m.lw(c.wrapping_add(0x2d0));
            let key = m.lw(c.wrapping_add(0x10));
            let y = lhu(m, keys.wrapping_add(key << 2).wrapping_add(0x10));
            sh(m, c.wrapping_add(count << 1).wrapping_add(0xdc), y);
            let count = m.lw(c.wrapping_add(0x2d0));
            sb(m, c.wrapping_add(count).wrapping_add(0x26c), 0x40);
            let count = m.lw(c.wrapping_add(0x2d0));
            sh(m, c.wrapping_add(count << 1).wrapping_add(0x1a4), 1);
        }
        let key = m.lw(c.wrapping_add(0x10));
        let time = m.lw(c.wrapping_add(0x2e4));
        if time == m.lw(keys.wrapping_add(key << 3).wrapping_add(4)) {
            let fading = lbu(m, c.wrapping_add(0x2d8));
            m.sw(c.wrapping_add(0x10), key.wrapping_add(1));
            if fading == 0 {
                let now = m.lw(c.wrapping_add(0x2e4));
                sb(m, c.wrapping_add(0x2d8), 1);
                m.sw(c.wrapping_add(0x2dc), now);
            }
            add_point(m, c);
        }
        if lbu(m, c.wrapping_add(0x2d8)) == 1 {
            let now = m.lw(c.wrapping_add(0x2e4));
            let fade_start = m.lw(c.wrapping_add(0x2dc));
            let shown = m.lw(c.wrapping_add(0x2d4));
            let since = now.wrapping_sub(fade_start);
            if (shown as i32) > 0 {
                let mut age = since;
                let mut i = 0u32;
                loop {
                    let product = lo_s(age, 0x3d);
                    let q = sra(hi_s(product, 0x51eb_851f), 6).wrapping_sub(sra(product, 31));
                    let fade = 0x40u32.wrapping_sub(q);
                    let brightness = if slt(sx16(fade), 3) { 3 } else { fade };
                    sb(m, c.wrapping_add(i).wrapping_add(0x26c), brightness);
                    if !slt(age, 100) {
                        let product = lo_s(age.wrapping_sub(100), 2);
                        let q = sra(hi_s(product, 0x51eb_851f), 5)
                            .wrapping_sub(sra(product, 31))
                            .wrapping_add(1);
                        let spread = if slt(sx16(q), 4) { q } else { 3 };
                        sh(m, c.wrapping_add(2 * i).wrapping_add(0x1a4), spread);
                    }
                    let shown = m.lw(c.wrapping_add(0x2d4));
                    i = i.wrapping_add(1);
                    age = age.wrapping_sub(100);
                    if !slt(i, shown) {
                        break;
                    }
                }
            }
            let q = sra(hi_s(since, 0x51eb_851f), 5).wrapping_sub(sra(since, 31));
            if since == q.wrapping_mul(100) && since != 0 {
                let shown = m.lw(c.wrapping_add(0x2d4));
                let count = m.lw(c.wrapping_add(0x2d0));
                if !slt(count, shown) {
                    m.sw(c.wrapping_add(0x2d4), shown.wrapping_add(1));
                }
            }
        }
        let mut p = base;
        let key = m.lw(c.wrapping_add(0x10));
        let time = m.lw(c.wrapping_add(0x2e4));
        let range = keys.wrapping_add(key << 3);
        if !slt(time, m.lw(range)) && !slt(m.lw(range.wrapping_add(4)), time) {
            for side in 0..2u32 {
                let q = p.wrapping_add(0x20);
                m.sw(q.wrapping_sub(0x1c), 0x3a40_4040);
                let count = m.lw(c.wrapping_add(0x2d0));
                let b = lbu(m, c.wrapping_add(count).wrapping_add(0x26c));
                m.sw(q.wrapping_sub(0x14), gray(b));
                m.sw(q.wrapping_sub(0xc), 0x0003_0303);
                m.sw(q.wrapping_sub(4), 0x0003_0303);
                let xy0 = centre(m, c, c, c.wrapping_add(2), 0, 0);
                m.sw(q.wrapping_sub(0x18), xy0);
                let count = m.lw(c.wrapping_add(0x2d0));
                let point = c.wrapping_add(count << 1);
                let xy1 = centre(
                    m,
                    c,
                    point.wrapping_add(0x14),
                    point.wrapping_add(0xdc),
                    0,
                    0,
                );
                m.sw(q.wrapping_sub(0x10), xy1);
                let (plus, minus) = if side == 0 {
                    (1, u32::MAX)
                } else {
                    (u32::MAX, 1)
                };
                let xy2 = centre(m, c, c, c.wrapping_add(2), plus, minus);
                m.sw(q.wrapping_sub(8), xy2);
                let count = m.lw(c.wrapping_add(0x2d0));
                let point = c.wrapping_add(count << 1);
                let xy3 = spread_centre(m, c, point, side);
                m.sw(q, xy3);
                link_quad(m, ot, p);
                p = p.wrapping_add(0x24);
            }
        }
        let mut i = 0u32;
        while slt(i, m.lw(c.wrapping_add(0x2d0))) {
            let newer = c.wrapping_add(i.wrapping_add(1) << 1);
            let older = c.wrapping_add(i << 1);
            for side in 0..2u32 {
                let q = p.wrapping_add(0x20);
                let b = lbu(m, c.wrapping_add(i.wrapping_add(1)).wrapping_add(0x26c));
                m.sw(q.wrapping_sub(0x1c), gray(b) | 0x3a00_0000);
                let b = lbu(m, c.wrapping_add(i).wrapping_add(0x26c));
                m.sw(q.wrapping_sub(0x14), gray(b));
                m.sw(q.wrapping_sub(0xc), 0x0003_0303);
                m.sw(q.wrapping_sub(4), 0x0003_0303);
                let xy0 = centre(
                    m,
                    c,
                    newer.wrapping_add(0x14),
                    newer.wrapping_add(0xdc),
                    0,
                    0,
                );
                m.sw(q.wrapping_sub(0x18), xy0);
                let xy1 = centre(
                    m,
                    c,
                    older.wrapping_add(0x14),
                    older.wrapping_add(0xdc),
                    0,
                    0,
                );
                m.sw(q.wrapping_sub(0x10), xy1);
                for (point, at) in [(newer, 8u32), (older, 0)] {
                    let xy = spread_centre(m, c, point, side);
                    m.sw(q.wrapping_sub(at), xy);
                }
                link_quad(m, ot, p);
                p = p.wrapping_add(0x24);
            }
            i = i.wrapping_add(1);
        }
        let fading = lbu(m, c.wrapping_add(0x2d8));
        if slt(m.lw(c.wrapping_add(0x2d0)), fading) {
            sb(m, c.wrapping_add(0x2d8), 0);
            m.sw(c.wrapping_add(0x2dc), 0);
        }
        sb(m, p.wrapping_add(3), 1);
        m.sw(p.wrapping_add(4), 0xe100_0020);
        add_prim(m, ot.wrapping_add(0xc), p);
        p.wrapping_add(8)
    })
}

/// 0x8011EAC4: the scroll step word (0x202; its low byte is the step).
const SCROLL_STEP: u32 = 0x8011_eac4;

/// 8011E474 (JP, new; PAL 800D90B0's role; `list`, `ot`, `packets`): the
/// scrolling credit list: every other scroll frame (+0x1538) records from
/// the first on screen (+0x1532) up to 0x151 move up 2; the records from
/// the first with y < 501 are on screen (+0x1534), those above y -49
/// leave; each as a SPRT (colour 0x66808080, CLUT +0x1530) with its
/// DR_TPAGE, then a DR_TPAGE 0x220. Returns the next packet.
pub fn draw_credits(m: &mut Machine, list: u32, ot: u32, packets: u32) -> u32 {
    framed(m, 0x10, |m, _| {
        let first = lh(m, list.wrapping_add(0x1532));
        m.sw(SCROLL_STEP, 0x202);
        let step = lbu(m, SCROLL_STEP);
        if slt(first, 0x152) {
            let mut i = first;
            let mut at = (first << 2).wrapping_add(list);
            loop {
                let time = m.lw(list.wrapping_add(0x1538));
                // `div time, 2`: the divisor is a constant, no trap
                let (_, rest) = div(m, time, 2, 0x8011_e4d0, 0x8011_e4e8);
                if rest == 0 {
                    let y = m.lw(at.wrapping_add(0x54c)).wrapping_sub(step);
                    m.sw(at.wrapping_add(0x54c), y);
                }
                i = i.wrapping_add(1);
                at = at.wrapping_add(4);
                if !slt(i, 0x152) {
                    break;
                }
            }
        }
        let first = lh(m, list.wrapping_add(0x1532));
        if slt(first, 0x152) {
            let mut i = first;
            let mut at = (first << 2).wrapping_add(list);
            loop {
                if !slt(m.lw(at.wrapping_add(0x54c)), 0x1f5) {
                    break;
                }
                sh(m, list.wrapping_add(0x1534), i);
                i = i.wrapping_add(1);
                at = at.wrapping_add(4);
                if !slt(i, 0x152) {
                    break;
                }
            }
        }
        let last = lh(m, list.wrapping_add(0x1534));
        if (last as i32) > 0 {
            let mut i = 0u32;
            loop {
                let y = m.lw(list.wrapping_add(i << 2).wrapping_add(0x54c));
                if !slt(y, 0xffff_ffcf) {
                    break;
                }
                let next = i.wrapping_add(1);
                sh(m, list.wrapping_add(0x1532), next);
                i = next;
                if !slt(i, last) {
                    break;
                }
            }
        }
        let mut i = lh(m, list.wrapping_add(0x1532));
        let mut p = packets;
        if !slt(lh(m, list.wrapping_add(0x1534)), i) {
            let mut halves = (i << 1).wrapping_add(list);
            let mut words = (i << 2).wrapping_add(list);
            loop {
                let w = lhu(m, halves.wrapping_add(0xa98));
                let h = lhu(m, halves.wrapping_add(0xd3e));
                let code = lhu(m, halves.wrapping_add(0x128a));
                halves = halves.wrapping_add(2);
                let x = lhu(m, words);
                let y = lhu(m, words.wrapping_add(0x54c));
                words = words.wrapping_add(4);
                let u = lbu(m, list.wrapping_add(i).wrapping_add(0xfe4));
                let v = lbu(m, list.wrapping_add(i).wrapping_add(0x1137));
                let clut = lhu(m, list.wrapping_add(0x1530));
                i = i.wrapping_add(1);
                m.sw(p.wrapping_add(4), 0x6680_8080);
                m.sw(p.wrapping_add(8), (y << 16) | sx16(x));
                m.sw(p.wrapping_add(0xc), (clut << 16) | (v << 8) | u);
                m.sw(p.wrapping_add(0x10), (h << 16) | sx16(w));
                let tag = m.lw(ot) | 0x0400_0000;
                m.sw(p, tag);
                m.sw(ot, (p << 8) >> 8);
                let page = p.wrapping_add(0x14);
                sb(m, page.wrapping_add(3), 1);
                m.sw(page.wrapping_add(4), (code & 0x9ff) | 0xe100_0200);
                add_prim(m, ot, page);
                p = page.wrapping_add(8);
                if slt(lh(m, list.wrapping_add(0x1534)), i) {
                    break;
                }
            }
        }
        sb(m, p.wrapping_add(3), 1);
        m.sw(p.wrapping_add(4), 0xe100_0220);
        add_prim(m, ot, p);
        p.wrapping_add(8)
    })
}

/// The routines of this file by JP address, while module 3 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    let [a0, a1, a2, _] = args;
    Some(match target {
        0x8011_cb5c => credits_frame(m),
        0x8011_d474 => layers(m, a0, a1, a2),
        0x8011_d59c => background(m, a0, a1, a2),
        0x8011_d8f4 => draw_reticle(m, a0, a1, a2, 0),
        0x8011_e474 => draw_credits(m, a0, a1, a2),
        _ => return None,
    })
}
