//! Overlay 2 mission loader (inner state 14: 800D3254 / 800D328C) and its
//! leaves, translated from the original instructions.
//!
//! Control halfwords (phase, substate) are at `[0x800F5E74]`; the loader
//! block `[0x800F1E4C]` holds +0 the mission resource's child table, +4
//! the common resource's, +8 the visual page, +0xC the fade timer, +0x10
//! total bytes, +0x14 loaded bytes, +0x18 the current read destination,
//! +0x1C the mission resource size. Resource reads go through the
//! resident queue (8002C214 size, 8002C404 read, 8002C604 progress,
//! 8002C280 cancel; resource id at `[gp+0x2CC]+0x60`), texture lists
//! through the queue `[0x800F1E48]` polled by [`poll_textures`].
//!
//! | Phase | Substates (callbacks of table 0x800F39F4) |
//! | ---: | --- |
//! | 0 | `800CED30` bind built-in scripts, `800CED5C` select page |
//! | 1 | `800CEDA8` resource sizes |
//! | 2 | `800CEE3C` read mission resource, `800CEEF0` wait, `800CEF34` bind, `800CF014` textures |
//! | 3 | `800CF064` read common resource 0x48, `800CF100` wait, `800CF144` bind, `800CF25C` textures + sound bank, `800CF2F4` fade |
//! | 4 | `800CF350` done |
use super::{LOADER, call, context, framed, scene, sext16, visual};
use crate::{
    arena,
    common::RESOURCES,
    machine::{Machine, SmallData},
};

/// `[0x800F1E4C]`: loader block.
pub const BLOCK: u32 = 0x800f_1e4c;
/// `[0x800F1E48]`: texture queue (+0 entries, +4 count, +8 arena mark,
/// +0x10 batch size, +0x14 cursor).
pub const TEXTURES: u32 = 0x800f_1e48;
/// `[0x800F1E44]`: common table block filled by [`bind_common_tables`].
pub const COMMON_TABLES: u32 = 0x800f_1e44;
/// Phase rows of loader callbacks.
pub const CALLBACKS: u32 = 0x800f_39f4;
/// Mission records (12 bytes: +0 mission id byte, +4 resource).
pub const MISSIONS: u32 = 0x800f_5958;
/// Aircraft loadouts (16 bytes: +0 aircraft, +4/+5 counts, +8/+0xC ids).
pub const LOADOUTS: u32 = 0x800f_5790;
/// Built-in visual scripts bound in phase 0.
pub const BUILTIN_SCRIPTS: u32 = 0x800f_1e50;
/// CD read completion routine passed to 8002C404 (resident 80099DD4).
pub const READ_COMPLETE: u32 = 0x8009_9dd4;
/// Resource id of the common resource.
pub const COMMON_RESOURCE: u32 = 0x48;

fn block(m: &mut Machine) -> u32 {
    m.lw(BLOCK)
}
fn resources(m: &mut Machine) -> u32 {
    m.gw(RESOURCES)
}

// ---- 800D3254 / 800D328C: callbacks ---------------------------------------------------------

/// 800D3254 (inner state 14 init): control = (0, 0), page 3 (800CEB80),
/// sound 0x56; returns 1.
pub fn initialize(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let control = m.lw(LOADER);
        m.sh(control + 2, 0);
        m.sh(control, 0);
        set_page(m, 3);
        call(m, 0x8002_d724, [0x56, 0, 0, 0]);
        1
    })
}

/// 800D328C (inner state 14 update): progress bar (TILE at (192,420),
/// 115 * loaded / total wide, into scene ordering-table entry 0), seven
/// progress styles (slots 1..6 lit below 7 * (loaded - 1) / total, slot 7
/// that count), then the phase callback; its flags 1/2/4 advance the
/// substate / phase or finish: mission 1 unlocks the first aircraft
/// (context +0x979/+0x9B9 and 800DC074(1)), the common scripts are bound
/// (800CEB90) and 0x100 is returned.
pub fn update(m: &mut Machine) -> u32 {
    framed(m, 0x30, |m, sp| {
        let ctx = context(m);
        let flags = ctx + 0x960;
        progress(m, sp + 0x10, sp + 0x14);
        let total = m.lw(sp + 0x10) as i32;
        let width = if total != 0 {
            let loaded = m.lw(sp + 0x14) as i32;
            visual::quotient(m, loaded.wrapping_mul(115), total, 0x800d_32f8) as u32
        } else {
            0
        };
        let base = scene(m).wrapping_add(0x20000);
        let tile = m.lw(base + 0x94);
        m.sw(base + 0x94, tile.wrapping_add(0x10));
        m.sb(tile + 3, 3);
        m.sb(tile + 7, 0x60);
        m.sb(tile + 4, 0xe5);
        m.sb(tile + 5, 0xe5);
        m.sb(tile + 6, 0xd5);
        let block = scene(m);
        m.sh(tile + 8, 0xc0);
        m.sh(tile + 0xa, 0x1a4);
        m.sh(tile + 0xc, width as u16);
        m.sh(tile + 0xe, 6);
        let ot = m.lw(block + 0x90);
        let link = m.lw(ot);
        super::link_word(m, tile, link);
        let ot = m.lw(block + 0x90);
        super::link_word(m, ot, tile);
        let total = m.lw(sp + 0x10) as i32;
        let lit = if total == 0 {
            0
        } else {
            let loaded = m.lw(sp + 0x14) as i32;
            visual::quotient(
                m,
                loaded.wrapping_sub(1).wrapping_mul(7),
                total,
                0x800d_33e8,
            )
        };
        for slot in 0..6i32 {
            visual::set_style(m, (slot + 1) as u32, u32::from(slot < lit));
        }
        visual::set_style(m, 7, lit as u32);
        let control = m.lw(LOADER);
        let phase = sext16(m.lh(control));
        let row = m.lw(CALLBACKS.wrapping_add(phase << 2));
        let substate = sext16(m.lh(control + 2));
        let callback = m.lw(row.wrapping_add(substate << 2));
        // The callbacks read no argument registers.
        let result = call(m, callback, [control, 0, 0, 0]);
        if result & 1 != 0 {
            let control = m.lw(LOADER);
            let substate = m.lhu(control + 2);
            m.sh(control + 2, substate.wrapping_add(1));
            0
        } else if result & 2 != 0 {
            let control = m.lw(LOADER);
            let phase = m.lhu(control);
            m.sh(control + 2, 0);
            m.sh(control, phase.wrapping_add(1));
            0
        } else if result & 4 != 0 {
            let ctx = context(m);
            if m.lh(ctx + 0x390) == 1 {
                m.sb(flags + 0x59, 1);
                m.sb(flags + 0x19, 1);
                unlock_loadout(m, 1);
            }
            bind_common_scripts(m);
            0x100
        } else {
            0
        }
    })
}

/// 800CEB80: visual page of the loader (+8). Returns the loader block.
pub fn set_page(m: &mut Machine, page: u32) -> u32 {
    let block = block(m);
    m.sw(block + 8, page);
    block
}
/// 800CEB90: bind the common resource's scripts (child table +0xC) and
/// select page 1 (whose v0, the visual block, it returns).
pub fn bind_common_scripts(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let block = block(m);
        let common = m.lw(block + 4);
        let scripts = m.lw(common + 0xc);
        visual::bind_scripts(m, scripts);
        visual::select_page(m, 1)
    })
}
/// 800CEBCC: `*total` = +0x10, `*loaded` = +0x14 clamped to the total
/// (signed). Returns 1 when it clamped, else 0.
pub fn progress(m: &mut Machine, total: u32, loaded: u32) -> u32 {
    let block = block(m);
    let value = m.lw(block + 0x10);
    m.sw(total, value);
    let block = self::block(m);
    let done = m.lw(block + 0x14);
    m.sw(loaded, done);
    let value = m.lw(total);
    let clamped = (value as i32) < (done as i32);
    if clamped {
        m.sw(loaded, value);
    }
    u32::from(clamped)
}

// ---- phase callbacks -----------------------------------------------------------------------

/// 800CED30 (0, 0): bind the built-in scripts, page 1; returns 1.
pub fn bind_builtin(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        visual::bind_scripts(m, BUILTIN_SCRIPTS);
        visual::select_page(m, 1);
        1
    })
}
/// 800CED5C (0, 1): select the loader page, record 1 time 0, fade 0;
/// returns 2.
pub fn select_loader_page(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let loader = block(m);
        let page = m.lw(loader + 8);
        visual::select_page(m, page);
        visual::set_parameter(m, 1, 0);
        let block = block(m);
        m.sw(block + 0xc, 0);
        2
    })
}
/// 800CEDA8 (1, 0): loaded 0; total = sizes of the mission resource
/// (800D1890) and the common resource; returns 2.
pub fn measure(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        let block = block(m);
        let ctx = context(m);
        m.sw(block + 0x14, 0);
        let mission = sext16(m.lh(ctx + 0x390));
        let resource = mission_resource(m, mission);
        let res = resources(m);
        m.sw(res + 0x60, resource);
        let size = call(m, 0x8002_c214, [res + 0x60, 1, 0, 0]);
        let res = resources(m);
        let block = self::block(m);
        m.sw(block + 0x10, size);
        m.sw(res + 0x60, COMMON_RESOURCE);
        let size = call(m, 0x8002_c214, [res + 0x60, 1, 0, 0]);
        let block = self::block(m);
        let total = m.lw(block + 0x10);
        m.sw(block + 0x10, total.wrapping_add(size));
        2
    })
}
/// 800CEE3C (2, 0): cancel queued reads, size and allocate the mission
/// resource and start its read (8002C404 with completion 80099DD4,
/// context +0x44 = 1 pending); returns 1.
pub fn read_mission(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        call(m, 0x8002_c280, [0, 0, 0, 0]);
        let ctx = context(m);
        let mission = sext16(m.lh(ctx + 0x390));
        let resource = mission_resource(m, mission);
        let res = resources(m);
        m.sw(res + 0x60, resource);
        let size = call(m, 0x8002_c214, [res + 0x60, 1, 0, 0]);
        let block = block(m);
        m.sw(block + 0x1c, size);
        let destination = arena::alloc(m, size);
        let res = resources(m);
        let ctx = context(m);
        m.sw(ctx + 0x44, 1);
        call(m, 0x8002_c404, [res + 0x60, 1, destination, READ_COMPLETE]);
        let block = self::block(m);
        m.sw(block + 0x18, destination);
        1
    })
}
/// 800CEEF0 (2, 1) and 800CF100 (3, 1): loaded = transfer position
/// (8002C604) - destination; returns 1 once the read completed (context
/// +0x44 == 0).
pub fn wait_read(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, sp| {
        call(m, 0x8002_c604, [sp + 0x10, 0, 0, 0]);
        let block = block(m);
        let position = m.lw(sp + 0x10);
        let destination = m.lw(block + 0x18);
        let ctx = context(m);
        m.sw(block + 0x14, position.wrapping_sub(destination));
        u32::from(m.lw(ctx + 0x44) == 0)
    })
}
/// 800CEF34 (2, 2): bind the mission resource (80026E98): scene +0x20098
/// child 0, +0x2009C child 1 (script), +0x200B8 child 2 (text), +0x200BC
/// child 3 relocated when non-empty; queue child 4's textures in batches of
/// 10; returns 1.
pub fn bind_mission(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        let res = resources(m);
        let resource = m.lw(res + 0x60);
        let table = call(m, 0x8002_6e98, [resource, 0, 0, 0]);
        let block = block(m);
        let base = scene(m).wrapping_add(0x20000);
        m.sw(block, table);
        let child = m.lw(table);
        m.sw(base + 0x98, child);
        let table = m.lw(block);
        let child = m.lw(table + 4);
        m.sw(base + 0x9c, child);
        let table = m.lw(block);
        let child = m.lw(table + 8);
        m.sw(base + 0xb8, child);
        let table = m.lw(block);
        let labels = m.lw(table + 0xc);
        if m.lw(labels) != 0 {
            let relocated = call(m, 0x8002_6e98, [labels, 0, 0, 0]);
            let base = scene(m).wrapping_add(0x20000);
            m.sw(base + 0xbc, relocated);
        } else {
            m.sw(base + 0xbc, 0);
        }
        let textures = m.lw(table + 0x10);
        queue_textures(m, textures, 0x1_0000, 10);
        1
    })
}
/// 800CF014 (2, 3): poll the texture queue; when done release the mission
/// resource's child 4 (arena reset to it) and return 2, else 0.
pub fn finish_mission_textures(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        if poll_textures(m) != 1 {
            return 0;
        }
        let block = block(m);
        let table = m.lw(block);
        let child = m.lw(table + 0x10);
        arena::reset(m, child);
        2
    })
}
/// 800CF064 (3, 0): cancel queued reads, read the common resource (0x48)
/// after the mission data; destination - loaded at +0x18; returns 1.
pub fn read_common(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        call(m, 0x8002_c280, [0, 0, 0, 0]);
        let res = resources(m);
        m.sw(res + 0x60, COMMON_RESOURCE);
        let size = call(m, 0x8002_c214, [res + 0x60, 1, 0, 0]);
        let destination = arena::alloc(m, size);
        let ctx = context(m);
        let res = resources(m);
        m.sw(ctx + 0x44, 1);
        call(m, 0x8002_c404, [res + 0x60, 1, destination, READ_COMPLETE]);
        let block = block(m);
        let loaded = m.lw(block + 0x14);
        m.sw(block + 0x18, destination.wrapping_sub(loaded));
        1
    })
}
/// 800CF144 (3, 2): bind the common resource: tables (800CC9AC with
/// children 0x30/0x2C), scene +0x200A0/+0x200A4/+0x200AC/+0x200B0 the
/// relocated children 6/0/1/2, +0x200C0/+0x200C4/+0x200B4 children 4/5/7,
/// the text lists (800D0ECC with children 8/7), and queue child 10's
/// textures; returns 1.
pub fn bind_common(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        let res = resources(m);
        let resource = m.lw(res + 0x60);
        let table = call(m, 0x8002_6e98, [resource, 0, 0, 0]);
        let block = block(m);
        m.sw(block + 4, table);
        let first = m.lw(table + 0x30);
        let second = m.lw(table + 0x2c);
        bind_common_tables(m, first, second);
        for (child, offset) in [(0x18u32, 0xa0u32), (0, 0xa4), (4, 0xac), (8, 0xb0)] {
            let data = m.lw(table + child);
            let relocated = call(m, 0x8002_6e98, [data, 0, 0, 0]);
            let base = scene(m).wrapping_add(0x20000);
            m.sw(base + offset, relocated);
        }
        let base = scene(m).wrapping_add(0x20000);
        let value = m.lw(table + 0x10);
        m.sw(base + 0xc0, value);
        let value = m.lw(table + 0x14);
        m.sw(base + 0xc4, value);
        let value = m.lw(table + 0x1c);
        m.sw(base + 0xb4, value);
        let list = m.lw(table + 0x20);
        let strings = m.lw(table + 0x1c);
        super::text::set_lists(m, list, strings);
        let textures = m.lw(table + 0x28);
        queue_textures(m, textures, 0x1_0000, 10);
        1
    })
}
/// 800CF25C (3, 3): poll the texture queue; when done release child 10,
/// load the sound bank of child 9 (8002D544) and release it, start the
/// background voices (800E8008) and keep the map palette (800E7E20);
/// returns 1, else 0.
pub fn finish_common(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        if poll_textures(m) != 1 {
            return 0;
        }
        let loader = block(m);
        let common = m.lw(loader + 4);
        let child = m.lw(common + 0x28);
        arena::reset(m, child);
        let loader = block(m);
        let common = m.lw(loader + 4);
        let bank = m.lw(common + 0x24);
        call(m, 0x8002_d544, [bank, 0, 0, 0]);
        let loader = block(m);
        let common = m.lw(loader + 4);
        let bank = m.lw(common + 0x24);
        arena::reset(m, bank);
        super::map::start_background_voices(m);
        super::map::keep_palette(m);
        1
    })
}
/// 800CF2F4 (3, 4): record 1 time = fade timer; count it to 10, then
/// return 2.
pub fn fade_in(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let loader = block(m);
        let fade = m.lw(loader + 0xc);
        visual::set_parameter(m, 1, fade);
        let block = block(m);
        let fade = m.lw(block + 0xc);
        if (fade as i32) < 10 {
            m.sw(block + 0xc, fade.wrapping_add(1));
            0
        } else {
            2
        }
    })
}
/// 800CF350 (4, 0): returns 4 (finished).
pub fn finished() -> u32 {
    4
}

// ---- leaves ---------------------------------------------------------------------------------

/// 800CE7E4: queue the TIM list `textures` (count word, relocated by
/// 80026E98) to be processed `batch` entries per poll; the arena mark is
/// a fresh 4-byte allocation (80026F70), which it returns. `_capacity`
/// (0x10000) is unused.
pub fn queue_textures(m: &mut Machine, textures: u32, _capacity: u32, batch: u32) -> u32 {
    framed(m, 0x20, |m, _| {
        let queue = m.lw(TEXTURES);
        let count = m.lw(textures);
        m.sw(queue + 4, count);
        let entries = call(m, 0x8002_6e98, [textures, 0, 0, 0]);
        let queue = m.lw(TEXTURES);
        m.sw(queue, entries);
        let mark = arena::alloc(m, 4);
        let queue = m.lw(TEXTURES);
        m.sw(queue + 8, mark);
        m.sw(queue + 0x10, batch);
        m.sw(queue + 0x14, 0);
        mark
    })
}

/// 800CE844: process the next batch of the texture queue: compressed
/// entries (magic 0x1A7A6C55) are expanded at the arena mark (800AE118)
/// and loaded from there (80026D00), plain TIMs (0x10) loaded with
/// DrawSync(0). Returns 1 and resets the arena to the mark when the list
/// is done, else advances the cursor and returns 0.
pub fn poll_textures(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        let queue = m.lw(TEXTURES);
        let mut index = m.lw(queue + 0x14);
        let batch = m.lw(queue + 0x10);
        let mut destination = m.lw(queue + 8);
        if (index as i32) < (index.wrapping_add(batch) as i32) {
            loop {
                let queue = m.lw(TEXTURES);
                let count = m.lw(queue + 4);
                if (index as i32) >= (count as i32) {
                    let mark = m.lw(queue + 8);
                    arena::reset(m, mark);
                    return 1;
                }
                let entries = m.lw(queue);
                let entry = m.lw(entries.wrapping_add(index << 2));
                let magic = m.lw(entry);
                if magic == 0x1a7a_6c55 {
                    let expanded = call(m, 0x800a_e118, [entry, destination, 0, 0]);
                    let source = destination;
                    destination = expanded;
                    call(m, 0x8002_6d00, [source, 0, 0, 0]);
                } else if magic == 0x10 {
                    call(m, 0x8002_6d00, [entry, 0, 0, 0]);
                    call(m, 0x800a_6664, [0, 0, 0, 0]);
                }
                let queue = m.lw(TEXTURES);
                let cursor = m.lw(queue + 0x14);
                let batch = m.lw(queue + 0x10);
                index = index.wrapping_add(1);
                if (index as i32) >= (cursor.wrapping_add(batch) as i32) {
                    break;
                }
            }
        }
        let queue = m.lw(TEXTURES);
        let count = m.lw(queue + 4);
        if (index as i32) < (count as i32) {
            let cursor = m.lw(queue + 0x14);
            let batch = m.lw(queue + 0x10);
            m.sw(queue + 0x14, cursor.wrapping_add(batch));
            0
        } else {
            let mark = m.lw(queue + 8);
            arena::reset(m, mark);
            1
        }
    })
}

/// 800CC9AC: fill the common table block `[0x800F1E44]` from `tables`
/// (relocated by 80026E98; byte +2 = count + 0x10, eight words from the
/// first entry +0x14 at +0x60, 32 words per entry from +0x40 at +0x80 +
/// entry*0x80) and copy 32 words of `extra` to +0x100; each part only for
/// a KSEG0 RAM pointer. Returns the `extra` test's v0: 0x801F0000 (the
/// `lui` in the delay slot of `bgez`) for a non-negative `extra`, 1 above
/// 0x801FFFFF, 0 after the copy.
pub fn bind_common_tables(m: &mut Machine, tables: u32, extra: u32) -> u32 {
    framed(m, 0x20, |m, _| {
        let target = m.lw(COMMON_TABLES);
        let in_ram = |pointer: u32| (pointer as i32) < 0 && pointer <= 0x801f_ffff;
        if in_ram(tables) {
            let count = m.lw(tables);
            let mut entries = call(m, 0x8002_6e98, [tables, 0, 0, 0]);
            m.sb(target + 2, count.wrapping_add(0x10) as u8);
            let mut from = m.lw(entries);
            for word in 0..8u32 {
                let value = m.lw(from + 0x14);
                from = from.wrapping_add(4);
                m.sw(target + 0x60 + word * 4, value);
            }
            let mut block = target;
            let mut entry = 0i32;
            while entry < count as i32 {
                let mut from = m.lw(entries);
                let mut to = block;
                for _ in 0..0x20 {
                    let value = m.lw(from + 0x40);
                    from = from.wrapping_add(4);
                    m.sw(to + 0x80, value);
                    to = to.wrapping_add(4);
                }
                block = block.wrapping_add(0x80);
                entry += 1;
                entries = entries.wrapping_add(4);
            }
        }
        if (extra as i32) >= 0 {
            return 0x801f_0000;
        }
        if extra > 0x801f_ffff {
            return 1;
        }
        for word in 0..0x20u32 {
            let value = m.lw(extra + word * 4);
            m.sw(target + 0x100 + word * 4, value);
        }
        0
    })
}

/// 800D1890: resource of `mission` from the 41 mission records (first
/// record's when absent); 0x6E when options 0x50 are both set.
pub fn mission_resource(m: &mut Machine, mission: u32) -> u32 {
    let ctx = context(m);
    if m.lw(ctx + 0x970) & 0x50 == 0x50 {
        return 0x6e;
    }
    for index in 0..0x29u32 {
        let record = MISSIONS + index * 0xc;
        if u32::from(m.lbu(record)) == mission {
            return m.lw(record + 4);
        }
    }
    m.lw(MISSIONS + 4)
}

/// 800DC074: make the loadout of `aircraft` available (first record when
/// absent): primary weapon flags context +0x9F8 + id, secondary +0x9FE + id.
/// Returns record + secondary count (the second loop's delay slot), or 0
/// without secondary weapons.
pub fn unlock_loadout(m: &mut Machine, aircraft: u32) -> u32 {
    let flags = context(m) + 0x960;
    let mut record = LOADOUTS;
    for index in 0..0x15u32 {
        let at = LOADOUTS + index * 0x10;
        if m.lw(at) == aircraft {
            record = at;
            break;
        }
    }
    let mut slot = 0u32;
    while (slot as i32) < i32::from(m.lbu(record + 4)) {
        let id = u32::from(m.lbu(record + slot + 8));
        m.sb(flags.wrapping_add(id) + 0x98, 1);
        slot += 1;
    }
    if m.lbu(record + 5) == 0 {
        return 0;
    }
    let mut slot = 0u32;
    while (slot as i32) < i32::from(m.lbu(record + 5)) {
        let id = u32::from(m.lbu(record + slot + 0xc));
        m.sb(id.wrapping_add(flags) + 0x9e, 1);
        slot += 1;
    }
    record.wrapping_add(slot)
}

// ---- dispatch -----------------------------------------------------------------------------------

/// Loader routines by original address (asked by `ov2_core::dispatch`
/// while module 2 is loaded).
pub(crate) fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let [a0, a1, a2, _] = args;
    Some(match target {
        0x800d_3254 => initialize(m),
        0x800d_328c => update(m),
        0x800c_eb80 => set_page(m, a0),
        0x800c_eb90 => bind_common_scripts(m),
        0x800c_ebcc => progress(m, a0, a1),
        0x800c_ed30 => bind_builtin(m),
        0x800c_ed5c => select_loader_page(m),
        0x800c_eda8 => measure(m),
        0x800c_ee3c => read_mission(m),
        0x800c_eef0 | 0x800c_f100 => wait_read(m),
        0x800c_ef34 => bind_mission(m),
        0x800c_f014 => finish_mission_textures(m),
        0x800c_f064 => read_common(m),
        0x800c_f144 => bind_common(m),
        0x800c_f25c => finish_common(m),
        0x800c_f2f4 => fade_in(m),
        0x800c_f350 => finished(),
        0x800c_e7e4 => queue_textures(m, a0, a1, a2),
        0x800c_e844 => poll_textures(m),
        0x800c_c9ac => bind_common_tables(m, a0, a1),
        0x800d_1890 => mission_resource(m, a0),
        0x800d_c074 => unlock_loadout(m, a0),
        _ => return None,
    })
}
