//! Patches of `boot` routines whose JP instructions differ from PAL
//! (REVIEW.md §2-§3, part `resident-part2`). Each is listed in
//! [`REPLACES`](super::REPLACES).
//!
//! The JP resident boots unpacked from `ACE3.BIN` (no PS-Y), entry 8002572C:
//! `start`, `stup1`, `main` and `ac3_initialize` are exact matches of PAL, so
//! the generated `boot::start` already runs the JP code (BSS 0x800BCE40..
//! 0x800CBFE8, stack table 0x800257D8, stack size word 0x800B473C). What
//! differs is device start-up ([`init_devices`]: NTSC, no 256-line
//! DISPENV), the 240-line environments of [`init_globals`], the archive date
//! words of [`open_resource_files`] and the context layout (a word less below
//! +0x3A0, so most byte fields move by -4).
use crate::{
    boot::{
        CD_FILES, DISPLAY, ENVIRONMENTS, PRINTF, SET_DISP_MASK, SS_UT_REVERB_OFF, init_pads,
        read_sectors, search_files, set_arena,
    },
    common::{CONTEXT, RESOURCES},
    machine::{Machine, SmallData},
    sdk,
};

/// Draw/display environment storage set up by 800257E8 (0x800BD0D0).
const ENVIRONMENT_BASE: u32 = 0x800b_d0d0;
/// 8002AE0C: the four resource file name pointers (BPH, SPH, BPB, SPB).
const FILE_NAMES: u32 = 0x800b_0380;
/// Resource header magic 'ACE3' and the JP archive date words checked by
/// 8002AE0C (PAL 0x1019 and 0x19991019).
const HEADER_MAGIC: u32 = 0x4533_4341;
const BPH_VERSION: u32 = 0x427;
const SPH_VERSION: u32 = 0x1999_0427;
/// 8002AE0C: CdlLOC positions and sizes of the four files (8002ACD4).
const FILE_POSITIONS: u32 = 0x800c_1368;
const FILE_SIZES: u32 = 0x800c_1378;
/// 8002D79C: the SpuCommonAttr block.
const SPU_COMMON_ATTR: u32 = 0x800c_5800;
/// 80025B00: the return address of its InitGeom call (jal at 80025B70),
/// which InitGeom stores.
const INIT_GEOM_RETURN: u32 = 0x8002_5b78;
/// 800B6264: libcd's debug level word.
const CD_DEBUG_LEVEL: u32 = 0x800b_6264;

/// 800A2F70 CdSetDebug(level) -> the previous level: libcd's debug level
/// word 800B6264 (read by CD_cw and the interrupt handler).
pub fn cd_set_debug(m: &mut Machine, level: u32) -> u32 {
    let previous = m.lw(CD_DEBUG_LEVEL);
    m.sw(CD_DEBUG_LEVEL, level);
    previous
}

/// The SDK routines [`init_devices`] calls that no other module claims,
/// claimed so that calls by original address (the test oracle's) run what
/// the translation runs: CdSetDebug ([`cd_set_debug`]); SetDispMask
/// (800A80FC, the display enable: a host call, as for its other caller
/// 800261C8); InitGeom (800A6270, `sdk::init_geom`, whose saved return
/// address is that of its only call, 80025B70); SsUtReverbOff (8009FD48,
/// a host call, as for its other caller 8002D6F0).
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    match target {
        0x800a_2f70 => Some(cd_set_debug(m, args[0])),
        0x800a_80fc => {
            m.host_call(SET_DISP_MASK, [args[0], 0, 0, 0]);
            Some(0)
        }
        0x800a_6270 => {
            sdk::init_geom(m, INIT_GEOM_RETURN);
            Some(0)
        }
        0x8009_fd48 => {
            m.host_call(SS_UT_REVERB_OFF, [0; 4]);
            Some(0)
        }
        _ => None,
    }
}

fn at(base: u32, offset: u32) -> u32 {
    base.wrapping_add(offset)
}

fn context(m: &mut Machine) -> u32 {
    m.gw(CONTEXT)
}

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

/// 800257E8 (JP; PAL 800252D8, changed): no arguments. Sets `[gp+0x2D4]`
/// (display block 0x800BD220), `[gp+0x2EC]` (context 0x800BF2D0),
/// `[gp+0x2DC]` (resources 0x800C039C; PAL base +0x3210, JP +0x32CC) and
/// `[gp+0x2E4]` (0x800BD2A8); builds three DRAWENV/DISPENV pairs at
/// 0x800BD0D0 (SetDefDrawEnv/SetDefDispEnv, 320x240 at y 0/256 and 320x480:
/// the NTSC heights; PAL 256/512), their background colours and the screen
/// offset x -2, y 0 (PAL y 16 and screen heights 256, JP leaves the
/// SetDefDispEnv values); initializes the display block, the context fields
/// (the JP layout: +0x38C..+0x392 halfwords 1, +0x394 word 1, +0x398/+0x399
/// 0xD0, +0x39C 1, +0x3A7 and +0x3A9..+0x3AB 1, the rest cleared, +0x95E,
/// +0x96C, +0x1038 and the +0x1048..+0x1074 block), the resources'
/// transition pair (1, 0), and the arena (800273FC with context +0x40).
/// Returns `v0` (the context).
pub fn init_globals(m: &mut Machine) -> u32 {
    let s0 = ENVIRONMENT_BASE;
    m.set_gw(DISPLAY, s0 + 0x150);
    m.set_gw(CONTEXT, s0 + 0x2200);
    m.set_gw(RESOURCES, s0 + 0x32cc);
    sdk::set_def_draw_env(m, s0, 0, 0, 0x140, 0xf0);
    sdk::set_def_disp_env(m, s0 + 0x114, 0, 0x100, 0x140, 0xf0);
    sdk::set_def_draw_env(m, s0 + 0x5c, 0, 0x100, 0x140, 0xf0);
    sdk::set_def_disp_env(m, s0 + 0x128, 0, 0, 0x140, 0xf0);
    sdk::set_def_draw_env(m, s0 + 0xb8, 0, 0, 0x140, 0x1e0);
    sdk::set_def_disp_env(m, s0 + 0x13c, 0, 0, 0x140, 0x1e0);
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
    m.sh(s0 + 0x11e, 0);
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
    for offset in [0, 0x44, 4, 0x20] {
        m.sw(at(ctx, offset), 0);
    }
    m.sw(at(ctx, 0x34), 0xffff_ffff);
    m.sw(at(ctx, 0x38), 0xffff_ffff);
    m.sw(at(ctx, 0x3c), 0x8001_0000);
    m.sw(at(ctx, 0x40), 0x800c_bfb8);
    m.sb(at(ctx, 0x398), 0xd0);
    let reloaded = context(m);
    for offset in [0x24, 0x28, 0x2c, 0x30] {
        m.sw(at(ctx, offset), 0);
    }
    for offset in [0x38c, 0x38e, 0x390, 0x392] {
        m.sh(at(ctx, offset), 1);
    }
    m.sw(at(ctx, 0x394), 1);
    m.sb(at(reloaded, 0x399), 0xd0);
    for (offset, value) in [
        (0x39a, 0),
        (0x39c, 1),
        (0x39d, 0),
        (0x39e, 0),
        (0x39f, 0),
        (0x3a4, 0),
    ] {
        let ctx = context(m);
        m.sb(at(ctx, offset), value);
    }
    let previous = context(m);
    let ctx = context(m);
    m.sw(at(previous, 0x3a0), 0);
    m.sb(at(ctx, 0x3a5), 0);
    let ctx = context(m);
    m.sb(at(ctx, 0x3a6), 0);
    let ctx = context(m);
    m.sh(at(ctx, 0x3ae), 0);
    m.sb(at(ctx, 0x3a7), 1);
    for (offset, value) in [(0x3a8, 0), (0x3a9, 1), (0x3aa, 1), (0x3ab, 1), (0x95e, 0)] {
        let ctx = context(m);
        m.sb(at(ctx, offset), value);
    }
    let previous = context(m);
    let ctx = context(m);
    m.sh(at(previous, 0x3ac), 0);
    m.sw(at(previous, 0x3b0), 0);
    m.sw(at(previous, 0x3b4), 0);
    m.sh(at(previous, 0x954), 0);
    m.sb(at(ctx, 0x158), 0);
    let next = context(m);
    m.sw(at(ctx, 0x96c), 0);
    m.sw(at(ctx, 0x1038), 0);
    m.sw(at(ctx, 0x120), 0);
    for offset in [0x15b, 0x15a, 0x159] {
        m.sb(at(next, offset), 0);
    }
    let ctx = context(m);
    for offset in [
        0x1048, 0x106c, 0x4c, 0x48, 0x1068, 0x1064, 0x1060, 0x105c, 0x1058,
    ] {
        m.sw(at(ctx, offset), 0);
    }
    let resources = m.gw(RESOURCES);
    for offset in [0x1054, 0x1050, 0x104c, 0x1074, 0x1070] {
        m.sw(at(ctx, offset), 0);
    }
    m.sw(at(resources, 0x70), 1);
    m.sw(at(resources, 0x74), 0);
    let arena = m.lw(at(ctx, 0x40));
    set_arena(m, arena)
}

/// 80025B00 (JP; PAL 80025610, changed): no arguments. Device start-up in
/// original order, in its 0x20-byte frame: an 8-byte copy of gp+8 into the
/// frame (+0x10, never read), ResetCallback, **SetVideoMode(0)** (NTSC; PAL
/// 1), CdInit until it succeeds, CdSetDebug(0), VSync(3), ResetGraph(0),
/// SetDispMask(1) (PAL builds and puts a 256x256 DISPENV between the last
/// two), InitGeom, SsUtReverbOff, InitCARD(0), StartCARD, _bu_init, the pads
/// (8002654C with context +0x50, gp, 2, 3) and ExitCriticalSection. Returns
/// `v0` (ExitCriticalSection's: the kernel leaves PadEnableCom's result).
pub fn init_devices(m: &mut Machine) -> u32 {
    let frame = m.push_frame(0x20);
    let low = load_unaligned(m, crate::GP + 8);
    let high = load_unaligned(m, crate::GP + 0xc);
    store_unaligned(m, frame + 0x10, low);
    store_unaligned(m, frame + 0x14, high);
    sdk::reset_callback(m);
    sdk::set_video_mode(m, 0);
    while crate::mainloop::pending_sdk::cd_init(m) == 0 {
        if m.fault().is_some() {
            break;
        }
    }
    cd_set_debug(m, 0);
    sdk::vsync(m, 3);
    sdk::reset_graph(m, 0);
    m.host_call(SET_DISP_MASK, [1, 0, 0, 0]);
    sdk::init_geom(m, INIT_GEOM_RETURN);
    m.host_call(SS_UT_REVERB_OFF, [0; 4]);
    crate::bios::init_card(m, 0);
    crate::bios::start_card(m);
    crate::bios::bu_init(m);
    let ctx = context(m);
    let pads = init_pads(m, at(ctx, 0x50), crate::GP, 2, 3);
    let v0 = crate::bios::exit_critical_section(m, pads);
    m.pop_frame(0x20);
    v0
}

/// 8002AE0C (JP; PAL 8002A9E4, near, `imm:3`): no arguments,
/// `open_resource_files`. Header buffers `[gp+0x304]` = `[gp+0x44]` and
/// `[gp+0x308]` = +0x2000; clears the CD sync, data and ready callbacks;
/// finds BPH, SPH, BPB and SPB (8002ACD4: positions at 0x800C1368, sizes at
/// 0x800C1378); reads four sectors of BPH and of SPH into the buffers
/// (8002AD90); checks the 'ACE3' magic, stores the BPB/SPB base sectors
/// (CdPosToInt) and both tables (buffer +0x14) in `[gp+0x34]` +0x10..+0x1C,
/// rebases each table's sector words, copies SPH word +4 (the JP disc
/// number) to +0xC and checks the JP archive dates (BPH +8 = 0x427, SPH +8 =
/// 0x19990427; PAL 0x1019, 0x19991019). Failures set `[gp+0x34]` +0xC = 5
/// and print. Returns `v0` (1 when done, or 8002ACD4's result when a file is
/// missing).
pub fn open_resource_files(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let v0 = open_resource_files_body(m);
    m.pop_frame(0x18);
    v0
}

fn open_resource_files_body(m: &mut Machine) -> u32 {
    let buffer = m.gw(0x44);
    m.set_gw(0x304, buffer);
    m.set_gw(0x308, buffer.wrapping_add(0x2000));
    // CdSyncCallback(0), CdDataCallback(0), CdReadyCallback(0): libcd's
    // callback words (800B625C, 800B6260) and the CD DMA callback, as the
    // JP SDK routines keep them.
    crate::mainloop::pending_sdk::cd_sync_callback(m, 0);
    crate::mainloop::pending_sdk::cd_data_callback(m, 0);
    crate::mainloop::pending_sdk::cd_ready_callback(m, 0);
    let found = search_files(m, FILE_NAMES, 4, FILE_POSITIONS, FILE_SIZES);
    if (found as i32) <= 0 {
        let files = m.gw(CD_FILES);
        m.sw(at(files, 0xc), 5);
        return found;
    }
    let first = m.gw(0x304);
    read_sectors(m, 4, FILE_POSITIONS, first);
    let second = m.gw(0x308);
    read_sectors(m, 4, FILE_POSITIONS + 4, second);
    let header = m.gw(0x304);
    let magic = m.lw(header);
    let bad = if magic == HEADER_MAGIC {
        let header = m.gw(0x308);
        let other = m.lw(header);
        (other != magic).then_some(header)
    } else {
        Some(header)
    };
    if let Some(header) = bad {
        let files = m.gw(CD_FILES);
        m.sw(at(files, 0xc), 5);
        let id = m.lw(header);
        m.host_call_result(PRINTF, [0x8001_03d8, id, HEADER_MAGIC, 0], 0);
        return 1;
    }
    let base = sdk::cd_pos_to_int(m, FILE_POSITIONS + 8);
    let files = m.gw(CD_FILES);
    m.sw(at(files, 0x10), base);
    let base = sdk::cd_pos_to_int(m, FILE_POSITIONS + 0xc);
    let files = m.gw(CD_FILES);
    let first = m.gw(0x304);
    m.sw(at(files, 0x14), base);
    let second = m.gw(0x308);
    m.sw(at(files, 0x18), at(first, 0x14));
    m.sw(at(files, 0x1c), at(second, 0x14));
    let count = m.lw(at(first, 0x10)) as i32;
    let base = m.lw(at(files, 0x10));
    rebase_table(m, at(first, 0x14), count, base);
    let second = m.gw(0x308);
    let files = m.gw(CD_FILES);
    let count = m.lw(at(second, 0x10)) as i32;
    let base = m.lw(at(files, 0x14));
    let table = m.lw(at(files, 0x1c));
    rebase_table(m, table, count, base);
    let files = m.gw(CD_FILES);
    let second = m.gw(0x308);
    m.sw(at(files, 0xc), 0);
    let flag = m.lw(at(second, 4));
    if flag != 0 {
        m.sw(at(files, 0xc), flag);
    }
    let first = m.gw(0x304);
    if m.lw(at(first, 8)) != BPH_VERSION {
        let files = m.gw(CD_FILES);
        m.sw(at(files, 0xc), 5);
        m.host_call_result(PRINTF, [0x8001_03f8, BPH_VERSION, 0, 0], 0);
    }
    let second = m.gw(0x308);
    if m.lw(at(second, 8)) != SPH_VERSION {
        let files = m.gw(CD_FILES);
        m.sw(at(files, 0xc), 5);
        m.host_call_result(PRINTF, [0x8001_0414, SPH_VERSION, 0, 0], 0);
    }
    1
}

/// 8002AE0C's loops: add `base` to the second word of `count` 8-byte entries.
fn rebase_table(m: &mut Machine, table: u32, count: i32, base: u32) {
    let mut word = at(table, 4);
    for _ in 0..count.max(0) {
        let value = m.lw(word).wrapping_add(base);
        m.sw(word, value);
        word = word.wrapping_add(8);
    }
}

/// 8002D79C (JP; PAL 8002D374, near, `imm:1`): a0 = CD-mix flag.
/// SpuCommonAttr at 0x800C5800: mask 0x3C3, master volume 0x3FFF/0x3FFF, CD
/// mix +0x18 = a0 != 0, CD reverb +0x14 = 0, CD volume +0x10/+0x12 = context
/// byte +0x399 (PAL +0x39D) / 2; SpuSetCommonAttr. Returns `v0`
/// (SpuSetCommonAttr's).
pub fn set_sound_attributes(m: &mut Machine, mix: u32) -> u32 {
    let attr = SPU_COMMON_ATTR;
    m.sw(attr, 0x3c3);
    m.sh(attr + 4, 0x3fff);
    m.sh(attr + 6, 0x3fff);
    m.sw(attr + 0x18, u32::from(mix != 0));
    let ctx = context(m);
    m.sw(attr + 0x14, 0);
    let volume = u32::from(m.lbu(at(ctx, 0x399))) >> 1;
    m.sh(attr + 0x12, volume as u16);
    m.sh(attr + 0x10, volume as u16);
    sdk::spu_set_common_attr(m, attr)
}
