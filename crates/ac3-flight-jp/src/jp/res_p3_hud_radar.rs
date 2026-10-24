//! Patches of the `hud` radar group (generated `hud::tree_3bdf8_a`,
//! `hud::tree_3bdf8_b` and the gauge composition `hud::compose`
//! `gauges_3bdf8`) whose JP instructions differ from PAL, part 3 of the
//! resident wave. On the 240-line screen the radar is 2 lines lower
//! (centre y 0x2E, PAL 0x2C) and the map window 8 lines higher (y 0x38, PAL
//! 0x40); the context byte +0x95E moved to +0x95A and the mission is the
//! halfword context +0x38C. Each is listed in
//! [`REPLACES`](super::REPLACES); the functions keep the generated names.
use crate::GP;
use crate::hud::tree_3bdf8_b::{blip_sprite_3d838, map_position_3e44c};
use crate::hud::{self, context, entities, gameplay};
use crate::machine::{Machine, SmallData};

/// `[gp+0x434]` (800BCFA8): the HUD primitive block. +0/+4 the primitive
/// area of each display buffer; +8 glyph sprite count (area+0x18, 0x14
/// bytes each); +0xC count of 0x10-byte packets at area+0x518; +0x10
/// tile count (area+0x918, 0x10 bytes each); +0x14 count of 0x18-byte
/// packets at area+0x998; +0x18 optional group flags (bit 0, bit 1).
const HUD_BLOCK: u32 = 0x434;

/// `[gp+0x438]` (800BCFAC): this frame's HUD primitive area.
const HUD_AREA: u32 = 0x438;

/// 800C6D60: HUD state: +8..+0xA frame tile color, +0xB..+0xD range-scale
/// sprite color, +0xE locked target entity (0xFF none), +0x1C radar range.
const HUD_STATE: u32 = 0x800c_6d60;

/// 800C6D60: radar state (+0x0E highlighted entity slot, +0x1C range).
const RADAR: u32 = 0x800c_6d60;

/// 800C6D7C: radar range (`RADAR + 0x1C`) in view position units; it
/// maps to the 32-pixel radar radius.
const RADAR_RANGE: u32 = 0x800c_6d7c;

/// 800C6A60: 64 entity blip records of 12 bytes (slot `i` at `+12 * i`):
/// +0 flags (1 shown, 4 highlighted, 8 range above 0x7FFF, 0x10
/// brightness valid), +1 shape, +2 color, +3 brightness, +4 heading
/// sector, +5 altitude class (or size), +6/+7 radar x/y, +8 the range
/// up to which the entity is shown.
const BLIPS: u32 = 0x800c_6a60;

/// 800C68E0: 32 ground-object blip records (layout of [`BLIPS`]).
const GROUND_BLIPS: u32 = 0x800c_68e0;

/// 800C6ED0: per entity slot, the object whose state sets the blip
/// brightness (+0 flags, +4 level word, +0x20 flags), or 0.
const BLIP_SOURCES: u32 = 0x800c_6ed0;

/// 800C6FD0: per entity slot radar flags (1 and 0x2000: shown; 0x100:
/// drawn in the first map-dot pass).
const SLOT_FLAGS: u32 = 0x800c_6fd0;

/// 800BD080: pointer to the ground object block (+0x1548 live mask,
/// objects of 0xA4 bytes from +0xC8 with x/z words at +0x1C/+0x24).
const GROUND_OBJECTS: u32 = 0x800b_d080;

/// 80023DEC: six words 8003DE5C copies into its frame for 8003E274.
const MARKER_ARGS: u32 = 0x8002_3dec;

/// 800104AC: sine halfwords, 0x1000 = 1.0, 0x4000 steps per turn; the
/// cosine of step `a` is entry `a + 0x1000`.
const SINE: u32 = 0x8001_04ac;

/// `[gp+0x434]`: HUD primitive counters (+8 sprites, +0xC tiles used).
const COUNTERS: u32 = 0x434;

/// `[gp+0x438]`: HUD primitive block.
const PRIMITIVES: u32 = 0x438;

/// `[gp+0x43C]`: radar view (+0 x, +4 z, +8/+0xC sine and cosine of
/// the view rotation).
const VIEW: u32 = 0x43c;

/// `gp+0xFC`, `0x1EC`, `0x1F4`, `0x1FC`: per map tile (7) u, v, width
/// and height bytes.
const MAP_TILES: [u32; 4] = [0xfc, 0x104, 0x10c, 0x114];

/// MIPS `div` quotient: division by zero gives -1 (1 for a negative
/// dividend), `i32::MIN / -1` gives `i32::MIN`.
fn div(dividend: i32, divisor: i32) -> i32 {
    match divisor {
        0 if dividend < 0 => 1,
        0 => -1,
        _ => dividend.wrapping_div(divisor),
    }
}

/// Radar pixel offset of the view-relative offset (`dx`, `dy`): rotated
/// by (`sin`, `cos`), then `((v >> 12) << 5) / range`.
fn radar_point(dx: i32, dy: i32, sin: i32, cos: i32, range: i32) -> (i32, i32) {
    let x = dx.wrapping_mul(cos).wrapping_add(dy.wrapping_mul(sin));
    let y = dx
        .wrapping_neg()
        .wrapping_mul(sin)
        .wrapping_add(dy.wrapping_mul(cos));
    (div((x >> 12) << 5, range), div((y >> 12) << 5, range))
}

/// Within the radar disc's bounding square: -0x1F..=0x1F
/// (`sltiu (v + 0x1F), 0x3F`).
fn on_radar(offset: i32) -> bool {
    (offset.wrapping_add(0x1f) as u32) < 0x3f
}

/// Map cell 0..=0xFF of a world coordinate (`(v >> 23) + 0x80`).
fn map_cell(coordinate: i32) -> i32 {
    ((coordinate >> 23) + 0x80).clamp(0, 0xff)
}

/// The JP mission number: the halfword at context +0x38C (PAL reads the
/// byte 0x800BAEB4).
fn jp_mission(m: &mut Machine) -> i32 {
    let ctx = context(m);
    i32::from(m.lh(ctx + 0x38c))
}

/// 8003D458 (JP; PAL 8003CCA0, near, `imm:3 ins:1 reg:1`): append two frame
/// tiles (`TILE`, color `[800C6D68..32]` / 2) at (0, 0x2D) 5x0x2D and
/// (0x56, 0xE) 0x140x0xE (PAL y and height 0x2B and 0xC) to the HUD area's
/// tile group, color the range-scale sprites at area+0xC0C (full) and
/// +0xC20 (half) from `[800C6D6B..35]`, and select the latter's u (0x18,
/// 0xC, 0) by the radar range (<= 0x4000, <= 0x8000, larger).
pub fn frame_tiles_3cca0(m: &mut Machine) {
    let hud = m.gw(HUD_BLOCK);
    let area = m.gw(HUD_AREA);
    let count = m.lw(hud.wrapping_add(0x10));
    let tile = area.wrapping_add((count << 4).wrapping_add(0x918));
    let red = m.lbu(HUD_STATE + 8);
    m.sb(tile.wrapping_add(4), red >> 1);
    let green = m.lbu(HUD_STATE + 9);
    m.sb(tile.wrapping_add(5), green >> 1);
    let blue = m.lbu(HUD_STATE + 0xa);
    m.sh(tile.wrapping_add(8), 0);
    m.sh(tile.wrapping_add(0xa), 0x2d);
    m.sh(tile.wrapping_add(0xe), 0x2d);
    m.sb(tile.wrapping_add(6), blue >> 1);
    let hud = m.gw(HUD_BLOCK);
    m.sh(tile.wrapping_add(0xc), 5);
    let count = m.lw(hud.wrapping_add(0x10));
    m.sw(hud.wrapping_add(0x10), count.wrapping_add(1));

    let tile = tile.wrapping_add(0x10);
    let red = m.lbu(HUD_STATE + 8);
    m.sb(tile.wrapping_add(4), red >> 1);
    let green = m.lbu(HUD_STATE + 9);
    m.sb(tile.wrapping_add(5), green >> 1);
    let blue = m.lbu(HUD_STATE + 0xa);
    m.sh(tile.wrapping_add(8), 0x56);
    m.sb(tile.wrapping_add(6), blue >> 1);
    let hud = m.gw(HUD_BLOCK);
    m.sh(tile.wrapping_add(0xa), 0xe);
    m.sh(tile.wrapping_add(0xc), 0x140);
    m.sh(tile.wrapping_add(0xe), 0xe);
    let count = m.lw(hud.wrapping_add(0x10));
    m.sw(hud.wrapping_add(0x10), count.wrapping_add(1));

    let area = m.gw(HUD_AREA);
    let sprite = area.wrapping_add(0xc0c);
    for i in 0..3 {
        let component = m.lbu(HUD_STATE + 0xb + i);
        m.sb(sprite.wrapping_add(4 + i), component);
    }
    let area = m.gw(HUD_AREA);
    let sprite = area.wrapping_add(0xc20);
    for i in 0..3 {
        let component = m.lbu(HUD_STATE + 0xb + i);
        m.sb(sprite.wrapping_add(4 + i), component >> 1);
    }
    let range = m.lw(HUD_STATE + 0x1c) as i32;
    let u = if range < 0x4001 {
        0x18
    } else if range > 0x8000 {
        0
    } else {
        0xc
    };
    m.sb(sprite.wrapping_add(0xc), u);
}

/// 8003D5C0 (JP; PAL 8003CE04, near, `imm:4`): map the radar terrain texture. The square of
/// [`RADAR_RANGE`] around the view, widened to whole 0x1000 cells and
/// clamped to the texture, becomes the POLY_FT4 at `[gp+0x438]+0xAA4`:
/// corners rotated into radar pixels around (0x2C, 0x2E) (PAL y 0x2C), texels
/// `(corner >> 12) + 0x40`.
pub fn radar_map_3ce04(m: &mut Machine) {
    let view = m.gw(VIEW);
    let range = m.lw(RADAR_RANGE) as i32;
    let x = m.lw(view) as i32;
    let z = m.lw(view + 4) as i32;
    let cell = |v: i32| (v >> 12) << 12;
    let left = cell(x.wrapping_sub(range)).clamp(-0x4_0000, 0x3_ffff);
    let right = cell(x.wrapping_add(range))
        .wrapping_add(0x1000)
        .clamp(-0x4_0000, 0x3_ffff);
    let top = cell(z.wrapping_add(range))
        .wrapping_add(0x1000)
        .clamp(-0x3_ffff, 0x4_0000);
    let bottom = cell(z.wrapping_sub(range)).clamp(-0x3_ffff, 0x4_0000);
    let cos = m.lw(view + 0xc) as i32;
    let sin = m.lw(view + 8) as i32;
    let dx = [left.wrapping_sub(x), right.wrapping_sub(x)];
    let dy = [z.wrapping_sub(top), z.wrapping_sub(bottom)];
    let quad = m.gw(PRIMITIVES).wrapping_add(0xaa4);
    let corners = [
        (dx[0], dy[0]),
        (dx[1], dy[0]),
        (dx[0], dy[1]),
        (dx[1], dy[1]),
    ];
    for (corner, (dx, dy)) in (0u32..).zip(corners) {
        let (px, py) = radar_point(dx, dy, sin, cos, range);
        let xy = quad + 8 + corner * 8;
        m.sh(xy, px.wrapping_add(0x2c) as u16);
        m.sh(xy + 2, py.wrapping_add(0x2e) as u16);
    }
    // Texels: u from the x edges, v from the z edges (view reloaded).
    let u = ((dx[0].wrapping_add(m.lw(view) as i32) >> 12) + 0x40) as u8;
    m.sb(quad + 0x1c, u);
    m.sb(quad + 0xc, u);
    let view = m.gw(VIEW);
    let u = ((dx[1].wrapping_add(m.lw(view) as i32) >> 12) + 0x40) as u8;
    m.sb(quad + 0x24, u);
    m.sb(quad + 0x14, u);
    let view = m.gw(VIEW);
    let v = ((dy[0].wrapping_sub(m.lw(view + 4) as i32) >> 12) + 0x40) as u8;
    m.sb(quad + 0x15, v);
    m.sb(quad + 0xd, v);
    let view = m.gw(VIEW);
    let v = ((dy[1].wrapping_sub(m.lw(view + 4) as i32) >> 12) + 0x40) as u8;
    m.sb(quad + 0x25, v);
    m.sb(quad + 0x1d, v);
}

/// 8003DA08 (JP; PAL 8003D24C, near, `imm:1`; radar y centre 0x2E, PAL
/// 0x2C): entity blip records of slots 2..63. Each record keeps its
/// high flag bits; a live entity (+0 bit 0x4000) shown on the radar
/// ([`SLOT_FLAGS`] bits 1 and 0x2000) within its range word and inside
/// the radar square gets its radar position, the visible flag (plus 8
/// above range 0x7FFF, 4 when highlighted), altitude class against the
/// player (0 below, 1 level, 2 above, by 0x1000), heading sector and,
/// with flag 0x10 and a source object, the brightness 0x13..=0x83.
pub fn entity_blips_3d24c(m: &mut Machine) {
    let range = m.lw(RADAR + 0x1c) as i32;
    let long_range = 0x7fff < range;
    let table = entities(m);
    let player = m.lw(table);
    for slot in 2..64u32 {
        let record = BLIPS + slot * 12;
        let table = entities(m);
        let flags = m.lbu(record);
        let entity = m.lw(table.wrapping_add(slot * 4));
        m.sb(record, flags & 0xf0);
        if m.lw(entity) & 0x4000 == 0 {
            continue;
        }
        let shown = m.lw(SLOT_FLAGS + slot * 4);
        if shown & 1 == 0 || shown & 0x2000 == 0 {
            continue;
        }
        if range < m.lw(record + 8) as i32 {
            continue;
        }
        let ex = (m.lw(entity + 8) as i32) >> 12;
        let view = m.gw(VIEW);
        let vx = m.lw(view) as i32;
        let cos = m.lw(view + 0xc) as i32;
        let ez = (m.lw(entity + 0x10) as i32) >> 12;
        let vz = m.lw(view + 4) as i32;
        let sin = m.lw(view + 8) as i32;
        let (x, y) = radar_point(ex.wrapping_sub(vx), vz.wrapping_sub(ez), sin, cos, range);
        if !on_radar(x) || !on_radar(y) {
            continue;
        }
        let flags = m.lbu(record);
        m.sb(record + 6, (x + 0x2c) as u8);
        m.sb(record + 7, (y + 0x2e) as u8);
        m.sb(record, flags | 1);
        if long_range {
            m.sb(record, flags | 9);
        }
        if u32::from(m.lbu(RADAR + 0xe)) == slot {
            let flags = m.lbu(record);
            m.sb(record, flags | 4);
        }
        let entity_y = m.lw(entity + 0xc);
        let player_y = m.lw(player + 0xc);
        let height = (player_y.wrapping_sub(entity_y) as i32) >> 12;
        let class = if height > 0x1000 {
            2
        } else if height < -0x1000 {
            0
        } else {
            1
        };
        m.sb(record + 5, class);
        let heading = i32::from(m.lh(entity + 0x1a)) - i32::from(m.lh(player + 0x1a));
        let sector = ((heading + 0x200) & 0x3fff) >> 10;
        let has_brightness = m.lbu(record) & 0x10 != 0;
        m.sb(record + 4, sector as u8);
        if !has_brightness {
            continue;
        }
        let source = m.lw(BLIP_SOURCES + slot * 4);
        if source == 0 {
            continue;
        }
        let brightness = if m.lw(source) & 2 != 0 || m.lw(source + 0x20) & 0x2_0000 != 0 {
            0x83
        } else {
            let level = (m.lw(source + 4).wrapping_add(0xffff_8000) as i32) >> 6;
            0x83i32.wrapping_sub(level).clamp(0x13, 0x83)
        };
        m.sb(record + 3, brightness as u8);
    }
}

/// 8003DCF0 (JP; PAL 8003D534, near, `imm:1`; radar y centre 0x2E, PAL
/// 0x2C): ground-object blip records 0..31: cleared, then for each
/// live object (mask bit `i`) inside the radar square the radar
/// position, shape 2, color 0 and the visible flag.
pub fn ground_blips_3d534(m: &mut Machine) {
    let range = m.lw(RADAR_RANGE) as i32;
    for index in 0..32u32 {
        let record = GROUND_BLIPS + index * 12;
        m.sb(record, 0);
        let objects = m.lw(GROUND_OBJECTS);
        let live = m.lw(objects.wrapping_add(0x1548));
        if !live & (1 << index) != 0 {
            continue;
        }
        let object = objects.wrapping_add(0xc8 + index * 0xa4);
        let ox = (m.lw(object + 0x1c) as i32) >> 12;
        let view = m.gw(VIEW);
        let vx = m.lw(view) as i32;
        let cos = m.lw(view + 0xc) as i32;
        let oz = (m.lw(object + 0x24) as i32) >> 12;
        let vz = m.lw(view + 4) as i32;
        let sin = m.lw(view + 8) as i32;
        let (x, y) = radar_point(ox.wrapping_sub(vx), vz.wrapping_sub(oz), sin, cos, range);
        if !on_radar(x) || !on_radar(y) {
            continue;
        }
        m.sb(record + 7, (y + 0x2e) as u8);
        let flags = m.lbu(record);
        m.sb(record + 6, (x + 0x2c) as u8);
        m.sb(record + 1, 2);
        m.sb(record + 2, 0);
        m.sb(record, flags | 1);
    }
}

/// 8003DE5C (JP; PAL 8003D6A0, near, `ins:1 op:2`; the mission is the
/// halfword context +0x38C, PAL the byte 0x800BAEB4): draw the radar blips: entity records with color 0, then
/// the other entity records (slots shown by [`SLOT_FLAGS`] bit 0x2000),
/// then at range 0x8000 or less all ground records; in missions 0x12
/// and 0x25 with gameplay +8 == 1 also the map marker 8003E274
/// ([`hud::extras::radar_marker_3dab4`]).
pub fn draw_blips_3d6a0(m: &mut Machine) {
    let frame = m.push_frame(0x38);
    for first_pass in [true, false] {
        for slot in 2..64u32 {
            let record = BLIPS + slot * 12;
            if m.lw(SLOT_FLAGS + slot * 4) & 0x2000 == 0 {
                continue;
            }
            if (m.lbu(record + 2) == 0) == first_pass {
                blip_sprite_3d838(m, record);
            }
        }
    }
    if m.lw(RADAR_RANGE) as i32 <= 0x8000 {
        for index in 0..32 {
            blip_sprite_3d838(m, GROUND_BLIPS + index * 12);
        }
    }
    let mission = jp_mission(m);
    if mission == 0x12 || mission == 0x25 {
        let state = gameplay(m);
        if m.lw(state + 8) == 1 {
            for word in 0..6 {
                let value = m.lw(MARKER_ARGS + word * 4);
                m.sw(frame + 0x10 + word * 4, value);
            }
            hud::extras::radar_marker_3dab4(m, frame + 0x10);
        }
    }
    m.pop_frame(0x38);
}

/// 8003E69C (JP; PAL 8003DEDC, near, `imm:4`): draw the 128x128 map window
/// at (0x60, 0x38) (PAL y 0x40). The scroll
/// origin follows the player's map cell; the seven map tiles get their
/// texels, the player arrow (sprite at `[gp+0x438]+0xFFC`, cell by
/// heading) and two heading wedges (POLY_F4 points at +0xFD4 and
/// +0xFEC) are placed, then one 2x2 tile per shown entity in the map
/// window: first the [`SLOT_FLAGS`] 0x100 slots (red, drawn at 1,1
/// less), then the others (blue for entity flag 0x40, yellow for 0x100,
/// else grey, semi-transparent).
pub fn map_window_3dedc(m: &mut Machine) {
    let frame = m.push_frame(0x40);
    let point = frame + 0x10;
    let origin = frame + 0x18;
    let table = entities(m);
    let player = m.lw(table);
    let x = map_cell(m.lw(player + 8) as i32);
    let z = map_cell((m.lw(player + 0x10) as i32).wrapping_neg());
    let scroll = |cell: i32| match cell {
        ..0x40 => 0,
        0x40..0xc1 => cell - 0x40,
        _ => 0x80,
    };
    m.sh(origin, scroll(x) as u16);
    m.sh(origin + 2, scroll(z) as u16);
    // `(short)v / 2` as compiled: (v + sign) >> 1 (logical).
    let half = |v: u16| (i32::from(v as i16) + i32::from(v >> 15)) as u32 >> 1;
    let half_x = half(m.lhu(origin));
    let half_z = half(m.lhu(origin + 2));
    for tile in 0..7u32 {
        let poly = m.gw(PRIMITIVES).wrapping_add(0xcd4 + tile * 40);
        let [u, v, w, h] = MAP_TILES.map(|offset| GP + offset + tile);
        let left = u32::from(m.lbu(u)).wrapping_add(half_x);
        m.sb(poly + 0xc, left as u8);
        let top = u32::from(m.lbu(v)).wrapping_add(half_z);
        m.sb(poly + 0xd, top as u8);
        let right = u32::from(m.lbu(w)).wrapping_add(left);
        m.sb(poly + 0x14, right as u8);
        m.sb(poly + 0x15, top as u8);
        m.sb(poly + 0x1c, left as u8);
        let bottom = u32::from(m.lbu(h)).wrapping_add(top);
        m.sb(poly + 0x1d, bottom as u8);
        m.sb(poly + 0x24, right as u8);
        m.sb(poly + 0x25, bottom as u8);
    }
    // The player arrow.
    let arrow = m.gw(PRIMITIVES).wrapping_add(0xffc);
    map_position_3e44c(m, player + 8, point, origin);
    let px = u32::from(m.lhu(point));
    let pz = u32::from(m.lhu(point + 2));
    let cx = px + 0x60;
    let cz = pz + 0x38;
    m.sh(arrow + 8, (px + 0x5c) as u16);
    m.sh(arrow + 0xa, (pz + 0x34) as u16);
    let heading = m.lhu(player + 0x1a);
    m.sb(arrow + 0xd, 0x3b);
    let cell = ((0x4200 - u32::from(heading & 0x3fff)) >> 7) & 0x78;
    m.sb(arrow + 0xc, cell.wrapping_sub(0x80) as u8);
    // Two wedges of 2 * 0x2AA steps around the heading.
    let heading = i32::from(m.lh(player + 0x1a));
    let mut angle = (0x1aac - heading) as u32 & 0x3fff;
    let mut wedge = m.gw(PRIMITIVES).wrapping_add(0xfd4);
    for _ in 0..2 {
        let start = angle;
        let middle = (start + 0x2aa) & 0x3fff;
        let end = (middle + 0x2aa) & 0x3fff;
        let sine = |m: &mut Machine, step: u32| (i32::from(m.lh(SINE + step * 2)) >> 10) as u32;
        let x0 = sine(m, start);
        let z0 = sine(m, start + 0x1000);
        let z2 = sine(m, middle + 0x1000);
        m.sh(wedge + 4, cx as u16);
        m.sh(wedge + 6, cz as u16);
        m.sh(wedge + 2, cz.wrapping_add(z0) as u16);
        m.sh(wedge, cx.wrapping_add(x0) as u16);
        let x2 = sine(m, middle);
        m.sh(wedge + 0xa, cz.wrapping_add(z2) as u16);
        m.sh(wedge + 8, cx.wrapping_add(x2) as u16);
        let x3 = sine(m, end);
        let z3 = sine(m, end + 0x1000);
        m.sh(wedge + 0xc, cx.wrapping_add(x3) as u16);
        m.sh(wedge + 0xe, cz.wrapping_add(z3) as u16);
        angle = end;
        wedge += 0x18;
    }
    // Entity dots: TILE primitives after the tiles already used.
    let counters = m.gw(COUNTERS);
    let used = m.lw(counters + 0xc);
    let mut tile = m
        .gw(PRIMITIVES)
        .wrapping_add((used << 4).wrapping_add(0x518));
    for slot in 2..64u32 {
        let table = entities(m);
        let shown = m.lw(SLOT_FLAGS + slot * 4);
        let entity = m.lw(table.wrapping_add(slot * 4));
        if shown & 0x100 == 0 || m.lw(entity) & 0x4000 == 0 {
            continue;
        }
        if shown & 1 == 0 || shown & 0x2000 == 0 {
            continue;
        }
        if (map_position_3e44c(m, entity + 8, point, origin) as i32) < 0 {
            continue;
        }
        let px = u32::from(m.lhu(point));
        let code = m.lbu(tile + 7);
        m.sh(tile + 8, (px + 0x60) as u16);
        let pz = u32::from(m.lhu(point + 2));
        m.sb(tile + 4, 0xff);
        m.sb(tile + 5, 0x40);
        m.sb(tile + 6, 0x40);
        m.sh(tile + 0xc, 2);
        m.sh(tile + 0xe, 2);
        let x = u32::from(m.lhu(tile + 8));
        m.sb(tile + 7, code & 0xfd);
        let counters = m.gw(COUNTERS);
        let z = pz + 0x38;
        m.sh(tile + 0xa, z as u16);
        m.sh(tile + 8, x.wrapping_sub(1) as u16);
        m.sh(tile + 0xa, (z - 1) as u16);
        let used = m.lw(counters + 0xc);
        tile = tile.wrapping_add(0x10);
        m.sw(counters + 0xc, used.wrapping_add(1));
    }
    for slot in 2..64u32 {
        let table = entities(m);
        let shown = m.lw(SLOT_FLAGS + slot * 4);
        let entity = m.lw(table.wrapping_add(slot * 4));
        if shown & 0x100 != 0 || m.lw(entity) & 0x4000 == 0 {
            continue;
        }
        if shown & 1 == 0 || shown & 0x2000 == 0 {
            continue;
        }
        if (map_position_3e44c(m, entity + 8, point, origin) as i32) < 0 {
            continue;
        }
        let px = u32::from(m.lhu(point));
        m.sh(tile + 8, (px + 0x60) as u16);
        let pz = u32::from(m.lhu(point + 2));
        m.sh(tile + 0xa, (pz + 0x38) as u16);
        let flags = m.lw(entity);
        let rgb = if flags & 0x40 != 0 {
            [0, 0, 0xff]
        } else if flags & 0x100 != 0 {
            [0xff, 0xff, 0]
        } else {
            [0x40; 3]
        };
        for (offset, component) in (4u32..).zip(rgb) {
            m.sb(tile + offset, component);
        }
        m.sh(tile + 0xc, 2);
        m.sh(tile + 0xe, 2);
        let code = m.lbu(tile + 7);
        m.sb(tile + 7, code | 2);
        let counters = m.gw(COUNTERS);
        let used = m.lw(counters + 0xc);
        tile = tile.wrapping_add(0x10);
        m.sw(counters + 0xc, used.wrapping_add(1));
    }
    m.pop_frame(0x40);
}

/// 8003C5B0 (JP; PAL 8003BDF8, near, `imm:1 op:1`) (`view`): radar, gauges and their packet linking; with
/// `[context+0x95E]` clear and `[context+0x128]` flag 0x8000, also the
/// second layer (8003E69C, 8003CB90).
pub fn gauges_3bdf8(m: &mut Machine, view: u32) {
    const FRAME: u32 = 0x18;
    m.push_frame(FRAME);
    hud::tree_3bdf8_a::begin_frame_3be98(m);
    hud::tree_3bdf8_a::radar_basis_3c8d4(m, view);
    hud::tree_3bdf8_a::radar_distances_3c980(m);
    hud::tree_3bdf8_a::radar_range_3cac8(m);
    frame_tiles_3cca0(m);
    radar_map_3ce04(m);
    hud::tree_3bdf8_b::radar_blips_3d20c(m);
    draw_blips_3d6a0(m);
    hud::tree_3bdf8_a::link_hud_3bed8(m);
    let context = context(m);
    if m.lbu(context + 0x95a) == 0 && m.lw(context + 0x128) & 0x8000 != 0 {
        map_window_3dedc(m);
        hud::tree_3bdf8_a::link_layer_3c3d8(m);
    }
    m.pop_frame(FRAME);
}
