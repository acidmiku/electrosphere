//! JP overlay 2, part 1: the KEY CONFIG screen's drawing and input (none
//! of these ran in the captures; they are validated against the original
//! with the oracle).
//!
//! The screen works on the options block `K` (`ov2_p1_options`) for the
//! controller type `K+0xC` (0..3): +0xE the settings row (0..4), +0x14 /
//! +0x15.. / +0x1B the row values, +0x1E the analog axis cursor, +0x1F the
//! button cursor, +0x20 / +0x21 the popups' choices, +0x22 / +0x23 the
//! popups (axis inversion or assignment, button assignment), +0xC8 four
//! pointers to the types' button orders; the button bytes `K+0x24+16*type`
//! and axis bytes `K+0x84+4*type` (`ov2_p1_keys`).
//!
//! - Labels: 800D3388, 800D38EC, 800D3BE0, 800D3FA0 the button labels of
//!   types 0..3 (by 800D4394), 800D4430, 800D47DC, 800D48F8 the analog
//!   labels (by 800D4CA4), each a quad of 800D1588.
//! - 800D4D20 the panels of the type, 800D5BD4 the view toggle, 800D4F78
//!   the settings rows, 800D65BC the button cursor, 800D60FC the button
//!   assignment popup, 800D5D64 the analog option popup, 800D7750 the axis
//!   cursor, 800D7194 the axis assignment popup, 800D6B3C the inversion
//!   popup.
use super::ov2::MODULE;
use super::ov2_p1::{FLAGS, at, call, context, lh, lt, sound, work};
use super::ov2_p1_keys::{action_label_d32fc, quad_d1588, save_settings_d1f54};
use super::ov2_p1_options::OPTIONS;
use super::ov2_p1_pages::{CURSOR, PANEL_SLIDE, PANEL_START, SET_CURSOR, SET_LAMP};
use super::ov2_p1_search::set_flag_byte;
use crate::common::loaded_module;
use crate::machine::Machine;

/// 8010A7C0: the button-label RECTs by action (8 bytes each).
pub const LABELS: u32 = 0x8010_a7c0;
/// 8010A880: the analog-label RECTs by axis code (8 bytes each).
pub const AXIS_LABELS: u32 = 0x8010_a880;
/// 8010AC5C: the number of buttons of each controller type (bytes).
pub const BUTTON_COUNTS: u32 = 0x8010_ac5c;
/// 800DEDA0: the position of panel `panel` entry `entry` into `out`
/// (two halfwords).
pub const PANEL_POSITION: u32 = 0x800d_eda0;
/// 800DED68: move panel `panel` entry `entry` to (`dx`, `dy`).
pub const PANEL_MOVE: u32 = 0x800d_ed68;
/// 800F3EF4: the text layer's table (`table`, or -1 to restore).
pub const TEXT_TABLE: u32 = 0x800f_3ef4;

fn options(m: &mut Machine) -> u32 {
    m.lw(OPTIONS)
}

/// Bit `n` of `bits` (arithmetic shift, as compiled) plus 1: the label
/// mode, 2 for the cursor.
fn mode(bits: u32, n: u32) -> u32 {
    (((bits as i32) >> n) as u32 & 1) + 1
}

/// `1 << K+offset` (`sllv`: the low five bits).
fn cursor_bits(m: &mut Machine, k: u32, offset: u32) -> u32 {
    1u32 << (u32::from(m.lbu(at(k, offset))) & 31)
}

/// The type's halfword `K+0xC`, sign-extended.
fn kind(m: &mut Machine) -> u32 {
    let k = options(m);
    lh(m, at(k, 0xc))
}

/// Pressed / held tests of the context's pad words.
fn pressed(m: &mut Machine, offset: u32, bits: u32) -> bool {
    let context = context(m);
    m.lw(at(context, offset)) & bits != 0
}

/// The confirm test: pressed `0x10` with none of `held` held.
fn confirmed(m: &mut Machine, held: u32) -> bool {
    let context = context(m);
    m.lw(at(context, 0x12c)) & 0x10 != 0 && m.lw(at(context, 0x128)) & held == 0
}

/// The cancel test: pressed `0x20` with none of 0x5F held.
fn cancelled(m: &mut Machine) -> bool {
    let context = context(m);
    m.lw(at(context, 0x12c)) & 0x20 != 0 && m.lw(at(context, 0x128)) & 0x5f == 0
}

fn clear_pressed(m: &mut Machine, offset: u32) {
    let context = context(m);
    m.sw(at(context, offset), 0);
}

/// The button labels: for each (button, x, y) the label of the button's
/// action (800D32FC) as a quad at (x, y) from [`LABELS`], mode 2 for the
/// button cursor (bit n+1 of `bits`), each from and back into `K+8`.
/// Returns the last quad's next primitive.
fn button_labels(
    m: &mut Machine,
    frame: u32,
    base: u32,
    bits: u32,
    items: &[(u32, u32, u32)],
) -> u32 {
    let mut next = 0;
    for (n, &(offset, x, y)) in (1u32..).zip(items) {
        let action = u32::from(m.lbu(at(base, offset)));
        let label = action_label_d32fc(m, action);
        let rect = (label << 3).wrapping_add(LABELS);
        let k = options(m);
        let prim = m.lw(at(k, 8));
        m.sw(at(frame, 0x10), prim);
        next = quad_d1588(m, x, y, rect, mode(bits, n));
        let k = options(m);
        m.sw(at(k, 8), next);
    }
    next
}

/// Flag bytes 0xA..0x1F cleared.
fn clear_marks(m: &mut Machine) {
    for offset in 0xau32..0x20 {
        set_flag_byte(m, offset, 0);
    }
}

/// 800D3388: type 0's fifteen button labels (`K+0x24`); while the
/// assignment popup shows (`K+0x23`), flag bytes 0xA+action mark the
/// assigned actions and axes (both bytes of an axis below 8).
pub fn labels_d3388(m: &mut Machine) -> u32 {
    let frame = m.push_frame(0x30);
    let k = options(m);
    let buttons = at(k, 0x24);
    let axes = at(k, 0x84);
    if m.lbu(at(k, 0x23)) != 0 {
        clear_marks(m);
        for b in 0..0xfu32 {
            let flags = m.lw(FLAGS);
            let action = u32::from(m.lbu(at(buttons, b)));
            let bytes = m.lw(at(flags, 0x5c));
            m.sb(at(bytes, action).wrapping_add(0xa), 1);
        }
        for n in 0..4u32 {
            if m.lbu(at(axes, n)) < 8 {
                for offset in [0xa, 0xb] {
                    let flags = m.lw(FLAGS);
                    let code = u32::from(m.lbu(at(axes, n)));
                    let bytes = m.lw(at(flags, 0x5c));
                    m.sb(at(code, bytes).wrapping_add(offset), 1);
                }
            }
        }
    } else {
        clear_marks(m);
    }
    let k = options(m);
    let bits = cursor_bits(m, k, 0x1f);
    let result = button_labels(
        m,
        frame,
        buttons,
        bits,
        &[
            (0, 0x28, 0x95),
            (1, 0x28, 0xb7),
            (2, 0x28, 0xd8),
            (3, 0x28, 0xf9),
            (4, 0x28, 0x11a),
            (5, 0x28, 0x13b),
            (6, 0x28, 0x15c),
            (7, 0xfc, 0x8a),
            (8, 0xfc, 0xa9),
            (9, 0xfc, 0xc7),
            (0xa, 0xfc, 0xe5),
            (0xb, 0xfc, 0x103),
            (0xc, 0xfc, 0x121),
            (0xd, 0xfc, 0x13f),
            (0xe, 0xfc, 0x15d),
        ],
    );
    m.pop_frame(0x30);
    result
}

/// 800D38EC: type 1's ten button labels (`K+0x34`).
pub fn labels_d38ec(m: &mut Machine) -> u32 {
    let frame = m.push_frame(0x30);
    let k = options(m);
    let buttons = at(k, 0x34);
    let bits = cursor_bits(m, k, 0x1f);
    let result = button_labels(
        m,
        frame,
        buttons,
        bits,
        &[
            (1, 0x28, 0xb7),
            (2, 0x28, 0xd8),
            (3, 0x28, 0xf9),
            (4, 0x28, 0x11a),
            (5, 0x28, 0x13b),
            (8, 0xfc, 0xb7),
            (9, 0xfc, 0xd8),
            (0xa, 0xfc, 0xf9),
            (0xb, 0xfc, 0x11a),
            (0xc, 0xfc, 0x13b),
        ],
    );
    m.pop_frame(0x30);
    result
}

/// 800D3BE0: type 2's thirteen button labels (`K+0x44`).
pub fn labels_d3be0(m: &mut Machine) -> u32 {
    let frame = m.push_frame(0x30);
    let k = options(m);
    let buttons = at(k, 0x44);
    let bits = cursor_bits(m, k, 0x1f);
    let result = button_labels(
        m,
        frame,
        buttons,
        bits,
        &[
            (0, 0x28, 0xa0),
            (1, 0x28, 0xc1),
            (2, 0x28, 0xe2),
            (3, 0x28, 0x103),
            (4, 0x28, 0x124),
            (5, 0x28, 0x145),
            (7, 0xfc, 0x95),
            (8, 0xfc, 0xb4),
            (9, 0xfc, 0xd3),
            (0xa, 0xfc, 0xf2),
            (0xb, 0xfc, 0x111),
            (0xc, 0xfc, 0x130),
            (0xe, 0xfc, 0x14f),
        ],
    );
    m.pop_frame(0x30);
    result
}

/// 800D3FA0: type 3's thirteen button labels (`K+0x54`); then the text
/// layer's table: the frame's (`[W+8]`) while hub cursor 6 is 2, and
/// restored (800F3EF4 -1, returned).
pub fn labels_d3fa0(m: &mut Machine) -> u32 {
    let frame = m.push_frame(0x30);
    let k = options(m);
    let buttons = at(k, 0x54);
    let bits = cursor_bits(m, k, 0x1f);
    button_labels(
        m,
        frame,
        buttons,
        bits,
        &[
            (1, 0x29, 0x96),
            (8, 0x29, 0xb5),
            (0, 0x29, 0xee),
            (7, 0x29, 0x10c),
            (0xe, 0x29, 0x154),
            (2, 0x90, 0x91),
            (3, 0x90, 0xb1),
            (4, 0x90, 0xd0),
            (5, 0x90, 0xf1),
            (0xb, 0xfc, 0x96),
            (0xc, 0xfc, 0xb5),
            (0xa, 0xfc, 0xee),
            (9, 0xfc, 0x10c),
        ],
    );
    if call(m, CURSOR, [6, 0, 0, 0]) == 2 {
        let w = work(m);
        let table = m.lw(at(w, 8));
        call(m, TEXT_TABLE, [table, 0, 0, 0]);
    }
    let result = call(m, TEXT_TABLE, [u32::MAX, 0, 0, 0]);
    m.pop_frame(0x30);
    result
}

/// 800D4394: the button labels of the type `K+0xC` (the halfword re-read
/// for each test). Returns 800D3FA0's result for type 3, else 3.
pub fn labels_d4394(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    if kind(m) == 0 {
        labels_d3388(m);
    }
    if kind(m) == 1 {
        labels_d38ec(m);
    }
    if kind(m) == 2 {
        labels_d3be0(m);
    }
    let result = if kind(m) == 3 { labels_d3fa0(m) } else { 3 };
    m.pop_frame(0x18);
    result
}

/// The inversion flag of axis code `code` (codes 0, 6 and 8 have one) at
/// `flag`, else 0.
fn inversion(m: &mut Machine, code: u32, flag: u32) -> u32 {
    if matches!(code, 0 | 6 | 8) {
        u32::from(m.lbu(flag))
    } else {
        0
    }
}

/// The analog labels of 800D4430 / 800D48F8: the four axes (`K+axes`),
/// each as two quads (the axis code plus its inversion, and the other
/// end) placed from panel 2 entry `entry`'s position (800DEDA0 into the
/// frame's +0x18), mode 2 for the axis cursor `K+0x1E`. Returns the last
/// next primitive.
fn analog_labels(m: &mut Machine, frame: u32, axes: u32, inverts: u32, entry: u32) -> u32 {
    let k = options(m);
    let axes = at(k, axes);
    let inverts = at(k, inverts);
    let mut inverted = [0u32; 4];
    for (n, slot) in (0u32..).zip(inverted.iter_mut()) {
        let code = u32::from(m.lbu(at(axes, n)));
        *slot = inversion(m, code, at(inverts, n));
    }
    let k = options(m);
    let bits = cursor_bits(m, k, 0x1e);
    call(m, PANEL_POSITION, [2, entry, at(frame, 0x18), 0]);
    // (x offset, y) of each axis's two quads
    let places: [((u32, u32), (u32, u32)); 4] = [
        ((0x2d, 0x9b), (0x2d, 0x130)),
        ((u32::MAX, 0xe7), (0x52, 0xe7)),
        ((0xeb, 0x9b), (0xeb, 0x130)),
        ((0xbc, 0xe7), (0x10f, 0xe7)),
    ];
    let k = options(m);
    let mut prim = m.lw(at(k, 8));
    for (n, ((dx1, y1), (dx2, y2))) in (0u32..).zip(places) {
        let mode = mode(bits, n + 1);
        let inverted = inverted[n as usize];
        let left = u32::from(m.lhu(at(frame, 0x18)));
        let code = u32::from(m.lbu(at(axes, n)));
        let x = left.wrapping_add(dx1) as u16 as i16 as i32 as u32;
        let rect = (code.wrapping_add(inverted) << 3).wrapping_add(AXIS_LABELS);
        m.sw(at(frame, 0x10), prim);
        let next = quad_d1588(m, x, y1, rect, mode);
        let k = options(m);
        m.sw(at(k, 8), next);
        let left = u32::from(m.lhu(at(frame, 0x18)));
        let code = u32::from(m.lbu(at(axes, n)));
        m.sw(at(frame, 0x10), next);
        let x = left.wrapping_add(dx2) as u16 as i16 as i32 as u32;
        let rect = (code.wrapping_add(1).wrapping_sub(inverted) << 3).wrapping_add(AXIS_LABELS);
        prim = quad_d1588(m, x, y2, rect, mode);
        let k = options(m);
        m.sw(at(k, 8), prim);
    }
    prim
}

/// 800D4430: type 0's analog labels (`K+0x84`, inversions `K+0x9C`, panel
/// 2 entry 9).
pub fn analog_d4430(m: &mut Machine) -> u32 {
    let frame = m.push_frame(0x48);
    let result = analog_labels(m, frame, 0x84, 0x9c, 9);
    m.pop_frame(0x48);
    result
}

/// 800D48F8: type 3's analog labels (`K+0x90`, inversions `K+0xA8`, panel
/// 2 entry 0xB).
pub fn analog_d48f8(m: &mut Machine) -> u32 {
    let frame = m.push_frame(0x48);
    let result = analog_labels(m, frame, 0x90, 0xa8, 0xb);
    m.pop_frame(0x48);
    result
}

/// 800D47DC: type 1's one analog label pair: its fourth axis (`K+0x8B`)
/// at (0x28, 0x14F) and (0x7A, 0x14F), with the inversion of `K+0xA0`
/// (the first axis's, as compiled), mode 2 for axis cursor 4.
pub fn analog_d47dc(m: &mut Machine) -> u32 {
    let frame = m.push_frame(0x30);
    let k = options(m);
    let axes = at(k, 0x88);
    let code = u32::from(m.lbu(at(axes, 3)));
    let inverted = inversion(m, code, at(k, 0xa0));
    let code = u32::from(m.lbu(at(axes, 3)));
    let k = options(m);
    let rect = (code.wrapping_add(inverted) << 3).wrapping_add(AXIS_LABELS);
    let bits = cursor_bits(m, k, 0x1e);
    let mode = mode(bits, 4);
    let prim = m.lw(at(k, 8));
    m.sw(at(frame, 0x10), prim);
    let next = quad_d1588(m, 0x28, 0x14f, rect, mode);
    let k = options(m);
    m.sw(at(k, 8), next);
    let code = u32::from(m.lbu(at(axes, 3)));
    m.sw(at(frame, 0x10), next);
    let rect = (code.wrapping_add(1).wrapping_sub(inverted) << 3).wrapping_add(AXIS_LABELS);
    let next = quad_d1588(m, 0x7a, 0x14f, rect, mode);
    let k = options(m);
    m.sw(at(k, 8), next);
    m.pop_frame(0x30);
    next
}

/// 800D4CA4: the analog labels of the type `K+0xC` (0, 1, 3; re-read for
/// each test). Returns 800D48F8's result for type 3, else 3.
pub fn analog_d4ca4(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    if kind(m) == 0 {
        analog_d4430(m);
    }
    if kind(m) == 1 {
        analog_d47dc(m);
    }
    let result = if kind(m) == 3 { analog_d48f8(m) } else { 3 };
    m.pop_frame(0x18);
    result
}

/// The panel of type 0, 1 or 3 by the view `K+0x1C` (panel 2: entry 4,
/// then `buttons` and lamp 2 = `button_lamp` for the button view (0), or
/// `analog` and `analog_lamp`).
fn view_panels(m: &mut Machine, buttons: (u32, u32), analog: (u32, u32)) {
    call(m, PANEL_START, [2, 4, 0, 0]);
    let k = options(m);
    let (entry, lamp) = if m.lbu(at(k, 0x1c)) == 0 {
        buttons
    } else {
        analog
    };
    call(m, PANEL_START, [2, entry, 0, 0]);
    call(m, SET_LAMP, [2, lamp, 0, 0]);
}

/// 800D4D20: the screen's start for the type: axis and button cursors 0,
/// `K+0x10` = 1, flag bytes 8..0x16 cleared, the type's panels (type 2
/// always its button view, `K+0x1C` = 1; type 3 also cursors 4/5 = 2 (0xA,
/// 0xA) and 6, 7, 2 = 0), cursor 1 = 0. Returns 800DEE08's result.
pub fn start_d4d20(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let k = options(m);
    m.sb(at(k, 0x1e), 0);
    let again = options(m);
    m.sw(at(k, 0x10), 1);
    m.sb(at(again, 0x1f), 0);
    for n in 0..0xfu32 {
        set_flag_byte(m, n + 8, 0);
    }
    if kind(m) == 0 {
        view_panels(m, (9, 0x210), (5, 0x30));
    }
    if kind(m) == 1 {
        view_panels(m, (0xa, 0x410), (6, 0x50));
    }
    if kind(m) == 2 {
        call(m, PANEL_START, [2, 4, 0, 0]);
        call(m, PANEL_START, [2, 7, 0, 0]);
        call(m, SET_LAMP, [2, 0x90, 0, 0]);
        let k = options(m);
        m.sb(at(k, 0x1c), 1);
    }
    if kind(m) == 3 {
        view_panels(m, (0xb, 0x810), (8, 0x110));
        call(m, SET_CURSOR, [4, 2, 0xa, 0xa]);
        call(m, SET_CURSOR, [5, 2, 0xa, 0xa]);
        call(m, SET_CURSOR, [6, 0, 0, 0]);
        call(m, SET_CURSOR, [7, 0, 0, 0]);
        call(m, SET_CURSOR, [2, 0, 0, 0]);
    }
    let result = call(m, SET_CURSOR, [1, 0, 0, 0]);
    m.pop_frame(0x18);
    result
}

/// The view toggle of 800D5BD4 for one type: from the button view
/// (`K+0x1C` 0) panel 2's `analog` entry and lamp; from the analog view
/// (1) the `buttons` entry and lamp.
fn toggle_panels(m: &mut Machine, analog: (u32, u32), buttons: (u32, u32)) {
    let k = options(m);
    if m.lbu(at(k, 0x1c)) == 0 {
        call(m, PANEL_START, [2, analog.0, 0, 0]);
        call(m, SET_LAMP, [2, analog.1, 0, 0]);
    }
    let k = options(m);
    if m.lbu(at(k, 0x1c)) == 1 {
        call(m, PANEL_START, [2, buttons.0, 0, 0]);
        call(m, SET_LAMP, [2, buttons.1, 0, 0]);
    }
}

/// 800D5BD4: toggle the view `K+0x1C` (button / analog) and its panels
/// (type 2 has only the analog view). Returns `1 - K+0x1C` (the new
/// view, as a word).
pub fn toggle_d5bd4(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let k = options(m);
    match lh(m, at(k, 0xc)) {
        0 => toggle_panels(m, (5, 0x30), (9, 0x210)),
        1 => toggle_panels(m, (6, 0x50), (0xa, 0x410)),
        2 => m.sb(at(k, 0x1c), 0),
        3 => toggle_panels(m, (8, 0x110), (0xb, 0x810)),
        _ => {}
    }
    let k = options(m);
    let view = 1u32.wrapping_sub(u32::from(m.lbu(at(k, 0x1c))));
    m.sb(at(k, 0x1c), view as u8);
    m.pop_frame(0x20);
    view
}

/// The row value flag bytes of 800D4F78: flag byte `offset` = 1 while the
/// value is not `value`, else 2 on the row `row` and 0 elsewhere.
fn row_mark(m: &mut Machine, offset: u32, current: u32, value: u32, row: u32) {
    if current != value {
        set_flag_byte(m, offset, 1);
        return;
    }
    let k = options(m);
    let on_row = lh(m, at(k, 0xe)) == row;
    set_flag_byte(m, offset, if on_row { 2 } else { 0 });
}

/// 800D4F78: the settings rows `K+0xE` (0 the view, 1 the vibration
/// `K+0x14`, 2 the analog mode `K+0x15+type`, 3 the response `K+0x1B`, 4
/// OK): triangle (0x40) jumps to OK; for types 0..3 down/up move the row
/// (type 2 skips rows 2 and 3), other types stay on row 0; right/left
/// change the row's value (the view through 800D4D20) with sound 0x3D;
/// confirm on OK stores the settings (800D1F54) and returns to the menu
/// (`K+0` 0 for types 0 and 2, 2 for types 1 and 3); cancel returns
/// without storing. Then the flag bytes show the rows. Returns the last
/// value the original computed (the help text number or a constant).
pub fn rows_d4f78(m: &mut Machine) -> u32 {
    m.push_frame(0x28);
    let result = rows(m);
    m.pop_frame(0x28);
    result
}

fn rows(m: &mut Machine) -> u32 {
    let k = options(m);
    let kind = lh(m, at(k, 0xc));
    if kind == 2 && m.lhu(at(k, 0xe)).wrapping_sub(2) < 2 {
        m.sh(at(k, 0xe), 0);
    }
    // triangle: to OK
    let context = context(m);
    if m.lw(at(context, 0x12c)) & 0x40 != 0 && m.lw(at(context, 0x128)) & 0x2f == 0 {
        let k = options(m);
        if lh(m, at(k, 0xe)) != 4 {
            sound(m, 0x3c);
            let context = self::context(m);
            let k = options(m);
            m.sw(at(context, 0x12c), 0);
            m.sb(at(k, 0x1e), 0);
            let again = options(m);
            m.sh(at(k, 0xe), 4);
            m.sb(at(again, 0x1f), 0);
        }
    }
    if lt(kind, 4) {
        if pressed(m, 0x130, 2) {
            sound(m, 0x3d);
            let k = options(m);
            let row = lh(m, at(k, 0xe));
            let value = m.lhu(at(k, 0xe));
            m.sh(
                at(k, 0xe),
                if lt(row, 4) { value.wrapping_add(1) } else { 0 },
            );
            if kind == 2 {
                let k = options(m);
                let row = lh(m, at(k, 0xe));
                let value = m.lhu(at(k, 0xe));
                if row == kind {
                    m.sh(at(k, 0xe), value.wrapping_add(2));
                }
            }
        }
        if pressed(m, 0x130, 1) {
            sound(m, 0x3d);
            let k = options(m);
            let row = lh(m, at(k, 0xe));
            let value = m.lhu(at(k, 0xe));
            m.sh(
                at(k, 0xe),
                if (row as i32) > 0 {
                    value.wrapping_sub(1)
                } else {
                    4
                },
            );
            if kind == 2 {
                let k = options(m);
                let row = lh(m, at(k, 0xe));
                let value = m.lhu(at(k, 0xe));
                if row == 3 {
                    m.sh(at(k, 0xe), value.wrapping_sub(2));
                }
            }
        }
    } else {
        let k = options(m);
        m.sh(at(k, 0xe), 0);
    }
    // right: the next value
    if pressed(m, 0x130, 8) {
        let k = options(m);
        if lt(lh(m, at(k, 0xe)), 4) {
            sound(m, 0x3d);
        }
        let k = options(m);
        if lh(m, at(k, 0xe)) == 0 && lt(kind, 4) {
            start_d4d20(m);
        }
        let k = options(m);
        let row = lh(m, at(k, 0xe));
        if row == 1 {
            let value = row.wrapping_sub(u32::from(m.lbu(at(k, 0x14))));
            m.sb(at(k, 0x14), value as u8);
        }
        let k = options(m);
        if lh(m, at(k, 0xe)) == 2 {
            let at_mode = at(k, kind).wrapping_add(0x15);
            let value = m.lbu(at_mode);
            m.sb(at_mode, if value < 2 { value + 1 } else { 0 });
        }
        let k = options(m);
        if lh(m, at(k, 0xe)) == 3 {
            let value = m.lbu(at(k, 0x1b));
            m.sb(at(k, 0x1b), if value < 2 { value + 1 } else { 0 });
        }
    }
    // left: the previous value
    if pressed(m, 0x130, 4) {
        let k = options(m);
        if lt(lh(m, at(k, 0xe)), 4) {
            sound(m, 0x3d);
        }
        let k = options(m);
        if lh(m, at(k, 0xe)) == 0 && lt(kind, 4) {
            start_d4d20(m);
        }
        let mut k = options(m);
        let mut row = lh(m, at(k, 0xe));
        if row == 1 {
            let value = row.wrapping_sub(u32::from(m.lbu(at(k, 0x14))));
            m.sb(at(k, 0x14), value as u8);
            k = options(m);
            row = lh(m, at(k, 0xe));
        }
        if row == 2 {
            let at_mode = at(k, kind).wrapping_add(0x15);
            let value = m.lbu(at_mode);
            m.sb(at_mode, if value != 0 { value - 1 } else { row as u8 });
        }
        let k = options(m);
        if lh(m, at(k, 0xe)) == 3 {
            let value = m.lbu(at(k, 0x1b));
            m.sb(at(k, 0x1b), if value != 0 { value - 1 } else { 2 });
        }
    }
    // confirm on OK: store and leave
    if confirmed(m, 0x2f) {
        let k = options(m);
        if lh(m, at(k, 0xe)) == 4 {
            save_settings_d1f54(m);
            sound(m, 0x3c);
            if kind == 0 || kind == 2 {
                call(m, PANEL_SLIDE, [0, 1, 0, 0]);
                let k = options(m);
                m.sb(k, 0);
            }
            if kind == 1 || kind == 3 {
                call(m, SET_LAMP, [2, 8, 0, 0]);
                let k = options(m);
                m.sb(k, 2);
            }
            let k = options(m);
            m.sb(at(k, 1), 0);
        }
    }
    // cancel: leave without storing
    if cancelled(m) {
        sound(m, 0x3b);
        call(m, PANEL_SLIDE, [0, 1, 0, 0]);
        let k = options(m);
        m.sb(k, 0);
        let k = options(m);
        m.sb(at(k, 1), 0);
        clear_pressed(m, 0x12c);
    }
    // the rows' flag bytes
    let flags = m.lw(FLAGS);
    let k = options(m);
    let bytes = m.lw(at(flags, 0x5c));
    let row = m.lbu(at(k, 0xe));
    m.sb(at(bytes, 1), row);
    let k = options(m);
    let vibration = u32::from(m.lbu(at(k, 0x14)));
    if vibration != 0 {
        set_flag_byte(m, 2, 1);
    } else {
        let on_row = lh(m, at(k, 0xe)) == 1;
        set_flag_byte(m, 2, if on_row { 2 } else { 0 });
    }
    let k = options(m);
    let vibration = u32::from(m.lbu(at(k, 0x14)));
    row_mark(m, 3, vibration, 1, 1);
    for (offset, value) in [(4u32, 0u32), (5, 1), (6, 2)] {
        let k = options(m);
        let mode = u32::from(m.lbu(at(k, kind).wrapping_add(0x15)));
        row_mark(m, offset, mode, value, 2);
    }
    for (offset, value) in [(7u32, 0u32), (0x22, 1), (9, 2)] {
        let k = options(m);
        let response = u32::from(m.lbu(at(k, 0x1b)));
        row_mark(m, offset, response, value, 3);
    }
    let k = options(m);
    let ok = lh(m, at(k, 0xe)) == 4;
    set_flag_byte(m, 0xa, if ok { 2 } else { 0 });
    // the rows the type lacks
    if kind < 2 || kind == 3 {
        for offset in [0xc, 0xd, 0xe] {
            set_flag_byte(m, offset, 0);
        }
    }
    if kind == 2 {
        set_flag_byte(m, 0xc, 0);
        for offset in [0xd, 0xe, 4, 5, 6, 7, 0x22, 9] {
            set_flag_byte(m, offset, 1);
        }
    }
    if kind.wrapping_sub(4) < 2 {
        for offset in [0xc, 0xd, 0xe, 2, 3, 4, 5, 6, 7, 0x22, 9] {
            set_flag_byte(m, offset, 1);
        }
    }
    // the help text of the row (flag byte 0x14)
    if lt(kind, 4) {
        let k = options(m);
        if lh(m, at(k, 0xe)) == 0 {
            set_flag_byte(m, 0x14, 0);
        }
        let k = options(m);
        if lh(m, at(k, 0xe)) == 1 {
            let flags = m.lw(FLAGS);
            let vibration = m.lbu(at(k, 0x14));
            let bytes = m.lw(at(flags, 0x5c));
            m.sb(at(bytes, 0x14), vibration.wrapping_add(1));
        }
        let k = options(m);
        if lh(m, at(k, 0xe)) == 2 {
            let flags = m.lw(FLAGS);
            let mode = m.lbu(at(k, kind).wrapping_add(0x15));
            let bytes = m.lw(at(flags, 0x5c));
            m.sb(at(bytes, 0x14), mode.wrapping_add(3));
        }
        let k = options(m);
        if lh(m, at(k, 0xe)) == 3 {
            let flags = m.lw(FLAGS);
            let response = m.lbu(at(k, 0x1b));
            let bytes = m.lw(at(flags, 0x5c));
            m.sb(at(bytes, 0x14), response.wrapping_add(0x65));
        }
        let k = options(m);
        if lh(m, at(k, 0xe)) == 4 {
            let text = if kind == 1 { 9 } else { 8 };
            set_flag_byte(m, 0x14, text);
            return u32::from(text);
        }
        return 4;
    }
    match kind {
        4 => {
            set_flag_byte(m, 0x14, 0x56);
            0x56
        }
        5 => {
            set_flag_byte(m, 0x14, 0x57);
            0x57
        }
        _ => 5,
    }
}

/// The assignment flag bytes of 800D5D64 / 800D60FC: 0xA..0x1E cleared,
/// 0x21 and 0x23 cleared, then 0xA+action for each of the type's fifteen
/// buttons (action 0xB at 0x21, 1 at 0x23) and both bytes of each axis
/// below 8 (code 0 at 0xA and 0x23). Returns 0.
fn assignment_flags(m: &mut Machine) -> u32 {
    for offset in 0xau32..0x1f {
        set_flag_byte(m, offset, 0);
    }
    set_flag_byte(m, 0x21, 0);
    set_flag_byte(m, 0x23, 0);
    for button in 0..0xfu32 {
        let k = options(m);
        let kind = lh(m, at(k, 0xc));
        let order = m.lw(at(k, kind << 2).wrapping_add(0xc8));
        let index = u32::from(m.lbu(at(order, button)));
        let action = u32::from(m.lbu(at(k, index.wrapping_add(kind << 4)).wrapping_add(0x24)));
        let mut offset = action.wrapping_add(0xa);
        if offset == 0x15 {
            offset = 0x21;
        }
        if offset == 0xb {
            offset = 0x23;
        }
        set_flag_byte(m, offset, 1);
    }
    for n in 0..4u32 {
        let k = options(m);
        let kind = lh(m, at(k, 0xc));
        let axis = at(k, n.wrapping_add(kind << 2)).wrapping_add(0x84);
        if m.lbu(axis) < 8 {
            let offset = u32::from(m.lbu(axis)).wrapping_add(0xa);
            if offset == 0xa {
                set_flag_byte(m, 0xa, 1);
                set_flag_byte(m, 0x23, 1);
            } else {
                set_flag_byte(m, offset, 1);
                set_flag_byte(m, offset.wrapping_add(1), 1);
            }
        }
    }
    0
}

/// The option popup of 800D5D64 for one option byte: confirm (0x10, none
/// of 0x6F held) sets it with sound 0x3C; cancel (0x20, none of 0x5F)
/// clears it with sound 0x3C.
fn option_popup(m: &mut Machine, k: u32, offset: u32) {
    if confirmed(m, 0x6f) {
        m.sb(at(k, offset), 1);
        sound(m, 0x3c);
    }
    if cancelled(m) {
        sound(m, 0x3c);
        let k = options(m);
        m.sb(at(k, offset), 0);
    }
}

/// 800D5D64: the analog options popup for the chosen action `K+0x21`
/// (0xE/0xF option 2 `K+0xB5`, 0x13 option 1 `K+0xB4`); confirm or cancel
/// closes it (`K+0x23` = 0, cursors 1 and 2 = 3, pressed bits cleared);
/// then the assignment flag bytes. Returns 0.
pub fn option_popup_d5d64(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let k = options(m);
    let action = m.lbu(at(k, 0x21));
    if (0xe..0x10).contains(&action) {
        option_popup(m, k, 0xb5);
    } else if action == 0x13 {
        option_popup(m, k, 0xb4);
    }
    let context = context(m);
    let pressed = m.lw(at(context, 0x12c));
    let close = (pressed & 0x10 != 0 && m.lw(at(context, 0x128)) & 0x6f == 0)
        || (pressed & 0x20 != 0 && m.lw(at(context, 0x128)) & 0x5f == 0);
    if close {
        let k = options(m);
        m.sb(at(k, 0x23), 0);
        call(m, SET_CURSOR, [1, 3, u32::MAX, u32::MAX]);
        call(m, SET_CURSOR, [2, 3, u32::MAX, u32::MAX]);
        clear_pressed(m, 0x12c);
    }
    let result = assignment_flags(m);
    m.pop_frame(0x18);
    result
}

/// 800D60FC: the button assignment popup: cancel (sound 0x3B) closes it;
/// down/up choose the action `K+0x21` (0..0x14, sound 0x3D); confirm
/// (sound 0x3C) removes the action from the type's other buttons (0x14)
/// and its axis (0xC), assigns it to the cursor's button and closes the
/// popup, or for the analog actions 0xE, 0xF and 0x13 opens their option
/// popup (`K+0x23` = 2). Flag byte 9 shows the popup's choice, 7 its help
/// text, then the assignment flag bytes. Returns 0.
pub fn assign_d60fc(m: &mut Machine) -> u32 {
    m.push_frame(0x28);
    let k = options(m);
    let kind = lh(m, at(k, 0xc));
    let buttons = at(k, (kind << 4).wrapping_add(0x24));
    let axes = at(k, (kind << 2).wrapping_add(0x84));
    if cancelled(m) {
        sound(m, 0x3b);
        let k = options(m);
        m.sb(at(k, 0x23), 0);
        call(m, SET_CURSOR, [1, 3, u32::MAX, u32::MAX]);
        call(m, SET_CURSOR, [2, 3, u32::MAX, u32::MAX]);
        clear_pressed(m, 0x12c);
    }
    if pressed(m, 0x130, 2) {
        sound(m, 0x3d);
        let k = options(m);
        let action = m.lbu(at(k, 0x21));
        m.sb(at(k, 0x21), if action < 0x14 { action + 1 } else { 0 });
    }
    if pressed(m, 0x130, 1) {
        sound(m, 0x3d);
        let k = options(m);
        let action = m.lbu(at(k, 0x21));
        m.sb(at(k, 0x21), if action != 0 { action - 1 } else { 0x14 });
    }
    if confirmed(m, 0x6f) {
        sound(m, 0x3c);
        for b in 0..0xfu32 {
            let button = at(buttons, b);
            let k = options(m);
            let assigned = m.lbu(button);
            if assigned == m.lbu(at(k, 0x21)) {
                m.sb(button, 0x14);
            }
        }
        for n in 0..4u32 {
            let k = options(m);
            let axis = at(axes, n);
            let chosen = m.lbu(at(k, 0x21)) & 0xfe;
            let code = m.lbu(axis);
            if code == chosen && code < 8 {
                m.sb(axis, 0xc);
            }
        }
        let k = options(m);
        let kind = lh(m, at(k, 0xc));
        let cursor = u32::from(m.lbu(at(k, 0x1f)));
        let order = m.lw(at(k, kind << 2).wrapping_add(0xc8));
        let index = u32::from(m.lbu(at(cursor, order).wrapping_sub(1)));
        let action = m.lbu(at(k, 0x21));
        m.sb(at(buttons, index), action);
        let k = options(m);
        let action = m.lbu(at(k, 0x21));
        if action == 0xe || action == 0x13 || action == 0xf {
            let k = options(m);
            m.sb(at(k, 0x23), 2);
        } else {
            m.sb(at(k, 0x23), 0);
            call(m, SET_CURSOR, [1, 3, u32::MAX, u32::MAX]);
            call(m, SET_CURSOR, [2, 3, u32::MAX, u32::MAX]);
        }
    }
    let k = options(m);
    if m.lbu(at(k, 0x23)) != 0 {
        let flags = m.lw(FLAGS);
        let action = m.lbu(at(k, 0x21));
        let bytes = m.lw(at(flags, 0x5c));
        m.sb(at(bytes, 9), action.wrapping_add(1));
    } else {
        set_flag_byte(m, 9, 0);
    }
    let k = options(m);
    let flags = m.lw(FLAGS);
    let action = m.lbu(at(k, 0x21));
    let bytes = m.lw(at(flags, 0x5c));
    m.sb(at(bytes, 7), action.wrapping_add(0x30));
    let result = assignment_flags(m);
    m.pop_frame(0x28);
    result
}

/// The type-3 moves of 800D65BC: at button `from`, hub cursor `off` = 3
/// and cursor `on` = 1 (0, 0xA).
fn swap_cursor(m: &mut Machine, from: u8, off: u32, on: u32) {
    let k = options(m);
    if m.lbu(at(k, 0x1f)) == from {
        call(m, SET_CURSOR, [off, 3, u32::MAX, u32::MAX]);
        call(m, SET_CURSOR, [on, 1, 0, 0xa]);
    }
}

/// 800D65BC: the button cursor `K+0x1F` (1..the type's count, 0 the
/// settings rows): types 0..2 confirm opens cursor 1 (0, 0xA); type 3
/// moves its four highlight cursors with down/up and confirm opens cursor
/// 1 or 2 (buttons 6..9) (0, 0x14); down/up move the cursor (leaving to
/// row 0 / row 2), sound 0x3D; confirm opens the assignment popup
/// (`K+0x23` = 1, `K+0x21` the button's action), sound 0x3C. Flag byte 8
/// is the cursor (returned).
pub fn cursor_d65bc(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let k = options(m);
    match lh(m, at(k, 0xc)) {
        0 | 2 => {
            if confirmed(m, 0x2f) {
                sound(m, 0x3c);
                call(m, SET_CURSOR, [1, 1, 0, 0xa]);
            }
        }
        1 => {
            if confirmed(m, 0x2f) {
                call(m, SET_CURSOR, [1, 1, 0, 0xa]);
                sound(m, 0x3c);
            }
        }
        3 => {
            if pressed(m, 0x130, 2) {
                sound(m, 0x3d);
                swap_cursor(m, 2, 4, 6);
                swap_cursor(m, 4, 6, 4);
                swap_cursor(m, 0xb, 5, 7);
                swap_cursor(m, 0xd, 7, 5);
            }
            if pressed(m, 0x130, 1) {
                sound(m, 0x3d);
                swap_cursor(m, 5, 4, 6);
                swap_cursor(m, 3, 6, 4);
                swap_cursor(m, 0xc, 7, 5);
            }
            if confirmed(m, 0x2f) {
                sound(m, 0x3c);
                let k = options(m);
                let analog = m.lbu(at(k, 0x1f)).wrapping_sub(6) < 4;
                if !analog {
                    call(m, SET_CURSOR, [1, 1, 0, 0x14]);
                    let k = options(m);
                    if m.lbu(at(k, 0x1f)).wrapping_sub(6) < 4 {
                        call(m, SET_CURSOR, [2, 1, 0, 0x14]);
                    }
                } else {
                    call(m, SET_CURSOR, [2, 1, 0, 0x14]);
                }
            }
        }
        _ => {}
    }
    if pressed(m, 0x130, 2) {
        let k = options(m);
        let kind = lh(m, at(k, 0xc));
        let cursor = m.lbu(at(k, 0x1f));
        let count = m.lbu(BUTTON_COUNTS.wrapping_add(kind));
        if cursor < count {
            m.sb(at(k, 0x1f), cursor + 1);
        } else {
            m.sb(at(k, 0x1f), 0);
            let k = options(m);
            m.sh(at(k, 0xe), 0);
        }
        sound(m, 0x3d);
    }
    if pressed(m, 0x130, 1) {
        sound(m, 0x3d);
        let k = options(m);
        let cursor = m.lbu(at(k, 0x1f));
        if cursor < 2 {
            m.sb(at(k, 0x1f), 0);
            let k = options(m);
            m.sh(at(k, 0xe), 2);
        } else {
            m.sb(at(k, 0x1f), cursor - 1);
        }
    }
    if confirmed(m, 0x2f) {
        sound(m, 0x3c);
        let k = options(m);
        m.sb(at(k, 0x23), 1);
        let k = options(m);
        let kind = lh(m, at(k, 0xc));
        let cursor = u32::from(m.lbu(at(k, 0x1f)));
        let order = m.lw(at(k, kind << 2).wrapping_add(0xc8));
        let index = u32::from(m.lbu(at(cursor, order).wrapping_sub(1)));
        let action = m.lbu(at(k, index.wrapping_add(kind << 4)).wrapping_add(0x24));
        m.sb(at(k, 0x21), action);
    }
    let flags = m.lw(FLAGS);
    let k = options(m);
    let bytes = m.lw(at(flags, 0x5c));
    let cursor = m.lbu(at(k, 0x1f));
    m.sb(at(bytes, 8), cursor);
    m.pop_frame(0x20);
    u32::from(cursor)
}

/// The axis flag bytes of 800D6B3C / 800D7194: 0xA..0x11 cleared, then
/// 0xA + code/2 for the type's axes (type 1 only its fourth; code 2 at
/// 0x11) and for each of its buttons with an axis action (below 8; action
/// code 2 at `button_two`).
fn axis_flags(m: &mut Machine, button_two: u32) {
    for offset in 0xau32..0x12 {
        set_flag_byte(m, offset, 0);
    }
    let k = options(m);
    let kind = lh(m, at(k, 0xc));
    if kind == 1 {
        let code = u32::from(m.lbu(at(k, 0x8b))) >> 1;
        let mut offset = code.wrapping_add(0xa);
        if offset == 0xb {
            offset = 0x11;
        }
        set_flag_byte(m, offset, kind as u8);
    } else {
        for n in 0..4u32 {
            let k = options(m);
            let kind = lh(m, at(k, 0xc));
            let code = u32::from(m.lbu(at(k, n.wrapping_add(kind << 2)).wrapping_add(0x84))) >> 1;
            let mut offset = code.wrapping_add(0xa);
            if offset == 0xb {
                offset = 0x11;
            }
            set_flag_byte(m, offset, 1);
        }
    }
    let count = |m: &mut Machine| {
        let k = options(m);
        let kind = lh(m, at(k, 0xc));
        u32::from(m.lbu(BUTTON_COUNTS.wrapping_add(kind)))
    };
    let mut button = 0u32;
    if count(m) == 0 {
        return;
    }
    loop {
        let k = options(m);
        let kind = lh(m, at(k, 0xc));
        let order = m.lw(at(k, kind << 2).wrapping_add(0xc8));
        let index = u32::from(m.lbu(at(order, button)));
        let action = u32::from(m.lbu(at(k, index.wrapping_add(kind << 4)).wrapping_add(0x24)));
        if action < 8 {
            let mut offset = (action >> 1).wrapping_add(0xa);
            if offset == 0xb {
                offset = button_two;
            }
            set_flag_byte(m, offset, 1);
        }
        button += 1;
        if !lt(button, count(m)) {
            break;
        }
    }
}

/// Parity of `K+0x1E - 1` (a signed remainder, as compiled).
fn axis_parity(m: &mut Machine, k: u32) -> u32 {
    let axis = u32::from(m.lbu(at(k, 0x1e))).wrapping_sub(1) as i32;
    (axis % 2) as u32
}

/// 800D6B3C: the inversion popup of the axis `K+0x1E`: cancel (0x20, none
/// of 0x5F) reopens the axis assignment (`K+0x22` = 1, flag bytes
/// 0x3E/0x3F cleared, sound 0x3C; returns 0); up/down on an even axis or
/// left/right on an odd one toggle its inversion (sound 0x3D, the pressed
/// bits cleared); confirm (0x10, none of 0x2F or 0x40 held) closes the
/// popups (cursors 1/2 by the axis, then both = 3), clears 0x3E/0x3F,
/// sound 0x3C (returns 0). Otherwise flag bytes 7, 0x3E and 0x3F show the
/// choice, then the axis flag bytes (button action 2 at 0x23). Returns 1.
pub fn invert_d6b3c(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let result = invert(m);
    m.pop_frame(0x20);
    result
}

fn invert(m: &mut Machine) -> u32 {
    let k = options(m);
    let context = context(m);
    let kind = lh(m, at(k, 0xc));
    let axis = u32::from(m.lbu(at(k, 0x1e))).wrapping_sub(1);
    let flag = at(k, (kind << 2).wrapping_add(0x9c)).wrapping_add(axis);
    if m.lw(at(context, 0x12c)) & 0x20 != 0 && m.lw(at(context, 0x128)) & 0x5f == 0 {
        m.sb(at(k, 0x22), 1);
        set_flag_byte(m, 0x3e, 0);
        set_flag_byte(m, 0x3f, 0);
        sound(m, 0x3c);
        clear_pressed(m, 0x12c);
        return 0;
    }
    if pressed(m, 0x130, 3) {
        let k = options(m);
        if u32::from(m.lbu(at(k, 0x1e))).wrapping_sub(1) & 1 == 0 {
            sound(m, 0x3d);
            let value = 1u32.wrapping_sub(u32::from(m.lbu(flag)));
            m.sb(flag, value as u8);
            clear_pressed(m, 0x130);
        }
    }
    if pressed(m, 0x130, 0xc) {
        let k = options(m);
        let parity = axis_parity(m, k);
        if parity == 1 {
            sound(m, 0x3d);
            let value = parity.wrapping_sub(u32::from(m.lbu(flag)));
            m.sb(flag, value as u8);
            clear_pressed(m, 0x130);
        }
    }
    let context = self::context(m);
    let press = m.lw(at(context, 0x12c)) & 0x10 != 0 && m.lw(at(context, 0x128)) & 0x2f == 0;
    let held = m.lw(at(context, 0x128));
    if press && (held >> 6) & 1 == 0 {
        let k = options(m);
        m.sb(at(k, 0x22), 0);
        let k = options(m);
        if m.lbu(at(k, 0x1e)).wrapping_sub(1) < 2 {
            call(m, SET_CURSOR, [1, 3, u32::MAX, u32::MAX]);
        }
        let k = options(m);
        if m.lbu(at(k, 0x1e)).wrapping_sub(3) < 2 {
            call(m, SET_CURSOR, [2, 3, u32::MAX, u32::MAX]);
        }
        call(m, SET_CURSOR, [1, 3, u32::MAX, u32::MAX]);
        call(m, SET_CURSOR, [2, 3, u32::MAX, u32::MAX]);
        set_flag_byte(m, 0x3e, 0);
        set_flag_byte(m, 0x3f, 0);
        sound(m, 0x3c);
        return 0;
    }
    let k = options(m);
    let flags = m.lw(FLAGS);
    let choice = u32::from(m.lbu(at(k, 0x20)));
    let bytes = m.lw(at(flags, 0x5c));
    m.sb(at(bytes, 7), (choice * 5 + 0x12) as u8);
    let k = options(m);
    let kind = lh(m, at(k, 0xc));
    // flag bytes 0x3E/0x3F: the two ends of the axis (`plain` the end shown
    // as chosen, the other its partner)
    let ends = |m: &mut Machine, first: u32, second: u32| {
        let k = options(m);
        let parity = axis_parity(m, k);
        let flags = m.lw(FLAGS);
        let bytes = m.lw(at(flags, 0x5c));
        m.sb(
            at(bytes, first),
            (parity.wrapping_mul(7)).wrapping_add(1) as u8,
        );
        let k = options(m);
        let parity = axis_parity(m, k);
        let flags = m.lw(FLAGS);
        let choice = u32::from(m.lbu(at(k, 0x20)));
        let bytes = m.lw(at(flags, 0x5c));
        let value = choice.wrapping_add(parity.wrapping_mul(7).wrapping_add(2));
        m.sb(at(bytes, second), value as u8);
    };
    let single = |m: &mut Machine, first: u32, second: u32| {
        set_flag_byte(m, first, 0xf);
        let k = options(m);
        let flags = m.lw(FLAGS);
        let choice = m.lbu(at(k, 0x20));
        let bytes = m.lw(at(flags, 0x5c));
        m.sb(at(bytes, second), choice.wrapping_add(0x10));
    };
    if kind == 1 || kind == 2 {
        if m.lbu(flag) == 0 {
            single(m, 0x3e, 0x3f);
        } else {
            single(m, 0x3f, 0x3e);
        }
    } else if m.lbu(flag) == 0 {
        ends(m, 0x3e, 0x3f);
    } else {
        ends(m, 0x3f, 0x3e);
    }
    axis_flags(m, 0x23);
    1
}

/// 800D7194: the axis assignment popup: cancel closes it (sound 0x3B);
/// down/up choose the axis `K+0x20` (0..6, sound 0x3D); confirm (sound
/// 0x3C) removes the axis from the type's axis bytes (the fifteen bytes
/// from the type's axes, as compiled: 0xC) and its two button actions
/// (0x14), assigns it to the cursor's axis and opens the inversion popup
/// for axes 0, 3 and 4 (`K+0x22` = 2) or closes. Flag byte 9 shows the
/// popup's choice, 7 its help text, then the axis flag bytes (button
/// action 2 at 0x11). Returns 0.
pub fn assign_axis_d7194(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let k = options(m);
    let kind = lh(m, at(k, 0xc));
    let axes = at(k, (kind << 2).wrapping_add(0x84));
    let buttons = at(k, (kind << 4).wrapping_add(0x24));
    if cancelled(m) {
        m.sb(at(k, 0x22), 0);
        call(m, SET_CURSOR, [1, 3, u32::MAX, u32::MAX]);
        call(m, SET_CURSOR, [2, 3, u32::MAX, u32::MAX]);
        sound(m, 0x3b);
        clear_pressed(m, 0x12c);
    }
    if pressed(m, 0x130, 2) {
        sound(m, 0x3d);
        let k = options(m);
        let choice = m.lbu(at(k, 0x20));
        m.sb(at(k, 0x20), if choice < 6 { choice + 1 } else { 0 });
    }
    if pressed(m, 0x130, 1) {
        sound(m, 0x3d);
        let k = options(m);
        let choice = m.lbu(at(k, 0x20));
        m.sb(at(k, 0x20), if choice != 0 { choice - 1 } else { 6 });
    }
    if confirmed(m, 0x6f) {
        sound(m, 0x3c);
        for n in 0..0xfu32 {
            let k = options(m);
            let axis = at(axes, n);
            let code = u32::from(m.lbu(at(k, 0x20))) << 1;
            if u32::from(m.lbu(axis)) == code {
                m.sb(axis, 0xc);
            }
        }
        let count = |m: &mut Machine| {
            let k = options(m);
            let kind = lh(m, at(k, 0xc));
            u32::from(m.lbu(BUTTON_COUNTS.wrapping_add(kind)))
        };
        if count(m) != 0 {
            let mut button = 0u32;
            loop {
                let k = options(m);
                let code = u32::from(m.lbu(at(k, 0x20))) << 1;
                if lt(code, 8) {
                    for end in 0..2u32 {
                        let k = options(m);
                        let kind = lh(m, at(k, 0xc));
                        let order = m.lw(at(k, kind << 2).wrapping_add(0xc8));
                        let index = u32::from(m.lbu(at(order, button)));
                        let action = at(buttons, index);
                        let code = (u32::from(m.lbu(at(k, 0x20))) << 1).wrapping_add(end);
                        if u32::from(m.lbu(action)) == code {
                            m.sb(action, 0x14);
                        }
                    }
                }
                button += 1;
                if !lt(button, count(m)) {
                    break;
                }
            }
        }
        let k = options(m);
        let axis = u32::from(m.lbu(at(k, 0x1e)));
        let code = m.lbu(at(k, 0x20)) << 1;
        m.sb(at(axis, axes).wrapping_sub(1), code);
        let k = options(m);
        let choice = m.lbu(at(k, 0x20));
        if choice == 0 || choice == 3 || choice == 4 {
            let k = options(m);
            m.sb(at(k, 0x22), 2);
        } else {
            m.sb(at(k, 0x22), 0);
            call(m, SET_CURSOR, [1, 3, u32::MAX, u32::MAX]);
            call(m, SET_CURSOR, [2, 3, u32::MAX, u32::MAX]);
        }
    }
    let k = options(m);
    if m.lbu(at(k, 0x22)) != 0 {
        let flags = m.lw(FLAGS);
        let choice = m.lbu(at(k, 0x20));
        let bytes = m.lw(at(flags, 0x5c));
        m.sb(at(bytes, 9), choice.wrapping_add(1));
    } else {
        set_flag_byte(m, 9, 0);
    }
    let k = options(m);
    let choice = u32::from(m.lbu(at(k, 0x20)));
    let text = if choice == 6 { 0x44 } else { choice * 5 + 0x11 };
    set_flag_byte(m, 7, text as u8);
    axis_flags(m, 0x11);
    m.pop_frame(0x20);
    0
}

/// The axis cursor moves of 800D7750 for types 0 and 3: panel 2 entry
/// `entry` (9 or 0xB): down/up move `K+0x1E` (0..4, leaving to row 0 /
/// row 2) with sound 0x3D, and the entry follows the cursor (800DED68: 0,
/// +0x28, -0x28).
fn axis_moves(m: &mut Machine, entry: u32) {
    let follow = |m: &mut Machine, places: [(u8, u32); 3]| {
        for (axis, dx) in places {
            let k = options(m);
            if m.lbu(at(k, 0x1e)) == axis {
                call(m, PANEL_MOVE, [2, entry, dx, 0]);
            }
        }
    };
    if pressed(m, 0x130, 2) {
        sound(m, 0x3d);
        let k = options(m);
        let axis = m.lbu(at(k, 0x1e));
        if axis < 4 {
            m.sb(at(k, 0x1e), axis + 1);
        } else {
            m.sb(at(k, 0x1e), 0);
            let k = options(m);
            m.sh(at(k, 0xe), 0);
        }
    }
    follow(m, [(0, 0), (1, 0x28), (3, (-0x28i32) as u32)]);
    if pressed(m, 0x130, 1) {
        sound(m, 0x3d);
        let k = options(m);
        let axis = m.lbu(at(k, 0x1e));
        if axis < 2 {
            m.sb(at(k, 0x1e), 0);
            let k = options(m);
            m.sh(at(k, 0xe), 2);
        } else {
            m.sb(at(k, 0x1e), axis - 1);
        }
    }
    follow(m, [(0, 0), (2, 0x28), (4, (-0x28i32) as u32)]);
}

/// 800D7750: the analog axis cursor `K+0x1E`: types 0 and 3 move it over
/// their four axes ([`axis_moves`]) and confirm (sound 0x3C) opens the
/// axis assignment popup (cursors 1 / 2 by the axis = 1 (0, 0xA), `K+0x22`
/// = 1, `K+0x20` the axis's code / 2); type 1 has one axis (down/up leave
/// to row 0 / 2, confirm opens cursor 1 and the popup); type 2 none. Flag
/// byte 7 is the axis's help text, 8 the cursor. Returns the cursor (or
/// the flag bytes' address for type 1's fourth axis).
pub fn axis_cursor_d7750(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let result = axis_cursor(m);
    m.pop_frame(0x20);
    result
}

fn axis_cursor(m: &mut Machine) -> u32 {
    let kind = kind(m);
    let open = match kind {
        0 | 3 => {
            axis_moves(m, if kind == 0 { 9 } else { 0xb });
            if confirmed(m, 0x2f) {
                sound(m, 0x3c);
                let k = options(m);
                if m.lbu(at(k, 0x1e)).wrapping_sub(1) < 2 {
                    call(m, SET_CURSOR, [1, 1, 0, 0xa]);
                }
                let k = options(m);
                if m.lbu(at(k, 0x1e)).wrapping_sub(3) < 2 {
                    call(m, SET_CURSOR, [2, 1, 0, 0xa]);
                }
                let k = options(m);
                m.sb(at(k, 0x22), 1);
                true
            } else {
                false
            }
        }
        1 => {
            if pressed(m, 0x130, 2) {
                sound(m, 0x3d);
                let k = options(m);
                m.sb(at(k, 0x1e), 0);
                let k = options(m);
                m.sh(at(k, 0xe), 0);
            }
            if pressed(m, 0x130, 1) {
                sound(m, 0x3d);
                let k = options(m);
                m.sb(at(k, 0x1e), 0);
                let k = options(m);
                m.sh(at(k, 0xe), 2);
            }
            if confirmed(m, 0x2f) {
                sound(m, 0x3c);
                call(m, SET_CURSOR, [1, 1, 0, 0xa]);
                let k = options(m);
                m.sb(at(k, 0x22), kind as u8);
                true
            } else {
                false
            }
        }
        _ => false,
    };
    if open {
        let k = options(m);
        let kind = lh(m, at(k, 0xc));
        let axis = u32::from(m.lbu(at(k, 0x1e)));
        let code = m.lbu(at(k, axis.wrapping_add((kind << 2).wrapping_sub(1))).wrapping_add(0x84));
        m.sb(at(k, 0x20), code >> 1);
    }
    let k = options(m);
    let kind = lh(m, at(k, 0xc));
    let axis = u32::from(m.lbu(at(k, 0x1e)));
    let code =
        u32::from(m.lbu(at(k, axis.wrapping_add((kind << 2).wrapping_sub(1))).wrapping_add(0x84)));
    let text = if code == 0xc {
        0x44
    } else {
        (code >> 1) * 5 + 0x11
    };
    set_flag_byte(m, 7, text as u8);
    let k = options(m);
    let kind = lh(m, at(k, 0xc));
    if kind == 1 && m.lbu(at(k, 0x1e)) == 4 {
        let flags = m.lw(FLAGS);
        let bytes = m.lw(at(flags, 0x5c));
        m.sb(at(bytes, 8), kind as u8);
        return bytes;
    }
    let flags = m.lw(FLAGS);
    let k = options(m);
    let bytes = m.lw(at(flags, 0x5c));
    let axis = m.lbu(at(k, 0x1e));
    m.sb(at(bytes, 8), axis);
    u32::from(axis)
}

/// The routines of this file by JP address, while module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    let _ = args;
    Some(match target {
        0x800d_3388 => labels_d3388(m),
        0x800d_38ec => labels_d38ec(m),
        0x800d_3be0 => labels_d3be0(m),
        0x800d_3fa0 => labels_d3fa0(m),
        0x800d_4394 => labels_d4394(m),
        0x800d_4430 => analog_d4430(m),
        0x800d_47dc => analog_d47dc(m),
        0x800d_48f8 => analog_d48f8(m),
        0x800d_4ca4 => analog_d4ca4(m),
        0x800d_4d20 => start_d4d20(m),
        0x800d_4f78 => rows_d4f78(m),
        0x800d_5bd4 => toggle_d5bd4(m),
        0x800d_5d64 => option_popup_d5d64(m),
        0x800d_60fc => assign_d60fc(m),
        0x800d_65bc => cursor_d65bc(m),
        0x800d_6b3c => invert_d6b3c(m),
        0x800d_7194 => assign_axis_d7194(m),
        0x800d_7750 => axis_cursor_d7750(m),
        _ => return None,
    })
}
