//! Overlay module 2 (game state 3: mission hub, map, briefing, aircraft and
//! weapon selection, launch) on the address-preserving machine: the core
//! every sub-mode stands on. See `docs/FLIGHT_MACHINE.md`, "Overlay modules".
//!
//! Module 2 loads at [`OVERLAY_BASE`](crate::common::OVERLAY_BASE); its
//! routines are claimed by [`dispatch`] only while
//! [`loaded_module`](crate::common::loaded_module) is 2.
//!
//! # State 3 callbacks (table 0x800F1E34)
//!
//! | Substate | Callback | Rust |
//! | ---: | --- | --- |
//! | 0 | `800CB8B4` | [`initialize`]: display state, scene block, inner state, subsystems |
//! | 1 | `800CBB88` | [`update`]: ordering tables, inner dispatcher, shared drawing |
//! | 2 | `800CBC70` | [`substate_2`]: returns 1 |
//! | 3 | `800CBC78` | [`substate_3`]: confirm/cancel → 0x100/0x200 |
//!
//! The resident dispatcher (`mainloop::dispatch_state_with`) calls them as
//! `callback(requested_module, context)`; [`initialize`] returns
//! NEXT_SUBSTATE (0x04000000) and [`update`] returns the inner dispatcher's
//! resident transition (0x60000002/4/9) or 0.
//!
//! # Inner states
//!
//! The scene block (`[0x800F69D0]`, 0x200E8 bytes from the arena) starts
//! with five signed halfwords: inner state, substate, previous state, choice,
//! auxiliary choice. +0x10..+0x8F are two 16-entry ordering tables (one per
//! display buffer, `[scene+0x90]` the current one), +0x94 and +0x10094 two
//! 64 KiB primitive areas (`[scene+0x20094]` the current packet pointer).
//! [`mission_dispatch`] (`800CB408`) calls `table[state][substate]` from
//! 0x800F1DD4 (the sub-mode callbacks, owned by `ov2_hub`/`ov2_brief`/
//! `ov2_air` and the loader here) and routes its result.
//!
//! Sub-modules: [`loader`] (inner state 14, the mission loader), [`text`]
//! (font text lists), [`map`] (map and background state), [`visual`] (the
//! visual-script interpreter and its opcode handlers).
//!
//! Resident routines are called through [`dispatch::call`] (arena routines
//! through [`arena`]); sub-mode routines of other agents too.
use crate::{
    arena,
    common::{CONTEXT, loaded_module},
    dispatch,
    machine::{Machine, SmallData},
    sdk,
};

#[path = "ov2_core_loader.rs"]
pub mod loader;
#[path = "ov2_core_map.rs"]
pub mod map;
#[path = "ov2_core_text.rs"]
pub mod text;
#[path = "ov2_core_visual.rs"]
pub mod visual;

/// Overlay module number of state 3.
pub const MODULE: u32 = 2;

// ---- globals (overlay 2 data) ----------------------------------------------------

/// `[0x800F69D0]`: the scene block (inner state halfwords, ordering tables,
/// primitive areas).
pub const SCENE: u32 = 0x800f_69d0;
/// `[0x800F9E14]`: visual block (0x528 bytes): +0 flags (bit 0 scripts
/// bound), +4 packet pointer, +8 current ordering table, +0xC bucket
/// pointer, +0x10.. two 16-entry ordering tables, +0x90 display buffer,
/// +0x98 128 RGB records, +0x298 32 offset pairs, +0x318 16 records of 16
/// bytes (+0x326 parameter), +0x418 16 records of 8 bytes, +0x498/+0x49C
/// record selectors, +0x4A0 page (script index), +0x4A4 128 style bytes,
/// +0x524 script table.
pub const VISUAL: u32 = 0x800f_9e14;
/// `[0x800F9E10]`: scratch state of the visual interpreter (0x1F800000).
pub const VISUAL_SCRATCH: u32 = 0x800f_9e10;
/// `[0x800F5394]`: stream request block (+0 flags, +0x1004 request).
pub const STREAM: u32 = 0x800f_5394;
/// `[0x800F5398]`: text list block (+0 list, +4 strings, +8 cursor, +0xC
/// format argument, +0x10 256 visibility bytes).
pub const TEXT: u32 = 0x800f_5398;
/// `[0x800F539C]`: hub backdrop wedges.
pub const BACKDROP: u32 = 0x800f_539c;
/// `[0x800F9890]`: map/background block.
pub const MAP: u32 = 0x800f_9890;
/// `[0x800F5E74]`: loader control halfwords (phase, substate).
pub const LOADER: u32 = 0x800f_5e74;
/// `[0x800F6860]`: briefing block (+4/+6 phase/substate, +0x10 ordering table).
pub const BRIEFING: u32 = 0x800f_6860;
/// `[0x800F5FD4]`: aircraft selection block (+0/+4 phase/substate words).
pub const AIRCRAFT: u32 = 0x800f_5fd4;
/// Inner-state callback arrays (16 pointers).
pub const STATE_TABLE: u32 = 0x800f_1dd4;
/// Next-route bytes per inner state (bit 7: remember previous; 0x7F: stay).
pub const NEXT_ROUTE: u32 = 0x800f_1e14;
/// Back-route bytes per inner state (0x7E: previous, 0x7F: stay).
pub const BACK_ROUTE: u32 = 0x800f_1e24;
/// Briefing phase callback arrays.
pub const BRIEFING_PHASES: u32 = 0x800f_68b8;
/// Aircraft selection phase callback arrays.
pub const AIRCRAFT_PHASES: u32 = 0x800f_6018;
/// `[gp+0x2C4]`: resident display request block (+0 flags, +4 phase,
/// +0x10..+0x12 background RGB, +0x14 VSync interval).
pub const DISPLAY: u32 = 0x2c4;

/// Resident state-dispatcher result: advance the substate.
pub const NEXT_SUBSTATE: u32 = 0x0400_0000;

// ---- shared helpers ----------------------------------------------------------------

/// Original-ABI call of another module's routine (resident or another
/// agent's sub-mode routine).
pub(crate) fn call(m: &mut Machine, target: u32, args: [u32; 4]) -> u32 {
    dispatch::call(m, target, args)
}
/// Run `body` inside the original stack frame of `size` bytes (its base is
/// the routine's `sp`), so callee frames and passed stack addresses match.
pub(crate) fn framed<R>(
    m: &mut Machine,
    size: u32,
    body: impl FnOnce(&mut Machine, u32) -> R,
) -> R {
    let sp = m.push_frame(size);
    let result = body(m, sp);
    m.pop_frame(size);
    result
}
pub(crate) fn scene(m: &mut Machine) -> u32 {
    m.lw(SCENE)
}
pub(crate) fn context(m: &mut Machine) -> u32 {
    m.gw(CONTEXT)
}
/// Word at context + `offset`.
pub(crate) fn context_word(m: &mut Machine, offset: u32) -> u32 {
    let ctx = context(m);
    m.lw(ctx.wrapping_add(offset))
}
/// The scene packet pointer (`[scene+0x20094]`).
pub(crate) fn current_packet(m: &mut Machine) -> u32 {
    let block = scene(m);
    m.lw(block.wrapping_add(0x20094))
}
pub(crate) fn set_packet(m: &mut Machine, packet: u32) {
    let block = scene(m);
    m.sw(block.wrapping_add(0x20094), packet);
}
pub(crate) fn visual_block(m: &mut Machine) -> u32 {
    m.lw(VISUAL)
}
pub(crate) fn sext16(value: i16) -> u32 {
    value as i32 as u32
}
pub(crate) fn sext8(value: i8) -> u32 {
    value as i32 as u32
}
/// `lwl rt, address` (little-endian).
pub(crate) fn lwl(m: &mut Machine, rt: u32, address: u32) -> u32 {
    let shift = (address & 3) * 8;
    let word = m.lw(address & !3);
    (rt & 0x00ff_ffffu32.wrapping_shr(shift)) | word.wrapping_shl(24 - shift)
}
/// `lwr rt, address` (little-endian).
pub(crate) fn lwr(m: &mut Machine, rt: u32, address: u32) -> u32 {
    let shift = (address & 3) * 8;
    let word = m.lw(address & !3);
    let keep = if shift == 0 {
        0
    } else {
        0xffff_ffffu32 << (32 - shift)
    };
    (rt & keep) | (word >> shift)
}
/// `swl rt, address` (little-endian).
pub(crate) fn swl(m: &mut Machine, rt: u32, address: u32) {
    let shift = (address & 3) * 8;
    let aligned = address & !3;
    let word = m.lw(aligned);
    let keep = if shift == 24 {
        0
    } else {
        0xffff_ff00u32 << shift
    };
    m.sw(aligned, (word & keep) | (rt >> (24 - shift)));
}
/// `swr rt, address` (little-endian).
pub(crate) fn swr(m: &mut Machine, rt: u32, address: u32) {
    let shift = (address & 3) * 8;
    let aligned = address & !3;
    let word = m.lw(aligned);
    let keep = if shift == 0 {
        0
    } else {
        0xffff_ffffu32 >> (32 - shift)
    };
    m.sw(aligned, (word & keep) | (rt << shift));
}
/// `lwl rt, address+3; lwr rt, address`: an unaligned word load.
pub(crate) fn load_unaligned(m: &mut Machine, address: u32) -> u32 {
    let high = lwl(m, 0, address.wrapping_add(3));
    lwr(m, high, address)
}
/// `swl rt, address+3; swr rt, address`: an unaligned word store.
pub(crate) fn store_unaligned(m: &mut Machine, address: u32, value: u32) {
    swl(m, value, address.wrapping_add(3));
    swr(m, value, address);
}
/// The libgpu `addPrim` idiom of compiled code (`lwl a, 2(ot); swl a,
/// 2(prim); swl prim<<8, 2(ot)`): the primitive takes over the ordering
/// table entry's link and the entry points at the primitive.
pub(crate) fn add_prim(m: &mut Machine, ot: u32, prim: u32) {
    let link = lwl(m, 0, ot.wrapping_add(2));
    swl(m, link, prim.wrapping_add(2));
    swl(m, prim << 8, ot.wrapping_add(2));
}
/// The masked-or idiom `p[0] = (p[0] & 0xFF000000) | (q & 0x00FFFFFF)`.
pub(crate) fn link_word(m: &mut Machine, at: u32, target: u32) {
    let word = m.lw(at);
    m.sw(at, (word & 0xff00_0000) | (target & 0x00ff_ffff));
}

// ---- 800CB8B4 / 800CBB88 / 800CBC70 / 800CBC78: state-3 callbacks -------------------

/// 800CB8B4 (substate 0): reset the display RGB bytes, display phase 1 and
/// +0x14, set display flags 9; allocate the scene block (0x200E8 bytes) and
/// clear its five halfwords and +0x200E4; set options bits 0x42 (and the
/// 8 → 0x10 conversion), choose the first inner state from the resume byte
/// and progress bits 0x10/0x20/8; then 800CB3BC and the subsystem
/// initializers 800F197C, 800F1A9C(0), 800D0C2C, 800D0E90, 800D169C,
/// 800EAF0C. Returns NEXT_SUBSTATE.
pub fn initialize(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let display = m.gw(DISPLAY);
        m.sb(display + 0x10, 0);
        let again = m.gw(DISPLAY);
        m.sw(display + 4, 1);
        m.sw(display + 0x14, 0);
        m.sb(again + 0x11, 0);
        let display = m.gw(DISPLAY);
        m.sb(display + 0x12, 0);
        let display = m.gw(DISPLAY);
        let flags = m.lw(display);
        m.sw(display, flags | 9);
        let block = arena::alloc(m, 0x200e8);
        let ctx = context(m);
        m.sh(block, 0);
        m.sh(block + 4, 0);
        m.sh(block + 2, 0);
        m.sh(block + 6, 0);
        m.sh(block + 8, 0);
        m.sw(block.wrapping_add(0x200e4), 0);
        let options = m.lw(ctx + 0x970);
        m.sw(SCENE, block);
        let options = options | 0x42;
        m.sw(ctx + 0x970, options);
        if options & 8 != 0 {
            m.sw(ctx + 0x970, (options & !2) | 0x10);
        }
        let ctx = context(m);
        let options = m.lw(ctx + 0x970);
        if options & 0x10 != 0 {
            m.sw(ctx + 0x970, options & !2);
        }
        let ctx = context(m);
        if m.lbu(ctx + 0x95b) != 0 {
            let scene = scene(m);
            m.sh(scene, 0xd);
            m.sb(ctx + 0x95b, 0);
        } else {
            let progress = m.lw(ctx + 0x964);
            if progress & 0x10 != 0 {
                let scene = scene(m);
                m.sh(scene, 0xe);
                let progress = m.lw(ctx + 0x964);
                m.sw(ctx + 0x964, progress & !0x10);
            } else if progress & 0x20 != 0 {
                let scene = scene(m);
                m.sh(scene, 0xf);
                let progress = m.lw(ctx + 0x964);
                m.sw(ctx + 0x964, progress & !0x20);
                m.sh(scene + 6, 3);
                if m.lw(ctx + 0x970) & 0x10 != 0 {
                    m.sh(scene + 4, 4);
                }
                let ctx = context(m);
                let mission = m.lhu(ctx + 0x394);
                let aircraft = m.lhu(ctx + 0x396);
                m.sh(ctx + 0x390, mission);
                m.sh(ctx + 0x392, aircraft);
            } else if progress & 8 != 0 {
                let scene = scene(m);
                m.sw(ctx + 0x964, progress & !8);
                m.sh(scene, 0xb);
            }
        }
        refresh_briefing_option(m);
        visual::initialize(m);
        visual::bind_scripts(m, 0);
        reset_stream(m);
        text::reset_visibility(m);
        reset_backdrop(m);
        map::initialize(m);
        NEXT_SUBSTATE
    })
}

/// 800CBB88 (substate 1): prepare the ordering tables (800CBAB8), set style
/// byte 0x78 from held input bit 0x10, run the inner dispatcher; when it
/// returns 0 draw the shared layers: text lists (800D0FC0), map ordering
/// table (800E7AB4), map (800EB014), visual scripts (800F1BB8), then the
/// stream monitor (800D0CC4). Returns the dispatcher's result.
pub fn update(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        prepare_frame(m);
        let held = context_word(m, 0x124);
        visual::set_style(m, 0x78, u32::from(held & 0x10 != 0));
        let result = mission_dispatch(m);
        if result != 0 {
            return result;
        }
        let block = scene(m);
        let packet = m.lw(block.wrapping_add(0x20094));
        let ot = m.lw(block + 0x90);
        let packet = text::draw_lists(m, packet, ot);
        let block = scene(m);
        m.sw(block.wrapping_add(0x20094), packet);
        let ot = m.lw(block + 0x90);
        map::prepare_ot(m, ot.wrapping_add(0x3c));
        let packet = current_packet(m);
        let packet = map::draw(m, packet);
        set_packet(m, packet);
        let packet = visual::draw(m, packet);
        set_packet(m, packet);
        update_stream(m);
        0
    })
}

/// 800CBC70 (substate 2): returns 1.
pub fn substate_2() -> u32 {
    1
}

/// 800CBC78 (substate 3): pressed input bit 0x10 → 0x100, else bit 0x20
/// shifted to 0x200.
pub fn substate_3(m: &mut Machine) -> u32 {
    let pressed = context_word(m, 0x12c);
    if pressed & 0x10 != 0 {
        0x100
    } else {
        (pressed << 4) & 0x200
    }
}

// ---- 800CB3BC / 800CB408: inner dispatcher ------------------------------------------

/// 800CB3BC: options bit 0x80 (briefing bypass) set exactly for mission IDs
/// 32..37 (`[context+0x390] - 32 < 6` unsigned). Returns the new options.
pub fn refresh_briefing_option(m: &mut Machine) -> u32 {
    let ctx = context(m);
    let mission = u32::from(m.lhu(ctx + 0x390));
    let options = m.lw(ctx + 0x970);
    let options = if mission.wrapping_sub(0x20) < 6 {
        options | 0x80
    } else {
        options & !0x80
    };
    m.sw(ctx + 0x970, options);
    options
}

/// 800CB408: call `table[state][substate]` (0x800F1DD4, signed halfword
/// indexes, a0 = scene block) and route its exact result: 1 advances the
/// substate; 0x100 follows the next route (0x800F1E14 and the choice
/// switches of states 0, 4, 12, 9, 13/14, 6); 0x200 follows the back route
/// (0x800F1E24); 0x10/0x20/0x80 set progress bits (0x20/4/8) and return the
/// resident transitions 0x60000009/0x60000004/0x60000002 (the first two
/// after stopping every voice, 8002D9B8; 0x10 also sets +0x3A4 = 3). Any
/// other result returns 0 with the state unchanged.
pub fn mission_dispatch(m: &mut Machine) -> u32 {
    framed(m, 0x28, |m, _| {
        let block = scene(m);
        let state = sext16(m.lh(block));
        let row = m.lw(STATE_TABLE.wrapping_add(state << 2));
        let substate = sext16(m.lh(block + 2));
        let callback = m.lw(row.wrapping_add(substate << 2));
        // The callbacks read no argument registers (a0 is the scene block).
        let result = call(m, callback, [block, 0, 0, 0]);
        match result {
            0x20 => {
                let ctx = context(m);
                let progress = m.lw(ctx + 0x964);
                m.sw(ctx + 0x964, progress | 4);
                call(m, 0x8002_d9b8, [0; 4]);
                0x6000_0004
            }
            1 => {
                let block = scene(m);
                let substate = m.lhu(block + 2);
                m.sh(block + 2, substate.wrapping_add(1));
                0
            }
            0x10 => {
                let ctx = context(m);
                let progress = m.lw(ctx + 0x964);
                m.sw(ctx + 0x3a4, 3);
                m.sw(ctx + 0x964, progress | 0x20);
                call(m, 0x8002_d9b8, [0; 4]);
                0x6000_0009
            }
            0x100 => next_route(m),
            0x80 => {
                let ctx = context(m);
                let progress = m.lw(ctx + 0x964);
                m.sw(ctx + 0x964, progress | 8);
                0x6000_0002
            }
            0x200 => {
                back_route(m);
                0
            }
            _ => 0,
        }
    })
}

/// 800CB4E8..800CB7A0: the 0x100 route of [`mission_dispatch`]. Returns
/// 0, or 0x60000002 for inner state 12 with briefing bypass (options 0x80,
/// neither 8 nor 0x10), which also sets progress bit 8 (800CB828).
fn next_route(m: &mut Machine) -> u32 {
    let block = scene(m);
    let state = m.lh(block);
    let route = u32::from(m.lbu(NEXT_ROUTE.wrapping_add(sext16(state))));
    let raw = m.lhu(block);
    if route & 0x80 != 0 {
        m.sh(block + 4, raw);
    }
    let block = scene(m);
    m.sh(block + 2, 0);
    let state = m.lh(block);
    let route = route & 0x7f;
    if state == 5 {
        refresh_briefing_option(m);
    }
    let block = scene(m);
    let state = m.lh(block);
    let raw = m.lhu(block);
    let set = |m: &mut Machine, value: u16| {
        let block = scene(m);
        m.sh(block, value);
    };
    match state {
        0 => match m.lh(block + 6) {
            0 => {
                if context_word(m, 0x970) & 0x80 != 0 {
                    m.sh(block, 2);
                } else {
                    set(m, 1);
                }
            }
            1 => m.sh(block, 7),
            2 => m.sh(block, 9),
            3 => set(m, 0xa),
            _ => {}
        },
        4 => match m.lh(block + 6) {
            0 => set(m, 5),
            1 => m.sh(block, 8),
            2 => set(m, 9),
            3 => set(m, 0xa),
            _ => {}
        },
        12 => {
            refresh_briefing_option(m);
            let ctx = context(m);
            let options = m.lw(ctx + 0x970);
            if options & 8 != 0 {
                set(m, 9);
            } else if options & 0x10 != 0 {
                set(m, 4);
            } else if options & 0x80 != 0 {
                let progress = m.lw(ctx + 0x964);
                m.sw(ctx + 0x964, progress | 8);
                return 0x6000_0002;
            } else {
                set(m, 0);
            }
        }
        9 => {
            let ctx = context(m);
            let options = m.lw(ctx + 0x970);
            if options & 8 != 0 {
                m.sw(ctx + 0x970, options & !8);
            }
            let ctx = context(m);
            if m.lw(ctx + 0x970) & 0x10 != 0 {
                set(m, 4);
            } else {
                set(m, 0);
            }
        }
        _ if raw.wrapping_sub(0xd) < 2 => {
            if context_word(m, 0x970) & 0x10 != 0 {
                m.sh(block, 4);
            } else {
                set(m, 0);
            }
        }
        _ => {
            let block = scene(m);
            if m.lh(block) == 6 {
                if context_word(m, 0x970) & 0x80 != 0 {
                    m.sh(block, 2);
                } else {
                    m.sh(block, 1);
                }
            } else if route != 0x7f {
                m.sh(block, route as u16);
            }
        }
    }
    0
}

/// 800CB7A4..800CB818: the 0x200 route of [`mission_dispatch`].
fn back_route(m: &mut Machine) {
    let block = scene(m);
    let state = m.lh(block);
    m.sh(block + 2, 0);
    let entry = BACK_ROUTE.wrapping_add(sext16(state));
    let route = m.lbu(entry);
    let previous = |m: &mut Machine| {
        let value = m.lhu(block + 4);
        m.sh(block, value);
    };
    match route {
        1 => {
            if context_word(m, 0x970) & 0x80 != 0 {
                previous(m);
            } else {
                m.sh(block, 1);
            }
        }
        0x7e => previous(m),
        0x7f => {}
        _ => {
            let value = m.lbu(entry);
            m.sh(block, u16::from(value));
        }
    }
}

// ---- 800CBAB8: ordering tables --------------------------------------------------------

/// 800CBAB8: select the scene ordering table (+0x10 + buffer*0x40) and
/// packet area (+0x94 + buffer*0x10000) of the display buffer (context +4),
/// clear the table (ClearOTagR, 16 entries), chain it into the resident
/// table entry `[context+8]` (entry 15 first), then prepare the visual
/// ordering table under scene entry 13 (800F1B00, whose v0 it returns).
pub fn prepare_frame(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        let ctx = context(m);
        let buffer = m.lw(ctx + 4);
        let block = scene(m);
        let ot = block.wrapping_add((buffer << 6).wrapping_add(0x10));
        m.sw(block + 0x90, ot);
        let buffer = m.lw(ctx + 4);
        let packets = block.wrapping_add((buffer << 16).wrapping_add(0x94));
        m.sw(block.wrapping_add(0x20094), packets);
        let ot = m.lw(block + 0x90);
        sdk::clear_otag_r(m, ot, 0x10);
        let block = scene(m);
        let ctx = context(m);
        let ot = m.lw(block + 0x90);
        let resident = m.lw(ctx + 8);
        let link = m.lw(resident);
        link_word(m, ot, link);
        let ot = m.lw(block + 0x90);
        let resident = m.lw(ctx + 8);
        link_word(m, resident, ot.wrapping_add(0x3c));
        let ot = m.lw(block + 0x90);
        visual::prepare_ot(m, ot.wrapping_add(0x34))
    })
}

// ---- 800D0C2C..800D0CC4: stream requests ------------------------------------------------

/// 800D0C2C: clear the stream block flags and its request record
/// (+0x1004..+0x100F). Returns the request record's address.
pub fn reset_stream(m: &mut Machine) -> u32 {
    let stream = m.lw(STREAM);
    m.sw(stream, 0);
    m.sh(stream + 0x1004, 0);
    let request = stream + 0x1004;
    m.sh(request + 2, 0);
    m.sh(request + 4, 0);
    m.sh(request + 6, 0);
    m.sw(request + 8, 0);
    request
}
/// 800D0C58: the stream request's resource (+0x1004). Returns the stream
/// block.
pub fn set_stream(m: &mut Machine, resource: u32) -> u32 {
    let stream = m.lw(STREAM);
    m.sh(stream + 0x1004, resource as u16);
    stream
}
/// 800D0C68: request the stream (flag bit 0). Returns the new flags.
pub fn start_stream(m: &mut Machine) -> u32 {
    let stream = m.lw(STREAM);
    let flags = m.lw(stream) | 1;
    m.sw(stream, flags);
    flags
}
/// 800D0C88: clear the request flag and stop streaming (8002C280(0, 0));
/// returns that routine's v0 (callers ignore it).
pub fn stop_stream(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let stream = m.lw(STREAM);
        let flags = m.lw(stream);
        m.sw(stream, flags & !1);
        call(m, 0x8002_c280, [0, 0, 0, 0])
    })
}
/// 800D0CC4: while a stream is requested, when 8002C730(resource) reports
/// 1 and the CD is idle (8002C158(1) == 0), start it (8002C4B0(request, 1,
/// 0)). Returns 0 without a request, else the last call's v0.
pub fn update_stream(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let stream = m.lw(STREAM);
        if m.lw(stream) & 1 == 0 {
            return 0;
        }
        let request = stream + 0x1004;
        let resource = sext16(m.lh(stream + 0x1004));
        let ready = call(m, 0x8002_c730, [resource, 0, 0, 0]);
        if ready != 1 {
            return ready;
        }
        let busy = call(m, 0x8002_c158, [1, 0, 0, 0]);
        if busy != 0 {
            return busy;
        }
        call(m, 0x8002_c4b0, [request, 1, 0, 0])
    })
}

// ---- 800D169C: hub backdrop -------------------------------------------------------------

/// 800D169C: initial backdrop wedges: flags 0, four angles (+0x14..+0x1A:
/// 0x30, 0x56, 0x7C, 0xA2), radii pairs (+0x1C..+0x22: 0x83/0x58 twice),
/// speeds +0xC.. = 0xE00 and +4.. = 0 for the four wedges. Its v0 is the
/// loop's exit test, 0 (the dispatch arm returns it).
pub fn reset_backdrop(m: &mut Machine) {
    let block = m.lw(BACKDROP);
    m.sh(block + 0x14, 0x30);
    m.sh(block + 0x16, 0x56);
    m.sh(block + 0x18, 0x7c);
    m.sw(block, 0);
    m.sh(block + 0x1c, 0x83);
    m.sh(block + 0x1e, 0x58);
    m.sh(block + 0x20, 0x83);
    m.sh(block + 0x1a, 0xa2);
    m.sh(block + 0x22, 0x58);
    for wedge in 0..4u32 {
        m.sh(block + 0xc + wedge * 2, 0xe00);
        m.sh(block + 4 + wedge * 2, 0);
    }
}

// ---- 800E79D4 / 800E7A18 / 800E9824..800E9854: scene color and background ------------------

/// 800E79D4: display background RGB bytes and display flag 8. Returns the
/// new display flags.
pub fn set_scene_color(m: &mut Machine, red: u32, green: u32, blue: u32) -> u32 {
    let display = m.gw(DISPLAY);
    m.sb(display + 0x10, red as u8);
    let display = m.gw(DISPLAY);
    m.sb(display + 0x11, green as u8);
    let display = m.gw(DISPLAY);
    m.sb(display + 0x12, blue as u8);
    let display = m.gw(DISPLAY);
    let flags = m.lw(display) | 8;
    m.sw(display, flags);
    flags
}
/// 800E7A18: fade the five background voices (map +0x110..+0x120) to
/// `level` over `ticks` (8002DB18). Returns the last call's v0.
pub fn fade_background_voices(m: &mut Machine, level: u32, ticks: u32) -> u32 {
    framed(m, 0x20, |m, _| {
        let mut v0 = 0;
        for offset in [0x110, 0x114, 0x118, 0x11c, 0x120] {
            let block = m.lw(MAP);
            let voice = m.lw(block + offset);
            v0 = call(m, 0x8002_db18, [voice, level, ticks, 0]);
        }
        v0
    })
}
/// 800E9824 / 800E983C: map mode word +0x18 (1 reveal, 3 hide) and
/// +0x24 = 0 (800E9854, the same with a mode argument, is only reached
/// from inner state 10). Returns the mode.
pub fn set_map_mode(m: &mut Machine, mode: u32) -> u32 {
    let block = m.lw(MAP);
    m.sw(block + 0x18, mode);
    m.sw(block + 0x24, 0);
    mode
}

// ---- 800EF01C / 800DA65C: sub-mode wrappers ------------------------------------------------

/// 800EF01C (inner state 1 update): briefing wrapper. Stores the visual
/// ordering table (800F1BA0) at briefing +0x10, calls the phase callback
/// `[0x800F68B8][phase][substate]` (signed halfwords +4/+6) and routes its
/// flags in priority 2 (next substate), 1 (next phase), 4 (accept: 800E9824,
/// stop stream, 0x100), 8 (cancel: stop stream, voice fade 800E7A18(100, 10),
/// 0x200), 0x10 (phase 3). Unless it accepted or cancelled it runs the
/// briefing drawing: 800EB654, 800EB9E8, 800EBC14, the eight labels
/// (800EB754, skipped while pressed bit 0x40), 800ED834 and 800ED8B0 into
/// the scene packet area, and returns 0.
pub fn briefing_wrapper(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        let ot = visual::current_ot(m);
        let block = m.lw(BRIEFING);
        m.sw(block + 0x10, ot);
        let phase = sext16(m.lh(block + 4));
        let row = m.lw(BRIEFING_PHASES.wrapping_add(phase << 2));
        let substate = sext16(m.lh(block + 6));
        let callback = m.lw(row.wrapping_add(substate << 2));
        let result = call(m, callback, [block, 0, 0, 0]);
        if result & 2 != 0 {
            let block = m.lw(BRIEFING);
            let substate = m.lhu(block + 6);
            m.sh(block + 6, substate.wrapping_add(1));
        } else if result & 1 != 0 {
            let block = m.lw(BRIEFING);
            let phase = m.lhu(block + 4);
            m.sh(block + 6, 0);
            m.sh(block + 4, phase.wrapping_add(1));
        } else if result & 4 != 0 {
            set_map_mode(m, 1);
            stop_stream(m);
            return 0x100;
        } else if result & 8 != 0 {
            stop_stream(m);
            fade_background_voices(m, 0x64, 0xa);
            return 0x200;
        } else if result & 0x10 != 0 {
            let block = m.lw(BRIEFING);
            m.sh(block + 4, 3);
            m.sh(block + 6, 0);
        }
        call(m, 0x800e_b654, [0; 4]);
        call(m, 0x800e_b9e8, [0; 4]);
        call(m, 0x800e_bc14, [0; 4]);
        if context_word(m, 0x12c) & 0x40 == 0 {
            for label in 0..8u32 {
                let block = m.lw(BRIEFING);
                call(m, 0x800e_b754, [block + 0x4a4 + label * 0xc, 0, 0, 0]);
            }
        }
        let block = scene(m);
        let packet = m.lw(block.wrapping_add(0x20094));
        let ot = m.lw(block + 0x90);
        let packet = call(m, 0x800e_d834, [packet, ot, 0, 0]);
        let briefing = m.lw(BRIEFING);
        set_packet(m, packet);
        let ot = m.lw(briefing + 0x10);
        let packet = call(m, 0x800e_d8b0, [packet, ot.wrapping_add(0x14), 0, 0]);
        set_packet(m, packet);
        0
    })
}

/// 800DA65C (inner state 2 update): aircraft selection wrapper. Calls the
/// phase callback `[0x800F6018][phase][substate]` (words +0/+4) and routes
/// its flags in priority 1 (next substate), 2 (next phase), 8 (previous
/// phase), 4 (accept: stop stream, 0x100), 0x10 (cancel: stop stream,
/// 0x200). Otherwise runs the aircraft records (800D7950), the selection
/// drawing (800D8CB4 into the scene packet area), the texture slide
/// (800D8D34), the weapon records (800D7C54) and slide (800D7DC8), and
/// returns 0.
pub fn aircraft_wrapper(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        let block = m.lw(AIRCRAFT);
        let phase = m.lw(block);
        let row = m.lw(AIRCRAFT_PHASES.wrapping_add(phase << 2));
        let substate = m.lw(block + 4);
        let callback = m.lw(row.wrapping_add(substate << 2));
        let result = call(m, callback, [block, 0, 0, 0]);
        if result & 1 != 0 {
            let block = m.lw(AIRCRAFT);
            let substate = m.lw(block + 4);
            m.sw(block + 4, substate.wrapping_add(1));
        } else if result & 2 != 0 {
            let block = m.lw(AIRCRAFT);
            let phase = m.lw(block);
            m.sw(block + 4, 0);
            m.sw(block, phase.wrapping_add(1));
        } else if result & 8 != 0 {
            let block = m.lw(AIRCRAFT);
            let phase = m.lw(block);
            m.sw(block + 4, 0);
            m.sw(block, phase.wrapping_sub(1));
        } else if result & 4 != 0 {
            stop_stream(m);
            return 0x100;
        } else if result & 0x10 != 0 {
            stop_stream(m);
            return 0x200;
        }
        call(m, 0x800d_7950, [0; 4]);
        let block = scene(m);
        let packet = m.lw(block.wrapping_add(0x20094));
        let ot = m.lw(block + 0x90);
        let packet = call(m, 0x800d_8cb4, [packet, ot, 0, 0]);
        set_packet(m, packet);
        call(m, 0x800d_8d34, [0; 4]);
        call(m, 0x800d_7c54, [0; 4]);
        call(m, 0x800d_7dc8, [0; 4]);
        0
    })
}

// ---- dispatch -----------------------------------------------------------------------------

/// Overlay-2 core routines by original address (a0..a3 in, v0 out: every
/// routine returns the v0 the original leaves, a callee's v0 included).
/// Claims nothing unless overlay module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    let [a0, a1, a2, a3] = args;
    Some(match target {
        0x800c_b3bc => refresh_briefing_option(m),
        0x800c_b408 => mission_dispatch(m),
        0x800c_b8b4 => initialize(m),
        0x800c_bab8 => prepare_frame(m),
        0x800c_bb88 => update(m),
        0x800c_bc70 => substate_2(),
        0x800c_bc78 => substate_3(m),
        0x800d_0c2c => reset_stream(m),
        0x800d_0c58 => set_stream(m, a0),
        0x800d_0c68 => start_stream(m),
        0x800d_0c88 => stop_stream(m),
        0x800d_0cc4 => update_stream(m),
        0x800d_169c => {
            reset_backdrop(m);
            0
        }
        0x800e_79d4 => set_scene_color(m, a0, a1, a2),
        0x800e_7a18 => fade_background_voices(m, a0, a1),
        0x800e_9824 => set_map_mode(m, 1),
        0x800e_983c => set_map_mode(m, 3),
        0x800e_f01c => briefing_wrapper(m),
        0x800d_a65c => aircraft_wrapper(m),
        _ => {
            return loader::dispatch(m, target, args)
                .or_else(|| text::dispatch(m, target, args))
                .or_else(|| map::dispatch(m, target, args))
                .or_else(|| visual::dispatch(m, target, [a0, a1, a2, a3]));
        }
    })
}
