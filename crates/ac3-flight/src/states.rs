//! Resident states 0/1, 5 and 6 and the overlay module loader on the
//! address-preserving machine (see `docs/FLIGHT_MACHINE.md`).
//!
//! | State | Callbacks | Contents |
//! | ---: | --- | --- |
//! | 0, 1 | 80025BA4 (`mainloop.rs`) | boot: select state 4 (module 1, menus) |
//! | 5 | 800563C8, 80056538, 800565C0, 8005687C | full-motion video (STR movie through the MDEC); with context +0x3AC = 0 the title attract mode alternating the opening movie and demo flights |
//! | 6 | 80059900, 80059ADC | streamed picture sequence (the in-mission "picture" event as a state) |
//! | any (module load) | 80099BC8, 80099CD8 | loading callbacks of table 800B24F4 while context +0x34 (requested module) differs from +0x38 (loaded) |
//!
//! The movie player is the game's own "GMV" layer (80055224..80055DCC) over
//! libpress: VLC decoding on the CPU (DecDCTvlc2), MDEC decoding of 16- or
//! 24-pixel slices (DecDCTin/DecDCTout) whose DMA-1 completion callback
//! 80055878 uploads each slice with LoadImage, and the CD data stream ring
//! (`replay.rs` 8002B5C4/8002B7B8, `cdstream.rs`). The picture player is the
//! "picture" layer 80056950.. over the same stream ring.
//!
//! Calls into other modules go through an [`Invoker`] (production:
//! [`dispatch::call`]), so snapshot tests can substitute the original's
//! recorded effect of a routine translated elsewhere that is not integrated
//! yet. PsyQ routines `sdk.rs` does not provide yet are in [`pending_sdk`]
//! with the original ABI.
use crate::{
    GP, arena,
    common::CONTEXT,
    dispatch,
    machine::{Fault, Machine, SmallData},
    mainloop::transition::{LOAD_COMPLETE, NEXT_SUBSTATE, RESET_ARENA, SET_STATE},
    sdk,
};

/// Calls into other modules: original address and `a0..a3`, returns `v0`.
pub type Invoker<'a> = crate::mainloop::CallbackInvoker<'a>;

/// `[gp+0x2C4]`: display request block (+0 request bits, +4 phase,
/// +8/+0xC screen x/y, +0x10..+0x12 clear RGB, +0x14 frame interval,
/// +0x18 DISPENV, +0x2C DRAWENV).
const DISPLAY: u32 = 0x2c4;

// ---- module loader (table 800B24F4) ----------------------------------------------------

/// `gp+0x2A8`: loading-screen mode of the module load (0 none, 1 a 320x256
/// fade, 2 a 320x512 fade).
const LOAD_MODE: u32 = 0x2a8;
/// `gp+0x2AC`: module being loaded (the CD file index).
const LOAD_MODULE: u32 = 0x2ac;
/// `gp+0x55C`: nonzero while the module read is pending.
const LOAD_PENDING: u32 = 0x55c;
/// 80099BBC: completion routine of the module read.
pub const MODULE_READ_COMPLETE: u32 = 0x8009_9bbc;
/// 8002C384 (`loading.rs`): queue a CD file read `(file, destination,
/// completion)`.
const QUEUE_READ: u32 = 0x8002_c384;
/// 8002C6A8 (`shared.rs`): file of the active CD request.
const ACTIVE_FILE: u32 = 0x8002_c6a8;
/// 8002C604 (`loading.rs`): read progress of the active request.
const READ_PROGRESS: u32 = 0x8002_c604;
/// 80092350: full-screen fade rectangle `(ot, packets, rect, level)`.
const FADE_RECT: u32 = 0x8009_2350;

/// 80099BBC: completion routine of the module read (CD request callback):
/// clears the pending flag `gp+0x55C`.
pub fn module_read_complete(m: &mut Machine) {
    m.set_gw(LOAD_PENDING, 0);
}

/// 8003606C: a VRAM-to-VRAM copy packet at `packet` (tag length 5, a 01
/// cache flush, then the 80 copy command with source `sx, sy`, destination
/// `dx, dy` and size `w, h`) linked first into ordering-table entry `ot`.
/// Returns `packet + 0x18`. (`dx..h` are the caller's stack words
/// +0x10..+0x1C; the halfword stores keep their low 16 bits.)
#[allow(clippy::too_many_arguments)]
pub fn move_image_packet(
    m: &mut Machine,
    ot: u32,
    packet: u32,
    sx: u32,
    sy: u32,
    dx: u32,
    dy: u32,
    w: u32,
    h: u32,
) -> u32 {
    m.sw(packet, 0x0500_0000);
    m.sw(packet + 4, 0x0100_0000);
    m.sw(packet + 0xc, (sx & 0xffff) | (sy << 16));
    m.sw(packet + 8, 0x8000_0000);
    m.sw(packet + 0x10, (dx & 0xffff) | (dy << 16));
    m.sw(packet + 0x14, (w & 0xffff) | (h << 16));
    let tag = m.lw(packet);
    let link = m.lw(ot);
    m.sw(packet, (tag & 0xff00_0000) | (link & 0x00ff_ffff));
    let link = m.lw(ot);
    m.sw(ot, (link & 0xff00_0000) | (packet & 0x00ff_ffff));
    packet.wrapping_add(0x18)
}

/// 8002C490: queue the read of CD file `file` to `destination` with the
/// completion routine `completion` (8002C384); returns its result (the end
/// of the destination).
pub fn queue_file_read(
    m: &mut Machine,
    c: &mut Invoker,
    file: u32,
    destination: u32,
    completion: u32,
) -> u32 {
    m.push_frame(0x18);
    let end = c(m, QUEUE_READ, [file, destination, completion, 0]);
    m.pop_frame(0x18);
    end
}

/// 80099BC8 (loading substate 0): begin loading overlay `module` to
/// `address`. Display requests 1|4 (drawing environments, no clearing) and
/// frame interval 0. In display phase 0 the current frame is copied into
/// the other buffer (8003606C on the context's table +0x10) and the
/// loading screen fades over 320x256 (mode 1); phase 1 uses mode 2
/// (320x512), others none. Then the read of file `module` is queued with
/// completion 80099BBC. Returns NEXT_SUBSTATE.
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
        m.sw(frame + 0x1c, 0x100);
        let ot = m.lw(context + 0x10);
        let packets = m.lw(context + 0x14);
        let next = move_image_packet(m, ot, packets, 0, source_y, 0, destination_y, 0x140, 0x100);
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

/// 80099CD8 (loading substate 1): while the module's read is the active CD
/// request, draw the loading fade (80092350 on the context's table +0xC,
/// rectangle at scratchpad 0, level `-progress`) in modes 1/2. When the
/// read completed (`gp+0x55C` clear): clear colour 0, request clearing
/// (display bit 8) and return LOAD_COMPLETE; otherwise 0.
pub fn module_load_poll(m: &mut Machine, c: &mut Invoker) -> u32 {
    m.push_frame(0x18);
    let active = c(m, ACTIVE_FILE, [0; 4]);
    if active == m.gw(LOAD_MODULE) {
        let progress = c(m, READ_PROGRESS, [0; 4]);
        let height = match m.gw(LOAD_MODE) {
            1 => Some(0x100),
            2 => Some(0x200),
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

// ---- state 5: movie -----------------------------------------------------------------------

/// `gp+0x4D4`: frames spent in state-5 substate 2 before the player starts.
const MOVIE_DELAY: u32 = 0x4d4;
/// `[gp+0x4D8]`: the 0x44-byte movie block: +0 phase (80055DCC), +4/+8 two
/// 0x400-byte packet areas, +0xC/+0x14 two 2-entry ordering tables, +0x1C
/// next packet, +0x20 current table, +0x24/+0x26 and +0x28/+0x2A the two
/// screen rectangles' x/y, +0x2C/+0x2E width/height, +0x30/+0x32 picture
/// offset, +0x34 stream file, +0x38 24-bit mode, +0x3C saved VRAM, +0x40
/// first-frame timeout.
const MOVIE: u32 = 0x4d8;
/// `[gp+0x4DC]`: the GMV decoder (see [`decoder`]).
const DECODER: u32 = 0x4dc;
/// `[gp+0x204]`: movie music block (+0 sound data, +4 music on, +8 read
/// pending).
const MOVIE_MUSIC: u32 = 0x204;
/// `gp+0x208`: index of the displayed movie rectangle.
const MOVIE_SCREEN: u32 = 0x208;
/// `[gp+0x20C]`: movie/picture table, 8 bytes per context +0x3AC entry: +0
/// stream file, +2 music flag, +4 sound file.
const MOVIE_TABLE: u32 = 0x20c;
/// `gp+0x210`: rectangle of the VRAM area the movie saves and restores.
const MOVIE_SAVED_RECT: u32 = GP + 0x210;
/// `gp+0x4CC`: rectangle cleared by ClearImage.
const CLEAR_RECT: u32 = GP + 0x4cc;
/// `gp+0x2FC`: a stream sector deferred while the MDEC was busy
/// (`cdstream.rs`); the slice callback services it (800297B8).
const DEFERRED_SECTOR: u32 = GP + 0x2fc;
/// 800563BC: completion routine of the movie's sound-data read.
pub const MOVIE_MUSIC_LOADED: u32 = 0x8005_63bc;
/// 80055878: DMA-1 (MDEC out) completion callback of the movie decoder.
pub const SLICE_DONE: u32 = 0x8005_5878;
/// 800553A4: completion routine of the movie's stream request.
pub const STREAM_DONE: u32 = 0x8005_53a4;

const RESET_CD_QUEUE: u32 = 0x8002_c280;
const FILE_SIZE: u32 = 0x8002_c1b8;
const RELOCATE_DIRECTORY: u32 = 0x8002_6e98;
const UPLOAD_TIMS: u32 = 0x8002_6d8c;
const MUSIC_SELECT: u32 = 0x8002_c81c;
const QUEUE_STREAM: u32 = 0x8002_c500;
const NEXT_CHUNK: u32 = 0x8002_b5c4;
const RELEASE_CHUNK: u32 = 0x8002_b7b8;
const PROCESS_CD_QUEUE: u32 = 0x8002_c0b4;
const STREAM_SECTOR: u32 = 0x8002_97b8;
const DRAW_CURSORS: u32 = 0x8002_cfcc;
/// 800A67E8 ClearImage(rect, r, g, b) (libgpu; not in `sdk.rs` yet).
pub const CLEAR_IMAGE: u32 = 0x800a_67e8;

fn table_entry(m: &mut Machine, table_pointer: u32) -> u32 {
    let context = m.gw(CONTEXT);
    let entry = u32::from(m.lbu(context + 0x3ac));
    let table = m.gw(table_pointer);
    (entry << 3).wrapping_add(table)
}

/// 800563BC: completion of the movie's sound-data read: music block +8 = 0.
pub fn movie_music_loaded(m: &mut Machine) {
    let music = m.gw(MOVIE_MUSIC);
    m.sw(music + 8, 0);
}

/// 800563C8 (state 5, substate 0). Resets the CD queue and the clear
/// colour (display bit 8). Attract mode (context +0x3AC = 0) counts its
/// visits (+0x3A8): unless the count is 1 modulo 4, it starts a demo flight
/// (+0x3A9 = 2 for a joystick pad, else 1; +0x3AA counted; SET_STATE 2).
/// Otherwise the music flag (+0x3A0 & +0x3AB) goes to music block +4 and,
/// with music and a sound file, that file is read into a new arena
/// allocation (completion 800563BC). Returns NEXT_SUBSTATE.
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
    if m.lbu(context + 0x3ac) == 0 {
        let visits = m.lbu(context + 0x3a8);
        m.sb(context + 0x3a8, visits.wrapping_add(1));
        let context = m.gw(CONTEXT);
        if m.lbu(context + 0x3a8) & 3 != 1 {
            let kind = if m.lw(context + 0x128) & 0x300 != 0x300 {
                1
            } else {
                2
            };
            m.sb(context + 0x3a9, kind);
            let context = m.gw(CONTEXT);
            let demos = m.lbu(context + 0x3aa);
            m.sb(context + 0x3aa, demos.wrapping_add(1));
            m.pop_frame(0x18);
            return RESET_ARENA | SET_STATE | 2;
        }
    }
    let context = m.gw(CONTEXT);
    let enabled = u32::from(m.lbu(context + 0x3a0) & m.lbu(context + 0x3ab));
    let music = m.gw(MOVIE_MUSIC);
    m.sw(music + 4, enabled);
    let entry = table_entry(m, MOVIE_TABLE);
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

/// 80056538 (state 5, substate 1): with music on and its sound data read,
/// install the data (80026E98 twice, 80026D8C) and `DrawSync(0)`. Returns
/// NEXT_SUBSTATE, or 0 while the read is pending.
pub fn movie_load_music(m: &mut Machine, c: &mut Invoker) -> u32 {
    m.push_frame(0x18);
    let music = m.gw(MOVIE_MUSIC);
    let result = if m.lw(music + 4) == 0 || m.lw(music) == 0 {
        NEXT_SUBSTATE
    } else if m.lw(music + 8) != 0 {
        0
    } else {
        let data = m.lw(music);
        let directory = c(m, RELOCATE_DIRECTORY, [data, 0, 0, 0]);
        let first = m.lw(directory);
        let bank = c(m, RELOCATE_DIRECTORY, [first, 0, 0, 0]);
        let first = m.lw(directory);
        let count = m.lw(first);
        c(m, UPLOAD_TIMS, [bank, count, 0, 0]);
        sdk::draw_sync(m, 0);
        NEXT_SUBSTATE
    };
    m.pop_frame(0x18);
    result
}

/// 800565C0 (state 5, substate 2). Context +0x3AC = 2 (the ending) goes to
/// state 7 (credits). Otherwise on the first frame: display phase 0, clear
/// colour 0, frame interval 0, requests 1|8, context bits 0x10000004 (no
/// pause, no DrawOTag). From the third frame (on buffer 1) the movie block
/// is built (two 0x400-byte packet areas, rectangles (0,0) and (0,256) of
/// 320x256, offset (0,32), 24-bit mode, stream file from the table,
/// timeout 120), the area at `gp+0x210` is saved (StoreImage into 0x28000
/// bytes), both halves of VRAM are cleared to 1/1/1 and music 9 starts
/// when on. Returns NEXT_SUBSTATE then, else 0.
pub fn movie_setup(m: &mut Machine, c: &mut Invoker) -> u32 {
    let frame = m.push_frame(0x28);
    let result = movie_setup_body(m, c, frame);
    m.pop_frame(0x28);
    result
}

fn movie_setup_body(m: &mut Machine, c: &mut Invoker, frame: u32) -> u32 {
    let context = m.gw(CONTEXT);
    if m.lbu(context + 0x3ac) == 2 {
        return RESET_ARENA | SET_STATE | 7;
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
    m.sh(block + 0x2e, 0x100);
    m.sh(block + 0x30, 0);
    m.sh(block + 0x32, 0x20);
    let entry = u32::from(m.lbu(context + 0x3ac));
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
    let pixels = arena::alloc(m, 0x2_8000);
    let block = m.gw(MOVIE);
    m.sw(block + 0x3c, pixels);
    sdk::store_image(m, rect, pixels);
    sdk::draw_sync(m, 0);
    m.sh(CLEAR_RECT, 0);
    m.sh(CLEAR_RECT + 2, 0);
    m.sh(CLEAR_RECT + 4, 0x1e0);
    m.sh(CLEAR_RECT + 6, 0x100);
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

/// 800567EC: movie end: reset the CD queue, remove the slice callback,
/// `VSync(0)`, restore the saved VRAM area, `DrawSync(0)`, clear context
/// bit 4 (DrawOTag again). Returns 1.
pub fn movie_finish(m: &mut Machine, c: &mut Invoker) -> u32 {
    let frame = m.push_frame(0x20);
    c(m, RESET_CD_QUEUE, [0; 4]);
    sdk::dec_dct_out_callback(m, 0);
    let rect = frame + 0x10;
    for i in 0..2 {
        let word = m.lw(MOVIE_SAVED_RECT + 4 * i);
        m.sw(rect + 4 * i, word);
    }
    sdk::vsync(m, 0);
    let block = m.gw(MOVIE);
    let pixels = m.lw(block + 0x3c);
    sdk::load_image(m, rect, pixels);
    sdk::draw_sync(m, 0);
    let context = m.gw(CONTEXT);
    let word = m.lw(context);
    m.sw(context, word & !4);
    m.pop_frame(0x20);
    1
}

/// 8005687C (state 5, substate 3): one movie frame. Selects the buffer's
/// ordering table (`ClearOTagR(table, 2)`) and packet area, runs the player
/// 80055DCC; when it finishes, [`movie_finish`] and SET_STATE to context
/// +0x3A4 with an arena reset. Otherwise the cursor sprites (8002CFCC) with
/// music on, then `DrawOTag(table + 4)`; returns 0.
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
        m.lw(context + 0x3a4) | RESET_ARENA | SET_STATE
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

// ---- 80055DCC: the GMV player ---------------------------------------------------------------

/// GMV decoder layout (`[gp+0x4DC]`, 0x11044 bytes, then the stream ring
/// and the VLC/image buffers).
pub mod decoder {
    /// Byte: VLC buffer being filled (0/1).
    pub const VLC_ID: u32 = 0;
    /// Byte: image (slice) buffer the MDEC writes next (0/1).
    pub const IMAGE_ID: u32 = 1;
    /// Byte: frame rectangle being drawn (0/1).
    pub const RECT_ID: u32 = 2;
    /// Byte: the frame's last slice is uploaded.
    pub const DONE: u32 = 3;
    /// Byte: end of the movie (stream end, timeout or skip).
    pub const END: u32 = 4;
    /// Byte: 24-bit mode.
    pub const DEPTH24: u32 = 5;
    /// Words: VLC buffers 0/1.
    pub const VLC_BUFFERS: u32 = 8;
    /// Words: slice buffers 0/1.
    pub const IMAGE_BUFFERS: u32 = 0x10;
    /// Two RECTs: the frame rectangles.
    pub const RECTS: u32 = 0x18;
    /// RECT: the slice being uploaded (+0x2C slice width in VRAM pixels).
    pub const SLICE: u32 = 0x28;
    /// DecDCTvlcBuild table.
    pub const VLC_TABLE: u32 = 0x30;
    /// Halfwords: frame width, height, number, slice size in words.
    pub const WIDTH: u32 = 0x1_1030;
    pub const HEIGHT: u32 = 0x1_1032;
    pub const FRAME: u32 = 0x1_1034;
    pub const SLICE_WORDS: u32 = 0x1_1036;
    /// Words: stream file, ring size, ring buffer.
    pub const FILE: u32 = 0x1_1038;
    pub const RING_SIZE: u32 = 0x1_103c;
    pub const RING: u32 = 0x1_1040;
    /// Size of the decoder block itself.
    pub const SIZE: u32 = 0x1_1044;
}
use decoder as dec;

fn dec_byte(m: &mut Machine, field: u32) -> u8 {
    let d = m.gw(DECODER);
    m.lbu(d.wrapping_add(field))
}
fn set_dec_byte(m: &mut Machine, field: u32, value: u8) {
    let d = m.gw(DECODER);
    m.sb(d.wrapping_add(field), value);
}

/// 80055224: 1 while frame rectangle 0 is being drawn (rectangle 0 is
/// then the one to display... the original's `rect id == 0`).
pub fn movie_rect_zero(m: &mut Machine) -> u32 {
    u32::from(dec_byte(m, dec::RECT_ID) == 0)
}

/// 80055238: decoder start: `DecDCTReset(0)`, `DecDCTvlcSize2(0)`, slice
/// callback 80055878, `DecDCTvlcBuild(decoder + 0x30)`, END 0, DONE 1.
/// Returns 1 (the original `v0`).
pub fn movie_decoder_reset(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    pending_sdk::dec_dct_reset(m, 0);
    pending_sdk::dec_dct_vlc_size2(m, 0);
    sdk::dec_dct_out_callback(m, SLICE_DONE);
    let d = m.gw(DECODER);
    pending_sdk::dec_dct_vlc_build(m, d.wrapping_add(dec::VLC_TABLE));
    set_dec_byte(m, dec::END, 0);
    set_dec_byte(m, dec::DONE, 1);
    m.pop_frame(0x18);
    1
}

/// 80055290: decoder fields from the parameter block `params` (+0 mode, +4
/// file, +8 ring size, +0xC ring, +0x10/+0x14 VLC buffers, +0x18/+0x1C
/// image buffers, +0x20..+0x26 the two rectangles' x/y). Slice width 24
/// (24-bit) or 16; everything else 0.
pub fn movie_decoder_setup(m: &mut Machine, params: u32) {
    let d = m.gw(DECODER);
    let high = d.wrapping_add(0x1_0000);
    let v = m.lw(params + 4);
    m.sw(high + 0x1038, v);
    let v = m.lw(params + 8);
    m.sw(high + 0x103c, v);
    let v = m.lw(params + 0xc);
    m.sw(high + 0x1040, v);
    for (from, to) in [(0x10, 8), (0x14, 0xc), (0x18, 0x10), (0x1c, 0x14)] {
        let v = m.lw(params + from);
        m.sw(d + to, v);
    }
    let mode = m.lbu(params);
    m.sb(d + dec::DEPTH24, mode);
    let d = m.gw(DECODER);
    let v = m.lhu(params + 0x20);
    m.sh(d + 0x18, v);
    let v = m.lhu(params + 0x22);
    m.sh(d + 0x1a, v);
    let v = m.lhu(params + 0x24);
    m.sh(d + 0x20, v);
    let v = m.lhu(params + 0x26);
    m.sh(d + 0x1c, 0);
    m.sh(d + 0x1e, 0);
    m.sh(d + 0x24, 0);
    m.sh(d + 0x26, 0);
    m.sh(d + 0x22, v);
    let v = m.lhu(params + 0x20);
    m.sh(d + 0x28, v);
    let v = m.lhu(params + 0x22);
    m.sh(d + 0x2a, v);
    let width = if m.lw(params) != 0 { 0x18 } else { 0x10 };
    m.sh(d + 0x2c, width);
    let d0 = m.gw(DECODER);
    m.sb(d0 + dec::IMAGE_ID, 0);
    m.sb(d0 + dec::VLC_ID, 0);
    let d = m.gw(DECODER);
    m.sh(d0 + 0x2e, 0);
    let high = d.wrapping_add(0x1_0000);
    m.sh(high + 0x1032, 0);
    m.sh(high + 0x1030, 0);
    m.sh(high + 0x1036, 0);
    m.sb(d + dec::RECT_ID, 0);
    let d = m.gw(DECODER);
    m.sb(d + dec::DONE, 0);
}

/// 800553A4: completion of the movie stream request: END = 1.
pub fn movie_stream_done(m: &mut Machine) {
    set_dec_byte(m, dec::END, 1);
}

/// 800553B4(file, mode, buffer, ring, ...): lay out the decoder at
/// `buffer` (`gp+0x4DC`): the decoder
/// block, a stream ring of `ring` bytes (rounded to words), two VLC
/// buffers of `vlc` bytes and two image buffers of `image` bytes; frame
/// rectangles (`x0`, `y0`) and (`x1`, `y1`). Then [`movie_decoder_setup`],
/// [`movie_decoder_reset`] and a CD queue reset. Returns the end of the
/// layout. (`vlc..y1` are the caller's stack words +0x10..+0x24.)
#[allow(clippy::too_many_arguments)]
pub fn movie_decoder_init(
    m: &mut Machine,
    c: &mut Invoker,
    file: u32,
    mode: u32,
    buffer: u32,
    ring: u32,
    stack: [u32; 6],
) -> u32 {
    let [vlc, image, x0, y0, x1, y1] = stack;
    let frame = m.push_frame(0x40);
    m.set_gw(DECODER, buffer);
    let ring_start = buffer.wrapping_add(dec::SIZE);
    let ring = ring.wrapping_add(3) >> 2 << 2;
    m.sw(frame + 0x1c, ring_start);
    let mut cursor = ring_start.wrapping_add(ring);
    m.sw(frame + 0x14, file);
    m.sw(frame + 0x18, ring);
    m.sw(frame + 0x20, cursor);
    m.sw(frame + 0x10, mode);
    let vlc = (vlc.wrapping_add(3) >> 2) << 2;
    let image = (image.wrapping_add(3) >> 2) << 2;
    cursor = cursor.wrapping_add(vlc);
    m.sw(frame + 0x24, cursor);
    cursor = cursor.wrapping_add(vlc);
    m.sw(frame + 0x28, cursor);
    cursor = cursor.wrapping_add(image);
    m.sw(frame + 0x2c, cursor);
    cursor = cursor.wrapping_add(image);
    m.sh(frame + 0x30, x0 as u16);
    m.sh(frame + 0x32, y0 as u16);
    m.sh(frame + 0x34, x1 as u16);
    m.sh(frame + 0x36, y1 as u16);
    movie_decoder_setup(m, frame + 0x10);
    movie_decoder_reset(m);
    c(m, RESET_CD_QUEUE, [0; 4]);
    m.pop_frame(0x40);
    cursor
}

/// 80055484: start streaming the movie: 8002C500(file, ring, ring size,
/// flags, completion 800553A4) with flags 0x4000 when music is on and
/// 0x800 in 24-bit mode.
pub fn movie_stream_start(m: &mut Machine, c: &mut Invoker) {
    let frame = m.push_frame(0x20);
    let music = m.gw(MOVIE_MUSIC);
    let d = m.gw(DECODER);
    let on = u32::from(m.lw(music + 4) != 0);
    let mut flags = on << 14;
    if m.lbu(d + dec::DEPTH24) != 0 {
        flags |= 0x800;
    }
    m.sw(frame + 0x10, STREAM_DONE);
    let high = d.wrapping_add(0x1_0000);
    let file = m.lw(high + 0x1038);
    let ring = m.lw(high + 0x1040);
    let size = m.lw(high + 0x103c);
    c(m, QUEUE_STREAM, [file, ring, size, flags]);
    m.pop_frame(0x20);
}

/// 800559D0: take the next stream frame (8002B5C4) and VLC-decode it into
/// the other VLC buffer. A frame number 0 marks the end; a new height or
/// width updates the slice size, the rectangles and the slice height
/// (24-bit widths count 1.5 VRAM pixels). Returns 1 when a frame was
/// decoded, 0 when none is ready.
pub fn movie_next_frame(m: &mut Machine, c: &mut Invoker, d: u32) -> u32 {
    let frame = m.push_frame(0x20);
    let result = movie_next_frame_body(m, c, d, frame);
    m.pop_frame(0x20);
    result
}

fn movie_next_frame_body(m: &mut Machine, c: &mut Invoker, d: u32, frame: u32) -> u32 {
    if c(m, NEXT_CHUNK, [frame + 0x10, frame + 0x14, 0, 0]) != 0 {
        return 0;
    }
    let header = m.lw(frame + 0x14);
    if m.lhu(header + 6) == 0 {
        m.sb(d + dec::END, 1);
    }
    let high = d.wrapping_add(0x1_0000);
    let header = m.lw(frame + 0x14);
    let height = m.lh(high + 0x1032);
    let number = m.lhu(header + 6);
    m.sh(high + 0x1034, number);
    let new_height = m.lh(header + 0xa);
    if height != new_height {
        m.sh(high + 0x1032, new_height as u16);
        let width = m.lh(d + 0x2c);
        let product = i32::from(new_height).wrapping_mul(i32::from(width));
        m.sh(high + 0x1036, (product >> 1) as u16);
        m.sh(d + 0x2e, new_height as u16);
        m.sh(d + 0x26, new_height as u16);
        m.sh(d + 0x1e, new_height as u16);
    }
    let header = m.lw(frame + 0x14);
    let width = m.lh(high + 0x1030);
    let new_width = m.lh(header + 8);
    if width != new_width {
        m.sh(high + 0x1030, new_width as u16);
        let pixels = if m.lbu(d + dec::DEPTH24) != 0 {
            let w = i32::from(new_width);
            (w.wrapping_shl(1).wrapping_add(w)) >> 1
        } else {
            i32::from(new_width)
        };
        m.sh(d + 0x24, pixels as u16);
        m.sh(d + 0x1c, pixels as u16);
    }
    let id = m.lbu(d + dec::VLC_ID) ^ 1;
    m.sb(d + dec::VLC_ID, id);
    let source = m.lw(frame + 0x10);
    let buffer = m.lw(d + dec::VLC_BUFFERS + (u32::from(id) << 2));
    pending_sdk::dec_dct_vlc2(m, source, buffer, d + dec::VLC_TABLE);
    let header = m.lw(frame + 0x14);
    c(m, RELEASE_CHUNK, [header, 0, 0, 0]);
    1
}

/// 800554E4: [`movie_next_frame`]; with `wait` 0, retry for up to 121
/// frames, each running the CD queue (8002C0B4) and `VSync(0)`, then print
/// the VLC timeout and set END. Returns 1 once a frame was decoded or the
/// wait gave up, else 0.
pub fn movie_wait_frame(m: &mut Machine, c: &mut Invoker, poll: u32) -> u32 {
    let frame = m.push_frame(0x28);
    let result = if poll != 0 {
        let d = m.gw(DECODER);
        u32::from(movie_next_frame(m, c, d) != 0)
    } else {
        m.sw(frame + 0x10, 0x78);
        loop {
            let d = m.gw(DECODER);
            if movie_next_frame(m, c, d) != 0 {
                break 1;
            }
            c(m, PROCESS_CD_QUEUE, [0; 4]);
            vsync_wait(m);
            let count = m.lw(frame + 0x10).wrapping_sub(1);
            m.sw(frame + 0x10, count);
            if (m.lw(frame + 0x10) as i32) < 0 {
                sdk::printf(m, [VLC_TIMEOUT, 0, 0, 0]);
                set_dec_byte(m, dec::END, 1);
                break 1;
            }
        }
    };
    m.pop_frame(0x28);
    result
}

/// 80023DF0: "GMV : timeout VLC\n".
const VLC_TIMEOUT: u32 = 0x8002_3df0;
/// 80023E04: "GMV : timeout DCT\n".
const DCT_TIMEOUT: u32 = 0x8002_3e04;
/// 80023E18: "GMV : FirstSync timeout\n".
const FIRST_SYNC_TIMEOUT: u32 = 0x8002_3e18;

/// 80055594: stop: reset the CD queue and remove the slice callback.
/// Returns 1.
pub fn movie_stop(m: &mut Machine, c: &mut Invoker) -> u32 {
    m.push_frame(0x18);
    c(m, RESET_CD_QUEUE, [0; 4]);
    sdk::dec_dct_out_callback(m, 0);
    m.pop_frame(0x18);
    1
}

/// 800555C0: decode the current VLC buffer: unless END, DONE = 0,
/// `DecDCTin(vlc[VLC_ID], depth | 2)` and `DecDCTout(image[IMAGE_ID], slice
/// words)`; then poll [`movie_next_frame`] up to 0x40000 times (END stops
/// early; running out prints the VLC timeout and sets END) and wait for the
/// slices with [`movie_sync`] (`mode`). Returns 1 at END, else 0.
pub fn movie_decode(m: &mut Machine, c: &mut Invoker, mode: u32) -> u32 {
    let frame = m.push_frame(0x28);
    let result = movie_decode_body(m, c, mode, frame);
    m.pop_frame(0x28);
    result
}

fn movie_decode_body(m: &mut Machine, c: &mut Invoker, mode: u32, frame: u32) -> u32 {
    if dec_byte(m, dec::END) != 0 {
        return 1;
    }
    set_dec_byte(m, dec::DONE, 0);
    let d = m.gw(DECODER);
    let id = u32::from(m.lbu(d + dec::VLC_ID));
    let depth = u32::from(m.lbu(d + dec::DEPTH24));
    let vlc = m.lw(d + dec::VLC_BUFFERS + (id << 2));
    pending_sdk::dec_dct_in(m, vlc, depth | 2);
    let d = m.gw(DECODER);
    let id = u32::from(m.lbu(d + dec::IMAGE_ID));
    let words = m.lh(d.wrapping_add(dec::SLICE_WORDS)) as i32 as u32;
    let image = m.lw(d + dec::IMAGE_BUFFERS + (id << 2));
    pending_sdk::dec_dct_out(m, image, words);
    m.sw(frame + 0x10, 0x4_0000);
    loop {
        let d = m.gw(DECODER);
        if movie_next_frame(m, c, d) != 0 {
            break;
        }
        if dec_byte(m, dec::END) == 1 {
            if (m.lw(frame + 0x10) as i32) <= 0 {
                sdk::printf(m, [VLC_TIMEOUT, 0, 0, 0]);
                set_dec_byte(m, dec::END, 1);
            }
            break;
        }
        let count = m.lw(frame + 0x10).wrapping_sub(1);
        m.sw(frame + 0x10, count);
        if (m.lw(frame + 0x10) as i32) <= 0 {
            sdk::printf(m, [VLC_TIMEOUT, 0, 0, 0]);
            set_dec_byte(m, dec::END, 1);
            break;
        }
        busy_wait(m, busy::POLL_FRAME, busy::POLL_FRAME_CYCLES);
    }
    let d = m.gw(DECODER);
    movie_sync(m, d, mode);
    0
}

/// Busy waits of the movie decoder (host contract).
///
/// 800555C0 polls for the next stream frame (up to 0x40000 times) and
/// 8005578C for the frame's last slice: the original spins while the CD,
/// MDEC and VBlank interrupts arrive. Natively every iteration of these
/// loops calls [`busy_wait`]: a host call [`BUSY_WAIT`](busy::BUSY_WAIT)
/// `[site, cycles]` (the loop's original address and the estimated CPU
/// cycles of one iteration) in which the host lets that much guest time
/// pass, delivering each VBlank that falls due exactly as in a waiting
/// `VSync` (libetc's counter, libpad's buffers, one SPU field), and
/// reports MDEC output DMAs it has completed since DecDCTout (result bit
/// [`MDEC_OUT_COMPLETE`](busy::MDEC_OUT_COMPLETE): the DMA-1 callback then
/// runs, standing in for its interrupt); then libcd's interrupt handler runs
/// (`cdstream::cd_service`), so sectors the drive paced by the VBlank count
/// arrive as during the original's spin. A host that completes MDEC
/// transfers inside DecDCTout (result 1, the default) never sees the slice
/// wait spin. Without a host the call does nothing and the loops run to
/// their original timeouts. The `VSync(0)` retry loops (800554E4,
/// 80056EF4, overlay 3's 800D6F48) run the CD interrupt after each wait
/// ([`vsync_wait`]).
pub mod busy {
    /// Host-call id (the frame poll's loop address).
    pub const BUSY_WAIT: u32 = 0x8005_5648;
    /// 800555C0's poll of 800559D0 (loop at 80055648).
    pub const POLL_FRAME: u32 = 0x8005_5648;
    /// 8005578C's wait for DONE (loop at 800557B4).
    pub const POLL_SLICES: u32 = 0x8005_57b4;
    /// Estimated CPU cycles per iteration: the frame poll (800559D0 and
    /// the stream ring check 8002B5C4 included) and the slice wait.
    pub const POLL_FRAME_CYCLES: u32 = 100;
    pub const POLL_SLICES_CYCLES: u32 = 30;
    /// [`BUSY_WAIT`] result bit: an MDEC output transfer completed.
    pub const MDEC_OUT_COMPLETE: u32 = 1;
}

/// One iteration of a busy-wait loop (see [`busy`]).
pub fn busy_wait(m: &mut Machine, site: u32, cycles: u32) {
    let completed = m.host_call_result(busy::BUSY_WAIT, [site, cycles, 0, 0], 0);
    if completed & busy::MDEC_OUT_COMPLETE != 0 {
        let callback = m.lw(sdk::DMA_CALLBACKS + 4);
        if callback != 0 {
            crate::dispatch::call(m, callback, [0; 4]);
        }
    }
    crate::cdstream::cd_service(m);
}

/// `VSync(0)` inside a retry loop, then the CD interrupt the original
/// receives while it waits (`cdstream`'s host contract).
pub fn vsync_wait(m: &mut Machine) {
    sdk::vsync(m, 0);
    crate::cdstream::cd_service(m);
}

/// 8005578C: with `mode` nonzero, return DONE. Otherwise wait for DONE,
/// polling up to 0x40000 times while END is clear; running out prints the
/// DCT timeout and forces the frame complete (next rectangle, DONE = 1,
/// slice reset). Returns 1.
pub fn movie_sync(m: &mut Machine, d: u32, mode: u32) -> u32 {
    let frame = m.push_frame(0x20);
    m.sw(frame + 0x10, 0x4_0000);
    let result = if mode != 0 {
        u32::from(m.lbu(d + dec::DONE))
    } else {
        while m.lbu(d + dec::DONE) == 0 {
            let end = u32::from(dec_byte(m, dec::END));
            m.sw(frame + 0x14, end);
            let count = m.lw(frame + 0x10).wrapping_sub(1);
            m.sw(frame + 0x10, count);
            if (m.lw(frame + 0x10) as i32) > 0 && m.lw(frame + 0x14) == 0 {
                busy_wait(m, busy::POLL_SLICES, busy::POLL_SLICES_CYCLES);
                continue;
            }
            if (m.lw(frame + 0x10) as i32) <= 0 {
                sdk::printf(m, [DCT_TIMEOUT, 0, 0, 0]);
            }
            let rect = u8::from(m.lbu(d + dec::RECT_ID) == 0);
            m.sb(d + dec::RECT_ID, rect);
            let rect = u32::from(rect) << 3;
            m.sb(d + dec::DONE, 1);
            let id = u32::from(m.lbu(d + dec::RECT_ID)) << 3;
            let x = m.lhu(d + rect + 0x18);
            m.sh(d + 0x28, x);
            let y = m.lhu(d + id + 0x1a);
            m.sh(d + 0x2a, y);
        }
        1
    };
    m.pop_frame(0x20);
    result
}

/// 80055878: DMA-1 (MDEC out) completion: in 24-bit mode service a sector
/// deferred while the MDEC was busy (800297B8, `gp+0x2FC` cleared); upload
/// the finished slice (LoadImage of the slice rectangle from image buffer
/// IMAGE_ID), switch image buffers and advance the slice. Within the frame
/// rectangle the next slice is started (`DecDCTout`); after the last one
/// DONE = 1, the other rectangle becomes current and the slice restarts at
/// its origin.
pub fn movie_slice_done(m: &mut Machine, c: &mut Invoker) {
    m.push_frame(0x18);
    let d = m.gw(DECODER);
    if m.lbu(d + dec::DEPTH24) != 0 && (m.lw(DEFERRED_SECTOR) as i32) > 0 {
        c(m, STREAM_SECTOR, [0; 4]);
        m.sw(DEFERRED_SECTOR, 0);
    }
    let d = m.gw(DECODER);
    let id = u32::from(m.lbu(d + dec::IMAGE_ID));
    let image = m.lw(d + dec::IMAGE_BUFFERS + (id << 2));
    sdk::load_image(m, d + dec::SLICE, image);
    let d = m.gw(DECODER);
    let id = m.lbu(d + dec::IMAGE_ID);
    m.sb(d + dec::IMAGE_ID, id ^ 1);
    let d = m.gw(DECODER);
    let x = m.lhu(d + 0x28).wrapping_add(m.lhu(d + 0x2c));
    let rect = u32::from(m.lbu(d + dec::RECT_ID)) << 3;
    m.sh(d + 0x28, x);
    let right = i32::from(m.lh(d + rect + 0x18)).wrapping_add(i32::from(m.lh(d + rect + 0x1c)));
    if i32::from(x as i16) < right {
        let id = u32::from(m.lbu(d + dec::IMAGE_ID));
        let words = m.lh(d.wrapping_add(dec::SLICE_WORDS)) as i32 as u32;
        let image = m.lw(d + dec::IMAGE_BUFFERS + (id << 2));
        pending_sdk::dec_dct_out(m, image, words);
    } else {
        m.sb(d + dec::DONE, 1);
        let d = m.gw(DECODER);
        let rect = m.lbu(d + dec::RECT_ID);
        m.sb(d + dec::RECT_ID, rect ^ 1);
        let d = m.gw(DECODER);
        let rect = u32::from(m.lbu(d + dec::RECT_ID)) << 3;
        let x = m.lhu(d + rect + 0x18);
        let rect2 = u32::from(m.lbu(d + dec::RECT_ID)) << 3;
        m.sh(d + 0x28, x);
        let y = m.lhu(d + rect2 + 0x1a);
        m.sh(d + 0x2a, y);
    }
    m.pop_frame(0x18);
}

/// The frame presentation shared by phase 1 and phase 2 of 80055DCC:
/// display `screen` (the rectangle 80055224 reports) with a custom
/// DISPENV/DRAWENV (requests 0x30): SetDefDispEnv on that rectangle,
/// 24-bit display flag, screen offset, SetDefDrawEnv on the other
/// rectangle (1.5 times wider in 24-bit mode) and drawing flags 1/0/0/0.
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

/// 80055DCC: one frame of the movie player (movie block phase +0):
/// 0 lay out the decoder in the free arena (ring 0x12300 bytes, VLC
/// buffers 0x23004, image buffers 0x2A00/0x1C00 for 24/16-bit; the attract
/// movie moves the picture down 12 or 8 lines) and start the stream;
/// 1 on buffer 0, wait for the first frame (then display it, phase 2) or
/// count down the 120-frame timeout to the end (phase 3);
/// 2 decode a frame (START skips) and display it; at the end clear the
/// displayed rectangle, stop the music and the decoder (phase 3);
/// 3 phase 4; 4 returns 1 (finished). Returns 0 otherwise.
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
    let context = m.gw(CONTEXT);
    if m.lbu(context + 0x3ac) == 0 {
        let block = m.gw(MOVIE);
        let step = if m.lw(block + 0x38) != 0 { 0xc } else { 8 };
        let y = m.lhu(block + 0x30).wrapping_add(step);
        m.sh(block + 0x30, y);
    }
    let buffer = arena::peek(m, 0);
    let block = m.gw(MOVIE);
    m.sw(frame + 0x10, 0x2_3004);
    let image = if m.lw(block + 0x38) != 0 {
        0x2a00
    } else {
        0x1c00
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
        [0x2_3004, image, x0, y0, x1, y1],
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

/// 800556E0 (no caller in the resident or overlay code): when a new frame
/// is ready ([`movie_next_frame`]) and not at END, start decoding it like
/// [`movie_decode`] and wait with [`movie_sync`] (`mode`). Returns 1 at
/// END, else 0.
pub fn movie_decode_ready(m: &mut Machine, c: &mut Invoker, mode: u32) -> u32 {
    m.push_frame(0x18);
    let d = m.gw(DECODER);
    let result = if m.lbu(d + dec::END) != 0 {
        1
    } else if movie_next_frame(m, c, d) == 0 {
        0
    } else {
        set_dec_byte(m, dec::DONE, 0);
        let d = m.gw(DECODER);
        let id = u32::from(m.lbu(d + dec::VLC_ID));
        let depth = u32::from(m.lbu(d + dec::DEPTH24));
        let vlc = m.lw(d + dec::VLC_BUFFERS + (id << 2));
        pending_sdk::dec_dct_in(m, vlc, depth | 2);
        let d = m.gw(DECODER);
        let id = u32::from(m.lbu(d + dec::IMAGE_ID));
        let words = m.lh(d.wrapping_add(dec::SLICE_WORDS)) as i32 as u32;
        let image = m.lw(d + dec::IMAGE_BUFFERS + (id << 2));
        pending_sdk::dec_dct_out(m, image, words);
        let d = m.gw(DECODER);
        movie_sync(m, d, mode);
        0
    };
    m.pop_frame(0x18);
    result
}

/// 80099DD4 (completion routine overlay 1/2 pass with their CD reads):
/// context +0x44 = 0.
pub fn context_read_complete(m: &mut Machine) {
    let context = m.gw(CONTEXT);
    m.sw(context + 0x44, 0);
}

/// 8002B174 (unlisted, called by overlay 5): `[[gp+0x38]+0x40]`, the CD
/// stream driver's elapsed time while paused.
pub fn stream_pause_time(m: &mut Machine) -> u32 {
    let driver = m.gw(0x38);
    m.lw(driver + 0x40)
}

// ---- state 6 and the picture player (80056950) ------------------------------------------

/// `[gp+0x4E4]`: the 0x64-byte picture block: +0 phase (80056950), +4/+8
/// two 0x2000-byte packet areas, +0xC/+0x20 two 5-entry ordering tables,
/// +0x34 next packet, +0x38 current table, +0x3C..+0x4A eight layout
/// halfwords, +0x4C/+0x4E width/height (320x480), +0x50 stream file, +0x54
/// mode, +0x58 saved VRAM, +0x60 first-frame timeout.
const PICTURE: u32 = 0x4e4;
/// `[gp+0x4E8]`: the picture decoder (see [`picture`]).
const PICTURE_DECODER: u32 = 0x4e8;
/// `[gp+0x218]`: picture music block (+4 music on).
const PICTURE_MUSIC: u32 = 0x218;
/// `gp+0x224` / `gp+0x23C`: the VRAM area the picture state saves (320x16
/// at 0,496) and restores.
const PICTURE_SAVED_RECT: u32 = GP + 0x224;
const PICTURE_RESTORE_RECT: u32 = GP + 0x23c;
/// `gp+0x21C` (800BAC10): the noise rectangle of the picture's fades
/// (32,100 256x280).
const PICTURE_NOISE_RECT: u32 = GP + 0x21c;
/// 800C55C8 / 800C5668: the 16 texture-page rectangles and source pointers
/// of the picture's uploads.
const PICTURE_RECTS: u32 = 0x800c_55c8;
const PICTURE_SOURCES: u32 = 0x800c_5668;
/// 800C56B8 / 800C5758: the border jitter (four rows of 38 random bytes,
/// four random row selectors) of 8005891C.
const BORDER_JITTER: u32 = 0x800c_56b8;
const BORDER_ROWS: u32 = 0x800c_5758;
/// 80023E4C: 0x98 bytes, the four border wave rows of 8005891C.
const BORDER_WAVES: u32 = 0x8002_3e4c;
/// 80056EAC: completion routine of the picture stream request.
pub const PICTURE_STREAM_DONE: u32 = 0x8005_6eac;
const DRAW_NOISE_TILES: u32 = 0x8008_2cd0;

/// Picture decoder layout (`[gp+0x4E8]`, 0xD8 bytes, then the stream ring).
pub mod picture {
    /// Byte: frame step of the last chunk (1 next frame, 2 restart).
    pub const STEP: u32 = 0;
    /// Byte: 1 once the sequence restarted (layered drawing).
    pub const LAYERED: u32 = 1;
    /// Word: frames since the sequence started.
    pub const FRAME: u32 = 4;
    /// Halfword: frames shown.
    pub const SHOWN: u32 = 8;
    /// Byte: current effect (0..9).
    pub const EFFECT: u32 = 0xa;
    /// Word: frames into the current effect.
    pub const EFFECT_TIME: u32 = 0xc;
    /// Byte: current effect slot.
    pub const SLOT: u32 = 0x10;
    /// Effect slots: two banks of five 0xE-byte records {+0 effect, +2
    /// start frame, +4 end frame, +6..+0xD four parameters}.
    pub const SLOTS: u32 = 0x12;
    pub const SLOT_SIZE: u32 = 0xe;
    pub const BANK_SIZE: u32 = 0x46;
    /// Byte: bank being loaded.
    pub const BANK: u32 = 0x9e;
    /// Byte: bank being shown.
    pub const SHOWN_BANK: u32 = 0x9f;
    /// Byte: end of the sequence.
    pub const END: u32 = 0xa0;
    /// Byte: mode.
    pub const MODE: u32 = 0xa1;
    /// Two banks of 20 bytes: per tile, a second texture page.
    pub const TILE_PAGES: u32 = 0xa2;
    /// Words: stream file, ring size, ring.
    pub const FILE: u32 = 0xcc;
    pub const RING_SIZE: u32 = 0xd0;
    pub const RING: u32 = 0xd4;
    pub const SIZE: u32 = 0xd8;
}
use picture as pic;

fn pic_decoder(m: &mut Machine) -> u32 {
    m.gw(PICTURE_DECODER)
}

/// The effect slot record `slot` of `bank` in decoder `d`.
fn pic_slot(d: u32, bank: u32, slot: u32) -> u32 {
    d.wrapping_add(bank.wrapping_mul(pic::BANK_SIZE))
        .wrapping_add(slot.wrapping_mul(pic::SLOT_SIZE))
}

/// The current slot of `d` (its +0x10 slot in its shown bank; `base` is the
/// decoder whose fields select it).
fn pic_current_slot(m: &mut Machine, base: u32, d: u32) -> u32 {
    let slot = u32::from(m.lbu(base + pic::SLOT));
    let bank = u32::from(m.lbu(base + pic::SHOWN_BANK));
    pic_slot(d, bank, slot)
}

/// Signed `div` quotient at `site` (the `div` instruction) behind the
/// compiler's checks: `break 7` at `site + 0xC` for a zero divisor and
/// `break 6` at `site + 0x24` for `0x80000000 / -1` (every site here has
/// that layout; found by `jp-map` with `ov3.rs`'s twin). A reached trap
/// records `Fault::Trap` and yields the MIPS result.
fn quotient(m: &mut Machine, n: i32, d: i32, site: u32) -> i32 {
    if d == 0 {
        m.raise(Fault::Trap {
            address: site + 0xc,
            code: 7,
        });
        return if n < 0 { 1 } else { -1 };
    }
    if d == -1 && n == i32::MIN {
        m.raise(Fault::Trap {
            address: site + 0x24,
            code: 6,
        });
        return i32::MIN;
    }
    n / d
}

/// Signed `div` remainder (no checks emitted before `mfhi` uses here
/// except the same traps).
fn remainder(m: &mut Machine, n: i32, d: i32, site: u32) -> i32 {
    if d == 0 {
        m.raise(Fault::Trap {
            address: site + 0xc,
            code: 7,
        });
        return n;
    }
    if d == -1 && n == i32::MIN {
        m.raise(Fault::Trap {
            address: site + 0x24,
            code: 6,
        });
        return 0;
    }
    n % d
}

fn sext16(value: u32) -> i32 {
    i32::from(value as u16 as i16)
}

/// Link `packet` into ordering-table entry `ot` with tag length `words`
/// (the inline addPrim: `*packet = *ot | words << 24; *ot = packet`).
fn link_or(m: &mut Machine, ot: u32, packet: u32, words: u32) {
    let link = m.lw(ot);
    m.sw(packet, link | (words << 24));
    m.sw(ot, (packet << 8) >> 8);
}

/// Link `packet` keeping its length byte (`addPrim` with masks).
fn link_masked(m: &mut Machine, ot: u32, packet: u32) {
    let tag = m.lw(packet);
    let link = m.lw(ot);
    m.sw(packet, (tag & 0xff00_0000) | (link & 0x00ff_ffff));
    let link = m.lw(ot);
    m.sw(ot, (link & 0xff00_0000) | (packet & 0x00ff_ffff));
}

/// 80059900 (state 6, substate 0): on buffer 1, context bit 0x10000000,
/// display phase 1 with requests 1|8 and colour 0, frame interval 0; the
/// picture block (two 0x2000-byte packet areas, 320x480, file from the
/// movie/picture table), the area at `gp+0x224` saved (StoreImage into
/// 0x2800 bytes), the screen cleared, music 10 when on. Returns
/// NEXT_SUBSTATE (0 on buffer 0).
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
        let entry = u32::from(m.lbu(context + 0x3ac));
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
        let enabled = m.lbu(context + 0x3a0) & m.lbu(context + 0x3ab);
        let music = if enabled != 0 {
            let entry = u32::from(m.lbu(context + 0x3ac));
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

/// 80059BB4: picture end: reset the CD queue, clear the screen, restore
/// the saved area (`gp+0x23C`), stop the music. Returns 1.
pub fn picture_state_finish(m: &mut Machine, c: &mut Invoker) -> u32 {
    let frame = m.push_frame(0x20);
    c(m, RESET_CD_QUEUE, [0; 4]);
    let rect = frame + 0x10;
    m.sh(rect + 4, 0x140);
    m.sh(rect, 0);
    m.sh(rect + 2, 0);
    m.sh(rect + 6, 0x1e0);
    c(m, CLEAR_IMAGE, [rect, 0, 0, 0]);
    let block = m.gw(PICTURE);
    let pixels = m.lw(block + 0x58);
    sdk::load_image(m, PICTURE_RESTORE_RECT, pixels);
    let music = m.gw(PICTURE_MUSIC);
    if m.lw(music + 4) != 0 {
        c(m, MUSIC_SELECT, [0xff, 0, 0, 0]);
    }
    m.pop_frame(0x20);
    1
}

/// 80059ADC (state 6, substate 1): one picture frame: the buffer's
/// 5-entry ordering table (`ClearOTagR`) and packet area, the player
/// 80056950; when it finishes, [`picture_state_finish`] and SET_STATE to
/// context +0x3A4. Otherwise cursor sprites with music on and
/// `DrawOTag(table + 0x10)`; returns 0.
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
        m.lw(context + 0x3a4) | RESET_ARENA | SET_STATE
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

/// 80056950: one frame of the picture player (picture block phase +0):
/// 0 lay out the decoder in the free arena (ring 0x47C0 bytes) and start
/// the stream; 1 on buffer 0 wait for the first frame (timeout 120 frames
/// ends it); 2 show frames (START ends); 3/4 clear the screen (on buffer
/// 0); 5..8 restore the display (display phase 0, interval 2, requests
/// 1|4, context bit 0x10000000 cleared); 9 returns 1 (finished).
pub fn picture_player(m: &mut Machine, c: &mut Invoker) -> u32 {
    let frame = m.push_frame(0x48);
    let block = m.gw(PICTURE);
    let phase = m.lw(block);
    let set_phase = |m: &mut Machine, phase: u32| {
        let block = m.gw(PICTURE);
        m.sw(block, phase);
    };
    let result = match phase {
        0 => {
            let buffer = arena::peek(m, 0);
            let block = m.gw(PICTURE);
            let mut layout = [0u32; 8];
            for (i, slot) in layout.iter_mut().enumerate() {
                *slot = m.lh(block + 0x3c + 2 * i as u32) as i32 as u32;
                m.sw(frame + 0x10 + 4 * i as u32, *slot);
            }
            let file = m.lw(block + 0x50);
            let mode = m.lw(block + 0x54);
            picture_decoder_init(m, c, file, mode, buffer, 0x47c0, layout);
            picture_stream_start(m, c);
            set_phase(m, 1);
            0
        }
        1 => {
            let context = m.gw(CONTEXT);
            if m.lw(context + 4) == 0 {
                m.sw(context + 4, 0);
                if picture_wait_frame(m, c, 1) != 0 {
                    set_phase(m, 2);
                } else {
                    let block = m.gw(PICTURE);
                    let count = m.lw(block + 0x60).wrapping_sub(1);
                    m.sw(block + 0x60, count);
                    if (count as i32) <= 0 {
                        let d = pic_decoder(m);
                        m.sb(d + pic::END, 1);
                    }
                    let d = pic_decoder(m);
                    if m.lbu(d + pic::END) != 0 {
                        let music = m.gw(PICTURE_MUSIC);
                        if m.lw(music + 4) != 0 {
                            c(m, MUSIC_SELECT, [0xff, 0, 0, 0]);
                        }
                        set_phase(m, 3);
                    }
                }
            }
            let d = pic_decoder(m);
            let count = m.lw(d + pic::FRAME).wrapping_add(1);
            m.sw(d + pic::FRAME, count);
            0
        }
        2 => {
            let context = m.gw(CONTEXT);
            if m.lw(context + 0x12c) & 0x40 != 0 {
                let d = pic_decoder(m);
                m.sb(d + pic::END, 1);
            }
            if picture_frame(m, c) != 0 {
                let block = m.gw(PICTURE);
                let d = pic_decoder(m);
                m.sw(block, 3);
                m.sw(d + pic::FRAME, 0);
            }
            0
        }
        3 => {
            picture_stop(m, c);
            picture_bank_zero(m);
            let rect = frame + 0x30;
            m.sh(rect + 4, 0x140);
            m.sh(rect, 0);
            m.sh(rect + 2, 0);
            m.sh(rect + 6, 0x1e0);
            c(m, CLEAR_IMAGE, [rect, 0, 0, 0]);
            let context = m.gw(CONTEXT);
            if m.lw(context + 4) == 0 {
                set_phase(m, 4);
            }
            0
        }
        4 => {
            let rect = frame + 0x38;
            m.sh(rect + 4, 0x140);
            m.sh(rect, 0);
            m.sh(rect + 2, 0);
            m.sh(rect + 6, 0x1e0);
            c(m, CLEAR_IMAGE, [rect, 0, 0, 0]);
            set_phase(m, 5);
            0
        }
        5 => {
            set_phase(m, 6);
            0
        }
        6 => {
            let context = m.gw(CONTEXT);
            let word = m.lw(context);
            let display = m.gw(DISPLAY);
            m.sw(context, word & 0xefff_ffff);
            let flags = m.lw(display);
            m.sw(display + 0x14, 2);
            m.sw(display + 4, 0);
            m.sw(display, flags | 5);
            set_phase(m, 7);
            0
        }
        7 => {
            set_phase(m, 8);
            0
        }
        8 => {
            set_phase(m, 9);
            0
        }
        9 => 1,
        _ => 0,
    };
    m.pop_frame(0x48);
    result
}

/// 80056C64(file, mode, buffer, ring, layout[8]): lay out the picture
/// decoder at `buffer` (`gp+0x4E8`): the 0xD8-byte decoder, then a ring of
/// `ring` bytes (rounded to words); [`picture_decoder_setup`] and a CD
/// queue reset. Returns the end of the ring. (`layout` are the caller's
/// stack words +0x10..+0x2C.)
#[allow(clippy::too_many_arguments)]
pub fn picture_decoder_init(
    m: &mut Machine,
    c: &mut Invoker,
    file: u32,
    mode: u32,
    buffer: u32,
    ring: u32,
    layout: [u32; 8],
) -> u32 {
    let frame = m.push_frame(0x38);
    m.set_gw(PICTURE_DECODER, buffer);
    let ring_start = buffer.wrapping_add(pic::SIZE);
    let ring = (ring.wrapping_add(3) >> 2) << 2;
    m.sw(frame + 0x1c, ring_start);
    let end = ring_start.wrapping_add(ring);
    m.sw(frame + 0x14, file);
    m.sw(frame + 0x18, ring);
    m.sw(frame + 0x10, mode);
    for (i, value) in layout.into_iter().enumerate() {
        m.sh(frame + 0x20 + 2 * i as u32, value as u16);
    }
    picture_decoder_setup(m, frame + 0x10);
    c(m, RESET_CD_QUEUE, [0; 4]);
    m.pop_frame(0x38);
    end
}

/// 80056D04: picture decoder fields from the parameter block `params` (+0
/// mode, +4 file, +8 ring size, +0xC ring); everything else cleared, the
/// slots' frames and parameters -1.
pub fn picture_decoder_setup(m: &mut Machine, params: u32) {
    let d = pic_decoder(m);
    let v = m.lw(params + 4);
    m.sw(d + pic::FILE, v);
    let v = m.lw(params + 8);
    m.sw(d + pic::RING_SIZE, v);
    let v = m.lw(params + 0xc);
    m.sw(d + pic::RING, v);
    let v = m.lbu(params);
    m.sb(d + pic::MODE, v);
    for field in [
        pic::BANK,
        pic::SHOWN_BANK,
        pic::END,
        pic::EFFECT,
        pic::LAYERED,
    ] {
        let d = pic_decoder(m);
        m.sb(d + field, 0);
    }
    let d0 = pic_decoder(m);
    let d = pic_decoder(m);
    m.sw(d0 + pic::EFFECT_TIME, 0);
    m.sw(d0 + pic::FRAME, 0);
    m.sb(d + pic::STEP, 0);
    let d = pic_decoder(m);
    m.sb(d + pic::SLOT, 0);
    let d = pic_decoder(m);
    m.sh(d + pic::SHOWN, 0);
    for bank in 0..2 {
        for slot in 0..5 {
            let d = pic_decoder(m);
            let record = pic_slot(d, bank, slot);
            m.sb(record + pic::SLOTS, 0);
            let d = pic_decoder(m);
            let record = pic_slot(d, bank, slot);
            m.sh(record + 0x14, 0);
            m.sh(record + 0x16, 0);
            for i in 0..4 {
                m.sh(record + 0x18 + 2 * i, 0xffff);
            }
        }
    }
    for bank in 0..2u32 {
        for i in 0..0x14u32 {
            let d = pic_decoder(m);
            m.sb(d + pic::TILE_PAGES + bank * 0x14 + i, 0);
        }
    }
}

/// 80056E58: start streaming the pictures: 8002C500(file, ring, ring size,
/// 0x2000 (0x3000 with music), completion 80056EAC).
pub fn picture_stream_start(m: &mut Machine, c: &mut Invoker) {
    let frame = m.push_frame(0x20);
    let music = m.gw(PICTURE_MUSIC);
    let flags = if m.lw(music + 4) != 0 { 0x3000 } else { 0x2000 };
    let d = pic_decoder(m);
    m.sw(frame + 0x10, PICTURE_STREAM_DONE);
    let file = m.lw(d + pic::FILE);
    let ring = m.lw(d + pic::RING);
    let size = m.lw(d + pic::RING_SIZE);
    c(m, QUEUE_STREAM, [file, ring, size, flags]);
    m.pop_frame(0x20);
}

/// 80056EAC: completion of the picture stream: END = 1.
pub fn picture_stream_done(m: &mut Machine) {
    let d = pic_decoder(m);
    m.sb(d + pic::END, 1);
}

/// 80056EBC: reset the CD queue; returns 1.
pub fn picture_stop(m: &mut Machine, c: &mut Invoker) -> u32 {
    m.push_frame(0x18);
    c(m, RESET_CD_QUEUE, [0; 4]);
    m.pop_frame(0x18);
    1
}

/// 80056EE0: 1 while bank 0 is being loaded.
pub fn picture_bank_zero(m: &mut Machine) -> u32 {
    let d = pic_decoder(m);
    u32::from(m.lbu(d + pic::BANK) == 0)
}

/// 80056EF4: [`picture_next_chunk`]; with `poll` 0, retry for up to 121
/// frames (CD queue 8002C0B4 and `VSync(0)` each), then set END. Returns 1
/// once a chunk was taken or the wait gave up, else 0.
pub fn picture_wait_frame(m: &mut Machine, c: &mut Invoker, poll: u32) -> u32 {
    let frame = m.push_frame(0x20);
    let result = if poll != 0 {
        let d = pic_decoder(m);
        u32::from(picture_next_chunk(m, c, d) != 0)
    } else {
        m.sw(frame + 0x10, 0x78);
        loop {
            let d = pic_decoder(m);
            if picture_next_chunk(m, c, d) != 0 {
                break 1;
            }
            c(m, PROCESS_CD_QUEUE, [0; 4]);
            vsync_wait(m);
            let count = m.lw(frame + 0x10).wrapping_sub(1);
            m.sw(frame + 0x10, count);
            if (m.lw(frame + 0x10) as i32) < 0 {
                let d = pic_decoder(m);
                m.sb(d + pic::END, 1);
                break 1;
            }
        }
    };
    m.pop_frame(0x20);
    result
}

/// 80056F90: take the next picture stream chunk (8002B5C4) and apply it.
/// Header: +6 frame (0 ends), +0xC step (high nibble) and kind (low
/// nibble), +0xD restart flag, +0xF texture slot, +0x10 slot record, +0x13
/// page and bank selectors, +0x14.. slot data. Slot 0/1 chunks are CLUTs
/// (nonzero entries get the STP bit), others texture pages (a word at
/// +0x8F4 removed). A chunk whose slot is (+0x13 & 0xF) + 15 starts a new
/// picture in the other bank. Slots 2..17 set their bank's tile page
/// flags; slot records go to the bank's effect slots; kind 0 uploads the
/// data (LoadImage to the slot's rectangle, recorded in 800C5668). Step 1
/// (2 with restart) goes to +0. Returns 1 when a chunk was taken.
pub fn picture_next_chunk(m: &mut Machine, c: &mut Invoker, d: u32) -> u32 {
    let frame = m.push_frame(0x28);
    let result = picture_next_chunk_body(m, c, d, frame);
    m.pop_frame(0x28);
    result
}

fn picture_next_chunk_body(m: &mut Machine, c: &mut Invoker, d: u32, frame: u32) -> u32 {
    if c(m, NEXT_CHUNK, [frame + 0x10, frame + 0x14, 0, 0]) != 0 {
        return 0;
    }
    let header = m.lw(frame + 0x14);
    if m.lhu(header + 6) == 0 {
        m.sb(d + pic::END, 1);
    }
    let header = m.lw(frame + 0x14);
    let kinds = m.lbu(header + 0xc);
    let page = u32::from(m.lbu(header + 0xf));
    let kind = kinds & 0xf;
    let step = kinds >> 4;
    let selector = u32::from(m.lbu(header + 0x13) & 0xf);
    let restart = m.lbu(header + 0xd);
    if page < 2 {
        let mut entry = m.lw(frame + 0x10).wrapping_add(0x14);
        for _ in 0..0x100 {
            let value = m.lhu(entry);
            if value != 0 {
                m.sh(entry, value | 0x8000);
            }
            entry = entry.wrapping_add(2);
        }
    } else {
        let mut word = m.lw(frame + 0x10).wrapping_add(0x8f4);
        for _ in 0x23d..0x465 {
            let next = m.lw(word + 4);
            m.sw(word, next);
            word = word.wrapping_add(4);
        }
    }
    if page == selector.wrapping_add(0xf) {
        let bank = m.lbu(d + pic::BANK);
        m.sb(d + pic::BANK, u8::from(bank == 0));
        if restart == 1 {
            m.sb(d + pic::BANK, 0);
        }
        for slot in 0..5u32 {
            let bank = u32::from(m.lbu(d + pic::BANK));
            let record = pic_slot(d, bank, slot);
            m.sb(record + pic::SLOTS, 0);
            let bank = u32::from(m.lbu(d + pic::BANK));
            m.sh(pic_slot(d, bank, slot) + 0x14, 0xffff);
            let bank = u32::from(m.lbu(d + pic::BANK));
            m.sh(pic_slot(d, bank, slot) + 0x16, 0xffff);
            for i in 0..4u32 {
                let bank = u32::from(m.lbu(d + pic::BANK));
                m.sh(pic_slot(d, bank, slot) + 0x18 + 2 * i, 0);
            }
        }
    }
    if (2..18).contains(&page) {
        // Slots 6..17 set one flag (+0xA2 + 17 - slot); 2..5 set two, the
        // second 4 bytes further (+0xB1/+0xB5 .. +0xAE/+0xB2).
        let first = pic::TILE_PAGES + 17 - page;
        let offsets: &[u32] = if page < 6 {
            &[first, first + 4]
        } else {
            &[first]
        };
        for &offset in offsets {
            let bank = u32::from(m.lbu(d + pic::BANK));
            let flags = d.wrapping_add(bank * 0x14);
            let header = m.lw(frame + 0x14);
            let value = m.lbu(header + 0x13) >> 4;
            m.sb(flags + offset, value);
        }
    }
    let header = m.lw(frame + 0x14);
    let record = m.lbu(header + 0x10);
    if record & 0xf == 1 {
        let slot = u32::from(record >> 4);
        let bank = u32::from(m.lbu(d + pic::BANK));
        let effect = m.lbu(header + 0x11) & 0xf;
        m.sb(pic_slot(d, bank, slot) + pic::SLOTS, effect);
        let header = m.lw(frame + 0x14);
        let bank = u32::from(m.lbu(d + pic::BANK));
        let start = m.lhu(header + 0x14);
        m.sh(pic_slot(d, bank, slot) + 0x14, start);
        let bank = u32::from(m.lbu(d + pic::BANK));
        let end = m.lhu(header + 0x16);
        m.sh(pic_slot(d, bank, slot) + 0x16, end);
        for i in 0..4u32 {
            let value = m.lhu(header + 0x18 + 2 * i);
            let bank = u32::from(m.lbu(d + pic::BANK));
            m.sh(pic_slot(d, bank, slot) + 0x18 + 2 * i, value);
        }
    }
    if kind == 0 {
        let rect = PICTURE_RECTS + page * 8;
        if page >= 2 {
            let x0 = if m.lbu(d + pic::BANK) != 0 {
                0x380
            } else {
                0x300
            };
            let tile = page - 2;
            let row = (tile as i32) >> 2;
            let column = (tile as i32) - (row << 2);
            m.sh(rect, (x0 - ((column - 3) << 5)) as u16);
            m.sh(rect + 2, (0 - (row - 3) * 0x46) as u16);
            m.sh(rect + 4, 0x20);
            m.sh(rect + 6, 0x46);
        } else {
            m.sh(rect, 0x300);
            m.sh(rect + 4, 0x100);
            m.sh(rect + 6, 1);
            let y = match (m.lbu(d + pic::BANK) != 0, page) {
                (false, 0) => Some((2, 0x119)),
                (false, _) => Some((0xa, 0x118)),
                (true, 0) => Some((2, 0x11b)),
                (true, _) => Some((0xa, 0x11a)),
            };
            if let Some((offset, value)) = y {
                m.sh(PICTURE_RECTS + offset, value);
            }
        }
        let source = m.lw(frame + 0x10).wrapping_add(0x14);
        m.sw(PICTURE_SOURCES + page * 4, source);
        sdk::load_image(m, rect, source);
    }
    if step == 1 {
        m.sb(d + pic::STEP, if restart == 1 { 2 } else { 1 });
    }
    let header = m.lw(frame + 0x14);
    c(m, RELEASE_CHUNK, [header, 0, 0, 0]);
    1
}

/// 80057774: one shown picture frame. A step of 1 flips the shown bank
/// (and counts it), 2 restarts layered drawing; either clears the step,
/// the slot, the frame and the effect. Layered sequences draw through
/// [`picture_layers`]; others add the noise fade (80083084, level by
/// frame: in over frames 30..93, out over 146..209) and the border
/// (8005891C). Then the next chunk ([`picture_next_chunk`]); counts the
/// frame. Returns 1 at END.
pub fn picture_frame(m: &mut Machine, c: &mut Invoker) -> u32 {
    let frame = m.push_frame(0x20);
    let d = pic_decoder(m);
    let step = m.lbu(d + pic::STEP);
    if step != 0 {
        if step == 1 {
            let shown = m.lbu(d + pic::SHOWN_BANK);
            m.sb(d + pic::SHOWN_BANK, shown ^ 1);
            let d = pic_decoder(m);
            let count = m.lhu(d + pic::SHOWN).wrapping_add(1);
            m.sh(d + pic::SHOWN, count);
        } else {
            m.sb(d + pic::SHOWN_BANK, 0);
            let d = pic_decoder(m);
            m.sh(d + pic::SHOWN, 0);
            m.sb(d + pic::LAYERED, 1);
        }
        let d0 = pic_decoder(m);
        m.sb(d0 + pic::STEP, 0);
        let d = pic_decoder(m);
        m.sb(d + pic::SLOT, 0);
        let d1 = pic_decoder(m);
        m.sw(d + pic::FRAME, 0);
        m.sb(d1 + pic::EFFECT, 0);
    }
    let d = pic_decoder(m);
    let next = if m.lbu(d + pic::LAYERED) == 1 {
        let block = m.gw(PICTURE);
        let packets = m.lw(block + 0x34);
        let ot = m.lw(block + 0x38);
        picture_layers(m, d, packets, ot)
    } else {
        let d = pic_decoder(m);
        let rect = frame + 0x10;
        for i in 0..2 {
            let word = m.lw(PICTURE_NOISE_RECT + 4 * i);
            m.sw(rect + 4 * i, word);
        }
        let count = m.lw(d + pic::FRAME);
        let since = count.wrapping_sub(0x1e);
        if since < 0xb5 {
            let mut level = if since < 0x40 {
                0x1000u32.wrapping_sub(since << 6)
            } else if count.wrapping_sub(0x5e) < 0x34 {
                0
            } else {
                let fade = count.wrapping_sub(0x92);
                if fade < 0x40 { fade << 6 } else { 0x1000 }
            };
            if (level as i32) < 0 {
                level = 0;
            }
            let block = m.gw(PICTURE);
            let ot = m.lw(block + 0x38).wrapping_add(0xc);
            let packets = m.lw(block + 0x34);
            let next = picture_noise(m, c, ot, packets, rect, level);
            let block = m.gw(PICTURE);
            m.sw(block + 0x34, next);
        }
        let block = m.gw(PICTURE);
        let packets = m.lw(block + 0x34);
        let ot = m.lw(block + 0x38);
        picture_border(m, packets, ot)
    };
    let block = m.gw(PICTURE);
    m.sw(block + 0x34, next);
    let d = pic_decoder(m);
    picture_next_chunk(m, c, d);
    let d = pic_decoder(m);
    let end = m.lbu(d + pic::END);
    let count = m.lw(d + pic::FRAME).wrapping_add(1);
    m.sw(d + pic::FRAME, count);
    m.pop_frame(0x20);
    u32::from(end ^ 1 == 0)
}

/// 80083084: the noise fade over `rect` (8-byte RECT): a texture-window /
/// page packet, 32x48 noise tiles (80082CD0, CLUT 0x4696) shaded by DPCS
/// towards (0x83,0x83,0x83) at `level`, a page reset, linked as one chain
/// first into `ot`. Returns the next packet (unchanged for an empty
/// rectangle).
pub fn picture_noise(
    m: &mut Machine,
    c: &mut Invoker,
    ot: u32,
    packets: u32,
    rect: u32,
    level: u32,
) -> u32 {
    m.push_frame(0x20);
    let result = if m.lh(rect + 4) <= 0 || m.lh(rect + 6) <= 0 {
        packets
    } else {
        let tiles = packets.wrapping_add(0x10);
        m.sw(packets, ((tiles << 8) >> 8) | 0x0300_0000);
        m.sw(packets + 4, 0xe100_0218);
        m.sw(packets + 8, 0xe20c_6318);
        m.sw(packets + 0xc, 0);
        m.gte.mtc2(ac3_gte::reg::RGBC, 0x6483_8383);
        m.gte.mtc2(ac3_gte::reg::IR0, level);
        let position = m.lw(rect);
        let size = m.lw(rect + 4);
        let end = c(m, DRAW_NOISE_TILES, [tiles, 0x4696, position, size]);
        m.sw(end + 4, 0xe100_0220);
        m.sw(end, 0x0300_0000);
        m.sw(end + 8, 0xe200_0000);
        m.sw(end + 0xc, 0);
        // lwl/swl at +2: copy the table's 24-bit link into the chain end,
        // then point the table at the chain (top bytes kept).
        let link = m.lw(ot) & 0x00ff_ffff;
        let tag = m.lw(end);
        m.sw(end, (tag & 0xff00_0000) | link);
        let head = m.lw(ot);
        m.sw(ot, (head & 0xff00_0000) | (packets & 0x00ff_ffff));
        end.wrapping_add(0x10)
    };
    m.pop_frame(0x20);
    result
}

/// 80057938(decoder, packets, ot): the layered picture: the effect slots
/// of the shown bank switch effects at their start frames and advance at
/// their end frames; the effect draws (0 tiles, 1 tiles with the other
/// bank blended in, 2..5 tiles with a tint, 6 zoom, 7 wipe, 8/9 tiles
/// with a pulse), counting its frames, then the border. Returns the next
/// packet.
pub fn picture_layers(m: &mut Machine, d: u32, packets: u32, ot: u32) -> u32 {
    m.push_frame(0x20);
    let slot = pic_current_slot(m, d, d);
    let end = i32::from(m.lh(slot + 0x16)).wrapping_add(i32::from(m.lh(slot + 0x1e)));
    if m.lw(d + pic::FRAME) == end as u32 {
        let next = m.lbu(d + pic::SLOT);
        m.sb(d + pic::EFFECT, 0);
        m.sw(d + pic::EFFECT_TIME, 0);
        m.sb(d + pic::SLOT, next.wrapping_add(1));
    }
    let slot = pic_current_slot(m, d, d);
    if m.lw(d + pic::FRAME) == m.lh(slot + 0x14) as i32 as u32 {
        let effect = m.lbu(slot + pic::SLOTS);
        m.sw(d + pic::EFFECT_TIME, 0);
        m.sb(d + pic::EFFECT, effect);
    }
    let effect = m.lbu(d + pic::EFFECT);
    let (next, timed) = match effect {
        0 => (picture_tiles(m, d, packets, ot), false),
        1 => {
            let next = picture_tiles(m, d, packets, ot);
            (picture_blend(m, d, next, ot), true)
        }
        2..=5 => {
            let next = picture_tiles(m, d, packets, ot);
            (picture_tint(m, d, next, ot), true)
        }
        6 => (picture_zoom(m, d, packets, ot), true),
        7 => (picture_wipe(m, d, packets, ot), true),
        8 | 9 => (picture_tiles(m, d, packets, ot), true),
        _ => (packets, false),
    };
    if timed {
        let time = m.lw(d + pic::EFFECT_TIME).wrapping_add(1);
        m.sw(d + pic::EFFECT_TIME, time);
    }
    let result = picture_border(m, next, ot);
    m.pop_frame(0x20);
    result
}

/// Height of tile row `row` (rows 0..2 70 lines, row 3 46, row 4 24) and
/// its screen y and texture v.
fn tile_row(row: u32) -> (u32, u32, u32) {
    match row {
        3 => (0x2e, 0x64 + 0xd2, 0xd2),
        4 => (0x18, 0x64 | 0x100, 0),
        _ => (0x46, 0x64 + 0x46 * row, 0x46 * row),
    }
}

/// Texture page and CLUT of tile `tile` from `bank` (pages 0x8C/0x9C or
/// 0x8E/0x9E for tiles 16..19; the alternate CLUT when the tile's page flag
/// is set); `None` for other banks.
fn tile_page(
    m: &mut Machine,
    d: u32,
    bank: u8,
    tile: u32,
    pages: [u32; 2],
    cluts: [u32; 2],
) -> Option<(u32, u32)> {
    let (index, base) = match bank {
        0 => (0, pic::TILE_PAGES),
        1 => (1, pic::TILE_PAGES + 0x14),
        _ => return None,
    };
    let page = if tile < 16 {
        pages[index]
    } else {
        pages[index] + 0x10
    };
    let flag = m.lbu(d.wrapping_add(tile).wrapping_add(base));
    let clut = if flag != 0 {
        cluts[index] + 0x40
    } else {
        cluts[index]
    };
    Some((page, clut))
}

/// 80057AE0(decoder, packets, ot): the 20 picture tiles (4 columns of 64
/// pixels, 5 rows) of the shown bank as SPRT with their texture page, in
/// table entry 3. Brightness 0x80, fading in over the slot for effect 1;
/// effect 8 pulses it (parameters: period, amplitude). Returns the next
/// packet.
pub fn picture_tiles(m: &mut Machine, d: u32, packets: u32, ot: u32) -> u32 {
    let mut level: u32 = 0x80;
    if m.lbu(d + pic::EFFECT) == 1 {
        let global = pic_decoder(m);
        let slot = pic_current_slot(m, global, d);
        let span = i32::from(m.lh(slot + 0x16))
            .wrapping_sub(i32::from(m.lh(slot + 0x14)))
            .wrapping_sub(1);
        let time = (m.lw(d + pic::EFFECT_TIME) << 7).wrapping_neg() as i32;
        level = quotient(m, time, span, 0x8005_7b74).wrapping_add(0x80) as u32;
        if sext16(level) > 0x80 {
            level = 0x80;
        }
        if sext16(level) < 0 {
            level = 0;
        }
    }
    if m.lbu(d + pic::EFFECT) == 8 {
        let slot = pic_current_slot(m, d, d);
        let (period, amplitude) = if m.lh(slot + 0x18) == 0 {
            (0x10, 8)
        } else {
            (u32::from(m.lhu(slot + 0x18)), u32::from(m.lhu(slot + 0x1a)))
        };
        let period = sext16(period);
        let time = m.lw(d + pic::EFFECT_TIME) as i32;
        let t = if time < period.wrapping_mul(2) {
            sext16(u32::from(m.lhu(d + pic::EFFECT_TIME)))
        } else {
            0
        };
        let amplitude = sext16(amplitude);
        let square = t.wrapping_mul(t).wrapping_mul(amplitude.wrapping_neg());
        let a = quotient(m, square, period.wrapping_mul(period), 0x8005_7c8c);
        let b = quotient(m, t.wrapping_mul(amplitude << 1), period, 0x8005_7cd0);
        level = a.wrapping_add(b).wrapping_add(0x80) as u32;
        if sext16(level) >= 0x100 {
            level = 0xff;
        }
        if sext16(level) < 0 {
            level = 0;
        }
    }
    let b = sext16(level) as u32;
    let color = (b << 16) | (b << 8) | b;
    let mut packet = packets;
    let (mut page, mut clut) = (0u32, 0u32);
    for tile in 0..20u32 {
        let row = tile >> 2;
        let (height, y, v) = tile_row(row);
        let bank = m.lbu(d + pic::SHOWN_BANK);
        if let Some((p, c)) = tile_page(m, d, bank, tile, [0x8c, 0x8e], [0x4630, 0x46b0]) {
            page = p;
            clut = c;
        }
        let u = (tile & 3) * 0x40;
        let x = 0x20 | u;
        m.sw(packet + 0xc, (y << 16) | (sext16(x) as u32));
        let uv = (clut << 16) | ((v & 0xff) << 8) | (u & 0xff);
        m.sw(packet + 0x14, (height << 16) | 0x40);
        m.sw(packet + 8, color);
        m.sb(packet + 0xb, 0x64);
        m.sw(packet + 4, page);
        m.sw(packet + 0x10, uv);
        m.sb(packet + 7, 0xe1);
        m.sb(packet + 6, 0);
        link_or(m, ot + 0xc, packet, 5);
        packet = packet.wrapping_add(0x18);
    }
    packet
}

/// 80057F14(decoder, packets, ot): the other bank's 20 tiles blended in
/// (semi-transparent SPRT, each with its DR_TPAGE, table entry 2) at a
/// level rising over the slot. Returns the next packet.
pub fn picture_blend(m: &mut Machine, d: u32, packets: u32, ot: u32) -> u32 {
    let global = pic_decoder(m);
    let slot = pic_current_slot(m, global, d);
    let span = i32::from(m.lh(slot + 0x16))
        .wrapping_sub(i32::from(m.lh(slot + 0x14)))
        .wrapping_sub(1);
    let time = (m.lw(d + pic::EFFECT_TIME) << 7) as i32;
    let mut level = quotient(m, time, span, 0x8005_7f8c) as u32;
    let other = m.lbu(d + pic::SHOWN_BANK) ^ 1;
    if sext16(level) > 0x80 {
        level = 0x80;
    }
    if sext16(level) < 0 {
        level = 0;
    }
    let b = sext16(level) as u32;
    let color = (b << 16) | (b << 8) | b | 0x6600_0000;
    let mut packet = packets;
    let (mut page, mut clut) = (0u32, 0u32);
    for tile in 0..20u32 {
        let row = tile >> 2;
        let (height, y, v) = tile_row(row);
        if let Some((p, c)) = tile_page(m, d, other, tile, [0xac, 0xae], [0x4630, 0x46b0]) {
            page = p;
            clut = c;
        }
        let u = (tile & 3) * 0x40;
        let x = 0x20 | u;
        let mode = 0xe100_0200 | page;
        m.sw(packet + 8, (y << 16) | (sext16(x) as u32));
        let uv = (clut << 16) | ((v & 0xff) << 8) | (u & 0xff);
        m.sw(packet + 4, color);
        m.sw(packet + 0xc, uv);
        let code = m.lbu(packet + 7);
        m.sw(packet + 0x10, (height << 16) | 0x40);
        m.sb(packet + 7, code | 2);
        link_or(m, ot + 8, packet, 4);
        m.sb(packet + 0x17, 1);
        m.sw(packet + 0x18, mode);
        link_masked(m, ot + 8, packet + 0x14);
        packet = packet.wrapping_add(0x1c);
    }
    packet
}

/// 80058230(decoder, packets, ot): a full-picture tint (semi-transparent
/// TILE 256x280 at 32,100 in table entry 1, with its DR_TPAGE: additive
/// for effects 2/3, subtractive for 4/5): grey level rising (2, 4) or
/// falling (3, 5) from the slot start + parameter 0 to parameter 1 (or the
/// slot end). Returns the next packet.
pub fn picture_tint(m: &mut Machine, d: u32, packets: u32, ot: u32) -> u32 {
    let slot = pic_current_slot(m, d, d);
    let start = m.lhu(slot + 0x14).wrapping_add(m.lhu(slot + 0x18));
    let until = if m.lh(slot + 0x1a) == 0 {
        m.lhu(slot + 0x16)
    } else {
        m.lhu(slot + 0x1a)
    };
    let span = sext16(u32::from(until)).wrapping_sub(sext16(u32::from(start)));
    let (delta, base): (i32, i32) = match m.lbu(d + pic::EFFECT) {
        2 | 4 => (0xff, 0),
        3 | 5 => (-0xff, 0xff),
        _ => (0, 0),
    };
    let level = if (m.lw(d + pic::FRAME) as i32) < sext16(u32::from(start)) {
        base
    } else {
        let scaled = (m.lw(d + pic::EFFECT_TIME) as i32).wrapping_mul(sext16(delta as u32));
        base.wrapping_add(quotient(m, scaled, span, 0x8005_8334))
    };
    let clamped = sext16(level as u32);
    let grey = (if clamped >= 0x100 {
        0xff
    } else if clamped >= 0 {
        level as u32
    } else {
        0
    }) & 0xff;
    let packet = packets;
    m.sw(packet + 4, (grey << 16) | (grey << 8) | grey | 0x6200_0000);
    m.sw(packet + 8, 0x0064_0020);
    m.sw(packet + 0xc, 0x0118_0100);
    link_or(m, ot + 4, packet, 3);
    let effect = m.lbu(d + pic::EFFECT);
    let mode = match effect {
        0 | 1 => 0,
        2 | 3 => 0x20,
        4 | 5 => 0x40,
        _ => 0,
    };
    let page = packet.wrapping_add(0x10);
    m.sb(page + 3, 1);
    m.sw(page + 4, 0xe100_0200 | mode);
    link_masked(m, ot + 4, page);
    page.wrapping_add(8)
}

/// 80058474(decoder, packets, ot): the shown bank's first page zoomed by
/// parameter 2 (a 256/z x 280/z window centred on parameters 0/1) as one
/// POLY_FT4 over 32,100 256x280 in table entry 3. Returns the next packet.
pub fn picture_zoom(m: &mut Machine, d: u32, packets: u32, ot: u32) -> u32 {
    let bank = m.lbu(d + pic::SHOWN_BANK);
    let slot = pic_current_slot(m, d, d);
    let zoom = i32::from(m.lh(slot + 0x1c));
    let w = quotient(m, 0x100, zoom, 0x8005_84ac) as u32;
    let h = quotient(m, 0x118, zoom, 0x8005_84e0) as u32;
    let half = |v: u32| {
        let s = sext16(v);
        (s + i32::from(s < 0)) >> 1
    };
    let u0 = u32::from(m.lbu(slot + 0x18)).wrapping_sub(half(w) as u32);
    let v0 = u32::from(m.lbu(slot + 0x1a)).wrapping_sub(half(h) as u32);
    let (page, clut) = match bank {
        0 => (0x8c, 0x4630),
        1 => (0x8e, 0x46b0),
        _ => (0, 0),
    };
    let p = packets;
    m.sw(p + 4, 0x0080_8080);
    m.sw(p + 8, 0x0064_0020);
    let v = v0 & 0xff;
    let u = u0 & 0xff;
    m.sw(p + 0xc, (clut << 16) | (v << 8) | u);
    let right = u.wrapping_add(sext16(w) as u32).wrapping_sub(1);
    m.sw(p + 0x10, 0x0064_0120);
    m.sw(p + 0x14, (page << 16) | (v << 8) | right);
    let bottom = v.wrapping_add(sext16(h) as u32).wrapping_sub(1) << 8;
    m.sb(p + 3, 9);
    m.sw(p + 0x20, 0x017c_0120);
    m.sw(p + 0x18, 0x017c_0020);
    m.sw(p + 0x1c, bottom | u);
    m.sw(p + 0x24, bottom | right);
    m.sb(p + 7, 0x2c);
    link_masked(m, ot + 0xc, p);
    p.wrapping_add(0x28)
}

/// 8005868C(decoder, packets, ot): a vertical wipe of the shown bank's
/// picture by the effect time (4 lines per frame), as one or two POLY_FT4
/// in table entry 3 (the second once the first page is used up). Returns
/// the next packet.
pub fn picture_wipe(m: &mut Machine, d: u32, packets: u32, ot: u32) -> u32 {
    let time = m.lw(d + pic::EFFECT_TIME);
    let t0 = (time << 1).wrapping_mul(3);
    let u_base = time.wrapping_mul(3);
    let mut v_top = time << 2;
    let rows = 0x118u32.wrapping_sub(time << 3);
    let not_t0 = !t0;
    let (count, mut height, mut v_size) =
        if ((v_top & 0xff) as i32).wrapping_add(sext16(rows)) < 0x100 {
            (1, 0x118, 0x18u32.wrapping_sub(time << 3))
        } else {
            (2, v_top.wrapping_add(0x100), !v_top)
        };
    let mut y = 0x64u32;
    let (mut page, mut clut) = (0u32, 0u32);
    let u_left = u_base & 0xff;
    let u_right = u_left.wrapping_add(not_t0 & 0xff);
    let mut packet = packets;
    for quad in 0..count {
        if quad == 1 {
            let step = time.wrapping_mul(4);
            v_top = 0;
            y = step.wrapping_add(0x164);
            height = 0x18u32.wrapping_sub(step);
            v_size = height;
        }
        match m.lbu(d + pic::SHOWN_BANK) {
            0 => {
                page = if quad != 0 { 0x9c } else { 0x8c };
                clut = 0x4630;
            }
            1 => {
                page = if quad != 0 { 0x9e } else { 0x8e };
                clut = 0x46b0;
            }
            _ => {}
        }
        let top = (sext16(y) as u32) << 16;
        m.sw(packet + 8, top | 0x20);
        let v = v_top & 0xff;
        m.sw(packet + 0xc, (clut << 16) | (v << 8) | u_left);
        m.sw(packet + 0x14, (page << 16) | (v << 8) | u_right);
        let bottom = (sext16(y).wrapping_add(sext16(height)) as u32) << 16;
        m.sw(packet + 0x18, bottom | 0x20);
        let v_end = v.wrapping_add(v_size & 0xff) << 8;
        m.sw(packet + 0x1c, v_end | u_left);
        m.sb(packet + 3, 9);
        m.sw(packet + 4, 0x0080_8080);
        m.sw(packet + 0x10, top | 0x120);
        m.sw(packet + 0x20, bottom | 0x120);
        m.sw(packet + 0x24, v_end | u_right);
        m.sb(packet + 7, 0x2c);
        link_masked(m, ot + 0xc, packet);
        packet = packet.wrapping_add(0x28);
    }
    packet
}

/// `(value * 0x80010003) >> 46`-style division by 32767 (the compiler's
/// signed reciprocal multiply): `(hi(value * 0x80010003) + value) >> 14`
/// minus the sign.
fn div32767(value: i32) -> i32 {
    let hi = ((i64::from(value) * i64::from(0x8001_0003u32 as i32)) >> 32) as i32;
    (hi.wrapping_add(value) >> 14).wrapping_sub(value >> 31)
}

/// One gouraud quad of the border (POLY_G4 semi-transparent, 9 words) in
/// table entry 0.
#[allow(clippy::too_many_arguments)]
fn border_quad(m: &mut Machine, ot: u32, p: u32, xy: [u32; 4], colors: [u32; 4], order: bool) {
    // `order`: the straight-edge loops store xy0/xy2 first, the corner
    // loops store the colours first.
    let [c0, c1, c2, c3] = colors;
    let [a, b, c, e] = xy;
    if order {
        m.sw(p + 8, a);
        m.sw(p + 0x18, c);
        m.sw(p + 4, c0);
        m.sw(p + 0xc, c1);
        m.sw(p + 0x14, c2);
        m.sw(p + 0x1c, c3);
        m.sw(p + 0x10, b);
        m.sw(p + 0x20, e);
    } else {
        m.sw(p + 4, c0);
        m.sw(p + 0x14, c2);
        m.sw(p + 0xc, c1);
        m.sw(p + 0x1c, c3);
        m.sw(p + 8, a);
        m.sw(p + 0x10, b);
        m.sw(p + 0x18, c);
        m.sw(p + 0x20, e);
    }
    link_or(m, ot, p, 8);
}

/// 8005891C(packets, ot): the picture's flickering border: every 80
/// frames (context frame counter +0x24) new jitter (800C56B8, 800C5758,
/// `rand`); per frame the wave rows (80023E4C) scaled by the 10-step phase
/// give each edge's offsets; the left/right edges are 36 gouraud quads
/// (white to dark) with quadratic corners, the top/bottom edges 33; all in
/// table entry 0, then a DR_TPAGE. Returns the next packet.
pub fn picture_border(m: &mut Machine, packets: u32, ot: u32) -> u32 {
    let frame = m.push_frame(0x178);
    for i in 0..0x98 / 4 {
        let word = m.lw(BORDER_WAVES + 4 * i);
        m.sw(frame + 0x10 + 4 * i, word);
    }
    let context = m.gw(CONTEXT);
    let count = m.lw(context + 0x24) as i32;
    if remainder(m, count, 0x50, 0x8005_8a14) == 0x4f {
        for row in 0..4u32 {
            for k in 0..0x26u32 {
                let r = sdk::rand(m) as i32;
                let jitter = div32767(r.wrapping_mul(8));
                m.sb(BORDER_JITTER + row * 0x26 + k, jitter as u8);
                let r = sdk::rand(m) as i32;
                let select = div32767(r.wrapping_mul(4));
                m.sw(BORDER_ROWS + row * 4, select as u32);
            }
        }
    }
    let context = m.gw(CONTEXT);
    let count = m.lw(context + 0x24) as i32;
    let cycle = remainder(m, count, 80, 0x8005_8b20);
    let phase = quotient(m, cycle, 8, 0x8005_8b54);
    let offsets = frame + 0xa8;
    for row in 0..4u32 {
        let base = row * 0x26;
        for k in 0..0x26u32 {
            let step = remainder(m, phase, 10, 0x8005_8bb8);
            let select = m.lw(BORDER_ROWS + row * 4);
            let jitter = i32::from(m.lbu(BORDER_JITTER + base + k));
            let wave_at = frame
                .wrapping_add(0x10)
                .wrapping_add(select.wrapping_mul(0x26))
                .wrapping_add(k);
            let wave = i32::from(m.lbu(wave_at));
            let value = if step < 5 {
                quotient(m, phase.wrapping_mul(wave + jitter), 5, 0x8005_8c34) + 0x10
            } else {
                let shrink = quotient(m, (phase - 5).wrapping_mul(wave + jitter), 5, 0x8005_8cb0);
                let wave = i32::from(m.lbu(wave_at));
                wave + jitter + 0x10 - shrink
            };
            m.sb(offsets + base + k, value as u8);
        }
    }
    const BRIGHT: u32 = 0x3aff_ffff;
    const DARK: u32 = 0x0003_0303;
    const CORNER: u32 = 0x3a03_0303;
    let mut p = packets;
    let offset = |m: &mut Machine, index: u32| u32::from(m.lbu(offsets + index));
    for row in 0..4u32 {
        let cur = row * 0x26;
        let next = cur + 1;
        match row {
            0 | 1 => {
                // Left (row 0) and right (row 1, mirrored at x = 0x120).
                let edge: u32 = if row == 0 { 0x20 } else { 0x120 };
                let mirror = |x: u32| -> u32 {
                    if row == 0 {
                        edge.wrapping_add(x)
                    } else {
                        edge.wrapping_sub(x)
                    }
                };
                let mut y = 0x64u32;
                for k in 0..0x24 {
                    let y0 = y << 16;
                    let y1 = y.wrapping_add(8) << 16;
                    y = y.wrapping_add(8);
                    let a = offset(m, cur + k);
                    let b = offset(m, next + k);
                    border_quad(
                        m,
                        ot,
                        p,
                        [y0 | edge, y0 | mirror(a), y1 | edge, y1 | mirror(b)],
                        [BRIGHT, DARK, 0x00ff_ffff, DARK],
                        true,
                    );
                    p = p.wrapping_add(0x24);
                }
                // Top corner: x = s^2/2 - 8s + 32, y = 100 + 4s.
                for s in 0..8i32 {
                    let curve = |s: i32| {
                        let sq = s * s;
                        let half = (sq + ((sq as u32) >> 31) as i32) >> 1;
                        sext16(half.wrapping_sub(s << 3).wrapping_add(0x20) as u32)
                    };
                    let y0 = ((s * 4 + 0x64) as u32) << 16;
                    let y1 = (((s + 1) * 4 + 0x64) as u32) << 16;
                    let x0 = mirror(curve(s) as u32);
                    let x1 = mirror(curve(s + 1) as u32);
                    border_quad(
                        m,
                        ot,
                        p,
                        [y0 | edge, y0 | x0, y1 | edge, y1 | x1],
                        [BRIGHT, CORNER, BRIGHT, CORNER],
                        false,
                    );
                    p = p.wrapping_add(0x24);
                }
                // Bottom corner: x = s^2/2, y = 348 + 4s.
                for s in 0..8i32 {
                    let curve = |s: i32| {
                        let sq = s * s;
                        (sq.wrapping_add(((sq as u32) >> 31) as i32) << 15) >> 16
                    };
                    let y0 = ((s * 4 + 0x15c) as u32) << 16;
                    let y1 = (((s + 1) * 4 + 0x15c) as u32) << 16;
                    let x0 = mirror(curve(s) as u32);
                    let x1 = mirror(curve(s + 1) as u32);
                    border_quad(
                        m,
                        ot,
                        p,
                        [y0 | edge, y0 | x0, y1 | edge, y1 | x1],
                        [BRIGHT, CORNER, BRIGHT, CORNER],
                        false,
                    );
                    p = p.wrapping_add(0x24);
                }
            }
            _ => {
                // Top (row 2, y = 100 + offset) and bottom (row 3, y = 380 -
                // offset).
                let edge: u32 = if row == 2 { 0x64 } else { 0x17c };
                let mut x = 0x20u32;
                for k in 0..0x21 {
                    let x0 = sext16(x) as u32;
                    let x1 = sext16(x.wrapping_add(8)) as u32;
                    x = x.wrapping_add(8);
                    let b = offset(m, next + k);
                    let a = offset(m, cur + k);
                    let along = |o: u32| {
                        if row == 2 {
                            edge.wrapping_add(o)
                        } else {
                            edge.wrapping_sub(o)
                        }
                    };
                    border_quad(
                        m,
                        ot,
                        p,
                        [
                            (edge << 16) | x0,
                            (along(a) << 16) | x0,
                            (edge << 16) | x1,
                            (along(b) << 16) | x1,
                        ],
                        [BRIGHT, DARK, 0x00ff_ffff, DARK],
                        true,
                    );
                    p = p.wrapping_add(0x24);
                }
            }
        }
    }
    m.sb(p + 3, 1);
    m.sw(p + 4, 0xe100_0240);
    link_masked(m, ot, p);
    m.pop_frame(0x178);
    p.wrapping_add(8)
}

// ---- resident debug-font text (cursor sprite queue) --------------------------------------

/// `[gp+0x24]`: the cursor sprite queue (+0 count, +4 entries {xy,
/// uv/CLUT}, +0xE14 CLUT words per font, +0xE20 current font); drawn by
/// the main loop (80027408).
const SPRITE_QUEUE: u32 = 0x24;

/// 80027168(x, y, text): queue the characters of `text` (6-pixel cells
/// from 32 on, 16 per texture row of 6 lines starting at v 64) as cursor
/// sprites at (`x`, `y`) with the current font's CLUT word, up to 450
/// sprites. Returns the x after the text.
pub fn queue_text(m: &mut Machine, x: u32, y: u32, text: u32) -> u32 {
    let mut x = x;
    let mut cursor = text;
    let mut ch = u32::from(m.lbu(cursor));
    cursor = cursor.wrapping_add(1);
    let queue = m.gw(SPRITE_QUEUE);
    let mut count = m.lw(queue);
    let font = m.lw(queue + 0xe20);
    let mut entry = queue.wrapping_add((count << 3).wrapping_add(4));
    let clut = m.lw(queue.wrapping_add(font << 2).wrapping_add(0xe14));
    let y = y << 16;
    while ch != 0 {
        count = count.wrapping_add(1);
        if (count as i32) >= 0x1c3 {
            count = count.wrapping_sub(1);
            break;
        }
        let code = ch.wrapping_sub(0x20);
        let u = (code & 0xf) * 6;
        let row = ((code as i32) >> 4) as u32;
        let v = row.wrapping_mul(6).wrapping_add(0x40) << 8;
        m.sw(entry, (x & 0xffff) | y);
        x = x.wrapping_add(6);
        m.sw(entry + 4, u | v | clut);
        entry = entry.wrapping_add(8);
        ch = u32::from(m.lbu(cursor));
        cursor = cursor.wrapping_add(1);
    }
    let queue = m.gw(SPRITE_QUEUE);
    m.sw(queue, count);
    x
}

/// 80027228(x, y, value, format): queue `value` as text: format low byte
/// = digits (10 or more: returns -1), 0x100 zero padding (else spaces),
/// 0x400 no padding, 0x200 a sign; magnitude digits right to left in a
/// stack buffer, then [`queue_text`]. Returns its result.
pub fn queue_number(m: &mut Machine, x: u32, y: u32, value: u32, format: u32) -> u32 {
    let frame = m.push_frame(0x28);
    let digits = format & 0xff & 0xffff;
    let result = if digits >= 10 {
        0xffff_ffff
    } else {
        let flags = format & 0xf00;
        let mut cursor = frame + 0x19;
        let mut rest = if (value as i32) < 0 {
            value.wrapping_neg()
        } else {
            value
        };
        m.sb(frame + 0x19, 0);
        m.sb(frame + 0x18, b'0');
        let mut placed: u32 = 0;
        while placed & 0xffff < digits {
            cursor = cursor.wrapping_sub(1);
            if rest != 0 {
                let r = rest as i32;
                let tenth = (((i64::from(r) * 0x6666_6667) >> 34) as i32).wrapping_sub(r >> 31);
                let digit = r.wrapping_sub(tenth.wrapping_mul(10)).wrapping_add(0x30);
                m.sb(cursor, digit as u8);
                rest = tenth as u32;
            } else if placed & 0xffff != 0 {
                if flags & 0x400 != 0 {
                    cursor = cursor.wrapping_add(1);
                    break;
                }
                m.sb(cursor, if flags & 0x100 != 0 { b'0' } else { b' ' });
            }
            placed = placed.wrapping_add(1);
        }
        if flags & 0x200 != 0 {
            cursor = cursor.wrapping_sub(1);
            m.sb(cursor, if (value as i32) < 0 { b'-' } else { b'+' });
        }
        queue_text(m, x, y, cursor)
    };
    m.pop_frame(0x28);
    result
}

// ---- PsyQ libpress (MDEC) -----------------------------------------------------------------

/// PsyQ routines (libpress for the movie decoder; libc strcpy/strlen for
/// overlays 3 and 4; libgpu GetTPage for overlay 4) that `sdk.rs` does not
/// provide yet, with the original ABI (requested from the shared agent).
///
/// # MDEC host protocol
///
/// The MDEC and its DMA channels 0 (in) and 1 (out) are devices; their
/// register writes are host calls named after the original routine that
/// performs them:
///
/// * [`MDEC_RESET`] `[mode]` (800AAF98, from DecDCTReset): mode 0 resets
///   the MDEC (MDEC1 = 0x80000000, DMA 0/1 CHCR = 0, MDEC1 = 0x60000000)
///   and uploads the default tables through the input DMA: the
///   quantization table at 800B9934 and the IDCT scale table at 800B99B8
///   (each a 0x20-word MDEC command block: header word, then data). Mode 1
///   resets without the tables.
/// * [`MDEC_IN`] `[buffer, words]` (800AB088, from DecDCTin): after the
///   MDEC input FIFO is free (status bit 29 clear; the original polls up to
///   0x100000 times, then prints "DecDCTin timeout" and resets as mode 0),
///   DMA 0 sends `words` (the header's low halfword) words from
///   `buffer + 4` with MDEC0 = `[buffer]`, the command: bits 31..29 = 1
///   (decode macroblocks), bit 27 clear = 24-bit / set = 15-bit output,
///   bit 25 = STP bit for 15-bit output, low halfword = size in words. The
///   data are the run-level codes [`dec_dct_vlc2`] writes: per block a DC
///   halfword (`quant << 10 | dc & 0x3FF`), AC halfwords
///   (`run << 10 | level & 0x3FF`) and the end code 0xFE00, six blocks
///   (Cr, Cb, Y1..Y4) per 16x16 macroblock, macroblocks in column order.
/// * [`MDEC_OUT`] `[buffer, words]` (800AB118, from DecDCTout): after DMA
///   1 is idle (the original polls CHCR bit 24 up to 0x100000 times, then
///   prints "DecDCTout timeout"), DMA 1 receives `words` words of decoded
///   pixels into `buffer` (15-bit: 16x16 pixels per macroblock as 0x80
///   words; 24-bit: 0xC0 words), in 32-word blocks. The device writes RAM
///   during the call. Its result is nonzero once the transfer is complete
///   (default 1): the DMA-1 completion callback ([`sdk::DMA_CALLBACKS`]
///   entry 1, set by DecDCToutCallback) then runs before DecDCTout
///   returns, standing in for the DMA interrupt. A host that completes the
///   transfer later must run that callback itself, before the game polls
///   for it (8005578C).
pub mod pending_sdk {
    use crate::{machine::Machine, sdk};

    /// Host call: the MDEC reset of 800AAF98.
    pub const MDEC_RESET: u32 = 0x800a_af98;
    /// Host call: the MDEC input DMA of 800AB088.
    pub const MDEC_IN: u32 = 0x800a_b088;
    /// Host call: the MDEC output DMA of 800AB118.
    pub const MDEC_OUT: u32 = 0x800a_b118;
    /// 800B9A4C/800B9A58: libpress pointers to DMA 0 / DMA 1 CHCR (the
    /// routines return them in `v0`).
    const DMA0_CHCR_POINTER: u32 = 0x800b_9a4c;
    const DMA1_CHCR_POINTER: u32 = 0x800b_9a58;
    /// 800B9A84: DecDCTvlcSize2 limit (halfwords; 0xFFFFFF = none).
    pub const VLC_SIZE: u32 = 0x800b_9a84;
    /// 800B9A88: DecDCTvlc2 state saved when the limit stops a frame: bit
    /// stream, output, bit buffer, bit count, quantizer, block, the three
    /// DC predictors (Cr, Cb, Y).
    pub const VLC_STATE: u32 = 0x800b_9a88;
    /// 800B9AAC: end of the run-level output of the current frame.
    pub const VLC_END: u32 = 0x800b_9aac;
    /// 800B9AB4: DecDCTvlcBuild's packed table.
    const VLC_PACKED: u32 = 0x800b_9ab4;
    /// 80024EB4: "MDEC_rest:bad option(%d)\n".
    const BAD_OPTION: u32 = 0x8002_4eb4;

    /// 800AACD8 DecDCTReset(mode): mode 0 first resets the interrupt
    /// callbacks ([`sdk::reset_callback`]). Modes 0/1 reset the MDEC ([`MDEC_RESET`]),
    /// others print the bad-option message. Returns the original `v0`:
    /// the DMA 0 CHCR pointer (mode 0), 0x60000000 (mode 1) or printf's.
    pub fn dec_dct_reset(m: &mut Machine, mode: u32) -> u32 {
        m.push_frame(0x18);
        if mode == 0 {
            sdk::reset_callback(m);
        }
        m.push_frame(0x18);
        let result = match mode {
            0 => {
                m.host_call(MDEC_RESET, [mode, 0, 0, 0]);
                m.lw(DMA0_CHCR_POINTER)
            }
            1 => {
                m.host_call(MDEC_RESET, [mode, 0, 0, 0]);
                0x6000_0000
            }
            _ => {
                sdk::printf(m, [BAD_OPTION, mode, 0, 0]);
                0
            }
        };
        m.pop_frame(0x18);
        m.pop_frame(0x18);
        result
    }

    /// 800AAE30 DecDCTin(buffer, mode): command header bit 27 cleared for
    /// mode bit 0 (24-bit) else set, bit 25 set for mode bit 1 else
    /// cleared (RAM), then [`MDEC_IN`] with the header's word count.
    /// Returns the DMA 0 CHCR pointer.
    pub fn dec_dct_in(m: &mut Machine, buffer: u32, mode: u32) -> u32 {
        let header = m.lw(buffer);
        let header = if mode & 1 != 0 {
            header & 0xf7ff_ffff
        } else {
            header | 0x0800_0000
        };
        m.sw(buffer, header);
        let header = m.lw(buffer);
        let header = if mode & 2 != 0 {
            header | 0x0200_0000
        } else {
            header & 0xfdff_ffff
        };
        m.sw(buffer, header);
        let words = u32::from(m.lhu(buffer));
        m.host_call(MDEC_IN, [buffer, words, 0, 0]);
        m.lw(DMA0_CHCR_POINTER)
    }

    /// 800AAEAC DecDCTout(buffer, words): [`MDEC_OUT`]; when the host
    /// reports the transfer complete (default), the DMA-1 callback runs.
    /// Returns the DMA 1 CHCR pointer.
    pub fn dec_dct_out(m: &mut Machine, buffer: u32, words: u32) -> u32 {
        let complete = m.host_call_result(MDEC_OUT, [buffer, words, 0, 0], 1);
        let result = m.lw(DMA1_CHCR_POINTER);
        if complete != 0 {
            let callback = m.lw(sdk::DMA_CALLBACKS + 4);
            if callback != 0 {
                crate::dispatch::call(m, callback, [0; 4]);
            }
        }
        result
    }

    /// 800AC2D8 strcpy(dst, src) -> dst: copy bytes up to and including
    /// the NUL; a null `dst` or `src` copies nothing and returns 0.
    pub fn strcpy(m: &mut Machine, dst: u32, src: u32) -> u32 {
        if dst == 0 || src == 0 {
            return 0;
        }
        let mut from = src;
        let mut to = dst;
        loop {
            let byte = m.lbu(from);
            from = from.wrapping_add(1);
            m.sb(to, byte);
            to = to.wrapping_add(1);
            if byte == 0 {
                break;
            }
        }
        dst
    }

    /// 800AC328 strlen(s): bytes before the NUL (0 for a null pointer).
    pub fn strlen(m: &mut Machine, s: u32) -> u32 {
        // One implementation: sdk.rs owns strlen 800AC328.
        crate::sdk::strlen(m, s)
    }

    /// 800AAB98 GetTPage(tp, abr, x, y) (libgpu): the texture page bits
    /// `(tp & 3) << 7 | (abr & 3) << 5 | (y & 0x100) >> 4 | (x & 0x3FF) >> 6
    /// | (y & 0x200) << 2`.
    pub fn get_tpage(tp: u32, abr: u32, x: u32, y: u32) -> u32 {
        ((tp & 3) << 7)
            | ((abr & 3) << 5)
            | ((y & 0x100) >> 4)
            | ((x & 0x3ff) >> 6)
            | ((y & 0x200) << 2)
    }

    /// 800AB368 DecDCTvlcSize2(size): the output limit for DecDCTvlc2 in
    /// halfwords (`size * 2`; `size` <= 1 removes it). Returns the previous
    /// limit.
    pub fn dec_dct_vlc_size2(m: &mut Machine, size: u32) -> u32 {
        let previous = m.lw(VLC_SIZE);
        let limit = if (size.wrapping_sub(1) as i32) <= 0 {
            0x00ff_ffff
        } else {
            size << 1
        };
        m.sw(VLC_SIZE, limit);
        previous
    }

    /// 800AB6E8 DecDCTvlcBuild(table): unpack the decoding table from
    /// 800B9AB4 into `table` (codes below 0xF0 copy that many + 1 literal
    /// bytes, or repeat bytes from `distance` back after a distance code;
    /// 0xF0 ends, larger codes set the distance), then XOR each halfword of
    /// `table + 8 ..` with the one 8 bytes before, 0x87FC times.
    pub fn dec_dct_vlc_build(m: &mut Machine, table: u32) {
        let mut distance: u32 = 0;
        let mut source = VLC_PACKED;
        let mut out = table;
        loop {
            let code = u32::from(m.lbu(source));
            source = source.wrapping_add(1);
            if code < 0xf0 {
                let mut count = code as i32;
                if distance != 0 {
                    while count >= 0 {
                        let byte = m.lbu(out.wrapping_sub(distance));
                        count -= 1;
                        m.sb(out, byte);
                        out = out.wrapping_add(1);
                    }
                } else {
                    while count >= 0 {
                        let byte = m.lbu(source);
                        source = source.wrapping_add(1);
                        count -= 1;
                        m.sb(out, byte);
                        out = out.wrapping_add(1);
                    }
                }
            } else {
                distance = 0;
                if code != 0xf0 {
                    let low = u32::from(m.lbu(source));
                    source = source.wrapping_add(1);
                    distance = ((code << 8) | low).wrapping_add(0xffff_0f01);
                }
            }
            if distance == 0xf00 {
                break;
            }
        }
        let mut cursor = table.wrapping_add(8);
        let mut count: i32 = 4;
        loop {
            let value = m.lhu(cursor) ^ m.lhu(cursor.wrapping_sub(8));
            count += 1;
            m.sh(cursor, value);
            cursor = cursor.wrapping_add(2);
            if count > 0x87ff {
                break;
            }
        }
    }

    /// Bit reader state of [`dec_dct_vlc2`] (the original's registers).
    struct Vlc {
        /// a0: next bit-stream halfword.
        stream: u32,
        /// a1: next output halfword.
        out: u32,
        /// v0: bit buffer (left-aligned).
        bits: u32,
        /// v1: bits consumed from the next halfword (0..15).
        used: u32,
        /// t4: quantizer << 10.
        quant: u32,
        /// t5: block in the macroblock (1..6; 0 for version 2 streams).
        block: u32,
        /// t7/t8/t9: DC predictors Cr, Cb, Y.
        dc: [u32; 3],
    }

    impl Vlc {
        /// Consume `count` more bits (`used += count`), refilling one
        /// halfword when 16 are used.
        fn refill(&mut self, m: &mut Machine) {
            let full = self.used & 0x10;
            self.used &= 0xf;
            if full != 0 {
                let word = u32::from(m.lhu(self.stream));
                self.stream = self.stream.wrapping_add(2);
                self.bits |= word << (self.used & 31);
            }
        }
    }

    /// 800AB398 DecDCTvlc2(bs, buffer, table): decode the bit stream `bs`
    /// (a 0x0C-byte header: run-level size word, quantizer, version, then
    /// the bits) into MDEC run-level codes at `buffer` with the table
    /// [`dec_dct_vlc_build`] made: the header word, then per block the DC
    /// code (version 2: 10 bits; version 3: differential, per-component
    /// predictors, luma and chroma tables), AC codes (13-bit lookup at
    /// `table + 0x800`, long codes at `table + 0x10800`, escape 0x7C1F
    /// followed by a 16-bit code) and 0xFE00 per block. A 0x3FF/0x1FF DC
    /// ends the data: 0xFE00 fills the rest up to the header's size;
    /// returns 0. Reaching the [`dec_dct_vlc_size2`] limit saves the state
    /// (800B9A88) and returns 1; `bs` = 0 continues from it.
    pub fn dec_dct_vlc2(m: &mut Machine, bs: u32, buffer: u32, table: u32) -> u32 {
        let ac = table.wrapping_add(0x800);
        let long = ac.wrapping_add(0x1_0000);
        let size = m.lw(VLC_SIZE);
        let mut s;
        let limit;
        let mut at_dc;
        if bs == 0 {
            s = Vlc {
                stream: m.lw(VLC_STATE),
                out: m.lw(VLC_STATE + 4),
                bits: m.lw(VLC_STATE + 8),
                used: m.lw(VLC_STATE + 0xc),
                quant: m.lw(VLC_STATE + 0x10),
                block: m.lw(VLC_STATE + 0x14),
                dc: [
                    m.lw(VLC_STATE + 0x18),
                    m.lw(VLC_STATE + 0x1c),
                    m.lw(VLC_STATE + 0x20),
                ],
            };
            limit = s.out.wrapping_add(size.wrapping_add(size));
            at_dc = false;
        } else {
            limit = buffer.wrapping_add(size.wrapping_add(size));
            let header = m.lw(bs);
            let quant = u32::from(m.lhu(bs + 4)) << 10;
            let version = u32::from(m.lhu(bs + 6));
            let high = u32::from(m.lhu(bs + 8));
            let low = u32::from(m.lhu(bs + 0xa));
            s = Vlc {
                stream: bs.wrapping_add(0xc),
                out: buffer,
                bits: (high << 16) | low,
                used: 0,
                quant,
                block: u32::from((version.wrapping_sub(3) as i32) >= 0),
                dc: [0; 3],
            };
            m.sw(buffer, header);
            let end = ((header & 0xffff) << 2)
                .wrapping_add(4)
                .wrapping_add(buffer);
            m.sw(VLC_END, end);
            s.out = buffer.wrapping_add(2);
            at_dc = true;
        }
        loop {
            if at_dc {
                // 800AB45C: the DC code of the next block.
                let top = s.bits >> 22;
                if s.block != 0 {
                    s.out = s.out.wrapping_add(2);
                    if top ^ 0x3ff == 0 {
                        return vlc_finish(m, &s);
                    }
                    let base = if (s.block.wrapping_sub(3) as i32) < 0 {
                        table.wrapping_add(0x400)
                    } else {
                        table
                    };
                    let entry = ((s.bits >> 24) << 2).wrapping_add(base);
                    let length = u32::from(m.lhu(entry));
                    let width = u32::from(m.lhu(entry + 2));
                    let mut value: u32 = 0;
                    s.bits <<= length & 31;
                    if width != 0 {
                        let shift = 32u32.wrapping_sub(width);
                        value = s.bits >> (shift & 31);
                        let negative = (s.bits as i32) >= 0;
                        s.bits <<= width & 31;
                        if negative {
                            value = value.wrapping_sub(0xffff_ffff >> (shift & 31));
                        }
                        s.used = s.used.wrapping_add(width);
                    }
                    s.used = s.used.wrapping_add(length);
                    s.refill(m);
                    let component = s.block.wrapping_sub(2) as i32;
                    let slot = if component > 0 {
                        2
                    } else if component == 0 {
                        1
                    } else {
                        0
                    };
                    let dc = s.dc[slot].wrapping_add(value);
                    s.dc[slot] = dc;
                    let code = ((dc << 2) & 0x3ff) | s.quant;
                    s.block = s.block.wrapping_add(1);
                    m.sh(s.out, code as u16);
                    if s.block == 7 {
                        s.block = 1;
                    }
                } else {
                    s.out = s.out.wrapping_add(2);
                    if top ^ 0x1ff == 0 {
                        return vlc_finish(m, &s);
                    }
                    s.bits <<= 10;
                    s.used = s.used.wrapping_add(10);
                    s.refill(m);
                    m.sh(s.out, (s.quant | top) as u16);
                }
                // 800AB56C
                let past = s.out.wrapping_sub(limit) as i32 >= 0;
                s.out = s.out.wrapping_add(2);
                if past {
                    return vlc_save(m, &s);
                }
                at_dc = false;
            }
            // 800AB578: an AC code.
            let entry = ((s.bits >> 19) << 3).wrapping_add(ac);
            let mut first = m.lw(entry);
            let second;
            if first != 0 {
                second = m.lw(entry + 4);
            } else {
                s.bits <<= 8;
                s.used = s.used.wrapping_add(8);
                s.refill(m);
                let entry = ((s.bits >> 23) << 2).wrapping_add(long);
                first = m.lw(entry);
                second = 0;
            }
            let length = first & 0xff;
            s.bits <<= length & 31;
            s.used = s.used.wrapping_add(length);
            s.refill(m);
            let mut escape = false;
            let mut block_end = false;
            for (i, code) in [first >> 16, second & 0xffff, second >> 16]
                .into_iter()
                .enumerate()
            {
                if i == 1 && second == 0 {
                    break;
                }
                if i == 2 && code == 0 {
                    break;
                }
                if code ^ 0x7c1f == 0 {
                    escape = true;
                    break;
                }
                m.sh(s.out, code as u16);
                if code ^ 0xfe00 == 0 {
                    block_end = true;
                    break;
                }
                s.out = s.out.wrapping_add(2);
            }
            if escape {
                // 800AB65C: a 16-bit escape code.
                m.sh(s.out, (s.bits >> 16) as u16);
                s.out = s.out.wrapping_add(2);
                let word = u32::from(m.lhu(s.stream));
                s.stream = s.stream.wrapping_add(2);
                s.bits = (s.bits << 16) | (word << (s.used & 31));
            } else if block_end {
                at_dc = true;
            }
        }
    }

    /// 800AB680: fill 0xFE00 up to [`VLC_END`]; returns 0.
    fn vlc_finish(m: &mut Machine, s: &Vlc) -> u32 {
        let end = m.lw(VLC_END);
        let mut out = s.out;
        while (out.wrapping_sub(end) as i32) < 0 {
            m.sh(out, 0xfe00);
            out = out.wrapping_add(2);
        }
        0
    }

    /// 800AB6B0: save the decoder state for a continuation; returns 1.
    fn vlc_save(m: &mut Machine, s: &Vlc) -> u32 {
        m.sw(VLC_STATE, s.stream);
        m.sw(VLC_STATE + 4, s.out);
        m.sw(VLC_STATE + 8, s.bits);
        m.sw(VLC_STATE + 0xc, s.used);
        m.sw(VLC_STATE + 0x10, s.quant);
        m.sw(VLC_STATE + 0x14, s.block);
        m.sw(VLC_STATE + 0x18, s.dc[0]);
        m.sw(VLC_STATE + 0x1c, s.dc[1]);
        m.sw(VLC_STATE + 0x20, s.dc[2]);
        1
    }
}

// ---- indirect calls ---------------------------------------------------------------------

/// Stack arguments `+0x10..` of the caller's frame (`m.sp`).
fn stack_args<const N: usize>(m: &mut Machine) -> [u32; N] {
    let sp = m.sp;
    std::array::from_fn(|i| m.lw(sp.wrapping_add(0x10 + 4 * i as u32)))
}

/// Native targets owned by this module, by original address (`a0..a3`,
/// stack arguments at `sp+0x10..`; returns `v0`, 0 for routines whose
/// result no caller uses).
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let [a0, a1, a2, a3] = args;
    let mut call = dispatch::call;
    let c: &mut Invoker = &mut call;
    Some(match target {
        0x8009_9bbc => {
            module_read_complete(m);
            0
        }
        0x8009_9bc8 => module_load_begin(m, c, a0, a1),
        0x8009_9cd8 => module_load_poll(m, c),
        0x8002_c490 => queue_file_read(m, c, a0, a1, a2),
        0x8003_606c => {
            let [dx, dy, w, h] = stack_args(m);
            move_image_packet(m, a0, a1, a2, a3, dx, dy, w, h)
        }
        0x8005_63bc => {
            movie_music_loaded(m);
            0
        }
        0x8005_63c8 => movie_prepare(m, c),
        0x8005_6538 => movie_load_music(m, c),
        0x8005_65c0 => movie_setup(m, c),
        0x8005_67ec => movie_finish(m, c),
        0x8005_687c => movie_frame(m, c),
        0x8005_5dcc => movie_player(m, c),
        0x8005_5224 => movie_rect_zero(m),
        0x8005_5238 => movie_decoder_reset(m),
        0x8005_5290 => {
            movie_decoder_setup(m, a0);
            0
        }
        0x8005_53a4 => {
            movie_stream_done(m);
            0
        }
        0x8005_53b4 => {
            let stack = stack_args(m);
            movie_decoder_init(m, c, a0, a1, a2, a3, stack)
        }
        0x8005_5484 => {
            movie_stream_start(m, c);
            0
        }
        0x8005_54e4 => movie_wait_frame(m, c, a0),
        0x8005_5594 => movie_stop(m, c),
        0x8005_55c0 => movie_decode(m, c, a0),
        0x8005_578c => movie_sync(m, a0, a1),
        0x8005_5878 => {
            movie_slice_done(m, c);
            0
        }
        0x8005_59d0 => movie_next_frame(m, c, a0),
        0x8005_56e0 => movie_decode_ready(m, c, a0),
        0x8009_9dd4 => {
            context_read_complete(m);
            0
        }
        0x8002_b174 => stream_pause_time(m),
        0x8002_7168 => queue_text(m, a0, a1, a2),
        0x8002_7228 => queue_number(m, a0, a1, a2, a3),
        0x8005_9900 => picture_state_setup(m, c),
        0x8005_9adc => picture_state_frame(m, c),
        0x8005_9bb4 => picture_state_finish(m, c),
        0x8005_6950 => picture_player(m, c),
        0x8005_6c64 => {
            let layout = stack_args(m);
            picture_decoder_init(m, c, a0, a1, a2, a3, layout)
        }
        0x8005_6d04 => {
            picture_decoder_setup(m, a0);
            0
        }
        0x8005_6e58 => {
            picture_stream_start(m, c);
            0
        }
        0x8005_6eac => {
            picture_stream_done(m);
            0
        }
        0x8005_6ebc => picture_stop(m, c),
        0x8005_6ee0 => picture_bank_zero(m),
        0x8005_6ef4 => picture_wait_frame(m, c, a0),
        0x8005_6f90 => picture_next_chunk(m, c, a0),
        0x8005_7774 => picture_frame(m, c),
        0x8005_7938 => picture_layers(m, a0, a1, a2),
        0x8005_7ae0 => picture_tiles(m, a0, a1, a2),
        0x8005_7f14 => picture_blend(m, a0, a1, a2),
        0x8005_8230 => picture_tint(m, a0, a1, a2),
        0x8005_8474 => picture_zoom(m, a0, a1, a2),
        0x8005_868c => picture_wipe(m, a0, a1, a2),
        0x8005_891c => picture_border(m, a0, a1),
        0x8008_3084 => picture_noise(m, c, a0, a1, a2, a3),
        0x800a_acd8 => pending_sdk::dec_dct_reset(m, a0),
        0x800a_ae30 => pending_sdk::dec_dct_in(m, a0, a1),
        0x800a_aeac => pending_sdk::dec_dct_out(m, a0, a1),
        0x800a_b368 => pending_sdk::dec_dct_vlc_size2(m, a0),
        0x800a_c2d8 => pending_sdk::strcpy(m, a0, a1),
        0x800a_ab98 => pending_sdk::get_tpage(a0, a1, a2, a3),
        0x800a_b398 => pending_sdk::dec_dct_vlc2(m, a0, a1, a2),
        0x800a_b6e8 => {
            pending_sdk::dec_dct_vlc_build(m, a0);
            0
        }
        _ => return None,
    })
}
