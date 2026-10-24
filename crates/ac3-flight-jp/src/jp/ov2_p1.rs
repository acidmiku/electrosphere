//! JP overlay 2, part 1 (800CD7C0..800D7DB0): the story hub's frame
//! framework, page loaders, the Search Engine and Archive screens, the hub
//! options and the debriefing-side screens that follow them. Translated
//! from the JP instructions (module 2 loads at 0x800CBFE8). Every routine
//! follows its original instructions and store order; pointer arguments
//! are guest addresses as in the original ABI. Routines with an original
//! stack frame that call other routines push that frame
//! (`Machine::push_frame`) so nested frames keep their original addresses.
//!
//! This file holds the shared names of the part and the hub framework:
//! - the state-3 substate 7 tick 800CDCD4 and its five phases at
//!   [`PHASES`] (800CD7C0, 800CD7F0 initialization, 800CDAF4 fade,
//!   800CDBF0, 800CDC68);
//! - the substate 8 per-frame tick 800CDF40, which links the ordering
//!   tables, loads the light matrix and runs the hub screen handler of
//!   [`SCREENS`] (`[W+0x7C][W+0x7D]`);
//! - the unreached debug meter 800CDD84 and resource load 800CE4D0.
//!
//! The other files of the part (`ov2_p1_*.rs`) hold the rest; each has its
//! own `dispatch`. Routines of other parts and resident code are called by
//! JP address through [`crate::dispatch::call`]; argument registers are
//! passed as the original holds them, and as zeros where the callee reads
//! no argument register (each such site is checked against the callee's
//! instructions).
//!
//! The work block `W` (`[0x80112DC4]`), as far as this part uses it: +8
//! and +0x14 the two ordering tables of the frame (double-buffered at +0
//! and +0xC by `[context+4]`), +0x24 a resource handle, +0x30/+0x32/+0x34
//! cursor halfwords, +0x40 (from +0x38), +0x4C frame counter, +0x50 fade
//! timer, +0x7C/+0x7D screen group and screen, +0x7E/+0x7F, +0x84 the hub
//! page, +0x8A the substate-7 phase, +0x94 the exit request, +0x11C, +0x130
//! .. +0x140 five sound handles, +0x144/+0x148, +0x1B0..+0x1B2, +0x1B4..
//! +0x1B8 the debug meter, +0x270/+0x274 and +0x278 (two words, by
//! `[context+4]`).
use super::ov2::MODULE;
use crate::common::{CONTEXT, GAMEPLAY, RESOURCES, loaded_module};
use crate::dispatch;
use crate::machine::{Machine, SmallData};
use ac3_gte::ctrl;

/// `[0x80112DC4]`: the hub work block `W`.
pub const WORK: u32 = 0x8011_2dc4;
/// `[0x80112DC0]`: the hub flag block; `+0x5C` points at its flag bytes
/// (set by 800DEF94).
pub const FLAGS: u32 = 0x8011_2dc0;
/// `[0x80112DA8]`: the second ordering-table block (+8 its table, +0x14
/// a mode word, +0x20, +0x6E).
pub const SCENE: u32 = 0x8011_2da8;
/// gp+0x2D4: the display control block (+0 request bits, +0x10..+0x12 the
/// fade colour).
pub const DISPLAY: u32 = 0x2d4;
/// 8010982C: the five phases of substate 7 (800CDCD4), by `W+0x8A`.
pub const PHASES: u32 = 0x8010_982c;
/// 80109814: the hub screen groups: six tables of screen handlers, run by
/// 800CDF40 at `[SCREENS + 4 * W+0x7C] + 4 * W+0x7D`.
pub const SCREENS: u32 = 0x8010_9814;

// Resident routines (JP addresses; the generated crate claims them).
/// 80027410: arena allocation (`bytes`).
pub const ALLOC: u32 = 0x8002_7410;
/// 80027538: arena reset to `mark`.
pub const ARENA_RESET: u32 = 0x8002_7538;
/// 80027338: relocate a loaded directory (`block`).
pub const RELOCATE: u32 = 0x8002_7338;
/// 8002C63C: total size of a file list (`list`, `count`).
pub const TOTAL_SIZE: u32 = 0x8002_c63c;
/// 8002C82C: queue a file list load (`list`, `count`, `buffer`, `callback`).
pub const QUEUE_FILES: u32 = 0x8002_c82c;
/// 8002C6A8 (PAL 8002C280).
pub const RESIDENT_C6A8: u32 = 0x8002_c6a8;
/// 8002DAF4 (PAL 8002D6CC).
pub const RESIDENT_DAF4: u32 = 0x8002_daf4;
/// 8002DB4C: start sound `id`; returns its handle.
pub const PLAY_SOUND: u32 = 0x8002_db4c;
/// 8002DDA8 (PAL 8002D980): stop sound `handle`.
pub const STOP_SOUND: u32 = 0x8002_dda8;
/// 8002DDE0 (PAL 8002D9B8).
pub const RESIDENT_DDE0: u32 = 0x8002_dde0;
/// 8002DF40: sound channel volume (`handle`, `volume`, `time`).
pub const SOUND_VOLUME: u32 = 0x8002_df40;
/// 8009B904: the file-load completion callback that 8002C82C is given.
pub const LOAD_DONE: u32 = 0x8009_b904;
/// 800A2098: VSync(`mode`).
pub const VSYNC: u32 = 0x800a_2098;
/// 800A7118: SetGeomOffset(`x`, `y`).
pub const SET_GEOM_OFFSET: u32 = 0x800a_7118;
/// 800A7138: SetGeomScreen(`h`).
pub const SET_GEOM_SCREEN: u32 = 0x800a_7138;
/// 800A8680: ClearOTagR(`table`, `count`).
pub const CLEAR_OTAG_R: u32 = 0x800a_8680;

// Overlay-2 routines of other parts (JP addresses).
/// 800DEF94: hub flag byte `index` = `value` (`[FLAGS]+0x5C`).
pub const SET_FLAG: u32 = 0x800d_ef94;
/// 800FEDEC: mark the unlocks of `id` (context +0xD48).
pub const MARK_UNLOCKS: u32 = 0x800f_edec;
/// 800FF2BC: start sound 0x62 and set the scene mode word to 1.
pub const SCENE_START: u32 = 0x800f_f2bc;
/// 800FF100: link the scene ordering table into `slot`.
pub const SCENE_LINK: u32 = 0x800f_f100;
/// 800F3DB4: link the text layer's ordering tables into `slot`.
pub const TEXT_LINK: u32 = 0x800f_3db4;
/// 800F35B8, 800F33F8, 800F34C8, 800F34B0, 800F33A8, 800F33D0: text-layer
/// setup (font, page, attributes, origin, colour).
pub const TEXT_FONT: u32 = 0x800f_35b8;
pub const TEXT_PAGE: u32 = 0x800f_33f8;
pub const TEXT_ATTRIBUTES: u32 = 0x800f_34c8;
pub const TEXT_MODE: u32 = 0x800f_34b0;
pub const TEXT_ORIGIN: u32 = 0x800f_33a8;
pub const TEXT_COLOR: u32 = 0x800f_33d0;
/// 800F4AE0: formatted text (`format`, arguments in `a1..a3`).
pub const TEXT_PRINT: u32 = 0x800f_4ae0;
/// 80101FEC: draw the scene (`next primitive`); returns the next free one.
pub const SCENE_DRAW: u32 = 0x8010_1fec;
/// 800E2494: draw the hub panels (`next primitive`); returns the next one.
pub const PANELS_DRAW: u32 = 0x800e_2494;

/// Base plus offset with the original's 32-bit wrapping.
#[inline]
pub(crate) fn at(base: u32, offset: u32) -> u32 {
    base.wrapping_add(offset)
}

/// Element `index` of a word table at `base + offset` (`sll 2; addu`).
#[inline]
pub(crate) fn slot(base: u32, index: u32, offset: u32) -> u32 {
    base.wrapping_add(index << 2).wrapping_add(offset)
}

/// An original call by JP address.
#[inline]
pub(crate) fn call(m: &mut Machine, target: u32, args: [u32; 4]) -> u32 {
    dispatch::call(m, target, args)
}

/// The work block pointer `W`.
#[inline]
pub(crate) fn work(m: &mut Machine) -> u32 {
    m.lw(WORK)
}

/// The game context (`[gp+0x2EC]`).
#[inline]
pub(crate) fn context(m: &mut Machine) -> u32 {
    m.gw(CONTEXT)
}

/// Halfword load, sign-extended to a register.
#[inline]
pub(crate) fn lh(m: &mut Machine, address: u32) -> u32 {
    m.lh(address) as i32 as u32
}

/// Signed `a < b` (`slt`).
#[inline]
pub(crate) fn lt(a: u32, b: u32) -> bool {
    (a as i32) < (b as i32)
}

/// Signed division by 32 rounding toward zero, as compiled
/// (`bgez; addiu 0x1F; sra 5`).
#[inline]
pub(crate) fn div32(value: u32) -> u32 {
    let v = if lt(value, 0) {
        value.wrapping_add(0x1f)
    } else {
        value
    };
    ((v as i32) >> 5) as u32
}

/// Signed division by 240 as compiled: `mult` by 0x88888889, `mfhi`, add
/// the dividend, `sra 7`, minus the dividend's sign.
#[inline]
pub(crate) fn div240(value: u32) -> u32 {
    let hi = ((i64::from(value as i32) * i64::from(0x8888_8889u32 as i32)) >> 32) as u32;
    let q = (hi.wrapping_add(value) as i32) >> 7;
    q.wrapping_sub((value as i32) >> 31) as u32
}

/// `table & 0xFF000000 | link & 0xFFFFFF` (an ordering-table tag).
#[inline]
pub(crate) fn tag(table: u32, link: u32) -> u32 {
    (table & 0xff00_0000) | (link & 0x00ff_ffff)
}

/// Start sound `id` (8002DB4C); returns its handle.
pub(crate) fn sound(m: &mut Machine, id: u32) -> u32 {
    call(m, PLAY_SOUND, [id, 0, 0, 0])
}

/// 800CD7C0 (phase 0): `W+0x274` = `W+0x278[context+4]`. Returns 1.
pub fn phase_cd7c0(m: &mut Machine) -> u32 {
    let context = context(m);
    let index = m.lw(at(context, 4));
    let w = work(m);
    let value = m.lw(slot(w, index, 0x278));
    m.sw(at(w, 0x274), value);
    1
}

/// 800CD7F0 (phase 1): initialise the hub: `W+0x274` from `W+0x278`, a
/// 0x100-byte buffer at `gameplay+0x250` holding 64 buffers of 0x1C bytes,
/// context +0x964 bit 0, the unlocks of `context+0x38C` (800FEDEC), the
/// geometry offset (160, 240) and screen (250), the five background sounds
/// 0x5D..0x61 (handles at `W+0x130..0x140`, each faded in), the screen
/// group and screen (`W+0x7C/0x7D`: 4/2 with context +0x95F = 1, 1/0 with
/// option bit 8, else 0 for pages 0 and 3 of `context+0x954`, 1 otherwise)
/// and the text layer. Returns 1 for pages 0 and 3 without option bit 8,
/// else 2.
pub fn init_cd7f0(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let context = context(m);
    let w = work(m);
    let index = m.lw(at(context, 4));
    let value = m.lw(slot(w, index, 0x278));
    m.sw(at(w, 0x4c), 0);
    m.sw(at(w, 0x270), 0);
    m.sw(at(w, 0x274), value);
    let table = call(m, ALLOC, [0x100, 0, 0, 0]);
    let gameplay = m.gw(GAMEPLAY);
    m.sw(at(gameplay, 0x250), table);
    for i in 0..0x40u32 {
        let buffer = call(m, ALLOC, [0x1c, 0, 0, 0]);
        let gameplay = m.gw(GAMEPLAY);
        let table = m.lw(at(gameplay, 0x250));
        m.sw(slot(table, i, 0), buffer);
    }
    let context = self::context(m);
    let flags = m.lw(at(context, 0x964));
    let id = lh(m, at(context, 0x38c));
    m.sw(at(context, 0x964), flags | 1);
    call(m, MARK_UNLOCKS, [id, 0, 0, 0]);
    let w = work(m);
    let scene = m.lw(SCENE);
    m.sw(at(w, 0x94), 0);
    m.sh(at(scene, 0x6e), 0);
    call(m, SET_GEOM_OFFSET, [0xa0, 0xf0, 0, 0]);
    call(m, SET_GEOM_SCREEN, [0xfa, 0, 0, 0]);
    let w = work(m);
    m.sh(at(w, 0x30), 0);
    m.sh(at(w, 0x32), 0);
    m.sw(at(w, 0x144), 0);
    m.sw(at(w, 0x148), 1);
    let mut handle = sound(m, 0x5d);
    for (id, offset) in [(0x5e, 0x130), (0x5f, 0x134), (0x60, 0x138), (0x61, 0x13c)] {
        let w = work(m);
        m.sw(at(w, offset), handle);
        handle = sound(m, id);
    }
    let w = work(m);
    let first = m.lw(at(w, 0x130));
    m.sw(at(w, 0x140), handle);
    call(m, SOUND_VOLUME, [first, 0, 1, 0]);
    for offset in [0x134, 0x138, 0x13c, 0x140] {
        let w = work(m);
        let handle = m.lw(at(w, offset));
        call(m, SOUND_VOLUME, [handle, 0, 1, 0]);
    }
    let w = work(m);
    m.sb(at(w, 0x7d), 0);
    let context = self::context(m);
    if m.lbu(at(context, 0x95f)) == 1 {
        let w = work(m);
        m.sb(at(w, 0x7c), 4);
        let w = work(m);
        m.sb(at(w, 0x7d), 2);
    } else if m.lw(at(context, 0x96c)) & 8 != 0 {
        let w = work(m);
        m.sb(at(w, 0x7c), 1);
        let w = work(m);
        m.sb(at(w, 0x7d), 0);
    } else {
        let page = lh(m, at(context, 0x954));
        let w = work(m);
        m.sb(at(w, 0x7c), u8::from(page != 0 && page != 3));
    }
    let w = work(m);
    m.sb(at(w, 0x7e), 4);
    let w = work(m);
    m.sb(at(w, 0x7f), 0);
    let w = work(m);
    m.sb(at(w, 0x1b0), 0);
    let again = work(m);
    m.sw(at(w, 0x11c), 0);
    m.sh(at(w, 0x34), 2);
    m.sb(at(again, 0x1b1), 0);
    let w = work(m);
    m.sb(at(w, 0x1b2), 0);
    let context = self::context(m);
    m.sw(at(context, 0x103c), 0xc);
    call(m, TEXT_FONT, [1, 0, 0, 0]);
    call(m, TEXT_PAGE, [u32::MAX, 0, 0, 0]);
    call(m, TEXT_ATTRIBUTES, [1, 1, 1, 0]);
    call(m, TEXT_MODE, [1, 0, 0, 0]);
    let w = work(m);
    let context = self::context(m);
    m.sw(at(w, 0x50), 0);
    let result = if m.lw(at(context, 0x96c)) & 8 != 0 {
        2
    } else {
        let page = lh(m, at(context, 0x954));
        if page == 0 || page == 3 { 1 } else { 2 }
    };
    m.pop_frame(0x20);
    result
}

/// 800CDAF4 (phase 2): fade in over 33 frames: the display fade colour
/// (+0x10..+0x12) is `W+0x50` times 143, 135 and 83, over 32; display
/// request bit 8. Returns 1 (and clears the timer) after the 33rd frame,
/// else 0.
pub fn fade_cdaf4(m: &mut Machine) -> u32 {
    for (factor, offset) in [(143u32, 0x10), (135, 0x11), (83, 0x12)] {
        let w = work(m);
        let timer = m.lw(at(w, 0x50));
        let display = m.gw(DISPLAY);
        m.sb(at(display, offset), div32(timer.wrapping_mul(factor)) as u8);
    }
    let display = m.gw(DISPLAY);
    let request = m.lw(display);
    let w = work(m);
    m.sw(display, request | 8);
    let timer = m.lw(at(w, 0x50)).wrapping_add(1);
    m.sw(at(w, 0x50), timer);
    if lt(timer, 0x21) {
        return 0;
    }
    m.sw(at(w, 0x50), 0);
    1
}

/// 800CDBF0 (phase 3): start the scene (800FF2BC) for pages 0 and 3 of
/// `context+0x954`, and again for page 2 or with option bit 0x20. Returns 1.
pub fn phase_cdbf0(m: &mut Machine, args: [u32; 4]) -> u32 {
    m.push_frame(0x18);
    let context = context(m);
    let page = lh(m, at(context, 0x954));
    if page == 0 || page == 3 {
        // 800FF2BC reads no argument register
        call(m, SCENE_START, args);
    }
    let context = self::context(m);
    let page = lh(m, at(context, 0x954));
    if page == 2 || m.lw(at(context, 0x96c)) & 0x20 != 0 {
        call(m, SCENE_START, [context, 0, 0, 0]);
    }
    m.pop_frame(0x18);
    1
}

/// 800CDC68 (phase 4): for pages 0, 2 and 3 of `context+0x954` or with
/// option bit 0x20, 2 once the scene mode word (`[SCENE]+0x14`) is 1;
/// otherwise 0.
pub fn phase_cdc68(m: &mut Machine) -> u32 {
    let context = context(m);
    let page = lh(m, at(context, 0x954));
    let waits = page == 0 || page == 3 || page == 2 || m.lw(at(context, 0x96c)) & 0x20 != 0;
    if waits {
        let scene = m.lw(SCENE);
        if m.lw(at(scene, 0x14)) == 1 {
            return 2;
        }
    }
    0
}

/// 800CDCD4 (state 3, substate 7; JP, changed from PAL 800DC868): run the
/// phase `W+0x8A` of [`PHASES`] (the argument registers pass through).
/// Result 1 advances the phase and returns 0; 2 and above clear
/// `W+0x1C6` (only for 2) and return 0x4000000 (next substate); anything
/// else returns 0.
pub fn substate_cdcd4(m: &mut Machine, args: [u32; 4]) -> u32 {
    m.push_frame(0x18);
    let w = work(m);
    let phase = lh(m, at(w, 0x8a));
    let handler = m.lw(slot(PHASES, phase, 0));
    let result = call(m, handler, args);
    let value = if result == 1 {
        let w = work(m);
        let phase = m.lhu(at(w, 0x8a)).wrapping_add(1);
        m.sh(at(w, 0x8a), phase);
        0
    } else if lt(result, 2) {
        0
    } else {
        if result == 2 {
            let w = work(m);
            m.sh(at(w, 0x1c6), 0);
        }
        0x0400_0000
    };
    m.pop_frame(0x18);
    value
}

/// 800CDD84 (unreached debug meter): `W+0x1B6` = `W+0x1B4`; `W+0x1B4` =
/// VSync(1); the text layer at (10, 10) in grey prints the CPU and GPU
/// loads (`context+0x48/+0x4C`, in percent of 240 lines), the arena use
/// (`context+0x18/+0x1C`), the primitive load (`W+0x1B8`) and the font
/// cost (`W+0x1B6`); then `W+0x1B4` = VSync(1) minus it, which it returns.
pub fn meter_cdd84(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let w = work(m);
    let previous = m.lhu(at(w, 0x1b4));
    m.sh(at(w, 0x1b6), previous);
    let now = call(m, VSYNC, [1, 0, 0, 0]);
    let w = work(m);
    m.sh(at(w, 0x1b4), now as u16);
    call(m, TEXT_FONT, [1, 0, 0, 0]);
    call(m, TEXT_PAGE, [2, 0, 0, 0]);
    call(m, TEXT_ATTRIBUTES, [1, 1, 1, 0]);
    call(m, TEXT_MODE, [1, 0, 0, 0]);
    call(m, TEXT_ORIGIN, [0xa, 0xa, 0, 0]);
    call(m, TEXT_COLOR, [0x80, 0x80, 0x80, 0]);
    // 800F4AE0 reads only the arguments its format names
    let context = context(m);
    let lines = m.lw(at(context, 0x48));
    call(
        m,
        TEXT_PRINT,
        [0x800c_bfec, div240(lines.wrapping_mul(100)), lines, 0],
    );
    let context = self::context(m);
    let lines = m.lw(at(context, 0x4c));
    call(
        m,
        TEXT_PRINT,
        [0x800c_bffc, div240(lines.wrapping_mul(100)), lines, 0],
    );
    let context = self::context(m);
    let low = m.lw(at(context, 0x18));
    let high = m.lw(at(context, 0x1c));
    call(
        m,
        TEXT_PRINT,
        [0x800c_c00c, low, high, high.wrapping_sub(low)],
    );
    let w = work(m);
    let load = m.lw(at(w, 0x1b8));
    call(
        m,
        TEXT_PRINT,
        [0x800c_c028, div240(load.wrapping_mul(100)), load, 0],
    );
    let w = work(m);
    let cost = lh(m, at(w, 0x1b6));
    call(m, TEXT_PRINT, [0x800c_c038, cost, 0, 0]);
    let now = call(m, VSYNC, [1, 0, 0, 0]);
    let w = work(m);
    let start = u32::from(m.lhu(at(w, 0x1b4)));
    let elapsed = now.wrapping_sub(start);
    m.sh(at(w, 0x1b4), elapsed as u16);
    m.pop_frame(0x20);
    elapsed
}

/// 800CDF40 (state 3, substate 8): the hub frame. Selects the frame's
/// buffers by `context+4` (`W+0x40`, `W+0x274`, the ordering tables `W+8`
/// (16 entries) and `W+0x14` (256 entries), gameplay +0x10/+0x14), links
/// them with the context's table and the text and scene layers, loads the
/// background, far colour, depth cue and the light and light-colour
/// matrices, sets hub flag 0x14 from context option bit 0x10 and runs the
/// screen handler `[W+0x7C][W+0x7D]` of [`SCREENS`] (with `a0` = `W`).
/// Then it sets flag 0x2C (the panel lamp, unless page `W+0x84` is 6),
/// draws the scene and the panels into `W+0x274`, counts `W+0x4C`, sets
/// flag byte +0x28 from option bit 0x10 and handles the exit requests:
/// `W+0x94` stops the five sounds and returns it; `W+0x7F` = 1 returns
/// 0x60000004 and `W+0x11C` = 1 returns 0x60000005 (with
/// `context+0x3A0` = 3); otherwise 0.
pub fn tick_cdf40(m: &mut Machine) -> u32 {
    m.push_frame(0x28);
    let context = context(m);
    let index = m.lw(at(context, 4));
    let w = work(m);
    let value = m.lw(slot(w, index, 0x38));
    m.sw(at(w, 0x40), value);
    let index = m.lw(at(context, 4));
    let value = m.lw(slot(w, index, 0x278));
    m.sw(at(w, 0x274), value);
    let index = m.lw(at(context, 4));
    let table = m.lw(slot(w, index, 0));
    m.sw(at(w, 8), table);
    call(m, CLEAR_OTAG_R, [table, 0x10, 0, 0]);
    let w = work(m);
    let context = self::context(m);
    let table = m.lw(at(w, 8));
    let shared = m.lw(at(context, 8));
    let head = m.lw(table);
    let link = m.lw(shared);
    m.sw(table, tag(head, link));
    let table = m.lw(at(w, 8));
    let shared = m.lw(at(context, 8));
    let head = m.lw(shared);
    m.sw(shared, tag(head, table.wrapping_add(0x3c)));
    let table = m.lw(at(w, 8));
    // 800F3DB4 and 800FF100 read only a0
    call(m, TEXT_LINK, [table, 0, 0, 0]);
    let w = work(m);
    let table = m.lw(at(w, 8));
    call(m, SCENE_LINK, [table.wrapping_add(0x3c), 0, 0, 0]);
    let context = self::context(m);
    let index = m.lw(at(context, 4));
    let w = work(m);
    let table = m.lw(slot(w, index, 0xc));
    m.sw(at(w, 0x14), table);
    call(m, CLEAR_OTAG_R, [table, 0x100, 0, 0]);
    let w = work(m);
    let second = m.lw(at(w, 0x14));
    let first = m.lw(at(w, 8));
    let head = m.lw(second);
    let link = m.lw(at(first, 4));
    m.sw(second, tag(head, link));
    let first = m.lw(at(w, 8));
    let second = m.lw(at(w, 0x14));
    let context = self::context(m);
    let head = m.lw(at(first, 4));
    m.sw(at(first, 4), tag(head, second.wrapping_add(0x3fc)));
    let index = m.lw(at(context, 4));
    let gameplay = m.gw(GAMEPLAY);
    let value = m.lw(slot(gameplay, index, 0x18));
    m.sw(at(gameplay, 0x10), value);
    let index = m.lw(at(context, 4));
    let value = m.lw(slot(gameplay, index, 0x20));
    m.sw(at(gameplay, 0x14), value);
    for r in [ctrl::RBK, ctrl::RBK + 1, ctrl::RBK + 2] {
        m.gte.ctc2(r, 0xc00);
    }
    for r in [ctrl::RFC, ctrl::GFC, ctrl::BFC] {
        m.gte.ctc2(r, 0x600);
    }
    m.gte.ctc2(ctrl::DQA, 0xffff_8800);
    m.gte.ctc2(ctrl::DQB, 0x0300_0000);
    // the light matrix (L11L12 .. L33) and the light colour matrix
    // (LR1LR2 .. LB3)
    for (r, value) in [
        (ctrl::L11L12, 0x0800_0000),
        (ctrl::L11L12 + 1, 0x0400_0000),
        (ctrl::L11L12 + 2, 0x400),
        (ctrl::L11L12 + 3, 0x0400_fc00),
        (ctrl::L33, 0),
        (ctrl::LR1LR2, 0x0200_0400),
        (ctrl::LR1LR2 + 1, 0x0400_0200),
        (ctrl::LR1LR2 + 2, 0x0200_0200),
        (ctrl::LR1LR2 + 3, 0x0200_0400),
        (ctrl::LB3, 0x200),
    ] {
        m.gte.ctc2(r, value);
    }
    let lit = u32::from(m.lw(at(context, 0x124)) & 0x10 != 0);
    call(m, SET_FLAG, [0x14, lit, 0, 0]);
    let flags = m.lw(FLAGS);
    let context = self::context(m);
    let bytes = m.lw(at(flags, 0x5c));
    let mode = m.lbu(at(context, 0x394));
    m.sb(at(bytes, 0x20), mode);
    let w = work(m);
    let group = u32::from(m.lbu(at(w, 0x7c)));
    let screen = u32::from(m.lbu(at(w, 0x7d)));
    let table = m.lw(slot(SCREENS, group, 0));
    let handler = m.lw(slot(table, screen, 0));
    // a1 keeps the flag value; the handlers read no argument but a0
    call(m, handler, [w, lit, 0, 0x200]);
    let context = self::context(m);
    let lamp = if m.lw(at(context, 0x96c)) & 0x10 != 0 {
        let w = work(m);
        if lh(m, at(w, 0x84)) == 6 {
            None
        } else if m.lbu(at(w, 0x7c)) != 0 {
            Some(5)
        } else {
            Some(u32::from(m.lbu(at(w, 0x7e))).wrapping_add(9))
        }
    } else {
        let w = work(m);
        let group = m.lbu(at(w, 0x7c));
        if group == 1 || lh(m, at(w, 0x84)) == 6 {
            None
        } else if group != 0 {
            Some(u32::from(m.lbu(at(w, 0x7e))).wrapping_add(5))
        } else {
            Some(u32::from(m.lbu(at(w, 0x7e))).wrapping_add(1))
        }
    };
    if let Some(value) = lamp {
        call(m, SET_FLAG, [0x2c, value, 0, 0]);
    }
    let w = work(m);
    let next = m.lw(at(w, 0x274));
    let next = call(m, SCENE_DRAW, [next, 0, 0, 0]);
    let w = work(m);
    m.sw(at(w, 0x274), next);
    let next = call(m, PANELS_DRAW, [next, 0, 0, 0]);
    let w = work(m);
    m.sw(at(w, 0x274), next);
    let frames = m.lw(at(w, 0x4c)).wrapping_add(1);
    let context = self::context(m);
    m.sw(at(w, 0x4c), frames);
    let lit = m.lw(at(context, 0x124)) & 0x10 != 0;
    let flags = m.lw(FLAGS);
    let bytes = m.lw(at(flags, 0x5c));
    m.sb(at(bytes, 0x28), u8::from(lit));
    let w = work(m);
    let result = if m.lw(at(w, 0x94)) != 0 {
        for offset in [0x130, 0x134, 0x138, 0x13c, 0x140] {
            let w = work(m);
            let handle = m.lw(at(w, offset));
            call(m, STOP_SOUND, [handle, 0, 0, 0]);
        }
        call(m, RESIDENT_DAF4, [0; 4]);
        call(m, RESIDENT_DDE0, [0; 4]);
        call(m, RESIDENT_C6A8, [0; 4]);
        let display = m.gw(DISPLAY);
        let request = m.lw(display);
        let w = work(m);
        m.sw(display, request | 4);
        if m.lbu(at(w, 0x94)) == 4 {
            let context = self::context(m);
            let first = m.lhu(at(context, 0x38c));
            let second = m.lhu(at(context, 0x38e));
            m.sh(at(context, 0x390), first);
            m.sh(at(context, 0x392), second);
        }
        let w = work(m);
        m.lw(at(w, 0x94))
    } else if m.lbu(at(w, 0x7f)) == 1 {
        call(m, RESIDENT_DDE0, [0; 4]);
        call(m, RESIDENT_C6A8, [0; 4]);
        let display = m.gw(DISPLAY);
        let context = self::context(m);
        let request = m.lw(display);
        m.sw(display, request | 4);
        let first = m.lhu(at(context, 0x38c));
        let second = m.lhu(at(context, 0x38e));
        m.sh(at(context, 0x390), first);
        m.sh(at(context, 0x392), second);
        0x6000_0004
    } else if m.lw(at(w, 0x11c)) == 1 {
        call(m, RESIDENT_DDE0, [0; 4]);
        call(m, RESIDENT_C6A8, [0; 4]);
        let context = self::context(m);
        let display = m.gw(DISPLAY);
        m.sw(at(context, 0x3a0), 3);
        let request = m.lw(display);
        m.sw(display, request | 4);
        0x6000_0005
    } else {
        0
    };
    m.pop_frame(0x28);
    result
}

/// 800CE4D0 (unreached): reset the arena to `[W+0x24]`, load resource
/// list 0x73 (resources +0x60, one file) into a new buffer of its total
/// size, with `context+0x44` = 1 and the completion callback 8009B904.
/// Returns 8002C82C's result.
pub fn load_ce4d0(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let w = work(m);
    let handle = m.lw(at(w, 0x24));
    let mark = m.lw(handle);
    call(m, ARENA_RESET, [mark, 0, 0, 0]);
    let result = load_list(m, 0x73);
    m.pop_frame(0x18);
    result
}

/// The load the page loaders share: resources +0x60 = `id`, a buffer of
/// the list's total size (8002C63C) from the arena, `context+0x44` = 1,
/// then 8002C82C with callback 8009B904. Returns 8002C82C's result.
pub(crate) fn load_list(m: &mut Machine, id: u32) -> u32 {
    let resources = m.gw(RESOURCES);
    m.sw(at(resources, 0x60), id);
    let bytes = call(m, TOTAL_SIZE, [at(resources, 0x60), 1, 0, 0]);
    let buffer = call(m, ALLOC, [bytes, 0, 0, 0]);
    let context = context(m);
    let resources = m.gw(RESOURCES);
    m.sw(at(context, 0x44), 1);
    call(m, QUEUE_FILES, [at(resources, 0x60), 1, buffer, LOAD_DONE])
}

/// The routines of this file by JP address, while module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    Some(match target {
        0x800c_d7c0 => phase_cd7c0(m),
        0x800c_d7f0 => init_cd7f0(m),
        0x800c_daf4 => fade_cdaf4(m),
        0x800c_dbf0 => phase_cdbf0(m, args),
        0x800c_dc68 => phase_cdc68(m),
        0x800c_dcd4 => substate_cdcd4(m, args),
        0x800c_dd84 => meter_cdd84(m),
        0x800c_df40 => tick_cdf40(m),
        0x800c_e4d0 => load_ce4d0(m),
        _ => return None,
    })
}
