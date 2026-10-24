//! Overlay 1 (front-end menus): the menu screens on the address-preserving
//! machine: language selection (menu state 1), title (2), main menu (3),
//! difficulty (4 and 7) and the load screen (5). Each screen is an
//! initialization / update callback pair of the table at `0x800D5940`;
//! the language, title, difficulty and load updates run phase callbacks
//! from tables in the overlay data (`0x800D878C`, `0x800D87A4`,
//! `0x800D87C4`, `0x800D8820`).
//!
//! Screen states (pointers in the overlay data): language `[0x800D8774]`
//! {phase, timer, selection}, title `[0x800D87A0]` {phase, timer, idle
//! timer}, main menu `[0x800D87B4]` {selection, choice count}, difficulty
//! `[0x800D87C0]` {phase, selection}, load screen `[0x800D87D8]` {+0 flags
//! (bit 0: probe the card each frame), +4/+6 phase group and phase
//! (halfwords), +8 card status per slot, +0x10 slot, +0x14 choice, +0x18
//! choice count}.
//!
//! Input is the context's processed pad words: +0x12C pressed, +0x130
//! repeat. Sounds are 8002D724 requests (0x3B cancel, 0x3C confirm, 0x3D
//! move, 0x58 start, 0x30 refused).
//!
//! Callbacks reached through `jalr` receive the caller's registers: the
//! load screen's per-slot texts (`800D23C0`, `800D25C0`) store a stale
//! `a1` as a visibility byte, so those values are passed along exactly.
use crate::{
    dispatch,
    machine::Machine,
    ov1::{MENU, context, lbu, lh, lhu, link, menu, off},
    ov1_card, ov1_load, ov1_script, ov1_text,
};

const LANGUAGE: u32 = 0x800d_8774;
const LANGUAGE_LABELS: u32 = 0x800d_8778;
const LANGUAGE_PHASES: u32 = 0x800d_878c;
const TITLE: u32 = 0x800d_87a0;
const TITLE_PHASES: u32 = 0x800d_87a4;
/// 0x800CAF98 / 0x800CAF9C: the title prompt's pulse colours.
const TITLE_PULSE: [u32; 2] = [0x800c_af98, 0x800c_af9c];
const MAIN: u32 = 0x800d_87b4;
const DIFFICULTY: u32 = 0x800d_87c0;
const DIFFICULTY_PHASES: u32 = 0x800d_87c4;
const LOAD: u32 = 0x800d_87d8;
const LOAD_PHASES: u32 = 0x800d_8820;
/// 0x800D8344: 12-byte mission records (+8 name string).
const MISSION_NAMES: u32 = 0x800d_8344;
/// 0x800CAFA0 / 0x800CAFA8: `sprintf` formats of the saved play time.
const TIME_FORMAT: u32 = 0x800c_afa0;
const HOURS_FORMAT: u32 = 0x800c_afa8;
/// 800ACD28 sprintf / 800AC328 strlen / 800AC1B8 strcat (libc, `sdk.rs`).
const STRCAT: u32 = 0x800a_c1b8;
/// 8002D724 (`script.rs`): request sound `a0`.
const PLAY_SOUND: u32 = 0x8002_d724;

fn sound(m: &mut Machine, id: u32) {
    dispatch::call(m, PLAY_SOUND, [id, 0, 0, 0]);
}
fn pressed(m: &mut Machine) -> u32 {
    let context = context(m);
    m.lw(off(context, 0x12c))
}
fn state(m: &mut Machine, pointer: u32) -> u32 {
    m.lw(pointer)
}

/// SetDrawMode `(p, 0, 0, tpage, NULL)` from the menu packet area into the
/// menu ordering table's entry (the fifth argument at `frame + 0x10`).
fn draw_mode(m: &mut Machine, frame: u32, tpage: u32) {
    let block = menu(m);
    let packet = m.lw(off(block, 0x8090));
    m.sw(off(block, 0x8090), packet.wrapping_add(0xc));
    m.sw(off(frame, 0x10), 0);
    dispatch::call(m, ov1_script::SET_DRAW_MODE, [packet, 0, 0, tpage]);
    let block = menu(m);
    let ot = m.lw(off(block, 0x8c));
    link(m, ot, packet);
}

/// The full-screen subtractive fade of the language exit (`800D11B4`) and
/// the title (`800D1578`): DR_MODE 0, a blended 320x511 tile of grey
/// `shade` (0x62), DR_MODE 0x40 (subtractive), linked in that order.
fn fade(m: &mut Machine, frame: u32, shade: u32) {
    draw_mode(m, frame, 0);
    let block = menu(m);
    let packet = m.lw(off(block, 0x8090));
    m.sw(off(block, 0x8090), packet.wrapping_add(0x10));
    m.sb(off(packet, 3), 3);
    m.sb(off(packet, 7), 0x62);
    m.sh(off(packet, 0xc), 0x140);
    for k in 4..7 {
        m.sb(off(packet, k), shade as u8);
    }
    m.sh(off(packet, 8), 0);
    m.sh(off(packet, 0xa), 0);
    m.sh(off(packet, 0xe), 0x1ff);
    let block = menu(m);
    let ot = m.lw(off(block, 0x8c));
    link(m, ot, packet);
    draw_mode(m, frame, 0x40);
}

/// `x * 255 / 25` by the compiler's reciprocal multiplication.
fn shade(x: u32) -> u32 {
    let product = (x << 8).wrapping_sub(x);
    let hi = ((i64::from(product as i32) * 0x51eb_851f) >> 32) as u32;
    ((hi as i32 >> 3) as u32).wrapping_sub(((product as i32) >> 31) as u32)
}

// ---- language (menu state 1) ---------------------------------------------------------

/// 800D0E78: the five language rows (labels `0x800D8778`, font 2): row
/// `i` at y 120 + 59 i, shifted by -d * d with d = max(timer - 5 + i, 0),
/// the selected one 8 further right with style 1 (others 2); visual slots
/// 1..5 take the shift. Returns the next packet address.
pub fn language_rows(m: &mut Machine, packet: u32, ot: u32, selection: u32, timer: u32) -> u32 {
    let frame = m.push_frame(0x48);
    let mut packet = packet;
    for i in 0..5u32 {
        let d = timer.wrapping_sub(5).wrapping_add(i);
        let d = if (d as i32) < 0 { 0 } else { d };
        let shift = d.wrapping_mul(d.wrapping_neg());
        let selected = selection == i;
        let x = if selected {
            shift.wrapping_add(8)
        } else {
            shift
        };
        ov1_script::set_style(m, i + 1, if selected { 1 } else { 2 });
        ov1_script::set_offset(m, i + 1, shift, 0);
        let label = m.lw(off(LANGUAGE_LABELS, i << 2));
        m.sw(off(frame, 0x10), x.wrapping_add(0x50));
        m.sw(off(frame, 0x14), (i * 59).wrapping_add(0x78));
        m.sw(off(frame, 0x18), 0x0080_8080);
        packet = ov1_text::draw_text(m, 2, label, ot, packet);
    }
    m.pop_frame(0x48);
    packet
}

/// Draw the rows with the language state's selection and `timer`.
fn draw_language(m: &mut Machine, timer: u32) {
    let block = menu(m);
    let language = state(m, LANGUAGE);
    let packet = m.lw(off(block, 0x8090));
    let ot = m.lw(off(block, 0x8c));
    let selection = m.lw(off(language, 8));
    let packet = language_rows(m, packet, ot, selection, timer);
    let block = menu(m);
    m.sw(off(block, 0x8090), packet);
}

/// 800D0FA4 (language phase 0): slide in; 1 when the timer has run out,
/// otherwise count it down.
pub fn language_intro(m: &mut Machine) -> u32 {
    m.push_frame(0x28);
    let language = state(m, LANGUAGE);
    let timer = m.lw(off(language, 4));
    draw_language(m, timer);
    let result = if (timer as i32) > 0 {
        let language = state(m, LANGUAGE);
        let t = m.lw(off(language, 4));
        m.sw(off(language, 4), t.wrapping_sub(1));
        0
    } else {
        1
    };
    m.pop_frame(0x28);
    result
}

/// 800D103C (language phase 1): draw, then confirm (0x10: store the
/// language ID of the row, `0 2 1 4 3`, sound 0x3C, 1) or move down (2)
/// / up (1) with wrap-around and sound 0x3D.
pub fn language_input(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    draw_language(m, 0);
    let input = pressed(m);
    let result = if input & 0x10 != 0 {
        let language = state(m, LANGUAGE);
        let selection = m.lw(off(language, 8));
        let id = match selection {
            0 => Some(0),
            1 => Some(2),
            2 => Some(1),
            3 => Some(4),
            4 => Some(3),
            _ => None,
        };
        if let Some(id) = id {
            let context = context(m);
            m.sw(off(context, 0x38c), id);
        }
        sound(m, 0x3c);
        1
    } else if input & 2 != 0 {
        let language = state(m, LANGUAGE);
        let selection = m.lw(off(language, 8));
        let next = if (selection as i32) < 4 {
            selection.wrapping_add(1)
        } else {
            0
        };
        m.sw(off(language, 8), next);
        sound(m, 0x3d);
        0
    } else if input & 1 != 0 {
        let language = state(m, LANGUAGE);
        let selection = m.lw(off(language, 8));
        let next = if (selection as i32) > 0 {
            selection.wrapping_sub(1)
        } else {
            4
        };
        m.sw(off(language, 8), next);
        sound(m, 0x3d);
        0
    } else {
        0
    };
    m.pop_frame(0x20);
    result
}

/// 800D11B4 (language phase 2): fade out (shade timer * 255 / 25) over the
/// rows; at timer 25 select page 0 and return 2.
pub fn language_exit(m: &mut Machine) -> u32 {
    let frame = m.push_frame(0x38);
    let language = state(m, LANGUAGE);
    let timer = m.lw(off(language, 4));
    fade(m, frame, shade(timer));
    draw_language(m, 0);
    let language = state(m, LANGUAGE);
    let timer = m.lw(off(language, 4));
    let result = if (timer as i32) < 0x19 {
        m.sw(off(language, 4), timer.wrapping_add(1));
        0
    } else {
        ov1_script::select_page(m, 0);
        2
    };
    m.pop_frame(0x38);
    result
}

/// 800D13E4 (menu state 1 initialization): the selection from the stored
/// language ID (inverse table; other IDs kept), timer 25, phase 0, page 2,
/// rows 1..5 off screen (x -625). Returns 1.
pub fn language_init(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let context = context(m);
    let language = state(m, LANGUAGE);
    let id = m.lw(off(context, 0x38c));
    m.sw(off(language, 8), id);
    let id = m.lw(off(context, 0x38c));
    let selection = match id {
        0 => Some(0),
        1 => Some(2),
        2 => Some(1),
        3 => Some(4),
        4 => Some(3),
        _ => None,
    };
    if let Some(selection) = selection {
        let language = state(m, LANGUAGE);
        m.sw(off(language, 8), selection);
    }
    let language = state(m, LANGUAGE);
    m.sw(off(language, 4), 0x19);
    m.sw(language, 0);
    ov1_script::select_page(m, 2);
    for slot in 1..=5 {
        ov1_script::set_offset(m, slot, 0xffff_fd8f, 0);
    }
    m.pop_frame(0x18);
    1
}

/// 800D14E0 (menu state 1 update): run the phase; bit 0 advances the
/// phase, else bit 1 records the language (+0x970 bit 0) and returns 0x100.
pub fn language_update(m: &mut Machine, args: [u32; 4]) -> u32 {
    m.push_frame(0x18);
    let language = state(m, LANGUAGE);
    let phase = m.lw(language);
    let callback = m.lw(off(LANGUAGE_PHASES, phase << 2));
    let result = dispatch::call(m, callback, args);
    let value = if result & 1 != 0 {
        let language = state(m, LANGUAGE);
        let phase = m.lw(language);
        m.sw(language, phase.wrapping_add(1));
        0
    } else if result & 2 != 0 {
        let context = context(m);
        let progress = m.lw(off(context, 0x970));
        m.sw(off(context, 0x970), progress | 1);
        0x100
    } else {
        0
    };
    m.pop_frame(0x18);
    value
}

// ---- title (menu state 2) ------------------------------------------------------------

/// 800D1578 (title phase 0): text 0 on page 3 under a fade from black
/// (shade (25 - timer) * 255 / 25); 1 after 25 frames.
pub fn title_fade(m: &mut Machine) -> u32 {
    let frame = m.push_frame(0x38);
    ov1_text::clear_visibility(m);
    ov1_text::set_visible(m, 0, 1);
    ov1_script::select_page(m, 3);
    let title = state(m, TITLE);
    let timer = m.lw(off(title, 4));
    fade(m, frame, shade(0x19u32.wrapping_sub(timer)));
    let title = state(m, TITLE);
    let timer = m.lw(off(title, 4));
    let result = if (timer as i32) < 0x19 {
        m.sw(off(title, 4), timer.wrapping_add(1));
        0
    } else {
        1
    };
    m.pop_frame(0x38);
    result
}

/// 800D1794 (title phase 1): START (0x40) plays 0x58 and returns 4;
/// otherwise the idle timer counts down, 2 once it is negative.
pub fn title_wait(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let result = if pressed(m) & 0x40 != 0 {
        sound(m, 0x58);
        4
    } else {
        let title = state(m, TITLE);
        let idle = m.lw(off(title, 8));
        if (idle as i32) < 0 {
            2
        } else {
            m.sw(off(title, 8), idle.wrapping_sub(1));
            0
        }
    };
    m.pop_frame(0x18);
    result
}

/// 800D1800: the 36 mission emblems in three rows: styles 1..36, 37..72,
/// 73..108 from context +0xC61, +0xC9D, +0xCD9 (0 shown as style 5).
pub fn title_emblems(m: &mut Machine) {
    m.push_frame(0x28);
    let context = context(m);
    let base = off(context, 0x960);
    for i in 0..0x24u32 {
        let entry = off(base, i + 1);
        for (offset, slot) in [(0x300, i + 1), (0x33c, i + 0x25), (0x378, i + 0x49)] {
            let value = lbu(m, off(entry, offset));
            let style = if value == 0 { 5 } else { value };
            ov1_script::set_style(m, slot, style);
        }
    }
    m.pop_frame(0x28);
}

/// 800D18B0 (menu state 2 initialization): phase and timer 0, page 0, no
/// texts, idle timer 1000, emblems. Returns 1.
pub fn title_init(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let title = state(m, TITLE);
    m.sw(title, 0);
    m.sw(off(title, 4), 0);
    ov1_script::select_page(m, 0);
    ov1_text::clear_visibility(m);
    let title = state(m, TITLE);
    m.sw(off(title, 8), 1000);
    title_emblems(m);
    m.pop_frame(0x18);
    1
}

/// 800D1900 (menu state 2 update): run the phase; bit 0 advances it,
/// bit 2 returns 0x100 (main menu), bit 1 closes the card events and
/// returns 8 (attract mode); otherwise pulse colour 0x78 and return 0.
pub fn title_update(m: &mut Machine, args: [u32; 4]) -> u32 {
    let frame = m.push_frame(0x28);
    let title = state(m, TITLE);
    let phase = m.lw(title);
    let callback = m.lw(off(TITLE_PHASES, phase << 2));
    let result = dispatch::call(m, callback, args);
    let value = if result & 1 != 0 {
        let title = state(m, TITLE);
        let phase = m.lw(title);
        m.sw(title, phase.wrapping_add(1));
        None
    } else if result & 4 != 0 {
        Some(0x100)
    } else if result & 2 != 0 {
        ov1_card::close_events(m);
        Some(8)
    } else {
        None
    };
    let value = value.unwrap_or_else(|| {
        crate::ov1::copy_word(m, TITLE_PULSE[0], off(frame, 0x10));
        crate::ov1::copy_word(m, TITLE_PULSE[1], off(frame, 0x18));
        ov1_script::pulse_color(m, 0x78, off(frame, 0x10), off(frame, 0x18));
        0
    });
    m.pop_frame(0x28);
    value
}

// ---- main menu (menu state 3) ---------------------------------------------------------

/// 800D19E4: styles 1..3 and fonts of texts 12..14 for the selection
/// (selected 1/2; others 2/3; the third choice 3/4 while progress bit 2
/// (+0x970) is clear). Other selections change nothing.
pub fn main_styles(m: &mut Machine) {
    m.push_frame(0x18);
    let main = state(m, MAIN);
    let context = context(m);
    let selection = m.lw(main);
    let progress = m.lw(off(context, 0x970));
    if selection < 3 {
        for i in 0..3 {
            let (style, font) = if selection == i {
                (1, 2)
            } else if i == 2 && progress & 2 == 0 {
                (3, 4)
            } else {
                (2, 3)
            };
            ov1_script::set_style(m, i + 1, style);
            ov1_text::set_font(m, i + 12, font);
        }
    }
    m.pop_frame(0x18);
}

/// 800D1B58 (menu state 3 initialization): page 4, texts 1 and 12..14,
/// selection from the menu block's choice (+6), two choices (three with
/// progress bit 2), styles, styles 4/5 = 1, stream 0x81C. Returns 1.
pub fn main_init(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let context = context(m);
    let progress = m.lw(off(context, 0x970)) & 2;
    ov1_script::select_page(m, 4);
    ov1_text::clear_visibility(m);
    for text in [1, 12, 13, 14] {
        ov1_text::set_visible(m, text, 1);
    }
    let block = m.lw(MENU);
    let main = state(m, MAIN);
    let choice = lh(m, off(block, 6));
    m.sw(main, choice);
    m.sw(off(main, 4), if progress != 0 { 3 } else { 2 });
    main_styles(m);
    ov1_script::set_style(m, 4, 1);
    ov1_script::set_style(m, 5, 1);
    ov1_load::select_stream(m, 0x81c);
    ov1_load::start_stream(m);
    m.pop_frame(0x18);
    1
}

/// 800D1C1C (menu state 3 update): cancel (0x20: sound 0x3B, stop the
/// stream, 0x200), confirm (0x10: choice to menu +6, sound 0x3C, 0x100),
/// else repeat-input up/down (+0x130 bits 0/1) clamped, sound 0x3D on a
/// move, and the styles.
pub fn main_update(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let input = pressed(m);
    let result = if input & 0x20 != 0 {
        sound(m, 0x3b);
        ov1_load::stop_stream(m);
        0x200
    } else if input & 0x10 != 0 {
        let main = state(m, MAIN);
        let block = m.lw(MENU);
        let choice = lhu(m, main);
        m.sh(off(block, 6), choice as u16);
        sound(m, 0x3c);
        0x100
    } else {
        let context = context(m);
        let repeat = m.lw(off(context, 0x130));
        let main = state(m, MAIN);
        if repeat & 1 != 0 {
            let selection = m.lw(main);
            if (selection as i32) > 0 {
                m.sw(main, selection.wrapping_sub(1));
                sound(m, 0x3d);
            }
        } else if repeat & 2 != 0 {
            let count = m.lw(off(main, 4));
            let selection = m.lw(main);
            if (selection as i32) < (count.wrapping_sub(1) as i32) {
                m.sw(main, selection.wrapping_add(1));
                sound(m, 0x3d);
            }
        }
        main_styles(m);
        0
    };
    m.pop_frame(0x18);
    result
}

// ---- difficulty (menu states 4 and 7) ---------------------------------------------------

/// 800D1D0C: reset the per-game progress: context +0xD50..+0xD8B bytes,
/// +0xD8C and +0xE7C words (60 each) cleared, +0xBE8 60 halfwords 45000,
/// +0x9B8..+0x9CF cleared (descending), +0x9B9 = 1, +0xF6C = 0.
pub fn reset_progress(m: &mut Machine) {
    let context = context(m);
    let base = off(context, 0x960);
    for i in 0..0x3c {
        m.sb(off(base, 0x3f0 + i), 0);
        m.sh(off(base, 0x288 + 2 * i), 0xafc8);
        m.sw(off(base, 0x51c + 4 * i), 0);
        m.sw(off(base, 0x42c + 4 * i), 0);
    }
    for i in (0..0x18).rev() {
        m.sb(off(base, 0x58 + i), 0);
    }
    m.sb(off(base, 0x59), 1);
    m.sw(off(base, 0x60c), 0);
}

/// 800D1D78 (difficulty phase 0): returns 1.
pub fn difficulty_start() -> u32 {
    1
}

/// 800D1D80 (difficulty phase 1): confirm (0x10: 0x3C, 1), cancel (0x20:
/// 0x3B, 4), down (2) / up (1) clamped to 0..2 with 0x3D; then the styles
/// (1..3 = 2, fonts 24..26 = 3, the selection 1 / 2).
pub fn difficulty_input(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let input = pressed(m);
    let result = if input & 0x10 != 0 {
        sound(m, 0x3c);
        1
    } else if input & 0x20 != 0 {
        sound(m, 0x3b);
        4
    } else {
        let difficulty = state(m, DIFFICULTY);
        if input & 2 != 0 {
            let selection = m.lw(off(difficulty, 4));
            if (selection as i32) < 2 {
                m.sw(off(difficulty, 4), selection.wrapping_add(1));
                sound(m, 0x3d);
            }
        } else if input & 1 != 0 {
            let selection = m.lw(off(difficulty, 4));
            if (selection as i32) > 0 {
                m.sw(off(difficulty, 4), selection.wrapping_sub(1));
                sound(m, 0x3d);
            }
        }
        for i in 0..3 {
            ov1_script::set_style(m, i + 1, 2);
            ov1_text::set_font(m, i + 0x18, 3);
        }
        let difficulty = state(m, DIFFICULTY);
        let selection = m.lw(off(difficulty, 4));
        ov1_script::set_style(m, selection.wrapping_add(1), 1);
        let difficulty = state(m, DIFFICULTY);
        let selection = m.lw(off(difficulty, 4));
        ov1_text::set_font(m, selection.wrapping_add(0x18), 2);
        0
    };
    m.pop_frame(0x20);
    result
}

/// 800D1EAC (difficulty phase 2): store the difficulty (+0x398), reset
/// the progress, mission 1 (+0x390). Returns 2.
pub fn difficulty_commit(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let difficulty = state(m, DIFFICULTY);
    let context = context(m);
    let selection = m.lw(off(difficulty, 4));
    m.sw(off(context, 0x398), selection);
    reset_progress(m);
    let context = self::context(m);
    m.sh(off(context, 0x390), 1);
    m.pop_frame(0x18);
    2
}

/// 800D1EF4 (menu states 4/7 initialization): page 4, style 5 = 3, texts
/// 10 and 24..26, context +0x961 = 6. Returns 1 (phase and selection are
/// kept).
pub fn difficulty_init(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    ov1_script::select_page(m, 4);
    ov1_script::set_style(m, 5, 3);
    ov1_text::clear_visibility(m);
    for text in [0xa, 0x18, 0x19, 0x1a] {
        ov1_text::set_visible(m, text, 1);
    }
    let context = context(m);
    m.sb(off(context, 0x961), 6);
    m.pop_frame(0x18);
    1
}

/// 800D1F68 (menu states 4/7 update): run the phase; bit 0 advances it;
/// bit 1 returns 0x100, else `(result << 7) & 0x200` (cancel).
pub fn difficulty_update(m: &mut Machine, args: [u32; 4]) -> u32 {
    m.push_frame(0x18);
    let difficulty = state(m, DIFFICULTY);
    let phase = m.lw(difficulty);
    let callback = m.lw(off(DIFFICULTY_PHASES, phase << 2));
    let result = dispatch::call(m, callback, args);
    if result & 1 != 0 {
        let difficulty = state(m, DIFFICULTY);
        let phase = m.lw(difficulty);
        m.sw(difficulty, phase.wrapping_add(1));
    }
    let value = if result & 2 != 0 {
        0x100
    } else {
        (result << 7) & 0x200
    };
    m.pop_frame(0x18);
    value
}

// ---- load screen (menu state 5) ----------------------------------------------------------

/// The card status word of the load screen's current slot.
fn slot_status(m: &mut Machine) -> u32 {
    let load = state(m, LOAD);
    let slot = m.lw(off(load, 0x10));
    m.lw(off(off(load, slot << 2), 8))
}

/// Cancel (0x20): sound 0x3B, 8; else 0.
fn cancel(m: &mut Machine) -> u32 {
    if pressed(m) & 0x20 != 0 {
        sound(m, 0x3b);
        8
    } else {
        0
    }
}

/// 800D1FF0 (load group 0, phase 0): style 4 = 1; with card status bit 0
/// show text 2 and wait (0), otherwise 1.
pub fn load_probe(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    ov1_script::set_style(m, 4, 1);
    let load = state(m, LOAD);
    let result = if m.lw(off(load, 8)) & 1 != 0 {
        ov1_text::clear_visibility(m);
        ov1_text::set_visible(m, 2, 1);
        0
    } else {
        1
    };
    m.pop_frame(0x18);
    result
}

/// 800D204C (phase): card status bit 2 → 1, else text 3 and 2.
pub fn load_check_card(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let load = state(m, LOAD);
    let result = if m.lw(off(load, 8)) & 4 != 0 {
        1
    } else {
        ov1_text::clear_visibility(m);
        ov1_text::set_visible(m, 3, 1);
        2
    };
    m.pop_frame(0x18);
    result
}

/// 800D2098 (phase): style 4 = 0; card status bit 2 → 4, else cancel.
pub fn load_wait_card(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    ov1_script::set_style(m, 4, 0);
    let load = state(m, LOAD);
    let result = if m.lw(off(load, 8)) & 4 != 0 {
        4
    } else {
        cancel(m)
    };
    m.pop_frame(0x18);
    result
}

/// 800D2108 (phase): slot status bit 1 → text 4 and 2; else bit 2 → 1,
/// otherwise 4.
pub fn load_check_slot(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let status = slot_status(m);
    let result = if status & 2 != 0 {
        ov1_text::clear_visibility(m);
        ov1_text::set_visible(m, 4, 1);
        2
    } else if status & 4 != 0 {
        1
    } else {
        4
    };
    m.pop_frame(0x18);
    result
}

/// 800D2178 / 800D2488 / 800D251C (phases): style 4 = 0 (800D2264 and
/// 800D234C skip it); slot status bit 2 clear → 4, else cancel.
fn load_wait_slot(m: &mut Machine, style: bool) -> u32 {
    m.push_frame(0x18);
    if style {
        ov1_script::set_style(m, 4, 0);
    }
    let result = if slot_status(m) & 4 == 0 {
        4
    } else {
        cancel(m)
    };
    m.pop_frame(0x18);
    result
}

/// 800D2178 (phase): see [`load_wait_slot`].
pub fn load_wait_slot_2178(m: &mut Machine) -> u32 {
    load_wait_slot(m, true)
}
/// 800D2264 (phase).
pub fn load_wait_slot_2264(m: &mut Machine) -> u32 {
    load_wait_slot(m, false)
}
/// 800D234C (phase).
pub fn load_wait_slot_234c(m: &mut Machine) -> u32 {
    load_wait_slot(m, false)
}
/// 800D2488 (phase).
pub fn load_wait_slot_2488(m: &mut Machine) -> u32 {
    load_wait_slot(m, true)
}
/// 800D251C (phase).
pub fn load_wait_slot_251c(m: &mut Machine) -> u32 {
    load_wait_slot(m, true)
}

/// 800D21FC (phase): slot status bit 3 → 1; bit 2 → text 5 and 2; else 4.
pub fn load_check_format(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let status = slot_status(m);
    let result = if status & 8 != 0 {
        1
    } else if status & 4 != 0 {
        ov1_text::clear_visibility(m);
        ov1_text::set_visible(m, 5, 1);
        2
    } else {
        4
    };
    m.pop_frame(0x18);
    result
}

/// 800D22D8 (phase): slot status bit 4 → 1; bit 2 → text 6, style 4 = 0
/// and 2; else 4.
pub fn load_check_file(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let status = slot_status(m);
    let result = if status & 0x10 != 0 {
        1
    } else if status & 4 != 0 {
        ov1_text::clear_visibility(m);
        ov1_text::set_visible(m, 6, 1);
        ov1_script::set_style(m, 4, 0);
        2
    } else {
        4
    };
    m.pop_frame(0x18);
    result
}

/// 800D23C0 (phase): text 11, read the slot's save (800CE0FC mode 1):
/// -1 → text 7 and 2; 1 or 3 → stop probing, close the card events, 1;
/// 2 → text 8 and 2; else 0. The texts 7/8 take the visibility byte left
/// in `a1` by the read.
pub fn load_read(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    ov1_text::clear_visibility(m);
    ov1_text::set_visible(m, 0xb, 1);
    let load = state(m, LOAD);
    let slot = m.lw(off(load, 0x10));
    let (result, a1) = ov1_card::read_save(m, slot, 1);
    let value = match result {
        0xffff_ffff => {
            ov1_text::clear_visibility(m);
            ov1_text::set_visible(m, 7, a1);
            2
        }
        1 | 3 => {
            let load = state(m, LOAD);
            let flags = m.lw(load);
            m.sw(load, flags & !1);
            ov1_card::disable_events(m);
            1
        }
        2 => {
            ov1_text::clear_visibility(m);
            ov1_text::set_visible(m, 8, a1);
            2
        }
        _ => 0,
    };
    m.pop_frame(0x18);
    value
}

/// 800D250C (phase): returns 1.
pub fn load_skip() -> u32 {
    1
}

/// 800D25A0 (phase): apply the loaded options (800CCDA4); returns 1.
pub fn load_apply_options(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    ov1_card::apply_options(m);
    m.pop_frame(0x18);
    1
}

/// 800D25C0 (phase): the save list: the card's last choice (800CCD50) to
/// the menu block (+8) and the screen, one more choice with context
/// +0x974 bit 0, page 5, text 9 (visibility = the stale `a1`), styles
/// 1..8 = 2 (8 = 3 without +0x974 bit 0), the choice 1, 11..46 = 5.
/// Returns 2.
pub fn load_list(m: &mut Machine, a1: u32) -> u32 {
    m.push_frame(0x20);
    let context = context(m);
    let base = off(context, 0x960);
    let choice = ov1_card::last_choice(m);
    let block = m.lw(MENU);
    m.sh(off(block, 8), choice as u16);
    let load = state(m, LOAD);
    m.sw(off(load, 0x14), choice as u16 as i16 as u32);
    if m.lw(off(base, 0x14)) & 1 != 0 {
        let count = m.lw(off(load, 0x18));
        m.sw(off(load, 0x18), count.wrapping_add(1));
    }
    ov1_script::select_page(m, 5);
    ov1_text::clear_visibility(m);
    ov1_text::set_visible(m, 9, a1);
    for slot in 1..=8 {
        ov1_script::set_style(m, slot, 2);
    }
    if m.lw(off(base, 0x14)) & 1 == 0 {
        ov1_script::set_style(m, 8, 3);
    }
    let load = state(m, LOAD);
    let choice = m.lw(off(load, 0x14));
    ov1_script::set_style(m, choice.wrapping_add(1), 1);
    for slot in 0..0x24 {
        ov1_script::set_style(m, slot + 0xb, 5);
    }
    m.pop_frame(0x20);
    2
}

/// 800D26CC (phase): the selected save's details (mission name at
/// (110, 274), difficulty text 18..20, play time `h:mm:ss` at (160, 326)
/// with spaces as zeros, 36 emblem nibbles as styles 11..46) or, without
/// one, text 21/22/23 and blank emblems; then confirm (2, or the refused
/// sound 0x30 on an empty save slot other than choices 6/7), cancel (8),
/// right/left (bits 3/2, wrapping over the choice count, 0x3D) and the
/// choice styles.
pub fn load_select(m: &mut Machine) -> u32 {
    let sp = m.push_frame(0xd8);
    ov1_text::clear_visibility(m);
    ov1_text::set_visible(m, 9, 1);
    let load = state(m, LOAD);
    let choice = m.lw(off(load, 0x14));
    let save = ov1_card::save_record(m, choice);
    if save == 0 {
        let load = state(m, LOAD);
        let choice = m.lw(off(load, 0x14));
        let text = match choice {
            6 => 0x16,
            7 => 0x17,
            _ => 0x15,
        };
        ov1_text::set_visible(m, text, 1);
        for slot in 0..0x24 {
            ov1_script::set_style(m, slot + 0xb, 0);
        }
    } else {
        save_details(m, sp, save);
    }
    let input = pressed(m);
    let result = if input & 0x10 != 0 {
        let load = state(m, LOAD);
        let choice = m.lw(off(load, 0x14));
        if save != 0 || choice.wrapping_sub(6) < 2 {
            sound(m, 0x3c);
            Some(2)
        } else {
            sound(m, 0x30);
            None
        }
    } else if input & 0x20 != 0 {
        sound(m, 0x3b);
        Some(8)
    } else {
        if input & 8 != 0 {
            let load = state(m, LOAD);
            let count = m.lw(off(load, 0x18));
            let choice = m.lw(off(load, 0x14));
            let next = if (choice as i32) < (count.wrapping_sub(1) as i32) {
                choice.wrapping_add(1)
            } else {
                0
            };
            m.sw(off(load, 0x14), next);
            sound(m, 0x3d);
        } else if input & 4 != 0 {
            let load = state(m, LOAD);
            let choice = m.lw(off(load, 0x14));
            let next = if (choice as i32) > 0 {
                choice.wrapping_sub(1)
            } else {
                m.lw(off(load, 0x18)).wrapping_sub(1)
            };
            m.sw(off(load, 0x14), next);
            sound(m, 0x3d);
        }
        None
    };
    let result = result.unwrap_or_else(|| {
        for slot in 1..=8 {
            ov1_script::set_style(m, slot, 2);
        }
        let context = context(m);
        if m.lw(off(context, 0x974)) & 1 == 0 {
            ov1_script::set_style(m, 8, 3);
        }
        let load = state(m, LOAD);
        let choice = m.lw(off(load, 0x14));
        ov1_script::set_style(m, choice.wrapping_add(1), 1);
        0
    });
    m.pop_frame(0xd8);
    result
}

/// The details half of [`load_select`] for the save record `save`.
fn save_details(m: &mut Machine, sp: u32, save: u32) {
    let mission = lbu(m, off(save, 0x129));
    m.sw(off(sp, 0x10), 0x6e);
    m.sw(off(sp, 0x18), 0x0080_8080);
    m.sw(off(sp, 0x14), 0x112);
    let name = m.lw(off(off(MISSION_NAMES, mission.wrapping_mul(12)), 8));
    let block = m.lw(MENU);
    let ot = m.lw(off(block, 0x8c));
    let packet = m.lw(off(block, 0x8090));
    let packet = ov1_text::draw_text(m, 5, name, ot, packet);
    let block = m.lw(MENU);
    m.sw(off(block, 0x8090), packet);
    ov1_text::set_visible(m, 0x10, 1);
    for i in 0..3 {
        if i == lbu(m, off(save, 0x128)) {
            ov1_text::set_visible(m, i + 0x12, 1);
        }
    }
    ov1_text::set_visible(m, 0x1b, 1);
    let frames = m.lw(off(save, 0x124));
    let sign = ((frames as i32) >> 31) as u32;
    let minutes_total = mulhi(frames, 0x0576_19f1, 5).wrapping_sub(sign);
    let seconds_total = mulhi(frames, 0x51eb_851f, 3).wrapping_sub(sign);
    let seconds = seconds_total.wrapping_sub(minutes_total.wrapping_mul(60));
    let hours_of_minutes = {
        let hi =
            ((i64::from(minutes_total as i32) * i64::from(0x8888_8889u32 as i32)) >> 32) as u32;
        ((hi.wrapping_add(minutes_total) as i32 >> 5) as u32)
            .wrapping_sub(((minutes_total as i32) >> 31) as u32)
    };
    let minutes = minutes_total.wrapping_sub(hours_of_minutes.wrapping_mul(60));
    let time = off(sp, 0x70);
    dispatch::call(m, ov1_text::SPRINTF, [time, TIME_FORMAT, minutes, seconds]);
    let length = dispatch::call(m, ov1_text::STRLEN, [time, 0, 0, 0]);
    for i in 0..length as i32 {
        let at = off(time, i as u32);
        if lbu(m, at) == 0x20 {
            m.sb(at, 0x30);
        }
    }
    let frames = m.lw(off(save, 0x124));
    let hours = mulhi(frames, 0x5d34_edef, 15).wrapping_sub(((frames as i32) >> 31) as u32);
    let text = off(sp, 0x20);
    dispatch::call(m, ov1_text::SPRINTF, [text, HOURS_FORMAT, hours, 0]);
    dispatch::call(m, STRCAT, [text, time, 0, 0]);
    m.sw(off(sp, 0x10), 0xa0);
    m.sw(off(sp, 0x14), 0x146);
    m.sw(off(sp, 0x18), 0x0080_8080);
    let block = m.lw(MENU);
    let ot = m.lw(off(block, 0x8c));
    let packet = m.lw(off(block, 0x8090));
    let packet = ov1_text::draw_text(m, 5, text, ot, packet);
    let block = m.lw(MENU);
    m.sw(off(block, 0x8090), packet);
    for k in 1..=0x24u32 {
        let byte = lbu(m, off(save, 0x80 + k / 2));
        let style = (byte >> ((k % 2) << 2)) & 0xf;
        ov1_script::set_style(m, k + 0xa, style);
    }
}

/// `sra(hi(x * multiplier), shift)` (signed `mult`).
fn mulhi(x: u32, multiplier: u32, shift: u32) -> u32 {
    let hi = ((i64::from(x as i32) * i64::from(multiplier as i32)) >> 32) as i32;
    (hi >> shift) as u32
}

/// 800D2B24 (phase): the chosen save: choices 6 and 7 are stored as is in
/// the menu block (+8), others also load save data (800CD224); context
/// +0x961 = that byte. Returns 0x10.
pub fn load_apply(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let load = state(m, LOAD);
    let choice = m.lw(off(load, 0x14));
    if choice == 7 || choice == 6 {
        let block = m.lw(MENU);
        m.sh(off(block, 8), choice as u16);
    } else {
        let block = m.lw(MENU);
        let slot = lhu(m, off(load, 0x14));
        m.sh(off(block, 8), slot as u16);
        ov1_card::apply_save(m, slot as u16 as i16 as u32);
    }
    let block = m.lw(MENU);
    let context = context(m);
    let value = lbu(m, off(block, 8));
    m.sb(off(context, 0x961), value as u8);
    m.pop_frame(0x18);
    0x10
}

/// 800D2BA4 (menu state 5 initialization): probing on, phase group and
/// phase 0, slot 0, choice 0, card events enabled and card state reset,
/// seven choices, page 4, styles 1..3 = 0, 5 = 2, no texts. Returns 1.
pub fn load_init(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let load = state(m, LOAD);
    m.sh(off(load, 6), 0);
    m.sh(off(load, 4), 0);
    m.sw(off(load, 0x10), 0);
    m.sw(off(load, 0x14), 0);
    m.sw(load, 1);
    ov1_card::enable_events(m);
    ov1_card::reset_state(m);
    let load = state(m, LOAD);
    m.sw(off(load, 0x18), 7);
    ov1_script::select_page(m, 4);
    ov1_script::set_style(m, 1, 0);
    ov1_script::set_style(m, 2, 0);
    ov1_script::set_style(m, 3, 0);
    ov1_script::set_style(m, 5, 2);
    ov1_text::clear_visibility(m);
    m.pop_frame(0x18);
    1
}

/// 800D2C3C (menu state 5 update): with probing on, poll the card
/// (800CBDAC) and store slot 0's status; run phase `[+6]` of group `[+4]`
/// (the callback receives the card poll's `a1`). Result bits: 2 next
/// phase, 1 next group, 4 restart, 8 close the card events and return
/// 0x200, 0x10 close them and return 0x100.
pub fn load_update(m: &mut Machine, args: [u32; 4]) -> u32 {
    m.push_frame(0x18);
    let mut args = args;
    let load = state(m, LOAD);
    if m.lw(load) & 1 != 0 {
        ov1_card::poll(m);
        let (status, a1) = ov1_card::slot_state(m, 0);
        let load = state(m, LOAD);
        m.sw(off(load, 8), status);
        args[1] = a1;
    }
    let load = state(m, LOAD);
    args[0] = load;
    let group = lh(m, off(load, 4));
    let phases = m.lw(off(LOAD_PHASES, group << 2));
    let phase = lh(m, off(load, 6));
    let callback = m.lw(off(phases, phase << 2));
    let result = dispatch::call(m, callback, args);
    if result & 2 != 0 {
        let load = state(m, LOAD);
        let phase = lhu(m, off(load, 6));
        m.sh(off(load, 6), phase.wrapping_add(1) as u16);
    }
    if result & 1 != 0 {
        let load = state(m, LOAD);
        let group = lhu(m, off(load, 4));
        m.sh(off(load, 6), 0);
        m.sh(off(load, 4), group.wrapping_add(1) as u16);
    }
    if result & 4 != 0 {
        let load = state(m, LOAD);
        m.sh(off(load, 4), 0);
        m.sh(off(load, 6), 0);
    }
    let value = if result & 8 != 0 {
        ov1_card::disable_events(m);
        0x200
    } else if result & 0x10 != 0 {
        ov1_card::disable_events(m);
        0x100
    } else {
        0
    };
    m.pop_frame(0x18);
    value
}
