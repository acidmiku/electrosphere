//! JP overlay 2: the story-event phases (800F00A8..800F33A7), translated
//! from the JP instructions (class `new`, except 800F07B0 and 800F07F4
//! `changed`: PAL 800D0C88 and 800CF064 are related but different routines
//! now).
//!
//! An event (`[0x80112DB0] + 0x189C` points at its id) has an 8-byte record
//! at 0x8010B400 (+0 resource id, +6 kind, +7 stream kind). Its phases run
//! from the hub's callback table; each returns the hub's phase result.
use super::ov2_p4::{At, call, copy8, lh, lt, sext16};
use super::ov2_p4_story::{open_broadcast_edcc0, slots_ef4f4};
use crate::common::{CONTEXT, RESOURCES, loaded_module};
use crate::machine::{Machine, SmallData};

/// `[0x80112DB0]`: the story-event block; `[0x80112DC4]` the hub block.
const BLOCK: u32 = 0x8011_2db0;
const HUB: u32 = 0x8011_2dc4;
/// 0x8010B400: the 8-byte event records.
const EVENTS: u32 = 0x8010_b400;
/// 0x8010B9B4: per mission, its result kind; 0x8010B360: per event id, a
/// mission byte.
const MISSION_RESULTS: u32 = 0x8010_b9b4;
const EVENT_MISSIONS: u32 = 0x8010_b360;
/// Resident: arena allocation, the HUD reset (8002CBCC), sound and music
/// requests, the stream request queue (8002C8D8), the CD queue reset
/// (8002C6A8), the read size and queue (8002C63C, 8002C82C) with its
/// completion routine 8009B904.
const ALLOC: u32 = 0x8002_7410;
const RESET_HUD: u32 = 0x8002_cbcc;
const PLAY_SOUND: u32 = 0x8002_db4c;
const MUSIC: u32 = 0x8002_cc44;
const QUEUE_STREAM: u32 = 0x8002_c8d8;
const RESET_CD_QUEUE: u32 = 0x8002_c6a8;
const TOTAL_SIZE: u32 = 0x8002_c63c;
const QUEUE_FILES: u32 = 0x8002_c82c;
const READ_COMPLETE: u32 = 0x8009_b904;
/// Overlay 2 (other parts): style entries, layer switch, text slot, the
/// offset animation (800DEBB8), the event's message (800FEE68).
const STYLE_ENTRY: u32 = 0x800d_ee08;
const LAYER: u32 = 0x800d_edf0;
const TEXT_SLOT: u32 = 0x800d_e908;
const SLIDE: u32 = 0x800d_ebb8;
const EVENT_MESSAGE: u32 = 0x800f_ee68;
/// More of overlay 2: 800DECA8 / 800DECD4 slides, 800FF2BC the fade's end,
/// 8010097C the map objects; this part's wireframe, uploads and list.
const SLIDE_OUT: u32 = 0x800d_eca8;
const SLIDE_BACK: u32 = 0x800d_ecd4;
const FADE_DONE: u32 = 0x800f_f2bc;
const MAP_OBJECTS: u32 = 0x8010_097c;
const WIREFRAME: u32 = 0x800e_e9e4;
const UPLOAD_TIM: u32 = 0x800e_cf88;
const UPLOAD_TIMS: u32 = 0x800e_ce38;
const LIST_INIT: u32 = 0x800e_ddf4;
/// `[0x80112DA8]` the story state; `[0x80112DC0]` the hub's style tables;
/// gp+0x2D4 the screen tint.
const STORY: u32 = 0x8011_2da8;
const STYLES: u32 = 0x8011_2dc0;
const TINT: u32 = 0x2d4;
/// Resident: arena reset, directory relocation; libgpu MoveImage,
/// DrawSync, StoreImage.
const RESET: u32 = 0x8002_7538;
const RELOCATE: u32 = 0x8002_7338;
const MOVE_IMAGE: u32 = 0x800a_8500;
const DRAW_SYNC: u32 = 0x800a_8194;
const STORE_IMAGE: u32 = 0x800a_84a0;
/// 0x800CCF60: the saved screen rectangle (0x340, 0x100, 0x40, 0x100);
/// 0x800CCF68: the one moved there (0x2C0, 0x100, 0x40, 0x100).
const SAVE_RECT: u32 = 0x800c_cf60;
const SCREEN_RECT: u32 = 0x800c_cf68;
/// 0x800CCF70: the panel rectangle (0x340, 0x100, 0xC4, 0x100).
const CLEAR_RECT: u32 = 0x800c_cf70;
/// Overlay 2: the style halfword table (800DEE84); resident: the stream
/// pause time (8002B59C), a sound channel's state (8002CAE4), channel
/// volume (8002DF40); libgpu ClearImage.
const STYLE_HALF: u32 = 0x800d_ee84;
const STREAM_PAUSE: u32 = 0x8002_b59c;
const CHANNEL_STATE: u32 = 0x8002_cae4;
const VOLUME: u32 = 0x8002_df40;
const CLEAR_IMAGE: u32 = 0x800a_8318;
/// This part's selection list ticks and resource release; overlay 2's
/// picture effect (800FD408) and quad blend (800FD48C); the resident HUD
/// overlay (8002D3F4); libgpu LoadImage.
const LIST_TICK: u32 = 0x800e_e0d8;
const LIST_TICK_ALL: u32 = 0x800e_e570;
const RELEASE: u32 = 0x800e_cd20;
const PICTURE_EFFECT: u32 = 0x800f_d408;
const BLEND_QUAD: u32 = 0x800f_d48c;
const HUD_OVERLAY: u32 = 0x8002_d3f4;
const LOAD_IMAGE: u32 = 0x800a_8440;
/// Resident disc and stream state: the stream entry's sign (8002CB58) and
/// kind (8002CBA4), the lid check (8002B428), the swap result (8002B46C),
/// the disc state (8002B528), the stream state (8002B504); sound channels:
/// release (8002DDA8), table pitch (8002DE68), set pitch (8002DE24).
const STREAM_NEGATIVE: u32 = 0x8002_cb58;
const STREAM_KIND: u32 = 0x8002_cba4;
const LID_OPENED: u32 = 0x8002_b428;
const SWAP_RESULT: u32 = 0x8002_b46c;
const DISC_STATE: u32 = 0x8002_b528;
const STREAM_STATE: u32 = 0x8002_b504;
const RELEASE_CHANNEL: u32 = 0x8002_dda8;
const SOUND_PITCH: u32 = 0x8002_de68;
const SET_PITCH: u32 = 0x8002_de24;
/// 0x80112750: per mission, the disc it needs (1, 2, 3 both); `[0x80112DB8]`
/// the disc-swap block.
const MISSION_DISCS: u32 = 0x8011_2750;
const SWAP_BLOCK: u32 = 0x8011_2db8;

/// The routines of this file by JP address, while module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != 2 {
        return None;
    }
    let [a0, ..] = args;
    Some(match target {
        0x800f_00a8 => kind_style_f00a8(a0),
        0x800f_0144 => has_box_f0144(a0),
        0x800f_0174 => clear_styles_f0174(m, a0),
        0x800f_0244 => start_event_f0244(m, a0),
        0x800f_072c => queue_voice_f072c(m),
        0x800f_07b0 => stop_voice_f07b0(m),
        0x800f_07f4 => read_resource_f07f4(m),
        0x800f_0898 => init_f0898(m),
        0x800f_0a7c => init_after_mission_f0a7c(m),
        0x800f_0dc4 => init_simulator_f0dc4(m),
        0x800f_0f5c | 0x800f_108c => fade_in_f0f5c(m),
        0x800f_11bc => after_download_f11bc(m),
        0x800f_132c | 0x800f_1384 => panel_done_f132c(m),
        0x800f_13dc => wireframe_wait_f13dc(m),
        0x800f_1430 => bind_event_f1430(m),
        0x800f_16c4 => voice_f16c4(m),
        0x800f_1848 => voice_wait_f1848(m),
        0x800f_1a7c => panel_close_f1a7c(m),
        0x800f_1b8c => start_f1b8c(m),
        0x800f_1c8c => stream_wait_f1c8c(m),
        0x800f_1d04 => play_f1d04(m),
        0x800f_2240 => second_voice_f2240(m),
        0x800f_23cc => second_wait_f23cc(m),
        0x800f_2618 => next_event_f2618(m),
        0x800f_26ec => leave_f26ec(m),
        0x800f_2820 => leave_map_f2820(m),
        0x800f_2990 => close_f2990(m),
        0x800f_29e8 | 0x800f_2d1c => voice_gone_f29e8(m),
        0x800f_2a8c => disc_kind_f2a8c(m),
        0x800f_2b6c => disc_wait_f2b6c(m),
        0x800f_2c34 => disc_wait2_f2c34(m),
        0x800f_2dc0 => disc_kind2_f2dc0(m),
        0x800f_2eb0 => disc_state_f2eb0(m),
        0x800f_3020 => lid_wait_f3020(m),
        0x800f_3064 => lid_wait2_f3064(m),
        0x800f_30d4 => disc_state2_f30d4(m),
        0x800f_3218 => restart_channels_f3218(m, a0),
        _ => return None,
    })
}

fn block(m: &mut Machine) -> u32 {
    m.lw(BLOCK)
}

/// The current event's record.
fn record(m: &mut Machine, b: u32) -> u32 {
    let event = m.lw(b.at(0x189c));
    let id = lh(m, event);
    EVENTS.wrapping_add(id << 3)
}

/// 800F00A8: the style of event kind `kind` (halfword): 0, 1, 10 -> 0x12,
/// 2 -> 0x13, 3..7 -> 0x10, 8 and 11 -> 0x11, 9 -> 0x16, others 0xA.
pub fn kind_style_f00a8(kind: u32) -> u32 {
    match sext16(kind) as i32 {
        0 | 1 | 10 => 0x12,
        2 => 0x13,
        3..=7 => 0x10,
        8 | 11 => 0x11,
        9 => 0x16,
        _ => 0xa,
    }
}

/// 800F0144: whether event kind `kind` draws the boxes (3..10). For a
/// negative kind the original returns its caller's `v0` (0 here; the
/// callers pass record bytes).
pub fn has_box_f0144(kind: u32) -> u32 {
    let k = kind as i32;
    u32::from((3..11).contains(&k))
}

/// 800F0174: style entries 1..5 cleared; returns the panel count of event
/// kind `kind` (3 -> 2, 4 -> 3, 5 -> 4, 6 -> 5, others 0).
pub fn clear_styles_f0174(m: &mut Machine, kind: u32) -> u32 {
    m.push_frame(0x18);
    for entry in 1..6 {
        call(m, STYLE_ENTRY, [entry, 0, 0, 0]);
    }
    let v = match kind {
        3 => 2,
        4 => 3,
        5 => 4,
        6 => 5,
        _ => 0,
    };
    m.pop_frame(0x18);
    v
}

/// The stream picture setup of the event-start cases: count +0x1650, the
/// first quad's screen rectangle (+0x1654) and texture rectangle (+0x1674).
fn picture(m: &mut Machine, b: u32, count: u32, screen: [u16; 4], texture: [u16; 4]) {
    m.sw(b.at(0x1650), count);
    for (i, v) in screen.iter().enumerate() {
        m.sh(b.at(0x1654 + 2 * i as u32), *v);
    }
    for (i, v) in texture.iter().enumerate() {
        m.sh(b.at(0x1674 + 2 * i as u32), *v);
    }
}

/// 800F0244 (stream kind): start the event's broadcast: the message
/// (800FEE68), then by stream kind (table 0x800CCF30): the picture layout
/// and stream (800EDCC0 with `kind`), its sound (0x6A, 0x67) or music
/// (8002CC44), the stream flags +0xF38 / +0x1698, the hub's +0x114 / +0x11C
/// and the context's mission state. Returns the last call's result (or the
/// case's constant).
pub fn start_event_f0244(m: &mut Machine, kind: u32) -> u32 {
    let frame = m.push_frame(0x20);
    let _ = frame;
    let hub = m.lw(HUB);
    let b = block(m);
    m.sw(hub.at(0x114), 0);
    m.sw(b.at(0xf38), 0);
    m.sw(hub.at(0x114), 0);
    let event = m.lw(b.at(0x189c));
    m.sw(b.at(0x1698), 0);
    let id = lh(m, event);
    call(m, EVENT_MESSAGE, [id, 0, 0, 0]);
    let k = sext16(kind);
    let stream = sext16(kind);
    let v = match k {
        0 | 1 => {
            let b = block(m);
            m.sw(b.at(0xf38), 1);
            m.sw(b.at(0x1698), 1);
            picture(m, b, 1, [0x78, 0x5d, 0x50, 0xa0], [0, 0, 0x50, 0x50]);
            open_broadcast_edcc0(m, stream);
            call(m, PLAY_SOUND, [if k == 0 { 0x6a } else { 0x67 }, 0, 0, 0])
        }
        2 => {
            let b = block(m);
            m.sw(b.at(0xf38), 1);
            m.sw(b.at(0x1698), 1);
            m.sw(b.at(0x1650), 2);
            m.sh(b.at(0x1654), 0x46);
            m.sh(b.at(0x1656), 0x5d);
            m.sh(b.at(0x1658), 0x50);
            m.sh(b.at(0x165a), 0xa0);
            m.sh(b.at(0x1674), 0);
            m.sh(b.at(0x1676), 0);
            m.sh(b.at(0x1678), 0x50);
            m.sh(b.at(0x167a), 0x50);
            m.sh(b.at(0x165c), 0xaa);
            m.sh(b.at(0x165e), 0x5d);
            m.sh(b.at(0x1660), 0x50);
            m.sh(b.at(0x1662), 0xa0);
            m.sh(b.at(0x167c), 0x50);
            m.sh(b.at(0x167e), 0);
            m.sh(b.at(0x1680), 0x50);
            m.sh(b.at(0x1682), 0x50);
            open_broadcast_edcc0(m, stream);
            call(m, PLAY_SOUND, [0x67, 0, 0, 0])
        }
        3 => {
            let b = block(m);
            m.sh(b.at(0x1654), 0x4a);
            m.sh(b.at(0x165c), 0x96);
            m.sh(b.at(0x1662), 0x80);
            m.sh(b.at(0x1682), 0x40);
            let event = m.lw(b.at(0x189c));
            for offset in [0x1658u32, 0x1678, 0x167a, 0x167c] {
                m.sh(b.at(offset), 0x30);
            }
            m.sw(b.at(0x1650), 2);
            m.sh(b.at(0x1656), 0x63);
            m.sh(b.at(0x165a), 0x60);
            m.sh(b.at(0x1674), 0);
            m.sh(b.at(0x1676), 0);
            m.sh(b.at(0x165e), 0x63);
            m.sh(b.at(0x1660), 0x60);
            m.sh(b.at(0x167e), 0);
            m.sh(b.at(0x1680), 0x60);
            let id = lh(m, event);
            let mode = m.lbu(EVENTS.wrapping_add(id << 3).at(7));
            if mode == 2 {
                m.sw(b.at(0xf38), 1);
                m.sw(b.at(0x1698), 1);
                open_broadcast_edcc0(m, stream);
            } else if mode == 1 {
                let hub = m.lw(HUB);
                m.sw(hub.at(0x114), 0x4000);
            }
            let hub = m.lw(HUB);
            let a = m.lw(hub.at(0xb0));
            let c = m.lw(hub.at(0xb4));
            call(m, MUSIC, [5, a, c, 0])
        }
        4 => {
            let b = block(m);
            m.sh(b.at(0x1656), 0x46);
            m.sh(b.at(0x165a), 0xc0);
            let event = m.lw(b.at(0x189c));
            m.sh(b.at(0x1658), 0x80);
            m.sh(b.at(0x1678), 0x80);
            m.sw(b.at(0x1650), 1);
            m.sh(b.at(0x1654), 0x60);
            m.sh(b.at(0x1674), 0);
            m.sh(b.at(0x1676), 0);
            m.sh(b.at(0x167a), 0x60);
            let id = lh(m, event);
            if m.lbu(EVENTS.wrapping_add(id << 3).at(7)) == 2 {
                m.sw(b.at(0xf38), 1);
                m.sw(b.at(0x1698), 1);
                open_broadcast_edcc0(m, stream)
            } else {
                let hub = m.lw(HUB);
                m.sw(hub.at(0x114), 0x4000);
                0x4000
            }
        }
        5 => {
            let b = block(m);
            m.sw(b.at(0x1650), 3);
            m.sh(b.at(0x1654), 0x34);
            m.sh(b.at(0x165c), 0xdd);
            m.sh(b.at(0x165a), 0x60);
            m.sh(b.at(0x1662), 0x60);
            m.sh(b.at(0x167c), 0x70);
            m.sh(b.at(0x1666), 0x73);
            for offset in [0x1668u32, 0x1688, 0x168a] {
                m.sh(b.at(offset), 0x40);
            }
            m.sh(b.at(0x1656), 0x5d);
            m.sh(b.at(0x1658), 0x30);
            m.sh(b.at(0x1674), 0);
            m.sh(b.at(0x1676), 0);
            m.sh(b.at(0x1678), 0x30);
            m.sh(b.at(0x167a), 0x30);
            m.sh(b.at(0x165e), 0x5d);
            m.sh(b.at(0x1660), 0x30);
            m.sh(b.at(0x167e), 0);
            m.sh(b.at(0x1680), 0x30);
            m.sh(b.at(0x1682), 0x30);
            m.sh(b.at(0x1664), 0x80);
            m.sh(b.at(0x166a), 0x80);
            m.sh(b.at(0x1684), 0x30);
            m.sh(b.at(0x1686), 0);
            m.sw(b.at(0xf38), 1);
            m.sw(b.at(0x1698), 1);
            open_broadcast_edcc0(m, stream)
        }
        6 => {
            let b = block(m);
            m.sh(b.at(0x1654), 0x78);
            m.sh(b.at(0x1656), 0x57);
            m.sw(b.at(0x1650), 1);
            m.sh(b.at(0x1658), 0x50);
            m.sh(b.at(0x165a), 0xa0);
            m.sh(b.at(0x1674), 0);
            m.sh(b.at(0x1676), 0);
            m.sh(b.at(0x1678), 0x50);
            m.sh(b.at(0x167a), 0x50);
            m.sw(b.at(0xf38), 1);
            m.sw(b.at(0x1698), 1);
            open_broadcast_edcc0(m, stream)
        }
        7 | 11 => {
            let b = block(m);
            m.sw(b.at(0x1698), 1);
            1
        }
        8 => 0x800f_0718,
        9 => {
            let b = block(m);
            m.sw(b.at(0x1650), 1);
            m.sh(b.at(0x1656), 0x64);
            m.sh(b.at(0x1654), 0x60);
            m.sh(b.at(0x1658), 0x80);
            m.sh(b.at(0x165a), 0xc0);
            m.sh(b.at(0x1674), 0);
            m.sh(b.at(0x1676), 0);
            m.sh(b.at(0x1678), 0x80);
            m.sh(b.at(0x167a), 0x60);
            let hub = m.lw(HUB);
            m.sw(hub.at(0x114), 0x4000);
            0x4000
        }
        10 => {
            let hub = m.lw(HUB);
            let context = m.gw(CONTEXT);
            m.sw(hub.at(0x11c), 1);
            let n = m.lbu(context.at(0x95e)).wrapping_add(1);
            m.sb(context.at(0x95e), n);
            let context = m.gw(CONTEXT);
            m.sh(context.at(0x954), 2);
            2
        }
        // beyond the table: the delay slot's `lui v0, 0x800d`
        _ => 0x800d_0000,
    };
    m.pop_frame(0x20);
    v
}

/// 800F072C: while the stream runs (+0x1698), queue the voice request
/// (+0x169C: the hub's voice id +0x12C, or 0x912). Returns 1, or 0.
pub fn queue_voice_f072c(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let b = block(m);
    let v = if m.lw(b.at(0x1698)) == 0 {
        0
    } else {
        let hub = m.lw(HUB);
        let request = b.at(0x169c);
        let voice = if m.lw(hub.at(0x12c)) == 0 {
            0x912
        } else {
            m.lhu(hub.at(0x12c))
        };
        m.sh(request, voice);
        m.sh(request.at(2), 0);
        m.sh(request.at(4), 0);
        m.sh(request.at(6), 0);
        m.sw(request.at(8), 0);
        call(m, QUEUE_STREAM, [request, 1, 0, 0]);
        1
    };
    m.pop_frame(0x18);
    v
}

/// 800F07B0 (JP; PAL 800D0C88, changed): while the stream runs, reset the
/// CD queue (8002C6A8(0, 0)). Returns 1, or 0.
pub fn stop_voice_f07b0(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let b = block(m);
    let v = if m.lw(b.at(0x1698)) == 0 {
        0
    } else {
        call(m, RESET_CD_QUEUE, [0, 0, 0, 0]);
        1
    };
    m.pop_frame(0x18);
    v
}

/// 800F07F4 (JP; PAL 800CF064, changed): when the event list is not empty
/// (+0xC), read the event's resource (record +0) into a new arena buffer
/// (`[resources + 0x60]` = id, 8002C63C size, 8002C82C read, context +0x44
/// = 1). Returns 1, or 0.
pub fn read_resource_f07f4(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let b = block(m);
    let v = if m.lw(b.at(0xc)) == 0 {
        0
    } else {
        let at = record(m, b);
        let resources = m.gw(RESOURCES);
        let id = lh(m, at);
        m.sw(resources.at(0x60), id);
        let size = call(m, TOTAL_SIZE, [resources.at(0x60), 1, 0, 0]);
        let destination = call(m, ALLOC, [size, 0, 0, 0]);
        let context = m.gw(CONTEXT);
        let resources = m.gw(RESOURCES);
        m.sw(context.at(0x44), 1);
        call(
            m,
            QUEUE_FILES,
            [resources.at(0x60), 1, destination, READ_COMPLETE],
        );
        1
    };
    m.pop_frame(0x18);
    v
}

/// Clear the 16 picture slots' and the 16 quad slots' modes (highest
/// first).
fn clear_slots(m: &mut Machine) {
    let b = block(m);
    for i in (0..16u32).rev() {
        m.sh(b.wrapping_add(i * 0xa0).at(0x52c), 0);
    }
    let b = block(m);
    for i in (0..16u32).rev() {
        m.sh(b.wrapping_add(i << 5).at(0x1450), 0);
    }
}

/// The box flag (+0x28, 800F0144) and the style (+0x1C, 800F00A8) of the
/// event's kind.
fn kind_flags(m: &mut Machine) -> u32 {
    let b = block(m);
    let at = record(m, b);
    let kind = u32::from(m.lbu(at.at(6)));
    let boxes = has_box_f0144(kind);
    let b = block(m);
    m.sw(b.at(0x28), boxes);
    let at = record(m, b);
    let kind = u32::from(m.lbu(at.at(6)));
    let style = kind_style_f00a8(kind);
    let b = block(m);
    m.sw(b.at(0x1c), style);
    style
}

/// 800F0898: start the story position's events: the block (0x18A0 bytes:
/// script position 0, +2 = 1, clock 0), context +0x96C flags (+0x40,
/// -0x80), the HUD reset, the event list of `[[hub + 0x120] + 4 +
/// 4 * [context + 0x956]]` at event `[context + 0x95E]`, its resource read;
/// slots cleared; hub step 1 with events left (else step 3 and the slide
/// in, 800DEBB8(0, 15)); layer 0; +0xF30 = 0x8000; box flag and style;
/// sound 0x5A. Returns the sound request's result.
pub fn init_f0898(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let b = call(m, ALLOC, [0x18a0, 0, 0, 0]);
    let context = m.gw(CONTEXT);
    m.sh(b, 0);
    m.sh(b.at(2), 1);
    m.sw(b.at(0x10), 0);
    m.sw(BLOCK, b);
    let flags = (m.lw(context.at(0x96c)) | 0x40) & !0x80;
    m.sw(context.at(0x96c), flags);
    call(m, RESET_HUD, [0; 4]);
    let context = m.gw(CONTEXT);
    let hub = m.lw(HUB);
    let b = block(m);
    let position = u32::from(m.lbu(context.at(0x956)));
    let tables = m.lw(hub.at(0x120));
    let table = m.lw(tables.wrapping_add(position << 2).at(4));
    m.sw(b.at(8), table);
    let count = m.lw(table);
    m.sw(b.at(0x18), table.at(4));
    m.sw(b.at(0xc), count);
    events_from(m, b, context);
    read_resource_f07f4(m);
    let b = block(m);
    m.sb(b.at(0xf2e), 0);
    let b = block(m);
    m.sw(b.at(0xf38), 0);
    clear_slots(m);
    let b = block(m);
    let left = m.lw(b.at(0xc));
    let hub = m.lw(HUB);
    if left != 0 {
        m.sh(hub.at(0x86), 1);
        call(m, LAYER, [0, 0, 0, 0]);
    } else {
        m.sh(hub.at(0x86), 3);
        call(m, SLIDE, [0, 0xf, 0, 0]);
        call(m, LAYER, [0, 0x8000, 0, 0]);
    }
    let b = block(m);
    m.sw(b.at(0xf30), 0x8000);
    kind_flags(m);
    let v = call(m, PLAY_SOUND, [0x5a, 0, 0, 0]);
    m.pop_frame(0x20);
    v
}

/// The event pointer and the events left: `+0x189C = [+8] + 4 + 4 * n` and
/// `+0xC -= n` for `n = [context + 0x95E]`.
fn events_from(m: &mut Machine, b: u32, context: u32) {
    let done = u32::from(m.lbu(context.at(0x95e)));
    let table = m.lw(b.at(8));
    m.sw(
        b.at(0x189c),
        table.wrapping_add((done << 2).wrapping_add(4)),
    );
    let done = u32::from(m.lbu(context.at(0x95e)));
    let count = m.lw(b.at(0xc));
    m.sw(b.at(0xc), count.wrapping_sub(done));
}

/// 800F0A7C: start the events after a mission: context +0x96C flags
/// (+0x80, -0x40); by the mission's result kind (0x8010B9B4): 0 the hub
/// result 0x60000002, 2 (missions 0x10 and 0x25 adjust the next mission)
/// the hub result 0x60000005 with context +0x3A0 = 2; the block, the first
/// event list (`[[hub + 0x120]]`) from event `[context + 0x95E]`, the
/// resource read, layer 1 off, text slot 0x1B, +0xF48/+0xF49 = 0, slots
/// cleared, hub step 1, box flag and style; for event kind 10 with events
/// left the hub result 0x60000005, the next event and its message.
/// Returns that message's result, or 0 / 10.
pub fn init_after_mission_f0a7c(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let context = m.gw(CONTEXT);
    let flags = (m.lw(context.at(0x96c)) | 0x80) & !0x40;
    m.sw(context.at(0x96c), flags);
    let mission = lh(m, context.at(0x38c));
    let result = u32::from(m.lbu(MISSION_RESULTS.wrapping_add(mission)));
    match result {
        0 => {
            let hub = m.lw(HUB);
            m.sw(hub.at(0x94), 0x6000_0002);
            m.sh(context.at(0x954), 1);
        }
        2 => {
            if mission == 0x10 {
                m.sb(context.at(0x3a8), 8);
            }
            let context = m.gw(CONTEXT);
            if m.lh(context.at(0x38c)) == 0x25 {
                m.sb(context.at(0x3a8), 0x1a);
                let context = m.gw(CONTEXT);
                m.sh(context.at(0x38c), 0x39);
            }
            let context = m.gw(CONTEXT);
            let hub = m.lw(HUB);
            m.sw(context.at(0x3a0), result);
            m.sh(context.at(0x954), 1);
            m.sw(hub.at(0x94), 0x6000_0005);
        }
        _ => {}
    }
    let b = call(m, ALLOC, [0x18a0, 0, 0, 0]);
    m.sw(BLOCK, b);
    call(m, RESET_HUD, [0; 4]);
    let b = block(m);
    let hub = m.lw(HUB);
    m.sh(b, 0);
    m.sh(b.at(2), 1);
    m.sw(b.at(0x10), 0);
    let tables = m.lw(hub.at(0x120));
    let table = m.lw(tables);
    m.sw(b.at(8), table);
    let count = m.lw(table);
    m.sw(b.at(0xc), count);
    let table = m.lw(b.at(8));
    let context = m.gw(CONTEXT);
    m.sw(b.at(0x18), table.at(4));
    events_from(m, b, context);
    read_resource_f07f4(m);
    let b = block(m);
    m.sb(b.at(0xf2e), 0);
    let b = block(m);
    m.sw(b.at(0xf38), 0);
    call(m, LAYER, [1, 0, 0, 0]);
    let b = block(m);
    let hub = m.lw(HUB);
    m.sw(b.at(0xf30), 0x8000);
    let entry = m.lw(hub.at(0x15c));
    call(m, TEXT_SLOT, [entry, 0x1b, 0, 0]);
    let b = block(m);
    m.sb(b.at(0xf48), 0);
    let b = block(m);
    m.sb(b.at(0xf49), 0);
    clear_slots(m);
    let hub = m.lw(HUB);
    m.sh(hub.at(0x86), 1);
    kind_flags(m);
    let b = block(m);
    let at = record(m, b);
    let v = if m.lbu(at.at(6)) != 0xa {
        0xa
    } else if m.lw(b.at(0xc)) == 0 {
        0
    } else {
        let hub = m.lw(HUB);
        let context = m.gw(CONTEXT);
        m.sw(hub.at(0x94), 0x6000_0005);
        let done = m.lbu(context.at(0x95e));
        m.sw(context.at(0x3a0), 3);
        m.sb(context.at(0x95e), done.wrapping_add(1));
        let context = m.gw(CONTEXT);
        let b = block(m);
        m.sh(context.at(0x954), 2);
        let event = m.lw(b.at(0x189c));
        let id = lh(m, event);
        let mission = m.lbu(EVENT_MISSIONS.wrapping_add(id));
        m.sb(context.at(0x3a8), mission);
        let b = block(m);
        let event = m.lw(b.at(0x189c));
        let id = lh(m, event);
        call(m, EVENT_MESSAGE, [id, 0, 0, 0])
    };
    m.pop_frame(0x20);
    v
}

/// 800F0DC4: start the Mission Simulator's event: the block, the
/// simulator's single event (`hub + 0x124`), its resource read, the slides
/// 800DECA8(0, 0x15) and (0, 0xE), style entry 11, layer 0 = 0x204001
/// (+0xF30 too), +0xF48/+0xF49 = 0, slots cleared, the box flag and style
/// (both into +0x1C), hub step 4, sound 0x5A. Returns the sound request's
/// result.
pub fn init_simulator_f0dc4(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let b = call(m, ALLOC, [0x18a0, 0, 0, 0]);
    m.sw(BLOCK, b);
    call(m, RESET_HUD, [0; 4]);
    let b = block(m);
    m.sh(b.at(2), 1);
    let hub = m.lw(HUB);
    m.sh(b, 0);
    m.sw(b.at(0x10), 0);
    m.sw(b.at(0xc), 1);
    let event = hub.at(0x124);
    m.sw(b.at(0x18), event);
    m.sw(b.at(0x189c), event);
    read_resource_f07f4(m);
    let b = block(m);
    m.sb(b.at(0xf2e), 0);
    let b = block(m);
    m.sw(b.at(0xf38), 0);
    call(m, SLIDE_OUT, [0, 0x15, 0, 0]);
    call(m, SLIDE_OUT, [0, 0xe, 0, 0]);
    call(m, STYLE_ENTRY, [0xb, 1, 0, 0x19]);
    let b = block(m);
    m.sw(b.at(0xf30), 0x0020_4001);
    call(m, LAYER, [0, 0x0020_4001, 0, 0]);
    let b = block(m);
    m.sb(b.at(0xf48), 0);
    let b = block(m);
    m.sb(b.at(0xf49), 0);
    clear_slots(m);
    let b = block(m);
    let at = record(m, b);
    let kind = u32::from(m.lbu(at.at(6)));
    let boxes = has_box_f0144(kind);
    let b = block(m);
    m.sw(b.at(0x1c), boxes);
    let at = record(m, b);
    let kind = u32::from(m.lbu(at.at(6)));
    let style = kind_style_f00a8(kind);
    let b = block(m);
    m.sw(b.at(0x1c), style);
    let hub = m.lw(HUB);
    m.sh(hub.at(0x86), 4);
    let v = call(m, PLAY_SOUND, [0x5a, 0, 0, 0]);
    m.pop_frame(0x20);
    v
}

/// Signed `x / 32` as compiled.
fn div32(x: u32) -> u32 {
    let x = x as i32;
    ((if x < 0 { x + 0x1f } else { x }) >> 5) as u32
}

/// 800F0F5C / 800F108C (the same code): the fade in: while the clock
/// (+0x10, counted up) was below 32 the tint (gp+0x2D4) is (143, 135, 83) *
/// (clock + 1) / 32 with flag 8 (returns the flags); then 800FF2BC, clock 0
/// and hub step 2 (returns 2).
pub fn fade_in_f0f5c(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let b = block(m);
    let clock = m.lw(b.at(0x10));
    let next = clock.wrapping_add(1);
    m.sw(b.at(0x10), next);
    let v = if (clock as i32) < 0x20 {
        let tint = m.gw(TINT);
        let red = div32(next.wrapping_add(1).wrapping_mul(143));
        m.sb(tint.at(0x10), red as u8);
        for (offset, scale) in [(0x11u32, 135u32), (0x12, 83)] {
            let b = block(m);
            let step = m.lw(b.at(0x10)).wrapping_add(1);
            let tint = m.gw(TINT);
            m.sb(tint.at(offset), div32(step.wrapping_mul(scale)) as u8);
        }
        let tint = m.gw(TINT);
        let flags = m.lw(tint) | 8;
        m.sw(tint, flags);
        flags
    } else {
        call(m, FADE_DONE, [0; 4]);
        let b = block(m);
        m.sw(b.at(0x10), 0);
        let hub = m.lw(HUB);
        m.sh(hub.at(0x86), 2);
        2
    };
    m.pop_frame(0x18);
    v
}

/// 800F11BC: after a download (story state +0x14 = 2): by the context's
/// result (+0x954): 1: with events left, the slide in and layer 0 = 0x8000
/// (step 3), else the slide 800DECD4(0, 15) (step 15); 2 or 3 with no
/// events left: the hub's +0x7C..+0x7E = (0, 0, 4), the block released,
/// slides 0 and 1, layer 0 = 3, 800F097C... (the map objects), the
/// context's story position and result cleared; otherwise the slide in
/// and layer 0 = 0x8000 (step 3). Returns the last call's result (or the
/// step / the context pointer as the original leaves `v0`).
pub fn after_download_f11bc(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let story = m.lw(STORY);
    let v = if m.lw(story.at(0x14)) != 2 {
        2
    } else {
        let context = m.gw(CONTEXT);
        let result = lh(m, context.at(0x954));
        let raw = u32::from(m.lhu(context.at(0x954)));
        if result == 1 {
            let b = block(m);
            if m.lw(b.at(0xc)) != 0 {
                call(m, SLIDE, [0, 0xf, 0, 0]);
                call(m, LAYER, [0, 0x8000, 0, 0]);
                let hub = m.lw(HUB);
                m.sh(hub.at(0x86), 3);
                3
            } else {
                call(m, SLIDE_BACK, [0, 0xf, 0, 0]);
                let hub = m.lw(HUB);
                m.sh(hub.at(0x86), 0xf);
                0xf
            }
        } else {
            let b = block(m);
            if raw.wrapping_sub(2) & 0xffff < 2 && m.lw(b.at(0xc)) == 0 {
                for (offset, value) in [(0x7cu32, 0u8), (0x7d, 0), (0x7e, 4)] {
                    let hub = m.lw(HUB);
                    m.sb(hub.at(offset), value);
                }
                let b = block(m);
                call(m, RESET, [b, 0, 0, 0]);
                call(m, SLIDE, [0, 0, 0, 0]);
                call(m, SLIDE, [0, 1, 0, 0]);
                call(m, LAYER, [0, 3, 0, 0]);
                call(m, MAP_OBJECTS, [0; 4]);
                let context = m.gw(CONTEXT);
                m.sb(context.at(0x95e), 0);
                let context = m.gw(CONTEXT);
                m.sh(context.at(0x954), 0);
                context
            } else {
                let hub = m.lw(HUB);
                m.sh(hub.at(0x86), 3);
                call(m, SLIDE, [0, 0xf, 0, 0]);
                call(m, LAYER, [0, 0x8000, 0, 0])
            }
        }
    };
    m.pop_frame(0x18);
    v
}

/// 800F132C / 800F1384 (the same code): when the hub's panel
/// (`[hub + 0x16C] + 0xBE`) is 1: step 4 with events left (clock 0), else
/// step 15. Returns the step, or 0x80110000 (the original's `v0`).
pub fn panel_done_f132c(m: &mut Machine) -> u32 {
    let hub = m.lw(HUB);
    let panel = m.lw(hub.at(0x16c));
    if m.lh(panel.at(0xbe)) != 1 {
        return 0x8011_0000;
    }
    let b = block(m);
    if m.lw(b.at(0xc)) != 0 {
        m.sh(hub.at(0x86), 4);
        m.sw(b.at(0x10), 0);
        4
    } else {
        m.sh(hub.at(0x86), 0xf);
        0xf
    }
}

/// 800F13DC (JP; PAL overlay-1 800CB690, changed): the clock runs to 60,
/// then hub step 5; the wireframe (800EE9E4). Returns its result.
pub fn wireframe_wait_f13dc(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let b = block(m);
    let clock = m.lw(b.at(0x10));
    if (clock as i32) < 0x3c {
        m.sw(b.at(0x10), clock.wrapping_add(1));
    } else {
        let hub = m.lw(HUB);
        m.sh(hub.at(0x86), 5);
    }
    let v = call(m, WIREFRAME, [0; 4]);
    m.pop_frame(0x18);
    v
}

/// 800F1430: the wireframe; when the event's resource is read (context
/// +0x44 = 0): bind it (hub +0x28; child 3 the sound bank, through the
/// stub 8002D6E0; child 0 the stream resource +0xF34; the event record's
/// style byte into style 0x33), move the screen (0x2C0, 0x100, 0x40, 0x100)
/// to (0x340, 0x100), upload child 1's 1..4 TIMs and child 2's TIM
/// directory (with the selection list), release; save the (0x340, 0x100,
/// 0x40, 0x100) rectangle into a 0x8100-byte buffer (+0x1694), hub step 6.
/// Returns 6, or 0 while the read runs.
pub fn bind_event_f1430(m: &mut Machine) -> u32 {
    let sp = m.push_frame(0x38);
    call(m, WIREFRAME, [0; 4]);
    let context = m.gw(CONTEXT);
    let v = if m.lw(context.at(0x44)) != 0 {
        0
    } else {
        let resources = m.gw(RESOURCES);
        let resource = m.lw(resources.at(0x60));
        let children = call(m, RELOCATE, [resource, 0, 0, 0]);
        let hub = m.lw(HUB);
        m.sw(hub.at(0x28), resource);
        // child 3's first word decides a call of 8002D6E0, which is `jr ra;
        // li v0, 0` (a stub the function map folds into the end of
        // 8002D560): no effect, its result unused
        let bank = m.lw(children.at(0xc));
        let _ = m.lw(bank);
        let b = block(m);
        let stream = m.lw(children);
        let styles = m.lw(STYLES);
        m.sw(b.at(0xf34), stream);
        let at = record(m, b);
        let bytes = m.lw(styles.at(0x5c));
        let style = m.lbu(at.at(5));
        m.sb(bytes.at(0x33), style);
        let list = m.lw(children.at(4));
        let hub = m.lw(HUB);
        let count = m.lw(list);
        copy8(m, SAVE_RECT, sp.at(0x10));
        m.sw(hub.at(0xb0), 0);
        m.sw(hub.at(0xb4), 0);
        let list = m.lw(children.at(4));
        let tims = call(m, RELOCATE, [list, 0, 0, 0]);
        copy8(m, SCREEN_RECT, sp.at(0x18));
        call(m, MOVE_IMAGE, [sp.at(0x18), 0x340, 0x100, 0]);
        call(m, DRAW_SYNC, [0; 4]);
        let b = block(m);
        m.sw(b.at(0x1694), 0);
        let uploads = match count {
            1 => 1,
            2 => 2,
            3 => 3,
            4 => 4,
            _ => 0,
        };
        for i in 0..uploads {
            let tim = m.lw(tims.wrapping_add(i << 2));
            call(m, UPLOAD_TIM, [tim, 0, 0, 0]);
        }
        let directory = m.lw(children.at(8));
        if m.lw(directory) != 0 {
            call(m, UPLOAD_TIMS, [directory, 0x4000, 0, 0]);
            call(m, LIST_INIT, [0; 4]);
        }
        let first = m.lw(tims);
        call(m, RESET, [first, 0, 0, 0]);
        let buffer = call(m, ALLOC, [0x8100, 0, 0, 0]);
        let b = block(m);
        m.sw(b.at(0x1694), buffer);
        call(m, STORE_IMAGE, [sp.at(0x10), buffer, 0, 0]);
        call(m, DRAW_SYNC, [0; 4]);
        let hub = m.lw(HUB);
        m.sh(hub.at(0x86), 6);
        6
    };
    m.pop_frame(0x38);
    v
}

/// 800F16C4: the wireframe; the event's voice: resource 0x10E / 0x14B
/// voice 0x913, 0x135 voice 0x915, 0x16A / 0x174 / 0x178 voice 0x912 (hub
/// +0x12C; others: no stream, +0x1698 = 0); with the voice queued, layer 0
/// += 0x800000, style entry 3 and style byte 0x32 = 0; sound 0x37 while
/// the stream runs; hub step 7, the voice timeout +0xF44 = 180. Returns
/// 180.
pub fn voice_f16c4(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    call(m, WIREFRAME, [0; 4]);
    let b = block(m);
    m.sw(b.at(0x1698), 1);
    let at = record(m, b);
    let id = lh(m, at) as i32;
    let voice = match id {
        0x10e | 0x14b => Some(0x913),
        0x135 => Some(0x915),
        0x16a | 0x174 | 0x178 => Some(0x912),
        _ => None,
    };
    match voice {
        Some(voice) => {
            let hub = m.lw(HUB);
            m.sw(hub.at(0x12c), voice);
        }
        None => {
            let b = block(m);
            m.sw(b.at(0x1698), 0);
        }
    }
    if queue_voice_f072c(m) != 0 {
        let b = block(m);
        let layer = m.lw(b.at(0xf30));
        call(m, LAYER, [0, layer | 0x0080_0000, 0, 0]);
        call(m, STYLE_ENTRY, [3, 1, 0, 0xa]);
        let styles = m.lw(STYLES);
        let bytes = m.lw(styles.at(0x5c));
        m.sb(bytes.at(0x32), 0);
    }
    let b = block(m);
    if m.lw(b.at(0x1698)) != 0 {
        call(m, PLAY_SOUND, [0x37, 0, 0, 0]);
    }
    let hub = m.lw(HUB);
    m.sh(hub.at(0x86), 7);
    let b = block(m);
    m.sw(b.at(0xf44), 0xb4);
    m.pop_frame(0x18);
    0xb4
}

/// 800F1848: the wireframe; while the voice stream plays (+0x1698), wait
/// for the stream (8002B59C) with the +0xF44 timeout (returns 0 while it
/// counts), then for the voice's channel (8002CAE4: entry 3 reset when it
/// is gone, or when pad bit 0x10 skips it, with the CD queue reset); once
/// style entry 3 (read first) is idle: the stream stopped, layer 0 with the
/// event style's bit, the panel count (+0x16AC, 800F0174) and its entries,
/// sound 0x64, style byte 0x32 by event kind 0/1, hub step 8 (returns 8).
/// Otherwise returns the original's last `v0` of the wait.
pub fn voice_wait_f1848(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let entry = call(m, STYLE_HALF, [3, 0, 0, 0]);
    call(m, WIREFRAME, [0; 4]);
    let v = voice_wait(m, entry);
    m.pop_frame(0x20);
    v
}

fn voice_wait(m: &mut Machine, entry: u32) -> u32 {
    let b = block(m);
    let mut v = m.lw(b.at(0x1698));
    if v != 0 {
        let paused = call(m, STREAM_PAUSE, [0; 4]);
        let mut ready = paused != 0;
        if !ready {
            let b = block(m);
            let timeout = m.lw(b.at(0xf44));
            if (timeout as i32) > 0 {
                m.sw(b.at(0xf44), timeout.wrapping_sub(1));
                return 0;
            }
            ready = true;
        }
        if ready {
            let b = block(m);
            // a stopped stream leaves the delay slot's `lui v0, 0x8011`
            v = 0x8011_0000;
            if m.lw(b.at(0x1698)) != 0 {
                let hub = m.lw(HUB);
                let voice = m.lw(hub.at(0x12c));
                if call(m, CHANNEL_STATE, [voice, 0, 0, 0]) == u32::MAX {
                    v = call(m, STYLE_ENTRY, [3, 3, u32::MAX, u32::MAX]);
                } else {
                    let context = m.gw(CONTEXT);
                    v = m.lw(context.at(0x12c)) & 0x10;
                    if v != 0 {
                        call(m, STYLE_ENTRY, [3, 3, u32::MAX, u32::MAX]);
                        v = call(m, RESET_CD_QUEUE, [0, 0, 0, 0]);
                    }
                }
            }
        }
    }
    if entry != 0 {
        return v;
    }
    stop_voice_f07b0(m);
    let b = block(m);
    let style = m.lw(b.at(0x1c));
    let layer = m.lw(b.at(0xf30));
    call(m, LAYER, [0, layer | (1u32.wrapping_shl(style)), 0, 0]);
    let b = block(m);
    let at = record(m, b);
    let kind = u32::from(m.lbu(at.at(6)));
    let panels = clear_styles_f0174(m, kind);
    let b = block(m);
    m.sw(b.at(0x16ac), panels);
    call(m, STYLE_ENTRY, [1, 1, 0, 0xf]);
    let b = block(m);
    let panels = m.lw(b.at(0x16ac));
    if panels != 0 {
        call(m, STYLE_ENTRY, [panels, 1, 0, 0xf]);
    }
    call(m, PLAY_SOUND, [0x64, 0, 0, 0]);
    let b = block(m);
    let at = record(m, b);
    let kind = m.lbu(at.at(6));
    if kind == 0 || kind == 1 {
        let styles = m.lw(STYLES);
        let bytes = m.lw(styles.at(0x5c));
        m.sb(bytes.at(0x32), kind);
    }
    let hub = m.lw(HUB);
    m.sh(hub.at(0x86), 8);
    8
}

/// 800F1A7C: the wireframe; when style entry 1 is 2: hub step 9, hub +0x1A4
/// = 0, clock -1, the five channels (hub +0x130..+0x140) faded to 0x32 over
/// 30 (8002DF40); else the hub's +0x1A4 counts and the (0x340, 0x100,
/// 0xC4, 0x100) rectangle is cleared. Returns the last call's result.
pub fn panel_close_f1a7c(m: &mut Machine) -> u32 {
    let sp = m.push_frame(0x20);
    call(m, WIREFRAME, [0; 4]);
    let v = if call(m, STYLE_HALF, [1, 0, 0, 0]) == 2 {
        let hub = m.lw(HUB);
        m.sh(hub.at(0x86), 9);
        let b = block(m);
        m.sw(hub.at(0x1a4), 0);
        m.sw(b.at(0x10), u32::MAX);
        let mut v = 0;
        for offset in [0x130u32, 0x134, 0x138, 0x13c, 0x140] {
            let hub = m.lw(HUB);
            let channel = m.lw(hub.at(offset));
            v = call(m, VOLUME, [channel, 0x32, 0x1e, 0]);
        }
        v
    } else {
        let hub = m.lw(HUB);
        let count = m.lw(hub.at(0x1a4)).wrapping_add(1);
        m.sw(hub.at(0x1a4), count);
        copy8(m, CLEAR_RECT, sp.at(0x10));
        call(m, CLEAR_IMAGE, [sp.at(0x10), 0, 0, 0])
    };
    m.pop_frame(0x20);
    v
}

/// 800F1B8C: the wireframe; start the event (800F0244 with its kind), its
/// voice id (record +2) into hub +0x12C, clock 0, its music (record +4,
/// 8002CC44); with the voice queued hub step 10 and timeout 180, else step
/// 11; hub +0x128 = 0. Returns 1.
pub fn start_f1b8c(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    call(m, WIREFRAME, [0; 4]);
    let b = block(m);
    let at = record(m, b);
    let kind = u32::from(m.lbu(at.at(6)));
    start_event_f0244(m, kind);
    let b = block(m);
    let event = m.lw(b.at(0x189c));
    let id = lh(m, event);
    let hub = m.lw(HUB);
    let voice = lh(m, EVENTS.wrapping_add(id << 3).at(2));
    m.sw(hub.at(0x12c), voice);
    let event = m.lw(b.at(0x189c));
    m.sw(b.at(0x10), 0);
    let a = m.lw(hub.at(0xb0));
    let id = lh(m, event);
    let c = m.lw(hub.at(0xb4));
    let music = u32::from(m.lbu(EVENTS.wrapping_add(id << 3).at(4)));
    call(m, MUSIC, [music, a, c, 0]);
    if queue_voice_f072c(m) != 0 {
        let hub = m.lw(HUB);
        m.sh(hub.at(0x86), 0xa);
        let b = block(m);
        m.sw(b.at(0xf44), 0xb4);
    } else {
        let hub = m.lw(HUB);
        m.sh(hub.at(0x86), 0xb);
    }
    let hub = m.lw(HUB);
    m.sw(hub.at(0x128), 0);
    m.pop_frame(0x20);
    1
}

/// 800F1C8C: the wireframe; when the stream is paused, or the timeout
/// +0xF44 runs out (then the stream flag +0xF38 = 2), hub step 11.
/// Returns 11, 2 or the timeout left.
pub fn stream_wait_f1c8c(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    call(m, WIREFRAME, [0; 4]);
    let v = if call(m, STREAM_PAUSE, [0; 4]) != 0 {
        let hub = m.lw(HUB);
        m.sh(hub.at(0x86), 0xb);
        0xb
    } else {
        let b = block(m);
        let left = m.lw(b.at(0xf44)).wrapping_sub(1);
        m.sw(b.at(0xf44), left);
        if (left as i32) >= 0 {
            left
        } else {
            let hub = m.lw(HUB);
            m.sh(hub.at(0x86), 0xb);
            m.sw(b.at(0xf38), 2);
            2
        }
    };
    m.pop_frame(0x18);
    v
}

/// 800F1D04: one frame of the event: the wireframe, the selection list
/// (kinds 8 and 11), with hub +0x114 the picture effect (800FD408: 0x40 for
/// kind 9, else 0x10) and, while hub +0x128 is 0, each stream quad blended
/// into the hub's picture (800FD48C); the picture slots (800EF4F4: a
/// finished stream sets +0xF38 = 2, a released one +0xF38 = 0 and hub
/// +0x128 = 1); pad bit 0x10 skips the stream; the HUD overlay (8002D3F4)
/// into the hub's picture. While the script runs (+2) and hub +0x128 is not
/// 1 the clock counts (returns it). Then: hub step 12, +0x24 the kind, the
/// event's seen flag (context +0xD98 + id: 0 -> 1 except events 0x58, 0x7F,
/// 0x95, 0xA7; 1 -> 2), the saved screen restored, and for event 0x153 with
/// context flag 0x20 the hub result 0x60000005 (returns 1); else the next
/// event, the slide out, style entries reset, the resource released, the
/// events left counted down, the stream stopped, sound 0x65 and the five
/// channels faded to 0x64. Returns the last channel's result.
pub fn play_f1d04(m: &mut Machine) -> u32 {
    let sp = m.push_frame(0x30);
    let v = play(m, sp);
    m.pop_frame(0x30);
    v
}

fn kind_of(m: &mut Machine) -> u8 {
    let b = block(m);
    let at = record(m, b);
    m.lbu(at.at(6))
}

fn play(m: &mut Machine, sp: u32) -> u32 {
    call(m, WIREFRAME, [0; 4]);
    if kind_of(m) == 8 {
        call(m, LIST_TICK_ALL, [0; 4]);
    }
    if kind_of(m) == 0xb {
        call(m, LIST_TICK, [0; 4]);
    }
    let hub = m.lw(HUB);
    if m.lw(hub.at(0x114)) != 0 {
        let effect = if kind_of(m) == 9 { 0x40 } else { 0x10 };
        call(m, PICTURE_EFFECT, [effect, 0, 0, 0]);
        let hub = m.lw(HUB);
        if m.lw(hub.at(0x128)) == 0 {
            let b = block(m);
            if lt(0, m.lw(b.at(0x1650))) {
                let (mut i, mut rects, mut textures) = (0u32, 0x1654u32, 0x1674u32);
                loop {
                    i = i.wrapping_add(1);
                    let b = block(m);
                    let hub = m.lw(HUB);
                    let table = m.lw(hub.at(8));
                    let picture = m.lw(hub.at(0x274));
                    let blended = call(
                        m,
                        BLEND_QUAD,
                        [
                            picture,
                            table.at(8),
                            b.wrapping_add(rects),
                            b.wrapping_add(textures),
                        ],
                    );
                    let hub = m.lw(HUB);
                    let b = block(m);
                    textures = textures.wrapping_add(8);
                    m.sw(hub.at(0x274), blended);
                    rects = rects.wrapping_add(8);
                    if !lt(i, m.lw(b.at(0x1650))) {
                        break;
                    }
                }
            }
        }
    }
    let result = slots_ef4f4(m);
    if result == 1 {
        let b = block(m);
        m.sw(b.at(0xf38), 2);
    } else if result == 2 {
        call(m, RELEASE, [0; 4]);
        let b = block(m);
        let hub = m.lw(HUB);
        m.sw(b.at(0xf38), 0);
        m.sw(hub.at(0x128), 1);
    }
    let b = block(m);
    if m.lw(b.at(0xf38)) == 1 {
        let context = m.gw(CONTEXT);
        if m.lw(context.at(0x12c)) & 0x10 != 0 {
            m.sw(b.at(0xf38), 2);
        }
    }
    let hub = m.lw(HUB);
    let table = m.lw(hub.at(8));
    let picture = m.lw(hub.at(0x274));
    let drawn = call(m, HUD_OVERLAY, [table, picture, 0, 0]);
    let hub = m.lw(HUB);
    let b = block(m);
    m.sw(hub.at(0x274), drawn);
    if m.lh(b.at(2)) != 0 && m.lw(hub.at(0x128)) != 1 {
        let clock = m.lw(b.at(0x10)).wrapping_add(1);
        m.sw(b.at(0x10), clock);
        return clock;
    }
    let event = m.lw(b.at(0x189c));
    let id = lh(m, event);
    m.sh(hub.at(0x86), 0xc);
    let event = m.lw(b.at(0x189c));
    let kind_id = lh(m, event);
    let kind = m.lbu(EVENTS.wrapping_add(kind_id << 3).at(6));
    m.sw(b.at(0x24), u32::from(kind));
    let context = m.gw(CONTEXT);
    let seen = context.wrapping_add(id.wrapping_add(0xd98));
    match m.lbu(seen) {
        0 => {
            let event = m.lw(b.at(0x189c));
            if !matches!(m.lh(event), 0x58 | 0x7f | 0x95 | 0xa7) {
                m.sb(seen, 1);
            }
        }
        1 => m.sb(seen, 2),
        _ => {}
    }
    let b = block(m);
    copy8(m, SCREEN_RECT, sp.at(0x10));
    let saved = m.lw(b.at(0x1694));
    if saved != 0 {
        call(m, LOAD_IMAGE, [sp.at(0x10), saved, 0, 0]);
        call(m, DRAW_SYNC, [0; 4]);
    }
    let b = block(m);
    let at = record(m, b);
    if m.lh(at) == 0x153 {
        let context = m.gw(CONTEXT);
        if m.lw(context.at(0x96c)) & 0x20 != 0 {
            let hub = m.lw(HUB);
            m.sw(hub.at(0x94), 0x6000_0005);
            m.sw(context.at(0x3a0), 3);
            m.sb(context.at(0x3a8), 0x1d);
            let again = m.gw(CONTEXT);
            m.sh(context.at(0x954), 1);
            m.sb(again.at(0x95e), 0);
            return 1;
        }
    }
    let b = block(m);
    let event = m.lw(b.at(0x189c)).wrapping_add(4);
    m.sw(b.at(0x189c), event);
    let position = m.lhu(b).wrapping_add(1);
    let context = m.gw(CONTEXT);
    m.sh(b, position);
    let done = m.lbu(context.at(0x95e)).wrapping_add(1);
    m.sb(context.at(0x95e), done);
    let b = block(m);
    let style = m.lw(b.at(0x1c));
    m.sh(b.at(2), 1);
    m.sw(b.at(0x10), 0);
    call(m, SLIDE_BACK, [0, style, 0, 0]);
    let hub = m.lw(HUB);
    m.sw(hub.at(0x1a4), 0x3c);
    call(m, STYLE_ENTRY, [1, 3, u32::MAX, u32::MAX]);
    let b = block(m);
    let panels = m.lw(b.at(0x16ac));
    if panels != 0 {
        call(m, STYLE_ENTRY, [panels, 3, u32::MAX, u32::MAX]);
    }
    let b = block(m);
    let hub = m.lw(HUB);
    m.sw(b.at(0xf38), 0);
    let resource = m.lw(hub.at(0x28));
    call(m, RESET, [resource, 0, 0, 0]);
    let b = block(m);
    let left = m.lw(b.at(0xc)).wrapping_sub(1);
    m.sw(b.at(0xc), left);
    stop_voice_f07b0(m);
    call(m, PLAY_SOUND, [0x65, 0, 0, 0]);
    let mut v = 0;
    for offset in [0x130u32, 0x134, 0x138, 0x13c, 0x140] {
        let hub = m.lw(HUB);
        let channel = m.lw(hub.at(offset));
        v = call(m, VOLUME, [channel, 0x64, 0x1e, 0]);
    }
    v
}

/// 800F2240: the wireframe (pad bit 0x10: style entry 1 reset); once style
/// entry 1 is idle: layer 0 from +0xF30, the stream on with the previous
/// event's voice (resource 0x10E / 0x14B / 0x15D voice 0x914, 0x135 voice
/// 0x916, others no stream); with the voice queued layer 0 = 0x808000,
/// style entry 3 and style byte 0x32 = 1, else entry 3 cleared; hub step
/// 13, timeout 180. Returns 180, or 1 while entry 1 runs.
pub fn second_voice_f2240(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    call(m, WIREFRAME, [0; 4]);
    let context = m.gw(CONTEXT);
    if m.lw(context.at(0x12c)) & 0x10 != 0 {
        call(m, STYLE_ENTRY, [1, 0, 0, 0]);
    }
    let v = if call(m, STYLE_HALF, [1, 0, 0, 0]) != 0 {
        1
    } else {
        let b = block(m);
        let layer = m.lw(b.at(0xf30));
        call(m, LAYER, [0, layer, 0, 0]);
        let b = block(m);
        m.sw(b.at(0x1698), 1);
        let event = m.lw(b.at(0x189c));
        let id = lh(m, event.wrapping_sub(4));
        let resource = m.lh(EVENTS.wrapping_add(id << 3)) as i32;
        let voice = match resource {
            0x10e | 0x14b | 0x15d => Some(0x914),
            0x135 => Some(0x916),
            _ => None,
        };
        match voice {
            Some(voice) => {
                let hub = m.lw(HUB);
                m.sw(hub.at(0x12c), voice);
            }
            None => {
                let b = block(m);
                m.sw(b.at(0x1698), 0);
            }
        }
        if queue_voice_f072c(m) != 0 {
            call(m, LAYER, [0, 0x0080_8000, 0, 0]);
            call(m, STYLE_ENTRY, [3, 1, 0, 0xa]);
            let styles = m.lw(STYLES);
            let bytes = m.lw(styles.at(0x5c));
            m.sb(bytes.at(0x32), 1);
        } else {
            call(m, STYLE_ENTRY, [3, 0, 0, 0]);
        }
        let hub = m.lw(HUB);
        m.sh(hub.at(0x86), 0xd);
        let b = block(m);
        m.sw(b.at(0xf44), 0xb4);
        0xb4
    };
    m.pop_frame(0x18);
    v
}

/// 800F23CC: the second voice's wait (as 800F1848: the timeout counts
/// without a lower bound); once style entry 3 is idle: the stream stopped,
/// the next event's resource read, its style (+0x1C); with no events left
/// or event kind 10 hub step 14 and +0x1A4 = 30 (returns 0); else by kind
/// (table 0x800CCF78): kinds 0..2 after a previous kind (+0x24) of 3 or more
/// step 14 with the clock at 30, kinds 3..9 and 11 after a previous kind
/// below 3 step 14 with +0x1A4 = 30 (both return 30), otherwise step 5
/// (returns 5).
pub fn second_wait_f23cc(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let entry = call(m, STYLE_HALF, [3, 0, 0, 0]);
    call(m, WIREFRAME, [0; 4]);
    let v = second_wait(m, entry);
    m.pop_frame(0x20);
    v
}

fn second_wait(m: &mut Machine, entry: u32) -> u32 {
    let b = block(m);
    let mut v = m.lw(b.at(0x1698));
    if v != 0 {
        if call(m, STREAM_PAUSE, [0; 4]) == 0 {
            let b = block(m);
            let left = m.lw(b.at(0xf44)).wrapping_sub(1);
            m.sw(b.at(0xf44), left);
            return 0;
        }
        let b = block(m);
        v = 0x8011_0000;
        if m.lw(b.at(0x1698)) != 0 {
            let hub = m.lw(HUB);
            let voice = m.lw(hub.at(0x12c));
            if call(m, CHANNEL_STATE, [voice, 0, 0, 0]) == u32::MAX {
                v = call(m, STYLE_ENTRY, [3, 3, u32::MAX, u32::MAX]);
            } else {
                let context = m.gw(CONTEXT);
                v = m.lw(context.at(0x12c)) & 0x10;
                if v != 0 {
                    call(m, STYLE_ENTRY, [3, 3, u32::MAX, u32::MAX]);
                    v = call(m, RESET_CD_QUEUE, [0, 0, 0, 0]);
                }
            }
        }
    }
    if entry != 0 {
        return v;
    }
    stop_voice_f07b0(m);
    read_resource_f07f4(m);
    let b = block(m);
    let at = record(m, b);
    let kind = u32::from(m.lbu(at.at(6)));
    let style = kind_style_f00a8(kind);
    let b = block(m);
    let left = m.lw(b.at(0xc));
    m.sw(b.at(0x1c), style);
    let kind = if left == 0 {
        0xa
    } else {
        let at = record(m, b);
        m.lbu(at.at(6))
    };
    if kind == 0xa {
        let hub = m.lw(HUB);
        m.sh(hub.at(0x86), 0xe);
        m.sw(hub.at(0x1a4), 0x1e);
        return 0;
    }
    match kind {
        0..=2 => {
            let b = block(m);
            if m.lw(b.at(0x24)) >= 3 {
                let hub = m.lw(HUB);
                m.sh(hub.at(0x86), 0xe);
                m.sw(b.at(0x10), 0x1e);
                return 0x1e;
            }
        }
        3..=9 | 11 => {
            let b = block(m);
            if m.lw(b.at(0x24)) < 3 {
                let hub = m.lw(HUB);
                m.sh(hub.at(0x86), 0xe);
                m.sw(hub.at(0x1a4), 0x1e);
                return 0x1e;
            }
        }
        _ => return 0,
    }
    let hub = m.lw(HUB);
    m.sh(hub.at(0x86), 5);
    5
}

/// 800F2618: the wireframe; the clock counts down; at 0: with no events
/// left or event kind 10 the hub step advances and the slide out
/// (800DECD4(0, 15)) with sound 0x5B (returns its result); else hub step 4,
/// the box flag toggled and the slide (800DEBB8(0, style)).
pub fn next_event_f2618(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    call(m, WIREFRAME, [0; 4]);
    let b = block(m);
    let clock = m.lw(b.at(0x10));
    let v = if (clock as i32) > 0 {
        let left = clock.wrapping_sub(1);
        m.sw(b.at(0x10), left);
        left
    } else {
        let done = m.lw(b.at(0xc)) == 0 || {
            let at = record(m, b);
            m.lbu(at.at(6)) == 0xa
        };
        if done {
            let hub = m.lw(HUB);
            let step = m.lhu(hub.at(0x86)).wrapping_add(1);
            m.sh(hub.at(0x86), step);
            call(m, SLIDE_BACK, [0, 0xf, 0, 0]);
            call(m, PLAY_SOUND, [0x5b, 0, 0, 0])
        } else {
            let hub = m.lw(HUB);
            m.sh(hub.at(0x86), 4);
            let boxes = m.lw(b.at(0x28));
            let style = m.lw(b.at(0x1c));
            m.sw(b.at(0x28), 1u32.wrapping_sub(boxes));
            call(m, SLIDE, [0, style, 0, 0])
        }
    };
    m.pop_frame(0x18);
    v
}

/// The story position's next event after a kind-10 event (800F26EC /
/// 800F2820): the hub's +0x11C = 1, the event count, result 2, the
/// event's mission byte (read before or after its message).
fn advance_event(m: &mut Machine, read_first: bool) {
    let hub = m.lw(HUB);
    let context = m.gw(CONTEXT);
    m.sw(hub.at(0x11c), 1);
    let done = m.lbu(context.at(0x95e)).wrapping_add(1);
    m.sb(context.at(0x95e), done);
    let b = block(m);
    m.sh(context.at(0x954), 2);
    let event = m.lw(b.at(0x189c));
    let id = lh(m, event);
    if read_first {
        let mission = m.lbu(EVENT_MISSIONS.wrapping_add(id));
        call(m, EVENT_MESSAGE, [id, 0, 0, 0]);
        let context = m.gw(CONTEXT);
        m.sb(context.at(0x3a8), mission);
    } else {
        let context = m.gw(CONTEXT);
        let mission = m.lbu(EVENT_MISSIONS.wrapping_add(id));
        m.sb(context.at(0x3a8), mission);
        let b = block(m);
        let event = m.lw(b.at(0x189c));
        let id = lh(m, event);
        call(m, EVENT_MESSAGE, [id, 0, 0, 0]);
    }
}

/// 800F26EC: leave the events: the hub's panel +0xBE = 1, +0x7E = 0, step
/// 6.0, mode 0; for a kind-10 event with events left the next event (its
/// message; returns 0); else the story position cleared, context flag 0x40
/// off, the block released and layer 0 = 0x8000 (returns that result).
pub fn leave_f26ec(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let hub = m.lw(HUB);
    let panel = m.lw(hub.at(0x16c));
    m.sh(panel.at(0xbe), 1);
    m.sb(hub.at(0x7e), 0);
    let b = block(m);
    m.sh(hub.at(0x84), 6);
    m.sh(hub.at(0x86), 0);
    m.sw(hub.at(0xe0), 0);
    let next = m.lw(b.at(0xc)) != 0 && {
        let at = record(m, b);
        m.lbu(at.at(6)) == 0xa
    };
    let v = if next {
        advance_event(m, false);
        0
    } else {
        let context = m.gw(CONTEXT);
        m.sb(context.at(0x95e), 0);
        let context = m.gw(CONTEXT);
        let b = block(m);
        let flags = m.lw(context.at(0x96c)) & !0x40;
        m.sw(context.at(0x96c), flags);
        call(m, RESET, [b, 0, 0, 0]);
        call(m, LAYER, [0, 0x8000, 0, 0])
    };
    m.pop_frame(0x18);
    v
}

/// 800F2820: once style entry 1 is idle: the hub's +0x7C..+0x7E cleared,
/// the block released, slide 1, layer 0 = 3, style entry 15, the map
/// objects; for a kind-10 event with events left the next event (returns
/// 0); else the story position and result cleared and context flag 0x80 off
/// (returns the flags). Returns the entry's state while it runs.
pub fn leave_map_f2820(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let busy = call(m, STYLE_HALF, [1, 0, 0, 0]);
    let v = if busy != 0 {
        busy
    } else {
        for offset in [0x7cu32, 0x7d, 0x7e] {
            let hub = m.lw(HUB);
            m.sb(hub.at(offset), 0);
        }
        let b = block(m);
        call(m, RESET, [b, 0, 0, 0]);
        call(m, SLIDE, [0, 1, 0, 0]);
        call(m, LAYER, [0, 3, 0, 0]);
        call(m, STYLE_ENTRY, [0xf, 1, 0, 0x19]);
        call(m, MAP_OBJECTS, [0; 4]);
        let b = block(m);
        let next = m.lw(b.at(0xc)) != 0 && {
            let at = record(m, b);
            m.lbu(at.at(6)) == 0xa
        };
        if next {
            advance_event(m, true);
            0
        } else {
            let context = m.gw(CONTEXT);
            m.sb(context.at(0x95e), 0);
            let context = m.gw(CONTEXT);
            let flags = m.lw(context.at(0x96c)) & !0x80;
            m.sh(context.at(0x954), 0);
            m.sw(context.at(0x96c), flags);
            flags
        }
    };
    m.pop_frame(0x20);
    v
}

/// 800F2990: once style entry 1 is idle: the hub's +0x7C/+0x7D = 3, +0x1C6
/// = 6, the block released. Returns the reset's result or the entry's
/// state.
pub fn close_f2990(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let busy = call(m, STYLE_HALF, [1, 0, 0, 0]);
    let v = if busy != 0 {
        busy
    } else {
        let hub = m.lw(HUB);
        m.sb(hub.at(0x7c), 3);
        let hub = m.lw(HUB);
        m.sb(hub.at(0x7d), 3);
        let b = block(m);
        let hub = m.lw(HUB);
        m.sh(hub.at(0x1c6), 6);
        call(m, RESET, [b, 0, 0, 0])
    };
    m.pop_frame(0x18);
    v
}

/// The simulator event's voice (record +2 of event `[hub + 0x124]`).
fn simulator_voice(m: &mut Machine) -> u32 {
    let hub = m.lw(HUB);
    let id = lh(m, hub.at(0x124));
    lh(m, EVENTS.wrapping_add(id << 3).at(2))
}

/// 800F29E8 / 800F2D1C (the same code): when the simulator voice's stream
/// entry is negative (8002CB58 = 1): layer 0 off, the resource released,
/// sound 0x3C, hub +0x7D = 2, step 0 (returns the hub block); else the
/// step advances (returns it).
pub fn voice_gone_f29e8(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let voice = simulator_voice(m);
    let v = if call(m, STREAM_NEGATIVE, [voice, 0, 0, 0]) == 1 {
        call(m, LAYER, [0, 0, 0, 0]);
        let hub = m.lw(HUB);
        let resource = m.lw(hub.at(0x28));
        call(m, RESET, [resource, 0, 0, 0]);
        call(m, PLAY_SOUND, [0x3c, 0, 0, 0]);
        let hub = m.lw(HUB);
        m.sb(hub.at(0x7d), 2);
        let hub = m.lw(HUB);
        m.sh(hub.at(0x86), 0);
        hub
    } else {
        step_up(m)
    };
    m.pop_frame(0x18);
    v
}

/// The hub step + 1. Returns it.
fn step_up(m: &mut Machine) -> u32 {
    let hub = m.lw(HUB);
    let step = u32::from(m.lhu(hub.at(0x86))).wrapping_add(1);
    m.sh(hub.at(0x86), step as u16);
    step
}

/// The disc-check request: the lid check (8002B428), layer 0 = 0x200000B,
/// style byte 0x3D = `style`.
fn disc_prompt(m: &mut Machine, style: u8) {
    call(m, LID_OPENED, [0; 4]);
    call(m, LAYER, [0, 0x0200_000b, 0, 0]);
    let styles = m.lw(STYLES);
    let bytes = m.lw(styles.at(0x5c));
    m.sb(bytes.at(0x3d), style);
}

/// 800F2A8C: by the simulator voice's stream kind (8002CBA4): 1 or 3 the
/// disc prompt with style 0, 2 with style 1, then the step advances
/// (returns it); others return 2.
pub fn disc_kind_f2a8c(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let voice = simulator_voice(m);
    let kind = call(m, STREAM_KIND, [voice, 0, 0, 0]);
    let v = match kind {
        1 | 3 => {
            disc_prompt(m, 0);
            step_up(m)
        }
        2 => {
            disc_prompt(m, 1);
            step_up(m)
        }
        _ => 2,
    };
    m.pop_frame(0x20);
    v
}

/// The disc swap's end (800F2B6C / 800F2C34 tails): with pad bit 0x20 and
/// the disc state not 1, the swap result (8002B46C): 1 -> hub +0x7D = 1
/// and step 0, else +0x7D = 5 and step 3.
fn swap_check(m: &mut Machine, state: u32, other: u32) -> u32 {
    let context = m.gw(CONTEXT);
    let pad = m.lw(context.at(0x12c)) & 0x20;
    if pad == 0 {
        return 0;
    }
    if state == 1 {
        return pad;
    }
    let result = call(m, SWAP_RESULT, [0; 4]);
    if result == 1 {
        let hub = m.lw(HUB);
        m.sb(hub.at(0x7d), 1);
        let hub = m.lw(HUB);
        m.sh(hub.at(0x86), 0);
        hub
    } else {
        let hub = m.lw(HUB);
        m.sb(hub.at(0x7d), 5);
        let hub = m.lw(HUB);
        m.sh(hub.at(0x86), 3);
        if other == 0 { hub } else { other }
    }
}

/// 800F2B6C: the disc state (8002B528): 3 advances the step; then the
/// swap check. Returns as the original leaves `v0` (0, 0x20, or the hub
/// block).
pub fn disc_wait_f2b6c(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let state = call(m, DISC_STATE, [0; 4]);
    if state == 3 {
        step_up(m);
    }
    let v = swap_check(m, state, 0);
    m.pop_frame(0x20);
    v
}

/// 800F2C34: the disc state: 0 or 2 advance the step, 1 sets style byte
/// 0x3D = 4; then the swap check (the last case returns 3).
pub fn disc_wait2_f2c34(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let state = call(m, DISC_STATE, [0; 4]);
    if state == 0 || state == 2 {
        step_up(m);
    } else if state == 1 {
        let styles = m.lw(STYLES);
        let bytes = m.lw(styles.at(0x5c));
        m.sb(bytes.at(0x3d), 4);
    }
    let v = swap_check(m, state, 3);
    m.pop_frame(0x18);
    v
}

/// 800F2DC0: by the simulator voice's stream kind: 1 or 3 the disc prompt
/// with style 2 and step 2 (returns 2), 2 with style 3 and step 2 (returns
/// the hub block); others return 2.
pub fn disc_kind2_f2dc0(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let voice = simulator_voice(m);
    let kind = call(m, STREAM_KIND, [voice, 0, 0, 0]);
    let v = match kind {
        1 | 3 => {
            disc_prompt(m, 2);
            let hub = m.lw(HUB);
            m.sh(hub.at(0x86), 2);
            2
        }
        2 => {
            disc_prompt(m, 3);
            let hub = m.lw(HUB);
            m.sh(hub.at(0x86), kind as u16);
            hub
        }
        _ => 2,
    };
    m.pop_frame(0x20);
    v
}

/// The mission's disc byte (0x80112750 + mission).
fn mission_disc(m: &mut Machine) -> u8 {
    let context = m.gw(CONTEXT);
    let mission = lh(m, context.at(0x38c));
    m.lbu(MISSION_DISCS.wrapping_add(mission))
}

/// The disc is right: layer 0 = 3, the hub's +0x7C/+0x7D = 0, +0x1C6 = 0,
/// `[0x80112DB8]` released, sound 0x3B. Returns the sound's result.
fn disc_ready(m: &mut Machine) -> u32 {
    call(m, LAYER, [0, 3, 0, 0]);
    let hub = m.lw(HUB);
    m.sb(hub.at(0x7c), 0);
    let hub = m.lw(HUB);
    m.sb(hub.at(0x7d), 0);
    let swap = m.lw(SWAP_BLOCK);
    let hub = m.lw(HUB);
    m.sh(hub.at(0x1c6), 0);
    call(m, RESET, [swap, 0, 0, 0]);
    call(m, PLAY_SOUND, [0x3b, 0, 0, 0])
}

/// 800F2EB0: by the stream state (8002B504): 1 with the mission's disc
/// byte 1 or 3, or 2 with it 2 or 3, the disc is ready (returns the sound's
/// result); 1 otherwise the prompt with style 1, 2 otherwise and 4 or -1
/// the prompt with style 0 (both then advance the step and return it);
/// other states return -1.
pub fn disc_state_f2eb0(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let state = call(m, STREAM_STATE, [0; 4]);
    let v = match state {
        1 => {
            let disc = mission_disc(m);
            if disc == 1 || disc == 3 {
                disc_ready(m)
            } else {
                disc_prompt(m, 1);
                step_up(m)
            }
        }
        2 => {
            if mission_disc(m).wrapping_sub(2) < 2 {
                disc_ready(m)
            } else {
                disc_prompt(m, 0);
                step_up(m)
            }
        }
        4 | u32::MAX => {
            disc_prompt(m, 0);
            step_up(m)
        }
        _ => u32::MAX,
    };
    m.pop_frame(0x18);
    v
}

/// 800F3020: the disc state 3 advances the step (returns it); else returns
/// 0x80110000 (the original's `v0`).
pub fn lid_wait_f3020(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let v = if call(m, DISC_STATE, [0; 4]) == 3 {
        step_up(m)
    } else {
        0x8011_0000
    };
    m.pop_frame(0x18);
    v
}

/// 800F3064: the disc state 0 or 2 advance the step (returns it), 1 sets
/// style byte 0x3D = 4 (returns 4); others return 0x80110000.
pub fn lid_wait2_f3064(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let v = match call(m, DISC_STATE, [0; 4]) {
        0 | 2 => step_up(m),
        1 => {
            let styles = m.lw(STYLES);
            let bytes = m.lw(styles.at(0x5c));
            m.sb(bytes.at(0x3d), 4);
            4
        }
        _ => 0x8011_0000,
    };
    m.pop_frame(0x18);
    v
}

/// 800F30D4: as 800F2EB0, the prompt styles 3 (state 1) and 2 (others) and
/// hub step 1 (returns the hub block).
pub fn disc_state2_f30d4(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let state = call(m, STREAM_STATE, [0; 4]);
    let prompt = match state {
        1 => {
            let disc = mission_disc(m);
            if disc == 1 || disc == 3 {
                None
            } else {
                Some(3)
            }
        }
        2 => {
            if mission_disc(m).wrapping_sub(2) < 2 {
                None
            } else {
                Some(2)
            }
        }
        4 | u32::MAX => Some(2),
        _ => {
            m.pop_frame(0x20);
            return u32::MAX;
        }
    };
    let v = match prompt {
        None => disc_ready(m),
        Some(style) => {
            disc_prompt(m, style);
            let hub = m.lw(HUB);
            m.sh(hub.at(0x86), 1);
            hub
        }
    };
    m.pop_frame(0x20);
    v
}

/// 800F3218 (pitch offset): restart the five hub channels: release them
/// (8002DDA8), start sounds 0x5D..0x61 into hub +0x130..+0x140, each pitch
/// its table pitch (8002DE68) plus `offset` (8002DE24). Returns the last
/// pitch call's result.
pub fn restart_channels_f3218(m: &mut Machine, offset: u32) -> u32 {
    m.push_frame(0x20);
    const SLOTS: [u32; 5] = [0x130, 0x134, 0x138, 0x13c, 0x140];
    for slot in SLOTS {
        let hub = m.lw(HUB);
        let channel = m.lw(hub.at(slot));
        call(m, RELEASE_CHANNEL, [channel, 0, 0, 0]);
    }
    for (i, slot) in SLOTS.iter().enumerate() {
        let channel = call(m, PLAY_SOUND, [0x5d + i as u32, 0, 0, 0]);
        let hub = m.lw(HUB);
        m.sw(hub.at(*slot), channel);
    }
    let mut v = 0;
    for (i, slot) in SLOTS.iter().enumerate() {
        let pitch = sext16(call(m, SOUND_PITCH, [0x5d + i as u32, 0, 0, 0]));
        let hub = m.lw(HUB);
        let channel = m.lw(hub.at(*slot));
        v = call(
            m,
            SET_PITCH,
            [channel, offset.wrapping_add(pitch) & 0xffff, 0, 0],
        );
    }
    m.pop_frame(0x20);
    v
}
