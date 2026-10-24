//! Patches of `init_objects` routines whose JP instructions differ from PAL
//! (REVIEW.md §2-§3), part 3 of the resident wave. Each is listed in
//! [`REPLACES`](super::REPLACES).
//!
//! The JP context has one word less before +0x3AA: PAL +0x390 (mission),
//! +0x398 (difficulty), +0x3AE/+0x3AF (game-mode bytes) and +0x970 are JP
//! +0x38C, +0x394, +0x3AA/+0x3AB and +0x96C. The JP missions are numbered
//! differently (JP has 52).
use crate::{
    arena,
    common::{self, CONTEXT, ENTITIES},
    init_objects::{
        EFFECT_TABLES, PLAYER_LIMITS, PLAYER_STATE, allocate_player_blocks,
        copy_resource_parameters, fill_random_tables, light_direction_vectors, rotate_vector_q14,
    },
    machine::{Fault, Machine, SmallData},
};

/// 8006CD6C jump table: case addresses for aircraft ids 1..=30.
const AIRCRAFT_CASES: u32 = 0x8002_4668;
/// 8006CD6C result: aircraft parameter block pointers.
const AIRCRAFT_PARAMETERS: u32 = 0x800b_3ac8;
/// 8006CE70 per-mode cap halfwords, indexed by context byte +0x3AA.
const AIRCRAFT_CAPS: u32 = 0x800b_3b28;

// ---- 80084A6C -------------------------------------------------------------------

/// 80084A6C (JP; PAL 80083414, near, `imm:13`): no arguments. Allocates the
/// 0x3B54-byte effect tables (`[gp+0x41C]`), fills the random tables
/// (80084B84), the light-direction vectors at +0x5C8 (8008B178) and the
/// resource parameters at +0x3948 (800900A4), and clears the effect lists
/// (800849EC). Then, when context word +0x96C has bit 0x10 and the mission
/// halfword +0x38C is not 0xD, 0x2A or 0x38, context halfword +0x5E selects
/// bytes +0x3AA/+0x3AB: mask 0x1008 (unless the mission is 0x20, 0x2F,
/// 0x35 or 0x37) gives 7/7, else 0x4008 gives 0xA/0xA, else 0x8008 gives
/// 5/5, else 0x2008 gives +0x3AB = 9. Returns `v0`: 0xD (bit clear), 0x2A
/// (mission 0xD), 0x38 (missions 0x2A/0x38), the context pointer (a
/// 7/0xA/5 store) or 9.
pub fn init_effect_tables(m: &mut Machine) -> u32 {
    let block = arena::alloc(m, 0x3b54);
    m.set_gw(EFFECT_TABLES, block);
    fill_random_tables(m, block);
    let block = m.gw(EFFECT_TABLES);
    light_direction_vectors(m, block.wrapping_add(0x5c8));
    let block = m.gw(EFFECT_TABLES);
    copy_resource_parameters(m, block.wrapping_add(0x3948));
    reset_effect_lists(m);
    let context = m.gw(CONTEXT);
    if m.lw(context.wrapping_add(0x96c)) & 0x10 == 0 {
        return 0xd;
    }
    let mission = i32::from(m.lh(context.wrapping_add(0x38c)));
    let bits = u32::from(m.lhu(context.wrapping_add(0x5e)));
    match mission {
        0xd => return 0x2a,
        0x2a | 0x38 => return 0x38,
        _ => {}
    }
    let value = if bits & 0x1008 == 0x1008 && !matches!(mission, 0x20 | 0x2f | 0x35 | 0x37) {
        // Stored through the context pointer loaded above.
        m.sb(context.wrapping_add(0x3aa), 7);
        7
    } else if bits & 0x4008 == 0x4008 {
        let context = m.gw(CONTEXT);
        m.sb(context.wrapping_add(0x3aa), 0xa);
        0xa
    } else if bits & 0x8008 == 0x8008 {
        let context = m.gw(CONTEXT);
        m.sb(context.wrapping_add(0x3aa), 5);
        5
    } else {
        if bits & 0x2008 == 0x2008 {
            let context = m.gw(CONTEXT);
            m.sb(context.wrapping_add(0x3ab), 9);
        }
        return 9;
    };
    let context = m.gw(CONTEXT);
    m.sb(context.wrapping_add(0x3ab), value);
    context
}

/// 800849EC (JP; PAL 800833A4, changed, `del:1 imm:2 ins:5 op:4 reg:6`): no
/// arguments. Clears the list heads of `[gp+0x41C]` (reloaded for each
/// group, unlike PAL): words +0x600/+0x108C (three each), +0x1098..+0x10BC,
/// +0x36C4, 32 words at stride 0x14 from +0x36D8, and +0x33C0, +0x3948,
/// +0x394C, +0x3964, +0x3B28. Returns `v0`, the block (PAL: block - 0x14).
pub fn reset_effect_lists(m: &mut Machine) -> u32 {
    let block = m.gw(EFFECT_TABLES);
    for i in 0..3u32 {
        let at = block.wrapping_add(i * 4);
        m.sw(at.wrapping_add(0x600), 0);
        m.sw(at.wrapping_add(0x108c), 0);
    }
    let block = m.gw(EFFECT_TABLES);
    for i in 0..10u32 {
        m.sw(block.wrapping_add(0x10bc - i * 4), 0);
    }
    let block = m.gw(EFFECT_TABLES);
    m.sw(block.wrapping_add(0x36c4), 0);
    for i in 0..0x20u32 {
        m.sw(block.wrapping_add(0x3944 - i * 0x14), 0);
    }
    let block = m.gw(EFFECT_TABLES);
    for offset in [0x33c0, 0x3948, 0x394c, 0x3964, 0x3b28] {
        m.sw(block.wrapping_add(offset), 0);
    }
    block
}

// ---- 8006CE70 -------------------------------------------------------------------

/// 8006CE70 (JP; PAL 8006B6A0, near, `imm:2`): a0 = the first entity.
/// Allocates the player blocks (8006CCE0), selects the aircraft parameter
/// block from a0's halfword +0x48 (8006CD6C), initializes the player entity
/// and fills the flight state `[gp+0x520]` and the limits `[gp+0x524]`, as
/// PAL, with the difficulty word at context +0x394 (PAL +0x398) and the
/// per-mode cap indexed by context byte +0x3AA (PAL +0x3AE). Returns `v0`
/// (0x55, from 80030178).
pub fn init_player_flight(m: &mut Machine, entity: u32) -> u32 {
    allocate_player_blocks(m);
    let table = m.gw(ENTITIES);
    let aircraft = i32::from(m.lh(entity.wrapping_add(0x48))) as u32;
    let player = m.lw(table);
    let parameters = aircraft_parameters(m, aircraft);
    m.sw(player.wrapping_add(0x20), 0);
    m.sw(player.wrapping_add(0x24), 0);
    m.sw(player.wrapping_add(0x28), 0x2_0000);
    for offset in [0x30, 0x32, 0x34] {
        m.sh(player.wrapping_add(offset), 0);
    }
    rotate_vector_q14(
        m,
        player.wrapping_add(0x20),
        player.wrapping_add(0x18),
        player.wrapping_add(0x38),
    );
    let state = m.gw(PLAYER_STATE);
    m.sw(player.wrapping_add(0x54), 0x8000_0000);
    m.sw(state, 0);
    m.sw(state.wrapping_add(4), 2);
    for offset in [8, 0xc, 0x10] {
        m.sw(state.wrapping_add(offset), 0);
    }
    for offset in [0x14, 0x16, 0x18] {
        m.sh(state.wrapping_add(offset), 0);
    }
    for offset in [0x1c, 0x20, 0x24, 0x2c] {
        m.sw(state.wrapping_add(offset), 0);
    }
    for offset in [0x30, 0x32, 0x34, 0x38, 0x3a, 0x3c, 0x40, 0x42, 0x44] {
        m.sh(state.wrapping_add(offset), 0);
    }
    let speed = m.lw(player.wrapping_add(0x28));
    m.sh(state.wrapping_add(0x4c), 0x1000);
    m.sh(state.wrapping_add(0x54), 0);
    m.sh(state.wrapping_add(0x56), 0);
    m.sw(state.wrapping_add(0x48), speed);
    for i in 0..0xc {
        let state = m.gw(PLAYER_STATE);
        m.sb(state.wrapping_add(i).wrapping_add(0x58), 0);
    }
    for i in 0..0xc {
        let state = m.gw(PLAYER_STATE);
        m.sb(state.wrapping_add(i).wrapping_add(0x64), 0);
    }
    let limits = m.gw(PLAYER_LIMITS);
    let context = m.gw(CONTEXT);
    m.sw(limits, parameters);
    let value = m.lw(parameters.wrapping_add(0x84));
    m.sw(limits.wrapping_add(0x14), value);
    m.sh(player.wrapping_add(0x4e), value as u16);
    if (m.lw(context.wrapping_add(0x394)) as i32) >= 2 {
        m.sw(limits.wrapping_add(0x14), 0x1e);
        m.sh(player.wrapping_add(0x4e), 0x1e);
    }
    let limits = m.gw(PLAYER_LIMITS);
    let value = m.lw(parameters.wrapping_add(0x74));
    m.sw(limits.wrapping_add(8), value);
    let value = i32::from(m.lh(parameters.wrapping_add(0x80))) as u32;
    m.sw(limits.wrapping_add(0xc), value);
    let value = i32::from(m.lh(parameters.wrapping_add(0x82))) as u32;
    m.sw(limits.wrapping_add(0x18), 0xffff_ffff);
    m.sw(limits.wrapping_add(0x10), value);
    let value = m.lw(parameters.wrapping_add(0x78));
    m.sw(limits.wrapping_add(0x1c), value);
    let requested = i32::from(m.lh(parameters.wrapping_add(0x7c)));
    let context = m.gw(CONTEXT);
    m.sw(limits.wrapping_add(4), requested as u32);
    let mode = u32::from(m.lbu(context.wrapping_add(0x3aa)));
    let cap = i32::from(m.lh(AIRCRAFT_CAPS.wrapping_add(mode << 1)));
    if cap < requested {
        m.sw(limits.wrapping_add(4), cap as u32);
    }
    let limits = m.gw(PLAYER_LIMITS);
    for offset in [0x20, 0x24, 0x28] {
        m.sw(limits.wrapping_add(offset), 0);
    }
    m.sw(limits.wrapping_add(0x2c), 0x7fff_ffff);
    for offset in [0x30, 0x32, 0x34] {
        m.sh(limits.wrapping_add(offset), 0);
    }
    m.sw(limits.wrapping_add(0x38), 0);
    common::reset_8002ff34(m);
    // `v0` of 80030178: its last `li v0, 0x55`.
    0x55
}

// ---- 8006CD6C -------------------------------------------------------------------

/// 8006CD6C's case bodies (`j 8006CE54` with `li v0, N` in the delay slot,
/// or `clear v0` at the default label 8006CE50), by code address.
fn aircraft_case(target: u32) -> Option<u32> {
    Some(match target {
        0x8006_cd98 => 3,
        0x8006_cda0 => 4,
        0x8006_cda8 => 7,
        0x8006_cdb0 => 8,
        0x8006_cdb8 => 9,
        0x8006_cdc0 => 0xa,
        0x8006_cdc8 => 0xb,
        0x8006_cdd0 => 0x13,
        0x8006_cdd8 => 0xc,
        0x8006_cde0 => 1,
        0x8006_cde8 => 2,
        0x8006_cdf0 => 5,
        0x8006_cdf8 => 6,
        0x8006_ce00 => 0xd,
        0x8006_ce08 => 0x12,
        0x8006_ce10 => 0xf,
        0x8006_ce18 => 0x10,
        0x8006_ce20 => 0x14,
        0x8006_ce28 => 0xe,
        0x8006_ce30 => 0x11,
        0x8006_ce38 => 0x16,
        0x8006_ce40 => 0x15,
        0x8006_ce48 => 0x17,
        0x8006_ce50 => 0,
        _ => return None,
    })
}

/// 8006CD6C (JP; PAL 8006B59C, near, `imm:1`): a0 = aircraft id
/// (sign-extended halfword). Ids 1..=30 (PAL 1..=42) select a case through
/// the resident jump table at 0x80024668; any other id uses the default
/// (0). Returns `[0x800B3AC8 + case * 4]`, the parameter block.
pub fn aircraft_parameters(m: &mut Machine, aircraft: u32) -> u32 {
    let case = aircraft.wrapping_sub(1);
    let index = if case < 0x1e {
        let target = m.lw(AIRCRAFT_CASES.wrapping_add(case << 2));
        match aircraft_case(target) {
            Some(index) => index,
            None => {
                // `jr` to an address that is not a case body.
                m.raise(Fault::Untranslated { address: target });
                return 0;
            }
        }
    } else {
        0
    };
    m.lw(AIRCRAFT_PARAMETERS.wrapping_add(index << 2))
}
