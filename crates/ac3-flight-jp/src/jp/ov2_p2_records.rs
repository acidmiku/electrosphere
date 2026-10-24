//! JP overlay 2, part 2: the hub's mission records page, translated from
//! the JP instructions.
//!
//! [`open_records`] (800DC144) builds the record list (`[0x80112DBC]`,
//! 0x40 bytes from the arena: +0..+0x3B mission numbers, +0x3C count,
//! +0x3D/+0x3E counts of completed entries, +0x3F the selection) from the
//! context's mission flags; [`draw_records`] (800DC43C) prints the
//! selected record each frame (text routines 800F33A8.. of another part)
//! and moves the selection. The record data is context +0x95C ("s3"):
//! +0x18 / +0x58 completion flags, +0x68/+0xE0/+0x158 best times per
//! difficulty (context +0x394), +0x248 times, +0x3B0 ranks, +0x4F0/+0x5E0
//! counts, +0x6D0 the play time, +0x6D4/+0x6D8 totals.
//!
//! Calls to routines of other parts pass the argument registers the
//! original holds at the call (the previous call's arguments where it sets
//! none), as the test oracle does; the callees read only the ones they
//! are given.
use super::ov2_p2::{MODULE, framed, lbu, less, lhu, mul, visual, work};
use crate::common::{CONTEXT, RESOURCES, loaded_module};
use crate::machine::{Machine, SmallData};
use crate::{arena, dispatch};

/// Pointer to the record list.
pub const LIST: u32 = 0x8011_2dbc;
/// Record pages' state handlers by visual block +0x86.
pub const PAGE_STEPS: u32 = 0x8010_b358;
/// Mission names by mission number.
pub const NAMES: u32 = 0x8010_a3bc;
/// Rank names by rank.
pub const RANKS: u32 = 0x8010_a4b0;

/// 80027338 (resident, `loading`).
const RESOURCE: u32 = 0x8002_7338;
/// 80027538 (resident, `arena`): reset the arena pointer.
const ARENA_RESET: u32 = 0x8002_7538;
/// 8002DB4C (resident, `script`): start sound effect `a0`.
const SOUND: u32 = 0x8002_db4c;
/// 800ECDA8, 800ED000 (overlay 2, another part): image release and load.
const RELEASE: u32 = 0x800e_cda8;
const LOAD_IMAGE: u32 = 0x800e_d000;
/// 800FB2A0 (overlay 2, another part): the image index of mission `a0`.
const IMAGE_OF: u32 = 0x800f_b2a0;
/// Text routines (overlay 2, another part): position, color, style,
/// alignment, print (format, up to three words), print (format, three
/// words).
const TEXT_AT: u32 = 0x800f_33a8;
const TEXT_COLOR: u32 = 0x800f_33d0;
const TEXT_ALIGN: u32 = 0x800f_33f8;
const TEXT_FONT: u32 = 0x800f_35b8;
const PRINT: u32 = 0x800f_4ae0;
const PRINT3: u32 = 0x800f_4b40;
/// 80100FC4 (overlay 2, another part; PAL 800EA020 `ov2_hub_save`).
const SAVE_STEP: u32 = 0x8010_0fc4;

/// The longest time printed (0x66FF2FF frames).
const MAX_TIME: u32 = 0x066f_f2ff;

/// The argument registers as the original holds them between calls.
struct Args([u32; 4]);
impl Args {
    fn call(&mut self, m: &mut Machine, target: u32) -> u32 {
        dispatch::call(m, target, self.0)
    }
    fn set(&mut self, values: &[(usize, u32)]) -> &mut Self {
        for &(i, v) in values {
            self.0[i] = v;
        }
        self
    }
}

fn list(m: &mut Machine) -> u32 {
    m.lw(LIST)
}

/// The selected mission number (list byte at the selection +0x3F).
fn selected(m: &mut Machine) -> u32 {
    let list = list(m);
    let selection = lbu(m, list.wrapping_add(0x3f));
    lbu(m, list.wrapping_add(selection))
}

/// 800DC144: open the records page: release the page images, reload
/// resource [resources +0x60] (its entry +0 into visual +0x1DC), build the
/// record list (every mission whose context flag +0xD0C is set, then the
/// selector +0x38C; the completed counts of +0x18 (64) and +0x58 (16)),
/// select the last entry, load its image, step the hub state, reset cells
/// 0x11/0x12 of list 2 and put word 0x60000 in slot 2, and copy the flags
/// of the selection into work flags +0x32/+0x33/+0x35. Returns the last
/// flag.
pub fn open_records(m: &mut Machine, args: [u32; 4]) -> u32 {
    framed(m, 0x30, |m, sp| {
        let resources = m.gw(RESOURCES);
        let context = m.gw(CONTEXT);
        let id = m.lw(resources.wrapping_add(0x60));
        let block = visual(m);
        let records = context.wrapping_add(0x95c);
        m.sw(block.wrapping_add(0x28), id);
        let mut a = Args(args);
        let resource = a.set(&[(0, id)]).call(m, RESOURCE);
        let image = m.lw(resource.wrapping_add(4));
        a.set(&[(0, image)]).call(m, RELEASE);
        let block = visual(m);
        let page = m.lw(block.wrapping_add(0x18));
        let image = m.lw(page.wrapping_add(0x18));
        a.set(&[(0, image)]).call(m, RELEASE);
        let pointer = m.lw(resource.wrapping_add(4));
        a.set(&[(0, pointer)]).call(m, ARENA_RESET);
        let id = m.lw(resource);
        let entry = a.set(&[(0, id)]).call(m, RESOURCE);
        let block = visual(m);
        m.sw(block.wrapping_add(0x1dc), entry);
        let list = arena::alloc(m, 0x40);
        m.sw(LIST, list);
        m.sb(list.wrapping_add(0x3c), 0);
        for mission in 0..0x3bu32 {
            let context = m.gw(CONTEXT);
            if lbu(m, context.wrapping_add(mission).wrapping_add(0xd0c)) != 0 {
                let list = self::list(m);
                let count = lbu(m, list.wrapping_add(0x3c));
                m.sb(list.wrapping_add(count), mission as u8);
                let list = self::list(m);
                let count = lbu(m, list.wrapping_add(0x3c));
                m.sb(list.wrapping_add(0x3c), count.wrapping_add(1) as u8);
            }
        }
        let list = self::list(m);
        let context = m.gw(CONTEXT);
        let count = lbu(m, list.wrapping_add(0x3c));
        let selector = lbu(m, context.wrapping_add(0x38c));
        m.sb(list.wrapping_add(count), selector as u8);
        let list = self::list(m);
        let count = lbu(m, list.wrapping_add(0x3c));
        m.sb(list.wrapping_add(0x3c), count.wrapping_add(1) as u8);
        for (field, base, entries) in [(0x3d, 0x18, 0x40), (0x3e, 0x58, 0x10)] {
            let list = self::list(m);
            m.sb(list.wrapping_add(field), 0);
            for index in 0..entries {
                if lbu(m, records.wrapping_add(index).wrapping_add(base)) == 1 {
                    let list = self::list(m);
                    let n = lbu(m, list.wrapping_add(field));
                    m.sb(list.wrapping_add(field), n.wrapping_add(1) as u8);
                }
            }
        }
        let list = self::list(m);
        let count = lbu(m, list.wrapping_add(0x3c));
        m.sb(list.wrapping_add(0x3f), count.wrapping_sub(1) as u8);
        let mission = selected(m);
        let image = a
            .set(&[(0, mission), (1, 0x8011_0000), (2, 1)])
            .call(m, IMAGE_OF);
        load_image(m, &mut a, sp, image);
        let record = m.lw(super::ov2_p2_hub::STATE);
        let step = lbu(m, record.wrapping_add(1)).wrapping_add(1);
        m.sb(record.wrapping_add(1), step as u8);
        super::ov2_p2::reset_cell(m, 2, 0x11);
        super::ov2_p2::reset_cell(m, 2, 0x12);
        super::ov2_p2::set_word_178(m, 2, 0x6_0000);
        copy_flags(m, records, false)
    })
}

/// Load image `index` of the page's image table (visual +0x1DC) at
/// (0x3C0, 0x100), CLUT row 0x300, mode 1 (800ED000, fifth argument in
/// the frame).
fn load_image(m: &mut Machine, a: &mut Args, sp: u32, index: u32) {
    let block = visual(m);
    let table = m.lw(block.wrapping_add(0x1dc));
    m.sw(sp + 0x10, 1);
    let image = m.lw((index << 2).wrapping_add(table));
    a.set(&[(0, image), (1, 0x3c0), (2, 0x100), (3, 0x300)])
        .call(m, LOAD_IMAGE);
}

/// Work flags +0x32 (the selection's context flag +0xD0C, or its rank
/// +0x3B0 of the records when `rank`), +0x33 (context +0x394) and +0x35
/// (context +0x95D). Returns the last.
fn copy_flags(m: &mut Machine, records: u32, rank: bool) -> u32 {
    let list = list(m);
    let selection = lbu(m, list.wrapping_add(0x3f));
    let block = work(m);
    let mission = lbu(m, list.wrapping_add(selection));
    if rank {
        let flags = m.lw(block.wrapping_add(0x5c));
        let value = lbu(m, records.wrapping_add(mission).wrapping_add(0x3b0));
        m.sb(flags.wrapping_add(0x32), value as u8);
    } else {
        let context = m.gw(CONTEXT);
        let flags = m.lw(block.wrapping_add(0x5c));
        let value = lbu(m, context.wrapping_add(mission).wrapping_add(0xd0c));
        m.sb(flags.wrapping_add(0x32), value as u8);
    }
    let block = work(m);
    let context = m.gw(CONTEXT);
    let flags = m.lw(block.wrapping_add(0x5c));
    let difficulty = lbu(m, context.wrapping_add(0x394));
    m.sb(flags.wrapping_add(0x33), difficulty as u8);
    let block = work(m);
    let flags = m.lw(block.wrapping_add(0x5c));
    let last = if rank {
        lbu(m, records.wrapping_add(1))
    } else {
        let context = m.gw(CONTEXT);
        lbu(m, context.wrapping_add(0x95d))
    };
    m.sb(flags.wrapping_add(0x35), last as u8);
    last
}

/// `n / d` as the compiler emits it for a constant `d`: the high word of
/// the signed product with `magic`, plus `n`, shifted, minus the sign.
fn divide(n: u32, magic: u32, shift: u32) -> u32 {
    let high = ((i64::from(n as i32) * i64::from(magic as i32)) >> 32) as u32;
    ((high.wrapping_add(n) as i32 >> shift) as u32).wrapping_sub((n as i32 >> 31) as u32)
}

/// Print `frames` (30 per second) with `format` as minutes, seconds and
/// hundredths (800F4B40). Returns the argument registers it passes.
fn print_time(m: &mut Machine, a: &mut Args, format: u32, frames: u32) {
    let seconds = divide(frames, 0x8888_8889, 4);
    let remainder = frames.wrapping_sub(mul(seconds, 30));
    let hundredths = mul(remainder, 100);
    let minutes = divide(frames, 0x91a2_b3c5, 10);
    let seconds = seconds.wrapping_sub(mul(minutes, 60));
    let hundredths = divide(hundredths, 0x8888_8889, 4);
    a.set(&[(0, format), (1, minutes), (2, seconds), (3, hundredths)])
        .call(m, PRINT3);
}

/// 800DC43C: print the selected record: its name (800FB2A0's image
/// rank), best time for the difficulty, totals, completed counts, mission
/// name, rank, time, counts and the play time; set flag 0x31 (more
/// entries after / before the selection) and the work flags; move the
/// selection with pad bits 8 / 4 (context +0x130; sound 0x3D, reload the
/// image) or leave on a press of 0x20 with none of 0x5F held (cell preset
/// 1, state page 0, arena reset, sound 0x3B). Returns work flag +0x35.
///
/// For a difficulty (context +0x394) other than 0..2 the original prints
/// the caller's `s2`, a register it never sets; the port prints 0, the
/// oracle's register value.
pub fn draw_records(m: &mut Machine, args: [u32; 4]) -> u32 {
    framed(m, 0x40, |m, sp| {
        let mut a = Args(args);
        let context = m.gw(CONTEXT);
        let mission = selected(m);
        let records = context.wrapping_add(0x95c);
        let rank = a.set(&[(0, mission)]).call(m, IMAGE_OF);
        a.set(&[(0, 4)]).call(m, TEXT_FONT);
        a.set(&[(0, 2)]).call(m, TEXT_ALIGN);
        a.set(&[(0, 0x80), (1, 0x80), (2, 0x80)])
            .call(m, TEXT_COLOR);
        a.set(&[(0, 0x1d), (1, 0x41)]).call(m, TEXT_AT);
        let context = m.gw(CONTEXT);
        a.set(&[(0, context.wrapping_add(0x948))]).call(m, PRINT);
        a.set(&[(0, u32::MAX)]).call(m, TEXT_ALIGN);
        a.set(&[(0, 8)]).call(m, TEXT_FONT);
        let list = list(m);
        let selection = lbu(m, list.wrapping_add(0x3f));
        let context = m.gw(CONTEXT);
        let difficulty = m.lw(context.wrapping_add(0x394));
        let mission = lbu(m, list.wrapping_add(selection));
        let best = match difficulty {
            0 => lhu(m, records.wrapping_add(mission << 1).wrapping_add(0x68)),
            1 => lhu(m, records.wrapping_add(mission << 1).wrapping_add(0xe0)),
            2 => lhu(m, records.wrapping_add(mission << 1).wrapping_add(0x158)),
            _ => 0,
        };
        let best = clamp(best);
        a.set(&[(0, 0xea), (1, 0x98)]).call(m, TEXT_AT);
        print_time(m, &mut a, 0x800c_c800, best);
        a.set(&[(0, 3)]).call(m, TEXT_FONT);
        a.set(&[(0, 2)]).call(m, TEXT_ALIGN);
        a.set(&[(0, 0xb4), (1, 0x6e)]).call(m, TEXT_AT);
        let number = 0x800c_c80c;
        let total = m.lw(records.wrapping_add(0x6d4));
        a.set(&[(0, number), (1, total)]).call(m, PRINT);
        a.set(&[(0, 0xb4), (1, 0x80)]).call(m, TEXT_AT);
        let total = m.lw(records.wrapping_add(0x6d8));
        a.set(&[(0, number), (1, total)]).call(m, PRINT);
        a.set(&[(0, 0xb4), (1, 0x93)]).call(m, TEXT_AT);
        let fraction = 0x800c_c810;
        let list = self::list(m);
        let done = lbu(m, list.wrapping_add(0x3d));
        a.set(&[(0, fraction), (1, done), (2, 0x14)]).call(m, PRINT);
        a.set(&[(0, 0xb4), (1, 0xa7)]).call(m, TEXT_AT);
        let list = self::list(m);
        let done = lbu(m, list.wrapping_add(0x3e));
        a.set(&[(0, fraction), (1, done), (2, 0xd)]).call(m, PRINT);
        a.set(&[(0, 0x91), (1, 0xd7)]).call(m, TEXT_AT);
        let mission = selected(m);
        let name = m.lw((mission << 2).wrapping_add(NAMES));
        a.set(&[(0, name)]).call(m, PRINT);
        a.set(&[(0, 0x5e), (1, 0xfd)]).call(m, TEXT_AT);
        let rank_name = m.lw((rank << 2).wrapping_add(RANKS));
        a.set(&[(0, rank_name)]).call(m, PRINT);
        a.set(&[(0, 1)]).call(m, TEXT_ALIGN);
        a.set(&[(0, 0xfd), (1, 0xfb)]).call(m, TEXT_AT);
        let mission = selected(m);
        let time = lhu(m, records.wrapping_add(mission << 1).wrapping_add(0x248));
        print_time(m, &mut a, 0x800c_c818, time);
        a.set(&[(0, 0xaf), (1, 0x11a)]).call(m, TEXT_AT);
        let mission = selected(m);
        let count = m.lw(records.wrapping_add(mission << 2).wrapping_add(0x4f0));
        a.set(&[(0, number), (1, count)]).call(m, PRINT);
        a.set(&[(0, 0xaf), (1, 0x12f)]).call(m, TEXT_AT);
        let mission = selected(m);
        let count = m.lw(records.wrapping_add(mission << 2).wrapping_add(0x5e0));
        a.set(&[(0, number), (1, count)]).call(m, PRINT);
        a.set(&[(0, 8)]).call(m, TEXT_FONT);
        a.set(&[(0, 2)]).call(m, TEXT_ALIGN);
        a.set(&[(0, 0x91), (1, 0x167)]).call(m, TEXT_AT);
        let play = clamp(m.lw(records.wrapping_add(0x6d0)));
        let hours = divide(play, 0x9b58_3739, 16);
        let minutes = divide(play, 0x91a2_b3c5, 10);
        let seconds = divide(play, 0x8888_8889, 4);
        let minutes_left = minutes.wrapping_sub(mul(hours, 60));
        let seconds_left = seconds.wrapping_sub(mul(minutes, 60));
        a.set(&[
            (0, 0x800c_c828),
            (1, hours),
            (2, minutes_left),
            (3, seconds_left),
        ])
        .call(m, PRINT3);
        copy_flags(m, records, true);
        let list = self::list(m);
        let count = lbu(m, list.wrapping_add(0x3c));
        let selection = lbu(m, list.wrapping_add(0x3f));
        let more = u32::from(less(selection, count.wrapping_sub(1)));
        let flag = if selection != 0 {
            more << 1 | 1
        } else {
            more << 1
        };
        super::ov2_p2::set_flag(m, 0x31, flag);
        let block = work(m);
        let flags = m.lw(block.wrapping_add(0x5c));
        m.sb(flags.wrapping_add(0x34), 0);
        let context = m.gw(CONTEXT);
        let pad = m.lw(context.wrapping_add(0x130));
        let moved = if pad & 8 != 0 {
            let list = self::list(m);
            let count = lbu(m, list.wrapping_add(0x3c));
            let selection = lbu(m, list.wrapping_add(0x3f));
            if less(selection, count.wrapping_sub(1)) {
                m.sb(list.wrapping_add(0x3f), selection.wrapping_add(1) as u8);
                a.set(&[(0, 0x3d), (1, selection)]).call(m, SOUND);
                Some(1)
            } else {
                a.set(&[(0, 0x3d), (1, selection)]);
                None
            }
        } else if pad & 4 != 0 {
            let list = self::list(m);
            let selection = lbu(m, list.wrapping_add(0x3f));
            if selection != 0 {
                m.sb(list.wrapping_add(0x3f), selection.wrapping_sub(1) as u8);
                a.set(&[(0, 0x3d)]).call(m, SOUND);
                Some(2)
            } else {
                a.set(&[(0, 0x3d)]);
                None
            }
        } else {
            let pressed = m.lw(context.wrapping_add(0x12c)) & 0x20;
            let held = m.lw(context.wrapping_add(0x128)) & 0x5f;
            if pressed != 0 && held == 0 {
                super::ov2_p2::set_cell_preset(m, 0, 1);
                let record = m.lw(super::ov2_p2_hub::STATE);
                m.sb(record, 0);
                let record = m.lw(super::ov2_p2_hub::STATE);
                m.sb(record.wrapping_add(1), 0);
                let block = visual(m);
                let pointer = m.lw(block.wrapping_add(0x28));
                a.set(&[(0, pointer), (1, 1)]).call(m, ARENA_RESET);
                a.set(&[(0, 0x3b)]).call(m, SOUND);
                let context = m.gw(CONTEXT);
                m.sw(context.wrapping_add(0x12c), 0);
            }
            None
        };
        if let Some(direction) = moved {
            let block = work(m);
            let flags = m.lw(block.wrapping_add(0x5c));
            m.sb(flags.wrapping_add(0x34), direction);
            let mission = selected(m);
            let image = a.set(&[(0, mission)]).call(m, IMAGE_OF);
            load_image(m, &mut a, sp, image);
        }
        let block = work(m);
        let context = m.gw(CONTEXT);
        let flags = m.lw(block.wrapping_add(0x5c));
        let last = lbu(m, context.wrapping_add(0x95d));
        m.sb(flags.wrapping_add(0x35), last as u8);
        last
    })
}

/// A time clamped to 0..[`MAX_TIME`] (signed).
fn clamp(time: u32) -> u32 {
    let time = if less(time, 0) { 0 } else { time };
    if less(MAX_TIME, time) { MAX_TIME } else { time }
}

/// 800DCB5C: step the hub state, cell 15 of list 0 idles, visual +0x84 =
/// 6, +0x86 = 0, +0xE0 = 3 when context +0x96C has bit 0x10 (else 1), and
/// word 0x8000 into visual slot 0 of +0x178. Returns that routine's value.
pub fn open_page(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let record = m.lw(super::ov2_p2_hub::STATE);
        let step = lbu(m, record.wrapping_add(1)).wrapping_add(1);
        m.sb(record.wrapping_add(1), step as u8);
        let block = visual(m);
        let cells = m.lw(block.wrapping_add(0x16c));
        m.sh(cells.wrapping_add(0xbe), 1);
        let context = m.gw(CONTEXT);
        m.sh(block.wrapping_add(0x84), 6);
        m.sh(block.wrapping_add(0x86), 0);
        let mode = if m.lw(context.wrapping_add(0x96c)) & 0x10 != 0 {
            3
        } else {
            1
        };
        m.sw(block.wrapping_add(0xe0), mode);
        super::ov2_p2::set_word_178(m, 0, 0x8000)
    })
}

/// 800DCBE0: run the page step of visual +0x86 (table 0x8010B358); when
/// visual +0x84 reaches 7, clear slot 1 of +0x178 and +0x84, step the hub
/// state, start fade slot 1 (mode 1, level 0, target 0x1E) and run
/// 80100FC4. Returns 7 or that routine's value.
pub fn step_page(m: &mut Machine, args: [u32; 4]) -> u32 {
    framed(m, 0x18, |m, _| {
        let block = visual(m);
        let page = super::ov2_p2::lh(m, block.wrapping_add(0x86));
        let handler = m.lw((page << 2).wrapping_add(PAGE_STEPS));
        dispatch::call(m, handler, args);
        let block = visual(m);
        if super::ov2_p2::lh(m, block.wrapping_add(0x84)) != 7 {
            return 7;
        }
        super::ov2_p2::set_word_178(m, 1, 0);
        let block = visual(m);
        let record = m.lw(super::ov2_p2_hub::STATE);
        m.sh(block.wrapping_add(0x84), 0);
        let step = lbu(m, record.wrapping_add(1)).wrapping_add(1);
        m.sb(record.wrapping_add(1), step as u8);
        super::ov2_p2::set_fade(m, 1, 1, 0, 0x1e);
        dispatch::call(m, SAVE_STEP, [1, 1, 0, 0x1e])
    })
}

// ---- dispatch -------------------------------------------------------------------------------------

/// The routines of this file by JP address, while module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    Some(match target {
        0x800d_c144 => open_records(m, args),
        0x800d_c43c => draw_records(m, args),
        0x800d_cb5c => open_page(m),
        0x800d_cbe0 => step_page(m, args),
        _ => return None,
    })
}
