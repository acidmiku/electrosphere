//! Frame-tail session step 800371C4 (called by 80033DC0) and its tree, the
//! pause path of 80033EF0 (80034718, PAL 80034640, 8002C6F4) and the alternate
//! flight update routines that the other flight states hand to 800339E0.
//!
//! Translated from the original instructions in their original order.
//! Routines owned by other modules (script, tail, loading, shared, SDK) are
//! called by original address through [`dispatch::call`]; every routine
//! translated here is claimed by [`dispatch`]. Stack-passed arguments of
//! such calls are written to the caller's guest frame (`[sp+0x10]`, ...).
use crate::{
    common::{CONTEXT, ENTITIES, GAMEPLAY, request_transition},
    dispatch,
    machine::{Fault, GP, Machine, SmallData},
    sdk,
};

/// `[gp+0x48]`: sound sequencer control block.
const SOUND: u32 = 0x48;
/// `[gp+0x520]`: pad state (word 0 = buttons).
const PAD: u32 = 0x520;
/// `[gp+0x460]`: radio indicator state byte.
const INDICATOR: u32 = GP + 0x460;
/// 800C7170: radio indicator block (80041900 resets it).
const INDICATOR_BLOCK: u32 = 0x800c_7170;
/// `[0x800BD094]`: camera request block.
const CAMERA_REQUEST: u32 = 0x800b_d094;
/// `[0x800BD098]`: flight state block.
const FLIGHT_STATE: u32 = 0x800b_d098;
/// 800248E0: jump table of 8009423C (five cases).
const SPAWN_CASES: u32 = 0x8002_48e0;

fn call(m: &mut Machine, address: u32, args: [u32; 4]) -> u32 {
    dispatch::call(m, address, args)
}
/// `~[context] & 0x20000000`: false while the game is paused.
fn running(m: &mut Machine) -> bool {
    let context = m.gw(CONTEXT);
    !m.lw(context) & 0x2000_0000 != 0
}
fn player_entity(m: &mut Machine) -> u32 {
    let entities = m.gw(ENTITIES);
    m.lw(entities)
}

// ---- routines owned by other modules (called by original address) ----------

/// 8002C71C (sound sequencer flush; called with a0 = 0).
fn sound_flush(m: &mut Machine) {
    call(m, 0x8002_c71c, [0; 4]);
}
/// 800793C4 (camera target).
fn camera_target(m: &mut Machine, camera: u32, mode: u32, target: u32) {
    call(m, 0x8007_93c4, [camera, mode, target, 0]);
}

// ---- radio messages and indicators ------------------------------------------

/// 8006CB30: current pad buttons `[[gp+0x520]]`.
pub fn pad_buttons(m: &mut Machine) -> u32 {
    let pad = m.gw(PAD);
    m.lw(pad)
}

/// 80041654: indicator byte = 1 unless it is 2.
pub fn indicator_on(m: &mut Machine) {
    if m.lbu(INDICATOR) != 2 {
        m.sb(INDICATOR, 1);
    }
}

/// 80041670: indicator byte.
pub fn indicator(m: &mut Machine) -> u32 {
    u32::from(m.lbu(INDICATOR))
}

/// 8004167C: indicator byte 2 becomes 3.
pub fn indicator_done(m: &mut Machine) {
    if m.lbu(INDICATOR) == 2 {
        m.sb(INDICATOR, 3);
    }
}

/// 8004197C: clear the 800C7170 halfword; if its byte +2 is set, set it to 3
/// (`mode` 1) or 0 and byte +3 to 0xFF.
pub fn indicator_close(m: &mut Machine, mode: u32) {
    let active = m.lbu(INDICATOR_BLOCK + 2);
    m.sh(INDICATOR_BLOCK, 0);
    if active == 0 {
        return;
    }
    m.sb(INDICATOR_BLOCK + 2, if mode == 1 { 3 } else { 0 });
    m.sb(INDICATOR_BLOCK + 3, 0xff);
}

// JP-REVIEW(R0475): changed function: PAL 80054E10 -> JP 8005628C, call+:2 del:6 imm:8 ins:28 op:10 reg:16, 68 instructions differ (whole routine)
/// 8005628C: start queued radio messages whose wait elapsed, tick the active
/// sequence and settle the radio state and indicator.
// JP-PATCH: replaced by `crate::jp::res_p1_session::radio_update` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn radio_update(m: &mut Machine) {
    crate::jp::res_p1_session::radio_update(m)
}

// JP-REVIEW(R0476): changed function: PAL 80055044 -> JP 80056518, call+:3 imm:1 ins:15 op:5, 21 instructions differ (whole routine)
/// 80056518: stop the radio: no sequence, all four slots free, indicator
/// closed. Returns 0.
// JP-PATCH: replaced by `crate::jp::res_p1_session::radio_reset` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn radio_reset(m: &mut Machine) -> u32 {
    crate::jp::res_p1_session::radio_reset(m)
}

// ---- sound sequencer commands -------------------------------------------------

/// 8002C6F4: sequencer command 6 with argument `pause` (pause on/off);
/// returns 1.
pub fn sound_pause(m: &mut Machine, pause: u32) -> u32 {
    let sound = m.gw(SOUND);
    m.sb(sound + 0x14c, 6);
    let current = m.gw(SOUND);
    m.sw(sound + 0x148, 1);
    m.sw(current + 0x150, pause);
    m.sw(current + 0x15c, 0);
    m.sb(current + 0x14d, 0);
    1
}

/// 80056A10: flush the sequencer and queue 800B2328.
pub fn sound_queue_80055184(m: &mut Machine) {
    sound_flush(m);
    call(m, 0x8002_c8d8, [0x800b_2328, 0, 0, 0]);
}

/// 800304CC: 8002F620; returns 0.
pub fn music_80030288(m: &mut Machine) -> u32 {
    call(m, 0x8002_f620, [0; 4]);
    0
}

// ---- camera requests ------------------------------------------------------------

fn camera_request(m: &mut Machine, mode: u32, argument: u32) {
    let request = m.lw(CAMERA_REQUEST);
    m.sw(request, mode);
    m.sw(request + 4, argument);
    m.sw(request + 8, 0);
}

/// 80076274: camera request (1, 1).
pub fn camera_74c04(m: &mut Machine) {
    camera_request(m, 1, 1);
}
/// 80076290: camera request (1, 0x1000000).
pub fn camera_74c20(m: &mut Machine) {
    camera_request(m, 1, 0x0100_0000);
}
/// 800762B0: camera request (1, 0x2000000).
pub fn camera_74c40(m: &mut Machine) {
    camera_request(m, 1, 0x0200_0000);
}
/// 800762D0: camera request (0x800, 4).
pub fn camera_74c60(m: &mut Machine) {
    camera_request(m, 0x800, 4);
}
/// 800762F0: keep the camera mode with bit 0x40 and argument 2 (`near` != 0)
/// or 0x400000.
pub fn camera_74c80(m: &mut Machine, near: u32) {
    let request = m.lw(CAMERA_REQUEST);
    let mode = m.lw(request);
    m.sw(request + 4, if near != 0 { 2 } else { 0x40_0000 });
    m.sw(request + 8, 0);
    m.sw(request, mode | 0x40);
}

/// 800794D8: unless the camera is locked (+0x50 bit 0x800), target the
/// player (mode 1, or 2 for `outside` != 0) and run 80035740(1).
pub fn camera_follow_player(m: &mut Machine, outside: u32) {
    let gameplay = m.gw(GAMEPLAY);
    let camera = m.lw(gameplay + 0x28);
    if m.lw(camera + 0x50) & 0x800 != 0 {
        return;
    }
    let mode = if outside == 0 { 1 } else { 2 };
    let target = player_entity(m);
    camera_target(m, camera, mode, target);
    call(m, 0x8003_5740, [1, 0, 0, 0]);
}

// JP-REVIEW(R0477): near function: PAL 80077ECC -> JP 8007953C, imm:1, 1 instructions differ (whole routine)
/// 8007953C: [`camera_follow_player`] with context byte +0x3A3.
// JP-PATCH: replaced by `crate::jp::res_p1_session::camera_follow_default` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn camera_follow_default(m: &mut Machine) {
    crate::jp::res_p1_session::camera_follow_default(m)
}

// ---- on-screen messages ---------------------------------------------------------

// ---- fade overlays (GTE depth-cued sprites) ------------------------------------

// JP-REVIEW(R0480): near function: PAL 80082F18 -> JP 80084560, imm:2, 2 instructions differ (whole routine)
/// 80084560: intro fade (level `level`, 0x1000 = black): a mask packet, a
/// 16-pixel CLUT upload whose last `level>>8` entries are 0x8000, then the faded
/// screen sprites; spliced in at OT slot `slot`. Returns the next packet.
// JP-PATCH: replaced by `crate::jp::res_p1_session::intro_fade` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn intro_fade(m: &mut Machine, slot: u32, packet: u32, level: u32) -> u32 {
    crate::jp::res_p1_session::intro_fade(m, slot, packet, level)
}

// JP-REVIEW(R0481): near function: PAL 80082E2C -> JP 80084474, imm:2, 2 instructions differ (whole routine)
/// 80084474: screen fade toward `level` (0: nothing; negative: subtractive
/// blend) spliced in at OT slot `slot`. Returns the next packet.
// JP-PATCH: replaced by `crate::jp::res_p1_session::screen_fade` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn screen_fade(m: &mut Machine, slot: u32, packet: u32, level: u32) -> u32 {
    crate::jp::res_p1_session::screen_fade(m, slot, packet, level)
}

// ---- mission objects ------------------------------------------------------------

/// 8009423C: spawn the 80089CD8 objects of group `group` (0..4, from the
/// 800248E0 jump table) through 8007FDB4, each with a random flag.
pub fn spawn_group(m: &mut Machine, group: u32) {
    if group >= 5 {
        return;
    }
    let target = m.lw((group << 2).wrapping_add(SPAWN_CASES));
    let (mut source, count) = match target {
        0x8009_4288 => (0x800b_445c, 7),
        0x8009_4298 => (0x800b_445c, 5),
        0x8009_42a8 => (0x800b_44b0, 5),
        0x8009_42b8 => (0x800b_44ec, 12),
        0x8009_42c8 => (0x800b_457c, 10),
        _ => {
            m.raise(Fault::Untranslated { address: target });
            return;
        }
    };
    for index in 0..count {
        let pool = m.gw(0x41c).wrapping_add(0x36c4);
        let object = call(m, 0x8007_fdb4, [pool, 1, 5, 0]);
        if object == 0 {
            return;
        }
        let random = sdk::rand(m);
        m.sh(object, ((index << 1) + (random & 1)) as u16);
        m.sh(object.wrapping_add(2), 0x7fff);
        let a = m.lw(source);
        m.sw(object.wrapping_add(4), a);
        let b = m.lw(source.wrapping_add(4));
        m.sw(object.wrapping_add(8), b);
        let c = m.lw(source.wrapping_add(8));
        m.sw(object.wrapping_add(0x10), 0x8008_9cd8);
        m.sw(object.wrapping_add(0xc), c);
        source = source.wrapping_add(0xc);
    }
}

/// 80076FA4: unless the camera request has bits 0x7000, decrement player
/// halfword +0x4E, then 80029588 and 8006CB44(2).
pub fn player_hit_75934(m: &mut Machine) {
    let request = m.lw(CAMERA_REQUEST);
    if m.lw(request) & 0x7000 != 0 {
        return;
    }
    let player = player_entity(m);
    let value = m.lhu(player + 0x4e).wrapping_sub(1);
    m.sh(player + 0x4e, value);
    call(m, 0x8002_9588, [0; 4]);
    call(m, 0x8006_cb44, [2, 0, 0, 0]);
}

// ---- mission logic (800540E0) -----------------------------------------------------

// JP-REVIEW(R0483): near function: PAL 80052AA0 -> JP 800540E0, call+:3 del:30 imm:16 ins:124 op:25 ref:1 reg:16, 212 instructions differ (whole routine)
/// 800540E0: per-frame mission logic: phase-2 intro spawns, phase-4 timers,
/// scripted triggers of the individual missions, the phase-0x800 clock and
/// the radio update.
// JP-PATCH: replaced by `crate::jp::res_p1_session::mission_logic` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn mission_logic(m: &mut Machine) {
    crate::jp::res_p1_session::mission_logic(m)
}

// ---- session step (800371C4) ------------------------------------------------------

// JP-REVIEW(R0490): near function: PAL 80036E54 -> JP 800371C4, call-:2 del:43 imm:53 ins:26 op:13, 135 instructions differ (whole routine)
/// 800371C4: per-frame session step (frame tail 80033DC0, unless replaying).
/// Runs the mission logic, then the phase-specific session handling:
/// intro (2), mission (4), result (8) and the 0x10/0x100000 endings.
/// Returns 0.
// JP-PATCH: replaced by `crate::jp::res_p1_session::session_step` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn session_step(m: &mut Machine) -> u32 {
    crate::jp::res_p1_session::session_step(m)
}

// ---- pause menu ---------------------------------------------------------------------

// JP-REVIEW(R0500): near function: PAL 800340D8 -> JP 800343C8, imm:2, 2 instructions differ (whole routine)
/// 800343C8: menu box at the guest RECT `rect` (x, y, w, h): four 8x8 corner
/// sprites, four edge tiles and three fill tiles in style `style` (1, 2 or
/// other), added to OT slot `slot`; returns `packet + 0xB0`.
// JP-PATCH: replaced by `crate::jp::res_p1_session::draw_box` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn draw_box(m: &mut Machine, slot: u32, packet: u32, rect: u32, style: u32) -> u32 {
    crate::jp::res_p1_session::draw_box(m, slot, packet, rect, style)
}

// JP-REVIEW(R0502): near function: PAL 80034428 -> JP 80034718, imm:5, 5 instructions differ (whole routine)
/// 80034718: pause menu input: move the cursor (0..4, wrapping), toggle the
/// two options, and on confirm resume (0), retry (3) or quit (4).
// JP-PATCH: replaced by `crate::jp::res_p1_session::pause_input` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn pause_input(m: &mut Machine) {
    crate::jp::res_p1_session::pause_input(m)
}

// ---- alternate flight update routines --------------------------------------------

/// 8006CB10: 8006C9B8.
pub fn pad_8006b340(m: &mut Machine) {
    call(m, 0x8006_c9b8, [0; 4]);
}

/// 8003307C: phase-0x10 update (state 80034000): 800396E8, then 8006CB10
/// and 8007DAC8 unless paused.
pub fn update_32de4(m: &mut Machine) {
    update_8003931c(m);
    if running(m) {
        pad_8006b340(m);
        call(m, 0x8007_dac8, [0; 4]);
    }
}

/// 800330CC: phase-0x80 update (state 80034040, replay): 8007F8E4, 8006CB10,
/// 8007DAC8, 800850D8.
pub fn update_32e34(m: &mut Machine) {
    update_8007e284(m);
    pad_8006b340(m);
    call(m, 0x8007_dac8, [0; 4]);
    call(m, 0x8008_50d8, [0; 4]);
}

/// The flight subsystem sequence shared with the regular update 80032F3C:
/// eight updates, then 8003A23C on a stack copy of the flight state's
/// target, whose results go to camera +0xBC and flight state +0x18.
fn flight_subsystems(m: &mut Machine, frame: u32) {
    let gameplay = m.gw(GAMEPLAY);
    let camera = m.lw(gameplay + 0x28);
    for routine in [
        0x8007_29dc,
        0x8004_5898,
        0x8003_8a3c,
        0x8005_17f4,
        0x8004_658c,
        0x8006_c5b4,
        0x8007_dac8,
        0x8008_50d8,
    ] {
        call(m, routine, [0; 4]);
    }
    let state = m.lw(FLIGHT_STATE);
    let target = m.lw(state + 0x1c);
    m.sw(frame + 0x10, target);
    call(m, 0x8003_a23c, [frame + 0x10, 0, 0, 0]);
    let value = m.lw(frame + 0x20);
    let state = m.lw(FLIGHT_STATE);
    m.sw(camera + 0xbc, value);
    let target = m.lw(frame + 0x24);
    m.sw(state + 0x18, target);
}

/// 80033104: phase-0x400 update (state 80034160): 8005C270, then the flight
/// subsystems unless paused or gameplay +4 has bit 0x1000.
pub fn update_32e6c(m: &mut Machine) {
    let frame = m.push_frame(0x30);
    update_8005a874(m);
    if running(m) {
        let gameplay = m.gw(GAMEPLAY);
        if !m.lw(gameplay + 4) & 0x1000 != 0 {
            flight_subsystems(m, frame);
        }
    }
    m.pop_frame(0x30);
}

/// 800331E0: phase-0x100000 update (state 800342AC): the flight subsystems
/// unless paused.
pub fn update_32f48(m: &mut Machine) {
    let frame = m.push_frame(0x30);
    if running(m) {
        flight_subsystems(m, frame);
    }
    m.pop_frame(0x30);
}

/// 8003329C: phase-0x200000 update (state 800342EC): nothing.
pub fn update_33004(_m: &mut Machine) {}

/// 800332A4: 80057BBC (called directly by state 8003423C, phase 0x4000).
pub fn update_3300c(m: &mut Machine) {
    update_800562b4(m);
}

/// 800332C4: 8005B124 (called directly by state 80034274, phase 0x20000).
pub fn update_3302c(m: &mut Machine) {
    update_8005971c(m);
}

// JP-REVIEW(R0503): near function: PAL 8003931C -> JP 800396E8, call+:2 imm:6 ins:28 op:1 reg:3, 38 instructions differ (whole routine)
/// 800396E8: phase-0x10 sequence player (gp+0x3EC..0x418 state).
// JP-PATCH: replaced by `crate::jp::res_p1_session::update_8003931c` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn update_8003931c(m: &mut Machine) {
    crate::jp::res_p1_session::update_8003931c(m)
}

// JP-REVIEW(R0506): near function: PAL 8007E284 -> JP 8007F8E4, del:8 imm:4 ins:2, 14 instructions differ (whole routine)
/// 8007F8E4: phase-0x80 (replay) control: 8007F448 in phase 0x80, 8007F808
/// in 0x200, otherwise 8007F5B4 and the replay exit checks.
// JP-PATCH: replaced by `crate::jp::res_p1_session::update_8007e284` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn update_8007e284(m: &mut Machine) {
    crate::jp::res_p1_session::update_8007e284(m)
}

/// 80057BBC: phase-0x4000 drawing: 800573E8 / 80057640 in phases 0x4000 /
/// 0x10000, otherwise clear and fill its own ordering table and draw it.
pub fn update_800562b4(m: &mut Machine) {
    let gameplay = m.gw(GAMEPLAY);
    let phase = m.lw(gameplay + 4);
    if phase == 0x4000 {
        call(m, 0x8005_73e8, [0; 4]);
        return;
    }
    if phase == 0x1_0000 {
        call(m, 0x8005_7640, [0; 4]);
        return;
    }
    let context = m.gw(CONTEXT);
    let buffer = m.lw(context + 4);
    let block = m.gw(0x4d0);
    let ot = block.wrapping_add((buffer << 3).wrapping_add(0xc));
    m.sw(block + 0x20, ot);
    let packets = m.lw(block.wrapping_add(buffer << 2) + 4);
    m.sw(block + 0x1c, packets);
    sdk::clear_otag_r(m, ot, 2);
    if call(m, 0x8005_7720, [0; 4]) != 0 {
        let gameplay = m.gw(GAMEPLAY);
        let stage = m.lw(gameplay + 8);
        request_transition(m, 0x1_0000, stage);
        return;
    }
    let screen = m.gw(0x218);
    if m.lw(screen + 4) != 0 {
        let block = m.gw(0x4d0);
        let ot = m.lw(block + 0x20);
        let packets = m.lw(block + 0x1c);
        let next = call(m, 0x8002_d3f4, [ot, packets, 0, 0]);
        let block = m.gw(0x4d0);
        m.sw(block + 0x1c, next);
    }
    let block = m.gw(0x4d0);
    let ot = m.lw(block + 0x20);
    sdk::draw_otag(m, ot.wrapping_add(4));
}

/// 8005B124: phase-0x20000 drawing: 8005AE8C / 8005B234 in phases 0x20000 /
/// 0x80000, otherwise clear and fill its own ordering table and draw it.
pub fn update_8005971c(m: &mut Machine) {
    let gameplay = m.gw(GAMEPLAY);
    let phase = m.lw(gameplay + 4);
    if phase == 0x2_0000 {
        call(m, 0x8005_ae8c, [0; 4]);
        return;
    }
    if phase == 0x8_0000 {
        call(m, 0x8005_b234, [0; 4]);
        return;
    }
    let context = m.gw(CONTEXT);
    let buffer = m.lw(context + 4);
    let block = m.gw(0x4dc);
    let ot = block.wrapping_add(buffer.wrapping_mul(0x14).wrapping_add(0xc));
    m.sw(block + 0x38, ot);
    let packets = m.lw(block.wrapping_add(buffer << 2) + 4);
    m.sw(block + 0x34, packets);
    sdk::clear_otag_r(m, ot, 5);
    if call(m, 0x8005_82f8, [0; 4]) != 0 {
        let gameplay = m.gw(GAMEPLAY);
        let stage = m.lw(gameplay + 8);
        request_transition(m, 0x8_0000, stage);
        return;
    }
    let screen = m.gw(0x22c);
    if m.lw(screen + 4) != 0 {
        let block = m.gw(0x4dc);
        let ot = m.lw(block + 0x38);
        let packets = m.lw(block + 0x34);
        let next = call(m, 0x8002_d3f4, [ot, packets, 0, 0]);
        let block = m.gw(0x4dc);
        m.sw(block + 0x34, next);
    }
    let block = m.gw(0x4dc);
    let ot = m.lw(block + 0x38);
    sdk::draw_otag(m, ot.wrapping_add(0x10));
}

// JP-REVIEW(R0507): changed function: PAL 8005A874 -> JP 8005C270, call-:3 del:31 imm:9 ins:4 op:8 reg:22, 74 instructions differ (whole routine)
/// 8005C270: phase-0x400 control: 8005BC64 / 8005BE0C in phases 0x400 /
/// 0x2000, nothing in 0x1000; otherwise pad shortcuts, the help overlay and
/// the exit conditions (transition 0x2000).
// JP-PATCH: replaced by `crate::jp::res_p1_session::update_8005a874` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn update_8005a874(m: &mut Machine) {
    crate::jp::res_p1_session::update_8005a874(m)
}

/// Native targets owned by this module, by original address.
#[allow(unused_variables)]
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let [a0, a1, a2, a3] = args;
    let v0 = match target {
        0x8003_71c4 => session_step(m),
        0x8005_40e0 => {
            mission_logic(m);
            0
        }
        0x8005_628c => {
            radio_update(m);
            0
        }
        0x8005_6518 => radio_reset(m),
        0x8004_1654 => {
            indicator_on(m);
            0
        }
        0x8004_1670 => indicator(m),
        0x8004_167c => {
            indicator_done(m);
            0
        }
        0x8004_197c => {
            indicator_close(m, a0);
            0
        }
        0x8006_cb30 => pad_buttons(m),
        0x8008_4560 => intro_fade(m, a0, a1, a2),
        0x8008_4474 => screen_fade(m, a0, a1, a2),
        0x8007_6274 => {
            camera_74c04(m);
            0
        }
        0x8007_6290 => {
            camera_74c20(m);
            0
        }
        0x8007_62b0 => {
            camera_74c40(m);
            0
        }
        0x8007_62d0 => {
            camera_74c60(m);
            0
        }
        0x8007_62f0 => {
            camera_74c80(m, a0);
            0
        }
        0x8007_94d8 => {
            camera_follow_player(m, a0);
            0
        }
        0x8007_953c => {
            camera_follow_default(m);
            0
        }
        0x8009_423c => {
            spawn_group(m, a0);
            0
        }
        0x8007_6fa4 => {
            player_hit_75934(m);
            0
        }
        0x8002_c6f4 => sound_pause(m, a0),
        0x8005_6a10 => {
            sound_queue_80055184(m);
            0
        }
        0x8003_04cc => music_80030288(m),
        0x8003_4718 => {
            pause_input(m);
            0
        }
        0x8003_43c8 => draw_box(m, a0, a1, a2, a3),
        0x8006_cb10 => {
            pad_8006b340(m);
            0
        }
        0x8003_307c => {
            update_32de4(m);
            0
        }
        0x8003_30cc => {
            update_32e34(m);
            0
        }
        0x8003_3104 => {
            update_32e6c(m);
            0
        }
        0x8003_31e0 => {
            update_32f48(m);
            0
        }
        0x8003_329c => {
            update_33004(m);
            0
        }
        0x8003_32a4 => {
            update_3300c(m);
            0
        }
        0x8003_32c4 => {
            update_3302c(m);
            0
        }
        0x8003_96e8 => {
            update_8003931c(m);
            0
        }
        0x8007_f8e4 => {
            update_8007e284(m);
            0
        }
        0x8005_7bbc => {
            update_800562b4(m);
            0
        }
        0x8005_b124 => {
            update_8005971c(m);
            0
        }
        0x8005_c270 => {
            update_8005a874(m);
            0
        }
        _ => return None,
    };
    Some(v0)
}
