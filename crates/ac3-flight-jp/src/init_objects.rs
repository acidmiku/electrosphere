//! Initializers 8007DA98, 80084A6C, 8006C588, 8006CE70 and 8004D794 (direct
//! callees of gameplay initialization 80032754, in that order) with their
//! complete static call trees. Translated from the instructions; validated
//! against original entry/return snapshots in `tests/init_objects.rs`.
//!
//! The trees also reach routines of `sdk.rs` (RotMatrixYXZ, ApplyMatrixLV,
//! rand, SetRotMatrix) and `shared.rs` (80030A68, and the player placement
//! 80072780 with 8006EE38, which the mission script also reaches); they are
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
    common::{self, GAMEPLAY, RESOURCES},
    machine::{Machine, SmallData},
    sdk, shared,
};
use ac3_gte::reg;

/// `[gp+0x53C]`: 0x1C248-byte ring-channel block (8007DA98).
pub const RING_CHANNELS: u32 = 0x53c;
/// `[gp+0x41C]`: 0x3B54-byte effect tables (80084A6C).
pub const EFFECT_TABLES: u32 = 0x41c;
/// `[gp+0x50C]`: 0x3564-byte slot table (8006C588).
pub const SLOT_TABLE: u32 = 0x50c;
/// `[gp+0x520]`: 0x70-byte player flight state (8006CCE0).
pub const PLAYER_STATE: u32 = 0x520;
/// `[gp+0x524]`: 0x3C-byte player aircraft limits (8006CCE0).
pub const PLAYER_LIMITS: u32 = 0x524;
/// Absolute word 0x800BD0A0 (`gp+0x52C`, addressed absolutely by 8006CCE0):
/// the last 0x38-byte player block.
pub const PLAYER_BLOCK_800BAF28: u32 = 0x800b_d0a0;

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

// ---- 8007DA98 ---------------------------------------------------------------------

/// 8007DA98: no arguments. Allocates the 0x1C248-byte ring-channel block
/// (32 channels of 0x908 bytes after an allocation bitmask word), stores it
/// in `[gp+0x53C]` and resets it (8007DA1C). Returns `v0` (0).
pub fn init_ring_channels(m: &mut Machine) -> u32 {
    let block = arena::alloc(m, 0x1_c248);
    m.set_gw(RING_CHANNELS, block);
    reset_ring_channels(m)
}

/// 8007DA1C: no arguments. Clears the bitmask word (+0) and the word at
/// +0x12124 of `[gp+0x53C]`, and fills the 32-byte maps at +0x12104 and
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

// ---- 80084A6C ---------------------------------------------------------------------

// JP-REVIEW(R0207): near function: PAL 80083414 -> JP 80084A6C, imm:13, 13 instructions differ (whole routine)
/// 80084A6C: no arguments. Allocates the 0x3B54-byte effect tables
/// (`[gp+0x41C]`), fills the random tables (80084B84), the light-direction
/// vectors at +0x5C8 (8008B178) and the resource parameters at +0x3948
/// (800900A4), and clears the effect lists (800849EC). Then, when context
/// word +0x970 has bit 0x10 and context halfword +0x390 is not 0x13, 0x11 or
/// 0x10, context halfword +0x5E selects bytes +0x3AE/+0x3AF: mask 0x1008
/// (unless +0x390 is 0x22, -1, 0x21 or 0x20) gives 7/7, else 0x4008 gives
/// 0xA/0xA, else 0x8008 gives 5/5, else 0x2008 gives +0x3AF = 9.
/// Returns `v0`: 0x13 (bit clear), 0x11/0x10 (excluded +0x390), the context
/// pointer (a 7/0xA/5 store) or 9.
// JP-PATCH: replaced by `crate::jp::res_p3_init_objects::init_effect_tables` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn init_effect_tables(m: &mut Machine) -> u32 {
    crate::jp::res_p3_init_objects::init_effect_tables(m)
}

/// Four `rand() & 0x7FF` summed, `sra 2`.
fn random_average(m: &mut Machine) -> u32 {
    let sum = (0..4).fold(0u32, |sum, _| sum + (sdk::rand(m) & 0x7ff));
    ((sum as i32) >> 2) as u32
}

/// 80084B84: a0 = table. Fills 0x210 signed halfwords at +0 (magnitude
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

/// 8008B178: a0 = output table. Builds the rotation of the light angles
/// (word +0x3C of `[[gp+0x2E0]+0x244]`, with z = 0) in scratchpad
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

/// 800900A4: a0 = destination. Copies the 0x14 bytes at
/// `[[gp+0x2DC]+0xC]+0x7C` to destination +8 (unaligned word copies).
/// Returns `v0`, the source block `[[gp+0x2DC]+0xC]`.
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

// JP-REVIEW(R0208): changed function: PAL 800833A4 -> JP 800849EC, del:1 imm:2 ins:5 op:4 reg:6, 18 instructions differ (whole routine)
/// 800849EC: no arguments. Clears the list heads of `[gp+0x41C]`: words
/// +0x600/+0x108C (three each), +0x1098..+0x10BC, +0x36C4, 32 words at
/// stride 0x14 from +0x36D8, and +0x33C0, +0x3948, +0x394C, +0x3964, +0x3B28.
/// Returns `v0`, the last loop pointer (block - 0x14).
// JP-PATCH: replaced by `crate::jp::res_p3_init_objects::reset_effect_lists` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn reset_effect_lists(m: &mut Machine) -> u32 {
    crate::jp::res_p3_init_objects::reset_effect_lists(m)
}

// ---- 8006C588 ---------------------------------------------------------------------

/// 8006C588: no arguments. Allocates the 0x3564-byte slot table, stores it
/// in `[gp+0x50C]` and resets it (8006C530). Returns `v0` (table - 4).
pub fn init_slot_table(m: &mut Machine) -> u32 {
    let table = arena::alloc(m, 0x3564);
    m.set_gw(SLOT_TABLE, table);
    reset_slot_table(m)
}

/// 8006C530: no arguments. Clears words +4 and +0 of `[gp+0x50C]`, sets the
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

// ---- 8006CE70 ---------------------------------------------------------------------

// JP-REVIEW(R0209): near function: PAL 8006B6A0 -> JP 8006CE70, imm:2, 2 instructions differ (whole routine)
/// 8006CE70: a0 = the first entity (`[[gp+0x2E8]]` in 80032754). Allocates
/// the player blocks (8006CCE0), selects the aircraft parameter block from
/// a0's halfword +0x48 (8006CD6C), and initializes the player entity
/// (`[[gp+0x2E8]]`): vector +0x20 = (0, 0, 0x20000) rotated by the angles at
/// +0x18 into +0x38 (80030EF8), word +0x54 = 0x80000000, halfword +0x4E from
/// the parameters. It then fills the flight state `[gp+0x520]` and the limits
/// `[gp+0x524]` (parameter words +0x74/+0x78/+0x84, halfwords +0x7C/+0x80/
/// +0x82, +0x7C capped by the per-mode table at 0x800B3B28), and resets the
/// bytes at gp+0x32C (80030178). Returns `v0` (0x55, from 80030178).
// JP-PATCH: replaced by `crate::jp::res_p3_init_objects::init_player_flight` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn init_player_flight(m: &mut Machine, entity: u32) -> u32 {
    crate::jp::res_p3_init_objects::init_player_flight(m, entity)
}

/// 8006CCE0: no arguments. Allocates the player blocks: 8 bytes at
/// `[gp+0x514]` (four halfwords cleared), 0x10 bytes at `[gp+0x51C]` (six
/// halfwords and word +0xC cleared), 0x10 bytes at `[gp+0x518]`, 0x70 at
/// `[gp+0x520]`, 0x3C at `[gp+0x524]` and 0x38 at 0x800BD0A0. Returns `v0`,
/// the last block.
pub fn allocate_player_blocks(m: &mut Machine) -> u32 {
    let block = arena::alloc(m, 8);
    m.set_gw(0x514, block);
    for offset in [0, 2, 4, 6] {
        m.sh(block.wrapping_add(offset), 0);
    }
    let block = arena::alloc(m, 0x10);
    m.set_gw(0x51c, block);
    for offset in [0, 2, 4, 6, 8, 0xa] {
        m.sh(block.wrapping_add(offset), 0);
    }
    m.sw(block.wrapping_add(0xc), 0);
    let block = arena::alloc(m, 0x10);
    m.set_gw(0x518, block);
    let block = arena::alloc(m, 0x70);
    m.set_gw(PLAYER_STATE, block);
    let block = arena::alloc(m, 0x3c);
    m.set_gw(PLAYER_LIMITS, block);
    let block = arena::alloc(m, 0x38);
    m.sw(PLAYER_BLOCK_800BAF28, block);
    block
}

// JP-REVIEW(R0211): near function: PAL 8006B59C -> JP 8006CD6C, imm:1, 1 instructions differ (whole routine)
/// 8006CD6C: a0 = aircraft id (sign-extended halfword). Ids 1..=42 select a
/// case through the resident jump table at 0x80024668; any other id uses the
/// default (0). Returns `[0x800B3AC8 + case * 4]`, the parameter block.
// JP-PATCH: replaced by `crate::jp::res_p3_init_objects::aircraft_parameters` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn aircraft_parameters(m: &mut Machine, aircraft: u32) -> u32 {
    crate::jp::res_p3_init_objects::aircraft_parameters(m, aircraft)
}
/// 80030EF8: a0 = VECTOR (three long words), a1 = angles (three 14-bit
/// halfwords), a2 = output VECTOR. Builds the 0x4000-scale rotation
/// (80030A68) in its 0x40-byte frame (MATRIX at +0x10), applies it with
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

// ---- 8004D794 ---------------------------------------------------------------------

/// 8004D794: a0 is not read (80032754 passes 0). Copies the six-word start
/// record at `[[gp+0x384]+0xC]+4` (words +4..+0x10 aligned, +0x14/+0x18
/// unaligned) into its 0x30-byte frame (+0x10) and places the player there
/// (80072780). Returns `v0` (80072780's).
pub fn init_player_start(m: &mut Machine) -> u32 {
    let frame = m.push_frame(0x30);
    let start = frame + 0x10;
    let pointer = m.gw(0x384);
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
        0x8007_da98 => init_ring_channels(m),
        0x8007_da1c => reset_ring_channels(m),
        0x8008_4a6c => init_effect_tables(m),
        0x8008_4b84 => fill_random_tables(m, a0),
        0x8008_b178 => light_direction_vectors(m, a0),
        0x8009_00a4 => copy_resource_parameters(m, a0),
        0x8008_49ec => reset_effect_lists(m),
        0x8006_c588 => init_slot_table(m),
        0x8006_c530 => reset_slot_table(m),
        0x8006_ce70 => init_player_flight(m, a0),
        0x8006_cce0 => allocate_player_blocks(m),
        0x8006_cd6c => aircraft_parameters(m, a0),
        0x8003_0ef8 => rotate_vector_q14(m, a0, a1, a2),
        0x8004_d794 => init_player_start(m),
        _ => return None,
    })
}
