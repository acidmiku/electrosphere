//! Mission subsystem initialization 80038818 (called by 80032510) and its
//! complete subtree: mission tables (8004BBB8), script tables (800527C8),
//! mission state and init script (80052574), entities and objects
//! (80038758/800387C0), links, results, messages and effect sequences.
//!
//! Translated from `analysis/main/disassembly.txt`; validated by
//! `tests/init_38818.rs`.
use crate::common::{self, CONTEXT, ENTITIES, GAMEPLAY};
use crate::machine::{GP, Machine, SmallData};
use crate::script::{self, ENTITY_AUX, ENTITY_FLAGS, MISSION_ID};
use crate::{arena, dispatch, sdk, shared};

/// `0x800BADC0`: resident mission resource block (offset table).
pub const RESOURCES: u32 = 0x800b_adc0;
/// 64 entity motion blocks (pointers to 0x28-byte records).
pub const ENTITY_MOTION: u32 = 0x800c_4c98;
/// Mission number by mission index (context halfword +0x390).
pub const MISSION_NUMBERS: u32 = 0x800b_0210;

fn or_word(m: &mut Machine, address: u32, bits: u32) {
    let value = m.lw(address);
    m.sw(address, value | bits);
}
fn entity(m: &mut Machine, index: u32) -> u32 {
    let table = m.gw(ENTITIES);
    m.lw(table.wrapping_add(index.wrapping_mul(4)))
}

/// 80038818: initialize the mission subsystems for the current mission.
pub fn initialize_38818(m: &mut Machine) {
    const SIZE: u32 = 0x18;
    m.push_frame(SIZE);
    let context = m.gw(CONTEXT);
    let index = m.lh(context.wrapping_add(0x390)) as i32 as u32;
    let gameplay = m.gw(GAMEPLAY);
    let variants = m.lw(gameplay.wrapping_add(0x260));
    let id = m.lbu(index.wrapping_add(MISSION_NUMBERS));
    m.set_gw(0x3cc, variants);
    m.sb(MISSION_ID, id);
    if m.lw(variants) == 3 {
        let value = sdk::rand(m) as i32;
        let quotient = ((i64::from(value) * 0x5555_5556) >> 32) as i32 - (value >> 31);
        let choice = value.wrapping_sub(quotient.wrapping_mul(3)) as u32;
        let base = m.gw(0x3cc);
        let offset = m.lw(choice.wrapping_mul(4).wrapping_add(base).wrapping_add(4));
        m.set_gw(0x3cc, base.wrapping_add(offset));
    }
    load_mission_tables_8004bbb8(m);
    load_script_tables_800527c8(m);
    start_mission_state_80052574(m);
    reset_hud_8002c7a4(m);
    reset_message_queue_80054b54(m);
    reset_8005ac20(m);
    dispatch::call(m, 0x8004_b74c, [0; 4]); // hud.rs: no message shown
    m.sb(GP + 0x4f0, 0); // 80059C40
    reset_result_800477c8(m);
    initialize_entities_80038758(m);
    initialize_objects_8004cdb0(m); // via 800387C0
    reset_links_800453b8(m);
    let gameplay = m.gw(GAMEPLAY);
    let object = m.lw(gameplay.wrapping_add(0x28));
    bind_player_object_800387e0(m, object);
    common::reset_800501f8(m);
    m.pop_frame(SIZE);
}

// ---- mission tables ----------------------------------------------------------------

/// 8004BBB8: resolve the mission resource tables into the `gp+0x35A..0x3C4`
/// pointers and counts, the 16 effect sequences (0x800C4ED8) and four
/// routes (0x800C4C58), allocate the trigger (`[gp+0x370]`, 12-byte) and
/// group (`[gp+0x394]`) arrays, build units and objects, and clear the
/// entity aux/motion tables.
pub fn load_mission_tables_8004bbb8(m: &mut Machine) {
    let resources = m.lw(RESOURCES);
    let offset = m.lw(resources.wrapping_add(4));
    let header = resources.wrapping_add(offset);
    let word = |m: &mut Machine, field: u32| {
        let value = m.lw(header.wrapping_add(field));
        header.wrapping_add(value.wrapping_shl(2))
    };
    let triggers = m.lbu(header);
    m.sb(GP + 0x35a, triggers);
    let groups = m.lbu(header.wrapping_add(4));
    m.sb(GP + 0x36d, groups);
    let units = m.lbu(header.wrapping_add(8));
    m.sb(GP + 0x364, units);
    let objects = m.lbu(header.wrapping_add(0xc));
    for (field, slot) in [
        (0x10, 0x368),
        (0x14, 0x388),
        (0x18, 0x37c),
        (0x1c, 0x3a8),
        (0x20, 0x3c4),
        (0x24, 0x3c0),
        (0x28, 0x3b4),
        (0x2c, 0x3bc),
        (0x30, 0x380),
        (0x34, 0x3a4),
        (0x38, 0x3ac),
    ] {
        let value = word(m, field);
        m.set_gw(slot, value);
    }
    m.sb(GP + 0x36c, objects);
    let value = word(m, 0x3c);
    m.set_gw(0x39c, value);
    let sequences = word(m, 0x40);
    for i in 0..16 {
        let entry = m.lw(sequences.wrapping_add(i * 4));
        let value = if entry == u32::MAX {
            0
        } else {
            sequences.wrapping_add(0x40).wrapping_add(entry << 5)
        };
        m.sw(0x800c_4ed8 + i * 4, value);
    }
    let routes = word(m, 0x44);
    for i in 0..4 {
        let entry = m.lw(routes.wrapping_add(i * 4));
        let value = if entry == u32::MAX {
            0
        } else {
            routes.wrapping_add(entry)
        };
        m.sw(0x800c_4c58 + i * 4, value);
    }
    let count = u32::from(m.lbu(GP + 0x35a));
    let array = arena::alloc(m, count * 12);
    m.set_gw(0x370, array);
    let mut i = 0;
    while i < u32::from(m.lbu(GP + 0x35a)) {
        let offset = i * 12;
        let array = m.gw(0x370);
        let source = m.gw(0x368);
        let at = array.wrapping_add(offset);
        let source = source.wrapping_add(offset);
        m.sw(at, 1);
        let value = m.lhu(source.wrapping_add(2));
        m.sh(at.wrapping_add(4), value);
        let value = m.lbu(source.wrapping_add(4));
        m.sb(at.wrapping_add(6), value);
        let array = m.gw(0x370);
        m.sb(array.wrapping_add(offset).wrapping_add(7), 0);
        let array = m.gw(0x370);
        i += 1;
        m.sw(array.wrapping_add(offset).wrapping_add(8), 0);
    }
    let count = u32::from(m.lbu(GP + 0x36d));
    let array = arena::alloc(m, count * 4);
    let count = u32::from(m.lbu(GP + 0x36d));
    m.set_gw(0x394, array);
    let source = m.gw(0x388);
    for i in 0..count {
        let at = array.wrapping_add(i * 4);
        if m.lhu(source.wrapping_add(i * 0x14)) == 4 {
            m.sw(at, 0);
        } else {
            or_word(m, at, 1);
        }
    }
    let offset = m.lw(resources.wrapping_add(8));
    m.set_gw(0x3a0, resources.wrapping_add(offset));
    build_units_8004bedc(m);
    build_objects_8004c08c(m);
    for i in 0..0x40 {
        m.sw(ENTITY_AUX + i * 4, 0);
        m.sw(ENTITY_MOTION + i * 4, 0);
    }
    let gameplay = m.gw(GAMEPLAY);
    let table = m.lw(gameplay.wrapping_add(0x264));
    m.set_gw(0x398, table);
    m.set_gw(0x35c, table.wrapping_add(0x20));
    start_effects_80047e40(m);
}

/// 8004BEDC: allocate and fill the unit records (`[gp+0x374]`, 0x40 bytes
/// each) and the 0x1700-byte unit state array (`[gp+0x33C]`, 0xB8 each).
pub fn build_units_8004bedc(m: &mut Machine) {
    let count = u32::from(m.lbu(GP + 0x364));
    let records = arena::alloc(m, count << 6);
    m.set_gw(0x374, records);
    let states = arena::alloc(m, 0x1700);
    let count = m.lbu(GP + 0x364);
    m.set_gw(0x33c, states);
    if count == 0 {
        return;
    }
    let mut index = 0u32;
    loop {
        let records = m.gw(0x374);
        let table = m.gw(ENTITIES);
        let record = (index << 6).wrapping_add(records);
        let object = m.lw((index * 4).wrapping_add(table));
        let states = m.gw(0x33c);
        let units = m.gw(0x37c);
        let state = states.wrapping_add(index * 0xb8);
        let unit = units.wrapping_add(index * 0x2c);
        m.sw(record.wrapping_add(8), state);
        m.sw(record.wrapping_add(0xc), unit);
        m.sw(record.wrapping_add(4), object);
        let value = u32::from(m.lbu(unit.wrapping_add(0x20)));
        let base = m.gw(0x3a8);
        m.sw(record.wrapping_add(0x10), base.wrapping_add(value << 4));
        let value = u32::from(m.lbu(unit.wrapping_add(3)));
        let base = m.gw(0x3a0);
        m.sw(record.wrapping_add(0x14), base.wrapping_add(value * 40));
        let value = u32::from(m.lbu(unit.wrapping_add(0x21)));
        let base = m.gw(0x3c0);
        m.sw(record.wrapping_add(0x18), base.wrapping_add(value * 24));
        let value = m.lbu(unit.wrapping_add(0x22));
        if value == 0xff {
            m.sw(record.wrapping_add(0x1c), 0);
        } else {
            let base = m.gw(0x3b4);
            m.sw(
                record.wrapping_add(0x1c),
                base.wrapping_add(u32::from(value) * 12),
            );
        }
        let units = m.gw(0x37c);
        let unit = units.wrapping_add(index * 0x2c);
        if m.lbu(unit.wrapping_add(0x23)) == 0xff {
            let records = m.gw(0x374);
            m.sw((index << 6).wrapping_add(records).wrapping_add(0x20), 0);
        } else {
            let records = m.gw(0x374);
            let value = u32::from(m.lbu(unit.wrapping_add(0x23)));
            let base = m.gw(0x3bc);
            m.sw(
                (index << 6).wrapping_add(records).wrapping_add(0x20),
                base.wrapping_add(value << 3),
            );
        }
        let records = m.gw(0x374);
        let units = m.gw(0x37c);
        let record = (index << 6).wrapping_add(records);
        let unit = (index * 0x2c).wrapping_add(units);
        index += 1;
        m.sw(record.wrapping_add(0x24), 0);
        let value = m.lw(unit.wrapping_add(0x24));
        m.sw(record.wrapping_add(0x28), value);
        let next = m.lw(unit.wrapping_add(0x28));
        let kind = u32::from(m.lbu(record.wrapping_add(0x28)));
        m.sw(record.wrapping_add(0x3c), 0);
        m.sw(record.wrapping_add(0x38), 0);
        m.sw(record.wrapping_add(0x34), 0);
        m.sw(record.wrapping_add(0x2c), next);
        let base = m.gw(0x3c4);
        let count = u32::from(m.lbu(GP + 0x364));
        m.sw(record.wrapping_add(0x30), base.wrapping_add(kind << 5));
        if index >= count {
            break;
        }
    }
}

/// 8004C08C: allocate and fill the object records (`[gp+0x378]`, 40 bytes
/// each) from the object table `[gp+0x380]` (0x24-byte entries) and the
/// object entity pointers `[gp+0x2C8]`.
pub fn build_objects_8004c08c(m: &mut Machine) {
    let count = u32::from(m.lbu(GP + 0x36c));
    let records = arena::alloc(m, count * 40);
    let count = m.lbu(GP + 0x36c);
    m.set_gw(0x378, records);
    if count == 0 {
        return;
    }
    let mut record = records;
    let mut source = m.gw(0x380);
    let mut index = 0u32;
    loop {
        let objects = m.gw(0x2c8);
        let object = m.lw((index * 4).wrapping_add(objects));
        m.sw(record.wrapping_add(8), source);
        m.sw(record.wrapping_add(4), object);
        let value = u32::from(m.lbu(source.wrapping_add(0x20)));
        let base = m.gw(0x3a8);
        m.sw(record.wrapping_add(0xc), base.wrapping_add(value << 4));
        let value = m.lbu(source.wrapping_add(0x21));
        if value == 0xff {
            m.sw(record.wrapping_add(0x10), 0);
        } else {
            let base = m.gw(0x3a4);
            m.sw(
                record.wrapping_add(0x10),
                base.wrapping_add(u32::from(value) * 24),
            );
        }
        let value = u32::from(m.lbu(source.wrapping_add(0x22)));
        let base = m.gw(0x3ac);
        m.sw(record.wrapping_add(0x14), base.wrapping_add(value * 48));
        let value = m.lbu(source.wrapping_add(0x23));
        if value == 0xff {
            m.sw(record.wrapping_add(0x18), 0);
        } else {
            let base = m.gw(0x39c);
            m.sw(
                record.wrapping_add(0x18),
                base.wrapping_add(u32::from(value) << 4),
            );
        }
        m.sw(record.wrapping_add(0x24), 0);
        m.sw(record.wrapping_add(0x20), 0);
        m.sw(record.wrapping_add(0x1c), 0);
        record = record.wrapping_add(0x28);
        index += 1;
        source = source.wrapping_add(0x24);
        if index >= u32::from(m.lbu(GP + 0x36c)) {
            break;
        }
    }
}

// ---- effect sequences --------------------------------------------------------------------

/// 80047E40: start the 32 initial effect sequences: for each byte of
/// `[gp+0x398]`, walk the 8-byte steps at `[gp+0x35C] + byte*8` until a
/// zero halfword, calling 8005CD38(step+2, step+3, step+4). Returns 0.
pub fn start_effects_80047e40(m: &mut Machine) -> u32 {
    for i in 0..0x20 {
        let list = m.gw(0x398);
        let first = u32::from(m.lbu(list.wrapping_add(i)));
        let steps = m.gw(0x35c);
        let mut step = steps.wrapping_add(first << 3);
        if m.lh(step) != 0 {
            loop {
                let a = u32::from(m.lbu(step.wrapping_add(2)));
                let b = u32::from(m.lbu(step.wrapping_add(3)));
                let c = m.lh(step.wrapping_add(4)) as i32 as u32;
                set_effect_8005cd38(m, a, b, c);
                step = step.wrapping_add(8);
                if m.lh(step) == 0 {
                    break;
                }
            }
        }
    }
    0
}

/// 8005CD38: replace the low 9 bits of effect cell `(row, column)` in the
/// gameplay table `+0x248` (512-byte rows) with `value` (when nonzero) and,
/// with gameplay flag 2, queue the change through 8007D4B4. Returns the
/// previous low 9 bits.
pub fn set_effect_8005cd38(m: &mut Machine, column: u32, row: u32, value: u32) -> u32 {
    let gameplay = m.gw(GAMEPLAY);
    let table = m.lw(gameplay.wrapping_add(0x248));
    let cell = (row << 9).wrapping_add(column.wrapping_mul(2).wrapping_add(table));
    let previous = u32::from(m.lhu(cell));
    if value != 0 {
        m.sh(cell, (value | (previous & 0xfe00)) as u16);
        if m.lw(gameplay) & 2 != 0 {
            queue_effect_8007ceb4(m, column, row, value, previous & 0x1ff);
        }
    }
    previous & 0x1ff
}

/// 8007D4B4 → 8007CEB4: queue a two-word effect command
/// `{type 4 len 2, previous << 14, value << 23; column, row}`.
pub fn queue_effect_8007ceb4(m: &mut Machine, column: u32, row: u32, value: u32, previous: u32) {
    let command = shared::reserve_events(m, 2);
    let word = m.lw(command);
    m.sb(command.wrapping_add(4), column as u8);
    m.sb(command.wrapping_add(5), row as u8);
    let word = ((((word & !0xf) | 2) & 0xffff_ff8f) | 0x40) & 0x3fc2;
    m.sw(
        command,
        word | ((previous & 0x1ff) << 14) | value.wrapping_shl(23),
    );
}

// ---- script tables and mission state --------------------------------------------------------

/// 800527C8: resolve the mission script tables (resource +0xC block,
/// `[gp+0x334]`) into the 0x258-byte pointer block `[gp+0x328]`, and store
/// the script pointers listed at block +0x174 into unit/object records.
pub fn load_script_tables_800527c8(m: &mut Machine) {
    let resources = m.lw(RESOURCES);
    let offset = m.lw(resources.wrapping_add(0xc));
    m.set_gw(0x334, resources.wrapping_add(offset));
    let block = arena::alloc(m, 0x258);
    let source = m.gw(0x334);
    m.set_gw(0x328, block);
    let resolve = |m: &mut Machine, field: u32| {
        let offset = m.lh(source.wrapping_add(field)) as i32 as u32;
        source.wrapping_add(offset)
    };
    let value = resolve(m, 4);
    m.sw(block, value);
    let value = resolve(m, 6);
    m.sw(block.wrapping_add(4), value);
    for group in 0..4u32 {
        let half = group * 2;
        let slot = block.wrapping_add(group * 4);
        for (field, out) in [
            (8, 8),
            (0x10, 0x18),
            (0x18, 0x28),
            (0x20, 0x38),
            (0xa8, 0x148),
        ] {
            let value = resolve(m, field + half);
            m.sw(slot.wrapping_add(out), value);
        }
        for i in 0..16u32 {
            let entry = group * 32 + i * 2;
            let value = resolve(m, 0x28 + entry);
            m.sw(block.wrapping_add(group * 64 + i * 4 + 0x48), value);
            let value = m.lhu(source.wrapping_add(entry).wrapping_add(0xb0));
            m.sh(block.wrapping_add(entry).wrapping_add(0x158), value);
        }
    }
    for (count, pairs) in [
        (8u32, [(0x130u32, 0x1d8u32), (0x140, 0x1f8)]),
        (4, [(0x150, 0x218), (0x158, 0x228)]),
        (4, [(0x160, 0x238), (0x168, 0x248)]),
    ] {
        let table = m.gw(0x334);
        let out = m.gw(0x328);
        for i in 0..count {
            for (field, slot) in pairs {
                let offset = m.lh(table.wrapping_add(field + i * 2)) as i32 as u32;
                m.sw(out.wrapping_add(slot + i * 4), source.wrapping_add(offset));
            }
        }
    }
    let table = m.gw(0x334);
    if (m.lw(table.wrapping_add(0x170)) as i32) <= 0 {
        return;
    }
    let units = m.gw(0x374);
    let objects = m.gw(0x378);
    let mut entry = table.wrapping_add(0x174);
    let mut i = 0u32;
    loop {
        let offset = m.lh(entry.wrapping_add(2)) as i32 as u32;
        let kind = m.lbu(entry.wrapping_add(1));
        let target = u32::from(m.lbu(entry));
        let script = source.wrapping_add(offset);
        let field = match kind {
            1 => Some(0),
            2 => Some(4),
            4 => Some(8),
            _ => None,
        };
        if let Some(field) = field {
            let at = if target < 0x20 {
                units.wrapping_add(target << 6).wrapping_add(0x34 + field)
            } else {
                objects
                    .wrapping_add(target * 40)
                    .wrapping_sub(0x4e4)
                    .wrapping_add(field)
            };
            m.sw(at, script);
        }
        let table = m.gw(0x334);
        i += 1;
        if (i as i32) >= (m.lw(table.wrapping_add(0x170)) as i32) {
            break;
        }
        entry = entry.wrapping_add(4);
    }
}

/// 80052574: allocate and clear the mission state block (`[gp+0x330]`,
/// 0x40 bytes; +0x2B = 0xFF, +0x2C = stream of `[gp+0x4C0]`) and run the
/// mission's initialization script `[[gp+0x328]]`.
pub fn start_mission_state_80052574(m: &mut Machine) {
    const SIZE: u32 = 0x18;
    m.push_frame(SIZE);
    let resources = m.lw(RESOURCES);
    let offset = m.lw(resources.wrapping_add(0x10));
    m.set_gw(0x32c, resources.wrapping_add(offset));
    let state = arena::alloc(m, 0x40);
    m.set_gw(0x330, state);
    m.sb(state.wrapping_add(0xa), 0);
    m.sw(state, 0);
    m.sw(state.wrapping_add(4), 0);
    m.sh(state.wrapping_add(8), 0);
    m.sb(state.wrapping_add(0xb), 0);
    m.sb(state.wrapping_add(0x1c), 0);
    m.sw(state.wrapping_add(0xc), 0);
    m.sw(state.wrapping_add(0x10), 0);
    m.sw(state.wrapping_add(0x14), 0);
    m.sb(state.wrapping_add(0x1d), 0);
    m.sh(state.wrapping_add(0x18), 0);
    m.sh(state.wrapping_add(0x1a), 0);
    for offset in [
        0x1e, 0x1f, 0x20, 0x21, 0x22, 0x23, 0x3d, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a,
    ] {
        let state = m.gw(0x330);
        m.sb(state.wrapping_add(offset), 0);
    }
    let state = m.gw(0x330);
    m.sb(state.wrapping_add(0x2b), 0xff);
    let index = u32::from(m.lbu(GP + 0x4c0));
    let stream = stream_of_800550d8(m, index);
    let state = m.gw(0x330);
    m.sb(state.wrapping_add(0x2c), stream as u8);
    for offset in [0x2d, 0x2e, 0x2f, 0x30, 0x31, 0x34] {
        let state = m.gw(0x330);
        m.sb(state.wrapping_add(offset), 0);
    }
    let state = m.gw(0x330);
    m.sh(state.wrapping_add(0x32), 0);
    for offset in [0x35, 0x36, 0x37, 0x3c] {
        let state = m.gw(0x330);
        m.sb(state.wrapping_add(offset), 0);
    }
    let state = m.gw(0x330);
    m.sh(state.wrapping_add(0x38), 0);
    m.sh(state.wrapping_add(0x3a), 0);
    m.sb(state.wrapping_add(0x3e), 0);
    let state = m.gw(0x330);
    m.sb(state.wrapping_add(0x3f), 0);
    let scripts = m.gw(0x328);
    let script = m.lw(scripts);
    script::interpret(m, script);
    m.pop_frame(SIZE);
}

/// 800550D8: byte `0x800B0540[index]`.
pub fn stream_of_800550d8(m: &mut Machine, index: u32) -> u32 {
    u32::from(m.lbu(0x800b_0540u32.wrapping_add(index)))
}

// ---- resets ------------------------------------------------------------------------------

/// 8002C7A4: reset the HUD state bytes `gp+0x30A..0x312` and the 0x800C35A8
/// block (`[gp+0x304]`). Returns 0.
pub fn reset_hud_8002c7a4(m: &mut Machine) -> u32 {
    for offset in [0x312, 0x311, 0x310, 0x30b, 0x30a] {
        m.sb(GP + offset, 0);
    }
    m.set_gw(0x304, 0x800c_35a8);
    let mut block = 0;
    for i in 0..3 {
        let at = m.gw(0x304);
        m.sb(at.wrapping_add(i).wrapping_add(4), 0);
        let at = m.gw(0x304);
        m.sb(at.wrapping_add(i).wrapping_add(7), 0);
        block = m.gw(0x304);
        m.sh(block.wrapping_add(i * 2).wrapping_add(0xa), 0);
    }
    m.sh(block.wrapping_add(0x1e), 0x4a16);
    m.sh(block.wrapping_add(0x1c), 0x3a);
    0
}

/// 80054B54: resolve the message tables for the current language
/// (`gp+0x4A0/0x4B0/0x4B4`), clear the message state words and allocate
/// the four-entry message queue `[gp+0x4A8]`.
pub fn reset_message_queue_80054b54(m: &mut Machine) {
    let resources = m.lw(RESOURCES);
    let context = m.gw(CONTEXT);
    let offset = m.lw(resources.wrapping_add(0x14));
    let language = m.lw(context.wrapping_add(0x38c));
    let tables = resources.wrapping_add(offset);
    let offset = m.lw(language.wrapping_mul(4).wrapping_add(tables));
    let block = tables.wrapping_add(offset);
    let first = m.lw(block.wrapping_add(4));
    let block = block.wrapping_add(4);
    m.set_gw(0x49c, u32::MAX);
    let second = m.lw(block.wrapping_add(4));
    m.set_gw(0x4b8, 0);
    m.set_gw(0x4ac, 0);
    m.set_gw(0x4c4, 0);
    m.set_gw(0x4a0, block.wrapping_add(first));
    let third = m.lw(block.wrapping_add(8));
    m.set_gw(0x4b0, block.wrapping_add(second));
    m.set_gw(0x4b4, block.wrapping_add(third));
    let queue = arena::alloc(m, 0x18);
    m.set_gw(0x4a8, queue);
    m.sw(queue, 0xff);
    for i in 0..4 {
        let queue = m.gw(0x4a8);
        m.sb(queue.wrapping_add(i).wrapping_add(0x14), 0xff);
        let queue = m.gw(0x4a8);
        let at = queue.wrapping_add(i * 2);
        m.sh(at.wrapping_add(0xc), 0);
        m.sh(at.wrapping_add(4), 0);
    }
    m.set_gw(0x4bc, 0);
}

/// 8005AC20: reset the 0x800C5790 block.
pub fn reset_8005ac20(m: &mut Machine) {
    const BLOCK: u32 = 0x800c_5790;
    m.sh(BLOCK + 4, 0x3a);
    m.sh(BLOCK + 6, 0x1e16);
    m.sh(BLOCK, 0);
    m.sh(BLOCK + 2, 0);
    m.sb(BLOCK + 8, 0x83);
    m.sb(BLOCK + 9, 0x83);
    m.sb(BLOCK + 0xa, 0x83);
    m.sw(BLOCK + 0xc, 0);
}

/// 800477C8: reset the context result block (+0x3BC) for the mission;
/// missions 0x36/0x3A/0x3B restore the saved score (`gp+0x464..0x468`),
/// others record the player's map cell from entity 0's position.
pub fn reset_result_800477c8(m: &mut Machine) {
    let context = m.gw(CONTEXT);
    let block = context.wrapping_add(0x3bc);
    m.sb(block, 0);
    m.sb(block.wrapping_add(1), 0);
    let context = m.gw(CONTEXT);
    let value = m.lbu(context.wrapping_add(0x390));
    m.sb(block.wrapping_add(4), value);
    let context = m.gw(CONTEXT);
    let value = m.lbu(context.wrapping_add(0x390));
    m.sb(block.wrapping_add(0xb), 4);
    m.sb(block.wrapping_add(5), value);
    let context = m.gw(CONTEXT);
    m.sb(context.wrapping_add(0x95a), 0);
    m.sb(block.wrapping_add(0xa), 0);
    let id = m.lbu(MISSION_ID);
    m.sh(block.wrapping_add(2), 0);
    m.sh(block.wrapping_add(8), 0);
    if matches!(id, 0x3a | 0x3b | 0x36) {
        let value = m.lhu(GP + 0x464);
        let state = m.gw(script::MISSION);
        m.sh(block.wrapping_add(2), value);
        let value = m.lhu(GP + 0x464);
        m.sw(state.wrapping_add(0xc), u32::from(value));
        let value = m.lhu(GP + 0x466);
        m.sh(block.wrapping_add(8), value);
        let value = m.lbu(GP + 0x468);
        m.sb(block.wrapping_add(0xa), value);
        return;
    }
    let player = entity(m, 0);
    for (i, field) in [8u32, 0x10].into_iter().enumerate() {
        let position = m.lw(player.wrapping_add(field)) as i32;
        let cell = ((position >> 12).wrapping_add(0x4_0000) >> 11).clamp(0, 0xff);
        let index = m.lh(block.wrapping_add(8)) as i32 as u32;
        m.sb(
            block
                .wrapping_add(index.wrapping_mul(2))
                .wrapping_add(0xc + i as u32),
            cell as u8,
        );
    }
}

/// 800453B8: clear the 64 link slots (byte 0 = 0, links 0xFF) and mark slot 0.
pub fn reset_links_800453b8(m: &mut Machine) {
    for i in 0..0x40 {
        let at = shared::LOCK_LINKS + i * 4;
        m.sb(at, 0);
        m.sb(at + 3, 0xff);
        m.sb(at + 2, 0xff);
        m.sb(at + 1, 0xff);
    }
    m.sb(shared::LOCK_LINKS, 0xff);
}

/// 800387E0: bind the gameplay draw object to the player (entity 0).
pub fn bind_player_object_800387e0(m: &mut Machine, object: u32) {
    let value = m.lw(object);
    let table = m.gw(ENTITIES);
    m.sw(object, value | 0x100);
    let player = m.lw(table);
    for offset in [0x38, 0x3c, 0x40] {
        m.sw(object.wrapping_add(offset), 0);
    }
    for offset in [0x48, 0x4a, 0x4c] {
        m.sh(object.wrapping_add(offset), 0);
    }
    m.sw(object.wrapping_add(0x34), player.wrapping_add(8));
}

// ---- entities --------------------------------------------------------------------------

/// 80038758: build units (8004C258), then place the player (entity 0) at
/// {0, 0xFFB50000, 0} and set entity 1's word to 1.
pub fn initialize_entities_80038758(m: &mut Machine) {
    initialize_units_8004c258(m);
    let player = entity(m, 0);
    m.sb(player.wrapping_add(4), 0);
    let table = m.gw(ENTITIES);
    m.sw(player, 0x1_c240);
    m.sw(player.wrapping_add(8), 0);
    m.sw(player.wrapping_add(0xc), 0xffb5_0000);
    m.sw(player.wrapping_add(0x10), 0);
    for offset in [0x18, 0x1a, 0x1c] {
        m.sh(player.wrapping_add(offset), 0);
    }
    let second = m.lw(table.wrapping_add(4));
    m.sw(second, 1);
}

/// 8004C20C: reset motion record `index` (0x28 bytes). Returns 0.
pub fn reset_motion_8004c20c(m: &mut Machine, index: u32) -> u32 {
    let record = m.lw(ENTITY_MOTION.wrapping_add(index.wrapping_mul(4)));
    m.sw(record, 0);
    m.sw(record.wrapping_add(4), 0x10_0000);
    m.sw(record.wrapping_add(0x10), 0);
    m.sw(record.wrapping_add(0xc), 0);
    m.sw(record.wrapping_add(8), 0);
    for offset in [0x18, 0x1a, 0x1c, 0x1e] {
        m.sh(record.wrapping_add(offset), 0);
    }
    m.sw(record.wrapping_add(0x20), 0);
    m.sw(record.wrapping_add(0x24), 0);
    0
}

/// 8004C258: initialize entities 2..31 (word = index) and units 2..count:
/// flags, aux (12 bytes) and motion (0x28 bytes) records, placement,
/// distance to the player (800827B4) and the unit state (0xB8 bytes).
pub fn initialize_units_8004c258(m: &mut Machine) {
    for index in 2..0x20 {
        let object = entity(m, index);
        m.sw(object, index);
        m.sw(ENTITY_FLAGS + index * 4, 0);
    }
    let mut index = 2u32;
    if index >= u32::from(m.lbu(GP + 0x364)) {
        return;
    }
    let mut state_offset = 0x170u32;
    loop {
        let object = entity(m, index);
        let records = m.gw(0x374);
        let record = (index << 6).wrapping_add(records);
        let placement = m.lw(record.wrapping_add(0xc));
        let kind = m.lw(record.wrapping_add(0x10));
        let value = m.lw(object);
        let behaviour = m.lw(record.wrapping_add(0x30));
        m.sw(object, value | 0x4000);
        let aux = arena::alloc(m, 0xc);
        m.sw(ENTITY_AUX + index * 4, aux);
        let motion = arena::alloc(m, 0x28);
        m.sw(ENTITY_MOTION + index * 4, motion);
        reset_motion_8004c20c(m, index);
        match m.lbu(placement) {
            0 => or_word(m, object, 0x40),
            1 => or_word(m, object, 0x80),
            2 => or_word(m, object, 0x100),
            _ => {}
        }
        let value = m.lbu(placement.wrapping_add(2));
        m.sb(object.wrapping_add(4), value);
        let value = m.lhu(kind);
        m.sh(object.wrapping_add(0x4e), value);
        let value = m.lbu(placement.wrapping_add(1));
        m.sh(object.wrapping_add(0x48), u16::from(value));
        for i in 0..4 {
            let word = m.lw(placement.wrapping_add(4 + i * 4));
            m.sw(object.wrapping_add(8 + i * 4), word);
        }
        // lwl/lwr pairs: two possibly unaligned words +0x14/+0x18 → +0x18/+0x1C.
        let words: [u32; 2] = std::array::from_fn(|i| {
            let bytes: [u8; 4] = std::array::from_fn(|b| {
                m.lbu(placement.wrapping_add(0x14 + i as u32 * 4 + b as u32))
            });
            u32::from_le_bytes(bytes)
        });
        for (i, word) in words.into_iter().enumerate() {
            for (b, byte) in word.to_le_bytes().into_iter().enumerate() {
                m.sb(object.wrapping_add(0x18 + i as u32 * 4 + b as u32), byte);
            }
        }
        let player = entity(m, 0);
        let distance =
            shared::f_800827b4(m, [object.wrapping_add(8), player.wrapping_add(8), 0, 0]);
        let motion = m.lw(ENTITY_MOTION + index * 4);
        m.sw(motion.wrapping_add(4), distance);
        let records = m.gw(0x374);
        m.sw(object.wrapping_add(0x20), 0);
        m.sw(object.wrapping_add(0x24), 0);
        let record = (index << 6).wrapping_add(records);
        let pattern = m.lw(record.wrapping_add(0x14));
        let speed = m.lw(pattern.wrapping_add(4));
        let states = m.gw(0x33c);
        for offset in [0x30, 0x32, 0x34] {
            m.sh(object.wrapping_add(offset), 0);
        }
        for offset in [0x38, 0x3c, 0x40] {
            m.sw(object.wrapping_add(offset), 0);
        }
        m.sw(object.wrapping_add(0x54), 0x8020_0000);
        let state = states.wrapping_add(state_offset);
        m.sw(object.wrapping_add(0x28), speed);
        m.sw(state, 0);
        let value = m.lbu(behaviour);
        m.sb(state.wrapping_add(0x1d), 0);
        m.sb(state.wrapping_add(0x1e), 0);
        m.sw(state.wrapping_add(0x38), 0);
        m.sb(state.wrapping_add(0x1c), value);
        let value = m.lbu(behaviour.wrapping_add(1));
        m.sb(state.wrapping_add(0x1f), value);
        for i in 0..4 {
            let word = m.lw(behaviour.wrapping_add(8 + i * 4));
            m.sw(state.wrapping_add(0x20 + i * 4), word);
        }
        let value = m.lw(behaviour.wrapping_add(0x18));
        m.sw(state.wrapping_add(0x30), value);
        let value = m.lw(behaviour.wrapping_add(0x1c));
        index += 1;
        m.sb(state.wrapping_add(0x3c), 0xff);
        m.sb(state.wrapping_add(0x3d), 0xff);
        m.sb(state.wrapping_add(0x3e), 0);
        m.sw(state.wrapping_add(0x34), value);
        m.sh(state.wrapping_add(0x40), 0);
        m.sb(state.wrapping_add(0x62), 0xff);
        m.sb(state.wrapping_add(0x63), 0xff);
        m.sb(state.wrapping_add(0x64), 0);
        m.sb(state.wrapping_add(0x3f), 0xff);
        m.sb(state.wrapping_add(0x60), 0xff);
        m.sb(state.wrapping_add(0x61), 0);
        let count = u32::from(m.lbu(GP + 0x364));
        state_offset += 0xb8;
        for offset in [0x66, 0xb6, 4, 6, 8] {
            m.sh(state.wrapping_add(offset), 0);
        }
        for offset in [0xc, 0x10, 0x14] {
            m.sw(state.wrapping_add(offset), 0);
        }
        for offset in [0x80, 0x82, 0x84] {
            m.sh(state.wrapping_add(offset), 0);
        }
        if index >= count {
            break;
        }
    }
}

/// 8004CDB0 (via 800387C0): initialize the 32 object entities (`[gp+0x2C8]`,
/// word = index + 0x20) and each mission object's flags, aux/motion
/// records, placement and distance to the player.
pub fn initialize_objects_8004cdb0(m: &mut Machine) {
    for i in 0..0x20u32 {
        let objects = m.gw(0x2c8);
        let object = m.lw(objects.wrapping_add(i * 4));
        m.sw(object, i + 0x20);
        m.sw(ENTITY_FLAGS + 0x80 + i * 4, 0);
    }
    if m.lbu(GP + 0x36c) == 0 {
        return;
    }
    let mut index = 0u32;
    loop {
        let entity_index = index + 0x20;
        let objects = m.gw(0x2c8);
        let object = m.lw(objects.wrapping_add(index * 4));
        let records = m.gw(0x378);
        let record = records.wrapping_add(index * 40);
        let source = m.lw(record.wrapping_add(8));
        let value = m.lw(object);
        let kind = m.lw(record.wrapping_add(0xc));
        m.sw(object, value | 0x4000);
        let aux = arena::alloc(m, 0xc);
        m.sw(ENTITY_AUX + entity_index * 4, aux);
        let motion = arena::alloc(m, 0x28);
        m.sw(ENTITY_MOTION + entity_index * 4, motion);
        reset_motion_8004c20c(m, entity_index);
        match m.lbu(source) {
            0 => or_word(m, object, 0x40),
            1 => or_word(m, object, 0x80),
            2 => or_word(m, object, 0x100),
            _ => {}
        }
        m.sw(object.wrapping_add(0x54), 0);
        m.sb(object.wrapping_add(5), 0);
        for i in 0..6 {
            let word = m.lw(source.wrapping_add(4 + i * 4));
            m.sw(object.wrapping_add(8 + i * 4), word);
        }
        let value = m.lbu(source.wrapping_add(2));
        m.sb(
            object.wrapping_add(4),
            if value == 0xff { 0 } else { value },
        );
        if m.lbu(source.wrapping_add(0x1f)) == 1 {
            let value = m.lbu(object.wrapping_add(5));
            m.sb(object.wrapping_add(5), value | 2);
        }
        m.sh(object.wrapping_add(6), 0);
        let value = m.lbu(source.wrapping_add(1));
        m.sh(object.wrapping_add(0x48), u16::from(value));
        let value = m.lhu(kind);
        for offset in [0x20, 0x24, 0x28] {
            m.sw(object.wrapping_add(offset), 0);
        }
        for offset in [0x30, 0x32, 0x34] {
            m.sh(object.wrapping_add(offset), 0);
        }
        for offset in [0x38, 0x3c, 0x40] {
            m.sw(object.wrapping_add(offset), 0);
        }
        m.sw(object.wrapping_add(0x54), 0x8000_0000);
        m.sh(object.wrapping_add(0x4e), value);
        let player = entity(m, 0);
        let distance =
            shared::f_800827b4(m, [object.wrapping_add(8), player.wrapping_add(8), 0, 0]);
        let motion = m.lw(ENTITY_MOTION + entity_index * 4);
        index += 1;
        m.sw(motion.wrapping_add(4), distance);
        m.sb(object.wrapping_add(0x58), 0);
        m.sb(object.wrapping_add(0x59), 0);
        m.sb(object.wrapping_add(0x5a), 0);
        m.sb(object.wrapping_add(0x5b), 0xff);
        let count = u32::from(m.lbu(GP + 0x36c));
        for offset in [0x5c, 0x60, 0x64] {
            m.sw(object.wrapping_add(offset), 0);
        }
        m.sw(object.wrapping_add(0x68), 1);
        if index >= count {
            break;
        }
    }
}

/// Every routine translated here, by original address (`a0..a3` in, `v0`
/// out; routines without a result return 0).
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let [a0, a1, a2, a3] = args;
    Some(match target {
        0x8003_8818 => {
            initialize_38818(m);
            0
        }
        0x8004_bbb8 => {
            load_mission_tables_8004bbb8(m);
            0
        }
        0x8004_bedc => {
            build_units_8004bedc(m);
            0
        }
        0x8004_c08c => {
            build_objects_8004c08c(m);
            0
        }
        0x8004_7e40 => start_effects_80047e40(m),
        0x8005_cd38 => set_effect_8005cd38(m, a0, a1, a2),
        0x8007_d4b4 | 0x8007_ceb4 => {
            queue_effect_8007ceb4(m, a0, a1, a2, a3);
            0
        }
        0x8005_27c8 => {
            load_script_tables_800527c8(m);
            0
        }
        0x8005_2574 => {
            start_mission_state_80052574(m);
            0
        }
        0x8005_50d8 => stream_of_800550d8(m, a0),
        0x8002_c7a4 => reset_hud_8002c7a4(m),
        0x8005_4b54 => {
            reset_message_queue_80054b54(m);
            0
        }
        0x8005_ac20 => {
            reset_8005ac20(m);
            0
        }
        0x8005_9c40 => {
            m.sb(GP + 0x4f0, 0);
            0
        }
        0x8004_77c8 => {
            reset_result_800477c8(m);
            0
        }
        0x8004_53b8 => {
            reset_links_800453b8(m);
            0
        }
        0x8003_87e0 => {
            bind_player_object_800387e0(m, a0);
            0
        }
        0x8003_8758 => {
            initialize_entities_80038758(m);
            0
        }
        0x8004_c20c => reset_motion_8004c20c(m, a0),
        0x8004_c258 => {
            initialize_units_8004c258(m);
            0
        }
        0x8003_87c0 | 0x8004_cdb0 => {
            initialize_objects_8004cdb0(m);
            0
        }
        _ => return None,
    })
}
