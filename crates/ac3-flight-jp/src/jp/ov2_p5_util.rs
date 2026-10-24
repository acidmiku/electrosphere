//! JP overlay 2, part 5: the primitive builders (800F6DA4..800F74C8) and
//! the stream block `[0x80112E0C]` (800F74C8..800F78B0).
//!
//! The builders fill one GPU packet from a source record and link it into
//! the ordering-table word `a2`; each returns the next free packet. The
//! "twice" variants draw the record a second time one line lower.
use super::ov2_p5::{
    MODULE, alloc, call, ctx, framed, hi_s, lbu, lh, lhu, lw, sb, sh, slt, sra, sw,
};
use crate::common::loaded_module;
use crate::machine::Machine;

/// `[0x80112E0C]`: the stream block (0x101C bytes): +0 flags (bit 0
/// playing, bit 1 paused), +4 the playing entry, +8 the requested entry,
/// +0xC, +0x10, +0x14, +0x18 the decoded-data buffers, +0x1018 the entry
/// table (12-byte records).
pub const STREAM: u32 = 0x8011_2e0c;

/// The tail of every builder: link packet `p` (its word 0 read earlier as
/// `head`) into the ordering-table word `ot`.
fn link(m: &mut Machine, head: u32, ot: u32, p: u32) {
    let word = lw(m, ot, 0);
    sw(m, p, 0, (head & 0xff00_0000) | (word & 0x00ff_ffff));
    let word = lw(m, ot, 0);
    sw(m, ot, 0, (word & 0xff00_0000) | (p & 0x00ff_ffff));
}

/// Copy the bytes `src+from..` to `p+to..` (`n` of them, in order).
fn bytes(m: &mut Machine, src: u32, from: u32, p: u32, to: u32, n: u32) {
    for k in 0..n {
        let c = lbu(m, src, from + k);
        sb(m, p, to + k, c);
    }
}

/// Copy the halfwords `src+from..` to `p+to..` (`n` of them, in order).
fn halves(m: &mut Machine, src: u32, from: u32, p: u32, to: u32, n: u32) {
    for k in 0..n {
        let v = lhu(m, src, from + 2 * k);
        sh(m, p, to + 2 * k, v);
    }
}

/// 800F6DA4: a LINE_F2 packet (code 0x40, 0x42 semi-transparent) from the
/// record `a0` (+0 x0, +2 y0, +4 x1, +6 y1, +8 RGB, +0xB semi-transparency
/// flag) at `a1`, linked into `a2`.
pub fn line_f2(m: &mut Machine, a0: u32, a1: u32, a2: u32) -> u32 {
    flat(m, a0, a1, a2, 0x40)
}

/// 800F7180: a TILE packet (code 0x60, 0x62 semi-transparent) from the
/// record `a0` (+0 x, +2 y, +4 w, +6 h, +8 RGB, +0xB flag).
pub fn tile(m: &mut Machine, a0: u32, a1: u32, a2: u32) -> u32 {
    flat(m, a0, a1, a2, 0x60)
}

/// The LINE_F2/TILE body (code `code`, `code | 2` semi-transparent).
fn flat(m: &mut Machine, src: u32, p: u32, ot: u32, code: u32) -> u32 {
    sb(m, p, 3, 3);
    sb(m, p, 7, code);
    let code = if lbu(m, src, 0xb) != 0 {
        code | 2
    } else {
        code
    };
    sb(m, p, 7, code);
    bytes(m, src, 8, p, 4, 3);
    halves(m, src, 0, p, 8, 3);
    let v = lhu(m, src, 6);
    let head = lw(m, p, 0);
    sh(m, p, 0xe, v);
    link(m, head, ot, p);
    p.wrapping_add(0x10)
}

/// 800F6E68: [`line_f2`] twice, the second one line lower (the record's
/// y0 and y1 stay incremented).
pub fn line_f2_twice(m: &mut Machine, a0: u32, a1: u32, a2: u32) -> u32 {
    framed(m, 0x20, |m, _| {
        let next = line_f2(m, a0, a1, a2);
        let y0 = lhu(m, a0, 2).wrapping_add(1);
        let y1 = lhu(m, a0, 6).wrapping_add(1);
        sh(m, a0, 2, y0);
        sh(m, a0, 6, y1);
        line_f2(m, a0, next, a2)
    })
}

/// The LINE_F3/LINE_F4 body: `points` points, the RGB and flag after them,
/// the terminator word at the end.
fn polyline(m: &mut Machine, src: u32, p: u32, ot: u32, points: u32) -> u32 {
    let (len, code) = if points == 3 { (5, 0x48) } else { (6, 0x4c) };
    let colour = 4 * points;
    sb(m, p, 3, len);
    sb(m, p, 7, code);
    sw(m, p, 8 + colour, 0x5555_5555);
    let code = if lbu(m, src, colour + 3) != 0 {
        lbu(m, p, 7) | 2
    } else {
        lbu(m, p, 7) & 0xfd
    };
    sb(m, p, 7, code);
    let c = lbu(m, src, colour);
    let head = lw(m, p, 0);
    sb(m, p, 4, c);
    bytes(m, src, colour + 1, p, 5, 2);
    halves(m, src, 0, p, 8, 2 * points);
    link(m, head, ot, p);
    p.wrapping_add(0xc + colour)
}

/// 800F6EC0: a LINE_F3 packet (code 0x48, bit 1 semi-transparency) from
/// the record `a0` (three points +0..+0xA, RGB +0xC, flag +0xF).
pub fn line_f3(m: &mut Machine, a0: u32, a1: u32, a2: u32) -> u32 {
    polyline(m, a0, a1, a2, 3)
}

/// 800F7014: a LINE_F4 packet (code 0x4C, bit 1 semi-transparency) from
/// the record `a0` (four points +0..+0xE, RGB +0x10, flag +0x13).
pub fn line_f4(m: &mut Machine, a0: u32, a1: u32, a2: u32) -> u32 {
    polyline(m, a0, a1, a2, 4)
}

/// 800F6FB0 (LINE_F3) and 800F711C (LINE_F4): the line twice, the second
/// one line lower (y0, y1, y2).
fn polyline_twice(m: &mut Machine, a0: u32, a1: u32, a2: u32, points: u32) -> u32 {
    framed(m, 0x20, |m, _| {
        let next = polyline(m, a0, a1, a2, points);
        let y0 = lhu(m, a0, 2).wrapping_add(1);
        let y2 = lhu(m, a0, 0xa);
        sh(m, a0, 2, y0);
        let y1 = lhu(m, a0, 6);
        sh(m, a0, 0xa, y2.wrapping_add(1));
        sh(m, a0, 6, y1.wrapping_add(1));
        polyline(m, a0, next, a2, points)
    })
}

/// 800F6FB0: [`line_f3`] twice, the second one line lower.
pub fn line_f3_twice(m: &mut Machine, a0: u32, a1: u32, a2: u32) -> u32 {
    polyline_twice(m, a0, a1, a2, 3)
}

/// 800F711C: [`line_f4`] twice, the second one line lower.
pub fn line_f4_twice(m: &mut Machine, a0: u32, a1: u32, a2: u32) -> u32 {
    polyline_twice(m, a0, a1, a2, 4)
}

/// The POLY_G3/POLY_G4 body: `vertices` RGB records at +0x10 + 4n, points at
/// +0 + 4n, the flag at +0x13.
fn gouraud(m: &mut Machine, src: u32, p: u32, ot: u32, vertices: u32) -> u32 {
    let (len, code) = if vertices == 3 { (6, 0x30) } else { (8, 0x38) };
    sb(m, p, 3, len);
    sb(m, p, 7, code);
    let code = if lbu(m, src, 0x13) != 0 {
        code | 2
    } else {
        code
    };
    sb(m, p, 7, code);
    for vertex in 0..vertices {
        bytes(m, src, 0x10 + 4 * vertex, p, 4 + 8 * vertex, 3);
    }
    for vertex in 0..vertices - 1 {
        halves(m, src, 4 * vertex, p, 8 + 8 * vertex, 2);
    }
    let last = 4 * (vertices - 1);
    let x = lhu(m, src, last);
    sh(m, p, 8 + 2 * last, x);
    let y = lhu(m, src, last + 2);
    let head = lw(m, p, 0);
    sh(m, p, 8 + 2 * last + 2, y);
    link(m, head, ot, p);
    p.wrapping_add(4 + 8 * vertices)
}

/// 800F7244: a POLY_G4 packet (code 0x38, 0x3A semi-transparent) from the
/// record `a0` (four points +0..+0xE, four RGB at +0x10/+0x14/+0x18/+0x1C,
/// flag +0x13).
pub fn poly_g4(m: &mut Machine, a0: u32, a1: u32, a2: u32) -> u32 {
    gouraud(m, a0, a1, a2, 4)
}

/// 800F73A4: a POLY_G3 packet (code 0x30, 0x32 semi-transparent) from the
/// record `a0` (three points +0..+0xA, three RGB at +0x10/+0x14/+0x18,
/// flag +0x13).
pub fn poly_g3(m: &mut Machine, a0: u32, a1: u32, a2: u32) -> u32 {
    gouraud(m, a0, a1, a2, 3)
}

// ---- the stream block -----------------------------------------------------------

/// 800F74C8: allocate the stream block (0x101C bytes) and reset it.
/// Returns it.
pub fn stream_create(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let block = alloc(m, 0x101c);
        sw(m, block, 0xc, 0x3fff);
        sw(m, block, 4, 0);
        sw(m, block, 8, 0);
        sw(m, block, 0, 0);
        sw(m, block, 0x10, 0xff);
        sw(m, block, 0x14, 0);
        m.sw(STREAM, block);
        block
    })
}

/// 800F7514 (JP; PAL 800D0C58, changed): set the stream entry table
/// (block +0x1018). Returns the block.
pub fn stream_set_table(m: &mut Machine, a0: u32) -> u32 {
    let block = m.lw(STREAM);
    sw(m, block, 0x1018, a0);
    block
}

/// Start entry `entry` of the stream table (8002C8D8(record, 1, 0)).
fn stream_start(m: &mut Machine, entry: u32) {
    let block = m.lw(STREAM);
    let table = lw(m, block, 0x1018);
    let record = table.wrapping_add(((entry << 1).wrapping_add(entry)) << 2);
    call(m, 0x8002_c8d8, [record, 1, 0, 0]);
}

/// 800F7524: the stream command `a0` with entry `a1` (1 play, 2 pause or
/// resume, 3 stop; 0 keep playing: restart the requested entry when the CD
/// is idle). Returns the original's last `v0`.
pub fn stream_command(m: &mut Machine, a0: u32, a1: u32) -> u32 {
    framed(m, 0x28, |m, _| {
        let (op, entry) = (a0, a1);
        let s2 = call(m, 0x8002_c580, [1, 0, 0, 0]);
        let v1 = m.lw(STREAM);
        sw(m, v1, 8, entry);
        match op {
            2 => {
                let paused = lw(m, v1, 0) & 2 != 0;
                call(m, 0x8002_c6f4, [u32::from(!paused), 0, 0, 0]);
            }
            1 => {
                if s2 == 0 {
                    stream_start(m, entry);
                    let v1 = m.lw(STREAM);
                    let flags = lw(m, v1, 0) | 1;
                    let requested = lw(m, v1, 8);
                    sw(m, v1, 4, requested);
                    sw(m, v1, 0, flags);
                }
            }
            3 => {
                call(m, 0x8002_c71c, [0, 0, 0, 0]);
                let a0 = m.lw(STREAM);
                let flags = lw(m, a0, 0) & !1 & !2;
                sw(m, a0, 0, flags);
            }
            _ => {}
        }
        let a0 = m.lw(STREAM);
        let playing = lw(m, a0, 4);
        let requested = lw(m, a0, 8);
        let below = u32::from(slt(s2, 2));
        if playing == requested {
            if s2 != 0 {
                return below;
            }
            if lw(m, a0, 0) & 1 == 0 {
                return below;
            }
        }
        if below == 0 {
            return 0x800c_0000;
        }
        let context = ctx(m);
        let busy = lw(m, context, 0x128) & 0xc;
        if busy != 0 {
            return busy;
        }
        if op != 0 {
            return 0;
        }
        call(m, 0x8002_c71c, [0, 0, 0, 0]);
        let v1 = m.lw(STREAM);
        let requested = lw(m, v1, 8);
        stream_start(m, requested);
        let a0 = m.lw(STREAM);
        let flags = lw(m, a0, 0) | 1;
        let requested = lw(m, a0, 8);
        sw(m, a0, 4, requested);
        let flags = flags & !2;
        sw(m, a0, 0, flags);
        flags
    })
}

/// 800F770C (JP; PAL 800D0D30, changed): read the SPU decoded data into
/// the stream block (SpuReadDecodedData(block + 0x18, 5), then
/// SpuIsTransferCompleted(1)) and return half `a0` of the 0x200-sample
/// buffer the SPU is not writing (0 left, 1 right; other `a0`: 1).
pub fn decoded_half(m: &mut Machine, a0: u32) -> u32 {
    framed(m, 0x20, |m, _| {
        let block = m.lw(STREAM);
        let s0 = call(m, 0x800a_0e88, [block.wrapping_add(0x18), 5, 0, 0]);
        call(m, 0x800a_1308, [1, 0, 0, 0]);
        let block = m.lw(STREAM);
        let (left, right) = if s0 == 0 {
            (block.wrapping_add(0xe0), block.wrapping_add(0x4e0))
        } else {
            (block.wrapping_add(0x18), block.wrapping_add(0x418))
        };
        match a0 {
            0 => left,
            1 => right,
            _ => 1,
        }
    })
}

/// 800F77A4 (JP; PAL 800D0DB8, exact): the stream level: the mean absolute
/// value of the first 100 samples of both decoded halves, averaged.
pub fn stream_level(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        let mut s1 = decoded_half(m, 0);
        let mut a1 = decoded_half(m, 1);
        let (mut s2, mut s0) = (0u32, 0u32);
        for _ in 0..100 {
            let v0 = lh(m, s1, 0);
            s1 = s1.wrapping_add(2);
            let v1 = lh(m, a1, 0);
            a1 = a1.wrapping_add(2);
            s2 = s2.wrapping_add(if (v0 as i32) < 0 {
                v0.wrapping_neg()
            } else {
                v0
            });
            s0 = s0.wrapping_add(if (v1 as i32) < 0 {
                v1.wrapping_neg()
            } else {
                v1
            });
        }
        const M100: u32 = 0x51eb_851f;
        let s2 = sra(hi_s(s2, M100), 5).wrapping_sub(sra(s2, 31));
        let s0 = sra(hi_s(s0, M100), 5).wrapping_sub(sra(s0, 31));
        let v0 = s2.wrapping_add(s0);
        let v0 = v0.wrapping_add(v0 >> 31);
        sra(v0 << 15, 16)
    })
}

/// 800F787C: the first halfword of the playing stream entry.
pub fn stream_entry_code(m: &mut Machine) -> u32 {
    let block = m.lw(STREAM);
    let playing = lw(m, block, 4);
    let table = lw(m, block, 0x1018);
    let record = (((playing << 1).wrapping_add(playing)) << 2).wrapping_add(table);
    lh(m, record, 0)
}

/// The routines of this file by JP address, while module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    let [a0, a1, a2, _] = args;
    Some(match target {
        0x800f_6da4 => line_f2(m, a0, a1, a2),
        0x800f_6e68 => line_f2_twice(m, a0, a1, a2),
        0x800f_6ec0 => line_f3(m, a0, a1, a2),
        0x800f_6fb0 => line_f3_twice(m, a0, a1, a2),
        0x800f_7014 => line_f4(m, a0, a1, a2),
        0x800f_711c => line_f4_twice(m, a0, a1, a2),
        0x800f_7180 => tile(m, a0, a1, a2),
        0x800f_7244 => poly_g4(m, a0, a1, a2),
        0x800f_73a4 => poly_g3(m, a0, a1, a2),
        0x800f_74c8 => stream_create(m),
        0x800f_7514 => stream_set_table(m, a0),
        0x800f_7524 => stream_command(m, a0, a1),
        0x800f_770c => decoded_half(m, a0),
        0x800f_77a4 => stream_level(m),
        0x800f_787c => stream_entry_code(m),
        _ => return None,
    })
}
