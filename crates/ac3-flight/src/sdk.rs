//! PsyQ SDK routines linked into the resident executable.
//!
//! Pure CPU/GTE routines are translated here once for every subsystem. Their
//! pointer arguments are guest addresses, exactly as in the original ABI.
//! Device-facing routines (GPU environment/ordering-table submission, SPU,
//! CD, VSync, root counters, pads) are not emulated: they append a
//! [`HostCall`](crate::machine::HostCall) and return the value documented on
//! each function. Their RAM effects that are not device state (driver
//! bookkeeping, tables a DMA writes) are reproduced exactly; every such
//! function documents the RAM it writes.
//!
//! Host calls carry only the original register arguments: a host bridge
//! that needs a structure an argument points to (SpuVoiceAttr, DRAWENV,
//! DISPENV, ...) must read it when the call is made, since stack locals
//! do not survive the frame. Device results are taken from
//! `Machine::host_results` in call order, as each function documents.
//!
//! Original trap instructions (`break 6/7` after a division check) have no
//! translated continuation: reaching one records `Fault::Untranslated` with
//! the address of the `break` and the routine returns the value the MIPS
//! division produces.
use crate::machine::{Fault, Machine};
use ac3_gte::{ctrl, reg};

/// `break` at `address`: no continuation is translated (see module docs).
fn trap(m: &mut Machine, address: u32) {
    m.raise(Fault::Untranslated { address });
}

/// MIPS `div` (low word of the quotient) behind the compiler's `break 7`
/// (divisor 0) and `break 6` (`0x80000000 / -1`) checks at the given
/// addresses.
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

/// `ldv0`: VXY0/VZ0 from an SVECTOR (two `lwc2`).
fn load_v0(m: &mut Machine, vector: u32) {
    let xy = m.lw(vector);
    let z = m.lw(vector.wrapping_add(4));
    m.gte.mtc2(reg::VXY0, xy);
    m.gte.mtc2(reg::VZ0, z);
}

fn at(base: u32, offset: u32) -> u32 {
    base.wrapping_add(offset)
}

// ---- matrix and geometry registers ------------------------------------------

/// 800A5518 SetRotMatrix(MATRIX*): five words into R11R12..R33.
pub fn set_rot_matrix(m: &mut Machine, matrix: u32) {
    let words: [u32; 5] = std::array::from_fn(|i| m.lw(at(matrix, i as u32 * 4)));
    for (i, word) in words.into_iter().enumerate() {
        m.gte.ctc2(ctrl::R11R12 + i, word);
    }
}

/// 800A5548 SetColorMatrix(MATRIX*): five words into LR1LR2..LB3.
pub fn set_color_matrix(m: &mut Machine, matrix: u32) {
    let words: [u32; 5] = std::array::from_fn(|i| m.lw(at(matrix, i as u32 * 4)));
    for (i, word) in words.into_iter().enumerate() {
        m.gte.ctc2(ctrl::LR1LR2 + i, word);
    }
}

/// 800A5578 SetTransMatrix(MATRIX*): words +0x14/+0x18/+0x1C into TRX..TRZ.
pub fn set_trans_matrix(m: &mut Machine, matrix: u32) {
    let words: [u32; 3] = std::array::from_fn(|i| m.lw(at(matrix, 0x14 + i as u32 * 4)));
    for (i, word) in words.into_iter().enumerate() {
        m.gte.ctc2(ctrl::TRX + i, word);
    }
}

/// 800A5598 ReadGeomOffset(long* ofx, long* ofy): OFX/OFY >> 16 (arithmetic).
pub fn read_geom_offset(m: &mut Machine, ofx: u32, ofy: u32) {
    let x = (m.gte.cfc2(ctrl::OFX) as i32) >> 16;
    let y = (m.gte.cfc2(ctrl::OFY) as i32) >> 16;
    m.sw(ofx, x as u32);
    m.sw(ofy, y as u32);
}

/// 800A55B8 ReadGeomScreen(): the stored H word (sign-extended by `ctc2`).
pub fn read_geom_screen(m: &mut Machine) -> u32 {
    m.gte.cfc2(ctrl::H)
}

/// 800A55C8 SetBackColor(r, g, b): RBK/GBK/BBK = component << 4.
pub fn set_back_color(m: &mut Machine, r: u32, g: u32, b: u32) {
    m.gte.ctc2(ctrl::RBK, r << 4);
    m.gte.ctc2(ctrl::RBK + 1, g << 4);
    m.gte.ctc2(ctrl::RBK + 2, b << 4);
}

/// 800A55E8 SetGeomOffset(x, y): OFX/OFY = value << 16.
pub fn set_geom_offset(m: &mut Machine, x: u32, y: u32) {
    m.gte.ctc2(ctrl::OFX, x << 16);
    m.gte.ctc2(ctrl::OFY, y << 16);
}

/// 800A5608 SetGeomScreen(h).
pub fn set_geom_screen(m: &mut Machine, h: u32) {
    m.gte.ctc2(ctrl::H, h);
}

// ---- transforms --------------------------------------------------------------

/// 800A5828 RotTransPers(SVECTOR* v, long* sxy, long* p, long* flag):
/// RTPS (sf=1). Stores SXY2, the IR0 word and FLAG; returns SZ3 >> 2.
pub fn rot_trans_pers(m: &mut Machine, vector: u32, sxy: u32, p: u32, flag: u32) -> u32 {
    load_v0(m, vector);
    m.gte.rtps(true, false);
    let screen = m.gte.mfc2(reg::SXY2);
    m.sw(sxy, screen);
    let depth_cue = m.gte.mfc2(reg::IR0);
    m.sw(p, depth_cue);
    let flags = m.gte.cfc2(ctrl::FLAG);
    let z = m.gte.mfc2(reg::SZ3);
    m.sw(flag, flags);
    ((z as i32) >> 2) as u32
}

/// 800A5858 RotTransPers3(v0, v1, v2, sxy0, sxy1, sxy2, p, flag): RTPT
/// (sf=1). The last four arguments are the caller's stack words +0x10..+0x1C.
pub fn rot_trans_pers3(
    m: &mut Machine,
    vectors: [u32; 3],
    sxy: [u32; 3],
    p: u32,
    flag: u32,
) -> u32 {
    for (i, vector) in vectors.into_iter().enumerate() {
        let xy = m.lw(vector);
        let z = m.lw(at(vector, 4));
        m.gte.mtc2(reg::VXY0 + i * 2, xy);
        m.gte.mtc2(reg::VZ0 + i * 2, z);
    }
    m.gte.rtpt(true, false);
    for (i, out) in sxy.into_iter().enumerate() {
        let screen = m.gte.mfc2(reg::SXY0 + i);
        m.sw(out, screen);
    }
    let depth_cue = m.gte.mfc2(reg::IR0);
    m.sw(p, depth_cue);
    let flags = m.gte.cfc2(ctrl::FLAG);
    let z = m.gte.mfc2(reg::SZ3);
    m.sw(flag, flags);
    ((z as i32) >> 2) as u32
}

/// 800A58B8 RotTrans(SVECTOR* v, VECTOR* out, long* flag): MVMVA sf=1,
/// rotation, V0, translation; stores MAC1-3 and FLAG; returns FLAG.
pub fn rot_trans(m: &mut Machine, vector: u32, out: u32, flag: u32) -> u32 {
    load_v0(m, vector);
    m.gte.mvmva(true, false, 0, 0, 0);
    for i in 0..3 {
        let value = m.gte.mfc2(reg::MAC1 + i);
        m.sw(at(out, i as u32 * 4), value);
    }
    let flags = m.gte.cfc2(ctrl::FLAG);
    m.sw(flag, flags);
    flags
}

/// 800A5348 ApplyMatrixSV(MATRIX* m, SVECTOR* v, SVECTOR* out): loads the
/// rotation registers, RTV0 (sf=1, no translation), stores IR1-3 halfwords.
pub fn apply_matrix_sv(m: &mut Machine, matrix: u32, vector: u32, out: u32) -> u32 {
    set_rot_matrix(m, matrix);
    load_v0(m, vector);
    m.gte.mvmva(true, false, 0, 0, 3);
    let ir: [u32; 3] = std::array::from_fn(|i| m.gte.mfc2(reg::IR1 + i));
    for (i, value) in ir.into_iter().enumerate() {
        m.sh(at(out, i as u32 * 2), value as u16);
    }
    out
}

/// One component of the long-vector split used by ApplyMatrixLV and
/// ApplyRotMatrixLV: `(x >> 15, x & 0x7FFF)` on the magnitude, with the sign
/// restored on both parts (`subu` wraps, so 0x80000000 gives (0x10000, 0)).
fn split_long(x: u32) -> (u32, u32) {
    let x = x as i32;
    if x >= 0 {
        ((x >> 15) as u32, (x & 0x7fff) as u32)
    } else {
        let magnitude = x.wrapping_neg();
        (
            (magnitude >> 15).wrapping_neg() as u32,
            (magnitude & 0x7fff).wrapping_neg() as u32,
        )
    }
}

/// Shared body of 800A4D78/800A4FE8 after the rotation registers are set:
/// RTIR (sf=0) on the high parts, RTIR (sf=1) on the low parts, result
/// `high << 3 + low` per component (the original's sign-symmetric shift
/// equals a wrapping `<< 3`).
fn apply_long(m: &mut Machine, vector: u32, out: u32) {
    let parts: [(u32, u32); 3] =
        std::array::from_fn(|i| split_long(m.lw(at(vector, i as u32 * 4))));
    for (i, part) in parts.iter().enumerate() {
        m.gte.mtc2(reg::IR1 + i, part.0);
    }
    m.gte.mvmva(false, false, 0, 3, 3);
    let high: [u32; 3] = std::array::from_fn(|i| m.gte.mfc2(reg::MAC1 + i));
    for (i, part) in parts.iter().enumerate() {
        m.gte.mtc2(reg::IR1 + i, part.1);
    }
    m.gte.mvmva(true, false, 0, 3, 3);
    let low: [u32; 3] = std::array::from_fn(|i| m.gte.mfc2(reg::MAC1 + i));
    for i in 0..3 {
        m.sw(at(out, i as u32 * 4), low[i].wrapping_add(high[i] << 3));
    }
}

/// 800A4D78 ApplyMatrixLV(MATRIX* m, VECTOR* v, VECTOR* out) -> out: loads
/// the rotation registers from `m` (they stay loaded) and transforms the
/// 32-bit vector in two 15-bit passes.
pub fn apply_matrix_lv(m: &mut Machine, matrix: u32, vector: u32, out: u32) -> u32 {
    set_rot_matrix(m, matrix);
    apply_long(m, vector, out);
    out
}

/// 800A4FE8 ApplyRotMatrixLV(VECTOR* v, VECTOR* out) -> out, with the
/// current rotation registers.
pub fn apply_rot_matrix_lv(m: &mut Machine, vector: u32, out: u32) -> u32 {
    apply_long(m, vector, out);
    out
}

/// Shared body of MulMatrix/MulMatrix2/MulMatrix0: the product words (see
/// [`mul_matrix_words`]) stored at `out` in the original order. The last
/// word is the `swc2` of IR3, so `out + 0x12` receives IR3's sign
/// extension. All loads precede the stores.
fn mul_matrix_into(m: &mut Machine, left: u32, right: u32, out: u32) {
    let words = mul_matrix_words(m, left, right);
    for index in [0, 3, 1, 2, 4] {
        m.sw(at(out, index as u32 * 4), words[index]);
    }
}

/// The product `left * right` as the five MATRIX words (+0..+0x10):
/// rotation = `left` (it stays loaded), each column of `right` through RTV0
/// (sf=1), as MulMatrix0 (800A4B08) and SetMulMatrix (800A4ED8) load them.
fn mul_matrix_words(m: &mut Machine, left: u32, right: u32) -> [u32; 5] {
    set_rot_matrix(m, left);
    let xy = u32::from(m.lhu(right)) | (m.lw(at(right, 4)) & 0xffff_0000);
    let z = m.lw(at(right, 0xc));
    m.gte.mtc2(reg::VXY0, xy);
    m.gte.mtc2(reg::VZ0, z);
    m.gte.mvmva(true, false, 0, 0, 3);
    let xy = u32::from(m.lhu(at(right, 2))) | (m.lw(at(right, 8)) << 16);
    let z = m.lh(at(right, 0xe)) as i32 as u32;
    let c0: [u32; 3] = std::array::from_fn(|i| m.gte.mfc2(reg::IR1 + i));
    m.gte.mtc2(reg::VXY0, xy);
    m.gte.mtc2(reg::VZ0, z);
    m.gte.mvmva(true, false, 0, 0, 3);
    let xy = u32::from(m.lhu(at(right, 4))) | (m.lw(at(right, 8)) & 0xffff_0000);
    let z = m.lw(at(right, 0x10));
    let c1: [u32; 3] = std::array::from_fn(|i| m.gte.mfc2(reg::IR1 + i));
    m.gte.mtc2(reg::VXY0, xy);
    m.gte.mtc2(reg::VZ0, z);
    m.gte.mvmva(true, false, 0, 0, 3);
    let c2: [u32; 3] = std::array::from_fn(|i| m.gte.mfc2(reg::IR1 + i));
    [
        (c1[0] << 16) | (c0[0] & 0xffff),
        (c2[0] & 0xffff) | (c0[1] << 16),
        (c2[1] << 16) | (c1[1] & 0xffff),
        (c1[2] << 16) | (c0[2] & 0xffff),
        c2[2],
    ]
}

/// 800A5128 MulMatrix(MATRIX* m0, MATRIX* m1) -> m0 (m0 = m0 * m1).
pub fn mul_matrix(m: &mut Machine, m0: u32, m1: u32) -> u32 {
    mul_matrix_into(m, m0, m1, m0);
    m0
}

/// 800A5238 MulMatrix2(MATRIX* m0, MATRIX* m1) -> m1 (m1 = m0 * m1).
pub fn mul_matrix2(m: &mut Machine, m0: u32, m1: u32) -> u32 {
    mul_matrix_into(m, m0, m1, m1);
    m1
}

/// 800A5918 TransposeMatrix(MATRIX* src, MATRIX* dst) -> dst. The original
/// load/store interleaving is kept so `src == dst` behaves identically.
pub fn transpose_matrix(m: &mut Machine, src: u32, dst: u32) -> u32 {
    let t1 = m.lw(src);
    let t2 = m.lw(at(src, 4));
    m.sw(at(dst, 4), t1);
    m.sw(dst, t2);
    m.sh(dst, t1 as u16);
    let t3 = m.lw(at(src, 8));
    let t1 = m.lw(at(src, 0xc));
    m.sw(at(dst, 0xc), t3);
    m.sw(at(dst, 8), t1);
    m.sh(at(dst, 0xc), t2 as u16);
    m.sh(at(dst, 8), t3 as u16);
    let t2 = m.lh(at(src, 0x10));
    m.sh(at(dst, 4), t1 as u16);
    m.sh(at(dst, 0x10), t2 as u16);
    dst
}

// ---- scalar math -------------------------------------------------------------

/// 800B4C54: SquareRoot0 mantissa table (halfwords, indexed by the
/// normalized top bits minus 64).
const SQRT_TABLE: u32 = 0x800b_4c54;
/// 800B4DF8: reciprocal square-root table of 800A4958.
const RSQRT_TABLE: u32 = 0x800b_4df8;
/// 800B4F84: 4096 words, low halfword sin, high halfword cos (4.12).
pub const SIN_COS_TABLE: u32 = 0x800b_4f84;
/// 800B8F84: ratan2 table, 1025 halfwords.
const ATAN_TABLE: u32 = 0x800b_8f84;

/// The table step shared by SquareRoot0 and 800A4958: normalize `value` by
/// its even leading-sign-bit count `lz & !1`, returning (table byte offset
/// `(top - 64) * 2`, result shift `(31 - even) >> 1`).
fn normalize_for_table(value: u32, lz: u32) -> (u32, u32) {
    let even = lz & !1;
    let shift = (31i32.wrapping_sub(even as i32) >> 1) as u32;
    let excess = (even as i32).wrapping_sub(24);
    let top = if excess >= 0 {
        value << (excess as u32 & 31)
    } else {
        ((value as i32) >> ((24i32.wrapping_sub(even as i32)) as u32 & 31)) as u32
    };
    (top.wrapping_sub(0x40) << 1, shift)
}

/// 800A47C8 SquareRoot0(long a) -> sqrt(a) (integer). LZCS/LZCR count the
/// leading sign bits; a count of 32 (0 or -1) returns 0. Negative inputs read
/// whatever halfword precedes the table, as the original does.
pub fn square_root0(m: &mut Machine, value: u32) -> u32 {
    m.gte.mtc2(reg::LZCS, value);
    let lz = m.gte.mfc2(reg::LZCR);
    if lz == 32 {
        return 0;
    }
    let (offset, shift) = normalize_for_table(value, lz);
    let mantissa = m.lh(SQRT_TABLE.wrapping_add(offset)) as i32 as u32;
    (mantissa << (shift & 31)) >> 12
}

/// 800A4958 (shared core of the VectorNormal family): with the vector in
/// IR1-3, SQR (sf=0, lm=1), sum MAC1-3, look up 1/sqrt by the normalized
/// sum, GPF (sf=0) with IR0 = that factor, and return MAC1-3 >> shift and
/// the sum (left in `v0`).
fn normal_core(m: &mut Machine, v: [u32; 3]) -> ([u32; 3], u32) {
    for (i, &value) in v.iter().enumerate() {
        m.gte.mtc2(reg::IR1 + i, value);
    }
    m.gte.sqr(false, true);
    let mac: [u32; 3] = std::array::from_fn(|i| m.gte.mfc2(reg::MAC1 + i));
    // `add` (overflow-trapping in hardware; never reached by compiled data).
    let sum = mac[0].wrapping_add(mac[1]).wrapping_add(mac[2]);
    m.gte.mtc2(reg::LZCS, sum);
    let lz = m.gte.mfc2(reg::LZCR);
    let (offset, shift) = normalize_for_table(sum, lz);
    let factor = m.lh(RSQRT_TABLE.wrapping_add(offset)) as i32 as u32;
    m.gte.mtc2(reg::IR0, factor);
    for (i, &value) in v.iter().enumerate() {
        m.gte.mtc2(reg::IR1 + i, value);
    }
    m.gte.gpf(false, false);
    let out = std::array::from_fn(|i| ((m.gte.mfc2(reg::MAC1 + i) as i32) >> (shift & 31)) as u32);
    (out, sum)
}

/// 800A4928 VectorNormalSS(SVECTOR* in, SVECTOR* out): the three signed
/// halfwords through 800A4958, stored back as halfwords.
pub fn vector_normal_ss(m: &mut Machine, vector: u32, out: u32) {
    vector_normal_ss_v0(m, vector, out);
}

/// [`vector_normal_ss`] returning the original's leftover `v0` (the sum of
/// squares from 800A4958).
pub fn vector_normal_ss_v0(m: &mut Machine, vector: u32, out: u32) -> u32 {
    let v: [u32; 3] = std::array::from_fn(|i| m.lh(at(vector, i as u32 * 2)) as i32 as u32);
    let (n, sum) = normal_core(m, v);
    for (i, value) in n.into_iter().enumerate() {
        m.sh(at(out, i as u32 * 2), value as u16);
    }
    sum
}

/// 800A6088 ratan2(long y, long x) -> angle (4096 per turn), using the
/// 1025-entry table at 800B8F84 (via 800A613C/800A61D8). (0, 0) returns 0.
/// The divisions keep the compiler's zero/overflow `break` checks, which
/// only `0x80000000` inputs can reach.
pub fn ratan2(m: &mut Machine, y: u32, x: u32) -> u32 {
    let mut a0 = y as i32;
    let mut a1 = x as i32;
    let negative_x = a1 < 0;
    if negative_x {
        a1 = a1.wrapping_neg();
    }
    let negative_y = a0 < 0;
    if negative_y {
        a0 = a0.wrapping_neg();
    }
    if a1 == 0 && a0 == 0 {
        return 0;
    }
    let lookup = |m: &mut Machine, q: i32| -> i32 {
        i32::from(m.lh(ATAN_TABLE.wrapping_add((q as u32) << 1)))
    };
    let mut angle = if a0 < a1 {
        let q = if a0 & 0x7fe0_0000 == 0 {
            checked_div(m, ((a0 as u32) << 10) as i32, a1, 0x800a_6118, 0x800a_6130)
        } else {
            checked_div(m, a0, a1 >> 10, 0x800a_60e0, 0x800a_60f8)
        };
        lookup(m, q)
    } else {
        let q = if a1 & 0x7fe0_0000 == 0 {
            checked_div(m, ((a1 as u32) << 10) as i32, a0, 0x800a_61a0, 0x800a_61b8)
        } else {
            checked_div(m, a1, a0 >> 10, 0x800a_6168, 0x800a_6180)
        };
        0x400i32.wrapping_sub(lookup(m, q))
    };
    if negative_x {
        angle = 0x800i32.wrapping_sub(angle);
    }
    if negative_y {
        angle = angle.wrapping_neg();
    }
    angle as u32
}

/// Sine/cosine of a 4.12 angle as RotMatrixYXZ reads them: the table word at
/// `|angle| & 0xFFF`, sine negated for negative angles (`-0x8000` reads
/// entry 0).
fn sin_cos(m: &mut Machine, angle: i16) -> (i32, i32) {
    let angle = i32::from(angle);
    let index = if angle >= 0 {
        angle & 0xfff
    } else {
        angle.wrapping_neg() & 0xfff
    };
    let word = m.lw(SIN_COS_TABLE.wrapping_add((index as u32) << 2));
    let sin = i32::from(word as u16 as i16);
    let cos = (word as i32) >> 16;
    (if angle >= 0 { sin } else { sin.wrapping_neg() }, cos)
}

/// `multu` low word then `sra 12`.
fn fixed(a: i32, b: i32) -> i32 {
    a.wrapping_mul(b) >> 12
}

/// 800A5BE8 RotMatrixYXZ(SVECTOR* angles, MATRIX* m) -> m (continuing
/// through 800A5C50, 800A5CB4, 800A5D48). All three angles are read before
/// the first store; the nine halfwords are stored in the original order.
pub fn rot_matrix_yxz(m: &mut Machine, angles: u32, matrix: u32) -> u32 {
    let (sx, cx) = {
        let a = m.lh(angles);
        sin_cos(m, a)
    };
    let (sy, cy) = {
        let a = m.lh(at(angles, 2));
        sin_cos(m, a)
    };
    let (sz, cz) = {
        let a = m.lh(at(angles, 4));
        sin_cos(m, a)
    };
    let store = |m: &mut Machine, offset: u32, value: i32| m.sh(at(matrix, offset), value as u16);
    store(m, 0xa, sx.wrapping_neg());
    store(m, 4, fixed(sy, cx));
    store(m, 0x10, fixed(cy, cx));
    store(m, 6, fixed(sz, cx));
    store(m, 8, fixed(cz, cx));
    let sysx = fixed(sy, sx);
    store(m, 0, fixed(cy, cz).wrapping_add(fixed(sysx, sz)));
    store(m, 2, fixed(sysx, cz).wrapping_sub(fixed(cy, sz)));
    let cysx = fixed(cy, sx);
    store(m, 0xe, fixed(sy, sz).wrapping_add(fixed(cysx, cz)));
    store(m, 0xc, fixed(cysx, sz).wrapping_sub(fixed(sy, cz)));
    matrix
}

/// 8009CE80 __divdi3: signed 64-bit quotient, truncated toward zero
/// (`a0:a1` / `a2:a3`, result in `v0:v1`; through 8009CF38 __udivmoddi4 and
/// its 8009CF98/8009D2EC blocks with a null remainder pointer). The
/// unsigned core is libgcc's exact long division, so the quotient of the
/// magnitudes equals Rust's `u64` division; `i64::MIN / -1` wraps to
/// `i64::MIN`. A zero divisor reaches libgcc's `1 / d0` at 8009D0B4, whose
/// `break 7` (8009D0C0) traps; that path records the trap and returns 0.
pub fn divdi3(m: &mut Machine, numerator: i64, denominator: i64) -> i64 {
    if denominator == 0 {
        trap(m, 0x8009_d0c0);
        return 0;
    }
    let quotient = numerator.unsigned_abs() / denominator.unsigned_abs();
    let quotient = if (numerator < 0) != (denominator < 0) {
        quotient.wrapping_neg()
    } else {
        quotient
    };
    quotient as i64
}

/// 8009CF18 __udivdi3: unsigned 64-bit quotient `a0:a1 / a2:a3` (low word
/// first), through 8009CF38 __udivmoddi4 with a null remainder pointer
/// (its fifth argument, stored at the frame's +0x10). The result is the
/// register pair `v0` (low) : `v1` (high): the only caller, 8002779C's
/// decimal path, keeps both halves, so it must call this function for the
/// full quotient; through [`dispatch`] (one result register) only the low
/// word arrives. A zero divisor reaches libgcc's `1 / d0` like
/// [`divdi3`]: the `break 7` at 8009D0C0 traps and the result is 0.
pub fn udivdi3(m: &mut Machine, numerator: u64, denominator: u64) -> u64 {
    if denominator == 0 {
        trap(m, 0x8009_d0c0);
        return 0;
    }
    numerator / denominator
}

// ---- libc ----------------------------------------------------------------------

/// 800AC108 memcpy(dst, src, n) -> dst: forward byte copy. A null `dst`
/// returns 0 without copying; `n <= 0` (signed) copies nothing.
pub fn memcpy(m: &mut Machine, dst: u32, src: u32, n: u32) -> u32 {
    if dst == 0 {
        return 0;
    }
    let mut remaining = n as i32;
    let (mut d, mut s) = (dst, src);
    while remaining > 0 {
        let byte = m.lbu(s);
        s = s.wrapping_add(1);
        remaining -= 1;
        m.sb(d, byte);
        d = d.wrapping_add(1);
    }
    dst
}

/// 800AC148 memset(dst, c, n) (with 800AC170): stores the low byte of `c`
/// `n` times and returns `dst`. A null `dst` or `n <= 0` (signed) stores
/// nothing and returns 0.
pub fn memset(m: &mut Machine, dst: u32, c: u32, n: u32) -> u32 {
    if dst == 0 || (n as i32) <= 0 {
        return 0;
    }
    let mut remaining = n as i32;
    let mut d = dst;
    while remaining > 0 {
        m.sb(d, c as u8);
        remaining -= 1;
        d = d.wrapping_add(1);
    }
    dst
}

/// 800C9D30: libc `rand` state word.
pub const RAND_STATE: u32 = 0x800c_9d30;

/// 800AC178 rand(): state = state * 0x41C64E6D + 0x3039; returns
/// `(state >> 16) & 0x7FFF`.
pub fn rand(m: &mut Machine) -> u32 {
    let state = m
        .lw(RAND_STATE)
        .wrapping_mul(0x41c6_4e6d)
        .wrapping_add(0x3039);
    m.sw(RAND_STATE, state);
    (state >> 16) & 0x7fff
}

/// 800AC1A8 srand(seed): stores the state word.
pub fn srand(m: &mut Machine, seed: u32) {
    m.sw(RAND_STATE, seed);
}

// ---- libgpu packets (RAM only) ---------------------------------------------------

/// 800AABD8 CatPrim(p0, p1): link `p0` to `p1`, keeping the length byte of
/// `p0`.
pub fn cat_prim(m: &mut Machine, p0: u32, p1: u32) {
    let tag = m.lw(p0);
    m.sw(p0, (tag & 0xff00_0000) | (p1 & 0x00ff_ffff));
}

/// 800AAC08 SetDrawTPage(DR_TPAGE* p, dfe, dtd, tpage): length 1 and
/// `E1000000 | dtd << 9 | dfe << 10 | (tpage & 0x9FF)` (flags as booleans).
pub fn set_draw_tpage(m: &mut Machine, p: u32, dfe: u32, dtd: u32, tpage: u32) {
    m.sb(at(p, 3), 1);
    let mut command = 0xe100_0000;
    if dtd != 0 {
        command |= 0x200;
    }
    let mut page = tpage & 0x9ff;
    if dfe != 0 {
        page |= 0x400;
    }
    m.sw(at(p, 4), command | page);
}

/// 800B97E8/800B97EA: libgpu drawing-area limits (width, height halfwords).
pub const DRAW_LIMIT_WIDTH: u32 = 0x800b_97e8;
pub const DRAW_LIMIT_HEIGHT: u32 = 0x800b_97ea;

/// 800A73F0 SetDrawArea(DR_AREA* p, RECT* r) -> the E4 command (`v0`):
/// length 2, then the clamped top-left (E3) and bottom-right (E4) commands.
/// `r` is a guest address (callers keep the RECT on the guest stack); it is
/// read in the original order (x/y after the length store, then x/w/y/h
/// after the first command store), so overlapping packets behave
/// identically.
pub fn set_draw_area(m: &mut Machine, p: u32, rect: u32) -> u32 {
    m.sb(at(p, 3), 2);
    let x = m.lh(rect);
    let y = m.lh(at(rect, 2));
    let start = draw_area_start(m, x, y);
    m.sw(at(p, 4), start);
    let x = m.lhu(rect);
    let w = m.lhu(at(rect, 4));
    let y = m.lhu(at(rect, 2));
    let right = x.wrapping_add(w).wrapping_sub(1) as i16;
    let h = m.lhu(at(rect, 6));
    let bottom = y.wrapping_add(h).wrapping_sub(1) as i16;
    let end = draw_area_end(m, right, bottom);
    m.sw(at(p, 8), end);
    end
}

/// 800A7938 (SetDrawArea helper): `E3000000 | y << 10 | x`, each clamped to
/// the drawing limits.
pub fn draw_area_start(m: &mut Machine, x: i16, y: i16) -> u32 {
    area_command(m, x, y, 0xe300_0000)
}

/// 800A79D0 (SetDrawArea helper): `E4000000 | y << 10 | x`, clamped like
/// [`draw_area_start`].
pub fn draw_area_end(m: &mut Machine, x: i16, y: i16) -> u32 {
    area_command(m, x, y, 0xe400_0000)
}

fn area_command(m: &mut Machine, x: i16, y: i16, command: u32) -> u32 {
    let x = clamp_to_limit(m, x, DRAW_LIMIT_WIDTH);
    let y = clamp_to_limit(m, y, DRAW_LIMIT_HEIGHT);
    ((y & 0x3ff) << 10) | (x & 0x3ff) | command
}

/// Negative → 0; above `lh(limit) - 1` (signed compare) → `lhu(limit) - 1`;
/// otherwise the value itself.
fn clamp_to_limit(m: &mut Machine, value: i16, limit: u32) -> u32 {
    if value < 0 {
        return 0;
    }
    let signed = i32::from(m.lh(limit));
    let unsigned = u32::from(m.lhu(limit));
    if signed - 1 < i32::from(value) {
        unsigned.wrapping_sub(1)
    } else {
        value as u32
    }
}

// ---- libgpu ordering tables ------------------------------------------------------

/// 800B97DC: pointer to libgpu's low-level function table.
const GPU_TABLE: u32 = 0x800b_97dc;
/// 800B97E0: libgpu's debug print function pointer.
const GPU_PRINT: u32 = 0x800b_97e0;
/// 800B97E6: libgpu debug level byte (>= 2 prints each call).
const GPU_DEBUG: u32 = 0x800b_97e6;
/// 800B98A4: libgpu terminator packet ClearOTagR links `ot[0]` to; it links
/// on to 800B9890.
const GPU_OT_TERMINATOR: u32 = 0x800b_98a4;
const GPU_OT_TERMINATOR_NEXT: u32 = 0x800b_9890;
/// 800B9920/800B9924: libgpu DMA timeout deadline (VSync count) and poll
/// counter, set by 800A8B98 and advanced by 800A8BCC.
pub const GPU_TIMEOUT_DEADLINE: u32 = 0x800b_9920;
pub const GPU_TIMEOUT_POLLS: u32 = 0x800b_9924;
/// 800B3FF4: VBlank counter word (incremented by the SDK VBlank handler).
pub const VSYNC_COUNTER: u32 = 0x800b_3ff4;
/// 800A7B1C: the standard low-level OTC routine in libgpu's table (+0x2C).
const GPU_CLEAR_OTAG_R: u32 = 0x800a_7b1c;

/// This executable's addresses of the device routines libgpu reaches
/// through pointers it loads from RAM (its low-level table entry +0x18,
/// the debug print pointer `[800B97E0]`): the routines of
/// [`host_ids::RUNTIME`](crate::host_ids::RUNTIME), in its order. In PAL
/// they equal their canonical ids; a version generated from this crate has
/// its own addresses here.
const RUNTIME_ADDRESSES: [u32; 2] = [0x800a_8360, 0x800a_c3e8];

/// The canonical host-call id of the routine at `address`, a routine
/// pointer loaded from RAM: its [`host_ids::RUNTIME`](crate::host_ids::RUNTIME)
/// id when it is one of [`RUNTIME_ADDRESSES`], else the address itself.
fn runtime_host_id(address: u32) -> u32 {
    match RUNTIME_ADDRESSES.iter().position(|&at| at == address) {
        Some(index) => crate::host_ids::RUNTIME[index],
        None => address,
    }
}

/// Debug print through `[800B97E0]` when the libgpu debug level is >= 2: a
/// host call with the format address and the arguments.
fn gpu_debug_print(m: &mut Machine, format: u32, a1: u32, a2: u32) {
    if m.lbu(GPU_DEBUG) >= 2 {
        let print = m.lw(GPU_PRINT);
        m.host_call(runtime_host_id(print), [format, a1, a2, 0]);
    }
}

/// 800A8B98: arm the DMA timeout: deadline = VSync(-1) + 240, polls = 0.
pub fn gpu_timeout_start(m: &mut Machine) {
    let now = vsync(m, -1i32 as u32);
    m.sw(GPU_TIMEOUT_DEADLINE, now.wrapping_add(0xf0));
    m.sw(GPU_TIMEOUT_POLLS, 0);
}

/// 800A8BCC: one timeout poll. Returns `true` when the original takes its
/// timeout path (VSync(-1) past the deadline, or more than 0xF0000 polls),
/// which prints, resets the GPU and returns -1; that path is device-only
/// and is reported as untranslated (800A8C20).
fn gpu_timeout_poll(m: &mut Machine) -> bool {
    let now = vsync(m, -1i32 as u32);
    let expired = (m.lw(GPU_TIMEOUT_DEADLINE) as i32) < (now as i32) || {
        let polls = m.lw(GPU_TIMEOUT_POLLS);
        m.sw(GPU_TIMEOUT_POLLS, polls.wrapping_add(1));
        (polls as i32) > 0xf_0000
    };
    if expired {
        trap(m, 0x800a_8c20);
    }
    expired
}

/// 800A7B1C (libgpu table +0x2C): reverse-clear `n` OT words with DMA
/// channel 6 and wait for completion. The DMA register writes are one host
/// call (`[ot, n]`); its result is the number of 800A8BCC polls the
/// original made before the channel went idle (default 0: the native
/// transfer is complete at once). RAM: `ot[n-1] .. ot[1]` = address of the
/// previous word (`& 0xFFFFFF`), `ot[0]` = 0xFFFFFF, then the timeout words
/// 800B9920/800B9924. Returns `n`.
fn clear_otag_r_dma(m: &mut Machine, ot: u32, n: u32) -> u32 {
    let polls = m.host_call_result(GPU_CLEAR_OTAG_R, [ot, n, 0, 0], 0);
    // DMA6 OTC writes from MADR = ot + 4n - 4 downward, each word linking
    // to the one below; the last word is the end marker.
    let mut address = ot.wrapping_add(n << 2).wrapping_sub(4);
    for _ in 1..n {
        m.sw(address, address.wrapping_sub(4) & 0x00ff_ffff);
        address = address.wrapping_sub(4);
    }
    m.sw(address, 0x00ff_ffff);
    gpu_timeout_start(m);
    for _ in 0..polls {
        if gpu_timeout_poll(m) {
            return -1i32 as u32;
        }
    }
    n
}

/// 800A6B50 ClearOTagR(ot, n) -> ot: the low-level clear through libgpu's
/// table (`[[800B97DC] + 0x2C]`, normally 800A7B1C), then 800B98A4 =
/// 0x040B9890 (a 4-word terminator linking to 800B9890) and `ot[0]` =
/// 0x0B98A4. RAM written: `ot[0..n]`, 800B98A4, 800B9920/800B9924 (see
/// `clear_otag_r_dma`). `n` must be at least 1 (the native DMA model does not
/// reproduce the hardware's 0 = 65536 word count).
pub fn clear_otag_r(m: &mut Machine, ot: u32, n: u32) -> u32 {
    gpu_debug_print(m, 0x8002_4cbc, ot, n);
    let table = m.lw(GPU_TABLE);
    let low_level = m.lw(at(table, 0x2c));
    if low_level == GPU_CLEAR_OTAG_R {
        clear_otag_r_dma(m, ot, n);
    } else {
        crate::dispatch::call(m, low_level, [ot, n, 0, 0]);
    }
    m.sw(
        GPU_OT_TERMINATOR,
        (GPU_OT_TERMINATOR_NEXT & 0x00ff_ffff) | 0x0400_0000,
    );
    m.sw(ot, GPU_OT_TERMINATOR & 0x00ff_ffff);
    ot
}

// ---- device-facing routines (host calls) ------------------------------------------

/// 800A0568 VSync(mode).
///
/// Device reads: GPUSTAT (`[800B2EBC]`) and root counter 1 (`[800B2EC0]`,
/// the hblank count, read until stable). `mode < 0` returns the VBlank
/// counter word 800B3FF4 (RAM, advanced by the SDK VBlank handler) and makes
/// no host call. `mode == 1` returns the hblanks since the previous VSync;
/// other modes wait for VBlank `[800B2EC8] + mode - 1` (mode 0: the next
/// one), then at least one more, then store 800B2EC8 = the VBlank counter
/// and 800B2EC4 = root counter 1. Host results for `mode >= 0`: the root
/// counter value at entry (default: `[800B2EC4]`, i.e. 0 elapsed), and for
/// the waiting modes the VBlank counter after the wait (default: the target
/// VBlank, at least `[800B3FF4] + 1`) and the root counter after it
/// (default 0). The VBlank counter is read at entry, before the host call:
/// the host device may advance the VBlank counter during the call (it
/// delivers the VBlank interrupts the wait spans), so the default counter
/// after the wait matches RAM afterwards, as in the original. Returns
/// `(entry counter - [800B2EC4]) & 0xFFFF` for `mode >= 0`.
pub fn vsync(m: &mut Machine, mode: u32) -> u32 {
    let mode = mode as i32;
    if mode < 0 {
        return m.lw(VSYNC_COUNTER);
    }
    let last_hcount = m.lw(VSYNC_LAST_HCOUNT);
    let counter = m.lw(VSYNC_COUNTER);
    let hcount = m.host_call_result(0x800a_0568, [mode as u32, 0, 0, 0], last_hcount);
    let elapsed = hcount.wrapping_sub(last_hcount) & 0xffff;
    if mode == 1 {
        return elapsed;
    }
    let target = if mode > 0 {
        m.lw(VSYNC_LAST_COUNT)
            .wrapping_sub(1)
            .wrapping_add(mode as u32)
    } else {
        m.lw(VSYNC_LAST_COUNT)
    };
    let reached = if (counter as i32) < (target as i32) {
        target
    } else {
        counter
    };
    let after = m
        .host_results
        .pop_front()
        .unwrap_or(reached.wrapping_add(1));
    let hcount_after = m.host_results.pop_front().unwrap_or(0);
    m.sw(VSYNC_LAST_COUNT, after);
    m.sw(VSYNC_LAST_HCOUNT, hcount_after);
    elapsed
}
/// 800B2EC4: root counter 1 value at the previous VSync.
pub const VSYNC_LAST_HCOUNT: u32 = 0x800b_2ec4;
/// 800B2EC8: VBlank counter at the previous VSync.
pub const VSYNC_LAST_COUNT: u32 = 0x800b_2ec8;

/// 800A08F4 SetIntrMask(mask) -> previous mask. Reads and writes I_MASK
/// through the pointer at 800B3FBC (device only; no RAM is written). Host
/// result: the previous mask (default 0).
pub fn set_intr_mask(m: &mut Machine, mask: u32) -> u32 {
    m.host_call_result(0x800a_08f4, [mask, 0, 0, 0], 0)
}

/// 800A08CC CheckCallback(): the halfword at 800B2F2E (nonzero while the
/// SDK runs an interrupt callback). RAM only.
pub fn check_callback(m: &mut Machine) -> u32 {
    u32::from(m.lhu(0x800b_2f2e))
}

/// 800B256C: libpad's VBlank flag word (set by its VBlank handler).
pub const PAD_VSYNC_FLAG: u32 = 0x800b_256c;

/// 8009B1E0 PadChkVsync() (through 8009A11C): returns the libpad VBlank
/// flag word 800B256C and clears it. RAM only.
pub fn pad_chk_vsync(m: &mut Machine) -> u32 {
    let flag = m.lw(PAD_VSYNC_FLAG);
    m.sw(PAD_VSYNC_FLAG, 0);
    flag
}

/// 800AD934 GetRCnt(spec) -> root counter `spec & 0xFFFF` (0..2; others
/// return 0 without a device access). The counter register (through the
/// pointer at 800BA998) is a device read: host result, 16 bits (default 0).
/// No RAM is written.
pub fn get_rcnt(m: &mut Machine, spec: u32) -> u32 {
    if spec & 0xffff < 3 {
        m.host_call_result(0x800a_d934, [spec, 0, 0, 0], 0) & 0xffff
    } else {
        0
    }
}

/// 800AD9D0 ResetRCnt(spec) -> 1 after clearing root counter
/// `spec & 0xFFFF` (0..2; a device write), 0 for other counters. No RAM is
/// written.
pub fn reset_rcnt(m: &mut Machine, spec: u32) -> u32 {
    if spec & 0xffff < 3 {
        m.host_call(0x800a_d9d0, [spec, 0, 0, 0]);
        1
    } else {
        0
    }
}

/// 800B3FB4: pointer to the SDK interrupt function table (0x800B3F94):
/// +4 DMA callback setter (800A1124 once ResetCallback ran), +8
/// InterruptCallback 800A0BB4, +0xC ResetCallback 800A090C, +0x10
/// StopCallback 800A0CFC, +0x14 VSync callback setter (800A0EFC once
/// ResetCallback ran), +0x18 RestartCallback 800A0D9C.
const INTERRUPT_TABLE: u32 = 0x800b_3fb4;
/// 800B4008: DMA callback function per channel (words).
pub const DMA_CALLBACKS: u32 = 0x800b_4008;
/// 800B2F2C: libetc's interrupt manager: halfword +0 initialized, +2 in a
/// callback (CheckCallback), +4 the eleven interrupt handlers, +0x30 their
/// enable mask (halfword), +0x32/+0x34 I_MASK/DPCR saved by StopCallback,
/// +0x38 the setjmp buffer the kernel resumes on interrupts, then the
/// handlers' stack up to 0x800B3F44. 0x41A words in all.
pub const INTERRUPT_MANAGER: u32 = 0x800b_2f2c;
const INTERRUPT_HANDLERS: u32 = 0x800b_2f30;
const INTERRUPT_ENABLED: u32 = 0x800b_2f5c;
const INTERRUPT_JMPBUF: u32 = 0x800b_2f64;
/// 800B3FD4: the eight VSync callbacks (VSyncCallbacks), run by the VBlank
/// handler 800A0E90.
pub const VSYNC_CALLBACKS: u32 = 0x800b_3fd4;
/// libetc's VBlank counter (cleared by ResetCallback).
const VBLANK_COUNTER: u32 = 0x800b_3ff4;
/// The handlers ResetCallback installs and the setters it returns.
const VBLANK_HANDLER: u32 = 0x800a_0e90;
const DMA_HANDLER: u32 = 0x800a_0fa4;
const VSYNC_SETTER: u32 = 0x800a_0efc;
const DMA_SETTER: u32 = 0x800a_1124;

/// libetc interrupt table entry `offset` (`[[0x800B3FB4] + offset]`).
fn interrupt_entry(m: &mut Machine, offset: u32) -> u32 {
    let table = m.lw(INTERRUPT_TABLE);
    m.lw(at(table, offset))
}

fn clear_words(m: &mut Machine, address: u32, words: u32) {
    for i in 0..words {
        m.sw(at(address, i * 4), 0);
    }
}

/// 800A0778 ResetCallback() through `[table + 0xC]` (800A090C). Once
/// (manager halfword 0x800B2F2C clear): clears the manager (0x41A words),
/// fills its setjmp buffer (800AD678: ra 800A0974, s0 0x800B2F2C, gp; the
/// sp slot is then pointed at the handler stack 0x800B3F44; the caller's
/// fp and s1..s7 are not reproduced), marks it initialized, starts the
/// VBlank interrupt (800A0E38: counter 0x800B3FF4 = 0, VSync callbacks
/// cleared, handler 800A0E90 on interrupt 0, setter 800A0EFC at `+0x14`)
/// and the DMA interrupt (800A0F58: DMA callbacks cleared, handler 800A0FA4
/// on interrupt 3, setter 800A1124 at `+4`). Returns 0x800B2F2C, or 0 when
/// already initialized. The device part (I_MASK/I_STAT/DPCR/DICR, the
/// kernel's HookEntryInt, ChangeClearPAD, ChangeClearRCnt and the critical
/// section) is the host call `800A0778`.
pub fn reset_callback(m: &mut Machine) -> u32 {
    let handler = interrupt_entry(m, 0xc);
    if handler != 0x800a_090c {
        return crate::dispatch::call(m, handler, [0; 4]);
    }
    m.host_call(0x800a_0778, [0; 4]);
    m.push_frame(0x18);
    if m.lhu(INTERRUPT_MANAGER) != 0 {
        m.pop_frame(0x18);
        return 0;
    }
    clear_words(m, INTERRUPT_MANAGER, 0x41a);
    // setjmp(0x800B2F64): ra, sp, fp, s0..s7, gp.
    m.sw(INTERRUPT_JMPBUF, 0x800a_0974);
    m.sw(at(INTERRUPT_JMPBUF, 0xc), INTERRUPT_MANAGER);
    m.sw(at(INTERRUPT_JMPBUF, 0x2c), crate::GP);
    m.sw(at(INTERRUPT_JMPBUF, 4), 0x800b_3f44);
    m.sh(INTERRUPT_MANAGER, 1);
    // 800A0E38 startIntrVSync.
    m.push_frame(0x18);
    m.sw(VBLANK_COUNTER, 0);
    clear_words(m, VSYNC_CALLBACKS, 8);
    interrupt_callback_impl(m, 0, VBLANK_HANDLER);
    m.pop_frame(0x18);
    let table = m.lw(INTERRUPT_TABLE);
    m.sw(at(table, 0x14), VSYNC_SETTER);
    // 800A0F58 startIntrDMA.
    m.push_frame(0x18);
    clear_words(m, DMA_CALLBACKS, 8);
    interrupt_callback_impl(m, 3, DMA_HANDLER);
    m.pop_frame(0x18);
    let table = m.lw(INTERRUPT_TABLE);
    m.sw(at(table, 4), DMA_SETTER);
    m.pop_frame(0x18);
    INTERRUPT_MANAGER
}

/// 800A07A8 InterruptCallback(irq, func) -> previous, through `[table + 8]`
/// (800A0BB4): with the manager initialized and `func` new, handler `irq`
/// (0x800B2F30) = `func` and its enable bit (halfword 0x800B2F5C) set, or
/// both cleared for `func` 0. The I_MASK update and the root-counter /
/// pad interrupt switches are the device part (host call `800A07A8`).
pub fn interrupt_callback(m: &mut Machine, irq: u32, func: u32) -> u32 {
    let handler = interrupt_entry(m, 8);
    if handler != 0x800a_0bb4 {
        return crate::dispatch::call(m, handler, [irq, func, 0, 0]);
    }
    m.host_call(0x800a_07a8, [irq, func, 0, 0]);
    interrupt_callback_impl(m, irq, func)
}

/// 800A0BB4's RAM effects (InterruptCallback's body).
fn interrupt_callback_impl(m: &mut Machine, irq: u32, func: u32) -> u32 {
    m.push_frame(0x28);
    let slot = at(INTERRUPT_HANDLERS, irq << 2);
    let previous = m.lw(slot);
    if func != previous && m.lhu(INTERRUPT_MANAGER) != 0 {
        let bit = 1u16 << (irq & 0x1f);
        let enabled = m.lhu(INTERRUPT_ENABLED);
        if func != 0 {
            m.sw(slot, func);
            m.sh(INTERRUPT_ENABLED, enabled | bit);
        } else {
            m.sw(slot, 0);
            m.sh(INTERRUPT_ENABLED, enabled & !bit);
        }
    }
    m.pop_frame(0x28);
    previous
}

/// 800A0808 VSyncCallback(func) -> previous: `[table + 0x14](4, func)`.
pub fn vsync_callback(m: &mut Machine, func: u32) -> u32 {
    vsync_callbacks(m, 4, func)
}

/// 800A083C VSyncCallbacks(index, func) -> previous, through
/// `[table + 0x14]` (800A0EFC): VSync callback `index` (0x800B3FD4) =
/// `func` when it changes. RAM only.
pub fn vsync_callbacks(m: &mut Machine, index: u32, func: u32) -> u32 {
    let setter = interrupt_entry(m, 0x14);
    if setter != VSYNC_SETTER {
        return crate::dispatch::call(m, setter, [index, func, 0, 0]);
    }
    let slot = at(VSYNC_CALLBACKS, index << 2);
    let previous = m.lw(slot);
    if func != previous {
        m.sw(slot, func);
    }
    previous
}

/// 800A086C StopCallback() through `[table + 0x10]` (800A0CFC): with the
/// manager initialized, saves I_MASK (halfword +0x32) and DPCR (+0x34), as
/// the host answers them (host results, default 0), and clears the
/// initialized flag; returns the manager, else 0. Disabling the
/// interrupts and DMA and the kernel's ResetEntryInt are the device part
/// (host call `800A086C`).
pub fn stop_callback(m: &mut Machine) -> u32 {
    let handler = interrupt_entry(m, 0x10);
    if handler != 0x800a_0cfc {
        return crate::dispatch::call(m, handler, [0; 4]);
    }
    if m.lhu(INTERRUPT_MANAGER) == 0 {
        m.host_call(0x800a_086c, [0; 4]);
        return 0;
    }
    let mask = m.host_call_result(0x800a_086c, [0; 4], 0);
    let dpcr = m.host_results.pop_front().unwrap_or(0);
    m.sh(at(INTERRUPT_MANAGER, 0x32), mask as u16);
    m.sw(at(INTERRUPT_MANAGER, 0x34), dpcr);
    m.sh(INTERRUPT_MANAGER, 0);
    INTERRUPT_MANAGER
}

/// 800A089C RestartCallback() through `[table + 0x18]` (800A0D9C): with
/// the manager stopped, marks it initialized again and returns it, else 0.
/// The kernel's HookEntryInt and restoring I_MASK/DPCR from +0x32/+0x34
/// are the device part (host call `800A089C` with the saved values).
pub fn restart_callback(m: &mut Machine) -> u32 {
    let handler = interrupt_entry(m, 0x18);
    if handler != 0x800a_0d9c {
        return crate::dispatch::call(m, handler, [0; 4]);
    }
    if m.lhu(INTERRUPT_MANAGER) != 0 {
        return 0;
    }
    let mask = u32::from(m.lhu(at(INTERRUPT_MANAGER, 0x32)));
    let dpcr = m.lw(at(INTERRUPT_MANAGER, 0x34));
    m.sh(INTERRUPT_MANAGER, 1);
    m.host_call(0x800a_089c, [mask, dpcr, 0, 0]);
    INTERRUPT_MANAGER
}

/// 800A07D8 DMACallback(channel, func) -> previous callback, through
/// `[[800B3FB4] + 4]` (800A1124): RAM `800B4008[channel]` = `func` (0
/// clears it) when it changes; the DICR enable bit is a device write (host
/// call `[channel, func]`, made only when the entry changes).
pub fn dma_callback(m: &mut Machine, channel: u32, func: u32) -> u32 {
    let table = m.lw(INTERRUPT_TABLE);
    let setter = m.lw(at(table, 4));
    if setter != 0x800a_1124 {
        return crate::dispatch::call(m, setter, [channel, func, 0, 0]);
    }
    let entry = DMA_CALLBACKS.wrapping_add(channel << 2);
    let previous = m.lw(entry);
    if func == previous {
        return previous;
    }
    m.sw(entry, func);
    m.host_call(0x800a_1124, [channel, func, 0, 0]);
    previous
}

/// 800AAF74 DecDCToutCallback(func) -> previous: `DMACallback(1, func)`.
pub fn dec_dct_out_callback(m: &mut Machine, func: u32) -> u32 {
    dma_callback(m, 1, func)
}

// ---- libgpu drawing environment and submission ------------------------------------

/// 800B97E5: libgpu queued-mode byte (nonzero: submissions may queue).
const GPU_ASYNC: u32 = 0x800b_97e5;
/// 800B97E7: libgpu byte selecting display mode bit 0x80.
const GPU_MODE_BIT7: u32 = 0x800b_97e7;
/// 800B97EC: word set to 1 by every submission (800A83FC).
const GPU_SUBMITTED: u32 = 0x800b_97ec;
/// 800B97F0: word; nonzero forces queued submission.
const GPU_FORCE_QUEUE: u32 = 0x800b_97f0;
/// 800B97F4: copy of the last DRAWENV (0x5C bytes).
pub const GPU_DRAW_ENV: u32 = 0x800b_97f4;
/// 800B9850: copy of the last DISPENV (0x14 bytes).
pub const GPU_DISP_ENV: u32 = 0x800b_9850;
/// 800B990C/800B9910: libgpu command queue write/read indices (mod 64).
pub const GPU_QUEUE_WRITE: u32 = 0x800b_990c;
pub const GPU_QUEUE_READ: u32 = 0x800b_9910;
/// 800B9914: interrupt mask saved by 800A83FC around a submission.
pub const GPU_SAVED_MASK: u32 = 0x800b_9914;
/// 800C8128: low byte of the last GP1 command, per command number.
pub const GPU_GP1_SHADOW: u32 = 0x800c_8128;
/// 800B98E0: dot-clock multipliers per horizontal resolution (5 bytes).
const GPU_DOT_CLOCK: u32 = 0x800b_98e0;
/// 800B98B8: horizontal display range (start, end) halfword pairs, five per
/// video mode.
const GPU_H_RANGE: u32 = 0x800b_98b8;
/// 800B4034: video mode word (0 NTSC, 1 PAL).
pub const VIDEO_MODE: u32 = 0x800b_4034;

fn gpu_table_entry(m: &mut Machine, offset: u32) -> u32 {
    let table = m.lw(GPU_TABLE);
    m.lw(at(table, offset))
}

/// 800A120C GetVideoMode(): the word at 800B4034. RAM only.
pub fn get_video_mode(m: &mut Machine) -> u32 {
    m.lw(VIDEO_MODE)
}

/// 800A82E8 (libgpu table +0x10): GP1 `command` (a device write, host call)
/// and its low byte into the shadow table at 800C8128 + `command >> 24`.
fn gpu_control(m: &mut Machine, command: u32) {
    let control = gpu_table_entry(m, 0x10);
    if control != 0x800a_82e8 {
        crate::dispatch::call(m, control, [command, 0, 0, 0]);
        return;
    }
    m.host_call(0x800a_82e8, [command, 0, 0, 0]);
    m.sb(GPU_GP1_SHADOW.wrapping_add(command >> 24), command as u8);
}

/// 800A83FC (libgpu table +8): submit `func(arg, param)` (`size` bytes of
/// packet for the queue). RAM: the DMA timeout words (800A8B98), 800B9914 =
/// the interrupt mask from `SetIntrMask(0)`, 800B97EC = 1; the call itself
/// is a host call `[arg, param, size]` to `func` (normally 800A8360, the
/// DMA channel 2 linked-list start; the LoadImage/StoreImage transfers
/// 800A7E2C/800A8068 run natively, see [`load_image`]), then
/// `SetIntrMask(saved)`. Host
/// results, in order: the two SetIntrMask results. A full queue or a queued
/// submission (queued mode with entries pending or 800B97F0 set) is not
/// translated (800A8430 / 800A8520).
pub fn gpu_submit(m: &mut Machine, func: u32, arg: u32, size: u32, param: u32) -> u32 {
    gpu_timeout_start(m);
    let write = m.lw(GPU_QUEUE_WRITE);
    if write.wrapping_add(1) & 0x3f == m.lw(GPU_QUEUE_READ) {
        trap(m, 0x800a_8430);
        return -1i32 as u32;
    }
    let mask = set_intr_mask(m, 0);
    m.sw(GPU_SAVED_MASK, mask);
    m.sw(GPU_SUBMITTED, 1);
    if m.lbu(GPU_ASYNC) != 0
        && (m.lw(GPU_QUEUE_WRITE) != m.lw(GPU_QUEUE_READ) || m.lw(GPU_FORCE_QUEUE) != 0)
    {
        trap(m, 0x800a_8520);
        return 0;
    }
    match func {
        GPU_LOAD_TRANSFER => {
            load_image_transfer(m, arg, param);
        }
        GPU_STORE_TRANSFER => {
            store_image_transfer(m, arg, param);
        }
        GPU_CLEAR_TRANSFER => {
            clear_transfer(m, arg, param);
        }
        _ => m.host_call(runtime_host_id(func), [arg, param, size, 0]),
    }
    let saved = m.lw(GPU_SAVED_MASK);
    set_intr_mask(m, saved);
    0
}

/// Submission through libgpu's table entry +8 with the entry +0x18 routine.
fn gpu_submit_packet(m: &mut Machine, arg: u32, size: u32) -> u32 {
    let submit = gpu_table_entry(m, 0x8);
    let func = gpu_table_entry(m, 0x18);
    if submit == 0x800a_83fc {
        gpu_submit(m, func, arg, size, 0)
    } else {
        crate::dispatch::call(m, submit, [func, arg, size, 0])
    }
}

/// 800A6C58 DrawOTag(ot): submit an ordering table (see `gpu_submit`; host
/// results: the two SetIntrMask results). RAM: 800B9920/800B9924,
/// 800B9914, 800B97EC.
pub fn draw_otag(m: &mut Machine, ot: u32) {
    gpu_debug_print(m, 0x8002_4cd4, ot, 0);
    gpu_submit_packet(m, ot, 0);
}

/// 800A8A5C (libgpu table +0x3C), DrawSync's body. Mode 0 arms the timeout
/// (800B9920/800B9924) and waits for the GPU: host result = the number of
/// 800A8BCC polls the original made (default 0), returns 0. Other modes
/// return the queued count, or 1 when the GPU is busy with an empty queue:
/// host result = 1 when DMA channel 2 is busy or the GPU is not ready
/// (default 0). Draining a non-empty queue (800A86AC) is not translated.
pub fn gpu_sync(m: &mut Machine, mode: u32) -> u32 {
    if mode == 0 {
        gpu_timeout_start(m);
        if m.lw(GPU_QUEUE_WRITE) != m.lw(GPU_QUEUE_READ) {
            trap(m, 0x800a_8a7c);
            return -1i32 as u32;
        }
        let polls = m.host_call_result(0x800a_8a5c, [0, 0, 0, 0], 0);
        for _ in 0..polls {
            if gpu_timeout_poll(m) {
                return -1i32 as u32;
            }
        }
        return 0;
    }
    let pending = m.lw(GPU_QUEUE_WRITE).wrapping_sub(m.lw(GPU_QUEUE_READ)) & 0x3f;
    if pending != 0 {
        trap(m, 0x800a_8b34);
    }
    let busy = m.host_call_result(0x800a_8a5c, [mode, 0, 0, 0], 0);
    if busy == 0 || pending != 0 {
        pending
    } else {
        1
    }
}

/// 800A6664 DrawSync(mode) through libgpu's table entry +0x3C (see
/// `gpu_sync` for RAM and host results).
pub fn draw_sync(m: &mut Machine, mode: u32) -> u32 {
    gpu_debug_print(m, 0x8002_4c30, mode, 0);
    let sync = gpu_table_entry(m, 0x3c);
    if sync == 0x800a_8a5c {
        gpu_sync(m, mode)
    } else {
        crate::dispatch::call(m, sync, [mode, 0, 0, 0])
    }
}

/// 800A7918: drawing-mode command `E1000000 | dtd << 9 | dfe << 10 |
/// (tpage & 0x9FF)` (flags as booleans).
pub fn draw_mode_command(dfe: u32, dtd: u32, tpage: u32) -> u32 {
    let mut command = 0xe100_0000;
    if dtd != 0 {
        command |= 0x200;
    }
    let mut page = tpage & 0x9ff;
    if dfe != 0 {
        page |= 0x400;
    }
    command | page
}

/// 800AAC38 SetDrawMode(DR_MODE* p, dfe, dtd, tpage, RECT* tw): p +3 = 2,
/// +4 = [`draw_mode_command`], +8 = the texture-window command `E2000000
/// | (y >> 3) << 15 | (x >> 3) << 10 | (-h << 2 & 0x3E0) | (-w & 0xFF) >> 3`
/// (x, y the RECT's low bytes) or 0 without a window. `tw` is the fifth
/// argument (`[sp+0x10]`). Returns the original's `v0`: the E1 word without
/// a window, else the last window term.
pub fn set_draw_mode(m: &mut Machine, p: u32, dfe: u32, dtd: u32, tpage: u32, tw: u32) -> u32 {
    m.sb(at(p, 3), 2);
    let mode = draw_mode_command(dfe, dtd, tpage);
    m.sw(at(p, 4), mode);
    if tw == 0 {
        m.sw(at(p, 8), 0);
        return mode;
    }
    let y = u32::from(m.lbu(at(tw, 2))) >> 3;
    let x = u32::from(m.lbu(tw)) >> 3;
    let height = (m.lh(at(tw, 6)) as i32 as u32).wrapping_neg() << 2 & 0x3e0;
    let width = ((m.lh(at(tw, 4)) as i32 as u32).wrapping_neg() & 0xff) >> 3;
    m.sw(at(p, 8), 0xe200_0000 | y << 15 | x << 10 | height | width);
    width
}

/// 800A7A68: drawing-offset command `E5000000 | (y & 0x7FF) << 11 |
/// (x & 0x7FF)`.
pub fn draw_offset_command(x: u32, y: u32) -> u32 {
    ((y & 0x7ff) << 11) | (x & 0x7ff) | 0xe500_0000
}

/// 800A7A84: texture-window command from a RECT (`0` for a null pointer):
/// `E2000000 | (y >> 3) << 15 | (x >> 3) << 10 | ((-h & 0xFF) >> 3) << 5 |
/// ((-w & 0xFF) >> 3)` with x/y the low bytes.
pub fn texture_window_command(m: &mut Machine, rect: u32) -> u32 {
    if rect == 0 {
        return 0;
    }
    let x = u32::from(m.lbu(rect)) >> 3;
    let w = (i32::from(m.lh(at(rect, 4))).wrapping_neg() as u32 & 0xff) >> 3;
    let y = u32::from(m.lbu(at(rect, 2))) >> 3;
    let h = (i32::from(m.lh(at(rect, 6))).wrapping_neg() as u32 & 0xff) >> 3;
    (y << 15) | (x << 10) | 0xe200_0000 | (h << 5) | w
}

/// Clamp of 800A76A8's clear rectangle size: negative → 0, above
/// `lh(limit) - 1` → `lhu(limit) - 1`, else unchanged.
fn clamp_size(m: &mut Machine, value: u16, limit: u32) -> u16 {
    let signed = value as i16;
    if signed < 0 {
        return 0;
    }
    if i32::from(m.lh(limit)) - 1 < i32::from(signed) {
        m.lhu(limit).wrapping_sub(1)
    } else {
        value
    }
}

/// 800A76A8 SetDrawEnv(DR_ENV* dr, DRAWENV* env) -> length (`v0`): the
/// drawing-area start/end, offset, mode, texture window and E6 mask
/// commands at `dr + 4..0x18`; with `env->isbg` a clear of the clipped area
/// at +0x1C: FILL (02) when x and width are 64-aligned, else a TILE (60) at
/// the offset-relative position. Length byte +3 = 6, or 9 with the clear.
pub fn set_draw_env(m: &mut Machine, dr: u32, env: u32) -> u32 {
    let x = m.lh(env);
    let y = m.lh(at(env, 2));
    let start = draw_area_start(m, x, y);
    m.sw(at(dr, 4), start);
    let w = m.lhu(at(env, 4));
    let x = m.lhu(env);
    let y = m.lhu(at(env, 2));
    let right = w.wrapping_add(x).wrapping_sub(1) as i16;
    let h = m.lhu(at(env, 6));
    let bottom = y.wrapping_add(h).wrapping_sub(1) as i16;
    let end = draw_area_end(m, right, bottom);
    m.sw(at(dr, 8), end);
    let ox = m.lh(at(env, 8)) as i32 as u32;
    let oy = m.lh(at(env, 0xa)) as i32 as u32;
    m.sw(at(dr, 0xc), draw_offset_command(ox, oy));
    let dfe = u32::from(m.lbu(at(env, 0x17)));
    let dtd = u32::from(m.lbu(at(env, 0x16)));
    let tpage = u32::from(m.lhu(at(env, 0x14)));
    m.sw(at(dr, 0x10), draw_mode_command(dfe, dtd, tpage));
    let window = texture_window_command(m, at(env, 0xc));
    m.sw(at(dr, 0x14), window);
    m.sw(at(dr, 0x18), 0xe600_0000);
    let mut words = 7u32;
    if m.lbu(at(env, 0x18)) != 0 {
        let rx = m.lhu(env);
        let ry = m.lhu(at(env, 2));
        let rw = m.lhu(at(env, 4));
        let rh = m.lhu(at(env, 6));
        let rw = clamp_size(m, rw, DRAW_LIMIT_WIDTH);
        let rh = clamp_size(m, rh, DRAW_LIMIT_HEIGHT);
        let size = (u32::from(rh) << 16) | u32::from(rw);
        if rx & 0x3f != 0 || rw & 0x3f != 0 {
            let px = rx.wrapping_sub(m.lhu(at(env, 8)));
            let py = ry.wrapping_sub(m.lhu(at(env, 0xa)));
            let color = (u32::from(m.lbu(at(env, 0x1b))) << 16)
                | (u32::from(m.lbu(at(env, 0x1a))) << 8)
                | 0x6000_0000
                | u32::from(m.lbu(at(env, 0x19)));
            m.sw(at(dr, 0x1c), color);
            m.sw(at(dr, 0x20), (u32::from(py) << 16) | u32::from(px));
            m.sw(at(dr, 0x24), size);
        } else {
            let color = (u32::from(m.lbu(at(env, 0x1b))) << 16)
                | (u32::from(m.lbu(at(env, 0x1a))) << 8)
                | 0x0200_0000
                | u32::from(m.lbu(at(env, 0x19)));
            m.sw(at(dr, 0x1c), color);
            m.sw(at(dr, 0x20), (u32::from(ry) << 16) | u32::from(rx));
            m.sw(at(dr, 0x24), size);
        }
        words = 10;
    }
    m.sb(at(dr, 3), (words - 1) as u8);
    words - 1
}

/// 800A6CC8 PutDrawEnv(DRAWENV* env) -> env: builds `env->dr_env` (+0x1C)
/// with SetDrawEnv, terminates its tag (`| 0xFFFFFF`), submits it (0x40
/// bytes; see `gpu_submit`, host results: the two SetIntrMask results) and
/// copies the environment to 800B97F4 (0x5C bytes). RAM written: env
/// +0x1C..+0x44, 800B9920/800B9924, 800B9914, 800B97EC, 800B97F4..800B9850.
pub fn put_draw_env(m: &mut Machine, env: u32) -> u32 {
    gpu_debug_print(m, 0x8002_4ce8, env, 0);
    let packet = at(env, 0x1c);
    set_draw_env(m, packet, env);
    let tag = m.lw(packet);
    m.sw(packet, tag | 0x00ff_ffff);
    gpu_submit_packet(m, packet, 0x40);
    memcpy(m, GPU_DRAW_ENV, env, 0x5c);
    env
}

/// Clamp `value` to `low..=high` the way PutDispEnv does (`slti` chains).
fn clamp_range(value: i32, low: i32, high: i32) -> i32 {
    if value < low {
        low
    } else if value < high + 1 {
        value
    } else {
        high
    }
}

/// 800A6E94 PutDispEnv(DISPENV* env) -> env. GP1 commands (host calls via
/// `gpu_control`, each also recorded in the 800C8128 shadow table): display
/// start (05) always; display mode (08) when `env->disp`/flags differ from
/// the copy at 800B9850, which also stores `env->pad0` (+0x12) = the video
/// mode and then 8; horizontal (06) and vertical (07) ranges when
/// `env->screen` differs or `pad0` is 8, after `pad0` = the video mode.
/// Finally copies 0x14 bytes to 800B9850. RAM written: env +0x12, 800C8128
/// shadow bytes, 800B9850..800B9864.
pub fn put_disp_env(m: &mut Machine, env: u32) -> u32 {
    gpu_debug_print(m, 0x8002_4d1c, env, 0);
    let y = u32::from(m.lhu(at(env, 2)));
    let x = u32::from(m.lhu(env));
    gpu_control(m, ((y & 0x3ff) << 10) | (x & 0x3ff) | 0x0500_0000);
    let changed = m.lw(at(GPU_DISP_ENV, 0x10)) != m.lw(at(env, 0x10))
        || (0..4).any(|i| m.lh(at(GPU_DISP_ENV, i * 2)) != m.lh(at(env, i * 2)));
    if changed {
        let mut mode = 0x0800_0000;
        let video = get_video_mode(m);
        m.sb(at(env, 0x12), video as u8);
        if video & 0xff == 1 {
            mode |= 0x8;
        }
        if m.lbu(at(env, 0x11)) != 0 {
            mode |= 0x10;
        }
        if m.lbu(at(env, 0x10)) != 0 {
            mode |= 0x20;
        }
        if m.lbu(GPU_MODE_BIT7) != 0 {
            mode |= 0x80;
        }
        let w = i32::from(m.lh(at(env, 4)));
        if w >= 0x119 {
            mode |= if w < 0x161 {
                0x1
            } else if w < 0x191 {
                0x40
            } else if w < 0x231 {
                0x2
            } else {
                0x3
            };
        }
        let pal = m.lbu(at(env, 0x12));
        let h = i32::from(m.lh(at(env, 6)));
        let lines = if pal != 0 { 0x121 } else { 0x101 };
        if h >= lines {
            mode |= 0x24;
        }
        gpu_control(m, mode);
        m.sb(at(env, 0x12), 8);
    }
    let screen_same = (0..4).all(|i| m.lh(at(GPU_DISP_ENV, 8 + i * 2)) == m.lh(at(env, 8 + i * 2)));
    if !screen_same || m.lbu(at(env, 0x12)) == 8 {
        let video = get_video_mode(m);
        m.sb(at(env, 0x12), video as u8);
        let sy = i32::from(m.lh(at(env, 0xa)));
        let mut top = sy.wrapping_add(if video & 0xff != 0 { 0x13 } else { 0x10 });
        let height = i32::from(m.lh(at(env, 0xe)));
        let mut bottom = top.wrapping_add(if height != 0 { height } else { 0xf0 });
        let w = i32::from(m.lh(at(env, 4)));
        let index: u32 = if w < 0x119 {
            0
        } else if w < 0x161 {
            1
        } else if w < 0x191 {
            2
        } else if w < 0x231 {
            3
        } else {
            4
        };
        let sx = i32::from(m.lh(at(env, 8)));
        let clock = i32::from(m.lbu(GPU_DOT_CLOCK.wrapping_add(index)));
        let product = sx.wrapping_mul(clock);
        let pal = u32::from(m.lbu(at(env, 0x12)));
        let entry = (pal * 5 + index) * 4;
        let range_start = i32::from(m.lhu(GPU_H_RANGE.wrapping_add(entry)));
        let range_end = i32::from(m.lhu(GPU_H_RANGE.wrapping_add(entry + 2)));
        let sw = i32::from(m.lh(at(env, 0xc)));
        let mut span = range_end.wrapping_sub(range_start);
        let left = range_start.wrapping_add(product);
        if sw != 0 {
            span = span.wrapping_mul(sw) >> 8;
        }
        let right = left.wrapping_add(span);
        let (x_low, x_high, right_high, top_low, top_high, bottom_high) = if pal != 0 {
            (0x21c, 0xc94, 0xcbc, 0x13, 0x12f, 0x131)
        } else {
            (0x1f4, 0xcb2, 0xcda, 0x10, 0x101, 0x102)
        };
        let left = clamp_range(left, x_low, x_high);
        let clock = i32::from(m.lbu(GPU_DOT_CLOCK.wrapping_add(index)));
        let minimum = left.wrapping_add(clock << 2);
        let right = if right < minimum {
            minimum
        } else {
            clamp_range(right, i32::MIN, right_high)
        };
        top = clamp_range(top, top_low, top_high);
        let minimum = top.wrapping_add(2);
        bottom = if bottom < minimum {
            minimum
        } else {
            clamp_range(bottom, i32::MIN, bottom_high)
        };
        gpu_control(
            m,
            ((right as u32 & 0xfff) << 12) | (left as u32 & 0xfff) | 0x0600_0000,
        );
        gpu_control(
            m,
            ((bottom as u32 & 0x3ff) << 10) | (top as u32 & 0x3ff) | 0x0700_0000,
        );
    }
    memcpy(m, GPU_DISP_ENV, env, 0x14);
    env
}

// ---- libspu ---------------------------------------------------------------------

/// 800B2630: libspu transfer-mode word; bit 0 buffers key changes in RAM.
const SPU_BUFFERED: u32 = 0x800b_2630;
/// 800C5C78: buffered key-on (lo, hi) and key-off (lo, hi) halfwords.
pub const SPU_KEY_BUFFER: u32 = 0x800c_5c78;
/// 800B25FC: buffered-key pending flag word (bit 0).
const SPU_KEY_PENDING: u32 = 0x800b_25fc;
/// 800B25F8: buffered key-on voice bits.
const SPU_KEY_PENDING_ON: u32 = 0x800b_25f8;
/// 800B25D0: key-on voice bits of the 24 voices.
pub const SPU_KEYED: u32 = 0x800b_25d0;
/// 800B2600: per-voice sample note (24 halfwords) set by SpuSetVoiceAttr.
pub const SPU_VOICE_NOTE: u32 = 0x800b_2600;

/// 8009F3C8 SpuSetKey(on, voices) (with 8009F574/8009F57C), `voices`
/// masked to 24 bits. Buffered mode (800B2630 bit 0): the bits go to the
/// key buffer at 800C5C78 (on: +0/+2, off: +4/+6, clearing them from the
/// opposite pair), 800B25FC |= 1 and 800B25F8 gains (on) or loses (off)
/// them; no device access. Otherwise the KON/KOFF registers are written
/// (host call `[on, voices]`) and 800B25D0 gains/loses the bits. Other `on`
/// values do nothing and return 1. Returns the original's `v0`.
pub fn spu_set_key(m: &mut Machine, on: u32, voices: u32) -> u32 {
    let bits = voices & 0x00ff_ffff;
    let high = bits >> 16;
    if on > 1 {
        return 1;
    }
    let (set, clear) = if on == 1 { (0, 4) } else { (4, 0) };
    if m.lw(SPU_BUFFERED) & 1 != 0 {
        let buffer = SPU_KEY_BUFFER;
        m.sh(at(buffer, set), bits as u16);
        m.sh(at(buffer, set + 2), high as u16);
        let pending = m.lw(SPU_KEY_PENDING);
        m.sw(SPU_KEY_PENDING, pending | 1);
        let on_bits = m.lw(SPU_KEY_PENDING_ON);
        m.sw(
            SPU_KEY_PENDING_ON,
            if on == 1 {
                on_bits | bits
            } else {
                on_bits & !bits
            },
        );
        let mut v0 = u32::from(m.lhu(at(buffer, clear))) & bits;
        if v0 != 0 {
            v0 = u32::from(m.lhu(at(buffer, clear))) & !bits;
            m.sh(at(buffer, clear), v0 as u16);
        }
        v0 = u32::from(m.lhu(at(buffer, clear + 2))) & high;
        if v0 != 0 {
            v0 = u32::from(m.lhu(at(buffer, clear + 2))) & !high;
            m.sh(at(buffer, clear + 2), v0 as u16);
        }
        return v0;
    }
    let keyed = m.lw(SPU_KEYED);
    let keyed = if on == 1 { keyed | bits } else { keyed & !bits };
    m.host_call(0x8009_f3c8, [on, bits, 0, 0]);
    m.sw(SPU_KEYED, keyed);
    keyed
}

/// 8009FD68 SpuSetVoiceAttr(SpuVoiceAttr* attr) (with its blocks to
/// 800A0328, 800A0368 `_spu_note2pitch` and 8009DFF8 `_spu_FsetRXXa`):
/// every voice register write is device state (one host call `[attr]`).
/// RAM: for each voice in `attr->voice` (+0), when `attr->mask` (+4) is 0
/// or has 0x40, the sample note 800B2600[voice] = `attr->sample_note`
/// (+0x18). Returns 0 (the final delay loop's `slti`).
pub fn spu_set_voice_attr(m: &mut Machine, attr: u32) -> u32 {
    m.host_call(0x8009_fd68, [attr, 0, 0, 0]);
    let mask = m.lw(at(attr, 4));
    for voice in 0..24u32 {
        if m.lw(attr) & (1 << voice) == 0 {
            continue;
        }
        if mask == 0 || mask & 0x40 != 0 {
            let note = m.lhu(at(attr, 0x18));
            m.sh(SPU_VOICE_NOTE + voice * 2, note);
        }
    }
    0
}

/// 8009F888 SpuSetCommonAttr(SpuCommonAttr* attr) (with its blocks to
/// 8009F9E0): master/CD/external volumes, reverb depth and the SPUCNT mix
/// bits are all device registers (one host call `[attr]`). No RAM is
/// written; returns 0.
pub fn spu_set_common_attr(m: &mut Machine, attr: u32) -> u32 {
    m.host_call(0x8009_f888, [attr, 0, 0, 0]);
    0
}

/// 8009FCDC SpuGetAllKeysStatus(char* status) (with 8009FD4C): 24 status
/// bytes. The original derives each from the key-on bit (800B25D0) and the
/// voice's current ADSR volume register (device): 1 on, 3 on with a silent
/// envelope, 2 off with a sounding envelope, 0 off. One host call `[status]`,
/// then per voice the device call 8009FD4C `[status, voice, keyed]` answers
/// the byte; without a device answer, host results give the 24 bytes in
/// voice order (default: 1 for keyed voices, else 0). Returns 0.
pub fn spu_get_all_keys_status(m: &mut Machine, status: u32) -> u32 {
    m.host_call(0x8009_fcdc, [status, 0, 0, 0]);
    for voice in 0..24u32 {
        let keyed = u32::from(m.lw(SPU_KEYED) & (1 << voice) != 0);
        let byte = m.host_call_result(0x8009_fd4c, [status, voice, keyed, 0], keyed);
        m.sb(status.wrapping_add(voice), byte as u8);
    }
    0
}

/// 800B2AB4/800B2ACC: `_spu_note2pitch` semitone and fine-tune tables.
const SPU_SEMITONES: u32 = 0x800b_2ab4;
const SPU_FINE: u32 = 0x800b_2acc;

/// 800B2668/800B266C/800B2670/800B2674: libspu address alignment flag,
/// address shift, block size and alignment mask.
const SPU_ALIGN: u32 = 0x800b_2668;
const SPU_ADDRESS_SHIFT: u32 = 0x800b_266c;
const SPU_BLOCK: u32 = 0x800b_2670;
const SPU_BLOCK_MASK: u32 = 0x800b_2674;

/// 8009DFF8 `_spu_FsetRXXa(reg, address)` (with 8009E094): with alignment
/// on (800B2668), a misaligned `address` is rounded up (`+ block & !mask`;
/// the `divu` keeps its `break 7` at 8009E024). `reg == -2` returns the
/// address, `reg == -1` returns `(address >> shift) & 0xFFFF`; otherwise the
/// shifted value goes to SPU register `reg` (device: host call
/// `[reg, value]`) and the address is returned. No RAM is written.
pub fn spu_fset_rxxa(m: &mut Machine, reg: u32, address: u32) -> u32 {
    let mut address = address;
    if m.lw(SPU_ALIGN) != 0 {
        let block = m.lw(SPU_BLOCK);
        let remainder = if block == 0 {
            trap(m, 0x8009_e024);
            address
        } else {
            address % block
        };
        if remainder != 0 {
            let mask = m.lw(SPU_BLOCK_MASK);
            address = address.wrapping_add(block) & !mask;
        }
    }
    let shift = m.lw(SPU_ADDRESS_SHIFT);
    let value = address >> (shift & 31);
    match reg as i32 {
        -2 => address,
        -1 => value & 0xffff,
        _ => {
            m.host_call(0x8009_dff8, [reg, value, 0, 0]);
            address
        }
    }
}

/// 800A0368 `_spu_note2pitch(center_note, center_fine, note, fine)` (with
/// 800A0430): the SPU pitch word for `note` relative to the sample's center
/// (0x3FFF at or above two octaves up). Pure.
pub fn spu_note2pitch(
    m: &mut Machine,
    center_note: u32,
    center_fine: u32,
    note: u32,
    fine: u32,
) -> u32 {
    let fine_sum = fine.wrapping_add(center_fine) & 0xffff;
    let semitones = (note.wrapping_add(fine_sum >> 7).wrapping_sub(center_note) << 16) as i32 >> 16;
    // `mult` by 0x2AAAAAAB, `sra 1` of HI, minus the sign: division by 12.
    let octave = ((i64::from(semitones) * 0x2aaa_aaab) >> 33) as i32 - (semitones >> 31);
    let mut shift = octave.wrapping_sub(2);
    let mut semitone = semitones.wrapping_sub(octave.wrapping_mul(12));
    if ((semitone as u32) << 16) as i32 >= 0 {
        // as is
    } else {
        semitone = semitone.wrapping_add(12);
        shift = octave.wrapping_sub(3);
    }
    let index = ((((semitone as u32) << 16) as i32) >> 15) as u32;
    let base = i32::from(m.lhu(SPU_SEMITONES.wrapping_add(index)));
    let tune = i32::from(m.lhu(SPU_FINE.wrapping_add((fine_sum & 0x7f) << 1)));
    let pitch = base.wrapping_mul(tune) >> 16;
    let shift = ((shift as u32) << 16) as i32 >> 16;
    let pitch = if shift >= 0 {
        0x3fff
    } else {
        let right = shift.wrapping_neg() as u32;
        let round = 1u32 << (right.wrapping_sub(1) & 31);
        (pitch as u32).wrapping_add(round) >> (right & 31)
    };
    pitch & 0xffff
}

// ---- libgpu image transfers and TIM parsing ------------------------------------------

/// 800B97E8/800B97EA: VRAM size halfwords LoadImage/StoreImage clamp to
/// (also the drawing-area limits).
const GPU_VRAM_WIDTH: u32 = DRAW_LIMIT_WIDTH;
const GPU_VRAM_HEIGHT: u32 = DRAW_LIMIT_HEIGHT;
/// 800A7E2C/800A8068: the LoadImage/StoreImage transfers (libgpu table
/// +0x20/+0x1C) that 800A83FC runs natively.
const GPU_LOAD_TRANSFER: u32 = 0x800a_7e2c;
const GPU_STORE_TRANSFER: u32 = 0x800a_8068;
/// 800C9A30: OpenTIM/ReadTIM cursor.
pub const TIM_CURSOR: u32 = 0x800c_9a30;

/// 800AC3E8 printf(format, ...): debug output only (host call with the
/// register arguments).
pub fn printf(m: &mut Machine, args: [u32; 4]) {
    m.host_call(0x800a_c3e8, args);
}

/// 800A655C: the libgpu debug level byte (800B97E6).
pub fn gpu_debug_level(m: &mut Machine) -> u32 {
    m.lbu(GPU_DEBUG).into()
}

/// 800A66CC: libgpu argument trace for `name` and `rect`: debug level 1
/// prints only an invalid rectangle (beyond the VRAM size, negative or
/// empty), level 2 prints every call, through the print routine at
/// 800B97E0 (host calls). No RAM is written.
fn gpu_check_rect(m: &mut Machine, name: u32, rect: u32) {
    let message = match m.lbu(GPU_DEBUG) {
        1 => {
            let w = i32::from(m.lh(at(rect, 4)));
            let x = i32::from(m.lh(rect));
            let y = i32::from(m.lh(at(rect, 2)));
            let h = i32::from(m.lh(at(rect, 6)));
            let width = i32::from(m.lh(GPU_VRAM_WIDTH));
            let height = i32::from(m.lh(GPU_VRAM_HEIGHT));
            let valid = w <= width
                && w.wrapping_add(x) <= width
                && y <= height
                && y.wrapping_add(h) <= height
                && w > 0
                && x >= 0
                && y >= 0
                && h > 0;
            (!valid).then_some(0x8002_4c44)
        }
        2 => Some(0x8002_4c64),
        _ => None,
    };
    if let Some(format) = message {
        let print = m.lw(GPU_PRINT);
        m.host_call(runtime_host_id(print), [format, name, 0, 0]);
        let x = m.lh(rect) as i32 as u32;
        let y = m.lh(at(rect, 2)) as i32 as u32;
        let w = m.lh(at(rect, 4)) as i32 as u32;
        let print = m.lw(GPU_PRINT);
        m.host_call(runtime_host_id(print), [0x8002_4c50, x, y, w]);
    }
}

/// Submission of a LoadImage/StoreImage `transfer` through libgpu's table
/// entry +8 (normally [`gpu_submit`]).
fn gpu_submit_image(m: &mut Machine, transfer: u32, rect: u32, pixels: u32) -> u32 {
    let submit = gpu_table_entry(m, 8);
    if submit == 0x800a_83fc {
        gpu_submit(m, transfer, rect, 8, pixels)
    } else {
        crate::dispatch::call(m, submit, [transfer, rect, 8, pixels])
    }
}

/// 800A6910 LoadImage(RECT* rect, u_long* pixels): the table +0x20 transfer
/// (800A7E2C, [`load_image_transfer`]) through [`gpu_submit`]. Returns the
/// submission result (0).
pub fn load_image(m: &mut Machine, rect: u32, pixels: u32) -> u32 {
    gpu_check_rect(m, 0x8002_4c80, rect);
    let transfer = gpu_table_entry(m, 0x20);
    gpu_submit_image(m, transfer, rect, pixels)
}

/// 800A6970 StoreImage(RECT* rect, u_long* pixels): the table +0x1C transfer
/// (800A8068, [`store_image_transfer`]) through [`gpu_submit`].
pub fn store_image(m: &mut Machine, rect: u32, pixels: u32) -> u32 {
    gpu_check_rect(m, 0x8002_4c8c, rect);
    let transfer = gpu_table_entry(m, 0x1c);
    gpu_submit_image(m, transfer, rect, pixels)
}

/// 800A8E9C StoreImage2(RECT* rect, u_long* pixels): StoreImage without the
/// queue. Traces the rectangle, arms the DMA timeout (VSync(-1) + 0xF0, 0
/// polls), waits for the GPU (device only; the host GPU is idle at once),
/// installs DMACallback(2, 800A91C8) and runs the table +0x1C transfer
/// (800A8068, [`store_image_transfer`]: its host call `[rect, pixels]`
/// 800A6970 is answered by the host's StoreImage). Returns 0.
pub fn store_image2(m: &mut Machine, rect: u32, pixels: u32) -> u32 {
    m.push_frame(0x20);
    gpu_check_rect(m, 0x8002_4c8c, rect);
    let now = vsync(m, -1i32 as u32);
    m.sw(GPU_TIMEOUT_DEADLINE, now.wrapping_add(0xf0));
    m.sw(GPU_TIMEOUT_POLLS, 0);
    dma_callback(m, 2, 0x800a_91c8);
    let transfer = gpu_table_entry(m, 0x1c);
    crate::dispatch::call(m, transfer, [rect, pixels, 0, 0]);
    m.pop_frame(0x20);
    0
}

/// Shared prologue of 800A7E2C/800A8068: clamp the rectangle size to the
/// VRAM size (written back to the RECT) and return the transfer size in
/// words (`(w * h + 1) / 2`).
fn gpu_clamp_rect(m: &mut Machine, rect: u32) -> i32 {
    let clamp = |m: &mut Machine, field: u32, limit: u32| -> u16 {
        let value = m.lh(at(rect, field));
        if value < 0 {
            0
        } else if m.lh(limit) < value {
            m.lhu(limit)
        } else {
            value as u16
        }
    };
    let width = clamp(m, 4, GPU_VRAM_WIDTH);
    m.sh(at(rect, 4), width);
    let height = clamp(m, 6, GPU_VRAM_HEIGHT);
    let product = i32::from(m.lh(at(rect, 4))).wrapping_mul(i32::from(height as i16));
    m.sh(at(rect, 6), height);
    let rounded = product.wrapping_add(1);
    let rounded = rounded.wrapping_add(((rounded as u32) >> 31) as i32);
    rounded >> 1
}

/// 800A7BFC (libgpu table +0xC): the ClearImage transfer.
const GPU_CLEAR_TRANSFER: u32 = 0x800a_7bfc;
/// 800C80E0 / 800C8108: the clear transfer's static packet lists.
const GPU_CLEAR_PACKET: u32 = 0x800c_80e0;
const GPU_CLEAR_RESTORE: u32 = 0x800c_8108;

/// 800A67E8 ClearImage(RECT* rect, r, g, b) -> 0: fill `rect` with the
/// colour through libgpu's queue (the table +0xC transfer, see
/// [`clear_transfer`]).
pub fn clear_image(m: &mut Machine, rect: u32, r: u32, g: u32, b: u32) -> u32 {
    gpu_check_rect(m, 0x8002_4c68, rect);
    let colour = (b & 0xff) << 16 | (g & 0xff) << 8 | (r & 0xff);
    let transfer = gpu_table_entry(m, 0xc);
    gpu_submit_image(m, transfer, rect, colour)
}

/// 800A6878 ClearImage2(RECT* rect, r, g, b) -> 0: [`clear_image`] with
/// colour bit 31 set (the transfer's E1 then enables drawing to the
/// displayed area).
pub fn clear_image2(m: &mut Machine, rect: u32, r: u32, g: u32, b: u32) -> u32 {
    gpu_check_rect(m, 0x8002_4c74, rect);
    let colour = 0x8000_0000 | (b & 0xff) << 16 | (g & 0xff) << 8 | (r & 0xff);
    let transfer = gpu_table_entry(m, 0xc);
    gpu_submit_image(m, transfer, rect, colour)
}

/// 800B987C: MoveImage's static one-packet list (`04FFFFFF`, the GP0(80h)
/// command word 0x80000000 at 800B9880, then source xy, destination xy and
/// size at 800B9884..800B988F, which MoveImage fills).
const GPU_MOVE_LIST: u32 = 0x800b_987c;

/// 800A69D0 MoveImage(RECT* rect, x, y) -> -1 for an empty rectangle,
/// else the submission result (0): trace the rectangle (800A66CC, name
/// 0x80024C98), fill the list's source (the RECT's xy word), destination
/// (`y << 16 | x & 0xFFFF`) and size (the RECT's wh word), and submit it
/// through libgpu's table +8 with the table +0x18 routine (800A8360, DMA
/// list start: host call `[list, 0, 0x14]`, see [`gpu_submit`]). The
/// VRAM-to-VRAM copy itself is the device's (GP0 80h).
pub fn move_image(m: &mut Machine, rect: u32, x: u32, y: u32) -> u32 {
    gpu_check_rect(m, 0x8002_4c98, rect);
    if m.lh(at(rect, 4)) == 0 || m.lh(at(rect, 6)) == 0 {
        return -1i32 as u32;
    }
    let packet = at(GPU_MOVE_LIST, 8);
    let destination = y << 16 | x & 0xffff;
    let source = m.lw(rect);
    m.sw(at(packet, 4), destination);
    m.sw(packet, source);
    let size = m.lw(at(rect, 4));
    m.sw(at(packet, 8), size);
    gpu_submit_packet(m, GPU_MOVE_LIST, 0x14)
}

/// 800A7B04 (libgpu table +0x38): read GPUSTAT (host result, default 0).
pub fn gpu_status(m: &mut Machine) -> u32 {
    m.host_call_result(0x800a_7b04, [0; 4], 0)
}

/// 800A73C0 GetODE(): bit 31 of GPUSTAT (the interlace field) through
/// libgpu's table +0x38 routine (normally [`gpu_status`]).
pub fn get_ode(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let read = gpu_table_entry(m, 0x38);
    let status = if read == 0x800a_7b04 {
        gpu_status(m)
    } else {
        crate::dispatch::call(m, read, [0; 4])
    };
    m.pop_frame(0x18);
    status >> 31
}

/// 800A7BFC (libgpu table +0xC): the ClearImage transfer. Clamps the RECT
/// size to the VRAM size minus one (negative: 0; written back), then builds
/// a packet list in libgpu's static buffer 800C80E0 and starts it (DMA
/// channel 2, host call 800A8360 `[list]`):
/// - a 16-aligned x and width: E6 (no mask), E1 (GPUSTAT texture-page bits,
///   bit 10 from colour bit 31), the GP0(02h) fill;
/// - otherwise E3/E4/E5 opening the whole VRAM, E6, E1, a TILE (60h) of the
///   rectangle, then a second list at 800C8108 restoring the drawing area
///   and offset read back through GP1(10h) (800A83A8, 3/4/5).
///
/// Device reads, in order: GPUSTAT (host call 800A7BFC `[rect, colour]`,
/// default 0), then for the unaligned case the three GP1(10h) results.
fn clear_transfer(m: &mut Machine, rect: u32, colour: u32) -> u32 {
    let clamp = |m: &mut Machine, field: u32, limit: u32| -> u16 {
        let value = m.lh(at(rect, field));
        if value < 0 {
            0
        } else if i32::from(m.lh(limit)) - 1 < i32::from(value) {
            m.lhu(limit).wrapping_sub(1)
        } else {
            value as u16
        }
    };
    let width = clamp(m, 4, GPU_VRAM_WIDTH);
    m.sh(at(rect, 4), width);
    let height = clamp(m, 6, GPU_VRAM_HEIGHT);
    m.sh(at(rect, 6), height);
    let draw_mode = |m: &mut Machine| {
        let status = m.host_call_result(GPU_CLEAR_TRANSFER, [rect, colour, 0, 0], 0);
        0xe100_0000 | (colour >> 31) << 10 | status & 0x7ff
    };
    let list = GPU_CLEAR_PACKET;
    if m.lhu(rect) & 0x3f != 0 || m.lhu(at(rect, 4)) & 0x3f != 0 {
        let restore = GPU_CLEAR_RESTORE;
        m.sw(list, restore & 0x00ff_ffff | 0x0800_0000);
        m.sw(at(list, 0x10), 0xe600_0000);
        m.sw(at(list, 4), 0xe300_0000);
        m.sw(at(list, 8), 0xe4ff_ffff);
        m.sw(at(list, 0xc), 0xe500_0000);
        let mode = draw_mode(m);
        m.sw(at(list, 0x18), colour & 0x00ff_ffff | 0x6000_0000);
        m.sw(at(list, 0x14), mode);
        let xy = m.lw(rect);
        m.sw(at(list, 0x1c), xy);
        let wh = m.lw(at(rect, 4));
        m.sw(restore, 0x03ff_ffff);
        m.sw(at(list, 0x20), wh);
        for (i, command) in [0xe300_0000u32, 0xe400_0000, 0xe500_0000]
            .into_iter()
            .enumerate()
        {
            let value = m.host_call_result(0x800a_83a8, [3 + i as u32, 0, 0, 0], 0) & 0x00ff_ffff;
            m.sw(at(restore, 4 + 4 * i as u32), value | command);
        }
    } else {
        m.sw(list, 0x05ff_ffff);
        m.sw(at(list, 4), 0xe600_0000);
        let mode = draw_mode(m);
        m.sw(at(list, 0xc), colour & 0x00ff_ffff | 0x0200_0000);
        m.sw(at(list, 8), mode);
        let xy = m.lw(rect);
        m.sw(at(list, 0x10), xy);
        let wh = m.lw(at(rect, 4));
        m.sw(at(list, 0x14), wh);
    }
    m.host_call(0x800a_8360, [list, 0, 0, 0]);
    0
}

/// 800A7E2C (libgpu table +0x20): LoadImage transfer to VRAM. Arms the
/// timeout, clamps the RECT (RAM), then the GPU/DMA work is one host call
/// `[rect, pixels]` (800A6910); the host GPU is ready at once. Returns -1
/// for an empty rectangle, else 0.
fn load_image_transfer(m: &mut Machine, rect: u32, pixels: u32) -> u32 {
    gpu_timeout_start(m);
    if gpu_clamp_rect(m, rect) <= 0 {
        return -1i32 as u32;
    }
    m.host_call(0x800a_6910, [rect, pixels, 0, 0]);
    0
}

/// 800A8068 (libgpu table +0x1C): StoreImage transfer from VRAM, like
/// [`load_image_transfer`]; host call `[rect, pixels]` (800A6970). An
/// attached [`Device`](crate::machine::Device) writes the VRAM words (two
/// pixels each) to `pixels` during that call.
fn store_image_transfer(m: &mut Machine, rect: u32, pixels: u32) -> u32 {
    gpu_timeout_start(m);
    if gpu_clamp_rect(m, rect) <= 0 {
        return -1i32 as u32;
    }
    m.host_call(0x800a_6970, [rect, pixels, 0, 0]);
    0
}

/// 800A9218 OpenTIM(address): the TIM cursor (800C9A30) = `address`;
/// returns 0.
pub fn open_tim(m: &mut Machine, address: u32) -> u32 {
    m.sw(TIM_CURSOR, address);
    0
}

/// 800A9228 ReadTIM(TIM_IMAGE* image): parse the TIM at the cursor into
/// `image` (+0 mode, +4/+8 CLUT rect/data, +0xC/+0x10 pixel rect/data) and
/// advance the cursor past it. Returns `image`, or 0 for a non-TIM.
pub fn read_tim(m: &mut Machine, image: u32) -> u32 {
    let cursor = m.lw(TIM_CURSOR);
    let words = parse_tim(m, cursor, image);
    if words == u32::MAX {
        return 0;
    }
    let cursor = m.lw(TIM_CURSOR).wrapping_add(words << 2);
    m.sw(TIM_CURSOR, cursor);
    image
}

/// 800A9574 (with 800A9654): TIM header parse into `image`; returns the TIM
/// size in words, or -1 when the id word is not 0x10. Debug level 2 traces
/// the id, mode and address (printf host calls).
fn parse_tim(m: &mut Machine, address: u32, image: u32) -> u32 {
    if m.lw(address) != 0x10 {
        return u32::MAX;
    }
    let mode = m.lw(at(address, 4));
    let mut cursor = at(address, 8);
    m.sw(image, mode);
    if gpu_debug_level(m) == 2 {
        printf(m, [0x8002_4d74, 0x10, 0, 0]);
    }
    if gpu_debug_level(m) == 2 {
        let mode = m.lw(image);
        printf(m, [0x8002_4d80, mode, 0, 0]);
    }
    if gpu_debug_level(m) == 2 {
        printf(m, [0x8002_4d8c, cursor, 0, 0]);
    }
    let clut_words = if m.lw(image) & 8 != 0 {
        let length = m.lw(cursor);
        m.sw(at(image, 4), at(cursor, 4));
        m.sw(at(image, 8), at(cursor, 0xc));
        let words = length >> 2;
        cursor = at(cursor, words << 2);
        words
    } else {
        m.sw(at(image, 4), 0);
        m.sw(at(image, 8), 0);
        0
    };
    let length = m.lw(cursor);
    m.sw(at(image, 0xc), at(cursor, 4));
    m.sw(at(image, 0x10), at(cursor, 0xc));
    clut_words.wrapping_add((length >> 2).wrapping_add(2))
}

// ---- libgte rsin -------------------------------------------------------------------

/// 800B4444: rsin quarter-wave table (0x401 halfwords, 4.12).
const RSIN_TABLE: u32 = 0x800b_4444;

/// 800A45C8 rsin(angle): sine of a 4096-per-turn angle in 4.12 (negative
/// angles through the negated magnitude).
pub fn rsin(m: &mut Machine, angle: u32) -> u32 {
    if (angle as i32) < 0 {
        sin_1(m, 0u32.wrapping_sub(angle) & 0xfff).wrapping_neg()
    } else {
        sin_1(m, angle & 0xfff)
    }
}

/// 800A4604: the quadrant lookup of rsin for `0..=0xFFF`.
fn sin_1(m: &mut Machine, angle: u32) -> u32 {
    let a = angle as i32;
    if a < 0x801 {
        let index = if a < 0x401 {
            angle
        } else {
            0x800u32.wrapping_sub(angle)
        };
        m.lh(at(RSIN_TABLE, index << 1)) as i32 as u32
    } else if a < 0xc01 {
        (m.lh(at(0x800b_3444, angle << 1)) as i32 as u32).wrapping_neg()
    } else {
        let index = 0x1000u32.wrapping_sub(angle);
        (m.lh(at(RSIN_TABLE, index << 1)) as i32 as u32).wrapping_neg()
    }
}

// ---- BIOS events -------------------------------------------------------------------

/// 800AD778 WaitEvent(event) (BIOS B0:0A): kernel event state is host-owned
/// (host call `[event]`). Returns 1.
pub fn wait_event(m: &mut Machine, event: u32) -> u32 {
    m.host_call(0x800a_d778, [event, 0, 0, 0]);
    1
}

/// 800AD788 TestEvent(event) (BIOS B0:0B): host result, default 1 (the
/// event has occurred).
pub fn test_event(m: &mut Machine, event: u32) -> u32 {
    m.host_call_result(0x800a_d788, [event, 0, 0, 0], 1)
}

// ---- libspu reverb and transfers ---------------------------------------------------

/// 800B25CC: libspu transfer-completion event.
const SPU_TRANSFER_EVENT: u32 = 0x800b_25cc;
/// 800B25D4: the SpuSetTransferMode argument.
const SPU_TRANSFER_MODE: u32 = 0x800b_25d4;
/// 800B25D8: reverb on flag (SpuSetReverb).
const SPU_REVERB_ON: u32 = 0x800b_25d8;
/// 800B25DC/800B25E0: reverb work-area reservation flag and start address.
const SPU_REVERB_RESERVED: u32 = 0x800b_25dc;
const SPU_REVERB_START: u32 = 0x800b_25e0;
/// 800B25EC/800B25EE: reverb depth (left, right) halfwords.
pub const SPU_REVERB_DEPTH: u32 = 0x800b_25ec;
/// 800B265C: transfer start address in SPU units.
const SPU_TRANSFER_ADDRESS: u32 = 0x800b_265c;
/// 800B2660: 1 for programmed-I/O transfers, 0 for DMA.
const SPU_IO_MODE: u32 = 0x800b_2660;
/// 800B2678/800B267C: transfer done flag and transfer callback.
const SPU_TRANSFER_DONE: u32 = 0x800b_2678;
const SPU_TRANSFER_CALLBACK: u32 = 0x800b_267c;
/// 800B2694/800B2698/800B269C: DMA direction (1 read), source and 64-byte
/// block count of the last SPU DMA.
const SPU_DMA_READ: u32 = 0x800b_2694;
const SPU_DMA_SOURCE: u32 = 0x800b_2698;
const SPU_DMA_BLOCKS: u32 = 0x800b_269c;
/// 800B26AC: reserved SPU area list (8-byte records).
const SPU_RESERVED_AREAS: u32 = 0x800b_26ac;
/// 800B26B4: the zero buffer SpuClearReverbWorkArea transfers.
const SPU_ZERO_BUFFER: u32 = 0x800b_26b4;
/// 800B2BD4: reverb work-area start (SPU units) per reverb mode.
const SPU_REVERB_OFFSETS: u32 = 0x800b_2bd4;
/// 800C5AF0: libspu register shadow for buffered writes (800B2630 bit 0).
const SPU_SHADOW: u32 = 0x800c_5af0;

/// 8009EE48 SpuSetReverbDepth(SpuReverbAttr* attr): mask (+0) 0 or bits
/// 1/2 set the left/right depth (+8/+0xA): the SPU register write is a host
/// call `[attr, mask]` and the depth is kept at 800B25EC/800B25EE.
/// Returns 0.
pub fn spu_set_reverb_depth(m: &mut Machine, attr: u32) -> u32 {
    let mask = m.lw(attr);
    m.host_call(0x8009_ee48, [attr, mask, 0, 0]);
    let all = mask == 0;
    if all || mask & 2 != 0 {
        let left = m.lhu(at(attr, 8));
        m.sh(SPU_REVERB_DEPTH, left);
    }
    if all || mask & 4 != 0 {
        let right = m.lhu(at(attr, 0xa));
        m.sh(at(SPU_REVERB_DEPTH, 2), right);
    }
    0
}

/// 8009E2B8 SpuSetReverb(on): 0 clears SPUCNT bit 7, 1 sets it unless the
/// reverb area collides with a reserved area (800B25DC reserves it); the
/// state goes to 800B25D8 and the register write is a host call `[on,
/// state]`. Other values change nothing. Returns 800B25D8.
pub fn spu_set_reverb(m: &mut Machine, on: u32) -> u32 {
    let enable = match on {
        0 => Some(false),
        1 => {
            if m.lw(SPU_REVERB_RESERVED) == 1 {
                Some(true)
            } else {
                let start = m.lw(SPU_REVERB_START);
                Some(spu_in_reserved_area(m, start) == 0)
            }
        }
        _ => None,
    };
    if let Some(enable) = enable {
        m.sw(SPU_REVERB_ON, u32::from(enable));
        m.host_call(0x8009_e2b8, [on, u32::from(enable), 0, 0]);
    }
    m.lw(SPU_REVERB_ON)
}

/// 8009EEC8 SpuSetReverbVoice(on, voices): [`spu_set_any_voice`] on the
/// reverb-enable registers 0xCC/0xCD.
pub fn spu_set_reverb_voice(m: &mut Machine, on: u32, voices: u32) -> u32 {
    spu_set_any_voice(m, on, voices, 0xcc, 0xcd)
}

/// 8009EEF8: set (1), clear (0) or write (8) a 24-bit voice mask in register
/// halfwords `low`/`high`: in the RAM shadow 800C5AF0 when 800B2630 bit 0
/// buffers writes (marking the register in 800B25FC), otherwise in the SPU
/// (host call `[on, voices, low, high]`, whose result is the old register
/// value, default 0). Returns the resulting 24-bit mask.
pub fn spu_set_any_voice(m: &mut Machine, on: u32, voices: u32, low: u32, high: u32) -> u32 {
    if m.lw(SPU_BUFFERED) & 1 == 0 {
        let old = m.host_call_result(0x8009_eef8, [on, voices, low, high], 0);
        return match on {
            1 => (old | voices) & 0x00ff_ffff,
            0 => old & !(voices & 0x00ff_ffff) & 0x00ff_ffff,
            8 => voices & 0x00ff_ffff,
            _ => old & 0x00ff_ffff,
        };
    }
    let low_at = at(low << 1, SPU_SHADOW);
    let high_at = at(high << 1, SPU_SHADOW);
    let old = u32::from(m.lhu(low_at)) | ((u32::from(m.lhu(high_at)) & 0xff) << 16);
    let dirty_bit = 1u32.wrapping_shl(((low.wrapping_sub(0xc6) as i32) >> 1) as u32);
    let mark = |m: &mut Machine| {
        let dirty = m.lw(SPU_KEY_PENDING);
        m.sw(SPU_KEY_PENDING, dirty | dirty_bit);
    };
    match on {
        1 => {
            let value = m.lhu(low_at) | voices as u16;
            m.sh(low_at, value);
            let value = u32::from(m.lhu(high_at)) | ((voices >> 16) & 0xff);
            m.sh(high_at, value as u16);
            mark(m);
            (old | (voices & 0x00ff_ffff)) & 0x00ff_ffff
        }
        0 => {
            let value = m.lhu(low_at) & !(voices as u16);
            m.sh(low_at, value);
            let value = u32::from(m.lhu(high_at)) & !((voices >> 16) & 0xff);
            m.sh(high_at, value as u16);
            mark(m);
            old & !(voices & 0x00ff_ffff) & 0x00ff_ffff
        }
        8 => {
            m.sh(low_at, voices as u16);
            m.sh(high_at, ((voices >> 16) & 0xff) as u16);
            mark(m);
            voices & 0x00ff_ffff
        }
        _ => old & 0x00ff_ffff,
    }
}

/// 8009E408 `_SpuIsInAllocateArea`: nonzero when SPU address `start` (in
/// SPU units) falls in or below a reserved area of the list at 800B26AC
/// (bit 31 entries are skipped, bit 30 ends the list).
pub fn spu_in_reserved_area(m: &mut Machine, start: u32) -> u32 {
    let shift = m.lw(SPU_ADDRESS_SHIFT);
    let mut area = m.lw(SPU_RESERVED_AREAS);
    let start = start.wrapping_shl(shift);
    if area == 0 {
        return 0;
    }
    loop {
        let word = m.lw(area);
        if word & 0x8000_0000 == 0 {
            if word & 0x4000_0000 != 0 {
                return 0;
            }
            let base = word & 0x0fff_ffff;
            if base >= start {
                return 1;
            }
            let length = m.lw(at(area, 4));
            if start < base.wrapping_add(length) {
                return 1;
            }
        }
        area = at(area, 8);
    }
}

/// 8009F1B8 SpuClearReverbWorkArea(mode): zero the reverb area of `mode`
/// (< 10; 0 is the last 0x10 units) by DMA from the zero buffer in
/// 0x400-byte chunks ([`spu_t`]), waiting for the transfer event after each
/// ([`wait_event`]). I/O transfer mode and a transfer callback are
/// suspended meanwhile. Returns 0, or -1 for a bad or reserved area.
pub fn spu_clear_reverb_work_area(m: &mut Machine, mode: u32) -> u32 {
    if mode >= 10 {
        return u32::MAX;
    }
    let size_entry = at(mode << 2, SPU_REVERB_OFFSETS);
    let start = m.lw(size_entry);
    if spu_in_reserved_area(m, start) != 0 {
        return u32::MAX;
    }
    let (mut remaining, mut address) = if mode == 0 {
        let shift = m.lw(SPU_ADDRESS_SHIFT);
        (0x10u32.wrapping_shl(shift), 0xfff0u32.wrapping_shl(shift))
    } else {
        let start = m.lw(size_entry);
        let shift = m.lw(SPU_ADDRESS_SHIFT);
        (
            0x1_0000u32.wrapping_sub(start).wrapping_shl(shift),
            start.wrapping_shl(shift),
        )
    };
    let io_mode = m.lw(SPU_IO_MODE);
    let restore_mode = io_mode == 1;
    if restore_mode {
        m.sw(SPU_IO_MODE, 0);
    }
    let mut saved_callback = 0;
    if m.lw(SPU_TRANSFER_CALLBACK) != 0 {
        saved_callback = m.lw(SPU_TRANSFER_CALLBACK);
        m.sw(SPU_TRANSFER_CALLBACK, 0);
    }
    loop {
        let (chunk, more) = if remaining < 0x401 {
            (remaining, false)
        } else {
            (0x400, true)
        };
        spu_t(m, 2, address, 0);
        spu_t(m, 1, 0, 0);
        spu_t(m, 3, SPU_ZERO_BUFFER, chunk);
        let event = m.lw(SPU_TRANSFER_EVENT);
        wait_event(m, event);
        remaining = remaining.wrapping_sub(0x400);
        address = address.wrapping_add(0x400);
        if !more {
            break;
        }
    }
    if restore_mode {
        m.sw(SPU_IO_MODE, io_mode);
    }
    if saved_callback != 0 {
        m.sw(SPU_TRANSFER_CALLBACK, saved_callback);
    }
    0
}

/// 8009DC4C `_spu_t(op, ...)`: SPU transfer control. 0/1 wait for the
/// transfer address and select DMA read/write (800B2694 = 1/0); 2 sets the
/// transfer address (800B265C, SPU units); 3 starts a DMA of `size` bytes
/// from `value` (800B2698/800B269C). The SPU/DMA register work is a host
/// call `[op, value, size, SPU byte address]`; the device is ready at once.
/// Returns 0.
pub fn spu_t(m: &mut Machine, op: u32, value: u32, size: u32) -> u32 {
    match op {
        0 | 1 => m.sw(SPU_DMA_READ, u32::from(op == 0)),
        2 => {
            let shift = m.lw(SPU_ADDRESS_SHIFT);
            m.sh(SPU_TRANSFER_ADDRESS, value.wrapping_shr(shift) as u16);
        }
        3 => {
            m.sw(SPU_DMA_SOURCE, value);
            let blocks = (size >> 6) + u32::from(size & 0x3f != 0);
            m.sw(SPU_DMA_BLOCKS, blocks);
        }
        _ => return 0,
    }
    let shift = m.lw(SPU_ADDRESS_SHIFT);
    let spu = u32::from(m.lhu(SPU_TRANSFER_ADDRESS)).wrapping_shl(shift);
    m.host_call(0x8009_dc4c, [op, value, size, spu]);
    0
}

/// 8009F7A8 SpuSetTransferMode(mode): 800B25D4 = mode, 800B2660 = (mode ==
/// 1); returns the latter.
pub fn spu_set_transfer_mode(m: &mut Machine, mode: u32) -> u32 {
    let io = u32::from(mode == 1);
    m.sw(SPU_TRANSFER_MODE, mode);
    m.sw(SPU_IO_MODE, io);
    io
}

/// 8009F748 SpuSetTransferStartAddr(address): 0 outside 0x1010..=0x7FFF8,
/// else the aligned address ([`spu_fset_rxxa`]), stored in SPU units at
/// 800B265C.
pub fn spu_set_transfer_start_addr(m: &mut Machine, address: u32) -> u32 {
    if address.wrapping_sub(0x1010) > 0x7_efe8 {
        return 0;
    }
    let units = spu_fset_rxxa(m, u32::MAX, address);
    m.sh(SPU_TRANSFER_ADDRESS, units as u16);
    let shift = m.lw(SPU_ADDRESS_SHIFT);
    u32::from(m.lhu(SPU_TRANSFER_ADDRESS)).wrapping_shl(shift)
}

/// 8009F588 SpuWrite(source, size): clamp to 0x7EFF0 bytes, transfer
/// (8009DECC), clear the done flag when no transfer callback is installed;
/// returns the size.
pub fn spu_write(m: &mut Machine, source: u32, size: u32) -> u32 {
    let size = size.min(0x7_eff0);
    spu_write_data(m, source, size);
    if m.lw(SPU_TRANSFER_CALLBACK) == 0 {
        m.sw(SPU_TRANSFER_DONE, 0);
    }
    size
}

/// 8009DECC: DMA transfer through [`spu_t`] (I/O mode off) or programmed
/// I/O through the SPU data port (8009D928, host call `[source, size, SPU
/// byte address]`). Returns `size`.
pub fn spu_write_data(m: &mut Machine, source: u32, size: u32) -> u32 {
    let shift = m.lw(SPU_ADDRESS_SHIFT);
    let address = u32::from(m.lhu(SPU_TRANSFER_ADDRESS)).wrapping_shl(shift);
    if m.lw(SPU_IO_MODE) == 0 {
        spu_t(m, 2, address, 0);
        spu_t(m, 1, 0, 0);
        spu_t(m, 3, source, size);
    } else {
        m.host_call(0x8009_d928, [source, size, address, 0]);
    }
    size
}

/// 8009F7D8 SpuIsTransferCompleted(wait): 1 in I/O mode or when done;
/// otherwise [`test_event`] (waiting while it reports 0 when `wait` is 1),
/// storing done = 1 on completion.
pub fn spu_is_transfer_completed(m: &mut Machine, wait: u32) -> u32 {
    if m.lw(SPU_TRANSFER_MODE) == 1 || m.lw(SPU_TRANSFER_DONE) == 1 {
        return 1;
    }
    let event = m.lw(SPU_TRANSFER_EVENT);
    let mut ready = test_event(m, event);
    if wait == 1 {
        while ready == 0 {
            let event = m.lw(SPU_TRANSFER_EVENT);
            ready = test_event(m, event);
        }
        ready = 1;
    } else if ready != 1 {
        return ready;
    }
    m.sw(SPU_TRANSFER_DONE, ready);
    ready
}

/// 8009DBA4 `_spu_Fr_(buf, spu, blocks)`: DMA `blocks` 64-byte blocks from
/// SPU RAM (transfer address `spu`, SPU units) into `buf`. The SPU
/// register writes (transfer address, DMA-read mode, the waits) and the
/// DMA channel 4 start are one host call `[buf, spu, blocks]`, during which
/// an attached device writes the words to `buf`; RAM: 800B2694 = 1 (the
/// original stores it just before starting the DMA). Returns 1 (the
/// original's `v0`).
pub fn spu_fr(m: &mut Machine, buf: u32, spu: u32, blocks: u32) -> u32 {
    m.sw(SPU_DMA_READ, 1);
    m.host_call(0x8009_dba4, [buf, spu, blocks, 0]);
    1
}

/// 8009F358 SpuReadDecodedData(SpuDecodedData* buf, part): read the SPU's
/// capture buffers (CD left/right at sound RAM 0..0x7FF, voices 1/3 at
/// 0x800..0xFFF, 0x400 bytes each) through [`spu_fr`]: part 5 (SPU_CDONLY)
/// the CD buffers into `buf`, part 6 (SPU_VOICEONLY) the voice buffers from
/// 0x100 (8-byte units) into `buf + 0x800`, any other part all 0x1000
/// bytes. Returns 1 when SPUSTAT bit 11 (the capture half being written) is
/// set: host result `[buf, part]` (after `buf` is adjusted), default 0.
pub fn spu_read_decoded_data(m: &mut Machine, buf: u32, part: u32) -> u32 {
    m.push_frame(0x18);
    let (buf, spu, blocks) = match part {
        5 => (buf, 0, 0x20),
        6 => (buf.wrapping_add(0x800), 0x100, 0x20),
        _ => (buf, 0, 0x40),
    };
    spu_fr(m, buf, spu, blocks);
    let status = m.host_call_result(0x8009_f358, [buf, part, 0, 0], 0);
    m.pop_frame(0x18);
    u32::from(status & 0x800 != 0)
}

// ---- libcd -----------------------------------------------------------------------

/// 800C5CB0/800C5CB4/800C5CB8: CD_sync timeout deadline, poll count and the
/// routine name pointer for its timeout message.
pub const CD_TIMEOUT_DEADLINE: u32 = 0x800c_5cb0;
pub const CD_TIMEOUT_POLLS: u32 = 0x800c_5cb4;
const CD_TIMEOUT_NAME: u32 = 0x800c_5cb8;
/// 800B43B8: CD command completion status byte (2 complete, 5 error; set
/// by the libcd interrupt handler).
pub const CD_SYNC_STATUS: u32 = 0x800b_43b8;
/// 800C5C98: last command result bytes (8).
pub const CD_SYNC_RESULT: u32 = 0x800c_5c98;

/// 800A14BC CdSync(mode, result) through 800A1EEC CD_sync. RAM: deadline
/// 800C5CB0 = VSync(-1) + 0x3C0, 800C5CB4 = 0 then one per poll, 800C5CB8 =
/// 0x80024878. Each poll reads the status byte 800B43B8 (written by the CD
/// interrupt handler): 2 or 5 sets it to 2, copies the 8 result bytes from
/// 800C5C98 to `result` (when non-null) and returns the status; mode 1
/// returns 0 when the command is still running. A blocking wait (mode 0)
/// re-polls as many extra times as the host result says (default 0) and
/// reports a still-running command as untranslated (800A2134). Polling the
/// CD inside an interrupt callback (CheckCallback) and the timeout paths
/// are not translated. One host call `[mode, result]` is logged.
pub fn cd_sync(m: &mut Machine, mode: u32, result: u32) -> u32 {
    m.host_call(0x800a_14bc, [mode, result, 0, 0]);
    let now = vsync(m, -1i32 as u32);
    m.sw(CD_TIMEOUT_DEADLINE, now.wrapping_add(0x3c0));
    m.sw(CD_TIMEOUT_POLLS, 0);
    m.sw(CD_TIMEOUT_NAME, 0x8002_4878);
    let mut extra = None;
    loop {
        let now = vsync(m, -1i32 as u32);
        if (m.lw(CD_TIMEOUT_DEADLINE) as i32) < (now as i32) {
            trap(m, 0x800a_1fb8);
            return -1i32 as u32;
        }
        let polls = m.lw(CD_TIMEOUT_POLLS);
        m.sw(CD_TIMEOUT_POLLS, polls.wrapping_add(1));
        if (polls as i32) > 0x3c_0000 {
            trap(m, 0x800a_1fb8);
            return -1i32 as u32;
        }
        if check_callback(m) != 0 {
            // 800A203C: inside an interrupt callback CD_sync polls the CD
            // interrupt itself (800A2040..800A20D8).
            crate::mainloop::pending_sdk::cd_poll_interrupts(
                m,
                crate::mainloop::pending_sdk::SITE_SYNC,
            );
        }
        let status = m.lbu(CD_SYNC_STATUS);
        if status == 2 || status == 5 {
            m.sb(CD_SYNC_STATUS, 2);
            if result != 0 {
                for i in 0..8 {
                    let byte = m.lbu(CD_SYNC_RESULT + i);
                    m.sb(result.wrapping_add(i), byte);
                }
            }
            return u32::from(status);
        }
        if mode != 0 {
            return 0;
        }
        let remaining = extra.get_or_insert_with(|| m.host_results.pop_front().unwrap_or(0));
        if *remaining == 0 {
            trap(m, 0x800a_2134);
            return 0;
        }
        *remaining -= 1;
    }
}
// ---- libgte: rcos, NormalClip, MulMatrix0, SetMulMatrix, InitGeom ----------------

/// 800A4698 rcos(angle): cosine of a 4096-per-turn angle in 4.12, from the
/// rsin quarter-wave table (cosine is even: negative angles are negated).
pub fn rcos(m: &mut Machine, angle: u32) -> u32 {
    let angle = if (angle as i32) < 0 {
        0u32.wrapping_sub(angle)
    } else {
        angle
    } & 0xfff;
    let a = angle as i32;
    if a < 0x801 {
        if a < 0x401 {
            m.lh(at(RSIN_TABLE, 0x400u32.wrapping_sub(angle) << 1)) as i32 as u32
        } else {
            (m.lh(at(0x800b_3c44, angle << 1)) as i32 as u32).wrapping_neg()
        }
    } else if a < 0xc01 {
        (m.lh(at(RSIN_TABLE, 0xc00u32.wrapping_sub(angle) << 1)) as i32 as u32).wrapping_neg()
    } else {
        m.lh(at(0x800b_2c44, angle << 1)) as i32 as u32
    }
}

/// 800A58E8 NormalClip(sxy0, sxy1, sxy2) -> MAC0: `ldsxy3` (SXY0, SXY2,
/// SXY1 in the original order), NCLIP, `stMAC0`.
pub fn normal_clip(m: &mut Machine, sxy0: u32, sxy1: u32, sxy2: u32) -> u32 {
    m.gte.mtc2(reg::SXY0, sxy0);
    m.gte.mtc2(reg::SXY2, sxy2);
    m.gte.mtc2(reg::SXY1, sxy1);
    m.gte.nclip();
    m.gte.mfc2(reg::MAC0)
}

/// 800A4B08 MulMatrix0(MATRIX* m0, MATRIX* m1, MATRIX* m2) -> m2 (m2 = m0 *
/// m1; the rotation registers keep m0). Same instruction sequence as
/// MulMatrix with a separate output (all loads precede the stores).
pub fn mul_matrix0(m: &mut Machine, m0: u32, m1: u32, m2: u32) -> u32 {
    mul_matrix_into(m, m0, m1, m2);
    m2
}

/// 800A4ED8 SetMulMatrix(MATRIX* m0, MATRIX* m1) -> m0: the product m0 * m1
/// (m0 loaded as the rotation, the columns of m1 through RTV0 sf=1) loaded
/// into the rotation registers. No RAM is written.
pub fn set_mul_matrix(m: &mut Machine, m0: u32, m1: u32) -> u32 {
    let words = mul_matrix_words(m, m0, m1);
    for (i, word) in words.into_iter().enumerate() {
        m.gte.ctc2(ctrl::R11R12 + i, word);
    }
    m0
}

/// 800A53A8 TransMatrix(MATRIX* m, VECTOR* v) -> m: the translation words
/// `m+0x14..+0x1C` = the three words of `v`.
pub fn trans_matrix(m: &mut Machine, matrix: u32, vector: u32) -> u32 {
    let words = [0, 4, 8].map(|offset| m.lw(at(vector, offset)));
    for (i, word) in words.into_iter().enumerate() {
        m.sw(at(matrix, 0x14 + i as u32 * 4), word);
    }
    matrix
}

/// 800A4C18 CompMatrix(MATRIX* m0, MATRIX* m1, MATRIX* m2) -> m2: m2 = m0 *
/// m1 with translation m0 * t1 + t0. The rotation is MulMatrix0's product
/// (m0 loaded as the rotation, each column of m1 through RTV0 sf=1); words
/// +0, +0xC and +0x10 are stored before RTV0 of m1's translation, +4 and
/// +8 after it, then MAC1..3 + t0 (`add`, overflow-trapping in hardware;
/// never reached by compiled data). The GTE keeps m0 and the translation
/// vector loaded.
pub fn comp_matrix(m: &mut Machine, m0: u32, m1: u32, m2: u32) -> u32 {
    let words = mul_matrix_words(m, m0, m1);
    m.sw(m2, words[0]);
    m.sw(at(m2, 0xc), words[3]);
    m.sw(at(m2, 0x10), words[4]);
    let xy = u32::from(m.lhu(at(m1, 0x14))) | (m.lw(at(m1, 0x18)) << 16);
    let z = m.lw(at(m1, 0x1c));
    m.gte.mtc2(reg::VXY0, xy);
    m.gte.mtc2(reg::VZ0, z);
    m.gte.mvmva(true, false, 0, 0, 3);
    m.sw(at(m2, 4), words[1]);
    m.sw(at(m2, 8), words[2]);
    let mac: [u32; 3] = std::array::from_fn(|i| m.gte.mfc2(reg::MAC1 + i));
    let translation = [0x14, 0x18, 0x1c].map(|offset| m.lw(at(m0, offset)));
    for i in 0..3 {
        let value = mac[i].wrapping_add(translation[i]);
        m.sw(at(m2, 0x14 + i as u32 * 4), value);
    }
    m2
}

/// 800A5E78 RotMatrix_gte(SVECTOR* r, MATRIX* m) -> m: RotMatrix (the same
/// matrix layout) with the products through GPF (sf=1, IR0 * IR1..3 >> 12,
/// IR saturated) and two `mult`s. Each angle's table word (800B4F84, low
/// half sin, high half cos of `|angle| & 0xFFF`) has its sine negated for a
/// negative angle. Stores: +0x10 (halfword), then the words +4, +0, +8,
/// +0xC (the `add`/`sub`s are overflow-trapping in hardware; never reached
/// by table data). The GTE keeps the last GPF's state.
pub fn rot_matrix_gte(m: &mut Machine, rotation: u32, matrix: u32) -> u32 {
    let entry = |m: &mut Machine, angle: i32| {
        let sign = angle >> 31;
        let index = (((angle.wrapping_add(sign) ^ sign) as u32) << 2) & 0x3ffc;
        let word = m.lw(SIN_COS_TABLE.wrapping_add(index));
        let sine = ((word << 16).wrapping_add(sign as u32) ^ sign as u32) >> 16;
        (word & 0xffff_0000) | sine
    };
    let low = |word: u32| ((word << 16) as i32 >> 16) as u32;
    let high = |word: u32| ((word as i32) >> 16) as u32;
    let z = i32::from(m.lh(at(rotation, 4)));
    let xy = m.lw(rotation);
    let ez = entry(m, z);
    let ey = entry(m, (xy as i32) >> 16);
    let ex = entry(m, (xy << 16) as i32 >> 16);
    let (cx, sx) = (high(ex), low(ex));
    let (cy, sy) = (high(ey), low(ey));
    let (cz, sz) = (high(ez), low(ez));
    let gpf = |m: &mut Machine, factor: u32, v: [u32; 3]| {
        m.gte.mtc2(reg::IR0, factor);
        for (i, value) in v.into_iter().enumerate() {
            m.gte.mtc2(reg::IR1 + i, value);
        }
        m.gte.gpf(true, false);
        let ir: [u32; 3] = std::array::from_fn(|i| m.gte.mfc2(reg::IR1 + i));
        ir
    };
    let [cxsy, cxsz, cxcz] = gpf(m, cx, [sy, sz, cz]);
    let cycx = (cy as i32).wrapping_mul(cx as i32);
    let [sxsy, sxsz, sxcz] = gpf(m, sx, [sy, sz, cz]);
    m.sh(at(matrix, 0x10), (cycx >> 12) as u16);
    let cysx = (cy as i32).wrapping_mul(sx as i32);
    let [czcy, czsxsy, czcxsy] = gpf(m, cz, [cy, sxsy, cxsy]);
    let [szcy, szsxsy, szcxsy] = gpf(m, sz, [cy, sxsy, cxsy]);
    let word = (czsxsy.wrapping_add(cxsz) << 16) | (sy & 0xffff);
    m.sw(at(matrix, 4), word);
    m.sw(matrix, (szcy.wrapping_neg() << 16) | (czcy & 0xffff));
    let word = (((cysx >> 12).wrapping_neg() as u32) << 16) | (cxcz.wrapping_sub(szsxsy) & 0xffff);
    m.sw(at(matrix, 8), word);
    let word = (sxcz.wrapping_add(szcxsy) << 16) | (sxsz.wrapping_sub(czcxsy) & 0xffff);
    m.sw(at(matrix, 0xc), word);
    matrix
}

/// 800A53D8 ScaleMatrix(MATRIX* m, VECTOR* v) -> m: column j of the 3x3
/// part scaled by v[j] (4.12, [`fixed`]). Each halfword pair is rebuilt
/// from the low 16 bits of its products; the last element (+0x10) is stored
/// as a whole word, so its padding halfword receives the product's upper
/// bits.
pub fn scale_matrix(m: &mut Machine, matrix: u32, vector: u32) -> u32 {
    let scale = [0, 4, 8].map(|offset| m.lw(at(vector, offset)) as i32);
    // Element k of the storage order belongs to column k % 3.
    for word in 0..4u32 {
        let address = at(matrix, word * 4);
        let value = m.lw(address) as i32;
        let column = (word as usize * 2) % 3;
        let low = fixed(i32::from(value as i16), scale[column]) as u32;
        let high = fixed(value >> 16, scale[(column + 1) % 3]) as u32;
        m.sw(address, (low & 0xffff) | (high << 16));
    }
    let last = at(matrix, 0x10);
    let value = m.lw(last);
    let product = fixed(i32::from(value as i16), scale[2]);
    m.sw(last, product as u32);
    matrix
}

/// 800A5958 RotMatrix(SVECTOR* r, MATRIX* m) -> m: the rotation matrix of
/// angles (x, y, z) in the order Rz·Ry·Rx (4.12), from the word sin/cos
/// table ([`sin_cos`]). All angles are read before the first store; stores
/// follow the original order (+4, +0xA, +0x10, +0, +2, +6, +0xC, +8, +0xE).
/// Negated products are shifted after negation, as in the original.
pub fn rot_matrix(m: &mut Machine, rotation: u32, matrix: u32) -> u32 {
    let [x, y, z] = [0, 2, 4].map(|offset| m.lh(at(rotation, offset)));
    let (sx, cx) = sin_cos(m, x);
    let (sy, cy) = sin_cos(m, y);
    let negated = |a: i32, b: i32| a.wrapping_mul(b).wrapping_neg() >> 12;
    m.sh(at(matrix, 4), sy as u16);
    m.sh(at(matrix, 0xa), negated(cy, sx) as u16);
    m.sh(at(matrix, 0x10), fixed(cy, cx) as u16);
    let (sz, cz) = sin_cos(m, z);
    m.sh(matrix, fixed(cz, cy) as u16);
    m.sh(at(matrix, 2), negated(sz, cy) as u16);
    let czsy = fixed(cz, sy.wrapping_neg());
    let value = fixed(sz, cx).wrapping_sub(fixed(czsy, sx));
    m.sh(at(matrix, 6), value as u16);
    let value = fixed(sz, sx).wrapping_add(fixed(czsy, cx));
    m.sh(at(matrix, 0xc), value as u16);
    let szsy = fixed(sz, sy.wrapping_neg());
    let value = fixed(cz, cx).wrapping_add(fixed(szsy, sx));
    m.sh(at(matrix, 8), value as u16);
    let value = fixed(cz, sx).wrapping_sub(fixed(szsy, cx));
    m.sh(at(matrix, 0xe), value as u16);
    matrix
}

/// 800B4DD4/800C80D0: libgte's saved return addresses of InitGeom and its
/// kernel patch call _patch_gte.
const INIT_GEOM_RA: u32 = 0x800b_4dd4;
const PATCH_GTE_RA: u32 = 0x800c_80d0;
/// 800A6208 _patch_gte: the kernel patch InitGeom runs (a host call).
pub const PATCH_GTE: u32 = 0x800a_6208;

/// 800A4740 InitGeom(), called from `return_address`: saves the return
/// address at 800B4DD4, patches the kernel through _patch_gte (800A6208, a
/// host call; its own saved return 800A4750 goes to 800C80D0), enables COP2
/// (not modeled) and sets the GTE controls ZSF3 0x155, ZSF4 0x100, H 1000,
/// DQA -0x1062, DQB 0x1400000, OFX/OFY 0. Not claimed by [`dispatch`]: the
/// stored return address is the caller's `ra`, which calls by original
/// address do not carry.
pub fn init_geom(m: &mut Machine, return_address: u32) {
    m.sw(INIT_GEOM_RA, return_address);
    m.sw(PATCH_GTE_RA, 0x800a_4750);
    m.host_call(PATCH_GTE, [0; 4]);
    for (register, value) in [
        (ctrl::ZSF3, 0x155),
        (ctrl::ZSF4, 0x100),
        (ctrl::H, 0x3e8),
        (ctrl::DQA, (-0x1062i32) as u32),
        (ctrl::DQB, 0x0140_0000),
        (ctrl::OFX, 0),
        (ctrl::OFY, 0),
    ] {
        m.gte.ctc2(register, value);
    }
}

// ---- libgpu/libetc environments and reset ------------------------------------------

/// 800A11F8 SetVideoMode(mode) -> previous mode (the word at 800B4034).
pub fn set_video_mode(m: &mut Machine, mode: u32) -> u32 {
    let previous = m.lw(VIDEO_MODE);
    m.sw(VIDEO_MODE, mode);
    previous
}

/// 800AAA98 SetDefDrawEnv(env, x, y, w, h) -> env, in its 0x28-byte frame
/// (the function table splits its tail off at 800AAB10): clip and offset
/// (x, y), clip size (w, h), texture window 0, `dtd` = h below 0x101 (NTSC)
/// or 0x121 (PAL, by GetVideoMode), `isbg` 0, `tpage` 0x0A, `dfe` 1. `h` is
/// the caller's stack word +0x10 (read from `m.sp + 0x10` through
/// [`dispatch`]).
pub fn set_def_draw_env(m: &mut Machine, env: u32, x: u32, y: u32, w: u32, h: u32) -> u32 {
    m.push_frame(0x28);
    let mode = get_video_mode(m);
    m.sh(env, x as u16);
    m.sh(at(env, 2), y as u16);
    m.sh(at(env, 4), w as u16);
    for offset in [0xc, 0xe, 0x10, 0x12] {
        m.sh(at(env, offset), 0);
    }
    for offset in [0x19, 0x1a, 0x1b] {
        m.sb(at(env, offset), 0);
    }
    m.sb(at(env, 0x16), 1);
    m.sh(at(env, 6), h as u16);
    let limit = if mode == 0 { 0x101 } else { 0x121 };
    m.sb(at(env, 0x17), u8::from((h as i32) < limit));
    m.sh(at(env, 8), x as u16);
    m.sh(at(env, 0xa), y as u16);
    m.sh(at(env, 0x14), 0xa);
    m.sb(at(env, 0x18), 0);
    m.pop_frame(0x28);
    env
}

/// 800AAB58 SetDefDispEnv(env, x, y, w, h) -> env: display area (x, y, w,
/// h), screen rectangle and flags 0. `h` is the caller's stack word +0x10.
pub fn set_def_disp_env(m: &mut Machine, env: u32, x: u32, y: u32, w: u32, h: u32) -> u32 {
    m.sh(env, x as u16);
    m.sh(at(env, 2), y as u16);
    m.sh(at(env, 4), w as u16);
    for offset in [8, 0xa, 0xc, 0xe] {
        m.sh(at(env, offset), 0);
    }
    for offset in [0x11, 0x10, 0x13, 0x12] {
        m.sb(at(env, offset), 0);
    }
    m.sh(at(env, 6), h as u16);
    env
}

/// 800A62E8 ResetGraph(mode).
pub const RESET_GRAPH: u32 = 0x800a_62e8;
/// 800B97E4: libgpu state block cleared by a full reset (0x80 bytes): +0
/// GPU type, +1 initialized flag, +2 async byte, +4/+6 the VRAM size, the
/// environment copies from +0x10 and +0x6C.
const GPU_STATE: u32 = 0x800b_97e4;
/// 800B9864/800B9870: VRAM width/height per GPU type (words, low half used).
const GPU_VRAM_WIDTHS: u32 = 0x800b_9864;
const GPU_VRAM_HEIGHTS: u32 = 0x800b_9870;
/// 800C8230: libgpu command queue (0x1800 bytes), cleared with the GP1
/// shadow table by the mode 0/5 reset.
const GPU_QUEUE: u32 = 0x800c_8230;

/// 800A62E8 ResetGraph(mode) -> the GPU type (full reset) or 0.
///
/// The device part is one host call `[mode]`: GPU and DMA channel 2 reset
/// (800A890C's register writes), ResetCallback, GPU_cw, the interrupt mask
/// save/restore around the reset, GPU type detection (800A8D10, mode 0) and
/// the `ResetGraph:jtb=...` printf of modes 0 and 3 (the other modes print
/// through `[800B97E0]` at debug level 2, a separate host call as in the
/// other libgpu routines). Host result for mode 0:
/// the GPU type (default 0, the type the captures report; modes 3 and 5
/// skip detection and use type 0).
///
/// RAM, reproduced because libgpu (`sdk.rs`) reads it later: modes 0, 3 and
/// 5 clear the 800B97E4 block (0x80 bytes), then 800A890C zeroes the queue
/// indices 800B990C/800B9910 (and, for modes 0 and 5, the GP1 shadow table
/// 800C8128 (0x100 bytes) and the queue 800C8230 (0x1800 bytes)); finally
/// 800B97E4 = type, 800B97E5 = 1, the VRAM size 800B97E8/800B97EA from the
/// per-type tables, and both environment copies (800B97F4, 0x5C bytes;
/// 800B9850, 0x14 bytes) are filled with 0xFF. Other modes (the soft reset
/// through libgpu's table +0x34, 800A890C(1)) only zero the queue indices
/// and return 0. Not reproduced: 800B991C, the interrupt mask the reset
/// saves and restores (device state, read only inside the reset).
pub fn reset_graph(m: &mut Machine, mode: u32) -> u32 {
    m.push_frame(0x20);
    let kind = mode & 7;
    let result = if matches!(kind, 0 | 3 | 5) {
        memset(m, GPU_STATE, 0, 0x80);
        let detected = if kind == 0 {
            m.host_call_result(RESET_GRAPH, [mode, 0, 0, 0], 0)
        } else {
            m.host_call(RESET_GRAPH, [mode, 0, 0, 0]);
            0
        };
        m.sw(GPU_QUEUE_READ, 0);
        m.sw(GPU_QUEUE_WRITE, 0);
        if kind != 3 {
            memset(m, GPU_GP1_SHADOW, 0, 0x100);
            memset(m, GPU_QUEUE, 0, 0x1800);
        }
        let gpu = detected & 0xff;
        m.sb(GPU_STATE, gpu as u8);
        m.sb(at(GPU_STATE, 1), 1);
        let width = m.lhu(at(GPU_VRAM_WIDTHS, gpu << 2));
        m.sh(at(GPU_STATE, 4), width);
        let height = m.lhu(at(GPU_VRAM_HEIGHTS, gpu << 2));
        m.sh(at(GPU_STATE, 6), height);
        memset(m, GPU_DRAW_ENV, 0xff, 0x5c);
        memset(m, GPU_DISP_ENV, 0xff, 0x14);
        u32::from(m.lbu(GPU_STATE))
    } else {
        gpu_debug_print(m, 0x8002_4bac, mode, 0);
        m.host_call(RESET_GRAPH, [mode, 0, 0, 0]);
        m.sw(GPU_QUEUE_READ, 0);
        m.sw(GPU_QUEUE_WRITE, 0);
        0
    };
    m.pop_frame(0x20);
    result
}

// ---- libcd: positions and file search --------------------------------------------

/// 800A30F8 CdSearchFile(CdlFILE* fp, char* name), a host call: the device
/// fills `fp` (position and size) and returns libcd's cached entry, or 0
/// when the file is missing (the default without a device).
pub fn cd_search_file(m: &mut Machine, fp: u32, name: u32) -> u32 {
    m.host_call_result(0x800a_30f8, [fp, name, 0, 0], 0)
}

/// 800A453C CdPosToInt(CdlLOC*): BCD minute/second/sector to a logical
/// sector (`(m * 60 + s) * 75 + f - 150`).
pub fn cd_pos_to_int(m: &mut Machine, location: u32) -> u32 {
    let decode = |byte: u8| u32::from(byte >> 4) * 10 + u32::from(byte & 0xf);
    let minute = decode(m.lbu(location));
    let second = decode(m.lbu(at(location, 1)));
    let sector = decode(m.lbu(at(location, 2)));
    (minute * 60 + second)
        .wrapping_mul(75)
        .wrapping_add(sector)
        .wrapping_sub(150)
}

// ---- libc: strings and sprintf -----------------------------------------------------

/// 800AC328 strlen(s): bytes before the terminating zero; 0 for a null `s`.
pub fn strlen(m: &mut Machine, s: u32) -> u32 {
    if s == 0 {
        return 0;
    }
    let mut length = 0u32;
    let mut p = s;
    while m.lb(p) != 0 {
        length = length.wrapping_add(1);
        p = p.wrapping_add(1);
    }
    length
}

/// 800AC1B8 strcat(dst, src): appends `src` (with its terminator) at the
/// end of `dst` and returns `dst`. Returns 0 without copying for a null
/// `dst` or `src`, or when `dst + strlen(dst) == src + strlen(src)` (the
/// two strings end together).
pub fn strcat(m: &mut Machine, dst: u32, src: u32) -> u32 {
    if dst == 0 || src == 0 {
        return 0;
    }
    m.push_frame(0x20);
    let dst_end = dst.wrapping_add(strlen(m, dst));
    let src_end = src.wrapping_add(strlen(m, src));
    let result = if dst_end == src_end {
        0
    } else {
        // The end of `dst`, found again byte by byte.
        let mut d = dst;
        while m.lb(d) != 0 {
            d = d.wrapping_add(1);
        }
        let mut s = src;
        loop {
            let c = m.lbu(s);
            s = s.wrapping_add(1);
            m.sb(d, c);
            d = d.wrapping_add(1);
            if c == 0 {
                break;
            }
        }
        dst
    };
    m.pop_frame(0x20);
    result
}

/// 800AC0B8 memchr(s, c, n): the first of `n` bytes at `s` equal to the low
/// byte of `c`, or 0 (also for a null `s` or `n <= 0`, signed).
pub fn memchr(m: &mut Machine, s: u32, c: u32, n: u32) -> u32 {
    if s == 0 || (n as i32) <= 0 {
        return 0;
    }
    let c = c & 0xff;
    let mut left = (n as i32) - 1;
    let mut p = s;
    while left >= 0 {
        let byte = u32::from(m.lbu(p));
        p = p.wrapping_add(1);
        if byte == c {
            return p.wrapping_sub(1);
        }
        left -= 1;
    }
    0
}

/// 800AD5A8 memmove(dst, src, n): byte copy of `n` (signed; none when
/// `<= 0`), backwards when `dst >= src` (unsigned). Returns `dst`, except
/// that a forward copy returns `dst + n` (the original returns its
/// advanced pointer).
pub fn memmove(m: &mut Machine, dst: u32, src: u32, n: u32) -> u32 {
    let count = n as i32;
    if dst >= src {
        let mut i = count - 1;
        if count > 0 {
            loop {
                let byte = m.lbu(src.wrapping_add(i as u32));
                let more = i > 0;
                m.sb(dst.wrapping_add(i as u32), byte);
                i -= 1;
                if !more {
                    break;
                }
            }
        }
        return dst;
    }
    let (mut d, mut s) = (dst, src);
    let mut i = count - 1;
    if count > 0 {
        loop {
            let byte = m.lbu(s);
            s = s.wrapping_add(1);
            let more = i > 0;
            i -= 1;
            m.sb(d, byte);
            d = d.wrapping_add(1);
            if !more {
                break;
            }
        }
    }
    d
}

/// 800BA974: sprintf's default conversion specification (flags, width,
/// precision words), copied for every `%`.
const SPRINTF_DEFAULTS: u32 = 0x800b_a974;
/// 80025124 / 80025138: hexadecimal digits, upper and lower case.
const HEX_UPPER: u32 = 0x8002_5124;
const HEX_LOWER: u32 = 0x8002_5138;

/// sprintf's specification in its frame (+0x210): flags word (bit 0 `-`,
/// 1 `+`, 2 `#`, 3 `0`, 4 precision given, 5 `h`, 6 `l`, 7 `L`; byte +1 is
/// the sign character), width (+0x214), precision (+0x218).
struct Spec {
    flags: u32,
    sign: u32,
    width: u32,
    precision: u32,
}
impl Spec {
    fn at(frame: u32) -> Self {
        Self {
            flags: at(frame, 0x210),
            sign: at(frame, 0x211),
            width: at(frame, 0x214),
            precision: at(frame, 0x218),
        }
    }
}

/// 800ACD28 sprintf(buf, format, ...) -> length. PsyQ's formatter, in its
/// 0x248-byte frame: the home slots of a1..a3 hold the format pointer and
/// the first variadic words, which continue in the caller's argument area
/// (`sp+0x10..`); digits are built downwards from frame +0x210 into the
/// 0x200-byte buffer below the specification. Supported: flags `- + space
/// # 0`, width and precision (digits or `*`), modifiers `h l L`,
/// conversions `d i u o x X p c s n` and `%%`; any other conversion ends
/// the output. Strings take strlen, or memchr with a precision; `#s` reads
/// a length-prefixed string. Fields are padded with spaces and copied with
/// memmove. The output is terminated; returns its length.
pub fn sprintf(m: &mut Machine, buf: u32, format: u32, a2: u32, a3: u32) -> u32 {
    let frame = m.push_frame(0x248);
    let caller = at(frame, 0x248);
    m.sw(at(caller, 4), format);
    m.sw(at(caller, 8), a2);
    m.sw(at(caller, 0xc), a3);
    let length = sprintf_body(m, buf, frame);
    m.pop_frame(0x248);
    length
}

fn sprintf_body(m: &mut Machine, buf: u32, frame: u32) -> u32 {
    let cursor = at(frame, 0x24c);
    let ap = at(frame, 0x220);
    let spec = Spec::at(frame);
    m.sw(ap, at(frame, 0x250));
    let mut length = 0u32;
    let next_arg = |m: &mut Machine| {
        let p = m.lw(ap);
        let value = m.lw(p);
        m.sw(ap, p.wrapping_add(4));
        value
    };
    // Advance the format pointer and read the next (signed) character.
    let advance = |m: &mut Machine| {
        let p = m.lw(cursor);
        m.sw(cursor, p.wrapping_add(1));
        m.lb(p.wrapping_add(1)) as i32
    };
    let format = m.lw(cursor);
    let mut c = m.lb(format) as i32;
    while c != 0 {
        if c != 0x25 {
            m.sb(buf.wrapping_add(length), c as u8);
            length = length.wrapping_add(1);
            c = advance(m);
            continue;
        }
        for (i, field) in [spec.flags, spec.width, spec.precision]
            .into_iter()
            .enumerate()
        {
            let value = m.lw(at(SPRINTF_DEFAULTS, i as u32 * 4));
            m.sw(field, value);
        }
        // Flags.
        loop {
            c = advance(m);
            let bit = match c {
                0x2d => 1,
                0x2b => 2,
                0x20 => {
                    m.sb(spec.sign, 0x20);
                    continue;
                }
                0x23 => 4,
                0x30 => 8,
                _ => break,
            };
            let flags = m.lw(spec.flags);
            m.sw(spec.flags, flags | bit);
        }
        // Width.
        if c == 0x2a {
            let width = next_arg(m);
            m.sw(spec.width, width);
            if (width as i32) < 0 {
                let flags = m.lw(spec.flags);
                m.sw(spec.width, width.wrapping_neg());
                m.sw(spec.flags, flags | 1);
            }
            c = advance(m);
        } else {
            while (c.wrapping_sub(0x30) as u32) < 10 {
                let width = m.lw(spec.width);
                let value = width
                    .wrapping_mul(10)
                    .wrapping_sub(0x30)
                    .wrapping_add(c as u32);
                m.sw(spec.width, value);
                c = advance(m);
            }
        }
        // Precision.
        if c == 0x2e {
            c = advance(m);
            if c == 0x2a {
                let precision = next_arg(m);
                c = advance(m);
                m.sw(spec.precision, precision);
            } else {
                while (c.wrapping_sub(0x30) as u32) < 10 {
                    let precision = m.lw(spec.precision);
                    let value = precision
                        .wrapping_mul(10)
                        .wrapping_sub(0x30)
                        .wrapping_add(c as u32);
                    m.sw(spec.precision, value);
                    c = advance(m);
                }
            }
            if (m.lw(spec.precision) as i32) >= 0 {
                let flags = m.lw(spec.flags);
                m.sw(spec.flags, flags | 0x10);
            }
        }
        let flags = m.lw(spec.flags);
        if flags & 1 != 0 {
            m.sw(spec.flags, flags & !8);
        }
        let mut digits = at(frame, 0x210);
        // Modifiers, then the conversion: `None` ends the output.
        let field: Option<(u32, u32)> = loop {
            let modifier = match c {
                0x68 => 0x20,
                0x6c => 0x40,
                0x4c => 0x80,
                _ => 0,
            };
            if modifier != 0 {
                let flags = m.lw(spec.flags);
                m.sw(spec.flags, flags | modifier);
                c = advance(m);
                continue;
            }
            break match c {
                0x64 | 0x69 | 0x75 => {
                    let mut value = next_arg(m);
                    let flags = m.lw(spec.flags);
                    if c == 0x75 {
                        if flags & 0x20 != 0 {
                            value &= 0xffff;
                        }
                        m.sb(spec.sign, 0);
                    } else {
                        if flags & 0x20 != 0 {
                            value = value as u16 as i16 as i32 as u32;
                        }
                        if (value as i32) < 0 {
                            value = value.wrapping_neg();
                            m.sb(spec.sign, 0x2d);
                        } else if flags & 2 != 0 {
                            m.sb(spec.sign, 0x2b);
                        }
                    }
                    let flags = m.lw(spec.flags);
                    if flags & 0x10 == 0 {
                        if flags & 8 != 0 {
                            let width = m.lw(spec.width);
                            m.sw(spec.precision, width);
                            if m.lb(spec.sign) != 0 {
                                m.sw(spec.precision, width.wrapping_sub(1));
                            }
                        }
                        if (m.lw(spec.precision) as i32) <= 0 {
                            m.sw(spec.precision, 1);
                        }
                    }
                    let mut count = 0u32;
                    while value != 0 {
                        let quotient = value / 10;
                        digits = digits.wrapping_sub(1);
                        count = count.wrapping_add(1);
                        let digit = value.wrapping_sub(quotient.wrapping_mul(10)) + 0x30;
                        value = quotient;
                        m.sb(digits, digit as u8);
                    }
                    count = zero_pad(m, &spec, &mut digits, count);
                    let sign = m.lbu(spec.sign);
                    if m.lb(spec.sign) != 0 {
                        digits = digits.wrapping_sub(1);
                        m.sb(digits, sign);
                        count = count.wrapping_add(1);
                    }
                    Some((digits, count))
                }
                0x6f => {
                    let mut value = next_arg(m);
                    let flags = m.lw(spec.flags);
                    if flags & 0x20 != 0 {
                        value &= 0xffff;
                    }
                    default_precision(m, &spec, flags, 0);
                    let mut count = 0u32;
                    while value != 0 {
                        digits = digits.wrapping_sub(1);
                        m.sb(digits, ((value & 7) + 0x30) as u8);
                        value >>= 3;
                        count = count.wrapping_add(1);
                    }
                    if m.lw(spec.flags) & 4 != 0 && count != 0 && m.lb(digits) != 0x30 {
                        digits = digits.wrapping_sub(1);
                        m.sb(digits, 0x30);
                        count = count.wrapping_add(1);
                    }
                    let count = zero_pad(m, &spec, &mut digits, count);
                    Some((digits, count))
                }
                0x70 | 0x58 | 0x78 => {
                    if c == 0x70 {
                        let flags = m.lw(spec.flags);
                        m.sw(spec.precision, 8);
                        m.sw(spec.flags, flags | 0x50);
                    }
                    let table = if c == 0x78 { HEX_LOWER } else { HEX_UPPER };
                    let mut value = next_arg(m);
                    let flags = m.lw(spec.flags);
                    if flags & 0x20 != 0 {
                        value &= 0xffff;
                    }
                    default_precision(m, &spec, flags, 2);
                    let mut count = 0u32;
                    while value != 0 {
                        digits = digits.wrapping_sub(1);
                        let digit = m.lbu(at(table, value & 0xf));
                        value >>= 4;
                        count = count.wrapping_add(1);
                        m.sb(digits, digit);
                    }
                    let mut count = zero_pad(m, &spec, &mut digits, count);
                    if m.lw(spec.flags) & 4 != 0 {
                        digits = digits.wrapping_sub(1);
                        m.sb(digits, c as u8);
                        digits = digits.wrapping_sub(1);
                        count = count.wrapping_add(2);
                        m.sb(digits, 0x30);
                    }
                    Some((digits, count))
                }
                0x63 => {
                    digits = digits.wrapping_sub(1);
                    let p = m.lw(ap);
                    let byte = m.lbu(p);
                    m.sb(digits, byte);
                    m.sw(ap, p.wrapping_add(4));
                    Some((digits, 1))
                }
                0x73 => {
                    let mut string = next_arg(m);
                    let flags = m.lw(spec.flags);
                    let count = if flags & 4 != 0 {
                        let mut count = u32::from(m.lbu(string));
                        string = string.wrapping_add(1);
                        if flags & 0x10 != 0 {
                            let precision = m.lw(spec.precision);
                            if (precision as i32) < count as i32 {
                                count = precision;
                            }
                        }
                        count
                    } else if flags & 0x10 == 0 {
                        strlen(m, string)
                    } else {
                        let precision = m.lw(spec.precision);
                        let found = memchr(m, string, 0, precision);
                        if found == 0 {
                            m.lw(spec.precision)
                        } else {
                            found.wrapping_sub(string)
                        }
                    };
                    Some((string, count))
                }
                0x6e => {
                    let target = next_arg(m);
                    if m.lw(spec.flags) & 0x20 != 0 {
                        m.sh(target, length as u16);
                    } else {
                        m.sw(target, length);
                    }
                    None
                }
                _ => {
                    if c != 0x25 {
                        m.sb(buf.wrapping_add(length), 0);
                        return length;
                    }
                    m.sb(buf.wrapping_add(length), 0x25);
                    length = length.wrapping_add(1);
                    None
                }
            };
        };
        if let Some((source, count)) = field {
            // 800AD4C8: right-justify with spaces, copy, left-justify.
            if (count as i32) < (m.lw(spec.width) as i32) && m.lw(spec.flags) & 1 == 0 {
                loop {
                    m.sb(buf.wrapping_add(length), 0x20);
                    let width = m.lw(spec.width).wrapping_sub(1);
                    m.sw(spec.width, width);
                    length = length.wrapping_add(1);
                    if (count as i32) >= width as i32 {
                        break;
                    }
                }
            }
            memmove(m, buf.wrapping_add(length), source, count);
            length = length.wrapping_add(count);
            let mut filled = count;
            while (filled as i32) < (m.lw(spec.width) as i32) {
                m.sb(buf.wrapping_add(length), 0x20);
                filled = filled.wrapping_add(1);
                length = length.wrapping_add(1);
            }
        }
        c = advance(m);
    }
    m.sb(buf.wrapping_add(length), 0);
    length
}

/// The default precision of `o` and `x`/`X`/`p` without one: the width when
/// zero-padding (less `prefix` with `#`), at least 1.
fn default_precision(m: &mut Machine, spec: &Spec, flags: u32, prefix: u32) {
    if flags & 0x10 != 0 {
        return;
    }
    if flags & 8 != 0 {
        let width = m.lw(spec.width);
        m.sw(spec.precision, width);
        if prefix != 0 && flags & 4 != 0 {
            m.sw(spec.precision, width.wrapping_sub(prefix));
        }
    }
    if (m.lw(spec.precision) as i32) <= 0 {
        m.sw(spec.precision, 1);
    }
}

/// Leading zeros up to the precision; returns the digit count.
fn zero_pad(m: &mut Machine, spec: &Spec, digits: &mut u32, mut count: u32) -> u32 {
    while (count as i32) < (m.lw(spec.precision) as i32) {
        *digits = digits.wrapping_sub(1);
        m.sb(*digits, 0x30);
        count = count.wrapping_add(1);
    }
    count
}

// ---- calls by original address ----------------------------------------------------

/// Native targets owned by this module, for `jalr` and calls by original
/// address with the original register ABI (`args` = a0..a3, result = v0).
/// Routines that leave `v0` unchanged return 0 here. 64-bit `__divdi3`
/// and `__udivdi3` return the low word (call [`divdi3`] / [`udivdi3`] for
/// both halves). RotTransPers3 reads its
/// four stack arguments from `m.sp + 0x10..0x1C`, the caller's outgoing
/// argument area, as the original does (SetDefDrawEnv/SetDefDispEnv their
/// `h` from `m.sp + 0x10`). InitGeom (800A4740, [`init_geom`]) is not
/// claimed: it stores its caller's return address.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let [a0, a1, a2, a3] = args;
    Some(match target {
        0x8009_ce80 => {
            let numerator = (u64::from(a1) << 32 | u64::from(a0)) as i64;
            let denominator = (u64::from(a3) << 32 | u64::from(a2)) as i64;
            divdi3(m, numerator, denominator) as u32
        }
        0x8009_cf18 => {
            let numerator = u64::from(a1) << 32 | u64::from(a0);
            let denominator = u64::from(a3) << 32 | u64::from(a2);
            udivdi3(m, numerator, denominator) as u32
        }
        0x800a_0568 => vsync(m, a0),
        0x800a_47c8 => square_root0(m, a0),
        0x800a_4928 => vector_normal_ss_v0(m, a0, a1),
        0x800a_4d78 => apply_matrix_lv(m, a0, a1, a2),
        0x800a_4fe8 => apply_rot_matrix_lv(m, a0, a1),
        0x800a_5128 => mul_matrix(m, a0, a1),
        0x800a_5238 => mul_matrix2(m, a0, a1),
        0x800a_5348 => apply_matrix_sv(m, a0, a1, a2),
        0x800a_5518 => {
            set_rot_matrix(m, a0);
            0
        }
        0x800a_5548 => {
            set_color_matrix(m, a0);
            0
        }
        0x800a_5578 => {
            set_trans_matrix(m, a0);
            0
        }
        0x800a_5598 => {
            read_geom_offset(m, a0, a1);
            0
        }
        0x800a_55b8 => read_geom_screen(m),
        0x800a_55c8 => {
            set_back_color(m, a0, a1, a2);
            0
        }
        0x800a_55e8 => {
            set_geom_offset(m, a0, a1);
            0
        }
        0x800a_5608 => {
            set_geom_screen(m, a0);
            0
        }
        0x800a_5828 => rot_trans_pers(m, a0, a1, a2, a3),
        0x800a_5858 => {
            let stack: [u32; 4] = std::array::from_fn(|i| m.lw(at(m.sp, 0x10 + i as u32 * 4)));
            rot_trans_pers3(
                m,
                [a0, a1, a2],
                [a3, stack[0], stack[1]],
                stack[2],
                stack[3],
            )
        }
        0x800a_58b8 => rot_trans(m, a0, a1, a2),
        0x800a_5918 => transpose_matrix(m, a0, a1),
        0x800a_5be8 => rot_matrix_yxz(m, a0, a1),
        0x800a_6088 => ratan2(m, a0, a1),
        0x800a_6b50 => clear_otag_r(m, a0, a1),
        0x800a_73f0 => set_draw_area(m, a0, a1),
        0x800a_7938 => draw_area_start(m, a0 as i16, a1 as i16),
        0x800a_79d0 => draw_area_end(m, a0 as i16, a1 as i16),
        0x800a_abd8 => {
            cat_prim(m, a0, a1);
            m.lw(a0)
        }
        0x800a_ac08 => {
            set_draw_tpage(m, a0, a1, a2, a3);
            m.lw(at(a0, 4))
        }
        0x800a_c108 => memcpy(m, a0, a1, a2),
        0x8009_b1e0 => pad_chk_vsync(m),
        0x8009_f3c8 => spu_set_key(m, a0, a1),
        0x8009_f888 => spu_set_common_attr(m, a0),
        0x8009_fcdc => spu_get_all_keys_status(m, a0),
        0x8009_fd68 => spu_set_voice_attr(m, a0),
        0x800a_0368 => spu_note2pitch(m, a0, a1, a2, a3),
        0x800a_0778 => reset_callback(m),
        0x800a_07a8 => interrupt_callback(m, a0, a1),
        0x800a_07d8 => dma_callback(m, a0, a1),
        0x800a_0808 => vsync_callback(m, a0),
        0x800a_083c => vsync_callbacks(m, a0, a1),
        0x800a_086c => stop_callback(m),
        0x800a_089c => restart_callback(m),
        0x800a_08cc => check_callback(m),
        0x800a_08f4 => set_intr_mask(m, a0),
        0x800a_120c => get_video_mode(m),
        0x800a_14bc | 0x800a_1eec => cd_sync(m, a0, a1),
        0x8009_dff8 => spu_fset_rxxa(m, a0, a1),
        0x800a_8a5c => gpu_sync(m, a0),
        0x800a_6664 => draw_sync(m, a0),
        0x800a_6c58 => {
            draw_otag(m, a0);
            0
        }
        0x800a_6cc8 => put_draw_env(m, a0),
        0x800a_6e94 => put_disp_env(m, a0),
        0x800a_76a8 => set_draw_env(m, a0, a1),
        0x800a_7918 => draw_mode_command(a0, a1, a2),
        0x800a_7a68 => draw_offset_command(a0, a1),
        0x800a_7a84 => texture_window_command(m, a0),
        0x800a_af74 => dec_dct_out_callback(m, a0),
        0x800a_d934 => get_rcnt(m, a0),
        0x800a_d9d0 => reset_rcnt(m, a0),
        0x800a_c148 => memset(m, a0, a1, a2),
        0x800a_c178 => rand(m),
        0x800a_c3e8 => {
            printf(m, args);
            0
        }
        0x800a_4698 => rcos(m, a0),
        0x800a_58e8 => normal_clip(m, a0, a1, a2),
        0x800a_4b08 => mul_matrix0(m, a0, a1, a2),
        0x800a_4ed8 => set_mul_matrix(m, a0, a1),
        0x800a_53a8 => trans_matrix(m, a0, a1),
        0x800a_4c18 => comp_matrix(m, a0, a1, a2),
        0x800a_5e78 => rot_matrix_gte(m, a0, a1),
        0x800a_8e9c => store_image2(m, a0, a1),
        0x800a_c328 => strlen(m, a0),
        0x800a_c1b8 => strcat(m, a0, a1),
        0x800a_c0b8 => memchr(m, a0, a1, a2),
        0x800a_d5a8 => memmove(m, a0, a1, a2),
        0x800a_cd28 => sprintf(m, a0, a1, a2, a3),
        0x800a_53d8 => scale_matrix(m, a0, a1),
        0x800a_5958 => rot_matrix(m, a0, a1),
        0x800a_11f8 => set_video_mode(m, a0),
        0x800a_aa98 => {
            let h = m.lw(at(m.sp, 0x10));
            set_def_draw_env(m, a0, a1, a2, a3, h)
        }
        0x800a_ab58 => {
            let h = m.lw(at(m.sp, 0x10));
            set_def_disp_env(m, a0, a1, a2, a3, h)
        }
        0x800a_62e8 => reset_graph(m, a0),
        0x800a_30f8 => cd_search_file(m, a0, a1),
        0x800a_453c => cd_pos_to_int(m, a0),
        0x800a_83fc => gpu_submit(m, a0, a1, a2, a3),
        0x800a_8b98 => {
            gpu_timeout_start(m);
            0
        }
        0x800a_655c => gpu_debug_level(m),
        0x800a_66cc => {
            gpu_check_rect(m, a0, a1);
            0
        }
        0x800a_6910 => load_image(m, a0, a1),
        0x800a_6970 => store_image(m, a0, a1),
        0x800a_7e2c => load_image_transfer(m, a0, a1),
        0x800a_7bfc => clear_transfer(m, a0, a1),
        0x800a_67e8 => clear_image(m, a0, a1, a2, a3),
        0x800a_6878 => clear_image2(m, a0, a1, a2, a3),
        0x800a_69d0 => move_image(m, a0, a1, a2),
        0x800a_73c0 => get_ode(m),
        0x800a_7b04 => gpu_status(m),
        0x800a_ac38 => {
            let tw = m.lw(at(m.sp, 0x10));
            set_draw_mode(m, a0, a1, a2, a3, tw)
        }
        0x800a_8068 => store_image_transfer(m, a0, a1),
        0x800a_9218 => open_tim(m, a0),
        0x800a_9228 => read_tim(m, a0),
        0x800a_9574 => parse_tim(m, a0, a1),
        0x800a_45c8 => rsin(m, a0),
        0x800a_4604 => sin_1(m, a0),
        0x800a_d778 => wait_event(m, a0),
        0x800a_d788 => test_event(m, a0),
        0x8009_ee48 => spu_set_reverb_depth(m, a0),
        0x8009_e2b8 => spu_set_reverb(m, a0),
        0x8009_eec8 => spu_set_reverb_voice(m, a0, a1),
        0x8009_eef8 => spu_set_any_voice(m, a0, a1, a2, a3),
        0x8009_e408 => spu_in_reserved_area(m, a0),
        0x8009_f1b8 => spu_clear_reverb_work_area(m, a0),
        0x8009_dc4c => spu_t(m, a0, a1, a2),
        0x8009_f7a8 => spu_set_transfer_mode(m, a0),
        0x8009_f748 => spu_set_transfer_start_addr(m, a0),
        0x8009_f588 => spu_write(m, a0, a1),
        0x8009_decc => spu_write_data(m, a0, a1),
        0x8009_f7d8 => spu_is_transfer_completed(m, a0),
        0x8009_f358 => spu_read_decoded_data(m, a0, a1),
        0x8009_dba4 => spu_fr(m, a0, a1, a2),
        0x800a_c1a8 => {
            srand(m, a0);
            0
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine_with_limits(width: u16, height: u16) -> Machine {
        let mut m = Machine::default();
        m.sh(DRAW_LIMIT_WIDTH, width);
        m.sh(DRAW_LIMIT_HEIGHT, height);
        m
    }

    #[test]
    fn draw_area_clamps_each_path() {
        let mut m = machine_with_limits(1024, 512);
        assert_eq!(draw_area_start(&mut m, 12, 12), 0xe300_300c);
        assert_eq!(draw_area_end(&mut m, 75, 75), 0xe401_2c4b);
        // Negative coordinates clamp to 0.
        assert_eq!(draw_area_start(&mut m, -5, -1), 0xe300_0000);
        // Above the limit: limit - 1 (masked to 10 bits).
        assert_eq!(
            draw_area_end(&mut m, 2000, 600),
            0xe400_0000 | (511 << 10) | 1023
        );
        // Exactly the last valid coordinate is kept.
        assert_eq!(
            draw_area_start(&mut m, 1023, 511),
            0xe300_0000 | (511 << 10) | 1023
        );
        // A zero limit compares against -1 and yields 0xFFFFFFFF & 0x3FF.
        let mut m = machine_with_limits(0, 0);
        assert_eq!(
            draw_area_start(&mut m, 0, 0),
            0xe300_0000 | (0x3ff << 10) | 0x3ff
        );
        assert!(m.take_fault().is_ok());
    }

    #[test]
    fn set_draw_area_wraps_like_sixteen_bit_registers() {
        let mut m = machine_with_limits(1024, 512);
        let prim = 0x8010_0000;
        let rect = 0x8011_0000;
        for (offset, value) in [(0, 0x7fff), (2, 0), (4, 2), (6, 0x100)] {
            m.sh(rect + offset, value);
        }
        set_draw_area(&mut m, prim, rect);
        assert_eq!(m.lbu(prim + 3), 2);
        assert_eq!(m.lw(prim + 4), 0xe300_0000 | 1023);
        // 0x7FFF + 2 - 1 wraps to -0x8000, which clamps to 0.
        assert_eq!(m.lw(prim + 8), 0xe400_0000 | (0xff << 10));
    }

    #[test]
    fn draw_tpage_flags_and_cat_prim() {
        let mut m = Machine::default();
        let prim = 0x8010_0000;
        m.sw(prim, 0x09ab_cdef);
        set_draw_tpage(&mut m, prim, 1, 0, 0xffff);
        assert_eq!(m.lbu(prim + 3), 1);
        assert_eq!(m.lw(prim), 0x01ab_cdef);
        assert_eq!(m.lw(prim + 4), 0xe100_0dff);
        set_draw_tpage(&mut m, prim, 0, 7, 0x36);
        assert_eq!(m.lw(prim + 4), 0xe100_0236);
        cat_prim(&mut m, prim, 0x8012_3456);
        assert_eq!(m.lw(prim), 0x0112_3456);
    }

    #[test]
    fn long_split_keeps_the_sign() {
        assert_eq!(split_long(0x0001_8005), (3, 5));
        assert_eq!(
            split_long((-0x0001_8005i32) as u32),
            ((-3i32) as u32, (-5i32) as u32)
        );
        assert_eq!(split_long(0x8000_0000), (0x1_0000, 0), "wrapped negation");
    }

    /// RotMatrixYXZ against the closed form on a synthetic table, for every
    /// sign combination (the captures have non-negative angles only).
    #[test]
    fn rot_matrix_yxz_negative_angles() {
        let mut m = Machine::default();
        let pair = |i: u32| -> (i32, i32) { (i as i32 * 3 - 0x700, 0x900 - i as i32 * 5) };
        for i in 0..0x1000u32 {
            let (s, c) = pair(i);
            m.sw(
                SIN_COS_TABLE + i * 4,
                (s as u32 & 0xffff) | ((c as u32) << 16),
            );
        }
        let sin_cos = |a: i16| {
            let (s, c) = pair(u32::from(a.unsigned_abs()) & 0xfff);
            (if a < 0 { -s } else { s }, c)
        };
        let mul = |a: i32, b: i32| a.wrapping_mul(b) >> 12;
        for angles in [
            [0x123i16, 0x456, 0x789],
            [-0x123, 0x456, -0x789],
            [-1, -0x800, i16::MIN],
        ] {
            for (i, a) in angles.iter().enumerate() {
                m.sh(0x8001_0000 + i as u32 * 2, *a as u16);
            }
            assert_eq!(
                rot_matrix_yxz(&mut m, 0x8001_0000, 0x8001_0010),
                0x8001_0010
            );
            let (sx, cx) = sin_cos(angles[0]);
            let (sy, cy) = sin_cos(angles[1]);
            let (sz, cz) = sin_cos(angles[2]);
            let expected = [
                mul(cy, cz) + mul(mul(sy, sx), sz),
                mul(mul(sy, sx), cz) - mul(cy, sz),
                mul(sy, cx),
                mul(sz, cx),
                mul(cz, cx),
                -sx,
                mul(mul(cy, sx), sz) - mul(sy, cz),
                mul(sy, sz) + mul(mul(cy, sx), cz),
                mul(cy, cx),
            ];
            for (i, value) in expected.into_iter().enumerate() {
                assert_eq!(
                    m.lhu(0x8001_0010 + i as u32 * 2),
                    value as u16,
                    "{angles:?} [{i}]"
                );
            }
        }
        assert!(m.take_fault().is_ok());
    }
}
