//! Tail of resident gameplay initialization 80032754 (80032D04..80032F10):
//! entity parameter adjustment, 80035740, 80032518, the frame buffers
//! (800323F8, or 800324AC for context byte +0x3A9) and the mission phase
//! entry 80032080 with its subtree. Mission scripts (80054EBC) run through
//! [`crate::script`]. Original snapshot checks are in `tests/init_tail.rs`.
use crate::{
    arena,
    common::{CONTEXT, ENTITIES, GAMEPLAY, RESOURCES, reset_8002ff34},
    init_objects,
    machine::{Fault, GP, Machine, SmallData},
    script, sdk, shared,
};

/// `[gp+0x338]`: mission script pointer table.
const SCRIPTS: u32 = 0x338;
/// `[gp+0x33C]`: per-phase table (bytes at +0x34, words at +0x38).
const PHASE_TABLE: u32 = 0x33c;
/// `[gp+0x340]`: phase state block written by 80053DA0.
const PHASE_STATE: u32 = 0x340;
/// `[gp+0x384]`: 64-byte setup records of entities 0..31 (+0xC kind,
/// +0x10 parameters).
const SETUP_LOW: u32 = 0x384;
/// `[gp+0x388]`: 40-byte setup records of entities 32..63 (+8 kind,
/// +0xC parameters, +0x18 swapped by 80053B24).
const SETUP_HIGH: u32 = 0x388;
/// Phase group bytes indexed by gameplay +8, for entities 2..31 / 32..63.
const GROUPS_LOW: u32 = 0x800b_1fc0;
/// MIPS address arithmetic (`addiu`/`addu`) wraps.
trait At {
    fn at(self, offset: u32) -> u32;
}
impl At for u32 {
    fn at(self, offset: u32) -> u32 {
        self.wrapping_add(offset)
    }
}

fn or_word(m: &mut Machine, address: u32, bits: u32) {
    let value = m.lw(address);
    m.sw(address, value | bits);
}

/// 8002F178 under its former name, still used by `mainloop.rs` (a
/// re-export of the `shared.rs` routine).
pub use crate::shared::reset_channel_group as reset_voice_group;

/// An unexpected jump table target (the original jumps through a table
/// entry no translated path covers).
fn untranslated(m: &mut Machine, address: u32) {
    m.raise(Fault::Untranslated { address });
}

// JP-REVIEW(R0212): near function: PAL 80032510 -> JP 80032754, del:1 imm:23 ins:22 op:23 reg:5, 74 instructions differ (the part at +0x55c)
/// 80032D04..80032F10: the rest of 80032754 after 8004D794 returns. Returns
/// the callback code from 80032080, which 80032754 returns.
pub fn finish_gameplay_initialization(m: &mut Machine) -> u32 {
    prepare_phase_entry(m);
    enter_mission_phase(m, 0, 0)
}

// JP-REVIEW(R0213): near function: PAL 80032510 -> JP 80032754, del:1 imm:23 ins:22 op:23 reg:5, 74 instructions differ (the part at +0x55c)
/// 80032D04..80032EF4: the tail before 80032080: entity adjustment,
/// 80035740(1), 80032518(context half +0x390), gameplay +0x2B8 = arena mark,
/// frame buffers (800324AC and phase request 0x4000000 when context byte
/// +0x3A9 is set, else 800323F8), and gameplay +4/+8 = resources +0x70/+0x74.
pub fn prepare_phase_entry(m: &mut Machine) {
    adjust_entity_parameters(m);
    update_gameplay_bit0(m, 1);
    let context = m.gw(CONTEXT);
    let mission = i32::from(m.lh(context.at(0x390)));
    set_flight_bounds(m, mission);
    let base = arena::peek(m, 0);
    let gameplay = m.gw(GAMEPLAY);
    let context = m.gw(CONTEXT);
    m.sw(gameplay.at(0x2b8), base);
    if m.lbu(context.at(0x3a9)) != 0 {
        allocate_frame_buffers(m, base);
        let resources = m.gw(RESOURCES);
        m.sw(resources.at(0x70), 0x0400_0000);
        m.sw(resources.at(0x74), 0);
    } else {
        prepare_frame_buffers(m, base);
    }
    let resources = m.gw(RESOURCES);
    let gameplay = m.gw(GAMEPLAY);
    let phase = m.lw(resources.at(0x70));
    m.sw(gameplay.at(4), phase);
    let argument = m.lw(resources.at(0x74));
    m.sw(gameplay.at(8), argument);
}

// JP-REVIEW(R0214): near function: PAL 80032510 -> JP 80032754, del:1 imm:23 ins:22 op:23 reg:5, 74 instructions differ (the part at +0x55c)
/// 80032D04..80032E68 (inline in 80032754): every active entity 2..63
/// (flag 0x4000) gets an amount from its kind byte +4. Flag 0x20 without
/// byte +5 bit 1 uses signed half +2 of the record at word `kind*4` of table
/// `[gameplay+0x258]` (kind below context +0xF88); otherwise it uses
/// `half >> 2` (signed, at least 15) for the record at word `kind*16` of
/// table `[gameplay+0x250]` (kind*4 below context +0xF80). Flag 0x20
/// entities set halves +0x4A/+0x4C that are not positive to
/// `amount - value`; others add the amount. Entities 0 and 1 then get
/// +0x4A = +0x4C = 1.
pub fn adjust_entity_parameters(m: &mut Machine) {
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
            if i32::from(kind) >= m.lw(context.at(0xf88)) as i32 {
                continue;
            }
            let table = m.lw(tables.at(0x14));
            let record = m.lw((u32::from(kind) << 2).at(table));
            m.lh(record.at(2)) as u16
        } else {
            if i32::from(kind) << 2 >= m.lw(context.at(0xf80)) as i32 {
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
        m.sh(entity.at(0x4a), 1);
        m.sh(entity.at(0x4c), 1);
    }
}

// JP-REVIEW(R0215): near function: PAL 80035248 -> JP 80035740, imm:1, 1 instructions differ (whole routine)
/// 80035740: set gameplay word 0 bit 0 when `request` is zero, context byte
/// +0x3A2 is set, or word +0x50 of record `[gameplay+0x28]` has bit 0x800;
/// otherwise clear it.
// JP-PATCH: replaced by `crate::jp::res_p1_init_tail::update_gameplay_bit0` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn update_gameplay_bit0(m: &mut Machine, request: u32) {
    crate::jp::res_p1_init_tail::update_gameplay_bit0(m, request)
}

// JP-REVIEW(R0216): near function: PAL 800322D4 -> JP 80032518, imm:2, 2 instructions differ (whole routine)
/// 80032518: flight bounds for `mission` (sign-extended context half
/// +0x390): three (min, max) word pairs at gameplay +0x2A0..+0x2B4 and the
/// bound words +0x7C/+0x80/+0x84 and +0x8C/+0x90/+0x94 of record
/// `[gameplay+0x28]`. Missions 10..37 select a case through the original
/// jump table at 80023520 (special cases for 10, 17, 32, 33, 34 and 37;
/// unlisted missions use the default at 8003270C).
// JP-PATCH: replaced by `crate::jp::res_p1_init_tail::set_flight_bounds` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn set_flight_bounds(m: &mut Machine, mission: i32) {
    crate::jp::res_p1_init_tail::set_flight_bounds(m, mission)
}

/// 800324AC: reset the arena to `base` and allocate the two frame buffer
/// pairs: 0x4000 bytes into gameplay +0x18/+0x1C and 0x1B800 bytes into
/// gameplay +0x20/+0x24. 800323F8 begins with the same instructions.
pub fn allocate_frame_buffers(m: &mut Machine, base: u32) {
    arena::reset(m, base);
    for index in 0..2u32 {
        let table = arena::alloc(m, 0x4000);
        let gameplay = m.gw(GAMEPLAY);
        m.sw(gameplay.at(index << 2).at(0x18), table);
        let packets = arena::alloc(m, 0x1_b800);
        let gameplay = m.gw(GAMEPLAY);
        m.sw(gameplay.at(index << 2).at(0x20), packets);
    }
}

/// 800323F8: 800324AC's frame buffers, flag 0x2000 on every active entity
/// (0x4000) of the 64, then the work heap 8007DB30.
pub fn prepare_frame_buffers(m: &mut Machine, base: u32) {
    allocate_frame_buffers(m, base);
    let entities = m.gw(ENTITIES);
    for index in 0..64u32 {
        let entity = m.lw(entities.at(index << 2));
        let flags = m.lw(entity);
        if flags & 0x4000 != 0 {
            m.sw(entity, flags | 0x2000);
        }
    }
    create_work_heap(m);
}

/// 8007DB30: allocate the 0x2054-byte heap header (`[gp+0x544]`) and give
/// it all remaining arena space but 0x8000 bytes: +0x2050 size, +4 start,
/// +8 end - 0x40, +0xC/+0x10 cursors; +0, +0x1018 and the 64 words
/// +0x18 + 0x20*i are cleared and +0x184C is -1.
pub fn create_work_heap(m: &mut Machine) {
    let header = arena::alloc(m, 0x2054);
    m.set_gw(0x544, header);
    m.sw(header, 0);
    m.sw(header.at(0x1018), 0);
    m.sw(header.at(0x184c), 0xffff_ffff);
    let size = arena::remaining(m).wrapping_add(0xffff_8000);
    let header = m.gw(0x544);
    m.sw(header.at(0x2050), size);
    let start = arena::alloc(m, size);
    let header = m.gw(0x544);
    let size = m.lw(header.at(0x2050));
    m.sw(header.at(4), start);
    let end = start.wrapping_add((size >> 2) << 2).wrapping_sub(0x40);
    let cursor = m.lw(header.at(4));
    m.sw(header.at(8), end);
    m.sw(header.at(0x10), cursor);
    m.sw(header.at(0xc), cursor);
    for index in 0..64u32 {
        m.sw(header.at(0x7f8).wrapping_sub(index << 5), 0);
    }
}

// JP-REVIEW(R0217): near function: PAL 80031E3C -> JP 80032080, imm:2, 2 instructions differ (whole routine)
/// 80032080: enter the mission phase whose code gameplay +4 holds (gameplay
/// +8 is its index) and return its callback code from the table at
/// 800B09D4 (0 for an unknown phase). `leaving` selects an exit hook: only
/// 0x40 does work (80031EDC); 8, 0x200, 0x2000 and 0x10000 have empty hooks
/// (80031E9C, 80031F1C, 80031F5C, 80031FA0). `argument` is the script index
/// of phases 8, 0x40, 0x2000, 0x10000 and 0x80000.
// JP-PATCH: replaced by `crate::jp::res_p1_init_tail::enter_mission_phase` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn enter_mission_phase(m: &mut Machine, leaving: u32, argument: u32) -> u32 {
    crate::jp::res_p1_init_tail::enter_mission_phase(m, leaving, argument)
}

/// Phase script `[[gp+0x338] + index*4 + offset]` through 80054EBC.
fn run_phase_script(m: &mut Machine, index: u32, offset: u32) {
    let scripts = m.gw(SCRIPTS);
    let script = m.lw(scripts.at(index << 2).at(offset));
    script::interpret(m, script);
}

/// 80031E64: phase 2 entry: script word +8 (`index`), then 80038C28.
pub fn enter_phase_2(m: &mut Machine, index: u32) {
    run_phase_script(m, index, 8);
    prepare_phase_entities(m);
}

/// 80031EA4: phase 0x10 entry: script word +0x1D8, then 80038C78.
pub fn enter_phase_10(m: &mut Machine, index: u32) {
    run_phase_script(m, index, 0x1d8);
    save_phase_state(m);
}

/// 80031EFC: phase 0x80 entry: 8002C71C(0).
pub fn enter_phase_80(m: &mut Machine) {
    shared::cancel_stream_requests(m);
}

/// 80031F24: phase 0x400 entry: script word +0x238, then 8005B654.
pub fn enter_phase_400(m: &mut Machine, index: u32) {
    run_phase_script(m, index, 0x238);
    setup_phase_400(m);
}

// JP-REVIEW(R0219): near function: PAL 80059C4C -> JP 8005B654, del:15 imm:3 ins:12 op:7 ref:1 reg:3, 41 instructions differ (whole routine)
/// 8005B654 (phase 0x400 after its script): phase state byte +0x28 = +0x25;
/// stage flags and mode from [`STAGE_MODES`] (`[gp+0x4E4]`, `[gp+0x334]`);
/// 800428EC; the phase record (+0x1C/+0x14: 0xFF/0 for flags 0x6000, 0/-
/// for 0x8000, else 1/0x1518); 800849EC, 8004CE9C, 80073928(flags) unless
/// 0x8000, 8007961C(flags), then 80076DBC (fixed camera), or 800762D0 (with
/// state half +8 = 0) for 0x8000, 800762B0 for 0x1000, else 800762D0;
/// context bit 0x10000000, 80030464 and 8002CC44(0, 0, 0). The player start
/// (a 0x18-byte position/angle block in the original frame at +0x10) comes
/// from the 56-byte records at 800B3B94 (flags 0x300; heading and a
/// `0x6000`/`0x9000`-scaled offset from the 0x4000-per-turn sine table at
/// 800104AC, randomized by context word +0x398), is fixed for flags
/// 0xC00/0x1000, or is the player's own position, saved in 800C79B0 on the
/// first pass (`[gp+0x4E8]` counts passes) and restored on later ones. It is
/// passed to 80072780 unless flags 0x6000; then 8004DC38, 8004E80C and
/// 8003A07C. Callees owned elsewhere are called by address.
///
/// The frame's half +0x26 is never written by the original but is copied to
/// 800C79B0 +0x16 on the first pass; on the machine it holds whatever the
/// guest stack holds at that address.
// JP-PATCH: replaced by `crate::jp::res_p1_init_tail::setup_phase_400` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn setup_phase_400(m: &mut Machine) {
    crate::jp::res_p1_init_tail::setup_phase_400(m)
}

/// The lock-on block 8003A07C resets (JP 800C6D60).
const LOCK: u32 = 0x800c_6d60;

/// 8003A07C: reset the lock-on block: +9 0xFF, +0xC 0x83, the halfword +6
/// and the bytes +8, +0xA, +0xB, +0xD, +0xF and +0x11 zero, in the original
/// store order. Returns the block's address. Called last by 8005B654; it
/// had no owner, so a phase-0x400 setup faulted as untranslated (found by
/// `jp-sdk` through its JP twin 8003A5B0, an exact routine).
pub fn reset_lock(m: &mut Machine) -> u32 {
    m.sb(LOCK + 9, 0xff);
    m.sb(LOCK + 0xf, 0);
    m.sh(LOCK + 6, 0);
    m.sb(LOCK + 8, 0);
    m.sb(LOCK + 0xa, 0);
    m.sb(LOCK + 0xb, 0);
    m.sb(LOCK + 0xc, 0x83);
    m.sb(LOCK + 0xd, 0);
    m.sb(LOCK + 0x11, 0);
    LOCK
}

/// 80031F64: phase 0x4000 entry: 800798A8, then script word +0x218.
pub fn enter_phase_4000(m: &mut Machine, index: u32) {
    reinitialize_camera(m);
    run_phase_script(m, index, 0x218);
}

/// 80031FA8: phase 8 entry: script word +0x18.
pub fn enter_phase_8(m: &mut Machine, index: u32) {
    run_phase_script(m, index, 0x18);
}

/// 80031FD8: phase 0x40 entry: script word +0x1F8.
pub fn enter_phase_40(m: &mut Machine, index: u32) {
    run_phase_script(m, index, 0x1f8);
}

/// 80032010: phase 0x2000 entry: script word +0x248.
pub fn enter_phase_2000(m: &mut Machine, index: u32) {
    run_phase_script(m, index, 0x248);
}

/// 80032040: phase 0x10000 and 0x80000 entry: script word +0x228.
pub fn enter_phase_10000(m: &mut Machine, index: u32) {
    run_phase_script(m, index, 0x228);
}

/// 80031EDC: exit hook of phase 0x40: 800798A8.
pub fn leave_phase_40(m: &mut Machine) {
    reinitialize_camera(m);
}

/// 800798A8: reinitialize the camera: 80077880 ([`crate::init::initialize_camera`])
/// with `([gameplay+0x28], 0x128, half +0xC, half +0xE of [gameplay+0x244],
/// [resources+0xC] + 4)`; the original passes the fifth argument at +0x10 of
/// its 0x20-byte frame.
pub fn reinitialize_camera(m: &mut Machine) {
    let gameplay = m.gw(GAMEPLAY);
    let resources = m.gw(RESOURCES);
    let mission = m.lw(gameplay.at(0x244));
    let aircraft = m.lw(resources.at(0xc));
    let near = m.lh(mission.at(0xc)) as i32 as u32;
    let far = m.lh(mission.at(0xe)) as i32 as u32;
    let camera = m.lw(gameplay.at(0x28));
    crate::init::initialize_camera(m, camera, 0x128, near, far, aircraft.at(4));
}

/// 80038C28: 80053DA0, 8004DB40, 8004E620, 80030464, then context word 0
/// bit 0x10000000.
pub fn prepare_phase_entities(m: &mut Machine) {
    select_phase_record(m);
    activate_group_entities(m);
    activate_group_entities_high(m);
    reset_sound_channels(m);
    let context = m.gw(CONTEXT);
    or_word(m, context, 0x1000_0000);
}

/// 80053DA0: phase state `[gp+0x340]`: +0x10 = 0, byte +0x1C = byte +0x34 of
/// `[gp+0x33C]` + phase; when that byte is 1, +0x14 = word +0x38 + phase*4.
pub fn select_phase_record(m: &mut Machine) {
    let state = m.gw(PHASE_STATE);
    let gameplay = m.gw(GAMEPLAY);
    let table = m.gw(PHASE_TABLE);
    m.sw(state.at(0x10), 0);
    let phase = m.lw(gameplay.at(8));
    let kind = m.lbu(table.at(phase).at(0x34));
    m.sb(state.at(0x1c), kind);
    let gameplay = m.gw(GAMEPLAY);
    let table = m.gw(PHASE_TABLE);
    let phase = m.lw(gameplay.at(8));
    if m.lbu(table.at(phase).at(0x34)) == 1 {
        let state = m.gw(PHASE_STATE);
        let value = m.lw(table.at(phase << 2).at(0x38));
        m.sw(state.at(0x14), value);
    }
}

/// Whether a group byte matches the phase group exactly or by high nibble.
fn in_group(value: u8, group: u8) -> bool {
    value == group || value == group & 0xf0
}

/// 8004DB40: for active entities 2..31 (flag 0x4000, half +0x4E > 0) whose
/// kind bytes +0x1D/+0x1E match the phase group byte 800B1FC0[phase],
/// 8004E0FC and 8004E124 respectively.
pub fn activate_group_entities(m: &mut Machine) {
    let gameplay = m.gw(GAMEPLAY);
    let phase = m.lw(gameplay.at(8));
    let group = m.lbu(phase.at(GROUPS_LOW));
    for index in 2..32u32 {
        let entities = m.gw(ENTITIES);
        let entity = m.lw((index << 2).at(entities));
        if m.lw(entity) & 0x4000 == 0 || m.lh(entity.at(0x4e)) <= 0 {
            continue;
        }
        let setup = m.gw(SETUP_LOW);
        let kind = m.lw((index << 6).at(setup).at(0xc));
        if in_group(m.lbu(kind.at(0x1d)), group) {
            shared::mark_unit(m, index);
        }
        if in_group(m.lbu(kind.at(0x1e)), group) {
            shared::activate_unit(m, index);
        }
    }
}

// JP-REVIEW(R0222): near function: PAL 8004D028 -> JP 8004E620, imm:4 ins:4 op:12, 20 instructions differ (whole routine)
/// 8004E620: 8004DB40 for entities 32..63 (group byte 800B2024[phase],
/// 8004E900/8004E94C), then the specials of mission code byte 800BD02C in
/// phase 0 (0x0D: 80053B5C, 0x2B: 80053BA0, 0x37: 800539D0) or phase 1
/// (0x15: 80053B78), and finally 80051824.
// JP-PATCH: replaced by `crate::jp::res_p1_init_tail::activate_group_entities_high` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn activate_group_entities_high(m: &mut Machine) {
    crate::jp::res_p1_init_tail::activate_group_entities_high(m)
}

/// 80053B5C: halves `gp+0x48C/0x48A/0x488` = 0x4000, `gp+0x494` = 0.
pub fn reset_mission_angles(m: &mut Machine) {
    for offset in [0x48c, 0x48a, 0x488] {
        m.sh(GP + offset, 0x4000);
    }
    m.sh(GP + 0x494, 0);
}

/// 80053B78: mission script `[[gp+0x338] + 0x148]` through 80054EBC.
pub fn run_mission_script_148(m: &mut Machine) {
    let scripts = m.gw(SCRIPTS);
    let script = m.lw(scripts.at(0x148));
    script::interpret(m, script);
}

/// 80053BA0: word `gp+0x490` = 0.
pub fn clear_mission_word(m: &mut Machine) {
    m.set_gw(0x490, 0);
}

/// 80053B24: swap word +0x18 of high setup records `first` and `second`.
pub fn swap_setup_targets(m: &mut Machine, first: u32, second: u32) {
    let setup = m.gw(SETUP_HIGH);
    let a = first.wrapping_mul(0x28).at(setup);
    let b = second.wrapping_mul(0x28).at(setup);
    let from_b = m.lw(b.at(0x18));
    let from_a = m.lw(a.at(0x18));
    m.sw(a.at(0x18), from_b);
    m.sw(b.at(0x18), from_a);
}

/// 800539D0: random permutations of high setup targets: for the four pairs
/// starting at bytes 800B20C8, swap with odds 1/2; for the six triples
/// starting at bytes 800B20CC, one of six orders (`rand() % 6`, original jump
/// table at 80024224). `rand` is the SDK's 800ADD48.
pub fn shuffle_mission_targets(m: &mut Machine) {
    for index in 0..4u32 {
        let first = u32::from(m.lbu(index.at(0x800b_20c8)));
        let second = first.wrapping_add(1);
        if sdk::rand(m) & 1 != 0 {
            swap_setup_targets(m, first, second);
        }
    }
    for index in 0..6u32 {
        let first = u32::from(m.lbu(index.at(0x800b_20cc)));
        let second = first.wrapping_add(1);
        let order = (sdk::rand(m) as i32 % 6) as u32;
        let third = first.wrapping_add(2);
        let case = if order < 6 {
            m.lw((order << 2).at(0x8002_4224))
        } else {
            0x8005_3af0
        };
        match case {
            0x8005_3af0 => {}
            0x8005_3aa0 => swap_setup_targets(m, second, third),
            0x8005_3aa8 => swap_setup_targets(m, first, second),
            0x8005_3ab4 => {
                swap_setup_targets(m, first, second);
                swap_setup_targets(m, second, third);
            }
            0x8005_3ac8 => {
                swap_setup_targets(m, second, third);
                swap_setup_targets(m, first, second);
            }
            0x8005_3ae0 => swap_setup_targets(m, first, third),
            other => untranslated(m, other),
        }
    }
}

/// 80030464: 80030178 and two sound channels (8002F500); returns 0.
pub fn reset_sound_channels(m: &mut Machine) -> u32 {
    reset_8002ff34(m);
    init_sound_channels(m, 2);
    0
}

/// 8002F500: `count` sound channels: count word 800C6610, voice bytes
/// +4/+8/+0xC = 6+3i, 7+3i, 8+3i (0xFF for unused slots below 4) and
/// 0x5C-byte records from 800C6630 (+0 = 0, +8 = -1, +0x24 state, +0x30 and
/// halves +0x50..+0x5A = 0).
pub fn init_sound_channels(m: &mut Machine, count: u32) {
    m.sw(shared::SOUND_SOURCES, count);
    let mut index = 0u32;
    let mut voice = 6u32;
    let mut record = shared::SOUND_SOURCE_RECORDS;
    while (index as i32) < (count as i32) {
        let slot = index.at(shared::SOUND_SOURCES);
        m.sb(slot.at(4), voice as u8);
        m.sb(slot.at(8), voice.wrapping_add(1) as u8);
        m.sb(slot.at(0xc), voice.wrapping_add(2) as u8);
        m.sw(record, 0);
        m.sw(record.at(8), 0xffff_ffff);
        shared::reset_envelope(m, record.at(0x24));
        m.sb(record.at(0x30), 0);
        for offset in [0x5a, 0x58, 0x56, 0x54, 0x52, 0x50] {
            m.sh(record.at(offset), 0);
        }
        record = record.at(0x5c);
        voice = voice.wrapping_add(3);
        index = index.wrapping_add(1);
    }
    while (index as i32) < 4 {
        let slot = index.at(shared::SOUND_SOURCES);
        index = index.wrapping_add(1);
        m.sb(slot.at(4), 0xff);
        m.sb(slot.at(8), 0xff);
        m.sb(slot.at(0xc), 0xff);
    }
}

/// 80038C78 (phase 0x10 after its script): context bit 0x10000000,
/// 8006C530 (owned by `init_objects`), `[gp+0x3E8]` = arena mark, and a 0x98C-byte copy (`[gp+0x3E4]`)
/// of the 99 words of record `[gameplay+0x28]` and the first 8 words of
/// each of the 64 entities.
pub fn save_phase_state(m: &mut Machine) {
    let context = m.gw(CONTEXT);
    or_word(m, context, 0x1000_0000);
    init_objects::reset_slot_table(m);
    let mark = arena::peek(m, 0);
    m.set_gw(0x3e8, mark);
    let copy = arena::alloc(m, 0x98c);
    let gameplay = m.gw(GAMEPLAY);
    let source = m.lw(gameplay.at(0x28));
    m.set_gw(0x3e4, copy);
    shared::copy_words(m, copy, source, 0x63);
    for index in 0..64u32 {
        let copy = m.gw(0x3e4);
        let entities = m.gw(ENTITIES);
        let entity = m.lw((index << 2).at(entities));
        shared::copy_words(m, copy.at(0x18c).at(index << 5), entity, 8);
    }
}

/// Every routine translated here, by original address, with the original
/// `a0..a3`; returns `v0` (0 for routines without a result). The empty
/// hooks of 80032080 (`jr ra`) leave `v0` unchanged in the original; their
/// caller never reads it.
#[allow(unused_variables)]
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let [a0, a1, _, _] = args;
    Some(match target {
        0x8003_2080 => enter_mission_phase(m, a0, a1),
        0x8003_0464 => reset_sound_channels(m),
        _ => {
            match target {
                0x8003_5740 => update_gameplay_bit0(m, a0),
                0x8003_2518 => set_flight_bounds(m, a0 as i32),
                0x8003_24ac => allocate_frame_buffers(m, a0),
                0x8003_23f8 => prepare_frame_buffers(m, a0),
                0x8007_db30 => create_work_heap(m),
                0x8003_1e64 => enter_phase_2(m, a0),
                0x8003_1ea4 => enter_phase_10(m, a0),
                0x8003_1efc => enter_phase_80(m),
                0x8003_1f24 => enter_phase_400(m, a0),
                0x8005_b654 => setup_phase_400(m),
                0x8003_1f64 => enter_phase_4000(m, a0),
                0x8003_1fa8 => enter_phase_8(m, a0),
                0x8003_1fd8 => enter_phase_40(m, a0),
                0x8003_2010 => enter_phase_2000(m, a0),
                0x8003_2040 => enter_phase_10000(m, a0),
                0x8003_1edc => leave_phase_40(m),
                0x8007_98a8 => reinitialize_camera(m),
                0x8003_8c28 => prepare_phase_entities(m),
                0x8005_3da0 => select_phase_record(m),
                0x8004_db40 => activate_group_entities(m),
                0x8004_e620 => activate_group_entities_high(m),
                0x8005_3b5c => reset_mission_angles(m),
                0x8005_3b78 => run_mission_script_148(m),
                0x8005_3ba0 => clear_mission_word(m),
                0x8005_39d0 => shuffle_mission_targets(m),
                0x8005_3b24 => swap_setup_targets(m, a0, a1),
                0x8002_f500 => init_sound_channels(m, a0),
                0x8003_8c78 => save_phase_state(m),
                0x8003_1e9c | 0x8003_1f1c | 0x8003_1f5c | 0x8003_1fa0 | 0x8003_2008
                | 0x8003_2070 | 0x8003_2078 => {}
                _ => return None,
            }
            0
        }
    })
}

/// Original addresses claimed by [`dispatch`].
pub const TRANSLATED: [u32; 38] = [
    0x8005_b654,
    0x8003_a07c,
    0x8003_5740,
    0x8003_2518,
    0x8003_24ac,
    0x8003_23f8,
    0x8007_db30,
    0x8003_2080,
    0x8003_1e64,
    0x8003_1ea4,
    0x8003_1efc,
    0x8003_1f24,
    0x8003_1f64,
    0x8003_1fa8,
    0x8003_1fd8,
    0x8003_2010,
    0x8003_2040,
    0x8003_1edc,
    0x8003_1e9c,
    0x8003_1f1c,
    0x8003_1f5c,
    0x8003_1fa0,
    0x8003_2008,
    0x8003_2070,
    0x8003_2078,
    0x8007_98a8,
    0x8003_8c28,
    0x8005_3da0,
    0x8004_db40,
    0x8004_e620,
    0x8005_3b5c,
    0x8005_3b78,
    0x8005_3ba0,
    0x8005_39d0,
    0x8005_3b24,
    0x8003_0464,
    0x8002_f500,
    0x8003_8c78,
];
