//! Overlay module 2 (state 3), mission hub sub-mode: the hub screen
//! callbacks of mission states 0 and 4 (`800D483C` initialization,
//! `800D4B44` per-frame tick and its four phase callbacks), the hub
//! background fade (800E7CE8), the hub's VRAM save for option bits 0x50
//! (800CEC18), the rotating wedge backdrop ([`backdrop`]) and the reveal
//! grid and minimap markers ([`minimap`]). These are the 16 routines the
//! `ov2core` classification (`analysis/flight-runtime/ov2core-001/
//! routines.tsv`) assigns to `ov2hub`.
//!
//! The other inner states reached from the hub menu are submodules: the
//! save screen of state 9 ([`save`], hub choice 2 and after a mission with
//! option bit 8) with its memory-card layer ([`card`], whose BIOS and
//! libcard calls are host calls, [`crate::bios`]), the aircraft unlock
//! screen of state 12 ([`unlock`], after the post-mission load of state
//! 11) and the "CONNECTION LOST" reload of state 13 ([`lost`], entered with
//! the context's resume byte set).
//!
//! Translated from the overlay-2 instructions (the module loads at
//! 0x800C9DC0). Every routine follows its original instructions and store
//! order. Pointer arguments are guest addresses, as in the original ABI;
//! routines with an original stack frame that call other routines push
//! that frame (`Machine::push_frame`) so nested frames keep their original
//! addresses.
//!
//! Everything else is called by original address through
//! [`crate::dispatch::call`]: the `ov2core` routines (visual-script setters
//! 800F1770/800F18EC/800F1908/800F193C/800F196C/800F1BA0, text visibility
//! 800D0EE4/800D0F20, scene color 800E79D4, the 3D map's object reveal
//! 800E9824) and resident code (sound 8002D724, voice fade 8002DB18, SDK).
//! Argument registers are passed as the original holds them; for callees
//! that read no argument register the translation passes zeros.
//!
//! [`dispatch()`] claims these routines only while module 2 is the loaded
//! overlay (`common::loaded_module`), asking each submodule's
//! `claims::dispatch` after its own arms.
use crate::{
    common::{CONTEXT, loaded_module},
    dispatch,
    machine::{Machine, SmallData},
};

#[path = "ov2_hub_backdrop.rs"]
pub mod backdrop;
#[path = "ov2_hub_card.rs"]
pub mod card;
#[path = "ov2_hub_lost.rs"]
pub mod lost;
#[path = "ov2_hub_minimap.rs"]
pub mod minimap;
#[path = "ov2_hub_save.rs"]
pub mod save;
#[path = "ov2_hub_unlock.rs"]
pub mod unlock;

/// Overlay module number of the mission screens (state 3).
pub const MODULE: u32 = 2;

/// `[0x800F69D0]`: the mission state block (allocated at 0x800F9E18): +6
/// choice (signed halfword), +0x90 outer ordering table, +0x200C8 and
/// +0x200D4 three saved VRAM areas each, +0x200E0 a 4-byte buffer,
/// +0x20094 next free primitive.
pub const MISSION: u32 = 0x800f_69d0;
/// Mission block offset of the next-free-primitive word.
pub const NEXT_PRIMITIVE: u32 = 0x2_0094;
/// `[0x800F5E98]`: the hub state (0x800F9448): +0 flags (1 slide in, 2
/// slide out, 4 confirmed), +4 selection, +8 previous selection, +0xC
/// phase, +0x10 timer, +0x14 reveal timer, +0x18 32 pending reveal rows,
/// +0x98 32 visible rows, +0x118/+0x11A cursor cell, +0x11C exit offset.
pub const HUB: u32 = 0x800f_5e98;
/// 800F5E9C: the four hub phase callbacks.
pub const PHASES: u32 = 0x800f_5e9c;
/// 800F5958: 41 twelve-byte mission records (+1 map cell).
pub const MISSION_RECORDS: u32 = 0x800f_5958;
/// 800F5B54: three pairs of VRAM RECTs saved by 800CEC18.
pub const SAVED_AREAS: u32 = 0x800f_5b54;
/// 800F5C04: minimap cursor cells, two signed halfwords per map cell.
pub const CURSOR_CELLS: u32 = 0x800f_5c04;
/// `[0x800F9890]`: the 3D map state (owned by `ov2core`); +0x110..+0x120
/// hold the five background voice slots, +0x14 the tube phase and +0x20
/// its progress.
pub const MAP: u32 = 0x800f_9890;
/// Context +0x12C: buttons pressed this frame (0x10 confirm, 8 next, 4
/// previous). +0x970: mission option bits.
const PRESSED: u32 = 0x12c;
const OPTIONS: u32 = 0x970;

// Original routines owned elsewhere, called by address.
/// 8002D724: start sound effect `id` (resident, owned by `script`).
pub const PLAY_SOUND: u32 = 0x8002_d724;
/// 8002DB18: fade background voice `slot` to `volume` over `time` ticks
/// (resident).
pub const VOICE_FADE: u32 = 0x8002_db18;
/// 800F1770: visual-script slot style (`slot`, `style`).
pub const SET_STYLE: u32 = 0x800f_1770;
/// 800F18EC: visual-script slot offset (`slot`, `x`, `y`).
pub const SET_OFFSET: u32 = 0x800f_18ec;
/// 800F193C: visual-script slot parameter (`slot`, `value`).
pub const SET_PARAMETER: u32 = 0x800f_193c;
/// 800F196C: visual-script page (`page`).
pub const SELECT_PAGE: u32 = 0x800f_196c;
/// 800D0EE4: text entry visibility (`entry`, `value`).
pub const SET_TEXT: u32 = 0x800d_0ee4;
/// 800D0F20: clear all 256 text visibility bytes.
pub const CLEAR_TEXT: u32 = 0x800d_0f20;
/// 800E79D4: scene RGB (`red`, `green`, `blue`).
pub const SET_SCENE_COLOR: u32 = 0x800e_79d4;
/// 800E9824: start the 3D map's object reveal (no arguments).
pub const MAP_OBJECTS_REVEAL: u32 = 0x800e_9824;
/// 800A6970 StoreImage / 800A6664 DrawSync (resident libgpu).
pub const STORE_IMAGE: u32 = 0x800a_6970;
pub const DRAW_SYNC: u32 = 0x800a_6664;

pub(crate) fn call(m: &mut Machine, target: u32, args: [u32; 4]) -> u32 {
    dispatch::call(m, target, args)
}

fn sound(m: &mut Machine, id: u32) {
    call(m, PLAY_SOUND, [id, 0, 0, 0]);
}

fn context(m: &mut Machine) -> u32 {
    m.gw(CONTEXT)
}

fn mission(m: &mut Machine) -> u32 {
    m.lw(MISSION)
}

fn hub(m: &mut Machine) -> u32 {
    m.lw(HUB)
}

/// Low 24 bits of a tag word (the link) and the high 8 (the length).
pub(crate) const LINK: u32 = 0x00ff_ffff;
pub(crate) const LENGTH: u32 = 0xff00_0000;

/// libgpu `addPrim(slot, prim)` as compiled inline: the packet keeps its
/// length and takes the slot's link, then the slot (reloaded) links to the
/// packet.
pub(crate) fn add_prim(m: &mut Machine, slot: u32, prim: u32) {
    let tag = m.lw(prim);
    let link = m.lw(slot);
    m.sw(prim, (tag & LENGTH) | (link & LINK));
    let link = m.lw(slot);
    m.sw(slot, (link & LENGTH) | (prim & LINK));
}

/// High word of the signed 64-bit product (`mult` + `mfhi`) of `a` and the
/// constant word `b`.
pub(crate) fn mulhi(a: i32, b: u32) -> i32 {
    ((i64::from(a) * i64::from(b as i32)) >> 32) as i32
}

/// 800D186C: map cell byte (+1) of mission record `index`.
pub fn mission_cell_d186c(m: &mut Machine, index: u32) -> u32 {
    let record = MISSION_RECORDS.wrapping_add(index.wrapping_mul(12));
    u32::from(m.lbu(record.wrapping_add(1)))
}

/// Whether the hub slides its panels for `selection` under option bits
/// `options` (the test shared by 800D483C and 800D45E4): with option 0x10
/// only selections 1 and 2, otherwise every selection but 3.
fn slides(options: u32, selection: u32) -> bool {
    if options & 0x10 != 0 {
        selection.wrapping_sub(1) < 2
    } else {
        selection != 3
    }
}

/// 800CEC18: save the three pairs of VRAM areas at [`SAVED_AREAS`]: each
/// RECT gets an arena buffer of `w * h * 2` bytes (mission +0x200C8 and
/// +0x200D4, indexed by pair) filled by StoreImage; then DrawSync(0) and a
/// 4-byte buffer at mission +0x200E0. Returns that buffer.
pub fn save_areas_cec18(m: &mut Machine) -> u32 {
    m.push_frame(0x30);
    let mut first = SAVED_AREAS;
    let mut second = SAVED_AREAS + 8;
    for pair in 0..3u32 {
        let height = m.lh(first + 6) as i32;
        let width = m.lh(first + 4) as i32;
        let bytes = width.wrapping_mul(height << 1) as u32;
        let buffer = crate::arena::alloc(m, bytes);
        let slot = mission(m).wrapping_add(pair * 4).wrapping_add(0x2_0000);
        m.sw(slot + 0xc8, buffer);
        let height = m.lh(first + 0xe) as i32;
        let width = m.lh(first + 0xc) as i32;
        let bytes = width.wrapping_mul(height << 1) as u32;
        let buffer = crate::arena::alloc(m, bytes);
        let slot = mission(m).wrapping_add(pair * 4).wrapping_add(0x2_0000);
        let pixels = m.lw(slot + 0xc8);
        m.sw(slot + 0xd4, buffer);
        call(m, STORE_IMAGE, [first, pixels, 0, 0]);
        first += 0x10;
        let slot = mission(m).wrapping_add(pair * 4).wrapping_add(0x2_0000);
        let pixels = m.lw(slot + 0xd4);
        call(m, STORE_IMAGE, [second, pixels, 0, 0]);
        second += 0x10;
    }
    call(m, DRAW_SYNC, [0, 0, 0, 0]);
    let buffer = crate::arena::alloc(m, 4);
    let block = mission(m);
    m.sw(block.wrapping_add(0x2_00e0), buffer);
    m.pop_frame(0x30);
    buffer
}

/// 800D483C: hub initialization (mission states 0 and 4, substate 0).
/// Returns 1 (advance the substate).
pub fn init_d483c(m: &mut Machine) -> u32 {
    m.push_frame(0x28);
    let ctx = context(m);
    let state = hub(m);
    let options = m.lw(ctx + OPTIONS);
    let block = mission(m);
    m.sw(state, 0);
    let choice = m.lh(block + 6) as i32 as u32;
    m.sw(state + 0xc, 0);
    m.sw(state + 4, choice);
    m.sw(state + 8, choice);
    let prim = m.lw(block.wrapping_add(NEXT_PRIMITIVE));
    let selection = m.lw(state + 4);
    let next = backdrop::draw_d170c(m, prim, selection, 0);
    let block = mission(m);
    m.sw(block.wrapping_add(NEXT_PRIMITIVE), next);
    let state = hub(m);
    let slide = if options & 0x40 != 0 {
        true
    } else {
        let selection = m.lw(state + 4);
        slides(options, selection)
    };
    if slide {
        let flags = m.lw(state);
        m.sw(state, flags | 1);
    }
    let state = hub(m);
    for row in 0..0x20u32 {
        let at = state + row * 4;
        m.sw(at + 0x14, 0);
        m.sw(at + 0x98, 0);
        m.sw(at + 0x18, 0);
    }
    let ctx = context(m);
    let cell = if m.lw(ctx + OPTIONS) & 0x10 != 0 {
        0x3b
    } else {
        let current = m.lh(ctx + 0x390) as i32 as u32;
        mission_cell_d186c(m, current)
    };
    let entry = CURSOR_CELLS.wrapping_add(cell.wrapping_mul(4));
    let column = m.lh(entry) as i32 as u32;
    let state = hub(m);
    let shift = m.lh(entry + 2) as i32 as u32;
    let x = m.lhu(entry);
    let row = state.wrapping_add(column.wrapping_mul(4));
    let bit = 1u32 << (shift & 31);
    let visible = m.lw(row + 0x98);
    let y = m.lhu(entry + 2);
    m.sw(row + 0x98, visible | bit);
    let timer: i32 = 0x19;
    let selection = m.lw(state + 4);
    m.sw(state + 0x10, timer as u32);
    m.sh(state + 0x118, x);
    m.sh(state + 0x11a, y);
    m.sw(state + 0x11c, 0);
    if selection == 2 || options & 0x40 != 0 {
        call(m, SET_OFFSET, [1, 0, 0x271, 0]);
    }
    let state = hub(m);
    if m.lw(state) & 1 != 0 {
        let x = timer.wrapping_neg().wrapping_mul(timer) as u32;
        call(m, SET_OFFSET, [2, x, 0, 0]);
        sound(m, 0x5a);
    }
    call(m, SET_OFFSET, [3, 0, 0, 0]);
    call(m, SET_STYLE, [5, 0, 0, 0]);
    call(m, CLEAR_TEXT, [0; 4]);
    let state = hub(m);
    let entry = m.lw(state + 4).wrapping_add(0xb9);
    call(m, SET_TEXT, [entry, 1, 0, 0]);
    call(m, SET_PARAMETER, [1, 0, 0, 0]);
    let rgb = if options & 0x40 != 0 {
        background_fade_e7ce8(m);
        call(m, MAP_OBJECTS_REVEAL, [0; 4]);
        [0, 0, 0]
    } else {
        [0x8f, 0x87, 0x53]
    };
    call(m, SET_SCENE_COLOR, [rgb[0], rgb[1], rgb[2], 0]);
    if options & 0x50 == 0x50 {
        save_areas_cec18(m);
    }
    call(m, SELECT_PAGE, [2, 0, 0, 0]);
    highlight(m);
    m.pop_frame(0x28);
    1
}

/// The highlight loop of the hub routines: slots 1..4 style 2, then the
/// selected slot (`[hub+4] + 1`) style 1.
fn highlight(m: &mut Machine) {
    for slot in 1..5 {
        call(m, SET_STYLE, [slot, 2, 0, 0]);
    }
    let state = hub(m);
    let slot = m.lw(state + 4).wrapping_add(1);
    call(m, SET_STYLE, [slot, 1, 0, 0]);
}

/// The navigation step of phases 0..2 (input `pressed`, known not to be
/// confirm): next (8) wraps 3 -> 0, previous (4) wraps 0 -> 3, each with
/// sound 0x3D.
fn navigate(m: &mut Machine, pressed: u32) {
    let state = hub(m);
    if pressed & 8 != 0 {
        let selection = m.lw(state + 4);
        if (selection as i32) < 3 {
            m.sw(state + 4, selection.wrapping_add(1));
        } else {
            m.sw(state + 4, 0);
        }
        sound(m, 0x3d);
    } else if pressed & 4 != 0 {
        let selection = m.lw(state + 4);
        let next = if (selection as i32) > 0 {
            selection.wrapping_sub(1)
        } else {
            3
        };
        m.sw(state + 4, next);
        sound(m, 0x3d);
    }
}

/// Signed `x / 25` as compiled (multiply by 0x51EB851F, shift 3).
fn div25(x: i32) -> i32 {
    (mulhi(x, 0x51eb_851f) >> 3).wrapping_sub(x >> 31)
}

/// 800D4180: hub phase 0, the slide-in. Returns 1 when finished.
pub fn phase_slide_in_d4180(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let state = hub(m);
    let flags = m.lw(state);
    let mut timer = m.lw(state + 0x10) as i32;
    if flags & 1 == 0 {
        m.sw(state + 0x10, 0);
        m.pop_frame(0x20);
        return 1;
    }
    let ctx = context(m);
    let pressed = m.lw(ctx + PRESSED);
    if pressed & 0x10 != 0 {
        m.sw(state, flags | 4);
    } else {
        navigate(m, pressed);
    }
    highlight(m);
    let state = hub(m);
    if m.lw(state) & 4 != 0 {
        m.sw(state + 0x10, 0);
        timer = 0;
        call(m, SET_OFFSET, [1, 0, 0, 0]);
        call(m, SET_OFFSET, [2, 0, 0, 0]);
        call(m, SET_SCENE_COLOR, [0x8f, 0x87, 0x53, 0]);
    }
    let result = if timer <= 0 {
        let ctx = context(m);
        let options = m.lw(ctx + OPTIONS);
        let state = hub(m);
        m.sw(ctx + OPTIONS, options & !0x40);
        let flags = m.lw(state);
        m.sw(state, flags & !1);
        sound(m, 0x5c);
        let state = hub(m);
        m.sw(state + 0x10, 0);
        1
    } else {
        let state = hub(m);
        let ctx = context(m);
        if m.lw(state + 8) == 2 || m.lw(ctx + OPTIONS) & 0x40 != 0 {
            let y = timer.wrapping_mul(timer) as u32;
            call(m, SET_OFFSET, [1, 0, y, 0]);
            let ctx = context(m);
            if m.lw(ctx + OPTIONS) & 0x40 != 0 {
                let amount = 25i32.wrapping_sub(timer);
                let red = div25(amount.wrapping_mul(143));
                let green = div25(amount.wrapping_mul(135));
                let blue = div25(amount.wrapping_mul(83));
                call(
                    m,
                    SET_SCENE_COLOR,
                    [red as u32, green as u32, blue as u32, 0],
                );
            }
        }
        let state = hub(m);
        if m.lw(state) & 1 != 0 {
            let x = timer.wrapping_neg().wrapping_mul(timer) as u32;
            call(m, SET_OFFSET, [2, x, 0, 0]);
        }
        let state = hub(m);
        let timer = m.lw(state + 0x10);
        m.sw(state + 0x10, timer.wrapping_sub(1));
        0
    };
    m.pop_frame(0x20);
    result
}

/// 800D4448: hub phase 1, the map reveal. Returns 1 when finished.
pub fn phase_reveal_d4448(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let ctx = context(m);
    let pressed = m.lw(ctx + PRESSED);
    if pressed & 0x10 != 0 {
        let state = hub(m);
        let flags = m.lw(state);
        m.sw(state, flags | 4);
    } else {
        navigate(m, pressed);
    }
    highlight(m);
    let state = hub(m);
    let result = if m.lw(state) & 4 != 0 {
        call(m, SET_PARAMETER, [1, 10, 0, 0]);
        call(m, SET_STYLE, [5, 1, 0, 0]);
        let state = hub(m);
        m.sw(state + 0x14, 0x78);
        let flags = m.lw(state);
        m.sw(state, flags & !4);
        1
    } else {
        let timer = m.lw(state + 0x14);
        if (timer as i32) < 0x3c {
            let timer = timer.wrapping_add(1);
            m.sw(state + 0x14, timer);
            if (timer as i32) < 0xb {
                call(m, SET_PARAMETER, [1, timer, 0, 0]);
            }
            let state = hub(m);
            if m.lw(state + 0x14) == 0x1e {
                call(m, SET_STYLE, [5, 1, 0, 0]);
            }
            0
        } else {
            1
        }
    };
    m.pop_frame(0x18);
    result
}

/// 800D45E4: hub phase 2, the steady selection. Confirm stores the
/// selection as the mission choice, marks the slide-out and returns 1.
pub fn phase_select_d45e4(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let ctx = context(m);
    let pressed = m.lw(ctx + PRESSED);
    let result = if pressed & 0x10 != 0 {
        let state = hub(m);
        let block = mission(m);
        let selection = m.lhu(state + 4);
        m.sh(block + 6, selection);
        let options = m.lw(ctx + OPTIONS);
        let selection = m.lw(state + 4);
        if slides(options, selection) {
            let flags = m.lw(state);
            m.sw(state, flags | 2);
        }
        sound(m, 0x3c);
        let state = hub(m);
        if m.lw(state) & 2 != 0 {
            sound(m, 0x5b);
        }
        1
    } else {
        navigate(m, pressed);
        highlight(m);
        0
    };
    m.pop_frame(0x18);
    result
}

/// 800D4760: hub phase 3, the slide-out. Returns 2 when finished.
pub fn phase_slide_out_d4760(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let state = hub(m);
    let timer = m.lw(state + 0x10) as i32;
    let result = if timer < 0x19 {
        let offset = timer.wrapping_neg().wrapping_mul(timer) as u32;
        let next = timer.wrapping_add(1);
        let selection = m.lw(state + 4);
        m.sw(state + 0x10, next as u32);
        m.sw(state + 0x11c, offset);
        let square = next.wrapping_mul(next);
        if selection == 2 {
            call(m, SET_OFFSET, [1, 0, square as u32, 0]);
        }
        let state = hub(m);
        if m.lw(state) & 2 != 0 {
            call(m, SET_OFFSET, [2, square.wrapping_neg() as u32, 0, 0]);
        }
        call(m, SET_OFFSET, [3, square.wrapping_neg() as u32, 0, 0]);
        0
    } else {
        let flags = m.lw(state);
        m.sw(state, flags & !2);
        2
    };
    m.pop_frame(0x20);
    result
}

/// 800D4B44: hub tick (mission states 0 and 4, substate 1). Runs the phase
/// callback (`jalr` through [`PHASES`]); 1 advances the phase, 2 returns
/// 0x100 (leave the hub). Otherwise the text, reveal grid, frame and
/// markers are updated and drawn, and the result is 0.
pub fn tick_d4b44(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let block = mission(m);
    let state = hub(m);
    let prim = m.lw(block.wrapping_add(NEXT_PRIMITIVE));
    let selection = m.lw(state + 4);
    let next = backdrop::draw_d170c(m, prim, selection, 0);
    let block = mission(m);
    m.sw(block.wrapping_add(NEXT_PRIMITIVE), next);
    let state = hub(m);
    let phase = m.lw(state + 0xc);
    let callback = m.lw(PHASES.wrapping_add(phase.wrapping_mul(4)));
    // The phase callbacks read no argument register.
    let result = call(m, callback, [0; 4]);
    if result == 2 {
        m.pop_frame(0x20);
        return 0x100;
    }
    if result == 1 {
        let state = hub(m);
        let phase = m.lw(state + 0xc);
        m.sw(state + 0xc, phase.wrapping_add(1));
    }
    call(m, CLEAR_TEXT, [0; 4]);
    let state = hub(m);
    let entry = m.lw(state + 4).wrapping_add(0xb9);
    call(m, SET_TEXT, [entry, 1, 0, 0]);
    minimap::propagate_d3c50(m);
    let block = mission(m);
    let prim = m.lw(block.wrapping_add(NEXT_PRIMITIVE));
    let state = hub(m);
    let ot = m.lw(block + 0x90).wrapping_add(0x38);
    let reveal = m.lw(state + 0x14);
    let next = minimap::grid_d3908(m, prim, ot, reveal);
    let block = mission(m);
    m.sw(block.wrapping_add(NEXT_PRIMITIVE), next);
    let state = hub(m);
    let ot = m.lw(block + 0x90);
    let reveal = m.lw(state + 0x14);
    let next = minimap::markers_d3ddc(m, next, ot, reveal);
    let block = mission(m);
    m.sw(block.wrapping_add(NEXT_PRIMITIVE), next);
    m.pop_frame(0x20);
    0
}

/// 800E7CE8: start the hub background fade-in: sound 0x62, the five
/// background voices to volume 100 over 60 ticks, then map +0x14 = 1 (tube
/// reveal) and +0x20 = 0.
pub fn background_fade_e7ce8(m: &mut Machine) {
    m.push_frame(0x18);
    sound(m, 0x62);
    for slot in [0x110, 0x114, 0x118, 0x11c, 0x120] {
        let block = m.lw(MAP);
        let voice = m.lw(block + slot);
        call(m, VOICE_FADE, [voice, 0x64, 0x3c, 0]);
    }
    let block = m.lw(MAP);
    m.sw(block + 0x14, 1);
    m.sw(block + 0x20, 0);
    m.pop_frame(0x18);
}

/// Claim this module's routines (original register ABI, returning `v0`)
/// while overlay module 2 is loaded. Routines whose `v0` no caller reads
/// still return the original's final `v0`.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    let [a0, a1, a2, a3] = args;
    Some(match target {
        0x800c_ec18 => save_areas_cec18(m),
        0x800c_f404 => {
            let stack: [u32; 5] = std::array::from_fn(|i| m.lw(m.sp + 0x10 + i as u32 * 4));
            minimap::tile_cf404(m, a0, a1, a2, a3, stack)
        }
        0x800d_12ec => {
            backdrop::spin_d12ec(m, a0);
            0
        }
        0x800d_13e8 => backdrop::wedges_d13e8(m, a0, a1),
        0x800d_170c => backdrop::draw_d170c(m, a0, a1, a2),
        0x800d_186c => mission_cell_d186c(m, a0),
        0x800d_3908 => minimap::grid_d3908(m, a0, a1, a2),
        0x800d_3c50 => {
            minimap::propagate_d3c50(m);
            0
        }
        0x800d_3ddc => minimap::markers_d3ddc(m, a0, a1, a2),
        0x800d_4180 => phase_slide_in_d4180(m),
        0x800d_4448 => phase_reveal_d4448(m),
        0x800d_45e4 => phase_select_d45e4(m),
        0x800d_4760 => phase_slide_out_d4760(m),
        0x800d_483c => init_d483c(m),
        0x800d_4b44 => tick_d4b44(m),
        0x800e_7ce8 => {
            background_fade_e7ce8(m);
            1
        }
        _ => {
            return card::claims::dispatch(m, target, args)
                .or_else(|| save::claims::dispatch(m, target, args))
                .or_else(|| unlock::claims::dispatch(m, target, args))
                .or_else(|| lost::claims::dispatch(m, target, args));
        }
    })
}
