//! Patches of `init` routines whose JP instructions differ from PAL
//! (REVIEW.md §2, part `resident-part2`). Each is listed in
//! [`REPLACES`](super::REPLACES).
//!
//! JP gameplay initialization (80032754) differs from PAL 80032510 in its
//! frame (0x88 bytes, PAL 0x78), the context layout (the fields from +0x390
//! move by -4, the +0xF80/+0xF88 table counts to +0x103C/+0x1044), a 38-byte
//! aircraft table (PAL 24 bytes) and the initial halves +0x4A/+0x4C of
//! entities 0 and 1 (0x36, PAL 1). The parts of its tail that the generated
//! crate keeps in `init_tail.rs` (80032D04..80032EF4, its review sites) are part
//! of the same routine and translated here; the separate routines it calls
//! (80035740, 80032518, 800324AC, 800323F8, 80032080) stay with their owner
//! and are called through their generated functions, which forward to any
//! patch of theirs.
use crate::{
    arena,
    common::{CONTEXT, ENTITIES, GAMEPLAY, RESOURCES},
    dispatch, init, init_tail,
    machine::{Machine, SmallData},
    sdk, shared,
};

/// `[gp+0x2D4]`: display state block written at the start of 80032754.
const DISPLAY: u32 = 0x2d4;
/// `[gp+0x2D8]`: entity pointer table slot 32 (`[gp+0x2E8]` + 0x80).
const ENTITIES_HIGH: u32 = 0x2d8;
/// Frame size of 80032754 (PAL 0x78); its locals at +0x18 (SVECTOR), +0x20
/// (angles), +0x28 (MATRIX) and +0x48 (the aircraft table) are addressed in
/// guest RAM.
const FRAME: u32 = 0x88;
/// The aircraft table copied into the frame at +0x48: 0x26 bytes in JP
/// (PAL 24).
const AIRCRAFT_TABLE: u32 = 0x8002_34f8;
const AIRCRAFT_TABLE_BYTES: u32 = 0x26;

/// MIPS address arithmetic (`addiu`/`addu`) wraps.
trait At {
    fn at(self, offset: u32) -> u32;
}
impl At for u32 {
    fn at(self, offset: u32) -> u32 {
        self.wrapping_add(offset)
    }
}

/// Sign-extended halfword load (`lh`) as a register value.
fn lh_word(m: &mut Machine, address: u32) -> u32 {
    i32::from(m.lh(address)) as u32
}

/// 80032754 (JP; PAL 80032510, near, `del:1 imm:23 ins:22 op:23 reg:5`):
/// resident gameplay initialization (the state-2 callback before flight),
/// in its 0x88-byte frame. Polls the loader (800357C0) while context half
/// +0x3AC (PAL +0x3B0) is zero, then the parts below, the subsystem
/// initializers and the tail. Returns the callback code of 80032080.
pub fn gameplay_initialization(m: &mut Machine) -> u32 {
    let frame = m.push_frame(FRAME);
    let context = m.gw(CONTEXT);
    if m.lh(context.at(0x3ac)) == 0 {
        // Loading poll; reads no arguments.
        dispatch::call(m, 0x8003_57c0, [0; 4]);
    }
    let camera = begin_flight_state(m);
    initialize_camera(m, camera[0], camera[1], camera[2], camera[3], camera[4]);
    init::reset_view_offsets(m);
    let cue = init::depth_cue_arguments(m);
    init::set_depth_cue(m, cue[0], cue[1], cue[2]);
    init::allocate_depth_buckets(m);
    init::set_light_vectors(m, frame);
    init::primitive_buffers(m);
    dispatch::call(m, 0x8003_8b2c, [0; 4]);
    select_player_aircraft(m, frame);
    for subsystem in [0x8003_a008, 0x8007_da98, 0x8008_4a6c, 0x8006_c588] {
        dispatch::call(m, subsystem, [0; 4]);
    }
    let entities = m.gw(ENTITIES);
    let player = m.lw(entities);
    dispatch::call(m, 0x8006_ce70, [player, 0, 0, 0]);
    dispatch::call(m, 0x8004_d794, [0; 4]);
    let result = finish_gameplay_initialization(m);
    m.pop_frame(FRAME);
    result
}

/// 8003278C..80032A20 (part of 80032754): display +0x14 = 2, +4 = 0, +0 |=
/// 5; context bit 0x10000000; resources +0x70/+0x74 = context +0x3B0/+0x3B4
/// (PAL +0x3B4/+0x3B8); DrawSync(0); the arena reset to resources +0x5C; the
/// 0x2BC-byte gameplay block (`[gp+0x2E0]`: words +0..+0xC and halves
/// +0x29C/+0x29E zero, open bounds at +0x2A0..+0x2B4, the 17 resource words
/// +0x18..+0x58 copied to +0x244, `[+0x250]` word +0xC = resources +8); the
/// 64-entry entity table (`[gp+0x2E8]`, slot 32 at `[gp+0x2D8]`) with records
/// of 0x58 (0, 1), 0x5C (2..31) and 0x94 (32..63) bytes, cleared fields and
/// entity 0 byte +5 bit 0; and gameplay +0x28 = gameplay + 0xB8. Returns the
/// 80077880 arguments `(camera, 0x128, mission0 halves +0xC/+0xE,
/// [resources+0xC] + 4)`.
fn begin_flight_state(m: &mut Machine) -> [u32; 5] {
    let display = m.gw(DISPLAY);
    let resources = m.gw(RESOURCES);
    let flags = m.lw(display);
    m.sw(display.at(0x14), 2);
    let context = m.gw(CONTEXT);
    m.sw(display.at(4), 0);
    m.sw(display, flags | 5);
    let flags = m.lw(context);
    let first = m.lw(context.at(0x3b0));
    m.sw(context, flags | 0x1000_0000);
    m.sw(resources.at(0x70), first);
    let second = m.lw(context.at(0x3b4));
    m.sw(resources.at(0x74), second);
    sdk::draw_sync(m, 0);
    let resources = m.gw(RESOURCES);
    let base = m.lw(resources.at(0x5c));
    arena::reset(m, base);

    let gameplay = arena::alloc(m, 0x2bc);
    let resources = m.gw(RESOURCES);
    m.set_gw(GAMEPLAY, gameplay);
    for offset in [0, 4, 8, 0xc] {
        m.sw(gameplay.at(offset), 0);
    }
    m.sh(gameplay.at(0x29c), 0);
    m.sh(gameplay.at(0x29e), 0);
    for (offset, value) in [
        (0x2a0, 0x8000_0001),
        (0x2a4, 0x7fff_ffff),
        (0x2a8, 0x8000_0001),
        (0x2ac, 0x7fff_ffff),
        (0x2b0, 0x8000_0001),
        (0x2b4, 0x7fff_ffff),
    ] {
        m.sw(gameplay.at(offset), value);
    }
    let mut source = resources.at(0x18);
    let mut target = gameplay.at(0x244);
    while source != resources.at(0x58) {
        let words: [u32; 4] = std::array::from_fn(|i| m.lw(source.at(i as u32 * 4)));
        for (i, word) in words.into_iter().enumerate() {
            m.sw(target.at(i as u32 * 4), word);
        }
        source = source.at(0x10);
        target = target.at(0x10);
    }
    let word = m.lw(source);
    m.sw(target, word);

    let gameplay = m.gw(GAMEPLAY);
    let resources = m.gw(RESOURCES);
    let records = m.lw(gameplay.at(0x250));
    let value = m.lw(resources.at(8));
    m.sw(records.at(0xc), value);
    let table = arena::alloc(m, 0x100);
    m.set_gw(ENTITIES, table);
    m.set_gw(ENTITIES_HIGH, table.at(0x80));
    for index in 0..32u32 {
        let record = arena::alloc(m, if index < 2 { 0x58 } else { 0x5c });
        let table = m.gw(ENTITIES);
        m.sw((index << 2).at(table), record);
    }
    for index in 0..32u32 {
        let record = arena::alloc(m, 0x94);
        let table = m.gw(ENTITIES_HIGH);
        m.sw((index << 2).at(table), record);
    }
    for index in 0..64u32 {
        let table = m.gw(ENTITIES);
        let entity = m.lw((index << 2).at(table));
        m.sw(entity, 0);
        m.sb(entity.at(4), 0);
        m.sb(entity.at(5), 0);
        m.sh(entity.at(6), 0);
        for offset in [8, 0xc, 0x10] {
            m.sw(entity.at(offset), 0);
        }
        for offset in [0x18, 0x1a, 0x1c] {
            m.sh(entity.at(offset), 0);
        }
    }
    let table = m.gw(ENTITIES);
    for index in 0..64u32 {
        let entity = m.lw(table.at(index << 2));
        for offset in [0x20, 0x24, 0x28] {
            m.sw(entity.at(offset), 0);
        }
        for offset in [0x30, 0x32, 0x34] {
            m.sh(entity.at(offset), 0);
        }
        for offset in [0x38, 0x3c, 0x40] {
            m.sw(entity.at(offset), 0);
        }
        for offset in [0x48, 0x4a, 0x4c, 0x4e] {
            m.sh(entity.at(offset), 0);
        }
        m.sw(entity.at(0x50), 0);
        m.sw(entity.at(0x54), 0);
    }
    let table = m.gw(ENTITIES);
    let player = m.lw(table);
    let flags = m.lbu(player.at(5));
    m.sb(player.at(5), flags | 1);

    let gameplay = m.gw(GAMEPLAY);
    let mission = m.lw(gameplay.at(0x244));
    m.sw(gameplay.at(0x28), gameplay.at(0xb8));
    let resources = m.gw(RESOURCES);
    let near = lh_word(m, mission.at(0xc));
    let aircraft = m.lw(resources.at(0xc));
    let far = lh_word(m, mission.at(0xe));
    let camera = m.lw(gameplay.at(0x28));
    [camera, 0x128, near, far, aircraft.at(4)]
}

/// 80032BE4..80032CCC (part of 80032754): copy the 0x26-byte aircraft table
/// at 0x800234F8 into the frame (+0x48; 16-byte blocks then 6 bytes, the
/// word-aligned or the `lwl/lwr` path by the source alignment, the same
/// bytes either way), clear entity 0 byte +4 and set its half +0x48 to the
/// frame byte +0x48 + [context half +0x38E] (PAL +0x392; the selected
/// aircraft).
fn select_player_aircraft(m: &mut Machine, frame: u32) {
    let table = frame.at(0x48);
    for offset in 0..AIRCRAFT_TABLE_BYTES {
        let byte = m.lbu(AIRCRAFT_TABLE.at(offset));
        m.sb(table.at(offset), byte);
    }
    let entities = m.gw(ENTITIES);
    let player = m.lw(entities);
    m.sb(player.at(4), 0);
    let context = m.gw(CONTEXT);
    let index = lh_word(m, context.at(0x38e));
    let aircraft = m.lbu(frame.at(index).at(0x48));
    m.sh(player.at(0x48), u16::from(aircraft));
}

/// 80032D04..80032F10 (part of 80032754): the rest after 8004D794 returns.
/// Returns the callback code from 80032080, which 80032754 returns.
fn finish_gameplay_initialization(m: &mut Machine) -> u32 {
    prepare_phase_entry(m);
    init_tail::enter_mission_phase(m, 0, 0)
}

/// 80032D04..80032EF4 (part of 80032754): entity adjustment, 80035740(1),
/// 80032518(context half +0x38C; PAL +0x390), gameplay +0x2B8 = arena mark,
/// frame buffers (800324AC and phase request 0x4000000 when context byte
/// +0x3A5 (PAL +0x3A9) is set, else 800323F8), and gameplay +4/+8 =
/// resources +0x70/+0x74.
fn prepare_phase_entry(m: &mut Machine) {
    adjust_entity_parameters(m);
    init_tail::update_gameplay_bit0(m, 1);
    let context = m.gw(CONTEXT);
    let mission = i32::from(m.lh(context.at(0x38c)));
    init_tail::set_flight_bounds(m, mission);
    let base = arena::peek(m, 0);
    let gameplay = m.gw(GAMEPLAY);
    let context = m.gw(CONTEXT);
    m.sw(gameplay.at(0x2b8), base);
    if m.lbu(context.at(0x3a5)) != 0 {
        init_tail::allocate_frame_buffers(m, base);
        let resources = m.gw(RESOURCES);
        m.sw(resources.at(0x70), 0x0400_0000);
        m.sw(resources.at(0x74), 0);
    } else {
        init_tail::prepare_frame_buffers(m, base);
    }
    let resources = m.gw(RESOURCES);
    let gameplay = m.gw(GAMEPLAY);
    let phase = m.lw(resources.at(0x70));
    m.sw(gameplay.at(4), phase);
    let argument = m.lw(resources.at(0x74));
    m.sw(gameplay.at(8), argument);
}

/// 80032D04..80032E68 (inline in 80032754): every active entity 2..63
/// (flag 0x4000) gets an amount from its kind byte +4. Flag 0x20 without
/// byte +5 bit 1 uses signed half +2 of the record at word `kind*4` of table
/// `[gameplay+0x258]` (kind below context +0x1044; PAL +0xF88); otherwise it
/// uses `half >> 2` (signed, at least 15) for the record at word `kind*16` of
/// table `[gameplay+0x250]` (kind*4 below context +0x103C; PAL +0xF80). Flag
/// 0x20 entities set halves +0x4A/+0x4C that are not positive to
/// `amount - value`; others add the amount. Entities 0 and 1 then get
/// +0x4A = +0x4C = 0x36 (PAL 1).
fn adjust_entity_parameters(m: &mut Machine) {
    let context = m.gw(CONTEXT);
    let gameplay = m.gw(GAMEPLAY);
    let entities = m.gw(ENTITIES);
    let tables = gameplay.at(0x244);
    for index in 2..64u32 {
        let entity = m.lw(entities.at(index << 2));
        let flags = m.lw(entity);
        if flags & 0x4000 == 0 {
            continue;
        }
        let kind = m.lbu(entity.at(4));
        let amount = if flags & 0x20 != 0 && m.lbu(entity.at(5)) & 2 == 0 {
            if i32::from(kind) >= m.lw(context.at(0x1044)) as i32 {
                continue;
            }
            let table = m.lw(tables.at(0x14));
            let record = m.lw((u32::from(kind) << 2).at(table));
            m.lh(record.at(2)) as u16
        } else {
            if i32::from(kind) << 2 >= m.lw(context.at(0x103c)) as i32 {
                continue;
            }
            let table = m.lw(tables.at(0xc));
            let record = m.lw((u32::from(kind) << 4).at(table));
            (i32::from(m.lhu(record.at(2)) as i16) >> 2).max(15) as u16
        };
        if m.lw(entity) & 0x20 != 0 {
            for offset in [0x4a, 0x4c] {
                let value = m.lh(entity.at(offset));
                let raw = m.lhu(entity.at(offset));
                if value <= 0 {
                    m.sh(entity.at(offset), amount.wrapping_sub(raw));
                }
            }
        } else {
            let first = m.lhu(entity.at(0x4a));
            let second = m.lhu(entity.at(0x4c));
            m.sh(entity.at(0x4a), first.wrapping_add(amount));
            m.sh(entity.at(0x4c), second.wrapping_add(amount));
        }
    }
    let entities = m.gw(ENTITIES);
    for index in 0..2u32 {
        let entity = m.lw(entities.at(index << 2));
        m.sh(entity.at(0x4a), 0x36);
        m.sh(entity.at(0x4c), 0x36);
    }
}

/// 80077880 (JP; PAL 80076210, near, `imm:3`): initialize the camera record
/// at `camera`: screen window halves +0x1C..+0x22 = (-160, -120, 320, 240)
/// (PAL -128 and 256: the NTSC screen), +0x24 = `projection`, +0x28/+0x2A =
/// `near`/`far`, +0x2C..+0x30 = the three halves at `aircraft` << 6, +8 =
/// 0xFFB50000, +0xBC/+0xC0 = -1, the listed state fields zero, +0x50 = mode
/// (1, or 2 when context byte +0x39F (PAL +0x3A3) is set), then 800793C4 on
/// the entity in slot word +0xC8 (0). `aircraft` is the fifth argument (the
/// caller's stack word +0x10). Returns 800793C4's `v0`.
pub fn initialize_camera(
    m: &mut Machine,
    camera: u32,
    projection: u32,
    near: u32,
    far: u32,
    aircraft: u32,
) -> u32 {
    m.sh(camera.at(0x1c), 0xff60);
    m.sh(camera.at(0x1e), 0xff88);
    m.sw(camera, 0);
    m.sh(camera.at(0x24), projection as u16);
    m.sw(camera.at(0xc4), 0);
    m.sw(camera.at(0xc8), 0);
    m.sh(camera.at(0x20), 0x140);
    m.sh(camera.at(0x22), 0xf0);
    m.sh(camera.at(0x28), near as u16);
    m.sh(camera.at(0x2a), far as u16);
    let x = m.lhu(aircraft);
    m.sh(camera.at(0x2c), x << 6);
    let y = m.lhu(aircraft.at(2));
    m.sh(camera.at(0x2e), y << 6);
    let z = m.lhu(aircraft.at(4));
    m.sw(camera.at(8), 0xffb5_0000);
    m.sw(camera.at(4), 0);
    m.sw(camera.at(0xc), 0);
    for offset in [0x14, 0x16, 0x18, 0xb4, 0xb6, 0xb8] {
        m.sh(camera.at(offset), 0);
    }
    m.sw(camera.at(0xbc), 0xffff_ffff);
    m.sw(camera.at(0xc0), 0xffff_ffff);
    m.sw(camera.at(0xec), 0);
    m.sw(camera.at(0xf0), 0);
    for offset in [0x11c, 0x11e, 0x120] {
        m.sh(camera.at(offset), 0);
    }
    m.sh(camera.at(0x30), z << 6);
    m.sw(camera.at(0x10c), 0);
    let context = m.gw(CONTEXT);
    m.sw(camera.at(0x110), 0);
    m.sw(camera.at(0x114), 0);
    for offset in [0x144, 0x146, 0x148] {
        m.sh(camera.at(offset), 0);
    }
    for offset in [
        0x134, 0x138, 0x13c, 0x124, 0x128, 0x12c, 0x14c, 0x150, 0x154,
    ] {
        m.sw(camera.at(offset), 0);
    }
    m.sh(camera.at(0x15c), 0);
    m.sh(camera.at(0x15e), 0);
    let mode = if m.lbu(context.at(0x39f)) != 0 { 2 } else { 1 };
    m.sw(camera.at(0x50), mode);
    let slot = m.lw(camera.at(0xc8));
    let entities = m.gw(ENTITIES);
    let mode = m.lw(camera.at(0x50));
    let entity = m.lw((slot << 2).at(entities));
    shared::set_camera_target(m, camera, mode, entity)
}
