//! Overlay module 4 (game state 8): a development font and message
//! checker. Loaded at [`OVERLAY_BASE`](crate::common::OVERLAY_BASE) from
//! `ACE.BIN` file 4 (63,740 bytes, 0x800C9DC0..0x800D96BC); the resident
//! state table entry 800AE5C4 points at its callback table 800CF2CC
//! (800CD1B4, 800CD440). No menu of the retail game selects state 8; the
//! documented probe (`.ovmisc/snapshots.lua`) reaches it.
//!
//! The checker reads its data from the development host through the BIOS
//! file system's `sim:` device: four font sheets (`sim:pm\font\asc_*.tim`),
//! a font test text (`sim:font\message.txt`), the message files of the 30
//! missions and of three other screens (`sim:\mescv\data\...\*.fhm`) and
//! nine background tiles per background (`sim:pm\sky\endBBNN.tim`). The
//! retail BIOS has no `sim:` device, so every open fails ("can't open
//! ...!"), the reads fail without writing, and the TIM uploads (80026D00)
//! see whatever the buffers held. PCSX-Redux (captures `ovmisc-017`,
//! `ovmisc-019`) prints "can't open sim:pm\font\asc_1_2.tim!" and "Can't
//! read TIM [801f7e60]", then the second sheet's upload reads the invalid
//! address 0xFFFF8960 (Redux pauses there; real hardware takes a bus
//! error). The static data is in the overlay: the 25 font descriptors at
//! 800D13E8 with their glyph tables, the character map 800D1578 and the
//! kerning rules of 800CDB24 ([`kerning`]).
//!
//! Port notes: the BIOS file calls, sprintf and the TIM upload go through
//! [`dispatch::call`] (BIOS open/read/close have no `sdk.rs` owner yet).
//! [`setup`]'s 0x8180-byte frame and [`load_backgrounds`]' 0x4E50-byte
//! frame reach below `machine::STACK_LIMIT`, so `push_frame` records
//! `Fault::Address` where the original has none (its frame starts at
//! 0x801F7E50, the TIM buffer at 0x801F7E60).
//!
//! | State 8 substate | Routine |
//! | --- | --- |
//! | 0 | 800CD1B4 [`setup`] |
//! | 1 | 800CD440 [`frame`] |
//!
//! [`frame`] phases (block +0): 0 font test (the font name, background
//! number and the test text in the selected font), 1 message browser
//! ([`message_browser`]), 2 a positioning test, 3 → 4, 4 display restore,
//! 5 return to context +0x3A4. Backgrounds 0..24 (pad bits 1/2): 0 is a
//! plain tile in the font's colour, the others eight 80x240 sprites of
//! the uploaded tiles ([`load_backgrounds`]).
//!
//! The overlay also holds unreferenced code (no call, jump or pointer in
//! the resident program or any overlay): 800CC4D4 and 800CC520, an earlier
//! "MUSIC PLAYER (SAMPLE)" (strings 800C9DC0..), not translated; and the
//! message lookups 800CF214 / 800CF278 ([`message_text2`],
//! [`message_word`]).
//!
//! Every routine is claimed only while module 4 is loaded
//! ([`common::loaded_module`]).
use crate::{
    arena,
    common::{self, CONTEXT},
    dispatch,
    machine::{Machine, SmallData},
    mainloop::{
        flags::PAUSE_LOCK,
        transition::{NEXT_SUBSTATE, RESET_ARENA, SET_STATE},
    },
    states::Invoker,
};

/// Overlay module number.
pub const MODULE: u32 = 4;

/// `gp+0x2C4`: display request block.
const DISPLAY: u32 = 0x2c4;
/// Context +4: buffer index; +8: current ordering table.
const BUFFER_INDEX: u32 = 4;
const ORDERING_TABLE: u32 = 8;
/// Context +0x128 / +0x12C: held and newly pressed pad bits.
const HELD: u32 = 0x128;
const PRESSED: u32 = 0x12c;
/// Context +0x38C: message language (0..4: English, German, French,
/// Spanish, Italian).
const LANGUAGE: u32 = 0x38c;
/// Context +0x3A4: the state to return to.
const RETURN_STATE: u32 = 0x3a4;

/// 800D96A8: the 0x464-byte block: +0 phase, +4..+0x450 0x113 words,
/// +0x450..+0x453 bytes, +0x454 / +0x458 the two packet buffers (0xA010
/// bytes each), +0x45C the packet cursor, +0x460 / +0x462 halfwords.
pub const BLOCK: u32 = 0x800d_96a8;
/// 800D96B0 / 800D96B4: position of the positioning test text.
pub const TEST_X: u32 = 0x800d_96b0;
pub const TEST_Y: u32 = 0x800d_96b4;
/// 800D96B8: a 0xC-byte block holding two 0x11C-byte allocations.
pub const LISTS: u32 = 0x800d_96b8;
/// 800CFD04: message file (1..30 missions, 31..33 the other screens).
pub const GROUP: u32 = 0x800c_fd04;
/// 800CFD08: message in the file.
pub const MESSAGE: u32 = 0x800c_fd08;
/// 800CFD0C: background (0..24).
pub const BACKGROUND: u32 = 0x800c_fd0c;
/// 800CFD10: font (0..24).
pub const FONT: u32 = 0x800c_fd10;
/// 800CFCDC: VRAM positions {x, y} of the background tiles (entries 1..9).
const TILE_POSITIONS: u32 = 0x800c_fcdc;
/// 800D1680: three words cleared by [`setup`].
const CLEARED_WORDS: u32 = 0x800d_1680;
/// 800D1690: five texture-page words (GetTPage << 16).
const FONT_PAGES: u32 = 0x800d_1690;
/// 800D16A8: the message file (0x8000 bytes).
pub const MESSAGE_FILE: u32 = 0x800d_16a8;
/// 800F6748: the font test text (0x8000 bytes).
pub const TEST_TEXT: u32 = 0x800f_6748;
/// 800D13E8: 16-byte font descriptors: +0 u, +1 v, +3 height, +4 line
/// spacing (signed), +6 texture page, +8 CLUT, +0xC glyph table (4 bytes
/// per glyph: u, v, width, y offset).
pub const FONTS: u32 = 0x800d_13e8;
/// 800D1578: character code to glyph.
pub const CHARACTER_MAP: u32 = 0x800d_1578;
/// 800CB6E4: 25 entries {name, background colour}.
const FONT_NAMES: u32 = 0x800c_b6e4;
/// 800CB494: the five language names.
const LANGUAGE_NAMES: u32 = 0x800c_b494;
/// 800CB580: the four font sheet file names.
const FONT_FILES: u32 = 0x800c_b580;

/// "sim:pm\sky\end%02d%02d.tim".
const TILE_PATH: u32 = 0x800c_b434;
/// "can't open %s!\n".
const CANT_OPEN: u32 = 0x800c_b450;
/// "%d / %d\n".
const COUNTER: u32 = 0x800c_b460;
/// "sim:\mescv\data\bfdftm\fhm\m%02d.fhm".
const MISSION_PATH: u32 = 0x800c_b4a8;
/// "sim:\mescv\data\bfdftm\fhm\m36.fhm".
const GROUP_31_PATH: u32 = 0x800c_b4d0;
/// "sim:\mescv\data\message\fhm\net.fhm".
const GROUP_32_PATH: u32 = 0x800c_b4f4;
/// "sim:\mescv\data\message\fhm\title.fhm".
const GROUP_33_PATH: u32 = 0x800c_b518;
/// "Can't open %s\n".
const CANT_OPEN_MESSAGES: u32 = 0x800c_b540;
/// "sim:pm\font\%s".
const FONT_PATH: u32 = 0x800c_b590;
/// "sim:font\message.txt".
const TEST_TEXT_PATH: u32 = 0x800c_b5a0;
/// "%s   BG:%d".
const FONT_TITLE: u32 = 0x800c_b7ac;
/// "\n A \nIHI".
const POSITION_TEXT: u32 = 0x800c_b7b8;

const SPRINTF: u32 = 0x800a_cd28;
const PRINTF: u32 = 0x800a_c3e8;
const STRLEN: u32 = 0x800a_c328;
const GET_TPAGE: u32 = 0x800a_ab98;
/// BIOS B0:32h open, B0:34h read, B0:36h close (device-facing; not in
/// `sdk.rs` yet).
pub const BIOS_OPEN: u32 = 0x800a_d808;
pub const BIOS_READ: u32 = 0x800a_d818;
pub const BIOS_CLOSE: u32 = 0x800a_d838;
const UPLOAD_TIM: u32 = 0x8002_6d00;

const GREY: u32 = 0x0080_8080;
const FAILED: u32 = 0xffff_ffff;

/// 800CCC94: clear the block's bytes +0x451 / +0x450 and its 0x113 words
/// from +4.
pub fn reset_messages(m: &mut Machine) {
    let block = m.lw(BLOCK);
    m.sb(block.wrapping_add(0x451), 0);
    m.sb(block.wrapping_add(0x450), 0);
    let block = m.lw(BLOCK);
    let mut cursor = block.wrapping_add(4);
    for _ in 0..0x113 {
        m.sw(cursor, 0);
        cursor = cursor.wrapping_add(4);
    }
}

/// 800CCCD0(set): load and upload the nine tiles `end<set><n>.tim` (n =
/// 1..9) of a background: each file is read (0x4D20 bytes) into the frame
/// buffer, its CLUT is placed at (0x2C0, 0x1FF) and its image at tile
/// position n before the upload (80026D00). Returns 0.
pub fn load_backgrounds(m: &mut Machine, c: &mut Invoker, set: u32) -> u32 {
    let frame = m.push_frame(0x4e50);
    let path = frame.wrapping_add(0x10);
    let tim = frame.wrapping_add(0x110);
    let mut tile: u32 = 1;
    let mut position = TILE_POSITIONS + 4;
    loop {
        c(m, SPRINTF, [path, TILE_PATH, set, tile]);
        let file = c(m, BIOS_OPEN, [path, 1, 0, 0]);
        if file == FAILED {
            c(m, PRINTF, [CANT_OPEN, path, 0, 0]);
        }
        c(m, BIOS_READ, [file, tim, 0x4d20, 0]);
        c(m, BIOS_CLOSE, [file, 0, 0, 0]);
        m.sh(tim.wrapping_add(0xc), 0x2c0);
        tile = tile.wrapping_add(1);
        m.sh(tim.wrapping_add(0xe), 0x1ff);
        let clut_bytes = m.lw(tim.wrapping_add(8));
        let x = m.lhu(position);
        let image = tim.wrapping_add(((clut_bytes >> 2) << 2).wrapping_add(8));
        m.sh(image.wrapping_add(4), x);
        let y = m.lhu(position + 2);
        position += 4;
        m.sh(image.wrapping_add(6), y);
        c(m, UPLOAD_TIM, [tim, 0, 0, 0]);
        if (tile as i32) >= 10 {
            break;
        }
    }
    m.pop_frame(0x4e50);
    0
}

/// 800CCDE0(font, packet): the message browser. Held bit 0x10 with
/// pressed 8/4 changes the language (0..4), held 0x20 the message file
/// (1..33, reloaded from `sim:`), otherwise pressed 8/4 the message. Draws
/// "index / count", the language name, the file name buffer and the
/// message ([`message_text`]) with [`draw_text`]. Returns the packet
/// cursor.
pub fn message_browser(m: &mut Machine, c: &mut Invoker, font: u32, packet: u32) -> u32 {
    let frame = m.push_frame(0x250);
    let mut packet = packet;
    let mut reload = false;
    let context = m.gw(CONTEXT);
    let held = m.lw(context.wrapping_add(HELD));
    let pressed = m.lw(context.wrapping_add(PRESSED));
    let language = context.wrapping_add(LANGUAGE);
    if held & 0x10 != 0 {
        if pressed & 8 != 0 {
            let next = m.lw(language).wrapping_add(1);
            m.sw(language, next);
            if (next as i32) >= 5 {
                m.sw(language, 0);
            }
        }
        if pressed & 4 != 0 {
            let previous = m.lw(language).wrapping_sub(1);
            m.sw(language, previous);
            if (previous as i32) < 0 {
                m.sw(language, 4);
            }
        }
    } else if held & 0x20 != 0 {
        if pressed & 8 != 0 {
            let next = m.lw(GROUP).wrapping_add(1);
            m.sw(GROUP, next);
            if (next as i32) >= 0x22 {
                m.sw(GROUP, 1);
            }
            m.sw(MESSAGE, 0);
            reload = true;
        }
        if pressed & 4 != 0 {
            let previous = m.lw(GROUP).wrapping_sub(1);
            m.sw(GROUP, previous);
            if (previous as i32) <= 0 {
                m.sw(GROUP, 0x21);
            }
            m.sw(MESSAGE, 0);
            reload = true;
        }
    } else {
        let current = m.lw(language);
        let offset = m.lw((current << 2).wrapping_add(MESSAGE_FILE).wrapping_add(4));
        let messages = ((offset >> 2) << 2).wrapping_add(MESSAGE_FILE);
        if pressed & 8 != 0 {
            let next = m.lw(MESSAGE).wrapping_add(1);
            m.sw(MESSAGE, next);
            let count = m.lw(messages);
            if (next as i32) >= (count as i32) {
                m.sw(MESSAGE, 0);
            }
        }
        if pressed & 4 != 0 {
            let previous = m.lw(MESSAGE).wrapping_sub(1);
            m.sw(MESSAGE, previous);
            if (previous as i32) < 0 {
                let count = m.lw(messages);
                m.sw(MESSAGE, count.wrapping_sub(1));
            }
        }
        let text = frame.wrapping_add(0x120);
        let index = m.lw(MESSAGE);
        let count = m.lw(messages);
        c(m, SPRINTF, [text, COUNTER, index, count]);
        m.sw(frame.wrapping_add(0x10), 8);
        let context = m.gw(CONTEXT);
        m.sw(frame.wrapping_add(0x14), 0x3c);
        m.sw(frame.wrapping_add(0x18), GREY);
        let ot = m.lw(context.wrapping_add(ORDERING_TABLE));
        packet = draw_text(m, c, font, text, ot, packet);
    }
    let names = frame.wrapping_add(0x220);
    for offset in (0..0x14).step_by(4) {
        let name = m.lw(LANGUAGE_NAMES + offset);
        m.sw(names.wrapping_add(offset), name);
    }
    let current = m.lw(language);
    m.sw(frame.wrapping_add(0x10), 8);
    m.sw(frame.wrapping_add(0x14), 0xa);
    let context = m.gw(CONTEXT);
    m.sw(frame.wrapping_add(0x18), GREY);
    let name = m.lw(names.wrapping_add(current << 2));
    let ot = m.lw(context.wrapping_add(ORDERING_TABLE));
    packet = draw_text(m, c, font, name, ot, packet);
    let path = frame.wrapping_add(0x20);
    if reload {
        let group = m.lw(GROUP);
        let format = if group.wrapping_sub(1) < 0x1e {
            Some(MISSION_PATH)
        } else if group == 0x1f {
            Some(GROUP_31_PATH)
        } else if group == 0x20 {
            Some(GROUP_32_PATH)
        } else if group == 0x21 {
            Some(GROUP_33_PATH)
        } else {
            None
        };
        if let Some(format) = format {
            c(m, SPRINTF, [path, format, group, 0]);
        }
        let file = c(m, BIOS_OPEN, [path, 1, 0, 0]);
        if file == FAILED {
            c(m, PRINTF, [CANT_OPEN_MESSAGES, path, 0, 0]);
        }
        c(m, BIOS_READ, [file, MESSAGE_FILE, 0x8000, 0]);
        c(m, BIOS_CLOSE, [file, 0, 0, 0]);
    }
    // The file name buffer is drawn every frame: without a reload it
    // holds whatever the stack held.
    let context = m.gw(CONTEXT);
    m.sw(frame.wrapping_add(0x10), 8);
    m.sw(frame.wrapping_add(0x14), 0x1e);
    m.sw(frame.wrapping_add(0x18), GREY);
    let ot = m.lw(context.wrapping_add(ORDERING_TABLE));
    packet = draw_text(m, c, font, path, ot, packet);
    let index = m.lw(MESSAGE);
    let text = message_text(m, index, MESSAGE_FILE);
    let context = m.gw(CONTEXT);
    m.sw(frame.wrapping_add(0x10), 8);
    m.sw(frame.wrapping_add(0x14), 0x64);
    m.sw(frame.wrapping_add(0x18), GREY);
    let ot = m.lw(context.wrapping_add(ORDERING_TABLE));
    let end = draw_text(m, c, font, text, ot, packet);
    m.pop_frame(0x250);
    end
}

/// 800CD1B4 (state 8, substate 0): display requests (+4 = 1, +0x14 = 0,
/// bit 1), context PAUSE_LOCK, the block (0x464 bytes, two 0xA010 packet
/// buffers) and the list block (two 0x11C allocations), [`reset_messages`];
/// then the four font sheets and the test text from `sim:` (each sheet
/// uploaded by 80026D00 from the frame buffer), five texture pages
/// (GetTPage(2, 1, 0x2C0 + 0x40 n, 0) << 16) and the test position
/// (0xA0, 0x190). Returns NEXT_SUBSTATE.
pub fn setup(m: &mut Machine, c: &mut Invoker) -> u32 {
    let frame = m.push_frame(0x8180);
    let display = m.gw(DISPLAY);
    let context = m.gw(CONTEXT);
    m.sw(display.wrapping_add(4), 1);
    let requests = m.lw(display);
    m.sw(display.wrapping_add(0x14), 0);
    m.sw(display, requests | 1);
    let flags = m.lw(context);
    m.sw(context, flags | PAUSE_LOCK);
    let block = arena::alloc(m, 0x464);
    m.sw(BLOCK, block);
    let lists = arena::alloc(m, 0xc);
    m.sw(LISTS, lists);
    let block = m.lw(BLOCK);
    let first = arena::alloc(m, 0xa010);
    m.sw(block.wrapping_add(0x454), first);
    let block = m.lw(BLOCK);
    let second = arena::alloc(m, 0xa010);
    m.sw(block.wrapping_add(0x458), second);
    let lists = m.lw(LISTS);
    let first = arena::alloc(m, 0x11c);
    m.sw(lists, first);
    let lists = m.lw(LISTS);
    let second = arena::alloc(m, 0x11c);
    m.sw(lists.wrapping_add(4), second);
    reset_messages(m);

    let buffer = frame.wrapping_add(0x10);
    let names = frame.wrapping_add(0x8050);
    let path = frame.wrapping_add(0x8060);
    let block = m.lw(BLOCK);
    m.sb(block.wrapping_add(0x452), 0);
    let block2 = m.lw(BLOCK);
    m.sw(block, 0);
    m.sb(block2.wrapping_add(0x453), 0);
    let block = m.lw(BLOCK);
    m.sh(block.wrapping_add(0x460), 0);
    m.sh(block.wrapping_add(0x462), 0);
    for offset in (0..0x10).step_by(4) {
        let name = m.lw(FONT_FILES + offset);
        m.sw(names.wrapping_add(offset), name);
    }
    let mut name = names;
    for _ in 0..4 {
        let file_name = m.lw(name);
        c(m, SPRINTF, [path, FONT_PATH, file_name, 0]);
        let file = c(m, BIOS_OPEN, [path, 1, 0, 0]);
        if file == FAILED {
            c(m, PRINTF, [CANT_OPEN, path, 0, 0]);
        }
        c(m, BIOS_READ, [file, buffer, 0x8040, 0]);
        c(m, BIOS_CLOSE, [file, 0, 0, 0]);
        c(m, UPLOAD_TIM, [buffer, 0, 0, 0]);
        name = name.wrapping_add(4);
    }
    let file = c(m, BIOS_OPEN, [TEST_TEXT_PATH, 1, 0, 0]);
    if file == FAILED {
        c(m, PRINTF, [CANT_OPEN, TEST_TEXT_PATH, 0, 0]);
    }
    c(m, BIOS_READ, [file, TEST_TEXT, 0x8000, 0]);
    c(m, BIOS_CLOSE, [file, 0, 0, 0]);
    m.sw(CLEARED_WORDS, 0);
    m.sw(CLEARED_WORDS + 4, 0);
    m.sw(CLEARED_WORDS + 8, 0);
    let mut x: u32 = 0x2c0;
    let mut slot = FONT_PAGES;
    for _ in 0..5 {
        let page = c(m, GET_TPAGE, [2, 1, x, 0]);
        m.sw(slot, page << 16);
        slot += 4;
        x += 0x40;
    }
    m.sw(TEST_X, 0xa0);
    m.sw(TEST_Y, 0x190);
    m.pop_frame(0x8180);
    NEXT_SUBSTATE
}

/// The frame's copy of [`FONT_NAMES`] (`frame + 0x120`, 0xC8 bytes).
fn font_entry(names: u32, font: u32) -> u32 {
    names.wrapping_add(font << 3)
}

/// 800CD440 (state 8, substate 1): the packet cursor is the buffer of the
/// current buffer index; then the phase (block +0, see the module table)
/// and the background: pad bits 1/2 step it (0..24, reloading its tiles),
/// 0 adds a full-screen tile in the font's colour, others eight 80x240
/// sprites (4x2, CLUT 0x7FEC) each followed by its texture page. Returns 0,
/// or in phase 5 context +0x3A4 | SET_STATE | RESET_ARENA.
pub fn frame(m: &mut Machine, c: &mut Invoker) -> u32 {
    let frame = m.push_frame(0x200);
    let names = frame.wrapping_add(0x120);
    for offset in (0..0xc8).step_by(4) {
        let word = m.lw(FONT_NAMES + offset);
        m.sw(names.wrapping_add(offset), word);
    }
    let context = m.gw(CONTEXT);
    let block = m.lw(BLOCK);
    let index = m.lw(context.wrapping_add(BUFFER_INDEX));
    let phase = m.lw(block);
    let buffer = m.lw(block.wrapping_add(index << 2).wrapping_add(0x454));
    m.sw(block.wrapping_add(0x45c), buffer);
    match phase {
        0 => font_test(m, c, frame, names),
        1 => {
            let block = m.lw(BLOCK);
            let font = m.lw(FONT);
            let packet = m.lw(block.wrapping_add(0x45c));
            let next = message_browser(m, c, font, packet);
            let block = m.lw(BLOCK);
            let context = m.gw(CONTEXT);
            m.sw(block.wrapping_add(0x45c), next);
            if m.lw(context.wrapping_add(PRESSED)) & 0x40 != 0 {
                m.sw(block, 2);
            }
        }
        2 => position_test(m, c, frame),
        3 => {
            let block = m.lw(BLOCK);
            m.sw(block, 4);
        }
        4 => {
            let display = m.gw(DISPLAY);
            m.sb(display.wrapping_add(0x10), 0);
            let display2 = m.gw(DISPLAY);
            m.sw(display.wrapping_add(4), 0);
            m.sw(display.wrapping_add(0x14), 0);
            m.sb(display2.wrapping_add(0x11), 0);
            let display = m.gw(DISPLAY);
            m.sb(display.wrapping_add(0x12), 0);
            let display = m.gw(DISPLAY);
            let requests = m.lw(display);
            m.sw(display, requests | 9);
            let block = m.lw(BLOCK);
            m.sw(block, 5);
        }
        5 => {
            let context = m.gw(CONTEXT);
            let state = m.lw(context.wrapping_add(RETURN_STATE));
            m.pop_frame(0x200);
            return state | SET_STATE | RESET_ARENA;
        }
        _ => {}
    }
    let mut reload = false;
    let context = m.gw(CONTEXT);
    if m.lw(context.wrapping_add(PRESSED)) & 1 != 0 {
        let next = m.lw(BACKGROUND).wrapping_add(1);
        m.sw(BACKGROUND, next);
        reload = true;
        if (next as i32) >= 0x19 {
            m.sw(BACKGROUND, 0);
        }
    }
    let context = m.gw(CONTEXT);
    if m.lw(context.wrapping_add(PRESSED)) & 2 != 0 {
        let previous = m.lw(BACKGROUND).wrapping_sub(1);
        m.sw(BACKGROUND, previous);
        if (previous as i32) < 0 {
            m.sw(BACKGROUND, 0x18);
        }
        reload = true;
    }
    let background = m.lw(BACKGROUND);
    if background == 0 {
        let font = m.lw(FONT);
        let block = m.lw(BLOCK);
        let colour = m.lw(font_entry(names, font).wrapping_add(4));
        let packet = m.lw(block.wrapping_add(0x45c));
        m.sw(packet.wrapping_add(4), colour | 0x6000_0000);
        m.sw(packet.wrapping_add(8), 0);
        m.sw(packet.wrapping_add(0xc), 0x01ff_0140);
        let context = m.gw(CONTEXT);
        let ot = m.lw(context.wrapping_add(ORDERING_TABLE));
        let head = m.lw(ot);
        m.sw(packet, head | 0x0300_0000);
        let ot = m.lw(context.wrapping_add(ORDERING_TABLE));
        m.sw(ot, packet & 0x00ff_ffff);
        let block = m.lw(BLOCK);
        m.sw(block.wrapping_add(0x45c), packet.wrapping_add(0x10));
    } else {
        let block = m.lw(BLOCK);
        let mut packet = m.lw(block.wrapping_add(0x45c));
        if reload {
            load_backgrounds(m, c, background);
        }
        background_sprites(m, &mut packet);
        let block = m.lw(BLOCK);
        m.sw(block.wrapping_add(0x45c), packet);
    }
    m.pop_frame(0x200);
    0
}

/// 800CD500 (phase 0): pressed 8/4 step the font (mod 25), 0x40 moves to
/// phase 1; "<font name>   BG:<background>" at (0x14, 0xA) and the test
/// text at (8, 0x64) in the font.
fn font_test(m: &mut Machine, c: &mut Invoker, frame: u32, names: u32) {
    let context = m.gw(CONTEXT);
    if m.lw(context.wrapping_add(PRESSED)) & 8 != 0 {
        let next = m.lw(FONT).wrapping_add(1) as i32;
        m.sw(FONT, next.wrapping_rem(25) as u32);
    }
    if m.lw(context.wrapping_add(PRESSED)) & 4 != 0 {
        let previous = m.lw(FONT).wrapping_add(0x18) as i32;
        m.sw(FONT, previous.wrapping_rem(25) as u32);
    }
    if m.lw(context.wrapping_add(PRESSED)) & 0x40 != 0 {
        let block = m.lw(BLOCK);
        m.sw(block, 1);
    }
    let title = frame.wrapping_add(0x20);
    let font = m.lw(FONT);
    let background = m.lw(BACKGROUND);
    let name = m.lw(font_entry(names, font));
    c(m, SPRINTF, [title, FONT_TITLE, name, background]);
    m.sw(frame.wrapping_add(0x10), 0x14);
    let font = m.lw(FONT);
    m.sw(frame.wrapping_add(0x14), 0xa);
    let context = m.gw(CONTEXT);
    let block = m.lw(BLOCK);
    m.sw(frame.wrapping_add(0x18), GREY);
    let ot = m.lw(context.wrapping_add(ORDERING_TABLE));
    let packet = m.lw(block.wrapping_add(0x45c));
    let next = draw_text(m, c, font, title, ot, packet);
    let block = m.lw(BLOCK);
    let font = m.lw(FONT);
    let context = m.gw(CONTEXT);
    m.sw(block.wrapping_add(0x45c), next);
    m.sw(frame.wrapping_add(0x10), 8);
    m.sw(frame.wrapping_add(0x14), 0x64);
    m.sw(frame.wrapping_add(0x18), GREY);
    let packet = m.lw(block.wrapping_add(0x45c));
    let ot = m.lw(context.wrapping_add(ORDERING_TABLE));
    let next = draw_text(m, c, font, TEST_TEXT, ot, packet);
    let block = m.lw(BLOCK);
    m.sw(block.wrapping_add(0x45c), next);
}

/// 800CD6B4 (phase 2): pressed 0x40 returns to phase 0; held 4/8 move the
/// test text 2 pixels left/right, held 1/2 4 pixels up/down; draws
/// "\n A \nIHI" there.
fn position_test(m: &mut Machine, c: &mut Invoker, frame: u32) {
    let context = m.gw(CONTEXT);
    if m.lw(context.wrapping_add(PRESSED)) & 0x40 != 0 {
        let block = m.lw(BLOCK);
        m.sw(block, 0);
    }
    let context = m.gw(CONTEXT);
    let held = m.lw(context.wrapping_add(HELD));
    if held & 4 != 0 {
        let x = m.lw(TEST_X).wrapping_sub(2);
        m.sw(TEST_X, x);
    } else if held & 8 != 0 {
        let x = m.lw(TEST_X).wrapping_add(2);
        m.sw(TEST_X, x);
    }
    let context = m.gw(CONTEXT);
    let held = m.lw(context.wrapping_add(HELD));
    if held & 1 != 0 {
        let y = m.lw(TEST_Y).wrapping_sub(4);
        m.sw(TEST_Y, y);
    } else if held & 2 != 0 {
        let y = m.lw(TEST_Y).wrapping_add(4);
        m.sw(TEST_Y, y);
    }
    let font = m.lw(FONT);
    let x = m.lw(TEST_X);
    let y = m.lw(TEST_Y);
    m.sw(frame.wrapping_add(0x18), GREY);
    m.sw(frame.wrapping_add(0x10), x);
    let context = m.gw(CONTEXT);
    m.sw(frame.wrapping_add(0x14), y);
    let block = m.lw(BLOCK);
    let ot = m.lw(context.wrapping_add(ORDERING_TABLE));
    let packet = m.lw(block.wrapping_add(0x45c));
    let next = draw_text(m, c, font, POSITION_TEXT, ot, packet);
    let block = m.lw(BLOCK);
    m.sw(block.wrapping_add(0x45c), next);
}

/// 800CD998..800CDAF0: eight sprites (code 0x64, grey, CLUT 0x7FEC, 80x240
/// at column (n % 4) * 80, row n / 4 * 240), each linked into the ordering
/// table and followed by the texture page of tile n + 1 (8-bit, rate 1).
fn background_sprites(m: &mut Machine, packet: &mut u32) {
    let mut sprite = *packet;
    let mut body = sprite.wrapping_add(4);
    let mut position = TILE_POSITIONS + 4;
    let mut tile: u32 = 1;
    loop {
        let n = tile.wrapping_sub(1);
        m.sw(body, 0x6480_8080);
        m.sw(body.wrapping_add(8), 0x7fec << 16);
        let rounded = if (n as i32) < 0 {
            tile.wrapping_add(2)
        } else {
            n
        };
        m.sw(body.wrapping_add(0xc), 0x00f0_0050);
        let column = n.wrapping_sub((((rounded as i32) >> 2) << 2) as u32);
        let x = column.wrapping_mul(0x50);
        m.sh(body.wrapping_add(4), x as u16);
        if (n as i32) < 4 {
            m.sh(body.wrapping_add(6), 0);
        } else {
            m.sh(body.wrapping_add(6), 0xf0);
        }
        let context = m.gw(CONTEXT);
        body = body.wrapping_add(0x14);
        let ot = m.lw(context.wrapping_add(ORDERING_TABLE));
        tile = tile.wrapping_add(1);
        let head = m.lw(ot);
        m.sw(sprite, head | 0x0400_0000);
        let link = sprite & 0x00ff_ffff;
        sprite = sprite.wrapping_add(0x14);
        let ot = m.lw(context.wrapping_add(ORDERING_TABLE));
        m.sw(ot, link);
        let y = m.lhu(position + 2);
        let x = m.lhu(position);
        position += 4;
        m.sb(body.wrapping_sub(1), 1);
        let y = u32::from(y);
        let x = u32::from(x);
        let page = ((((y & 0x100) << 16) as i32 >> 20) as u32)
            | ((x & 0x3ff) >> 6)
            | 0xa0
            | ((((y & 0x200) as u16 as i16) as i32 as u32) << 2);
        let context = m.gw(CONTEXT);
        m.sw(body, page | 0xe100_0000);
        body = body.wrapping_add(8);
        let ot = m.lw(context.wrapping_add(ORDERING_TABLE));
        let tag = m.lw(sprite);
        let head = m.lw(ot);
        m.sw(sprite, (tag & 0xff00_0000) | (head & 0x00ff_ffff));
        let ot = m.lw(context.wrapping_add(ORDERING_TABLE));
        let head = m.lw(ot);
        m.sw(ot, (head & 0xff00_0000) | (sprite & 0x00ff_ffff));
        sprite = sprite.wrapping_add(8);
        if (tile as i32) >= 9 {
            break;
        }
    }
    *packet = sprite;
}

/// 800CEB68(font, text, ot, packet; x, y, colour in the caller's frame):
/// one 0x14-byte sprite (code 0x66 | colour) per character through the
/// character map and the font's glyph table, linked into `*ot`; '\n'
/// returns to x and advances y by 1 + the line spacing, '\r' is skipped.
/// Fonts 5..15 close up `l` and `t` after most letters and open `.`, fonts
/// 6/8 draw space and digits 5 wide; fonts 16/17 draw codes 1..6 at
/// height 0x14 and 7 6 wide and close up `g`, `c`, `i`, `t` after most
/// letters (`!` opens); fonts 18..20 draw glyphs from 0x9C at height
/// 0x14; font 21 closes up `j`, font 22 `T` and `V`. The advance is the
/// width plus [`kerning`] with the next character. A texture page
/// packet (the font's page) ends the text. Returns the packet cursor.
pub fn draw_text(
    m: &mut Machine,
    c: &mut Invoker,
    font: u32,
    text: u32,
    ot: u32,
    packet: u32,
) -> u32 {
    let frame = m.push_frame(0x48);
    let y = m.lw(frame.wrapping_add(0x5c));
    m.sw(frame.wrapping_add(0x48), font);
    let mut x = m.lw(frame.wrapping_add(0x58));
    m.sw(frame.wrapping_add(0x10), y);
    m.sw(frame.wrapping_add(0x50), ot);
    let font = m.lw(frame.wrapping_add(0x48));
    let descriptor = (font << 4).wrapping_add(FONTS);
    let length = c(m, STRLEN, [text, 0, 0, 0]);
    m.sw(frame.wrapping_add(0x14), length);
    let mut cursor = text;
    let mut body = packet.wrapping_add(10);
    let mut sprite = packet;
    let mut index: u32 = 0;
    while (index as i32) < (length as i32) {
        let ch = u32::from(m.lbu(cursor));
        cursor = cursor.wrapping_add(1);
        if ch == u32::from(b'\n') {
            x = m.lw(frame.wrapping_add(0x58));
            let line = m.lw(frame.wrapping_add(0x10));
            let spacing = m.lb(descriptor.wrapping_add(4)) as i32 as u32;
            m.sw(
                frame.wrapping_add(0x10),
                line.wrapping_add(1).wrapping_add(spacing),
            );
        } else if ch != u32::from(b'\r') {
            let glyph = u32::from(m.lbu(CHARACTER_MAP.wrapping_add(ch)));
            let table = m.lw(descriptor.wrapping_add(0xc));
            let u0 = u32::from(m.lbu(descriptor));
            let v0 = u32::from(m.lbu(descriptor.wrapping_add(1)));
            let colour = m.lw(frame.wrapping_add(0x60));
            let entry = table.wrapping_add(glyph << 2);
            let mut width = u32::from(m.lbu(entry.wrapping_add(2)));
            let u = u32::from(m.lbu(entry));
            let v = u32::from(m.lbu(entry.wrapping_add(1)));
            let rise = m.lb(entry.wrapping_add(3)) as i32 as u32;
            m.sw(body.wrapping_sub(6), colour | 0x6600_0000);
            let clut = u32::from(m.lhu(descriptor.wrapping_add(8)));
            let uv = u0.wrapping_add(u) | (v0.wrapping_add(v) << 8) | (clut << 16);
            m.sw(body.wrapping_add(2), uv);
            let font = m.lw(frame.wrapping_add(0x48));
            let previous = |m: &mut Machine| u32::from(m.lbu(cursor.wrapping_sub(2)));
            // `None`: the size word takes the font's height.
            let mut size = None;
            match font.wrapping_sub(5) {
                // Fonts 5..15; 6 and 8 first draw space and digits 5 wide.
                0..=10 => {
                    if matches!(font, 6 | 8) && (glyph == 0 || glyph.wrapping_sub(0x10) < 0xa) {
                        width = 5;
                    }
                    if (index as i32) > 0 {
                        let before = previous(m);
                        x = x.wrapping_add(narrow_lt(ch, before));
                    }
                }
                11 | 12 => {
                    if ch.wrapping_sub(1) < 6 {
                        size = Some(width | 0x0014_0000);
                    } else {
                        if ch == 7 {
                            width = 6;
                        }
                        if (index as i32) > 0 {
                            let before = previous(m);
                            x = x.wrapping_add(narrow_04(ch, before));
                        }
                    }
                }
                13..=15 if (glyph as i32) >= 0x9c => size = Some(width | 0x0014_0000),
                16 if (index as i32) > 0 && ch == u32::from(b'j') => x = x.wrapping_sub(1),
                17 if (index as i32) > 0 && (ch == u32::from(b'T') || ch == u32::from(b'V')) => {
                    x = x.wrapping_sub(1);
                }
                _ => {}
            }
            let size = match size {
                Some(size) => size,
                None => width | (u32::from(m.lbu(descriptor.wrapping_add(3))) << 16),
            };
            m.sw(body.wrapping_add(6), size);
            m.sh(body.wrapping_sub(2), x as u16);
            let line = m.lw(frame.wrapping_add(0x10));
            m.sh(body, line.wrapping_add(rise) as u16);
            let ot = m.lw(frame.wrapping_add(0x50));
            body = body.wrapping_add(0x14);
            let head = m.lw(ot);
            let font = m.lw(frame.wrapping_add(0x48));
            m.sw(sprite, head | 0x0400_0000);
            m.sw(ot, sprite & 0x00ff_ffff);
            let next = u32::from(m.lbu(cursor));
            sprite = sprite.wrapping_add(0x14);
            m.sw(frame.wrapping_add(0x18), u32::from(b'a'));
            m.sw(frame.wrapping_add(0x1c), u32::from(b'g'));
            let adjust = kerning(font, ch, next);
            x = x.wrapping_add(width.wrapping_add(adjust));
        }
        let length = m.lw(frame.wrapping_add(0x14));
        index = index.wrapping_add(1);
        if (index as i32) >= (length as i32) {
            break;
        }
    }
    m.sb(sprite.wrapping_add(3), 1);
    let page = u32::from(m.lhu(descriptor.wrapping_add(6)));
    m.sw(sprite.wrapping_add(4), (page & 0x9ff) | 0xe100_0000);
    let ot = m.lw(frame.wrapping_add(0x50));
    let tag = m.lw(sprite);
    let head = m.lw(ot);
    m.sw(sprite, (tag & 0xff00_0000) | (head & 0x00ff_ffff));
    let head = m.lw(ot);
    m.sw(ot, (head & 0xff00_0000) | (sprite & 0x00ff_ffff));
    m.pop_frame(0x48);
    sprite.wrapping_add(8)
}

/// Fonts 5..15 (800CECD4): `l` after a letter other than l b d e f i j u
/// and `t` after one other than B L Q W Z a d f g i j r w x y z close up by
/// one; `.` opens by one.
fn narrow_lt(ch: u32, before: u32) -> u32 {
    let ch = char::from_u32(ch);
    let before = char::from_u32(before);
    match ch {
        Some('l') if !matches!(before, Some('l' | 'b' | 'd' | 'e' | 'f' | 'i' | 'j' | 'u')) => {
            0xffff_ffff
        }
        Some('t')
            if !matches!(
                before,
                Some(
                    'B' | 'L'
                        | 'Q'
                        | 'W'
                        | 'Z'
                        | 'a'
                        | 'd'
                        | 'f'
                        | 'g'
                        | 'i'
                        | 'j'
                        | 'r'
                        | 'w'
                        | 'x'
                        | 'y'
                        | 'z'
                )
            ) =>
        {
            0xffff_ffff
        }
        Some('.') => 1,
        _ => 0,
    }
}

/// Fonts 16/17 (800CEDF4..): `g`, `c`, `i` and `t` close up by one after
/// letters other than the listed ones; `!` opens by one.
fn narrow_04(ch: u32, before: u32) -> u32 {
    let ch = char::from_u32(ch);
    let before = char::from_u32(before);
    let keep = match ch {
        Some('g') => matches!(
            before,
            Some('i' | 'n' | '\u{e4}' | 'r' | 'a' | 'o' | 'e' | '\u{e9}')
        ),
        Some('!') => return 1,
        Some('c') => matches!(
            before,
            Some('a' | 'o' | '\u{e9}' | 'i' | 'p' | 's' | 'n' | 'r' | '\u{e4}' | 'A')
        ),
        Some('i') => matches!(
            before,
            Some(
                'l' | 'v'
                    | 'g'
                    | 'M'
                    | 'm'
                    | 'r'
                    | 'L'
                    | 'G'
                    | 'c'
                    | 'V'
                    | '/'
                    | 's'
                    | 'o'
                    | 'w'
                    | 'f'
                    | 'a'
                    | 'd'
                    | 'E'
                    | 'u'
                    | 'n'
                    | 'R'
                    | 'e'
                    | 'h'
                    | 'z'
                    | 'b'
                    | 'Z'
                    | 'p'
                    | 'N'
                    | 'B'
                    | 'k'
                    | 'D'
                    | 'S'
                    | 'A'
                    | 'C'
                    | 'x'
                    | 'T'
                    | 'H'
            )
        ),
        Some('t') => matches!(
            before,
            Some(
                'i' | 'u'
                    | 'o'
                    | 'p'
                    | 's'
                    | 'r'
                    | 'x'
                    | 'k'
                    | 'a'
                    | 'S'
                    | '\u{e9}'
                    | '\u{c9}'
                    | '\u{e4}'
                    | 'A'
            )
        ),
        _ => return 0,
    };
    if keep { 0 } else { 0xffff_ffff }
}

/// 800CF1C8(index, file): message `index` of the context language in a
/// message file: the language table at `file + (file[1 + language] & ~3)`,
/// the text at `table + (table[1 + index] & ~3)`.
pub fn message_text(m: &mut Machine, index: u32, file: u32) -> u32 {
    let context = m.gw(CONTEXT);
    let language = m.lw(context.wrapping_add(LANGUAGE));
    let offset = m.lw((language << 2).wrapping_add(file).wrapping_add(4));
    let table = file.wrapping_add((offset >> 2) << 2);
    let offset = m.lw((index << 2).wrapping_add(table).wrapping_add(4));
    table.wrapping_add((offset >> 2) << 2)
}

/// 800CF214(group, index, file) (unreferenced): the same lookup one level
/// deeper.
pub fn message_text2(m: &mut Machine, group: u32, index: u32, file: u32) -> u32 {
    let context = m.gw(CONTEXT);
    let language = m.lw(context.wrapping_add(LANGUAGE));
    let offset = m.lw((language << 2).wrapping_add(file).wrapping_add(4));
    let table = file.wrapping_add((offset >> 2) << 2);
    let offset = m.lw((group << 2).wrapping_add(table).wrapping_add(4));
    let groups = table.wrapping_add((offset >> 2) << 2);
    let offset = m.lw((index << 2).wrapping_add(groups).wrapping_add(4));
    groups.wrapping_add((offset >> 2) << 2)
}

/// 800CF278(index, file) (unreferenced): the first word of entry `index`.
pub fn message_word(m: &mut Machine, index: u32, file: u32) -> u32 {
    let context = m.gw(CONTEXT);
    let language = m.lw(context.wrapping_add(LANGUAGE));
    let offset = m.lw((language << 2).wrapping_add(file).wrapping_add(4));
    let table = file.wrapping_add((offset >> 2) << 2);
    let offset = m.lw((index << 2).wrapping_add(table).wrapping_add(4));
    m.lw(table.wrapping_add((offset >> 2) << 2))
}

/// 800CDB24(font, left, right): the kerning of a character pair (added to
/// the advance by [`draw_text`]), per font group; fonts from 23 on have
/// none. The original is a cascade of jump tables on the font and the left
/// character and of compares on the right one; characters are Latin-1.
pub fn kerning(font: u32, left: u32, right: u32) -> u32 {
    let Some(left) = char::from_u32(left) else {
        return 0;
    };
    let right = char::from_u32(right);
    let adjust = match font {
        0 | 1 => kerning_01(left, right),
        2..=4 => kerning_02(left, right),
        5..=15 => kerning_03(font, left, right),
        16 | 17 => kerning_04(left, right),
        18..=20 => kerning_09(left, right),
        21 => kerning_upper(left, right),
        22 => kerning_lower(left, right),
        _ => 0,
    };
    adjust as u32
}

/// Fonts 0 and 1 (ASC_01 a/b).
fn kerning_01(left: char, right: Option<char>) -> i32 {
    match (left, right) {
        ('e', Some('t')) | ('o', Some('t')) => -2,
        ('F', Some('o'))
        | ('P', Some('a'))
        | ('R', Some('e'))
        | ('S', Some('t'))
        | ('a', Some('t'))
        | ('c', Some('o'))
        | ('e', Some('l' | 'x'))
        | ('f', Some('t'))
        | ('n', Some('t'))
        | ('r', Some('a' | 'd' | 'e' | 'f' | 'o' | 'p' | 's' | 't'))
        | ('s', Some('t'))
        | ('t', Some('e' | 'o' | 't' | 'u' | 'y'))
        | ('u', Some('t'))
        | ('x', Some('c')) => -1,
        _ => 0,
    }
}

/// Fonts 2..4 (ASC_02 a..c).
fn kerning_02(left: char, right: Option<char>) -> i32 {
    match (left, right) {
        ('e', Some('s')) => -2,
        ('N', Some('e'))
        | ('S', Some('c'))
        | ('c', Some('h' | 'i'))
        | ('d', Some('e'))
        | ('e', Some('n' | 'u' | 'w'))
        | ('f', Some('\u{e1}'))
        | ('g', Some('e'))
        | ('j', Some('e'))
        | ('l', Some('e' | 'i' | 'o'))
        | ('n', Some('e' | 'f' | 'u' | '\u{e7}'))
        | ('o', Some('c'))
        | ('p', Some('e'))
        | ('r', Some('e'))
        | ('s', Some('c' | 'p' | 'y' | '\u{ed}'))
        | ('u', Some('e'))
        | ('w', Some('i'))
        | ('y', Some('e'))
        | ('\u{ed}', Some('c')) => -1,
        ('i', Some('d')) => 1,
        _ => 0,
    }
}

/// Fonts 5..15 (ASC_03 a..k): left `1` closes up except in fonts 6 and 8
/// (800CE160).
fn kerning_03(font: u32, left: char, right: Option<char>) -> i32 {
    match (left, right) {
        ('O', Some('Y'))
        | ('g', Some('e' | 't'))
        | (
            'r',
            Some(
                'a' | 'c' | 'd' | 'e' | 'f' | 'i' | 'm' | 'o' | 'r' | 's' | 't' | 'u' | '\u{e9}'
                | '\u{fc}',
            ),
        ) => -2,
        ('0', Some('1'))
        | ('E', Some('R' | 'S' | 'T' | 'V'))
        | ('F', Some('A' | '\u{c1}'))
        | ('I', Some('S' | '\u{d3}'))
        | ('L', Some('O' | 'S' | 'U' | 'e'))
        | ('M', Some('A'))
        | ('N', Some('A' | 'C' | 'O' | 'S' | 'Y' | 'e' | 'o'))
        | ('O', Some('P' | 'R' | 'S' | 'T' | 'b' | 'v'))
        | ('P', Some('E' | 'O' | 'R' | 'o' | '\u{c9}'))
        | ('T', Some('A' | '\u{d3}'))
        | ('U', Some('T'))
        | ('a', Some('j' | 's' | 't' | 'v' | 'y'))
        | ('d', Some('!'))
        | ('e', Some('v'))
        | ('g', Some('a' | 'i' | 'o'))
        | ('i', Some('s' | 't' | 'v'))
        | ('l', Some('o' | '\u{f3}'))
        | ('o', Some('g' | 'n' | 'p' | 'r' | 's' | 'u' | 'v'))
        | ('t', Some('!'))
        | ('v', Some('e')) => -1,
        ('w', Some('h' | 'i')) => 1,
        ('C', Some('h')) => 0,
        ('1', _) => {
            if font == 6 || font == 8 {
                0
            } else {
                -1
            }
        }
        ('C', _) | ('D', _) | ('R', _) | ('n', _) | ('r', _) | ('\u{f1}', _) => -1,
        ('.', _) => 1,
        _ => 0,
    }
}

/// Fonts 16 and 17 (ASC_04 a/b).
fn kerning_04(left: char, right: Option<char>) -> i32 {
    match (left, right) {
        ('E', Some('S' | 'n' | 'x'))
        | ('c', Some('h'))
        | ('e', Some('d'))
        | ('g', Some('z'))
        | ('h', Some('y'))
        | ('l', Some('\u{d3}'))
        | ('t', Some('y' | '\u{ed}' | '\u{f3}'))
        | ('u', Some('y')) => -1,
        ('A', Some('R' | 'U' | 'i' | 's'))
        | ('B', Some('M'))
        | ('F', Some('F'))
        | ('G', Some('a' | 'e' | 'u'))
        | ('J', Some('u'))
        | ('N', Some('U' | 'a' | 'e'))
        | ('O', Some('F' | 'N' | 'U' | 'f' | 'p' | 'u'))
        | ('P', Some('E' | 'u'))
        | ('R', Some('B' | 'O' | 'a' | 'e' | 'i'))
        | ('S', Some('/' | 'a' | 'e' | 'i' | 'p'))
        | ('V', Some('i'))
        | ('W', Some('a'))
        | ('e', Some('e' | 's' | 't'))
        | ('i', Some('\u{f9}'))
        | ('k', Some('a' | 'b' | 'e' | 'i' | 'k' | 'p' | 's' | 't' | 'u'))
        | (
            'o',
            Some(
                '/' | 'a' | 'b' | 'e' | 'i' | 'k' | 'm' | 'p' | 'r' | 's' | 'u' | 'z' | '\u{df}'
                | '\u{f9}',
            ),
        )
        | ('s', Some('a' | 'e' | 'i' | 'k' | 'm' | 'o' | 'p' | 's' | 'u' | '\u{fa}'))
        | ('t', Some('/' | 'a' | 'i'))
        | ('u', Some('a' | 'e' | 'i' | 's'))
        | ('v', Some('e' | 'i'))
        | ('w', Some('a' | 'e' | 'i' | 'k' | 'p'))
        | ('x', Some('i' | 'p'))
        | ('\u{df}', Some('b' | 'e'))
        | ('\u{e1}', Some('i'))
        | ('\u{e4}', Some('h' | 'r' | 'u'))
        | ('\u{e8}', Some('t'))
        | ('\u{e9}', Some('a' | 'e' | 'p' | 'q' | 'r' | 's' | 'u'))
        | ('\u{f6}', Some('s')) => 1,
        ('W', Some('i')) => 2,
        ('M', Some('R' | 'S' | 'a' | 'e' | 'i' | 'o' | 'r' | 'u'))
        | (
            'a',
            Some(
                '/' | ';' | 'a' | 'b' | 'e' | 'f' | 'g' | 'i' | 'k' | 'l' | 'm' | 'n' | 'o' | 'p'
                | 'q' | 'r' | 's' | 't' | 'u' | 'v' | 'w' | 'z' | '\u{df}' | '\u{e9}' | '\u{f1}',
            ),
        )
        | (
            'i',
            Some(
                'a' | 'b' | 'e' | 'i' | 'k' | 'l' | 'm' | 'n' | 'o' | 'p' | 'q' | 'r' | 's' | 'u'
                | 'v' | 'z' | '\u{e8}' | '\u{e9}',
            ),
        )
        | (
            'n',
            Some(
                '/' | 'a' | 'b' | 'e' | 'i' | 'k' | 'm' | 'o' | 'p' | 'q' | 's' | 't' | 'u' | 'z'
                | '\u{e9}' | '\u{f6}',
            ),
        )
        | ('p', Some('/' | 'a' | 'e' | 'i' | 'm' | 'o' | 'r' | 'u'))
        | ('r', Some('.' | 'S' | 'a' | 'b' | 'c' | 'e' | 'i' | 'k' | 'm' | 's')) => 0,
        ('M', _) | ('a', _) | ('i', _) | ('n', _) | ('p', _) | ('r', _) => -1,
        ('-', _) => 1,
        _ => 0,
    }
}

/// Fonts 18..20 (ASC_09 a..c).
fn kerning_09(left: char, right: Option<char>) -> i32 {
    match (left, right) {
        ('C', Some('T')) | ('e', Some('l')) | ('r', Some('l')) => -1,
        _ => 0,
    }
}

/// Font 21 (ASC_ED UPPER).
fn kerning_upper(left: char, right: Option<char>) -> i32 {
    match (left, right) {
        ('L', Some('T')) => -2,
        ('E', Some('D' | 'S'))
        | ('M', Some('C'))
        | ('T', Some('o'))
        | ('e', Some('s'))
        | ('g', Some('o'))
        | ('u', Some('k')) => -1,
        ('(', Some('M')) | ('h', Some('i')) => 1,
        ('a', _) => -1,
        _ => 0,
    }
}

/// Font 22 (ASC_ED LOWER).
fn kerning_lower(left: char, right: Option<char>) -> i32 {
    match (left, right) {
        ('o', Some('j')) => -2,
        ('(', Some('C'))
        | ('H', Some('a'))
        | ('L', Some('T'))
        | ('T', Some('a'))
        | ('c', Some('u'))
        | ('e', Some('n'))
        | ('i', Some('a' | 'c' | 'd' | 'z'))
        | ('k', Some('a' | 's'))
        | ('o', Some('d' | 's' | 'z'))
        | ('p', Some('e'))
        | ('r', Some('a' | 'e'))
        | ('u', Some('c')) => -1,
        ('A', Some('L')) | ('G', Some('r')) => 1,
        ('V', _) | ('Y', _) => -1,
        _ => 0,
    }
}

/// Native targets of module 4 (only while it is loaded).
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if !(0x800c_9dc0..0x800d_96bc).contains(&target) || common::loaded_module(m) != MODULE {
        return None;
    }
    let [a0, a1, a2, a3] = args;
    let mut call = dispatch::call;
    let c: &mut Invoker = &mut call;
    Some(match target {
        0x800c_cc94 => {
            reset_messages(m);
            0
        }
        0x800c_ccd0 => load_backgrounds(m, c, a0),
        0x800c_cde0 => message_browser(m, c, a0, a1),
        0x800c_d1b4 => setup(m, c),
        0x800c_d440 => frame(m, c),
        0x800c_db24 => kerning(a0, a1, a2),
        0x800c_eb68 => draw_text(m, c, a0, a1, a2, a3),
        0x800c_f1c8 => message_text(m, a0, a1),
        0x800c_f214 => message_text2(m, a0, a1, a2),
        0x800c_f278 => message_word(m, a0, a1),
        _ => return None,
    })
}
