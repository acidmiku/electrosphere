//! Overlay 2 aircraft statistics drawing (800D8CB4 and its four children),
//! translated from `analysis/overlays/overlay_002`. The primitives go into
//! the scene's primitive buffer and are linked at the head of one ordering
//! table entry, in original order. See [`crate::ov2_aircraft`] for the
//! selection state and the call conventions.
use crate::machine::Machine;
use crate::ov2_aircraft::{
    BLOCK, Calls, FORMAT_HUNDREDTHS, FORMAT_INTEGER, TEXT, clear_labels, div_magic, set_label,
};

/// 0x800F5FD8: the radar chart's six points (i16 x, y): the centre, then the
/// five statistic axes.
pub const RADAR_AXES: u32 = 0x800f_5fd8;
/// 0x800ACD28: resident `sprintf`.
const SPRINTF: u32 = 0x800a_cd28;

/// `addPrim`: link the primitive at `prim` at the head of OT entry `ot`.
/// The tag's length byte was already stored, so the tag word is read back
/// (the original reads it a few stores earlier; those stores never touch
/// the tag word or the OT entry).
fn link(m: &mut Machine, prim: u32, ot: u32) {
    let tag = m.lw(prim) & 0xff00_0000;
    let head = m.lw(ot) & 0x00ff_ffff;
    m.sw(prim, tag | head);
    let head = m.lw(ot) & 0xff00_0000;
    m.sw(ot, head | (prim & 0x00ff_ffff));
}

/// The opening bytes of a grey (0x40,0x40,0x40) LINE_F2 (length 3, code 0x40).
fn line_header(m: &mut Machine, prim: u32) {
    m.sb(prim + 3, 3);
    m.sb(prim + 7, 0x40);
    m.sb(prim + 4, 0x40);
    m.sb(prim + 5, 0x40);
    m.sb(prim + 6, 0x40);
}

/// 800D83FC: the radar grid. Five lines from the centre to each axis, then
/// the five rim lines between consecutive axes, offset by the aircraft
/// panel's x (`[block]+0x10` + 0xEC) and y 0x134. Returns the next free
/// primitive address.
pub fn radar_lines_800d83fc(m: &mut Machine, prim: u32, ot: u32) -> u32 {
    let mut next = prim;
    let block = m.lw(BLOCK);
    let panel = m.lh(block + 0x10) as i32 as u32;
    let x = panel.wrapping_add(0xec);
    let mut axis = RADAR_AXES + 4;
    for _ in 1..6 {
        let p = next;
        line_header(m, p);
        let v = u32::from(m.lhu(RADAR_AXES)).wrapping_add(x);
        m.sh(p + 8, v as u16);
        next = next.wrapping_add(0x10);
        let v = u32::from(m.lhu(RADAR_AXES + 2)).wrapping_add(0x134);
        m.sh(p + 0xa, v as u16);
        let v = u32::from(m.lhu(axis)).wrapping_add(x);
        m.sh(p + 0xc, v as u16);
        let v = u32::from(m.lhu(axis + 2)).wrapping_add(0x134);
        m.sh(p + 0xe, v as u16);
        axis += 4;
        link(m, p, ot);
    }
    let rim = RADAR_AXES + 4;
    for i in 0..5u32 {
        let p = next;
        let j = i + 1;
        let point = rim + i * 4;
        line_header(m, p);
        let v = u32::from(m.lhu(point)).wrapping_add(x);
        m.sh(p + 8, v as u16);
        let v = u32::from(m.lhu(point + 2)).wrapping_add(0x134);
        next = next.wrapping_add(0x10);
        m.sh(p + 0xa, v as u16);
        // (i + 1) % 5 through the compiler's reciprocal.
        let quotient = div_magic(j, 0x6666_6667, 1, false);
        let wrapped = j.wrapping_sub(quotient.wrapping_mul(5));
        let other = (wrapped << 2).wrapping_add(rim);
        let v = u32::from(m.lhu(other)).wrapping_add(x);
        m.sh(p + 0xc, v as u16);
        let v = u32::from(m.lhu(other + 2)).wrapping_add(0x134);
        m.sh(p + 0xe, v as u16);
        link(m, p, ot);
    }
    next
}

/// One radar coordinate: `axis` * `stat` / 240 (wrapping product, signed
/// reciprocal division) plus `offset`.
fn scaled(m: &mut Machine, axis: u32, stat: u32, offset: u32) -> u32 {
    let product = (m.lh(axis) as i32 as u32).wrapping_mul(stat);
    div_magic(product, 0x8888_8889, 7, true).wrapping_add(offset)
}

/// 800D85B8: the five filled radar triangles (POLY_F3, colour CF 8B 17)
/// between the centre and consecutive statistic points of the current
/// aircraft record (`[block]+0x50`: halfwords +2, +4, +0xA, +6, +8 in axis
/// order). Returns the next free primitive address.
pub fn radar_triangles_800d85b8(m: &mut Machine, prim: u32, ot: u32) -> u32 {
    let block = m.lw(BLOCK);
    let record = block + 0x50;
    let panel = m.lh(block + 0x10) as i32 as u32;
    let mut stats = [0u32; 5];
    for (slot, offset) in stats.iter_mut().zip([2, 4, 0xa, 6, 8]) {
        *slot = m.lh(record + offset) as i32 as u32;
    }
    let x = panel.wrapping_add(0xec);
    let rim = RADAR_AXES + 4;
    let mut next = prim;
    for i in 0..5u32 {
        let j = i + 1;
        let quotient = div_magic(j, 0x6666_6667, 1, false);
        let wrapped = j.wrapping_sub(quotient.wrapping_mul(5));
        let first = stats[i as usize];
        let second = stats[wrapped as usize];
        let p = next;
        m.sb(p + 3, 4);
        m.sb(p + 7, 0x20);
        m.sb(p + 4, 0xcf);
        m.sb(p + 5, 0x8b);
        m.sb(p + 6, 0x17);
        let v = u32::from(m.lhu(RADAR_AXES)).wrapping_add(x);
        m.sh(p + 8, v as u16);
        let v = u32::from(m.lhu(RADAR_AXES + 2)).wrapping_add(0x134);
        m.sh(p + 0xa, v as u16);
        let a = rim + i * 4;
        let v = scaled(m, a, first, x);
        m.sh(p + 0xc, v as u16);
        let v = scaled(m, a + 2, first, 0x134);
        m.sh(p + 0xe, v as u16);
        let b = rim + wrapped * 4;
        let v = scaled(m, b, second, x);
        m.sh(p + 0x10, v as u16);
        let v = scaled(m, b + 2, second, 0x134);
        m.sh(p + 0x12, v as u16);
        link(m, p, ot);
        next = next.wrapping_add(0x14);
    }
    next
}

/// 800D87F8: the five numeric statistics of the current aircraft record.
/// Words +0x10, +0xC, +0x14 print as hundredths ("%2d.%2d", byte 3 of a
/// blank tens digit becomes '0'), +0x18 and +0x1C as "%5d", through the
/// shared text routine 800D04C8 with font 0x17 (0x18 in weapon selection,
/// block flag 0x80000) at the aircraft panel's second offset. Then the
/// label table is cleared and the record's label (+0x21, if non-zero)
/// shown. Returns the next free primitive address (800D04C8's).
pub fn statistics_numbers_800d87f8(m: &mut Machine, c: &mut dyn Calls, prim: u32, ot: u32) -> u32 {
    let frame = m.push_frame(0xa0);
    let text = frame + 0x20;
    let block = m.lw(BLOCK);
    let record = block + 0x50;
    let flags = m.lw(block + 8);
    let panel = m.lh(block + 0x12) as i32 as u32;
    let font = if flags & 0x8_0000 != 0 { 0x18 } else { 0x17 };
    let x = panel.wrapping_add(0x4e);
    let colour = 0x0080_8080;
    let mut next = prim;
    for (offset, y) in [(0x10, 0xf9), (0xc, 0x105), (0x14, 0x111)] {
        let value = m.lw(record + offset);
        let whole = div_magic(value, 0x51eb_851f, 5, false);
        let fraction = value.wrapping_sub(whole.wrapping_mul(100));
        c.call(m, SPRINTF, [text, FORMAT_HUNDREDTHS, whole, fraction]);
        if m.lbu(text + 3) == b' ' {
            m.sb(text + 3, b'0');
        }
        m.sw(frame + 0x14, y);
        m.sw(frame + 0x18, colour);
        m.sw(frame + 0x10, x);
        next = c.call(m, TEXT, [font, text, ot, next]);
    }
    for (offset, x, y) in [(0x18, x, 0x11d), (0x1c, panel.wrapping_add(0x44), 0x129)] {
        let value = m.lw(record + offset);
        c.call(m, SPRINTF, [text, FORMAT_INTEGER, value, 0]);
        m.sw(frame + 0x10, x);
        m.sw(frame + 0x14, y);
        m.sw(frame + 0x18, colour);
        next = c.call(m, TEXT, [font, text, ot, next]);
    }
    clear_labels(m, c);
    let label = m.lbu(record + 0x21);
    if label != 0 {
        set_label(m, c, u32::from(label) + 0xa3, 1);
    }
    m.pop_frame(0xa0);
    next
}

/// One weapon bar: a TILE (code 0x60, colour E5 E5 D5) at (`x`, `y`), 8
/// high, `stat` * 38 / 240 wide.
fn weapon_bar(m: &mut Machine, p: u32, ot: u32, x: u32, y: u32, stat: u8) {
    m.sb(p + 3, 3);
    m.sb(p + 7, 0x60);
    m.sh(p + 8, x as u16);
    m.sh(p + 0xa, y as u16);
    let width = div_magic(u32::from(stat) * 38, 0x8888_8889, 7, true);
    m.sh(p + 0xe, 8);
    m.sb(p + 4, 0xe5);
    m.sb(p + 5, 0xe5);
    m.sb(p + 6, 0xd5);
    m.sh(p + 0xc, width as u16);
    link(m, p, ot);
}

/// 800D8ABC: three bars for the statistics bytes +4..+6 of the selected
/// weapon row's current record (`[block]+0x100 + row*8`), at the weapon
/// panel's x (`[block]+0x16` + 0x10A). Returns the next free primitive.
pub fn weapon_bars_800d8abc(m: &mut Machine, prim: u32, ot: u32) -> u32 {
    let block = m.lw(BLOCK);
    let row = m.lw(block + 0xd8);
    let x = (m.lh(block + 0x16) as i32 as u32).wrapping_add(0x10a);
    let record = block.wrapping_add((row << 3).wrapping_add(0x100));
    let mut p = prim;
    for (index, y) in [0x13a, 0x14d, 0x160].into_iter().enumerate() {
        let stat = m.lbu(record + 4 + index as u32);
        weapon_bar(m, p, ot, x, y, stat);
        p = p.wrapping_add(0x10);
    }
    p
}

/// 800D8CB4: the statistics panel: radar lines, radar triangles, numbers
/// and weapon bars, chained through the primitive pointer it returns.
pub fn draw_statistics_800d8cb4(m: &mut Machine, c: &mut dyn Calls, prim: u32, ot: u32) -> u32 {
    m.push_frame(0x18);
    let p = radar_lines_800d83fc(m, prim, ot);
    let p = radar_triangles_800d85b8(m, p, ot);
    let p = statistics_numbers_800d87f8(m, c, p, ot);
    let p = weapon_bars_800d8abc(m, p, ot);
    m.pop_frame(0x18);
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_prepends_to_ordering_table_entry() {
        let mut m = Machine::default();
        m.sw(0x8010_0000, 0x0012_3456);
        m.sb(0x8011_0003, 3);
        link(&mut m, 0x8011_0000, 0x8010_0000);
        assert_eq!(m.lw(0x8011_0000), 0x0312_3456);
        assert_eq!(m.lw(0x8010_0000), 0x0011_0000);
    }
}
