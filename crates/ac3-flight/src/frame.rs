//! Flight frame control: state callback 80033C58, frame body 80033748,
//! regular update 80032CA4 and frame tail 80033B28, and the other resident
//! state callbacks of table 800AEE50 (80033D68 sequence, 80033DA8 replay,
//! 80033ED8 mission result, 80033FB4, 80033FEC, 80034024, 80034064 pause,
//! 80035E78/80035EF8 aircraft loading) with their overlays and helpers.
//!
//! Subsystems owned by other modules are called by original address through
//! `dispatch::call`, which every module answers for the routines it
//! translates; an unclaimed address faults explicitly.
use crate::{
    common::{CONTEXT, GAMEPLAY, RESOURCES, request_transition},
    dispatch,
    machine::{Machine, SmallData},
    sdk,
};

/// `[0x800BAF20]`: flight state block read and updated by 80032CA4.
const FLIGHT_STATE_POINTER: u32 = 0x800b_af20;
/// `[gp+0x2D4]`: per-buffer drawing environment table (stride 0x1014).
const DRAW_BUFFERS: u32 = 0x2d4;
const OT_ENTRIES: u32 = 0x1000;
const LOW24: u32 = 0x00ff_ffff;
const HIGH8: u32 = 0xff00_0000;
const UPDATE_REGULAR: u32 = 0x8003_2ca4;

fn call(m: &mut Machine, address: u32, args: [u32; 4]) -> u32 {
    dispatch::call(m, address, args)
}

/// Link `packet` (len `words`) at ordering-table slot `slot`, as the
/// original's inline addPrim: packet tag keeps the slot's link, slot points
/// at the packet.
fn add_prim(m: &mut Machine, slot: u32, packet: u32) {
    let tag = m.lw(packet);
    let link = m.lw(slot);
    m.sw(packet, (tag & HIGH8) | (link & LOW24));
    let link = m.lw(slot);
    m.sw(slot, (link & HIGH8) | (packet & LOW24));
}

/// Emit a two-word DR_OFFSET packet (E5 + NOP) at `packet` whose tag is the
/// slot's current word with length 2, and make the slot point at it.
fn draw_offset_packet(m: &mut Machine, slot: u32, packet: u32, x: i16, y: i16) {
    // The original ORs the sign-extended halfwords: a negative offset sets
    // high bits, exactly as `or` of the 32-bit registers does.
    let command = ((y as i32 as u32) << 11) | (x as i32 as u32) | 0xe500_0000;
    m.sw(packet + 4, command);
    m.sw(packet + 8, 0);
    let link = m.lw(slot);
    m.sw(packet, link | 0x0200_0000);
    m.sw(slot, (packet << 8) >> 8);
}

/// 80033C58: regular flight state callback (resident state 2, substate 7).
pub fn flight_callback(m: &mut Machine) -> u32 {
    // Original frames (0x18 here, 0x20 in 80033748) keep nested guest
    // frames at their original addresses; 8005CFB4 stores stack-buffer
    // addresses in the scratchpad, so they are observable.
    m.push_frame(0x18);
    let gameplay = m.gw(GAMEPLAY);
    let flags = m.lw(gameplay);
    m.sw(gameplay, flags | 2);
    frame(m, UPDATE_REGULAR);
    let context = m.gw(CONTEXT);
    let flags = m.lw(context);
    if flags & 0x2000_0000 != 0 {
        call(m, 0x8003_4428, [0; 4]);
        let context = m.gw(CONTEXT);
        let gameplay = m.gw(GAMEPLAY);
        let a0 = m.lw(context + 0xc);
        let a1 = m.lw(gameplay + 0x14);
        let primitives = call(m, 0x8003_4640, [a0, a1, 0, 0]);
        let gameplay = m.gw(GAMEPLAY);
        m.sw(gameplay + 0x14, primitives);
        let context = m.gw(CONTEXT);
        if m.lw(context) & 0x0800_0000 != 0 {
            call(m, 0x8002_c2cc, [0, 0, 0, 0]);
        }
    } else if !flags & 0x1000_0000 != 0 {
        let buttons = m.lhu(context + 0x156);
        let mut pause = false;
        if buttons & 1 != 0 {
            m.sh(context + 0x156, buttons & 0xfffe);
            pause = true;
        }
        let context = m.gw(CONTEXT);
        if m.lw(context + 0x12c) & 0x40 != 0 {
            pause = true;
        }
        if pause {
            let flags = m.lw(context);
            m.sw(context, flags | 0x0400_0000);
            call(m, 0x8002_c2cc, [1, 0, 0, 0]);
        }
    }
    // The original returns 80033B28's v0: the transition code of a phase
    // change (leaving the mission from the pause menu, for one).
    let v0 = frame_tail(m);
    m.pop_frame(0x18);
    v0
}

/// 80033748: one flight frame. `update` is the update routine (80032CA4 or
/// a mode variant), called with a0 = gameplay, a1 = `[context+8]`,
/// a2 = context as the original leaves them.
pub fn frame(m: &mut Machine, update: u32) {
    m.push_frame(0x20);
    let context = m.gw(CONTEXT);
    let gameplay = m.gw(GAMEPLAY);
    let buffer = m.lw(context + 4);
    let slot = gameplay + (buffer << 2);
    let ot = m.lw(slot + 0x18);
    m.sw(gameplay + 0x10, ot);
    let primitives = m.lw(slot + 0x20);
    m.sw(gameplay + 0x14, primitives);
    sdk::clear_otag_r(m, ot, OT_ENTRIES);

    let gameplay = m.gw(GAMEPLAY);
    let context = m.gw(CONTEXT);
    // Chain the previous table: ot[0] continues at *[context+8], which then
    // continues at this table's last entry.
    let ot = m.lw(gameplay + 0x10);
    let chain = m.lw(context + 8);
    let head = m.lw(ot);
    let next = m.lw(chain);
    m.sw(ot, (head & HIGH8) | (next & LOW24));
    let last = m.lw(gameplay + 0x10).wrapping_add(0x3ffc);
    let chain = m.lw(context + 8);
    let word = m.lw(chain);
    m.sw(chain, (word & HIGH8) | (last & LOW24));
    for offset in [0x28c, 0x28e, 0x290, 0x292] {
        m.sh(gameplay + offset, 0);
    }
    call(m, update, [gameplay, chain, context, 0]);

    let gameplay = m.gw(GAMEPLAY);
    let camera = m.lw(gameplay + 0x28);
    call(m, 0x8007_9da4, [camera, 0, 0, 0]);
    let gameplay = m.gw(GAMEPLAY);
    let camera = m.lw(gameplay + 0x28);
    call(
        m,
        0x8003_304c,
        [camera, gameplay + 0x2c, gameplay + 0x4c, 0],
    );

    // Screen offsets of the current drawing environment and full-width
    // letterbox tiles for the two bar heights at gameplay+0x290/+0x292.
    let context = m.gw(CONTEXT);
    let buffer = m.lw(context + 4);
    let gameplay = m.gw(GAMEPLAY);
    let table = m.gw(DRAW_BUFFERS).wrapping_add(buffer.wrapping_mul(0x1014));
    let env = m.lw(table);
    let mut packet = m.lw(gameplay + 0x14);
    let ofs_x = m.lh(env + 8);
    let ofs_y = m.lh(env + 0xa);
    let width = m.lhu(env + 4);
    let height = m.lhu(env + 6);
    let shake = [0x28c, 0x28e, 0x290, 0x292].map(|offset| m.lhu(gameplay + offset));
    m.sh(gameplay + 0x294, shake[0].wrapping_add(ofs_x as u16));
    m.sh(gameplay + 0x296, shake[1].wrapping_add(ofs_y as u16));
    let center_x = (ofs_x as i32 + ((u32::from(width) << 16) as i32 >> 17)) as u16;
    let center_y = (ofs_y as i32 + ((u32::from(height) << 16) as i32 >> 17)) as u16;
    m.sh(gameplay + 0x29a, shake[3].wrapping_add(center_y));
    m.sh(gameplay + 0x298, shake[2].wrapping_add(center_x));
    let full_width = ((u32::from(width) << 16) as i32 >> 16) as u16;
    for (index, offset) in [0x290_u32, 0x292].into_iter().enumerate() {
        let gameplay = m.gw(GAMEPLAY);
        if m.lh(gameplay + offset) == 0 {
            continue;
        }
        m.sb(packet + 3, 3);
        m.sb(packet + 7, 0x60);
        for byte in 4..7 {
            m.sb(packet + byte, 0);
        }
        let gameplay = m.gw(GAMEPLAY);
        m.sh(packet + 8, 0);
        m.sh(packet + 0xa, 0);
        if index == 0 {
            let bar = m.lhu(gameplay + 0x290);
            m.sh(packet + 0xe, 0x100);
            m.sh(packet + 0xc, bar);
        } else {
            m.sh(packet + 0xc, full_width);
            let bar = m.lhu(gameplay + 0x292);
            m.sh(packet + 0xe, bar);
        }
        let ot = m.lw(gameplay + 0x10);
        add_prim(m, ot, packet);
        packet += 0x10;
    }

    let gameplay = m.gw(GAMEPLAY);
    m.sw(gameplay + 0x14, packet);
    let ot = m.lw(gameplay + 0x10);
    draw_offset_packet(m, ot, packet, ofs_x, ofs_y);
    let gameplay = m.gw(GAMEPLAY);
    let x = m.lh(gameplay + 0x294);
    let y = m.lh(gameplay + 0x296);
    let second = packet + 0xc;
    m.sw(gameplay + 0x14, second);
    let ot = m.lw(gameplay + 0x10);
    draw_offset_packet(m, ot + 4, second, x, y);
    m.sw(gameplay + 0x14, packet + 0x18);
    sdk::set_geom_offset(m, 0, 0);

    // Widescreen (host-side, off by default): the world drawers project
    // squeezed; the update before and the HUD after keep the 4:3 projection.
    crate::widescreen::world_begin(m);
    for routine in [0x8003_308c, 0x8003_3140, 0x8003_3194, 0x8003_31d4] {
        call(m, routine, [0; 4]);
    }

    let gameplay = m.gw(GAMEPLAY);
    let packet = m.lw(gameplay + 0x14);
    let x = m.lh(gameplay + 0x298);
    let y = m.lh(gameplay + 0x29a);
    let ot = m.lw(gameplay + 0x10);
    draw_offset_packet(m, ot + 0x3ffc, packet, x, y);
    m.sw(gameplay + 0x14, packet + 0xc);
    crate::widescreen::world_end(m);
    sdk::set_geom_offset(m, 0xa0, 0x80);

    for routine in [0x8003_9d84, 0x8003_3524, 0x8003_3544] {
        call(m, routine, [0; 4]);
    }
    let gameplay = m.gw(GAMEPLAY);
    if !m.lw(gameplay) & 1 != 0 {
        call(m, 0x8003_357c, [0; 4]);
    }
    m.pop_frame(0x20);
}

/// 80032CA4: regular flight update, unless a resident transition is pending.
pub fn update(m: &mut Machine) {
    let context = m.gw(CONTEXT);
    if !m.lw(context) & 0x2000_0000 == 0 {
        return;
    }
    let gameplay = m.gw(GAMEPLAY);
    let camera = m.lw(gameplay + 0x28);
    for routine in [
        0x8007_1358,
        0x8004_4d50,
        0x8003_8728,
        0x8005_01c8,
        0x8004_5950,
        0x8006_ade4,
        0x8007_c44c,
        0x8008_3a64,
    ] {
        call(m, routine, [0; 4]);
    }
    let frame = m.push_frame(0x30);
    let state = m.lw(FLIGHT_STATE_POINTER);
    for (offset, source) in [(0x10, 0x1c), (0x14, 8), (0x18, 0xc), (0x1c, 0x10)] {
        let value = m.lw(state + source);
        m.sw(frame + offset, value);
    }
    call(m, 0x8003_9d08, [frame + 0x10, 0, 0, 0]);
    let state = m.lw(FLIGHT_STATE_POINTER);
    let target = m.lw(frame + 0x24);
    if m.lw(state + 0x18) != target {
        let gameplay = m.gw(GAMEPLAY);
        if m.lw(gameplay) & 2 != 0 {
            call(m, 0x8007_d214, [0; 4]);
        }
    }
    let value = m.lw(frame + 0x20);
    let state = m.lw(FLIGHT_STATE_POINTER);
    m.sw(camera + 0xbc, value);
    let target = m.lw(frame + 0x24);
    m.sw(state + 0x18, target);
    m.pop_frame(0x30);
    let gameplay = m.gw(GAMEPLAY);
    if m.lw(gameplay) & 2 != 0 {
        call(m, 0x8007_da7c, [0; 4]);
    }
}

/// 80033B28: per-frame tail. Steps 80036E54 unless replaying (context
/// +0x3A9), counts frames while no transition is pending, turns gameplay
/// request bits into resident transitions and applies a pending one through
/// 80031E3C. Returns the original v0 (0 or 80031E3C's result).
pub fn frame_tail(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let result = frame_tail_body(m);
    m.pop_frame(0x18);
    result
}

fn frame_tail_body(m: &mut Machine) -> u32 {
    let context = m.gw(CONTEXT);
    if m.lbu(context + 0x3a9) == 0 {
        call(m, 0x8003_6e54, [0; 4]);
    }
    let context = m.gw(CONTEXT);
    if !m.lw(context) & 0x2000_0000 != 0 {
        let gameplay = m.gw(GAMEPLAY);
        let frames = m.lw(gameplay + 0xc);
        m.sw(gameplay + 0xc, frames.wrapping_add(1));
    }
    let gameplay = m.gw(GAMEPLAY);
    let flags = m.lw(gameplay);
    if flags & 0x4000_0000 != 0 {
        m.sw(gameplay, flags & 0xbfff_ffff);
        request_transition(m, 0x1000_0000, 0);
    } else if (flags as i32) < 0 {
        let context = m.gw(CONTEXT);
        m.sb(context + 0x95b, 1);
        let gameplay = m.gw(GAMEPLAY);
        let flags = m.lw(gameplay);
        m.sw(gameplay, flags & 0x7fff_ffff);
        request_transition(m, 0x0080_0000, 0);
    }
    let gameplay = m.gw(GAMEPLAY);
    let flags = m.lw(gameplay);
    if flags & 0x2000_0000 == 0 {
        return 0;
    }
    let resources = m.gw(RESOURCES);
    let a0 = m.lw(gameplay + 4);
    let a1 = m.lw(gameplay + 8);
    m.sw(gameplay, flags & 0xdfff_ffff);
    let first = m.lw(resources + 0x70);
    m.sw(gameplay + 4, first);
    let second = m.lw(resources + 0x74);
    m.sw(gameplay + 0xc, 0);
    m.sw(gameplay + 8, second);
    call(m, 0x8003_1e3c, [a0, a1, 0, 0])
}

// ==== the other resident state callbacks (table 800AEE50) ==================================

/// 80010498: the 1.14 sine table.
const SINE: u32 = 0x8001_0498;
/// `[gp+0x150]`: the loading-screen state.
const LOADING: u32 = 0x150;

/// `slt`: signed less-than.
fn lt(a: u32, b: u32) -> bool {
    (a as i32) < (b as i32)
}
/// `sra`: arithmetic right shift by the low five bits.
fn sra(value: u32, shift: u32) -> u32 {
    ((value as i32) >> (shift & 31)) as u32
}
/// Clear gameplay word 0 bit 1 (the states other than 80033C58 run
/// without it).
fn clear_flight_bit(m: &mut Machine) {
    let gameplay = m.gw(GAMEPLAY);
    let flags = m.lw(gameplay) & !2;
    m.sw(gameplay, flags);
}
/// Store the caller's stack arguments +0x10.. of the next call.
fn stack_words(m: &mut Machine, frame: u32, words: &[u32]) {
    for (index, &word) in words.iter().enumerate() {
        m.sw(frame + 0x10 + index as u32 * 4, word);
    }
}
/// 800353D8 (`loading`): a coloured text sprite; stack words +0x10.. are
/// the colour, the mode and the text index.
fn text_sprite(m: &mut Machine, frame: u32, args: [u32; 4], words: [u32; 3]) -> u32 {
    stack_words(m, frame, &words);
    call(m, 0x8003_53d8, args)
}
/// 800343D0 (`loading`): a draw-mode packet.
fn draw_mode(m: &mut Machine, ot: u32, prim: u32, mode: u32) -> u32 {
    call(m, 0x8003_43d0, [ot, prim, mode, 0])
}
/// The OT and primitive cursor of a state's screen overlay: context +0xC,
/// gameplay +0x14.
fn overlay_cursors(m: &mut Machine) -> (u32, u32) {
    let context = m.gw(CONTEXT);
    let gameplay = m.gw(GAMEPLAY);
    (m.lw(context + 0xc), m.lw(gameplay + 0x14))
}
/// Store the new primitive cursor at gameplay +0x14.
fn set_primitives(m: &mut Machine, primitives: u32) {
    let gameplay = m.gw(GAMEPLAY);
    m.sw(gameplay + 0x14, primitives);
}

/// 80033D68: state 4 (phase 0x10): the frame with update 80032DE4.
pub fn sequence_state(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    clear_flight_bit(m);
    frame(m, 0x8003_2de4);
    let v0 = frame_tail(m);
    m.pop_frame(0x18);
    v0
}

/// 80033DA8: state 5 (phase 0x80, the replay): the frame with update
/// 80032E34 and its overlay: with context +0x3A9 set, the fade bar
/// 80035080 (modes 1/2) and on the replay's end (gameplay bit 28) the
/// return to the menu (80030248, 8002C280; result 0x60000000 | context
/// +0x3A4 when the replay was cut, else 0x50000000, without the frame
/// tail); otherwise the replay caption 80034E84 unless context +0x3A2.
pub fn replay_state(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    clear_flight_bit(m);
    frame(m, 0x8003_2e34);
    let context = m.gw(CONTEXT);
    let mode = m.lbu(context + 0x3a9);
    let v0 = 'tail: {
        if mode == 0 {
            if m.lbu(context + 0x3a2) == 0 {
                let (ot, prim) = overlay_cursors(m);
                let prim = replay_caption(m, ot, prim);
                set_primitives(m, prim);
            }
        } else {
            if mode < 3 {
                let (ot, prim) = overlay_cursors(m);
                let prim = fade_bar(m, ot, prim, 0x1000);
                set_primitives(m, prim);
            }
            let gameplay = m.gw(GAMEPLAY);
            if m.lw(gameplay) & 0x1000_0000 != 0 {
                call(m, 0x8003_0248, [0; 4]);
                call(m, 0x8002_c280, [0, 0, 0, 0]);
                let context = m.gw(CONTEXT);
                let cut = m.lbu(context + 0x3a9) == 1 || m.lw(context + 0x128) & 0x10 != 0;
                if cut {
                    m.sb(context + 0x3a9, 0);
                    let context = m.gw(CONTEXT);
                    break 'tail m.lw(context + 0x3a4) | 0x6000_0000;
                }
                break 'tail 0x5000_0000;
            }
        }
        frame_tail(m)
    };
    m.pop_frame(0x18);
    v0
}

/// 80033ED8: state 6 (phase 0x400, the mission result): the frame with
/// update 80032E6C; with gameplay +4 bit 0x1000 the result overlay
/// (80034B78, the blinking caption 80034DF4, context bit 25) and on a
/// confirm (gameplay bit 5) the transition to the next resident (8002D9B8,
/// 80028F5C, 80032C80(0x400, gameplay +8)) with the player reset 8006B8A8.
pub fn result_state(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    clear_flight_bit(m);
    frame(m, 0x8003_2e6c);
    let gameplay = m.gw(GAMEPLAY);
    if m.lw(gameplay + 4) & 0x1000 != 0 {
        result_flags(m);
        let (ot, prim) = overlay_cursors(m);
        let prim = result_caption(m, ot, prim);
        let gameplay = m.gw(GAMEPLAY);
        let context = m.gw(CONTEXT);
        m.sw(gameplay + 0x14, prim);
        let flags = m.lw(context) | 0x0200_0000;
        m.sw(context, flags);
        let flags = m.lw(gameplay);
        if flags & 0x20 != 0 {
            m.sw(gameplay, flags & !0x20);
            call(m, 0x8002_d9b8, [0; 4]);
            call(m, 0x8002_8f5c, [0; 4]);
            let gameplay = m.gw(GAMEPLAY);
            let next = m.lw(gameplay + 8);
            request_transition(m, 0x400, next);
            reset_player(m);
        }
    }
    let v0 = frame_tail(m);
    m.pop_frame(0x18);
    v0
}

/// 80033FB4: state 7 (phase 0x4000): 8003300C, then the frame tail.
pub fn state_33fb4(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    clear_flight_bit(m);
    call(m, 0x8003_300c, [0; 4]);
    let v0 = frame_tail(m);
    m.pop_frame(0x18);
    v0
}

/// 80033FEC: state 8 (phase 0x20000): 8003302C, then the frame tail.
pub fn state_33fec(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    clear_flight_bit(m);
    call(m, 0x8003_302c, [0; 4]);
    let v0 = frame_tail(m);
    m.pop_frame(0x18);
    v0
}

/// 80034024: state 9 (phase 0x100000): the frame with update 80032F48.
pub fn state_34024(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    clear_flight_bit(m);
    frame(m, 0x8003_2f48);
    let v0 = frame_tail(m);
    m.pop_frame(0x18);
    v0
}

/// 80034064: state 10 (phase 0x200000, the pause menu): context bit 25,
/// the menu input 80034A70, the frame with update 80033004, and the menu
/// overlay 80034BB0.
pub fn pause_state(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let context = m.gw(CONTEXT);
    let flags = m.lw(context) | 0x0200_0000;
    m.sw(context, flags);
    clear_flight_bit(m);
    pause_input(m);
    frame(m, 0x8003_3004);
    let (ot, prim) = overlay_cursors(m);
    let prim = pause_menu(m, ot, prim);
    set_primitives(m, prim);
    let v0 = frame_tail(m);
    m.pop_frame(0x18);
    v0
}

/// 80035E78 -> 0x4000000: state 11: start loading the aircraft file of
/// context +0x3AA (file index table 800AF7C8): resource +0x60 file, its
/// size (8002C214) allocated (80026F70), the read started (8002C404 with
/// completion 800354AC, loading state +8 = 1), then a poll (800354B8).
pub fn load_aircraft_state(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let context = m.gw(CONTEXT);
    let aircraft = u32::from(m.lbu(context + 0x3aa));
    let resources = m.gw(RESOURCES);
    let file = m.lh(0x800a_f7c8 + aircraft * 4) as i32 as u32;
    m.sw(resources + 0x60, file);
    let size = call(m, 0x8002_c214, [resources + 0x60, 1, 0, 0]);
    let buffer = crate::arena::alloc(m, size);
    let resources = m.gw(RESOURCES);
    let state = m.gw(LOADING);
    m.sw(state + 8, 1);
    call(m, 0x8002_c404, [resources + 0x60, 1, buffer, 0x8003_54ac]);
    call(m, 0x8003_54b8, [0; 4]);
    m.pop_frame(0x18);
    0x0400_0000
}

/// 80035EF8: state 13: poll (800354B8), install the loaded effect blocks
/// (8007C548 on the block pair the resource +0x60 file resolves to through
/// 80026E98 twice), phase 0x80, and 80031E3C(0, 0).
pub fn install_effects_state(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    call(m, 0x8003_54b8, [0; 4]);
    let resources = m.gw(RESOURCES);
    let file = m.lw(resources + 0x60);
    let entry = call(m, 0x8002_6e98, [file, 0, 0, 0]);
    let first = m.lw(entry);
    let pair = call(m, 0x8002_6e98, [first, 0, 0, 0]);
    install_effect_blocks(m, pair);
    let gameplay = m.gw(GAMEPLAY);
    m.sw(gameplay + 4, 0x80);
    m.sw(gameplay + 8, 0);
    let v0 = call(m, 0x8003_1e3c, [0, 0, 0, 0]);
    m.pop_frame(0x18);
    v0
}

/// 80034A70: the pause menu input: gameplay bit 6; context +0x130 bits 0/1
/// move the cursor (gameplay +0x29E, wrapped to 0..1); a confirm (context
/// +0x12C bit 4) sets gameplay bit 30 (continue) or 31 (quit).
pub fn pause_input(m: &mut Machine) {
    let gameplay = m.gw(GAMEPLAY);
    let flags = m.lw(gameplay) | 0x40;
    m.sw(gameplay, flags);
    let cursor = gameplay + 0x29c + 2;
    let context = m.gw(CONTEXT);
    if m.lw(context + 0x130) & 1 != 0 {
        let value = m.lhu(cursor).wrapping_sub(1);
        m.sh(cursor, value);
    }
    let context = m.gw(CONTEXT);
    if m.lw(context + 0x130) & 2 != 0 {
        let value = m.lhu(cursor).wrapping_add(1);
        m.sh(cursor, value);
    }
    if m.lh(cursor) < 0 {
        m.sh(cursor, 1);
    }
    if m.lh(cursor) >= 2 {
        m.sh(cursor, 0);
    }
    let context = m.gw(CONTEXT);
    if m.lw(context + 0x12c) & 0x10 != 0 {
        let bit = match m.lh(cursor) {
            0 => 0x4000_0000,
            1 => 0x8000_0000,
            _ => return,
        };
        let gameplay = m.gw(GAMEPLAY);
        let flags = m.lw(gameplay) | bit;
        m.sw(gameplay, flags);
    }
}

/// 80034B78: the result screen flags: gameplay bit 6, and bit 5 (confirm)
/// with context +0x12C bit 6.
pub fn result_flags(m: &mut Machine) {
    let gameplay = m.gw(GAMEPLAY);
    let flags = m.lw(gameplay);
    let context = m.gw(CONTEXT);
    m.sw(gameplay, flags | 0x40);
    if m.lw(context + 0x12c) & 0x40 != 0 {
        m.sw(gameplay, flags | 0x60);
    }
}

/// 80034BB0 (ot, prim) -> next primitive: the pause menu overlay: the two
/// option texts (800353D8 at the positions 800AF734/800AF738 below the
/// frame rectangles 800BAB30/800BAB38, highlighted by the cursor gameplay
/// +0x29E), the cursor box and the frame (800340D8), and the pulsing title.
pub fn pause_menu(m: &mut Machine, ot: u32, prim: u32) -> u32 {
    let frame = m.push_frame(0x40);
    let outer = m.read_bytes(0x800b_ab30, 8);
    m.write_bytes(frame + 0x20, &outer);
    let inner = m.read_bytes(0x800b_ab38, 8);
    m.write_bytes(frame + 0x28, &inner);
    let top = m.lh(frame + 0x2a) as i32 as u32;
    let y = m.lw(0x800a_f734).wrapping_add(2).wrapping_add(top);
    let prim = text_sprite(m, frame, [ot, prim, 0xa0, y], [0x80_8080, 1, 3]);
    let gameplay = m.gw(GAMEPLAY);
    let cursor = gameplay + 0x29c;
    let mode = if m.lh(cursor + 2) != 0 { 0x55 } else { 0x35 };
    let prim = draw_mode(m, ot, prim, mode);
    let top = m.lh(frame + 0x2a) as i32 as u32;
    let y = m.lw(0x800a_f738).wrapping_add(2).wrapping_add(top);
    let prim = text_sprite(m, frame, [ot, prim, 0xa0, y], [0x80_8080, 1, 4]);
    let mode = if m.lh(cursor + 2) != 1 { 0x55 } else { 0x35 };
    let prim = draw_mode(m, ot, prim, mode);
    let index = m.lh(cursor + 2) as i32 as u32;
    let offset = m.lhu(0x800a_f734u32.wrapping_add(index << 2));
    let top = m.lhu(frame + 0x2a);
    m.sh(frame + 0x2a, top.wrapping_add(offset));
    let prim = call(m, 0x8003_40d8, [ot, prim, frame + 0x28, 1]);
    let prim = draw_mode(m, ot, prim, 0x55);
    let prim = call(m, 0x8003_40d8, [ot, prim, frame + 0x20, 2]);
    let context = m.gw(CONTEXT);
    let mut level = (m.lw(context + 0x24) << 3) & 0x1ff;
    if !lt(level, 0x100) {
        level = 0x1ff - level;
    }
    let grey = level | (level << 8) | (level << 16);
    let prim = text_sprite(m, frame, [ot, prim, 0xa0, 0x5a], [grey, 1, 0]);
    let prim = text_sprite(m, frame, [ot, prim, 0xa1, 0x5b], [grey & 0xff_0000, 1, 0]);
    let prim = draw_mode(m, ot, prim, 0x15);
    m.pop_frame(0x40);
    prim
}

/// 80034DF4 (ot, prim) -> next primitive: the blinking result caption
/// (text 0xB at (0xA0, 0xD4), green by the frame counter's sine).
pub fn result_caption(m: &mut Machine, ot: u32, prim: u32) -> u32 {
    let frame = m.push_frame(0x28);
    let gameplay = m.gw(GAMEPLAY);
    let counter = m.lw(gameplay + 0xc);
    let sine = sra(u32::from(m.lhu(SINE + ((counter << 9) & 0x7e00))) << 16, 18);
    let level = if lt(sine, 0) {
        sine.wrapping_neg()
    } else {
        sine
    };
    let colour = sra(level, 6) << 8;
    let prim = text_sprite(m, frame, [ot, prim, 0xa0, 0xd4], [colour, 1, 0xb]);
    let prim = draw_mode(m, ot, prim, 0x35);
    m.pop_frame(0x28);
    prim
}

/// 80034E84 (ot, prim) -> next primitive: the replay caption (text 2 with
/// its shadow at (0x10, 0x1C)) and after frame 90 the fading-in text 0xB
/// at (0xA0, 0xE4).
pub fn replay_caption(m: &mut Machine, ot: u32, prim: u32) -> u32 {
    let frame = m.push_frame(0x30);
    let prim = draw_mode(m, ot, prim, 0x35);
    let prim = text_sprite(m, frame, [ot, prim, 0x10, 0x1c], [0x80_8080, 0, 2]);
    let prim = text_sprite(m, frame, [ot, prim, 0x11, 0x1d], [0x80_0000, 0, 2]);
    let mut prim = draw_mode(m, ot, prim, 0x15);
    let gameplay = m.gw(GAMEPLAY);
    let counter = m.lw(gameplay + 0xc);
    if !lt(counter, 0x5b) {
        let mut level = counter.wrapping_sub(0x5a) << 2;
        if !lt(level, 0x41) {
            level = 0x40;
        }
        prim = text_sprite(m, frame, [ot, prim, 0xa0, 0xe4], [level << 8, 1, 0xb]);
    }
    let prim = draw_mode(m, ot, prim, 0x35);
    m.pop_frame(0x30);
    prim
}

/// 80035080 (ot, prim, level) -> next primitive: the replay fade bar: a
/// grey TILE (0, 0xE8, 0x140 x 2) of `level` (clamped 0..0x1000) linked at
/// `ot`, then the bar sprite 80035144 at (0xE0, 0xE0) of width `level / 128`
/// with the image 80023770.
pub fn fade_bar(m: &mut Machine, ot: u32, prim: u32, level: u32) -> u32 {
    let frame = m.push_frame(0x20);
    let mut level = level;
    if !lt(level, 0x1001) {
        level = 0x1000;
    }
    if lt(level, 0) {
        level = 0;
    }
    let grey = sra(((level << 5).wrapping_sub(level)) << 2, 13) << 8;
    m.sw(prim + 4, grey | 0x6200_0000);
    m.sh(prim + 0xa, 0xe8);
    m.sh(prim + 0xc, 0x140);
    m.sh(prim + 8, 0);
    m.sh(prim + 0xe, 2);
    let head = m.lw(ot) | 0x0300_0000;
    m.sw(prim, head);
    m.sw(ot, (prim << 8) >> 8);
    m.sw(frame + 0x10, sra(level, 7) << 8);
    m.sw(frame + 0x14, 0x8002_3770);
    let next = call(m, 0x8003_5144, [ot, prim + 0x10, 0xe0, 0xe0]);
    m.pop_frame(0x20);
    next
}

/// 8006B8A8: reset the player for the next resident: flags 0x1C000, hit
/// points from its type (8006B59C +0x84), 30 at difficulty 2 and above.
pub fn reset_player(m: &mut Machine) {
    m.push_frame(0x18);
    let table = m.gw(crate::common::ENTITIES);
    let player = m.lw(table);
    let flags = m.lw(player) | 0x1_c000;
    let kind = m.lh(player + 0x48) as i32 as u32;
    m.sw(player, flags);
    let info = call(m, 0x8006_b59c, [kind, 0, 0, 0]);
    let points = m.lhu(info + 0x84);
    m.sh(player + 0x4e, points);
    let context = m.gw(CONTEXT);
    if !lt(m.lw(context + 0x398), 2) {
        m.sh(player + 0x4e, 0x1e);
    }
    m.pop_frame(0x18);
}

/// 8007C548 (pair): install an effect block: `[gp+0x54C]` = block
/// `[pair]` with its data `[pair + 4]` at +4, the end of the data (+0x2050
/// bytes, word aligned) - 0x40 at +8, the offsets +0xC/+0x10 rebased onto
/// the data when not negative, +0x1018 cleared and +0x184C = -1.
pub fn install_effect_blocks(m: &mut Machine, pair: u32) {
    let block = m.lw(pair);
    let data = m.lw(pair + 4);
    let second = m.lw(block + 0x10);
    let size = m.lw(block + 0x2050);
    m.sw(block + 4, data);
    let base = m.lw(block + 4);
    m.set_gw(0x54c, block);
    m.sw(block + 0x1018, 0);
    m.sw(block + 0x184c, u32::MAX);
    m.sw(
        block + 8,
        data.wrapping_add((size >> 2) << 2).wrapping_sub(0x40),
    );
    if !lt(second, 0) {
        m.sw(block + 0x10, base.wrapping_add(second));
    }
    let block = m.gw(0x54c);
    let first = m.lw(block + 0xc);
    if !lt(first, 0) {
        m.sw(block + 0xc, base.wrapping_add(first));
    }
}

/// Native targets owned by this module for indirect calls (`jalr`).
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let [a0, a1, a2, _] = args;
    match target {
        0x8003_3c58 => Some(flight_callback(m)),
        0x8003_2ca4 => {
            update(m);
            Some(0)
        }
        0x8003_3b28 => Some(frame_tail(m)),
        0x8003_3748 => {
            frame(m, a0);
            Some(0)
        }
        0x8003_3d68 => Some(sequence_state(m)),
        0x8003_3da8 => Some(replay_state(m)),
        0x8003_3ed8 => Some(result_state(m)),
        0x8003_3fb4 => Some(state_33fb4(m)),
        0x8003_3fec => Some(state_33fec(m)),
        0x8003_4024 => Some(state_34024(m)),
        0x8003_4064 => Some(pause_state(m)),
        0x8003_5e78 => Some(load_aircraft_state(m)),
        0x8003_5ef8 => Some(install_effects_state(m)),
        0x8003_4a70 => {
            pause_input(m);
            Some(0)
        }
        0x8003_4b78 => {
            result_flags(m);
            Some(0)
        }
        0x8003_4bb0 => Some(pause_menu(m, a0, a1)),
        0x8003_4df4 => Some(result_caption(m, a0, a1)),
        0x8003_4e84 => Some(replay_caption(m, a0, a1)),
        0x8003_5080 => Some(fade_bar(m, a0, a1, a2)),
        0x8006_b8a8 => {
            reset_player(m);
            Some(0)
        }
        0x8007_c548 => {
            install_effect_blocks(m, a0);
            Some(0)
        }
        // common.rs translates 80032C80 without a claim; claimed here so
        // the states' callers can reach it by address.
        0x8003_2c80 => {
            request_transition(m, a0, a1);
            Some(0)
        }
        _ => None,
    }
}
