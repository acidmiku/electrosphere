//! Patches of `loading` routines whose JP instructions differ from PAL
//! (REVIEW.md §2, part `resident-part2`). Each is listed in
//! [`REPLACES`](super::REPLACES).
//!
//! The state-2 loading callbacks read the context fields from +0x390 at
//! their JP offsets (4 bytes lower; +0xF80..+0xF88 become
//! +0x103C..+0x1044), draw on the 240-line screen, and call two JP routines
//! in place of PAL-only ones: the replay selection 80036130 (PAL 80035E28)
//! and the palette builder 8005EBB8 (PAL 8005D490). The JP loading text draws
//! the difficulty line in the fixed font (8003563C) and in replays too.
use crate::{
    arena,
    common::{CONTEXT, RESOURCES},
    dispatch,
    loading::{
        READ_COMPLETE, decompress, draw_aperture, draw_fixed_text, draw_screen, initialize_screen,
        install_sound_bank, queue_files, read_progress, relocate_directory, relocate_fields,
        reset_sound, total_size, upload_texture_group,
    },
    machine::{Machine, SmallData},
    sdk, shared,
};

/// Result a loading callback returns to advance the resident substate.
const ADVANCE: u32 = 0x0400_0000;
/// `[gp+0x6C]`: the 60-byte loading state (frame, progress, mode, stage, ...).
const LOADING: u32 = 0x6c;
/// `[gp+0x2D4]`: render status block (flags, phase, background, parameter).
const RENDER: u32 = 0x2d4;
const MISSION_FILES: u32 = 0x800b_0b08;
const AIRCRAFT_FILES: u32 = 0x800b_0b84;
const MISSION_MODES: u32 = 0x800b_0c64;
const MISSION_NAMES: u32 = 0x800b_0ca0;
const NAME_PREFIX: u32 = 0x800b_cc0c;
/// 80036824: the four difficulty texts (pointers), by context word +0x394 & 3.
const DIFFICULTY_TEXTS: u32 = 0x800b_0d90;
/// 80036E84: the 17-entry aperture size table.
const APERTURE_SIZES: u32 = 0x8002_39a0;
/// Top of the resident arena; compressed prefixes are staged below it.
const ARENA_TOP: u32 = 0x801f_b700;
/// 80036130 (JP-only): the replay mission/aircraft selection (PAL 80035E28).
const SELECT_REPLAY: u32 = 0x8003_6130;
/// 8005EBB8 (JP-only): the mission palettes (PAL 8005D490).
const PREPARE_PALETTES: u32 = 0x8005_ebb8;

const fn off(base: u32, offset: u32) -> u32 {
    base.wrapping_add(offset)
}
/// `lw t, 0(p); lw t, 0(t)`: the word a stored pointer points at.
fn word_at(m: &mut Machine, pointer: u32) -> u32 {
    let address = m.lw(pointer);
    m.lw(address)
}
fn sext16(value: i16) -> u32 {
    value as i32 as u32
}
/// Run `body` inside an original stack frame of `size` bytes (its base is the
/// callee's `sp`), so stack addresses passed down match the original.
fn framed<R>(m: &mut Machine, size: u32, body: impl FnOnce(&mut Machine, u32) -> R) -> R {
    let sp = m.push_frame(size);
    let result = body(m, sp);
    m.pop_frame(size);
    result
}
/// `lwl rt, address` (little-endian): the bytes from `address` down to its
/// word boundary replace the high end of `rt`.
fn lwl(m: &mut Machine, rt: u32, address: u32) -> u32 {
    let shift = (address & 3) * 8;
    let word = m.lw(address & !3);
    (rt & 0x00ff_ffffu32.wrapping_shr(shift)) | word.wrapping_shl(24 - shift)
}
/// `swl rt, address` (little-endian): the high end of `rt` replaces the bytes
/// from `address` down to its word boundary.
fn swl(m: &mut Machine, rt: u32, address: u32) {
    let shift = (address & 3) * 8;
    let aligned = address & !3;
    let word = m.lw(aligned);
    let keep = if shift == 24 {
        0
    } else {
        0xffff_ff00u32 << shift
    };
    m.sw(aligned, (word & keep) | (rt >> (24 - shift)));
}

/// 800358F8 (JP; PAL 800355F0, near, `imm:6`, one call changed): state 2,
/// substate 0. Resets the CD queue and sound; in a replay (context byte
/// +0x3A5, PAL +0x3A9) selects the replay mission through JP 80036130 (PAL
/// 80035E28); clears the exit fields (+0x957, +0x958, +0x3AC; PAL +0x95B,
/// +0x95C, +0x3B0); queues the mission file (context half +0x38C, PAL +0x390)
/// into a new arena allocation, sets render flag 1, selects the visual mode
/// and polls once. Returns [`ADVANCE`].
pub fn initialize(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        shared::reset_cd_queue(m, 0, 0);
        reset_sound(m);
        let context = m.gw(CONTEXT);
        if m.lbu(off(context, 0x3a5)) != 0 {
            dispatch::call(m, SELECT_REPLAY, [0; 4]);
        }
        let context = m.gw(CONTEXT);
        m.sb(off(context, 0x957), 0);
        let context = m.gw(CONTEXT);
        let mission = sext16(m.lh(off(context, 0x38c)));
        m.sh(off(context, 0x958), 0);
        m.sh(off(context, 0x3ac), 0);
        let entry = off(mission << 1, MISSION_FILES);
        let resources = m.gw(RESOURCES);
        let file = sext16(m.lh(entry));
        m.sw(off(resources, 0x60), file);
        let size = total_size(m, off(resources, 0x60), 1);
        let destination = arena::alloc(m, size);
        let list = off(m.gw(RESOURCES), 0x60);
        let state = m.gw(LOADING);
        m.sw(off(state, 8), 1);
        queue_files(m, list, 1, destination, READ_COMPLETE);
        let render = m.gw(RENDER);
        let state = m.gw(LOADING);
        let flags = m.lw(render);
        m.sw(off(render, 4), 0);
        m.sw(off(render, 0x14), 0);
        m.sw(render, flags | 1);
        m.sh(state, 0);
        let mode = m.lbu(off(mission, MISSION_MODES));
        m.sb(off(state, 4), mode);
        let context = m.gw(CONTEXT);
        if m.lbu(off(context, 0x3a5)) != 0 {
            let state = m.gw(LOADING);
            m.sb(off(state, 4), 1);
        }
        let state = m.gw(LOADING);
        m.sb(off(state, 6), 0);
        let state = m.gw(LOADING);
        m.sh(off(state, 2), 0);
        poll(m);
        ADVANCE
    })
}

/// 800357C0 (JP; PAL 800354B8, near, `imm:2`): substates 1 and 4. While the
/// mission (stage 1; context half +0x38C, PAL +0x390) or aircraft (stage 2;
/// +0x38E, PAL +0x392) file is the active read, store its stage and 0..4096
/// progress; initialize the screen on frame 0, fade on frames 1-2, draw from
/// frame 3, advance the frame counter and return [`ADVANCE`] once the read
/// is no longer pending (else 0).
pub fn poll(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let active = shared::current_cd_file(m);
        let context = m.gw(CONTEXT);
        let mission = sext16(m.lh(off(context, 0x38c)));
        let mission_file = sext16(m.lh(off(mission << 1, MISSION_FILES)));
        let stage = if active == mission_file {
            Some(1)
        } else {
            let aircraft = sext16(m.lh(off(context, 0x38e)));
            let aircraft_file = sext16(m.lh(off(aircraft << 1, AIRCRAFT_FILES)));
            (active == aircraft_file).then_some(2)
        };
        if let Some(stage) = stage {
            let state = m.gw(LOADING);
            m.sb(off(state, 6), stage);
            let progress = read_progress(m, 0);
            let state = m.gw(LOADING);
            m.sh(off(state, 2), progress as u16);
        }
        let state = m.gw(LOADING);
        if m.lh(state) <= 0 {
            initialize_screen(m);
        }
        let state = m.gw(LOADING);
        if u32::from(m.lhu(state)).wrapping_sub(1) < 2 {
            let context = m.gw(CONTEXT);
            let ot = m.lw(off(context, 0xc));
            let primitives = m.lw(off(context, 0x14));
            let next = fade(m, ot, primitives, 0);
            let context = m.gw(CONTEXT);
            m.sw(off(context, 0x14), next);
        }
        let state = m.gw(LOADING);
        if m.lh(state) >= 3 {
            draw_screen(m);
        }
        let state = m.gw(LOADING);
        let pending = m.lw(off(state, 8));
        let frame = m.lhu(state);
        m.sh(state, frame.wrapping_add(1));
        u32::from(pending == 0) << 26
    })
}

/// 80035A38 (JP; PAL 80035730, near, `imm:2`, one call changed): substate
/// 2. Prepare the loaded mission resource (17-child directory): install
/// sound child 16, upload texture children 15..11 (each expanded over the
/// following, already consumed child), stage the compressed prefix below
/// [`ARENA_TOP`], expand children 0..10 after the directory, bind the
/// resource fields (the section counts go to context +0x103C/+0x1044; PAL
/// +0xF80/+0xF88), relocate the record directories, alias object slots 96
/// onward, build the palettes through JP 8005EBB8 (PAL 8005D490; same
/// arguments: section 0 +8, +0x78 and word +0x74), set the arena end and
/// poll once. Returns [`ADVANCE`].
pub fn prepare_mission(m: &mut Machine) -> u32 {
    framed(m, 0x28, |m, _| {
        let resources = m.gw(RESOURCES);
        let raw = m.lw(off(resources, 0x60));
        let directory = relocate_directory(m, raw);
        let bank = m.lw(off(directory, 0x40));
        install_sound_bank(m, bank);
        for child in (11..16).rev() {
            let source = m.lw(off(directory, child * 4));
            let target = m.lw(off(directory, child * 4 + 4));
            upload_texture_group(m, source, target);
            sdk::draw_sync(m, 0);
        }
        let end = m.lw(off(directory, 0x2c));
        let words = ((end.wrapping_sub(directory) as i32) >> 2) as u32;
        let staged = ARENA_TOP.wrapping_sub(words << 2);
        let delta = (((staged.wrapping_sub(directory)) as i32) >> 2) as u32;
        shared::copy_words(m, staged, directory, words);
        let delta = delta << 2;
        for slot in 0..11 {
            let pointer = m.lw(off(staged, slot * 4));
            m.sw(off(staged, slot * 4), pointer.wrapping_add(delta));
        }
        let mut output = off(directory, 0x44);
        m.sw(directory, output);
        for child in 0..11 {
            let source = m.lw(off(staged, child * 4));
            output = decompress(m, source, output);
            if child < 10 {
                m.sw(off(directory, child * 4 + 4), output);
            }
        }
        let first = m.lw(directory);
        m.sw(off(directory, 0x2c), output);
        m.sw(off(resources, 0x18), first);
        let context = m.gw(CONTEXT);
        let section3 = m.lw(off(directory, 0xc));
        let count = m.lw(section3);
        m.sw(off(context, 0x103c), count);
        let section5 = m.lw(off(directory, 0x14));
        let count = m.lw(section5);
        m.sw(off(context, 0x1044), count);
        let fields = off(resources, 0x18);
        for (field, child) in [(4, 1), (0x20, 8), (0x18, 6), (0x1c, 7)] {
            let value = m.lw(off(directory, child * 4));
            m.sw(off(fields, field), value);
        }
        let section = m.lw(off(directory, 8));
        let table = relocate_directory(m, section);
        m.sw(off(fields, 8), table.wrapping_sub(4));
        for (field, child) in [(0xc, 3), (0x10, 4), (0x14, 5), (0x24, 9), (0x28, 10)] {
            let section = m.lw(off(directory, child * 4));
            let table = relocate_directory(m, section);
            m.sw(off(fields, field), table);
        }
        // Section 2: the stored table points at the count word (index 1..=n).
        let count = word_at(m, off(directory, 8));
        if count as i32 > 0 {
            let mut index = 1u32;
            loop {
                let table = m.lw(off(fields, 8));
                let record = m.lw(off(table, index << 2));
                relocate_fields(m, record, off(record, 4), 4);
                let count = word_at(m, off(directory, 8));
                index = index.wrapping_add(1);
                if (count as i32) < (index as i32) {
                    break;
                }
            }
        }
        for (child, field, width) in [(3, 0xc, 6), (4, 0x10, 6), (5, 0x14, 4)] {
            relocate_records(m, off(directory, child * 4), off(fields, field), width);
        }
        // Mission section 4 records replace section 3 slots 96 onward.
        let count = word_at(m, off(directory, 0x10));
        if count as i32 > 0 {
            let mut index = 0u32;
            loop {
                let source = m.lw(off(fields, 0x10));
                let target = m.lw(off(fields, 0xc));
                let record = m.lw(off(index << 2, source));
                m.sw(off(off(index << 2, target), 0x180), record);
                let count = word_at(m, off(directory, 0x10));
                index = index.wrapping_add(1);
                if (index as i32) >= (count as i32) {
                    break;
                }
            }
        }
        let section0 = m.lw(fields);
        let patches = m.lw(off(section0, 0x74));
        dispatch::call(
            m,
            PREPARE_PALETTES,
            [off(section0, 8), off(section0, 0x78), patches, 0],
        );
        let end = m.lw(off(directory, 0x2c));
        arena::reset(m, end);
        poll(m);
        ADVANCE
    })
}

/// Shared loop shape of 80035A38/80035F98: relocate `width` fields of every
/// record listed by the directory whose count word is `[count_slot]`, using
/// the stored table pointer at `table_field`.
fn relocate_records(m: &mut Machine, count_slot: u32, table_field: u32, width: u32) {
    let count = word_at(m, count_slot);
    if count as i32 <= 0 {
        return;
    }
    let mut index = 0u32;
    loop {
        let table = m.lw(table_field);
        let record = m.lw(off(index << 2, table));
        relocate_fields(m, record, off(record, 4), width);
        let count = word_at(m, count_slot);
        index = index.wrapping_add(1);
        if (index as i32) >= (count as i32) {
            break;
        }
    }
}

/// 80035F18 (JP; PAL 80035C10, near, `imm:1`): substate 3. Queue the
/// selected aircraft file (context half +0x38E, PAL +0x392) into a new arena
/// allocation and poll once. Returns [`ADVANCE`].
pub fn queue_aircraft(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let context = m.gw(CONTEXT);
        let aircraft = sext16(m.lh(off(context, 0x38e)));
        let entry = off(aircraft << 1, AIRCRAFT_FILES);
        let resources = m.gw(RESOURCES);
        let file = sext16(m.lh(entry));
        m.sw(off(resources, 0x60), file);
        let size = total_size(m, off(resources, 0x60), 1);
        let destination = arena::alloc(m, size);
        let list = off(m.gw(RESOURCES), 0x60);
        let state = m.gw(LOADING);
        m.sw(off(state, 8), 1);
        queue_files(m, list, 1, destination, READ_COMPLETE);
        poll(m);
        ADVANCE
    })
}

/// 80035F98 (JP; PAL 80035C90, near, `imm:1`): substate 5. Prepare the
/// loaded aircraft resource (4-child directory): sound child 3, texture
/// child 2, expansion of children 0/1, record relocation, context +0x1040
/// (PAL +0xF84), resource +0x08/+0x0C/+0x5C, arena end, one poll, then render
/// phase/parameter 0 and flags |= 5. Returns [`ADVANCE`].
pub fn prepare_aircraft(m: &mut Machine) -> u32 {
    framed(m, 0x28, |m, _| {
        let resources = m.gw(RESOURCES);
        let raw = m.lw(off(resources, 0x60));
        let directory = relocate_directory(m, raw);
        let bank = m.lw(off(directory, 0xc));
        install_sound_bank(m, bank);
        let source = m.lw(off(directory, 8));
        let target = m.lw(off(directory, 0xc));
        upload_texture_group(m, source, target);
        sdk::draw_sync(m, 0);
        let end = m.lw(off(directory, 8));
        let words = ((end.wrapping_sub(directory) as i32) >> 2) as u32;
        let staged = ARENA_TOP.wrapping_sub(words << 2);
        let delta = (((staged.wrapping_sub(directory)) as i32) >> 2) as u32;
        shared::copy_words(m, staged, directory, words);
        let delta = delta << 2;
        let first = m.lw(staged).wrapping_add(delta);
        let second = m.lw(off(staged, 4)).wrapping_add(delta);
        m.sw(staged, first);
        m.sw(off(staged, 4), second);
        m.sw(directory, off(directory, 0x10));
        let source = m.lw(staged);
        let output = decompress(m, source, off(directory, 0x10));
        m.sw(off(directory, 4), output);
        let source = m.lw(off(staged, 4));
        let output = decompress(m, source, output);
        let section0 = m.lw(directory);
        m.sw(off(directory, 8), output);
        let table = relocate_directory(m, section0);
        m.sw(off(resources, 8), table);
        let fields = off(resources, 8);
        relocate_records(m, directory, fields, 6);
        let context = m.gw(CONTEXT);
        let count = word_at(m, directory);
        m.sw(off(context, 0x1040), count);
        let section1 = m.lw(off(directory, 4));
        m.sw(off(fields, 4), section1);
        let end = m.lw(off(directory, 8));
        arena::reset(m, end);
        let top = arena::peek(m, 0);
        let resources = m.gw(RESOURCES);
        m.sw(off(resources, 0x5c), top);
        poll(m);
        let render = m.gw(RENDER);
        let flags = m.lw(render);
        m.sw(off(render, 4), 0);
        m.sw(off(render, 0x14), 0);
        m.sw(render, flags | 5);
        ADVANCE
    })
}

/// 80036E84 (JP; PAL 80036B20, near, `imm:2 ins:3 reg:4`): mode-1 frame:
/// texts (80036824), then the noise aperture sized by the opening table
/// (800239A0, 17 entries, clamped tick: width = half 0 * 320 >> 12, height =
/// half 1 * 240 >> 12, PAL half 1 >> 4; both rounded toward zero; centred on
/// the 320x240 screen, PAL 256 lines) and darkened by the remaining progress;
/// advances the tick (+0x38). Returns the next primitive address.
pub fn draw_aperture_screen(m: &mut Machine, ot: u32, primitives: u32) -> u32 {
    framed(m, 0x70, |m, sp| {
        let sizes: [u32; 17] = std::array::from_fn(|i| m.lw(off(APERTURE_SIZES, 4 * i as u32)));
        let state = m.gw(LOADING);
        let stage = m.lbu(off(state, 6));
        let mut amount = if stage == 1 {
            sext16(m.lh(off(state, 2)))
        } else {
            u32::from(stage == 2) << 12
        };
        if (amount as i32) >= 0x1001 {
            amount = 0x1000;
        }
        let remaining = 0x1000u32.wrapping_sub(amount);
        let primitives = draw_texts(m, ot, primitives);
        let state = m.gw(LOADING);
        let tick = (m.lw(off(state, 0x38)) as i32).clamp(0, 16);
        let entry = sizes[tick as usize];
        let scaled = i32::from(entry as u16 as i16).wrapping_mul(320);
        let width = if scaled < 0 {
            scaled.wrapping_add(0xfff) >> 12
        } else {
            scaled >> 12
        };
        m.sh(off(sp, 0x14), width as u16);
        let scaled = i32::from((entry >> 16) as u16 as i16).wrapping_mul(240);
        let height = if scaled < 0 {
            scaled.wrapping_add(0xfff) >> 12
        } else {
            scaled >> 12
        };
        m.sh(off(sp, 0x10), (0x140i32.wrapping_sub(width) >> 1) as u16);
        m.sh(off(sp, 0x16), height as u16);
        m.sh(off(sp, 0x12), (0xf0i32.wrapping_sub(height) >> 1) as u16);
        let next = draw_aperture(m, ot, primitives, off(sp, 0x10), remaining << 1);
        let state = m.gw(LOADING);
        let tick = m.lw(off(state, 0x38));
        m.sw(off(state, 0x38), tick.wrapping_add(1));
        next
    })
}

/// 80036824 (JP; PAL 800364D0, near, `del:1 imm:3 ins:4 op:6 reg:1`, one
/// call changed): for the first 40 frames the difficulty line: text
/// `[0x800B0D90 + (context word +0x394 & 3) * 4]` in the fixed font
/// (8003563C) at (0xBE, 0x12), colour 0x4000, also in replays (PAL: "Level:"
/// in the proportional font 80034F7C at x 0xAA, not in replays); then the
/// mission name (context half +0x38C, PAL +0x390) in the fixed font, its
/// green level following the progress, with the blinking prefix while
/// context clock bit 0x10 is set. Returns the next primitive address.
pub fn draw_texts(m: &mut Machine, ot: u32, primitives: u32) -> u32 {
    framed(m, 0x28, |m, _| {
        let mut next = primitives;
        let state = m.gw(LOADING);
        if m.lh(state) < 0x28 {
            let context = m.gw(CONTEXT);
            let difficulty = m.lw(off(context, 0x394)) & 3;
            let text = m.lw(off(difficulty << 2, DIFFICULTY_TEXTS));
            next = draw_fixed_text(m, ot, next, 0xbe, 0x12, 0x4000, text);
        }
        let state = m.gw(LOADING);
        let stage = m.lbu(off(state, 6));
        let visible = (stage == 1 && m.lh(off(state, 2)) > 0) || stage == 2;
        if !visible {
            return next;
        }
        let context = m.gw(CONTEXT);
        let mission = sext16(m.lh(off(context, 0x38c)));
        let text = m.lw(off(mission << 2, MISSION_NAMES));
        let mut length = 0u32;
        while m.lbu(off(text, length)) != 0 {
            length = length.wrapping_add(1);
        }
        let x = 0x140u32.wrapping_sub(length << 3) as i32 >> 1;
        let state = m.gw(LOADING);
        let level = if m.lbu(off(state, 6)) == 1 {
            i32::from(m.lh(off(state, 2)))
        } else {
            0x1000 - i32::from(m.lh(off(state, 2)))
        };
        let level = (if level < 0 { level + 0x1f } else { level }) >> 5;
        let color = ((level as u32) & 0xff) << 8;
        next = draw_fixed_text(m, ot, next, x as u32, 0x76, color, text);
        let context = m.gw(CONTEXT);
        if m.lw(off(context, 0x20)) & 0x10 != 0 {
            next = draw_fixed_text(
                m,
                ot,
                next,
                x.wrapping_sub(0xc) as u32,
                0x76,
                color,
                NAME_PREFIX,
            );
        }
        next
    })
}

/// 800362A0 (JP; PAL 80035F5C, near, `imm:1`): full-screen fade: E6 mask
/// on, E1 (add 0x220 / subtract 0x240 for negative amounts), a 320x240
/// (PAL 320x256) semi-transparent TILE of grey |amount|, E6 mask off; the
/// four packets form one forward chain prepended to `ot`. Returns the next
/// primitive address.
pub fn fade(m: &mut Machine, ot: u32, primitive: u32, amount: u32) -> u32 {
    let second = off(primitive, 0xc);
    let third = off(primitive, 0x14);
    m.sw(primitive, ((second << 8) >> 8) | 0x0200_0000);
    m.sw(off(primitive, 4), 0xe600_0001);
    m.sw(off(primitive, 8), 0);
    let (mode, amount) = if (amount as i32) < 0 {
        (0xe100_0240, amount.wrapping_neg())
    } else {
        (0xe100_0220, amount)
    };
    m.sw(off(primitive, 0xc), ((third << 8) >> 8) | 0x0100_0000);
    m.sw(off(second, 4), mode);
    m.sw(third, 0x0300_0000);
    m.sw(
        off(third, 4),
        amount | (amount << 8) | (amount << 16) | 0x6200_0000,
    );
    m.sh(off(third, 0xc), 0x140);
    m.sh(off(third, 0xe), 0xf0);
    let last = off(third, 0x10);
    m.sw(last, 0x0200_0000);
    m.sh(off(third, 8), 0);
    m.sh(off(third, 0xa), 0);
    m.sw(off(last, 4), 0xe600_0000);
    m.sw(off(last, 8), 0);
    let tag = m.lw(last);
    let link = m.lw(ot);
    let merged = (tag & 0xff00_0000) | (link & 0x00ff_ffff);
    m.sw(last, merged);
    let link = m.lw(ot);
    m.sw(ot, (link & 0xff00_0000) | (last & 0x00ff_ffff));
    // addPrim(ot, primitive) through lwl/swl at +2: the low byte of the
    // register is shifted out, so only the OT link reaches memory.
    let link = lwl(m, merged, off(ot, 2));
    swl(m, link, off(third, 2));
    swl(m, primitive << 8, off(ot, 2));
    off(third, 0x1c)
}
