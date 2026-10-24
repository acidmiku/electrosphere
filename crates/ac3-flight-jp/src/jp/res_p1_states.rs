//! Patches of `states` routines whose JP instructions differ from PAL
//! (REVIEW.md §2-§3, resident part 1): the module loader (loading substates
//! 0 and 1), the movie state 5 and the picture state 6. Each is listed in
//! [`REPLACES`](super::REPLACES).
//!
//! JP context layout: the movie/picture index PAL keeps at +0x3AC is at
//! +0x3A8, and the fields PAL keeps at +0x3A0..+0x3AB are 4 lower (the
//! word removed near 0x38C..0x3B0, JP_PORT_PLAN §4.3).
use crate::{
    GP, arena,
    common::CONTEXT,
    machine::{Machine, SmallData},
    mainloop::transition::{LOAD_COMPLETE, NEXT_SUBSTATE, RESET_ARENA, SET_STATE},
    sdk,
    states::{
        CLEAR_IMAGE, Invoker, MODULE_READ_COMPLETE, decoder as dec, move_image_packet,
        movie_decode, movie_decoder_init, movie_finish, movie_rect_zero, movie_stop,
        movie_stream_start, movie_wait_frame, picture_player, picture_state_finish,
        queue_file_read,
    },
};

/// `[gp+0x2D4]`: display request block (`states.rs`).
const DISPLAY: u32 = 0x2d4;

// ---- module loader (table 800B4674) ----------------------------------------------------

/// `gp+0x2BC`: loading-screen fade mode (1: 320x240, 2: 320x480, else none).
const LOAD_MODE: u32 = 0x2bc;
/// `gp+0x2C0`: module being loaded (the CD file index).
const LOAD_MODULE: u32 = 0x2c0;
/// `gp+0x554`: nonzero while the module read is pending.
const LOAD_PENDING: u32 = 0x554;
/// 8002CAD0 (`loading.rs`): the file of the active CD request.
const ACTIVE_FILE: u32 = 0x8002_cad0;
/// 8002CA2C (`loading.rs`): read progress of the active request.
const READ_PROGRESS: u32 = 0x8002_ca2c;
/// 80093B2C: full-screen fade rectangle `(ot, packets, rect, level)`.
const FADE_RECT: u32 = 0x8009_3b2c;
/// 800A2098 VSync(mode), 800A8194 DrawSync(mode).
const VSYNC: u32 = 0x800a_2098;
const DRAW_SYNC: u32 = 0x800a_8194;
/// `gp+0x2C4`: the RECT 8009B7CC clears once the module is loaded.
const LOADED_CLEAR_RECT: u32 = GP + 0x2c4;

/// 8009B6B8 (JP; PAL 80099BC8, near, `del:1 imm:2 ins:2`: the copied frame
/// is 320x240, PAL 320x256) (loading substate 0): begin loading overlay
/// `module` to `address`. Display requests 1|4 (drawing environments, no
/// clearing) and frame interval 0. In display phase 0 the current frame is
/// copied into the other buffer (800363B0 on the context's table +0x10) and
/// the loading screen fades over 320x240 (mode 1); phase 1 uses mode 2
/// (320x480), others none. Then the read of file `module` is queued with
/// completion 8009B6AC. Returns NEXT_SUBSTATE.
pub fn module_load_begin(m: &mut Machine, c: &mut Invoker, module: u32, address: u32) -> u32 {
    let frame = m.push_frame(0x30);
    let display = m.gw(DISPLAY);
    let flags = m.lw(display);
    let phase = m.lw(display + 4);
    m.sw(display + 0x14, 0);
    m.sw(display, flags | 5);
    if phase == 0 {
        let context = m.gw(CONTEXT);
        let (source_y, destination_y) = if m.lw(context + 4) != 0 {
            (0x100, 0)
        } else {
            (0, 0x100)
        };
        m.sw(frame + 0x10, 0);
        m.sw(frame + 0x14, destination_y);
        m.sw(frame + 0x18, 0x140);
        m.sw(frame + 0x1c, 0xf0);
        let ot = m.lw(context + 0x10);
        let packets = m.lw(context + 0x14);
        let next = move_image_packet(m, ot, packets, 0, source_y, 0, destination_y, 0x140, 0xf0);
        let context = m.gw(CONTEXT);
        m.sw(context + 0x14, next);
        m.set_gw(LOAD_MODE, 1);
    } else if phase == 1 {
        m.set_gw(LOAD_MODE, 2);
    } else {
        m.set_gw(LOAD_MODE, 0);
    }
    m.set_gw(LOAD_MODULE, module);
    m.set_gw(LOAD_PENDING, 1);
    queue_file_read(m, c, module, address, MODULE_READ_COMPLETE);
    m.pop_frame(0x30);
    NEXT_SUBSTATE
}

/// 8009B7CC (JP; PAL 80099CD8, near, `call+:3 imm:3 ins:15`: fade heights
/// 0xF0 / 0x1E0 for PAL 0x100 / 0x200, and the JP clear on completion)
/// (loading substate 1): while the module's read is the active CD request,
/// draw the loading fade (80093B2C on the context's table +0xC, rectangle at
/// scratchpad 0, level `-progress`) in modes 1/2. When the read completed
/// (`gp+0x554` clear): JP first, in display phase 0, waits a vertical blank
/// (VSync(0)), clears the RECT at gp+0x2C4 to black (ClearImage) and waits
/// for it (DrawSync(0)); then clear colour 0, request clearing (display bit
/// 8) and return LOAD_COMPLETE; otherwise 0.
pub fn module_load_poll(m: &mut Machine, c: &mut Invoker) -> u32 {
    m.push_frame(0x18);
    let active = c(m, ACTIVE_FILE, [0; 4]);
    if active == m.gw(LOAD_MODULE) {
        let progress = c(m, READ_PROGRESS, [0; 4]);
        let height = match m.gw(LOAD_MODE) {
            1 => Some(0xf0),
            2 => Some(0x1e0),
            _ => None,
        };
        if let Some(height) = height {
            let rect = crate::machine::SCRATCH_BASE;
            let context = m.gw(CONTEXT);
            m.sh(rect + 4, 0x140);
            m.sh(rect, 0);
            m.sh(rect + 2, 0);
            m.sh(rect + 6, height);
            let ot = m.lw(context + 0xc);
            let packets = m.lw(context + 0x14);
            let next = c(m, FADE_RECT, [ot, packets, rect, progress.wrapping_neg()]);
            let context = m.gw(CONTEXT);
            m.sw(context + 0x14, next);
        }
    }
    let result = if m.gw(LOAD_PENDING) != 0 {
        0
    } else {
        let display = m.gw(DISPLAY);
        if m.lw(display + 4) == 0 {
            c(m, VSYNC, [0; 4]);
            c(m, CLEAR_IMAGE, [LOADED_CLEAR_RECT, 0, 0, 0]);
            c(m, DRAW_SYNC, [0; 4]);
        }
        for offset in [0x10, 0x11, 0x12] {
            let display = m.gw(DISPLAY);
            m.sb(display + offset, 0);
        }
        let display = m.gw(DISPLAY);
        let flags = m.lw(display);
        m.sw(display, flags | 8);
        LOAD_COMPLETE
    };
    m.pop_frame(0x18);
    result
}
/// `gp+0x4CC`: frames spent in state-5 substate 2 before the player starts.
const MOVIE_DELAY: u32 = 0x4cc;
/// `[gp+0x4D0]`: the 0x44-byte movie block (`states.rs`).
const MOVIE: u32 = 0x4d0;
/// `[gp+0x4D4]`: the GMV decoder.
const DECODER: u32 = 0x4d4;
/// `[gp+0x218]`: movie music block (+0 sound data, +4 music on, +8 read
/// pending).
const MOVIE_MUSIC: u32 = 0x218;
/// `gp+0x21C`: index of the displayed movie rectangle.
const MOVIE_SCREEN: u32 = 0x21c;
/// `[gp+0x220]`: movie/picture table, 8 bytes per context +0x3A8 entry: +0
/// stream file, +2 music flag, +4 sound file.
const MOVIE_TABLE: u32 = 0x220;
/// `gp+0x224`: rectangle of the VRAM area the movie saves and restores.
const MOVIE_SAVED_RECT: u32 = GP + 0x224;
/// `gp+0x4C4`: rectangle cleared by ClearImage.
const CLEAR_RECT: u32 = GP + 0x4c4;
/// `gp+0x30C`: a stream sector deferred while the MDEC was busy.
const DEFERRED_SECTOR: u32 = GP + 0x30c;
/// 80057CC4: completion routine of the movie's sound-data read.
const MOVIE_MUSIC_LOADED: u32 = 0x8005_7cc4;
/// 80024398: "first sync timeout" format string.
const FIRST_SYNC_TIMEOUT: u32 = 0x8002_4398;
/// `[gp+0x4DC]`: the 0x64-byte picture block (`states.rs`).
const PICTURE: u32 = 0x4dc;
/// `[gp+0x22C]`: picture music block (+4 music on).
const PICTURE_MUSIC: u32 = 0x22c;
/// `gp+0x238`: the VRAM area the picture state saves.
const PICTURE_SAVED_RECT: u32 = GP + 0x238;

const RESET_CD_QUEUE: u32 = 0x8002_c6a8;
const FILE_SIZE: u32 = 0x8002_c5e0;
const QUEUE_READ: u32 = 0x8002_c7ac;
const MUSIC_SELECT: u32 = 0x8002_cc44;
const DRAW_CURSORS: u32 = 0x8002_d3f4;

/// Context +0x3A8 (JP; PAL +0x3AC): the movie/picture index.
const MOVIE_INDEX: u32 = 0x3a8;

/// The movie/picture table entry of the context's index.
fn table_entry(m: &mut Machine) -> u32 {
    let context = m.gw(CONTEXT);
    let entry = u32::from(m.lbu(context + MOVIE_INDEX));
    let table = m.gw(MOVIE_TABLE);
    (entry << 3).wrapping_add(table)
}

fn dec_byte(m: &mut Machine, field: u32) -> u8 {
    let d = m.gw(DECODER);
    m.lbu(d.wrapping_add(field))
}

fn set_dec_byte(m: &mut Machine, field: u32, value: u8) {
    let d = m.gw(DECODER);
    m.sb(d.wrapping_add(field), value);
}

/// 80057CD0 (JP; PAL 800563C8, near, `del:8 imm:9 ins:5 op:1`; state 5,
/// substate 0). Resets the CD queue and the clear colour (display bit 8).
/// Attract mode (context +0x3A8 = 0; PAL +0x3AC) counts its visits
/// (+0x3A4): unless the count is 1 modulo 4, it starts a demo flight
/// (+0x3A5 = 2 for a joystick pad, else 1; SET_STATE 2; PAL also counts
/// the demos at +0x3AA, JP does not). Otherwise the music flag (+0x39C &
/// +0x3A7) goes to music block +4 and, with music and a sound file, that
/// file is read into a new arena allocation (completion 80057CC4). Returns
/// NEXT_SUBSTATE.
pub fn movie_prepare(m: &mut Machine, c: &mut Invoker) -> u32 {
    m.push_frame(0x18);
    m.set_gw(MOVIE_DELAY, 0);
    c(m, RESET_CD_QUEUE, [0; 4]);
    for offset in [0x10, 0x11, 0x12] {
        let display = m.gw(DISPLAY);
        m.sb(display + offset, 0);
    }
    let display = m.gw(DISPLAY);
    let flags = m.lw(display);
    let context = m.gw(CONTEXT);
    m.sw(display, flags | 8);
    if m.lbu(context + MOVIE_INDEX) == 0 {
        let visits = m.lbu(context + 0x3a4);
        m.sb(context + 0x3a4, visits.wrapping_add(1));
        let context = m.gw(CONTEXT);
        if m.lbu(context + 0x3a4) & 3 != 1 {
            let kind = if m.lw(context + 0x128) & 0x300 != 0x300 {
                1
            } else {
                2
            };
            m.sb(context + 0x3a5, kind);
            m.pop_frame(0x18);
            return RESET_ARENA | SET_STATE | 2;
        }
    }
    let context = m.gw(CONTEXT);
    let enabled = u32::from(m.lbu(context + 0x39c) & m.lbu(context + 0x3a7));
    let music = m.gw(MOVIE_MUSIC);
    m.sw(music + 4, enabled);
    let entry = table_entry(m);
    let file = m.lh(entry + 4) as i32 as u32;
    if enabled != 0 && file != 0 {
        let size = c(m, FILE_SIZE, [file, 0, 0, 0]);
        let data = arena::alloc(m, size);
        let music = m.gw(MOVIE_MUSIC);
        m.sw(music, data);
        m.sw(music + 8, 1);
        c(m, QUEUE_READ, [file, data, MOVIE_MUSIC_LOADED, 0]);
    } else {
        let music = m.gw(MOVIE_MUSIC);
        m.sw(music, 0);
        m.sw(music + 4, 0);
    }
    m.pop_frame(0x18);
    NEXT_SUBSTATE
}

/// 80057EBC (JP; PAL 800565C0, near, `imm:9 ins:19 reg:2`; state 5,
/// substate 2). Movies 7, 0x10 and 0xC (context +0x3A8) go to state 6, 0x1D
/// to state 8 and 0x1E to state 7 (PAL: only the ending, 2, to state 7).
/// Otherwise on the first frame: display phase 0, clear colour 0, frame
/// interval 0, requests 1|8, context bits 0x10000004. From the third frame
/// (on buffer 1) the movie block is built (two 0x400-byte packet areas,
/// rectangles (0,0) and (0,256) of 320x240 (PAL 320x256), offset (0,32),
/// 24-bit mode, stream file from the table, timeout 120), the area at
/// `gp+0x224` is saved (StoreImage into 0x26C00 bytes; PAL 0x28000), the
/// two 480x240 halves of VRAM at y 0 and 256 are cleared to 1/1/1 and
/// music 9 starts when on. Returns NEXT_SUBSTATE then, else 0.
pub fn movie_setup(m: &mut Machine, c: &mut Invoker) -> u32 {
    let frame = m.push_frame(0x28);
    let result = movie_setup_body(m, c, frame);
    m.pop_frame(0x28);
    result
}

fn movie_setup_body(m: &mut Machine, c: &mut Invoker, frame: u32) -> u32 {
    let context = m.gw(CONTEXT);
    match m.lbu(context + MOVIE_INDEX) {
        7 | 0x10 | 0xc => return RESET_ARENA | SET_STATE | 6,
        0x1d => return RESET_ARENA | SET_STATE | 8,
        0x1e => return RESET_ARENA | SET_STATE | 7,
        _ => {}
    }
    let delay = m.gw(MOVIE_DELAY);
    if delay == 0 {
        let display = m.gw(DISPLAY);
        m.sb(display + 0x10, 0);
        let display2 = m.gw(DISPLAY);
        m.sw(display + 4, 0);
        m.sw(display + 0x14, 0);
        m.sb(display2 + 0x11, 0);
        let display = m.gw(DISPLAY);
        m.sb(display + 0x12, 0);
        let display = m.gw(DISPLAY);
        let flags = m.lw(display);
        let context = m.gw(CONTEXT);
        m.sw(display, flags | 9);
        let word = m.lw(context);
        m.sw(context, word | 0x1000_0004);
    }
    let delay = m.gw(MOVIE_DELAY).wrapping_add(1);
    m.set_gw(MOVIE_DELAY, delay);
    if (delay as i32) < 3 {
        return 0;
    }
    let context = m.gw(CONTEXT);
    if m.lw(context + 4) == 0 {
        return 0;
    }
    let block = arena::alloc(m, 0x44);
    m.set_gw(MOVIE, block);
    let first = arena::alloc(m, 0x400);
    let block = m.gw(MOVIE);
    m.sw(block + 4, first);
    let second = arena::alloc(m, 0x400);
    let block = m.gw(MOVIE);
    let context = m.gw(CONTEXT);
    m.sw(block + 8, second);
    m.sh(block + 0x2c, 0x140);
    m.sw(block, 0);
    m.sh(block + 0x24, 0);
    m.sh(block + 0x26, 0);
    m.sh(block + 0x28, 0);
    m.sh(block + 0x2a, 0x100);
    m.sh(block + 0x2e, 0xf0);
    m.sh(block + 0x30, 0);
    m.sh(block + 0x32, 0x20);
    let entry = u32::from(m.lbu(context + MOVIE_INDEX));
    let table = m.gw(MOVIE_TABLE);
    m.sw(DEFERRED_SECTOR, 0);
    let file = m.lh((entry << 3).wrapping_add(table)) as i32 as u32;
    m.sw(block + 0x38, 1);
    m.sw(block + 0x40, 0x78);
    m.sw(block + 0x34, file);
    let rect = frame + 0x10;
    for i in 0..2 {
        let word = m.lw(MOVIE_SAVED_RECT + 4 * i);
        m.sw(rect + 4 * i, word);
    }
    let pixels = arena::alloc(m, 0x2_6c00);
    let block = m.gw(MOVIE);
    m.sw(block + 0x3c, pixels);
    sdk::store_image(m, rect, pixels);
    sdk::draw_sync(m, 0);
    m.sh(CLEAR_RECT, 0);
    m.sh(CLEAR_RECT + 2, 0);
    m.sh(CLEAR_RECT + 4, 0x1e0);
    m.sh(CLEAR_RECT + 6, 0xf0);
    c(m, CLEAR_IMAGE, [CLEAR_RECT, 1, 1, 1]);
    m.sh(CLEAR_RECT, 0);
    m.sh(CLEAR_RECT + 2, 0x100);
    c(m, CLEAR_IMAGE, [CLEAR_RECT, 1, 1, 1]);
    sdk::draw_sync(m, 0);
    let music = m.gw(MOVIE_MUSIC);
    if m.lw(music + 4) != 0 {
        c(m, MUSIC_SELECT, [9, 0, 0, 0]);
    }
    NEXT_SUBSTATE
}

/// 800581C4 (JP; PAL 8005687C, changed, `imm:2 ins:24`; state 5, substate
/// 3): one movie frame. Selects the buffer's ordering table
/// (`ClearOTagR(table, 2)`) and packet area, runs the player 80057720; when
/// it finishes, [`movie_finish`], then (new in JP) movies 9, 0x15, 0x17,
/// 0x19 and 0x1C go to state 7 unless context +0x96C has bit 0x10 (the
/// switch table 800243D0); otherwise SET_STATE to context +0x3A0 (PAL
/// +0x3A4), both with an arena reset. While playing: the cursor sprites
/// (8002D3F4) with music on, then `DrawOTag(table + 4)`; returns 0.
pub fn movie_frame(m: &mut Machine, c: &mut Invoker) -> u32 {
    m.push_frame(0x18);
    let context = m.gw(CONTEXT);
    let buffer = m.lw(context + 4);
    let block = m.gw(MOVIE);
    let ot = block.wrapping_add((buffer << 3).wrapping_add(0xc));
    m.sw(block + 0x20, ot);
    let packets = m.lw(block.wrapping_add(buffer << 2) + 4);
    m.sw(block + 0x1c, packets);
    sdk::clear_otag_r(m, ot, 2);
    let result = if movie_player(m, c) != 0 {
        movie_finish(m, c);
        let context = m.gw(CONTEXT);
        let credits = !m.lw(context + 0x96c) & 0x10 != 0
            && matches!(m.lbu(context + MOVIE_INDEX), 9 | 0x15 | 0x17 | 0x19 | 0x1c);
        if credits {
            RESET_ARENA | SET_STATE | 7
        } else {
            let context = m.gw(CONTEXT);
            m.lw(context + 0x3a0) | RESET_ARENA | SET_STATE
        }
    } else {
        let music = m.gw(MOVIE_MUSIC);
        if m.lw(music + 4) != 0 {
            let block = m.gw(MOVIE);
            let ot = m.lw(block + 0x20);
            let packets = m.lw(block + 0x1c);
            let next = c(m, DRAW_CURSORS, [ot, packets, 0, 0]);
            let block = m.gw(MOVIE);
            m.sw(block + 0x1c, next);
        }
        let block = m.gw(MOVIE);
        let ot = m.lw(block + 0x20);
        sdk::draw_otag(m, ot.wrapping_add(4));
        0
    };
    m.pop_frame(0x18);
    result
}

/// 80057720 (JP; PAL 80055DCC, near, `del:19 imm:5`): one frame of the
/// movie player (movie block phase +0): 0 lay out the decoder in the free
/// arena (ring 0x12300 bytes, VLC buffers 0x1B804 (PAL 0x23004), image
/// buffers 0x2100/0x1600 (PAL 0x2A00/0x1C00) for 24/16-bit; PAL's attract
/// movie also moved the picture down, JP does not) and start the stream;
/// 1 on buffer 0, wait for the first frame (then display it, phase 2) or
/// count down the 120-frame timeout to the end (phase 3); 2 decode a frame
/// (START skips) and display it; at the end clear the displayed rectangle,
/// stop the music and the decoder (phase 3); 3 phase 4; 4 returns 1
/// (finished). Returns 0 otherwise.
pub fn movie_player(m: &mut Machine, c: &mut Invoker) -> u32 {
    let frame = m.push_frame(0x30);
    let block = m.gw(MOVIE);
    let result = match m.lw(block) {
        0 => {
            movie_layout(m, c, frame);
            0
        }
        1 => movie_first_frame(m, c, frame),
        2 => {
            movie_next(m, c, frame);
            0
        }
        3 => {
            let block = m.gw(MOVIE);
            m.sw(block, 4);
            0
        }
        4 => 1,
        _ => 0,
    };
    m.pop_frame(0x30);
    result
}

fn movie_layout(m: &mut Machine, c: &mut Invoker, frame: u32) {
    let buffer = arena::peek(m, 0);
    let block = m.gw(MOVIE);
    m.sw(frame + 0x10, 0x1_b804);
    let image = if m.lw(block + 0x38) != 0 {
        0x2100
    } else {
        0x1600
    };
    m.sw(frame + 0x14, image);
    let block2 = m.gw(MOVIE);
    let sum = |m: &mut Machine, a: u32, b: u32| {
        (i32::from(m.lh(block2 + a)).wrapping_add(i32::from(m.lh(block2 + b)))) as u32
    };
    let x0 = sum(m, 0x24, 0x30);
    m.sw(frame + 0x18, x0);
    let y0 = sum(m, 0x26, 0x32);
    m.sw(frame + 0x1c, y0);
    let x1 = sum(m, 0x28, 0x30);
    m.sw(frame + 0x20, x1);
    let y1 = sum(m, 0x2a, 0x32);
    m.sw(frame + 0x24, y1);
    let file = m.lw(block + 0x34);
    let mode = m.lw(block + 0x38);
    movie_decoder_init(
        m,
        c,
        file,
        mode,
        buffer,
        0x1_2300,
        [0x1_b804, image, x0, y0, x1, y1],
    );
    movie_stream_start(m, c);
    let block = m.gw(MOVIE);
    m.sw(block, 1);
}

fn movie_first_frame(m: &mut Machine, c: &mut Invoker, frame: u32) -> u32 {
    let context = m.gw(CONTEXT);
    if m.lw(context + 4) != 0 {
        return 0;
    }
    if movie_wait_frame(m, c, 1) != 0 {
        let block = m.gw(MOVIE);
        m.sw(block, 2);
        let screen = movie_rect_zero(m);
        movie_present(m, frame, screen);
        return 0;
    }
    let block = m.gw(MOVIE);
    let count = m.lw(block + 0x40).wrapping_sub(1);
    m.sw(block + 0x40, count);
    if (count as i32) <= 0 {
        sdk::printf(m, [FIRST_SYNC_TIMEOUT, 0, 0, 0]);
        set_dec_byte(m, dec::END, 1);
    }
    if dec_byte(m, dec::END) == 0 {
        return 0;
    }
    let music = m.gw(MOVIE_MUSIC);
    if m.lw(music + 4) != 0 {
        c(m, MUSIC_SELECT, [0xff, 0, 0, 0]);
    }
    movie_stop(m, c);
    let block = m.gw(MOVIE);
    m.sw(block, 3);
    0
}

fn movie_next(m: &mut Machine, c: &mut Invoker, frame: u32) {
    let context = m.gw(CONTEXT);
    if m.lw(context + 0x12c) & 0x40 != 0 {
        set_dec_byte(m, dec::END, 1);
    }
    if movie_decode(m, c, 0) == 0 {
        let screen = movie_rect_zero(m);
        movie_present(m, frame, screen);
        return;
    }
    movie_stop(m, c);
    let screen = movie_rect_zero(m);
    let block = m.gw(MOVIE);
    let rect = block.wrapping_add(screen << 2);
    let x = m.lhu(rect + 0x24);
    m.sh(CLEAR_RECT, x);
    let y = m.lhu(rect + 0x26);
    m.sh(CLEAR_RECT + 2, y);
    let h = m.lhu(block + 0x2e);
    m.sh(CLEAR_RECT + 6, h);
    let depth = m.lw(block + 0x38);
    m.set_gw(MOVIE_SCREEN, screen);
    if depth != 0 {
        let w = i32::from(m.lh(block + 0x2c));
        m.sh(
            CLEAR_RECT + 4,
            (w.wrapping_shl(1).wrapping_add(w) >> 1) as u16,
        );
    } else {
        let w = m.lhu(block + 0x2c);
        m.sh(CLEAR_RECT + 4, w);
    }
    c(m, CLEAR_IMAGE, [CLEAR_RECT, 1, 1, 1]);
    let music = m.gw(MOVIE_MUSIC);
    if m.lw(music + 4) != 0 {
        c(m, MUSIC_SELECT, [0xff, 0, 0, 0]);
    }
    let block = m.gw(MOVIE);
    m.sw(block, 3);
}

/// The frame presentation shared by phase 1 and phase 2 of 80057720 (the
/// same instructions in JP and PAL; a copy of the private `states.rs`
/// helper).
fn movie_present(m: &mut Machine, frame: u32, screen: u32) {
    let display = m.gw(DISPLAY);
    let flags = m.lw(display);
    m.sw(display, flags | 0x30);
    let block = m.gw(MOVIE);
    let rect = block.wrapping_add(screen << 2);
    let x = m.lh(rect + 0x24) as i32 as u32;
    let y = m.lh(rect + 0x26) as i32 as u32;
    let w = m.lh(block + 0x2c) as i32 as u32;
    let h = m.lh(block + 0x2e) as i32 as u32;
    m.set_gw(MOVIE_SCREEN, screen);
    m.sw(frame + 0x10, h);
    sdk::set_def_disp_env(m, display + 0x18, x, y, w, h);
    let display = m.gw(DISPLAY);
    m.sb(display + 0x28, 0);
    let block = m.gw(MOVIE);
    let display = m.gw(DISPLAY);
    let depth = m.lbu(block + 0x38);
    m.sb(display + 0x29, depth);
    let display = m.gw(DISPLAY);
    let block = m.gw(MOVIE);
    let sx = m.lhu(display + 8);
    let sy = m.lhu(display + 0xc);
    m.sh(display + 0x20, sx);
    m.sh(display + 0x22, sy);
    let other = screen ^ 1;
    let rect = block.wrapping_add(other << 2);
    let x = m.lh(rect + 0x24) as i32 as u32;
    let y = m.lh(rect + 0x26) as i32 as u32;
    let width = i32::from(m.lh(block + 0x2c));
    let w = if m.lw(block + 0x38) != 0 {
        (width.wrapping_shl(1).wrapping_add(width) >> 1) as u32
    } else {
        width as u32
    };
    let h = m.lh(block + 0x2e) as i32 as u32;
    m.sw(frame + 0x10, h);
    sdk::set_def_draw_env(m, display + 0x2c, x, y, w, h);
    for (offset, value) in [(0x44, 1), (0x45, 0), (0x46, 0), (0x47, 0)] {
        let display = m.gw(DISPLAY);
        m.sb(display + offset, value);
    }
}

/// 8005B308 (JP; PAL 80059900, near, `imm:4`; state 6, substate 0): on
/// buffer 1, context bit 0x10000000, display phase 1 with requests 1|8 and
/// colour 0, frame interval 0; the picture block (two 0x2000-byte packet
/// areas, 320x480, file from the movie/picture table at context +0x3A8;
/// PAL +0x3AC), the area at `gp+0x238` saved (StoreImage into 0x2800
/// bytes), the screen cleared, music 10 when on (context +0x39C & +0x3A7
/// and the table's music flag 1). Returns NEXT_SUBSTATE (0 on buffer 0).
pub fn picture_state_setup(m: &mut Machine, c: &mut Invoker) -> u32 {
    let frame = m.push_frame(0x30);
    let context = m.gw(CONTEXT);
    let result = if m.lw(context + 4) == 0 {
        0
    } else {
        let word = m.lw(context);
        let display = m.gw(DISPLAY);
        m.sw(context, word | 0x1000_0000);
        m.sb(display + 0x10, 0);
        let display2 = m.gw(DISPLAY);
        m.sw(display + 0x14, 0);
        m.sw(display + 4, 1);
        m.sb(display2 + 0x11, 0);
        let display = m.gw(DISPLAY);
        m.sb(display + 0x12, 0);
        let display = m.gw(DISPLAY);
        let flags = m.lw(display);
        m.sw(display, flags | 9);
        let block = arena::alloc(m, 0x64);
        m.set_gw(PICTURE, block);
        let first = arena::alloc(m, 0x2000);
        let block = m.gw(PICTURE);
        m.sw(block + 4, first);
        let second = arena::alloc(m, 0x2000);
        let block = m.gw(PICTURE);
        m.sw(block + 8, second);
        let context = m.gw(CONTEXT);
        m.sw(block, 0);
        m.sh(block + 0x4c, 0x140);
        m.sh(block + 0x4e, 0x1e0);
        let entry = u32::from(m.lbu(context + MOVIE_INDEX));
        let table = m.gw(MOVIE_TABLE);
        let file = m.lh((entry << 3).wrapping_add(table)) as i32 as u32;
        m.sw(block + 0x54, 0);
        m.sw(block + 0x50, file);
        let rect = frame + 0x10;
        for i in 0..2 {
            let word = m.lw(PICTURE_SAVED_RECT + 4 * i);
            m.sw(rect + 4 * i, word);
        }
        let pixels = arena::alloc(m, 0x2800);
        let block = m.gw(PICTURE);
        m.sw(block + 0x58, pixels);
        sdk::store_image(m, rect, pixels);
        let clear = frame + 0x18;
        m.sh(clear, 0);
        m.sh(clear + 2, 0);
        m.sh(clear + 4, 0x140);
        m.sh(clear + 6, 0x1e0);
        c(m, CLEAR_IMAGE, [clear, 0, 0, 0]);
        sdk::draw_sync(m, 0);
        let context = m.gw(CONTEXT);
        let enabled = m.lbu(context + 0x39c) & m.lbu(context + 0x3a7);
        let music = if enabled != 0 {
            let entry = u32::from(m.lbu(context + MOVIE_INDEX));
            let table = m.gw(MOVIE_TABLE);
            let flag = m.lh((entry << 3).wrapping_add(table) + 2);
            u32::from(flag == 1)
        } else {
            0
        };
        let block = m.gw(PICTURE_MUSIC);
        m.sw(block + 4, music);
        let block = m.gw(PICTURE_MUSIC);
        if m.lw(block + 4) != 0 {
            c(m, MUSIC_SELECT, [0xa, 0, 0, 0]);
        }
        NEXT_SUBSTATE
    };
    m.pop_frame(0x30);
    result
}

/// 8005B4E4 (JP; PAL 80059ADC, near, `imm:1`; state 6, substate 1): one
/// picture frame: the buffer's 5-entry ordering table (`ClearOTagR`) and
/// packet area, the player 800582F8; when it finishes,
/// [`picture_state_finish`] and SET_STATE to context +0x3A0 (PAL +0x3A4).
/// Otherwise cursor sprites with music on and `DrawOTag(table + 0x10)`;
/// returns 0.
pub fn picture_state_frame(m: &mut Machine, c: &mut Invoker) -> u32 {
    m.push_frame(0x18);
    let context = m.gw(CONTEXT);
    let buffer = m.lw(context + 4);
    let block = m.gw(PICTURE);
    let ot = block.wrapping_add(buffer.wrapping_mul(0x14).wrapping_add(0xc));
    m.sw(block + 0x38, ot);
    let packets = m.lw(block.wrapping_add(buffer << 2) + 4);
    m.sw(block + 0x34, packets);
    sdk::clear_otag_r(m, ot, 5);
    let result = if picture_player(m, c) != 0 {
        picture_state_finish(m, c);
        let context = m.gw(CONTEXT);
        m.lw(context + 0x3a0) | RESET_ARENA | SET_STATE
    } else {
        let music = m.gw(PICTURE_MUSIC);
        if m.lw(music + 4) != 0 {
            let block = m.gw(PICTURE);
            let ot = m.lw(block + 0x38);
            let packets = m.lw(block + 0x34);
            let next = c(m, DRAW_CURSORS, [ot, packets, 0, 0]);
            let block = m.gw(PICTURE);
            m.sw(block + 0x34, next);
        }
        let block = m.gw(PICTURE);
        let ot = m.lw(block + 0x38);
        sdk::draw_otag(m, ot.wrapping_add(0x10));
        0
    };
    m.pop_frame(0x18);
    result
}
