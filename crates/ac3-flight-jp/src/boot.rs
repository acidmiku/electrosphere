//! Resident boot on the machine: load the unpacked resident payload (0xAB000
//! bytes at 0x80010000) and run what the original runs from resident entry
//! 8002572C (`start`: BSS clear, stack/heap set-up) through `main`'s prologue,
//! `__main` and `ac3_initialize` 80025F68, `VSync(0)` and `SetDispMask(1)`,
//! up to main's loop calling 80025BC8 at 800261D0.
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
//! (the resource queue and file table 8002995C/8002C5E0/8002C7AC, 80027338,
//! 8002D96C, 8002722C, OpenTIM/ReadTIM), `script.rs`/`init_tail.rs`
//! (8002F140, 8002F254), `init_38818.rs` (8002CBCC) and `mainloop.rs`
//! (80025BC8, first called inside 80025DDC, and the request pump 8002C4DC
//! that performs the boot resource load).
//!
//! 800273FC, 8002ACD4, 8002AD90 and 8002AE0C are also translated in
//! `mainloop.rs` (the disc-swap check); those copies are pending removal
//! there (its 8002AE0C takes CdPosToInt as a host call, which returns 0).
use crate::{
    arena,
    common::CONTEXT,
    dispatch,
    machine::{Fault, Machine, SmallData},
    sdk,
};
pub use ac3_machine::disc::{DiscDevice, DiscDeviceState};

/// Load address and size of the unpacked resident payload.
pub const PAYLOAD_ADDRESS: u32 = 0x8001_0000;
pub const PAYLOAD_BYTES: usize = 0xab000;
/// BSS cleared word by word by `start`.
pub const BSS_START: u32 = 0x800b_ce40;
pub const BSS_END: u32 = 0x800c_bfe8;
/// `[gp+0x2D4]`: display control block (flags word, environment selection).
pub const DISPLAY: u32 = 0x2d4;
/// `[gp+0x2E4]`: current draw/display environment pointers.
pub const ENVIRONMENTS: u32 = 0x2e4;
/// `[gp+0x34]`: CD file block (file positions, header buffers, tables).
pub const CD_FILES: u32 = 0x34;
/// `[gp+0x48]`: resource request queue (16 entries of 0x14 bytes from +8).
pub const REQUESTS: u32 = 0x48;
/// `[gp+0x10]`: pad block (0xD0 bytes; PadInitDirect buffers at +0, +0x22).
pub const PADS: u32 = 0x10;

/// 80028848 jump table (16 entries for kinds 1..=16).
const CHANNEL_CASES: u32 = 0x8001_0340;
/// libcd's file cache (24-byte CdlFILE entries); CdSearchFile returns one.
pub const CD_FILE_CACHE: u32 = 0x800c_7ef8;

// SDK and kernel entry points recorded as host calls.
pub const INIT_HEAP: u32 = 0x800a_d6f8;
pub const RESET_CALLBACK: u32 = 0x800a_0778;
pub const CD_INIT: u32 = 0x800a_1228;
pub const CD_SET_DEBUG: u32 = 0x800a_1440;
pub const VSYNC: u32 = 0x800a_2098;
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
pub const SPU_SET_COMMON_ATTR: u32 = 0x800a_13b8;
pub const SPU_SET_REVERB_MODE_PARAM: u32 = 0x8009_e498;
pub const CD_MIX: u32 = 0x800a_18e0;
pub const CD_SYNC_CALLBACK: u32 = 0x800a_14fc;
pub const CD_DATA_CALLBACK: u32 = 0x800a_1940;
pub const CD_READY_CALLBACK: u32 = 0x800a_1510;
pub const CD_SEARCH_FILE: u32 = 0x800a_4c28;
pub const CD_CONTROL_B: u32 = 0x800a_1794;
pub const CD_READ: u32 = 0x800a_4168;
pub const CD_READ_SYNC: u32 = 0x800a_4304;
pub const LOAD_IMAGE: u32 = 0x800a_6910;
pub const PRINTF: u32 = 0x800a_c3e8;
/// mainloop.rs: presentation step and resource request pump.
pub const PRESENT: u32 = 0x8002_5bc8;
pub const PUMP_REQUESTS: u32 = 0x8002_c4dc;
/// Routines of other modules on the boot path (called through `dispatch`).
const RESOURCE_SIZE: u32 = 0x8002_c5e0;
const QUEUE_READ: u32 = 0x8002_c7ac;
const RELOCATE_DIRECTORY: u32 = 0x8002_7338;
const INSTALL_SOUND_BANK: u32 = 0x8002_d96c;
const LOAD_TIMS: u32 = 0x8002_722c;
const OPEN_TIM: u32 = 0x800a_ad48;
const READ_TIM: u32 = 0x800a_ad58;
const RESET_VOICE_GROUPS: u32 = 0x8002_f140;
const RESET_VOICE_ASSIGNMENTS: u32 = 0x8002_f254;
const INIT_STREAM_STATE: u32 = 0x8002_cbcc;

/// Where [`boot_until`] stops.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootStop {
    /// Before 80025DDC calls 80025BC8 (the first 80025BC8 call; capture
    /// `analysis/flight-runtime/boot-001` ends here).
    FirstPresent,
    /// Before main's loop calls 80025BC8 at 800261D0.
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
/// leaves in RAM before jumping to 8002572C). `payload` must be the
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

/// 8002572C `start` and 80025750 `stup1`: no arguments. Clears the BSS
/// 0x800BCE40..0x800CBFE8, sets `$sp` to `[0x800257DC] | 0x80000000` and the
/// heap (InitHeap from PAL 0x800C9DC4 up to the stack, minus `[0x800B473C]` bytes
/// of stack), then runs `main` (800257CC `stup0` calls 800261A8). Returns
/// whether `stop` was reached; main itself never returns.
///
/// `stup1` saves `ra` (the BIOS `Exec` return) at 0x800BCE40 and reloads it
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
    let top = m.lw(0x8002_57d8 + 4);
    m.sp = top | 0x8000_0000;
    let heap = ((BSS_END << 3) >> 3) | 0x8000_0000;
    let stack_size = m.lw(0x800b_473c);
    let heap_size = top
        .wrapping_sub(stack_size)
        .wrapping_sub((BSS_END << 3) >> 3);
    m.host_call(INIT_HEAP, [heap.wrapping_add(4), heap_size, 0, 0]);
    matches!(main(m, stop), Err(Halt::Stop))
}

/// 800261A8 `main` up to its loop: `__main` (80025724, an empty `jr ra`),
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

/// 80025F68 `ac3_initialize`: globals and environments (800257E8), sound
/// channels (80028BC0 on context +0x15C), context +0x120 bit 0, devices
/// (80025B00), display timing (80025DDC), audio attributes (80025E38), CD
/// resource files (8002BCD8), boot resources (80025EA0), sound (8002D75C),
/// stream state (8002CBCC) and master volume (8002D880(1)).
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

// ---- 800257E8: globals and environments ----------------------------------------------

// JP-REVIEW(R0030): changed function: PAL 800252D8 -> JP 800257E8, del:31 imm:17 ins:23 op:9 reg:7, 87 instructions differ (whole routine)
/// 800257E8: no arguments. Sets `[gp+0x2D4]` (display block PAL 0x800BB0A8),
/// `[gp+0x2EC]` (context PAL 0x800BD158), `[gp+0x2DC]` (resources PAL 0x800BE168) and
/// `[gp+0x2E4]` (PAL 0x800BB130); builds three DRAWENV/DISPENV pairs at
/// 0x800BD0D0 (SetDefDrawEnv/SetDefDispEnv, 320x256 at y 0/256 and 320x512),
/// their background colours and screen offsets; initializes the display
/// block and the context fields, the resources' transition pair (1, 0), and
/// the arena (800273FC with context +0x40). Returns `v0` (the context).
// JP-PATCH: replaced by `crate::jp::res_p2_boot::init_globals` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn init_globals(m: &mut Machine) -> u32 {
    crate::jp::res_p2_boot::init_globals(m)
}

/// 800273FC: a0 = arena base. Sets context +0x1C (bump pointer) and +0x18
/// (base); returns `v0` (the context).
pub fn set_arena(m: &mut Machine, base: u32) -> u32 {
    let ctx = context(m);
    m.sw(at(ctx, 0x1c), base);
    m.sw(at(ctx, 0x18), base);
    ctx
}

// ---- 80028BC0: sound channel records ---------------------------------------------------

/// 80028BC0: a0 = five 0x5C-byte channel records (context +0x15C). Clears
/// the six slot bytes at `[gp+0x30]` stride 4 (8002937C) and, for a non-null
/// a0, initializes the records as kinds 1, 2, 4, 8 and 0x10 (80028848).
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

/// 8002937C: no arguments. Clears six bytes at `[gp+0x30]` stride 4;
/// returns `v0` (-1).
pub fn clear_slot_flags(m: &mut Machine) -> u32 {
    let mut address = m.gw(0x30);
    for _ in 0..6 {
        m.sb(address, 0);
        address = address.wrapping_add(4);
    }
    0xffff_ffff
}

/// 80028848: a0 = kind, a1 = record. Clears word +0x58, copies a 32-halfword
/// template into the record (800287BC) and, for kinds 2, 4, 8 and 0x10, a
/// 24-byte template into +0x40 (800287E4); every known kind (1, 2, 4, 8,
/// 0x10, through the jump table at 0x80010340) ORs 0x18 into +0x58 (kind 1
/// also clears halfword +0x50). Returns `v0` (+0x58, or 0 for other kinds).
pub fn init_channel(m: &mut Machine, kind: u32, record: u32) -> u32 {
    let case = kind.wrapping_sub(1);
    m.sw(at(record, 0x58), 0);
    let target = if case < 0x10 {
        m.lw(CHANNEL_CASES + (case << 2))
    } else {
        0x8002_893c
    };
    let templates = match target {
        0x8002_8888 => {
            copy_halfwords(m, record, 0x800b_01e0);
            let flags = m.lw(at(record, 0x58));
            m.sh(at(record, 0x50), 0);
            let flags = flags | 0x18;
            m.sw(at(record, 0x58), flags);
            return flags;
        }
        0x8002_88a4 => (0x800b_0220, 0x800b_02e0),
        0x8002_88c4 => (0x800b_0220, 0x800b_02f8),
        0x8002_88e4 => (0x800b_0260, 0x800b_0310),
        0x8002_8904 => (0x800b_02a0, 0x800b_0328),
        0x8002_893c => return copy_halfwords(m, record, 0x800b_01e0),
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

/// 800287BC: a0 = destination, a1 = source. Copies 32 halfwords; returns
/// `v0` (0).
pub fn copy_halfwords(m: &mut Machine, destination: u32, source: u32) -> u32 {
    for i in 0..0x20 {
        let value = m.lhu(at(source, i * 2));
        m.sh(at(destination, i * 2), value);
    }
    0
}

/// 800287E4: a0 = destination, a1 = source. Copies 24 bytes as unaligned
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

// ---- 80025B00: devices ----------------------------------------------------------------

// JP-REVIEW(R0031): changed function: PAL 80025610 -> JP 80025B00, call-:2 del:18 imm:8 op:2, 28 instructions differ (whole routine)
/// 80025B00: no arguments. Device start-up, in original order: ResetCallback,
/// SetVideoMode(1) (PAL), CdInit (until it succeeds), CdSetDebug(0),
/// VSync(3), ResetGraph(0), a 256x256 DISPENV in its 0x48-byte frame (+0x20,
/// screen x -2, y 16, h 256) through PutDispEnv, SetDispMask(1), InitGeom,
/// SsUtReverbOff, InitCARD(0), StartCARD, _bu_init, the pads (8002654C with
/// context +0x50, gp, 2, 3) and ExitCriticalSection. Returns `v0`
/// (ExitCriticalSection's, 3 on the traced path).
// JP-PATCH: replaced by `crate::jp::res_p2_boot::init_devices` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn init_devices(m: &mut Machine) -> u32 {
    crate::jp::res_p2_boot::init_devices(m)
}

/// 8002654C: a0 = eight 0x1A-byte pad records (context +0x50), a1, a2, a3 =
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

// ---- 8009DDD0: libpad PadInitDirect (RAM effects) --------------------------------------

/// libpad's driver words set by PadInitDirect: its state flag, the
/// interrupt-time flag, the port records' base, and the routine pointers.
pub const PAD_STATE: u32 = 0x800b_46bc;
pub const PAD_SIO_FLAG: u32 = 0x800b_46d0;
pub const PAD_RECORDS_POINTER: u32 = 0x800b_46b8;
/// The two 0xF0-byte port records (cleared, then set per port).
pub const PAD_RECORDS: u32 = 0x800c_7cb8;
/// Per-port 0x23-byte tables the records point to (+0x3C, +0x40).
const PAD_SEND_TABLES: u32 = 0x800c_7c28;
const PAD_ACT_TABLES: u32 = 0x800c_7c70;
/// _padSetVsyncParam's block (handler pointers at +4, +8; zeros at +0, +0xC).
const PAD_VSYNC_PARAM: u32 = 0x800c_7bf8;
/// PadInitDirect's routine pointers (address, value).
const PAD_ROUTINES: [(u32, u32); 7] = [
    (0x800b_4684, 0x8009_df98),
    (0x800b_4688, 0x8009_df30),
    (0x800b_468c, 0x8009_e09c),
    (0x800b_4690, 0x8009_e158),
    (0x800b_4694, 0x8009_e3d4),
    (0x800b_4698, 0x8009_e40c),
    (0x800b_46a8, 0x8009_e08c),
];
/// _padInitDirSeq's routine pointers (address, value).
const PAD_SEQUENCE_ROUTINES: [(u32, u32); 3] = [
    (0x800b_469c, 0x8009_e464),
    (0x800b_46a0, 0x8009_e8b8),
    (0x800b_46a4, 0x8009_e574),
];

/// 8009DDD0 PadInitDirect(first, second), RAM effects (libpad; the routine
/// touches no device). Clears the state flag and 0x800B46D0, installs the
/// direct-mode routines (_padInitDirSeq 8009E430: 0x800B469C..24; then
/// 0x800B4684..18 and 0x800B46A8), points 0x800B46B8 at the port records
/// 0x800C7CB8, clears them (bzero 0x1E0) and sets per port i: `+0x30` the
/// receive buffer (`first`, `second`), `+0xC` = 0, `+0x10` = the record,
/// buffer bytes 0xFF, 0, `+0x3C`/`+0x40` its tables (0x800C7C28 and
/// 0x800C7C70, each plus i * 0x23), `+0x5D..+0x62` = 0xFF. Then
/// _padSetVsyncParam 8009BA28 and state flag 1. Returns 1.
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
    // bzero 800ADBE8.
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
    // _padSetVsyncParam 8009BA28.
    m.sw(at(PAD_VSYNC_PARAM, 4), 0x8009_babc);
    m.sw(at(PAD_VSYNC_PARAM, 8), 0x8009_ba54);
    m.sw(PAD_VSYNC_PARAM, 0);
    m.sw(at(PAD_VSYNC_PARAM, 0xc), 0);
    m.sw(PAD_STATE, 1);
    m.pop_frame(0x20);
    1
}

// ---- 80025DDC: display timing ---------------------------------------------------------

/// 80025DDC: no arguments. SetGeomOffset(0, 0), SetGeomScreen(0x115),
/// display flags `[[gp+0x2D4]]` = 0xB, the presentation step 80025BC8
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

/// 80025E38: no arguments. In its 0x48-byte frame builds a SpuCommonAttr
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

// ---- 8002BCD8: CD resource files ----------------------------------------------------------

/// 8002BCD8: no arguments. Opens the resource files (8002B034 with context
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

/// 8002B034: a0 = owner pointer for the CD file block (context +0x20).
/// Stores it at `[gp+0x34]` +0x20, clears +0, +4, +0xC, +8 and +0x24, sets
/// the CD mix (800299D8(0xFF, 0)) and retries 8002AE0C once per VSync(0)
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

/// 800299D8: a0 = volume (clamped to 0..=0x96), a1 = stereo flag. Builds a
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

// JP-REVIEW(R0032): near function: PAL 8002A9E4 -> JP 8002AE0C, imm:3, 3 instructions differ (whole routine)
/// 8002AE0C: no arguments. Header buffers `[gp+0x304]` = `[gp+0x44]` and
/// `[gp+0x308]` = +0x2000; clears the CD sync, data and ready callbacks;
/// finds BPH, SPH, BPB and SPB (8002ACD4: positions at 0x800C1368, sizes at
/// 0x800C1378); reads four sectors of BPH and of SPH into the buffers
/// (8002AD90); checks the 'ACE3' magic, stores the BPB/SPB base sectors
/// (CdPosToInt) and both tables (buffer +0x14) in `[gp+0x34]` +0x10..+0x1C,
/// rebases each table's sector words, copies SPH word +4 to +0xC and checks
/// the version words. Failures set `[gp+0x34]` +0xC = 5 and print. Returns
/// `v0` (1 when done, or 8002ACD4's result when a file is missing).
// JP-PATCH: replaced by `crate::jp::res_p2_boot::open_resource_files` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn open_resource_files(m: &mut Machine) -> u32 {
    crate::jp::res_p2_boot::open_resource_files(m)
}

/// 8002ACD4: a0 = name pointer table, a1 = count, a2 = CdlLOC output, a3 =
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
            m.host_call_result(PRINTF, [0x8001_038c, name, 0, 0], 0);
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

/// 8002AD90: a0 = sector count, a1 = CdlLOC, a2 = destination. Retries
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

// ---- 80025EA0: boot resources ----------------------------------------------------------

/// 80025EA0: no arguments. Loads resource 0 (8002C5E0 size) at the arena's
/// next address (80027478) through the request queue (8002C7AC, then
/// 8002C580 pumps it to completion), relocates the container (80027338) and
/// its parts: the sound bank (8002D96C), two TIM lists (8002722C) and two
/// TIMs (800271A0), then DrawSync(0) and 800275C8. Returns `v0` (800275C8's).
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

/// 8002C580: a0 = poll flag. With a0, returns the pending request count
/// (tail - head); without, pumps the queue (8002C4DC, mainloop.rs) once per
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

/// 800271A0: a0 = TIM. One TIM through a TIM_IMAGE in its 0x30-byte frame
/// (+0x10); returns `v0` (the last LoadImage's, else ReadTIM's).
pub fn upload_tim(m: &mut Machine, tim: u32) -> u32 {
    let frame = m.push_frame(0x30);
    let v0 = upload_tim_into(m, frame + 0x10, tim);
    m.pop_frame(0x30);
    v0
}

/// 800271A0's body (`image` is the frame's TIM_IMAGE): OpenTIM, ReadTIM
/// (loading.rs), LoadImage of the pixels then of the CLUT when present.
/// OpenTIM always returns 0 and ReadTIM fails only on a bad id, so their
/// printf paths are only taken for corrupt data. Returns the last `v0`.
fn upload_tim_into(m: &mut Machine, image: u32, tim: u32) -> u32 {
    if dispatch::call(m, OPEN_TIM, [tim, 0, 0, 0]) != 0 {
        m.host_call_result(PRINTF, [0x8001_0030, tim, 0, 0], 0);
    }
    let mut v0 = dispatch::call(m, READ_TIM, [image, 0, 0, 0]);
    if v0 == 0 {
        v0 = m.host_call_result(PRINTF, [0x8001_0048, tim, 0, 0], 0);
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

/// 800275C8: no arguments. `[gp+0x2F4]` = 0x100010 and, in `[gp+0x24]`,
/// word +0 = 0 and the GPU words +0xE14..+0xE20 (0x56140000, 0x56540000,
/// 0x56940000, 0). Returns `v0` (0x56940000).
pub fn init_display_lists(m: &mut Machine) -> u32 {
    let lists = m.gw(0x24);
    m.set_gw(0x2f4, 0x0010_0010);
    m.sw(at(lists, 0xe14), 0x5614_0000);
    m.sw(at(lists, 0xe18), 0x5654_0000);
    m.sw(lists, 0);
    m.sw(at(lists, 0xe1c), 0x5694_0000);
    m.sw(at(lists, 0xe20), 0);
    0x5694_0000
}

// ---- 8002D75C: sound -------------------------------------------------------------------

/// 8002D75C: no arguments. Voice records (8002E604), common attributes
/// (8002D79C(0)), the eight voice groups (8002F140) and reverb (8002DA20)
/// here, the voice assignments (8002F254). Returns `v0` (8002F254's, 0).
pub fn init_sound(m: &mut Machine) -> u32 {
    reset_voice_records(m);
    set_sound_attributes(m, 0);
    dispatch::call(m, RESET_VOICE_GROUPS, [0; 4]);
    set_reverb(m);
    dispatch::call(m, RESET_VOICE_ASSIGNMENTS, [0; 4])
}

/// 8002E604: no arguments. 24 voice records of 0x14 bytes at 0x800C5830:
/// byte 0 = 0xFF, 1..3 = 0, +0x10/+0x11 = 0x80, halfword +0x12 = 0. Returns
/// `v0` (0).
pub fn reset_voice_records(m: &mut Machine) -> u32 {
    let mut record = 0x800c_5830;
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

// JP-REVIEW(R0035): near function: PAL 8002D374 -> JP 8002D79C, imm:1, 1 instructions differ (whole routine)
/// 8002D79C: a0 = CD-mix flag. SpuCommonAttr at 0x800C5800: mask 0x3C3,
/// master volume 0x3FFF/0x3FFF, CD reverb +0x14 = 0, CD mix +0x18 = a0 != 0,
/// CD volume +0x10/+0x12 = context byte +0x39D / 2; SpuSetCommonAttr.
/// Returns `v0` (SpuSetCommonAttr's, 0 on the traced path).
// JP-PATCH: replaced by `crate::jp::res_p2_boot::set_sound_attributes` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn set_sound_attributes(m: &mut Machine, mix: u32) -> u32 {
    crate::jp::res_p2_boot::set_sound_attributes(m, mix)
}

/// 8002DA20: no arguments. SpuReverbAttr at 0x800C65F0: mask 1, mode 3,
/// depth 0x1999/0x1999; SpuSetReverbModeParam. Returns `v0` (its result, 0).
pub fn set_reverb(m: &mut Machine) -> u32 {
    let attr = 0x800c_65f0;
    m.sw(attr, 1);
    m.sw(attr + 4, 3);
    m.sh(attr + 8, 0x1999);
    m.sh(attr + 0xa, 0x1999);
    m.host_call_result(SPU_SET_REVERB_MODE_PARAM, [attr, 0, 0, 0], 0)
}

/// 8002D880: a0 = CD-mix flag. SpuCommonAttr 0x800C5800 mask 0x200 (CD mix
/// only), +0x18 = a0 != 0; SpuSetCommonAttr. Returns `v0` (0 on the traced
/// path: SpuSetCommonAttr's).
pub fn set_master_volume(m: &mut Machine, mix: u32) -> u32 {
    let attr = 0x800c_5800;
    m.sw(attr, 0x200);
    m.sw(attr + 0x18, u32::from(mix != 0));
    sdk::spu_set_common_attr(m, attr)
}

/// Every routine this module translates, by original address, with the
/// original `a0..a3`, returning `v0` (`dispatch::call` convention).
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let [a0, a1, a2, a3] = args;
    Some(match target {
        0x8002_57e8 => init_globals(m),
        0x8002_73fc => set_arena(m, a0),
        0x8002_8bc0 => init_sound_channels(m, a0),
        0x8002_937c => clear_slot_flags(m),
        0x8002_8848 => init_channel(m, a0, a1),
        0x8002_87bc => copy_halfwords(m, a0, a1),
        0x8002_87e4 => copy_bytes_24(m, a0, a1),
        0x8002_5b00 => init_devices(m),
        0x8002_5ddc => display_timing(m),
        0x8002_5ea0 => load_boot_resources(m),
        0x8002_654c => init_pads(m, a0, a1, a2, a3),
        0x8002_5e38 => init_audio_attributes(m),
        0x8002_bcd8 => init_resources(m),
        0x8002_b034 => init_resource_files(m, a0),
        0x8002_99d8 => set_cd_volume(m, a0, a1),
        0x8002_ae0c => open_resource_files(m),
        0x8002_acd4 => search_files(m, a0, a1, a2, a3),
        0x8002_ad90 => read_sectors(m, a0, a1, a2),
        0x8002_c580 => wait_resources(m, a0),
        0x8002_71a0 => upload_tim(m, a0),
        0x8002_75c8 => init_display_lists(m),
        0x8002_d75c => init_sound(m),
        0x8002_e604 => reset_voice_records(m),
        0x8002_d79c => set_sound_attributes(m, a0),
        0x8002_da20 => set_reverb(m),
        0x8002_d880 => set_master_volume(m, a0),
        _ => return None,
    })
}
