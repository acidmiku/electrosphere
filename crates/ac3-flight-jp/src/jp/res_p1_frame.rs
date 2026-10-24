//! Patches of `frame` routines whose JP instructions differ from PAL
//! (REVIEW.md §1-§3b, resident part 1): the flight frame, its tail and the
//! resident state callbacks. Each is listed in [`REPLACES`](super::REPLACES).
use crate::{
    common::{CONTEXT, GAMEPLAY, RESOURCES, request_transition},
    dispatch,
    machine::{Machine, SmallData},
    sdk,
};

/// 80032F3C: the regular flight update.
const UPDATE_REGULAR: u32 = 0x8003_2f3c;
/// `[gp+0x2E4]`: per-buffer drawing environment table (stride 0x1014).
const DRAW_BUFFERS: u32 = 0x2e4;
const OT_ENTRIES: u32 = 0x1000;
const LOW24: u32 = 0x00ff_ffff;
const HIGH8: u32 = 0xff00_0000;
/// 800104AC: the 1.14 sine table.
const SINE: u32 = 0x8001_04ac;
/// `[gp+0x6C]`: the loading-screen state.
const LOADING: u32 = 0x6c;

/// 80034930 (JP-only, resident part 3): the flight session overlay that
/// replaces PAL 80034640, (ot, prim) -> next primitive.
const SESSION_OVERLAY: u32 = 0x8003_4930;
/// 80035184 (JP-only, resident part 3): the pause menu overlay that
/// replaces PAL 80034BB0, (ot, prim) -> next primitive.
const PAUSE_MENU: u32 = 0x8003_5184;
/// 8003545C (JP-only, resident part 3): the replay caption that replaces
/// PAL 80034E84, (ot, prim) -> next primitive.
const REPLAY_CAPTION: u32 = 0x8003_545c;
/// 8003563C (`loading`): a bar/text sprite (ot, prim, x, y; stack +0x10
/// level, +0x14 image) -> next primitive.
const SPRITE: u32 = 0x8003_563c;
/// 800235F4: the image of the JP result caption.
const RESULT_CAPTION_IMAGE: u32 = 0x8002_35f4;
/// 8002362C: the fade bar image.
const FADE_BAR_IMAGE: u32 = 0x8002_362c;

fn call(m: &mut Machine, address: u32, args: [u32; 4]) -> u32 {
    dispatch::call(m, address, args)
}

fn lt(a: u32, b: u32) -> bool {
    (a as i32) < (b as i32)
}

fn sra(value: u32, shift: u32) -> u32 {
    ((value as i32) >> (shift & 31)) as u32
}

/// Clear gameplay word 0 bit 1.
fn clear_flight_bit(m: &mut Machine) {
    let gameplay = m.gw(GAMEPLAY);
    let flags = m.lw(gameplay) & !2;
    m.sw(gameplay, flags);
}

/// The OT and primitive cursor of a state's screen overlay: context +0xC,
/// gameplay +0x14.
fn overlay_cursors(m: &mut Machine) -> (u32, u32) {
    let context = m.gw(CONTEXT);
    let gameplay = m.gw(GAMEPLAY);
    (m.lw(context + 0xc), m.lw(gameplay + 0x14))
}

fn set_primitives(m: &mut Machine, primitives: u32) {
    let gameplay = m.gw(GAMEPLAY);
    m.sw(gameplay + 0x14, primitives);
}

fn add_prim(m: &mut Machine, slot: u32, packet: u32) {
    let tag = m.lw(packet);
    let link = m.lw(slot);
    m.sw(packet, (tag & HIGH8) | (link & LOW24));
    let link = m.lw(slot);
    m.sw(slot, (link & HIGH8) | (packet & LOW24));
}

fn draw_offset_packet(m: &mut Machine, slot: u32, packet: u32, x: i16, y: i16) {
    let command = ((y as i32 as u32) << 11) | (x as i32 as u32) | 0xe500_0000;
    m.sw(packet + 4, command);
    m.sw(packet + 8, 0);
    let link = m.lw(slot);
    m.sw(packet, link | 0x0200_0000);
    m.sw(slot, (packet << 8) >> 8);
}

/// 80033EF0 (JP; PAL 80033C58, exact with a different call, REVIEW §3b
/// R0048 and §1 R0049): regular flight state callback (resident state 2,
/// substate 7). On a pending transition (context bit 29) the session
/// overlay is the JP routine 80034930 (PAL 80034640).
pub fn flight_callback(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let gameplay = m.gw(GAMEPLAY);
    let flags = m.lw(gameplay);
    m.sw(gameplay, flags | 2);
    frame(m, UPDATE_REGULAR);
    let context = m.gw(CONTEXT);
    let flags = m.lw(context);
    if flags & 0x2000_0000 != 0 {
        call(m, 0x8003_4718, [0; 4]);
        let context = m.gw(CONTEXT);
        let gameplay = m.gw(GAMEPLAY);
        let a0 = m.lw(context + 0xc);
        let a1 = m.lw(gameplay + 0x14);
        let primitives = call(m, SESSION_OVERLAY, [a0, a1, 0, 0]);
        let gameplay = m.gw(GAMEPLAY);
        m.sw(gameplay + 0x14, primitives);
        let context = m.gw(CONTEXT);
        if m.lw(context) & 0x0800_0000 != 0 {
            call(m, 0x8002_c6f4, [0, 0, 0, 0]);
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
            call(m, 0x8002_c6f4, [1, 0, 0, 0]);
        }
    }
    let v0 = frame_tail(m);
    m.pop_frame(0x18);
    v0
}

/// 800339E0 (JP; PAL 80033748, near, `imm:2`): one flight frame. `update`
/// is the update routine (80032F3C or a mode variant), called with a0 =
/// gameplay, a1 = `[context+8]`, a2 = context. JP: the upper letterbox
/// tile is 240 lines high (PAL 256) and the geometry offset after the world
/// is (0xA0, 0x78) (PAL (0xA0, 0x80)), the 240-line screen centre.
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
    call(m, 0x8007_b438, [camera, 0, 0, 0]);
    let gameplay = m.gw(GAMEPLAY);
    let camera = m.lw(gameplay + 0x28);
    call(
        m,
        0x8003_32e4,
        [camera, gameplay + 0x2c, gameplay + 0x4c, 0],
    );

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
            m.sh(packet + 0xe, 0xf0);
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

    crate::widescreen::world_begin(m);
    for routine in [0x8003_3324, 0x8003_33d8, 0x8003_342c, 0x8003_346c] {
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
    sdk::set_geom_offset(m, 0xa0, 0x78);

    for routine in [0x8003_a2b8, 0x8003_37bc, 0x8003_37dc] {
        call(m, routine, [0; 4]);
    }
    let gameplay = m.gw(GAMEPLAY);
    if !m.lw(gameplay) & 1 != 0 {
        call(m, 0x8003_3814, [0; 4]);
    }
    m.pop_frame(0x20);
}

/// 80033DC0 (JP; PAL 80033B28, near, `imm:2`): per-frame tail. Steps
/// 800371C4 unless replaying (context +0x3A5; PAL +0x3A9), counts frames
/// while no transition is pending, turns gameplay request bits into
/// resident transitions (the quit request sets context +0x957; PAL +0x95B)
/// and applies a pending one through 80032080. Returns 0 or 80032080's
/// result.
pub fn frame_tail(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let result = frame_tail_body(m);
    m.pop_frame(0x18);
    result
}

fn frame_tail_body(m: &mut Machine) -> u32 {
    let context = m.gw(CONTEXT);
    if m.lbu(context + 0x3a5) == 0 {
        call(m, 0x8003_71c4, [0; 4]);
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
        m.sb(context + 0x957, 1);
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
    call(m, 0x8003_2080, [a0, a1, 0, 0])
}

/// 80034040 (JP; PAL 80033DA8, near, `del:4 imm:6 ref:1`): state 5 (phase
/// 0x80, the replay): the frame with update 800330CC and its overlay: with
/// context +0x3A5 (PAL +0x3A9) set, the fade bar 80035578 (any mode; PAL
/// modes 1/2 only) and on the replay's end (gameplay bit 28) the return to
/// the menu (8003048C, 8002C6A8; result 0x60000000 | context +0x3A0 when
/// the replay was cut, else 0x50000000, without the frame tail); otherwise
/// the replay caption, the JP routine 8003545C (PAL 80034E84), unless
/// context +0x39E (PAL +0x3A2).
pub fn replay_state(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    clear_flight_bit(m);
    frame(m, 0x8003_30cc);
    let context = m.gw(CONTEXT);
    let mode = m.lbu(context + 0x3a5);
    let v0 = 'tail: {
        if mode == 0 {
            if m.lbu(context + 0x39e) == 0 {
                let (ot, prim) = overlay_cursors(m);
                let prim = call(m, REPLAY_CAPTION, [ot, prim, 0, 0]);
                set_primitives(m, prim);
            }
        } else {
            let (ot, prim) = overlay_cursors(m);
            let prim = fade_bar(m, ot, prim, 0x1000);
            set_primitives(m, prim);
            let gameplay = m.gw(GAMEPLAY);
            if m.lw(gameplay) & 0x1000_0000 != 0 {
                call(m, 0x8003_048c, [0; 4]);
                call(m, 0x8002_c6a8, [0, 0, 0, 0]);
                let context = m.gw(CONTEXT);
                let cut = m.lbu(context + 0x3a5) == 1 || m.lw(context + 0x128) & 0x10 != 0;
                if cut {
                    m.sb(context + 0x3a5, 0);
                    let context = m.gw(CONTEXT);
                    break 'tail m.lw(context + 0x3a0) | 0x6000_0000;
                }
                break 'tail 0x5000_0000;
            }
        }
        frame_tail(m)
    };
    m.pop_frame(0x18);
    v0
}

/// 800342EC (JP; PAL 80034064, exact with a different call, REVIEW §3b
/// R0054): state 10 (phase 0x200000, the pause menu): context bit 25, the
/// menu input 80035044, the frame with update 8003329C, and the menu
/// overlay, the JP routine 80035184 (PAL 80034BB0).
pub fn pause_state(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let context = m.gw(CONTEXT);
    let flags = m.lw(context) | 0x0200_0000;
    m.sw(context, flags);
    clear_flight_bit(m);
    crate::frame::pause_input(m);
    frame(m, 0x8003_329c);
    let (ot, prim) = overlay_cursors(m);
    let prim = call(m, PAUSE_MENU, [ot, prim, 0, 0]);
    set_primitives(m, prim);
    let v0 = frame_tail(m);
    m.pop_frame(0x18);
    v0
}

/// 800353E8 (JP; PAL 80034DF4, changed, `call-:1 del:7 imm:4 op:5 reg:1`)
/// (ot, prim) -> next primitive: the blinking result caption, now one
/// sprite 8003563C at (0x28, 0xD4) of the image 800235F4 whose level
/// follows the frame counter's sine (PAL: text 0xB at (0xA0, 0xD4) and a
/// draw-mode packet).
pub fn result_caption(m: &mut Machine, ot: u32, prim: u32) -> u32 {
    let frame = m.push_frame(0x20);
    let gameplay = m.gw(GAMEPLAY);
    let counter = m.lw(gameplay + 0xc);
    let sine = sra(u32::from(m.lhu(SINE + ((counter << 9) & 0x7e00))) << 16, 18);
    let level = if lt(sine, 0) {
        sine.wrapping_neg()
    } else {
        sine
    };
    m.sw(frame + 0x10, sra(level, 6) << 8);
    m.sw(frame + 0x14, RESULT_CAPTION_IMAGE);
    let next = call(m, SPRITE, [ot, prim, 0x28, 0xd4]);
    m.pop_frame(0x20);
    next
}

/// 80035578 (JP; PAL 80035080, near, `imm:1 op:1`) (ot, prim, level) ->
/// next primitive: the replay fade bar: a grey TILE (0, 0xD8, 0x140 x 2;
/// PAL y 0xE8) of `level` (clamped 0..0x1000) linked at `ot`, then the bar
/// sprite 8003563C at (0xE0, 0xD0) (PAL (0xE0, 0xE0)) of width
/// `level / 128` with the image 8002362C.
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
    m.sh(prim + 0xa, 0xd8);
    m.sh(prim + 0xc, 0x140);
    m.sh(prim + 8, 0);
    m.sh(prim + 0xe, 2);
    let head = m.lw(ot) | 0x0300_0000;
    m.sw(prim, head);
    m.sw(ot, (prim << 8) >> 8);
    m.sw(frame + 0x10, sra(level, 7) << 8);
    m.sw(frame + 0x14, FADE_BAR_IMAGE);
    let next = call(m, SPRITE, [ot, prim + 0x10, 0xe0, 0xd0]);
    m.pop_frame(0x20);
    next
}

/// 800361BC (JP; PAL 80035E78, near, `imm:1`) -> 0x4000000: state 11:
/// start loading the aircraft file of context +0x3A6 (PAL +0x3AA; file
/// index table 800B0BD0): resource +0x60 file, its size (8002C63C)
/// allocated (80027410), the read started (8002C82C with completion
/// 800357B4, loading state +8 = 1), then a poll (800357C0).
pub fn load_aircraft_state(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let context = m.gw(CONTEXT);
    let aircraft = u32::from(m.lbu(context + 0x3a6));
    let resources = m.gw(RESOURCES);
    let file = m.lh(0x800b_0bd0 + aircraft * 4) as i32 as u32;
    m.sw(resources + 0x60, file);
    let size = call(m, 0x8002_c63c, [resources + 0x60, 1, 0, 0]);
    let buffer = crate::arena::alloc(m, size);
    let resources = m.gw(RESOURCES);
    let state = m.gw(LOADING);
    m.sw(state + 8, 1);
    call(m, 0x8002_c82c, [resources + 0x60, 1, buffer, 0x8003_57b4]);
    call(m, 0x8003_57c0, [0; 4]);
    m.pop_frame(0x18);
    0x0400_0000
}

/// 8006D078 (JP; PAL 8006B8A8, near, `imm:1`): reset the player for the
/// next resident: flags 0x1C000, hit points from its type (8006CD6C
/// +0x84), 30 at difficulty (context +0x394; PAL +0x398) 2 and above.
pub fn reset_player(m: &mut Machine) {
    m.push_frame(0x18);
    let table = m.gw(crate::common::ENTITIES);
    let player = m.lw(table);
    let flags = m.lw(player) | 0x1_c000;
    let kind = m.lh(player + 0x48) as i32 as u32;
    m.sw(player, flags);
    let info = call(m, 0x8006_cd6c, [kind, 0, 0, 0]);
    let points = m.lhu(info + 0x84);
    m.sh(player + 0x4e, points);
    let context = m.gw(CONTEXT);
    if !lt(m.lw(context + 0x394), 2) {
        m.sh(player + 0x4e, 0x1e);
    }
    m.pop_frame(0x18);
}
