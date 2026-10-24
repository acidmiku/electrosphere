//! JP overlay 4, part 1: state 8, the "purge" terminal, the routines
//! 800CD734..800CF2A7 (20 functions, `jpwave/ov4-part1.tsv`), translated
//! from the JP instructions.
//!
//! Module 4 is BPB entry 4 (21,860 bytes at 0x800CBFE8: the font image,
//! strings, code from 0x800CD734, the state's callback table 0x800CF2A8,
//! the terminal script 0x800D0D50, the subsystem names 0x800D0FC4). The
//! resident movie state 5 enters state 8 for movie 0x1D
//! (`res_p1_states::movie_setup`); state 8 returns to context +0x3A0.
//!
//! State 8 shows a computer terminal: after a sweep in (phase 1,
//! [`sweep`]) and a fade, the script ([`super::ov4_p1_text`])
//! types its messages, the player's name (context +0x948, "Nemo" when
//! empty) and the "... purged" lines of every subsystem, then the screen
//! fades out and the display is restored.
//!
//! | State 8 substate | Callback | Rust |
//! | ---: | --- | --- |
//! | 0 | `800CED74` | [`start`] |
//! | 1 | `800CEEFC` | [`frame`] |
//!
//! The overlay also holds an unreferenced music player (800CD734,
//! 800CD780, the strings "MUSIC PLAYER (SAMPLE)" at 0x800CC0FC: PAL
//! overlay 4's earlier one, [`super::ov4_p1_music`]) and the unreferenced
//! cursor placement 800CE070.
use super::ov3_p1::{add_prim, call, ctx, framed, hi_s, lo_s, slt, sra};
use super::ov4_p1_text::{clear, draw, script};
use crate::common::loaded_module;
use crate::machine::Machine;

/// The module number (context +0x38 while it is loaded).
pub const MODULE: u32 = 4;

/// `[gp+0x2D4]` (0x800BCE48): the display request block.
const DISPLAY: u32 = 0x800b_ce48;
/// `[0x800D1544]`: the terminal block (0x464 bytes,
/// [`super::ov4_p1_text`]); `[0x800D1548]`: the sweep's block (0xC bytes:
/// two 0x11C-byte packet areas and the current one).
pub const TERMINAL: u32 = 0x800d_1544;
const SWEEP: u32 = 0x800d_1548;
/// 0x800D1520: the purge index; 0x800D1524: the fade level; 0x800D1528:
/// the sweep's frame; 0x800D1530: five tpages (<< 16) of the sweep.
const PURGE_INDEX: u32 = 0x800d_1520;
const FADE: u32 = 0x800d_1524;
const SWEEP_FRAME: u32 = 0x800d_1528;
const SWEEP_PAGES: u32 = 0x800d_1530;
/// 0x800D1504: two display rectangles (by buffer) MoveImage copies to
/// 0x2C0,0.
const DISPLAY_RECTS: u32 = 0x800d_1504;
/// 0x800CF4D0: the font TIM; 0x800CD6FC: "Nemo".
const FONT_TIM: u32 = 0x800c_f4d0;
const DEFAULT_NAME: u32 = 0x800c_d6fc;
/// The script and its strings.
const SCRIPT: u32 = 0x800d_0d50;
const STRINGS: u32 = 0x800d_0d10;

/// Resident and SDK routines (JP addresses).
const ALLOC: u32 = 0x8002_7410;
const UPLOAD_TIM: u32 = 0x8002_71a0;
const GET_TPAGE: u32 = 0x800a_c6c8;
const MOVE_IMAGE: u32 = 0x800a_8500;
const RSIN: u32 = 0x800a_60f8;
const SET_DRAW_MODE: u32 = 0x800a_c7f8;
const RESET_CD_QUEUE: u32 = 0x8002_c6a8;
/// 80093B2C (resident, `ai`): the screen fade (`ot`, `packets`, `rect`,
/// `level`).
const FADE_SCREEN: u32 = 0x8009_3b2c;

const NEXT_SUBSTATE: u32 = 0x0400_0000;
const RESET_AND_SET_STATE: u32 = 0x6000_0000;

/// `[TERMINAL]`.
pub(crate) fn terminal(m: &mut Machine) -> u32 {
    m.lw(TERMINAL)
}

/// 800CED74 (JP, new; state 8, substate 0): display phase 1 (requests |1,
/// interval 0), context bit 0x10000000; the terminal block (two 0x65F0-byte
/// packet areas), the sweep block (two 0x11C-byte areas), the screen
/// cleared, no cursor, the script at its start; the font TIM uploaded;
/// "Nemo" as the player's name when it is empty; the five sweep tpages
/// (GetTPage(2, 1, 0x2C0 + 0x40i, 0)); the display copied ([`show`]).
/// Returns NEXT_SUBSTATE.
pub fn start(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        let display = m.lw(DISPLAY);
        let context = ctx(m);
        m.sw(display.wrapping_add(4), 1);
        let flags = m.lw(display) | 1;
        m.sw(display.wrapping_add(0x14), 0);
        m.sw(display, flags);
        let word = m.lw(context) | 0x1000_0000;
        m.sw(context, word);
        let t = call(m, ALLOC, [0x464, 0, 0, 0]);
        m.sw(TERMINAL, t);
        let s = call(m, ALLOC, [0xc, 0, 0, 0]);
        m.sw(SWEEP, s);
        let area = call(m, ALLOC, [0x65f0, 0, 0, 0]);
        let t = terminal(m);
        m.sw(t.wrapping_add(0x454), area);
        let area = call(m, ALLOC, [0x65f0, 0, 0, 0]);
        let t = terminal(m);
        m.sw(t.wrapping_add(0x458), area);
        let area = call(m, ALLOC, [0x11c, 0, 0, 0]);
        let s = m.lw(SWEEP);
        m.sw(s, area);
        let area = call(m, ALLOC, [0x11c, 0, 0, 0]);
        let s = m.lw(SWEEP);
        m.sw(s.wrapping_add(4), area);
        clear(m);
        let t = terminal(m);
        m.sb(t.wrapping_add(0x452), 0);
        let t2 = terminal(m);
        m.sw(t, 0);
        m.sb(t2.wrapping_add(0x453), 0);
        let t = terminal(m);
        m.sh(t.wrapping_add(0x460), 0);
        m.sh(t.wrapping_add(0x462), 0);
        call(m, UPLOAD_TIM, [FONT_TIM, 0, 0, 0]);
        let context = ctx(m);
        if m.lbu(context.wrapping_add(0x948)) == 0 {
            let word = m.lw(DEFAULT_NAME);
            let last = m.lb(DEFAULT_NAME + 4) as u8;
            m.sw(context.wrapping_add(0x948), word);
            m.sb(context.wrapping_add(0x94c), last);
        }
        m.sw(PURGE_INDEX, 0);
        m.sw(FADE, 0);
        m.sw(SWEEP_FRAME, 0);
        for i in 0..5u32 {
            let page = call(m, GET_TPAGE, [2, 1, 0x2c0 + 0x40 * i, 0]);
            m.sw(SWEEP_PAGES + 4 * i, page << 16);
        }
        show(m);
        NEXT_SUBSTATE
    })
}

/// 800CEEFC (JP, new; state 8, substate 1): one frame: the screen
/// ([`draw`] at 4,0x28 in grey into the context's table) and the phase
/// (terminal +0): 0 the sweep starts; 1 the sweep ([`sweep`]); 2 fade the
/// level down by 0x88 to -0x1000; 3 display phase 0 (requests |8); 4, 5
/// the first script step (cursor 1); 6 fade in by 0x2A; 7 the script until
/// it ends; 8 fade out by 0x2A; 9; 10 stop the CD queue and restore the
/// display (requests |9); 11 SET_STATE to context +0x3A0 with an arena
/// reset. Then the fade (80093B2C over 0,0 320x480 in the scratchpad) and
/// 0.
pub fn frame(m: &mut Machine) -> u32 {
    framed(m, 0x28, |m, sp| {
        const RECT: u32 = 0x1f80_0000;
        m.sh(RECT + 4, 0x140);
        m.sh(RECT + 6, 0x1e0);
        let context = ctx(m);
        let t = terminal(m);
        m.sh(RECT, 0);
        m.sh(RECT + 2, 0);
        let buffer = m.lw(context.wrapping_add(4)) << 2;
        let s = m.lw(SWEEP);
        let packets = m.lw(t.wrapping_add(buffer).wrapping_add(0x454));
        m.sw(t.wrapping_add(0x45c), packets);
        let area = m.lw(s.wrapping_add(buffer));
        m.sw(s.wrapping_add(8), area);
        m.sw(sp.wrapping_add(0x10), 0x0080_8080);
        let ot = m.lw(context.wrapping_add(8));
        let packets = m.lw(t.wrapping_add(0x45c));
        let next = draw(m, ot, packets, 4, 0x28);
        let t = terminal(m);
        let phase = m.lw(t);
        m.sw(t.wrapping_add(0x45c), next);
        let set_phase = |m: &mut Machine, phase: u32| {
            let t = terminal(m);
            m.sw(t, phase);
        };
        match phase {
            0 => {
                m.sw(FADE, 0);
                m.sw(SWEEP_FRAME, 0);
                set_phase(m, 1);
            }
            1 => {
                let context = ctx(m);
                let s = m.lw(SWEEP);
                let ot = m.lw(context.wrapping_add(8));
                let area = m.lw(s.wrapping_add(8));
                let next = sweep(m, ot, area);
                let s = m.lw(SWEEP);
                let count = m.lw(SWEEP_FRAME).wrapping_add(1);
                m.sw(FADE, 0);
                m.sw(s.wrapping_add(8), next);
                m.sw(SWEEP_FRAME, count);
            }
            2 => {
                let level = m.lw(FADE).wrapping_sub(0x88);
                m.sw(FADE, level);
                if slt(level, 0xffff_f000) {
                    set_phase(m, 3);
                }
            }
            3 => {
                for offset in [0x10, 0x11, 0x12] {
                    let display = m.lw(DISPLAY);
                    m.sb(display.wrapping_add(offset), 0);
                }
                let display = m.lw(DISPLAY);
                let flags = m.lw(display) | 8;
                m.sw(FADE, 0xffff_f000);
                m.sw(display, flags);
                set_phase(m, 4);
            }
            4 => {
                m.sw(FADE, 0xffff_f000);
                set_phase(m, 5);
            }
            5 => {
                script(m, SCRIPT, STRINGS);
                m.sw(FADE, 0xffff_f000);
                let t = terminal(m);
                m.sw(t, 6);
                m.sb(t.wrapping_add(0x452), 1);
            }
            6 => {
                let t = terminal(m);
                m.sb(t.wrapping_add(0x452), 1);
                let level = m.lw(FADE).wrapping_add(0x2a);
                m.sw(FADE, level);
                if !slt(level, 0) {
                    m.sw(FADE, 0);
                    set_phase(m, 7);
                }
            }
            7 => {
                if script(m, SCRIPT, STRINGS) != 0 {
                    set_phase(m, 8);
                }
                m.sw(FADE, 0);
            }
            8 => {
                let t = terminal(m);
                m.sb(t.wrapping_add(0x452), 1);
                let level = m.lw(FADE).wrapping_sub(0x2a);
                m.sw(FADE, level);
                if slt(level, 0xffff_f001) {
                    m.sw(FADE, 0xffff_f000);
                    set_phase(m, 9);
                }
            }
            9 => {
                m.sw(FADE, 0xffff_f000);
                set_phase(m, 10);
            }
            10 => {
                call(m, RESET_CD_QUEUE, [0, 0, 0, 0]);
                let display = m.lw(DISPLAY);
                m.sb(display.wrapping_add(0x10), 0);
                let display2 = m.lw(DISPLAY);
                m.sw(display.wrapping_add(4), 0);
                m.sw(display.wrapping_add(0x14), 0);
                m.sb(display2.wrapping_add(0x11), 0);
                let display = m.lw(DISPLAY);
                m.sb(display.wrapping_add(0x12), 0);
                let display = m.lw(DISPLAY);
                let flags = m.lw(display) | 9;
                m.sw(FADE, 0xffff_f000);
                m.sw(display, flags);
                set_phase(m, 11);
            }
            11 => {
                let context = ctx(m);
                return m.lw(context.wrapping_add(0x3a0)) | RESET_AND_SET_STATE;
            }
            _ => {}
        }
        let level = m.lw(FADE);
        let context = ctx(m);
        let t = terminal(m);
        let ot = m.lw(context.wrapping_add(0xc));
        let packets = m.lw(t.wrapping_add(0x45c));
        let next = call(m, FADE_SCREEN, [ot, packets, RECT, level]);
        let t = terminal(m);
        m.sw(t.wrapping_add(0x45c), next);
        0
    })
}

/// 800CE9B4 (JP, new): MoveImage of the buffer's display rectangle
/// (0x800D1504 + 8 * buffer) to 0x2C0,0. Returns its result.
pub fn show(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let context = ctx(m);
        let buffer = m.lw(context.wrapping_add(4));
        call(
            m,
            MOVE_IMAGE,
            [(buffer << 3).wrapping_add(DISPLAY_RECTS), 0x2c0, 0, 0],
        )
    })
}

/// 800CE9F4 (JP, new; `ot`, `packets`): the sweep: in frames 0..10 a
/// rising line pair (LINE_F4 and LINE_F2, a DR_MODE), its height
/// `240 - rsin(frame * 1024 / 10) * 240 / 4096`, brightening; later the
/// five textured stripes narrow (2 per frame from 32) and at width 1 the
/// terminal goes to phase 2; then five 32-wide stripes (POLY_FT4, the
/// tpages 0x800D1530) and a clearing TILE behind a DR_MODE. Returns the
/// next packet.
pub fn sweep(m: &mut Machine, ot: u32, packets: u32) -> u32 {
    framed(m, 0x38, |m, sp| {
        let frame = m.lw(SWEEP_FRAME);
        let mut width = 0x20u32;
        let (height, color) = if slt(frame, 0xb) {
            let a = frame << 10;
            let angle = sra(hi_s(a, 0x6666_6667), 2).wrapping_sub(sra(a, 31));
            let sine = call(m, RSIN, [angle, 0, 0, 0]);
            let mut scaled = ((sine << 4).wrapping_sub(sine)) << 4;
            if slt(scaled, 0) {
                scaled = scaled.wrapping_add(0xfff);
            }
            let height = 0xf0u32.wrapping_sub(sra(scaled, 12));
            let f = m.lw(SWEEP_FRAME);
            let color = (f << 19)
                .wrapping_add((f << 11).wrapping_add(0x0080_8080))
                .wrapping_add(f << 3);
            (height, color)
        } else {
            let height = 1u32;
            width = 0x20u32.wrapping_sub(frame.wrapping_sub(0xa) << 1);
            if slt(width, 2) {
                width = 1;
                let t = terminal(m);
                m.sw(t, 2);
            }
            (height, 0x00d8_d8d8)
        };
        let mut p = packets;
        if !slt(height, 2) {
            let mode = p.wrapping_add(0x2c);
            let left = 0xa0u32.wrapping_sub(width.wrapping_mul(5));
            m.sw(p.wrapping_add(4), 0x4ea0_a0a0);
            let top = 0xf0u32.wrapping_sub(height) << 16;
            let xy0 = left | top;
            let right = width.wrapping_mul(5).wrapping_add(0x9f);
            m.sw(p.wrapping_add(0xc), right | top);
            let bottom = height.wrapping_add(0xef) << 16;
            let xy2 = right | bottom;
            let xy3 = left | bottom;
            m.sw(p.wrapping_add(8), xy0);
            m.sw(p.wrapping_add(0x10), xy2);
            m.sw(p.wrapping_add(0x14), xy3);
            m.sw(p.wrapping_add(0x18), 0x5555_5555);
            let tag = m.lw(ot) | 0x0600_0000;
            m.sw(p, tag);
            m.sw(ot, (p << 8) >> 8);
            let line = p.wrapping_add(0x1c);
            m.sw(line.wrapping_add(0xc), xy3);
            m.sw(line.wrapping_add(4), 0x42a0_a0a0);
            m.sw(line.wrapping_add(8), xy0);
            let tag = m.lw(ot) | 0x0300_0000;
            m.sw(p.wrapping_add(0x1c), tag);
            m.sw(ot, (line << 8) >> 8);
            m.sw(sp.wrapping_add(0x10), 0);
            call(m, SET_DRAW_MODE, [mode, 0, 1, 0x120]);
            add_prim(m, ot, mode);
            p = p.wrapping_add(0x38);
        }
        let code = color | 0x2c00_0000;
        let top = 0xf0u32.wrapping_sub(height) << 16;
        let bottom = height.wrapping_add(0xef) << 16;
        let mut pages = SWEEP_PAGES;
        for i in 0..5u32 {
            m.sw(p.wrapping_add(4), code);
            m.sw(p.wrapping_add(0xc), 0);
            let page = m.lw(pages);
            m.sw(p.wrapping_add(0x1c), 0xf000);
            m.sw(p.wrapping_add(0x24), 0xf040);
            m.sw(p.wrapping_add(0x14), page | 0x40);
            if slt(width, 2) {
                width = u32::from(i == 2);
            }
            let product = lo_s(i, width);
            pages = pages.wrapping_add(4);
            let offset = width.wrapping_mul(5).wrapping_sub(0xa0);
            let x0 = (product << 1).wrapping_sub(offset);
            let x1 = x0.wrapping_add(width).wrapping_add(width);
            m.sw(p.wrapping_add(8), x0 | top);
            m.sw(p.wrapping_add(0x10), x1 | top);
            m.sw(p.wrapping_add(0x18), x0 | bottom);
            m.sw(p.wrapping_add(0x20), x1 | bottom);
            let tag = m.lw(ot) | 0x0900_0000;
            m.sw(p, tag);
            m.sw(ot, (p << 8) >> 8);
            p = p.wrapping_add(0x28);
        }
        let mode = p.wrapping_add(0x10);
        m.sw(p.wrapping_add(4), 0x6200_0000);
        m.sw(p.wrapping_add(8), 0);
        m.sw(p.wrapping_add(0xc), 0x01e0_0140);
        let tag = m.lw(ot) | 0x0300_0000;
        m.sw(p, tag);
        m.sw(ot, (p << 8) >> 8);
        m.sw(sp.wrapping_add(0x10), 0);
        call(m, SET_DRAW_MODE, [mode, 0, 1, 0x100]);
        add_prim(m, ot, mode);
        p.wrapping_add(0x1c)
    })
}

/// The routines of this file by JP address, while module 4 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    let [a0, a1, ..] = args;
    Some(match target {
        0x800c_e9b4 => show(m),
        0x800c_e9f4 => sweep(m, a0, a1),
        0x800c_ed74 => start(m),
        0x800c_eefc => frame(m),
        _ => return None,
    })
}
