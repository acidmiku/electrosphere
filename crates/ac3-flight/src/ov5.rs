//! Overlay module 5 (game state 9): the music player. Loaded at
//! [`OVERLAY_BASE`](crate::common::OVERLAY_BASE) from `ACE.BIN` file 5
//! (7,288 bytes; arena from 0x800CBA38), reached from the mission hub
//! (overlay 2 returns 0x60000009 and sets context +0x3A4 = 3, so the player
//! returns to the hub).
//!
//! The player runs the resident flight (state 2's callbacks 800355F0..,
//! with context +0x3A9 = 3, a demo flight) in the background, and on top a
//! small transport panel: previous / play / next / stop / eject icons, the
//! track number and time in the debug font, and the track title (39 tracks,
//! names at 800CB990, stream records at 800CB7BC). Pad: right/left select,
//! START plays or pauses, triangle/circle stop; eject leaves after 20
//! frames.
//!
//! | State 9 substate | Routine |
//! | --- | --- |
//! | 0 | 800CA0A0 [`start`] |
//! | 1 | 800CA0DC [`frame`] |
//!
//! Every routine is claimed only while module 5 is loaded
//! ([`common::loaded_module`]).
use crate::{
    arena,
    common::{self, CONTEXT},
    dispatch,
    machine::{Machine, SmallData},
    mainloop::transition::{NEXT_SUBSTATE, RESET_ARENA, SET_STATE},
    states::Invoker,
};

/// Overlay module number.
pub const MODULE: u32 = 5;
/// 800CBA30: substate of the background flight (state 2's callbacks).
const FLIGHT_SUBSTATE: u32 = 0x800c_ba30;
/// 800CBA34: the 0x20-byte player block: +0 flags (1 playing, 2 paused),
/// +4 playing track, +8 selected track (0..38), +0xC panel mode, +0x10
/// stream time, +0x14 eject countdown, +0x18 pressed buttons, +0x1C button
/// highlight time.
const PLAYER: u32 = 0x800c_ba34;
/// 800AEE40: state 2's callback table.
const FLIGHT_CALLBACKS: u32 = 0x800a_ee40;
/// 800CAE5C: the panel's TIM images.
const PANEL_IMAGES: u32 = 0x800c_ae5c;
/// 800CB7BC: 12-byte stream records of the 39 tracks.
const TRACK_STREAMS: u32 = 0x800c_b7bc;
/// 800CB990: pointers to the track titles.
const TRACK_TITLES: u32 = 0x800c_b990;
/// 800C9FA8: the four status strings ("PAUSE  :", "PLAY   :", ...).
const STATUS_TEXT: u32 = 0x800c_9fa8;
/// 800C9FB8 "--", 800C9FBC "--:--", 800C9FC4 ":".
const NO_TRACK: u32 = 0x800c_9fb8;
const NO_TIME: u32 = 0x800c_9fbc;
const TIME_COLON: u32 = 0x800c_9fc4;

const UPLOAD_TIMS: u32 = 0x8002_6d00;
const RESET_CD_QUEUE: u32 = 0x8002_c280;
const STREAM_STATUS: u32 = 0x8002_c158;
const ACTIVE_FILE: u32 = 0x8002_c6a8;
const DRIVER_FLAGS: u32 = 0x8002_b160;
const STREAM_STATE: u32 = 0x8002_b0dc;
const PLAY_STREAM: u32 = 0x8002_c4b0;
const PAUSE_STREAM: u32 = 0x8002_c2cc;
const PLAY_SOUND: u32 = 0x8002_d724;
const PAUSE_TIME: u32 = 0x8002_b174;
const DRAW_TEXT: u32 = 0x8003_52bc;
const TEXT_WIDTH: u32 = 0x8003_5384;

fn player(m: &mut Machine) -> u32 {
    m.lw(PLAYER)
}

/// 800C9FE0: background flight substate 0.
pub fn reset_flight(m: &mut Machine) {
    m.sw(FLIGHT_SUBSTATE, 0);
}

/// 800C9FEC: run the background flight's current callback (state 2's
/// table, the caller's `a0`/`a1` passed on) and apply its result like the
/// dispatcher: SET_SUBSTATE / NEXT_SUBSTATE move the substate, RESET_ARENA
/// requests DrawSync (context bit 8). Returns 1 once the flight reached
/// substate 9.
pub fn run_flight(m: &mut Machine, c: &mut Invoker, a0: u32, a1: u32) -> u32 {
    m.push_frame(0x18);
    let substate = m.lw(FLIGHT_SUBSTATE);
    let callback = m.lw(FLIGHT_CALLBACKS.wrapping_add(substate << 2));
    let result = c(m, callback, [a0, a1, 0, 0]);
    if result != 0 {
        if result & 0x1000_0000 != 0 {
            m.sw(FLIGHT_SUBSTATE, result & 0xff);
        } else if result & NEXT_SUBSTATE != 0 {
            let substate = m.lw(FLIGHT_SUBSTATE);
            m.sw(FLIGHT_SUBSTATE, substate.wrapping_add(1));
        }
        if result & RESET_ARENA != 0 {
            let context = m.gw(CONTEXT);
            let word = m.lw(context);
            m.sw(context, word | 8);
        }
    }
    m.pop_frame(0x18);
    u32::from(m.lw(FLIGHT_SUBSTATE) == 9)
}

/// 800CA154: allocate and clear the player block, upload the panel images
/// (80026D00).
pub fn setup_player(m: &mut Machine, c: &mut Invoker) {
    m.push_frame(0x18);
    let block = arena::alloc(m, 0x20);
    m.sw(PLAYER, block);
    for offset in [4, 8, 0, 0xc, 0x10, 0x14, 0x18, 0x1c] {
        m.sw(block + offset, 0);
    }
    c(m, UPLOAD_TIMS, [PANEL_IMAGES, 0, 0, 0]);
    m.pop_frame(0x18);
}

/// 800CA0A0 (state 9, substate 0): context bit 0x10000000, the player
/// block, background flight substate 0. Returns NEXT_SUBSTATE.
pub fn start(m: &mut Machine, c: &mut Invoker) -> u32 {
    m.push_frame(0x18);
    let context = m.gw(CONTEXT);
    let word = m.lw(context);
    m.sw(context, word | 0x1000_0000);
    setup_player(m, c);
    reset_flight(m);
    m.pop_frame(0x18);
    NEXT_SUBSTATE
}

/// 800CA0DC (state 9, substate 1): one frame: the background flight (with
/// context +0x3A9 = 3, +0x3AA = 0; cleared again after it), then, once the
/// flight runs, the player ([`update`]); when that ends, a CD queue reset
/// and SET_STATE to context +0x3A4 with an arena reset. Returns 0
/// otherwise.
pub fn frame(m: &mut Machine, c: &mut Invoker, a0: u32, a1: u32) -> u32 {
    m.push_frame(0x18);
    let context = m.gw(CONTEXT);
    m.sb(context + 0x3a9, 3);
    let context = m.gw(CONTEXT);
    m.sb(context + 0x3aa, 0);
    let result = if run_flight(m, c, a0, a1) == 0 {
        0
    } else {
        let context = m.gw(CONTEXT);
        m.sb(context + 0x3a9, 0);
        if update(m, c) == 0 {
            0
        } else {
            c(m, RESET_CD_QUEUE, [0; 4]);
            let context = m.gw(CONTEXT);
            m.lw(context + 0x3a4) | RESET_ARENA | SET_STATE
        }
    };
    m.pop_frame(0x18);
    result
}

/// 800CA6C8(ot, packet, mode): a DR_TPAGE (`mode & 0x9FF`) linked first
/// into `ot`; returns `packet + 8`.
pub fn tpage_packet(m: &mut Machine, ot: u32, packet: u32, mode: u32) -> u32 {
    m.sb(packet + 3, 1);
    m.sw(packet + 4, (mode & 0x9ff) | 0xe100_0000);
    let tag = m.lw(packet);
    let link = m.lw(ot);
    m.sw(packet, (tag & 0xff00_0000) | (link & 0x00ff_ffff));
    let link = m.lw(ot);
    m.sw(ot, (link & 0xff00_0000) | (packet & 0x00ff_ffff));
    packet.wrapping_add(8)
}

/// 800CA1A8: the player. Stream status (8002C158(1)), active file, pause
/// time, driver flags (paused bit) and stream state; a finished track
/// advances the selection. With the stream idle and no eject pending the
/// pad acts: right next, left previous (restart within 30 frames of
/// play), START play (or pause/resume while playing), triangle/circle
/// stop. The selection wraps 0..38; the panel mode (+0xC) and stream time
/// are recorded, the action applied (play: 8002C4B0 on the track's
/// record; pause: 8002C2CC; stop: CD queue reset; eject: 20-frame
/// countdown), the panel drawn ([`draw`], context table +0xC, packets
/// +0x14). Returns 1 when the eject countdown ends.
pub fn update(m: &mut Machine, c: &mut Invoker) -> u32 {
    m.push_frame(0x30);
    let mut action = 0u32;
    let status = c(m, STREAM_STATUS, [1, 0, 0, 0]);
    let file = c(m, ACTIVE_FILE, [0; 4]);
    let elapsed = c(m, PAUSE_TIME, [0; 4]);
    let p = player(m);
    let mut idle = true;
    let highlight = m.lw(p + 0x1c);
    if highlight != 0 {
        m.sw(p + 0x1c, highlight.wrapping_sub(1));
    }
    let flags = c(m, DRIVER_FLAGS, [0; 4]);
    let p = player(m);
    let word = m.lw(p);
    if flags & 0x4000_0000 != 0 {
        m.sw(p, word | 2);
    } else {
        m.sw(p, word & !2);
    }
    let state = c(m, STREAM_STATE, [0; 4]);
    if state.wrapping_sub(1) >= 3 {
        idle = false;
    }
    let mut elapsed = elapsed;
    if status == 0 {
        let p = player(m);
        if m.lw(p) & 1 != 0 {
            action = 1;
            let track = m.lw(p + 8).wrapping_add(1);
            m.sw(p + 8, track);
        }
    }
    let p = player(m);
    if (status as i32) < 6 && action == 0 && m.lw(p + 0x14) == 0 && idle {
        let mut buttons = 0u32;
        let context = m.gw(CONTEXT);
        if m.lw(context + 0x130) & 8 != 0 {
            action = 1;
            buttons = 8;
            let track = m.lw(p + 8).wrapping_add(1);
            let stopped = !m.lw(p) & 1;
            m.sw(p + 8, track);
            if stopped != 0 {
                m.sw(p + 8, 0);
            }
        }
        let context = m.gw(CONTEXT);
        if m.lw(context + 0x130) & 4 != 0 {
            let back = (status as i32) >= 2 || (elapsed as i32) < 0x1e || file == 0xffff_ffff;
            if back {
                let p = player(m);
                let track = m.lw(p + 8).wrapping_sub(1);
                m.sw(p + 8, track);
            }
            let p = player(m);
            action = 1;
            buttons |= 4;
            if !m.lw(p) & 1 != 0 {
                m.sw(p + 8, 0x26);
            }
        }
        if action == 0 {
            let context = m.gw(CONTEXT);
            if m.lw(context + 0x12c) & 0x10 != 0 {
                let p = player(m);
                if m.lw(p) & 1 != 0 {
                    if (file as i32) >= 0 && elapsed != 0 {
                        action = 2;
                        buttons |= 1;
                    }
                } else {
                    action = 1;
                    m.sw(p + 0x1c, 4);
                    buttons |= 1;
                }
            }
        }
        let context = m.gw(CONTEXT);
        if m.lw(context + 0x12c) & 0xa0 != 0 {
            let p = player(m);
            if m.lw(p) & 1 != 0 {
                action = 3;
                m.sw(p + 0x1c, 4);
            } else {
                action = 4;
            }
            buttons |= 2;
        }
        let p = player(m);
        m.sw(p + 0x18, buttons);
    }
    let p = player(m);
    if (m.lw(p + 8) as i32) < 0 {
        m.sw(p + 8, 0x26);
    }
    let p = player(m);
    if (m.lw(p + 8) as i32) >= 0x27 {
        m.sw(p + 8, 0);
    }
    let mut mode = 0u32;
    if status != 0 {
        let p = player(m);
        let word = m.lw(p);
        mode = if word & 1 != 0 { 2 } else { 4 };
        if word & 2 != 0 {
            mode = 3;
        }
        let shown =
            (status as i32) < 2 && (file as i32) >= 0 && elapsed != 0 && m.lw(p + 4) == m.lw(p + 8);
        if !shown {
            mode = 1;
        }
    }
    let p = player(m);
    if m.lw(p + 0x14) != 0 {
        mode |= 5;
    }
    m.sw(p + 0xc, mode);
    match action {
        1 => {
            if status != 0 {
                c(m, RESET_CD_QUEUE, [0; 4]);
            }
            let p = player(m);
            let track = m.lw(p + 8);
            let record = TRACK_STREAMS.wrapping_add(track.wrapping_mul(12));
            c(m, PLAY_STREAM, [record, 1, 0, 0]);
            let p = player(m);
            elapsed = 0;
            let word = m.lw(p) | 1;
            let track = m.lw(p + 8);
            m.sw(p + 4, track);
            m.sw(p, word & !2);
        }
        2 => {
            let paused = m.lw(p) & 2 != 0;
            c(m, PAUSE_STREAM, [u32::from(!paused), 0, 0, 0]);
        }
        3 => {
            c(m, RESET_CD_QUEUE, [0; 4]);
            let p = player(m);
            let word = m.lw(p) & !1;
            m.sw(p, word & !2);
        }
        4 if m.lw(p + 0x14) == 0 => m.sw(p + 0x14, 0x14),
        _ => {}
    }
    let p = player(m);
    let context = m.gw(CONTEXT);
    m.sw(p + 0x10, elapsed);
    let ot = m.lw(context + 0xc);
    let packets = m.lw(context + 0x14);
    let next = draw(m, c, ot, packets);
    let context = m.gw(CONTEXT);
    let p = player(m);
    m.sw(context + 0x14, next);
    let countdown = m.lw(p + 0x14);
    let result = if countdown == 0 {
        0
    } else {
        let countdown = countdown.wrapping_sub(1);
        m.sw(p + 0x14, countdown);
        u32::from((countdown as i32) <= 0)
    };
    m.pop_frame(0x30);
    result
}

/// Status flags of the panel.
mod panel {
    /// Previous / next / play / stop highlighted, playing, eject pending.
    pub const NEXT: u32 = 1;
    pub const PREVIOUS: u32 = 2;
    pub const PLAY: u32 = 4;
    pub const STOP: u32 = 0x18;
    pub const PAUSE: u32 = 0x20;
    pub const BLINK_OFF: u32 = 0x40;
    pub const NO_TIME: u32 = 0x80;
    pub const PLAYING: u32 = 0x800;
}

/// One 0x14-byte SPRT (`code` and colour word, x/y halfwords, uv/CLUT
/// word, size word) linked first into `ot`.
#[allow(clippy::too_many_arguments)]
fn sprite(m: &mut Machine, ot: u32, p: u32, color: u32, x: u32, y: u32, uv: u32, size: u32) {
    m.sw(p + 4, color);
    m.sh(p + 8, x as u16);
    m.sw(p + 0xc, uv);
    m.sw(p + 0x10, size);
    m.sh(p + 0xa, y as u16);
    let link = m.lw(ot);
    m.sw(p, link | 0x0400_0000);
    m.sw(ot, (p << 8) >> 8);
}

/// 800CA720(ot, packets): the panel: the status sprite with "PAUSE /
/// PLAY / SEARCH / ..." and the track number (debug font, 80027168 /
/// 80027228), the time (minutes:seconds of the stream time at 50 Hz,
/// blinking while paused), the track title twice (800352BC, colours
/// 0x7F7F7F and 0x7F0000) underlined with a white and a red TILE, the
/// four transport
/// icons lit by the status, the panel background and two DR_TPAGEs.
/// Returns the next packet.
pub fn draw(m: &mut Machine, c: &mut Invoker, ot: u32, packets: u32) -> u32 {
    let frame = m.push_frame(0x50);
    let p = player(m);
    let selected = m.lw(p + 8);
    let mode = m.lw(p + 0xc);
    let time = m.lw(p + 0x10);
    let mut number = selected.wrapping_add(1);
    let mut status_text = 0u32;
    let mut flags = 0u32;
    match mode {
        0 | 4 => {
            number = 0;
            status_text = 1;
        }
        1 => {
            let context = m.gw(CONTEXT);
            if m.lw(context + 0x24) & 2 != 0 {
                status_text = 2;
            }
        }
        2 => status_text = 2,
        3 => {
            status_text = 3;
            let context = m.gw(CONTEXT);
            if m.lw(context + 0x24) & 8 != 0 {
                flags |= panel::BLINK_OFF;
            }
        }
        _ => {
            number = 0;
            status_text = 0;
            flags = 0x88;
        }
    }
    let p = player(m);
    let buttons = m.lw(p + 0x18);
    if buttons & 8 != 0 {
        flags |= panel::NEXT;
    }
    if buttons & 4 != 0 {
        flags |= panel::PREVIOUS;
    }
    if m.lw(p) & 1 != 0 {
        flags |= panel::PLAYING;
        if buttons & 1 != 0 {
            flags |= panel::PAUSE;
        }
        if buttons & 2 != 0 {
            flags |= 0x10;
        }
    } else {
        if buttons & 1 != 0 {
            flags |= panel::PLAY;
        }
        if buttons & 2 != 0 {
            flags |= panel::STOP;
        }
    }
    let p = player(m);
    if m.lw(p + 0x1c) != 0 {
        flags ^= panel::PLAYING;
    }
    if flags & 0x37 != 0 && m.lw(p + 0x14) == 0 {
        c(m, PLAY_SOUND, [0x2e, 0, 0, 0]);
    }
    if flags & 8 != 0 {
        let p = player(m);
        if m.lw(p + 0x14) == 0x14 {
            c(m, PLAY_SOUND, [0x30, 0, 0, 0]);
        }
    }
    let text_y = 0xedu32;
    for i in 0..4 {
        let word = m.lw(STATUS_TEXT + 4 * i);
        m.sw(frame + 0x18 + 4 * i, word);
    }
    let base_y = 0xecu32;
    let mut p = packets;
    sprite(
        m,
        ot,
        p,
        0x6608_1020,
        0x24,
        base_y,
        0x4c3c_1800,
        0x0008_0040,
    );
    p = p.wrapping_add(0x14);
    let label = m.lw(frame + 0x18 + (status_text << 2));
    let x = queue_text_call(m, c, 0x27, text_y, label).wrapping_add(1);
    let mut x = if number != 0 {
        queue_number_call(m, c, x, text_y, number, 0x102)
    } else {
        queue_text_call(m, c, x, 0xed, NO_TRACK)
    };
    x = x.wrapping_add(6);
    if !flags & panel::BLINK_OFF != 0 {
        let t = time as i32;
        let seconds_total = (((i64::from(t) * 0x51eb_851f) >> 36) as i32).wrapping_sub(t >> 31);
        let divide60 = |v: i32| -> i32 {
            let hi = ((i64::from(v) * i64::from(0x8888_8889u32 as i32)) >> 32) as i32;
            (hi.wrapping_add(v) >> 5).wrapping_sub(v >> 31)
        };
        let minutes_total = divide60(seconds_total);
        let seconds = seconds_total.wrapping_sub(minutes_total.wrapping_mul(60));
        let hours = divide60(minutes_total);
        let minutes = minutes_total.wrapping_sub(hours.wrapping_mul(60));
        if flags & panel::NO_TIME != 0 {
            queue_text_call(m, c, x, text_y, NO_TIME);
        } else {
            let x = queue_number_call(m, c, x, text_y, minutes as u32, 0x102);
            let x = queue_text_call(m, c, x, text_y, TIME_COLON);
            queue_number_call(m, c, x, text_y, seconds as u32, 0x102);
        }
    }
    let title_x = 0x24u32 + 0x50;
    let title_y = base_y - 8;
    if flags & 0x100 == 0 {
        // The title (flag 0x100 is never set).
        let title = m.lw(TRACK_TITLES.wrapping_add(number << 2));
        let next = tpage_packet(m, ot, p, 0x1f);
        m.sw(frame + 0x10, 0x007f_7f7f);
        let name = m.lw(TRACK_TITLES.wrapping_add(number << 2));
        m.sw(frame + 0x14, name);
        let next = c(m, DRAW_TEXT, [ot, next, title_x, title_y]);
        let shadow_x = 0x24u32 + 0x51;
        m.sw(frame + 0x10, 0x007f_0000);
        m.sw(frame + 0x14, title);
        let tiles = c(m, DRAW_TEXT, [ot, next, shadow_x, base_y - 7]);
        let width = c(m, TEXT_WIDTH, [title, 0, 0, 0]);
        m.sw(tiles + 4, 0x62ff_ffff);
        m.sh(tiles + 8, title_x as u16);
        m.sh(tiles + 0xa, (base_y + 6) as u16);
        m.sh(tiles + 0xc, width as u16);
        m.sh(tiles + 0xe, 1);
        let link = m.lw(ot);
        m.sw(tiles, link | 0x0300_0000);
        m.sw(ot, (tiles << 8) >> 8);
        let second = tiles.wrapping_add(0x10);
        m.sw(second + 4, 0x62ff_0000);
        m.sh(second + 0xa, (base_y + 7) as u16);
        m.sh(second + 8, shadow_x as u16);
        m.sh(second + 0xc, width as u16);
        m.sh(second + 0xe, 1);
        let link = m.lw(ot);
        m.sw(tiles + 0x10, link | 0x0300_0000);
        m.sw(ot, (second << 8) >> 8);
        p = tpage_packet(m, ot, tiles.wrapping_add(0x20), 0x35);
    }
    const LIT: u32 = 0x0080_8080;
    const DIM: u32 = 0x0038_3838;
    let lit = |on: bool| 0x6400_0000 | if on { LIT } else { DIM };
    let icon_y = 0xdeu32;
    let mut icon_x = 0x26u32;
    let playing = flags & panel::PLAYING != 0;
    let icons = [
        (flags & panel::PREVIOUS != 0, 0x4c3c_0000),
        (
            flags & 0x24 != 0,
            if playing { 0x4c3c_0050 } else { 0x4c3c_0030 },
        ),
        (flags & panel::NEXT != 0, 0x4c3c_0010),
        (
            flags & panel::STOP != 0,
            if playing { 0x4c3c_0040 } else { 0x4c3c_0020 },
        ),
    ];
    for (on, uv) in icons {
        sprite(m, ot, p, lit(on), icon_x, icon_y, uv, 0x0008_000c);
        icon_x = icon_x.wrapping_add(0xf);
        p = p.wrapping_add(0x14);
    }
    let panel_color = 0x20 | (0x20 << 7) | (0x10 << 15) | 0x6600_0000;
    sprite(m, ot, p, panel_color, 0x20, 0xdc, 0x4c3c_0800, 0x000d_0044);
    let next = tpage_packet(m, ot, p.wrapping_add(0x14), 0x1f);
    let next = tpage_packet(m, ot, next, 0x5f);
    m.pop_frame(0x50);
    next
}

fn queue_text_call(m: &mut Machine, c: &mut Invoker, x: u32, y: u32, text: u32) -> u32 {
    c(m, 0x8002_7168, [x, y, text, 0])
}

fn queue_number_call(
    m: &mut Machine,
    c: &mut Invoker,
    x: u32,
    y: u32,
    value: u32,
    format: u32,
) -> u32 {
    c(m, 0x8002_7228, [x, y, value, format])
}

/// Native targets of module 5 (only while it is loaded).
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if !(0x800c_9dc0..0x800c_ba38).contains(&target) || common::loaded_module(m) != MODULE {
        return None;
    }
    let [a0, a1, a2, _] = args;
    let mut call = dispatch::call;
    let c: &mut Invoker = &mut call;
    Some(match target {
        0x800c_9fe0 => {
            reset_flight(m);
            0
        }
        0x800c_9fec => run_flight(m, c, a0, a1),
        0x800c_a0a0 => start(m, c),
        0x800c_a0dc => frame(m, c, a0, a1),
        0x800c_a154 => {
            setup_player(m, c);
            0
        }
        0x800c_a1a8 => update(m, c),
        0x800c_a6c8 => tpage_packet(m, a0, a1, a2),
        0x800c_a720 => draw(m, c, a0, a1),
        _ => return None,
    })
}
