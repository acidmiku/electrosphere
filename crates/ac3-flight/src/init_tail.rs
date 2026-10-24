//! Tail of resident gameplay initialization 80032510 (80032A6C..80032C78):
//! entity parameter adjustment, 80035248, 800322D4, the frame buffers
//! (800321B4, or 80032268 for context byte +0x3A9) and the mission phase
//! entry 80031E3C with its subtree. Mission scripts (80053704) run through
//! [`crate::script`]. Original snapshot checks are in `tests/init_tail.rs`.
use crate::{
    arena,
    common::{CONTEXT, ENTITIES, GAMEPLAY, RESOURCES, reset_8002ff34, reset_800501f8},
    dispatch, init_objects,
    machine::{Fault, GP, Machine, SmallData},
    script, sdk, shared,
};

/// `[gp+0x328]`: mission script pointer table.
const SCRIPTS: u32 = 0x328;
/// `[gp+0x32C]`: per-phase table (bytes at +0x34, words at +0x38).
const PHASE_TABLE: u32 = 0x32c;
/// `[gp+0x330]`: phase state block written by 80052760.
const PHASE_STATE: u32 = 0x330;
/// `[gp+0x374]`: 64-byte setup records of entities 0..31 (+0xC kind,
/// +0x10 parameters).
const SETUP_LOW: u32 = 0x374;
/// `[gp+0x378]`: 40-byte setup records of entities 32..63 (+8 kind,
/// +0xC parameters, +0x18 swapped by 800524EC).
const SETUP_HIGH: u32 = 0x378;
/// Phase group bytes indexed by gameplay +8, for entities 2..31 / 32..63.
const GROUPS_LOW: u32 = 0x800b_00fc;
const GROUPS_HIGH: u32 = 0x800b_0160;
/// Mission code byte tested by 8004D028.
const MISSION_CODE: u32 = 0x800b_aeb4;
/// 80031E3C result words, one per phase code.
const PHASE_RESULTS: u32 = 0x800a_ee04;
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

/// 8002ED5C under its former name, still used by `mainloop.rs` (a
/// re-export of the `shared.rs` routine).
pub use crate::shared::reset_channel_group as reset_voice_group;

/// An unexpected jump table target (the original jumps through a table
/// entry no translated path covers).
fn untranslated(m: &mut Machine, address: u32) {
    m.raise(Fault::Untranslated { address });
}

/// 80032A6C..80032C78: the rest of 80032510 after 8004C1A0 returns. Returns
/// the callback code from 80031E3C, which 80032510 returns.
pub fn finish_gameplay_initialization(m: &mut Machine) -> u32 {
    prepare_phase_entry(m);
    enter_mission_phase(m, 0, 0)
}

/// 80032A6C..80032C5C: the tail before 80031E3C: entity adjustment,
/// 80035248(1), 800322D4(context half +0x390), gameplay +0x2B8 = arena mark,
/// frame buffers (80032268 and phase request 0x4000000 when context byte
/// +0x3A9 is set, else 800321B4), and gameplay +4/+8 = resources +0x70/+0x74.
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

/// 80032A6C..80032BD0 (inline in 80032510): every active entity 2..63
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

/// 80035248: set gameplay word 0 bit 0 when `request` is zero, context byte
/// +0x3A2 is set, or word +0x50 of record `[gameplay+0x28]` has bit 0x800;
/// otherwise clear it.
pub fn update_gameplay_bit0(m: &mut Machine, request: u32) {
    if request != 0 {
        let context = m.gw(CONTEXT);
        if m.lbu(context.at(0x3a2)) == 0 {
            let gameplay = m.gw(GAMEPLAY);
            let record = m.lw(gameplay.at(0x28));
            if m.lw(record.at(0x50)) & 0x800 == 0 {
                let flags = m.lw(gameplay);
                m.sw(gameplay, flags & 0xffff_fffe);
                return;
            }
        }
    }
    let gameplay = m.gw(GAMEPLAY);
    or_word(m, gameplay, 1);
}

/// 800322D4: flight bounds for `mission` (sign-extended context half
/// +0x390): three (min, max) word pairs at gameplay +0x2A0..+0x2B4 and the
/// bound words +0x7C/+0x80/+0x84 and +0x8C/+0x90/+0x94 of record
/// `[gameplay+0x28]`. Missions 10..37 select a case through the original
/// jump table at 800234FC (special cases for 10, 17, 32, 33, 34 and 37;
/// unlisted missions use the default at 800324C8).
pub fn set_flight_bounds(m: &mut Machine, mission: i32) {
    const LOW: u32 = 0x8000_0001;
    const HIGH: u32 = 0x7fff_ffff;
    let index = (mission as u32).wrapping_sub(10);
    let gameplay = m.gw(GAMEPLAY);
    let bounds = m.lw(gameplay.at(0x28));
    let case = if index < 0x1c {
        m.lw((index << 2).at(0x8002_34fc))
    } else {
        0x8003_24c8
    };
    let pairs = |m: &mut Machine, values: [u32; 6]| {
        for (offset, value) in [0x2a0, 0x2a4, 0x2a8, 0x2ac, 0x2b0, 0x2b4]
            .into_iter()
            .zip(values)
        {
            m.sw(gameplay.at(offset), value);
        }
    };
    let limits = |m: &mut Machine, values: [u32; 6]| {
        for (offset, value) in [0x7c, 0x80, 0x84, 0x8c, 0x90, 0x94].into_iter().zip(values) {
            m.sw(bounds.at(offset), value);
        }
    };
    match case {
        0x8003_2308 => {
            let flags = m.lw(gameplay);
            pairs(m, [LOW, HIGH, LOW, 0, LOW, HIGH]);
            m.sw(gameplay, flags | 0x18);
            limits(m, [LOW, 0xff6e_0000, LOW, HIGH, 0xffef_8000, HIGH]);
        }
        0x8003_2368 => {
            let flags = m.lw(gameplay);
            m.sw(gameplay.at(0x2a8), 0xff48_a000);
            for (offset, value) in [
                (0x2a0, LOW),
                (0x2a4, HIGH),
                (0x2ac, 0x0177_0000),
                (0x2b0, LOW),
                (0x2b4, HIGH),
            ] {
                m.sw(gameplay.at(offset), value);
            }
            m.sw(gameplay, flags | 0x10);
            limits(
                m,
                [
                    0xde96_e000,
                    0xff5d_4000,
                    0xea90_6000,
                    0x056f_0000,
                    0xffef_8000,
                    0x1176_4000,
                ],
            );
        }
        0x8003_23f4 => {
            let flags = m.lw(gameplay);
            m.sw(gameplay.at(0x2a8), 0xff48_a000);
            for (offset, value) in [
                (0x2a0, LOW),
                (0x2a4, HIGH),
                (0x2ac, 0x0177_0000),
                (0x2b0, LOW),
                (0x2b4, HIGH),
            ] {
                m.sw(gameplay.at(offset), value);
            }
            m.sw(gameplay, flags | 0x10);
            limits(m, [LOW, 0xff5d_4000, LOW, HIGH, 0xffef_8000, HIGH]);
        }
        0x8003_2464 => {
            pairs(m, [LOW, HIGH, LOW, HIGH, LOW, HIGH]);
            let bound_flags = m.lw(bounds);
            limits(m, [LOW, LOW, LOW, HIGH, HIGH, HIGH]);
            m.sw(bounds, bound_flags | 0x1000);
            or_word(m, gameplay, 0x300);
        }
        0x8003_24c8 => {
            limits(m, [LOW, LOW, LOW, HIGH, 0, HIGH]);
            pairs(m, [LOW, HIGH, LOW, HIGH, LOW, HIGH]);
        }
        other => untranslated(m, other),
    }
}

/// 80032268: reset the arena to `base` and allocate the two frame buffer
/// pairs: 0x4000 bytes into gameplay +0x18/+0x1C and 0x1B800 bytes into
/// gameplay +0x20/+0x24. 800321B4 begins with the same instructions.
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

/// 800321B4: 80032268's frame buffers, flag 0x2000 on every active entity
/// (0x4000) of the 64, then the work heap 8007C4B4.
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

/// 8007C4B4: allocate the 0x2054-byte heap header (`[gp+0x54C]`) and give
/// it all remaining arena space but 0x8000 bytes: +0x2050 size, +4 start,
/// +8 end - 0x40, +0xC/+0x10 cursors; +0, +0x1018 and the 64 words
/// +0x18 + 0x20*i are cleared and +0x184C is -1.
pub fn create_work_heap(m: &mut Machine) {
    let header = arena::alloc(m, 0x2054);
    m.set_gw(0x54c, header);
    m.sw(header, 0);
    m.sw(header.at(0x1018), 0);
    m.sw(header.at(0x184c), 0xffff_ffff);
    let size = arena::remaining(m).wrapping_add(0xffff_8000);
    let header = m.gw(0x54c);
    m.sw(header.at(0x2050), size);
    let start = arena::alloc(m, size);
    let header = m.gw(0x54c);
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

/// Phase codes of gameplay +4 that 80031E3C maps to a result word
/// (index into the table at 800AEE04).
const PHASE_RESULT_SLOTS: [(u32, u32); 15] = [
    (1, 0),
    (2, 1),
    (0x10, 2),
    (0x80, 3),
    (0x400, 4),
    (0x4000, 5),
    (0x2_0000, 6),
    (0x10_0000, 7),
    (0x20_0000, 8),
    (0x400_0000, 14),
    (0x1000_0000, 11),
    (0x40_0000, 9),
    (0x80_0000, 10),
    (0x100_0000, 12),
    (0x200_0000, 13),
];

/// 80031E3C: enter the mission phase whose code gameplay +4 holds (gameplay
/// +8 is its index) and return its callback code from the table at
/// 800AEE04 (0 for an unknown phase). `leaving` selects an exit hook: only
/// 0x40 does work (80031C98); 8, 0x200, 0x2000 and 0x10000 have empty hooks
/// (80031C58, 80031CD8, 80031D18, 80031D5C). `argument` is the script index
/// of phases 8, 0x40, 0x2000, 0x10000 and 0x80000.
pub fn enter_mission_phase(m: &mut Machine, leaving: u32, argument: u32) -> u32 {
    let gameplay = m.gw(GAMEPLAY);
    let phase = m.lw(gameplay.at(4));
    let index = m.lw(gameplay.at(8));
    if leaving == 0x40 {
        leave_phase_40(m);
    }
    // Entry hooks; 0x200 (80031DC4), 0x100000 (80031E2C) and 0x200000
    // (80031E34) are empty.
    match phase {
        2 => enter_phase_2(m, index),
        0x10 => enter_phase_10(m, index),
        0x80 => enter_phase_80(m),
        0x400 => enter_phase_400(m, index),
        0x4000 => enter_phase_4000(m, index),
        8 => enter_phase_8(m, argument),
        0x40 => enter_phase_40(m, argument),
        0x2000 => enter_phase_2000(m, argument),
        0x1_0000 | 0x8_0000 => enter_phase_10000(m, argument),
        _ => {}
    }
    let Some(&(_, slot)) = PHASE_RESULT_SLOTS.iter().find(|&&(code, _)| code == phase) else {
        return 0;
    };
    let result_at = (slot << 2).at(PHASE_RESULTS);
    match phase {
        1 => {
            let result = m.lw(result_at);
            shared::reset_cd_queue(m, 0, 0);
            result
        }
        0x1000_0000 => {
            let context = m.gw(CONTEXT);
            let count = m.lhu(context.at(0x3b0));
            let result = m.lw(result_at);
            m.sh(context.at(0x3b0), count.wrapping_add(1));
            stop_flight_media(m);
            result
        }
        0x40_0000 | 0x80_0000 | 0x100_0000 | 0x200_0000 => {
            let result = m.lw(result_at);
            stop_flight_media(m);
            result
        }
        _ => m.lw(result_at),
    }
}

/// Common exit of phases 0x400000..0x10000000 in 80031E3C: 80030248,
/// 8002C280(0, 0) and `DecDCToutCallback(0)`.
fn stop_flight_media(m: &mut Machine) {
    shared::stop_all_sound(m);
    shared::reset_cd_queue(m, 0, 0);
    sdk::dec_dct_out_callback(m, 0);
}

/// Phase script `[[gp+0x328] + index*4 + offset]` through 80053704.
fn run_phase_script(m: &mut Machine, index: u32, offset: u32) {
    let scripts = m.gw(SCRIPTS);
    let script = m.lw(scripts.at(index << 2).at(offset));
    script::interpret(m, script);
}

/// 80031C20: phase 2 entry: script word +8 (`index`), then 80038930.
pub fn enter_phase_2(m: &mut Machine, index: u32) {
    run_phase_script(m, index, 8);
    prepare_phase_entities(m);
}

/// 80031C60: phase 0x10 entry: script word +0x1D8, then 80038980.
pub fn enter_phase_10(m: &mut Machine, index: u32) {
    run_phase_script(m, index, 0x1d8);
    save_phase_state(m);
}

/// 80031CB8: phase 0x80 entry: 8002C2F4(0).
pub fn enter_phase_80(m: &mut Machine) {
    shared::cancel_stream_requests(m);
}

/// 80031CE0: phase 0x400 entry: script word +0x238, then 80059C4C.
pub fn enter_phase_400(m: &mut Machine, index: u32) {
    run_phase_script(m, index, 0x238);
    setup_phase_400(m);
}

/// Stage modes of 80059C4C by mission code and phase index (`None`: any
/// index): flags (`[gp+0x4EC]`), the fixed-camera choice (8007574C) and the
/// mode number stored in `[gp+0x324]`.
const STAGE_MODES: [(u8, Option<u32>, u32, bool, u32); 24] = [
    (0x04, Some(0), 0x207, false, 0x00),
    (0x04, Some(1), 0x100, false, 0x01),
    (0x04, Some(2), 0x8000, false, 0x02),
    (0x07, Some(0), 0x2001, true, 0x03),
    (0x07, Some(1), 0x8000, false, 0x04),
    (0x07, Some(2), 0x101, false, 0x05),
    (0x09, Some(0), 0x400, false, 0x06),
    (0x09, Some(1), 0x401, false, 0x07),
    (0x0e, None, 0x101, false, 0x08),
    (0x18, None, 0x402, false, 0x09),
    (0x19, None, 0x102, false, 0x0a),
    (0x1d, Some(0), 0x208, false, 0x0b),
    (0x1d, Some(1), 0x803, false, 0x0c),
    (0x1d, Some(2), 0x8000, false, 0x0d),
    (0x1f, None, 0x4009, true, 0x0e),
    (0x21, None, 0x104, false, 0x0f),
    (0x22, None, 0x2003, true, 0x10),
    (0x29, Some(0), 0x2006, true, 0x11),
    (0x29, Some(1), 0x105, false, 0x12),
    (0x2a, None, 0x1000, false, 0x13),
    (0x2c, Some(0), 0x803, false, 0x14),
    (0x2c, Some(1), 0x20b, false, 0x15),
    (0x2c, Some(2), 0x8000, false, 0x16),
    (0x32, None, 0x2003, true, 0x17),
];

/// 80059C4C (phase 0x400 after its script): phase state byte +0x28 = +0x25;
/// stage flags and mode from [`STAGE_MODES`] (`[gp+0x4EC]`, `[gp+0x324]`);
/// 80041DBC; the phase record (+0x1C/+0x14: 0xFF/0 for flags 0x6000, 0/-
/// for 0x8000, else 1/0x1518); 800833A4, 8004B74C, 800722A4(flags) unless
/// 0x8000, 80077FAC(flags), then 8007574C (fixed camera), or 80074C60 (with
/// state half +8 = 0) for 0x8000, 80074C40 for 0x1000, else 80074C60;
/// context bit 0x10000000, 80030220 and 8002C81C(0, 0, 0). The player start
/// (a 0x18-byte position/angle block in the original frame at +0x10) comes
/// from the 56-byte records at 800B1904 (flags 0x300; heading and a
/// `0x6000`/`0x9000`-scaled offset from the 0x4000-per-turn sine table at
/// 80010498, randomized by context word +0x398), is fixed for flags
/// 0xC00/0x1000, or is the player's own position, saved in 800C5778 on the
/// first pass (`[gp+0x4F0]` counts passes) and restored on later ones. It is
/// passed to 800710FC unless flags 0x6000; then 8004C644, 8004D204 and
/// 80039AF8. Callees owned elsewhere are called by address.
///
/// The frame's half +0x26 is never written by the original but is copied to
/// 800C5778 +0x16 on the first pass; on the machine it holds whatever the
/// guest stack holds at that address.
pub fn setup_phase_400(m: &mut Machine) {
    let code = m.lbu(MISSION_CODE);
    let gameplay = m.gw(GAMEPLAY);
    let state = m.gw(PHASE_STATE);
    let frame = m.push_frame(0x38);
    let index = m.lw(gameplay.at(8));
    let value = m.lbu(state.at(0x25));
    m.sb(state.at(0x28), value);
    let mode = STAGE_MODES
        .iter()
        .find(|&&(c, i, ..)| c == code && i.is_none_or(|i| i == index));
    let (flags, fixed) = match mode {
        Some(&(_, _, flags, fixed, number)) => {
            m.set_gw(0x324, number);
            (flags, fixed)
        }
        None => (0, false),
    };
    m.set_gw(0x4ec, flags);
    dispatch::call(m, 0x8004_1dbc, [0; 4]);
    if flags & 0x6000 != 0 {
        let state = m.gw(PHASE_STATE);
        m.sb(state.at(0x1c), 0xff);
        let state = m.gw(PHASE_STATE);
        m.sw(state.at(0x14), 0);
    } else if flags & 0x8000 != 0 {
        let state = m.gw(PHASE_STATE);
        m.sb(state.at(0x1c), 0);
    } else {
        let state = m.gw(PHASE_STATE);
        m.sb(state.at(0x1c), 1);
        let state = m.gw(PHASE_STATE);
        m.sw(state.at(0x14), 0x1518);
    }
    dispatch::call(m, 0x8008_33a4, [0; 4]);
    dispatch::call(m, 0x8004_b74c, [0; 4]);
    if flags & 0x8000 == 0 {
        dispatch::call(m, 0x8007_22a4, [flags, 0, 0, 0]);
    }
    dispatch::call(m, 0x8007_7fac, [flags, 0, 0, 0]);
    if fixed {
        dispatch::call(m, 0x8007_574c, [0; 4]);
    } else if flags & 0x8000 != 0 {
        dispatch::call(m, 0x8007_4c60, [0; 4]);
        let state = m.gw(PHASE_STATE);
        m.sh(state.at(8), 0);
    } else if flags & 0x1000 != 0 {
        dispatch::call(m, 0x8007_4c40, [0; 4]);
    } else {
        dispatch::call(m, 0x8007_4c60, [0; 4]);
    }
    let context = m.gw(CONTEXT);
    or_word(m, context, 0x1000_0000);
    reset_sound_channels(m);
    dispatch::call(m, 0x8002_c81c, [0; 4]);
    let start = frame.at(0x10);
    if flags & 0x300 != 0 {
        place_from_stage_record(m, start, flags & 0xff);
    } else if flags & 0xc00 != 0 || flags & 0x1000 != 0 {
        m.sw(start, 0);
        m.sw(start.at(4), 0xfd8f_0000);
        m.sw(start.at(8), 0);
        for offset in [0x10, 0x12, 0x14] {
            m.sh(start.at(offset), 0);
        }
    } else {
        let saved = 0x800c_5778;
        if m.lbu(GP + 0x4f0) == 0 {
            let table = m.gw(ENTITIES);
            let entities = m.gw(ENTITIES);
            let player = m.lw(table);
            let words: [u32; 4] = std::array::from_fn(|i| m.lw(player.at(8 + 4 * i as u32)));
            for (i, word) in words.into_iter().enumerate() {
                m.sw(start.at(4 * i as u32), word);
            }
            m.sw(start.at(4), 0xfd8f_0000);
            m.sh(start.at(0x10), 0);
            let player = m.lw(entities);
            let heading = m.lhu(player.at(0x1a));
            m.sh(start.at(0x14), 0);
            m.sh(start.at(0x12), heading);
            copy_block(m, start, saved);
        } else {
            copy_block(m, saved, start);
        }
        let passes = m.lbu(GP + 0x4f0);
        m.sb(GP + 0x4f0, passes.wrapping_add(1));
    }
    if flags & 0x6000 == 0 {
        dispatch::call(m, 0x8007_10fc, [start, 0, 0, 0]);
    }
    for routine in [0x8004_c644, 0x8004_d204, 0x8003_9af8] {
        dispatch::call(m, routine, [0; 4]);
    }
    m.pop_frame(0x38);
}

/// The lock-on block 80039AF8 resets (JP 800C6D60).
const LOCK: u32 = 0x800c_4b28;

/// 80039AF8: reset the lock-on block: +9 0xFF, +0xC 0x83, the halfword +6
/// and the bytes +8, +0xA, +0xB, +0xD, +0xF and +0x11 zero, in the original
/// store order. Returns the block's address. Called last by 80059C4C; it
/// had no owner, so a phase-0x400 setup faulted as untranslated (found by
/// `jp-sdk` through its JP twin 8003A07C, an exact routine).
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

/// The 0x18-byte block copy of 80059C4C (words +0/+4/+8, then +0xC/+0x10/
/// +0x14, each three loaded before they are stored).
fn copy_block(m: &mut Machine, source: u32, target: u32) {
    for half in [0, 0xc] {
        let words: [u32; 3] = std::array::from_fn(|i| m.lw(source.at(half + 4 * i as u32)));
        for (i, word) in words.into_iter().enumerate() {
            m.sw(target.at(half + 4 * i as u32), word);
        }
    }
}

/// 80059FC8..8005A108: start block from stage record `record` (56 bytes at
/// 800B1904): position words +0/+4/+8 (y - 0x1000000) and heading half
/// +0x12. The offset angle is the heading + 0x2000, or, by context word
/// +0x398, heading + (rand & 0x3FF) + 0x1E00 (value 1) or heading +
/// (rand & 0xFFF) + 0x1800 (2 and above, also turning the stored heading by
/// -0x2000). The
/// position moves by `scale * sin` (x) and `scale * cos` (z) of that angle,
/// shifted right by 2 (scale 0x9000 for mission code 0x21, else 0x6000).
fn place_from_stage_record(m: &mut Machine, start: u32, record: u32) {
    let record = record.wrapping_mul(56).at(0x800b_1904);
    let x = m.lw(record);
    m.sw(start, x);
    let y = m.lw(record.at(4)).wrapping_add(0xff00_0000);
    m.sw(start.at(4), y);
    let z = m.lw(record.at(8));
    let code_now = m.lbu(MISSION_CODE);
    m.sh(start.at(0x10), 0);
    m.sw(start.at(8), z);
    let heading = m.lhu(record.at(0x12));
    m.sh(start.at(0x14), 0);
    m.sh(start.at(0x12), heading);
    let scale: i32 = if code_now == 0x21 { 0x9000 } else { 0x6000 };
    let context = m.gw(CONTEXT);
    let random = m.lw(context.at(0x398)) as i32;
    let angle = if random == 1 {
        let offset = (sdk::rand(m) & 0x3ff).wrapping_add(0x1e00);
        u32::from(m.lhu(record.at(0x12))).wrapping_add(offset)
    } else if random >= 2 {
        let offset = (sdk::rand(m) & 0xfff).wrapping_add(0x1800);
        let angle = u32::from(m.lhu(record.at(0x12))).wrapping_add(offset);
        m.sh(start.at(0x12), angle.wrapping_sub(0x2000) as u16);
        angle
    } else {
        u32::from(m.lhu(record.at(0x12))).wrapping_add(0x2000)
    };
    let index = angle & 0x3fff;
    let sine = i32::from(m.lh((index << 1).at(0x8001_0498)));
    let cosine = i32::from(m.lh((index.wrapping_add(0x1000) << 1).at(0x8001_0498)));
    let x = m.lw(start) as i32;
    m.sw(start, x.wrapping_add(scale.wrapping_mul(sine) >> 2) as u32);
    let z = m.lw(start.at(8)) as i32;
    m.sw(
        start.at(8),
        z.wrapping_add(scale.wrapping_mul(cosine) >> 2) as u32,
    );
}

/// 80031D20: phase 0x4000 entry: 80078238, then script word +0x218.
pub fn enter_phase_4000(m: &mut Machine, index: u32) {
    reinitialize_camera(m);
    run_phase_script(m, index, 0x218);
}

/// 80031D64: phase 8 entry: script word +0x18.
pub fn enter_phase_8(m: &mut Machine, index: u32) {
    run_phase_script(m, index, 0x18);
}

/// 80031D94: phase 0x40 entry: script word +0x1F8.
pub fn enter_phase_40(m: &mut Machine, index: u32) {
    run_phase_script(m, index, 0x1f8);
}

/// 80031DCC: phase 0x2000 entry: script word +0x248.
pub fn enter_phase_2000(m: &mut Machine, index: u32) {
    run_phase_script(m, index, 0x248);
}

/// 80031DFC: phase 0x10000 and 0x80000 entry: script word +0x228.
pub fn enter_phase_10000(m: &mut Machine, index: u32) {
    run_phase_script(m, index, 0x228);
}

/// 80031C98: exit hook of phase 0x40: 80078238.
pub fn leave_phase_40(m: &mut Machine) {
    reinitialize_camera(m);
}

/// 80078238: reinitialize the camera: 80076210 ([`crate::init::initialize_camera`])
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

/// 80038930: 80052760, 8004C54C, 8004D028, 80030220, then context word 0
/// bit 0x10000000.
pub fn prepare_phase_entities(m: &mut Machine) {
    select_phase_record(m);
    activate_group_entities(m);
    activate_group_entities_high(m);
    reset_sound_channels(m);
    let context = m.gw(CONTEXT);
    or_word(m, context, 0x1000_0000);
}

/// 80052760: phase state `[gp+0x330]`: +0x10 = 0, byte +0x1C = byte +0x34 of
/// `[gp+0x32C]` + phase; when that byte is 1, +0x14 = word +0x38 + phase*4.
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

/// 8004C54C: for active entities 2..31 (flag 0x4000, half +0x4E > 0) whose
/// kind bytes +0x1D/+0x1E match the phase group byte 800B00FC[phase],
/// 8004CB04 and 8004CB2C respectively.
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

/// 8004D028: 8004C54C for entities 32..63 (group byte 800B0160[phase],
/// 8004D2F8/8004D344), then the specials of mission code byte 800BAEB4 in
/// phase 0 (0x0D: 80052524, 0x2B: 80052568, 0x37: 80052398) or phase 1
/// (0x15: 80052540), and finally 800501F8.
pub fn activate_group_entities_high(m: &mut Machine) {
    let gameplay = m.gw(GAMEPLAY);
    let phase = m.lw(gameplay.at(8));
    let group = m.lbu(phase.at(GROUPS_HIGH));
    for index in 0..32u32 {
        let entities = m.gw(ENTITIES);
        let entity = m.lw((index << 2).at(entities).at(0x80));
        if m.lw(entity) & 0x4000 == 0 || m.lh(entity.at(0x4e)) <= 0 {
            continue;
        }
        let setup = m.gw(SETUP_HIGH);
        let kind = m.lw(index.wrapping_mul(0x28).at(setup).at(8));
        if in_group(m.lbu(kind.at(0x1d)), group) {
            shared::mark_object(m, index);
        }
        if in_group(m.lbu(kind.at(0x1e)), group) {
            shared::activate_object(m, index);
        }
    }
    let phase = |m: &mut Machine| {
        let gameplay = m.gw(GAMEPLAY);
        m.lw(gameplay.at(8))
    };
    let mut code = m.lbu(MISSION_CODE);
    if code == 0x0d && phase(m) == 0 {
        reset_mission_angles(m);
        code = m.lbu(MISSION_CODE);
    }
    if code == 0x15 && phase(m) == 1 {
        run_mission_script_148(m);
    }
    let mut code = m.lbu(MISSION_CODE);
    if code == 0x2b && phase(m) == 0 {
        clear_mission_word(m);
        code = m.lbu(MISSION_CODE);
    }
    if code == 0x37 && phase(m) == 0 {
        shuffle_mission_targets(m);
    }
    reset_800501f8(m);
}

/// 80052524: halves `gp+0x484/0x482/0x480` = 0x4000, `gp+0x48C` = 0.
pub fn reset_mission_angles(m: &mut Machine) {
    for offset in [0x484, 0x482, 0x480] {
        m.sh(GP + offset, 0x4000);
    }
    m.sh(GP + 0x48c, 0);
}

/// 80052540: mission script `[[gp+0x328] + 0x148]` through 80053704.
pub fn run_mission_script_148(m: &mut Machine) {
    let scripts = m.gw(SCRIPTS);
    let script = m.lw(scripts.at(0x148));
    script::interpret(m, script);
}

/// 80052568: word `gp+0x488` = 0.
pub fn clear_mission_word(m: &mut Machine) {
    m.set_gw(0x488, 0);
}

/// 800524EC: swap word +0x18 of high setup records `first` and `second`.
pub fn swap_setup_targets(m: &mut Machine, first: u32, second: u32) {
    let setup = m.gw(SETUP_HIGH);
    let a = first.wrapping_mul(0x28).at(setup);
    let b = second.wrapping_mul(0x28).at(setup);
    let from_b = m.lw(b.at(0x18));
    let from_a = m.lw(a.at(0x18));
    m.sw(a.at(0x18), from_b);
    m.sw(b.at(0x18), from_a);
}

/// 80052398: random permutations of high setup targets: for the four pairs
/// starting at bytes 800B0204, swap with odds 1/2; for the six triples
/// starting at bytes 800B0208, one of six orders (`rand() % 6`, original jump
/// table at 80023C98). `rand` is the SDK's 800AC178.
pub fn shuffle_mission_targets(m: &mut Machine) {
    for index in 0..4u32 {
        let first = u32::from(m.lbu(index.at(0x800b_0204)));
        let second = first.wrapping_add(1);
        if sdk::rand(m) & 1 != 0 {
            swap_setup_targets(m, first, second);
        }
    }
    for index in 0..6u32 {
        let first = u32::from(m.lbu(index.at(0x800b_0208)));
        let second = first.wrapping_add(1);
        let order = (sdk::rand(m) as i32 % 6) as u32;
        let third = first.wrapping_add(2);
        let case = if order < 6 {
            m.lw((order << 2).at(0x8002_3c98))
        } else {
            0x8005_24b8
        };
        match case {
            0x8005_24b8 => {}
            0x8005_2468 => swap_setup_targets(m, second, third),
            0x8005_2470 => swap_setup_targets(m, first, second),
            0x8005_247c => {
                swap_setup_targets(m, first, second);
                swap_setup_targets(m, second, third);
            }
            0x8005_2490 => {
                swap_setup_targets(m, second, third);
                swap_setup_targets(m, first, second);
            }
            0x8005_24a8 => swap_setup_targets(m, first, third),
            other => untranslated(m, other),
        }
    }
}

/// 80030220: 8002FF34 and two sound channels (8002F0E4); returns 0.
pub fn reset_sound_channels(m: &mut Machine) -> u32 {
    reset_8002ff34(m);
    init_sound_channels(m, 2);
    0
}

/// 8002F0E4: `count` sound channels: count word 800C43D8, voice bytes
/// +4/+8/+0xC = 6+3i, 7+3i, 8+3i (0xFF for unused slots below 4) and
/// 0x5C-byte records from 800C43F8 (+0 = 0, +8 = -1, +0x24 state, +0x30 and
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

/// 80038980 (phase 0x10 after its script): context bit 0x10000000,
/// 8006AD60 (owned by `init_objects`), `[gp+0x3D8]` = arena mark, and a 0x98C-byte copy (`[gp+0x3D4]`)
/// of the 99 words of record `[gameplay+0x28]` and the first 8 words of
/// each of the 64 entities.
pub fn save_phase_state(m: &mut Machine) {
    let context = m.gw(CONTEXT);
    or_word(m, context, 0x1000_0000);
    init_objects::reset_slot_table(m);
    let mark = arena::peek(m, 0);
    m.set_gw(0x3d8, mark);
    let copy = arena::alloc(m, 0x98c);
    let gameplay = m.gw(GAMEPLAY);
    let source = m.lw(gameplay.at(0x28));
    m.set_gw(0x3d4, copy);
    shared::copy_words(m, copy, source, 0x63);
    for index in 0..64u32 {
        let copy = m.gw(0x3d4);
        let entities = m.gw(ENTITIES);
        let entity = m.lw((index << 2).at(entities));
        shared::copy_words(m, copy.at(0x18c).at(index << 5), entity, 8);
    }
}

/// Every routine translated here, by original address, with the original
/// `a0..a3`; returns `v0` (0 for routines without a result). The empty
/// hooks of 80031E3C (`jr ra`) leave `v0` unchanged in the original; their
/// caller never reads it.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let [a0, a1, _, _] = args;
    Some(match target {
        0x8003_1e3c => enter_mission_phase(m, a0, a1),
        0x8003_0220 => reset_sound_channels(m),
        0x8003_9af8 => reset_lock(m),
        _ => {
            match target {
                0x8003_5248 => update_gameplay_bit0(m, a0),
                0x8003_22d4 => set_flight_bounds(m, a0 as i32),
                0x8003_2268 => allocate_frame_buffers(m, a0),
                0x8003_21b4 => prepare_frame_buffers(m, a0),
                0x8007_c4b4 => create_work_heap(m),
                0x8003_1c20 => enter_phase_2(m, a0),
                0x8003_1c60 => enter_phase_10(m, a0),
                0x8003_1cb8 => enter_phase_80(m),
                0x8003_1ce0 => enter_phase_400(m, a0),
                0x8005_9c4c => setup_phase_400(m),
                0x8003_1d20 => enter_phase_4000(m, a0),
                0x8003_1d64 => enter_phase_8(m, a0),
                0x8003_1d94 => enter_phase_40(m, a0),
                0x8003_1dcc => enter_phase_2000(m, a0),
                0x8003_1dfc => enter_phase_10000(m, a0),
                0x8003_1c98 => leave_phase_40(m),
                0x8007_8238 => reinitialize_camera(m),
                0x8003_8930 => prepare_phase_entities(m),
                0x8005_2760 => select_phase_record(m),
                0x8004_c54c => activate_group_entities(m),
                0x8004_d028 => activate_group_entities_high(m),
                0x8005_2524 => reset_mission_angles(m),
                0x8005_2540 => run_mission_script_148(m),
                0x8005_2568 => clear_mission_word(m),
                0x8005_2398 => shuffle_mission_targets(m),
                0x8005_24ec => swap_setup_targets(m, a0, a1),
                0x8002_f0e4 => init_sound_channels(m, a0),
                0x8003_8980 => save_phase_state(m),
                0x8003_1c58 | 0x8003_1cd8 | 0x8003_1d18 | 0x8003_1d5c | 0x8003_1dc4
                | 0x8003_1e2c | 0x8003_1e34 => {}
                _ => return None,
            }
            0
        }
    })
}

/// Original addresses claimed by [`dispatch`].
pub const TRANSLATED: [u32; 38] = [
    0x8005_9c4c,
    0x8003_9af8,
    0x8003_5248,
    0x8003_22d4,
    0x8003_2268,
    0x8003_21b4,
    0x8007_c4b4,
    0x8003_1e3c,
    0x8003_1c20,
    0x8003_1c60,
    0x8003_1cb8,
    0x8003_1ce0,
    0x8003_1d20,
    0x8003_1d64,
    0x8003_1d94,
    0x8003_1dcc,
    0x8003_1dfc,
    0x8003_1c98,
    0x8003_1c58,
    0x8003_1cd8,
    0x8003_1d18,
    0x8003_1d5c,
    0x8003_1dc4,
    0x8003_1e2c,
    0x8003_1e34,
    0x8007_8238,
    0x8003_8930,
    0x8005_2760,
    0x8004_c54c,
    0x8004_d028,
    0x8005_2524,
    0x8005_2540,
    0x8005_2568,
    0x8005_2398,
    0x8005_24ec,
    0x8003_0220,
    0x8002_f0e4,
    0x8003_8980,
];
