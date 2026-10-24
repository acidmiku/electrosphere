//! The hub's four rotating black wedges (800D170C, 800D12EC, 800D13E8;
//! their initial state is set by 800D169C, owned by `ov2core`). State
//! `[0x800F539C]`: +0 flags (bit 0: spin direction), +4 four speeds, +0xC four angles, +0x14 four centre x, +0x1C four
//! centre y (halfwords). The wedges are flat quadrilaterals (`POLY_F4`,
//! 0x18 bytes) linked into outer ordering-table bucket 15.
use super::{add_prim, call};
use crate::machine::Machine;

/// `[0x800F539C]`: the wedge state.
pub const WEDGES: u32 = 0x800f_539c;
/// 800F1BA0: the current outer ordering table (owned by `ov2core`).
pub const CURRENT_OT: u32 = 0x800f_1ba0;
/// 800F1908: read visual-script group `a0`'s offset into the halfwords at
/// `a1`/`a2` (owned by `ov2core`).
pub const GROUP_OFFSET: u32 = 0x800f_1908;
/// Resident rsin/rcos (owned by `sdk`).
pub const RSIN: u32 = 0x800a_45c8;
pub const RCOS: u32 = 0x800a_4698;

/// 800D12EC: speed and angle update. The `selected` wedge accelerates to
/// 0x80 (unsigned halfword compare) and turns by its speed in the flag-0
/// direction (set: subtract); the others decelerate (signed), lose 0x20 of
/// angle on reaching speed 5, stop below 5, and turn forward.
pub fn spin_d12ec(m: &mut Machine, selected: u32) {
    let state = m.lw(WEDGES);
    for wedge in 0..4u32 {
        let at = state + wedge * 2;
        if wedge == selected {
            let speed = m.lhu(at + 4);
            if speed < 0x80 {
                m.sh(at + 4, speed.wrapping_add(1));
            }
            let angle = m.lhu(at + 0xc);
            let speed = m.lhu(at + 4);
            let angle = if m.lw(state) & 1 != 0 {
                angle.wrapping_sub(speed)
            } else {
                angle.wrapping_add(speed)
            };
            m.sh(at + 0xc, angle);
        } else {
            let speed = m.lh(at + 4);
            let raw = m.lhu(at + 4);
            if speed > 0 {
                m.sh(at + 4, raw.wrapping_sub(1));
            }
            if m.lh(at + 4) == 5 {
                let angle = m.lhu(at + 0xc);
                m.sh(at + 0xc, angle.wrapping_sub(0x20));
            }
            if m.lh(at + 4) < 5 {
                m.sh(at + 4, 0);
            }
            let angle = m.lhu(at + 0xc);
            let speed = m.lhu(at + 4);
            m.sh(at + 0xc, angle.wrapping_add(speed));
        }
    }
}

/// `x * factor / 4096` truncated toward zero, as compiled (`bgez` + 0xFFF).
fn scaled(value: u32, factor: i32) -> u32 {
    let product = (value as i32).wrapping_mul(factor);
    let biased = if product < 0 {
        product.wrapping_add(0xfff)
    } else {
        product
    };
    (biased >> 12) as u32
}

/// 800D13E8: build the four wedge quadrilaterals at `prim` and link them
/// to `slot`. Radii (22, 40) and (18, 29) at angle +/-0x80 around each
/// centre plus visual group 2's offset. Returns the next free primitive.
pub fn wedges_d13e8(m: &mut Machine, prim: u32, slot: u32) -> u32 {
    let frame = m.push_frame(0x40);
    call(m, GROUP_OFFSET, [2, frame + 0x10, frame + 0x12, 0]);
    let mut prim = prim;
    for wedge in 0..4u32 {
        let p = prim;
        m.sb(p + 3, 5);
        m.sb(p + 7, 0x28);
        m.sb(p + 4, 0);
        m.sb(p + 5, 0);
        m.sb(p + 6, 0);
        let state = m.lw(WEDGES);
        let at = state.wrapping_add(wedge * 2);
        let angle = m.lh(at + 0xc) as i32;
        let cx = m.lh(at + 0x14) as i32 as u32;
        let cy = m.lh(at + 0x1c) as i32 as u32;
        prim = prim.wrapping_add(0x18);
        let low = angle.wrapping_sub(0x80) as u32;
        let high = angle.wrapping_add(0x80) as u32;
        let corners = [
            (RCOS, low, 22, cx, frame + 0x10, 8),
            (RSIN, low, 40, cy, frame + 0x12, 0xa),
            (RCOS, high, 22, cx, frame + 0x10, 0xc),
            (RSIN, high, 40, cy, frame + 0x12, 0xe),
            (RCOS, low, 18, cx, frame + 0x10, 0x10),
            (RSIN, low, 29, cy, frame + 0x12, 0x12),
            (RCOS, high, 18, cx, frame + 0x10, 0x14),
            (RSIN, high, 29, cy, frame + 0x12, 0x16),
        ];
        for (function, angle, factor, centre, offset, field) in corners {
            let value = call(m, function, [angle, 0, 0, 0]);
            let coordinate = scaled(value, factor).wrapping_add(centre);
            let base = u32::from(m.lhu(offset));
            m.sh(p + field, base.wrapping_add(coordinate) as u16);
        }
        add_prim(m, slot, p);
    }
    m.pop_frame(0x40);
    prim
}

/// 800D170C: set the spin direction (bit 0 = `direction == 1`), update
/// the speeds for `selected` and draw the wedges at `prim` into outer
/// bucket 15 of the current ordering table. Returns the next primitive.
pub fn draw_d170c(m: &mut Machine, prim: u32, selected: u32, direction: u32) -> u32 {
    m.push_frame(0x28);
    // 800F1BA0 reads no argument register.
    let ot = call(m, CURRENT_OT, [0; 4]);
    let state = m.lw(WEDGES);
    let flags = m.lw(state);
    m.sw(
        state,
        if direction == 1 {
            flags | 1
        } else {
            flags & !1
        },
    );
    let slot = ot.wrapping_add(0x3c);
    spin_d12ec(m, selected);
    let next = wedges_d13e8(m, prim, slot);
    m.pop_frame(0x28);
    next
}
