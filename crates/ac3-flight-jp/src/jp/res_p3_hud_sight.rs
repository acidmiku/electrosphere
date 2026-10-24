//! Patches of the `hud` sight group (generated `hud::tree_62680_a`) whose JP
//! instructions differ from PAL, part 3 of the resident wave: the centre
//! lines and the target arrow use the 240-line screen centre y 0x78 (PAL
//! 0x80). Listed in [`REPLACES`](super::REPLACES); the functions keep the
//! generated names.
use crate::hud::tree_62680_a::relative_view_4444c;
use crate::hud::{entities, gameplay};
use crate::machine::{Machine, SmallData};
use crate::{sdk, shared};

/// `[gp+0x500]`: the HUD packet pool descriptor.
const POOL: u32 = 0x500;

/// `[gp+0x504]`: the pool buffer of this frame.
const BUFFER: u32 = 0x504;

/// 800C6D60: HUD state block (+0x08..+0x0A color, +0x0E target slot or
/// 0xFF, +0x14 boresight screen xy, +0x18 boresight depth).
const STATE: u32 = 0x800c_6d60;

/// 800104AC: 16-bit sine table (0x1000 entries per quarter turn, cosine
/// at +0x1000 entries).
const SINE: u32 = 0x8001_04ac;

/// 800C6ED0: target object pointer per target slot.
const TARGETS: u32 = 0x800c_6ed0;

/// 800C67A0: target screen record per target slot (+0x08/+0x0A screen x/y).
const TARGET_SCREEN: u32 = 0x800c_67a0;

/// Line class of the pool: count word +0x14, 0x10-byte `LINE_F2`
/// packets from buffer +0x3D8.
const LINE_COUNT: u32 = 0x14;

const LINE_BASE: u32 = 0x3d8;

/// Quad class of the pool: count word +0x1C, 0x28-byte packets
/// (`POLY_G4`) from buffer +0x878.
const QUAD_COUNT: u32 = 0x1c;

const QUAD_BASE: u32 = 0x878;

/// Pool count word `offset` plus one.
fn bump(m: &mut Machine, pool: u32, offset: u32) {
    let count = m.lw(pool + offset);
    m.sw(pool + offset, count.wrapping_add(1));
}

/// The next free packet of the line class.
fn next_line(m: &mut Machine) -> u32 {
    let pool = m.gw(POOL);
    let count = m.lw(pool + LINE_COUNT);
    let buffer = m.gw(BUFFER);
    buffer.wrapping_add((count << 4).wrapping_add(LINE_BASE))
}

/// The HUD color with each component shifted right by `shift`.
fn dim_color(m: &mut Machine, shift: u32) -> [u8; 3] {
    std::array::from_fn(|i| m.lbu(STATE + 8 + i as u32) >> shift)
}

/// 80064CB4 (JP; PAL 800634E8, near, `imm:1`): append three half-intensity
/// lines at y = 0x78 (PAL 0x80) (x 0..0x3C,
/// 0x104..0x122 and 0x13A..0x140) to the line class. Reads no argument
/// register.
pub fn center_lines_634e8(m: &mut Machine) {
    let mut prim = next_line(m);
    let [r, g, b] = dim_color(m, 1);
    for (index, (left, right)) in [(0, 0x3c), (0x104, 0x122), (0x13a, 0x140)]
        .into_iter()
        .enumerate()
    {
        if index > 0 {
            prim += 0x10;
        }
        m.sb(prim + 4, r);
        m.sb(prim + 5, g);
        m.sb(prim + 6, b);
        let pool = m.gw(POOL);
        m.sh(prim + 8, left);
        m.sh(prim + 0xc, right);
        m.sh(prim + 0xe, 0x78);
        m.sh(prim + 0xa, 0x78);
        bump(m, pool, LINE_COUNT);
    }
}

/// 80065D8C (JP; PAL 800645C0, near, `imm:2`; screen centre y 0x78, PAL
/// 0x80): the direction arrow of the current target (slot byte state
/// +0x0E, 0xFF for none; object `[800C6ED0 + 4 * slot]`). It is drawn
/// for a live object (`[object] & 1`) whose halfword +0x18 is at least
/// 0x400 or whose word +0x20 has bit 0x20000; with that bit the screen
/// record (`[800C67A0 + 4 * slot]`) must exist and lie at least 0x40
/// pixels (SquareRoot0) from the screen centre (0xA0, 0x78). The target
/// position in the player's frame (80044F94) gives the arrow direction
/// (80030D54 of x, y) and length (sine of 80030D54 of the planar
/// distance and z, >> 9, at least 6; base half-width 11 - length / 2, at
/// least 3). The triangle is rotated by the direction about a centre
/// 12 pixels from (0xA0, 0x78) along it (`3 * sin >> 10`, `3 * cos >>
/// 10`) and appended as a Gouraud quad (bright tip, quarter-intensity
/// base) to the quad class.
/// Returns 0 on every path. Reads no argument register.
pub fn target_arrow_645c0(m: &mut Machine) -> u32 {
    const FRAME: u32 = 0x30;
    let frame = m.push_frame(FRAME);
    arrow_645c0(m, frame);
    m.pop_frame(FRAME);
    0
}

/// Body of [`target_arrow_645c0`] in its frame (+0x10..+0x1B the three
/// arrow points, +0x18..+0x23 the target offset VECTOR before that).
fn arrow_645c0(m: &mut Machine, frame: u32) {
    if m.lbu(STATE + 0xe) == 0xff {
        return;
    }
    let slot = u32::from(m.lbu(STATE + 0xe));
    let object = m.lw(TARGETS + slot * 4);
    if object == 0 || m.lw(object) & 1 == 0 {
        return;
    }
    if m.lh(object + 0x18) < 0x400 && m.lw(object + 0x20) & 0x2_0000 == 0 {
        return;
    }
    if m.lw(object + 0x20) & 0x2_0000 != 0 {
        let slot = u32::from(m.lbu(STATE + 0xe));
        let record = m.lw(TARGET_SCREEN + slot * 4);
        if record == 0 {
            return;
        }
        let dx = i32::from(m.lh(record + 8)).wrapping_sub(0xa0);
        let dy = i32::from(m.lh(record + 0xa)).wrapping_sub(0x78);
        let square = dx.wrapping_mul(dx).wrapping_add(dy.wrapping_mul(dy));
        if (sdk::square_root0(m, square as u32) as i32) < 0x40 {
            return;
        }
    }

    let offset = frame + 0x18; // VECTOR
    let slot = u32::from(m.lbu(STATE + 0xe));
    let table = entities(m);
    let gameplay = gameplay(m);
    let player = m.lw(gameplay + 0x28);
    let target = m.lw(table.wrapping_add(slot * 4));
    relative_view_4444c(m, player.wrapping_add(4), target.wrapping_add(8), offset);
    let x = m.lw(offset);
    let y = m.lw(offset + 4);
    let direction = shared::f_80030b10(m, [x, y, 0, 0]);
    let x = m.lw(offset) as i32;
    let y = m.lw(offset + 4) as i32;
    let square = x.wrapping_mul(x).wrapping_add(y.wrapping_mul(y));
    let planar = sdk::square_root0(m, square as u32);
    let z = m.lw(offset + 8);
    let elevation = shared::f_80030b10(m, [planar, z, 0, 0]);

    let points = frame + 0x10; // three (x, y) halfword pairs
    let sine = m.lhu(SINE + (elevation & 0x7ffe));
    m.sh(points, 0);
    let length = i32::from(sine as i16) >> 9;
    m.sh(points + 2, length as u16);
    if length < 6 {
        m.sh(points + 2, 6);
    }
    let width = 0xb_i32.wrapping_sub(length >> 1).max(3);
    m.sh(points + 4, width as u16);
    let angle = direction & 0x3fff;
    m.sh(points + 8, width.wrapping_neg() as u16);
    let sin = i32::from(m.lh(SINE + angle * 2));
    let cos = i32::from(m.lh(SINE + (angle + 0x1000) * 2));
    m.sh(points + 6, 0);
    m.sh(points + 0xa, 0);
    let centre_x = (sin.wrapping_mul(3) >> 10).wrapping_add(0xa0);
    let centre_y = (cos.wrapping_mul(3) >> 10).wrapping_add(0x78);
    let neg_sin = sin.wrapping_neg();
    for index in 0..3 {
        let point = points + index * 4;
        let px = i32::from(m.lh(point));
        let py = i32::from(m.lh(point + 2));
        let rx = cos.wrapping_mul(px).wrapping_add(sin.wrapping_mul(py)) >> 14;
        m.sh(point, rx.wrapping_add(centre_x) as u16);
        let ry = neg_sin.wrapping_mul(px).wrapping_add(cos.wrapping_mul(py)) >> 14;
        m.sh(point + 2, ry.wrapping_add(centre_y) as u16);
    }

    // POLY_G4: v0 = base point 1, v1 = tip (full color), v2 = base
    // point 2, v3 = base point 1 again; base colors at 1/4 intensity.
    let pool = m.gw(POOL);
    let [r, g, b] = dim_color(m, 2);
    let count = m.lw(pool + QUAD_COUNT);
    let buffer = m.gw(BUFFER);
    let prim = buffer.wrapping_add(count.wrapping_mul(0x28).wrapping_add(QUAD_BASE));
    m.sb(prim + 4, r);
    m.sb(prim + 5, g);
    m.sb(prim + 6, b);
    let red = m.lbu(STATE + 8);
    m.sb(prim + 0xc, red);
    let green = m.lbu(STATE + 9);
    m.sb(prim + 0xd, green);
    let blue = m.lbu(STATE + 0xa);
    for vertex in [0x14, 0x1c] {
        m.sb(prim + vertex, r);
        m.sb(prim + vertex + 1, g);
        m.sb(prim + vertex + 2, b);
    }
    m.sb(prim + 0xe, blue);
    let base = m.lw(points + 4);
    m.sw(prim + 8, base);
    let tip = m.lw(points);
    m.sw(prim + 0x10, tip);
    let other = m.lw(points + 8);
    m.sw(prim + 0x20, base);
    m.sw(prim + 0x18, other);
    let pool = m.gw(POOL);
    bump(m, pool, QUAD_COUNT);
}
