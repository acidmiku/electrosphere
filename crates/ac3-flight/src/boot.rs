//! Resident boot on the machine: load the unpacked resident payload (0xAB000
//! bytes at 0x80010000) and run what the original runs from resident entry
//! 8002521C (`start`: BSS clear, stack/heap set-up) through `main`'s prologue,
//! `__main` and `ac3_initialize` 80025AC0, `VSync(0)` and `SetDispMask(1)`,
//! up to main's loop calling 80025720 at 80025D30.
//!
//! Device-facing SDK and kernel routines (interrupts, GPU, SPU, CD, pads,
//! memory card, root counters, heap) are not emulated: each is recorded as a
//! [`HostCall`](crate::machine::HostCall), in original order and with the
//! original arguments. Their RAM effects that game code later reads are
//! reproduced here (pad buffer heads), by `sdk.rs` (SetVideoMode's mode word,
//! InitGeom's GTE controls, ResetGraph's libgpu state) or by the host device
//! (CD file positions, sector data and drive status: [`DiscDevice`], the
//! version-independent `ac3_machine::disc`, re-exported here, which returns
//! the PAL [`CD_FILE_CACHE`] entries).
//! SDK/kernel-internal state that only the SDK reads is not produced;
//! `tests/boot.rs` lists every such range.
//!
//! Routines owned elsewhere are called through their owners: `sdk.rs`
//! (VSync, GetVideoMode, SetVideoMode, ResetGraph, SetDefDrawEnv,
//! SetDefDispEnv, PutDispEnv, InitGeom, SpuSetCommonAttr, DrawSync,
//! SetGeomOffset/SetGeomScreen, CdSearchFile, CdPosToInt), and through [`dispatch::call`] `loading.rs`
//! (the resource queue and file table 8002953C/8002C1B8/8002C384, 80026E98,
//! 8002D544, 80026D8C, OpenTIM/ReadTIM), `script.rs`/`init_tail.rs`
//! (8002ED24, 8002EE38), `init_38818.rs` (8002C7A4) and `mainloop.rs`
//! (80025720, first called inside 80025934, and the request pump 8002C0B4
//! that performs the boot resource load).
//!
//! 80026F5C, 8002A8AC, 8002A968 and 8002A9E4 are also translated in
//! `mainloop.rs` (the disc-swap check); those copies are pending removal
//! there (its 8002A9E4 takes CdPosToInt as a host call, which returns 0).
use crate::{
    arena,
    common::{CONTEXT, RESOURCES},
    dispatch,
    machine::{Fault, Machine, SmallData},
    sdk,
};
pub use ac3_machine::disc::{DiscDevice, DiscDeviceState};

/// Load address and size of the unpacked resident payload.
pub const PAYLOAD_ADDRESS: u32 = 0x8001_0000;
pub const PAYLOAD_BYTES: usize = 0xab000;
/// BSS cleared word by word by `start`.
pub const BSS_START: u32 = 0x800b_acb0;
pub const BSS_END: u32 = 0x800c_9dc0;
/// `[gp+0x2C4]`: display control block (flags word, environment selection).
pub const DISPLAY: u32 = 0x2c4;
/// `[gp+0x2D4]`: current draw/display environment pointers.
pub const ENVIRONMENTS: u32 = 0x2d4;
/// `[gp+0x34]`: CD file block (file positions, header buffers, tables).
pub const CD_FILES: u32 = 0x34;
/// `[gp+0x48]`: resource request queue (16 entries of 0x14 bytes from +8).
pub const REQUESTS: u32 = 0x48;
/// `[gp+0x10]`: pad block (0xD0 bytes; PadInitDirect buffers at +0, +0x22).
pub const PADS: u32 = 0x10;

/// Draw/display environment storage set up by 800252D8 (0x800BAF58).
const ENVIRONMENT_BASE: u32 = 0x800b_af58;
/// 800283A8 jump table (16 entries for kinds 1..=16).
const CHANNEL_CASES: u32 = 0x8001_032c;
/// 8002A9E4: the four resource file name pointers (BPH, SPH, BPB, SPB).
const FILE_NAMES: u32 = 0x800a_e7a0;
/// libcd's file cache (24-byte CdlFILE entries); CdSearchFile returns one.
pub const CD_FILE_CACHE: u32 = 0x800c_5cd0;
/// Resource header magic 'ACE3' and version words checked by 8002A9E4.
const HEADER_MAGIC: u32 = 0x4533_4341;
const BPH_VERSION: u32 = 0x1019;
const SPH_VERSION: u32 = 0x1999_1019;

// SDK and kernel entry points recorded as host calls.
pub const INIT_HEAP: u32 = 0x800a_d6f8;
pub const RESET_CALLBACK: u32 = 0x800a_0778;
pub const CD_INIT: u32 = 0x800a_1228;
pub const CD_SET_DEBUG: u32 = 0x800a_1440;
pub const VSYNC: u32 = 0x800a_0568;
pub const RESET_GRAPH: u32 = sdk::RESET_GRAPH;
pub const SET_DISP_MASK: u32 = 0x800a_65cc;
pub const PATCH_GTE: u32 = sdk::PATCH_GTE;
pub const SS_UT_REVERB_OFF: u32 = 0x8009_e218;
pub const INIT_CARD: u32 = 0x800a_b868;
pub const START_CARD: u32 = 0x800a_b8f8;
pub const BU_INIT: u32 = 0x800a_b7d8;
pub const PAD_INIT_DIRECT: u32 = 0x8009_c2a0;
pub const PAD_START_COM: u32 = 0x8009_b200;
pub const PAD_ENABLE_COM: u32 = 0x8009_9de0;
pub const EXIT_CRITICAL_SECTION: u32 = 0x800a_d7f8;
pub const SET_RCNT: u32 = 0x800a_d898;
pub const START_RCNT: u32 = 0x800a_d96c;
pub const SPU_SET_COMMON_ATTR: u32 = 0x8009_f888;
pub const SPU_SET_REVERB_MODE_PARAM: u32 = 0x8009_e498;
pub const CD_MIX: u32 = 0x800a_18e0;
pub const CD_SYNC_CALLBACK: u32 = 0x800a_14fc;
pub const CD_DATA_CALLBACK: u32 = 0x800a_1940;
pub const CD_READY_CALLBACK: u32 = 0x800a_1510;
pub const CD_SEARCH_FILE: u32 = 0x800a_30f8;
pub const CD_CONTROL_B: u32 = 0x800a_1794;
pub const CD_READ: u32 = 0x800a_4168;
pub const CD_READ_SYNC: u32 = 0x800a_4304;
pub const LOAD_IMAGE: u32 = 0x800a_6910;
pub const PRINTF: u32 = 0x800a_c3e8;
/// mainloop.rs: presentation step and resource request pump.
pub const PRESENT: u32 = 0x8002_5720;
pub const PUMP_REQUESTS: u32 = 0x8002_c0b4;
/// Routines of other modules on the boot path (called through `dispatch`).
const RESOURCE_SIZE: u32 = 0x8002_c1b8;
const QUEUE_READ: u32 = 0x8002_c384;
const RELOCATE_DIRECTORY: u32 = 0x8002_6e98;
const INSTALL_SOUND_BANK: u32 = 0x8002_d544;
const LOAD_TIMS: u32 = 0x8002_6d8c;
const OPEN_TIM: u32 = 0x800a_9218;
const READ_TIM: u32 = 0x800a_9228;
const RESET_VOICE_GROUPS: u32 = 0x8002_ed24;
const RESET_VOICE_ASSIGNMENTS: u32 = 0x8002_ee38;
const INIT_STREAM_STATE: u32 = 0x8002_c7a4;

/// Where [`boot_until`] stops.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootStop {
    /// Before 80025934 calls 80025720 (the first 80025720 call; capture
    /// `analysis/flight-runtime/boot-001` ends here).
    FirstPresent,
    /// Before main's loop calls 80025720 at 80025D30.
    MainLoop,
}

/// Why the boot sequence ended early: the requested stop point, or a fault
/// (an untranslated owned routine or a bad address) after which the boot
/// does not continue on an incomplete machine.
enum Halt {
    Stop,
    Fault,
}
type Flow = Result<u32, Halt>;

fn halt_on_fault(m: &Machine) -> Result<(), Halt> {
    match m.fault() {
        Some(_) => Err(Halt::Fault),
        None => Ok(()),
    }
}

// ---- small helpers --------------------------------------------------------------

fn load_unaligned(m: &mut Machine, address: u32) -> u32 {
    u32::from_le_bytes(std::array::from_fn(|i| {
        m.lbu(address.wrapping_add(i as u32))
    }))
}

fn store_unaligned(m: &mut Machine, address: u32, value: u32) {
    for (i, byte) in value.to_le_bytes().into_iter().enumerate() {
        m.sb(address.wrapping_add(i as u32), byte);
    }
}

fn at(base: u32, offset: u32) -> u32 {
    base.wrapping_add(offset)
}

fn context(m: &mut Machine) -> u32 {
    m.gw(CONTEXT)
}

// ---- payload and start -----------------------------------------------------------

/// Copy the unpacked resident payload to 0x80010000 (what the boot loader
/// leaves in RAM before jumping to 8002521C). `payload` must be the
/// 0xAB000-byte image (`ACE.unpacked.bin`).
pub fn load_payload(m: &mut Machine, payload: &[u8]) {
    m.write_bytes(PAYLOAD_ADDRESS, payload);
}

/// Load `payload` and boot to main's loop (see the module documentation).
/// The CD data the boot reads comes from `m.device` (a [`DiscDevice`]).
pub fn boot(m: &mut Machine, payload: &[u8]) {
    boot_until(m, payload, BootStop::MainLoop);
}

/// [`boot`], stopping at `stop`; returns whether `stop` was reached.
pub fn boot_until(m: &mut Machine, payload: &[u8], stop: BootStop) -> bool {
    load_payload(m, payload);
    start(m, stop)
}

/// 8002521C `start` and 80025240 `stup1`: no arguments. Clears the BSS
/// 0x800BACB0..0x800C9DC0, sets `$sp` to `[0x800252CC] | 0x80000000` and the
/// heap (InitHeap from 0x800C9DC4 up to the stack, minus `[0x800B25BC]` bytes
/// of stack), then runs `main` (800252BC `stup0` calls 80025D08). Returns
/// whether `stop` was reached; main itself never returns.
///
/// `stup1` saves `ra` (the BIOS `Exec` return) at 0x800BACB0 and reloads it
/// only if `main` returns, which it never does; the native boot has no BIOS
/// caller and leaves the cleared word.
pub fn start(m: &mut Machine, stop: BootStop) -> bool {
    let mut address = BSS_START;
    loop {
        m.sw(address, 0);
        address = address.wrapping_add(4);
        if address >= BSS_END {
            break;
        }
    }
    // stup1: stack top from the startup table (index 4), heap below it.
    let top = m.lw(0x8002_52c8 + 4);
    m.sp = top | 0x8000_0000;
    let heap = ((BSS_END << 3) >> 3) | 0x8000_0000;
    let stack_size = m.lw(0x800b_25bc);
    let heap_size = top
        .wrapping_sub(stack_size)
        .wrapping_sub((BSS_END << 3) >> 3);
    m.host_call(INIT_HEAP, [heap.wrapping_add(4), heap_size, 0, 0]);
    matches!(main(m, stop), Err(Halt::Stop))
}

/// 80025D08 `main` up to its loop: `__main` (80025214, an empty `jr ra`),
/// `ac3_initialize`, `VSync(0)`, `SetDispMask(1)`. Its 0x18-byte frame stays
/// allocated (main never returns).
fn main(m: &mut Machine, stop: BootStop) -> Flow {
    m.push_frame(0x18);
    ac3_initialize(m, stop)?;
    halt_on_fault(m)?;
    sdk::vsync(m, 0);
    m.host_call(SET_DISP_MASK, [1, 0, 0, 0]);
    if stop == BootStop::MainLoop {
        return Err(Halt::Stop);
    }
    Ok(0)
}

/// 80025AC0 `ac3_initialize`: globals and environments (800252D8), sound
/// channels (80028720 on context +0x15C), context +0x120 bit 0, devices
/// (80025610), display timing (80025934), audio attributes (80025990), CD
/// resource files (8002B8B0), boot resources (800259F8), sound (8002D334),
/// stream state (8002C7A4) and master volume (8002D458(1)).
fn ac3_initialize(m: &mut Machine, stop: BootStop) -> Flow {
    m.push_frame(0x18);
    init_globals(m);
    let ctx = context(m);
    init_sound_channels(m, at(ctx, 0x15c));
    let ctx = context(m);
    let flags = m.lw(at(ctx, 0x120)) | 1;
    m.sw(at(ctx, 0x120), flags);
    init_devices(m);
    halt_on_fault(m)?;
    init_display_timing(m, stop)?;
    init_audio_attributes(m);
    init_resources(m);
    halt_on_fault(m)?;
    load_boot_resources(m);
    halt_on_fault(m)?;
    init_sound(m);
    dispatch::call(m, INIT_STREAM_STATE, [0; 4]);
    let v0 = set_master_volume(m, 1);
    halt_on_fault(m)?;
    m.pop_frame(0x18);
    Ok(v0)
}

// ---- 800252D8: globals and environments ----------------------------------------------

/// 800252D8: no arguments. Sets `[gp+0x2C4]` (display block 0x800BB0A8),
/// `[gp+0x2DC]` (context 0x800BD158), `[gp+0x2CC]` (resources 0x800BE168) and
/// `[gp+0x2D4]` (0x800BB130); builds three DRAWENV/DISPENV pairs at
/// 0x800BAF58 (SetDefDrawEnv/SetDefDispEnv, 320x256 at y 0/256 and 320x512),
/// their background colours and screen offsets; initializes the display
/// block and the context fields, the resources' transition pair (1, 0), and
/// the arena (80026F5C with context +0x40). Returns `v0` (the context).
pub fn init_globals(m: &mut Machine) -> u32 {
    let s0 = ENVIRONMENT_BASE;
    m.set_gw(DISPLAY, s0 + 0x150);
    m.set_gw(CONTEXT, s0 + 0x2200);
    m.set_gw(RESOURCES, s0 + 0x3210);
    sdk::set_def_draw_env(m, s0, 0, 0, 0x140, 0x100);
    sdk::set_def_disp_env(m, s0 + 0x114, 0, 0x100, 0x140, 0x100);
    sdk::set_def_draw_env(m, s0 + 0x5c, 0, 0x100, 0x140, 0x100);
    sdk::set_def_disp_env(m, s0 + 0x128, 0, 0, 0x140, 0x100);
    sdk::set_def_draw_env(m, s0 + 0xb8, 0, 0, 0x140, 0x200);
    sdk::set_def_disp_env(m, s0 + 0x13c, 0, 0, 0x140, 0x200);
    m.sb(s0 + 0x14c, 1);
    for base in [0x18, 0x74, 0xd0] {
        m.sb(s0 + base + 1, 4);
        m.sb(s0 + base + 2, 0x38);
        m.sb(s0 + base + 3, 0x48);
    }
    for offset in [0x18, 0x74, 0xd0] {
        m.sb(s0 + offset, 0);
    }
    let display = m.gw(DISPLAY);
    m.sh(s0 + 0x11c, 0xfffe);
    m.sh(s0 + 0x122, 0x100);
    m.sh(s0 + 0x136, 0x100);
    m.sh(s0 + 0x14a, 0x100);
    m.sh(s0 + 0x11e, 0x10);
    m.sw(display, 0);
    m.sw(at(display, 4), 0);
    let x = i32::from(m.lh(s0 + 0x11c)) as u32;
    m.sw(at(display, 8), x);
    let y = i32::from(m.lh(s0 + 0x11e)) as u32;
    m.set_gw(ENVIRONMENTS, s0 + 0x1d8);
    m.sb(at(display, 0x10), 0);
    let block = m.gw(DISPLAY);
    m.sw(at(display, 0x14), 0);
    m.sw(at(display, 0xc), y);
    m.sb(at(block, 0x11), 0);
    let block = m.gw(DISPLAY);
    m.sb(at(block, 0x12), 0);
    let ctx = context(m);
    m.sw(ctx, 0);
    m.sw(at(ctx, 0x34), 0xffff_ffff);
    m.sw(at(ctx, 0x38), 0xffff_ffff);
    m.sw(at(ctx, 0x3c), 0x8001_0000);
    m.sw(at(ctx, 0x40), 0x800c_9d98);
    m.sb(at(ctx, 0x39c), 0xd0);
    let reloaded = context(m);
    for offset in [0x44, 0x38c, 0x44, 4, 0x20, 0x24, 0x28, 0x2c, 0x30] {
        m.sw(at(ctx, offset), 0);
    }
    for offset in [0x390, 0x392, 0x394, 0x396] {
        m.sh(at(ctx, offset), 1);
    }
    m.sw(at(ctx, 0x398), 1);
    m.sb(at(reloaded, 0x39d), 0xd0);
    for (offset, value) in [(0x39e, 0), (0x3a0, 1), (0x3a1, 0), (0x3a2, 0), (0x3a3, 0)] {
        let ctx = context(m);
        m.sb(at(ctx, offset), value);
    }
    let ctx = context(m);
    m.sw(at(ctx, 0x3a4), 0);
    m.sb(at(ctx, 0x3a8), 0);
    for (offset, value) in [(0x3a9, 0), (0x3aa, 0), (0x3ab, 1)] {
        let ctx = context(m);
        m.sb(at(ctx, offset), value);
    }
    let previous = context(m);
    let ctx = context(m);
    m.sh(at(previous, 0x3b2), 0);
    m.sb(at(ctx, 0x3ac), 0);
    for (offset, value) in [(0x3ad, 1), (0x3ae, 1), (0x3af, 1), (0x962, 0)] {
        let ctx = context(m);
        m.sb(at(ctx, offset), value);
    }
    let previous = context(m);
    let ctx = context(m);
    m.sh(at(previous, 0x3b0), 0);
    m.sw(at(previous, 0x3b4), 0);
    m.sw(at(previous, 0x3b8), 0);
    m.sh(at(previous, 0x958), 0);
    m.sb(at(ctx, 0x158), 0);
    let next = context(m);
    m.sw(at(ctx, 0x970), 0);
    m.sw(at(ctx, 0xf7c), 0);
    m.sw(at(ctx, 0x120), 0);
    for offset in [0x15b, 0x15a, 0x159] {
        m.sb(at(next, offset), 0);
    }
    let ctx = context(m);
    m.sw(at(ctx, 0xf8c), 0);
    m.sw(at(ctx, 0xfb0), 0);
    m.sw(at(ctx, 0x4c), 0);
    let resources = m.gw(RESOURCES);
    for offset in [
        0x48, 0xfac, 0xfa8, 0xfa4, 0xfa0, 0xf9c, 0xf98, 0xf94, 0xf90, 0xfb8, 0xfb4,
    ] {
        m.sw(at(ctx, offset), 0);
    }
    m.sw(at(resources, 0x70), 1);
    m.sw(at(resources, 0x74), 0);
    let arena = m.lw(at(ctx, 0x40));
    set_arena(m, arena)
}

/// 80026F5C: a0 = arena base. Sets context +0x1C (bump pointer) and +0x18
/// (base); returns `v0` (the context).
pub fn set_arena(m: &mut Machine, base: u32) -> u32 {
    let ctx = context(m);
    m.sw(at(ctx, 0x1c), base);
    m.sw(at(ctx, 0x18), base);
    ctx
}

// ---- 80028720: sound channel records ---------------------------------------------------

/// 80028720: a0 = five 0x5C-byte channel records (context +0x15C). Clears
/// the six slot bytes at `[gp+0x30]` stride 4 (80028F5C) and, for a non-null
/// a0, initializes the records as kinds 1, 2, 4, 8 and 0x10 (800283A8).
/// Returns `v0` (0x18, or -1 for a null a0).
pub fn init_sound_channels(m: &mut Machine, records: u32) -> u32 {
    let v0 = clear_slot_flags(m);
    if records == 0 {
        return v0;
    }
    let mut v0 = 0;
    for (index, kind) in [1, 2, 4, 8, 0x10].into_iter().enumerate() {
        v0 = init_channel(m, kind, at(records, index as u32 * 0x5c));
    }
    v0
}

/// 80028F5C: no arguments. Clears six bytes at `[gp+0x30]` stride 4;
/// returns `v0` (-1).
pub fn clear_slot_flags(m: &mut Machine) -> u32 {
    let mut address = m.gw(0x30);
    for _ in 0..6 {
        m.sb(address, 0);
        address = address.wrapping_add(4);
    }
    0xffff_ffff
}

/// 800283A8: a0 = kind, a1 = record. Clears word +0x58, copies a 32-halfword
/// template into the record (8002831C) and, for kinds 2, 4, 8 and 0x10, a
/// 24-byte template into +0x40 (80028344); every known kind (1, 2, 4, 8,
/// 0x10, through the jump table at 0x8001032C) ORs 0x18 into +0x58 (kind 1
/// also clears halfword +0x50). Returns `v0` (+0x58, or 0 for other kinds).
pub fn init_channel(m: &mut Machine, kind: u32, record: u32) -> u32 {
    let case = kind.wrapping_sub(1);
    m.sw(at(record, 0x58), 0);
    let target = if case < 0x10 {
        m.lw(CHANNEL_CASES + (case << 2))
    } else {
        0x8002_849c
    };
    let templates = match target {
        0x8002_83e8 => {
            copy_halfwords(m, record, 0x800a_e600);
            let flags = m.lw(at(record, 0x58));
            m.sh(at(record, 0x50), 0);
            let flags = flags | 0x18;
            m.sw(at(record, 0x58), flags);
            return flags;
        }
        0x8002_8404 => (0x800a_e640, 0x800a_e700),
        0x8002_8424 => (0x800a_e640, 0x800a_e718),
        0x8002_8444 => (0x800a_e680, 0x800a_e730),
        0x8002_8464 => (0x800a_e6c0, 0x800a_e748),
        0x8002_849c => return copy_halfwords(m, record, 0x800a_e600),
        other => {
            m.raise(Fault::Untranslated { address: other });
            return 0;
        }
    };
    copy_halfwords(m, record, templates.0);
    copy_bytes_24(m, at(record, 0x40), templates.1);
    let flags = m.lw(at(record, 0x58)) | 0x18;
    m.sw(at(record, 0x58), flags);
    flags
}

/// 8002831C: a0 = destination, a1 = source. Copies 32 halfwords; returns
/// `v0` (0).
pub fn copy_halfwords(m: &mut Machine, destination: u32, source: u32) -> u32 {
    for i in 0..0x20 {
        let value = m.lhu(at(source, i * 2));
        m.sh(at(destination, i * 2), value);
    }
    0
}

/// 80028344: a0 = destination, a1 = source. Copies 24 bytes as unaligned
/// words (four loads, four stores, two loads, two stores); returns `v0` (the
/// fifth word).
pub fn copy_bytes_24(m: &mut Machine, destination: u32, source: u32) -> u32 {
    let first: [u32; 4] = std::array::from_fn(|i| load_unaligned(m, at(source, i as u32 * 4)));
    for (i, word) in first.into_iter().enumerate() {
        store_unaligned(m, at(destination, i as u32 * 4), word);
    }
    let last: [u32; 2] =
        std::array::from_fn(|i| load_unaligned(m, at(source, 0x10 + i as u32 * 4)));
    for (i, word) in last.into_iter().enumerate() {
        store_unaligned(m, at(destination, 0x10 + i as u32 * 4), word);
    }
    last[0]
}

// ---- 80025610: devices ----------------------------------------------------------------

/// 80025610: no arguments. Device start-up, in original order: ResetCallback,
/// SetVideoMode(1) (PAL), CdInit (until it succeeds), CdSetDebug(0),
/// VSync(3), ResetGraph(0), a 256x256 DISPENV in its 0x48-byte frame (+0x20,
/// screen x -2, y 16, h 256) through PutDispEnv, SetDispMask(1), InitGeom,
/// SsUtReverbOff, InitCARD(0), StartCARD, _bu_init, the pads (800260AC with
/// context +0x50, gp, 2, 3) and ExitCriticalSection. Returns `v0`
/// (ExitCriticalSection's, 3 on the traced path).
pub fn init_devices(m: &mut Machine) -> u32 {
    let frame = m.push_frame(0x48);
    // An 8-byte copy of gp+8 into the frame (+0x18); never read.
    let low = load_unaligned(m, crate::GP + 8);
    let high = load_unaligned(m, crate::GP + 0xc);
    store_unaligned(m, frame + 0x18, low);
    store_unaligned(m, frame + 0x1c, high);
    // libetc's interrupt manager: RAM effects and the host call.
    sdk::reset_callback(m);
    sdk::set_video_mode(m, 1);
    while crate::mainloop::pending_sdk::cd_init(m) == 0 {
        if m.fault().is_some() {
            break;
        }
    }
    m.host_call(CD_SET_DEBUG, [0; 4]);
    sdk::vsync(m, 3);
    sdk::reset_graph(m, 0);
    let disp = frame + 0x20;
    sdk::set_def_disp_env(m, disp, 0, 0, 0x100, 0x100);
    m.sh(frame + 0x28, 0xfffe);
    m.sh(frame + 0x2a, 0x10);
    m.sh(frame + 0x2e, 0x100);
    sdk::put_disp_env(m, disp);
    m.host_call(SET_DISP_MASK, [1, 0, 0, 0]);
    sdk::init_geom(m, 0x8002_56cc);
    m.host_call(SS_UT_REVERB_OFF, [0; 4]);
    crate::bios::init_card(m, 0);
    crate::bios::start_card(m);
    crate::bios::bu_init(m);
    let ctx = context(m);
    let pads = init_pads(m, at(ctx, 0x50), crate::GP, 2, 3);
    // The kernel leaves v0 alone: PadEnableCom's result.
    let v0 = crate::bios::exit_critical_section(m, pads);
    m.pop_frame(0x48);
    v0
}

/// 800260AC: a0 = eight 0x1A-byte pad records (context +0x50), a1, a2, a3 =
/// words for pad block +0xC8, +0xC4, +0xCC (gp, 2, 3). Clears the 0xD0-byte
/// pad block `[gp+0x10]`, fills its eight 0x10-byte port slots from +0x44
/// (port byte 0..3 or 0x10..0x13, record pointer), and starts the pads:
/// PadInitDirect(block, block + 0x22), PadStartCom, PadEnableCom(3).
/// The three stay recorded host calls; their RAM effects are translated
/// ([`pad_init_direct`], `mainloop::libpad::pad_start_com` and
/// `pad_enable_com`). Returns `v0` (PadEnableCom's previous port mask).
pub fn init_pads(m: &mut Machine, records: u32, a1: u32, a2: u32, a3: u32) -> u32 {
    let block = m.gw(PADS);
    for i in 0..0xd0 {
        m.sb(at(block, i), 0);
    }
    let block = m.gw(PADS);
    m.sw(at(block, 0xc8), a1);
    m.sw(at(block, 0xc4), a2);
    m.sw(at(block, 0xcc), a3);
    let mut record = records;
    let mut offset = 0x44;
    for slot in 0..8u32 {
        let block = m.gw(PADS);
        let entry = at(block, offset);
        let port = if slot < 4 { 0 } else { 0xc } + slot;
        m.sw(at(entry, 4), record);
        record = record.wrapping_add(0x1a);
        offset += 0x10;
        m.sb(entry, port as u8);
    }
    let block = m.gw(PADS);
    let second = at(block, 0x22);
    m.host_call(PAD_INIT_DIRECT, [block, second, 0, 0]);
    pad_init_direct(m, block, second);
    // PadStartCom and PadEnableCom stay recorded host calls (the host starts
    // libpad's VBlank handler); their RAM effects are translated.
    m.host_call(PAD_START_COM, [0; 4]);
    crate::mainloop::libpad::pad_start_com(m);
    m.host_call_result(PAD_ENABLE_COM, [3, 0, 0, 0], 3);
    crate::mainloop::libpad::pad_enable_com(m, 3)
}

// ---- 8009C2A0: libpad PadInitDirect (RAM effects) --------------------------------------

/// libpad's driver words set by PadInitDirect: its state flag, the
/// interrupt-time flag, the port records' base, and the routine pointers.
pub const PAD_STATE: u32 = 0x800b_253c;
pub const PAD_SIO_FLAG: u32 = 0x800b_2550;
pub const PAD_RECORDS_POINTER: u32 = 0x800b_2538;
/// The two 0xF0-byte port records (cleared, then set per port).
pub const PAD_RECORDS: u32 = 0x800c_5a90;
/// Per-port 0x23-byte tables the records point to (+0x3C, +0x40).
const PAD_SEND_TABLES: u32 = 0x800c_5a00;
const PAD_ACT_TABLES: u32 = 0x800c_5a48;
/// _padSetVsyncParam's block (handler pointers at +4, +8; zeros at +0, +0xC).
const PAD_VSYNC_PARAM: u32 = 0x800c_59d0;
/// PadInitDirect's routine pointers (address, value).
const PAD_ROUTINES: [(u32, u32); 7] = [
    (0x800b_2504, 0x8009_c468),
    (0x800b_2508, 0x8009_c400),
    (0x800b_250c, 0x8009_c56c),
    (0x800b_2510, 0x8009_c628),
    (0x800b_2514, 0x8009_c8a4),
    (0x800b_2518, 0x8009_c8dc),
    (0x800b_2528, 0x8009_c55c),
];
/// _padInitDirSeq's routine pointers (address, value).
const PAD_SEQUENCE_ROUTINES: [(u32, u32); 3] = [
    (0x800b_251c, 0x8009_c934),
    (0x800b_2520, 0x8009_cd88),
    (0x800b_2524, 0x8009_ca44),
];

/// 8009C2A0 PadInitDirect(first, second), RAM effects (libpad; the routine
/// touches no device). Clears the state flag and 0x800B2550, installs the
/// direct-mode routines (_padInitDirSeq 8009C900: 0x800B251C..24; then
/// 0x800B2504..18 and 0x800B2528), points 0x800B2538 at the port records
/// 0x800C5A90, clears them (bzero 0x1E0) and sets per port i: `+0x30` the
/// receive buffer (`first`, `second`), `+0xC` = 0, `+0x10` = the record,
/// buffer bytes 0xFF, 0, `+0x3C`/`+0x40` its tables (0x800C5A00 and
/// 0x800C5A48, each plus i * 0x23), `+0x5D..+0x62` = 0xFF. Then
/// _padSetVsyncParam 80099EF8 and state flag 1. Returns 1.
pub fn pad_init_direct(m: &mut Machine, first: u32, second: u32) -> u32 {
    m.push_frame(0x20);
    m.sw(PAD_STATE, 0);
    m.sw(PAD_SIO_FLAG, 0);
    for (address, routine) in PAD_SEQUENCE_ROUTINES {
        m.sw(address, routine);
    }
    for (address, routine) in PAD_ROUTINES[..6].iter().copied() {
        m.sw(address, routine);
    }
    m.sw(PAD_RECORDS_POINTER, PAD_RECORDS);
    let (address, routine) = PAD_ROUTINES[6];
    m.sw(address, routine);
    // bzero 800AC018.
    for i in 0..0x1e0 {
        m.sb(at(PAD_RECORDS, i), 0);
    }
    m.sw(at(PAD_RECORDS, 0x30), first);
    m.sw(at(PAD_RECORDS, 0xf0 + 0x30), second);
    for i in 0..2 {
        let record = at(PAD_RECORDS, i * 0xf0);
        let buffer = m.lw(at(record, 0x30));
        m.sw(at(record, 0xc), 0);
        m.sw(at(record, 0x10), record);
        m.sb(buffer, 0xff);
        let buffer = m.lw(at(record, 0x30));
        m.sb(at(buffer, 1), 0);
        m.sw(at(record, 0x3c), at(PAD_SEND_TABLES, i * 0x23));
        m.sw(at(record, 0x40), at(PAD_ACT_TABLES, i * 0x23));
        for k in 0..6 {
            m.sb(at(record, 0x5d + k), 0xff);
        }
    }
    // _padSetVsyncParam 80099EF8.
    m.sw(at(PAD_VSYNC_PARAM, 4), 0x8009_9f8c);
    m.sw(at(PAD_VSYNC_PARAM, 8), 0x8009_9f24);
    m.sw(PAD_VSYNC_PARAM, 0);
    m.sw(at(PAD_VSYNC_PARAM, 0xc), 0);
    m.sw(PAD_STATE, 1);
    m.pop_frame(0x20);
    1
}

// ---- 80025934: display timing ---------------------------------------------------------

/// 80025934: no arguments. SetGeomOffset(0, 0), SetGeomScreen(0x115),
/// display flags `[[gp+0x2C4]]` = 0xB, the presentation step 80025720
/// (mainloop.rs, through `dispatch`), then root counter 2 (SetRCnt(0xF2000001,
/// 0x1E00, 0x2000), StartRCnt). Returns `v0` (StartRCnt's).
pub fn display_timing(m: &mut Machine) -> u32 {
    init_display_timing(m, BootStop::MainLoop).unwrap_or(0)
}

fn init_display_timing(m: &mut Machine, stop: BootStop) -> Flow {
    sdk::set_geom_offset(m, 0, 0);
    sdk::set_geom_screen(m, 0x115);
    let display = m.gw(DISPLAY);
    m.sw(display, 0xb);
    if stop == BootStop::FirstPresent {
        return Err(Halt::Stop);
    }
    dispatch::call(m, PRESENT, [0x115, 0, 0, 0]);
    halt_on_fault(m)?;
    m.host_call(SET_RCNT, [0xf200_0001, 0x1e00, 0x2000, 0]);
    Ok(m.host_call_result(START_RCNT, [0xf200_0001, 0, 0, 0], 1))
}

/// 80025990: no arguments. In its 0x48-byte frame builds a SpuCommonAttr
/// (+0x10: mask 0x3C3, master volume 0x3FFF/0x3FFF, CD volume 0x7FFF/0x7FFF,
/// CD reverb and mix on) for SpuSetCommonAttr, and a CdlATV (+0x38: FF 00 FF
/// 00) for CdMix. Returns `v0` (CdMix's, 1).
pub fn init_audio_attributes(m: &mut Machine) -> u32 {
    let frame = m.push_frame(0x48);
    let attr = frame + 0x10;
    m.sw(attr, 0x3c3);
    m.sh(attr + 4, 0x3fff);
    m.sh(attr + 6, 0x3fff);
    m.sh(attr + 0x10, 0x7fff);
    m.sh(attr + 0x12, 0x7fff);
    m.sw(attr + 0x14, 1);
    m.sw(attr + 0x18, 1);
    sdk::spu_set_common_attr(m, attr);
    let atv = frame + 0x38;
    m.sb(atv, 0xff);
    m.sb(atv + 1, 0);
    m.sb(atv + 2, 0xff);
    m.sb(atv + 3, 0);
    let v0 = m.host_call_result(CD_MIX, [atv, 0, 0, 0], 1);
    m.pop_frame(0x48);
    v0
}

// ---- 8002B8B0: CD resource files ----------------------------------------------------------

/// 8002B8B0: no arguments. Opens the resource files (8002AC0C with context
/// +0x20) and resets the request queue `[gp+0x48]`: +0x164 = 0xFF, +0x168 =
/// 1, +0, +4, +0x148, +0x160, +0x16C, +0x178 = 0. Returns `v0` (the queue).
pub fn init_resources(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let ctx = context(m);
    init_resource_files(m, at(ctx, 0x20));
    let queue = m.gw(REQUESTS);
    m.sw(at(queue, 0x164), 0xff);
    for offset in [0, 4, 0x148, 0x160] {
        m.sw(at(queue, offset), 0);
    }
    m.sw(at(queue, 0x168), 1);
    m.sw(at(queue, 0x16c), 0);
    m.sw(at(queue, 0x178), 0);
    m.pop_frame(0x18);
    queue
}

/// 8002AC0C: a0 = owner pointer for the CD file block (context +0x20).
/// Stores it at `[gp+0x34]` +0x20, clears +0, +4, +0xC, +8 and +0x24, sets
/// the CD mix (800295B8(0xFF, 0)) and retries 8002A9E4 once per VSync(0)
/// until it succeeds. Returns `v0` (1).
pub fn init_resource_files(m: &mut Machine, owner: u32) -> u32 {
    m.push_frame(0x18);
    let files = m.gw(CD_FILES);
    m.sw(at(files, 0x20), owner);
    for offset in [0, 4, 0xc, 8, 0x24] {
        m.sw(at(files, offset), 0);
    }
    set_cd_volume(m, 0xff, 0);
    let v0 = loop {
        let v0 = open_resource_files(m);
        if v0 != 0 || m.fault().is_some() {
            break v0;
        }
        sdk::vsync(m, 0);
    };
    m.pop_frame(0x18);
    v0
}

/// 800295B8: a0 = volume (clamped to 0..=0x96), a1 = stereo flag. Builds a
/// CdlATV in its 0x20-byte frame (+0x10): with a1, all four entries volume /
/// 2; without, left-to-left and right-to-right = volume, crosses 0. Calls
/// CdMix; returns `v0` (CdMix's, 1).
pub fn set_cd_volume(m: &mut Machine, volume: u32, stereo: u32) -> u32 {
    let frame = m.push_frame(0x20);
    let atv = frame + 0x10;
    let mut volume = volume as i32;
    if volume >= 0x97 {
        volume = 0x96;
    } else if volume < 0 {
        volume = 0;
    }
    if stereo != 0 {
        let half = (volume >> 1) as u8;
        for offset in [3, 2, 1, 0] {
            m.sb(atv + offset, half);
        }
    } else {
        m.sb(atv + 2, volume as u8);
        m.sb(atv, volume as u8);
        m.sb(atv + 3, 0);
        m.sb(atv + 1, 0);
    }
    let v0 = m.host_call_result(CD_MIX, [atv, 0, 0, 0], 1);
    m.pop_frame(0x20);
    v0
}

/// 8002A9E4: no arguments. Header buffers `[gp+0x2F4]` = `[gp+0x44]` and
/// `[gp+0x2F8]` = +0x2000; clears the CD sync, data and ready callbacks;
/// finds BPH, SPH, BPB and SPB (8002A8AC: positions at 0x800BF130, sizes at
/// 0x800BF140); reads four sectors of BPH and of SPH into the buffers
/// (8002A968); checks the 'ACE3' magic, stores the BPB/SPB base sectors
/// (CdPosToInt) and both tables (buffer +0x14) in `[gp+0x34]` +0x10..+0x1C,
/// rebases each table's sector words, copies SPH word +4 to +0xC and checks
/// the version words. Failures set `[gp+0x34]` +0xC = 5 and print. Returns
/// `v0` (1 when done, or 8002A8AC's result when a file is missing).
pub fn open_resource_files(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let v0 = open_resource_files_body(m);
    m.pop_frame(0x18);
    v0
}

fn open_resource_files_body(m: &mut Machine) -> u32 {
    let buffer = m.gw(0x44);
    m.set_gw(0x2f4, buffer);
    m.set_gw(0x2f8, buffer.wrapping_add(0x2000));
    m.host_call(CD_SYNC_CALLBACK, [0; 4]);
    m.host_call(CD_DATA_CALLBACK, [0; 4]);
    m.host_call(CD_READY_CALLBACK, [0; 4]);
    let positions = 0x800b_f130;
    let found = search_files(m, FILE_NAMES, 4, positions, 0x800b_f140);
    if (found as i32) <= 0 {
        let files = m.gw(CD_FILES);
        m.sw(at(files, 0xc), 5);
        return found;
    }
    let first = m.gw(0x2f4);
    read_sectors(m, 4, positions, first);
    let second = m.gw(0x2f8);
    read_sectors(m, 4, positions + 4, second);
    let header = m.gw(0x2f4);
    let magic = m.lw(header);
    let bad = if magic == HEADER_MAGIC {
        let header = m.gw(0x2f8);
        let other = m.lw(header);
        (other != magic).then_some(header)
    } else {
        Some(header)
    };
    if let Some(header) = bad {
        let files = m.gw(CD_FILES);
        m.sw(at(files, 0xc), 5);
        let id = m.lw(header);
        m.host_call_result(PRINTF, [0x8001_03c4, id, HEADER_MAGIC, 0], 0);
        return 1;
    }
    let base = sdk::cd_pos_to_int(m, positions + 8);
    let files = m.gw(CD_FILES);
    m.sw(at(files, 0x10), base);
    let base = sdk::cd_pos_to_int(m, positions + 0xc);
    let files = m.gw(CD_FILES);
    let first = m.gw(0x2f4);
    m.sw(at(files, 0x14), base);
    let second = m.gw(0x2f8);
    m.sw(at(files, 0x18), at(first, 0x14));
    m.sw(at(files, 0x1c), at(second, 0x14));
    let count = m.lw(at(first, 0x10)) as i32;
    let base = m.lw(at(files, 0x10));
    rebase_table(m, at(first, 0x14), count, base);
    let second = m.gw(0x2f8);
    let files = m.gw(CD_FILES);
    let count = m.lw(at(second, 0x10)) as i32;
    let base = m.lw(at(files, 0x14));
    let table = m.lw(at(files, 0x1c));
    rebase_table(m, table, count, base);
    let files = m.gw(CD_FILES);
    let second = m.gw(0x2f8);
    m.sw(at(files, 0xc), 0);
    let flag = m.lw(at(second, 4));
    if flag != 0 {
        m.sw(at(files, 0xc), flag);
    }
    let first = m.gw(0x2f4);
    if m.lw(at(first, 8)) != BPH_VERSION {
        let files = m.gw(CD_FILES);
        m.sw(at(files, 0xc), 5);
        m.host_call_result(PRINTF, [0x8001_03e4, BPH_VERSION, 0, 0], 0);
    }
    let second = m.gw(0x2f8);
    if m.lw(at(second, 8)) != SPH_VERSION {
        let files = m.gw(CD_FILES);
        m.sw(at(files, 0xc), 5);
        m.host_call_result(PRINTF, [0x8001_0400, SPH_VERSION, 0, 0], 0);
    }
    1
}

/// 8002A9E4's loops: add `base` to the second word of `count` 8-byte entries.
fn rebase_table(m: &mut Machine, table: u32, count: i32, base: u32) {
    let mut word = at(table, 4);
    for _ in 0..count.max(0) {
        let value = m.lw(word).wrapping_add(base);
        m.sw(word, value);
        word = word.wrapping_add(8);
    }
}

/// 8002A8AC: a0 = name pointer table, a1 = count, a2 = CdlLOC output, a3 =
/// size output. CdSearchFile into a CdlFILE in its 0x48-byte frame (+0x10)
/// for each name, copying the position (unaligned) and size; a missing file
/// prints and returns -1. Returns `v0` (1).
pub fn search_files(m: &mut Machine, names: u32, count: u32, positions: u32, sizes: u32) -> u32 {
    let frame = m.push_frame(0x48);
    let file = frame + 0x10;
    let v0 = search_files_body(m, file, names, count, positions, sizes);
    m.pop_frame(0x48);
    v0
}

fn search_files_body(
    m: &mut Machine,
    file: u32,
    names: u32,
    count: u32,
    mut positions: u32,
    mut sizes: u32,
) -> u32 {
    if (count as i32) <= 0 {
        return 1;
    }
    let mut name_pointer = names;
    for _ in 0..count {
        let name = m.lw(name_pointer);
        if sdk::cd_search_file(m, file, name) == 0 {
            let name = m.lw(name_pointer);
            m.host_call_result(PRINTF, [0x8001_0378, name, 0, 0], 0);
            return 0xffff_ffff;
        }
        name_pointer = name_pointer.wrapping_add(4);
        let position = load_unaligned(m, file);
        store_unaligned(m, positions, position);
        let size = m.lw(file + 4);
        positions = positions.wrapping_add(4);
        m.sw(sizes, size);
        sizes = sizes.wrapping_add(4);
    }
    1
}

/// 8002A968: a0 = sector count, a1 = CdlLOC, a2 = destination. Retries
/// CdControlB(CdlSetloc, a1, `[gp+0x34]` + 0x28) and CdRead(a0, a2, 0x80)
/// until accepted, then CdReadSync(0, frame +0x10) in its 0x28-byte frame.
/// The device writes the drive status into the Setloc result and the sectors
/// into the destination. Returns `v0` (1).
pub fn read_sectors(m: &mut Machine, count: u32, location: u32, buffer: u32) -> u32 {
    let frame = m.push_frame(0x28);
    loop {
        let files = m.gw(CD_FILES);
        let result = at(files, 0x28);
        if m.host_call_result(CD_CONTROL_B, [2, location, result, 0], 1) != 0 || m.fault().is_some()
        {
            break;
        }
    }
    while m.host_call_result(CD_READ, [count, buffer, 0x80, 0], 1) == 0 {
        if m.fault().is_some() {
            break;
        }
    }
    m.host_call_result(CD_READ_SYNC, [0, frame + 0x10, 0, 0], 0);
    m.pop_frame(0x28);
    1
}

// ---- 800259F8: boot resources ----------------------------------------------------------

/// 800259F8: no arguments. Loads resource 0 (8002C1B8 size) at the arena's
/// next address (80026FD8) through the request queue (8002C384, then
/// 8002C158 pumps it to completion), relocates the container (80026E98) and
/// its parts: the sound bank (8002D544), two TIM lists (80026D8C) and two
/// TIMs (80026D00), then DrawSync(0) and 80027128. Returns `v0` (80027128's).
pub fn load_boot_resources(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let size = dispatch::call(m, RESOURCE_SIZE, [0; 4]);
    let destination = arena::peek(m, size);
    dispatch::call(m, QUEUE_READ, [0, destination, 0, 0]);
    wait_resources(m, 0);
    if m.fault().is_some() {
        // The request pump is missing: the resource never arrived.
        m.pop_frame(0x18);
        return 0;
    }
    let parts = dispatch::call(m, RELOCATE_DIRECTORY, [destination, 0, 0, 0]);
    let bank = m.lw(at(parts, 0x10));
    dispatch::call(m, INSTALL_SOUND_BANK, [bank, 0, 0, 0]);
    let list = m.lw(parts);
    let entries = dispatch::call(m, RELOCATE_DIRECTORY, [list, 0, 0, 0]);
    let list = m.lw(parts);
    let count = m.lw(list);
    dispatch::call(m, LOAD_TIMS, [entries, count, 0, 0]);
    let tim = m.lw(at(parts, 4));
    upload_tim(m, tim);
    let tim = m.lw(at(parts, 8));
    upload_tim(m, tim);
    let list = m.lw(at(parts, 0xc));
    let entries = dispatch::call(m, RELOCATE_DIRECTORY, [list, 0, 0, 0]);
    let list = m.lw(at(parts, 0xc));
    let count = m.lw(list);
    dispatch::call(m, LOAD_TIMS, [entries, count, 0, 0]);
    sdk::draw_sync(m, 0);
    let v0 = init_display_lists(m);
    m.pop_frame(0x18);
    v0
}

/// 8002C158: a0 = poll flag. With a0, returns the pending request count
/// (tail - head); without, pumps the queue (8002C0B4, mainloop.rs) once per
/// VSync(0) until it is empty and returns 0.
pub fn wait_resources(m: &mut Machine, poll: u32) -> u32 {
    m.push_frame(0x18);
    let v0 = if poll != 0 {
        let queue = m.gw(REQUESTS);
        let tail = m.lw(at(queue, 4));
        let head = m.lw(queue);
        tail.wrapping_sub(head)
    } else {
        loop {
            let queue = m.gw(REQUESTS);
            let tail = m.lw(at(queue, 4));
            let head = m.lw(queue);
            if tail == head || m.fault().is_some() {
                break 0;
            }
            dispatch::call(m, PUMP_REQUESTS, [0; 4]);
            sdk::vsync(m, 0);
            // The CD interrupt work the original receives while it waits.
            crate::cdstream::cd_service(m);
        }
    };
    m.pop_frame(0x18);
    v0
}

/// 80026D00: a0 = TIM. One TIM through a TIM_IMAGE in its 0x30-byte frame
/// (+0x10); returns `v0` (the last LoadImage's, else ReadTIM's).
pub fn upload_tim(m: &mut Machine, tim: u32) -> u32 {
    let frame = m.push_frame(0x30);
    let v0 = upload_tim_into(m, frame + 0x10, tim);
    m.pop_frame(0x30);
    v0
}

/// 80026D00's body (`image` is the frame's TIM_IMAGE): OpenTIM, ReadTIM
/// (loading.rs), LoadImage of the pixels then of the CLUT when present.
/// OpenTIM always returns 0 and ReadTIM fails only on a bad id, so their
/// printf paths are only taken for corrupt data. Returns the last `v0`.
fn upload_tim_into(m: &mut Machine, image: u32, tim: u32) -> u32 {
    if dispatch::call(m, OPEN_TIM, [tim, 0, 0, 0]) != 0 {
        m.host_call_result(PRINTF, [0x8001_001c, tim, 0, 0], 0);
    }
    let mut v0 = dispatch::call(m, READ_TIM, [image, 0, 0, 0]);
    if v0 == 0 {
        v0 = m.host_call_result(PRINTF, [0x8001_0034, tim, 0, 0], 0);
    }
    let pixels = m.lw(image + 0x10);
    if pixels != 0 {
        let rect = m.lw(image + 0xc);
        v0 = m.host_call_result(LOAD_IMAGE, [rect, pixels, 0, 0], 0);
    }
    let clut = m.lw(image + 8);
    if clut != 0 {
        let rect = m.lw(image + 4);
        v0 = m.host_call_result(LOAD_IMAGE, [rect, clut, 0, 0], 0);
    }
    v0
}

/// 80027128: no arguments. `[gp+0x2E4]` = 0x100010 and, in `[gp+0x24]`,
/// word +0 = 0 and the GPU words +0xE14..+0xE20 (0x56140000, 0x56540000,
/// 0x56940000, 0). Returns `v0` (0x56940000).
pub fn init_display_lists(m: &mut Machine) -> u32 {
    let lists = m.gw(0x24);
    m.set_gw(0x2e4, 0x0010_0010);
    m.sw(at(lists, 0xe14), 0x5614_0000);
    m.sw(at(lists, 0xe18), 0x5654_0000);
    m.sw(lists, 0);
    m.sw(at(lists, 0xe1c), 0x5694_0000);
    m.sw(at(lists, 0xe20), 0);
    0x5694_0000
}

// ---- 8002D334: sound -------------------------------------------------------------------

/// 8002D334: no arguments. Voice records (8002E1E8), common attributes
/// (8002D374(0)), the eight voice groups (8002ED24) and reverb (8002D5F8)
/// here, the voice assignments (8002EE38). Returns `v0` (8002EE38's, 0).
pub fn init_sound(m: &mut Machine) -> u32 {
    reset_voice_records(m);
    set_sound_attributes(m, 0);
    dispatch::call(m, RESET_VOICE_GROUPS, [0; 4]);
    set_reverb(m);
    dispatch::call(m, RESET_VOICE_ASSIGNMENTS, [0; 4])
}

/// 8002E1E8: no arguments. 24 voice records of 0x14 bytes at 0x800C35F8:
/// byte 0 = 0xFF, 1..3 = 0, +0x10/+0x11 = 0x80, halfword +0x12 = 0. Returns
/// `v0` (0).
pub fn reset_voice_records(m: &mut Machine) -> u32 {
    let mut record = 0x800c_35f8;
    for _ in 0..0x18 {
        m.sb(record, 0xff);
        m.sb(record + 1, 0);
        m.sb(record + 2, 0);
        m.sb(record + 3, 0);
        m.sb(record + 0x10, 0x80);
        m.sb(record + 0x11, 0x80);
        m.sh(record + 0x12, 0);
        record += 0x14;
    }
    0
}

/// 8002D374: a0 = CD-mix flag. SpuCommonAttr at 0x800C35C8: mask 0x3C3,
/// master volume 0x3FFF/0x3FFF, CD reverb +0x14 = 0, CD mix +0x18 = a0 != 0,
/// CD volume +0x10/+0x12 = context byte +0x39D / 2; SpuSetCommonAttr.
/// Returns `v0` (SpuSetCommonAttr's, 0 on the traced path).
pub fn set_sound_attributes(m: &mut Machine, mix: u32) -> u32 {
    let attr = 0x800c_35c8;
    m.sw(attr, 0x3c3);
    m.sh(attr + 4, 0x3fff);
    m.sh(attr + 6, 0x3fff);
    m.sw(attr + 0x18, u32::from(mix != 0));
    let ctx = context(m);
    m.sw(attr + 0x14, 0);
    let volume = u32::from(m.lbu(at(ctx, 0x39d))) >> 1;
    m.sh(attr + 0x12, volume as u16);
    m.sh(attr + 0x10, volume as u16);
    sdk::spu_set_common_attr(m, attr)
}

/// 8002D5F8: no arguments. SpuReverbAttr at 0x800C43B8: mask 1, mode 3,
/// depth 0x1999/0x1999; SpuSetReverbModeParam. Returns `v0` (its result, 0).
pub fn set_reverb(m: &mut Machine) -> u32 {
    let attr = 0x800c_43b8;
    m.sw(attr, 1);
    m.sw(attr + 4, 3);
    m.sh(attr + 8, 0x1999);
    m.sh(attr + 0xa, 0x1999);
    m.host_call_result(SPU_SET_REVERB_MODE_PARAM, [attr, 0, 0, 0], 0)
}

/// 8002D458: a0 = CD-mix flag. SpuCommonAttr 0x800C35C8 mask 0x200 (CD mix
/// only), +0x18 = a0 != 0; SpuSetCommonAttr. Returns `v0` (0 on the traced
/// path: SpuSetCommonAttr's).
pub fn set_master_volume(m: &mut Machine, mix: u32) -> u32 {
    let attr = 0x800c_35c8;
    m.sw(attr, 0x200);
    m.sw(attr + 0x18, u32::from(mix != 0));
    sdk::spu_set_common_attr(m, attr)
}

/// Every routine this module translates, by original address, with the
/// original `a0..a3`, returning `v0` (`dispatch::call` convention).
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let [a0, a1, a2, a3] = args;
    Some(match target {
        0x8002_52d8 => init_globals(m),
        0x8002_6f5c => set_arena(m, a0),
        0x8002_8720 => init_sound_channels(m, a0),
        0x8002_8f5c => clear_slot_flags(m),
        0x8002_83a8 => init_channel(m, a0, a1),
        0x8002_831c => copy_halfwords(m, a0, a1),
        0x8002_8344 => copy_bytes_24(m, a0, a1),
        0x8002_5610 => init_devices(m),
        0x8002_5934 => display_timing(m),
        0x8002_59f8 => load_boot_resources(m),
        0x8002_60ac => init_pads(m, a0, a1, a2, a3),
        0x8002_5990 => init_audio_attributes(m),
        0x8002_b8b0 => init_resources(m),
        0x8002_ac0c => init_resource_files(m, a0),
        0x8002_95b8 => set_cd_volume(m, a0, a1),
        0x8002_a9e4 => open_resource_files(m),
        0x8002_a8ac => search_files(m, a0, a1, a2, a3),
        0x8002_a968 => read_sectors(m, a0, a1, a2),
        0x8002_c158 => wait_resources(m, a0),
        0x8002_6d00 => upload_tim(m, a0),
        0x8002_7128 => init_display_lists(m),
        0x8002_d334 => init_sound(m),
        0x8002_e1e8 => reset_voice_records(m),
        0x8002_d374 => set_sound_attributes(m, a0),
        0x8002_d5f8 => set_reverb(m),
        0x8002_d458 => set_master_volume(m, a0),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GP;
    use std::io::Cursor;

    fn calls(m: &Machine, function: u32) -> Vec<[u32; 4]> {
        m.host_calls
            .iter()
            .filter(|c| c.function == function)
            .map(|c| c.args)
            .collect()
    }

    /// A raw (2352-byte sector) image with an ISO9660 root directory at LBA
    /// 20 listing `files` after `.` and `..`, and data sectors from `data`.
    fn image(files: &[(&str, u32, u32)], data: fn(u32) -> [u8; 2048], sectors: u32) -> Vec<u8> {
        let mut raw = vec![0; sectors as usize * 2352];
        let mut put = |lba: u32, user: &[u8]| {
            let at = lba as usize * 2352 + 24;
            raw[at..at + user.len()].copy_from_slice(user);
        };
        for lba in 0..sectors {
            put(lba, &data(lba));
        }
        let record = |name: &[u8], lba: u32, size: u32| {
            let mut r = vec![0u8; 33 + name.len() + (1 - name.len() % 2)];
            r[0] = r.len() as u8;
            r[2..6].copy_from_slice(&lba.to_le_bytes());
            r[10..14].copy_from_slice(&size.to_le_bytes());
            r[32] = name.len() as u8;
            r[33..33 + name.len()].copy_from_slice(name);
            r
        };
        let mut directory = Vec::new();
        directory.extend(record(&[0], 20, 2048));
        directory.extend(record(&[1], 20, 2048));
        for &(name, lba, size) in files {
            directory.extend(record(name.as_bytes(), lba, size));
        }
        put(20, &directory);
        let mut volume = [0u8; 2048];
        volume[156..156 + 34].copy_from_slice(&record(&[0], 20, 2048));
        put(16, &volume);
        raw
    }

    fn header_sector(lba: u32) -> [u8; 2048] {
        let mut data = [0; 2048];
        let words: [u32; 5] = match lba {
            // BPH: magic, flag, version, -, one table entry.
            467 => [HEADER_MAGIC, 0, BPH_VERSION, 0, 1],
            // SPH (also the second sector of BPH's four-sector read): flag
            // word 7 and a bad version.
            468 => [HEADER_MAGIC, 7, 0x1234, 0, 0],
            _ => [0; 5],
        };
        for (i, w) in words.into_iter().enumerate() {
            data[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
        }
        // Table entry 0: size word, sector word 2.
        data[0x18..0x1c].copy_from_slice(&2u32.to_le_bytes());
        data
    }

    const FILES: [(&str, u32, u32); 4] = [
        ("ACE.BPB;1", 469, 0x10),
        ("ACE.BPH;1", 467, 1036),
        ("ACE.SPB;1", 17388, 0x20),
        ("ACE.SPH;1", 468, 596),
    ];

    fn resource_machine(files: &[(&str, u32, u32)]) -> Machine {
        let mut m = Machine::default();
        m.device = Some(Box::new(DiscDevice::new(
            Cursor::new(image(files, header_sector, 480)),
            CD_FILE_CACHE,
        )));
        m.sw(GP + 0x44, 0x8010_0000);
        m.sw(GP + CD_FILES, 0x8011_0000);
        let names: [&[u8]; 4] = [
            b"\\ACE.BPH;1\0",
            b"\\ACE.SPH;1\0",
            b"\\ACE.BPB;1\0",
            b"\\ACE.SPB;1\0",
        ];
        for (i, name) in names.into_iter().enumerate() {
            let at = 0x8012_0000 + i as u32 * 0x10;
            m.write_bytes(at, name);
            m.sw(FILE_NAMES + i as u32 * 4, at);
        }
        m
    }

    #[test]
    fn draw_env_dtd_follows_video_mode() {
        let mut m = Machine::default();
        let env = 0x8010_0000;
        for (mode, h, dtd) in [(0, 0x100, 1), (0, 0x101, 0), (1, 0x120, 1), (1, 0x121, 0)] {
            m.sw(sdk::VIDEO_MODE, mode);
            assert_eq!(sdk::set_def_draw_env(&mut m, env, 4, 8, 0x140, h), env);
            assert_eq!(m.lbu(env + 0x17), dtd, "mode {mode} h {h:#x}");
            assert_eq!(m.lhu(env + 6), h as u16);
            assert_eq!(m.lhu(env + 0x14), 0xa);
            assert_eq!(m.lbu(env + 0x16), 1);
            assert_eq!([m.lhu(env + 8), m.lhu(env + 0xa)], [4, 8]);
        }
        assert_eq!(sdk::set_video_mode(&mut m, 0), 1);
        assert_eq!(sdk::get_video_mode(&mut m), 0);
    }

    #[test]
    fn cd_volume_clamps_and_stereo() {
        for (volume, stereo, expected) in [
            (0x200, 0, [0x96, 0, 0x96, 0]),
            ((-5i32) as u32, 0, [0, 0, 0, 0]),
            (0x96, 0, [0x96, 0, 0x96, 0]),
            (0x41, 1, [0x20; 4]),
        ] {
            let mut m = Machine::default();
            let sp = m.sp;
            assert_eq!(set_cd_volume(&mut m, volume, stereo), 1);
            assert_eq!(m.sp, sp);
            let atv = sp - 0x20 + 0x10;
            assert_eq!(calls(&m, CD_MIX), vec![[atv, 0, 0, 0]]);
            let bytes: [u8; 4] = std::array::from_fn(|i| m.lbu(atv + i as u32));
            assert_eq!(bytes, expected, "{volume:#x} {stereo}");
        }
    }

    #[test]
    fn channel_kinds_outside_the_table() {
        let mut m = Machine::default();
        for i in 0..0x20 {
            m.sh(0x800a_e600 + i * 2, i as u16 + 1);
        }
        let record = 0x8010_0000;
        // Kinds 0 and 0x11 are outside the table (sltiu): template copy only.
        for kind in [0, 0x11] {
            m.sw(record + 0x58, 0x1234);
            assert_eq!(init_channel(&mut m, kind, record), 0);
            assert_eq!(m.lw(record + 0x58), 0);
            assert_eq!(m.lhu(record + 0x3e), 0x20);
        }
        // A table entry that is not a case body.
        m.sw(CHANNEL_CASES + 4, 0x8002_83ec);
        init_channel(&mut m, 2, record);
        assert_eq!(
            m.take_fault(),
            Err(Fault::Untranslated {
                address: 0x8002_83ec
            })
        );
        assert_eq!(init_sound_channels(&mut m, 0), 0xffff_ffff);
    }

    #[test]
    fn resource_files_success_flag_and_version_paths() {
        let mut m = resource_machine(&FILES);
        assert_eq!(open_resource_files(&mut m), 1);
        assert!(m.take_fault().is_ok());
        // Positions (BCD minute/second/sector, track 0) and sizes.
        assert_eq!(m.lw(0x800b_f130), 0x0017_0800);
        assert_eq!(m.lw(0x800b_f13c), 0x0063_5303);
        assert_eq!(m.lw(0x800b_f140), 1036);
        let files = 0x8011_0000;
        assert_eq!([m.lw(files + 0x10), m.lw(files + 0x14)], [469, 17388]);
        // The BPH table's sector word is rebased onto BPB.
        assert_eq!(m.lw(0x8010_0000 + 0x18), 2 + 469);
        // SPH flag word 7, then the bad SPH version sets 5 and prints.
        assert_eq!(m.lw(files + 0xc), 5);
        assert_eq!(calls(&m, PRINTF), vec![[0x8001_0400, SPH_VERSION, 0, 0]]);
        assert_eq!(m.lw(files + 0x28), 2, "Setloc status");
        assert_eq!(calls(&m, CD_READ).len(), 2);
        assert_eq!(m.sp, Machine::default().sp);
        // libcd's cache entries: BPH is the fourth record after "." and "..".
        assert_eq!(
            calls(&m, CD_SEARCH_FILE).len(),
            4,
            "one CdSearchFile per file"
        );
    }

    #[test]
    fn resource_files_missing_and_bad_magic() {
        let mut m = resource_machine(&FILES[1..2]);
        assert_eq!(open_resource_files(&mut m), 0xffff_ffff);
        assert_eq!(m.lw(0x8011_000c), 5);
        assert_eq!(calls(&m, PRINTF), vec![[0x8001_0378, 0x8012_0010, 0, 0]]);
        assert_eq!(m.sp, Machine::default().sp);

        let mut files = FILES;
        files[3].1 = 470;
        let mut m = resource_machine(&files);
        assert_eq!(open_resource_files(&mut m), 1);
        assert_eq!(m.lw(0x8011_000c), 5);
        assert_eq!(calls(&m, PRINTF), vec![[0x8001_03c4, 0, HEADER_MAGIC, 0]]);
    }

    #[test]
    fn cd_positions_round_trip() {
        let mut m = resource_machine(&FILES);
        m.write_bytes(0x8010_0100, b"\\ACE.SPB;1\0");
        assert_eq!(
            sdk::cd_search_file(&mut m, 0x8010_0000, 0x8010_0100),
            CD_FILE_CACHE + 4 * 24
        );
        assert_eq!(m.lw(0x8010_0000), 0x0063_5303);
        assert_eq!(m.lw(0x8010_0004), 0x20);
        assert_eq!(sdk::cd_pos_to_int(&mut m, 0x8010_0000), 17388);
        m.write_bytes(0x8010_0100, b"\\NONE;1\0");
        assert_eq!(sdk::cd_search_file(&mut m, 0x8010_0000, 0x8010_0100), 0);
        m.device = None;
        m.write_bytes(0x8010_0100, b"\\ACE.SPB;1\0");
        assert_eq!(sdk::cd_search_file(&mut m, 0x8010_0000, 0x8010_0100), 0);
    }

    #[test]
    fn wait_resources_polls_the_queue() {
        let mut m = Machine::default();
        let queue = 0x8015_0000;
        m.sw(GP + REQUESTS, queue);
        m.sw(queue, 3);
        m.sw(queue + 4, 0x11);
        assert_eq!(wait_resources(&mut m, 1), 0xe);
        assert!(m.take_fault().is_ok());
        // The blocking mode runs the request pump (mainloop.rs, translated);
        // it is covered end to end by tests/boot.rs, not on synthetic state.
    }
}
