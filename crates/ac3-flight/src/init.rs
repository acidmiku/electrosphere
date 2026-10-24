//! Resident gameplay initialization 80032510 on the machine model, with the
//! direct callees it owns: the camera initializer 80076210 (its camera
//! target routine 80077D54 is `shared.rs`), depth cueing 8005D0C4, depth
//! buckets 8005D390 and the primitive buffers 8004AF9C. Subsystem initializers owned by other modules
//! are called by address through [`dispatch::call`]; the tail after 8004C1A0
//! is [`init_tail`]. Original snapshot checks are in `tests/init_callees.rs`.
use crate::{
    arena,
    common::{CONTEXT, ENTITIES, GAMEPLAY, RESOURCES},
    dispatch, init_tail,
    machine::{Machine, SmallData},
    sdk, shared,
};
use ac3_gte::ctrl;

/// `[gp+0x2C4]`: display state block written at the start of 80032510.
const DISPLAY: u32 = 0x2c4;
/// `[gp+0x2C8]`: entity pointer table slot 32 (`[gp+0x2D8]` + 0x80).
const ENTITIES_HIGH: u32 = 0x2c8;
/// Frame size of 80032510; its locals at +0x18 (SVECTOR), +0x20 (angles),
/// +0x28 (MATRIX) and +0x48 (24-byte table) are addressed in guest RAM.
const FRAME: u32 = 0x78;
/// The 24-byte aircraft table copied into the frame at +0x48.
const AIRCRAFT_TABLE: u32 = 0x8002_34e4;

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

/// 80032510: resident gameplay initialization (the state-2 callback before
/// flight). Returns the callback code of 80031E3C (0x10000007 on the
/// traced path).
pub fn gameplay_initialization(m: &mut Machine) -> u32 {
    let frame = m.push_frame(FRAME);
    let context = m.gw(CONTEXT);
    if m.lh(context.at(0x3b0)) == 0 {
        // Loading poll; reads no arguments.
        dispatch::call(m, 0x8003_54b8, [0; 4]);
    }
    let camera = begin_flight_state(m);
    initialize_camera(m, camera[0], camera[1], camera[2], camera[3], camera[4]);
    reset_view_offsets(m);
    let cue = depth_cue_arguments(m);
    set_depth_cue(m, cue[0], cue[1], cue[2]);
    allocate_depth_buckets(m);
    set_light_vectors(m, frame);
    primitive_buffers(m);
    dispatch::call(m, 0x8003_8818, [0; 4]);
    select_player_aircraft(m, frame);
    for subsystem in [0x8003_9a68, 0x8007_c41c, 0x8008_3414, 0x8006_adb8] {
        dispatch::call(m, subsystem, [0; 4]);
    }
    let entities = m.gw(ENTITIES);
    let player = m.lw(entities);
    dispatch::call(m, 0x8006_b6a0, [player, 0, 0, 0]);
    dispatch::call(m, 0x8004_c1a0, [0; 4]);
    let result = init_tail::finish_gameplay_initialization(m);
    m.pop_frame(FRAME);
    result
}

/// 80032548..800327DC: display +0x14 = 2, +4 = 0, +0 |= 5; context bit
/// 0x10000000; resources +0x70/+0x74 = context +0x3B4/+0x3B8; DrawSync(0);
/// the arena reset to resources +0x5C; the 0x2BC-byte gameplay block
/// (`[gp+0x2D0]`: words +0..+0xC and halves +0x29C/+0x29E zero, open bounds
/// at +0x2A0..+0x2B4, the 17 resource words +0x18..+0x58 copied to +0x244,
/// `[+0x250]` word +0xC = resources +8); the 64-entry entity table
/// (`[gp+0x2D8]`, slot 32 at `[gp+0x2C8]`) with records of 0x58 (0, 1),
/// 0x5C (2..31) and 0x94 (32..63) bytes, cleared fields and entity 0 byte +5
/// bit 0; and gameplay +0x28 = gameplay + 0xB8. Returns the 80076210
/// arguments `(camera, 0x128, mission0 halves +0xC/+0xE, [resources+0xC] + 4)`.
pub fn begin_flight_state(m: &mut Machine) -> [u32; 5] {
    let display = m.gw(DISPLAY);
    let resources = m.gw(RESOURCES);
    let flags = m.lw(display);
    m.sw(display.at(0x14), 2);
    let context = m.gw(CONTEXT);
    m.sw(display.at(4), 0);
    m.sw(display, flags | 5);
    let flags = m.lw(context);
    let first = m.lw(context.at(0x3b4));
    m.sw(context, flags | 0x1000_0000);
    m.sw(resources.at(0x70), first);
    let second = m.lw(context.at(0x3b8));
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

/// 800327E8..80032848: gameplay words +0x48/+0x44/+0x40/+0x68/+0x64/+0x60 =
/// 0, SetGeomOffset(0, 0) and SetGeomScreen(camera half +0x24).
pub fn reset_view_offsets(m: &mut Machine) {
    let gameplay = m.gw(GAMEPLAY);
    for offset in [0x48, 0x44, 0x40, 0x68, 0x64, 0x60] {
        m.sw(gameplay.at(offset), 0);
    }
    sdk::set_geom_offset(m, 0, 0);
    let gameplay = m.gw(GAMEPLAY);
    let camera = m.lw(gameplay.at(0x28));
    let screen = lh_word(m, camera.at(0x24));
    sdk::set_geom_screen(m, screen);
}

/// 80032830..80032844: 8005D0C4 arguments: camera halves +0x28 (near),
/// +0x2A (far) and +0x24 (projection).
pub fn depth_cue_arguments(m: &mut Machine) -> [u32; 3] {
    let gameplay = m.gw(GAMEPLAY);
    let camera = m.lw(gameplay.at(0x28));
    let near = lh_word(m, camera.at(0x28));
    let far = lh_word(m, camera.at(0x2a));
    let projection = lh_word(m, camera.at(0x24));
    [near, far, projection]
}

/// 80032850..80032874: a 0x24-byte table in gameplay +0x288, filled by
/// 8005D390 from mission0 halves +0xC/+0xE.
pub fn allocate_depth_buckets(m: &mut Machine) {
    let table = arena::alloc(m, 0x24);
    let gameplay = m.gw(GAMEPLAY);
    let mission = m.lw(gameplay.at(0x244));
    m.sw(gameplay.at(0x288), table);
    let near = lh_word(m, mission.at(0xc));
    let far = lh_word(m, mission.at(0xe));
    depth_buckets(m, near, far, table);
}

/// 80032878..8003298C: lighting from mission0 (`[gameplay+0x244]`): the
/// light colour matrix at gameplay +0x8C (mission halves +0x40/+0x42/+0x44,
/// +0x50/+0x52/+0x54, other elements zero), gameplay +0x6C..+0x7C zero,
/// words +0xAC/+0xB0/+0xB4 = signed halves +0x46/+0x48/+0x4A, and the two
/// light directions at +0x6C and +0x72: (0, 0, 0x1000) rotated by
/// RotMatrixYXZ of angles (+0x3C, +0x3E, 0) and (+0x4C, +0x4E, 0). `frame`
/// is the 80032510 frame (SVECTOR at +0x18, angles at +0x20, MATRIX at
/// +0x28).
pub fn set_light_vectors(m: &mut Machine, frame: u32) {
    let gameplay = m.gw(GAMEPLAY);
    let (unit, angles, matrix) = (frame.at(0x18), frame.at(0x20), frame.at(0x28));
    m.sh(unit, 0);
    m.sh(unit.at(2), 0);
    m.sh(unit.at(4), 0x1000);
    let mission = m.lw(gameplay.at(0x244));
    let colours = gameplay.at(0x8c);
    for (source, target) in [(0x40, 0), (0x42, 6), (0x44, 0xc), (0x50, 2), (0x52, 8)] {
        let value = m.lhu(mission.at(source));
        m.sh(colours.at(target), value);
    }
    let last = m.lhu(mission.at(0x54));
    let directions = gameplay.at(0x6c);
    for offset in [4, 0xa, 0x10] {
        m.sh(colours.at(offset), 0);
    }
    m.sh(colours.at(0xe), last);
    for offset in (0..=0x10).step_by(2) {
        m.sh(directions.at(offset), 0);
    }
    for (source, target) in [(0x46, 0), (0x48, 4), (0x4a, 8)] {
        let value = lh_word(m, mission.at(source));
        m.sw(gameplay.at(0xac).at(target), value);
    }
    for (first, second, out) in [(0x3c, 0x3e, directions), (0x4c, 0x4e, gameplay.at(0x72))] {
        let x = m.lhu(mission.at(first));
        m.sh(angles, x);
        let y = m.lhu(mission.at(second));
        m.sh(angles.at(4), 0);
        m.sh(angles.at(2), y);
        sdk::rot_matrix_yxz(m, angles, matrix);
        sdk::apply_matrix_sv(m, matrix, unit, out);
    }
}

/// 800329A0..80032A34: copy the 24-byte table at 800234E4 into the frame
/// (+0x48), clear entity 0 byte +4 and set its half +0x48 to table byte
/// [context half +0x392] (the selected aircraft).
pub fn select_player_aircraft(m: &mut Machine, frame: u32) {
    let table = frame.at(0x48);
    for offset in (0..24).step_by(4) {
        let bytes: [u8; 4] = std::array::from_fn(|i| m.lbu(AIRCRAFT_TABLE.at(offset + i as u32)));
        m.sw(table.at(offset), u32::from_le_bytes(bytes));
    }
    let entities = m.gw(ENTITIES);
    let player = m.lw(entities);
    m.sb(player.at(4), 0);
    let context = m.gw(CONTEXT);
    let index = lh_word(m, context.at(0x392));
    let aircraft = m.lbu(frame.at(index).at(0x48));
    m.sh(player.at(0x48), u16::from(aircraft));
}

/// 80076210: initialize the camera record at `camera`: screen window halves
/// +0x1C..+0x22 = (-160, -128, 320, 256), +0x24 = `projection`, +0x28/+0x2A =
/// `near`/`far`, +0x2C..+0x30 = the three halves at `aircraft` << 6, +8 =
/// 0xFFB50000, +0xBC/+0xC0 = -1, the listed state fields zero, +0x50 = mode
/// (1, or 2 when context byte +0x3A3 is set), then 80077D54 on the entity in
/// slot word +0xC8 (0). `aircraft` is the fifth argument (the caller's
/// stack word +0x10). Returns 80077D54's `v0`.
pub fn initialize_camera(
    m: &mut Machine,
    camera: u32,
    projection: u32,
    near: u32,
    far: u32,
    aircraft: u32,
) -> u32 {
    m.sh(camera.at(0x1c), 0xff60);
    m.sh(camera.at(0x1e), 0xff80);
    m.sw(camera, 0);
    m.sh(camera.at(0x24), projection as u16);
    m.sw(camera.at(0xc4), 0);
    m.sw(camera.at(0xc8), 0);
    m.sh(camera.at(0x20), 0x140);
    m.sh(camera.at(0x22), 0x100);
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
    let mode = if m.lbu(context.at(0x3a3)) != 0 { 2 } else { 1 };
    m.sw(camera.at(0x50), mode);
    let slot = m.lw(camera.at(0xc8));
    let entities = m.gw(ENTITIES);
    let mode = m.lw(camera.at(0x50));
    let entity = m.lw((slot << 2).at(entities));
    shared::set_camera_target(m, camera, mode, entity)
}

/// Low 64 bits of a 64-bit product (the `multu`/`mult` sequence of 8005D0C4).
fn mul64(a: i64, b: i64) -> i64 {
    a.wrapping_mul(b)
}

/// Saturation of 8005D0C4 to `-0x7FFFFFFF..=0x7FFFFFFF`.
fn saturate(value: i64) -> u32 {
    value.clamp(-0x7fff_ffff, 0x7fff_ffff) as i32 as u32
}

/// 8005D0C4: GTE depth cueing for `near`, `far` and `projection` (signed
/// words) through the SDK's `__divdi3`. The slope is
/// `((near - far) << 24) / near * projection / far` and DQA is
/// `2^32 / slope`, saturated to a halfword when it does not fit. DQB is
/// `(-DQA << 16) * projection / near`, or for a saturated DQA
/// `0x1000000 - (DQA << 16) * projection / far` (the near quotient is still
/// computed and discarded), saturated to +-0x7FFFFFFF. Returns DQB.
pub fn set_depth_cue(m: &mut Machine, near: u32, far: u32, projection: u32) -> u32 {
    let (near, far, projection) = (
        i64::from(near as i32),
        i64::from(far as i32),
        i64::from(projection as i32),
    );
    let difference = i64::from((near as i32).wrapping_sub(far as i32));
    let first = sdk::divdi3(m, difference * 0x0100_0000, near);
    let slope = sdk::divdi3(m, mul64(first, projection), far);
    let mut dqa = sdk::divdi3(m, 0x1_0000_0000, slope) as i32;
    let scaled = |value: i32| mul64(i64::from(value) << 16, projection);
    let dqb = if (dqa as u32).wrapping_add(0x8000) > 0xffff {
        dqa = if dqa > 0 { 0x7fff } else { -0x8000 };
        sdk::divdi3(m, scaled(dqa.wrapping_neg()), near);
        let far_term = sdk::divdi3(m, scaled(dqa), far);
        0x0100_0000_i64.wrapping_sub(far_term)
    } else {
        sdk::divdi3(m, scaled(dqa.wrapping_neg()), near)
    };
    let dqb = saturate(dqb);
    m.gte.ctc2(ctrl::DQA, dqa as u32);
    m.gte.ctc2(ctrl::DQB, dqb);
    dqb
}

/// 8005D390: 16 depth bucket halves at `out`: ((i+1)*(far-near) / 16 (toward
/// zero) + near) >> 3, limited to 0..=0xFFD, then 0xFFE. Returns 0xFFE.
pub fn depth_buckets(m: &mut Machine, near: u32, far: u32, out: u32) -> u32 {
    let span = far.wrapping_sub(near) as i32;
    for index in 0..16u32 {
        let product = (index as i32 + 1).wrapping_mul(span);
        let rounded = if product < 0 {
            product.wrapping_add(15)
        } else {
            product
        };
        let mut value = ((rounded >> 4).wrapping_add(near as i32)) >> 3;
        if value >= 0xffe {
            value = 0xffd;
        }
        if value < 0 {
            value = 0;
        }
        m.sh(out.at(index << 1), value as u16);
    }
    m.sh(out.at(0x20), 0xffe);
    0xffe
}

/// 8004AF9C: two 0x788-byte primitive buffers and their 0x120-byte control
/// block (`[gp+0x470]`; the last buffer remains in `[gp+0x46C]`). Each
/// buffer holds 32 FT4 headers (len 9, code 0x2C) at stride 0x28 from +0x280,
/// 32 SPRT headers (len 4, code 0x64) at stride 0x14 from +0, and a DR_TPAGE
/// header (len 1, word E1000656) at +0x780.
pub fn primitive_buffers(m: &mut Machine) {
    let control = arena::alloc(m, 0x120);
    m.set_gw(0x470, control);
    for index in 0..2 {
        let buffer = arena::alloc(m, 0x788);
        m.set_gw(0x46c, buffer);
        let control = m.gw(0x470);
        m.sw(control + index * 4, buffer);
        for i in 0..32 {
            let at = m.gw(0x46c) + i * 0x28;
            m.sb(at + 0x283, 9);
            m.sb(at + 0x287, 0x2c);
        }
        for i in 0..32 {
            let at = m.gw(0x46c) + i * 0x14;
            m.sb(at + 3, 4);
            m.sb(at + 7, 100);
        }
        let buffer = m.gw(0x46c);
        m.sb(buffer + 0x783, 1);
        m.sw(buffer + 0x784, 0xe100_0656);
    }
    let control = m.gw(0x470);
    for i in 0..32 {
        m.sb(control + i * 8 + 8, 0);
    }
    for (offset, value) in [
        (0x10c, 0),
        (0x108, 0),
        (0x110, 0),
        (0x114, 0),
        (0x118, 0),
        (0x11c, 0xff),
    ] {
        m.sw(control + offset, value);
    }
}

/// Every routine translated here, by original address, with the original
/// `a0..a3` (80076210's fifth argument from the caller's stack word +0x10);
/// returns `v0` (0 for routines without a result).
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let [a0, a1, a2, a3] = args;
    Some(match target {
        0x8003_2510 => gameplay_initialization(m),
        0x8007_6210 => {
            let aircraft = m.lw(m.sp.at(0x10));
            initialize_camera(m, a0, a1, a2, a3, aircraft)
        }
        0x8005_d0c4 => set_depth_cue(m, a0, a1, a2),
        0x8005_d390 => depth_buckets(m, a0, a1, a2),
        0x8004_af9c => {
            primitive_buffers(m);
            0
        }
        _ => return None,
    })
}

/// Original addresses claimed by [`dispatch`].
pub const TRANSLATED: [u32; 5] = [
    0x8003_2510,
    0x8007_6210,
    0x8005_d0c4,
    0x8005_d390,
    0x8004_af9c,
];
