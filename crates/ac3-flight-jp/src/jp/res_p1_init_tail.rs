//! Patches of `init_tail` routines whose JP instructions differ from PAL
//! (REVIEW.md §2-§3, resident part 1). Each is listed in
//! [`REPLACES`](super::REPLACES).
use crate::{
    common::{CONTEXT, ENTITIES, GAMEPLAY, reset_800501f8},
    dispatch,
    init_tail::{
        clear_mission_word, enter_phase_2, enter_phase_8, enter_phase_10, enter_phase_40,
        enter_phase_80, enter_phase_2000, enter_phase_4000, enter_phase_10000, leave_phase_40,
        reset_mission_angles, reset_sound_channels, run_mission_script_148,
        shuffle_mission_targets,
    },
    machine::{Fault, GP, Machine, SmallData},
    script, sdk, shared,
};

/// `[gp+0x388]`: 40-byte setup records of entities 32..63.
const SETUP_HIGH: u32 = 0x388;
/// Phase group bytes indexed by gameplay +8, for entities 32..63.
const GROUPS_HIGH: u32 = 0x800b_2024;
/// 80032080 result words, one per phase code.
const PHASE_RESULTS: u32 = 0x800b_09d4;
/// Context +0x38C (JP): the mission number, a signed halfword (PAL: the
/// byte 800BAEB4).
const CONTEXT_MISSION: u32 = 0x38c;
/// The flight-bounds switch table of 80032518 (53 cases from mission 6).
const BOUNDS_CASES: u32 = 0x8002_3520;

/// Phase codes of gameplay +4 that 80032080 maps to a result word (index
/// into the table at 800B09D4; the same instructions as PAL).
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

fn at(base: u32, offset: u32) -> u32 {
    base.wrapping_add(offset)
}

fn or_word(m: &mut Machine, address: u32, bits: u32) {
    let value = m.lw(address);
    m.sw(address, value | bits);
}

/// 80035740 (JP; PAL 80035248, near, `imm:1`): set gameplay word 0 bit 0
/// when `request` is zero, context byte +0x39E (PAL +0x3A2) is set, or word
/// +0x50 of record `[gameplay+0x28]` has bit 0x800; otherwise clear it.
pub fn update_gameplay_bit0(m: &mut Machine, request: u32) {
    if request != 0 {
        let context = m.gw(CONTEXT);
        if m.lbu(at(context, 0x39e)) == 0 {
            let gameplay = m.gw(GAMEPLAY);
            let record = m.lw(at(gameplay, 0x28));
            if m.lw(at(record, 0x50)) & 0x800 == 0 {
                let flags = m.lw(gameplay);
                m.sw(gameplay, flags & 0xffff_fffe);
                return;
            }
        }
    }
    let gameplay = m.gw(GAMEPLAY);
    or_word(m, gameplay, 1);
}

/// 80032518 (JP; PAL 800322D4, near, `imm:2`): flight bounds for `mission`:
/// three (min, max) word pairs at gameplay +0x2A0..+0x2B4 and the bound
/// words +0x7C/+0x80/+0x84 and +0x8C/+0x90/+0x94 of record
/// `[gameplay+0x28]`. Missions 6..58 (PAL 10..37) select a case through the
/// original jump table at 80023520: 6 (8003254C), 32/47/53 (800325AC), 55
/// (80032638), 42/58 (800326A8); the others and missions outside the table
/// use the default at 8003270C.
pub fn set_flight_bounds(m: &mut Machine, mission: i32) {
    const LOW: u32 = 0x8000_0001;
    const HIGH: u32 = 0x7fff_ffff;
    let index = (mission as u32).wrapping_sub(6);
    let gameplay = m.gw(GAMEPLAY);
    let bounds = m.lw(at(gameplay, 0x28));
    let case = if index < 0x35 {
        m.lw(at(index << 2, BOUNDS_CASES))
    } else {
        0x8003_270c
    };
    let pairs = |m: &mut Machine, values: [u32; 6]| {
        for (offset, value) in [0x2a0, 0x2a4, 0x2a8, 0x2ac, 0x2b0, 0x2b4]
            .into_iter()
            .zip(values)
        {
            m.sw(at(gameplay, offset), value);
        }
    };
    let limits = |m: &mut Machine, values: [u32; 6]| {
        for (offset, value) in [0x7c, 0x80, 0x84, 0x8c, 0x90, 0x94].into_iter().zip(values) {
            m.sw(at(bounds, offset), value);
        }
    };
    match case {
        0x8003_254c => {
            let flags = m.lw(gameplay);
            pairs(m, [LOW, HIGH, LOW, 0, LOW, HIGH]);
            m.sw(gameplay, flags | 0x18);
            limits(m, [LOW, 0xff6e_0000, LOW, HIGH, 0xffef_8000, HIGH]);
        }
        0x8003_25ac => {
            let flags = m.lw(gameplay);
            m.sw(at(gameplay, 0x2a8), 0xff48_a000);
            for (offset, value) in [
                (0x2a0, LOW),
                (0x2a4, HIGH),
                (0x2ac, 0x0177_0000),
                (0x2b0, LOW),
                (0x2b4, HIGH),
            ] {
                m.sw(at(gameplay, offset), value);
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
        0x8003_2638 => {
            let flags = m.lw(gameplay);
            m.sw(at(gameplay, 0x2a8), 0xff48_a000);
            for (offset, value) in [
                (0x2a0, LOW),
                (0x2a4, HIGH),
                (0x2ac, 0x0177_0000),
                (0x2b0, LOW),
                (0x2b4, HIGH),
            ] {
                m.sw(at(gameplay, offset), value);
            }
            m.sw(gameplay, flags | 0x10);
            limits(m, [LOW, 0xff5d_4000, LOW, HIGH, 0xffef_8000, HIGH]);
        }
        0x8003_26a8 => {
            pairs(m, [LOW, HIGH, LOW, HIGH, LOW, HIGH]);
            let bound_flags = m.lw(bounds);
            limits(m, [LOW, LOW, LOW, HIGH, HIGH, HIGH]);
            m.sw(bounds, bound_flags | 0x1000);
            or_word(m, gameplay, 0x300);
        }
        0x8003_270c => {
            limits(m, [LOW, LOW, LOW, HIGH, 0, HIGH]);
            pairs(m, [LOW, HIGH, LOW, HIGH, LOW, HIGH]);
        }
        other => m.raise(Fault::Untranslated { address: other }),
    }
}

/// 80032080 (JP; PAL 80031E3C, near, `imm:2`): enter the mission phase
/// whose code gameplay +4 holds (gameplay +8 is its index) and return its
/// callback code from the table at 800B09D4 (0 for an unknown phase).
/// `leaving` selects an exit hook (only 0x40 does work); `argument` is the
/// script index of phases 8, 0x40, 0x2000, 0x10000 and 0x80000. Phase
/// 0x10000000 counts at context +0x3AC (PAL +0x3B0).
pub fn enter_mission_phase(m: &mut Machine, leaving: u32, argument: u32) -> u32 {
    let gameplay = m.gw(GAMEPLAY);
    let phase = m.lw(at(gameplay, 4));
    let index = m.lw(at(gameplay, 8));
    if leaving == 0x40 {
        leave_phase_40(m);
    }
    match phase {
        2 => enter_phase_2(m, index),
        0x10 => enter_phase_10(m, index),
        0x80 => enter_phase_80(m),
        0x400 => {
            // 80031F24: its script, then 8005B654 with `leaving` in s3.
            run_phase_script(m, index, 0x238);
            setup_phase_400_carrying(m, leaving);
        }
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
    let result_at = at(slot << 2, PHASE_RESULTS);
    match phase {
        1 => {
            let result = m.lw(result_at);
            shared::reset_cd_queue(m, 0, 0);
            result
        }
        0x1000_0000 => {
            let context = m.gw(CONTEXT);
            let count = m.lhu(at(context, 0x3ac));
            let result = m.lw(result_at);
            m.sh(at(context, 0x3ac), count.wrapping_add(1));
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

/// Common exit of phases 0x400000..0x10000000 in 80032080: 8003048C,
/// 8002C6A8(0, 0) and `DecDCToutCallback(0)`.
fn stop_flight_media(m: &mut Machine) {
    shared::stop_all_sound(m);
    shared::reset_cd_queue(m, 0, 0);
    sdk::dec_dct_out_callback(m, 0);
}

/// Whether a group byte matches the phase group exactly or by high nibble.
fn in_group(value: u8, group: u8) -> bool {
    value == group || value == group & 0xf0
}

/// 8004E620 (JP; PAL 8004D028, near, `imm:4 ins:4 op:12`): 8004DB40 for
/// entities 32..63 (group byte 800B2024[phase], 8004E900/8004E94C), then
/// the specials of the mission (context +0x38C, re-read for each test; PAL:
/// the byte 800BAEB4) in phase 0 (0x0D: 80053B5C, 0x2B: 80053BA0, 0x37:
/// 800539D0) or phase 1 (0x15: 80053B78), and finally 80051824.
pub fn activate_group_entities_high(m: &mut Machine) {
    let gameplay = m.gw(GAMEPLAY);
    let phase = m.lw(at(gameplay, 8));
    let group = m.lbu(at(phase, GROUPS_HIGH));
    for index in 0..32u32 {
        let entities = m.gw(ENTITIES);
        let entity = m.lw(at(at(index << 2, entities), 0x80));
        if m.lw(entity) & 0x4000 == 0 || m.lh(at(entity, 0x4e)) <= 0 {
            continue;
        }
        let setup = m.gw(SETUP_HIGH);
        let kind = m.lw(at(at(index.wrapping_mul(0x28), setup), 8));
        if in_group(m.lbu(at(kind, 0x1d)), group) {
            shared::mark_object(m, index);
        }
        if in_group(m.lbu(at(kind, 0x1e)), group) {
            shared::activate_object(m, index);
        }
    }
    let mission = |m: &mut Machine| {
        let context = m.gw(CONTEXT);
        i32::from(m.lh(at(context, CONTEXT_MISSION)))
    };
    let phase = |m: &mut Machine| {
        let gameplay = m.gw(GAMEPLAY);
        m.lw(at(gameplay, 8))
    };
    if mission(m) == 0x0d && phase(m) == 0 {
        reset_mission_angles(m);
    }
    if mission(m) == 0x15 && phase(m) == 1 {
        run_mission_script_148(m);
    }
    if mission(m) == 0x2b && phase(m) == 0 {
        clear_mission_word(m);
    }
    if mission(m) == 0x37 && phase(m) == 0 {
        shuffle_mission_targets(m);
    }
    reset_800501f8(m);
}

/// Phase script `[[gp+0x338] + index*4 + offset]` through 80054EBC.
fn run_phase_script(m: &mut Machine, index: u32, offset: u32) {
    let scripts = m.gw(SCRIPTS);
    let script = m.lw(scripts.at(index << 2).at(offset));
    script::interpret(m, script);
}

/// `[gp+0x338]`: mission script pointer table.
const SCRIPTS: u32 = 0x338;

/// 8005B654 (JP; PAL 80059C4C, near, `del:15 imm:3 ins:12 op:7 ref:1 reg:3`:
/// the mission halfword context +0x38C for PAL byte 800BAEB4, the level at
/// context +0x394 for PAL +0x398, a 0x40-byte frame) (phase 0x400 after its
/// script): phase state byte +0x28 = +0x25; stage flags and mode by mission
/// from [`STAGE_MODES`] (`[gp+0x4E4]`, `[gp+0x334]`);
/// 800428EC; the phase record (+0x1C/+0x14: 0xFF/0 for flags 0x6000, 0/-
/// for 0x8000, else 1/0x1518); 800849EC, 8004CE9C, 80073928(flags) unless
/// 0x8000, 8007961C(flags), then 80076DBC (fixed camera), or 800762D0 (with
/// state half +8 = 0) for 0x8000, 800762B0 for 0x1000, else 800762D0;
/// context bit 0x10000000, 80030464 and 8002CC44(0, 0, 0). The player start
/// (a 0x18-byte position/angle block in the original frame at +0x10) comes
/// from the 56-byte records at 800B3B94 (flags 0x300; heading and a
/// `0x6000`/`0x9000`-scaled offset from the 0x4000-per-turn sine table at
/// 800104AC, randomized by the level), is fixed for flags
/// 0xC00/0x1000, or is the player's own position, saved in 800C79B0 on the
/// first pass (`[gp+0x4E8]` counts passes) and restored on later ones. It is
/// passed to 80072780 unless flags 0x6000; then 8004DC38, 8004E80C and
/// 8003A07C. Callees owned elsewhere are called by address.
///
/// The frame's half +0x26 is never written by the original but is copied to
/// 800C79B0 +0x16 on the first pass; on the machine it holds whatever the
/// guest stack holds at that address. Called by address, without the
/// caller's `s3`, a negative level takes angle 0.
pub fn setup_phase_400(m: &mut Machine) {
    setup_phase_400_carrying(m, 0);
}

/// [`setup_phase_400`] with `carried`, the caller's `s3`: the offset angle
/// of a negative level (see [`place_from_stage_record`]). Its only caller,
/// 80031F24 (entered from 80032080), leaves 80032080's first argument there.
fn setup_phase_400_carrying(m: &mut Machine, carried: u32) {
    let context = m.gw(CONTEXT);
    let code = i32::from(m.lh(context.at(CONTEXT_MISSION)));
    let gameplay = m.gw(GAMEPLAY);
    let state = m.gw(PHASE_STATE);
    let frame = m.push_frame(0x40);
    let index = m.lw(gameplay.at(8));
    let value = m.lbu(state.at(0x25));
    m.sb(state.at(0x28), value);
    let mode = STAGE_MODES
        .iter()
        .find(|&&(c, i, ..)| i32::from(c) == code && i.is_none_or(|i| i == index));
    let (flags, fixed) = match mode {
        Some(&(_, _, flags, fixed, number)) => {
            m.set_gw(0x334, number);
            (flags, fixed)
        }
        None => (0, false),
    };
    m.set_gw(0x4e4, flags);
    dispatch::call(m, 0x8004_28ec, [0; 4]);
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
    dispatch::call(m, 0x8008_49ec, [0; 4]);
    dispatch::call(m, 0x8004_ce9c, [0; 4]);
    if flags & 0x8000 == 0 {
        dispatch::call(m, 0x8007_3928, [flags, 0, 0, 0]);
    }
    dispatch::call(m, 0x8007_961c, [flags, 0, 0, 0]);
    if fixed {
        dispatch::call(m, 0x8007_6dbc, [0; 4]);
    } else if flags & 0x8000 != 0 {
        dispatch::call(m, 0x8007_62d0, [0; 4]);
        let state = m.gw(PHASE_STATE);
        m.sh(state.at(8), 0);
    } else if flags & 0x1000 != 0 {
        dispatch::call(m, 0x8007_62b0, [0; 4]);
    } else {
        dispatch::call(m, 0x8007_62d0, [0; 4]);
    }
    let context = m.gw(CONTEXT);
    or_word(m, context, 0x1000_0000);
    reset_sound_channels(m);
    dispatch::call(m, 0x8002_cc44, [0; 4]);
    let start = frame.at(0x10);
    if flags & 0x300 != 0 {
        place_from_stage_record(m, start, flags & 0xff, carried);
    } else if flags & 0xc00 != 0 || flags & 0x1000 != 0 {
        m.sw(start, 0);
        m.sw(start.at(4), 0xfd8f_0000);
        m.sw(start.at(8), 0);
        for offset in [0x10, 0x12, 0x14] {
            m.sh(start.at(offset), 0);
        }
    } else {
        let saved = 0x800c_79b0;
        if m.lbu(GP + 0x4e8) == 0 {
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
        let passes = m.lbu(GP + 0x4e8);
        m.sb(GP + 0x4e8, passes.wrapping_add(1));
    }
    if flags & 0x6000 == 0 {
        dispatch::call(m, 0x8007_2780, [start, 0, 0, 0]);
    }
    for routine in [0x8004_dc38, 0x8004_e80c, 0x8003_a07c] {
        dispatch::call(m, routine, [0; 4]);
    }
    m.pop_frame(0x40);
}

/// The 0x18-byte block copy of 8005B654 (words +0/+4/+8, then +0xC/+0x10/
/// +0x14, each three loaded before they are stored).
fn copy_block(m: &mut Machine, source: u32, target: u32) {
    for half in [0, 0xc] {
        let words: [u32; 3] = std::array::from_fn(|i| m.lw(source.at(half + 4 * i as u32)));
        for (i, word) in words.into_iter().enumerate() {
            m.sw(target.at(half + 4 * i as u32), word);
        }
    }
}

/// 8005B9D4..8005BB00: start block from stage record `record` (56 bytes at
/// 800B3B94): position words +0/+4/+8 (y - 0x1000000) and heading half
/// +0x12. The offset angle by the level (context +0x394): the heading +
/// 0x2000 (0), heading + (rand & 0x3FF) + 0x1E00 (1) or heading +
/// (rand & 0xFFF) + 0x1800 (2 and above, also turning the stored heading by
/// -0x2000). JP sets nothing for a negative level (PAL: heading + 0x2000):
/// the angle is then whatever the caller left in `s3` (`carried`). The
/// position moves by `scale * sin` (x) and `scale * cos` (z) of that angle,
/// shifted right by 2 (scale 0x9000 on mission 0x21, else 0x6000).
fn place_from_stage_record(m: &mut Machine, start: u32, record: u32, carried: u32) {
    let record = record.wrapping_mul(56).at(0x800b_3b94);
    let x = m.lw(record);
    m.sw(start, x);
    let y = m.lw(record.at(4)).wrapping_add(0xff00_0000);
    m.sw(start.at(4), y);
    let z = m.lw(record.at(8));
    let context = m.gw(CONTEXT);
    let code_now = i32::from(m.lh(context.at(CONTEXT_MISSION)));
    m.sh(start.at(0x10), 0);
    m.sw(start.at(8), z);
    let heading = m.lhu(record.at(0x12));
    m.sh(start.at(0x14), 0);
    m.sh(start.at(0x12), heading);
    let scale: i32 = if code_now == 0x21 { 0x9000 } else { 0x6000 };
    let level = m.lw(context.at(0x394)) as i32;
    let angle = if level == 0 {
        u32::from(m.lhu(record.at(0x12))).wrapping_add(0x2000)
    } else if level == 1 {
        let offset = (sdk::rand(m) & 0x3ff).wrapping_add(0x1e00);
        u32::from(m.lhu(record.at(0x12))).wrapping_add(offset)
    } else if level >= 2 {
        let offset = (sdk::rand(m) & 0xfff).wrapping_add(0x1800);
        let angle = u32::from(m.lhu(record.at(0x12))).wrapping_add(offset);
        m.sh(start.at(0x12), angle.wrapping_sub(0x2000) as u16);
        angle
    } else {
        carried
    };
    let index = angle & 0x3fff;
    let sine = i32::from(m.lh((index << 1).at(0x8001_04ac)));
    let cosine = i32::from(m.lh((index.wrapping_add(0x1000) << 1).at(0x8001_04ac)));
    let x = m.lw(start) as i32;
    m.sw(start, x.wrapping_add(scale.wrapping_mul(sine) >> 2) as u32);
    let z = m.lw(start.at(8)) as i32;
    m.sw(
        start.at(8),
        z.wrapping_add(scale.wrapping_mul(cosine) >> 2) as u32,
    );
}

/// MIPS address arithmetic (`addiu`/`addu`) wraps.
trait At {
    fn at(self, offset: u32) -> u32;
}
impl At for u32 {
    fn at(self, offset: u32) -> u32 {
        self.wrapping_add(offset)
    }
}

/// `[gp+0x340]`: phase state block written by 80053DA0.
const PHASE_STATE: u32 = 0x340;

/// Stage modes of 8005B654 by mission code and phase index (`None`: any
/// index): flags (`[gp+0x4E4]`), the fixed-camera choice (80076DBC) and the
/// mode number stored in `[gp+0x334]`.
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
