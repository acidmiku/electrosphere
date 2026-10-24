//! Resident game routines shared by several flight subsystems.
//!
//! Each routine has a typed translation and a uniform entry point
//! `f_XXXXXXXX(m, [a0, a1, a2, a3]) -> v0` (the original register ABI; the
//! signature confirmed from the instructions is on each entry). Routines
//! whose result is a memory write return the value the original leaves in
//! `v0`, which callers ignore. [`dispatch`] claims every routine here.
//!
//! Angles in this game code are 14-bit (0x4000 per turn) and use the 1.14
//! sine table at 80010498 (0x5000 halfwords: cos(a) = sin(a + 0x1000)).
//!
//! Also here: the sound channel reset tree (80030248 and its callees), the
//! CD request queue resets (8002C280, 8002C2F4, 8002C6A8), entity
//! activation (8004CB04..8004D6BC) and the player placement/camera target
//! routines (800710FC, 8006D7DC, 80077D54, 800761FC), which the
//! initialization, loading, mission phase and script subsystems all reach.
use crate::{
    GP, common,
    machine::Fault,
    machine::{Machine, SmallData},
    sdk,
};
use ac3_gte::reg;

/// 80010498: 1.14 sine table, 16384 entries per turn plus a quarter.
pub const SINE_TABLE: u32 = 0x8001_0498;
/// 8001A49C: arctangent table, 2049 halfwords (14-bit angles over 0..=1).
pub const ATAN_TABLE: u32 = 0x8001_a49c;
/// 8001B4A0: arcsine table, 0x4001 halfwords (1.14 input, 14-bit angles).
pub const ASIN_TABLE: u32 = 0x8001_b4a0;
/// 800234A4: eight octant base angles copied to the stack by 80030B10.
pub const OCTANT_TABLE: u32 = 0x8002_34a4;
/// 800C35F8: 24 channel records of 20 bytes written by 8002DAA0.
pub const CHANNELS: u32 = 0x800c_35f8;
/// 800C4B36: byte, index of the current target entity (0xFF: none).
pub const TARGET_INDEX: u32 = 0x800c_4b36;
/// 800C4D98: word flags per entity index.
pub const ENTITY_FLAGS: u32 = 0x800c_4d98;
/// 800BAEB4 (`[gp+0x4C0]`): mode byte tested by 8003A0F8.
pub const MODE_BYTE: u32 = GP + 0x4c0;
/// 800B4C54: SquareRoot0 mantissa table (shared with the SDK).
const SQRT_TABLE: u32 = 0x800b_4c54;

/// `break` reached: no continuation is translated.
fn trap(m: &mut Machine, address: u32) {
    m.raise(Fault::Untranslated { address });
}

/// MIPS `div` low word behind the compiler's `break 7`/`break 6` checks.
fn checked_div(m: &mut Machine, n: i32, d: i32, zero_break: u32, overflow_break: u32) -> i32 {
    if d == 0 {
        trap(m, zero_break);
        return if n < 0 { 1 } else { -1 };
    }
    if d == -1 && n == i32::MIN {
        trap(m, overflow_break);
        return i32::MIN;
    }
    n / d
}

fn at(base: u32, offset: u32) -> u32 {
    base.wrapping_add(offset)
}

/// Sign-extended entry `index` of the 1.14 sine table.
fn sine(m: &mut Machine, index: u32) -> u32 {
    m.lh(SINE_TABLE.wrapping_add(index << 1)) as i32 as u32
}

fn sra(value: u32, shift: u32) -> u32 {
    ((value as i32) >> shift) as u32
}

fn mac(m: &mut Machine, index: usize) -> u32 {
    m.gte.mfc2(reg::MAC1 + index)
}

fn ir(m: &mut Machine, index: usize, value: u32) {
    m.gte.mtc2(reg::IR0 + index, value);
}

/// `GPF0`: GPF with sf=0, lm=0.
fn gpf0(m: &mut Machine) {
    m.gte.gpf(false, false);
}

fn store(m: &mut Machine, base: u32, offset: u32, value: u32) {
    m.sh(at(base, offset), value as u16);
}

/// 8002DAA0 (channel, a, b) -> channel, or 0xFF when `channel >= 24`
/// (unsigned). Record `800C35F8 + 20 * channel`: +4 = a, +6 = b (each
/// replaced by 0x3FFF when its signed halfword is >= 0x4000), then byte +1
/// gains 0x10.
pub fn set_channel_levels(m: &mut Machine, channel: u32, a: u32, b: u32) -> u32 {
    if channel >= 0x18 {
        return 0xff;
    }
    let a = if (a as u16 as i16) < 0x4000 {
        a
    } else {
        0x3fff
    };
    let b = if (b as u16 as i16) < 0x4000 {
        b
    } else {
        0x3fff
    };
    let record = CHANNELS.wrapping_add(channel.wrapping_mul(20));
    let flags = m.lbu(at(record, 1));
    m.sh(at(record, 4), a as u16);
    m.sh(at(record, 6), b as u16);
    m.sb(at(record, 1), flags | 0x10);
    channel
}

/// Store offsets of 80030534/800306AC in their common store order:
/// cx*sy, cx*sz, cx*cz, cy*cx, -sx>>2, sx*(sy*cz)-cy*sz, sx*(cy*sz)-sy*cz,
/// sy*sz+sx*(cy*cz), cy*cz+sx*(sy*sz).
type YxzOffsets = [u32; 9];

/// Shared body of 80030534/800306AC: the YXZ rotation of three 14-bit
/// angles (`angles` halfwords x, y, z; `& 0x3FFF`) as a 4.12 matrix, built
/// with six GPF (sf=0) in the original register flow. Products `>> 16`,
/// cross terms pre-shifted `>> 14`; GPF2/GPF5 reuse IR3 (and GPF5 IR2) left
/// by the previous GPF. Returns the leftover `v0` (MAC1 of the last GPF).
fn rotation_yxz_4_12(m: &mut Machine, angles: u32, out: u32, o: &YxzOffsets) -> u32 {
    let rx = u32::from(m.lhu(angles)) & 0x3fff;
    let rz = u32::from(m.lhu(at(angles, 4))) & 0x3fff;
    let ry = u32::from(m.lhu(at(angles, 2))) & 0x3fff;
    let cx = sine(m, rx + 0x1000);
    let cz = sine(m, rz + 0x1000);
    let sy = sine(m, ry);
    let sz = sine(m, rz);
    ir(m, 0, cx);
    ir(m, 1, sy);
    ir(m, 2, sz);
    ir(m, 3, cz);
    let sx = sine(m, rx);
    gpf0(m);
    let cy = sine(m, ry + 0x1000);
    let cx_sy = mac(m, 0);
    let cx_sz = mac(m, 1);
    let cx_cz = mac(m, 2);
    ir(m, 0, cy);
    ir(m, 1, sz);
    ir(m, 2, cz);
    ir(m, 3, cx);
    store(m, out, o[0], sra(cx_sy, 16));
    gpf0(m);
    store(m, out, o[1], sra(cx_sz, 16));
    store(m, out, o[2], sra(cx_cz, 16));
    let cy_sz = mac(m, 0);
    let cy_cz = mac(m, 1);
    let cy_cx = mac(m, 2);
    ir(m, 0, sy);
    ir(m, 1, cz);
    ir(m, 2, sz);
    store(m, out, o[3], sra(cy_cx, 16));
    gpf0(m);
    let cy_cz14 = sra(cy_cz, 14);
    store(m, out, o[4], sra(sx.wrapping_neg(), 2));
    let cy_sz14 = sra(cy_sz, 14);
    let sy_cz = mac(m, 0);
    let sy_sz = mac(m, 1);
    ir(m, 0, sx);
    ir(m, 1, sra(sy_cz, 14));
    ir(m, 2, cy_sz14);
    ir(m, 3, cy_cz14);
    gpf0(m);
    let p = mac(m, 0);
    let q = mac(m, 1);
    let r = mac(m, 2);
    ir(m, 0, sx);
    ir(m, 1, sra(sy_sz, 14));
    store(m, out, o[5], sra(p.wrapping_sub(cy_sz), 16));
    gpf0(m);
    store(m, out, o[6], sra(q.wrapping_sub(sy_cz), 16));
    store(m, out, o[7], sra(sy_sz.wrapping_add(r), 16));
    let s = mac(m, 0);
    store(m, out, o[8], sra(cy_cz.wrapping_add(s), 16));
    s
}

/// 80030534 (angles: *const [i16; 3], out: *mut MATRIX): 4.12 YXZ rotation
/// (row-major: m00 +0 .. m22 +0x10) of 14-bit angles.
pub fn rotation_yxz(m: &mut Machine, angles: u32, out: u32) -> u32 {
    rotation_yxz_4_12(
        m,
        angles,
        out,
        &[0x4, 0x6, 0x8, 0x10, 0xa, 0x2, 0xc, 0xe, 0x0],
    )
}

/// 800306AC (angles, out): the transpose of [`rotation_yxz`] (same values,
/// mirrored store offsets).
pub fn rotation_yxz_transposed(m: &mut Machine, angles: u32, out: u32) -> u32 {
    rotation_yxz_4_12(
        m,
        angles,
        out,
        &[0xc, 0x2, 0x8, 0x10, 0xe, 0x6, 0x4, 0xa, 0x0],
    )
}

/// 80030824 (angles, out): the YXZ rotation of [`rotation_yxz`] as a 1.14
/// matrix (every product `>> 14`; m12 = -sx unshifted). Returns the
/// leftover `v0`.
pub fn rotation_yxz_q14(m: &mut Machine, angles: u32, out: u32) -> u32 {
    let rx = u32::from(m.lhu(angles)) & 0x3fff;
    let rz = u32::from(m.lhu(at(angles, 4))) & 0x3fff;
    let ry = u32::from(m.lhu(at(angles, 2))) & 0x3fff;
    let cx = sine(m, rx + 0x1000);
    let cz = sine(m, rz + 0x1000);
    let sy = sine(m, ry);
    let sz = sine(m, rz);
    ir(m, 0, cx);
    ir(m, 1, sy);
    ir(m, 2, sz);
    ir(m, 3, cz);
    let sx = sine(m, rx);
    gpf0(m);
    let cy = sine(m, ry + 0x1000);
    let cx_sy = mac(m, 0);
    let cx_sz = mac(m, 1);
    let cx_cz = mac(m, 2);
    ir(m, 0, cy);
    ir(m, 1, sz);
    ir(m, 2, cz);
    ir(m, 3, cx);
    store(m, out, 0x4, sra(cx_sy, 14));
    gpf0(m);
    store(m, out, 0x6, sra(cx_sz, 14));
    store(m, out, 0x8, sra(cx_cz, 14));
    let cy_sz = mac(m, 0);
    let cy_cz = mac(m, 1);
    let cy_cx = mac(m, 2);
    ir(m, 0, sy);
    ir(m, 1, cz);
    ir(m, 2, sz);
    store(m, out, 0x10, sra(cy_cx, 14));
    gpf0(m);
    let cy_cz14 = sra(cy_cz, 14);
    store(m, out, 0xa, sx.wrapping_neg());
    let cy_sz14 = sra(cy_sz, 14);
    let sy_cz = mac(m, 0);
    let sy_sz = mac(m, 1);
    ir(m, 0, sx);
    ir(m, 1, sra(sy_cz, 14));
    ir(m, 2, cy_sz14);
    ir(m, 3, cy_cz14);
    gpf0(m);
    let p = mac(m, 0);
    let q = mac(m, 1);
    let r = mac(m, 2);
    ir(m, 0, sx);
    ir(m, 1, sra(sy_sz, 14));
    store(m, out, 0x2, sra(p.wrapping_sub(cy_sz), 14));
    gpf0(m);
    store(m, out, 0xc, sra(q.wrapping_sub(sy_cz), 14));
    store(m, out, 0xe, sra(sy_sz.wrapping_add(r), 14));
    let s = mac(m, 0);
    store(m, out, 0x0, sra(cy_cz.wrapping_add(s), 14));
    s
}

/// 80030998 (angles, out): the ZXY rotation (Rz * Rx * Ry) of 14-bit
/// angles as a 1.14 matrix: m00 = cz*cy - sx*sz*sy, m01 = -cx*sz,
/// m02 = cz*sy + sx*sz*cy, m10 = sz*cy + sx*cz*sy, m11 = cz*cx,
/// m12 = sz*sy - sx*cz*cy, m20 = -cx*sy, m21 = sx, m22 = cx*cy. Returns
/// the leftover `v0`.
pub fn rotation_zxy_q14(m: &mut Machine, angles: u32, out: u32) -> u32 {
    let rx = u32::from(m.lhu(angles)) & 0x3fff;
    let ry = u32::from(m.lhu(at(angles, 2))) & 0x3fff;
    let rz = u32::from(m.lhu(at(angles, 4))) & 0x3fff;
    let cx = sine(m, rx + 0x1000);
    let cy = sine(m, ry + 0x1000);
    let sy = sine(m, ry);
    let sz = sine(m, rz);
    ir(m, 0, cx);
    ir(m, 1, sz);
    ir(m, 2, sy);
    ir(m, 3, cy);
    let sx = sine(m, rx);
    gpf0(m);
    let cz = sine(m, rz + 0x1000);
    let cx_sz = mac(m, 0);
    let cx_sy = mac(m, 1);
    let cx_cy = mac(m, 2);
    ir(m, 0, cz);
    ir(m, 1, cx);
    ir(m, 2, cy);
    ir(m, 3, sy);
    store(m, out, 0x2, sra(cx_sz.wrapping_neg(), 14));
    gpf0(m);
    store(m, out, 0xc, sra(cx_sy.wrapping_neg(), 14));
    store(m, out, 0xe, sx);
    let cz_cx = mac(m, 0);
    let cz_cy = mac(m, 1);
    let cz_sy = mac(m, 2);
    ir(m, 0, sz);
    ir(m, 1, cy);
    ir(m, 2, sy);
    store(m, out, 0x10, sra(cx_cy, 14));
    gpf0(m);
    let cz_sy14 = sra(cz_sy, 14);
    store(m, out, 0x8, sra(cz_cx, 14));
    let cz_cy14 = sra(cz_cy, 14);
    let sz_sy = mac(m, 1);
    let sz_cy = mac(m, 0);
    ir(m, 0, sx);
    ir(m, 1, cz_sy14);
    ir(m, 2, cz_cy14);
    ir(m, 3, sra(sz_sy, 14));
    gpf0(m);
    let sz_cy14 = sra(sz_cy, 14);
    let p = mac(m, 0);
    let q = mac(m, 1);
    let r = mac(m, 2);
    ir(m, 0, sx);
    ir(m, 1, sz_cy14);
    store(m, out, 0x0, sra(cz_cy.wrapping_sub(r), 14));
    gpf0(m);
    store(m, out, 0x6, sra(sz_cy.wrapping_add(p), 14));
    store(m, out, 0xa, sra(sz_sy.wrapping_sub(q), 14));
    let s = mac(m, 0);
    store(m, out, 0x4, sra(cz_sy.wrapping_add(s), 14));
    s
}

/// 80030B10 (y, x) -> 14-bit angle of (x, y) in `[-0x2000, 0x2000)`; (0, 0)
/// returns 0. Folds into an octant (`a3` index into the 800234A4 bases,
/// `a2` sign parity) and reads the 2049-entry table at 8001A49C with
/// `(min << 11) / max`. The divisions keep the compiler's `break` checks,
/// which only 0x80000000 inputs can reach.
pub fn atan2(m: &mut Machine, y: u32, x: u32) -> u32 {
    // The original first copies the eight octant words to its stack frame.
    let octants: [u32; 8] = std::array::from_fn(|i| m.lw(OCTANT_TABLE + i as u32 * 4));
    let (mut a0, mut a1) = (y as i32, x as i32);
    if a1 == 0 && a0 == 0 {
        return 0;
    }
    let mut octant = 0usize;
    let mut negate = false;
    if a0 < 0 {
        octant += 4;
        a0 = a0.wrapping_neg();
        negate = !negate;
    }
    if a1 < 0 {
        octant += 2;
        a1 = a1.wrapping_neg();
        negate = !negate;
    }
    let index = if a0 < a1 {
        if a1 == 0 {
            0
        } else {
            checked_div(m, ((a0 as u32) << 11) as i32, a1, 0x8003_0bbc, 0x8003_0bd4)
        }
    } else {
        if a1 != a0 {
            octant += 1;
            negate = !negate;
        }
        if a0 == 0 {
            0
        } else {
            checked_div(m, ((a1 as u32) << 11) as i32, a0, 0x8003_0c08, 0x8003_0c20)
        }
    };
    let mut angle = i32::from(m.lh(ATAN_TABLE.wrapping_add((index as u32) << 1)));
    if negate {
        angle = angle.wrapping_neg();
    }
    let angle = angle.wrapping_add(octants[octant] as i32);
    if angle < 0x2000 {
        angle as u32
    } else {
        angle.wrapping_sub(0x4000) as u32
    }
}

/// 80030C74 (value) -> arcsine of a 1.14 value as a 14-bit angle: |value|
/// clamped to 0x4000, table 8001B4A0, sign restored.
pub fn asin(m: &mut Machine, value: u32) -> u32 {
    let negative = (value as i32) < 0;
    let mut magnitude = if negative {
        value.wrapping_neg()
    } else {
        value
    };
    if (magnitude as i32) >= 0x4001 {
        magnitude = 0x4000;
    }
    let angle = m.lh(ASIN_TABLE.wrapping_add(magnitude << 1)) as i32;
    (if negative {
        angle.wrapping_neg()
    } else {
        angle
    }) as u32
}

/// 8003A08C () -> 1 when the target entity (index byte 800C4B36, not 0xFF)
/// is not in state 100 (`entity +0x48`) and its flag word (800C4D98) lacks
/// 0x400; otherwise 0.
pub fn target_available(m: &mut Machine) -> u32 {
    let index = u32::from(m.lbu(TARGET_INDEX));
    if index == 0xff {
        return 0;
    }
    let table = m.gw(common::ENTITIES);
    let entity = m.lw(table.wrapping_add(index << 2));
    if m.lh(at(entity, 0x48)) == 100 {
        return 0;
    }
    let flags = m.lw(ENTITY_FLAGS.wrapping_add(index << 2));
    u32::from(flags & 0x400 == 0)
}

/// 8003A0F8 () -> 11 when the mode byte `[gp+0x4C0]` is 0x16 and the target
/// entity (800C4B36, not 0xFF) has flag 0x100 set and 0x400 clear;
/// otherwise -1.
pub fn target_mode_code(m: &mut Machine) -> u32 {
    let mode = m.lbu(MODE_BYTE);
    let index = u32::from(m.lbu(TARGET_INDEX));
    if mode != 0x16 || index == 0xff {
        return -1i32 as u32;
    }
    let flags = m.lw(ENTITY_FLAGS.wrapping_add(index << 2));
    if flags & 0x100 != 0 && !flags & 0x400 != 0 {
        0xb
    } else {
        -1i32 as u32
    }
}

/// Table step of 80082740/800827B4: `value` (> 0) normalized by its even
/// leading-zero count `lz` (`& 0x1E`/`& 0xFE`), the 800B4C54 mantissa,
/// shifted back and `sra 12`.
fn table_sqrt(m: &mut Machine, value: u32, lz: u32) -> u32 {
    let top = if (lz as i32) < 0x18 {
        sra(value, (0x18 - lz) & 31)
    } else {
        value << ((lz - 0x18) & 31)
    };
    let mantissa = m.lh(SQRT_TABLE.wrapping_add(top.wrapping_sub(0x40) << 1)) as i32 as u32;
    let shift = sra(0x1fu32.wrapping_sub(lz), 1);
    sra(mantissa << (shift & 31), 12)
}

/// 80082740 (value) -> integer square root; `value <= 0` (signed) returns 0.
/// Like SquareRoot0 but with an arithmetic final shift.
pub fn square_root(m: &mut Machine, value: u32) -> u32 {
    m.gte.mtc2(reg::LZCS, value);
    if (value as i32) <= 0 {
        return 0;
    }
    let lz = m.gte.mfc2(reg::LZCR) & 0x1e;
    table_sqrt(m, value, lz)
}

/// 800827B4 (a: *const VECTOR, b: *const VECTOR) -> |a - b| >> 12 (integer
/// length of the 4.12-reduced difference). Returns 0x100000 when the sum of
/// the reduced components' magnitudes exceeds 0x58000; above 0xB000 it
/// squares the differences `>> 15` and scales the root by 8.
pub fn vector_distance(m: &mut Machine, a: u32, b: u32) -> u32 {
    let a0 = m.lw(a);
    let a1 = m.lw(at(a, 4));
    let a2 = m.lw(at(a, 8));
    let b0 = m.lw(b);
    let b1 = m.lw(at(b, 4));
    let b2 = m.lw(at(b, 8));
    let d = [
        a0.wrapping_sub(b0),
        a1.wrapping_sub(b1),
        a2.wrapping_sub(b2),
    ];
    let reduced = d.map(|v| sra(v, 12));
    for (i, &v) in reduced.iter().enumerate() {
        ir(m, 1 + i, v);
    }
    m.gte.sqr(false, true);
    let magnitude = |v: u32| {
        if (v as i32) < 0 { v.wrapping_neg() } else { v }
    };
    let sum = magnitude(reduced[0])
        .wrapping_add(magnitude(reduced[1]))
        .wrapping_add(magnitude(reduced[2]));
    if 0x58000 < sum as i32 {
        return 0x10_0000;
    }
    let mut scale = 0;
    if 0xb000 < sum as i32 {
        for (i, &v) in d.iter().enumerate() {
            ir(m, 1 + i, sra(v, 15));
        }
        scale = 3;
        m.gte.sqr(false, true);
    }
    let squares = [mac(m, 0), mac(m, 1), mac(m, 2)];
    let total = squares[0].wrapping_add(squares[1]).wrapping_add(squares[2]);
    m.gte.mtc2(reg::LZCS, total);
    if (total as i32) <= 0 {
        return 0;
    }
    let lz = m.gte.mfc2(reg::LZCR) & 0xfe;
    table_sqrt(m, total, lz) << scale
}

/// 800270FC (target, source, count) -> end of the target: copies `count`
/// words forward (none unless `count` is positive, signed).
pub fn copy_words(m: &mut Machine, target: u32, source: u32, count: u32) -> u32 {
    let (mut target, mut source) = (target, source);
    let mut copied = 0i32;
    while copied < count as i32 {
        let word = m.lw(source);
        source = source.wrapping_add(4);
        copied += 1;
        m.sw(target, word);
        target = target.wrapping_add(4);
    }
    target
}

// ---- sound channels (8002D980..80030248) -----------------------------------------

/// 800C43D8: sound source count word; per-source channel bytes follow at
/// +4/+8/+0xC (four sources, one byte each).
pub const SOUND_SOURCES: u32 = 0x800c_43d8;
/// 800C43F8: four 0x5C-byte sound source records.
pub const SOUND_SOURCE_RECORDS: u32 = 0x800c_43f8;
/// 800C4318: eight 4-byte channel group records.
pub const CHANNEL_GROUPS: u32 = 0x800c_4318;
/// 800C4338: 32 queued 4-byte sound events (`[gp+0x315]` write index).
pub const SOUND_EVENTS: u32 = 0x800c_4338;

/// 80030248 () -> 0: stop all sound: 8002FF64, 8002F204, 8002D9B8,
/// 8002ED24 and 8002EE38.
pub fn stop_all_sound(m: &mut Machine) -> u32 {
    release_engine_channels(m);
    reset_sound_sources(m);
    stop_channels(m);
    reset_channel_groups(m);
    reset_sound_events(m);
    0
}

/// 8002FF64 (): release the three engine channels at `gp+0x31D..0x31F`
/// (8002D980), then mark them free (0xFF).
pub fn release_engine_channels(m: &mut Machine) {
    for offset in [0x31d, 0x31e, 0x31f] {
        let channel = u32::from(m.lbu(GP + offset));
        release_channel(m, channel);
    }
    for offset in [0x31f, 0x31e, 0x31d] {
        m.sb(GP + offset, 0xff);
    }
}

/// 8002D980 (channel) -> channel, or 0xFF when `channel >= 24`: channel
/// record byte +1 = 2 (releasing).
pub fn release_channel(m: &mut Machine, channel: u32) -> u32 {
    if channel < 0x18 {
        m.sb(at(CHANNELS, channel.wrapping_mul(20) + 1), 2);
        channel
    } else {
        0xff
    }
}

/// 8002F204 (): clear the source count and, for the four sources, release
/// their three channels, mark them free and reset the source record (+0 =
/// 0, +8 = -1, envelope +0x24, byte +0x30 = 0).
pub fn reset_sound_sources(m: &mut Machine) {
    m.sw(SOUND_SOURCES, 0);
    for index in 0..4 {
        let bytes = SOUND_SOURCES + index;
        for offset in [4, 8, 0xc] {
            let channel = u32::from(m.lbu(bytes + offset));
            release_channel(m, channel);
        }
        let record = SOUND_SOURCE_RECORDS + index * 0x5c;
        m.sb(bytes + 4, 0xff);
        m.sb(bytes + 8, 0xff);
        m.sb(bytes + 0xc, 0xff);
        m.sw(record, 0);
        m.sw(record + 8, u32::MAX);
        reset_envelope(m, record + 0x24);
        m.sb(record + 0x30, 0);
    }
}

/// 8002E418 (envelope): +4 = 0, +0 = 0x100000, halfwords +8/+0xA = 0.
pub fn reset_envelope(m: &mut Machine, envelope: u32) {
    m.sw(at(envelope, 4), 0);
    m.sw(envelope, 0x10_0000);
    m.sh(at(envelope, 8), 0);
    m.sh(at(envelope, 0xa), 0);
}

/// 8002D9B8 (): reset all 24 channels (8002E198) and key every voice off
/// (`SpuSetKey(0, 0xFFFFFF)`).
pub fn stop_channels(m: &mut Machine) {
    for channel in 0..0x18 {
        reset_channel(m, channel);
    }
    sdk::spu_set_key(m, 0, 0xff_ffff);
}

/// 8002E198 (channel) -> channel: free the channel record (+0 = 0xFF, +1/+2
/// /+3 = 0, +0x10/+0x11 = 0x80, half +0x12 = 0); 0xFF is ignored.
pub fn reset_channel(m: &mut Machine, channel: u32) -> u32 {
    if channel == 0xff {
        return 0xff;
    }
    let record = at(CHANNELS, channel.wrapping_mul(20));
    m.sb(record, 0xff);
    m.sb(at(record, 1), 0);
    m.sb(at(record, 2), 0);
    m.sb(at(record, 0x10), 0x80);
    m.sb(at(record, 0x11), 0x80);
    m.sh(at(record, 0x12), 0);
    m.sb(at(record, 3), 0);
    channel
}

/// 8002ED24 (): 8002ED5C for the eight channel groups.
pub fn reset_channel_groups(m: &mut Machine) {
    for group in 0..8 {
        reset_channel_group(m, group);
    }
}

/// 8002ED5C (group): group record 800C4318 + group*4: byte +0 = 0xFF, half
/// +2 = -1.
pub fn reset_channel_group(m: &mut Machine, group: u32) {
    let record = at(group << 2, CHANNEL_GROUPS);
    m.sb(record, 0xff);
    m.sh(at(record, 2), 0xffff);
}

/// 8002EE38 (): event indexes `gp+0x314/0x315` = 0 and the 32 queued events
/// = {0xFF, 0xFF, -1}.
pub fn reset_sound_events(m: &mut Machine) {
    m.sb(GP + 0x314, 0);
    m.sb(GP + 0x315, 0);
    for index in 0..0x20 {
        let event = SOUND_EVENTS + index * 4;
        m.sb(event, 0xff);
        m.sb(event + 1, 0xff);
        m.sh(event + 2, 0xffff);
    }
}

// ---- CD / stream request queue (`[gp+0x48]`) --------------------------------------

/// `[gp+0x48]`: the CD request queue: head +0, tail +4, sixteen 0x14-byte
/// entries from +8 (type byte +0), control request +0x148 (flag) / +0x14C
/// (type) / +0x14D, arguments +0x150/+0x15C, current file +0x174.
pub const CD_QUEUE: u32 = 0x48;

/// 8002C280 (request, argument): clear the sixteen entry types and post
/// control request type 1 (reset) with `request`/`argument`.
pub fn reset_cd_queue(m: &mut Machine, request: u32, argument: u32) -> u32 {
    for index in 0..16 {
        let queue = m.gw(CD_QUEUE);
        m.sb(at(queue, index * 0x14 + 8), 0);
    }
    let queue = m.gw(CD_QUEUE);
    m.sb(at(queue, 0x14c), 1);
    let current = m.gw(CD_QUEUE);
    m.sw(at(queue, 0x148), 1);
    m.sw(at(current, 0x150), request);
    m.sw(at(current, 0x15c), argument);
    m.sb(at(current, 0x14d), 0);
    // v0 is left holding the queue block (the last `lw v0, 0x48(gp)`).
    current
}

/// 8002C2F4 (): post control request type 5 (stop streaming: +0x14C = 5,
/// +0x148 = 1, +0x14D = 0, +0x15C = 0) and cancel the queued entries
/// head..=tail (ring of 16) of type 3 or 4.
pub fn cancel_stream_requests(m: &mut Machine) {
    let first = m.gw(CD_QUEUE);
    m.sb(at(first, 0x14c), 5);
    let second = m.gw(CD_QUEUE);
    m.sw(at(first, 0x148), 1);
    m.sb(at(second, 0x14d), 0);
    let mut queue = m.gw(CD_QUEUE);
    m.sw(at(second, 0x15c), 0);
    let mut index = m.lw(queue);
    let tail = m.lw(at(queue, 4));
    if (tail as i32) < (index as i32) {
        return;
    }
    loop {
        let entry = at(queue, (index & 0xf) * 0x14 + 8);
        let kind = m.lbu(entry);
        if u32::from(kind).wrapping_sub(3) < 2 {
            m.sb(entry, 0);
        }
        queue = m.gw(CD_QUEUE);
        let tail = m.lw(at(queue, 4));
        index = index.wrapping_add(1);
        if (tail as i32) < (index as i32) {
            return;
        }
    }
}

/// 8002C6A8 () -> the file or stream the CD service is reading (queue word
/// +0x174; -1 when idle).
pub fn current_cd_file(m: &mut Machine) -> u32 {
    let queue = m.gw(CD_QUEUE);
    m.lw(at(queue, 0x174))
}

// ---- entity activation (8004CB04..8004D6BC) ----------------------------------------

/// 800C4568: 64 per-entity marker record pointers (0: none).
pub const ENTITY_MARKERS: u32 = 0x800c_4568;
/// 800C4828: 12-byte status records per entity.
pub const ENTITY_STATUS: u32 = 0x800c_4828;
/// `[gp+0x374]`/`[gp+0x378]`: unit (64-byte) and object (40-byte) setup
/// records.
const UNIT_SETUP: u32 = 0x374;
const OBJECT_SETUP: u32 = 0x378;

fn or_word(m: &mut Machine, address: u32, bits: u32) {
    let value = m.lw(address);
    m.sw(address, value | bits);
}
fn entity(m: &mut Machine, index: u32) -> u32 {
    let table = m.gw(common::ENTITIES);
    m.lw(at(table, index.wrapping_mul(4)))
}
fn entity_flags(index: u32) -> u32 {
    at(ENTITY_FLAGS, index.wrapping_mul(4))
}

/// 8004CB04 (index): entity `index` word |= 0x10000.
pub fn mark_unit(m: &mut Machine, index: u32) {
    let unit = entity(m, index);
    or_word(m, unit, 0x1_0000);
}

/// 8004D2F8 (index): object `index` (entity `index + 0x20`) word |= 0x10000
/// unless its kind byte +2 is 0xFF.
pub fn mark_object(m: &mut Machine, index: u32) {
    let records = m.gw(OBJECT_SETUP);
    let table = m.gw(common::ENTITIES);
    let record = at(records, index.wrapping_mul(40));
    let kind = m.lw(at(record, 8));
    let value = m.lbu(at(kind, 2));
    let object = m.lw(at(at(table, index.wrapping_mul(4)), 0x80));
    if value != 0xff {
        or_word(m, object, 0x1_0000);
    }
}

/// Setup flags shared by 8004CB2C and 8004D344, in original order: bit 1
/// (and 0x101 for kind byte +0x1C bit 0), 0x200 (bit 1), 0x80000 (bit 2,
/// units only), then from the placement bytes: 0x1000 (+7), 0x2000 (+8),
/// 0x4000/0x8000 (+6 bits 1/0), 0x10 (+0xA zero), 0x800000 (+3 not 0xFF),
/// 0x400 (+4 zero).
fn activation_flags(m: &mut Machine, flags: u32, kind: u32, placement: u32, unit: bool) {
    let old = m.lw(flags);
    m.sw(flags, old | 1);
    if m.lbu(at(kind, 0x1c)) & 1 != 0 {
        m.sw(flags, old | 0x101);
    }
    if m.lbu(at(kind, 0x1c)) & 2 != 0 {
        or_word(m, flags, 0x200);
    }
    if unit && m.lbu(at(kind, 0x1c)) & 4 != 0 {
        or_word(m, flags, 0x8_0000);
    }
    if m.lbu(at(placement, 7)) != 0 {
        or_word(m, flags, 0x1000);
    }
    if m.lbu(at(placement, 8)) != 0 {
        or_word(m, flags, 0x2000);
    }
    if m.lbu(at(placement, 6)) & 2 != 0 {
        or_word(m, flags, 0x4000);
    }
    if m.lbu(at(placement, 6)) & 1 != 0 {
        or_word(m, flags, 0x8000);
    }
    if m.lbu(at(placement, 0xa)) == 0 {
        or_word(m, flags, 0x10);
    }
    if m.lbu(at(placement, 3)) != 0xff {
        or_word(m, flags, 0x80_0000);
    }
    if m.lbu(at(placement, 4)) == 0 {
        or_word(m, flags, 0x400);
    }
}

/// Common tail of 8004CB2C and 8004D344: entity word bit 0x800/0x1000 for
/// placement byte +9 = 1/2; halves +0x4A/+0x4C from placement halves
/// +0xC/+0xE with bits 0x200/0x400, or 0 when the placement value is -1.
fn activation_tail(m: &mut Machine, object: u32, placement: u32) {
    match m.lbu(at(placement, 9)) {
        1 => or_word(m, object, 0x800),
        2 => or_word(m, object, 0x1000),
        _ => {}
    }
    for (field, offset, bit) in [(0xc, 0x4a, 0x200), (0xe, 0x4c, 0x400)] {
        let value = m.lh(at(placement, field));
        if value != -1 {
            let word = m.lw(object);
            m.sh(at(object, offset), value as u16);
            m.sw(object, word | bit);
        } else {
            m.sh(at(object, offset), 0);
        }
    }
}

/// 8004CB2C (index): activate unit `index` (< 0x20) from its setup record
/// (`[gp+0x374] + index*0x40`: +0xC kind, +0x10 placement): setup flags
/// 800C4D98[index] (0x800 when 0x1000 is set and entity half +0x48 is
/// nonzero), 8004D59C, 8004D6BC, then the entity parameters.
pub fn activate_unit(m: &mut Machine, index: u32) {
    let table = m.gw(common::ENTITIES);
    let records = m.gw(UNIT_SETUP);
    let record = at(records, index << 6);
    let flags = entity_flags(index);
    let object = m.lw(at(table, index.wrapping_mul(4)));
    let kind = m.lw(at(record, 0xc));
    let placement = m.lw(at(record, 0x10));
    activation_flags(m, flags, kind, placement, true);
    let word = m.lw(flags);
    if word & 0x1000 != 0 && m.lh(at(object, 0x48)) != 0 {
        m.sw(flags, word | 0x800);
    }
    update_marker(m, index, placement);
    update_status(m, index, placement);
    activation_tail(m, object, placement);
}

/// 8004D344 (index): 8004CB2C for object `index` (entity `index + 0x20`,
/// `[gp+0x378] + index*40`: +8 kind, +0xC placement; no kind bit 2 and no
/// half +0x48 condition).
pub fn activate_object(m: &mut Machine, index: u32) {
    let flags = entity_flags(index.wrapping_add(0x20));
    let table = m.gw(common::ENTITIES);
    let records = m.gw(OBJECT_SETUP);
    let record = at(records, index.wrapping_mul(40));
    let object = m.lw(at(at(table, index.wrapping_mul(4)), 0x80));
    let kind = m.lw(at(record, 8));
    let placement = m.lw(at(record, 0xc));
    activation_flags(m, flags, kind, placement, false);
    let word = m.lw(flags);
    if word & 0x1000 != 0 {
        m.sw(flags, word | 0x800);
    }
    let index = index.wrapping_add(0x20);
    update_marker(m, index, placement);
    update_status(m, index, placement);
    activation_tail(m, object, placement);
}

/// 8004D59C (index, placement): the marker record `[800C4568 + index*4]`
/// (if any): byte +2 mode (0 without placement byte +7, 3 without byte +6
/// bit 1, else 1/2 for byte +4 = 0/1; bit 0x80 for byte +5 = 1), +4 =
/// placement byte +3, +3 = entity byte +0x48, +5 = 1 for setup flag 0x100,
/// else 3/2/0 for entity flags 0x40/0x100.
pub fn update_marker(m: &mut Machine, index: u32, placement: u32) {
    let marker = m.lw(at(ENTITY_MARKERS, index.wrapping_mul(4)));
    if marker == 0 {
        return;
    }
    let object = entity(m, index);
    if m.lbu(at(placement, 7)) == 0 {
        m.sb(at(marker, 2), 0);
    } else if !m.lbu(at(placement, 6)) & 2 != 0 {
        m.sb(at(marker, 2), 3);
    } else {
        match m.lbu(at(placement, 4)) {
            0 => m.sb(at(marker, 2), 1),
            1 => m.sb(at(marker, 2), 2),
            _ => {}
        }
    }
    if m.lbu(at(placement, 5)) == 1 {
        let value = m.lbu(at(marker, 2));
        m.sb(at(marker, 2), value | 0x80);
    }
    let value = m.lbu(at(placement, 3));
    m.sb(at(marker, 4), value);
    let value = m.lbu(at(object, 0x48));
    m.sb(at(marker, 3), value);
    let state = if m.lw(entity_flags(index)) & 0x100 != 0 {
        1
    } else {
        let word = m.lw(object);
        if word & 0x40 != 0 {
            3
        } else if word & 0x100 != 0 {
            2
        } else {
            0
        }
    };
    m.sb(at(marker, 5), state);
}

/// 8004D6BC (index, placement): the 12-byte status record 800C4828 +
/// index*12: byte +0 bit 0x10 for placement byte +5 = 1; +1 = placement
/// byte +4 when it is 0 or 1; +2 = 1 for setup flag 0x100, else 2/3/0 for
/// entity flags 0x40/0x100.
pub fn update_status(m: &mut Machine, index: u32, placement: u32) {
    let record = at(ENTITY_STATUS, index.wrapping_mul(12));
    if m.lbu(at(placement, 5)) == 1 {
        let value = m.lbu(record);
        m.sb(record, value | 0x10);
    }
    match m.lbu(at(placement, 4)) {
        0 => m.sb(at(record, 1), 0),
        1 => m.sb(at(record, 1), 1),
        _ => {}
    }
    let state = if m.lw(entity_flags(index)) & 0x100 != 0 {
        1
    } else {
        let object = entity(m, index);
        let word = m.lw(object);
        if word & 0x40 != 0 {
            2
        } else if word & 0x100 != 0 {
            3
        } else {
            0
        }
    };
    m.sb(at(record, 2), state);
}

// ---- player placement and camera target (800710FC, 8006D7DC, 80077D54) -------------

/// `[gp+0x528]`: 0x70-byte player flight state.
const PLAYER_STATE: u32 = 0x528;
/// `[gp+0x52C]`: 0x3C-byte player aircraft limits.
const PLAYER_LIMITS: u32 = 0x52c;
/// 800BAC78: 8006D7DC reference SVECTOR (8 bytes, copied with lwl/lwr).
const REFERENCE_VECTOR: u32 = 0x800b_ac78;

/// `lwl/lwr` pair: an unaligned little-endian word.
fn load_unaligned(m: &mut Machine, address: u32) -> u32 {
    u32::from_le_bytes(std::array::from_fn(|i| m.lbu(at(address, i as u32))))
}
/// `swl/swr` pair: an unaligned little-endian word.
fn store_unaligned(m: &mut Machine, address: u32, value: u32) {
    for (i, byte) in value.to_le_bytes().into_iter().enumerate() {
        m.sb(at(address, i as u32), byte);
    }
}
fn load_words<const N: usize>(m: &mut Machine, address: u32) -> [u32; N] {
    std::array::from_fn(|i| m.lw(at(address, i as u32 * 4)))
}
/// `ldv0 vector; RTV0 (4A486012: MVMVA sf=1, rotation, V0, no translation);
/// stlvnl out`.
fn rotate_v0(m: &mut Machine, vector: u32, out: u32) {
    let xy = m.lw(vector);
    let z = m.lw(at(vector, 4));
    m.gte.mtc2(reg::VXY0, xy);
    m.gte.mtc2(reg::VZ0, z);
    m.gte.mvmva(true, false, 0, 0, 3);
    for i in 0..3 {
        let value = m.gte.mfc2(reg::MAC1 + i);
        m.sw(at(out, i as u32 * 4), value);
    }
}

/// 800710FC (start) -> v0: place the player (`[[gp+0x2D8]]`) at the
/// six-word start record: flags 0x1C000, clear the small player blocks
/// (`[gp+0x51C]`, `[gp+0x520]`, `[gp+0x524]`) and the flight state angles,
/// store the record at player +8..+0x1C (angles at +0x18) and the offset
/// `R(angles) * (0, 0, 0x1080) << 5` at +0x38..+0x40 (80030534 into its
/// 0x58-byte frame, RTV0), then derive the relative angles (8006D7DC).
/// Returns 8006D7DC's `v0`.
pub fn place_player(m: &mut Machine, start: u32) -> u32 {
    let frame = m.push_frame(0x58);
    let vector = frame + 0x10;
    let matrix = frame + 0x18;
    let result = frame + 0x38;
    let table = m.gw(common::ENTITIES);
    let player = m.lw(table);
    let state = m.gw(PLAYER_STATE);
    let flags = m.lw(player);
    let block_51c = m.gw(0x51c);
    m.sw(player, flags | 0x1_c000);
    let block_520 = m.gw(0x520);
    let block_524 = m.gw(0x524);
    for offset in [0x30, 0x32, 0x34, 0x38, 0x3a, 0x3c] {
        m.sh(at(state, offset), 0);
    }
    m.sh(at(state, 0x4c), 0x1000);
    for offset in [4, 6, 8, 0xa, 0xc] {
        m.sh(at(block_520, offset), 0);
    }
    m.sh(at(block_520, 0xe), 0x1000);
    for offset in [0, 2, 4, 6, 8] {
        m.sh(at(block_524, offset), 0);
    }
    m.sw(at(block_524, 0xc), 0);
    for offset in [0, 2, 4, 6] {
        m.sh(at(block_51c, offset), 0);
    }
    let first: [u32; 4] = load_words(m, start);
    for (i, word) in first.into_iter().enumerate() {
        m.sw(at(player, 8 + i as u32 * 4), word);
    }
    let last: [u32; 2] = load_words(m, at(start, 0x10));
    m.sw(at(player, 0x18), last[0]);
    m.sw(at(player, 0x1c), last[1]);
    m.sw(at(state, 0x48), 0x2_1000);
    // SVECTOR (0, 0, 0x1080); its pad halfword stays stale stack data that
    // reaches only the high half of VZ0 (reloaded by 8006D7DC).
    m.sh(vector, 0);
    m.sh(vector + 2, 0);
    m.sh(vector + 4, 0x1080);
    rotation_yxz(m, at(player, 0x18), matrix);
    sdk::set_rot_matrix(m, matrix);
    rotate_v0(m, vector, result);
    let offset: [u32; 3] = load_words(m, result);
    let limits = m.gw(PLAYER_LIMITS);
    let offset = offset.map(|value| value << 5);
    for (i, value) in offset.into_iter().enumerate() {
        m.sw(result + i as u32 * 4, value);
    }
    m.sw(at(player, 0x38), offset[0]);
    let value = m.lw(result + 4);
    let state_again = m.gw(PLAYER_STATE);
    m.sw(at(player, 0x3c), value);
    let value = m.lw(result + 8);
    m.sw(at(player, 0x40), value);
    m.sw(at(state_again, 0x10), 0);
    m.sw(at(limits, 0x24), 0);
    let v0 = relative_angles(m, player, state);
    m.pop_frame(0x58);
    v0
}

/// 8006D7DC (entity, state) -> v0: in its 0x78-byte frame, rotate the
/// reference SVECTOR copied from 800BAC78 (+0x60) by `transpose(R(entity
/// angles +0x18)) * R(state angles +0x40)` (800306AC into +0x10, 80030534
/// into +0x30, MulMatrix, RTV0 into +0x50), and store the arctangents
/// (80030B10) of `(-y, z)` at state +0x38 and `(x, z)` at +0x3A, and 0 at
/// +0x3C. Returns the second arctangent.
pub fn relative_angles(m: &mut Machine, entity: u32, state: u32) -> u32 {
    let frame = m.push_frame(0x78);
    let (inverse, local, result, reference) =
        (frame + 0x10, frame + 0x30, frame + 0x50, frame + 0x60);
    let xy = load_unaligned(m, REFERENCE_VECTOR);
    let z = load_unaligned(m, REFERENCE_VECTOR + 4);
    store_unaligned(m, reference, xy);
    store_unaligned(m, reference + 4, z);
    rotation_yxz_transposed(m, at(entity, 0x18), inverse);
    rotation_yxz(m, at(state, 0x40), local);
    sdk::mul_matrix(m, inverse, local);
    sdk::set_rot_matrix(m, inverse);
    rotate_v0(m, reference, result);
    let y = m.lw(result + 4);
    let z = m.lw(result + 8);
    let pitch = atan2(m, y.wrapping_neg(), z);
    m.sh(at(state, 0x38), pitch as u16);
    let x = m.lw(result);
    let z = m.lw(result + 8);
    let heading = atan2(m, x, z);
    m.sh(at(state, 0x3a), heading as u16);
    m.sh(at(state, 0x3c), 0);
    m.pop_frame(0x78);
    heading
}

/// 80077D54 (camera, mode, entity) -> v0: camera mode `mode` on `entity`:
/// 800761FC; for mode bits 0x401 the entity's low six flag bits become the
/// target slot; for any mode bit in 0xDFF the camera follows the entity
/// (word 0 low bits = slot, bit 0x100, +0x34 = entity + 8, offsets
/// +0x38..+0x4C zero, +0xC4/+0xC8 slots), otherwise bit 0x100 is cleared
/// and +0xC4 = -1. Then +0x50 = `mode` and the state fields +0xCC..+0xE4
/// are zero.
pub fn set_camera_target(m: &mut Machine, camera: u32, mode: u32, entity: u32) -> u32 {
    mark_camera(m, camera);
    let mut slot = 0xffff_ffff;
    if mode & 0x401 != 0 {
        slot = m.lw(entity) & 0x3f;
    }
    let result = if mode & 0xdff != 0 {
        let flags = m.lw(camera) & 0xffff_ffc0;
        m.sw(camera, flags);
        let target = m.lw(entity) & 0x3f;
        m.sw(camera, flags | 0x100 | target);
        let target = m.lw(entity);
        m.sw(at(camera, 0x34), at(entity, 8));
        for offset in [0x38, 0x3c, 0x40] {
            m.sw(at(camera, offset), 0);
        }
        for offset in [0x48, 0x4a, 0x4c] {
            m.sh(at(camera, offset), 0);
        }
        m.sw(at(camera, 0xc8), target & 0x3f);
        let flags = m.lw(entity);
        m.sw(at(camera, 0xc4), slot);
        m.sw(at(camera, 0xc8), flags & 0x3f);
        flags
    } else {
        let flags = m.lw(camera);
        m.sw(at(camera, 0xc4), 0xffff_ffff);
        let flags = flags & 0xffff_feff;
        m.sw(camera, flags);
        flags
    };
    m.sw(at(camera, 0x50), mode);
    m.sw(at(camera, 0xd0), 0);
    m.sw(at(camera, 0xcc), 0);
    for offset in [0xd4, 0xd6, 0xd8] {
        m.sh(at(camera, offset), 0);
    }
    for offset in [0xdc, 0xe0, 0xe4] {
        m.sw(at(camera, offset), 0);
    }
    result
}

/// 800761FC (camera) -> the new word: camera word 0 |= 0x400.
pub fn mark_camera(m: &mut Machine, camera: u32) -> u32 {
    let flags = m.lw(camera) | 0x400;
    m.sw(camera, flags);
    flags
}

// ---- lock links (8004577C, 80045874) --------------------------------------------

/// 800C4B48: per-slot lock links, 4 bytes: +1 the slot's target, +2/+3 the
/// slots tracking it (0xFF = none).
pub const LOCK_LINKS: u32 = 0x800c_4b48;

/// 80045874: release `slot`'s lock: its target link (+1) becomes 0xFF and
/// the target forgets `slot` in its tracker bytes (+2, else +3). Returns 0xFF.
pub fn release_lock(m: &mut Machine, slot: u32) -> u32 {
    if slot == 0xff {
        return 0xff;
    }
    let record = LOCK_LINKS.wrapping_add(slot << 2);
    let target = u32::from(m.lbu(at(record, 1)));
    m.sb(at(record, 1), 0xff);
    if target != 0xff {
        let other = LOCK_LINKS.wrapping_add(target << 2);
        if u32::from(m.lbu(at(other, 2))) == slot {
            m.sb(at(other, 2), 0xff);
        } else if u32::from(m.lbu(at(other, 3))) == slot {
            m.sb(at(other, 3), 0xff);
        }
    }
    0xff
}

/// 8004577C: lock `slot` onto `target` (0x20-byte frame). A target keeps
/// two trackers (+2, +3); the player (target 0) takes a second one only
/// while context +0x398 is set, and a fully tracked target yields its +3
/// tracker to the player. Returns `target`, or 0xFF when refused.
pub fn set_lock(m: &mut Machine, slot: u32, target: u32) -> u32 {
    m.push_frame(0x20);
    let result = set_lock_framed(m, slot, target);
    m.pop_frame(0x20);
    result
}

fn set_lock_framed(m: &mut Machine, slot: u32, target: u32) -> u32 {
    if target == 0xff {
        return target;
    }
    if target == 0 && u32::from(m.lbu(LOCK_LINKS + 1)) == slot {
        return 0xff;
    }
    let record = LOCK_LINKS.wrapping_add(target << 2);
    if m.lbu(at(record, 2)) == 0xff {
        m.sb(at(record, 2), slot as u8);
    } else if m.lbu(at(record, 3)) == 0xff {
        let context = m.gw(common::CONTEXT);
        if m.lw(at(context, 0x398)) == 0 && target == 0 {
            return 0xff;
        }
        m.sb(at(record, 3), slot as u8);
    } else {
        if slot != 0 {
            return 0xff;
        }
        let previous = u32::from(m.lbu(at(record, 3)));
        release_lock(m, previous);
        m.sb(at(record, 3), 0);
    }
    m.sb(at(LOCK_LINKS.wrapping_add(slot << 2), 1), target as u8);
    target
}

// ---- event ring (8007C8AC, 8007C7E8, 8007C778, 8007C5C0, 8007C6DC) ------------

/// `[gp+0x54C]`: the event recorder: +0 recorded frames, +4 ring base, +8
/// ring end, +0xC write and +0x10 read pointers, +0x14 frame-marker flag,
/// +0x18 the replay copies (0x20 bytes per slot), +0x1830..+0x1840 focus.
pub const EVENTS: u32 = 0x54c;

/// `slt`: signed less-than.
fn lt(a: u32, b: u32) -> bool {
    (a as i32) < (b as i32)
}

/// 8007C8AC: reserve `words` words in the event ring (+4 base, +8 end,
/// +0xC write, +0x10 read; 8007C7E8 drops old events when the write would
/// pass the read pointer), in a 0x20-byte frame. Returns the reserved
/// address.
pub fn reserve_events(m: &mut Machine, words: u32) -> u32 {
    m.push_frame(0x20);
    let events = m.gw(EVENTS);
    let start = m.lw(at(events, 0xc));
    let end = m.lw(at(events, 8));
    let mut next = start.wrapping_add(words << 2);
    let read = m.lw(at(events, 0x10));
    let flush = if lt(next, end) {
        !lt(read, start) && !lt(next, read) && m.lw(events) != 0
    } else {
        next = m.lw(at(events, 4));
        !lt(read, start) || !lt(next, read)
    };
    if flush {
        drop_events(m, words);
    }
    let events = m.gw(EVENTS);
    m.sw(at(events, 0xc), next);
    m.pop_frame(0x20);
    start
}

/// 8007C7E8: drop the oldest recorded events until `words` words are
/// free (0x28-byte frame): each is applied to the replay copies (8007C778),
/// frame markers (word 1) decrement the frame count, and the read pointer
/// (+0x10) advances, wrapping from +8 to +4.
pub fn drop_events(m: &mut Machine, words: u32) {
    m.push_frame(0x28);
    let events = m.gw(EVENTS);
    let mut read = m.lw(at(events, 0x10));
    let end = m.lw(at(events, 8));
    if lt(0, words) {
        let mut dropped = 0u32;
        loop {
            if m.lw(read) == 1 {
                let events = m.gw(EVENTS);
                let frames = m.lw(events).wrapping_sub(1);
                m.sw(events, frames);
            }
            let length = apply_event(m, read);
            read = read.wrapping_add(length << 2);
            dropped = dropped.wrapping_add(length);
            if !lt(read, end) {
                let events = m.gw(EVENTS);
                read = m.lw(at(events, 4));
            }
            if !lt(dropped, words) {
                break;
            }
        }
    }
    let events = m.gw(EVENTS);
    m.sw(at(events, 0x10), read);
    m.pop_frame(0x28);
}

/// 8007C778: apply one recorded event to the replay copies (+0x18), in a
/// 0x18-byte frame: class 0 one-word events (8007C5C0), class 1 positions
/// (8007C6DC). Returns the event length in words (low nibble).
pub fn apply_event(m: &mut Machine, event: u32) -> u32 {
    m.push_frame(0x18);
    let events = m.gw(EVENTS);
    let class = (m.lw(event) >> 4) & 7;
    let copies = at(events, 0x18);
    match class {
        0 => apply_word_event(m, event, copies),
        1 => apply_position_event(m, event, copies),
        _ => {}
    }
    let length = m.lw(event) & 0xf;
    m.pop_frame(0x18);
    length
}

/// 8007C5C0: apply a one-word event (kind in bits 7..9, slot in 10..15;
/// jump table 80024248): 0 frame marker (+0x14 = 1), 1 presence, 2 flag
/// bytes, 3 kind, 4 focus.
pub fn apply_word_event(m: &mut Machine, event: u32, copies: u32) {
    let word = m.lw(event);
    let copy = |word: u32| copies.wrapping_add((word >> 5) & 0x7e0);
    match (word >> 7) & 7 {
        0 => {
            let events = m.gw(EVENTS);
            m.sw(at(events, 0x14), 1);
        }
        1 => {
            let word = m.lw(event);
            let address = copy(word);
            let value = m.lw(address);
            if (word >> 16) & 1 != 0 {
                m.sw(address, value | 0x1_4000);
            } else {
                m.sw(address, value & !0x1_4000);
            }
        }
        2 => {
            let word = m.lw(event);
            let first = m.lbu(at(event, 2));
            let address = copy(word);
            m.sb(at(address, 4), first);
            let second = m.lbu(at(event, 3));
            m.sb(at(address, 5), second);
        }
        3 => {
            let word = m.lw(event);
            m.sh(at(copy(word), 6), ((word >> 16) & 0x7fff) as u16);
        }
        4 => {
            let mut focus = (m.lw(event) >> 10) & 0x3f;
            if focus == 0 {
                focus = u32::MAX;
            }
            let events = m.gw(EVENTS);
            m.sw(at(events, 0x1840), focus);
            m.sw(at(events, 0x1830), 0);
            m.sw(at(events, 0x1834), 0);
            m.sw(at(events, 0x1838), 0);
        }
        _ => {}
    }
}

/// 8007C6DC: apply a position event (subtype 0 only): coordinates (high 20
/// bits) and angles (low 12 bits << 2) of the slot in bits 9..14.
pub fn apply_position_event(m: &mut Machine, event: u32, copies: u32) {
    let word = m.lw(event);
    if (word >> 7) & 3 != 0 {
        return;
    }
    let copy = copies.wrapping_add((word >> 4) & 0x7e0);
    for i in 0..3u32 {
        let value = m.lw(at(event, 4 + i * 4)) & 0xffff_f000;
        m.sw(at(copy, 8 + i * 4), value);
    }
    for i in 0..3u32 {
        let value = m.lw(at(event, 4 + i * 4));
        m.sh(at(copy, 0x18 + i * 2), sra(value << 20, 18) as u16);
    }
}

// ---- uniform entry points (original a0..a3 -> v0) ------------------------------

/// 800270FC (target, source, count) -> end of the target. See
/// [`copy_words`].
pub fn f_800270fc(m: &mut Machine, args: [u32; 4]) -> u32 {
    copy_words(m, args[0], args[1], args[2])
}

/// 8002DAA0 (channel, a, b) -> channel or 0xFF. See [`set_channel_levels`].
pub fn f_8002daa0(m: &mut Machine, args: [u32; 4]) -> u32 {
    set_channel_levels(m, args[0], args[1], args[2])
}
/// 80030534 (angles, out) -> leftover v0. See [`rotation_yxz`].
pub fn f_80030534(m: &mut Machine, args: [u32; 4]) -> u32 {
    rotation_yxz(m, args[0], args[1])
}
/// 800306AC (angles, out) -> leftover v0. See [`rotation_yxz_transposed`].
pub fn f_800306ac(m: &mut Machine, args: [u32; 4]) -> u32 {
    rotation_yxz_transposed(m, args[0], args[1])
}
/// 80030824 (angles, out) -> leftover v0. See [`rotation_yxz_q14`].
pub fn f_80030824(m: &mut Machine, args: [u32; 4]) -> u32 {
    rotation_yxz_q14(m, args[0], args[1])
}
/// 80030998 (angles, out) -> leftover v0. See [`rotation_zxy_q14`].
pub fn f_80030998(m: &mut Machine, args: [u32; 4]) -> u32 {
    rotation_zxy_q14(m, args[0], args[1])
}
/// 80030B10 (y, x) -> angle. See [`atan2`].
pub fn f_80030b10(m: &mut Machine, args: [u32; 4]) -> u32 {
    atan2(m, args[0], args[1])
}
/// 80030C74 (value) -> angle. See [`asin`].
pub fn f_80030c74(m: &mut Machine, args: [u32; 4]) -> u32 {
    asin(m, args[0])
}
/// 8003A08C () -> 0/1. See [`target_available`].
pub fn f_8003a08c(m: &mut Machine, _args: [u32; 4]) -> u32 {
    target_available(m)
}
/// 8003A0F8 () -> 11/-1. See [`target_mode_code`].
pub fn f_8003a0f8(m: &mut Machine, _args: [u32; 4]) -> u32 {
    target_mode_code(m)
}
/// 80082740 (value) -> sqrt. See [`square_root`].
pub fn f_80082740(m: &mut Machine, args: [u32; 4]) -> u32 {
    square_root(m, args[0])
}
/// 800827B4 (a, b) -> distance. See [`vector_distance`].
pub fn f_800827b4(m: &mut Machine, args: [u32; 4]) -> u32 {
    vector_distance(m, args[0], args[1])
}

/// 80030248 () -> 0. See [`stop_all_sound`].
pub fn f_80030248(m: &mut Machine, _args: [u32; 4]) -> u32 {
    stop_all_sound(m)
}
/// 8002FF64 () -> 0. See [`release_engine_channels`].
pub fn f_8002ff64(m: &mut Machine, _args: [u32; 4]) -> u32 {
    release_engine_channels(m);
    0
}
/// 8002D980 (channel) -> channel or 0xFF. See [`release_channel`].
pub fn f_8002d980(m: &mut Machine, args: [u32; 4]) -> u32 {
    release_channel(m, args[0])
}
/// 8002F204 () -> 0. See [`reset_sound_sources`].
pub fn f_8002f204(m: &mut Machine, _args: [u32; 4]) -> u32 {
    reset_sound_sources(m);
    0
}
/// 8002E418 (envelope) -> 0. See [`reset_envelope`].
pub fn f_8002e418(m: &mut Machine, args: [u32; 4]) -> u32 {
    reset_envelope(m, args[0]);
    0
}
/// 8002D9B8 () -> 0. See [`stop_channels`].
pub fn f_8002d9b8(m: &mut Machine, _args: [u32; 4]) -> u32 {
    stop_channels(m);
    0
}
/// 8002E198 (channel) -> channel. See [`reset_channel`].
pub fn f_8002e198(m: &mut Machine, args: [u32; 4]) -> u32 {
    reset_channel(m, args[0])
}
/// 8002ED24 () -> 0. See [`reset_channel_groups`].
pub fn f_8002ed24(m: &mut Machine, _args: [u32; 4]) -> u32 {
    reset_channel_groups(m);
    0
}
/// 8002ED5C (group) -> 0. See [`reset_channel_group`].
pub fn f_8002ed5c(m: &mut Machine, args: [u32; 4]) -> u32 {
    reset_channel_group(m, args[0]);
    0
}
/// 8002EE38 () -> 0. See [`reset_sound_events`].
pub fn f_8002ee38(m: &mut Machine, _args: [u32; 4]) -> u32 {
    reset_sound_events(m);
    0
}
/// 8002C280 (request, argument) -> the CD queue block. See [`reset_cd_queue`].
pub fn f_8002c280(m: &mut Machine, args: [u32; 4]) -> u32 {
    reset_cd_queue(m, args[0], args[1])
}
/// 8002C2F4 () -> 0. See [`cancel_stream_requests`].
pub fn f_8002c2f4(m: &mut Machine, _args: [u32; 4]) -> u32 {
    cancel_stream_requests(m);
    0
}
/// 8002C6A8 () -> current file. See [`current_cd_file`].
pub fn f_8002c6a8(m: &mut Machine, _args: [u32; 4]) -> u32 {
    current_cd_file(m)
}
/// 8004CB04 (index) -> 0. See [`mark_unit`].
pub fn f_8004cb04(m: &mut Machine, args: [u32; 4]) -> u32 {
    mark_unit(m, args[0]);
    0
}
/// 8004CB2C (index) -> 0. See [`activate_unit`].
pub fn f_8004cb2c(m: &mut Machine, args: [u32; 4]) -> u32 {
    activate_unit(m, args[0]);
    0
}
/// 8004D2F8 (index) -> 0. See [`mark_object`].
pub fn f_8004d2f8(m: &mut Machine, args: [u32; 4]) -> u32 {
    mark_object(m, args[0]);
    0
}
/// 8004D344 (index) -> 0. See [`activate_object`].
pub fn f_8004d344(m: &mut Machine, args: [u32; 4]) -> u32 {
    activate_object(m, args[0]);
    0
}
/// 8004D59C (index, placement) -> 0. See [`update_marker`].
pub fn f_8004d59c(m: &mut Machine, args: [u32; 4]) -> u32 {
    update_marker(m, args[0], args[1]);
    0
}
/// 8004D6BC (index, placement) -> 0. See [`update_status`].
pub fn f_8004d6bc(m: &mut Machine, args: [u32; 4]) -> u32 {
    update_status(m, args[0], args[1]);
    0
}
/// 800710FC (start) -> v0. See [`place_player`].
pub fn f_800710fc(m: &mut Machine, args: [u32; 4]) -> u32 {
    place_player(m, args[0])
}
/// `gp+0x454`: guidance message request {+0 pending = 1, +1 message,
/// +2 halfword 0}.
const GUIDANCE_MESSAGE: u32 = 0x454;

/// 80041DEC (id) -> `gp+0x454`: request guidance message `id` (stored as
/// `id + 0xC`, low byte).
pub fn request_guidance_message(m: &mut Machine, id: u32) -> u32 {
    let request = crate::GP.wrapping_add(GUIDANCE_MESSAGE);
    m.sb(request, 1);
    m.sb(request.wrapping_add(1), id.wrapping_add(0xc) as u8);
    m.sh(request.wrapping_add(2), 0);
    request
}
/// 80041DEC. See [`request_guidance_message`].
pub fn f_80041dec(m: &mut Machine, args: [u32; 4]) -> u32 {
    request_guidance_message(m, args[0])
}

/// Sound channel records, 0x5C bytes each (+4 the requested sound).
const SOUND_CHANNELS: u32 = 0x800c_43f8;
/// Sound per entity kind (64 bytes, indexed by the kind's low six bits).
const KIND_SOUNDS: u32 = 0x800a_ed84;

/// 8002F2F0 (channel, entity index) -> v0: when entity `index`'s kind
/// (halfword +0x48) is below 0x40, channel record `channel` +4 receives
/// the kind's sound byte, which is returned; otherwise nothing is written
/// and 0 is returned. The third argument register is overwritten unread.
pub fn request_kind_sound(m: &mut Machine, channel: u32, index: u32) -> u32 {
    let table = m.gw(crate::common::ENTITIES);
    let entity = m.lw(table.wrapping_add(index << 2));
    let kind = u32::from(m.lhu(entity.wrapping_add(0x48)));
    if kind >= 0x40 {
        return 0;
    }
    let sound = u32::from(m.lbu(KIND_SOUNDS.wrapping_add(kind & 0x3f)));
    m.sw(
        SOUND_CHANNELS
            .wrapping_add(channel.wrapping_mul(0x5c))
            .wrapping_add(4),
        sound,
    );
    sound
}
/// 8002F2F0. See [`request_kind_sound`].
pub fn f_8002f2f0(m: &mut Machine, args: [u32; 4]) -> u32 {
    request_kind_sound(m, args[0], args[1])
}

/// 8002D854 (a0, voice) -> `voice`: unless `voice` is 0xFF, start it through
/// 8002E0F8 with both arguments unchanged.
pub fn start_voice_unless_none(m: &mut Machine, a0: u32, voice: u32) -> u32 {
    if voice != 0xff {
        crate::dispatch::call(m, 0x8002_e0f8, [a0, voice, 0, 0]);
    }
    voice
}
/// 8002D854. See [`start_voice_unless_none`].
pub fn f_8002d854(m: &mut Machine, args: [u32; 4]) -> u32 {
    start_voice_unless_none(m, args[0], args[1])
}

/// 8002E8EC (record, a, b, c, d) -> 0xFF: fill a sound request record:
/// +0 = 0, +1 = `a`, +2 = `b`, +4 halfword `c`, +3 = 0xFF, +6 = `d` (the
/// fifth argument, the caller's stack word at `sp+0x10`).
pub fn sound_request(m: &mut Machine, record: u32, a: u32, b: u32, c: u32, d: u32) -> u32 {
    m.sb(record, 0);
    m.sb(record.wrapping_add(1), a as u8);
    m.sb(record.wrapping_add(2), b as u8);
    m.sh(record.wrapping_add(4), c as u16);
    m.sb(record.wrapping_add(3), 0xff);
    m.sb(record.wrapping_add(6), d as u8);
    0xff
}
/// 8002E8EC. See [`sound_request`]; the fifth argument is read from the
/// guest stack.
pub fn f_8002e8ec(m: &mut Machine, args: [u32; 4]) -> u32 {
    let d = m.lw(m.sp.wrapping_add(0x10));
    sound_request(m, args[0], args[1], args[2], args[3], d)
}

/// 8002EA60 (record, sound): +0 = 0, +1 = `sound`. Leaves `v0` as the
/// caller had it; callers ignore it (0 here).
pub fn sound_request_reset(m: &mut Machine, record: u32, sound: u32) -> u32 {
    m.sb(record, 0);
    m.sb(record.wrapping_add(1), sound as u8);
    0
}

/// 8002EA6C (record, a, b) -> new flags: +2 = `a`, +4 halfword = `b & 0xFF`,
/// +8 halfword = 0, flags byte +0 |= 1.
pub fn sound_request_first(m: &mut Machine, record: u32, a: u32, b: u32) -> u32 {
    let flags = u32::from(m.lbu(record));
    m.sb(record.wrapping_add(2), a as u8);
    m.sh(record.wrapping_add(4), (b & 0xff) as u16);
    m.sh(record.wrapping_add(8), 0);
    m.sb(record, (flags | 1) as u8);
    flags | 1
}

/// 8002EA8C (record, a, b) -> new flags: +3 = `a`, +6 halfword = `b & 0xFF`,
/// +0xA halfword = 0, flags byte +0 |= 2.
pub fn sound_request_second(m: &mut Machine, record: u32, a: u32, b: u32) -> u32 {
    let flags = u32::from(m.lbu(record));
    m.sb(record.wrapping_add(3), a as u8);
    m.sh(record.wrapping_add(6), (b & 0xff) as u16);
    m.sh(record.wrapping_add(0xa), 0);
    m.sb(record, (flags | 2) as u8);
    flags | 2
}

/// Voice records, 0x14 bytes (+3 in use, +4/+6 volume, +0xC note).
const VOICE_RECORDS: u32 = 0x800c_35f8;
/// 16384-entry signed halfword wave table of the sound envelopes.
const ENVELOPE_WAVE: u32 = 0x8001_0498;

/// 8002E910 (state, request) -> request voice: a request (+0 started, +1/+2
/// sounds, +3 voice, +4 halfword distance threshold, +6 near limit) that has
/// not started starts when the state's distance (`[state]`, unsigned) is
/// below its threshold:
/// - state +4 above the near limit: sound +1 (state +0xA >= 0) or +2
///   (8002D888) on a new voice;
/// - else above half the limit: one of sounds 10..12 at random (8002D724);
/// - either way, then detuned by a random 0..0x3FF (8002ECB8).
///
/// A started request whose voice has stopped (voice record +3 = 0) is reset
/// (voice 0xFF, not started). Returns the request's voice.
pub fn update_sound_request(m: &mut Machine, state: u32, request: u32) -> u32 {
    m.push_frame(0x18);
    if m.lbu(request) == 0 {
        let threshold = m.lh(request.wrapping_add(4)) as i32 as u32;
        if m.lw(state) < threshold {
            let limit = u32::from(m.lbu(request.wrapping_add(6)));
            let distance = m.lw(state.wrapping_add(4));
            if (limit as i32) < (distance as i32) {
                m.sb(request, 1);
                let sound = if m.lh(state.wrapping_add(0xa)) >= 0 {
                    m.lbu(request.wrapping_add(1))
                } else {
                    m.lbu(request.wrapping_add(2))
                };
                let voice =
                    crate::dispatch::call(m, 0x8002_d888, [u32::from(sound), distance, 0, 0]);
                m.sb(request.wrapping_add(3), voice as u8);
            } else if ((limit >> 1) as i32) < (distance as i32) {
                m.sb(request, 1);
                let random = crate::sdk::rand(m);
                let pick = (random as i32).wrapping_rem(3) as u32;
                let voice =
                    crate::dispatch::call(m, 0x8002_d724, [pick.wrapping_add(10), distance, 0, 0]);
                m.sb(request.wrapping_add(3), voice as u8);
            }
            let random = crate::sdk::rand(m);
            let voice = u32::from(m.lbu(request.wrapping_add(3)));
            crate::dispatch::call(
                m,
                0x8002_ecb8,
                [voice, (random & 0x3ff).wrapping_neg(), 0, 0],
            );
        }
    }
    if m.lbu(request) == 1 {
        let voice = u32::from(m.lbu(request.wrapping_add(3)));
        if voice == 0xff {
            m.pop_frame(0x18);
            return 0xff;
        }
        if m.lbu(VOICE_RECORDS + voice * 0x14 + 3) == 0 {
            m.sb(request.wrapping_add(3), 0xff);
            m.sb(request, 0);
        }
    }
    m.pop_frame(0x18);
    u32::from(m.lbu(request.wrapping_add(3)))
}

/// One envelope step: `(wave[(step << 14) / period & 0x3FFF] * depth)` (the
/// original's `div`; a zero period is its divide-by-zero `break` at
/// `trap`).
fn envelope_step(m: &mut Machine, step: u32, period: u32, depth: u32, trap: u32) -> Option<i32> {
    if period == 0 {
        m.raise(crate::machine::Fault::Trap {
            address: trap,
            code: 7,
        });
        return None;
    }
    let phase = ((m.lh(step) as i32) << 14) / period as i32;
    let wave = m.lh(ENVELOPE_WAVE + ((phase as u32 & 0x3fff) << 1)) as i32;
    Some(wave.wrapping_mul(m.lh(depth) as i32))
}

/// Advance a request's step counter at `step` modulo `period`.
fn envelope_advance(m: &mut Machine, step: u32, period: u32) {
    let next = m.lhu(step).wrapping_add(1);
    m.sh(step, next);
    if i32::from(next as i16) >= period as i32 {
        m.sh(step, 0);
    }
}

/// 8002EAAC (request) -> request +1: the envelopes of a sound request
/// (+1 voice, 0xFF none, +0 flags). Flag 1: both volumes of the voice
/// record (+4, +6) move by `wave * depth(+4) >> 14` and are clamped to
/// 0..0x3FFF; step +8 runs modulo period +2. Flag 2: the voice's note (+0xC)
/// is offset through 8002E230 by `(wave * depth(+6) << 2) >> 16`; step +0xA
/// runs modulo period +3.
pub fn update_sound_envelopes(m: &mut Machine, request: u32) -> u32 {
    if m.lbu(request.wrapping_add(1)) == 0xff {
        return 0xff;
    }
    m.push_frame(0x20);
    let voice = u32::from(m.lbu(request.wrapping_add(1)));
    let record = VOICE_RECORDS.wrapping_add(voice * 0x14);
    if m.lbu(request) & 1 != 0 {
        let period = u32::from(m.lbu(request.wrapping_add(2)));
        let Some(product) = envelope_step(m, request + 8, period, request + 4, 0x8002_eb10) else {
            m.pop_frame(0x20);
            return 0;
        };
        let delta = (product >> 14) as u32;
        let left = u32::from(m.lhu(record + 4)).wrapping_add(delta);
        m.sh(record + 4, left as u16);
        let right = u32::from(m.lhu(record + 6)).wrapping_add(delta);
        m.sh(record + 6, right as u16);
        for offset in [4, 6] {
            if m.lh(record + offset) >= 0x4000 {
                m.sh(record + offset, 0x3fff);
            }
        }
        for offset in [4, 6] {
            if m.lh(record + offset) < 0 {
                m.sh(record + offset, 0);
            }
        }
        envelope_advance(m, request + 8, period);
    }
    if m.lbu(request) & 2 != 0 {
        let period = u32::from(m.lbu(request.wrapping_add(3)));
        let Some(product) = envelope_step(m, request + 0xa, period, request + 6, 0x8002_ec1c)
        else {
            m.pop_frame(0x20);
            return 0;
        };
        let note = m.lh(record + 0xc) as i32 as u32;
        let offset = ((product as u32) << 2) as i32 >> 16;
        let note = crate::dispatch::call(m, 0x8002_e230, [note, offset as u32, 0, 0]);
        m.sh(record + 0xc, note as u16);
        envelope_advance(m, request + 0xa, period);
    }
    m.pop_frame(0x20);
    u32::from(m.lbu(request.wrapping_add(1)))
}

/// 8002E658 (source, target, params) -> `source`: pan a sound between two
/// voices (0xFF: none, returns 0xFF). Both voice records get the volume
/// update request (+1 |= 0x10); the target takes the source's note. With
/// balance `b = params +0xA`:
/// - `b > 0x400`: the target falls silent (volumes 0);
/// - `b < -0x400`: the target takes the source's volumes and the source
///   falls silent;
/// - otherwise the angle `((b + 0x400) * 2) & 0x3FFF` splits the source's
///   volumes, `cos` part to the target and `sin` part kept (wave table
///   80010498, read before either store).
///
/// The phase of the target is then inverted on one side: the left side when
/// the target's old left volume was negative, the right side when its old
/// right volume had bit 15 set, otherwise the louder side.
pub fn pan_sound(m: &mut Machine, source: u32, target: u32, params: u32) -> u32 {
    if source == 0xff || target == 0xff {
        return 0xff;
    }
    let from = VOICE_RECORDS.wrapping_add(source * 0x14);
    let flags = m.lbu(from + 1) | 0x10;
    m.sb(from + 1, flags);
    let to = VOICE_RECORDS.wrapping_add(target * 0x14);
    let old_left = m.lh(to + 4);
    let flags = m.lbu(to + 1) | 0x10;
    m.sb(to + 1, flags);
    let note = m.lhu(from + 0xc);
    m.sh(to + 0xc, note);
    let side = if old_left < 0 {
        1
    } else {
        (u32::from(m.lhu(to + 6)) >> 14) & 2
    };
    let balance = i32::from(m.lh(params.wrapping_add(0xa)));
    if balance >= 0x401 {
        m.sh(to + 4, 0);
        m.sh(to + 6, 0);
    } else if balance < -0x400 {
        let left = m.lhu(from + 4);
        m.sh(to + 4, left);
        let right = m.lhu(from + 6);
        m.sh(to + 6, right);
        m.sh(from + 4, 0);
        m.sh(from + 6, 0);
    } else {
        let angle = ((balance + 0x400) as u32) << 1 & 0x3fff;
        let cos = i32::from(m.lh(ENVELOPE_WAVE + ((angle + 0x1000) << 1)));
        let left = i32::from(m.lh(from + 4)).wrapping_mul(cos) >> 14;
        m.sh(to + 4, left as u16);
        let right = i32::from(m.lh(from + 6)).wrapping_mul(cos) >> 14;
        let sin = i32::from(m.lh(ENVELOPE_WAVE + (angle << 1)));
        m.sh(to + 6, right as u16);
        let kept_left = i32::from(m.lh(from + 4)).wrapping_mul(sin) >> 14;
        let kept_right = i32::from(m.lh(from + 6)).wrapping_mul(sin) >> 14;
        m.sh(from + 4, kept_left as u16);
        m.sh(from + 6, kept_right as u16);
    }
    match side {
        1 => {
            let left = m.lhu(to + 4).wrapping_neg();
            m.sh(to + 4, left);
        }
        2 => {
            let right = m.lhu(to + 6).wrapping_neg();
            m.sh(to + 6, right);
        }
        _ => {
            if m.lh(to + 6) < m.lh(to + 4) {
                let right = m.lhu(to + 6).wrapping_neg();
                m.sh(to + 6, right);
            } else {
                let left = m.lhu(to + 4).wrapping_neg();
                m.sh(to + 4, left);
            }
        }
    }
    source
}

/// 8002FE9C (voice, current, target, step) -> 8002DAA0's `v0`: move each
/// halfword of `target` (two volumes) toward `current` by at most `step`
/// (`target + clamp(current - target, -step, step)`, clamped low first,
/// then high), then set them on `voice & 0xFF` through 8002DAA0.
pub fn approach_volume(m: &mut Machine, voice: u32, current: u32, target: u32, step: u32) -> u32 {
    m.push_frame(0x18);
    let clamp = |difference: u32| {
        let low = step.wrapping_neg();
        let mut value = difference;
        if (value as i32) < (low as i32) {
            value = low;
        }
        if (step as i32) < (value as i32) {
            value = step;
        }
        value
    };
    let base_left = m.lh(target) as i32 as u32;
    let left = clamp((m.lh(current) as i32 as u32).wrapping_sub(base_left)).wrapping_add(base_left);
    let base_right = m.lh(target.wrapping_add(2)) as i32 as u32;
    let right = clamp((m.lh(current.wrapping_add(2)) as i32 as u32).wrapping_sub(base_right))
        .wrapping_add(base_right);
    let result = crate::dispatch::call(
        m,
        0x8002_daa0,
        [
            voice & 0xff,
            left as i16 as i32 as u32,
            right as i16 as i32 as u32,
            step,
        ],
    );
    m.pop_frame(0x18);
    result
}

/// Text state: +0/+2 start x/y, +4 texture page, +6 CLUT, +8..+0xA RGB.
const TEXT_STATE: u32 = 0x800c_5790;
/// Font: 4-byte glyphs {u, v, width, y offset (signed)} for characters
/// from 0x20; entry 0's width is the space width and its y offset byte the
/// line height.
const FONT: u32 = 0x800b_05e8;

/// `addPrim(ot, packet)` as the original inlines it.
fn add_prim(m: &mut Machine, ot: u32, packet: u32) {
    let tag = m.lw(packet) & 0xff00_0000 | m.lw(ot) & 0x00ff_ffff;
    m.sw(packet, tag);
    let head = m.lw(ot) & 0xff00_0000 | packet & 0x00ff_ffff;
    m.sw(ot, head);
}

/// 8005AC84 (ot, packets, string) -> next packet: one textured sprite
/// (SPRT, code 0x66) per printable character (> 0x20) of the
/// NUL-terminated `string`, linked at `ot`, laid out from the text state's
/// start position: `\n` returns to the start x and adds the line height,
/// `\t` advances x to the next multiple of four space widths (signed 16-bit
/// x; a zero space width is the original's divide-by-zero `break`), a space
/// adds the space width and other control characters are skipped. When at
/// least one sprite was written, a draw-mode packet (E1h with the text
/// state's texture page) follows them and is linked last; the result is the
/// address after it, otherwise `packets`.
pub fn draw_text(m: &mut Machine, ot: u32, packets: u32, string: u32) -> u32 {
    let mut cursor = packets;
    let mut count = 0u32;
    let mut x = u32::from(m.lhu(TEXT_STATE));
    let mut y = u32::from(m.lhu(TEXT_STATE + 2));
    let mut text = string;
    let mut c = u32::from(m.lbu(text));
    while c != 0 {
        if c == 0x0a {
            x = u32::from(m.lhu(TEXT_STATE));
            y = y.wrapping_add(m.lb(FONT + 3) as i32 as u32);
        } else if c == 0x09 {
            let tab = u32::from(m.lbu(FONT + 2)) << 2;
            if tab == 0 {
                m.raise(crate::machine::Fault::Trap {
                    address: 0x8005_ad10,
                    code: 7,
                });
                return packets;
            }
            let column = i32::from(x as i16) / tab as i32;
            x = (column.wrapping_add(1) as u32).wrapping_mul(tab);
        } else if c == 0x20 {
            x = x.wrapping_add(u32::from(m.lbu(FONT + 2)));
        } else if c > 0x20 {
            let glyph = FONT + (((c - 0x20) & 0xff) << 2);
            m.sb(cursor + 3, 4);
            m.sb(cursor + 7, 0x64);
            for (i, offset) in [8u32, 9, 0xa].into_iter().enumerate() {
                let channel = m.lbu(TEXT_STATE + offset);
                m.sb(cursor + 4 + i as u32, channel);
            }
            m.sh(cursor + 8, x as u16);
            let y_offset = m.lb(glyph + 3) as i32 as u32;
            m.sh(cursor + 0xa, y.wrapping_add(y_offset) as u16);
            let u = m.lbu(glyph);
            m.sb(cursor + 0xc, u);
            let v = m.lbu(glyph + 1);
            m.sb(cursor + 0xd, v);
            let width = m.lbu(glyph + 2);
            m.sh(cursor + 0x10, u16::from(width));
            let height = m.lb(FONT + 3);
            count += 1;
            m.sh(cursor + 0x12, height as i16 as u16);
            let code = m.lbu(cursor + 7) | 2;
            let clut = m.lhu(TEXT_STATE + 6);
            m.sb(cursor + 7, code);
            m.sh(cursor + 0xe, clut);
            add_prim(m, ot, cursor);
            x = x.wrapping_add(u32::from(m.lbu(glyph + 2)));
            cursor += 0x14;
        }
        text = text.wrapping_add(1);
        c = u32::from(m.lbu(text));
    }
    if count == 0 {
        return packets;
    }
    m.sb(cursor + 3, 1);
    let tpage = u32::from(m.lhu(TEXT_STATE + 4)) & 0x9ff;
    m.sw(cursor + 4, 0xe100_0200 | tpage);
    add_prim(m, ot, cursor);
    cursor + 8
}
/// 8005AC84. See [`draw_text`].
pub fn f_8005ac84(m: &mut Machine, args: [u32; 4]) -> u32 {
    draw_text(m, args[0], args[1], args[2])
}

/// 8006D7DC (entity, state) -> heading. See [`relative_angles`].
pub fn f_8006d7dc(m: &mut Machine, args: [u32; 4]) -> u32 {
    relative_angles(m, args[0], args[1])
}
/// 80077D54 (camera, mode, entity) -> v0. See [`set_camera_target`].
pub fn f_80077d54(m: &mut Machine, args: [u32; 4]) -> u32 {
    set_camera_target(m, args[0], args[1], args[2])
}
/// 800761FC (camera) -> word. See [`mark_camera`].
pub fn f_800761fc(m: &mut Machine, args: [u32; 4]) -> u32 {
    mark_camera(m, args[0])
}

/// 80045874 (slot) -> 0xFF. See [`release_lock`].
pub fn f_80045874(m: &mut Machine, args: [u32; 4]) -> u32 {
    release_lock(m, args[0])
}
/// 8004577C (slot, target) -> target or 0xFF. See [`set_lock`].
pub fn f_8004577c(m: &mut Machine, args: [u32; 4]) -> u32 {
    set_lock(m, args[0], args[1])
}
/// 8007C8AC (words) -> address. See [`reserve_events`].
pub fn f_8007c8ac(m: &mut Machine, args: [u32; 4]) -> u32 {
    reserve_events(m, args[0])
}
/// 8007C7E8 (words). See [`drop_events`].
pub fn f_8007c7e8(m: &mut Machine, args: [u32; 4]) -> u32 {
    drop_events(m, args[0]);
    0
}
/// 8007C778 (event) -> length. See [`apply_event`].
pub fn f_8007c778(m: &mut Machine, args: [u32; 4]) -> u32 {
    apply_event(m, args[0])
}
/// 8007C5C0 (event, copies). See [`apply_word_event`].
pub fn f_8007c5c0(m: &mut Machine, args: [u32; 4]) -> u32 {
    apply_word_event(m, args[0], args[1]);
    0
}
/// 8007C6DC (event, copies). See [`apply_position_event`].
pub fn f_8007c6dc(m: &mut Machine, args: [u32; 4]) -> u32 {
    apply_position_event(m, args[0], args[1]);
    0
}

/// Native targets owned by this module for indirect calls (`jalr`) and
/// calls by original address: every routine above.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let entry: fn(&mut Machine, [u32; 4]) -> u32 = match target {
        0x8002_70fc => f_800270fc,
        0x8002_daa0 => f_8002daa0,
        0x8003_0534 => f_80030534,
        0x8003_06ac => f_800306ac,
        0x8003_0824 => f_80030824,
        0x8003_0998 => f_80030998,
        0x8003_0b10 => f_80030b10,
        0x8003_0c74 => f_80030c74,
        0x8003_a08c => f_8003a08c,
        0x8003_a0f8 => f_8003a0f8,
        0x8008_2740 => f_80082740,
        0x8008_27b4 => f_800827b4,
        0x8003_0248 => f_80030248,
        0x8002_ff64 => f_8002ff64,
        0x8002_d980 => f_8002d980,
        0x8002_f204 => f_8002f204,
        0x8002_e418 => f_8002e418,
        0x8002_d9b8 => f_8002d9b8,
        0x8002_e198 => f_8002e198,
        0x8002_ed24 => f_8002ed24,
        0x8002_ed5c => f_8002ed5c,
        0x8002_ee38 => f_8002ee38,
        0x8002_f2f0 => f_8002f2f0,
        0x8002_d854 => f_8002d854,
        0x8002_e8ec => f_8002e8ec,
        0x8002_e910 => |m, a| update_sound_request(m, a[0], a[1]),
        0x8002_e658 => |m, a| pan_sound(m, a[0], a[1], a[2]),
        0x8002_fe9c => |m, a| approach_volume(m, a[0], a[1], a[2], a[3]),
        0x8002_eaac => |m, a| update_sound_envelopes(m, a[0]),
        0x8002_ea60 => |m, a| sound_request_reset(m, a[0], a[1]),
        0x8002_ea6c => |m, a| sound_request_first(m, a[0], a[1], a[2]),
        0x8002_ea8c => |m, a| sound_request_second(m, a[0], a[1], a[2]),
        0x8005_ac84 => f_8005ac84,
        0x8004_1dec => f_80041dec,
        0x8002_c280 => f_8002c280,
        0x8002_c2f4 => f_8002c2f4,
        0x8002_c6a8 => f_8002c6a8,
        0x8004_cb04 => f_8004cb04,
        0x8004_cb2c => f_8004cb2c,
        0x8004_d2f8 => f_8004d2f8,
        0x8004_d344 => f_8004d344,
        0x8004_d59c => f_8004d59c,
        0x8004_d6bc => f_8004d6bc,
        0x8007_10fc => f_800710fc,
        0x8006_d7dc => f_8006d7dc,
        0x8007_7d54 => f_80077d54,
        0x8007_61fc => f_800761fc,
        0x8004_5874 => f_80045874,
        0x8004_577c => f_8004577c,
        0x8007_c8ac => f_8007c8ac,
        0x8007_c7e8 => f_8007c7e8,
        0x8007_c778 => f_8007c778,
        0x8007_c5c0 => f_8007c5c0,
        0x8007_c6dc => f_8007c6dc,
        _ => return None,
    };
    Some(entry(m, args))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic atan table (entry i = i) and octant bases 0x100 * k: the
    /// octant/negation choice of every quadrant and the edge inputs.
    #[test]
    fn atan2_paths() {
        let mut m = Machine::default();
        for i in 0..=0x800u32 {
            m.sh(ATAN_TABLE + i * 2, i as u16);
        }
        for k in 0..8 {
            m.sw(OCTANT_TABLE + k * 4, k * 0x100);
        }
        let at = |m: &mut Machine, y: i32, x: i32| atan2(m, y as u32, x as u32) as i32;
        assert_eq!(at(&mut m, 0, 0), 0);
        // |y| < |x|: ratio (|y| << 11) / |x|, octants 0/2/4/6.
        assert_eq!(at(&mut m, 1, 4), 0x200);
        assert_eq!(at(&mut m, 1, -4), 0x200 - 0x200, "octant 2, negated");
        assert_eq!(at(&mut m, -1, 4), 0x400 - 0x200, "octant 4, negated");
        assert_eq!(at(&mut m, -1, -4), 0x600 + 0x200, "octant 6");
        // |y| > |x|: ratio (|x| << 11) / |y|, octant +1, negation flipped.
        assert_eq!(at(&mut m, 4, 1), 0x100 - 0x200);
        // |y| == |x|: no octant change.
        assert_eq!(at(&mut m, 3, 3), 0x800);
        assert_eq!(at(&mut m, 5, 0), 0x100, "x = 0: ratio 0");
        // A wrapped negation of i32::MIN reaches neither divide trap.
        let _ = at(&mut m, i32::MIN, 1);
        let _ = at(&mut m, 1, i32::MIN);
        let _ = at(&mut m, i32::MIN, i32::MIN);
        assert!(m.take_fault().is_ok());
        // Results at or above 0x2000 wrap by 0x4000.
        m.sw(OCTANT_TABLE, 0x2000);
        assert_eq!(at(&mut m, 0, 4), 0x2000 - 0x4000);
        assert!(m.take_fault().is_ok());
    }
}
