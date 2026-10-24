//! Patches of the `hud` overlay group (generated `hud::tree_40a24`) and of
//! the HUD composition 8003A0B4 (`hud::compose::compose_39b80`) whose JP
//! instructions differ from PAL, part 3 of the resident wave, and the
//! JP-only mission counter texts 8003A68C (claimed in [`dispatch`]). Each
//! patch is listed in [`REPLACES`](super::REPLACES); the functions keep the
//! generated names.
use crate::hud::tree_40a24::{HUD_COLOR, area_status_46aa8, raise_warning_41d4c, voice_4b108};
use crate::hud::{self, add_prim, context, entities, gameplay, sound_2d724};
use crate::machine::{GP, Machine, SmallData};

/// 800C6ECC: alert flags (bit 1 forces the alert colors and warning 1,
/// bit 0 warning 2).
const ALERT_FLAGS: u32 = 0x800c_6ecc;

/// `[gp+0x340]`: mission state (+0 flags, +4 counter, +0x3D byte).
const MISSION: u32 = 0x340;

/// `[gp+0x33C]`: area limits (+8 minimum altitude, +0xC altitude limit,
/// +0x10..+0x1C x/z minimum and maximum).
const AREA: u32 = 0x33c;

/// `[gp+0x450]`: overlay packet set (+0/+4 the two buffers, +8 sprite
/// count, +0xC draw-mode count); `[gp+0x454]`: this frame's buffer.
const SET_440_TABLE: u32 = 0x450;

const SET_440_BUFFER: u32 = 0x454;

/// gp+0x460: nonzero links the upper-right frame primitives.
const FRAME_LINKED: u32 = GP + 0x460;

/// gp+0x44C: frames the altitude warning has been pending.
const ALTITUDE_HOLD: u32 = GP + 0x44c;

/// 800C7180: active warning (+0 code or -1, +4 remaining frames, +8 and
/// +0xC two label slots: active byte, label id, frame halfword).
const WARNING: u32 = 0x800c_7180;

/// 800B1254: 16-byte labels (+0 blink kind in the low nibble, +1 color,
/// +2 frames, +4/+6 x/y, +8/+9 and +A/+B glyph and x offset of two
/// sprites (+A 0xFF: none), +C sound id, +E sound frame).
const LABELS: u32 = 0x800b_1254;

/// 800B1424: label glyphs (u, v, w, h bytes).
const GLYPHS: u32 = 0x800b_1424;

/// Sprite texture page/CLUT halfword of the labels and their modes.
const LABEL_CLUT: u16 = 0x7f9b;

/// Label sprite color component.
const LABEL_GREY: u8 = 0x50;

/// Fill a grey label glyph sprite at `sprite` (x = label x + `dx`, y =
/// label y + 2) and link it, counting it in set `[gp+0x450]` +8.
fn glyph_sprite(m: &mut Machine, ot: u32, sprite: u32, label: u32, glyph: u8, dx: u32) {
    m.sb(sprite + 4, LABEL_GREY);
    m.sb(sprite + 5, LABEL_GREY);
    m.sb(sprite + 6, LABEL_GREY);
    m.sh(sprite + 0xe, LABEL_CLUT);
    let dx = m.lbu(label + dx);
    let x = m.lhu(label + 4);
    m.sh(sprite + 8, x.wrapping_add(u16::from(dx)));
    let uv = (u32::from(glyph) << 2).wrapping_add(GLYPHS);
    let y = m.lhu(label + 6);
    m.sh(sprite + 0xa, y.wrapping_add(2));
    let u = m.lbu(uv);
    m.sb(sprite + 0xc, u);
    let v = m.lbu(uv + 1);
    m.sb(sprite + 0xd, v);
    let w = m.lbu(uv + 2);
    m.sh(sprite + 0x10, u16::from(w));
    let h = m.lbu(uv + 3);
    m.sh(sprite + 0x12, u16::from(h));
    add_prim(m, ot, sprite);
    count_440(m, 8);
}

/// Increment the set `[gp+0x450]` count word at `offset`.
fn count_440(m: &mut Machine, offset: u32) {
    let set = m.gw(SET_440_TABLE);
    let count = m.lw(set + offset);
    m.sw(set + offset, count.wrapping_add(1));
}

/// Link the draw-mode packet `mode` (one word, `word`), counting it in
/// set `[gp+0x450]` +0xC.
fn draw_mode(m: &mut Machine, ot: u32, mode: u32, word: u32) {
    m.sb(mode + 3, 1);
    m.sw(mode + 4, word);
    add_prim(m, ot, mode);
    count_440(m, 0xc);
}

/// The JP mission number: the halfword at context +0x38C (PAL reads the
/// byte 0x800BAEB4).
fn jp_mission(m: &mut Machine) -> i32 {
    let ctx = context(m);
    i32::from(m.lh(ctx + 0x38c))
}

/// Color of a label's arrow and marker sprites by its color byte.
fn marker_rgb(code: u8) -> [u8; 3] {
    match code {
        0 => [0x80, 0, 0],
        1 => [0x80, 0x40, 0],
        2 => [0, 0, 0x80],
        _ => [0xff; 3],
    }
}

/// 8003A0B4 (JP; PAL 80039B80, exact, calls differ, REVIEW.md §3b, `view`):
/// the HUD drawers in their original order, with the JP mission counter
/// texts 8003A68C in place of PAL 8003A158; with gameplay +4 flag 0x800,
/// also the training guidance 80042B20.
pub fn compose_39b80(m: &mut Machine, view: u32) {
    const FRAME: u32 = 0x18;
    m.push_frame(FRAME);
    hud::tree_40a24::hud_color_39bf8(m, view);
    counters_3a68c(m, view);
    hud::tree_40a24::overlay_40a24(m, view);
    hud::compose::gauges_3bdf8(m, view);
    hud::tree_3e828::markers_3e828(m);
    hud::compose::overlay_62680(m, view);
    hud::tree_5cbf4::panel_5cbf4(m, view);
    let game = gameplay(m);
    if m.lw(game + 4) & 0x800 != 0 {
        hud::tree_41ff0::guidance_41ff0(m, view);
    }
    m.pop_frame(FRAME);
}

/// 800C7150: the 16-character name buffer of [`counters_3a68c`].
const NAME: u32 = 0x800c_7150;
/// 800BCC1C / 800BCC24: the two label strings of [`counters_3a68c`].
const LABEL_A: u32 = 0x800b_cc1c;
const LABEL_B: u32 = 0x800b_cc24;

/// 8003563C (`ot`, `prim`, `x`, `y`; `[sp+0x10]` color, `[sp+0x14]` text)
/// called with its stack arguments in `frame` on the gameplay ordering
/// table and primitive pointer; the next free primitive goes back to
/// gameplay +0x14.
fn text(m: &mut Machine, frame: u32, x: u32, y: u32, color: u32, string: u32) {
    m.sw(frame + 0x10, color);
    m.sw(frame + 0x14, string);
    let game = gameplay(m);
    let ot = m.lw(game + 0x10);
    let prim = m.lw(game + 0x14);
    let next = crate::dispatch::call(m, 0x8003_563c, [ot, prim, x, y]);
    let game = gameplay(m);
    m.sw(game + 0x14, next);
}

/// The two decimal digits of the mission state byte `offset` at
/// (0x128, `y`) and (0x130, `y`), through the one-glyph string at
/// `frame + 0x18`.
fn two_digits(m: &mut Machine, frame: u32, offset: u32, y: u32, color: u32) {
    let digits = frame + 0x18;
    let state = m.gw(MISSION);
    let value = u32::from(m.lbu(state + offset));
    m.sb(digits, (value / 10 + 0x30) as u8);
    text(m, frame, 0x128, y, color, digits);
    let state = m.gw(MISSION);
    let value = u32::from(m.lbu(state + offset));
    m.sb(digits, (value - value / 10 * 10 + 0x30) as u8);
    text(m, frame, 0x130, y, color, digits);
}

/// 8003A68C (`view`, unused; JP-only, `new`; replaces PAL 8003A158 in
/// 8003A0B4): the mission counter texts in the HUD color `[800C6D6B..D] /
/// 2`. Mission 30: on the first frame (`[gameplay+8]` and `+0xC` zero) the
/// 16-byte name at context +0x948 goes to 800C7150 with lower case raised;
/// then, unless the mission state `[gp+0x340]` +0x3D is set without
/// gameplay +4 bit 1, the name at (0xDE, 0x50) with the two digits of
/// state +0x2E, and label 800BCC1C at (0xDE, 0x58) with the two digits of
/// state +0x2D. Mission 3: on the same condition (or +0x3D equal to 3),
/// label 800BCC24 at (0x110, 0x50) and the decimal digits of the state word
/// +4 right to left from x 0x130 at y 0x58 (none for 0).
pub fn counters_3a68c(m: &mut Machine, view: u32) {
    let _ = view;
    let frame = m.push_frame(0x38);
    let digits = frame + 0x18;
    let blue = u32::from(m.lbu(HUD_COLOR + 0xd)) >> 1;
    let green = u32::from(m.lbu(HUD_COLOR + 0xc)) >> 1;
    let red = u32::from(m.lbu(HUD_COLOR + 0xb)) >> 1;
    let color = (blue << 16) | (green << 8) | red;
    let shown = |m: &mut Machine, alternate: bool| {
        let state = m.gw(MISSION);
        let flag = m.lbu(state + 0x3d);
        if flag == 0 || (alternate && flag == 3) {
            return true;
        }
        let game = gameplay(m);
        m.lw(game + 4) & 2 != 0
    };
    if jp_mission(m) == 0x1e {
        let game = gameplay(m);
        if m.lw(game + 8) == 0 && m.lw(game + 0xc) == 0 {
            for i in 0..16 {
                let ctx = context(m);
                let mut c = m.lbu(ctx.wrapping_add(i).wrapping_add(0x948));
                if c.wrapping_sub(0x61) < 0x1a {
                    c = c.wrapping_sub(0x20);
                }
                m.sb(NAME + i, c);
            }
        }
        if jp_mission(m) == 0x1e && shown(m, false) {
            text(m, frame, 0xde, 0x50, color, NAME);
            m.sb(digits + 1, 0);
            two_digits(m, frame, 0x2e, 0x50, color);
            text(m, frame, 0xde, 0x58, color, LABEL_A);
            two_digits(m, frame, 0x2d, 0x58, color);
        }
    }
    if jp_mission(m) == 3 && shown(m, true) {
        m.sb(digits + 1, 0);
        text(m, frame, 0x110, 0x50, color, LABEL_B);
        let state = m.gw(MISSION);
        let mut value = m.lw(state + 4) as i32;
        let mut x = 0x130_u32;
        while value != 0 {
            let quotient = value / 10;
            m.sb(digits, (value - quotient * 10 + 0x30) as u8);
            text(m, frame, x, 0x58, color, digits);
            x -= 8;
            value = quotient;
        }
    }
    m.pop_frame(0x38);
}

/// Dispatch claims of the JP-only overlay routines.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    match target {
        0x8003_a68c => counters_3a68c(m, args[0]),
        _ => return None,
    }
    Some(0)
}

/// 80041794 (JP; PAL 80040DE4, near, `imm:4`; y 0xE/0x11/0x33, PAL
/// 0xC/0xF/0x31): fill the upper-right frame primitives of this frame's
/// buffer, shifted left by `shift` (the sign-extended gp+0x462): two
/// base-color sprites (+0, +0x14), highlight bars (+0x28, +0x48) and a
/// black backing tile (+0x38), whose widths grow with the shift.
pub fn frame_40de4(m: &mut Machine, shift: u32) {
    let colors = HUD_COLOR;
    let prim = m.gw(SET_440_BUFFER);
    let r = m.lbu(colors + 0xb);
    m.sb(prim + 4, r);
    let g = m.lbu(colors + 0xc);
    m.sb(prim + 5, g);
    let b = m.lbu(colors + 0xd);
    m.sh(prim + 8, 0x10fu32.wrapping_sub(shift) as u16);
    m.sh(prim + 0xa, 0xe);
    m.sb(prim + 6, b);
    m.sb(prim + 0xc, if shift == 0 { 0xaa } else { 0x78 });

    let prim = m.gw(SET_440_BUFFER) + 0x14;
    let r = m.lbu(colors + 0xb);
    m.sb(prim + 4, r);
    let g = m.lbu(colors + 0xc);
    m.sb(prim + 5, g);
    let b = m.lbu(colors + 0xd);
    m.sh(prim + 8, 0x140u32.wrapping_sub(shift) as u16);
    m.sh(prim + 0xa, 0xe);
    m.sb(prim + 6, b);

    let width = if (shift as i32) < 0xf {
        0
    } else {
        shift.wrapping_sub(0xe)
    };
    let bar_x = 0x14eu32.wrapping_sub(shift) as u16;
    let highlight = |m: &mut Machine, prim: u32| {
        for i in 0..3 {
            let c = m.lbu(colors + 8 + i);
            m.sb(prim + 4 + i, c);
        }
    };
    let prim = m.gw(SET_440_BUFFER) + 0x28;
    m.sh(prim + 8, bar_x);
    m.sh(prim + 0xa, 0xe);
    m.sh(prim + 0xc, width as u16);
    m.sh(prim + 0xe, 3);
    highlight(m, prim);

    let prim = m.gw(SET_440_BUFFER) + 0x38;
    m.sb(prim + 4, 0);
    m.sb(prim + 5, 0);
    m.sb(prim + 6, 0);
    let buffer = m.gw(SET_440_BUFFER);
    m.sh(prim + 8, 0x149u32.wrapping_sub(shift) as u16);
    m.sh(prim + 0xa, 0x11);
    m.sh(prim + 0xc, width.wrapping_add(5) as u16);
    m.sh(prim + 0xe, 0x22);

    let prim = buffer + 0x48;
    m.sh(prim + 8, bar_x);
    m.sh(prim + 0xa, 0x33);
    m.sh(prim + 0xc, width as u16);
    m.sh(prim + 0xe, 3);
    highlight(m, prim);
}

/// 800414D8 (JP; PAL 80040B00, changed, `call-:1 del:11 ins:1 op:3
/// reg:29`): link the buffer's first primitive and, when gp+0x460 is set,
/// the upper-right frame primitives (+0x14, +0x28, +0x48, +0x58, +0x38,
/// +0x60) into the ordering-table slot gameplay +0x10. JP draws no message
/// line here (PAL 80054D18 first, REVIEW.md §4) and has no frame.
pub fn link_frame_40b00(m: &mut Machine) {
    let game = gameplay(m);
    let buffer = m.gw(SET_440_BUFFER);
    let ot = m.lw(game + 0x10);
    add_prim(m, ot, buffer);
    if m.lbu(FRAME_LINKED) != 0 {
        for offset in [0x14, 0x28, 0x48, 0x58, 0x38, 0x60] {
            add_prim(m, ot, buffer + offset);
        }
    }
}

/// 80042544 (JP; PAL 80041A68, near, `call+:3 del:7 imm:8 ins:28 op:2 reg:5`):
/// pick the highest-priority warning (codes 0..0xA: view
/// flags, alert flags, leaving the area (800476F0), minimum altitude,
/// altitude limit after 30 frames, context/mission conditions) and raise
/// it (8004287C); keep mission flag 0x20 equal to "view +0x1C is zero";
/// then draw both labels of the active warning and expire it. JP: the
/// view warning (code 0, view bits 0..2) is raised at once and adds the
/// radio cues 80048C0C(1, 0x3C, 0) for view bit 1 and (2, 0x3C, 0) for
/// bit 2; the mission is the halfword context +0x38C (PAL the byte
/// 0x800BAEB4).
pub fn warnings_41a68(m: &mut Machine, view: u32) {
    let flags = m.lw(ALERT_FLAGS);
    let table = entities(m);
    let player = m.lw(table);
    let mut holding = false;
    let warning = 'pick: {
        if flags & 2 != 0 {
            break 'pick Some(1);
        }
        if flags & 1 != 0 {
            break 'pick Some(2);
        }
        if m.lw(view) & 7 != 0 {
            // JP: raised at once, with the radio cues of view bits 1/2.
            raise_warning_41d4c(m, 0);
            if m.lw(view) & 2 != 0 {
                crate::jp::res_p3_resident::radio_cue_48c0c(m, 1, 0x3c, 0, 0);
            }
            if m.lw(view) & 4 != 0 {
                crate::jp::res_p3_resident::radio_cue_48c0c(m, 2, 0x3c, 0, 0);
            }
            break 'pick None;
        }
        if area_status_46aa8(m, 0) == 1 && !matches!(jp_mission(m), 0x20 | 0x2f | 0x35) {
            break 'pick Some(3);
        }
        let area = m.gw(AREA);
        let altitude = m.lw(player + 0xc) as i32;
        if altitude < m.lw(area + 8) as i32 {
            break 'pick Some(4);
        }
        if m.lw(view) & 0x18 != 0 {
            break 'pick Some(5);
        }
        let limit = m.lw(area + 0xc) as i32;
        let height = m.lw(view + 0x30) as i32;
        if height < limit.wrapping_neg()
            || (matches!(jp_mission(m), 0x2a | 0x3a) && limit < altitude)
        {
            // Over the altitude limit: warning 6 after 30 frames.
            let frames = m.lbu(ALTITUDE_HOLD).wrapping_add(1);
            m.sb(ALTITUDE_HOLD, frames);
            holding = true;
            break 'pick (frames >= 0x1e).then_some(6);
        }
        if m.lw(view + 0x1c) == 0 {
            let ctx = context(m);
            if m.lw(ctx + 0x12c) & 0x1000 != 0 {
                break 'pick Some(7);
            }
        }
        let mission = m.gw(MISSION);
        let mission_flags = m.lw(mission);
        if mission_flags & 8 != 0 {
            break 'pick Some(8);
        }
        let ctx = context(m);
        if (m.lw(ctx + 0x128) as i32) < 0 {
            if m.lw(ctx + 0x12c) & 0x8000_0000 != 0 {
                sound_2d724(m, 0x37);
            }
            break 'pick Some(9);
        }
        if mission_flags & 0x10 == 0 || jp_mission(m) != 3 {
            break 'pick None;
        }
        let game = gameplay(m);
        (m.lw(game + 8) == 0).then_some(0xa)
    };
    if let Some(code) = warning {
        raise_warning_41d4c(m, code);
    }
    if !holding {
        m.sb(ALTITUDE_HOLD, 0);
    }
    let clear = m.lw(view + 0x1c) == 0;
    let mission = m.gw(MISSION);
    let mission_flags = m.lw(mission);
    m.sw(
        mission,
        if clear {
            mission_flags | 0x20
        } else {
            mission_flags & !0x20
        },
    );

    if m.lw(WARNING) == 0xffff_ffff {
        return;
    }
    let first = warning_label_415d8(m, WARNING + 8);
    let second = warning_label_415d8(m, WARNING + 0xc);
    let ctx = context(m);
    if m.lw(ctx) & 0x2000_0000 == 0 {
        let frames = m.lw(WARNING + 4);
        m.sw(WARNING + 4, frames.wrapping_sub(1));
    }
    if first == 0 && second == 0 && m.lw(WARNING + 4) == 0 {
        m.sw(WARNING, 0xffff_ffff);
    }
}

/// 80041F88 (JP; PAL 800415D8, changed, `del:4 imm:2 ins:79 op:11 reg:74`):
/// run the label slot `slot` (+0 active, +1 label id, +2 frame
/// halfword): advance its frame (starting the label's sound at its sound
/// frame when no voice plays) unless context +0 has bit 29, deactivate
/// it after the label's frames, and, in its blink phase, draw its one or
/// two glyph sprites and the colored marker sprite with their draw
/// modes; JP adds an arrow sprite (u 0x1C, 0x4A x 9, at x + 0x1C and y -
/// 0xA for blink-byte high nibble 0x10, else y + 0xA) when the label's
/// high nibble is set. Returns the slot's active byte (0 for an empty
/// slot).
pub fn warning_label_415d8(m: &mut Machine, slot: u32) -> u32 {
    let game = gameplay(m);
    let ot = m.lw(game + 0x10);
    if m.lbu(slot) == 0 || m.lbu(slot + 1) == 0xff {
        return 0;
    }
    let label = (u32::from(m.lbu(slot + 1)) << 4).wrapping_add(LABELS);
    let ctx = context(m);
    if m.lw(ctx) & 0x2000_0000 == 0 {
        if m.lh(slot + 2) == m.lh(label + 0xe) && voice_4b108(m) == 0xff {
            let sound = m.lh(label + 0xc);
            sound_2d724(m, sound as i32 as u32);
        }
        let frame = m.lhu(slot + 2).wrapping_add(1);
        m.sh(slot + 2, frame);
        if frame as i16 >= m.lh(label + 2) {
            m.sb(slot, 0);
        }
    }
    let visible = match m.lbu(label) & 0xf {
        1 => m.lhu(slot + 2) & 6 != 0,
        2 => m.lhu(slot + 2) & 2 != 0,
        _ => true,
    };
    if visible {
        let set = m.gw(SET_440_TABLE);
        let sprites = m.lw(set + 8);
        let glyph = m.lbu(label + 8);
        let buffer = m.gw(SET_440_BUFFER);
        let mut sprite = buffer.wrapping_add(sprites.wrapping_mul(20).wrapping_add(0x68));
        glyph_sprite(m, ot, sprite, label, glyph, 9);
        let glyph = m.lbu(label + 0xa);
        if glyph != 0xff {
            sprite = sprite.wrapping_add(0x14);
            glyph_sprite(m, ot, sprite, label, glyph, 0xb);
        }
        // JP: the arrow sprite of a label with a high blink nibble.
        let arrow = m.lbu(label) & 0xf0;
        if arrow != 0 {
            sprite = sprite.wrapping_add(0x14);
            let [r, g, b] = marker_rgb(m.lbu(label + 1));
            m.sb(sprite + 4, r);
            m.sb(sprite + 5, g);
            m.sb(sprite + 6, b);
            m.sh(sprite + 0xe, LABEL_CLUT);
            let x = m.lhu(label + 4);
            m.sh(sprite + 8, x.wrapping_add(0x1c));
            let y = m.lhu(label + 6);
            let y = if arrow == 0x10 {
                y.wrapping_sub(0xa)
            } else {
                y.wrapping_add(0xa)
            };
            m.sh(sprite + 0xa, y);
            m.sb(sprite + 0xc, 0x1c);
            m.sh(sprite + 0x10, 0x4a);
            m.sb(sprite + 0xd, 0);
            m.sh(sprite + 0x12, 9);
            add_prim(m, ot, sprite);
            count_440(m, 8);
        }

        let set = m.gw(SET_440_TABLE);
        let modes = m.lw(set + 0xc);
        let buffer = m.gw(SET_440_BUFFER);
        let mode = buffer.wrapping_add((modes << 3).wrapping_add(0x1a8));
        draw_mode(m, ot, mode, 0xe100_0636);

        // The marker follows the last glyph sprite (the count was
        // advanced with it).
        sprite = sprite.wrapping_add(0x14);
        let [r, g, b] = marker_rgb(m.lbu(label + 1));
        m.sb(sprite + 4, r);
        m.sb(sprite + 5, g);
        m.sb(sprite + 6, b);
        m.sh(sprite + 0xe, LABEL_CLUT);
        let x = m.lhu(label + 4);
        m.sh(sprite + 8, x);
        let y = m.lhu(label + 6);
        m.sb(sprite + 0xd, 0xa);
        m.sh(sprite + 0x10, 0x84);
        m.sb(sprite + 0xc, 0);
        m.sh(sprite + 0x12, 9);
        m.sh(sprite + 0xa, y);
        add_prim(m, ot, sprite);
        count_440(m, 8);
        draw_mode(m, ot, mode.wrapping_add(8), 0xe100_0616);
    }
    u32::from(m.lbu(slot))
}
