//! Resident state-2 loading (substates 0..5) on the address-preserving machine.
//!
//! The five dispatcher callbacks and everything they reach are translated
//! from the original instructions:
//!
//! | Callback | Substate | Rust |
//! | --- | --- | --- |
//! | `800358F8` | 0 | [`initialize`]: CD queue reset, sound reset, queue mission file |
//! | `800357C0` | 1, 4 | [`poll`]: progress, loading screen, completion test |
//! | `80035A38` | 2 | [`prepare_mission`]: sound bank, textures, expansion, relocation, palettes |
//! | `80035F18` | 3 | [`queue_aircraft`] |
//! | `80035F98` | 5 | [`prepare_aircraft`] |
//!
//! Devices are never emulated. CD reads are the game's own RAM queue
//! (`[gp+0x48]`); the queueing routine logs each request as a host call and
//! [`complete_cd_read`] is the host side that delivers the file bytes and the
//! completion state. GPU transfers (`LoadImage`/`StoreImage`), SPU transfers,
//! SPU register updates and BIOS event calls are the `sdk.rs` routines (host
//! calls plus their RAM bookkeeping); the queue resets 8002C6A8/8002CAD0,
//! 8002759C and the sound reset tree are in `shared.rs`.
//! Every translated routine is claimed by [`dispatch`] with its original
//! `a0..a3` and `v0`; routines with stack arguments read them at `sp+0x10..`.
use crate::{
    common::CONTEXT,
    dispatch,
    machine::{Fault, Machine, SmallData},
    sdk,
};
use ac3_gte::{ctrl, reg};

/// Result a loading callback returns to advance the resident substate.
pub const ADVANCE: u32 = 0x0400_0000;
/// 800357B4: CD completion callback stored in the queued read requests.
pub const READ_COMPLETE: u32 = 0x8003_57b4;

/// `[gp+0x48]`: CD request queue. +0 head, +4 tail, +8 sixteen 0x14-byte
/// entries, +0x148/+0x14C pending control request, +0x168 driver done flag,
/// +0x16C driver status, +0x174 active file, +0x17C/+0x180 read extent.
const CD_QUEUE: u32 = 0x48;
/// `[gp+0x34]`: file system state; +0x18/+0x1C point at the BPB and stream
/// file tables (8-byte records: size/flags word, sector word).
const FILE_SYSTEM: u32 = 0x34;
/// `[gp+0x38]`: CD read driver; +0x14 words left, +0x18 current destination.
const CD_DRIVER: u32 = 0x38;
/// `[gp+0x6C]`: the 60-byte loading state (frame, progress, mode, stage, ...).
const LOADING: u32 = 0x6c;
/// `[gp+0x2D4]`: render status block (flags, phase, background, parameter).
const RENDER: u32 = 0x2d4;

const SOUND_DESCRIPTORS: u32 = 0x800c_5a10;
const REVERB_MODE: u32 = 0x800c_65f4;
const DECOMPRESSORS: u32 = 0x800a_fd30;

const fn off(base: u32, offset: u32) -> u32 {
    base.wrapping_add(offset)
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

// JP-REVIEW(R0224): near function: PAL 800355F0 -> JP 800358F8, imm:6, 6 instructions differ (whole routine)
/// 800358F8 (state 2, substate 0): reset the CD queue and sound, apply a
/// replay selection, clear the exit fields, queue the mission file into a new
/// arena allocation, set render flag 1, select the visual mode and poll once.
// JP-PATCH: replaced by `crate::jp::res_p2_loading::initialize` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn initialize(m: &mut Machine) -> u32 {
    crate::jp::res_p2_loading::initialize(m)
}

// JP-REVIEW(R0225): near function: PAL 800354B8 -> JP 800357C0, imm:2, 2 instructions differ (whole routine)
/// 800357C0 (substates 1 and 4): while the mission (stage 1) or aircraft
/// (stage 2) file is the active read, store its stage and 0..4096 progress;
/// initialize the screen on frame 0, fade on frames 1-2, draw from frame 3,
/// advance the frame counter and return [`ADVANCE`] once the read is no
/// longer pending.
// JP-PATCH: replaced by `crate::jp::res_p2_loading::poll` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn poll(m: &mut Machine) -> u32 {
    crate::jp::res_p2_loading::poll(m)
}

// JP-REVIEW(R0226): near function: PAL 80035730 -> JP 80035A38, imm:2, 2 instructions differ (whole routine)
/// 80035A38 (substate 2): prepare the loaded mission resource (17-child
/// directory): install sound child 16, upload texture children 15..11 (each
/// expanded over the following, already consumed child), stage the
/// compressed prefix below [`ARENA_TOP`], expand children 0..10 after the
/// directory, bind the resource fields, relocate the record directories,
/// alias object slots 96 onward, build the eight palettes, set the arena end
/// and poll once.
// JP-PATCH: replaced by `crate::jp::res_p2_loading::prepare_mission` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn prepare_mission(m: &mut Machine) -> u32 {
    crate::jp::res_p2_loading::prepare_mission(m)
}

// JP-REVIEW(R0228): near function: PAL 80035C10 -> JP 80035F18, imm:1, 1 instructions differ (whole routine)
/// 80035F18 (substate 3): queue the selected aircraft file into a new arena
/// allocation and poll once.
// JP-PATCH: replaced by `crate::jp::res_p2_loading::queue_aircraft` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn queue_aircraft(m: &mut Machine) -> u32 {
    crate::jp::res_p2_loading::queue_aircraft(m)
}

// JP-REVIEW(R0229): near function: PAL 80035C90 -> JP 80035F98, imm:1, 1 instructions differ (whole routine)
/// 80035F98 (substate 5): prepare the loaded aircraft resource (4-child
/// directory): sound child 3, texture child 2, expansion of children 0/1,
/// record relocation, context+0xF84, resource +0x08/+0x0C/+0x5C, arena end,
/// one poll, then render phase/parameter 0 and flags |= 5.
// JP-PATCH: replaced by `crate::jp::res_p2_loading::prepare_aircraft` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn prepare_aircraft(m: &mut Machine) -> u32 {
    crate::jp::res_p2_loading::prepare_aircraft(m)
}

/// 800357B4: CD completion callback; clears the loading state's pending word.
pub fn read_complete(m: &mut Machine) {
    let state = m.gw(LOADING);
    m.sw(off(state, 8), 0);
}

// ---- CD request queue ---------------------------------------------------------

/// 8002995C: file table record of `file` (bit 11 selects the stream table).
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

/// 8002C5E0: file size in bytes (BPB: words * 4; stream: units << scale).
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

/// 8002C63C: summed size of `count` files listed at `list`.
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

/// 8002C7AC: append a type-2 read request (`file` to `destination`, then
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

/// 8002C82C: queue `count` files from `list` back to back starting at
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

/// 8002B5FC: the CD driver's current destination pointer.
pub fn read_position(m: &mut Machine) -> u32 {
    let driver = m.gw(CD_DRIVER);
    m.lw(off(driver, 0x18))
}

/// 8002CA2C: progress of the current read extent, 0..=0x1000; stores the
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

/// A CD read the original queued (8002C7AC) at the queue head.
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

/// Host side of the queued read: what the main-loop CD service (8002C4DC and
/// its type-1/type-2 handlers 8002BDCC/8002BE14) and the CD driver leave in
/// RAM once the data has arrived. A pending reset control request is retired
/// first (active file -1, +0x178 cleared, request flag cleared). Then the
/// file bytes `ACE.BPB[(sector - archive_sector) * 2048..][..size]` (the
/// resident table holds absolute disc sectors) are written to the
/// destination, the driver position reaches the extent end with no words
/// left, the handler marks the entry started, the driver's completion
/// callback (8002BDB0) stores done = 1 and status 0, the active file becomes
/// -1, the request callback runs with the status (800357B4 clears the
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

/// 8002DAF4: reverb depth 0 (SpuSetReverbDepth with mask 6), reverb off,
/// reverb off for every voice, and clear the reverb work area of the
/// configured mode (800C65F4).
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

/// 8002D96C: install a bank's 20-byte sample descriptors (first ID +0,
/// count +1) at 800C5A10 and upload its samples (SPU address +4, size +8,
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

/// 8002D910: I/O-mode SPU upload of `size` bytes from `source` to SPU byte
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

/// 80027338: relocate a directory in place. The halfword count at +0 is
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

/// 80027380: relocate `count` relative words at `fields` against `base`
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

/// 800272F0: expand a texture group to `target`, relocate its directory and
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

/// 8002722C: OpenTIM/ReadTIM each listed TIM and LoadImage its pixels, then
/// its CLUT. The TIM_IMAGE lives in this frame at `sp+0x10`.
pub fn upload_tims(m: &mut Machine, list: u32, count: u32) {
    framed(m, 0x38, |m, sp| {
        let image = off(sp, 0x10);
        let mut cursor = list;
        for _ in 0..(count as i32).max(0) {
            let tim = m.lw(cursor);
            if sdk::open_tim(m, tim) != 0 {
                let tim = m.lw(cursor);
                sdk::printf(m, [0x8001_0030, tim, 0, 0]);
            }
            if sdk::read_tim(m, image) == 0 {
                let tim = m.lw(cursor);
                sdk::printf(m, [0x8001_0048, tim, 0, 0]);
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

/// 800AFD08: expand an AC3 LZ block at `source` to `target` and return the
/// output end. Header: +7 kind (table 800AFD30), +0xB offset bits,
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
        0x800a_fd40 => expand_bytes(m, flags, literals, pairs, bits, target),
        0x800a_fde4 => expand_halfwords(m, flags, literals, pairs, bits, target),
        0x800a_fe90 => {
            let count = m.lw(off(source, 4)) & 0x00ff_ffff;
            expand_counted(m, flags, literals, pairs, bits, count, target)
        }
        address => {
            m.raise(Fault::Untranslated { address });
            target
        }
    }
}

// JP-REVIEW(R0231): near function: PAL 800AE150 -> JP 800AFD40, imm:3, 3 instructions differ (whole routine)
/// 800AFD40: flag words MSB first, each terminated by its lowest set bit;
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

// JP-REVIEW(R0232): near function: PAL 800AE150 -> JP 800AFD40, imm:3, 3 instructions differ (the part at +0xa4)
/// 800AFDE4: the halfword variant (offset bits - 1, distance `2 * offset +
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

// JP-REVIEW(R0233): near function: PAL 800AE150 -> JP 800AFD40, imm:3, 3 instructions differ (the part at +0x150)
/// 800AFE90: byte variant bounded by an output count; 32 flags per word,
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

/// 8005EB20: for each nonzero RGB555 color, INTPL toward the far color
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

/// 80037070: screen initialization by visual mode (0/1: 800369C8, 2:
/// 80036440 through the dispatcher), after clearing the visual status.
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
                let next = dispatch::call(m, 0x8003_6440, [ot, primitives, 0, 0]);
                let context = m.gw(CONTEXT);
                m.sw(off(context, 0x14), next);
            }
            _ => {}
        }
    })
}

/// 800369C8: aperture-mode state: render flags |= 8, background RGB
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

/// 80037108: draw the screen of the visual mode (1: [`draw_aperture_screen`];
/// 0: 80036A84 and 2: 800364C4 through the dispatcher) into the context's
/// primitive buffer (+0x14) and OT (+8).
pub fn draw_screen(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        let state = m.gw(LOADING);
        let routine = match m.lbu(off(state, 4)) {
            0 => 0x8003_6a84,
            1 => 0x8003_6e84,
            2 => 0x8003_64c4,
            _ => return,
        };
        let context = m.gw(CONTEXT);
        let ot = m.lw(off(context, 8));
        let primitives = m.lw(off(context, 0x14));
        let next = if routine == 0x8003_6e84 {
            draw_aperture_screen(m, ot, primitives)
        } else {
            dispatch::call(m, routine, [ot, primitives, 0, 0])
        };
        let context = m.gw(CONTEXT);
        m.sw(off(context, 0x14), next);
    })
}

// JP-REVIEW(R0235): near function: PAL 80036B20 -> JP 80036E84, imm:2 ins:3 reg:4, 9 instructions differ (whole routine)
/// 80036E84: mode-1 frame: texts, then the noise aperture sized by the
/// opening table (800239A0, 17 entries, clamped tick) and darkened by
/// the remaining progress; advances the tick (+0x38).
// JP-PATCH: replaced by `crate::jp::res_p2_loading::draw_aperture_screen` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn draw_aperture_screen(m: &mut Machine, ot: u32, primitives: u32) -> u32 {
    crate::jp::res_p2_loading::draw_aperture_screen(m, ot, primitives)
}

// JP-REVIEW(R0236): near function: PAL 800364D0 -> JP 80036824, del:1 imm:3 ins:4 op:6 reg:1, 15 instructions differ (whole routine)
/// 80036824: difficulty line for the first 40 frames (not in replays), then
/// the mission name in the fixed font, its green level following the
/// progress, with the blinking prefix while context clock bit 0x10 is set.
// JP-PATCH: replaced by `crate::jp::res_p2_loading::draw_texts` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn draw_texts(m: &mut Machine, ot: u32, primitives: u32) -> u32 {
    crate::jp::res_p2_loading::draw_texts(m, ot, primitives)
}

/// 800346C0: one-word draw-mode packet E1 `(mode & 0x9FF)` linked into `ot`.
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

/// 8003563C: fixed 8-pixel font, one 8x8 SPRT (0x76) per non-space glyph
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

// JP-REVIEW(R0241): near function: PAL 80035F5C -> JP 800362A0, imm:1, 1 instructions differ (whole routine)
/// 800362A0: full-screen fade: E6 mask on, E1 (add 0x220 / subtract 0x240
/// for negative amounts), a 320x256 semi-transparent TILE of grey |amount|,
/// E6 mask off; the four packets form one forward chain prepended to `ot`.
// JP-PATCH: replaced by `crate::jp::res_p2_loading::fade` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn fade(m: &mut Machine, ot: u32, primitive: u32, amount: u32) -> u32 {
    crate::jp::res_p2_loading::fade(m, ot, primitive, amount)
}

/// 800847A8: the noise aperture: E1 0x218/E2 texture window, the noise
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

/// 80084318: 32x48 textured tiles covering the rectangle, one `rand()` UV
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
#[allow(unused_variables)]
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let [a0, a1, a2, a3] = args;
    Some(match target {
        0x8003_58f8 => initialize(m),
        0x8003_57c0 => poll(m),
        0x8003_5a38 => prepare_mission(m),
        0x8003_5f18 => queue_aircraft(m),
        0x8003_5f98 => prepare_aircraft(m),
        0x8003_57b4 => {
            read_complete(m);
            0
        }
        0x8002_995c => file_entry(m, a0),
        0x8002_c5e0 => file_size(m, a0),
        0x8002_c63c => total_size(m, a0, a1),
        0x8002_c7ac => queue_read(m, a0, a1, a2),
        0x8002_c82c => queue_files(m, a0, a1, a2, a3),
        0x8002_b5fc => read_position(m),
        0x8002_ca2c => read_progress(m, a0),
        0x8002_daf4 => {
            reset_sound(m);
            0
        }
        0x8002_d96c => install_sound_bank(m, a0),
        0x8002_d910 => upload_samples(m, a0, a1, a2),
        0x8002_7338 => relocate_directory(m, a0),
        0x8002_7380 => relocate_fields(m, a0, a1, a2),
        0x8002_72f0 => upload_texture_group(m, a0, a1),
        0x8002_722c => {
            upload_tims(m, a0, a1);
            0
        }
        0x800a_fd08 => decompress(m, a0, a1),
        0x8005_eb20 => {
            let [level] = stack_args(m);
            interpolate_palette(m, a0, a1, a2, a3, level);
            0
        }
        0x8003_7070 => {
            initialize_screen(m);
            0
        }
        0x8003_69c8 => {
            initialize_aperture(m);
            0
        }
        0x8003_7108 => {
            draw_screen(m);
            0
        }
        0x8003_6e84 => draw_aperture_screen(m, a0, a1),
        0x8003_6824 => draw_texts(m, a0, a1),
        0x8003_46c0 => draw_mode(m, a0, a1, a2),
        0x8003_563c => {
            let [color, text] = stack_args(m);
            draw_fixed_text(m, a0, a1, a2, a3, color, text)
        }
        0x8003_62a0 => fade(m, a0, a1, a2),
        0x8008_47a8 => draw_aperture(m, a0, a1, a2, a3),
        0x8008_4318 => draw_noise_tiles(m, a0, a1, a2, a3),
        _ => return None,
    })
}
