//! JP overlay 2, part 2: hub state callbacks, translated from the JP
//! instructions.
//!
//! The hub state record (`[0x80112DDC]`): +0 page, +1 step (the handler of
//! [`update`] is `[[0x8010AD00 + 4 * page] + 4 * step]`), +2 leave flag,
//! +8 saved packet pointer, +0xC selection, +0xB8..+0xE4 table pointers.
//! The visual block's +0x1C6 selects the second handler of [`update`]
//! (table 0x8010AC6C) and the load phase of [`run_menu`] (table 0x8010984C,
//! part 1's `LOADS`).
//!
//! Calls to routines of other parts pass the argument registers the
//! original holds at the call (the caller's arguments where it sets none),
//! as the test oracle does; the callees read only the ones they are given.
use super::ov2_p1_pages::run_load_phase;
use super::ov2_p2::{MODULE, framed, lbu, lh, visual};
use crate::common::{CONTEXT, RESOURCES, loaded_module};
use crate::dispatch;
use crate::machine::{Machine, SmallData};

/// Pointer to the hub state record.
pub const STATE: u32 = 0x8011_2ddc;
/// Page handler tables of [`update`] (one table per page).
pub const PAGES: u32 = 0x8010_ad00;
/// Second handlers of [`update`] by visual block +0x1C6.
pub const UPDATE_MODES: u32 = 0x8010_ac6c;

/// 80100994 (overlay 2, another part; PAL 800E983C `ov2_core`).
const MAP_HIDE: u32 = 0x8010_0994;
/// 800ECF88 (overlay 2, another part).
const SELECT: u32 = 0x800e_cf88;
/// 800D180C, 800D18E4, 800D19A8, 800D1A3C (overlay 2, another part).
const REFRESH: [u32; 4] = [0x800d_180c, 0x800d_18e4, 0x800d_19a8, 0x800d_1a3c];
/// 80027338 (resident, `loading`).
const RESOURCE: u32 = 0x8002_7338;
/// 80027538 (resident, `arena`): reset the arena pointer.
const ARENA_RESET: u32 = 0x8002_7538;
/// 800FF19C (overlay 2, another part; PAL 800E7B50).
const FADE_OUT: u32 = 0x800f_f19c;
/// `jr ra; nop` stubs outside the function map (so no routine claims them)
/// in the handler tables of [`update`]: 800D1410 (mode 0 of
/// [`UPDATE_MODES`]) and 800F6CB4 (the steps of page 4, table 0x8010EA1C).
/// The original returns from them at once, and [`update`] ignores the
/// handlers' `v0`.
const IDLE: [u32; 2] = [0x800d_1410, 0x800f_6cb4];

fn state(m: &mut Machine) -> u32 {
    m.lw(STATE)
}

/// A handler of [`update`]'s tables (the [`IDLE`] stubs return at once).
fn run_handler(m: &mut Machine, handler: u32, args: [u32; 4]) {
    if !IDLE.contains(&handler) {
        dispatch::call(m, handler, args);
    }
}

/// Clear the state record's bytes `offsets` (the pointer re-read before
/// each, as the original does).
fn clear_bytes(m: &mut Machine, offsets: &[u32]) {
    for &offset in offsets {
        let record = state(m);
        m.sb(record.wrapping_add(offset), 0);
    }
}

/// The table pointers of the state record (overlay data addresses), the
/// selection's handler (800ECF88 for selections 0..3) and the four refresh
/// routines. `a` are the argument registers before the stores.
fn rebuild(m: &mut Machine, a: [u32; 4]) -> u32 {
    let record = state(m);
    for (offset, table) in [
        (0xc8, 0x8010_a784),
        (0xcc, 0x8010_a794),
        (0xd0, 0x8010_a7a0),
        (0xd4, 0x8010_a7b0),
        (0xb8, 0x8010_a8f0),
        (0xbc, 0x8010_a9e0),
        (0xc0, 0x8010_aa80),
    ] {
        m.sw(record.wrapping_add(offset), table);
    }
    let mut a0 = lh(m, record.wrapping_add(0xc));
    m.sw(record.wrapping_add(0xe4), 0);
    m.sw(record.wrapping_add(0xc4), 0x8010_ab50);
    if (a0 as i32) < 4 {
        let block = visual(m);
        let handlers = m.lw(block.wrapping_add(0x1bc));
        a0 = m.lw((a0 << 2).wrapping_add(handlers));
        dispatch::call(m, SELECT, [a0, a[1], a[2], a[3]]);
    }
    for routine in REFRESH {
        dispatch::call(m, routine, [a0, a[1], a[2], a[3]]);
    }
    a0
}

/// 800DE188: enter the hub (on the second call of visual block +0x270):
/// reset the state record, hide the map (80100994), rebuild the tables,
/// reload resource [resources +0x60] and its entry +4 into visual block
/// +0x1C and word table +0x160 slot 2, and copy its words +8/+0x10..+0x18
/// to visual block +0x98/+0xA0..+0xA8. Returns 0 on the first call, else
/// the visual block.
pub fn enter(m: &mut Machine, args: [u32; 4]) -> u32 {
    framed(m, 0x20, |m, _| {
        let block = visual(m);
        let count = m.lw(block.wrapping_add(0x270)).wrapping_add(1);
        m.sw(block.wrapping_add(0x270), count);
        if (count as i32) < 2 {
            return 0;
        }
        let first = state(m);
        m.sw(block.wrapping_add(0x270), 0);
        m.sb(first, 0);
        let record = state(m);
        m.sh(first.wrapping_add(0xc), 0);
        m.sb(record.wrapping_add(1), 0);
        clear_bytes(m, &[4, 5, 6, 2]);
        let record = state(m);
        m.sb(record.wrapping_add(0x1c), 0);
        let next = state(m);
        m.sw(record.wrapping_add(0x140), 0);
        m.sw(record.wrapping_add(0x10), 0);
        m.sb(next.wrapping_add(0x1d), 0);
        clear_bytes(m, &[0x1e, 0x1f, 0x20, 0x21, 0x22, 0x23]);
        dispatch::call(m, MAP_HIDE, [block, args[1], args[2], args[3]]);
        rebuild(m, args);
        finish_enter(m, args)
    })
}

/// The tail of [`enter`] (800DE324..).
fn finish_enter(m: &mut Machine, args: [u32; 4]) -> u32 {
    let block = visual(m);
    m.sb(block.wrapping_add(0x7d), 1);
    let current = visual(m);
    let resources = m.gw(RESOURCES);
    m.sw(block.wrapping_add(0x50), 0);
    m.sh(current.wrapping_add(0x1c6), 0);
    let id = m.lw(resources.wrapping_add(0x60));
    let resource = dispatch::call(m, RESOURCE, [id, args[1], args[2], args[3]]);
    let current = visual(m);
    m.sw(current.wrapping_add(0x1c), resource);
    let id = m.lw(resource.wrapping_add(4));
    let entry = dispatch::call(m, RESOURCE, [id, args[1], args[2], args[3]]);
    super::ov2_p2::set_word_160(m, entry, 2);
    let block = visual(m);
    let resource = m.lw(block.wrapping_add(0x1c));
    let words = m.lw(block.wrapping_add(0x1c));
    let value = m.lw(resource.wrapping_add(8));
    m.sw(block.wrapping_add(0x98), value);
    for (from, to) in [(0x10, 0xa0), (0x14, 0xa4), (0x18, 0xa8)] {
        let value = m.lw(words.wrapping_add(from));
        m.sw(block.wrapping_add(to), value);
    }
    block
}

/// 800DE3CC: one hub frame: save the visual packet pointer (+0x274) in
/// the state record (+8), run the page's step handler, on the leave flag
/// clear visual bytes +0x7C/+0x7D and halfwords +0x1C4/+0x1C6 and reset
/// the arena to the state record, run the mode handler (visual +0x1C6),
/// and restore the packet pointer. Returns it.
pub fn update(m: &mut Machine, args: [u32; 4]) -> u32 {
    framed(m, 0x20, |m, _| {
        let block = visual(m);
        let record = state(m);
        let packet = m.lw(block.wrapping_add(0x274));
        let page = lbu(m, record);
        m.sw(record.wrapping_add(8), packet);
        let step = lbu(m, record.wrapping_add(1));
        let table = m.lw((page << 2).wrapping_add(PAGES));
        let handler = m.lw((step << 2).wrapping_add(table));
        run_handler(m, handler, [record, args[1], args[2], args[3]]);
        let mut a0 = record;
        let current = state(m);
        if lbu(m, current.wrapping_add(2)) == 1 {
            let block = visual(m);
            m.sb(block.wrapping_add(0x7c), 0);
            let block = visual(m);
            m.sb(block.wrapping_add(0x7d), 0);
            let block = visual(m);
            a0 = state(m);
            m.sh(block.wrapping_add(0x1c4), 0);
            m.sh(block.wrapping_add(0x1c6), 0);
            dispatch::call(m, ARENA_RESET, [a0, args[1], args[2], args[3]]);
        }
        let block = visual(m);
        let mode = lh(m, block.wrapping_add(0x1c6));
        let handler = m.lw((mode << 2).wrapping_add(UPDATE_MODES));
        run_handler(m, handler, [a0, args[1], args[2], args[3]]);
        let record = state(m);
        let block = visual(m);
        let packet = m.lw(record.wrapping_add(8));
        m.sw(block.wrapping_add(0x274), packet);
        packet
    })
}

/// 800DE4BC: count visual byte +0x7D, set the fade color of [gp+0x2D4]
/// (+0x10..+0x12 = 0x8F, 0x87, 0x53; flag 8 in +0), start fade 2
/// (800FF19C) and select mode 0xA. Returns 0xA.
pub fn start_fade(m: &mut Machine, args: [u32; 4]) -> u32 {
    framed(m, 0x18, |m, _| {
        let block = visual(m);
        let count = lbu(m, block.wrapping_add(0x7d)).wrapping_add(1);
        m.sb(block.wrapping_add(0x7d), count as u8);
        for (offset, value) in [(0x10, 0x8f), (0x11, 0x87), (0x12, 0x53)] {
            let fade = m.gw(0x2d4);
            m.sb(fade.wrapping_add(offset), value);
        }
        let fade = m.gw(0x2d4);
        let flags = m.lw(fade) | 8;
        m.sw(fade, flags);
        dispatch::call(m, FADE_OUT, [2, args[1], args[2], args[3]]);
        let block = visual(m);
        m.sh(block.wrapping_add(0x1c6), 0xa);
        0xa
    })
}

/// 800DE540: run the load phase visual +0x1C6 (table 0x8010984C, argument
/// 0, through part 1's [`run_load_phase`], which also covers the idle stub
/// 800CE754 of phase 3); in mode 3 reset the state record to page 9, hide
/// the map, rebuild the tables, clear visual word +0x50 and set byte
/// +0x7D. Returns 3 when the mode is not 3, else 1.
pub fn run_menu(m: &mut Machine, args: [u32; 4]) -> u32 {
    framed(m, 0x20, |m, _| {
        run_load_phase(m);
        let block = visual(m);
        if lh(m, block.wrapping_add(0x1c6)) != 3 {
            return 3;
        }
        let record = state(m);
        m.sb(record, 9);
        clear_bytes(m, &[1]);
        let first = state(m);
        m.sb(first.wrapping_add(4), 0);
        let record = state(m);
        m.sh(first.wrapping_add(0xc), 0);
        m.sb(record.wrapping_add(5), 0);
        clear_bytes(m, &[6, 2]);
        let record = state(m);
        m.sb(record.wrapping_add(0x1c), 0);
        let next = state(m);
        m.sw(record.wrapping_add(0x10), 0);
        m.sb(next.wrapping_add(0x1d), 0);
        clear_bytes(m, &[0x1e, 0x1f, 0x20, 0x21, 0x22, 0x23]);
        dispatch::call(m, MAP_HIDE, [0, args[1], args[2], args[3]]);
        rebuild(m, [0, args[1], args[2], args[3]]);
        let block = visual(m);
        m.sw(block.wrapping_add(0x50), 0);
        m.sb(block.wrapping_add(0x7d), 1);
        1
    })
}

/// 800DE718: visual byte +0x7E = 0, word +0x50 = 0, byte +0x7D = 1.
/// Returns 1.
pub fn reset_menu(m: &mut Machine) -> u32 {
    let block = visual(m);
    m.sb(block.wrapping_add(0x7e), 0);
    let current = visual(m);
    m.sw(block.wrapping_add(0x50), 0);
    m.sb(current.wrapping_add(0x7d), 1);
    1
}

/// 800DE73C: on a press of pad bit 0x20 (context +0x12C) with none of
/// 0x5F held (+0x128), clear visual bytes +0x7C/+0x7D and set +0x7E = 4.
/// Returns the last test's value (0, the held bits, or 4).
pub fn check_cancel(m: &mut Machine) -> u32 {
    let context = m.gw(CONTEXT);
    let pressed = m.lw(context.wrapping_add(0x12c)) & 0x20;
    if pressed == 0 {
        return 0;
    }
    let held = m.lw(context.wrapping_add(0x128)) & 0x5f;
    if held != 0 {
        return held;
    }
    let block = visual(m);
    m.sb(block.wrapping_add(0x7c), 0);
    let block = visual(m);
    m.sb(block.wrapping_add(0x7d), 0);
    let block = visual(m);
    m.sb(block.wrapping_add(0x7e), 4);
    4
}

// ---- dispatch -------------------------------------------------------------------------------------

/// The routines of this file by JP address, while module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    Some(match target {
        0x800d_e188 => enter(m, args),
        0x800d_e3cc => update(m, args),
        0x800d_e4bc => start_fade(m, args),
        0x800d_e540 => run_menu(m, args),
        0x800d_e718 => reset_menu(m),
        0x800d_e73c => check_cancel(m),
        _ => return None,
    })
}
