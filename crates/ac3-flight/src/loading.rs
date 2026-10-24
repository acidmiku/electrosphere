//! Resident state-2 loading (substates 0..5) on the address-preserving machine.
//!
//! The five dispatcher callbacks and everything they reach are translated
//! from the original instructions:
//!
//! | Callback | Substate | Rust |
//! | --- | --- | --- |
//! | `800355F0` | 0 | [`initialize`]: CD queue reset, sound reset, queue mission file |
//! | `800354B8` | 1, 4 | [`poll`]: progress, loading screen, completion test |
//! | `80035730` | 2 | [`prepare_mission`]: sound bank, textures, expansion, relocation, palettes |
//! | `80035C10` | 3 | [`queue_aircraft`] |
//! | `80035C90` | 5 | [`prepare_aircraft`] |
//!
//! Devices are never emulated. CD reads are the game's own RAM queue
//! (`[gp+0x48]`); the queueing routine logs each request as a host call and
//! [`complete_cd_read`] is the host side that delivers the file bytes and the
//! completion state. GPU transfers (`LoadImage`/`StoreImage`), SPU transfers,
//! SPU register updates and BIOS event calls are the `sdk.rs` routines (host
//! calls plus their RAM bookkeeping); the queue resets 8002C280/8002C6A8,
//! 800270FC and the sound reset tree are in `shared.rs`.
//! Every translated routine is claimed by [`dispatch`] with its original
//! `a0..a3` and `v0`; routines with stack arguments read them at `sp+0x10..`.
use crate::{
    arena,
    common::{CONTEXT, RESOURCES},
    dispatch,
    machine::{Fault, Machine, SmallData},
    sdk, shared,
};
use ac3_gte::{ctrl, reg};

/// Result a loading callback returns to advance the resident substate.
pub const ADVANCE: u32 = 0x0400_0000;
/// 800354AC: CD completion callback stored in the queued read requests.
pub const READ_COMPLETE: u32 = 0x8003_54ac;

/// `[gp+0x48]`: CD request queue. +0 head, +4 tail, +8 sixteen 0x14-byte
/// entries, +0x148/+0x14C pending control request, +0x168 driver done flag,
/// +0x16C driver status, +0x174 active file, +0x17C/+0x180 read extent.
const CD_QUEUE: u32 = 0x48;
/// `[gp+0x34]`: file system state; +0x18/+0x1C point at the BPB and stream
/// file tables (8-byte records: size/flags word, sector word).
const FILE_SYSTEM: u32 = 0x34;
/// `[gp+0x38]`: CD read driver; +0x14 words left, +0x18 current destination.
const CD_DRIVER: u32 = 0x38;
/// `[gp+0x150]`: the 60-byte loading state (frame, progress, mode, stage, ...).
const LOADING: u32 = 0x150;
/// `[gp+0x2C4]`: render status block (flags, phase, background, parameter).
const RENDER: u32 = 0x2c4;

const MISSION_FILES: u32 = 0x800a_f748;
const AIRCRAFT_FILES: u32 = 0x800a_f79c;
const REPLAY_PAIRS: u32 = 0x800a_f7c8;
const MISSION_MODES: u32 = 0x800a_f808;
const MISSION_NAMES: u32 = 0x800a_f844;
const LOCALIZED_TEXT: u32 = 0x800a_f6dc;
const DIFFICULTY_LABEL: u32 = 0x800a_f710;
const GLYPHS: u32 = 0x800a_ee88;
const DIFFICULTY_TEXT: u32 = 0x800b_ab40;
const NAME_PREFIX: u32 = 0x800b_ab7c;
const APERTURE_SIZES: u32 = 0x8002_3948;
const SOUND_DESCRIPTORS: u32 = 0x800c_37d8;
const REVERB_MODE: u32 = 0x800c_43bc;
const PALETTE_SOURCE: u32 = 0x800b_ac38;
const DECOMPRESSORS: u32 = 0x800a_e140;
/// Top of the resident arena; compressed prefixes are staged below it.
const ARENA_TOP: u32 = 0x801f_b700;

const fn off(base: u32, offset: u32) -> u32 {
    base.wrapping_add(offset)
}
/// `lw t, 0(p); lw t, 0(t)`: the word a stored pointer points at.
fn word_at(m: &mut Machine, pointer: u32) -> u32 {
    let address = m.lw(pointer);
    m.lw(address)
}
fn sext16(value: i16) -> u32 {
    value as i32 as u32
}
/// Run `body` inside an original stack frame of `size` bytes (its base is the
/// callee's `sp`), so stack addresses passed down match the original.
fn framed<R>(m: &mut Machine, size: u32, body: impl FnOnce(&mut Machine, u32) -> R) -> R {
    let sp = m.push_frame(size);
    let result = body(m, sp);
    m.pop_frame(size);
    result
}
/// `lwl rt, address` (little-endian): the bytes from `address` down to its
/// word boundary replace the high end of `rt`.
fn lwl(m: &mut Machine, rt: u32, address: u32) -> u32 {
    let shift = (address & 3) * 8;
    let word = m.lw(address & !3);
    (rt & 0x00ff_ffffu32.wrapping_shr(shift)) | word.wrapping_shl(24 - shift)
}
/// `swl rt, address` (little-endian): the high end of `rt` replaces the bytes
/// from `address` down to its word boundary.
fn swl(m: &mut Machine, rt: u32, address: u32) {
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
fn stack_args<const N: usize>(m: &mut Machine) -> [u32; N] {
    let sp = m.sp;
    std::array::from_fn(|i| m.lw(off(sp, 0x10 + 4 * i as u32)))
}

// ---- dispatcher callbacks -------------------------------------------------------

/// 800355F0 (state 2, substate 0): reset the CD queue and sound, apply a
/// replay selection, clear the exit fields, queue the mission file into a new
/// arena allocation, set render flag 1, select the visual mode and poll once.
pub fn initialize(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        shared::reset_cd_queue(m, 0, 0);
        reset_sound(m);
        let context = m.gw(CONTEXT);
        if m.lbu(off(context, 0x3a9)) != 0 {
            select_replay(m);
        }
        let context = m.gw(CONTEXT);
        m.sb(off(context, 0x95b), 0);
        let context = m.gw(CONTEXT);
        let mission = sext16(m.lh(off(context, 0x390)));
        m.sh(off(context, 0x95c), 0);
        m.sh(off(context, 0x3b0), 0);
        let entry = off(mission << 1, MISSION_FILES);
        let resources = m.gw(RESOURCES);
        let file = sext16(m.lh(entry));
        m.sw(off(resources, 0x60), file);
        let size = total_size(m, off(resources, 0x60), 1);
        let destination = arena::alloc(m, size);
        let list = off(m.gw(RESOURCES), 0x60);
        let state = m.gw(LOADING);
        m.sw(off(state, 8), 1);
        queue_files(m, list, 1, destination, READ_COMPLETE);
        let render = m.gw(RENDER);
        let state = m.gw(LOADING);
        let flags = m.lw(render);
        m.sw(off(render, 4), 0);
        m.sw(off(render, 0x14), 0);
        m.sw(render, flags | 1);
        m.sh(state, 0);
        let mode = m.lbu(off(mission, MISSION_MODES));
        m.sb(off(state, 4), mode);
        let context = m.gw(CONTEXT);
        if m.lbu(off(context, 0x3a9)) != 0 {
            let state = m.gw(LOADING);
            m.sb(off(state, 4), 1);
        }
        let state = m.gw(LOADING);
        m.sb(off(state, 6), 0);
        let state = m.gw(LOADING);
        m.sh(off(state, 2), 0);
        poll(m);
        ADVANCE
    })
}

/// 800354B8 (substates 1 and 4): while the mission (stage 1) or aircraft
/// (stage 2) file is the active read, store its stage and 0..4096 progress;
/// initialize the screen on frame 0, fade on frames 1-2, draw from frame 3,
/// advance the frame counter and return [`ADVANCE`] once the read is no
/// longer pending.
pub fn poll(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let active = shared::current_cd_file(m);
        let context = m.gw(CONTEXT);
        let mission = sext16(m.lh(off(context, 0x390)));
        let mission_file = sext16(m.lh(off(mission << 1, MISSION_FILES)));
        let stage = if active == mission_file {
            Some(1)
        } else {
            let aircraft = sext16(m.lh(off(context, 0x392)));
            let aircraft_file = sext16(m.lh(off(aircraft << 1, AIRCRAFT_FILES)));
            (active == aircraft_file).then_some(2)
        };
        if let Some(stage) = stage {
            let state = m.gw(LOADING);
            m.sb(off(state, 6), stage);
            let progress = read_progress(m, 0);
            let state = m.gw(LOADING);
            m.sh(off(state, 2), progress as u16);
        }
        let state = m.gw(LOADING);
        if m.lh(state) <= 0 {
            initialize_screen(m);
        }
        let state = m.gw(LOADING);
        if u32::from(m.lhu(state)).wrapping_sub(1) < 2 {
            let context = m.gw(CONTEXT);
            let ot = m.lw(off(context, 0xc));
            let primitives = m.lw(off(context, 0x14));
            let next = fade(m, ot, primitives, 0);
            let context = m.gw(CONTEXT);
            m.sw(off(context, 0x14), next);
        }
        let state = m.gw(LOADING);
        if m.lh(state) >= 3 {
            draw_screen(m);
        }
        let state = m.gw(LOADING);
        let pending = m.lw(off(state, 8));
        let frame = m.lhu(state);
        m.sh(state, frame.wrapping_add(1));
        u32::from(pending == 0) << 26
    })
}

/// 80035730 (substate 2): prepare the loaded mission resource (17-child
/// directory): install sound child 16, upload texture children 15..11 (each
/// expanded over the following, already consumed child), stage the
/// compressed prefix below [`ARENA_TOP`], expand children 0..10 after the
/// directory, bind the resource fields, relocate the record directories,
/// alias object slots 96 onward, build the eight palettes, set the arena end
/// and poll once.
pub fn prepare_mission(m: &mut Machine) -> u32 {
    framed(m, 0x28, |m, _| {
        let resources = m.gw(RESOURCES);
        let raw = m.lw(off(resources, 0x60));
        let directory = relocate_directory(m, raw);
        let bank = m.lw(off(directory, 0x40));
        install_sound_bank(m, bank);
        for child in (11..16).rev() {
            let source = m.lw(off(directory, child * 4));
            let target = m.lw(off(directory, child * 4 + 4));
            upload_texture_group(m, source, target);
            sdk::draw_sync(m, 0);
        }
        let end = m.lw(off(directory, 0x2c));
        let words = ((end.wrapping_sub(directory) as i32) >> 2) as u32;
        let staged = ARENA_TOP.wrapping_sub(words << 2);
        let delta = (((staged.wrapping_sub(directory)) as i32) >> 2) as u32;
        shared::copy_words(m, staged, directory, words);
        let delta = delta << 2;
        for slot in 0..11 {
            let pointer = m.lw(off(staged, slot * 4));
            m.sw(off(staged, slot * 4), pointer.wrapping_add(delta));
        }
        let mut output = off(directory, 0x44);
        m.sw(directory, output);
        for child in 0..11 {
            let source = m.lw(off(staged, child * 4));
            output = decompress(m, source, output);
            if child < 10 {
                m.sw(off(directory, child * 4 + 4), output);
            }
        }
        let first = m.lw(directory);
        m.sw(off(directory, 0x2c), output);
        m.sw(off(resources, 0x18), first);
        let context = m.gw(CONTEXT);
        let section3 = m.lw(off(directory, 0xc));
        let count = m.lw(section3);
        m.sw(off(context, 0xf80), count);
        let section5 = m.lw(off(directory, 0x14));
        let count = m.lw(section5);
        m.sw(off(context, 0xf88), count);
        let fields = off(resources, 0x18);
        for (field, child) in [(4, 1), (0x20, 8), (0x18, 6), (0x1c, 7)] {
            let value = m.lw(off(directory, child * 4));
            m.sw(off(fields, field), value);
        }
        let section = m.lw(off(directory, 8));
        let table = relocate_directory(m, section);
        m.sw(off(fields, 8), table.wrapping_sub(4));
        for (field, child) in [(0xc, 3), (0x10, 4), (0x14, 5), (0x24, 9), (0x28, 10)] {
            let section = m.lw(off(directory, child * 4));
            let table = relocate_directory(m, section);
            m.sw(off(fields, field), table);
        }
        // Section 2: the stored table points at the count word (index 1..=n).
        let count = word_at(m, off(directory, 8));
        if count as i32 > 0 {
            let mut index = 1u32;
            loop {
                let table = m.lw(off(fields, 8));
                let record = m.lw(off(table, index << 2));
                relocate_fields(m, record, off(record, 4), 4);
                let count = word_at(m, off(directory, 8));
                index = index.wrapping_add(1);
                if (count as i32) < (index as i32) {
                    break;
                }
            }
        }
        for (child, field, width) in [(3, 0xc, 6), (4, 0x10, 6), (5, 0x14, 4)] {
            relocate_records(m, off(directory, child * 4), off(fields, field), width);
        }
        // Mission section 4 records replace section 3 slots 96 onward.
        let count = word_at(m, off(directory, 0x10));
        if count as i32 > 0 {
            let mut index = 0u32;
            loop {
                let source = m.lw(off(fields, 0x10));
                let target = m.lw(off(fields, 0xc));
                let record = m.lw(off(index << 2, source));
                m.sw(off(off(index << 2, target), 0x180), record);
                let count = word_at(m, off(directory, 0x10));
                index = index.wrapping_add(1);
                if (index as i32) >= (count as i32) {
                    break;
                }
            }
        }
        let section0 = m.lw(fields);
        let patches = m.lw(off(section0, 0x74));
        prepare_palettes(m, off(section0, 8), off(section0, 0x78), patches);
        let end = m.lw(off(directory, 0x2c));
        arena::reset(m, end);
        poll(m);
        ADVANCE
    })
}

/// Shared loop shape of 80035730/80035C90: relocate `width` fields of every
/// record listed by the directory whose count word is `[count_slot]`, using
/// the stored table pointer at `table_field`.
fn relocate_records(m: &mut Machine, count_slot: u32, table_field: u32, width: u32) {
    let count = word_at(m, count_slot);
    if count as i32 <= 0 {
        return;
    }
    let mut index = 0u32;
    loop {
        let table = m.lw(table_field);
        let record = m.lw(off(index << 2, table));
        relocate_fields(m, record, off(record, 4), width);
        let count = word_at(m, count_slot);
        index = index.wrapping_add(1);
        if (index as i32) >= (count as i32) {
            break;
        }
    }
}

/// 80035C10 (substate 3): queue the selected aircraft file into a new arena
/// allocation and poll once.
pub fn queue_aircraft(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let context = m.gw(CONTEXT);
        let aircraft = sext16(m.lh(off(context, 0x392)));
        let entry = off(aircraft << 1, AIRCRAFT_FILES);
        let resources = m.gw(RESOURCES);
        let file = sext16(m.lh(entry));
        m.sw(off(resources, 0x60), file);
        let size = total_size(m, off(resources, 0x60), 1);
        let destination = arena::alloc(m, size);
        let list = off(m.gw(RESOURCES), 0x60);
        let state = m.gw(LOADING);
        m.sw(off(state, 8), 1);
        queue_files(m, list, 1, destination, READ_COMPLETE);
        poll(m);
        ADVANCE
    })
}

/// 80035C90 (substate 5): prepare the loaded aircraft resource (4-child
/// directory): sound child 3, texture child 2, expansion of children 0/1,
/// record relocation, context+0xF84, resource +0x08/+0x0C/+0x5C, arena end,
/// one poll, then render phase/parameter 0 and flags |= 5.
pub fn prepare_aircraft(m: &mut Machine) -> u32 {
    framed(m, 0x28, |m, _| {
        let resources = m.gw(RESOURCES);
        let raw = m.lw(off(resources, 0x60));
        let directory = relocate_directory(m, raw);
        let bank = m.lw(off(directory, 0xc));
        install_sound_bank(m, bank);
        let source = m.lw(off(directory, 8));
        let target = m.lw(off(directory, 0xc));
        upload_texture_group(m, source, target);
        sdk::draw_sync(m, 0);
        let end = m.lw(off(directory, 8));
        let words = ((end.wrapping_sub(directory) as i32) >> 2) as u32;
        let staged = ARENA_TOP.wrapping_sub(words << 2);
        let delta = (((staged.wrapping_sub(directory)) as i32) >> 2) as u32;
        shared::copy_words(m, staged, directory, words);
        let delta = delta << 2;
        let first = m.lw(staged).wrapping_add(delta);
        let second = m.lw(off(staged, 4)).wrapping_add(delta);
        m.sw(staged, first);
        m.sw(off(staged, 4), second);
        m.sw(directory, off(directory, 0x10));
        let source = m.lw(staged);
        let output = decompress(m, source, off(directory, 0x10));
        m.sw(off(directory, 4), output);
        let source = m.lw(off(staged, 4));
        let output = decompress(m, source, output);
        let section0 = m.lw(directory);
        m.sw(off(directory, 8), output);
        let table = relocate_directory(m, section0);
        m.sw(off(resources, 8), table);
        let fields = off(resources, 8);
        relocate_records(m, directory, fields, 6);
        let context = m.gw(CONTEXT);
        let count = word_at(m, directory);
        m.sw(off(context, 0xf84), count);
        let section1 = m.lw(off(directory, 4));
        m.sw(off(fields, 4), section1);
        let end = m.lw(off(directory, 8));
        arena::reset(m, end);
        let top = arena::peek(m, 0);
        let resources = m.gw(RESOURCES);
        m.sw(off(resources, 0x5c), top);
        poll(m);
        let render = m.gw(RENDER);
        let flags = m.lw(render);
        m.sw(off(render, 4), 0);
        m.sw(off(render, 0x14), 0);
        m.sw(render, flags | 5);
        ADVANCE
    })
}

/// 800354AC: CD completion callback; clears the loading state's pending word.
pub fn read_complete(m: &mut Machine) {
    let state = m.gw(LOADING);
    m.sw(off(state, 8), 0);
}

/// 80035E28: replay mode takes mission/aircraft from the replay pair table
/// (an index of 16 or more restarts at 1).
pub fn select_replay(m: &mut Machine) {
    let context = m.gw(CONTEXT);
    if m.lbu(off(context, 0x3aa)) >= 16 {
        m.sb(off(context, 0x3aa), 1);
    }
    let context = m.gw(CONTEXT);
    let pair = off(u32::from(m.lbu(off(context, 0x3aa))) << 2, REPLAY_PAIRS);
    let mission = m.lbu(off(pair, 2));
    m.sh(off(context, 0x390), mission.into());
    let aircraft = m.lbu(off(pair, 3));
    m.sh(off(context, 0x392), aircraft.into());
}

// ---- CD request queue ---------------------------------------------------------

/// 8002953C: file table record of `file` (bit 11 selects the stream table).
pub fn file_entry(m: &mut Machine, file: u32) -> u32 {
    let index = file & 0x7ff;
    let system = m.gw(FILE_SYSTEM);
    let table = if file & 0x800 == 0 {
        m.lw(off(system, 0x18))
    } else {
        m.lw(off(system, 0x1c))
    };
    off(table, index << 3)
}

/// 8002C1B8: file size in bytes (BPB: words * 4; stream: units << scale).
pub fn file_size(m: &mut Machine, file: u32) -> u32 {
    framed(m, 0x18, |m, _| {
        let entry = file_entry(m, file);
        let word = m.lw(entry);
        if file & 0x800 == 0 {
            (word & 0x7_ffff) << 2
        } else {
            (word & 0x7_ffff).wrapping_shl((word >> 24) & 7)
        }
    })
}

/// 8002C214: summed size of `count` files listed at `list`.
pub fn total_size(m: &mut Machine, list: u32, count: u32) -> u32 {
    framed(m, 0x28, |m, _| {
        let mut total = 0u32;
        let mut index = 0i32;
        let mut cursor = list;
        while index < count as i32 {
            let file = m.lw(cursor);
            cursor = off(cursor, 4);
            index += 1;
            total = total.wrapping_add(file_size(m, file));
        }
        total
    })
}

/// 8002C384: append a type-2 read request (`file` to `destination`, then
/// `callback`) and record the read extent at queue +0x17C/+0x180. Returns the
/// word-rounded end. Logged as a host call `[file, destination, callback,
/// size]` for the CD bridge ([`complete_cd_read`]).
pub fn queue_read(m: &mut Machine, file: u32, destination: u32, callback: u32) -> u32 {
    framed(m, 0x18, |m, _| {
        let queue = m.gw(CD_QUEUE);
        let tail = m.lw(off(queue, 4));
        m.sw(off(queue, 0x17c), destination);
        m.sw(off(queue, 4), tail.wrapping_add(1));
        let entry = off(queue, (tail & 0xf) * 0x14 + 8);
        m.sb(entry, 2);
        m.sh(off(entry, 2), file as u16);
        m.sw(off(entry, 4), destination);
        m.sw(off(entry, 0x10), callback);
        m.sb(off(entry, 1), 0);
        let size = file_size(m, file);
        m.host_call(0x8002_c384, [file, destination, callback, size]);
        let end = destination.wrapping_add((((size as i32) >> 2) << 2) as u32);
        let queue = m.gw(CD_QUEUE);
        m.sw(off(queue, 0x180), end);
        end
    })
}

/// 8002C404: queue `count` files from `list` back to back starting at
/// `destination`; each list slot is replaced by its destination address and
/// only the last request carries `callback`. Returns the last end.
pub fn queue_files(m: &mut Machine, list: u32, count: u32, destination: u32, callback: u32) -> u32 {
    framed(m, 0x28, |m, _| {
        let mut cursor = list;
        let mut destination = destination;
        let leading = count.wrapping_sub(1) as i32;
        let mut index = 0i32;
        while index < leading {
            let file = m.lw(cursor);
            m.sw(cursor, destination);
            cursor = off(cursor, 4);
            index += 1;
            destination = queue_read(m, file, destination, 0);
        }
        let file = m.lw(cursor);
        m.sw(cursor, destination);
        queue_read(m, file, destination, callback)
    })
}

/// 8002B1D4: the CD driver's current destination pointer.
pub fn read_position(m: &mut Machine) -> u32 {
    let driver = m.gw(CD_DRIVER);
    m.lw(off(driver, 0x18))
}

/// 8002C604: progress of the current read extent, 0..=0x1000; stores the
/// current position at `position` when it is not null.
pub fn read_progress(m: &mut Machine, position: u32) -> u32 {
    framed(m, 0x20, |m, _| {
        let queue = m.gw(CD_QUEUE);
        let start = m.lw(off(queue, 0x17c));
        let end = m.lw(off(queue, 0x180));
        let current = read_position(m);
        if position != 0 {
            m.sw(position, current);
        }
        let done = current.wrapping_sub(start) >> 2;
        let total = end.wrapping_sub(start) >> 2;
        // divu with a zero divisor is skipped by the original (progress 0).
        let mut progress = (done << 12).checked_div(total).unwrap_or(0);
        if (progress as i32) < 0 {
            progress = 0;
        }
        if (progress as i32) >= 0x1001 {
            progress = 0x1000;
        }
        progress
    })
}

// ---- host side of CD reads ----------------------------------------------------------

/// A CD read the original queued (8002C384) at the queue head.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CdRead {
    /// Queue entry address.
    pub entry: u32,
    /// Resident file ID (ACE.BPH index).
    pub file: u32,
    /// Absolute disc sector, as the resident file table holds it.
    pub sector: u32,
    /// First disc sector of ACE.BPB (file system state +0x10); the file
    /// starts at ACE.BPB offset `(sector - archive_sector) * 2048`.
    pub archive_sector: u32,
    /// Bytes (file table words * 4).
    pub size: u32,
    pub destination: u32,
    pub callback: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CdReadError {
    /// The queue head has no request.
    Idle,
    /// The head request is not a file read (type 2).
    NotFileRead {
        kind: u8,
    },
    /// The pending control request is not the reset (type 1).
    UnsupportedControl {
        kind: u8,
    },
    /// Stream files (ID bit 11) are not ACE.BPB extents.
    StreamFile {
        file: u32,
    },
    /// The extent lies outside the supplied ACE.BPB body.
    OutsideBody {
        offset: u64,
        size: u32,
    },
    /// The file starts before ACE.BPB.
    BeforeArchive {
        sector: u32,
        archive_sector: u32,
    },
    Fault(Fault),
}
impl std::fmt::Display for CdReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for CdReadError {}

/// The read request at the head of the CD queue, as the original stored it.
pub fn pending_cd_read(m: &mut Machine) -> Result<CdRead, CdReadError> {
    let queue = m.gw(CD_QUEUE);
    let head = m.lw(queue);
    let tail = m.lw(off(queue, 4));
    if (head as i32) >= (tail as i32) {
        return Err(CdReadError::Idle);
    }
    let entry = off(queue, (head & 0xf) * 0x14 + 8);
    let kind = m.lbu(entry);
    if kind != 2 {
        return Err(CdReadError::NotFileRead { kind });
    }
    let file = sext16(m.lh(off(entry, 2)));
    if file & 0x800 != 0 {
        return Err(CdReadError::StreamFile { file });
    }
    let record = file_entry(m, file);
    let size = file_size(m, file);
    let sector = m.lw(off(record, 4));
    let system = m.gw(FILE_SYSTEM);
    let archive_sector = m.lw(off(system, 0x10));
    let destination = m.lw(off(entry, 4));
    let callback = m.lw(off(entry, 0x10));
    m.take_fault().map_err(CdReadError::Fault)?;
    Ok(CdRead {
        entry,
        file,
        sector,
        archive_sector,
        size,
        destination,
        callback,
    })
}

/// Host side of the queued read: what the main-loop CD service (8002C0B4 and
/// its type-1/type-2 handlers 8002B9A4/8002B9EC) and the CD driver leave in
/// RAM once the data has arrived. A pending reset control request is retired
/// first (active file -1, +0x178 cleared, request flag cleared). Then the
/// file bytes `ACE.BPB[(sector - archive_sector) * 2048..][..size]` (the
/// resident table holds absolute disc sectors) are written to the
/// destination, the driver position reaches the extent end with no words
/// left, the handler marks the entry started, the driver's completion
/// callback (8002B988) stores done = 1 and status 0, the active file becomes
/// -1, the request callback runs with the status (800354AC clears the
/// loading state's pending word) and the queue head advances.
pub fn complete_cd_read(m: &mut Machine, bpb: &[u8]) -> Result<CdRead, CdReadError> {
    let queue = m.gw(CD_QUEUE);
    if m.lw(off(queue, 0x148)) != 0 {
        let kind = m.lbu(off(queue, 0x14c));
        if kind != 1 {
            return Err(CdReadError::UnsupportedControl { kind });
        }
        m.sw(off(queue, 0x174), u32::MAX);
        m.sw(off(queue, 0x178), 0);
        m.sw(off(queue, 0x148), 0);
    }
    let read = pending_cd_read(m)?;
    let relative =
        read.sector
            .checked_sub(read.archive_sector)
            .ok_or(CdReadError::BeforeArchive {
                sector: read.sector,
                archive_sector: read.archive_sector,
            })?;
    let offset = u64::from(relative) * 2048;
    let bytes = usize::try_from(offset)
        .ok()
        .and_then(|start| bpb.get(start..start.checked_add(read.size as usize)?))
        .ok_or(CdReadError::OutsideBody {
            offset,
            size: read.size,
        })?;
    m.write_bytes(read.destination, bytes);
    let driver = m.gw(CD_DRIVER);
    m.sw(off(driver, 0x14), 0);
    m.sw(off(driver, 0x18), read.destination.wrapping_add(read.size));
    m.sb(off(read.entry, 1), 1);
    let queue = m.gw(CD_QUEUE);
    m.sw(off(queue, 0x168), 1);
    m.sw(off(queue, 0x16c), 0);
    m.sw(off(queue, 0x174), u32::MAX);
    if read.callback != 0 {
        let status = m.lw(off(queue, 0x16c));
        dispatch::call(m, read.callback, [status, 0, 0, 0]);
    }
    let queue = m.gw(CD_QUEUE);
    let head = m.lw(queue);
    m.sw(queue, head.wrapping_add(1));
    m.take_fault().map_err(CdReadError::Fault)?;
    Ok(read)
}

// ---- sound ----------------------------------------------------------------------------

/// 8002D6CC: reverb depth 0 (SpuSetReverbDepth with mask 6), reverb off,
/// reverb off for every voice, and clear the reverb work area of the
/// configured mode (800C43BC).
pub fn reset_sound(m: &mut Machine) {
    framed(m, 0x30, |m, sp| {
        let attributes = off(sp, 0x10);
        m.sw(attributes, 6);
        m.sh(off(sp, 0x18), 0);
        m.sh(off(sp, 0x1a), 0);
        sdk::spu_set_reverb_depth(m, attributes);
        sdk::spu_set_reverb(m, 0);
        sdk::spu_set_reverb_voice(m, 0, 0x00ff_ffff);
        let mode = m.lw(REVERB_MODE);
        sdk::spu_clear_reverb_work_area(m, mode);
    })
}

/// 8002D544: install a bank's 20-byte sample descriptors (first ID +0,
/// count +1) at 800C37D8 and upload its samples (SPU address +4, size +8,
/// data at word offset +2). Returns the transfer completion result.
pub fn install_sound_bank(m: &mut Machine, bank: u32) -> u32 {
    framed(m, 0x18, |m, _| {
        let mut index = 0u32;
        let mut source = off(bank, 0xc);
        if m.lbu(off(bank, 1)) != 0 {
            loop {
                let id = u32::from(m.lbu(bank)).wrapping_add(index);
                let target = off(id.wrapping_mul(20), SOUND_DESCRIPTORS);
                let words: [u32; 4] = std::array::from_fn(|i| m.lw(off(source, 4 * i as u32)));
                for (i, word) in words.into_iter().enumerate() {
                    m.sw(off(target, 4 * i as u32), word);
                }
                let last = m.lw(off(source, 0x10));
                m.sw(off(target, 0x10), last);
                index = index.wrapping_add(1);
                source = off(source, 0x14);
                if (index as i32) >= i32::from(m.lbu(off(bank, 1))) {
                    break;
                }
            }
        }
        let header = sext16(m.lh(off(bank, 2)));
        let address = m.lw(off(bank, 4));
        let size = m.lw(off(bank, 8));
        upload_samples(m, address, off(bank, header << 2), size)
    })
}

/// 8002D4E8: I/O-mode SPU upload of `size` bytes from `source` to SPU byte
/// address `address`; returns SpuIsTransferCompleted(1).
pub fn upload_samples(m: &mut Machine, address: u32, source: u32, size: u32) -> u32 {
    framed(m, 0x20, |m, _| {
        sdk::spu_set_transfer_mode(m, 1);
        sdk::spu_set_transfer_start_addr(m, address);
        sdk::spu_write(m, source, size);
        sdk::spu_is_transfer_completed(m, 1)
    })
}

// ---- resource preparation -----------------------------------------------------------------

/// 80026E98: relocate a directory in place. The halfword count at +0 is
/// followed by offsets; each without bit 31 gains the directory address.
/// Returns the first slot (`directory + 4`).
pub fn relocate_directory(m: &mut Machine, directory: u32) -> u32 {
    let count = i32::from(m.lhu(directory));
    let mut slot = off(directory, 4);
    for _ in 0..count {
        let value = m.lw(slot);
        if value & 0x8000_0000 == 0 {
            m.sw(slot, value.wrapping_add(directory));
        }
        slot = off(slot, 4);
    }
    off(directory, 4)
}

/// 80026EE0: relocate `count` relative words at `fields` against `base`
/// (bit 31 marks an encoded value). Returns `base`.
pub fn relocate_fields(m: &mut Machine, base: u32, fields: u32, count: u32) -> u32 {
    let mut slot = fields;
    for _ in 0..(count as i32).max(0) {
        let value = m.lw(slot);
        if value & 0x8000_0000 == 0 {
            m.sw(slot, value.wrapping_add(base));
        }
        slot = off(slot, 4);
    }
    base
}

/// 80026E50: expand a texture group to `target`, relocate its directory and
/// upload every TIM; returns the expansion end.
pub fn upload_texture_group(m: &mut Machine, source: u32, target: u32) -> u32 {
    framed(m, 0x20, |m, _| {
        let end = decompress(m, source, target);
        let list = relocate_directory(m, target);
        let count = m.lw(target);
        upload_tims(m, list, count);
        end
    })
}

/// 80026D8C: OpenTIM/ReadTIM each listed TIM and LoadImage its pixels, then
/// its CLUT. The TIM_IMAGE lives in this frame at `sp+0x10`.
pub fn upload_tims(m: &mut Machine, list: u32, count: u32) {
    framed(m, 0x38, |m, sp| {
        let image = off(sp, 0x10);
        let mut cursor = list;
        for _ in 0..(count as i32).max(0) {
            let tim = m.lw(cursor);
            if sdk::open_tim(m, tim) != 0 {
                let tim = m.lw(cursor);
                sdk::printf(m, [0x8001_001c, tim, 0, 0]);
            }
            if sdk::read_tim(m, image) == 0 {
                let tim = m.lw(cursor);
                sdk::printf(m, [0x8001_0034, tim, 0, 0]);
            }
            let pixels = m.lw(off(image, 0x10));
            if pixels != 0 {
                let rect = m.lw(off(image, 0xc));
                sdk::load_image(m, rect, pixels);
            }
            let clut = m.lw(off(image, 8));
            if clut != 0 {
                let rect = m.lw(off(image, 4));
                sdk::load_image(m, rect, clut);
            }
            cursor = off(cursor, 4);
        }
    })
}

/// 800AE118: expand an AC3 LZ block at `source` to `target` and return the
/// output end. Header: +7 kind (table 800AE140), +0xB offset bits,
/// +8/+0xC literal and back-reference stream offsets, flag words from +0x10.
/// Kinds: 0 bytes until a zero flag word; 1 halfwords; 2 byte count at +4.
pub fn decompress(m: &mut Machine, source: u32, target: u32) -> u32 {
    let kind = u32::from(m.lbu(off(source, 7)));
    let bits = u32::from(m.lbu(off(source, 0xb)));
    let handler = m.lw(off(DECOMPRESSORS, kind << 2));
    let literals = off(m.lw(off(source, 8)) & 0x00ff_ffff, source);
    let pairs = off(m.lw(off(source, 0xc)) & 0x00ff_ffff, source);
    let flags = off(source, 0x10);
    match handler {
        0x800a_e150 => expand_bytes(m, flags, literals, pairs, bits, target),
        0x800a_e1f4 => expand_halfwords(m, flags, literals, pairs, bits, target),
        0x800a_e2a0 => {
            let count = m.lw(off(source, 4)) & 0x00ff_ffff;
            expand_counted(m, flags, literals, pairs, bits, count, target)
        }
        address => {
            m.raise(Fault::Untranslated { address });
            target
        }
    }
}

/// 800AE150: flag words MSB first, each terminated by its lowest set bit;
/// a set bit copies one literal byte, a clear bit a back-reference pair
/// (`offset + 1`, `length + 3` bytes).
fn expand_bytes(
    m: &mut Machine,
    flags: u32,
    literals: u32,
    pairs: u32,
    bits: u32,
    target: u32,
) -> u32 {
    let (mut flags_at, mut literals, mut pairs, mut out) = (flags, literals, pairs, target);
    let mask = 1u32.wrapping_shl(bits).wrapping_sub(1);
    loop {
        let mut word = m.lw(flags_at);
        flags_at = off(flags_at, 4);
        if word == 0 {
            return out;
        }
        let mut copy = (word as i32) > 0;
        loop {
            word <<= 1;
            if word == 0 {
                break;
            }
            let this = copy;
            copy = (word as i32) > 0;
            if this {
                let pair = u32::from(m.lhu(pairs));
                pairs = off(pairs, 2);
                let distance = (pair & mask).wrapping_add(1);
                let mut length = pair.wrapping_shr(bits).wrapping_add(3) as i32;
                let mut from = out.wrapping_sub(distance);
                loop {
                    let byte = m.lbu(from);
                    length -= 1;
                    m.sb(out, byte);
                    from = off(from, 1);
                    out = off(out, 1);
                    if length <= 0 {
                        break;
                    }
                }
            } else {
                let byte = m.lbu(literals);
                literals = off(literals, 1);
                m.sb(out, byte);
                out = off(out, 1);
            }
        }
    }
}

/// 800AE1F4: the halfword variant (offset bits - 1, distance `2 * offset +
/// 2` bytes, `length + 2` halfwords).
fn expand_halfwords(
    m: &mut Machine,
    flags: u32,
    literals: u32,
    pairs: u32,
    bits: u32,
    target: u32,
) -> u32 {
    let (mut flags_at, mut literals, mut pairs, mut out) = (flags, literals, pairs, target);
    let bits = bits.wrapping_sub(1);
    let mask = 1u32.wrapping_shl(bits).wrapping_sub(1);
    loop {
        let mut word = m.lw(flags_at);
        flags_at = off(flags_at, 4);
        if word == 0 {
            return out;
        }
        let mut copy = (word as i32) > 0;
        loop {
            word <<= 1;
            if word == 0 {
                break;
            }
            let this = copy;
            copy = (word as i32) > 0;
            if this {
                let pair = u32::from(m.lhu(pairs));
                pairs = off(pairs, 2);
                let distance = ((pair & mask) << 1).wrapping_add(2);
                let mut length = pair.wrapping_shr(bits).wrapping_add(2) as i32;
                let mut from = out.wrapping_sub(distance);
                loop {
                    let half = m.lhu(from);
                    length -= 1;
                    m.sh(out, half);
                    from = off(from, 2);
                    out = off(out, 2);
                    if length <= 0 {
                        break;
                    }
                }
            } else {
                let half = m.lhu(literals);
                literals = off(literals, 2);
                m.sh(out, half);
                out = off(out, 2);
            }
        }
    }
}

/// 800AE2A0: byte variant bounded by an output count; 32 flags per word,
/// literals fetched a word at a time and emitted low byte first.
fn expand_counted(
    m: &mut Machine,
    flags: u32,
    literals: u32,
    pairs: u32,
    bits: u32,
    count: u32,
    target: u32,
) -> u32 {
    let (mut flags_at, mut literals, mut pairs, mut out) = (flags, literals, pairs, target);
    let mask = 1u32.wrapping_shl(bits).wrapping_sub(1);
    let mut remaining = count as i32;
    let mut buffered: i32 = -1;
    let mut literal_word = 0u32;
    let mut word = m.lw(flags_at);
    flags_at = off(flags_at, 4);
    let mut left: i32 = 0x1f;
    if remaining <= 0 {
        return out;
    }
    loop {
        if left < 0 {
            left = 0x1f;
            word = m.lw(flags_at);
            flags_at = off(flags_at, 4);
        }
        let literal = (word as i32) < 0;
        word <<= 1;
        if literal {
            remaining = remaining.wrapping_sub(1);
            let had = buffered > 0;
            buffered = buffered.wrapping_sub(1);
            if !had {
                literal_word = m.lw(literals);
                literals = off(literals, 4);
                buffered = 3;
            }
            m.sb(out, literal_word as u8);
            literal_word >>= 8;
            out = off(out, 1);
        } else {
            let pair = u32::from(m.lhu(pairs));
            pairs = off(pairs, 2);
            let distance = (pair & mask).wrapping_add(1);
            let mut length = pair.wrapping_shr(bits).wrapping_add(3) as i32;
            let mut from = out.wrapping_sub(distance);
            remaining = remaining.wrapping_sub(length);
            loop {
                let byte = m.lbu(from);
                length -= 1;
                m.sb(out, byte);
                from = off(from, 1);
                out = off(out, 1);
                if length <= 0 {
                    break;
                }
            }
        }
        if remaining <= 0 {
            return out;
        }
        left -= 1;
    }
}

/// 8005D490: read the 1,024-color source rectangle (800BAC38) from VRAM,
/// interpolate it toward the far color at `far` for 16 angles (rsin of
/// 0, 64, ... 960), upload the even ones as 16x64 palettes at x320..368,
/// y384/448, and patch `count` 12-byte records at `patches` (+6 = source
/// color at index +0).
pub fn prepare_palettes(m: &mut Machine, far: u32, patches: u32, count: u32) {
    framed(m, 0x1048, |m, sp| {
        let rect = off(sp, 0x1018);
        let source = off(sp, 0x18);
        let output = off(sp, 0x818);
        let upload = off(sp, 0x1020);
        let (low, high) = (m.lw(PALETTE_SOURCE), m.lw(off(PALETTE_SOURCE, 4)));
        m.sw(rect, low);
        m.sw(off(rect, 4), high);
        sdk::draw_sync(m, 0);
        if sdk::store_image(m, rect, source) == u32::MAX {
            sdk::printf(m, [0x8002_402c, 0, 0, 0]);
        }
        sdk::draw_sync(m, 0);
        let (mut x, mut y) = (0x140u32, 0x180u32);
        for step in 0..16u32 {
            let level = sdk::rsin(m, step << 6);
            m.sw(off(sp, 0x10), level);
            interpolate_palette(m, output, source, 0x400, far, level);
            if step & 1 == 0 {
                m.sh(upload, x as u16);
                x += 0x10;
                m.sh(off(upload, 4), 0x10);
                m.sh(off(upload, 6), 0x40);
                m.sh(off(upload, 2), y as u16);
                if x >= 0x180 {
                    x = 0x140;
                    y += 0x40;
                }
                sdk::load_image(m, upload, output);
                sdk::draw_sync(m, 0);
            }
        }
        let mut record = patches;
        let mut index = 0i32;
        while index < count as i32 {
            let color_index = sext16(m.lh(record));
            let color = m.lhu(off(source, color_index << 1));
            index += 1;
            m.sh(off(record, 6), color);
            record = off(record, 0xc);
        }
    })
}

/// 8005D3F8: for each nonzero RGB555 color, INTPL toward the far color
/// (`far` bytes << 4) with IR0 = `level`, keeping the STP bit; zero stays 0.
pub fn interpolate_palette(
    m: &mut Machine,
    target: u32,
    source: u32,
    count: u32,
    far: u32,
    level: u32,
) {
    let r = u32::from(m.lbu(far)) << 4;
    let g = u32::from(m.lbu(off(far, 1))) << 4;
    let b = u32::from(m.lbu(off(far, 2))) << 4;
    m.gte.ctc2(ctrl::RFC, r);
    m.gte.ctc2(ctrl::GFC, g);
    m.gte.ctc2(ctrl::BFC, b);
    m.gte.mtc2(reg::IR0, level);
    let (mut target, mut source) = (target, source);
    for _ in 0..(count as i32).max(0) {
        let color = u32::from(m.lhu(source));
        source = off(source, 2);
        let value = if color == 0 {
            0
        } else {
            m.gte.mtc2(reg::IRGB, color);
            // INTPL sf=1 lm=0 (0x4A980011), three instructions after the write.
            m.gte.intpl(true, false);
            let result = m.gte.mfc2(reg::ORGB);
            if color & 0x8000 != 0 {
                result | 0x8000
            } else {
                result
            }
        };
        m.sh(target, value as u16);
        target = off(target, 2);
    }
}

// ---- loading screen ---------------------------------------------------------------------

/// 80036D00: screen initialization by visual mode (0/1: 80036668, 2:
/// 800360FC through the dispatcher), after clearing the visual status.
pub fn initialize_screen(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        let state = m.gw(LOADING);
        m.sb(off(state, 5), 0);
        let state = m.gw(LOADING);
        match m.lbu(off(state, 4)) {
            0 | 1 => initialize_aperture(m),
            2 => {
                let context = m.gw(CONTEXT);
                let ot = m.lw(off(context, 0xc));
                let primitives = m.lw(off(context, 0x14));
                let next = dispatch::call(m, 0x8003_60fc, [ot, primitives, 0, 0]);
                let context = m.gw(CONTEXT);
                m.sw(off(context, 0x14), next);
            }
            _ => {}
        }
    })
}

/// 80036668: aperture-mode state: render flags |= 8, background RGB
/// (0,12,0), offsets, colors (0,255,0)/(0,131,0), scale 0x2C3B2, ticks 0.
pub fn initialize_aperture(m: &mut Machine) {
    let render = m.gw(RENDER);
    let flags = m.lw(render);
    m.sb(off(render, 0x10), 0);
    m.sw(render, flags | 8);
    let render = m.gw(RENDER);
    m.sb(off(render, 0x11), 0xc);
    let render = m.gw(RENDER);
    m.sb(off(render, 0x12), 0);
    let state = m.gw(LOADING);
    m.sw(off(state, 0x10), 0xf800_0000);
    m.sw(off(state, 0x14), 0x00a0_0000);
    m.sw(off(state, 0x1c), 0xc7);
    m.sw(off(state, 0x20), 4);
    m.sb(off(state, 0x28), 0);
    let current = m.gw(LOADING);
    m.sw(off(state, 0x38), 0);
    m.sw(off(state, 0xc), 0);
    m.sw(off(state, 0x18), 0);
    m.sw(off(state, 0x24), 0);
    m.sw(off(state, 0x34), 0x0002_c3b2);
    m.sh(off(state, 0x32), 0);
    m.sb(off(current, 0x29), 0xff);
    for (offset, value) in [(0x2a, 0), (0x2c, 0), (0x2d, 0x83), (0x2e, 0)] {
        let state = m.gw(LOADING);
        m.sb(off(state, offset), value);
    }
    let state = m.gw(LOADING);
    m.sh(off(state, 0x30), 0);
}

/// 80036D98: draw the screen of the visual mode (1: [`draw_aperture_screen`];
/// 0: 80036724 and 2: 8003617C through the dispatcher) into the context's
/// primitive buffer (+0x14) and OT (+8).
pub fn draw_screen(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        let state = m.gw(LOADING);
        let routine = match m.lbu(off(state, 4)) {
            0 => 0x8003_6724,
            1 => 0x8003_6b20,
            2 => 0x8003_617c,
            _ => return,
        };
        let context = m.gw(CONTEXT);
        let ot = m.lw(off(context, 8));
        let primitives = m.lw(off(context, 0x14));
        let next = if routine == 0x8003_6b20 {
            draw_aperture_screen(m, ot, primitives)
        } else {
            dispatch::call(m, routine, [ot, primitives, 0, 0])
        };
        let context = m.gw(CONTEXT);
        m.sw(off(context, 0x14), next);
    })
}

/// 80036B20: mode-1 frame: texts, then the noise aperture sized by the
/// opening table (80023948, 17 entries, clamped tick) and darkened by
/// the remaining progress; advances the tick (+0x38).
pub fn draw_aperture_screen(m: &mut Machine, ot: u32, primitives: u32) -> u32 {
    framed(m, 0x70, |m, sp| {
        let sizes: [u32; 17] = std::array::from_fn(|i| m.lw(off(APERTURE_SIZES, 4 * i as u32)));
        let state = m.gw(LOADING);
        let stage = m.lbu(off(state, 6));
        let mut amount = if stage == 1 {
            sext16(m.lh(off(state, 2)))
        } else {
            u32::from(stage == 2) << 12
        };
        if (amount as i32) >= 0x1001 {
            amount = 0x1000;
        }
        let remaining = 0x1000u32.wrapping_sub(amount);
        let primitives = draw_texts(m, ot, primitives);
        let state = m.gw(LOADING);
        let tick = (m.lw(off(state, 0x38)) as i32).clamp(0, 16);
        let entry = sizes[tick as usize];
        let scaled = i32::from(entry as u16 as i16).wrapping_mul(320);
        let width = if scaled < 0 {
            scaled.wrapping_add(0xfff) >> 12
        } else {
            scaled >> 12
        };
        m.sh(off(sp, 0x14), width as u16);
        let raw = i32::from((entry >> 16) as u16 as i16);
        let height = (if raw < 0 { raw + 0xf } else { raw }) >> 4;
        m.sh(off(sp, 0x10), (0x140i32.wrapping_sub(width) >> 1) as u16);
        m.sh(off(sp, 0x16), height as u16);
        m.sh(off(sp, 0x12), (0x100i32.wrapping_sub(height) >> 1) as u16);
        let next = draw_aperture(m, ot, primitives, off(sp, 0x10), remaining << 1);
        let state = m.gw(LOADING);
        let tick = m.lw(off(state, 0x38));
        m.sw(off(state, 0x38), tick.wrapping_add(1));
        next
    })
}

/// 800364D0: difficulty line for the first 40 frames (not in replays), then
/// the mission name in the fixed font, its green level following the
/// progress, with the blinking prefix while context clock bit 0x10 is set.
pub fn draw_texts(m: &mut Machine, ot: u32, primitives: u32) -> u32 {
    framed(m, 0x28, |m, _| {
        let mut next = primitives;
        let state = m.gw(LOADING);
        if m.lh(state) < 0x28 {
            let context = m.gw(CONTEXT);
            if m.lbu(off(context, 0x3a9)) == 0 {
                let difficulty = m.lw(off(context, 0x398)) & 3;
                next = draw_difficulty(m, ot, next, 0xaa, 0x12, difficulty);
            }
        }
        let state = m.gw(LOADING);
        let stage = m.lbu(off(state, 6));
        let visible = (stage == 1 && m.lh(off(state, 2)) > 0) || stage == 2;
        if !visible {
            return next;
        }
        let context = m.gw(CONTEXT);
        let mission = sext16(m.lh(off(context, 0x390)));
        let text = m.lw(off(mission << 2, MISSION_NAMES));
        let mut length = 0u32;
        while m.lbu(off(text, length)) != 0 {
            length = length.wrapping_add(1);
        }
        let x = 0x140u32.wrapping_sub(length << 3) as i32 >> 1;
        let state = m.gw(LOADING);
        let level = if m.lbu(off(state, 6)) == 1 {
            i32::from(m.lh(off(state, 2)))
        } else {
            0x1000 - i32::from(m.lh(off(state, 2)))
        };
        let level = (if level < 0 { level + 0x1f } else { level }) >> 5;
        let color = ((level as u32) & 0xff) << 8;
        next = draw_fixed_text(m, ot, next, x as u32, 0x76, color, text);
        let context = m.gw(CONTEXT);
        if m.lw(off(context, 0x20)) & 0x10 != 0 {
            next = draw_fixed_text(
                m,
                ot,
                next,
                x.wrapping_sub(0xc) as u32,
                0x76,
                color,
                NAME_PREFIX,
            );
        }
        next
    })
}

/// 80034F7C: "Level: <difficulty>" in the proportional font, bracketed by
/// draw-mode primitives 0x35/0x15. The label is localized text 13; the
/// value text index comes from the byte table 800BAB40 (local copy).
pub fn draw_difficulty(
    m: &mut Machine,
    ot: u32,
    primitives: u32,
    x: u32,
    y: u32,
    difficulty: u32,
) -> u32 {
    framed(m, 0x48, |m, sp| {
        let indices = m.lw(DIFFICULTY_TEXT);
        m.sw(off(sp, 0x20), indices);
        let context = m.gw(CONTEXT);
        let table = m.lw(DIFFICULTY_LABEL);
        let language = m.lw(off(context, 0x38c));
        let label = m.lw(off(language << 2, table));
        let next = draw_mode(m, ot, primitives, 0x35);
        let next = draw_localized(m, ot, next, x, y, 0x8000, 0, 0xd);
        let width = text_width(m, label);
        let index = u32::from(m.lbu(off(off(sp, 0x20), difficulty)));
        let next = draw_localized(m, ot, next, x.wrapping_add(width), y, 0x8000, 0, index);
        draw_mode(m, ot, next, 0x15)
    })
}

/// 800343D0: one-word draw-mode packet E1 `(mode & 0x9FF)` linked into `ot`.
pub fn draw_mode(m: &mut Machine, ot: u32, primitive: u32, mode: u32) -> u32 {
    m.sb(off(primitive, 3), 1);
    m.sw(off(primitive, 4), (mode & 0x9ff) | 0xe100_0000);
    let tag = m.lw(primitive);
    let link = m.lw(ot);
    m.sw(primitive, (tag & 0xff00_0000) | (link & 0x00ff_ffff));
    let link = m.lw(ot);
    m.sw(ot, (link & 0xff00_0000) | (primitive & 0x00ff_ffff));
    off(primitive, 8)
}

/// 80035384: proportional text width (space 6, else glyph width +4).
pub fn text_width(m: &mut Machine, text: u32) -> u32 {
    let mut width = 0u32;
    let mut cursor = text;
    let mut byte = u32::from(m.lbu(cursor));
    cursor = off(cursor, 1);
    while byte != 0 {
        let index = byte.wrapping_sub(0x20);
        width = width.wrapping_add(if index == 0 {
            6
        } else {
            u32::from(m.lbu(off(off(index << 3, GLYPHS), 4)))
        });
        byte = u32::from(m.lbu(cursor));
        cursor = off(cursor, 1);
    }
    width
}

/// 800353D8: localized text `index` (800AF6DC, context language +0x38C),
/// centered (align 1) or right-aligned (2) at `x`.
#[allow(clippy::too_many_arguments)]
pub fn draw_localized(
    m: &mut Machine,
    ot: u32,
    primitives: u32,
    x: u32,
    y: u32,
    color: u32,
    align: u32,
    index: u32,
) -> u32 {
    framed(m, 0x30, |m, _| {
        let context = m.gw(CONTEXT);
        let table = m.lw(off(index << 2, LOCALIZED_TEXT));
        let language = m.lw(off(context, 0x38c));
        let text = m.lw(off(language << 2, table));
        let x = match align {
            1 => x.wrapping_sub(((text_width(m, text) as i32) >> 1) as u32),
            2 => x.wrapping_sub(text_width(m, text)),
            _ => x,
        };
        draw_variable_text(m, ot, primitives, x, y, color, text)
    })
}

/// 800352BC: proportional font, one SPRT (0x66) per glyph from the
/// descriptor table 800AEE88 (CLUT, UV, size, y offset), each prepended to
/// `ot`; spaces advance 6 pixels.
pub fn draw_variable_text(
    m: &mut Machine,
    ot: u32,
    primitives: u32,
    x: u32,
    y: u32,
    color: u32,
    text: u32,
) -> u32 {
    let mut cursor = text;
    let mut byte = u32::from(m.lbu(cursor));
    cursor = off(cursor, 1);
    let mut primitive = primitives;
    let mut x = x;
    let code = color | 0x6600_0000;
    while byte != 0 {
        let index = byte.wrapping_sub(0x20);
        if index == 0 {
            x = x.wrapping_add(6);
        } else {
            let glyph = off(index << 3, GLYPHS);
            m.sw(off(primitive, 4), code);
            let clut = sext16(m.lh(glyph));
            let uv = u32::from(m.lhu(off(glyph, 2)));
            let width = m.lbu(off(glyph, 4));
            let height = m.lbu(off(glyph, 5));
            m.sh(off(primitive, 8), x as u16);
            m.sw(off(primitive, 0xc), (clut << 16) | uv);
            let offset = m.lhu(off(glyph, 6));
            x = x.wrapping_add(width.into());
            m.sh(off(primitive, 0x10), width.into());
            m.sh(off(primitive, 0x12), height.into());
            m.sh(off(primitive, 0xa), offset.wrapping_add(y as u16));
            let link = m.lw(ot);
            m.sw(primitive, link | 0x0400_0000);
            m.sw(ot, (primitive << 8) >> 8);
            primitive = off(primitive, 0x14);
        }
        byte = u32::from(m.lbu(cursor));
        cursor = off(cursor, 1);
    }
    primitive
}

/// 80035144: fixed 8-pixel font, one 8x8 SPRT (0x76) per non-space glyph
/// (texture page row from the character, CLUT 0x7F98), with the original
/// kerning for '-', 'W', '[', ']' and ':'/';', closed by draw mode 0x36.
pub fn draw_fixed_text(
    m: &mut Machine,
    ot: u32,
    primitives: u32,
    x: u32,
    y: u32,
    color: u32,
    text: u32,
) -> u32 {
    framed(m, 0x18, |m, _| {
        let mut cursor = text;
        let mut character = u32::from(m.lbu(cursor));
        cursor = off(cursor, 1);
        let mut primitive = primitives;
        let mut x = x;
        let code = color | 0x7600_0000;
        while character != 0 {
            let index = character.wrapping_sub(0x20);
            if index != 0 {
                let u = ((index & 0xf) << 3).wrapping_add(0x80);
                let v = (((index as i32) >> 4) as u32)
                    .wrapping_shl(11)
                    .wrapping_add(0x4700);
                let uv = u | v | 0x7f98_0000;
                m.sw(off(primitive, 4), code);
                m.sw(off(primitive, 0xc), uv);
                m.sh(off(primitive, 8), x as u16);
                m.sh(off(primitive, 0xa), y as u16);
                let link = m.lw(ot);
                m.sw(primitive, link | 0x0300_0000);
                m.sw(ot, (primitive << 8) >> 8);
                primitive = off(primitive, 0x10);
                if index == 0x2d || index == 0x37 {
                    x = x.wrapping_add(1);
                }
                if index == 0x3b || index == 0x3d {
                    x = x.wrapping_sub(4);
                }
                if index.wrapping_sub(0x1a) < 2 {
                    x = x.wrapping_sub(5);
                }
            }
            x = x.wrapping_add(8);
            character = u32::from(m.lbu(cursor));
            cursor = off(cursor, 1);
        }
        draw_mode(m, ot, primitive, 0x36)
    })
}

/// 80035F5C: full-screen fade: E6 mask on, E1 (add 0x220 / subtract 0x240
/// for negative amounts), a 320x256 semi-transparent TILE of grey |amount|,
/// E6 mask off; the four packets form one forward chain prepended to `ot`.
pub fn fade(m: &mut Machine, ot: u32, primitive: u32, amount: u32) -> u32 {
    let second = off(primitive, 0xc);
    let third = off(primitive, 0x14);
    m.sw(primitive, ((second << 8) >> 8) | 0x0200_0000);
    m.sw(off(primitive, 4), 0xe600_0001);
    m.sw(off(primitive, 8), 0);
    let (mode, amount) = if (amount as i32) < 0 {
        (0xe100_0240, amount.wrapping_neg())
    } else {
        (0xe100_0220, amount)
    };
    m.sw(off(primitive, 0xc), ((third << 8) >> 8) | 0x0100_0000);
    m.sw(off(second, 4), mode);
    m.sw(third, 0x0300_0000);
    m.sw(
        off(third, 4),
        amount | (amount << 8) | (amount << 16) | 0x6200_0000,
    );
    m.sh(off(third, 0xc), 0x140);
    m.sh(off(third, 0xe), 0x100);
    let last = off(third, 0x10);
    m.sw(last, 0x0200_0000);
    m.sh(off(third, 8), 0);
    m.sh(off(third, 0xa), 0);
    m.sw(off(last, 4), 0xe600_0000);
    m.sw(off(last, 8), 0);
    let tag = m.lw(last);
    let link = m.lw(ot);
    let merged = (tag & 0xff00_0000) | (link & 0x00ff_ffff);
    m.sw(last, merged);
    let link = m.lw(ot);
    m.sw(ot, (link & 0xff00_0000) | (last & 0x00ff_ffff));
    // addPrim(ot, primitive) through lwl/swl at +2: the low byte of the
    // register is shifted out, so only the OT link reaches memory.
    let link = lwl(m, merged, off(ot, 2));
    swl(m, link, off(third, 2));
    swl(m, primitive << 8, off(ot, 2));
    off(third, 0x1c)
}

/// 80083160: the noise aperture: E1 0x218/E2 texture window, the noise
/// tiles (CLUT 0x46D6) darkened with DPCS toward far color 48 by `amount`
/// (IR0 = 0x1000 - amount, RGB 0x64838383), then E1/E2 reset; the whole
/// chain is prepended to `ot`. An empty rectangle draws nothing.
pub fn draw_aperture(m: &mut Machine, ot: u32, primitive: u32, rect: u32, amount: u32) -> u32 {
    framed(m, 0x20, |m, _| {
        if m.lh(off(rect, 4)) <= 0 || m.lh(off(rect, 6)) <= 0 {
            return primitive;
        }
        let tiles = off(primitive, 0x10);
        m.sw(primitive, ((tiles << 8) >> 8) | 0x0300_0000);
        m.sw(off(primitive, 4), 0xe100_0218);
        m.sw(off(primitive, 8), 0xe20c_4318);
        m.sw(off(primitive, 0xc), 0);
        m.gte.mtc2(reg::RGBC, 0x6483_8383);
        m.gte.mtc2(reg::IR0, 0x1000u32.wrapping_sub(amount));
        let position = m.lw(rect);
        let size = m.lw(off(rect, 4));
        let last = draw_noise_tiles(m, tiles, 0x46d6, position, size);
        m.sw(off(last, 4), 0xe100_0220);
        m.sw(last, 0x0300_0000);
        m.sw(off(last, 8), 0xe200_0000);
        m.sw(off(last, 0xc), 0);
        // lwl/swl addPrim: the register's prior low byte never reaches memory.
        let link = lwl(m, 0, off(ot, 2));
        swl(m, link, off(last, 2));
        swl(m, primitive << 8, off(ot, 2));
        off(last, 0x10)
    })
}

/// 80082CD0: 32x48 textured tiles covering the rectangle, one `rand()` UV
/// per tile, colored by DPCS (sf=1) with far color (48,48,48); a forward
/// chain ending at the returned address.
pub fn draw_noise_tiles(
    m: &mut Machine,
    primitive: u32,
    clut: u32,
    position: u32,
    size: u32,
) -> u32 {
    framed(m, 0x40, |m, _| {
        m.gte.ctc2(ctrl::RFC, 0x30);
        m.gte.ctc2(ctrl::GFC, 0x30);
        m.gte.ctc2(ctrl::BFC, 0x30);
        let x0 = sext16(position as u16 as i16);
        // DPCS sf=1 lm=0 (0x4A780010); the far color was written four
        // instructions earlier.
        m.gte.dpcs(true, false);
        let mut y = sext16((position >> 16) as u16 as i16);
        let width = sext16(size as u16 as i16);
        let mut rows = ((size as i32) >> 16) as u32;
        let color = m.gte.mfc2(reg::RGB2);
        let mut primitive = primitive;
        while rows as i32 > 0 {
            let mut columns = width;
            let mut x = x0;
            if columns as i32 > 0 {
                let mut link = (primitive << 8).wrapping_add(0x1400);
                let mut field = off(primitive, 0x12);
                loop {
                    m.sw(primitive, (link >> 8) | 0x0400_0000);
                    m.sw(off(field, 0u32.wrapping_sub(0xe)), color);
                    m.sh(off(field, 0u32.wrapping_sub(0xa)), x as u16);
                    m.sh(off(field, 0u32.wrapping_sub(8)), y as u16);
                    let random = sdk::rand(m);
                    let uv = random & if clut == 0x4696 { 0x7c7e } else { 0x7e7f };
                    m.sh(off(field, 0u32.wrapping_sub(6)), uv as u16);
                    m.sh(off(field, 0u32.wrapping_sub(4)), clut as u16);
                    let w = if (columns as i32) < 0x20 {
                        columns
                    } else {
                        0x20
                    };
                    m.sh(off(field, 0u32.wrapping_sub(2)), w as u16);
                    let h = if (rows as i32) < 0x30 { rows } else { 0x30 };
                    m.sh(field, h as u16);
                    field = off(field, 0x14);
                    link = link.wrapping_add(0x1400);
                    primitive = off(primitive, 0x14);
                    columns = columns.wrapping_sub(0x20);
                    x = x.wrapping_add(0x20);
                    if columns as i32 <= 0 {
                        break;
                    }
                }
            }
            rows = rows.wrapping_sub(0x30);
            y = y.wrapping_add(0x30);
        }
        primitive
    })
}

// ---- indirect calls ----------------------------------------------------------------------

/// Native targets owned by this module: every translated routine, called
/// with the original `a0..a3` (stack arguments at `sp+0x10..`), returning `v0`.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let [a0, a1, a2, a3] = args;
    Some(match target {
        0x8003_55f0 => initialize(m),
        0x8003_54b8 => poll(m),
        0x8003_5730 => prepare_mission(m),
        0x8003_5c10 => queue_aircraft(m),
        0x8003_5c90 => prepare_aircraft(m),
        0x8003_54ac => {
            read_complete(m);
            0
        }
        0x8003_5e28 => {
            select_replay(m);
            0
        }
        0x8002_953c => file_entry(m, a0),
        0x8002_c1b8 => file_size(m, a0),
        0x8002_c214 => total_size(m, a0, a1),
        0x8002_c384 => queue_read(m, a0, a1, a2),
        0x8002_c404 => queue_files(m, a0, a1, a2, a3),
        0x8002_b1d4 => read_position(m),
        0x8002_c604 => read_progress(m, a0),
        0x8002_d6cc => {
            reset_sound(m);
            0
        }
        0x8002_d544 => install_sound_bank(m, a0),
        0x8002_d4e8 => upload_samples(m, a0, a1, a2),
        0x8002_6e98 => relocate_directory(m, a0),
        0x8002_6ee0 => relocate_fields(m, a0, a1, a2),
        0x8002_6e50 => upload_texture_group(m, a0, a1),
        0x8002_6d8c => {
            upload_tims(m, a0, a1);
            0
        }
        0x800a_e118 => decompress(m, a0, a1),
        0x8005_d490 => {
            prepare_palettes(m, a0, a1, a2);
            0
        }
        0x8005_d3f8 => {
            let [level] = stack_args(m);
            interpolate_palette(m, a0, a1, a2, a3, level);
            0
        }
        0x8003_6d00 => {
            initialize_screen(m);
            0
        }
        0x8003_6668 => {
            initialize_aperture(m);
            0
        }
        0x8003_6d98 => {
            draw_screen(m);
            0
        }
        0x8003_6b20 => draw_aperture_screen(m, a0, a1),
        0x8003_64d0 => draw_texts(m, a0, a1),
        0x8003_4f7c => {
            let [difficulty] = stack_args(m);
            draw_difficulty(m, a0, a1, a2, a3, difficulty)
        }
        0x8003_43d0 => draw_mode(m, a0, a1, a2),
        0x8003_5384 => text_width(m, a0),
        0x8003_53d8 => {
            let [color, align, index] = stack_args(m);
            draw_localized(m, a0, a1, a2, a3, color, align, index)
        }
        0x8003_52bc => {
            let [color, text] = stack_args(m);
            draw_variable_text(m, a0, a1, a2, a3, color, text)
        }
        0x8003_5144 => {
            let [color, text] = stack_args(m);
            draw_fixed_text(m, a0, a1, a2, a3, color, text)
        }
        0x8003_5f5c => fade(m, a0, a1, a2),
        0x8008_3160 => draw_aperture(m, a0, a1, a2, a3),
        0x8008_2cd0 => draw_noise_tiles(m, a0, a1, a2, a3),
        _ => return None,
    })
}
