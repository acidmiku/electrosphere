//! JP overlay 2, part 5: the selection block `[0x80112E10]`
//! (800F78B0..800F9B10): the rank mask, the record selection, the CLUT fade,
//! the five-axis stats chart, the two picture columns and their input.
use super::ov2_p5::{
    MODULE, RECORD, add_prim, call, ctx, framed, hi_s, hub, lbu, lh, lhu, lw, sb, sh, slt, sra, sw,
    sx16,
};
use crate::common::{ENTITIES, GAMEPLAY, loaded_module};
use crate::machine::{Machine, SmallData};

/// `[0x80112E10]`: the selection block: +0 (halfword) a state (5: input
/// locked), +4 a word for 800AFD08, +8 the current 0x18-byte record, +0xC
/// the previous record, +0x10 flags (bit 0 the figures, bit 1 the filled
/// chart), +0x14/+0x15 the shown entries of the two columns, +0x16/+0x17
/// the requested ones, +0x18 and +0x68 two 2x2 SPRT packets, +0xB8 two
/// DR_MODE packets, +0x118 the active column, +0x119 the chosen value,
/// +0x11A/+0x11B the column sizes, +0x120 a snapshot flag, +0x124 the
/// snapshot record.
pub const NET: u32 = 0x8011_2e10;

/// `x / 25` (`sra 3`), `/ 100` (`sra 5`), `/ 400` (`sra 7`) by `mult`.
const M25: u32 = 0x51eb_851f;
/// `x / 375` (`sra 3`).
const M375: u32 = 0x0576_19f1;
/// `x / 625` (`sra 8`).
const M625: u32 = 0x68db_8bad;
/// `x / 5` (`sra 1`), `/ 10` (`sra 2`).
const M10: u32 = 0x6666_6667;
/// `x / 15` (`mfhi + x`, `sra 3`).
const M15: u32 = 0x8888_8889;

/// `lwl rt, off+3(base); lwr rt, off(base)`: the (possibly unaligned) word
/// at `base + off`.
fn ulw(m: &mut Machine, base: u32, off: u32) -> u32 {
    (0..4u32).fold(0, |word, k| word | (lbu(m, base, off + k) << (8 * k)))
}

/// 800F78B0: the rank mask over the missions 1..0x37 (except 0x13, 0x31,
/// 0x36): each mission's best rank over difficulties 0..2 (context +0xC1C,
/// +0xC58, +0xC94 + mission; 0 is none, 5 when all are none). Returns 0 when
/// a best rank is 5, else 0xF with bit 3 cleared by a best rank >= 2, bit 2
/// by >= 3 and bit 1 by >= 4.
pub fn rank_mask(m: &mut Machine) -> u32 {
    let context = ctx(m);
    let mut mask = 0xfu32;
    for mission in 1..0x38u32 {
        if matches!(mission, 0x13 | 0x31 | 0x36) {
            continue;
        }
        let at = context.wrapping_add(mission);
        let mut best = lbu(m, at, 0xc1c);
        if best == 0 {
            best = 5;
        }
        for offset in [0xc58, 0xc94] {
            let rank = lbu(m, at, offset);
            if rank != 0 && slt(rank, best) {
                best = rank;
            }
        }
        if best == 5 {
            return 0;
        }
        if !slt(best, 2) {
            mask &= !8;
        }
        if !slt(best, 3) {
            mask &= !4;
        }
        if !slt(best, 4) {
            mask &= !2;
        }
    }
    mask
}

/// 800F7994: the record selection. Returns 0 at once when 800DEE84(2) is
/// set and `a0` is 1. In hub phase 3 (`[HUB]+0x1C6`): look up the record
/// index 800FE864(mission, hub +0xC0, hub +0x80), open its file (80027338 of
/// name `[hub+0xF4][index]`), set it up (800ED0D4, 800AFD08, 80027380),
/// clear entity 0's word 0 and set 0x1C000 in entity 2's, start the widgets
/// 2 and 8 (800DEE08), make the previous record the snapshot (if one was
/// taken) or the current one, point NET +8 at `[hub+0xB8] + 0x18 * index`
/// and return 1. In the other phases (phase 0 first snapshots the current
/// record into NET +0x124): point NET +8 at the snapshot, run the phase
/// routine `[0x8010984C + 4 * phase](1)` (a call table: 800CE554, 800CE6B0,
/// 800CE6EC, 800CE754, ...) and return 0.
pub fn select_record(m: &mut Machine, a0: u32) -> u32 {
    framed(m, 0x20, |m, _| {
        if call(m, 0x800d_ee84, [2, 0, 0, 0]) != 0 && a0 == 1 {
            return 0;
        }
        let block = hub(m);
        let phase = lh(m, block, 0x1c6);
        if phase != 3 {
            if phase == 0 {
                let net = m.lw(NET);
                let record = lw(m, net, 8);
                let w0 = lw(m, record, 0);
                let w1 = lw(m, record, 4);
                let w2 = lw(m, record, 8);
                let w3 = lw(m, record, 0xc);
                sw(m, net, 0x124, w0);
                sw(m, net, 0x128, w1);
                sw(m, net, 0x12c, w2);
                sw(m, net, 0x130, w3);
                let w4 = lw(m, record, 0x10);
                let w5 = lw(m, record, 0x14);
                sw(m, net, 0x134, w4);
                sw(m, net, 0x138, w5);
                let net = m.lw(NET);
                sw(m, net, 0x120, 1);
            }
            let net = m.lw(NET);
            sw(m, net, 8, net.wrapping_add(0x124));
            let block = hub(m);
            let phase = lh(m, block, 0x1c6);
            // the load phases of `ov2_p1_pages::LOADS` with a0 = 1 (the
            // `jalr` delay slot); phase 3, the `jr ra` stub LOAD_IDLE outside
            // the function map, does nothing (and is skipped above)
            let routine = lw(m, phase << 2, super::ov2_p1_pages::LOADS);
            if routine != super::ov2_p1_pages::LOAD_IDLE {
                call(m, routine, [1, 0, 0, 0]);
            }
            return 0;
        }
        let a1 = lw(m, block, 0xc0);
        let context = ctx(m);
        let a2 = lw(m, block, 0x80);
        let mission = lh(m, context, 0x38c);
        let index = call(m, 0x800f_e864, [mission, a1, a2, 0]);
        let block = hub(m);
        let names = lw(m, block, 0xf4);
        let name = lw(m, names, index << 2);
        let file = call(m, 0x8002_7338, [name, 0, 0, 0]);
        let block = hub(m);
        let a0 = lw(m, file, 0);
        let a1 = lw(m, block, 0xd0);
        let a2 = lw(m, block, 0xd4);
        call(m, 0x800e_d0d4, [a0, a1, a2, 0]);
        let net = m.lw(NET);
        let a0 = lw(m, file, 4);
        let a1 = lw(m, net, 4);
        // a2 is a leftover of the previous call's set-up
        call(m, 0x800a_fd08, [a0, a1, 0, 0]);
        let gameplay = m.gw(GAMEPLAY);
        let net = m.lw(NET);
        let target = lw(m, gameplay, 0x250);
        let word = lw(m, net, 4);
        sw(m, target, 0x20, word);
        let target = lw(m, gameplay, 0x250);
        let a0 = lw(m, target, 0x20);
        call(m, 0x8002_7380, [a0, a0.wrapping_add(4), 6, 0]);
        let entities = m.gw(ENTITIES);
        let first = lw(m, entities, 0);
        sw(m, first, 0, 0);
        let third = lw(m, entities, 8);
        let flags = lw(m, third, 0) | 0x1_c000;
        sw(m, third, 0, flags);
        call(m, 0x800d_ee08, [2, 1, 0, 0x19]);
        call(m, 0x800d_ee08, [8, 1, 0, 0x19]);
        let net = m.lw(NET);
        if lw(m, net, 0x120) != 0 {
            let snapshot = net.wrapping_add(0x124);
            sw(m, net, 0xc, snapshot);
            sw(m, net, 8, snapshot);
            sw(m, net, 0x120, 0);
        } else {
            let current = lw(m, net, 8);
            sw(m, net, 0xc, current);
        }
        let block = hub(m);
        let records = lw(m, block, 0xb8);
        let net = m.lw(NET);
        let offset = ((index << 1).wrapping_add(index)) << 3;
        sw(m, net, 8, records.wrapping_add(offset));
        1
    })
}

/// 800F7BE8 (JP; PAL 800D70AC, changed): when 800DEE84(2) is set, run
/// 801085F8(2) and 80108874(1) and return 1; else return 0.
pub fn fn_800f7be8(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        if call(m, 0x800d_ee84, [2, 0, 0, 0]) == 0 {
            return 0;
        }
        call(m, 0x8010_85f8, [2, 0, 0, 0]);
        call(m, 0x8010_8874, [1, 0, 0, 0]);
        1
    })
}

/// 800F7C24: fade the hub CLUTs: `16 * [HUB]+0xDE` 15-bit colours
/// `base + (target - base) * a0 / 25` per channel (base `[hub+0xD4]`, target
/// `[hub+0xD0]`) into a stack table, then one DR_LOAD packet (0x44 bytes from
/// `[GAMEPLAY]+0x14`, SetDrawLoad with the RECT hub +0xD8, height 1, row
/// hub +0xDA + n) of 16 colours per row, linked into `[GAMEPLAY]+0x10`.
/// Returns 0, or the row count when it is not positive.
pub fn clut_fade(m: &mut Machine, a0: u32) -> u32 {
    framed(m, 0x238, |m, sp| {
        let block = hub(m);
        let mut i = 0u32;
        if slt(0, lh(m, block, 0xde) << 4) {
            loop {
                let t1 = i << 1;
                let base = lw(m, block, 0xd4);
                let target = lw(m, block, 0xd0);
                let from = lhu(m, base, t1);
                let to = lhu(m, target, t1);
                let r0 = from & 0x1f;
                let dr = (to & 0x1f).wrapping_sub(r0).wrapping_mul(a0);
                let hr = hi_s(dr, M25);
                let from = from << 16;
                let g0 = sra(from, 21) & 0x1f;
                let to = to << 16;
                let dg = (sra(to, 21) & 0x1f).wrapping_sub(g0).wrapping_mul(a0);
                let b0 = sra(from, 26) & 0x1f;
                let db = (sra(to, 26) & 0x1f).wrapping_sub(b0).wrapping_mul(a0);
                let hg = hi_s(dg, M25);
                let hb = hi_s(db, M25);
                let r = r0.wrapping_add(sra(hr, 3).wrapping_sub(sra(dr, 31)));
                let g = g0.wrapping_add(sra(hg, 3).wrapping_sub(sra(dg, 31)));
                let b = b0.wrapping_add(sra(hb, 3).wrapping_sub(sra(db, 31)));
                let colour = r | (g << 5) | (b << 10);
                sh(m, sp.wrapping_add(t1), 0x10, colour);
                i = i.wrapping_add(1);
                if !slt(i, lh(m, block, 0xde) << 4) {
                    break;
                }
            }
        }
        let first = hub(m);
        let second = hub(m);
        let w0 = ulw(m, first, 0xd8);
        let w1 = ulw(m, first, 0xdc);
        sw(m, sp, 0x210, w0);
        sw(m, sp, 0x214, w1);
        sh(m, sp, 0x216, 1);
        let rows = lh(m, second, 0xde);
        if !slt(0, rows) {
            return rows;
        }
        let mut row = 0u32;
        loop {
            let gameplay = m.gw(GAMEPLAY);
            let packet = lw(m, gameplay, 0x14);
            let block = hub(m);
            sw(m, gameplay, 0x14, packet.wrapping_add(0x44));
            let y = lhu(m, block, 0xda).wrapping_add(row);
            sh(m, sp, 0x212, y);
            // a2 holds 0x80110000 (a leftover)
            call(m, 0x800a_c788, [packet, sp + 0x210, 0, 0]);
            let mut index = row << 4;
            for k in 0..8u32 {
                let word = lw(m, sp.wrapping_add(index << 1), 0x10);
                index = index.wrapping_add(2);
                sw(m, packet.wrapping_add(k << 2), 0x10, word);
            }
            let gameplay = m.gw(GAMEPLAY);
            let ot = lw(m, gameplay, 0x10);
            let link = (lw(m, packet, 0) & 0xff00_0000) | (lw(m, ot, 0) & 0x00ff_ffff);
            sw(m, packet, 0, link);
            let ot = lw(m, gameplay, 0x10);
            let head = (lw(m, ot, 0) & 0xff00_0000) | (packet & 0x00ff_ffff);
            let block = hub(m);
            sw(m, ot, 0, head);
            row = row.wrapping_add(1);
            if !slt(row, lh(m, block, 0xde)) {
                break 0;
            }
        }
    })
}

/// 800F7EA4: the five-axis stats chart of the record NET +8, faded in from
/// the record NET +0xC (800DEE84(2) == 1: by 800DEEA8(2) / 25), slid in by
/// 800DEEA8(6): the axes and the outline (LINE_F2 twice, grey 0x40) around
/// the centre `[0x8010EB24]` + (0xEC, 0x134), the filled pentagon (five
/// POLY_G3, colour 0xCF8B17) when NET +0x10 bit 1 is set, the figures
/// (`"%2d@"`/`"%2d"` of +8, +6, +0xA in hundredths, `"%5d"` of +0xC and
/// +0x10) when bit 0 is set, then the widgets 6, 1 and 5 (800DEF94) from the
/// record bytes +0x13 and +0x12. Packets from `a0`; returns the next one
/// (`a0` when NET +0x10 is clear).
pub fn stats_chart(m: &mut Machine, a0: u32) -> u32 {
    framed(m, 0xf8, |m, sp| {
        const SHAPE: u32 = 0x8010_eb24;
        let net = m.lw(NET);
        let mut packet = a0;
        if lw(m, net, 0x10) == 0 {
            return packet;
        }
        let to = lw(m, net, 8);
        let from = lw(m, net, 0xc);
        let mut slide = call(m, 0x800d_eea8, [6, 0, 0, 0]);
        if !slt(slide, 0x15) {
            slide = 0x14;
        }
        let d = 0x14u32.wrapping_sub(slide);
        let square = d.wrapping_mul(d);
        let v = (square << 2).wrapping_add(square);
        let v = v.wrapping_add(v << 6);
        let s6 = sra(hi_s(v, M25), 7).wrapping_sub(sra(v, 31));
        let div25 = |x: u32| sra(hi_s(x, M25), 3).wrapping_sub(sra(x, 31));
        // fields: 25 * the bytes +1..+5; coords: the halfwords +6, +8, +0xA,
        // the word +0xC and the halfword +0x10
        let mut fields = [0u32; 5];
        let mut coords = [0u32; 5];
        if call(m, 0x800d_ee84, [2, 0, 0, 0]) == 1 {
            for (at, offset) in [(0x10u32, 0u32), (0x22, 0x12), (0x23, 0x13), (0x24, 0x14)] {
                let byte = lbu(m, to, offset);
                sb(m, sp, at, byte);
            }
            let r = call(m, 0x800d_eea8, [2, 0, 0, 0]);
            for (k, offset) in (1u32..6).enumerate() {
                let base = lbu(m, from, offset);
                let delta = lbu(m, to, offset).wrapping_sub(base).wrapping_mul(r);
                fields[k] = base.wrapping_mul(25).wrapping_add(delta);
            }
            for (k, offset) in [6u32, 8, 0xa, 0xc, 0x10].into_iter().enumerate() {
                let (base, target) = if offset == 0xc {
                    (lw(m, from, offset), lw(m, to, offset))
                } else {
                    (lh(m, from, offset), lh(m, to, offset))
                };
                coords[k] = base.wrapping_add(div25(target.wrapping_sub(base).wrapping_mul(r)));
            }
        } else {
            for (k, offset) in [6u32, 8, 0xa, 0xc, 0x10].into_iter().enumerate() {
                coords[k] = if offset == 0xc {
                    lw(m, to, offset)
                } else {
                    lh(m, to, offset)
                };
            }
            for (k, offset) in (1u32..6).enumerate() {
                fields[k] = lbu(m, to, offset).wrapping_mul(25);
            }
        }
        let scale = |m: &mut Machine, value: u32, offset: u32| {
            let p = sx16(value).wrapping_mul(lh(m, SHAPE, offset));
            sra(hi_s(p, M375), 3).wrapping_sub(sra(p, 31))
        };
        // the stack points sp+0x28 + 8 * slot: the centre, then the axis ends
        // of the fields (slot, field, shape offset)
        let mut points = [(0u32, 0u32); 6];
        let x = lhu(m, SHAPE, 0).wrapping_add(0xec);
        let y = lhu(m, SHAPE, 2).wrapping_add(0x134);
        points[0] = (x & 0xffff, y & 0xffff);
        let axes = [
            (1, 0, 4),
            (2, 1, 8),
            (3, 2, 0x10),
            (4, 3, 0x14),
            (5, 4, 0xc),
        ];
        for (slot, field, offset) in axes {
            let x = scale(m, fields[field], offset).wrapping_add(0xec);
            let y = scale(m, fields[field], offset + 2).wrapping_add(0x134);
            points[slot] = (x & 0xffff, y & 0xffff);
        }
        // the axes: centre to shape point k (LINE_F2 record at sp+0x58)
        let line = sp + 0x58;
        let left = s6.wrapping_add(0xec);
        for k in 1..6u32 {
            sb(m, line, 8, 0x40);
            sb(m, line, 9, 0x40);
            sb(m, line, 0xa, 0x40);
            sb(m, line, 0xb, 0);
            let x0 = lhu(m, SHAPE, 0).wrapping_add(left);
            sh(m, line, 0, x0);
            let y0 = lhu(m, SHAPE, 2).wrapping_add(0x134);
            sh(m, line, 2, y0);
            let x1 = lhu(m, SHAPE, k << 2).wrapping_add(left);
            sh(m, line, 4, x1);
            let y1 = lhu(m, SHAPE, (k << 2) + 2).wrapping_add(0x134);
            let block = hub(m);
            sh(m, line, 6, y1);
            let ot = lw(m, block, 8);
            packet = call(m, 0x800f_6e68, [line, packet, ot, 0]);
        }
        // the outline: shape point k to point k % 5 + 1
        for k in 1..6u32 {
            let start = (k << 2).wrapping_add(SHAPE);
            let x0 = lhu(m, start, 0).wrapping_add(left);
            sh(m, line, 0, x0);
            let y0 = lhu(m, start, 2).wrapping_add(0x134);
            sh(m, line, 2, y0);
            let q = sra(hi_s(k, M10), 1).wrapping_sub(sra(k, 31));
            let next = k.wrapping_sub((q << 2).wrapping_add(q)).wrapping_add(1);
            let end = (next << 2).wrapping_add(SHAPE);
            let x1 = lhu(m, end, 0).wrapping_add(left);
            sh(m, line, 4, x1);
            let y1 = lhu(m, end, 2).wrapping_add(0x134);
            let block = hub(m);
            sh(m, line, 6, y1);
            let ot = lw(m, block, 8);
            packet = call(m, 0x800f_6e68, [line, packet, ot, 0]);
        }
        let net = m.lw(NET);
        if lw(m, net, 0x10) & 2 != 0 {
            // POLY_G3 record at sp+0x68: the centre and two neighbouring axis
            // ends per triangle
            let poly = sp + 0x68;
            for at in [0x10u32, 0x14, 0x18] {
                sb(m, poly, at, 0xcf);
                sb(m, poly, at + 1, 0x8b);
                sb(m, poly, at + 2, 0x17);
            }
            sb(m, poly, 0x13, 0);
            for (a, b) in [(1usize, 2usize), (2, 5), (5, 3), (3, 4), (4, 1)] {
                sh(m, poly, 0, points[0].0.wrapping_add(s6));
                sh(m, poly, 2, points[0].1);
                sh(m, poly, 4, points[a].0.wrapping_add(s6));
                sh(m, poly, 6, points[a].1);
                let block = hub(m);
                sh(m, poly, 8, points[b].0.wrapping_add(s6));
                sh(m, poly, 0xa, points[b].1);
                let ot = lw(m, block, 8);
                packet = call(m, 0x800f_73a4, [poly, packet, ot, 0]);
            }
        }
        let net = m.lw(NET);
        if lw(m, net, 0x10) & 1 != 0 {
            let r = call(m, 0x800d_eea8, [1, 0, 0, 0]);
            let d = 0x19u32.wrapping_sub(r);
            let square = d.wrapping_mul(d);
            let v = (square << 2).wrapping_add(square);
            let v = v.wrapping_add(v << 6).wrapping_neg();
            let s6 = sra(hi_s(v, M625), 8).wrapping_sub(sra(v, 31));
            call(m, 0x800f_35b8, [7, 0, 0, 0]);
            call(m, 0x800f_33f8, [0xffff_ffff, 0, 0, 0]);
            call(m, 0x800f_34c8, [1, 0, 1, 0]);
            call(m, 0x800f_34b0, [1, 0, 0, 0]);
            if call(m, 0x800d_ee60, [1, 0, 0, 0]) == 1 {
                call(m, 0x800f_33d0, [0xe1, 0x5f, 0xa, 0]);
            } else if call(m, 0x800d_ee60, [1, 0, 0, 0]) == 0 {
                call(m, 0x800f_33d0, [0xe5, 0xe5, 0xd5, 0]);
            }
            let s5 = s6.wrapping_add(0x46);
            let s3 = s6.wrapping_add(0x5b);
            for (y, value) in [
                (0x102u32, coords[1]),
                (0x10d, coords[0]),
                (0x118, coords[2]),
            ] {
                call(m, 0x800f_33a8, [s5, y, 0, 0]);
                let whole = sra(hi_s(value, M25), 5).wrapping_sub(sra(value, 31));
                call(m, 0x800f_4ae0, [0x800c_d384, whole, 0, 0]);
                call(m, 0x800f_33a8, [s3, y, 0, 0]);
                let hundred = ((((whole << 1).wrapping_add(whole)) << 3).wrapping_add(whole)) << 2;
                call(
                    m,
                    0x800f_4b40,
                    [0x800c_d38c, value.wrapping_sub(hundred), 0, 0],
                );
            }
            call(m, 0x800f_33a8, [s5, 0x123, 0, 0]);
            call(m, 0x800f_4ae0, [0x800c_d390, coords[3], 0, 0]);
            call(m, 0x800f_33a8, [s6.wrapping_add(0x3c), 0x12e, 0, 0]);
            call(m, 0x800f_4ae0, [0x800c_d390, coords[4], 0, 0]);
        }
        let s0 = call(m, 0x800d_ee60, [1, 0, 0, 0]);
        let a1 = lbu(m, to, 0x13);
        call(m, 0x800d_ef94, [6, a1, 0, 0]);
        call(m, 0x800d_ef94, [1, 1, 0, 0]);
        let a1 = lbu(m, to, 0x12).wrapping_add((s0 << 1).wrapping_add(s0));
        call(m, 0x800d_ef94, [5, a1, 0, 0]);
        packet
    })
}

/// 800F8AC4: set up the selection packets: per column `c` (0, 1) and entry
/// `k` (0, 1) the SPRTs NET +0x18 + 0x28c + 0x14k (CLUT 0xA18) and
/// +0x68 + 0x28c + 0x14k (CLUT 0xA58) (code 0x66, grey 0x80, 0x30x0x58),
/// and the DR_MODE NET +0xB8 + 0xCc (SetDrawMode(0, 0, 0x4C, NULL), set
/// twice). Returns 0.
pub fn init_sprites(m: &mut Machine) -> u32 {
    framed(m, 0x48, |m, sp| {
        for column in 0..2u32 {
            let row = column * 0x28;
            for k in 0..2u32 {
                let at = k * 0x14;
                let net = m.lw(NET);
                let p = net.wrapping_add(row + 0x18).wrapping_add(at);
                sb(m, p, 3, 4);
                sb(m, p, 7, 0x66);
                sb(m, p, 4, 0x80);
                sb(m, p, 5, 0x80);
                sb(m, p, 6, 0x80);
                let net = m.lw(NET);
                sh(m, p, 0xe, 0xa18);
                sh(m, p, 0x10, 0x30);
                sh(m, p, 0x12, 0x58);
                let p = net.wrapping_add(row + 0x68).wrapping_add(at);
                sb(m, p, 3, 4);
                sb(m, p, 7, 0x66);
                sb(m, p, 4, 0x80);
                sb(m, p, 5, 0x80);
                sb(m, p, 6, 0x80);
                sh(m, p, 0x10, 0x30);
                sh(m, p, 0x12, 0x58);
                sh(m, p, 0xe, 0xa58);
                let net = m.lw(NET);
                sw(m, sp, 0x10, 0);
                call(
                    m,
                    0x800a_c7f8,
                    [net.wrapping_add(0xb8 + column * 0xc), 0, 0, 0x4c],
                );
            }
        }
        0
    })
}

/// 800F8C0C: the selection input. The widgets 9 and 10 (800DEE84) report
/// the two columns: idle (0) or done (2) copies the requested entry
/// (+0x16/+0x17) to the shown one (+0x14/+0x15), done also resets the
/// widget (800DEE08(n, 0, 0, 0)); while either moves, return (v0: the last
/// reset's result, the copied byte of column 2, or 2). Else: a press of
/// context +0x12C bits 0..1 toggles the active column +0x118, the arrow
/// widgets 8..0xB are cleared, and (unless NET +0 is 5) context +0x130 bit 3
/// steps the active column's entry up (to its size - 1) and bit 2 down,
/// starting the slide (800DEE08), the arrow (800DEF94) and the sound
/// 8002DB4C(0x3D); then the arrows 7 and 0xE (bit 1: more above, bit 0: more
/// below) and widget 0xF (the active column) are set, and the chosen value
/// 800FEBB8(mission, context +0x38E, 0 or 2, the requested entry) is stored
/// in NET +0x119 and returned (0x800C0000 when the active column is not 0 or
/// 1).
pub fn select_input(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        let left = call(m, 0x800d_ee84, [9, 0, 0, 0]);
        let right = call(m, 0x800d_ee84, [0xa, 0, 0, 0]);
        if left == 0 || left == 2 {
            let net = m.lw(NET);
            let entry = lbu(m, net, 0x16);
            sb(m, net, 0x14, entry);
        }
        let mut v0 = 2;
        if right == 0 || right == 2 {
            let net = m.lw(NET);
            v0 = lbu(m, net, 0x17);
            sb(m, net, 0x15, v0);
        }
        if left == 2 {
            v0 = call(m, 0x800d_ee08, [9, 0, 0, 0]);
        }
        if right == 2 {
            v0 = call(m, 0x800d_ee08, [0xa, 0, 0, 0]);
        }
        if left != 0 || right != 0 {
            return v0;
        }
        let context = ctx(m);
        if lw(m, context, 0x12c) & 3 != 0 {
            let net = m.lw(NET);
            let column = lbu(m, net, 0x118);
            sb(m, net, 0x118, 1u32.wrapping_sub(column));
        }
        for widget in [9, 8, 0xb, 0xa] {
            call(m, 0x800d_ef94, [widget, 0, 0, 0]);
        }
        let net = m.lw(NET);
        if lh(m, net, 0) != 5 {
            let context = ctx(m);
            let pad = lw(m, context, 0x130);
            if pad & 8 != 0 {
                if lbu(m, net, 0x118) == 0 {
                    let limit = lbu(m, net, 0x11a).wrapping_sub(1);
                    let entry = lbu(m, net, 0x14);
                    if slt(entry, limit) {
                        sb(m, net, 0x16, entry.wrapping_add(1));
                        call(m, 0x800d_ee08, [9, 3, 0xa, 0xa]);
                        call(m, 0x800d_ef94, [9, 1, 0, 0]);
                        call(m, 0x8002_db4c, [0x3d, 0, 0, 0]);
                    }
                }
                let net = m.lw(NET);
                if lbu(m, net, 0x118) == 1 {
                    let limit = lbu(m, net, 0x11b).wrapping_sub(1);
                    let entry = lbu(m, net, 0x15);
                    if slt(entry, limit) {
                        sb(m, net, 0x17, entry.wrapping_add(1));
                        call(m, 0x800d_ee08, [0xa, 3, 0xa, 0xa]);
                        call(m, 0x800d_ef94, [0xb, 1, 0, 0]);
                        call(m, 0x8002_db4c, [0x3d, 0, 0, 0]);
                    }
                }
            } else if pad & 4 != 0 {
                if lbu(m, net, 0x118) == 0 {
                    let entry = lbu(m, net, 0x14);
                    if entry != 0 {
                        sb(m, net, 0x16, entry.wrapping_sub(1));
                        call(m, 0x800d_ee08, [9, 1, 0, 0xa]);
                        call(m, 0x800d_ef94, [8, 1, 0, 0]);
                        call(m, 0x8002_db4c, [0x3d, 0, 0, 0]);
                    }
                }
                let net = m.lw(NET);
                if lbu(m, net, 0x118) == 1 {
                    let entry = lbu(m, net, 0x15);
                    if entry != 0 {
                        sb(m, net, 0x17, entry.wrapping_sub(1));
                        call(m, 0x800d_ee08, [0xa, 1, 0, 0xa]);
                        call(m, 0x800d_ef94, [0xa, 1, 0, 0]);
                        call(m, 0x8002_db4c, [0x3d, 0, 0, 0]);
                    }
                }
            }
        }
        for (widget, size, entry) in [(7u32, 0x11au32, 0x14u32), (0xe, 0x11b, 0x15)] {
            let net = m.lw(NET);
            let limit = lbu(m, net, size).wrapping_sub(1);
            let shown = lbu(m, net, entry);
            let mut arrows = u32::from(slt(shown, limit)) << 1;
            if shown != 0 {
                arrows |= 1;
            }
            call(m, 0x800d_ef94, [widget, arrows, 0, 0]);
        }
        let net = m.lw(NET);
        let column = lbu(m, net, 0x118);
        call(m, 0x800d_ef94, [0xf, column, 0, 0]);
        let net = m.lw(NET);
        let (kind, requested) = match lbu(m, net, 0x118) {
            0 => (0, 0x16),
            1 => (2, 0x17),
            _ => return 0x800c_0000,
        };
        let context = ctx(m);
        let a3 = lbu(m, net, requested);
        let mission = lh(m, context, 0x38c);
        let a1 = lh(m, context, 0x38e);
        let value = call(m, 0x800f_ebb8, [mission, a1, kind, a3]);
        let net = m.lw(NET);
        sb(m, net, 0x119, value);
        value
    })
}

/// The texture column (`(v & 3) * 48`) and row (`(v >> 2 & 1) * 88 + 10`)
/// of picture `v`.
fn picture_uv(v: u32) -> (u32, u32) {
    ((v & 3) * 48, ((v >> 2) & 1) * 88 + 0xa)
}

/// The CLUT word of picture `v` (a byte): `((v >> 3) + 0x18) << 6 | 0x1B`.
fn picture_clut(v: u32) -> u32 {
    (((v >> 3) + 0x18) << 6) | 0x1b
}

/// One 0x30x0x58 SPRT (code 0x66, grey 0x80) at `p` at `xy` with texture
/// offset `uv` and CLUT `clut`, linked into the ordering-table word `ot`.
fn sprite(m: &mut Machine, p: u32, ot: u32, xy: (u32, u32), uv: (u32, u32), clut: u32) {
    sb(m, p, 3, 4);
    sb(m, p, 7, 0x66);
    sb(m, p, 4, 0x80);
    sb(m, p, 5, 0x80);
    sb(m, p, 6, 0x80);
    sh(m, p, 0x10, 0x30);
    sh(m, p, 0x12, 0x58);
    sh(m, p, 0xe, clut);
    sh(m, p, 0xa, xy.1);
    sb(m, p, 0xc, uv.0);
    sb(m, p, 0xd, uv.1);
    sh(m, p, 8, xy.0);
    add_prim(m, ot, p);
}

/// The column clip: the drawing area `x`..min(`x` + 0x30, 0x140) by
/// `y`..+0x58 (RECT at `sp + 0x20`) into the DR_AREA `p`, set and linked
/// into `ot` only when `x` < 0x140.
fn column_area(m: &mut Machine, sp: u32, p: u32, ot: u32, x: u32, y: u32) {
    sh(m, sp, 0x20, x);
    sh(m, sp, 0x22, y);
    sh(m, sp, 0x24, 0x30);
    sh(m, sp, 0x26, 0x58);
    let left = sx16(x);
    if !slt(left.wrapping_add(0x30), 0x140) {
        sh(m, sp, 0x24, 0x140u32.wrapping_sub(x));
    }
    if slt(left, 0x140) {
        call(m, 0x800a_8f20, [p, sp + 0x20, 0, 0]);
        add_prim(m, ot, p);
    }
}

/// 800F8FA8: draw the selection (while 800DEE84(8) is set) from the
/// packets `a0` into the ordering-table word `a1 + 4`: the full-screen
/// drawing area (RECT 0x800CD398), the two pictures of each column (entries
/// 800FEBB8(mission, context +0x38E, 0/2, NET +0x14..+0x17), sliding by
/// 800DEEA8(9)/(10) * 48 / 10, swapped while 800DEE84(9)/(10) is 3, the
/// whole set sliding in by 800DEEA8(8) while 800DEE84(8) is 1 or 3) each
/// clipped to its column, a DR_MODE (tpage 0x4C), and three TILE bars
/// (colour 0xE5E5D5, 8 high, width 38 * byte / 15 of the entry
/// `0x8010EAC0 + 4 * NET[0x119]`) through `[GAMEPLAY]+0x14`; stores the
/// entry's byte 3 and NET +0x119 into `[[RECORD]+0x5C]` +0xC/+0xD. Returns
/// the next packet (`a0` when not shown).
pub fn draw_select(m: &mut Machine, a0: u32, a1: u32) -> u32 {
    framed(m, 0x78, |m, sp| {
        let mut packet = a0;
        if call(m, 0x800d_ee84, [8, 0, 0, 0]) == 0 {
            return packet;
        }
        let pick = |m: &mut Machine, kind: u32, offset: u32| {
            let context = ctx(m);
            let net = m.lw(NET);
            let mission = lh(m, context, 0x38c);
            let stage = lh(m, context, 0x38e);
            let entry = lbu(m, net, offset);
            call(m, 0x800f_ebb8, [mission, stage, kind, entry])
        };
        // the stack bytes sp+0x40, +0x41, +0x42 and s6
        let left_shown = pick(m, 0, 0x14) & 0xff;
        let right_shown = pick(m, 2, 0x15) & 0xff;
        let left_next = pick(m, 0, 0x16);
        let right_next = pick(m, 2, 0x17) & 0xff;
        let left_slide = call(m, 0x800d_eea8, [9, 0, 0, 0]);
        let right_slide = call(m, 0x800d_eea8, [0xa, 0, 0, 0]);
        let left_state = call(m, 0x800d_ee84, [9, 0, 0, 0]);
        let right_state = call(m, 0x800d_ee84, [0xa, 0, 0, 0]);
        let fade = call(m, 0x800d_eea8, [8, 0, 0, 0]);
        let state = call(m, 0x800d_ee84, [8, 0, 0, 0]);
        let ot = a1.wrapping_add(4);
        let area = packet;
        packet = packet.wrapping_add(0xc);
        let w0 = ulw(m, 0x800c_d398, 0);
        let w1 = ulw(m, 0x800c_d398, 4);
        sw(m, sp, 0x18, w0);
        sw(m, sp, 0x1c, w1);
        call(m, 0x800a_8f20, [area, sp + 0x18, 0, 0]);
        add_prim(m, ot, area);
        let s5 = match state {
            1 | 3 => {
                let d = 0x19u32.wrapping_sub(fade);
                let k = (d << 2).wrapping_add(d);
                let k = k.wrapping_add(k << 6);
                let v = d.wrapping_mul(k);
                sra(hi_s(v, M625), 8).wrapping_sub(sra(v, 31))
            }
            // 2: 0; other states keep the caller's s5 (0 in the oracle)
            _ => 0,
        };
        let div10 = |x: u32| sra(hi_s(x, M10), 2).wrapping_sub(sra(x, 31));
        // column 1 (y 0x44): CLUT of the requested entry for both pictures
        let offset = div10(((left_slide << 1).wrapping_add(left_slide)) << 4);
        let next = left_next & 0xff;
        let (first, second) = if left_state == 3 {
            (next, left_shown)
        } else {
            (left_shown, next)
        };
        let clut = picture_clut(next);
        for (source, x) in [(first, 0xeau32), (second, 0xba)] {
            let p = packet;
            packet = packet.wrapping_add(0x14);
            let x = offset.wrapping_add(s5.wrapping_add(x));
            sprite(m, p, ot, (x, 0x44), picture_uv(source), clut);
        }
        let column_x = s5.wrapping_add(0xea);
        let p = packet;
        packet = packet.wrapping_add(0xc);
        column_area(m, sp, p, ot, column_x, 0x44);
        // column 2 (y 0xB8): while swapped, the texture column also gains s5
        let offset = div10(((right_slide << 1).wrapping_add(right_slide)) << 4);
        let (first, second, bias) = if right_state == 3 {
            (right_next, right_shown, s5)
        } else {
            (right_shown, right_next, 0)
        };
        for (source, x) in [(first, column_x), (second, s5.wrapping_add(0xba))] {
            let p = packet;
            packet = packet.wrapping_add(0x14);
            let (u, v) = picture_uv(source);
            let xy = (offset.wrapping_add(x), 0xb8);
            sprite(
                m,
                p,
                ot,
                xy,
                (u.wrapping_add(bias), v),
                picture_clut(source),
            );
        }
        let p = packet;
        packet = packet.wrapping_add(0xc);
        column_area(m, sp, p, ot, s5.wrapping_add(0xea), 0xb8);
        let mode = packet;
        packet = packet.wrapping_add(0xc);
        sw(m, sp, 0x10, 0);
        call(m, 0x800a_c7f8, [mode, 0, 0, 0x4c]);
        let link = (lw(m, mode, 0) & 0xff00_0000) | (lw(m, ot, 0) & 0x00ff_ffff);
        sw(m, mode, 0, link);
        let word = lw(m, ot, 0);
        let net = m.lw(NET);
        sw(m, ot, 0, (word & 0xff00_0000) | (mode & 0x00ff_ffff));
        // the three bars: TILE record at sp+0x28
        let choice = lbu(m, net, 0x119);
        let bar = sp + 0x28;
        let x = s5.wrapping_add(0x10a);
        sb(m, sp, 0x30, 0xe5);
        sb(m, sp, 0x31, 0xe5);
        sb(m, sp, 0x32, 0xd5);
        sh(m, sp, 0x2a, 0x13a);
        sb(m, sp, 0x33, 0);
        sh(m, sp, 0x28, x);
        let entry = (choice << 2).wrapping_add(0x8010_eac0);
        let mut gameplay = m.gw(GAMEPLAY);
        let mut record = 0;
        for (k, y) in [(0u32, 0x13au32), (1, 0x14d), (2, 0x160)] {
            if k != 0 {
                sh(m, sp, 0x28, x);
                sh(m, sp, 0x2a, y);
            }
            let value = lbu(m, entry, k);
            let v = value * 38;
            let width = sra(hi_s(v, M15).wrapping_add(v), 3).wrapping_sub(sra(v, 31));
            sh(m, sp, 0x2e, 8);
            sh(m, sp, 0x2c, width);
            let p = lw(m, gameplay, 0x14);
            let next = call(m, 0x800f_7180, [bar, p, ot, 0]);
            gameplay = m.gw(GAMEPLAY);
            if k == 2 {
                record = m.lw(RECORD);
            }
            sw(m, gameplay, 0x14, next);
        }
        let current = lw(m, record, 0x5c);
        let class = lbu(m, entry, 3);
        sb(m, current, 0xc, class);
        let record = m.lw(RECORD);
        let net = m.lw(NET);
        let current = lw(m, record, 0x5c);
        let choice = lbu(m, net, 0x119);
        sb(m, current, 0xd, choice);
        packet
    })
}

/// The routines of this file by JP address, while module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    let [a0, a1, _, _] = args;
    Some(match target {
        0x800f_78b0 => rank_mask(m),
        0x800f_7994 => select_record(m, a0),
        0x800f_7be8 => fn_800f7be8(m),
        0x800f_7c24 => clut_fade(m, a0),
        0x800f_7ea4 => stats_chart(m, a0),
        0x800f_8ac4 => init_sprites(m),
        0x800f_8c0c => select_input(m),
        0x800f_8fa8 => draw_select(m, a0, a1),
        _ => return None,
    })
}
