//! Overlay module 2 (game state 3), briefing sub-mode: sub-mode 1 of the
//! overlay's task table (0x800F1DD4), whose steps are the initialization
//! 800EEE70 (here) and the per-frame wrapper 800EF01C (owned by ov2core).
//!
//! The wrapper runs one step of the phase table at 0x800F68B8 (panel
//! opening, zoom, the timed briefing script with its text pages, the label
//! map and the closing animation: the `phase_*` routines here), then the
//! object, label and camera movement (800EB654, 800EB9E8, 800EBC14), the
//! label connector lines (800EB754) and the 3D briefing scene and minimap
//! (`ov2_briefing_scene`).
//!
//! Ownership follows ov2core's classification
//! (`analysis/flight-runtime/ov2core-001/routines.tsv`, rows `ov2brief`),
//! plus four routines its runs never reached that only the briefing
//! reaches: the no-op command 800EB0E8 (opcodes 0 and 4), the label
//! navigation 800EDD28 and camera aim 800EDE98 (LEFT/RIGHT on the label
//! map, reached in `ov2brief-calltree-001`), and the phase step 800EEC90
//! (2,2). The briefing
//! script is a list of 16-byte commands {time, opcode, object slot, six
//! halfword arguments}; opcodes 0..7 run through the table at 0x800F6864.
//!
//! Every routine follows its original instructions and store order. Pointer
//! arguments are guest addresses, as in the original ABI. Routines with an
//! original stack frame push it (`Machine::push_frame`) so that locals
//! passed to callees and nested frames keep their original addresses.
//! Routines shared with other sub-modes (ov2core: text drawing, visual
//! slots, streams), the SDK and the sound driver are called by original
//! address through [`external`].
//!
//! [`dispatch`] claims these routines only while module 2 is loaded
//! (`common::loaded_module`), since every overlay loads at 0x800C9DC0.
use crate::{
    common::{self, CONTEXT},
    dispatch as owners,
    machine::{Fault, Machine, SmallData},
    ov2_briefing_scene as scene,
};
use std::cell::RefCell;

/// Overlay module number of the hub/briefing/aircraft overlay.
pub const MODULE: u32 = 2;
/// 800F6860: pointer to the briefing state block (in the overlay arena).
pub const BRIEFING: u32 = 0x800f_6860;
/// 800F69D0: pointer to the overlay-2 work block (`+0x90` ordering table,
/// `+0x20094` next free primitive, `+0x20098` height map, `+0x2009C`
/// briefing script, `+0x200B4..+0x200BC` text tables).
pub const WORK: u32 = 0x800f_69d0;
/// 800F6864: the eight briefing-script command routines, by opcode.
pub const COMMANDS: u32 = 0x800f_6864;
/// 800F68B8: the four phase step lists (phase, then substate).
pub const PHASES: u32 = 0x800f_68b8;
/// Work block offset of the second half (`lui 2`): primitive cursor at
/// `+0x94` from here, height map at `+0x98`.
pub const WORK_HIGH: u32 = 0x2_0000;

// Briefing state block offsets.
/// Flags: 1 scene on, 2 fast-forward, 4 cancelled, 8 text page full,
/// 0x10 text done, 0x20 script done.
pub const FLAGS: u32 = 0;
pub const PHASE: u32 = 4;
pub const SUBSTATE: u32 = 6;
pub const CLOCK: u32 = 8;
/// Script cursor (a guest pointer to the next command).
pub const CURSOR: u32 = 0xc;
pub const VISUAL: u32 = 0x10;
pub const PANEL_TIMER: u32 = 0x14;
pub const PANEL_OFFSETS: u32 = 0x18;
/// 32 objects of 0x24 bytes.
pub const OBJECTS: u32 = 0x1c;
pub const OBJECT_SIZE: u32 = 0x24;
pub const LABEL_COUNT: u32 = 0x4a0;
/// 8 labels of 12 bytes {flags, style byte, object byte, timer, x, y}.
pub const LABELS: u32 = 0x4a4;
pub const LABEL_SIZE: u32 = 0xc;
pub const SELECTED: u32 = 0x504;
pub const FINISH_TIMER: u32 = 0x508;
pub const MARKER: u32 = 0x50c;
pub const MARKER_ID: u32 = 0x514;
pub const CAMERA_DURATION: u32 = 0x518;
/// Camera x, y, z words.
pub const CAMERA: u32 = 0x51c;
pub const CAMERA_SPEED: u32 = 0x52c;
pub const CAMERA_ANGLES: u32 = 0x530;
pub const ZOOM: u32 = 0x538;
pub const VIEW_ANGLES: u32 = 0x53c;
pub const ZOOM_TIMER: u32 = 0x540;
pub const VIEW_OFFSETS: u32 = 0x544;
pub const TEXT_TIMER: u32 = 0x548;
pub const TEXT_COUNT: u32 = 0x54c;
pub const TEXT_INDEX: u32 = 0x550;
/// The scene camera matrix (rotation and translation, 0x20 bytes) that
/// 800EBCD8 builds for the object and marker projections.
pub const VIEW_MATRIX: u32 = 0x554;

// Routines owned elsewhere, called by original address.
/// 8002D724: start sound effect `id` (sound driver, owned by `script`).
pub const PLAY_SOUND: u32 = 0x8002_d724;
const SQUARE_ROOT0: u32 = 0x800a_47c8;
const RATAN2: u32 = 0x800a_6088;
const RSIN: u32 = 0x800a_45c8;
const RCOS: u32 = 0x800a_4698;
/// ov2core: visual slot `a0` offset (x, y) (800F18EC).
pub const SET_VISUAL_OFFSET: u32 = 0x800f_18ec;
/// ov2core: read visual slot `a0` offset into two halfword pointers (800F1908).
pub const GET_VISUAL_OFFSET: u32 = 0x800f_1908;
/// ov2core: visual slot `a0` parameter (800F193C).
pub const SET_VISUAL_PARAMETER: u32 = 0x800f_193c;
/// ov2core: visual slot `a0` style byte (800F1770).
pub const SET_VISUAL_STYLE: u32 = 0x800f_1770;
/// ov2core: select visual page (800F196C).
const SELECT_VISUAL_PAGE: u32 = 0x800f_196c;
/// ov2core: localized string `a0` of text table `a1` (800D0B28).
const TEXT_STRING: u32 = 0x800d_0b28;
/// ov2core: draw a string (font a0, text a1, ordering table a2, primitive
/// cursor a3; x, y and color on the stack); returns the new cursor (800D04C8).
pub const DRAW_TEXT: u32 = 0x800d_04c8;
/// ov2core: clear text visibility (800D0F20).
const CLEAR_TEXT: u32 = 0x800d_0f20;
/// ov2core: stream helpers (800D0C58 select stream `a0`, 800D0C68 play).
const STREAM_START: u32 = 0x800d_0c58;
const STREAM_PLAY: u32 = 0x800d_0c68;
/// ov2core: fade (800E7A18) and the briefing entry helper (800E983C).
const FADE: u32 = 0x800e_7a18;
const ENTER_BRIEFING: u32 = 0x800e_983c;
/// SDK `sprintf` (800ACD28) and `strlen` (800AC328).
pub const SPRINTF: u32 = 0x800a_cd28;
pub const STRLEN: u32 = 0x800a_c328;

// ---- register-level helpers ------------------------------------------------------

pub(crate) fn lh(m: &mut Machine, address: u32) -> u32 {
    m.lh(address) as i32 as u32
}
pub(crate) fn lhu(m: &mut Machine, address: u32) -> u32 {
    u32::from(m.lhu(address))
}
pub(crate) fn lbu(m: &mut Machine, address: u32) -> u32 {
    u32::from(m.lbu(address))
}
pub(crate) fn sh(m: &mut Machine, address: u32, value: u32) {
    m.sh(address, value as u16);
}
pub(crate) fn sb(m: &mut Machine, address: u32, value: u32) {
    m.sb(address, value as u8);
}
/// `sll 16` + `sra 16`.
pub(crate) fn sext16(value: u32) -> u32 {
    value as u16 as i16 as i32 as u32
}
/// `sra`/`srav`: arithmetic right shift by the low five bits.
pub(crate) fn sra(value: u32, shift: u32) -> u32 {
    ((value as i32) >> (shift & 31)) as u32
}
/// `slt`: signed less-than.
pub(crate) fn slt(a: u32, b: u32) -> bool {
    (a as i32) < (b as i32)
}
/// `mult` + `mflo`.
pub(crate) fn mul(a: u32, b: u32) -> u32 {
    (a as i32).wrapping_mul(b as i32) as u32
}
/// `mult` + `mfhi`.
pub(crate) fn mulhi(a: u32, b: u32) -> u32 {
    ((i64::from(a as i32) * i64::from(b as i32)) >> 32) as u32
}
/// The compiled signed division by `2^shift`: `bgez` over an `addiu` of
/// `2^shift - 1`, then `sra`.
pub(crate) fn div_pow2(value: u32, shift: u32) -> u32 {
    let biased = if (value as i32) < 0 {
        value.wrapping_add((1 << shift) - 1)
    } else {
        value
    };
    sra(biased, shift)
}
/// The compiled signed halving: `srl 31`, `addu`, `sra 1`.
pub(crate) fn half(value: u32) -> u32 {
    sra(value.wrapping_add(value >> 31), 1)
}
/// The compiled `abs`: `subu` from zero when negative.
pub(crate) fn abs(value: u32) -> u32 {
    if (value as i32) < 0 {
        0u32.wrapping_sub(value)
    } else {
        value
    }
}
/// `div` at `pc` + `mflo` behind the compiler's checks: `break 7` at
/// `pc + 0xC` for a zero divisor and `break 6` at `pc + 0x24` for
/// `0x80000000 / -1`. Reaching a trap records [`Fault::Trap`] and yields the
/// R3000 quotient.
pub(crate) fn div(m: &mut Machine, numerator: u32, divisor: u32, pc: u32) -> u32 {
    let (n, d) = (numerator as i32, divisor as i32);
    if d == 0 {
        m.raise(Fault::Trap {
            address: pc + 0xc,
            code: 7,
        });
        return if n < 0 { 1 } else { u32::MAX };
    }
    if n == i32::MIN && d == -1 {
        m.raise(Fault::Trap {
            address: pc + 0x24,
            code: 6,
        });
        return numerator;
    }
    n.wrapping_div(d) as u32
}
/// Compiled `x / 25`: `mult` by 0x51EB851F, `mfhi`, `sra 3`, minus the sign.
pub(crate) fn div25(value: u32) -> u32 {
    sra(mulhi(value, 0x51eb_851f), 3).wrapping_sub(sra(value, 31))
}
/// Inline `addPrim(slot, prim)`: the packet keeps its length byte and takes
/// the slot's link, then the slot (reloaded) links to the packet.
pub(crate) fn add_prim(m: &mut Machine, slot: u32, prim: u32) {
    let tag = m.lw(prim) & 0xff00_0000;
    let link = m.lw(slot) & 0x00ff_ffff;
    m.sw(prim, tag | link);
    let high = m.lw(slot) & 0xff00_0000;
    m.sw(slot, high | (prim & 0x00ff_ffff));
}

/// `[0x800F6860]`: the briefing state block.
pub(crate) fn briefing(m: &mut Machine) -> u32 {
    m.lw(BRIEFING)
}
/// `[0x800F69D0]`: the overlay-2 work block.
pub(crate) fn work(m: &mut Machine) -> u32 {
    m.lw(WORK)
}
/// `[gp+0x2DC]` + 0x12C: the buttons pressed this frame.
pub(crate) fn pressed(m: &mut Machine) -> u32 {
    let context = m.gw(CONTEXT);
    m.lw(context + 0x12c)
}
fn label(m: &mut Machine, index: u32) -> u32 {
    briefing(m)
        .wrapping_add(LABELS)
        .wrapping_add(index.wrapping_mul(LABEL_SIZE))
}
fn object(state: u32, index: u32) -> u32 {
    state
        .wrapping_add(OBJECTS)
        .wrapping_add(index.wrapping_mul(OBJECT_SIZE))
}
/// `(arg - 128) * 128`: a script map coordinate (`addiu -0x80`, `sll 7`).
fn map(value: u32) -> u32 {
    value.wrapping_sub(0x80) << 7
}

// ---- calls into other modules ----------------------------------------------------

/// A replay of original callee effects for snapshot tests (see
/// [`set_external_replay`]): given the target and register arguments, it
/// returns `Some(v0)` when it applied the original's recorded effect.
pub type ExternalReplay = Box<dyn FnMut(&mut Machine, u32, [u32; 4]) -> Option<u32>>;

thread_local! {
    static REPLAY: RefCell<Option<ExternalReplay>> = const { RefCell::new(None) };
}

/// Test fixture support, never installed by the player: while `replay` is
/// installed on this thread, every call these modules make into another
/// module is offered to it first, so that tests can substitute the recorded
/// effects of routines owned elsewhere that are not translated yet. Returns
/// the previous replay.
pub fn set_external_replay(replay: Option<ExternalReplay>) -> Option<ExternalReplay> {
    REPLAY.with(|cell| std::mem::replace(&mut *cell.borrow_mut(), replay))
}

/// Call a routine owned by another module (or another overlay sub-mode) by
/// original address, with the original register arguments; returns `v0`.
/// Stack arguments are in the caller's pushed frame at `m.sp + 0x10..`.
pub fn external(m: &mut Machine, target: u32, args: [u32; 4]) -> u32 {
    let replay = REPLAY.with(|cell| cell.borrow_mut().take());
    if let Some(mut replay) = replay {
        let result = replay(m, target, args);
        REPLAY.with(|cell| *cell.borrow_mut() = Some(replay));
        if let Some(v0) = result {
            return v0;
        }
    }
    owners::call(m, target, args)
}

/// `jalr` through a table of code addresses: this module's routines run
/// natively (module 2 is loaded whenever they run), anything else goes
/// through [`external`].
fn call_pointer(m: &mut Machine, target: u32, args: [u32; 4]) -> u32 {
    match dispatch(m, target, args) {
        Some(v0) => v0,
        None => external(m, target, args),
    }
}

pub(crate) fn sound(m: &mut Machine, id: u32) {
    external(m, PLAY_SOUND, [id, 0, 0, 0]);
}

/// Routines translated here, by original address (original register ABI,
/// result = `v0`). Claimed only while overlay module 2 is loaded. Routines
/// without a result return 0.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if common::loaded_module(m) != MODULE {
        return None;
    }
    let [a0, a1, a2, a3] = args;
    Some(match target {
        0x800e_b0e8 => 0,
        0x800e_b0f0 => {
            command_place(m, a0);
            0
        }
        0x800e_b1d8 => {
            command_move(m, a0);
            0
        }
        0x800e_b304 => {
            command_label(m, a0);
            0
        }
        0x800e_b414 => {
            command_pan(m, a0);
            0
        }
        0x800e_b4f0 => {
            command_camera(m, a0);
            0
        }
        0x800e_b524 => {
            command_marker(m, a0);
            0
        }
        0x800e_b564 => {
            reset_objects(m);
            0
        }
        0x800e_b5ac => {
            reset_labels(m);
            0
        }
        0x800e_b630 => {
            reset_camera(m);
            0
        }
        0x800e_b654 => tick_objects(m),
        0x800e_b754 => {
            scene::label_connector(m, a0);
            0
        }
        0x800e_b9e8 => {
            tick_labels(m);
            0
        }
        0x800e_bc14 => tick_camera(m),
        0x800e_bcd8 => {
            scene::view_matrix(m);
            0
        }
        0x800e_bf68 => scene::terrain(m, a0, a1),
        0x800e_ce7c => scene::objects(m, a0, a1),
        0x800e_d63c => scene::marker(m, a0, a1),
        0x800e_d834 => scene::draw(m, a0, a1),
        0x800e_d8b0 => scene::minimap(m, a0, a1),
        0x800e_dd28 => navigate(m, a0),
        0x800e_de98 => {
            aim_camera(m, a0);
            0
        }
        0x800e_df78 => phase_reset_view(m),
        0x800e_dfa4 => phase_open_panels(m),
        0x800e_e128 => phase_zoom_in(m),
        0x800e_e1e4 => phase_fly_in(m),
        0x800e_e3b8 => phase_script(m),
        0x800e_e860 => phase_finish(m),
        0x800e_e8d0 => phase_hide_labels(m),
        0x800e_e95c => phase_labels_hidden(m),
        0x800e_e9a0 => phase_select_label(m),
        0x800e_ea20 => phase_map(m),
        0x800e_ec90 => phase_close_panels(m),
        0x800e_ed50 => phase_cancel(m),
        0x800e_edbc => phase_close(m),
        0x800e_ee70 => initialize(m),
        0x800d_0b74 => text_line(m, a0, a1, a2),
        0x800d_0bd8 => text_line_count(m, a0, a1),
        0x800d_0f60 => {
            spaces_to_gaps(m, a0);
            0
        }
        0x800f_1788 => set_visual_color(m, a0, a1, a2, a3),
        _ => return None,
    })
}

// ---- script commands (table 0x800F6864, argument = command pointer) -------------

/// 800EB0F0 (opcode 1): place object `slot`: flags |= 1, state = opcode,
/// appearance timers = time + 15, type = arg0, position from args 1..3
/// (`(a - 128) * 128`, height `-a / 10`), heading `arg4 * 4096 / 360`.
pub fn command_place(m: &mut Machine, command: u32) {
    let slot = lbu(m, command + 3);
    let state = briefing(m);
    let object = object(state, slot);
    let flags = m.lw(object);
    m.sw(object, flags | 1);
    let opcode = lbu(m, command + 2);
    sh(m, object + 6, opcode);
    let time = lhu(m, command);
    sh(m, object + 8, time.wrapping_add(0xf));
    let time = lhu(m, command);
    sh(m, object + 0xa, time.wrapping_add(0xf));
    let kind = lhu(m, command + 4);
    sh(m, object + 4, kind);
    let x = lh(m, command + 6);
    sh(m, object + 0xe, map(x));
    let height = 0u32.wrapping_sub(lh(m, command + 8));
    // height / 10: mult by 0x66666667, mfhi, sra 2, minus the sign.
    let quotient = sra(mulhi(height, 0x6666_6667), 2).wrapping_sub(sra(height, 31));
    sh(m, object + 0x10, quotient);
    let z = lh(m, command + 0xa);
    sh(m, object + 0x16, 0);
    sh(m, object + 0x12, map(z));
    let degrees = lh(m, command + 0xc) << 12;
    sh(m, object + 0x1a, 0);
    // * 4096 / 360: mult by 0xB60B60B7, mfhi + x, sra 8, minus the sign.
    let heading =
        sra(mulhi(degrees, 0xb60b_60b7).wrapping_add(degrees), 8).wrapping_sub(sra(degrees, 31));
    sh(m, object + 0x18, heading);
}

/// 800EB1D8 (opcode 2): start moving object `slot` towards args 1/3 over
/// `arg0` frames: heading `ratan2` of the full deltas, speed = distance of
/// the 16-bit deltas / arg0.
pub fn command_move(m: &mut Machine, command: u32) {
    m.push_frame(0x28);
    let slot = lbu(m, command + 3);
    let object = object(briefing(m), slot);
    let opcode = lbu(m, command + 2);
    sh(m, object + 6, opcode);
    let x_raw = lhu(m, object + 0xe);
    let duration = lhu(m, command + 4);
    let x_old = sext16(x_raw);
    sh(m, object + 8, duration);
    let duration = lhu(m, command + 4);
    sh(m, object + 0xa, duration);
    let x_arg = lh(m, command + 6).wrapping_sub(0x80);
    let z_arg = lh(m, command + 0xa).wrapping_sub(0x80);
    let dy = sext16(x_arg << 7).wrapping_sub(x_old);
    let dx_wide = (x_arg << 7).wrapping_sub(x_raw);
    let z_raw = lhu(m, object + 0x12);
    let dz_wide = (z_arg << 7).wrapping_sub(z_raw);
    let dx = sext16(z_arg << 7).wrapping_sub(sext16(z_raw));
    let heading = external(m, RATAN2, [dy, dx, 0, 0]);
    let a = sext16(dx_wide);
    let b = sext16(dz_wide);
    let squares = mul(a, a);
    sh(m, object + 0x18, heading);
    let squares = squares.wrapping_add(mul(b, b));
    let distance = external(m, SQUARE_ROOT0, [squares, 0, 0, 0]);
    let duration = lh(m, command + 4);
    let speed = div(m, distance, duration, 0x800e_b2b4);
    sh(m, object + 0xc, speed);
    m.pop_frame(0x28);
}

/// 800EB304 (opcode 3): add a label for object `slot` with style `arg0`
/// (sound 100 unless fast-forwarding); the two previous labels move up.
pub fn command_label(m: &mut Machine, command: u32) {
    m.push_frame(0x20);
    let state = briefing(m);
    let count = m.lw(state + LABEL_COUNT);
    let label = state + LABELS + count.wrapping_mul(LABEL_SIZE);
    let flags = m.lw(label);
    m.sw(label, flags | 7);
    if m.lw(state + FLAGS) & 2 == 0 {
        sound(m, 100);
    }
    let style = lbu(m, command + 4);
    sb(m, label + 4, style);
    let slot = lbu(m, command + 3);
    sb(m, label + 5, slot);
    let state = briefing(m);
    let count = m.lw(state + LABEL_COUNT);
    let style = lbu(m, label + 4);
    external(m, SET_VISUAL_STYLE, [count.wrapping_add(1), style, 0, 0]);
    let state = briefing(m);
    let count = m.lw(state + LABEL_COUNT).wrapping_add(1);
    m.sw(state + LABEL_COUNT, count);
    if !slt(count, 2) {
        let flags = m.lw(label.wrapping_sub(0xc));
        m.sw(label.wrapping_sub(0xc), flags | 8);
    }
    let state = briefing(m);
    if !slt(m.lw(state + LABEL_COUNT), 3) {
        let flags = m.lw(label.wrapping_sub(0x18));
        m.sw(label.wrapping_sub(0x18), flags | 0x10);
    }
    m.pop_frame(0x20);
}

/// 800EB414 (opcode 5): pan the camera to args 1/3 over `arg0` frames.
pub fn command_pan(m: &mut Machine, command: u32) {
    m.push_frame(0x20);
    let state = briefing(m);
    let duration = lh(m, command + 4);
    m.sw(state + CAMERA_DURATION, duration);
    let x = lh(m, command + 6);
    let camera_x = m.lw(state + CAMERA);
    let dy = map(x).wrapping_sub(camera_x);
    let squares = mul(dy, dy);
    let z = lh(m, command + 0xa);
    let camera_z = m.lw(state + CAMERA + 8);
    let dx = map(z).wrapping_sub(camera_z);
    let squares = squares.wrapping_add(mul(dx, dx));
    let distance = external(m, SQUARE_ROOT0, [squares, 0, 0, 0]);
    let state = briefing(m);
    let duration = m.lw(state + CAMERA_DURATION);
    let speed = div(m, distance, duration, 0x800e_b490);
    m.sw(state + CAMERA_SPEED, speed);
    let heading = external(m, RATAN2, [dy, dx, 0, 0]);
    let state = briefing(m);
    sh(m, state + CAMERA_ANGLES + 2, heading);
    m.pop_frame(0x20);
}

/// 800EB4F0 (opcode 6): set the camera to args 1/3 (y = 0).
pub fn command_camera(m: &mut Machine, command: u32) {
    let x = lh(m, command + 6);
    let state = briefing(m);
    m.sw(state + CAMERA, map(x));
    m.sw(state + CAMERA + 4, 0);
    let z = lh(m, command + 0xa);
    m.sw(state + CAMERA + 8, map(z));
}

/// 800EB524 (opcode 7): place the timer marker at args 1/3 (height -300)
/// showing `arg0`.
pub fn command_marker(m: &mut Machine, command: u32) {
    let x = lh(m, command + 6);
    let state = briefing(m);
    sh(m, state + MARKER, map(x));
    sh(m, state + MARKER + 2, 0xfed4);
    let z = lh(m, command + 0xa);
    sh(m, state + MARKER + 4, map(z));
    let id = lh(m, command + 4);
    m.sw(state + MARKER_ID, id);
}

// ---- resets and per-frame movement ----------------------------------------------

/// 800EB564: clear the 32 objects (flags, timers; state and screen
/// position -1).
pub fn reset_objects(m: &mut Machine) {
    let state = briefing(m);
    for index in 0..32 {
        let object = object(state, index);
        m.sw(object, 0);
        sh(m, object + 8, 0);
        sh(m, object + 0xa, 0);
        sh(m, object + 6, 0xffff);
        sh(m, object + 0x1e, 0xffff);
        sh(m, object + 0x20, 0xffff);
    }
}

/// 800EB5AC: clear the selection and the 8 labels, and their visual
/// parameters (slots 1..8).
pub fn reset_labels(m: &mut Machine) {
    m.push_frame(0x20);
    let state = briefing(m);
    m.sw(state + SELECTED, 0);
    for index in 0..8 {
        let label = label(m, index);
        m.sw(label, 0);
        sb(m, label + 5, 0);
        sb(m, label + 4, 0);
        sh(m, label + 0xa, 0);
        sh(m, label + 8, 0);
        sh(m, label + 6, 0);
        external(m, SET_VISUAL_PARAMETER, [index + 1, 0, 0, 0]);
    }
    m.pop_frame(0x20);
}

/// 800EB630: stop the camera movement.
pub fn reset_camera(m: &mut Machine) {
    let state = briefing(m);
    m.sw(state + CAMERA_SPEED, 0);
    m.sw(state + CAMERA_DURATION, 0);
    sh(m, state + CAMERA_ANGLES, 0);
    sh(m, state + CAMERA_ANGLES + 2, 0);
    sh(m, state + CAMERA_ANGLES + 4, 0);
}

/// `speed * trig(angle) / 4096` as compiled (`mult`, `bgez`/`addiu 0xFFF`,
/// `sra 12`).
fn step(speed: u32, trig: u32) -> u32 {
    div_pow2(mul(speed, trig), 12)
}

/// 800EB654: advance every moving object (state 2 with frames left) by its
/// speed along its heading. Returns 1.
pub fn tick_objects(m: &mut Machine) -> u32 {
    m.push_frame(0x28);
    for index in 0..32 {
        let object = object(briefing(m), index);
        if lh(m, object + 6) != 2 || (lh(m, object + 8) as i32) <= 0 {
            continue;
        }
        let angle = lh(m, object + 0x18);
        let sine = external(m, RSIN, [angle, 0, 0, 0]);
        let speed = lh(m, object + 0xc);
        let dy = step(speed, sine);
        let angle = lh(m, object + 0x18);
        let cosine = external(m, RCOS, [angle, 0, 0, 0]);
        let speed = lh(m, object + 0xc);
        let dx = step(speed, cosine);
        let x = lhu(m, object + 0xe);
        let frames = lhu(m, object + 8);
        sh(m, object + 0xe, x.wrapping_add(dy));
        let z = lhu(m, object + 0x12);
        sh(m, object + 8, frames.wrapping_sub(1));
        sh(m, object + 0x12, z.wrapping_add(dx));
    }
    m.pop_frame(0x28);
    1
}

/// 800EB9E8: animate the 8 labels (appear, slide up, highlight, fade) and
/// place their visual slots (4..11) relative to slot 2's offset.
pub fn tick_labels(m: &mut Machine) {
    let frame = m.push_frame(0x30);
    external(m, GET_VISUAL_OFFSET, [2, frame + 0x10, frame + 0x12, 0]);
    for index in 0..8u32 {
        let slot = index + 1;
        let label = label(m, index);
        let flags = m.lw(label);
        if flags & 1 == 0 {
            continue;
        }
        if flags & 0x20 != 0 {
            // Fading out.
            let timer = lh(m, label + 6);
            if (timer as i32) <= 0 {
                m.sw(label, flags & !0x22);
                sh(m, label + 6, 0);
            } else {
                external(m, SET_VISUAL_PARAMETER, [slot, timer, 0, 0]);
                let timer = lhu(m, label + 6);
                sh(m, label + 6, timer.wrapping_sub(1));
            }
        } else if flags & 0x40 != 0 {
            // Selected: wait 25 frames, then highlight.
            let timer = lh(m, label + 6);
            let raw = lhu(m, label + 6);
            if slt(timer, 0x19) {
                sh(m, label + 6, raw.wrapping_add(1));
            } else {
                m.sw(label, (flags & !0x40) | 4);
                sh(m, label + 6, 0);
                sound(m, 100);
            }
        } else if flags & 4 != 0 {
            // Fading in.
            let timer = lh(m, label + 6);
            if slt(timer, 0x19) {
                external(m, SET_VISUAL_PARAMETER, [slot, timer, 0, 0]);
                let timer = lhu(m, label + 6);
                sh(m, label + 6, timer.wrapping_add(1));
            } else {
                m.sw(label, flags & !4);
                sh(m, label + 6, 0);
            }
        } else {
            if flags & 8 != 0 {
                let timer = lh(m, label + 6);
                if slt(timer, 0x19) {
                    let raw = lhu(m, label + 6);
                    sh(m, label + 0xa, (timer << 2).wrapping_add(timer));
                    sh(m, label + 6, raw.wrapping_add(1));
                } else {
                    m.sw(label, flags & !8);
                    sh(m, label + 6, 0);
                }
                external(m, SET_VISUAL_PARAMETER, [slot, 0x19, 0, 0]);
            } else if flags & 0x10 != 0 {
                let timer = lh(m, label + 6);
                if slt(timer, 0x19) {
                    let y = ((timer << 3).wrapping_add(timer) << 1).wrapping_add(0x7d);
                    let raw = lhu(m, label + 6);
                    sh(m, label + 0xa, y);
                    sh(m, label + 6, raw.wrapping_add(1));
                } else {
                    m.sw(label, flags & !0x12);
                    sh(m, label + 6, 0);
                }
                external(m, SET_VISUAL_PARAMETER, [slot, 0x19, 0, 0]);
            } else if flags & 2 != 0 {
                external(m, SET_VISUAL_PARAMETER, [slot, 0x19, 0, 0]);
            } else {
                external(m, SET_VISUAL_PARAMETER, [slot, 0, 0, 0]);
                sh(m, label + 0xa, 0);
            }
        }
        let x = lh(m, label + 8);
        let dx = lh(m, frame + 0x10);
        let y = lh(m, label + 0xa);
        let dy = lh(m, frame + 0x12);
        external(
            m,
            SET_VISUAL_OFFSET,
            [index + 4, x.wrapping_add(dx), y.wrapping_add(dy), 0],
        );
    }
    m.pop_frame(0x30);
}

/// 800EBC14: move the camera one frame along its heading while its
/// duration lasts. Returns 1 when it moved, else 0.
pub fn tick_camera(m: &mut Machine) -> u32 {
    let state = briefing(m);
    if (m.lw(state + CAMERA_DURATION) as i32) <= 0 {
        return 0;
    }
    m.push_frame(0x20);
    let angle = lh(m, state + CAMERA_ANGLES + 2);
    let sine = external(m, RSIN, [angle, 0, 0, 0]);
    let state = briefing(m);
    let speed = m.lw(state + CAMERA_SPEED);
    let dy = step(speed, sine);
    let angle = lh(m, state + CAMERA_ANGLES + 2);
    let cosine = external(m, RCOS, [angle, 0, 0, 0]);
    let state = briefing(m);
    let speed = m.lw(state + CAMERA_SPEED);
    let dx = step(speed, cosine);
    let x = m.lw(state + CAMERA);
    let duration = m.lw(state + CAMERA_DURATION);
    m.sw(state + CAMERA, x.wrapping_add(dy));
    let z = m.lw(state + CAMERA + 8);
    m.sw(state + CAMERA_DURATION, duration.wrapping_sub(1));
    m.sw(state + CAMERA + 8, z.wrapping_add(dx));
    m.pop_frame(0x20);
    1
}

// ---- label navigation -------------------------------------------------------------

/// 800EDD28: move the selection right (`pressed & 8`) or left (`& 4`),
/// wrapping; the old label fades out (sound 0x65), the new one is selected.
/// Returns the new label's address.
pub fn navigate(m: &mut Machine, pressed: u32) -> u32 {
    m.push_frame(0x18);
    let state = briefing(m);
    let selected = m.lw(state + SELECTED);
    let old = state + LABELS + selected.wrapping_mul(LABEL_SIZE);
    let flags = m.lw(old);
    if flags & 4 != 0 {
        m.sw(old, (flags & !4) | 0x20);
        sound(m, 0x65);
    } else if flags & 0x40 != 0 {
        m.sw(old, flags & !0x42);
    } else {
        let flags = m.lw(old);
        sh(m, old + 6, 0x19);
        m.sw(old, flags | 0x20);
        sound(m, 0x65);
    }
    if pressed & 8 != 0 {
        let state = briefing(m);
        let last = m.lw(state + LABEL_COUNT).wrapping_sub(1);
        let selected = m.lw(state + SELECTED);
        let next = if slt(selected, last) {
            selected.wrapping_add(1)
        } else {
            0
        };
        m.sw(state + SELECTED, next);
    } else if pressed & 4 != 0 {
        let state = briefing(m);
        let selected = m.lw(state + SELECTED);
        let next = if (selected as i32) > 0 {
            selected.wrapping_sub(1)
        } else {
            m.lw(state + LABEL_COUNT).wrapping_sub(1)
        };
        m.sw(state + SELECTED, next);
    }
    let state = briefing(m);
    let selected = m.lw(state + SELECTED);
    let new = state + LABELS + selected.wrapping_mul(LABEL_SIZE);
    let flags = m.lw(new);
    if flags & 0x20 != 0 {
        m.sw(new, (flags & !0x20) | 4);
    } else {
        m.sw(new, flags | 0x42);
        sh(m, new + 6, 0);
        sh(m, new + 0xa, 0);
        sh(m, new + 8, 0);
    }
    m.pop_frame(0x18);
    new
}

/// 800EDE98: pan the camera to object `index` over 50 frames.
pub fn aim_camera(m: &mut Machine, index: u32) {
    m.push_frame(0x20);
    let state = briefing(m);
    m.sw(state + CAMERA_DURATION, 0x32);
    let object = object(state, index);
    let camera_x = m.lw(state + CAMERA);
    let dy = lh(m, object + 0xe).wrapping_sub(camera_x);
    let squares = mul(dy, dy);
    let z = lh(m, object + 0x12);
    let camera_z = m.lw(state + CAMERA + 8);
    let dx = z.wrapping_sub(camera_z);
    let squares = squares.wrapping_add(mul(dx, dx));
    let distance = external(m, SQUARE_ROOT0, [squares, 0, 0, 0]);
    let state = briefing(m);
    let duration = m.lw(state + CAMERA_DURATION);
    let speed = div(m, distance, duration, 0x800e_df18);
    m.sw(state + CAMERA_SPEED, speed);
    let heading = external(m, RATAN2, [dy, dx, 0, 0]);
    let state = briefing(m);
    sh(m, state + CAMERA_ANGLES + 2, heading);
    m.pop_frame(0x20);
}

// ---- phase steps (table 0x800F68B8) ------------------------------------------------
//
// Each returns the step result of 800EF01C: 2 next substate, 1 next phase,
// 4 leave forwards (0x100), 8 leave backwards (0x200), 0x10 jump to phase 3.

/// 800EDF78 (0,0): full zoom-out view, no offsets. Returns 2.
pub fn phase_reset_view(m: &mut Machine) -> u32 {
    let state = briefing(m);
    m.sw(state + ZOOM, 0x7fff);
    sh(m, state + VIEW_ANGLES + 2, 0);
    sh(m, state + VIEW_ANGLES, 0);
    m.sw(state + ZOOM_TIMER, 0);
    sh(m, state + VIEW_OFFSETS + 2, 0);
    sh(m, state + VIEW_OFFSETS, 0);
    2
}

/// 800EDFA4 (0,1): slide the two panels in (sounds 0x5A, 0x5C, 100);
/// CROSS skips. Returns 2 when done.
pub fn phase_open_panels(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    if pressed(m) & 0x10 != 0 {
        let state = briefing(m);
        m.sw(state + PANEL_TIMER, 0x32);
    }
    let state = briefing(m);
    if m.lw(state + PANEL_TIMER) == 0 {
        sound(m, 0x5a);
    }
    let state = briefing(m);
    let timer = m.lw(state + PANEL_TIMER);
    let result = if slt(timer, 0x19) {
        let timer = timer.wrapping_add(1);
        let left = 0x19u32.wrapping_sub(timer);
        let y = mul(0u32.wrapping_sub(left), left);
        m.sw(state + PANEL_TIMER, timer);
        external(m, SET_VISUAL_OFFSET, [3, 0, y, 0]);
        let state = briefing(m);
        sh(m, state + PANEL_OFFSETS + 2, y);
        0
    } else if slt(timer, 0x32) {
        if timer == 0x19 {
            sound(m, 0x5c);
        }
        let state = briefing(m);
        let timer = m.lw(state + PANEL_TIMER).wrapping_add(1);
        let left = 0x32u32.wrapping_sub(timer);
        let y = mul(left, left);
        m.sw(state + PANEL_TIMER, timer);
        external(m, SET_VISUAL_OFFSET, [2, 0, y, 0]);
        external(m, SET_VISUAL_OFFSET, [3, 0, 0, 0]);
        let state = briefing(m);
        sh(m, state + PANEL_OFFSETS, y);
        sh(m, state + PANEL_OFFSETS + 2, 0);
        0
    } else {
        m.sw(state + PANEL_TIMER, 0);
        external(m, SET_VISUAL_OFFSET, [2, 0, 0, 0]);
        external(m, SET_VISUAL_OFFSET, [3, 0, 0, 0]);
        let state = briefing(m);
        sh(m, state + PANEL_OFFSETS, 0);
        sh(m, state + PANEL_OFFSETS + 2, 0);
        external(m, SET_VISUAL_STYLE, [9, 4, 0, 0]);
        sound(m, 100);
        2
    };
    m.pop_frame(0x20);
    result
}

/// 800EE128 (0,2): grow visual slots 9/10 over 25 frames (style of slot 10
/// = timer / 5), then sound 0x66. Returns 2 when done.
pub fn phase_zoom_in(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let state = briefing(m);
    let timer = m.lw(state + ZOOM_TIMER);
    external(m, SET_VISUAL_PARAMETER, [9, timer, 0, 0]);
    external(m, SET_VISUAL_PARAMETER, [0xa, timer, 0, 0]);
    // timer / 5: mult by 0x66666667, mfhi, sra 1, minus the sign.
    let fifth = sra(mulhi(timer, 0x6666_6667), 1).wrapping_sub(sra(timer, 31));
    external(m, SET_VISUAL_STYLE, [0xa, fifth, 0, 0]);
    let result = if !slt(timer, 0x19) {
        let state = briefing(m);
        m.sw(state + ZOOM_TIMER, 0);
        external(m, SET_VISUAL_STYLE, [9, 0, 0, 0]);
        external(m, SET_VISUAL_STYLE, [0xb, 0, 0, 0]);
        sound(m, 0x66);
        2
    } else {
        let state = briefing(m);
        let timer = m.lw(state + ZOOM_TIMER).wrapping_add(1);
        m.sw(state + ZOOM_TIMER, timer);
        0
    };
    m.pop_frame(0x20);
    result
}

/// 800EE1E4 (0,3): the 25-frame fly-in: scene on, zoom, view angles, fade
/// color of visual slot 120 and view offsets towards the camera. Returns 1
/// when done.
pub fn phase_fly_in(m: &mut Machine) -> u32 {
    m.push_frame(0x28);
    let state = briefing(m);
    let t = m.lw(state + ZOOM_TIMER);
    // 0x6DA3 * t, built as ((7t * 8 - t) * 4 + t) * 128 - that.
    let seven = (t << 3).wrapping_sub(t);
    let fifty_five = (seven << 3).wrapping_sub(t);
    let product = (fifty_five << 2).wrapping_add(t);
    let scaled = (product << 7).wrapping_sub(product);
    let flags = m.lw(state + FLAGS);
    m.sw(state + FLAGS, flags | 1);
    let remaining = 0x19u32.wrapping_sub(t);
    m.sw(state + ZOOM, 0x7fffu32.wrapping_sub(div25(scaled)));
    let pitch = div25(0u32.wrapping_sub(t << 9)).wrapping_add(0x400);
    sh(m, state + VIEW_ANGLES, pitch);
    let yaw = div25(t << 8).wrapping_add(0x800);
    sh(m, state + VIEW_ANGLES + 2, yaw);
    let color = div25(remaining << 7);
    set_visual_color(m, 0x78, color, color, color);
    let state = briefing(m);
    let x = div_pow2(m.lw(state + CAMERA), 7);
    let x = div_pow2(x.wrapping_sub(0x80), 2);
    sh(m, state + VIEW_OFFSETS, div25(mul(x, remaining)));
    let z = div_pow2(m.lw(state + CAMERA + 8), 7);
    let z = half(0x80u32.wrapping_sub(z));
    sh(m, state + VIEW_OFFSETS + 2, div25(mul(z, remaining)));
    let result = if !slt(t, 0x19) {
        1
    } else {
        let timer = m.lw(state + ZOOM_TIMER).wrapping_add(1);
        m.sw(state + ZOOM_TIMER, timer);
        0
    };
    m.pop_frame(0x28);
    result
}

/// Run the command at the cursor (table 0x800F6864) and advance the cursor.
/// Returns the cursor's previous value.
fn run_command(m: &mut Machine) -> u32 {
    let state = briefing(m);
    let command = m.lw(state + CURSOR);
    let opcode = lbu(m, command + 2);
    let routine = m.lw(COMMANDS + (opcode << 2));
    call_pointer(m, routine, [command, 0, 0, 0]);
    let state = briefing(m);
    let command = m.lw(state + CURSOR);
    m.sw(state + CURSOR, command.wrapping_add(0x10));
    command
}

/// 800EE3B8 (1,0): the timed script. Commands due at the clock run (an
/// opcode-0 command next ends the script: flag 0x20). TRIANGLE (0x20)
/// cancels (0x10); START (0x40), or CROSS once the text is done,
/// fast-forwards: every remaining command runs with the movement ticks in
/// between (sound 0x3C) and the finish phase follows. Otherwise the clock
/// advances, the text pages (95 frames per line, 15 frames between pages of
/// three lines, CROSS skips) and the visible lines are drawn.
pub fn phase_script(m: &mut Machine) -> u32 {
    let frame = m.push_frame(0x40);
    let result = script_step(m, frame);
    m.pop_frame(0x40);
    result
}

fn script_step(m: &mut Machine, frame: u32) -> u32 {
    let state = briefing(m);
    if m.lw(state + FLAGS) & 0x20 == 0 {
        loop {
            let state = briefing(m);
            let cursor = m.lw(state + CURSOR);
            if lh(m, cursor) != m.lw(state + CLOCK) {
                break;
            }
            run_command(m);
            let state = briefing(m);
            let next = m.lw(state + CURSOR);
            if lbu(m, next + 2) == 0 {
                let flags = m.lw(state + FLAGS);
                m.sw(state + FINISH_TIMER, 0);
                m.sw(state + FLAGS, flags | 0x20);
            }
        }
    }
    let buttons = pressed(m);
    if buttons & 0x20 != 0 {
        let state = briefing(m);
        let flags = m.lw(state + FLAGS);
        m.sw(state + FLAGS, flags | 4);
        return 0x10;
    }
    let fast = buttons & 0x40 != 0 || {
        buttons & 0x10 != 0 && {
            let state = briefing(m);
            m.lw(state + FLAGS) & 0x10 != 0
        }
    };
    if fast {
        let state = briefing(m);
        if m.lw(state + FLAGS) & 0x20 != 0 {
            return 2;
        }
        sound(m, 0x3c);
        let state = briefing(m);
        let flags = m.lw(state + FLAGS);
        let mut clock = m.lw(state + CLOCK);
        m.sw(state + FLAGS, flags | 2);
        loop {
            let state = briefing(m);
            let cursor = m.lw(state + CURSOR);
            if lh(m, cursor) == clock {
                loop {
                    let command = run_command(m);
                    if lbu(m, command + 0x12) == 0 {
                        let state = briefing(m);
                        m.sw(state + FINISH_TIMER, 0);
                        return 2;
                    }
                    if lh(m, command + 0x10) != clock {
                        break;
                    }
                }
            }
            clock = clock.wrapping_add(1);
            tick_objects(m);
            tick_labels(m);
            tick_camera(m);
        }
    }
    let state = briefing(m);
    if m.lw(state + FLAGS) & 0x30 == 0x30 {
        return 2;
    }
    let clock = m.lw(state + CLOCK).wrapping_add(1);
    let flags = m.lw(state + FLAGS);
    m.sw(state + CLOCK, clock);
    if flags & 0x10 == 0 {
        if flags & 8 != 0 {
            // Page full: 15 frames, then the next page.
            if pressed(m) & 0x10 != 0 {
                m.sw(state + TEXT_TIMER, 0xf);
            }
            let state = briefing(m);
            let timer = m.lw(state + TEXT_TIMER);
            if slt(timer, 0xf) {
                m.sw(state + TEXT_TIMER, timer.wrapping_add(1));
            } else {
                let index = m.lw(state + TEXT_INDEX).wrapping_add(1);
                m.sw(state + TEXT_INDEX, index);
                let flags = m.lw(state + FLAGS);
                m.sw(state + TEXT_TIMER, 0);
                m.sw(state + FLAGS, flags & !8);
            }
        } else {
            if pressed(m) & 0x10 != 0 {
                m.sw(state + TEXT_TIMER, 0x5f);
            }
            let state = briefing(m);
            let timer = m.lw(state + TEXT_TIMER);
            if slt(timer, 0x5f) {
                m.sw(state + TEXT_TIMER, timer.wrapping_add(1));
            } else {
                let last = m.lw(state + TEXT_COUNT).wrapping_sub(1);
                let index = m.lw(state + TEXT_INDEX);
                m.sw(state + TEXT_TIMER, 0);
                if slt(index, last) {
                    // index / 3: mult by 0x55555556, mfhi, minus the sign.
                    let third = mulhi(index, 0x5555_5556).wrapping_sub(sra(index, 31));
                    if (third << 1).wrapping_add(third) != index.wrapping_sub(2) {
                        m.sw(state + TEXT_INDEX, index.wrapping_add(1));
                    } else {
                        let flags = m.lw(state + FLAGS);
                        m.sw(state + FLAGS, flags | 8);
                    }
                } else {
                    let flags = m.lw(state + FLAGS);
                    m.sw(state + FLAGS, flags | 0x10);
                }
            }
        }
    }
    let state = briefing(m);
    if m.lw(state + FLAGS) & 8 != 0 {
        return 0;
    }
    let index = m.lw(state + TEXT_INDEX);
    let third = mulhi(index, 0x5555_5556).wrapping_sub(sra(index, 31));
    let row = index.wrapping_sub((third << 1).wrapping_add(third));
    let lines = row.wrapping_add(1);
    let first = index.wrapping_sub(row);
    let mut y = 0x140u32;
    let mut line = 0u32;
    while slt(line, lines) {
        let base = work(m).wrapping_add(WORK_HIGH);
        let table = m.lw(base + 0xb8);
        let text = text_line(m, 2, first.wrapping_add(line), table);
        line = line.wrapping_add(1);
        draw_text(m, frame, 0x10, text, 0x19, y);
        y = y.wrapping_add(0x14);
    }
    0
}

/// The compiled call of 800D04C8 (font `font`, string `text`, at `x`/`y`,
/// color 0x808080) with the work block's ordering table and primitive
/// cursor; stores the returned cursor.
pub(crate) fn draw_text(m: &mut Machine, frame: u32, font: u32, text: u32, x: u32, y: u32) {
    let block = work(m);
    m.sw(frame + 0x10, x);
    m.sw(frame + 0x14, y);
    m.sw(frame + 0x18, 0x0080_8080);
    let table = m.lw(block + 0x90);
    let cursor = m.lw(block + WORK_HIGH + 0x94);
    let next = external(m, DRAW_TEXT, [font, text, table, cursor]);
    let block = work(m);
    m.sw(block + WORK_HIGH + 0x94, next);
}

/// 800EE860 (1,1): wait 25 frames after the script ends; CROSS skips
/// (sound 0x3C). Returns 2 when done.
pub fn phase_finish(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let result = if pressed(m) & 0x10 != 0 {
        sound(m, 0x3c);
        2
    } else {
        let state = briefing(m);
        let timer = m.lw(state + FINISH_TIMER);
        if !slt(timer, 0x19) {
            2
        } else {
            m.sw(state + FINISH_TIMER, timer.wrapping_add(1));
            0
        }
    };
    m.pop_frame(0x18);
    result
}

/// 800EE8D0 (1,2): every shown label fades out (sound 0x65 each). Returns 2.
pub fn phase_hide_labels(m: &mut Machine) -> u32 {
    m.push_frame(0x28);
    for index in 0..8 {
        let label = label(m, index);
        if m.lw(label) & 2 != 0 {
            let flags = m.lw(label);
            sh(m, label + 6, 0x19);
            m.sw(label, flags | 0x20);
            sound(m, 0x65);
        }
    }
    m.pop_frame(0x28);
    2
}

/// 800EE95C (1,3): 1 once no label is fading out, else 0.
pub fn phase_labels_hidden(m: &mut Machine) -> u32 {
    let state = briefing(m);
    for index in 0..8 {
        if m.lw(state + LABELS + index * LABEL_SIZE) & 0x20 != 0 {
            return 0;
        }
    }
    1
}

/// 800EE9A0 (2,0): select the current label and center the camera on its
/// object. Returns 2.
pub fn phase_select_label(m: &mut Machine) -> u32 {
    let state = briefing(m);
    let flags = m.lw(state + FLAGS);
    let selected = m.lw(state + SELECTED);
    m.sw(state + FLAGS, flags & !2);
    let label = state + LABELS + selected.wrapping_mul(LABEL_SIZE);
    let label_flags = m.lw(label);
    let index = lbu(m, label + 5);
    sh(m, label + 6, 0);
    sh(m, label + 0xa, 0);
    sh(m, label + 8, 0);
    m.sw(label, label_flags | 0x42);
    let object = object(state, index);
    let x = lh(m, object + 0xe);
    m.sw(state + CAMERA + 4, 0);
    m.sw(state + CAMERA, x);
    let z = lh(m, object + 0x12);
    m.sw(state + CAMERA + 8, z);
    2
}

/// 800EEA20 (2,1): the label map: localized title (string 9), mission
/// name, selected label name and the camera grid position. TRIANGLE
/// (sound 0x3B, cancel) or CROSS (sound 0x3C) leaves (0x10); LEFT/RIGHT
/// move the selection and pan the camera (sound 0x3D).
pub fn phase_map(m: &mut Machine) -> u32 {
    let frame = m.push_frame(0x90);
    let buffer = frame + 0x20;
    let block = work(m);
    let table = m.lw(block + WORK_HIGH + 0xb4);
    let title = external(m, TEXT_STRING, [9, table, 0, 0]);
    draw_text(m, frame, 0x10, title, 0x19, 0x140);
    let context = m.gw(CONTEXT);
    let mission = lh(m, context + 0x390);
    let entry = 0x800f_5958u32.wrapping_add(mul(mission, 0xc));
    let name = m.lw(entry + 8);
    external(m, SPRINTF, [buffer, 0x800c_b394, name, 0]);
    draw_text(m, frame, 0x10, buffer, 0x73, 0x140);
    let block = work(m);
    let names = m.lw(block + WORK_HIGH + 0xbc);
    let state = briefing(m);
    let selected = m.lw(state + SELECTED);
    let name = m.lw(names.wrapping_add(selected << 2));
    draw_text(m, frame, 0x10, name, 0x19, 0x154);
    let state = briefing(m);
    let x = div_pow2(m.lw(state + CAMERA), 7).wrapping_add(0x80);
    let z = div_pow2(m.lw(state + CAMERA + 8), 7).wrapping_add(0x80);
    external(m, SPRINTF, [buffer, 0x800c_b39c, sext16(x), sext16(z)]);
    spaces_to_gaps(m, buffer);
    draw_text(m, frame, 0x10, buffer, 0x19, 0x168);
    let buttons = pressed(m);
    let result = if buttons & 0x20 != 0 {
        let state = briefing(m);
        let flags = m.lw(state + FLAGS);
        m.sw(state + FLAGS, flags | 4);
        sound(m, 0x3b);
        0x10
    } else if buttons & 0x10 != 0 {
        sound(m, 0x3c);
        0x10
    } else if buttons & 0xc != 0 {
        let label = navigate(m, buttons);
        let index = lbu(m, label + 5);
        aim_camera(m, index);
        sound(m, 0x3d);
        0
    } else {
        0
    };
    m.pop_frame(0x90);
    result
}

/// The panel-closing step shared by 800EEC90 (2,2) and 800EEDBC (3,1):
/// while the panel timer runs, slots 3/2 move by -(25-t)^2 / (25-t)^2;
/// at 0, 4 (forwards) or 8 (cancelled). `store_first` selects 800EEDBC's
/// order (panel offsets stored before the slot calls).
fn close_panels(m: &mut Machine, store_first: bool) -> u32 {
    m.push_frame(0x20);
    let state = briefing(m);
    let timer = m.lw(state + PANEL_TIMER);
    let result = if (timer as i32) <= 0 {
        if m.lw(state + FLAGS) & 4 != 0 { 8 } else { 4 }
    } else {
        let left = 0x19u32.wrapping_sub(timer);
        let up = mul(0u32.wrapping_sub(left), left);
        let down = mul(left, left);
        if store_first {
            sh(m, state + PANEL_OFFSETS + 2, up);
            sh(m, state + PANEL_OFFSETS, down);
            external(m, SET_VISUAL_OFFSET, [3, 0, up, 0]);
            external(m, SET_VISUAL_OFFSET, [2, 0, down, 0]);
            let state = briefing(m);
            let timer = m.lw(state + PANEL_TIMER);
            m.sw(state + PANEL_TIMER, timer.wrapping_sub(1));
        } else {
            external(m, SET_VISUAL_OFFSET, [3, 0, up, 0]);
            external(m, SET_VISUAL_OFFSET, [2, 0, down, 0]);
            let state = briefing(m);
            let timer = m.lw(state + PANEL_TIMER);
            sh(m, state + PANEL_OFFSETS + 2, up);
            sh(m, state + PANEL_OFFSETS, down);
            m.sw(state + PANEL_TIMER, timer.wrapping_sub(1));
        }
        0
    };
    m.pop_frame(0x20);
    result
}

/// 800EEC90 (2,2): close the panels (see [`close_panels`]).
pub fn phase_close_panels(m: &mut Machine) -> u32 {
    close_panels(m, false)
}

/// 800EED50 (3,0): start closing (panel timer 25, sound 0x5B); every shown
/// label fades out. Returns 2.
pub fn phase_cancel(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let state = briefing(m);
    m.sw(state + PANEL_TIMER, 0x19);
    sound(m, 0x5b);
    let state = briefing(m);
    for index in 0..8 {
        let label = state + LABELS + index * LABEL_SIZE;
        let flags = m.lw(label);
        if flags & 2 != 0 {
            m.sw(label, flags | 0x20);
        }
    }
    m.pop_frame(0x18);
    2
}

/// 800EEDBC (3,1): close the panels, then leave (see [`close_panels`]).
pub fn phase_close(m: &mut Machine) -> u32 {
    close_panels(m, true)
}

// ---- sub-mode entry points (task table 0x800F1DD4, mode 1) -----------------------

/// 800EEE70: briefing initialization: script from the work block, line
/// count of text table 2, panels at +-625, objects/labels/camera reset, the
/// time -1 prefix of the script, visual page 3, slots 9..11, slot 120
/// color 0x808080, stream 0x827 and the fade-in. Returns 1.
pub fn initialize(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let block = work(m).wrapping_add(WORK_HIGH);
    let state = briefing(m);
    let script = m.lw(block + 0x9c);
    m.sw(state + CLOCK, 0);
    m.sw(state + LABEL_COUNT, 0);
    m.sw(state + FLAGS, 0);
    m.sw(state + TEXT_TIMER, 0);
    m.sw(state + TEXT_INDEX, 0);
    m.sw(state + CURSOR, script);
    let table = m.lw(block + 0xb8);
    let lines = text_line_count(m, 2, table);
    let state = briefing(m);
    m.sw(state + TEXT_COUNT, lines);
    sh(m, state + PANEL_OFFSETS, 0x271);
    m.sw(state + PANEL_TIMER, 0);
    sh(m, state + PANEL_OFFSETS + 2, 0xfd8f);
    sh(m, state + SUBSTATE, 0);
    sh(m, state + PHASE, 0);
    m.sw(state + MARKER_ID, 0xffff_ffff);
    reset_objects(m);
    reset_labels(m);
    reset_camera(m);
    let state = briefing(m);
    let cursor = m.lw(state + CURSOR);
    if lh(m, cursor) == 0xffff_ffff {
        loop {
            let command = run_command(m);
            if lh(m, command + 0x10) != 0xffff_ffff {
                break;
            }
        }
    }
    external(m, SELECT_VISUAL_PAGE, [3, 0, 0, 0]);
    external(m, SET_VISUAL_PARAMETER, [9, 0, 0, 0]);
    external(m, SET_VISUAL_PARAMETER, [0xa, 0, 0, 0]);
    set_visual_color(m, 0x78, 0x80, 0x80, 0x80);
    external(m, CLEAR_TEXT, [0, 0, 0, 0]);
    external(m, SET_VISUAL_STYLE, [9, 1, 0, 0]);
    external(m, SET_VISUAL_STYLE, [0xb, 1, 0, 0]);
    external(m, ENTER_BRIEFING, [0, 0, 0, 0]);
    external(m, STREAM_START, [0x827, 0, 0, 0]);
    external(m, STREAM_PLAY, [0, 0, 0, 0]);
    external(m, FADE, [0x1e, 0xa, 0, 0]);
    m.pop_frame(0x20);
    1
}

// ---- small helpers reached only from the briefing --------------------------------

/// The language's section `section` of a localized text table (800D0B74
/// and 800D0BD8 share this): `table + (word[table + 4 + language * 4] &
/// !3)`, then `+ (word[that + 4 + section * 4] & !3)`. The language is
/// context +0x38C.
fn text_section(m: &mut Machine, section: u32, table: u32) -> u32 {
    let context = m.gw(CONTEXT);
    let language = m.lw(context + 0x38c);
    let offset = m.lw(table.wrapping_add(language << 2).wrapping_add(4));
    let languages = table.wrapping_add((offset >> 2) << 2);
    let offset = m.lw((section << 2).wrapping_add(languages).wrapping_add(4));
    languages.wrapping_add((offset >> 2) << 2)
}

/// 800D0B74: the address of line `line` of section `section` of text table
/// `table` in the current language.
pub fn text_line(m: &mut Machine, section: u32, line: u32, table: u32) -> u32 {
    let lines = text_section(m, section, table);
    let offset = m.lw((line << 2).wrapping_add(lines).wrapping_add(4));
    lines.wrapping_add((offset >> 2) << 2)
}

/// 800D0BD8: the line count (first word) of section `section` of text
/// table `table` in the current language.
pub fn text_line_count(m: &mut Machine, section: u32, table: u32) -> u32 {
    let lines = text_section(m, section, table);
    m.lw(lines)
}

/// 800D0F60: replace every space of the string with byte 7 (a narrow gap
/// glyph).
pub fn spaces_to_gaps(m: &mut Machine, text: u32) {
    m.push_frame(0x18);
    let length = external(m, STRLEN, [text, 0, 0, 0]);
    let mut index = 0u32;
    while slt(index, length) {
        let at = text.wrapping_add(index);
        if lbu(m, at) == 0x20 {
            sb(m, at, 7);
        }
        index = index.wrapping_add(1);
    }
    m.pop_frame(0x18);
}

/// 800F1788: color (`r`, `g`, `b`) of visual slot `slot` in the visual
/// block (`[0x800F9E14]` + slot * 4 + 0x98). Returns the slot address
/// minus 0x98.
pub fn set_visual_color(m: &mut Machine, slot: u32, r: u32, g: u32, b: u32) -> u32 {
    let offset = slot << 2;
    let base = m.lw(0x800f_9e14).wrapping_add(offset);
    sb(m, base + 0x98, r);
    let base = m.lw(0x800f_9e14).wrapping_add(offset);
    sb(m, base + 0x99, g);
    let base = m.lw(0x800f_9e14).wrapping_add(offset);
    sb(m, base + 0x9a, b);
    base
}
