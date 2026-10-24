//! JP overlay 2, part 2: the analog stick screens of the options (the
//! stick position test and calibration), translated from the JP
//! instructions.
//!
//! Both draw with the primitive routines of part 5 (800F6E68 lines,
//! 800F711C, 800F7244 and 800F73A4 shapes from a vertex/color record:
//! (record, packet [state +8], ordering table [visual +8])); every call's
//! returned packet pointer goes back to state +8. The context's stick
//! axes are halfwords +0x62..+0x68; the state record keeps their maxima
//! (+0xEC..+0xEF), minima (+0xF0..+0xF3), the phase +0xFC and a vertex
//! record at +0x10A.
use super::ov2_p2::{MODULE, framed, lbu, less, lh, lhu, visual, work};
use super::ov2_p2_hub::STATE;
use crate::common::{CONTEXT, loaded_module};
use crate::dispatch;
use crate::machine::{Machine, SmallData};

/// 8002DB4C (resident, `script`): start sound effect `a0`.
const SOUND: u32 = 0x8002_db4c;
/// Primitive routines (overlay 2, part 5).
const LINE: u32 = 0x800f_6e68;
const QUAD: u32 = 0x800f_711c;
const BAR: u32 = 0x800f_7244;
const TRIANGLE: u32 = 0x800f_73a4;

fn state(m: &mut Machine) -> u32 {
    m.lw(STATE)
}

/// `value * factor / 2^shift` as compiled: the product (shifts and adds),
/// biased toward zero when negative, shifted arithmetically.
fn scale(value: u32, factor: u32, shift: u32) -> u32 {
    let product = value.wrapping_mul(factor) as i32;
    let biased = if product < 0 {
        product.wrapping_add((1 << shift) - 1)
    } else {
        product
    };
    (biased >> shift) as u32
}

/// Draw `record` with `routine` into the current packet and store the new
/// packet pointer (returned). The state is re-read for the packet and the
/// store.
fn draw(m: &mut Machine, routine: u32, record: u32) -> u32 {
    let current = state(m);
    let vis = visual(m);
    let packet = m.lw(current.wrapping_add(8));
    let ot = m.lw(vis.wrapping_add(8));
    let next = dispatch::call(m, routine, [record, packet, ot, 0]);
    let current = state(m);
    m.sw(current.wrapping_add(8), next);
    next
}

/// Store halfwords `values` at `record + 2 * i`.
fn halfwords(m: &mut Machine, record: u32, values: &[u32]) {
    for (i, &value) in values.iter().enumerate() {
        m.sh(record.wrapping_add(2 * i as u32), value as u16);
    }
}

/// 800D91FC: the stick position screen: phase +0xFC 0 waits for the
/// stick pushed right of 0x90 (confirm, sound 0x3C, keeps it at +0xED),
/// phase 1 for it pushed left of 0x70 (+0xF1), phase 2 confirms (the two
/// and the centre into context +0x310..+0x312, leave to page 0); draws the
/// two scales (ten gray lines) and their markers; flag 0x14 = phase +
/// 0x4A. Returns the last drawer's value.
pub fn stick_position(m: &mut Machine) -> u32 {
    framed(m, 0x50, |m, sp| {
        let context = m.gw(CONTEXT);
        let record = state(m);
        let phase = m.lw(record.wrapping_add(0xfc));
        let stick = lh(m, context.wrapping_add(0x64));
        let bars = record.wrapping_add(0x10a);
        let offset = |value: u32| scale(value.wrapping_sub(0x80), 70, 7);
        match phase {
            0 => {
                let (right, left) = if less(stick, 0x80) {
                    (0, offset(stick))
                } else {
                    (offset(stick), 0)
                };
                m.sw(sp + 0x20, right);
                m.sw(sp + 0x24, left);
                let context = m.gw(CONTEXT);
                if m.lw(context.wrapping_add(0x12c)) & 0x40 != 0 && !less(stick, 0x91) {
                    dispatch::call(m, SOUND, [0x3c, 0, 0, 0]);
                    let record = state(m);
                    m.sb(record.wrapping_add(0xed), stick as u8);
                    let record = state(m);
                    m.sw(record.wrapping_add(0xfc), 1);
                }
            }
            1 => {
                let right = offset(lbu(m, record.wrapping_add(0xed)));
                let left = if less(stick, 0x80) { offset(stick) } else { 0 };
                m.sw(sp + 0x20, right);
                m.sw(sp + 0x24, left);
                let context = m.gw(CONTEXT);
                if m.lw(context.wrapping_add(0x12c)) & 0x40 != 0 && less(stick, 0x70) {
                    dispatch::call(m, SOUND, [0x3c, 0, 0, 0]);
                    let record = state(m);
                    m.sb(record.wrapping_add(0xf1), stick as u8);
                    let record = state(m);
                    m.sw(record.wrapping_add(0xfc), 2);
                }
            }
            2 => {
                let right = offset(lbu(m, record.wrapping_add(0xed)));
                let left = offset(lbu(m, record.wrapping_add(0xf1)));
                m.sw(sp + 0x20, right);
                m.sw(sp + 0x24, left);
                if m.lw(context.wrapping_add(0x12c)) & 0x40 != 0 {
                    dispatch::call(m, SOUND, [0x3c, 0, 0, 0]);
                    let record = state(m);
                    let context = m.gw(CONTEXT);
                    let values = context.wrapping_add(0x30c);
                    let low = lbu(m, record.wrapping_add(0xf1));
                    m.sb(values.wrapping_add(4), low as u8);
                    let context = m.gw(CONTEXT);
                    let centre = lbu(m, context.wrapping_add(0x64));
                    m.sb(values.wrapping_add(5), centre as u8);
                    let record = state(m);
                    let high = lbu(m, record.wrapping_add(0xed));
                    m.sb(values.wrapping_add(6), high as u8);
                    let record = state(m);
                    m.sb(record, 0);
                    let record = state(m);
                    m.sb(record.wrapping_add(1), 0);
                    super::ov2_p2::reset_cell(m, 2, 0);
                    super::ov2_p2::set_cell_preset(m, 0, 1);
                }
            }
            _ => {}
        }
        let record = state(m);
        let block = work(m);
        let step = lbu(m, record.wrapping_add(0xfc)).wrapping_add(0x4a);
        let flags = m.lw(block.wrapping_add(0x5c));
        m.sb(flags.wrapping_add(0x14), step as u8);
        for at in [0x18, 0x19, 0x1a] {
            m.sb(sp + at, 0x80);
        }
        for line in [
            [0x82, 0x28, 0x96, 0x28],
            [0x82, 0x6e, 0x96, 0x6e],
            [0x82, 0xb4, 0x96, 0xb4],
            [0x82, 0x28, 0x82, 0xb4],
            [0x96, 0x28, 0x96, 0xb4],
            [0xaa, 0x28, 0xbe, 0x28],
            [0xaa, 0x6e, 0xbe, 0x6e],
            [0xaa, 0xb4, 0xbe, 0xb4],
            [0xaa, 0x28, 0xaa, 0xb4],
            [0xbe, 0x28, 0xbe, 0xb4],
        ] {
            halfwords(m, sp + 0x10, &line);
            draw(m, LINE, sp + 0x10);
        }
        halfwords(m, bars, &[0x82, 0x6e, 0x96, 0x6e, 0x82]);
        let right = m.lw(sp + 0x20);
        m.sh(bars.wrapping_add(0xc), 0x96);
        for (at, value) in [(0x18, 0x40), (0x1a, 0), (0x1c, 0x40), (0x1e, 0)] {
            m.sb(bars.wrapping_add(at), value);
        }
        let y = right.wrapping_add(0x6e);
        m.sh(bars.wrapping_add(0xa), y as u16);
        m.sh(bars.wrapping_add(0xe), y as u16);
        m.sb(bars.wrapping_add(0x19), 0x37);
        m.sb(bars.wrapping_add(0x1d), 0x37);
        draw(m, BAR, bars);
        halfwords(m, bars, &[0x82, 0x6e, 0x96, 0x6e, 0x82]);
        let left = m.lw(sp + 0x24);
        m.sh(bars.wrapping_add(0xc), 0x96);
        let y = left.wrapping_add(0x6e);
        m.sh(bars.wrapping_add(0xa), y as u16);
        m.sh(bars.wrapping_add(0xe), y as u16);
        draw(m, BAR, bars);
        for value in [left, right] {
            halfwords(m, bars, &[0xaa, 0x6e, 0xbe, 0x6e, 0xaa]);
            let y = 0u32.wrapping_sub(value).wrapping_add(0x6e);
            m.sh(bars.wrapping_add(0xa), y as u16);
            m.sh(bars.wrapping_add(0xe), y as u16);
            m.sh(bars.wrapping_add(0xc), 0xbe);
            draw(m, BAR, bars);
        }
        let context = m.gw(CONTEXT);
        let marker = scale(lh(m, context.wrapping_add(0x64)).wrapping_sub(0x80), 70, 7);
        let mut last = 0;
        for (x, ys, xs) in [
            (0x82u32, [0x6e, 0x64, 0x78], 0x78u32),
            (0xbe, [0x6e, 0x64, 0x78], 0xc8),
        ] {
            let sign = if x == 0x82 { 1u32 } else { u32::MAX };
            for (i, base) in ys.into_iter().enumerate() {
                let y = (base as u32).wrapping_add(marker.wrapping_mul(sign));
                m.sh(bars.wrapping_add(2 + 4 * i as u32), y as u16);
            }
            m.sh(bars, x as u16);
            m.sh(bars.wrapping_add(4), xs as u16);
            m.sh(bars.wrapping_add(8), xs as u16);
            last = draw(m, TRIANGLE, bars);
        }
        last
    })
}

/// 800D9994: the stick calibration screen: draw both sticks' crosses at
/// their positions (context +0x62..+0x68), track each axis's minimum and
/// maximum, draw the eight range markers and four frames, and step the
/// phase +0xFC once the ranges reach the corners (minima below 0x40,
/// maxima above 0xC0); phase 4 confirms (sound 0x3C; minima, centres and
/// maxima into context +0x2B0.., leave to page 0). Flag 0x14 = phase +
/// 0x45. Returns it.
pub fn stick_calibration(m: &mut Machine) -> u32 {
    framed(m, 0x50, |m, sp| {
        let first = state(m);
        let record = first.wrapping_add(0x10a);
        for at in [
            0x10, 0x11, 0x12, 0x14, 0x15, 0x16, 0x18, 0x19, 0x1a, 0x1c, 0x1d, 0x1e,
        ] {
            m.sb(record + at, 0x80);
        }
        for (vertical, horizontal, x) in [(0x68, 0x66, 0x69u32), (0x64, 0x62, 0xd7)] {
            let context = m.gw(CONTEXT);
            let dx = scale(
                lh(m, context.wrapping_add(vertical)).wrapping_sub(0x80),
                35,
                7,
            );
            let dy = scale(
                lh(m, context.wrapping_add(horizontal)).wrapping_sub(0x80),
                70,
                7,
            );
            let left = dx.wrapping_add(x);
            m.sh(first.wrapping_add(0x10a), left as u16);
            m.sh(record.wrapping_add(2), dy.wrapping_add(0x68) as u16);
            m.sh(record.wrapping_add(4), dx.wrapping_add(x + 3) as u16);
            m.sh(record.wrapping_add(8), dx.wrapping_add(x - 3) as u16);
            let middle = dy.wrapping_add(0x6e);
            if x == 0x69 {
                m.sh(record.wrapping_add(0xe), dy.wrapping_add(0x74) as u16);
                m.sh(record.wrapping_add(6), middle as u16);
                m.sh(record.wrapping_add(0xa), middle as u16);
                m.sh(record.wrapping_add(0xc), left as u16);
            } else {
                m.sh(record.wrapping_add(6), middle as u16);
                m.sh(record.wrapping_add(0xa), middle as u16);
                m.sh(record.wrapping_add(0xc), left as u16);
                m.sh(record.wrapping_add(0xe), dy.wrapping_add(0x74) as u16);
            }
            draw(m, BAR, record);
        }
        for axis in 0..4u32 {
            let context = m.gw(CONTEXT);
            let current = state(m);
            let at = (axis << 1).wrapping_add(context);
            let entry = current.wrapping_add(axis);
            let value = lh(m, at.wrapping_add(0x62));
            let minimum = lbu(m, entry.wrapping_add(0xf0));
            let raw = lhu(m, at.wrapping_add(0x62));
            if less(value, minimum) {
                m.sb(entry.wrapping_add(0xf0), raw as u8);
            }
            let current = state(m);
            let entry = current.wrapping_add(axis);
            if less(lbu(m, entry.wrapping_add(0xec)), value) {
                m.sb(entry.wrapping_add(0xec), raw as u8);
            }
        }
        for (at, value) in [
            (0x10, 0xf0),
            (0x11, 0x6e),
            (0x12, 0),
            (0x14, 0x40),
            (0x15, 0x37),
            (0x16, 0),
            (0x18, 0x40),
            (0x19, 0x37),
            (0x1a, 0),
        ] {
            m.sb(record + at, value);
        }
        for (x, top, side, base) in [
            (0x69u32, 0xf2u32, 0xefu32, 0x46u32),
            (0x69, 0xee, 0xef, 0x46),
            (0x69, 0xee, 0xf3, 0x46),
            (0x69, 0xf2, 0xf3, 0x46),
            (0xd7, 0xf0, 0xed, 0xb4),
            (0xd7, 0xec, 0xed, 0xb4),
            (0xd7, 0xec, 0xf1, 0xb4),
            (0xd7, 0xf0, 0xf1, 0xb4),
        ] {
            let current = state(m);
            halfwords(m, record, &[x, 0x6e, x]);
            let y = scale(lbu(m, current.wrapping_add(top)), 140, 8).wrapping_add(0x28);
            m.sh(record.wrapping_add(6), y as u16);
            let value = scale(lbu(m, current.wrapping_add(side)), 70, 8).wrapping_add(base);
            m.sh(record.wrapping_add(8), value as u16);
            m.sh(record.wrapping_add(0xa), 0x6e);
            draw(m, TRIANGLE, record);
        }
        for at in [0x20, 0x21, 0x22] {
            m.sb(sp + at, 0x80);
        }
        m.sb(sp + 0x23, 0);
        m.sb(sp + 0x24, 0);
        for frame in [
            [0x69, 0x28, 0x8c, 0x6e, 0x69, 0xb4, 0x46, 0x6e],
            [0x69, 0xb4, 0x69, 0x28, 0x46, 0x6e, 0x8c, 0x6e],
            [0xd7, 0x28, 0xfa, 0x6e, 0xd7, 0xb4, 0xb4, 0x6e],
            [0xd7, 0xb4, 0xd7, 0x28, 0xb4, 0x6e, 0xfa, 0x6e],
        ] {
            halfwords(m, sp + 0x10, &frame);
            let current = state(m);
            let vis = visual(m);
            let packet = m.lw(current.wrapping_add(8));
            let ot = m.lw(vis.wrapping_add(8));
            let next = dispatch::call(m, QUAD, [sp + 0x10, packet, ot, 0]);
            let current = state(m);
            if frame[0] == 0xd7 && frame[1] == 0xb4 {
                let phase = m.lw(current.wrapping_add(0xfc));
                m.sw(current.wrapping_add(8), next);
                step_calibration(m, phase);
            } else {
                m.sw(current.wrapping_add(8), next);
            }
        }
        let record = state(m);
        let block = work(m);
        let phase = lbu(m, record.wrapping_add(0xfc)).wrapping_add(0x45);
        let flags = m.lw(block.wrapping_add(0x5c));
        m.sb(flags.wrapping_add(0x14), phase as u8);
        phase
    })
}

/// The phase step of [`stick_calibration`] (the original's jump table at
/// 0x800CC7EC, phases 0..4).
fn step_calibration(m: &mut Machine, phase: u32) {
    let record = state(m);
    let corner = |m: &mut Machine, a: u32, b: u32, low: bool| {
        let first = lbu(m, record.wrapping_add(a));
        let reached = |value: u32| if low { value < 0x40 } else { value >= 0xc1 };
        reached(first) && reached(lbu(m, record.wrapping_add(b)))
    };
    let next = match phase {
        0 => corner(m, 0xf0, 0xf2, true).then_some(1),
        1 => corner(m, 0xec, 0xee, false).then_some(2),
        2 => corner(m, 0xf1, 0xf3, true).then_some(3),
        3 => corner(m, 0xed, 0xef, false).then_some(4),
        4 => {
            let context = m.gw(CONTEXT);
            if m.lw(context.wrapping_add(0x12c)) & 0x40 != 0 {
                dispatch::call(m, SOUND, [0x3c, 0, 0, 0]);
                let context = m.gw(CONTEXT);
                let mut out = context.wrapping_add(0x2b0);
                for axis in 0..4u32 {
                    let record = state(m);
                    let minimum = lbu(m, record.wrapping_add(axis).wrapping_add(0xf0));
                    m.sb(out, minimum as u8);
                    let context = m.gw(CONTEXT);
                    let centre = lbu(m, (axis << 1).wrapping_add(context).wrapping_add(0x62));
                    m.sb(out.wrapping_add(1), centre as u8);
                    let record = state(m);
                    let maximum = lbu(m, record.wrapping_add(axis).wrapping_add(0xec));
                    m.sb(out.wrapping_add(2), maximum as u8);
                    out = out.wrapping_add(4);
                }
                let record = state(m);
                m.sb(record, 0);
                let record = state(m);
                m.sb(record.wrapping_add(1), 0);
                super::ov2_p2::reset_cell(m, 2, 0);
                super::ov2_p2::set_cell_preset(m, 0, 1);
            }
            None
        }
        _ => None,
    };
    if let Some(next) = next {
        m.sw(record.wrapping_add(0xfc), next);
    }
}

// ---- dispatch -------------------------------------------------------------------------------------

/// The routines of this file by JP address, while module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    let _ = args;
    Some(match target {
        0x800d_91fc => stick_position(m),
        0x800d_9994 => stick_calibration(m),
        _ => return None,
    })
}
