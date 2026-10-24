//! JP overlay 2: the broadcast picture stream (800EC340..800ECD1F), the
//! animated picture of the story broadcasts, translated from the JP
//! instructions (class `new`).
//!
//! Block `[0x80112DFC]` (0xCDCC bytes, 800EC6B4): +0 a flip flag, +8 ticks
//! per frame, +0xA the current frame (halfwords); +0xC / +0xAC the two
//! buffers of up to four POLY_FT4 (0x28 bytes) showing the picture, +0x14C
//! the next one to link; +0x150 the clut's VRAM rectangle (8 bytes),
//! +0x158 the picture's VRAM rectangle; +0x160 the quads' texture rectangles and
//! +0x180 their screen rectangles (8 bytes each, +0x1A0 the count);
//! +0x1A4 the frame directory (word 0 the count, then 12-byte frames: +0
//! tile count, +4 the tick it shows at, +8 the tile list), +0x1A8 the
//! current frame, +0x1AC the tile directory (+4 the CLUT, +0x204 the
//! tiles' pixels); +0x1B0/+0x1B2 the picture size in 8-pixel tiles, +0x1B4
//! /+0x1B6 4, +0x1B8 the tick counter; +0x81C0 / +0xE7C0 the two buffers
//! of tile upload packets (0x34 bytes: an image load of 4x4 halfwords),
//! +0xCDC0 the next free one, +0xCDC4 / +0xCDC8 counters.
use super::ov2_p4::{At, call, copy8, div, lh, lt, lwl, sra, swl};
use crate::common::{CONTEXT, loaded_module};
use crate::machine::{Machine, SmallData};

/// `[0x80112DFC]`: the stream block.
pub const BLOCK: u32 = 0x8011_2dfc;
/// Resident arena allocation and directory relocation.
const ALLOC: u32 = 0x8002_7410;
const RELOCATE: u32 = 0x8002_7338;
/// libgpu ClearImage and LoadImage.
const CLEAR_IMAGE: u32 = 0x800a_8318;
const LOAD_IMAGE: u32 = 0x800a_8440;

/// The routines of this file by JP address, while module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != 2 {
        return None;
    }
    let [a0, a1, a2, a3] = args;
    Some(match target {
        0x800e_c340 => build_quads_ec340(m),
        0x800e_c4ec => start_ec4ec(m, a0, a1, a2, a3),
        0x800e_c6b4 => open_ec6b4(m, a0, a1, a2, a3),
        0x800e_c824 => select_buffers_ec824(m),
        0x800e_c880 => upload_tiles_ec880(m, a0),
        0x800e_cbec => link_quads_ecbec(m, a0),
        0x800e_cc70 => tick_ecc70(m, a0, a1),
        _ => return None,
    })
}

fn block(m: &mut Machine) -> u32 {
    m.lw(BLOCK)
}

/// 800EC340: for both buffers, one POLY_FT4 (grey 0x80, tpage 0x9E, the
/// picture's clut) per screen rectangle (+0x180) textured by its texture
/// rectangle (+0x160). Returns 0.
pub fn build_quads_ec340(m: &mut Machine) -> u32 {
    let mut first = 0xcu32;
    for _ in 0..2 {
        let b = block(m);
        let count = m.lw(b.at(0x1a0));
        if lt(0, count) {
            let mut quad = b.wrapping_add(first);
            let (mut screen, mut texture, mut i) = (0x180u32, 0x160u32, 0u32);
            loop {
                let b = block(m);
                m.sb(quad.at(3), 9);
                m.sb(quad.at(7), 0x2c);
                let r = b.wrapping_add(screen);
                let x = m.lhu(r);
                m.sh(quad.at(8), x);
                let y = m.lhu(r.at(2));
                m.sh(quad.at(0xa), y);
                let x = m.lhu(r);
                let w = m.lhu(r.at(4));
                m.sh(quad.at(0x10), x.wrapping_add(w));
                let y = m.lhu(r.at(2));
                m.sh(quad.at(0x12), y);
                let x = m.lhu(r);
                m.sh(quad.at(0x18), x);
                let y = m.lhu(r.at(2));
                let h = m.lhu(r.at(6));
                m.sh(quad.at(0x1a), y.wrapping_add(h));
                let x = m.lhu(r);
                let w = m.lhu(r.at(4));
                m.sh(quad.at(0x20), x.wrapping_add(w));
                let y = m.lhu(r.at(2));
                let h = m.lhu(r.at(6));
                let t = b.wrapping_add(texture);
                m.sh(quad.at(0x22), y.wrapping_add(h));
                let u = m.lbu(t);
                m.sb(quad.at(0xc), u);
                let v = m.lbu(t.at(2));
                m.sb(quad.at(0xd), v);
                let u = m.lbu(t);
                let w = m.lbu(t.at(4));
                m.sb(quad.at(0x14), u.wrapping_add(w));
                let v = m.lbu(t.at(2));
                m.sb(quad.at(0x15), v);
                let u = m.lbu(t);
                m.sb(quad.at(0x1c), u);
                let v = m.lbu(t.at(2));
                let h = m.lbu(t.at(6));
                i = i.wrapping_add(1);
                m.sb(quad.at(0x1d), v.wrapping_add(h));
                let u = m.lbu(t);
                let w = m.lbu(t.at(4));
                screen = screen.wrapping_add(8);
                m.sb(quad.at(0x24), u.wrapping_add(w));
                let v = m.lbu(t.at(2));
                let h = m.lbu(t.at(6));
                texture = texture.wrapping_add(8);
                m.sb(quad.at(4), 0x80);
                m.sb(quad.at(5), 0x80);
                m.sb(quad.at(6), 0x80);
                m.sb(quad.at(0x25), v.wrapping_add(h));
                let b = block(m);
                m.sh(quad.at(0x16), 0x9e);
                let clut_y = u32::from(m.lhu(b.at(0x152)));
                let clut_x = u32::from(m.lhu(b.at(0x150)));
                m.sh(
                    quad.at(0xe),
                    ((clut_y << 6) | ((clut_x >> 4) & 0x3f)) as u16,
                );
                let count = m.lw(b.at(0x1a0));
                quad = quad.at(0x28);
                if !lt(i, count) {
                    break;
                }
            }
        }
        first = first.wrapping_add(0xa0);
    }
    0
}

/// 800EC4EC (picture rect, texture rects, clut rect, screen rects; count on
/// the stack): the clut rectangle (+0x150) and the picture's VRAM
/// rectangle (+0x158, where the tiles go), the `count` quad rectangles,
/// the first frame; clear the picture's VRAM (ClearImage), load the CLUT
/// (LoadImage), the picture size in tiles (rounded toward zero), counters
/// 0; build the quads. Returns block + 0x8000.
pub fn start_ec4ec(m: &mut Machine, screen: u32, textures: u32, picture: u32, rects: u32) -> u32 {
    let count = m.lw(m.sp.at(0x10));
    let frame = m.push_frame(0x20);
    let _ = frame;
    let b = block(m);
    copy8(m, picture, b.at(0x150));
    copy8(m, screen, b.at(0x158));
    m.sw(b.at(0x1a0), count);
    if lt(0, count) {
        let (mut textures, mut rects, mut i) = (textures, rects, 0u32);
        loop {
            let b = block(m);
            let at = b.wrapping_add(i << 3);
            i = i.wrapping_add(1);
            copy8(m, textures, at.at(0x160));
            copy8(m, rects, at.at(0x180));
            rects = rects.at(8);
            textures = textures.at(8);
            if !lt(i, count) {
                break;
            }
        }
    }
    let context = m.gw(CONTEXT);
    let b = block(m);
    let index = m.lw(context.at(4));
    let frames = m.lw(b.at(0x1a4));
    m.sw(b, index);
    m.sw(b.at(0x1a8), frames.at(4));
    call(m, CLEAR_IMAGE, [screen, 0, 0, 0]);
    // 800EC608..800EC614: a 255-step delay loop (no memory access)
    let b = block(m);
    let tiles = m.lw(b.at(0x1ac));
    call(m, LOAD_IMAGE, [picture, tiles.at(4), 0, 0]);
    let w = lh(m, screen.at(4));
    let b = block(m);
    let w = if lt(w, 0) { w.wrapping_add(7) } else { w };
    m.sh(b.at(0x1b0), sra(w, 3) as u16);
    let h = lh(m, screen.at(6));
    let h = if lt(h, 0) { h.wrapping_add(7) } else { h };
    m.sh(b.at(0x1b2), sra(h, 3) as u16);
    m.sh(b.at(0x1b4), 4);
    m.sh(b.at(0x1b6), 4);
    m.sw(b.at(0x1b8), 0);
    m.sh(b.at(8), 4);
    m.sh(b.at(0xa), 0);
    build_quads_ec340(m);
    let b = block(m).at(0x8000);
    m.sw(b.at(0x4dc4), 0);
    m.sw(b.at(0x4dc8), 0);
    m.pop_frame(0x20);
    b
}

/// 800EC6B4 (rect, texture rects, picture rect, screen rects; resource and
/// count on the stack): allocate the block (0xCDCC bytes), relocate the
/// resource's directory: frame directory (child 0, its frames' tile lists
/// relocated by child 1), tile directory (child 2, its pixel pointers
/// relocated by child 3); then 800EC4EC. Returns the frame count.
pub fn open_ec6b4(m: &mut Machine, screen: u32, textures: u32, picture: u32, rects: u32) -> u32 {
    let resource = m.lw(m.sp.at(0x10));
    let frame = m.push_frame(0x38);
    let b = call(m, ALLOC, [0xcdcc, 0, 0, 0]);
    m.sw(BLOCK, b);
    let children = call(m, RELOCATE, [resource, 0, 0, 0]);
    let b = block(m);
    let frames = m.lw(children);
    m.sw(b.at(0x1a4), frames);
    let tiles = m.lw(children.at(8));
    let frames = m.lw(b.at(0x1a4));
    m.sw(b.at(0x1ac), tiles);
    if lt(0, m.lw(frames)) {
        let (mut i, mut offset) = (0u32, 0u32);
        loop {
            let frames = m.lw(b.at(0x1a4));
            let base = m.lw(children.at(4));
            let entry = frames.wrapping_add(offset);
            let list = m.lw(entry.at(0xc));
            m.sw(entry.at(0xc), list.wrapping_add(base));
            let frames = m.lw(b.at(0x1a4));
            i = i.wrapping_add(1);
            let n = m.lw(frames);
            offset = offset.wrapping_add(0xc);
            if !lt(i, n) {
                break;
            }
        }
    }
    let b = block(m);
    let tiles = m.lw(b.at(0x1ac));
    if lt(0, m.lw(tiles)) {
        let mut i = 0u32;
        loop {
            let tiles = m.lw(b.at(0x1ac));
            let entry = tiles.wrapping_add(i << 2);
            let pixels = m.lw(entry.at(0x204));
            let base = m.lw(children.at(0xc));
            m.sw(entry.at(0x204), pixels.wrapping_add(base));
            let tiles = m.lw(b.at(0x1ac));
            let n = m.lw(tiles);
            i = i.wrapping_add(1);
            if !lt(i, n) {
                break;
            }
        }
    }
    // the count is read from the caller's frame only now (800EC7E0)
    let count = m.lw(frame.at(0x38 + 0x14));
    m.sw(frame.at(0x10), count);
    start_ec4ec(m, screen, textures, picture, rects);
    let b = block(m);
    let frames = m.lw(b.at(0x1a4));
    let n = m.lw(frames);
    m.pop_frame(0x38);
    n
}

/// 800EC824: the buffer of frame `[context + 4]`: next quad (+0x14C) and
/// next packet (+0xCDC0). Returns the packet buffer's offset.
pub fn select_buffers_ec824(m: &mut Machine) -> u32 {
    let context = m.gw(CONTEXT);
    let index = m.lw(context.at(4));
    let b = block(m);
    m.sw(
        b.at(0x14c),
        b.wrapping_add(index.wrapping_mul(0xa0).wrapping_add(0xc)),
    );
    let index = m.lw(context.at(4));
    let offset = index.wrapping_mul(0x6600).wrapping_add(0x1c0);
    m.sw(b.at(0x8000).at(0x4dc0), b.wrapping_add(offset));
    offset
}

/// Link the packets from `[+0xCDC0]` to the last one before `free` into
/// the ordering-table entry `table` (the original's lwl/swl sequence: the
/// last packet's next pointer from `table`, `table`'s the first packet;
/// `v1` is the register `lwl` merges into, which matters only for an
/// unaligned `table`) and make `free` the next free packet.
fn link_packets(m: &mut Machine, b: u32, free: u32, table: u32, v1: u32) {
    let packets = b.at(0x8000);
    let start = m.lw(packets.at(0x4dc0));
    if free != start {
        let last = free.wrapping_sub(0x34);
        let next = lwl(m, table.at(2), v1);
        swl(m, last.at(2), next);
        swl(m, table.at(2), start << 8);
        m.sw(packets.at(0x4dc0), free);
    }
}

/// 800EC880: upload the tiles of every frame due by the tick counter
/// (+0x1B8 / ticks per frame): two packets per tile (its two 4x4 halves)
/// at the frame's screen position (tile index % width, / width, from the
/// screen rectangle +0x158), linked into `table`. Returns 1 when the
/// stream ended (or there is nothing to show), else 0.
pub fn upload_tiles_ec880(m: &mut Machine, table: u32) -> u32 {
    let t1 = block(m);
    let frames = m.lw(t1.at(0x1a4));
    if m.lw(frames) == 0 {
        let counter = m.lw(t1.at(0x8000).at(0x4dc4));
        if !lt(m.lw(counter), 0x1f) {
            return 1;
        }
    }
    let frames = m.lw(t1.at(0x1a4));
    let current = lh(m, t1.at(0xa));
    if !lt(current, m.lw(frames)) {
        return 1;
    }
    let per = lh(m, t1.at(8));
    let ticks = m.lw(t1.at(0x1b8));
    let (due, _) = div(m, ticks, per, 0x800e_c914, 0x800e_c92c);
    let entry = frames.wrapping_add(current.wrapping_mul(12).wrapping_add(4));
    m.sw(t1.at(0x1a8), entry);
    let shows = m.lw(entry.at(4));
    let mut packet = m.lw(t1.at(0x8000).at(0x4dc0));
    // v1 at the link: block + 0x8000 here, the frame count after a frame
    let mut v1 = t1.at(0x8000);
    if !lt(due, shows) {
        loop {
            let entry = m.lw(t1.at(0x1a8));
            let x0 = lh(m, t1.at(0x158));
            let y0 = lh(m, t1.at(0x15a));
            let tiles = m.lw(entry);
            let mut list = m.lw(entry.at(8));
            if lt(0, tiles) {
                let b = block(m);
                let mut a1 = packet.at(0x30);
                let mut next = (packet << 8).wrapping_add(0x3400);
                let mut i = 0u32;
                loop {
                    let position = lh(m, list.at(2));
                    let width = lh(m, t1.at(0x1b0));
                    let (_, column) = div(m, position, width, 0x800e_c9c4, 0x800e_c9dc);
                    let x = x0.wrapping_add(column << 2);
                    m.sh(a1.at(0xc), x as u16);
                    m.sh(a1.wrapping_sub(0x28), x as u16);
                    let width = lh(m, t1.at(0x1b0));
                    let (row, _) = div(m, position, width, 0x800e_ca0c, 0x800e_ca24);
                    let y = y0.wrapping_add(row << 3);
                    m.sh(a1.wrapping_sub(0x26), y as u16);
                    m.sh(a1.at(0xe), y.wrapping_add(4) as u16);
                    let image = lh(m, list);
                    let directory = m.lw(b.at(0x1ac));
                    let mut pixels = m.lw(directory.wrapping_add(image << 2).at(0x204));
                    for _ in 0..2 {
                        m.sw(packet, (next >> 8) | 0x0c00_0000);
                        m.sw(a1.wrapping_sub(0x2c), 0xa000_0000);
                        m.sw(a1.wrapping_sub(0x24), 0x0004_0004);
                        m.sw(a1, 0x0100_0000);
                        for k in 0..8u32 {
                            let word = m.lw(pixels);
                            pixels = pixels.at(4);
                            m.sw(packet.wrapping_add(k << 2).at(0x10), word);
                        }
                        a1 = a1.at(0x34);
                        next = next.wrapping_add(0x3400);
                        packet = packet.at(0x34);
                    }
                    i = i.wrapping_add(1);
                    list = list.at(4);
                    if !lt(i, tiles) {
                        break;
                    }
                }
            }
            let current = u32::from(m.lhu(t1.at(0xa))).wrapping_add(1);
            m.sh(t1.at(0xa), current as u16);
            let current = (current as u16 as i16) as i32 as u32;
            let frames = m.lw(t1.at(0x1a4));
            let flip = m.lw(t1);
            m.sw(
                t1.at(0x1a8),
                frames.wrapping_add(current.wrapping_mul(12).wrapping_add(4)),
            );
            let frames = m.lw(t1.at(0x1a4));
            m.sw(t1, 1u32.wrapping_sub(flip));
            let current = lh(m, t1.at(0xa));
            v1 = m.lw(frames);
            if !lt(current, v1) {
                link_packets(m, t1, packet, table, v1);
                return 1;
            }
            let entry = m.lw(t1.at(0x1a8));
            if lt(due, m.lw(entry.at(4))) {
                break;
            }
        }
    }
    link_packets(m, t1, packet, table, v1);
    0
}

/// 800ECBEC: link the frame's quads (+0x14C onwards, +0x1A0 of them) into
/// the ordering-table entry `table`. Returns the next quad, or the count
/// when it is not positive.
pub fn link_quads_ecbec(m: &mut Machine, table: u32) -> u32 {
    let b = block(m);
    let count = m.lw(b.at(0x1a0));
    if !lt(0, count) {
        return count;
    }
    let mut i = 0u32;
    loop {
        i = i.wrapping_add(1);
        let quad = m.lw(b.at(0x14c));
        let head = m.lw(table);
        let tag = m.lw(quad);
        m.sw(quad, (tag & 0xff00_0000) | (head & 0x00ff_ffff));
        let head = m.lw(table);
        let quad = m.lw(b.at(0x14c));
        m.sw(table, (head & 0xff00_0000) | (quad & 0x00ff_ffff));
        let next = m.lw(b.at(0x14c)).at(0x28);
        let count = m.lw(b.at(0x1a0));
        m.sw(b.at(0x14c), next);
        if !lt(i, count) {
            return next;
        }
    }
}

/// 800ECC70: one frame of the stream into `table`: buffers, tile uploads,
/// quads; tick +1; `*frame_out` = the current frame; counter +0xCDC8 + 1.
/// Returns 800EC880's result.
pub fn tick_ecc70(m: &mut Machine, table: u32, frame_out: u32) -> u32 {
    let frame = m.push_frame(0x20);
    let _ = frame;
    let context = m.gw(CONTEXT);
    let index = m.lw(context.at(4));
    let b = block(m);
    m.sw(
        b.at(0x14c),
        b.wrapping_add(index.wrapping_mul(0xa0).wrapping_add(0xc)),
    );
    select_buffers_ec824(m);
    let ended = upload_tiles_ec880(m, table);
    link_quads_ecbec(m, table);
    let b = block(m);
    let ticks = m.lw(b.at(0x1b8)).wrapping_add(1);
    let current = lh(m, b.at(0xa));
    m.sw(b.at(0x1b8), ticks);
    m.sw(frame_out, current);
    let b = block(m).at(0x8000);
    let counter = m.lw(b.at(0x4dc8)).wrapping_add(1);
    m.sw(b.at(0x4dc8), counter);
    m.pop_frame(0x20);
    ended
}
