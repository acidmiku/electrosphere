//! Overlay module 1 (game state 4: the front-end menus) on the
//! address-preserving machine, translated from
//! `analysis/overlays/overlay_001` (loaded at
//! [`OVERLAY_BASE`](crate::common::OVERLAY_BASE) from `ACE.BIN`).
//!
//! State 4's callback table (`0x800D597C`) is [`initialize`] (substate 0),
//! [`frame`] (substate 1, every frame) and the exit pair [`begin_exit`] /
//! [`exit_frame`]. [`frame`] runs the nested menu dispatcher [`menus`]
//! over the nine menu states of the table at `0x800D5940` (callback pairs
//! for initialization and update), then the visual script pass and the
//! texts. The parts:
//!
//! - `ov1_load.rs`: menu states 0 and 8 (save probing and resource
//!   loading, the phase table `0x800D63D0`), resource 0x45 and its
//!   textures, CD streams;
//! - `ov1_card.rs`: memory-card state, BIOS events and save data;
//! - `ov1_menu.rs`: the language, title, main-menu, difficulty and load
//!   screens (menu states 1..5, 7);
//! - `ov1_text.rs`: text tables, string banks, the glyph renderer and its
//!   spacing table;
//! - `ov1_script.rs`: the draw-script interpreter, its 28 commands and the
//!   visual state.
//!
//! Every routine is claimed by [`dispatch`] (only while module 1 is
//! loaded). Calls into resident code go through [`dispatch::call`]
//! (`arena::alloc`/`arena::reset` directly: they have no dispatch entry).
//! Routines no code or data references (dead code) are listed in
//! [`UNREFERENCED`]; the small ones are translated as well.
use crate::{
    arena,
    common::{CONTEXT, loaded_module},
    dispatch,
    machine::{Fault, Machine, SmallData},
    ov1_card, ov1_load, ov1_menu, ov1_script, ov1_text,
};

/// The module number [`dispatch`] answers for.
pub const MODULE: u32 = 1;

/// `[0x800D8948]`: the menu block (0x809C bytes): +0 state, +2 substate,
/// +4 previous state, +6 main-menu choice, +8 load-screen choice
/// (halfwords), +0xC two 16-entry ordering tables, +0x8C the current one,
/// +0x90 two 0x4000-byte packet areas, +0x8090 the packet pointer.
pub const MENU: u32 = 0x800d_8948;
/// `[0x800DB00C]`: the visual block (`ov1_script.rs`).
pub const VISUAL: u32 = 0x800d_b00c;
/// `[0x800DB008]`: the script interpreter context (scratchpad).
pub const SCRIPT_CONTEXT: u32 = 0x800d_b008;
/// `[0x800D8740]`: the text table (`ov1_text.rs`).
pub const TEXT: u32 = 0x800d_8740;
/// `[0x800D640C]`: the memory-card block (`ov1_card.rs`).
pub const CARD: u32 = 0x800d_640c;
/// `[0x800D598C]`: the resource-loading state (`ov1_load.rs`).
pub const RESOURCE: u32 = 0x800d_598c;
/// `[gp+0x2C4]`: the resident display-request block (flags, phase, screen
/// offset, background colour +0x10..+0x12, VSync interval +0x14).
pub const DISPLAY: u32 = 0x2c4;
/// `0x800D8950`: frame counter of the exit pair.
const EXIT_COUNTER: u32 = 0x800d_8950;
/// `0x800D5940`: the nine menu-state callback-pair tables.
const MENU_STATES: u32 = 0x800d_5940;
/// `0x800D5964` / `0x800D5970`: forward / backward menu-state routes
/// (0xFF: stay, 0xFE: previous state).
const FORWARD: u32 = 0x800d_5964;
const BACKWARD: u32 = 0x800d_5970;

/// Routines nothing references (no `jal`, no stored pointer, no address
/// formed in code): dead code in this module, the address and its length
/// in instructions. Those of at most 25 instructions and 800D0358 (the one
/// among the 128 functions of the static export) are translated and
/// claimed; the rest are not.
pub const UNREFERENCED: [(u32, u32); 26] = [
    (0x800c_b3f0, 2),
    (0x800c_c62c, 4),
    (0x800c_d380, 259),
    (0x800c_d78c, 99),
    (0x800c_d918, 20),
    (0x800c_d968, 80),
    (0x800c_daa8, 42),
    (0x800c_db50, 40),
    (0x800c_dbf0, 57),
    (0x800c_dcd4, 114),
    (0x800c_e354, 29),
    (0x800c_e3c8, 135),
    (0x800c_e778, 51),
    (0x800c_e844, 79),
    (0x800c_e980, 43),
    (0x800c_ea2c, 32),
    (0x800d_019c, 25),
    (0x800d_0200, 21),
    (0x800d_0358, 34),
    (0x800d_03e0, 54),
    (0x800d_0578, 4),
    (0x800d_0588, 24),
    (0x800d_0798, 95),
    (0x800d_52f4, 14),
    (0x800d_54a8, 6),
    (0x800d_570c, 6),
];

// ---- helpers shared by the ov1 parts ---------------------------------------------

pub(crate) const fn off(base: u32, offset: u32) -> u32 {
    base.wrapping_add(offset)
}
/// `lh`, sign-extended to a register value.
pub(crate) fn lh(m: &mut Machine, address: u32) -> u32 {
    m.lh(address) as u32
}
pub(crate) fn lhu(m: &mut Machine, address: u32) -> u32 {
    u32::from(m.lhu(address))
}
/// `lb`, sign-extended to a register value.
pub(crate) fn lb(m: &mut Machine, address: u32) -> u32 {
    m.lb(address) as u32
}
pub(crate) fn lbu(m: &mut Machine, address: u32) -> u32 {
    u32::from(m.lbu(address))
}
/// `sra`.
pub(crate) fn sra(value: u32, shift: u32) -> u32 {
    ((value as i32) >> shift) as u32
}
/// An `lwl/lwr` + `swl/swr` pair: copy the (possibly unaligned) word at
/// `source` to `destination`.
pub(crate) fn copy_word(m: &mut Machine, source: u32, destination: u32) {
    let bytes = m.read_bytes(source, 4);
    m.write_bytes(destination, &bytes);
}
/// The `lwl a0,2(bin); swl a0,2(packet); swl packet<<8,2(bin)` idiom
/// (addPrim): the packet takes the 24-bit link of the ordering-table entry
/// `bin`, which then links to the packet; both length bytes stay.
/// Packets and entries are word aligned.
pub(crate) fn link(m: &mut Machine, bin: u32, packet: u32) {
    let next = m.read_bytes(bin, 3);
    m.write_bytes(packet, &next);
    m.write_bytes(bin, &packet.to_le_bytes()[..3]);
}
/// Signed `div` quotient with the compiler's `break 7` (zero divisor, at
/// `zero_break`) and `break 6` (`0x80000000 / -1`, at `overflow_break`)
/// checks: a reached trap records [`Fault::Trap`] and yields the MIPS
/// result.
pub(crate) fn div_trap(
    m: &mut Machine,
    numerator: u32,
    divisor: u32,
    zero_break: u32,
    overflow_break: u32,
) -> u32 {
    let (n, d) = (numerator as i32, divisor as i32);
    if d == 0 {
        m.raise(Fault::Trap {
            address: zero_break,
            code: 7,
        });
        return if n < 0 { 1 } else { u32::MAX };
    }
    if d == -1 && n == i32::MIN {
        m.raise(Fault::Trap {
            address: overflow_break,
            code: 6,
        });
        return n as u32;
    }
    (n / d) as u32
}
pub(crate) fn context(m: &mut Machine) -> u32 {
    m.gw(CONTEXT)
}
pub(crate) fn menu(m: &mut Machine) -> u32 {
    m.lw(MENU)
}

// ---- state-4 callbacks ------------------------------------------------------------

/// 800CB2BC (state 4, substate 0): display requests 9 with phase 1 and a
/// black background, the menu block (state/substate cleared; context
/// +0x970 bit 0x10 cleared; a fresh context (+0x970 zero) sets return flag
/// +0x964 = 1, return flag 2 restores the saved pair +0x394 into +0x390
/// and return flag 4 starts in menu state 8), then the card, visual and
/// text setups. Returns NEXT_SUBSTATE.
pub fn initialize(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let display = m.gw(DISPLAY);
    m.sb(off(display, 0x10), 0);
    let display2 = m.gw(DISPLAY);
    m.sw(off(display, 4), 1);
    m.sw(off(display, 0x14), 0);
    m.sb(off(display2, 0x11), 0);
    let display = m.gw(DISPLAY);
    m.sb(off(display, 0x12), 0);
    let display = m.gw(DISPLAY);
    let flags = m.lw(display);
    m.sw(display, flags | 9);
    let block = arena::alloc(m, 0x809c);
    let context = context(m);
    for k in 0..6 {
        m.sh(off(block, 2 * k), 0);
    }
    let progress = m.lw(off(context, 0x970));
    m.sw(MENU, block);
    let progress = progress & !0x10;
    m.sw(off(context, 0x970), progress);
    if progress == 0 {
        m.sw(off(context, 0x964), 1);
    } else {
        let flags = m.lw(off(context, 0x964));
        if flags & 2 != 0 {
            m.sh(block, 0);
            let flags = m.lw(off(context, 0x964));
            let first = lhu(m, off(context, 0x394));
            let second = lhu(m, off(context, 0x396));
            m.sw(off(context, 0x964), flags & !2);
            m.sh(off(context, 0x390), first as u16);
            m.sh(off(context, 0x392), second as u16);
        } else if flags & 4 != 0 {
            m.sh(block, 8);
            let flags = m.lw(off(context, 0x964));
            m.sw(off(context, 0x964), flags & !4);
        }
    }
    ov1_card::initialize(m, 0, 0);
    ov1_script::init_visuals(m);
    ov1_script::set_scripts(m, 0);
    ov1_load::reset_streams(m);
    ov1_text::reset_texts(m);
    m.pop_frame(0x18);
    0x0400_0000
}

/// 800CB3F0 (unreferenced): returns NEXT_SUBSTATE.
pub fn next_substate() -> u32 {
    0x0400_0000
}

/// 800CB3F8: select the menu ordering table and packet area of the
/// context's buffer (+4), clear the table (ClearOTagR, 16 entries), splice
/// it into the context ordering table (+8) and put the visual table in
/// front of its last entry.
pub fn begin_menu_frame(m: &mut Machine) {
    m.push_frame(0x20);
    let context = context(m);
    let block = menu(m);
    let buffer = m.lw(off(context, 4));
    let ot = off(block, (buffer << 6).wrapping_add(0xc));
    m.sw(off(block, 0x8c), ot);
    let buffer = m.lw(off(context, 4));
    let packets = off(block, (buffer << 14).wrapping_add(0x90));
    m.sw(off(block, 0x8090), packets);
    let ot = m.lw(off(block, 0x8c));
    dispatch::call(m, ov1_script::CLEAR_OTAG_R, [ot, 0x10, 0, 0]);
    let block = menu(m);
    let context = self::context(m);
    let ot = m.lw(off(block, 0x8c));
    let main = m.lw(off(context, 8));
    let head = m.lw(ot);
    let target = m.lw(main);
    m.sw(ot, (head & 0xff00_0000) | (target & 0x00ff_ffff));
    let ot = m.lw(off(block, 0x8c));
    let main = m.lw(off(context, 8));
    let word = m.lw(main);
    m.sw(
        main,
        (word & 0xff00_0000) | (ot.wrapping_add(0x3c) & 0x00ff_ffff),
    );
    let ot = m.lw(off(block, 0x8c));
    ov1_script::begin_visual_frame(m, ot.wrapping_add(0x3c));
    m.pop_frame(0x20);
}

/// 800CB4C8 (state 4, substate 1): one menu frame. Begins the frame, runs
/// [`menus`]; a nonzero result is returned to the resident dispatcher.
/// Otherwise draws the visual page and the texts into the packet area,
/// turns rotation records 1 and 2 by +/-64 per frame counter (context
/// +0x20), sets style 0x78 from held input bit 0x10 (+0x124), polls the
/// stream and returns 0.
pub fn frame(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    begin_menu_frame(m);
    // 800CB3F8 leaves a1 = the visual ordering table (800D566C), a2 =
    // 0xFF000000 and a3 = the visual block; the menu callbacks receive them.
    let visual = m.lw(VISUAL);
    let table = m.lw(off(visual, 8));
    let result = menus(m, [0, table, 0xff00_0000, visual]);
    if result == 0 {
        let block = menu(m);
        let packet = m.lw(off(block, 0x8090));
        let packet = ov1_script::draw_visuals(m, packet);
        let block = menu(m);
        m.sw(off(block, 0x8090), packet);
        let ot = m.lw(off(block, 0x8c));
        let packet = ov1_text::draw_texts(m, packet, ot);
        let block = menu(m);
        let context = context(m);
        m.sw(off(block, 0x8090), packet);
        let counter = m.lw(off(context, 0x20));
        ov1_script::set_rotation_angle(m, 1, counter << 6);
        let context = self::context(m);
        let counter = m.lw(off(context, 0x20));
        ov1_script::set_rotation_angle(m, 2, counter.wrapping_neg() << 6);
        let context = self::context(m);
        let held = m.lw(off(context, 0x124));
        ov1_script::set_style(m, 0x78, u32::from(held & 0x10 != 0));
        ov1_load::poll_stream(m);
    }
    m.pop_frame(0x20);
    result
}

/// 800CB5B4 (state 4 substate 2, and menu state 6 initialization): clear
/// the exit counter, select page 0, hide all texts; returns 1.
pub fn begin_exit(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    m.sw(EXIT_COUNTER, 0);
    ov1_script::select_page(m, 0);
    ov1_text::clear_visibility(m);
    m.pop_frame(0x18);
    1
}

/// 800CB5E4 (state 4 substate 3, and menu state 6 update): count three
/// frames; then stop the stream, close the card events, set return flag
/// 0x10 (context +0x964) and return 4 (the hub route).
pub fn exit_frame(m: &mut Machine) -> u32 {
    let counter = m.lw(EXIT_COUNTER);
    m.push_frame(0x18);
    let result = if (counter as i32) < 3 {
        m.sw(EXIT_COUNTER, counter.wrapping_add(1));
        0
    } else {
        ov1_load::stop_stream(m);
        ov1_card::close_events(m);
        let context = context(m);
        let flags = m.lw(off(context, 0x964));
        m.sw(off(context, 0x964), flags | 0x10);
        4
    };
    m.pop_frame(0x18);
    result
}

/// 800CAFC0: the nested menu dispatcher. Calls callback `[substate]` of
/// menu state `[state]` (table `0x800D5940`, a0 = the menu block) and
/// applies its result: 1 next substate; 4 returns SET_STATE|RESET_ARENA
/// state 3 (`0x60000003`); 8 saves the current pair, sets mode 4 (+0x3A4),
/// clears +0x3AC, sets return flag 2 and returns state 5 (`0x60000005`);
/// 0x10 and 0x40 restart at menu state 0; 0x100 leaves for the next state
/// (state 0: 1, or 2 once language is chosen (+0x970 bit 0); state 3 by
/// the main-menu choice: 4, 5, or 6 with return flag 0x10; state 5 by the
/// load choice: 7, 6 with progress flag 0x10, else 6; others by
/// [`FORWARD`]); 0x200 goes back by [`BACKWARD`]. Returns 0 otherwise.
/// The callback receives the menu block and the caller's `a1..a3`.
pub fn menus(m: &mut Machine, args: [u32; 4]) -> u32 {
    m.push_frame(0x18);
    let block = menu(m);
    let state = lh(m, block);
    let table = m.lw(off(MENU_STATES, state << 2));
    let substate = lh(m, off(block, 2));
    let callback = m.lw(off(table, substate << 2));
    let result = dispatch::call(m, callback, [block, args[1], args[2], args[3]]);
    let value = match result {
        1 => {
            let block = menu(m);
            let substate = lhu(m, off(block, 2));
            m.sh(off(block, 2), substate.wrapping_add(1) as u16);
            0
        }
        4 => 0x6000_0003,
        8 => {
            let context = context(m);
            m.sw(off(context, 0x3a4), 4);
            let flags = m.lw(off(context, 0x964));
            let first = lhu(m, off(context, 0x390));
            let second = lhu(m, off(context, 0x392));
            m.sb(off(context, 0x3ac), 0);
            m.sw(off(context, 0x964), flags | 2);
            m.sh(off(context, 0x394), first as u16);
            m.sh(off(context, 0x396), second as u16);
            0x6000_0005
        }
        0x10 | 0x40 => {
            let block = menu(m);
            m.sh(block, 0);
            m.sh(off(block, 2), 0);
            0
        }
        0x100 => {
            forward(m);
            0
        }
        0x200 => {
            backward(m);
            0
        }
        _ => 0,
    };
    m.pop_frame(0x18);
    value
}

/// The 0x100 route of [`menus`] (`0x800CB0C0`).
fn forward(m: &mut Machine) {
    let block = menu(m);
    let raw = lhu(m, block);
    let state = lh(m, block);
    m.sh(off(block, 2), 0);
    m.sh(off(block, 4), raw as u16);
    match state {
        0 => {
            let context = context(m);
            let progress = m.lw(off(context, 0x970));
            m.sh(block, if progress & 1 != 0 { 2 } else { 1 });
        }
        3 => match lh(m, off(block, 6)) {
            0 => m.sh(block, 4),
            1 => m.sh(block, 5),
            2 => {
                let context = context(m);
                let flags = m.lw(off(context, 0x964));
                m.sw(off(context, 0x964), flags | 0x10);
                m.sh(block, 6);
            }
            _ => {}
        },
        5 => match lh(m, off(block, 8)) {
            6 => m.sh(block, 7),
            7 => {
                let context = context(m);
                let progress = m.lw(off(context, 0x970));
                m.sw(off(context, 0x970), progress | 0x10);
                m.sh(block, 6);
            }
            _ => {
                let block = menu(m);
                m.sh(block, 6);
            }
        },
        _ => {
            let route = off(state, FORWARD);
            if lbu(m, route) != 0xff {
                let next = lbu(m, route);
                m.sh(block, next as u16);
            }
        }
    }
}

/// The 0x200 route of [`menus`] (`0x800CB220`).
fn backward(m: &mut Machine) {
    let block = menu(m);
    let state = lh(m, block);
    m.sh(off(block, 2), 0);
    let route = off(state, BACKWARD);
    match lbu(m, route) {
        0xfe => {
            let previous = lhu(m, off(block, 4));
            m.sh(block, previous as u16);
        }
        0xff => {}
        _ => {
            let next = lbu(m, route);
            m.sh(block, next as u16);
        }
    }
}

// ---- dispatch ---------------------------------------------------------------------

/// Overlay-1 routines by original address (only while module 1 is
/// loaded): a0..a3 in, v0 out (0 for routines without a meaningful one);
/// stack arguments are read from sp + 0x10.. by the routines.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    let [a0, a1, a2, a3] = args;
    Some(match target {
        // ov1.rs
        0x800c_afc0 => menus(m, args),
        0x800c_b2bc => initialize(m),
        0x800c_b3f0 => next_substate(),
        0x800c_b3f8 => {
            begin_menu_frame(m);
            0
        }
        0x800c_b4c8 => frame(m),
        0x800c_b5b4 => begin_exit(m),
        0x800c_b5e4 => exit_frame(m),
        // ov1_load.rs
        0x800c_b648 => {
            ov1_load::poll_slots(m);
            0
        }
        0x800c_b690 => ov1_load::card_begin(m),
        0x800c_b6ec => ov1_load::card_wait_probe(m),
        0x800c_b70c => ov1_load::card_has_file(m),
        0x800c_b738 => ov1_load::card_read(m),
        0x800c_b7a4 => ov1_load::card_skip(),
        0x800c_b7ac => ov1_load::card_apply(m),
        0x800c_b7cc => ov1_load::card_finish(m),
        0x800c_b828 => ov1_load::card_abandon(m),
        0x800c_b884 => ov1_load::resource_boot_scripts(m),
        0x800c_b8b0 => ov1_load::resource_page(m),
        0x800c_b8dc => ov1_load::resource_request(m),
        0x800c_b94c => ov1_load::resource_wait(m),
        0x800c_b964 => ov1_load::resource_install(m),
        0x800c_ba04 => ov1_load::resource_textures(m),
        0x800c_ba54 => ov1_load::resource_done(),
        0x800c_ba5c => {
            ov1_load::set_page(m, a0);
            0
        }
        0x800c_ba6c => {
            ov1_load::install_scripts(m);
            0
        }
        0x800c_e5e4 => {
            ov1_load::begin_textures(m, a0, a1, a2);
            0
        }
        0x800c_e644 => ov1_load::upload_textures(m),
        0x800d_0254 => {
            ov1_load::reset_streams(m);
            0
        }
        0x800d_0280 => {
            ov1_load::select_stream(m, a0);
            0
        }
        0x800d_0290 => {
            ov1_load::start_stream(m);
            0
        }
        0x800d_02b0 => {
            ov1_load::stop_stream(m);
            0
        }
        0x800d_02ec => {
            ov1_load::poll_stream(m);
            0
        }
        0x800d_0358 => ov1_load::decoded_audio(m, a0),
        0x800d_0914 => {
            ov1_load::reset_tables(m);
            0
        }
        0x800d_097c => ov1_load::boot_start(m),
        0x800d_09b4 => ov1_load::boot_frame(m, args),
        0x800d_0b6c => ov1_load::boot_done(),
        0x800d_0b74 => ov1_load::boot_init(m),
        0x800d_0bf4 => ov1_load::boot_update(m, args),
        0x800d_0c7c => ov1_load::return_start(),
        0x800d_0c84 => ov1_load::return_load(m, args),
        0x800d_0cfc => ov1_load::return_done(),
        0x800d_0d04 => ov1_load::return_init(m),
        0x800d_0d38 => ov1_load::return_update(m, args),
        // ov1_card.rs
        0x800c_baa8 => ov1_card::reset_state(m),
        0x800c_bb20 => ov1_card::card_ready(m),
        0x800c_bb50 => ov1_card::test_card_events(m, a0, a1),
        0x800c_bc30 => {
            ov1_card::drain_card_events(m);
            0
        }
        0x800c_bc9c => {
            ov1_card::drain_file_events(m);
            0
        }
        0x800c_bcf4 => ov1_card::test_file_events(m, a0, a1),
        0x800c_bdac => {
            ov1_card::poll(m);
            0
        }
        0x800c_c354 => ov1_card::slot_state(m, a0).0,
        0x800c_c46c => {
            ov1_card::open_events(m);
            0
        }
        0x800c_c574 => {
            ov1_card::enable_events(m);
            0
        }
        0x800c_c62c => {
            ov1_card::clear_changed(m);
            0
        }
        0x800c_c63c => {
            ov1_card::disable_events(m);
            0
        }
        0x800c_c6f4 => {
            ov1_card::close_events(m);
            0
        }
        0x800c_c7ac => {
            ov1_card::load_icons(m, a0, a1);
            0
        }
        0x800c_c8c8 => {
            ov1_card::init_header(m, a0, a1, a2);
            0
        }
        0x800c_c9ac => {
            ov1_card::init_record(m, a0);
            0
        }
        0x800c_cbc0 => {
            ov1_card::clear_record(m, a0);
            0
        }
        0x800c_cbc8 => {
            ov1_card::init_names(m);
            0
        }
        0x800c_ccb8 => {
            ov1_card::initialize(m, a0, a1);
            0
        }
        0x800c_cd50 => ov1_card::last_choice(m),
        0x800c_cd68 => {
            ov1_card::restore_language(m);
            0
        }
        0x800c_cda4 => {
            ov1_card::apply_options(m);
            0
        }
        0x800c_d224 => {
            ov1_card::apply_save(m, a0);
            0
        }
        0x800c_d918 => ov1_card::format_card(m, a0),
        0x800c_de9c => ov1_card::check_record(m).0,
        0x800c_dfa4 => {
            ov1_card::check_saves(m);
            0
        }
        0x800c_e0fc => ov1_card::read_save(m, a0, a1).0,
        0x800c_e300 => ov1_card::save_record(m, a0),
        // ov1_text.rs
        0x800c_eaac => ov1_text::kerning(a0, a1, a2),
        0x800c_faf0 => ov1_text::draw_text(m, a0, a1, a2, a3),
        0x800d_0150 => ov1_text::string(m, a0, a1),
        0x800d_019c => ov1_text::string3(m, a0, a1, a2),
        0x800d_0200 => ov1_text::entry_word(m, a0, a1),
        0x800d_04b8 => {
            ov1_text::reset_texts(m);
            0
        }
        0x800d_04f4 => {
            ov1_text::set_texts(m, a0, a1);
            0
        }
        0x800d_050c => {
            ov1_text::set_visible(m, a0, a1);
            0
        }
        0x800d_0524 => {
            ov1_text::set_font(m, a0, a1);
            0
        }
        0x800d_0548 => {
            ov1_text::clear_visibility(m);
            0
        }
        0x800d_0578 => {
            ov1_text::set_format_argument(m, a0);
            0
        }
        0x800d_0588 => {
            ov1_text::spaces_to_code7(m, a0);
            0
        }
        0x800d_05e8 => ov1_text::draw_texts(m, a0, a1),
        // ov1_menu.rs
        0x800d_0e78 => ov1_menu::language_rows(m, a0, a1, a2, a3),
        0x800d_0fa4 => ov1_menu::language_intro(m),
        0x800d_103c => ov1_menu::language_input(m),
        0x800d_11b4 => ov1_menu::language_exit(m),
        0x800d_13e4 => ov1_menu::language_init(m),
        0x800d_14e0 => ov1_menu::language_update(m, args),
        0x800d_1578 => ov1_menu::title_fade(m),
        0x800d_1794 => ov1_menu::title_wait(m),
        0x800d_1800 => {
            ov1_menu::title_emblems(m);
            0
        }
        0x800d_18b0 => ov1_menu::title_init(m),
        0x800d_1900 => ov1_menu::title_update(m, args),
        0x800d_19e4 => {
            ov1_menu::main_styles(m);
            0
        }
        0x800d_1b58 => ov1_menu::main_init(m),
        0x800d_1c1c => ov1_menu::main_update(m),
        0x800d_1d0c => {
            ov1_menu::reset_progress(m);
            0
        }
        0x800d_1d78 => ov1_menu::difficulty_start(),
        0x800d_1d80 => ov1_menu::difficulty_input(m),
        0x800d_1eac => ov1_menu::difficulty_commit(m),
        0x800d_1ef4 => ov1_menu::difficulty_init(m),
        0x800d_1f68 => ov1_menu::difficulty_update(m, args),
        0x800d_1ff0 => ov1_menu::load_probe(m),
        0x800d_204c => ov1_menu::load_check_card(m),
        0x800d_2098 => ov1_menu::load_wait_card(m),
        0x800d_2108 => ov1_menu::load_check_slot(m),
        0x800d_2178 => ov1_menu::load_wait_slot_2178(m),
        0x800d_21fc => ov1_menu::load_check_format(m),
        0x800d_2264 => ov1_menu::load_wait_slot_2264(m),
        0x800d_22d8 => ov1_menu::load_check_file(m),
        0x800d_234c => ov1_menu::load_wait_slot_234c(m),
        0x800d_23c0 => ov1_menu::load_read(m),
        0x800d_2488 => ov1_menu::load_wait_slot_2488(m),
        0x800d_250c => ov1_menu::load_skip(),
        0x800d_251c => ov1_menu::load_wait_slot_251c(m),
        0x800d_25a0 => ov1_menu::load_apply_options(m),
        0x800d_25c0 => ov1_menu::load_list(m, a1),
        0x800d_26cc => ov1_menu::load_select(m),
        0x800d_2b24 => ov1_menu::load_apply(m),
        0x800d_2ba4 => ov1_menu::load_init(m),
        0x800d_2c3c => ov1_menu::load_update(m, args),
        // ov1_script.rs
        0x800d_2d60 => {
            ov1_script::double_row(m, a0);
            0
        }
        0x800d_2d88 => ov1_script::animate(m, a0, a1),
        0x800d_33c8 => ov1_script::quad_corners(m, a0, a1, a2, a3),
        0x800d_3634 => {
            ov1_script::command_end();
            0
        }
        0x800d_363c => {
            ov1_script::command_tile(m, a0);
            0
        }
        0x800d_3824 => {
            ov1_script::command_sprite(m, a0);
            0
        }
        0x800d_3ae0 => {
            ov1_script::command_quad(m, a0);
            0
        }
        0x800d_4058 => {
            ov1_script::command_draw_mode(m, a0);
            0
        }
        0x800d_40cc => {
            ov1_script::command_draw_area(m, a0);
            0
        }
        0x800d_42b4 => {
            ov1_script::command_line(m, a0);
            0
        }
        0x800d_4444 => {
            ov1_script::command_line3(m, a0);
            0
        }
        0x800d_4608 => {
            ov1_script::command_line4(m, a0);
            0
        }
        0x800d_47f4 => {
            ov1_script::command_color_slot(m, a0);
            0
        }
        0x800d_4808 => {
            ov1_script::command_set_color(m, a0);
            0
        }
        0x800d_4844 => {
            ov1_script::command_blend(m, a0);
            0
        }
        0x800d_4858 => {
            ov1_script::command_layer(m, a0);
            0
        }
        0x800d_4878 => {
            ov1_script::command_select(m, a0);
            0
        }
        0x800d_48ac => {
            ov1_script::command_select_half(m, a0);
            0
        }
        0x800d_48dc => {
            ov1_script::command_case(m, a0);
            0
        }
        0x800d_48f0 => {
            ov1_script::command_case_minus_one(m, a0);
            0
        }
        0x800d_4920 => {
            ov1_script::command_clear_flag(m);
            0
        }
        0x800d_4940 => {
            ov1_script::command_case_source(m, a0);
            0
        }
        0x800d_4988 => {
            ov1_script::command_offset_slot(m, a0);
            0
        }
        0x800d_499c => {
            ov1_script::command_translate(m, a0);
            0
        }
        0x800d_49d0 => {
            ov1_script::command_set_animation(m, a0);
            0
        }
        0x800d_4a3c => {
            ov1_script::command_select_animation(m, a0);
            0
        }
        0x800d_4a60 => {
            ov1_script::command_set_rotation(m, a0);
            0
        }
        0x800d_4a9c => {
            ov1_script::command_select_rotation(m, a0);
            0
        }
        0x800d_4ab0 => {
            ov1_script::command_call(m, a0);
            0
        }
        0x800d_4bbc => {
            ov1_script::command_sprite_frames(m, a0);
            0
        }
        0x800d_4f9c => {
            ov1_script::command_tile_frames(m, a0);
            0
        }
        0x800d_51e0 => {
            ov1_script::run_script(m, a0);
            0
        }
        0x800d_52dc => {
            ov1_script::set_style(m, a0, a1);
            0
        }
        0x800d_52f4 => {
            ov1_script::set_color(m, a0, a1, a2, a3);
            0
        }
        0x800d_532c => {
            ov1_script::pulse_color(m, a0, a1, a2);
            0
        }
        0x800d_5458 => {
            ov1_script::set_offset(m, a0, a1, a2);
            0
        }
        0x800d_5474 => {
            ov1_script::get_offset(m, a0, a1, a2);
            0
        }
        0x800d_54a8 => {
            ov1_script::set_animation_time(m, a0, a1);
            0
        }
        0x800d_54c0 => {
            ov1_script::set_rotation_angle(m, a0, a1);
            0
        }
        0x800d_54d8 => {
            ov1_script::select_page(m, a0);
            0
        }
        0x800d_54e8 => {
            ov1_script::init_visuals(m);
            0
        }
        0x800d_5608 => {
            ov1_script::set_scripts(m, a0);
            0
        }
        0x800d_566c => {
            ov1_script::begin_visual_frame(m, a0);
            0
        }
        0x800d_570c => ov1_script::visual_table(m),
        0x800d_5724 => ov1_script::draw_visuals(m, a0),
        _ => return None,
    })
}
