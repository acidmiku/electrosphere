//! Initializers 8007C41C, 80083414, 8006ADB8, 8006B6A0 and 8004C1A0 (direct
//! callees of gameplay initialization 80032510, in that order) with their
//! complete static call trees. Translated from the instructions; validated
//! against original entry/return snapshots in `tests/init_objects.rs`.
//!
//! The trees also reach routines of `sdk.rs` (RotMatrixYXZ, ApplyMatrixLV,
//! rand, SetRotMatrix) and `shared.rs` (80030824, and the player placement
//! 800710FC with 8006D7DC, which the mission script also reaches); they are
//! called there.
//!
//! Stack locals whose address the original passes to a callee live in guest
//! frames (`Machine::push_frame` with the original frame size); the others
//! stay in Rust. A MATRIX local's pad halfword (after m\[2\]\[2\]) is never
//! written by the routines that build it, so it holds stale stack data, as in
//! the original. It only reaches the high half of R33, which the
//! sign-extending `ctc2` discards, or the high half of VZ0, which is reloaded
//! before anything observes it.
//!
//! GTE write-to-use hazards (Redux `gte-write-to-use.h`): every command here
//! has at least two instructions after a VXY0/IR0/IR1 write and at least one
//! after an IR2 write. ApplyMatrixLV writes IR3 one instruction before RTIR,
//! and the MVMVA vector z component needs no gap, so no hazard applies.
use crate::{
    arena,
    common::{self, CONTEXT, ENTITIES, GAMEPLAY, RESOURCES},
    machine::{Fault, Machine, SmallData},
    sdk, shared,
};
use ac3_gte::reg;

/// `[gp+0x544]`: 0x1C248-byte ring-channel block (8007C41C).
pub const RING_CHANNELS: u32 = 0x544;
/// `[gp+0x40C]`: 0x3B54-byte effect tables (80083414).
pub const EFFECT_TABLES: u32 = 0x40c;
/// `[gp+0x514]`: 0x3564-byte slot table (8006ADB8).
pub const SLOT_TABLE: u32 = 0x514;
/// `[gp+0x528]`: 0x70-byte player flight state (8006B510).
pub const PLAYER_STATE: u32 = 0x528;
/// `[gp+0x52C]`: 0x3C-byte player aircraft limits (8006B510).
pub const PLAYER_LIMITS: u32 = 0x52c;
/// Absolute word 0x800BAF28 (`gp+0x534`, addressed absolutely by 8006B510):
/// the last 0x38-byte player block.
pub const PLAYER_BLOCK_800BAF28: u32 = 0x800b_af28;

/// 8006B59C jump table: 42 case addresses for aircraft ids 1..=42.
const AIRCRAFT_CASES: u32 = 0x8002_4128;
/// 8006B59C result: 24 aircraft parameter block pointers.
const AIRCRAFT_PARAMETERS: u32 = 0x800b_1838;
/// 8006B6A0 per-mode cap halfwords, indexed by context byte +0x3AE.
const AIRCRAFT_CAPS: u32 = 0x800b_1898;
// ---- small machine helpers ------------------------------------------------------

/// `lwl/lwr` pair: an unaligned little-endian word.
fn load_unaligned(m: &mut Machine, address: u32) -> u32 {
    u32::from_le_bytes(std::array::from_fn(|i| {
        m.lbu(address.wrapping_add(i as u32))
    }))
}

/// `swl/swr` pair: an unaligned little-endian word.
fn store_unaligned(m: &mut Machine, address: u32, value: u32) {
    for (i, byte) in value.to_le_bytes().into_iter().enumerate() {
        m.sb(address.wrapping_add(i as u32), byte);
    }
}

fn load_words<const N: usize>(m: &mut Machine, address: u32) -> [u32; N] {
    std::array::from_fn(|i| m.lw(address.wrapping_add(i as u32 * 4)))
}

/// RTV0 (`4A486012`): MVMVA sf=1, rotation, V0, no translation.
fn rtv0(m: &mut Machine) {
    m.gte.mvmva(true, false, 0, 0, 3);
}

/// `stsv`: `mfc2` IR1..IR3, then three halfword stores.
fn store_ir_halfwords(m: &mut Machine, address: u32) {
    let values: [u32; 3] = std::array::from_fn(|i| m.gte.mfc2(reg::IR1 + i));
    for (i, value) in values.into_iter().enumerate() {
        m.sh(address.wrapping_add(i as u32 * 2), value as u16);
    }
}

// ---- 8007C41C ---------------------------------------------------------------------

/// 8007C41C: no arguments. Allocates the 0x1C248-byte ring-channel block
/// (32 channels of 0x908 bytes after an allocation bitmask word), stores it
/// in `[gp+0x544]` and resets it (8007C3A0). Returns `v0` (0).
pub fn init_ring_channels(m: &mut Machine) -> u32 {
    let block = arena::alloc(m, 0x1_c248);
    m.set_gw(RING_CHANNELS, block);
    reset_ring_channels(m)
}

/// 8007C3A0: no arguments. Clears the bitmask word (+0) and the word at
/// +0x12124 of `[gp+0x544]`, and fills the 32-byte maps at +0x12104 and
/// +0x1C228 with 0xFF. Returns `v0` (the final loop test, 0).
pub fn reset_ring_channels(m: &mut Machine) -> u32 {
    let block = m.gw(RING_CHANNELS);
    m.sw(block, 0);
    m.sw(block.wrapping_add(0x1_2124), 0);
    for i in 0..0x20 {
        let block = m.gw(RING_CHANNELS);
        m.sb(block.wrapping_add(i).wrapping_add(0x1_2104), 0xff);
    }
    for i in 0..0x20 {
        let block = m.gw(RING_CHANNELS);
        m.sb(block.wrapping_add(i).wrapping_add(0x1_c228), 0xff);
    }
    0
}

// ---- 80083414 ---------------------------------------------------------------------

/// 80083414: no arguments. Allocates the 0x3B54-byte effect tables
/// (`[gp+0x40C]`), fills the random tables (8008352C), the light-direction
/// vectors at +0x5C8 (80089A08) and the resource parameters at +0x3948
/// (8008E834), and clears the effect lists (800833A4). Then, when context
/// word +0x970 has bit 0x10 and context halfword +0x390 is not 0x13, 0x11 or
/// 0x10, context halfword +0x5E selects bytes +0x3AE/+0x3AF: mask 0x1008
/// (unless +0x390 is 0x22, -1, 0x21 or 0x20) gives 7/7, else 0x4008 gives
/// 0xA/0xA, else 0x8008 gives 5/5, else 0x2008 gives +0x3AF = 9.
/// Returns `v0`: 0x13 (bit clear), 0x11/0x10 (excluded +0x390), the context
/// pointer (a 7/0xA/5 store) or 9.
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
    if m.lw(context.wrapping_add(0x970)) & 0x10 == 0 {
        return 0x13;
    }
    let mode = i32::from(m.lh(context.wrapping_add(0x390)));
    let bits = u32::from(m.lhu(context.wrapping_add(0x5e)));
    match mode {
        0x13 => return 0x11,
        0x11 | 0x10 => return 0x10,
        _ => {}
    }
    let value = if bits & 0x1008 == 0x1008 && !matches!(mode, 0x22 | -1 | 0x21 | 0x20) {
        // Stored through the context pointer loaded above.
        m.sb(context.wrapping_add(0x3ae), 7);
        7
    } else if bits & 0x4008 == 0x4008 {
        let context = m.gw(CONTEXT);
        m.sb(context.wrapping_add(0x3ae), 0xa);
        0xa
    } else if bits & 0x8008 == 0x8008 {
        let context = m.gw(CONTEXT);
        m.sb(context.wrapping_add(0x3ae), 5);
        5
    } else {
        if bits & 0x2008 == 0x2008 {
            let context = m.gw(CONTEXT);
            m.sb(context.wrapping_add(0x3af), 9);
        }
        return 9;
    };
    let context = m.gw(CONTEXT);
    m.sb(context.wrapping_add(0x3af), value);
    context
}

/// Four `rand() & 0x7FF` summed, `sra 2`.
fn random_average(m: &mut Machine) -> u32 {
    let sum = (0..4).fold(0u32, |sum, _| sum + (sdk::rand(m) & 0x7ff));
    ((sum as i32) >> 2) as u32
}

/// 8008352C: a0 = table. Fills 0x210 signed halfwords at +0 (magnitude
/// `rand()`, negated when a second `rand()` is odd), then 0x6A packed words
/// at +0x420: three averages of four `rand() & 0x7FF`, each minus 0x400,
/// packed as `x & 0x7FF | (y & 0x7FE) << 10 | z << 21`. Returns `v0` (the
/// final loop test, 0).
pub fn fill_random_tables(m: &mut Machine, table: u32) -> u32 {
    for i in 0..0x210u32 {
        let magnitude = sdk::rand(m);
        let value = if sdk::rand(m) & 1 != 0 {
            magnitude.wrapping_neg()
        } else {
            magnitude
        };
        m.sh(table.wrapping_add(i * 2), value as u16);
    }
    for i in 0..0x6au32 {
        let x = random_average(m);
        let y = random_average(m);
        let z = random_average(m);
        let word = (x.wrapping_sub(0x400) & 0x7ff)
            | ((y.wrapping_sub(0x400) & 0x7fe) << 10)
            | (z.wrapping_sub(0x400) << 21);
        m.sw(table.wrapping_add(0x420 + i * 4), word);
    }
    0
}

/// 80089A08: a0 = output table. Builds the rotation of the light angles
/// (word +0x3C of `[[gp+0x2D0]+0x244]`, with z = 0) in scratchpad
/// 0x1F80001C/0x1F800024 through RotMatrixYXZ, then rotates (RTV0) and
/// stores: (0, 0, 0x4000) as two scratch words at +8; the four corners
/// (±442, ±442, 0x4000) as IR halfwords at +0x10 stride 8; and (0, 0, 0x1000)
/// as two scratch words at +0x30. Word +0 is cleared. Returns `v0` (the
/// final loop test, 0).
pub fn light_direction_vectors(m: &mut Machine, out: u32) -> u32 {
    const VECTOR: u32 = 0x1f80_001c;
    const MATRIX: u32 = 0x1f80_0024;
    let gameplay = m.gw(GAMEPLAY);
    let section = m.lw(gameplay.wrapping_add(0x244));
    let angles = m.lw(section.wrapping_add(0x3c));
    m.sh(VECTOR + 4, 0);
    m.sw(VECTOR, angles);
    sdk::rot_matrix_yxz(m, VECTOR, MATRIX);
    sdk::set_rot_matrix(m, MATRIX);
    m.gte.mtc2(reg::VXY0, 0);
    m.gte.mtc2(reg::VZ0, 0x4000);
    m.sw(out, 0);
    rtv0(m);
    store_ir_halfwords(m, VECTOR);
    let xy = m.lw(VECTOR);
    let z = m.lw(VECTOR + 4);
    m.sw(out.wrapping_add(8), xy);
    m.sw(out.wrapping_add(0xc), z);
    let mut corner = out.wrapping_add(0x10);
    for index in 0..4u32 {
        // 442 * sign, computed by the original as (s*7 << 6) - s*7 + s.
        let sign_x = ((index & 1) << 1) as i32 - 1;
        let sign_y = (index & 2) as i32 - 1;
        m.sh(VECTOR, (sign_x * 442) as u16);
        m.sh(VECTOR + 2, (sign_y * 442) as u16);
        m.sh(VECTOR + 4, 0x4000);
        let xy = m.lw(VECTOR);
        let z = m.lw(VECTOR + 4);
        m.gte.mtc2(reg::VXY0, xy);
        m.gte.mtc2(reg::VZ0, z);
        rtv0(m);
        store_ir_halfwords(m, corner);
        corner = corner.wrapping_add(8);
    }
    m.gte.mtc2(reg::VXY0, 0);
    m.gte.mtc2(reg::VZ0, 0x1000);
    rtv0(m);
    store_ir_halfwords(m, VECTOR);
    let xy = m.lw(VECTOR);
    let z = m.lw(VECTOR + 4);
    m.sw(corner, xy);
    m.sw(corner.wrapping_add(4), z);
    0
}

/// 8008E834: a0 = destination. Copies the 0x14 bytes at
/// `[[gp+0x2CC]+0xC]+0x7C` to destination +8 (unaligned word copies).
/// Returns `v0`, the source block `[[gp+0x2CC]+0xC]`.
pub fn copy_resource_parameters(m: &mut Machine, out: u32) -> u32 {
    let resources = m.gw(RESOURCES);
    let source = m.lw(resources.wrapping_add(0xc));
    let words: [u32; 4] =
        std::array::from_fn(|i| load_unaligned(m, source.wrapping_add(0x7c + i as u32 * 4)));
    for (i, word) in words.into_iter().enumerate() {
        store_unaligned(m, out.wrapping_add(8 + i as u32 * 4), word);
    }
    let last = load_unaligned(m, source.wrapping_add(0x8c));
    store_unaligned(m, out.wrapping_add(0x18), last);
    source
}

/// 800833A4: no arguments. Clears the list heads of `[gp+0x40C]`: words
/// +0x600/+0x108C (three each), +0x1098..+0x10BC, +0x36C4, 32 words at
/// stride 0x14 from +0x36D8, and +0x33C0, +0x3948, +0x394C, +0x3964, +0x3B28.
/// Returns `v0`, the last loop pointer (block - 0x14).
pub fn reset_effect_lists(m: &mut Machine) -> u32 {
    let block = m.gw(EFFECT_TABLES);
    for i in 0..3u32 {
        let at = block.wrapping_add(i * 4);
        m.sw(at.wrapping_add(0x600), 0);
        m.sw(at.wrapping_add(0x108c), 0);
    }
    for i in 0..10u32 {
        m.sw(block.wrapping_add(0x10bc - i * 4), 0);
    }
    m.sw(block.wrapping_add(0x36c4), 0);
    for i in 0..0x20u32 {
        m.sw(block.wrapping_add(0x3944 - i * 0x14), 0);
    }
    for offset in [0x33c0, 0x3948, 0x394c, 0x3964, 0x3b28] {
        m.sw(block.wrapping_add(offset), 0);
    }
    block.wrapping_sub(0x14)
}

// ---- 8006ADB8 ---------------------------------------------------------------------

/// 8006ADB8: no arguments. Allocates the 0x3564-byte slot table, stores it
/// in `[gp+0x514]` and resets it (8006AD60). Returns `v0` (table - 4).
pub fn init_slot_table(m: &mut Machine) -> u32 {
    let table = arena::alloc(m, 0x3564);
    m.set_gw(SLOT_TABLE, table);
    reset_slot_table(m)
}

/// 8006AD60: no arguments. Clears words +4 and +0 of `[gp+0x514]`, sets the
/// 64 bytes at +8 to 0xFF and the 64 bytes at +0x48 and +0x88 to zero, then
/// clears words +0x154C, +0x33DC..+0x33D0, +0x1548 and +0x33E0. Returns
/// `v0`, the last loop pointer (table - 4).
pub fn reset_slot_table(m: &mut Machine) -> u32 {
    let table = m.gw(SLOT_TABLE);
    m.sw(table.wrapping_add(4), 0);
    m.sw(table, 0);
    for i in 0..0x40 {
        let at = table.wrapping_add(i);
        m.sb(at.wrapping_add(8), 0xff);
        m.sb(at.wrapping_add(0x48), 0);
        m.sb(at.wrapping_add(0x88), 0);
    }
    m.sw(table.wrapping_add(0x154c), 0);
    for i in 0..4u32 {
        m.sw(table.wrapping_add(0x33dc - i * 4), 0);
    }
    m.sw(table.wrapping_add(0x1548), 0);
    m.sw(table.wrapping_add(0x33e0), 0);
    table.wrapping_sub(4)
}

// ---- 8006B6A0 ---------------------------------------------------------------------

/// 8006B6A0: a0 = the first entity (`[[gp+0x2D8]]` in 80032510). Allocates
/// the player blocks (8006B510), selects the aircraft parameter block from
/// a0's halfword +0x48 (8006B59C), and initializes the player entity
/// (`[[gp+0x2D8]]`): vector +0x20 = (0, 0, 0x20000) rotated by the angles at
/// +0x18 into +0x38 (80030CB4), word +0x54 = 0x80000000, halfword +0x4E from
/// the parameters. It then fills the flight state `[gp+0x528]` and the limits
/// `[gp+0x52C]` (parameter words +0x74/+0x78/+0x84, halfwords +0x7C/+0x80/
/// +0x82, +0x7C capped by the per-mode table at 0x800B1898), and resets the
/// bytes at gp+0x31C (8002FF34). Returns `v0` (0x55, from 8002FF34).
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
    if (m.lw(context.wrapping_add(0x398)) as i32) >= 2 {
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
    let mode = u32::from(m.lbu(context.wrapping_add(0x3ae)));
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
    // `v0` of 8002FF34: its last `li v0, 0x55`.
    0x55
}

/// 8006B510: no arguments. Allocates the player blocks: 8 bytes at
/// `[gp+0x51C]` (four halfwords cleared), 0x10 bytes at `[gp+0x524]` (six
/// halfwords and word +0xC cleared), 0x10 bytes at `[gp+0x520]`, 0x70 at
/// `[gp+0x528]`, 0x3C at `[gp+0x52C]` and 0x38 at 0x800BAF28. Returns `v0`,
/// the last block.
pub fn allocate_player_blocks(m: &mut Machine) -> u32 {
    let block = arena::alloc(m, 8);
    m.set_gw(0x51c, block);
    for offset in [0, 2, 4, 6] {
        m.sh(block.wrapping_add(offset), 0);
    }
    let block = arena::alloc(m, 0x10);
    m.set_gw(0x524, block);
    for offset in [0, 2, 4, 6, 8, 0xa] {
        m.sh(block.wrapping_add(offset), 0);
    }
    m.sw(block.wrapping_add(0xc), 0);
    let block = arena::alloc(m, 0x10);
    m.set_gw(0x520, block);
    let block = arena::alloc(m, 0x70);
    m.set_gw(PLAYER_STATE, block);
    let block = arena::alloc(m, 0x3c);
    m.set_gw(PLAYER_LIMITS, block);
    let block = arena::alloc(m, 0x38);
    m.sw(PLAYER_BLOCK_800BAF28, block);
    block
}

/// 8006B59C's case bodies (`j 8006B684` with `li v0, N` in the delay slot,
/// or `clear v0` at the default label 8006B680), by code address.
fn aircraft_case(target: u32) -> Option<u32> {
    Some(match target {
        0x8006_b5c8 => 3,
        0x8006_b5d0 => 4,
        0x8006_b5d8 => 7,
        0x8006_b5e0 => 8,
        0x8006_b5e8 => 9,
        0x8006_b5f0 => 0xa,
        0x8006_b5f8 => 0xb,
        0x8006_b600 => 0x13,
        0x8006_b608 => 0xc,
        0x8006_b610 => 1,
        0x8006_b618 => 2,
        0x8006_b620 => 5,
        0x8006_b628 => 6,
        0x8006_b630 => 0xd,
        0x8006_b638 => 0x12,
        0x8006_b640 => 0xf,
        0x8006_b648 => 0x10,
        0x8006_b650 => 0x14,
        0x8006_b658 => 0xe,
        0x8006_b660 => 0x11,
        0x8006_b668 => 0x16,
        0x8006_b670 => 0x15,
        0x8006_b678 => 0x17,
        0x8006_b680 => 0,
        _ => return None,
    })
}

/// 8006B59C: a0 = aircraft id (sign-extended halfword). Ids 1..=42 select a
/// case through the resident jump table at 0x80024128; any other id uses the
/// default (0). Returns `[0x800B1838 + case * 4]`, the parameter block.
pub fn aircraft_parameters(m: &mut Machine, aircraft: u32) -> u32 {
    let case = aircraft.wrapping_sub(1);
    let index = if case < 0x2a {
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
/// 80030CB4: a0 = VECTOR (three long words), a1 = angles (three 14-bit
/// halfwords), a2 = output VECTOR. Builds the 0x4000-scale rotation
/// (80030824) in its 0x40-byte frame (MATRIX at +0x10), applies it with
/// ApplyMatrixLV and shifts each output component right by 2. Returns `v0`,
/// the shifted y component.
pub fn rotate_vector_q14(m: &mut Machine, vector: u32, angles: u32, out: u32) -> u32 {
    let frame = m.push_frame(0x40);
    let matrix = frame + 0x10;
    shared::rotation_yxz_q14(m, angles, matrix);
    sdk::apply_matrix_lv(m, matrix, vector, out);
    let x = m.lw(out);
    let z = m.lw(out.wrapping_add(8));
    m.sw(out, ((x as i32) >> 2) as u32);
    let y = ((m.lw(out.wrapping_add(4)) as i32) >> 2) as u32;
    m.sw(out.wrapping_add(8), ((z as i32) >> 2) as u32);
    m.sw(out.wrapping_add(4), y);
    m.pop_frame(0x40);
    y
}

// ---- 8004C1A0 ---------------------------------------------------------------------

/// 8004C1A0: a0 is not read (80032510 passes 0). Copies the six-word start
/// record at `[[gp+0x374]+0xC]+4` (words +4..+0x10 aligned, +0x14/+0x18
/// unaligned) into its 0x30-byte frame (+0x10) and places the player there
/// (800710FC). Returns `v0` (800710FC's).
pub fn init_player_start(m: &mut Machine) -> u32 {
    let frame = m.push_frame(0x30);
    let start = frame + 0x10;
    let pointer = m.gw(0x374);
    let record = m.lw(pointer.wrapping_add(0xc));
    let aligned: [u32; 4] = load_words(m, record.wrapping_add(4));
    for (i, word) in aligned.into_iter().enumerate() {
        m.sw(start + i as u32 * 4, word);
    }
    let fifth = load_unaligned(m, record.wrapping_add(0x14));
    let sixth = load_unaligned(m, record.wrapping_add(0x18));
    store_unaligned(m, start + 0x10, fifth);
    store_unaligned(m, start + 0x14, sixth);
    let v0 = shared::place_player(m, start);
    m.pop_frame(0x30);
    v0
}

/// Every routine this module translates, by original address, with the
/// original `a0..a3` and returning `v0` (`dispatch::call` convention).
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let [a0, a1, a2, _] = args;
    Some(match target {
        0x8007_c41c => init_ring_channels(m),
        0x8007_c3a0 => reset_ring_channels(m),
        0x8008_3414 => init_effect_tables(m),
        0x8008_352c => fill_random_tables(m, a0),
        0x8008_9a08 => light_direction_vectors(m, a0),
        0x8008_e834 => copy_resource_parameters(m, a0),
        0x8008_33a4 => reset_effect_lists(m),
        0x8006_adb8 => init_slot_table(m),
        0x8006_ad60 => reset_slot_table(m),
        0x8006_b6a0 => init_player_flight(m, a0),
        0x8006_b510 => allocate_player_blocks(m),
        0x8006_b59c => aircraft_parameters(m, a0),
        0x8003_0cb4 => rotate_vector_q14(m, a0, a1, a2),
        0x8004_c1a0 => init_player_start(m),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::GP;

    #[test]
    fn aircraft_parameters_default_and_bad_targets() {
        let mut m = Machine::default();
        for i in 0..24 {
            m.sw(AIRCRAFT_PARAMETERS + i * 4, 0x8010_0000 + i);
        }
        m.sw(AIRCRAFT_CASES, 0x8006_b5c8);
        m.sw(AIRCRAFT_CASES + 4 * 41, 0x8006_b680);
        assert_eq!(aircraft_parameters(&mut m, 1), 0x8010_0003);
        assert_eq!(aircraft_parameters(&mut m, 42), 0x8010_0000);
        assert_eq!(aircraft_parameters(&mut m, 0), 0x8010_0000, "id 0");
        assert_eq!(aircraft_parameters(&mut m, 43), 0x8010_0000);
        assert_eq!(aircraft_parameters(&mut m, 0xffff_ffff), 0x8010_0000);
        assert!(m.take_fault().is_ok());
        m.sw(AIRCRAFT_CASES + 4, 0x8006_b5cc);
        assert_eq!(aircraft_parameters(&mut m, 2), 0);
        assert_eq!(
            m.take_fault(),
            Err(Fault::Untranslated {
                address: 0x8006_b5cc
            })
        );
    }

    /// The context selection at the end of 80083414: each mask and mode
    /// exclusion (the capture takes the disabled path).
    #[test]
    fn effect_selection_paths() {
        let context = 0x800b_d158;
        let run = |enabled: bool, mode: i16, bits: u16| {
            let mut m = Machine::default();
            m.sw(GP + CONTEXT, context);
            m.sw(GP + RESOURCES, 0x800b_e168);
            m.sw(0x800b_e168 + 0xc, 0x8012_0000);
            m.sw(GP + GAMEPLAY, 0x8013_0000);
            m.sw(0x8013_0000 + 0x244, 0x8014_0000);
            m.sw(context + 0x18, 0x8015_0000);
            m.sw(context + 0x1c, 0x8015_0000);
            m.sw(context + 0x970, if enabled { 0x10 } else { 0 });
            m.sh(context + 0x390, mode as u16);
            m.sh(context + 0x5e, bits);
            m.sb(context + 0x3ae, 0xee);
            m.sb(context + 0x3af, 0xee);
            let v0 = init_effect_tables(&mut m);
            assert!(m.take_fault().is_ok());
            (m.lbu(context + 0x3ae), m.lbu(context + 0x3af), v0)
        };
        assert_eq!(run(false, 0, 0x1008), (0xee, 0xee, 0x13));
        assert_eq!(run(true, 0x13, 0x1008), (0xee, 0xee, 0x11));
        assert_eq!(run(true, 0x11, 0x1008), (0xee, 0xee, 0x10));
        assert_eq!(run(true, 0x10, 0x1008), (0xee, 0xee, 0x10));
        assert_eq!(run(true, 0, 0x1008), (7, 7, context));
        assert_eq!(run(true, -1, 0x1008), (0xee, 0xee, 9), "other masks only");
        assert_eq!(run(true, 0x21, 0x5008), (0xa, 0xa, context));
        assert_eq!(run(true, 0, 0x4008), (0xa, 0xa, context));
        assert_eq!(run(true, 0, 0x8008), (5, 5, context));
        assert_eq!(run(true, 0, 0x2008), (0xee, 9, 9));
        assert_eq!(run(true, 0, 0x2000), (0xee, 0xee, 9));
    }

    /// 8006B6A0's context +0x398 test (limit +0x14 forced to 0x1E from 2 up)
    /// and the per-mode cap of limit +4, on a synthetic machine.
    #[test]
    fn player_limit_branches() {
        let (context, player, parameters) = (0x800b_d158, 0x8014_1000, 0x8014_2000);
        let run = |level: u32, cap: i16| {
            let mut m = Machine::default();
            m.sw(GP + CONTEXT, context);
            m.sw(context + 0x18, 0x8015_0000);
            m.sw(context + 0x1c, 0x8015_0000);
            m.sw(context + 0x398, level);
            m.sb(context + 0x3ae, 2);
            m.sh(AIRCRAFT_CAPS + 4, cap as u16);
            m.sw(GP + ENTITIES, 0x8014_0000);
            m.sw(0x8014_0000, player);
            m.sh(player + 0x48, 1);
            m.sw(AIRCRAFT_CASES, 0x8006_b5c8);
            m.sw(AIRCRAFT_PARAMETERS + 3 * 4, parameters);
            m.sw(parameters + 0x84, 0x40);
            m.sh(parameters + 0x7c, 500);
            assert_eq!(init_player_flight(&mut m, player), 0x55);
            assert!(m.take_fault().is_ok());
            let limits = m.gw(PLAYER_LIMITS);
            (
                m.lw(limits + 0x14),
                m.lhu(player + 0x4e),
                m.lw(limits + 4) as i32,
                m.lw(limits),
            )
        };
        assert_eq!(run(1, 600), (0x40, 0x40, 500, parameters));
        assert_eq!(run(2, 600), (0x1e, 0x1e, 500, parameters));
        assert_eq!(run(0xffff_ffff, 499), (0x40, 0x40, 499, parameters));
        assert_eq!(run(5, -3), (0x1e, 0x1e, -3, parameters));
    }
}
