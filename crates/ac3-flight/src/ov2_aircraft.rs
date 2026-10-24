//! Overlay module 2 (state 3): aircraft selection, weapon selection and the
//! launch sequence, translated from `analysis/overlays/overlay_002` (loaded
//! at 0x800C9DC0). Inner states 2 and 3 of the overlay's dispatcher
//! (callback pairs at 0x800F6020 and 0x800F602C) run:
//!
//! - aircraft selection: 800DA5F4 (initialization, here) and 800DA65C
//!   (update; the sub-mode wrapper, owned by `ov2_core`). The wrapper calls
//!   the phase routine `[[0x800F6018 + phase*4] + substate*4]` (aircraft
//!   phase 0: 800D9038, 800D92B8, 800D9438, 800D9770, 800D98A0; weapon
//!   phase 1: 800D98C4, 800D9C78, 800D9DDC, 800DA2CC, 800DA440), then the
//!   record interpolation (800D7950, 800D7C54), the statistics panel
//!   (800D8CB4, [`crate::ov2_aircraft_draw`]) and the image and label
//!   slides (800D8D34, 800D7DC8), all here.
//! - launch: 800DA7AC and 800DA80C. 800DA80C returns 0x80 after 50 updates;
//!   the dispatcher (`ov2_core`) turns that into the state-2 transition.
//!
//! The 0x138-byte selection block is `[0x800F5FD4]` (field map in
//! `ac3-mission`'s `AircraftSelectionState`); the launch timer is
//! `[[0x800F6028]]`.
//!
//! Ownership follows `analysis/flight-runtime/ov2core-001/routines.tsv`:
//! this module translates the 31 routines marked `ov2air` (the routines
//! above plus 800CE978, 800D17D0, 800E7B50 and the leaves below). Overlay-2
//! routines `ov2_core` owns and resident routines are called by original
//! address through [`Calls`] (production: [`Dispatch`], i.e.
//! `dispatch::call`); the arena through [`crate::arena`]. Snapshot
//! tests pass a [`Calls`] that splices the original's effect of such a call
//! from a capture, after checking the machine state and arguments at the
//! call. Frames are pushed with their original sizes wherever a routine
//! calls out, so `Machine::sp` equals the original `$sp` at every call
//! (stack arguments of 800CE978 and 800D04C8 live at `sp+0x10..`).
use crate::{
    arena,
    common::{CONTEXT, loaded_module},
    dispatch,
    machine::{Fault, Machine, SmallData},
    ov2_aircraft_draw as draw,
};

/// 0x800F5FD4: pointer to the 0x138-byte aircraft/weapon selection block.
pub const BLOCK: u32 = 0x800f_5fd4;
/// 0x800F6028: pointer to the launch timer word.
pub const LAUNCH_TIMER: u32 = 0x800f_6028;
/// 0x800F69D0: pointer to the overlay's scene block (primitive pointer at
/// +0x20094, ordering table entry at +0x90, aircraft resources at
/// +0x200AC/+0x200B0).
pub const SCENE: u32 = 0x800f_69d0;
/// 0x800F53A0: 21 aircraft records of 0x30 bytes (halfword id at +0).
pub const AIRCRAFT_RECORDS: u32 = 0x800f_53a0;
/// 0x800F5790: 21 loadout records of 16 bytes: aircraft id word, primary
/// and secondary weapon counts (+4, +5), default choices (+6, +7), weapon
/// lists (+8, +0xC).
pub const LOADOUTS: u32 = 0x800f_5790;
/// 0x800F58E0 / 0x800F5918: primary and secondary weapon records (8 bytes).
pub const PRIMARY_WEAPONS: u32 = 0x800f_58e0;
pub const SECONDARY_WEAPONS: u32 = 0x800f_5918;
/// 0x800CB150 "%2d.%2d" and 0x800CB158 "%5d" (overlay rodata).
pub const FORMAT_HUNDREDTHS: u32 = 0x800c_b150;
pub const FORMAT_INTEGER: u32 = 0x800c_b158;

// ---- routines owned elsewhere, called by original address ----------------
/// 800D04C8 (`ov2_core`): draw a string (a0 font, a1 text, a2 OT entry, a3
/// primitive; stack +0x10 x, +0x14 y, +0x18 colour). Returns the primitive.
pub const TEXT: u32 = 0x800d_04c8;
/// 800D0C58 / 800D0C68 (`ov2_core`): common script page and start.
pub const SCRIPT_PAGE: u32 = 0x800d_0c58;
pub const SCRIPT_START: u32 = 0x800d_0c68;
/// 800D0EE4 (`ov2_core`): label visibility byte `a0` = `a1`.
pub const LABEL: u32 = 0x800d_0ee4;
/// 800D0F20 (`ov2_core`): clear all 256 label visibility bytes.
pub const CLEAR_LABELS: u32 = 0x800d_0f20;
/// 800E79D4 (`ov2_core`): background colour (a0, a1, a2).
pub const SCENE_COLOUR: u32 = 0x800e_79d4;
/// 800E7A18 (`ov2_core`): five background voice volumes (a0, a1).
pub const BACKGROUND_VOLUME: u32 = 0x800e_7a18;
/// 800E983C (`ov2_core`): background object phase 3.
pub const BACKGROUND_PHASE: u32 = 0x800e_983c;
/// 800F1770 / 800F18EC / 800F196C (`ov2_core`): visual slot style byte,
/// slot offset (x, y) and visual page.
pub const STYLE: u32 = 0x800f_1770;
pub const OFFSET: u32 = 0x800f_18ec;
pub const PAGE: u32 = 0x800f_196c;
/// 8002D724 (resident, `script`): start sound effect `a0`.
pub const SOUND: u32 = 0x8002_d724;
/// 8002DB18 (resident, `replay`): sound channel `a0` volume `a1`, rate `a2`.
pub const CHANNEL_VOLUME: u32 = 0x8002_db18;
/// 80026D00 (resident, `boot`): upload one TIM.
pub const UPLOAD_TIM: u32 = 0x8002_6d00;
/// 800AE118 (resident, `loading`): expand an "Ulz" block.
pub const DECOMPRESS: u32 = 0x800a_e118;
/// 800A6664 (resident SDK): DrawSync.
pub const DRAW_SYNC: u32 = 0x800a_6664;
/// 800A6878 (resident SDK): ClearImage2(rect, r, g, b).
pub const CLEAR_IMAGE: u32 = 0x800a_6878;
/// 0x800F1E48: pointer to the overlay's work block (+8: decompression
/// buffer of 800CE978).
pub const WORK: u32 = 0x800f_1e48;
/// 0x800F9890: pointer to the background state (+0x14 fade phase, +0x20
/// timer, +0x110..+0x120 five sound channels).
pub const BACKGROUND: u32 = 0x800f_9890;
/// 0x800F5958: 42 mission records of 12 bytes (id byte +0, category +2).
pub const MISSIONS: u32 = 0x800f_5958;

/// Calls out of this module (original `jal`/`jalr` to a routine another
/// module owns), with the original argument registers; returns `v0`.
pub trait Calls {
    fn call(&mut self, m: &mut Machine, target: u32, args: [u32; 4]) -> u32;
}

/// The production [`Calls`]: [`dispatch::call`].
pub struct Dispatch;
impl Calls for Dispatch {
    fn call(&mut self, m: &mut Machine, target: u32, args: [u32; 4]) -> u32 {
        dispatch::call(m, target, args)
    }
}

/// Signed `mult` high word.
pub fn mult_hi(a: u32, b: u32) -> u32 {
    ((i64::from(a as i32) * i64::from(b as i32)) >> 32) as u32
}

/// Compiler reciprocal division: `mult x, magic; mfhi` (plus `x` when
/// `add`), arithmetic shift, minus the sign of `x`.
pub fn div_magic(x: u32, magic: u32, shift: u32, add: bool) -> u32 {
    let mut high = mult_hi(x, magic);
    if add {
        high = high.wrapping_add(x);
    }
    (((high as i32) >> shift) as u32).wrapping_sub(((x as i32) >> 31) as u32)
}

fn block(m: &mut Machine) -> u32 {
    m.lw(BLOCK)
}

fn context(m: &mut Machine) -> u32 {
    m.gw(CONTEXT)
}

fn style(m: &mut Machine, c: &mut dyn Calls, slot: u32, value: u32) {
    c.call(m, STYLE, [slot, value, 0, 0]);
}

fn offset(m: &mut Machine, c: &mut dyn Calls, slot: u32, x: u32, y: u32) {
    c.call(m, OFFSET, [slot, x, y, 0]);
}

fn sound(m: &mut Machine, c: &mut dyn Calls, id: u32) {
    c.call(m, SOUND, [id, 0, 0, 0]);
}

pub(crate) fn set_label(m: &mut Machine, c: &mut dyn Calls, index: u32, value: u32) {
    c.call(m, LABEL, [index, value, 0, 0]);
}

pub(crate) fn clear_labels(m: &mut Machine, c: &mut dyn Calls) {
    c.call(m, CLEAR_LABELS, [0; 4]);
}

/// Copy an 8-byte record (two loads, then two stores).
fn copy_pair(m: &mut Machine, from: u32, to: u32) {
    let first = m.lw(from);
    let second = m.lw(from.wrapping_add(4));
    m.sw(to, first);
    m.sw(to.wrapping_add(4), second);
}

/// Copy one 0x30-byte record in the original's 16-byte groups (four loads,
/// then four stores).
fn copy_record(m: &mut Machine, from: u32, to: u32) {
    for group in (0..0x30).step_by(16) {
        let words: [u32; 4] = std::array::from_fn(|i| m.lw(from + group + i as u32 * 4));
        for (i, word) in words.into_iter().enumerate() {
            m.sw(to + group + i as u32 * 4, word);
        }
    }
}

// ---- record helpers ------------------------------------------------------

/// 800CE978: upload TIM `index` of the offset table `table`. With a clear
/// rectangle `rect` (non-zero) ClearImage2(rect, 0, 0, 0) runs first. An
/// "Ulz" record is expanded into a `capacity`-byte arena buffer (kept at
/// `[0x800F1E48]+8`) first. The stack argument (`sp+0x10`) 1 waits with
/// DrawSync(0); the arena pointer then returns to `[[0x800F1E48]+8]`.
/// Returns that reset's `v0`.
pub fn load_resource_800ce978(
    m: &mut Machine,
    c: &mut dyn Calls,
    table: u32,
    index: u32,
    capacity: u32,
    rect: u32,
) -> u32 {
    let sync = m.lw(m.sp.wrapping_add(0x10));
    m.push_frame(0x20);
    let mut tim = m.lw((index << 2).wrapping_add(table));
    if rect != 0 {
        c.call(m, CLEAR_IMAGE, [rect, 0, 0, 0]);
    }
    if m.lw(tim) == 0x1a7a_6c55 {
        let buffer = arena::alloc(m, capacity);
        let work = m.lw(WORK);
        m.sw(work + 8, buffer);
        c.call(m, DECOMPRESS, [tim, buffer, 0, 0]);
        let work = m.lw(WORK);
        tim = m.lw(work + 8);
    }
    c.call(m, UPLOAD_TIM, [tim, 0, 0, 0]);
    if sync == 1 {
        c.call(m, DRAW_SYNC, [0; 4]);
    }
    let work = m.lw(WORK);
    let buffer = m.lw(work + 8);
    let result = arena::reset(m, buffer);
    m.pop_frame(0x20);
    result
}

/// 800D17D0: category byte (+2) of the first of the 41 mission records at
/// 0x800F5958 whose id byte is `mission`; record 41's when none matches.
pub fn mission_category_800d17d0(m: &mut Machine, mission: u32) -> u32 {
    let mut index = 0u32;
    loop {
        let record = MISSIONS + index * 12;
        let id = m.lbu(record);
        index += 1;
        if u32::from(id) == mission {
            return u32::from(m.lbu(record + 2));
        }
        if index >= 0x29 {
            break;
        }
    }
    u32::from(m.lbu(MISSIONS + index * 12 + 2))
}

/// 800E7B50: background fade phase `phase` (state +0x14, timer +0x20 = 0).
/// Phase 1: sound 0x62, channels to volume 100 at rate 60; 2: volume 100 at
/// rate 10; 3: sound 0x63, volume 0 at rate 60 (8002DB18 on the five
/// channel indices at +0x110..+0x120).
pub fn background_fade_800e7b50(m: &mut Machine, c: &mut dyn Calls, phase: u32) {
    m.push_frame(0x18);
    let state = m.lw(BACKGROUND);
    m.sw(state + 0x14, phase);
    m.sw(state + 0x20, 0);
    let fade = match phase {
        1 => Some((Some(0x62), 100, 60)),
        2 => Some((None, 100, 10)),
        3 => Some((Some(0x63), 0, 60)),
        _ => None,
    };
    if let Some((effect, volume, rate)) = fade {
        if let Some(id) = effect {
            sound(m, c, id);
        }
        for channel in (0x110..=0x120).step_by(4) {
            let state = m.lw(BACKGROUND);
            let index = m.lw(state + channel);
            c.call(m, CHANNEL_VOLUME, [index, volume, rate, 0]);
        }
    }
    m.pop_frame(0x18);
}

/// 800D77E4: rebuild the 24 availability bytes at `[block]+0xBC` from the
/// context's saved (`+0x978`, option 0x10) or current (`+0x9B8`) table.
/// Mission categories 12/19 allow only aircraft 17, category 16 only 18;
/// otherwise 17 and 18 are excluded. If the context's aircraft (+0x392) is
/// unavailable it becomes the first available one.
pub fn rebuild_availability_800d77e4(m: &mut Machine) {
    m.push_frame(0x20);
    let ctx = context(m);
    let b = block(m);
    let mission = m.lh(ctx + 0x390) as i32 as u32;
    let available = b + 0xbc;
    let category = mission_category_800d17d0(m, mission);
    let ctx = context(m);
    let options = m.lw(ctx + 0x970);
    let source = if options & 0x10 != 0 {
        ctx + 0x978
    } else {
        ctx + 0x9b8
    };
    for i in (0..0x18).rev() {
        m.sb(available + i, 0);
    }
    match category & 0xff {
        0xc | 0x13 => {
            m.sb(available + 0x11, 1);
            let ctx = context(m);
            m.sh(ctx + 0x392, 0x11);
        }
        0x10 => {
            m.sb(available + 0x12, 1);
            let ctx = context(m);
            m.sh(ctx + 0x392, 0x12);
        }
        _ => {
            for i in 0..0x18 {
                if i == 0x11 || i == 0x12 {
                    continue;
                }
                let value = m.lbu(source + i);
                if value == 1 {
                    m.sb(available + i, value);
                }
            }
            let ctx = context(m);
            let current = m.lh(ctx + 0x392) as i32 as u32;
            if m.lbu(available.wrapping_add(current)) == 0 {
                for i in 0..0x18u32 {
                    if m.lbu(available + i) == 1 {
                        let ctx = context(m);
                        m.sh(ctx + 0x392, i as u16);
                        break;
                    }
                }
            }
        }
    }
    m.pop_frame(0x20);
}

/// One interpolated statistic: `from` + (`to` - `from`) * `t` / `steps`
/// (wrapping product, reciprocal division).
fn lerp(from: u32, to: u32, t: u32, magic: u32, shift: u32) -> u32 {
    let product = to.wrapping_sub(from).wrapping_mul(t);
    from.wrapping_add(div_magic(product, magic, shift, false))
}

/// 800D7950: while block flag 1 is set, the current aircraft record
/// (+0x50) moves from the previous (+0x20) to the next (+0x80) over 50
/// updates: id and bytes +0x20..+0x22 jump, five halfwords and five words
/// interpolate with timer +0xB0. The timer then resets and the flag clears.
pub fn interpolate_aircraft_800d7950(m: &mut Machine) {
    let b = block(m);
    if m.lw(b + 8) & 1 == 0 {
        return;
    }
    let previous = b + 0x20;
    let next = b + 0x80;
    let current = b + 0x50;
    let id = m.lhu(next);
    m.sh(current, id);
    let t = m.lw(b + 0xb0);
    for at in (2..0xc).step_by(2) {
        let to = m.lh(next + at) as i32 as u32;
        let from = m.lh(previous + at) as i32 as u32;
        let step = div_magic(to.wrapping_sub(from).wrapping_mul(t), 0x51eb_851f, 4, false);
        let base = u32::from(m.lhu(previous + at));
        m.sh(current + at, base.wrapping_add(step) as u16);
    }
    for at in (0xc..0x20).step_by(4) {
        let to = m.lw(next + at);
        let from = m.lw(previous + at);
        m.sw(current + at, lerp(from, to, t, 0x51eb_851f, 4));
    }
    for at in 0x20..0x23 {
        let byte = m.lbu(next + at);
        m.sb(current + at, byte);
    }
    if (t as i32) < 50 {
        let b = block(m);
        let timer = m.lw(b + 0xb0);
        m.sw(b + 0xb0, timer.wrapping_add(1));
    } else {
        let b = block(m);
        let flags = m.lw(b + 8);
        m.sw(b + 0xb0, 0);
        m.sw(b + 8, flags & !1);
    }
}

/// 800D7C54: per weapon row (flags +0x128 bit 0), the current record
/// (+0x100) moves from the previous (+0xF0) to the next (+0x110) over 10
/// updates (timer +0x120): word +0 and byte +7 jump, bytes +4..+6
/// interpolate.
pub fn interpolate_weapons_800d7c54(m: &mut Machine) {
    for row in 0..2u32 {
        let b = block(m);
        let state = b + row * 4;
        if m.lw(state + 0x128) & 1 == 0 {
            continue;
        }
        let next = b + 0x110 + row * 8;
        let current = b + 0x100 + row * 8;
        let previous = b + 0xf0 + row * 8;
        let t = m.lw(state + 0x120);
        let id = m.lw(next);
        m.sw(current, id);
        for at in 4..7 {
            let to = u32::from(m.lbu(next + at));
            let from = u32::from(m.lbu(previous + at));
            let value = lerp(from, to, t, 0x6666_6667, 2);
            m.sb(current + at, value as u8);
        }
        let byte = m.lbu(next + 7);
        m.sb(current + 7, byte);
        if (t as i32) < 10 {
            let state = block(m) + row * 4;
            let timer = m.lw(state + 0x120);
            m.sw(state + 0x120, timer.wrapping_add(1));
        } else {
            let state = block(m) + row * 4;
            let flags = m.lw(state + 0x128);
            m.sw(state + 0x120, 0);
            m.sw(state + 0x128, flags & !1);
        }
    }
}

/// 800D7DC8: weapon labels. Hide label slots 9..15 and 16..23; per row,
/// advance the slide timer (+0x130, flags +0x128 bits 2/4: slide right or
/// left over 10 updates), then show the previous and next labels offset by
/// +-5 per tick (or the current label when idle). In weapon selection
/// (block flag 0x80000) the selected row's current weapon label is shown
/// and visual slot 7 takes its record's byte +7.
pub fn slide_weapons_800d7dc8(m: &mut Machine, c: &mut dyn Calls) {
    m.push_frame(0x30);
    for i in 0..7 {
        style(m, c, 9 + i, 0);
    }
    for i in 0..8 {
        style(m, c, 0x10 + i, 0);
    }
    for row in 0..2u32 {
        let records = row * 8;
        let state = block(m) + row * 4;
        if m.lw(state + 0x128) & 6 != 0 {
            let timer = m.lw(state + 0x130);
            if (timer as i32) < 10 {
                m.sw(state + 0x130, timer.wrapping_add(1));
            } else {
                let flags = m.lw(state + 0x128);
                m.sw(state + 0x130, 0);
                m.sw(state + 0x128, flags & !6);
            }
        }
        let b = block(m);
        let state = b + row * 4;
        let timer = m.lw(state + 0x130);
        let flags = m.lw(state + 0x128);
        let shift = (timer << 2).wrapping_add(timer);
        let base = if row == 0 { 9 } else { 0x10 };
        if flags & 6 != 0 {
            let id = m.lw(b + records + 0xf0);
            style(m, c, id.wrapping_add(base), 1);
            let b = block(m);
            let id = m.lw(b + records + 0x110);
            style(m, c, id.wrapping_add(base), 1);
            if flags & 2 != 0 {
                let b = block(m);
                let id = m.lw(b + records + 0xf0);
                offset(m, c, id.wrapping_add(base), shift, 0);
                let b = block(m);
                let id = m.lw(b + records + 0x110);
                offset(m, c, id.wrapping_add(base), shift.wrapping_sub(50), 0);
            } else if flags & 4 != 0 {
                let left = shift.wrapping_neg();
                let b = block(m);
                let id = m.lw(b + records + 0xf0);
                offset(m, c, id.wrapping_add(base), left, 0);
                let b = block(m);
                let id = m.lw(b + records + 0x110);
                offset(m, c, id.wrapping_add(base), left.wrapping_add(50), 0);
            }
        } else {
            let id = m.lw(b + records + 0x100);
            style(m, c, id.wrapping_add(base), 1);
        }
    }
    let b = block(m);
    if m.lw(b + 8) & 0x8_0000 != 0 {
        clear_labels(m, c);
        let b = block(m);
        let row = m.lw(b + 0xd8);
        if row == 0 {
            let id = m.lw(b + 0x100);
            set_label(m, c, id.wrapping_add(0xaa), 1);
        } else if row == 1 {
            let id = m.lw(b + 0x108);
            set_label(m, c, id.wrapping_add(0xb1), 1);
        }
        let b = block(m);
        let row = m.lw(b + 0xd8);
        let value = m.lbu(b.wrapping_add(row << 3).wrapping_add(0x107));
        style(m, c, 7, u32::from(value));
    }
    m.pop_frame(0x30);
}

/// 800D804C: aircraft record statistics halfwords +2..+0xA times 16.
pub fn scale_aircraft_record_800d804c(m: &mut Machine, record: u32) {
    let a = m.lh(record + 2) as u16;
    let b = m.lh(record + 6) as u16;
    m.sh(record + 2, a << 4);
    let c = m.lh(record + 4) as u16;
    m.sh(record + 6, b << 4);
    let d = m.lh(record + 0xa) as u16;
    m.sh(record + 4, c << 4);
    let e = m.lh(record + 8) as u16;
    m.sh(record + 0xa, d << 4);
    m.sh(record + 8, e << 4);
}

/// 800D808C: weapon record statistics bytes +4..+6 times 16 (truncated).
pub fn scale_weapon_record_800d808c(m: &mut Machine, record: u32) {
    let a = m.lbu(record + 4);
    let b = m.lbu(record + 6);
    m.sb(record + 4, a << 4);
    let c = m.lbu(record + 5);
    m.sb(record + 6, b << 4);
    m.sb(record + 5, c << 4);
}

/// 800D80B4: weapon arrows. Block flags 0x100/0x200 (row 0) and
/// 0x400/0x800 (row 1) mark an available weapon left/right of the choices
/// `first`/`second`; slots 1..4 show them, slot 5 the selected row, slot 6
/// is set. Pending arrow flashes (0x1000..0x8000, 0x10) show for 10
/// updates (timer +0x18).
pub fn weapon_arrows_800d80b4(m: &mut Machine, c: &mut dyn Calls, first: u32, second: u32) {
    m.push_frame(0x18);
    let b = block(m);
    let primary = b + 0xdc;
    let secondary = b + 0xe4;
    let flags = m.lw(b + 8);
    m.sw(b + 8, flags & 0xffff_f0ff);
    let mut i = first.wrapping_add(1);
    while (i as i32) < 8 {
        if m.lbu(primary.wrapping_add(i)) == 1 {
            let flags = m.lw(b + 8);
            m.sw(b + 8, flags | 0x200);
        }
        i = i.wrapping_add(1);
    }
    let mut i = first.wrapping_sub(1);
    if (i as i32) >= 0 {
        let b = block(m);
        while (i as i32) >= 0 {
            if m.lbu(primary.wrapping_add(i)) == 1 {
                let flags = m.lw(b + 8);
                m.sw(b + 8, flags | 0x100);
            }
            i = i.wrapping_sub(1);
        }
    }
    let mut i = second.wrapping_add(1);
    if (i as i32) < 8 {
        let b = block(m);
        while (i as i32) < 8 {
            if m.lbu(secondary.wrapping_add(i)) == 1 {
                let flags = m.lw(b + 8);
                m.sw(b + 8, flags | 0x800);
            }
            i = i.wrapping_add(1);
        }
    }
    let mut i = second.wrapping_sub(1);
    if (i as i32) >= 0 {
        let b = block(m);
        while (i as i32) >= 0 {
            if m.lbu(secondary.wrapping_add(i)) == 1 {
                let flags = m.lw(b + 8);
                m.sw(b + 8, flags | 0x400);
            }
            i = i.wrapping_sub(1);
        }
    }
    for (slot, bit) in [(1, 0x100), (2, 0x200), (3, 0x400), (4, 0x800)] {
        let b = block(m);
        let shown = if m.lw(b + 8) & bit != 0 { 2 } else { 0 };
        style(m, c, slot, shown);
    }
    let b = block(m);
    let row = m.lw(b + 0xd8);
    style(m, c, 5, row.wrapping_add(1));
    style(m, c, 6, 2);
    let b = block(m);
    let flags = m.lw(b + 8);
    if flags & 0xf010 != 0 {
        if flags & 0x1000 != 0 {
            style(m, c, 1, 1);
        }
        for (slot, bit) in [(2, 0x2000), (3, 0x4000), (4, 0x8000), (6, 0x10)] {
            let b = block(m);
            if m.lw(b + 8) & bit != 0 {
                style(m, c, slot, 1);
            }
        }
        let b = block(m);
        let timer = m.lw(b + 0x18);
        if (timer as i32) < 10 {
            m.sw(b + 0x18, timer.wrapping_add(1));
        } else {
            let flags = m.lw(b + 8);
            m.sw(b + 0x18, 0);
            m.sw(b + 8, flags & 0xffff_0fef);
        }
    }
    m.pop_frame(0x18);
}

/// 800D8CFC: start an aircraft image slide-out (block flag 0x10000) unless
/// one is running; clears the slide-in flag 0x20000.
pub fn begin_aircraft_slide_800d8cfc(m: &mut Machine) {
    let b = block(m);
    let flags = m.lw(b + 8);
    if flags & 0x1_0000 == 0 {
        m.sw(b + 8, (flags & 0xfffd_ffff) | 0x1_0000);
    }
}

/// The two 800CE978 loads of aircraft `[block]+0x1C`'s image (scene
/// +0x200AC, capacity 0x3820, clear rectangle `[block]+0xB4`) and label
/// (+0x200B0, 0x828), then slots 0x1F/0x20 take the record's bytes +0x21
/// and +0x20. `frame` is the caller's frame (stack argument at +0x10).
fn load_aircraft_resources(m: &mut Machine, c: &mut dyn Calls, frame: u32, b: u32) {
    let scene = m.lw(SCENE);
    m.sw(frame + 0x10, 1);
    let index = m.lw(b + 0x1c);
    let table = m.lw(scene + 0x2_00ac);
    load_resource_800ce978(m, c, table, index, 0x3820, b + 0xb4);
    let scene = m.lw(SCENE);
    let current = block(m);
    m.sw(frame + 0x10, 1);
    let table = m.lw(scene + 0x2_00b0);
    let index = m.lw(current + 0x1c);
    load_resource_800ce978(m, c, table, index, 0x828, 0);
    let record = b + 0x50;
    let value = m.lbu(record + 0x21);
    style(m, c, 0x1f, u32::from(value));
    let value = m.lbu(record + 0x20);
    style(m, c, 0x20, u32::from(value));
}

/// 800D8D34: aircraft image slide. Flag 0x10000 counts timer +0xD4 up to
/// 25, then loads the new aircraft's resources and switches to slide-in
/// (0x20000), which counts back to 0. Slot 5 sits at y = -t*t + panel.
pub fn slide_aircraft_resources_800d8d34(m: &mut Machine, c: &mut dyn Calls) {
    let frame = m.push_frame(0x30);
    let b = block(m);
    let flags = m.lw(b + 8);
    if flags & 0x1_0000 != 0 {
        let timer = m.lw(b + 0xd4);
        if (timer as i32) < 25 {
            m.sw(b + 0xd4, timer.wrapping_add(1));
        } else {
            load_aircraft_resources(m, c, frame, b);
            let b = block(m);
            let flags = m.lw(b + 8);
            m.sw(b + 8, (flags & 0xfffe_ffff) | 0x2_0000);
        }
    } else if flags & 0x2_0000 != 0 {
        let timer = m.lw(b + 0xd4);
        if (timer as i32) > 0 {
            m.sw(b + 0xd4, timer.wrapping_sub(1));
        } else {
            m.sw(b + 8, flags & 0xfffd_ffff);
        }
    }
    let b = block(m);
    let timer = m.lw(b + 0xd4);
    let y = timer.wrapping_neg().wrapping_mul(timer);
    let panel = m.lh(b + 0x14) as i32 as u32;
    offset(m, c, 5, y.wrapping_add(panel), 0);
    m.pop_frame(0x30);
}

/// 800D8E9C: aircraft arrows. Slots 1/2 show 2 when an available aircraft
/// lies left/right of `selection` (one call per available one), slot 3 is
/// 2; pending flashes (block flags 4/8/0x10) show for 25 updates (timer
/// +0x18).
pub fn aircraft_arrows_800d8e9c(m: &mut Machine, c: &mut dyn Calls, selection: u32) {
    m.push_frame(0x28);
    style(m, c, 1, 3);
    style(m, c, 2, 3);
    style(m, c, 3, 2);
    let mut i = selection.wrapping_add(1);
    while (i as i32) < 0x16 {
        let b = block(m);
        if m.lbu(b.wrapping_add(i).wrapping_add(0xbc)) == 1 {
            style(m, c, 2, 2);
        }
        i = i.wrapping_add(1);
    }
    let mut i = selection.wrapping_sub(1);
    while (i as i32) >= 0 {
        let b = block(m);
        if m.lbu(b.wrapping_add(i).wrapping_add(0xbc)) == 1 {
            style(m, c, 1, 2);
        }
        i = i.wrapping_sub(1);
    }
    let b = block(m);
    if m.lw(b + 8) & 0x1c != 0 {
        for (slot, bit) in [(1, 4), (2, 8), (3, 0x10)] {
            let b = block(m);
            if m.lw(b + 8) & bit != 0 {
                style(m, c, slot, 1);
            }
        }
        let b = block(m);
        let timer = m.lw(b + 0x18);
        if (timer as i32) < 25 {
            m.sw(b + 0x18, timer.wrapping_add(1));
        } else {
            let flags = m.lw(b + 8);
            m.sw(b + 0x18, 0);
            m.sw(b + 8, flags & !0x1c);
        }
    }
    m.pop_frame(0x28);
}

// ---- aircraft phase ------------------------------------------------------

/// Panel offsets of slots 2..4 (`slot`, x, y) for the reveal parameter
/// `square` = t*t and `negative` = -t*t.
fn side_panels(m: &mut Machine, c: &mut dyn Calls, square: u32, negative: u32) {
    offset(m, c, 2, 0, negative);
    offset(m, c, 3, square, 0);
    offset(m, c, 4, negative, 0);
}

/// 800D9038: aircraft phase entry. Page 4, panels off-screen (timer +0xC =
/// 25), sound 0x5A, availability, the context aircraft's record (scaled)
/// and resources, arrows. Returns 1.
pub fn aircraft_phase_init_800d9038(m: &mut Machine, c: &mut dyn Calls) -> u32 {
    let frame = m.push_frame(0x30);
    let b = block(m);
    let flags = m.lw(b + 8);
    m.sw(b + 0xb0, 0);
    m.sw(b + 8, flags & 0xfff7_ffff);
    c.call(m, PAGE, [4, 0, 0, 0]);
    let b = block(m);
    m.sw(b + 0xc, 25);
    offset(m, c, 6, (-0x271i32) as u32, 0);
    offset(m, c, 7, 0x271, 0);
    let b = block(m);
    m.sh(b + 0x10, 0x271);
    m.sh(b + 0x16, 0x271);
    if m.lw(b + 8) & 0x4_0000 != 0 {
        side_panels(m, c, 0x271, (-0x271i32) as u32);
        let b = block(m);
        m.sh(b + 0x12, (-0x271i32) as u16);
        m.sh(b + 0x14, (-0x271i32) as u16);
    } else {
        side_panels(m, c, 0, 0);
        let b = block(m);
        m.sh(b + 0x12, 0);
        m.sh(b + 0x14, 0);
    }
    sound(m, c, 0x5a);
    rebuild_availability_800d77e4(m);
    let b = block(m);
    m.sh(b + 0xb4, 0x3c0);
    m.sh(b + 0xb6, 0x100);
    m.sh(b + 0xb8, 0x20);
    m.sh(b + 0xba, 0x90);
    let mut record = AIRCRAFT_RECORDS;
    for index in 0..0x15u32 {
        let ctx = context(m);
        let id = m.lh(record);
        if id == m.lh(ctx + 0x392) {
            let b = block(m);
            m.sw(b + 0x1c, index);
            copy_record(m, record, b + 0x50);
            let b = block(m);
            scale_aircraft_record_800d804c(m, b + 0x50);
            let b = block(m);
            load_aircraft_resources(m, c, frame, b);
            break;
        }
        record += 0x30;
    }
    let b = block(m);
    let selection = m.lh(b + 0x50) as i32 as u32;
    aircraft_arrows_800d8e9c(m, c, selection);
    m.pop_frame(0x30);
    1
}

/// 800D92B8: aircraft panels slide in while timer +0xC counts down
/// (offsets +-t*t). Returns 1 when done (flag 0x1000000 set, 0x40000
/// cleared).
pub fn aircraft_intro_800d92b8(m: &mut Machine, c: &mut dyn Calls) -> u32 {
    m.push_frame(0x20);
    let b = block(m);
    let t = m.lw(b + 0xc);
    let result = if (t as i32) > 0 {
        let negative = t.wrapping_neg().wrapping_mul(t);
        let square = t.wrapping_mul(t);
        offset(m, c, 6, negative, 0);
        offset(m, c, 7, square, 0);
        let b = block(m);
        m.sh(b + 0x10, square as u16);
        if m.lw(b + 8) & 0x4_0000 != 0 {
            side_panels(m, c, square, negative);
            let b = block(m);
            m.sh(b + 0x12, negative as u16);
            m.sh(b + 0x14, negative as u16);
        }
        let b = block(m);
        let t = m.lw(b + 0xc);
        m.sw(b + 0xc, t.wrapping_sub(1));
        0
    } else {
        for slot in [6, 7, 2, 3, 4] {
            offset(m, c, slot, 0, 0);
        }
        let b = block(m);
        let flags = m.lw(b + 8);
        m.sh(b + 0x10, 0);
        m.sh(b + 0x12, 0);
        m.sh(b + 0x14, 0);
        m.sw(b + 0xc, 0);
        m.sw(b + 8, (flags & 0xfffb_ffff) | 0x100_0000);
        1
    };
    m.pop_frame(0x20);
    result
}

/// The aircraft-input step to `index`: sound 0x3D, selection, slide-out,
/// arrow flash `flash`, current record -> previous, record `index` ->
/// next (scaled), interpolation start (flag 1, timer +0xB0 = 0).
fn step_aircraft(m: &mut Machine, c: &mut dyn Calls, index: u32, flash: u32) {
    sound(m, c, 0x3d);
    let b = block(m);
    m.sw(b + 0x1c, index);
    begin_aircraft_slide_800d8cfc(m);
    let b = block(m);
    let flags = m.lw(b + 8);
    m.sw(b + 0x18, 0);
    m.sw(b + 8, (flags & !0x1c) | flash);
    copy_record(m, b + 0x50, b + 0x20);
    let b = block(m);
    let index = m.lw(b + 0x1c);
    let record = (index << 1).wrapping_add(index) << 4;
    copy_record(m, record.wrapping_add(AIRCRAFT_RECORDS), b + 0x80);
    let b = block(m);
    scale_aircraft_record_800d804c(m, b + 0x80);
    let b = block(m);
    let flags = m.lw(b + 8);
    m.sw(b + 0xb0, 0);
    m.sw(b + 8, flags | 1);
}

/// 800D9438: aircraft input (context +0x12C). Confirm (0x10) and cancel
/// (0x20) set the exit flags, play 0x3C/0x3B then 0x5B and return 1.
/// Right (8) / left (4) step to the next available record without
/// wrapping. Otherwise the arrows update and it returns 0.
pub fn aircraft_input_800d9438(m: &mut Machine, c: &mut dyn Calls) -> u32 {
    m.push_frame(0x20);
    let ctx = context(m);
    let b = block(m);
    let pressed = m.lw(ctx + 0x12c);
    let available = b + 0xbc;
    if pressed & 0x30 != 0 {
        let first = if pressed & 0x10 != 0 {
            let flags = m.lw(b + 8);
            m.sw(b + 0x18, 0);
            m.sw(b + 8, (flags & !0x1c) | 0x10);
            0x3c
        } else {
            let flags = m.lw(b + 8);
            m.sw(b + 8, (flags | 0x4_0000) & 0xfeff_ffff);
            0x3b
        };
        sound(m, c, first);
        sound(m, c, 0x5b);
        m.pop_frame(0x20);
        return 1;
    }
    if pressed & 8 != 0 {
        let mut index = m.lw(b + 0x1c).wrapping_add(1);
        while (index as i32) < 0x15 {
            let record = ((index << 1).wrapping_add(index) << 4).wrapping_add(AIRCRAFT_RECORDS);
            let id = m.lh(record) as i32 as u32;
            if m.lbu(available.wrapping_add(id)) != 0 {
                step_aircraft(m, c, index, 8);
                break;
            }
            index = index.wrapping_add(1);
        }
    } else if pressed & 4 != 0 {
        let mut index = m.lw(b + 0x1c).wrapping_sub(1);
        while (index as i32) >= 0 {
            let record = ((index << 1).wrapping_add(index) << 4).wrapping_add(AIRCRAFT_RECORDS);
            let id = m.lh(record) as i32 as u32;
            if m.lbu(available.wrapping_add(id)) != 0 {
                step_aircraft(m, c, index, 4);
                break;
            }
            index = index.wrapping_sub(1);
        }
    }
    let b = block(m);
    let selection = m.lh(b + 0x50) as i32 as u32;
    aircraft_arrows_800d8e9c(m, c, selection);
    m.pop_frame(0x20);
    0
}

/// 800D9770: aircraft panels slide out over 25 updates (timer +0xC up).
/// Returns 1 (accept: on to commit) or 0x10 (cancel: flag 0x40000).
pub fn aircraft_exit_800d9770(m: &mut Machine, c: &mut dyn Calls) -> u32 {
    m.push_frame(0x20);
    let b = block(m);
    let selection = m.lh(b + 0x50) as i32 as u32;
    let t = m.lw(b + 0xc);
    aircraft_arrows_800d8e9c(m, c, selection);
    let b = block(m);
    let result = if (m.lw(b + 0xc) as i32) < 25 {
        let negative = t.wrapping_neg().wrapping_mul(t);
        let square = t.wrapping_mul(t);
        offset(m, c, 6, negative, 0);
        offset(m, c, 7, square, 0);
        let b = block(m);
        m.sh(b + 0x10, square as u16);
        if m.lw(b + 8) & 0x4_0000 != 0 {
            side_panels(m, c, square, negative);
            let b = block(m);
            m.sh(b + 0x12, negative as u16);
            m.sh(b + 0x14, negative as u16);
        }
        let b = block(m);
        let t = m.lw(b + 0xc);
        m.sw(b + 0xc, t.wrapping_add(1));
        0
    } else if m.lw(b + 8) & 0x4_0000 != 0 {
        0x10
    } else {
        1
    };
    m.pop_frame(0x20);
    result
}

/// 800D98A0: commit the selected aircraft id to context +0x392. Returns 2.
pub fn commit_aircraft_800d98a0(m: &mut Machine) -> u32 {
    let b = block(m);
    let ctx = context(m);
    let id = m.lhu(b + 0x50);
    m.sh(ctx + 0x392, id);
    2
}

// ---- weapon phase --------------------------------------------------------

/// Offsets of the weapon label slots 9..15 and 16..23.
fn weapon_label_offsets(m: &mut Machine, c: &mut dyn Calls, first: u32, second: u32) {
    for i in 0..7 {
        offset(m, c, 9 + i, first, 0);
    }
    for i in 0..8 {
        offset(m, c, 0x10 + i, second, 0);
    }
}

/// 800D98C4: weapon phase entry. Page 5, panels off-screen, the loadout of
/// the context aircraft (availability rows +0xDC/+0xE4, default choices
/// +0xEC/+0xEE; category 0x12 forces secondary 7 / primary 6), the two
/// current weapon records (scaled), special unlocks (+0xA03/+0xA04 under
/// option 0x10), arrows and labels, sound 0x5A. Returns 1.
pub fn weapon_phase_init_800d98c4(m: &mut Machine, c: &mut dyn Calls) -> u32 {
    m.push_frame(0x38);
    let ctx = context(m);
    let b = block(m);
    let primary = b + 0xdc;
    let secondary = b + 0xe4;
    let mission = m.lh(ctx + 0x390) as i32 as u32;
    let category = mission_category_800d17d0(m, mission);
    let b = block(m);
    let flags = m.lw(b + 8);
    m.sw(b + 8, flags | 0x8_0000);
    c.call(m, PAGE, [5, 0, 0, 0]);
    let b = block(m);
    m.sw(b + 0xc, 25);
    weapon_label_offsets(m, c, 0x271, 25 * 25);
    let b = block(m);
    m.sw(b + 0xd8, 0);
    for i in 0..8 {
        m.sb(secondary + i, 0);
        m.sb(primary + i, 0);
    }
    let ctx = context(m);
    let aircraft = m.lh(ctx + 0x392) as i32 as u32;
    let mut loadout = None;
    let mut record = LOADOUTS;
    for _ in 0..0x15 {
        if m.lw(record) == aircraft {
            loadout = Some(record);
            break;
        }
        record += 0x10;
    }
    if category & 0xff == 0x12 {
        m.sb(primary + 6, 1);
        m.sb(secondary + 7, 1);
        let b = block(m);
        m.sh(b + 0xec, 6);
        m.sh(b + 0xee, 7);
    } else {
        let Some(loadout) = loadout else {
            // The original reads through an uninitialized `s2` here.
            m.raise(Fault::Untranslated {
                address: 0x800d_9a2c,
            });
            m.pop_frame(0x38);
            return 0;
        };
        let mut i = 0u32;
        while i < u32::from(m.lbu(loadout + 4)) {
            let weapon = m.lbu(loadout + 8 + i);
            m.sb(primary + u32::from(weapon), 1);
            i += 1;
        }
        let mut i = 0u32;
        while i < u32::from(m.lbu(loadout + 5)) {
            let weapon = m.lbu(loadout + 0xc + i);
            m.sb(secondary + u32::from(weapon), 1);
            i += 1;
        }
        let b = block(m);
        let first = m.lbu(loadout + 6);
        m.sh(b + 0xec, u16::from(first));
        let second = m.lbu(loadout + 7);
        m.sh(b + 0xee, u16::from(second));
    }
    for (choice, table, record) in [
        (0xec, PRIMARY_WEAPONS, 0x100),
        (0xee, SECONDARY_WEAPONS, 0x108),
    ] {
        let b = block(m);
        let index = m.lh(b + choice) as i32 as u32;
        copy_pair(m, (index << 3).wrapping_add(table), b + record);
    }
    for row in 0..2u32 {
        let b = block(m);
        scale_weapon_record_800d808c(m, b + 0x100 + row * 8);
        let state = block(m) + row * 4;
        let flags = m.lw(state + 0x128);
        m.sw(state + 0x120, 0);
        m.sw(state + 0x128, flags & !1);
    }
    let category = category & 0xff;
    if category != 0x10 && category != 0x12 {
        let ctx = context(m);
        if m.lw(ctx + 0x970) & 0x10 != 0 {
            if category.wrapping_sub(0x1f) & 0xff >= 3 && m.lbu(ctx + 0xa03) != 0 {
                m.sb(secondary + 5, 1);
            }
            let ctx = context(m);
            if m.lw(ctx + 0x970) & 0x10 != 0 && m.lbu(ctx + 0xa04) != 0 {
                m.sb(secondary + 6, 1);
            }
        }
    }
    let b = block(m);
    let first = m.lw(b + 0x100);
    let second = m.lw(b + 0x108);
    weapon_arrows_800d80b4(m, c, first, second);
    clear_labels(m, c);
    let b = block(m);
    let id = m.lw(b + 0x100);
    set_label(m, c, id.wrapping_add(0xaa), 1);
    let b = block(m);
    let id = m.lw(b + 0x108);
    set_label(m, c, id.wrapping_add(0xb1), 1);
    sound(m, c, 0x5a);
    m.pop_frame(0x38);
    1
}

/// 800D9C78: weapon panels slide in while timer +0xC counts down (panel
/// +0x16 = t*t). Returns 1 when done.
pub fn weapon_intro_800d9c78(m: &mut Machine, c: &mut dyn Calls) -> u32 {
    m.push_frame(0x20);
    let b = block(m);
    let t = m.lw(b + 0xc);
    let result = if (t as i32) > 0 {
        let square = t.wrapping_mul(t);
        let negative = t.wrapping_neg().wrapping_mul(t);
        m.sh(b + 0x16, square as u16);
        offset(m, c, 6, negative, 0);
        offset(m, c, 7, square, 0);
        weapon_label_offsets(m, c, square, t.wrapping_mul(t));
        let b = block(m);
        let t = m.lw(b + 0xc);
        m.sw(b + 0xc, t.wrapping_sub(1));
        0
    } else {
        m.sw(b + 0xc, 0);
        m.sh(b + 0x16, 0);
        offset(m, c, 6, 0, 0);
        offset(m, c, 7, 0, 0);
        weapon_label_offsets(m, c, 0, 0);
        1
    };
    m.pop_frame(0x20);
    result
}

/// The selected weapon row of 800D9DDC: index, availability bytes,
/// choice halfword and slide timer addresses (from the block and row at
/// entry).
#[derive(Clone, Copy)]
struct WeaponRow {
    row: u32,
    available: u32,
    choice: u32,
    timer: u32,
}

/// One weapon-input step of the selected row to choice `index`, with
/// slide flag `toward` (4 right, 2 left) and arrow flash bits `flashes`
/// (row 0, row 1). Returns false when a slide toward the same side is
/// already running (the original stops scanning there).
fn step_weapon(
    m: &mut Machine,
    c: &mut dyn Calls,
    row: &WeaponRow,
    index: u32,
    toward: u32,
    flashes: (u32, u32),
) -> bool {
    let WeaponRow {
        row, choice, timer, ..
    } = *row;
    sound(m, c, 0x3d);
    let state = block(m).wrapping_add(row << 2);
    let flags = m.lw(state + 0x128);
    if flags & toward != 0 {
        return false;
    }
    let away = toward ^ 6;
    if flags & away != 0 {
        m.sw(state + 0x128, (flags & !away) | toward);
        let t = m.lw(timer);
        m.sw(timer, 10u32.wrapping_sub(t));
    } else {
        m.sw(state + 0x128, flags | toward);
        m.sw(timer, 0);
    }
    m.sh(choice, index as u16);
    let b = block(m);
    let flags = m.lw(b + 8) & 0xffff_0fef;
    let selected = m.lw(b + 0xd8);
    m.sw(b + 8, flags);
    if selected == 0 {
        m.sw(b + 8, flags | flashes.0);
    } else if selected == 1 {
        m.sw(b + 8, flags | flashes.1);
    }
    let b = block(m);
    let selected = m.lw(b + 0xd8);
    m.sw(b + 0x18, 0);
    let records = b.wrapping_add(selected << 3);
    copy_pair(m, records + 0x100, records + 0xf0);
    let selected = m.lw(b + 0xd8);
    if selected == 0 {
        let index = m.lh(choice) as i32 as u32;
        copy_pair(m, (index << 3).wrapping_add(PRIMARY_WEAPONS), b + 0x110);
    } else if selected == 1 {
        let index = m.lh(choice) as i32 as u32;
        copy_pair(m, (index << 3).wrapping_add(SECONDARY_WEAPONS), b + 0x118);
    }
    let b = block(m);
    let selected = m.lw(b + 0xd8);
    scale_weapon_record_800d808c(m, b.wrapping_add((selected << 3).wrapping_add(0x110)));
    let b = block(m);
    let selected = m.lw(b + 0xd8);
    m.sw(b.wrapping_add(selected << 2) + 0x120, 0);
    let selected = m.lw(b + 0xd8);
    let state = b.wrapping_add(selected << 2);
    let flags = m.lw(state + 0x128);
    m.sw(state + 0x128, flags | 1);
    true
}

/// 800D9DDC: weapon input (context +0x12C). Cancel (0x20) plays 0x3B and
/// confirm (0x10, exit flags) 0x3C, then 0x5B, returning 1. Vertical (3)
/// toggles the row (sound 0x3D). Right (8) / left (4) scan for available
/// weapons of the selected row; every available one plays 0x3D, and the
/// scan stops at a running slide toward that side. Then the arrows update
/// and it returns 0.
pub fn weapon_input_800d9ddc(m: &mut Machine, c: &mut dyn Calls) -> u32 {
    m.push_frame(0x38);
    let b = block(m);
    let row = m.lw(b + 0xd8);
    let selected = WeaponRow {
        row,
        available: b.wrapping_add((row << 3).wrapping_add(0xdc)),
        choice: b.wrapping_add((row << 1).wrapping_add(0xec)),
        timer: b.wrapping_add((row << 2).wrapping_add(0x130)),
    };
    let WeaponRow {
        available, choice, ..
    } = selected;
    let ctx = context(m);
    let pressed = m.lw(ctx + 0x12c);
    if pressed & 0x30 != 0 {
        let first = if pressed & 0x20 != 0 {
            0x3b
        } else {
            let flags = m.lw(b + 8);
            m.sw(b + 0x18, 0);
            m.sw(b + 8, ((flags & 0xffff_0fef) | 0x4_0010) & 0xfeff_ffff);
            0x3c
        };
        sound(m, c, first);
        sound(m, c, 0x5b);
        m.pop_frame(0x38);
        return 1;
    }
    if pressed & 3 != 0 {
        m.sw(b + 0xd8, row ^ 1);
        sound(m, c, 0x3d);
    } else if pressed & 8 != 0 {
        let mut index = (m.lh(choice) as i32 as u32).wrapping_add(1);
        while (index as i32) < 8 {
            if m.lbu(available.wrapping_add(index)) == 1
                && !step_weapon(m, c, &selected, index, 4, (0x2000, 0x8000))
            {
                break;
            }
            index = index.wrapping_add(1);
        }
    } else if pressed & 4 != 0 {
        let mut index = (m.lh(choice) as i32 as u32).wrapping_sub(1);
        while (index as i32) >= 0 {
            if m.lbu(available.wrapping_add(index)) == 1
                && !step_weapon(m, c, &selected, index, 2, (0x1000, 0x4000))
            {
                break;
            }
            index = index.wrapping_sub(1);
        }
    }
    let b = block(m);
    let first = m.lw(b + 0x100);
    let second = m.lw(b + 0x108);
    weapon_arrows_800d80b4(m, c, first, second);
    m.pop_frame(0x38);
    0
}

/// 800DA2CC: weapon panels slide out over 25 updates. Returns 1 (accept,
/// on to commit) or 8 (cancel: back to the aircraft phase); flag 0x40000
/// marks accept here.
pub fn weapon_exit_800da2cc(m: &mut Machine, c: &mut dyn Calls) -> u32 {
    m.push_frame(0x28);
    let b = block(m);
    let t = m.lw(b + 0xc);
    let result = if (t as i32) < 25 {
        let negative = t.wrapping_neg().wrapping_mul(t);
        let square = t.wrapping_mul(t);
        m.sh(b + 0x16, square as u16);
        offset(m, c, 6, negative, 0);
        offset(m, c, 7, square, 0);
        let b = block(m);
        if m.lw(b + 8) & 0x4_0000 != 0 {
            offset(m, c, 4, negative, 0);
            offset(m, c, 3, negative, 0);
            let b = block(m);
            m.sh(b + 0x12, negative as u16);
            m.sh(b + 0x14, negative as u16);
        }
        weapon_label_offsets(m, c, square, t.wrapping_mul(t));
        let b = block(m);
        let t = m.lw(b + 0xc);
        m.sw(b + 0xc, t.wrapping_add(1));
        0
    } else {
        let square = t.wrapping_mul(t);
        m.sw(b + 0xc, 0);
        m.sh(b + 0x16, square as u16);
        if m.lw(b + 8) & 0x4_0000 != 0 { 1 } else { 8 }
    };
    m.pop_frame(0x28);
    result
}

/// 800DA440: commit the weapon choices to context +0x3AD (primary code)
/// and +0x3AE/+0x3AF (secondary codes), through the original jump tables
/// 0x800CB160/0x800CB180. Returns 4.
pub fn commit_weapons_800da440(m: &mut Machine) -> u32 {
    let b = block(m);
    let first = m.lh(b + 0xec) as i32 as u32;
    if first < 7 {
        let code = [1, 2, 3, 4, 5, 6, 0][first as usize];
        let ctx = context(m);
        m.sb(ctx + 0x3ad, code);
    }
    let b = block(m);
    let second = m.lh(b + 0xee) as i32 as u32;
    if second < 8 {
        let (low, high) = [
            (1, 1),
            (2, 2),
            (1, 3),
            (4, 4),
            (0xa, 0xa),
            (7, 7),
            (1, 5),
            (6, 6),
        ][second as usize];
        let ctx = context(m);
        m.sb(ctx + 0x3ae, low);
        let ctx = context(m);
        m.sb(ctx + 0x3af, high);
    }
    4
}

// ---- inner-state callbacks -----------------------------------------------

/// 800DA5F4: aircraft selection entry: phase/substate 0, flags 0x40000,
/// slide timers 0, common script page 0x81D, labels cleared, background
/// volumes (0x1E, 0xA). Returns 1.
pub fn initialize_aircraft_800da5f4(m: &mut Machine, c: &mut dyn Calls) -> u32 {
    m.push_frame(0x18);
    let b = block(m);
    m.sw(b + 4, 0);
    m.sw(b, 0);
    m.sw(b + 8, 0x4_0000);
    m.sw(b + 0xd4, 0);
    m.sw(b + 0x134, 0);
    m.sw(b + 0x130, 0);
    m.sw(b + 0x12c, 0);
    m.sw(b + 0x128, 0);
    c.call(m, SCRIPT_PAGE, [0x81d, 0, 0, 0]);
    c.call(m, SCRIPT_START, [0; 4]);
    clear_labels(m, c);
    c.call(m, BACKGROUND_VOLUME, [0x1e, 0xa, 0, 0]);
    m.pop_frame(0x18);
    1
}

/// 800DA7AC: launch entry: page 6, slot 1 style 1, background fade 3 and
/// object phase, timer 0, context option 4. Returns 1.
pub fn initialize_launch_800da7ac(m: &mut Machine, c: &mut dyn Calls) -> u32 {
    m.push_frame(0x18);
    c.call(m, PAGE, [6, 0, 0, 0]);
    style(m, c, 1, 1);
    background_fade_800e7b50(m, c, 3);
    c.call(m, BACKGROUND_PHASE, [0; 4]);
    let timer = m.lw(LAUNCH_TIMER);
    let ctx = context(m);
    m.sw(timer, 0);
    let options = m.lw(ctx + 0x970);
    m.sw(ctx + 0x970, options | 4);
    m.pop_frame(0x18);
    1
}

/// 800DA80C: launch update. Confirm (0x10) closes the panels and blacks
/// out at once, comparing as timer 75 (the stored timer is unchanged).
/// Timer < 25: panels at +-t*t, colour fades from (143, 135, 83); < 50:
/// black; < 75: return 0x80 without counting. The stored timer otherwise
/// increments and it returns 0.
pub fn tick_launch_800da80c(m: &mut Machine, c: &mut dyn Calls) -> u32 {
    m.push_frame(0x20);
    let ctx = context(m);
    let timer = m.lw(LAUNCH_TIMER);
    let pressed = m.lw(ctx + 0x12c);
    let mut t = m.lw(timer);
    if pressed & 0x10 != 0 {
        offset(m, c, 1, 0, 0x271);
        offset(m, c, 2, 0, (-0x271i32) as u32);
        style(m, c, 1, 0);
        c.call(m, SCENE_COLOUR, [0, 0, 0, 0]);
        t = 75;
    }
    if (t as i32) < 25 {
        let timer = m.lw(LAUNCH_TIMER);
        let t = m.lw(timer);
        offset(m, c, 1, 0, t.wrapping_mul(t));
        offset(m, c, 2, 0, t.wrapping_neg().wrapping_mul(t));
        let left = 25u32.wrapping_sub(t);
        let red = div_magic(left.wrapping_mul(143), 0x51eb_851f, 3, false);
        let green = div_magic(left.wrapping_mul(135), 0x51eb_851f, 3, false);
        let blue = div_magic(left.wrapping_mul(83), 0x51eb_851f, 3, false);
        c.call(m, SCENE_COLOUR, [red, green, blue, 0]);
    } else if (t as i32) < 50 {
        c.call(m, SCENE_COLOUR, [0, 0, 0, 0]);
        style(m, c, 1, 0);
    } else if (t as i32) < 75 {
        m.pop_frame(0x20);
        return 0x80;
    }
    let timer = m.lw(LAUNCH_TIMER);
    let t = m.lw(timer);
    m.sw(timer, t.wrapping_add(1));
    m.pop_frame(0x20);
    0
}

/// The claims of [`dispatch`](fn@dispatch) with outside calls through any
/// [`Calls`] (snapshot tests splice the original's callees through it).
pub mod routines {
    use super::*;

    /// Run a routine of this module by original address with the original
    /// argument registers; `None` for any other address.
    pub fn dispatch(
        m: &mut Machine,
        c: &mut dyn Calls,
        target: u32,
        args: [u32; 4],
    ) -> Option<u32> {
        let [a0, a1, ..] = args;
        Some(match target {
            0x800c_e978 => load_resource_800ce978(m, c, a0, a1, args[2], args[3]),
            0x800d_17d0 => mission_category_800d17d0(m, a0),
            0x800d_a5f4 => initialize_aircraft_800da5f4(m, c),
            0x800d_a7ac => initialize_launch_800da7ac(m, c),
            0x800d_a80c => tick_launch_800da80c(m, c),
            0x800d_9038 => aircraft_phase_init_800d9038(m, c),
            0x800d_92b8 => aircraft_intro_800d92b8(m, c),
            0x800d_9438 => aircraft_input_800d9438(m, c),
            0x800d_9770 => aircraft_exit_800d9770(m, c),
            0x800d_98a0 => commit_aircraft_800d98a0(m),
            0x800d_98c4 => weapon_phase_init_800d98c4(m, c),
            0x800d_9c78 => weapon_intro_800d9c78(m, c),
            0x800d_9ddc => weapon_input_800d9ddc(m, c),
            0x800d_a2cc => weapon_exit_800da2cc(m, c),
            0x800d_a440 => commit_weapons_800da440(m),
            0x800d_8cb4 => draw::draw_statistics_800d8cb4(m, c, a0, a1),
            0x800d_83fc => draw::radar_lines_800d83fc(m, a0, a1),
            0x800d_85b8 => draw::radar_triangles_800d85b8(m, a0, a1),
            0x800d_87f8 => draw::statistics_numbers_800d87f8(m, c, a0, a1),
            0x800d_8abc => draw::weapon_bars_800d8abc(m, a0, a1),
            _ => {
                match target {
                    0x800e_7b50 => background_fade_800e7b50(m, c, a0),
                    0x800d_77e4 => rebuild_availability_800d77e4(m),
                    0x800d_7950 => interpolate_aircraft_800d7950(m),
                    0x800d_7c54 => interpolate_weapons_800d7c54(m),
                    0x800d_7dc8 => slide_weapons_800d7dc8(m, c),
                    0x800d_804c => scale_aircraft_record_800d804c(m, a0),
                    0x800d_808c => scale_weapon_record_800d808c(m, a0),
                    0x800d_80b4 => weapon_arrows_800d80b4(m, c, a0, a1),
                    0x800d_8cfc => begin_aircraft_slide_800d8cfc(m),
                    0x800d_8d34 => slide_aircraft_resources_800d8d34(m, c),
                    0x800d_8e9c => aircraft_arrows_800d8e9c(m, c, a0),
                    _ => return None,
                }
                0
            }
        })
    }
}

/// Every routine this module translates (original addresses).
pub const ROUTINES: [u32; 31] = [
    0x800c_e978,
    0x800d_17d0,
    0x800d_77e4,
    0x800d_7950,
    0x800d_7c54,
    0x800d_7dc8,
    0x800d_804c,
    0x800d_808c,
    0x800d_80b4,
    0x800d_83fc,
    0x800d_85b8,
    0x800d_87f8,
    0x800d_8abc,
    0x800d_8cb4,
    0x800d_8cfc,
    0x800d_8d34,
    0x800d_8e9c,
    0x800d_9038,
    0x800d_92b8,
    0x800d_9438,
    0x800d_9770,
    0x800d_98a0,
    0x800d_98c4,
    0x800d_9c78,
    0x800d_9ddc,
    0x800d_a2cc,
    0x800d_a440,
    0x800d_a5f4,
    0x800d_a7ac,
    0x800d_a80c,
    0x800e_7b50,
];

/// Claims this module's routines while overlay module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != 2 {
        return None;
    }
    routines::dispatch(m, &mut Dispatch, target, args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reciprocal_division_matches_truncation() {
        for x in [
            0i32,
            1,
            49,
            50,
            99,
            -1,
            -49,
            -50,
            -51,
            i32::MAX,
            i32::MIN,
            123_456,
        ] {
            assert_eq!(
                div_magic(x as u32, 0x51eb_851f, 4, false) as i32,
                x / 50,
                "{x}"
            );
            assert_eq!(
                div_magic(x as u32, 0x51eb_851f, 3, false) as i32,
                x / 25,
                "{x}"
            );
            assert_eq!(
                div_magic(x as u32, 0x51eb_851f, 5, false) as i32,
                x / 100,
                "{x}"
            );
            assert_eq!(
                div_magic(x as u32, 0x6666_6667, 2, false) as i32,
                x / 10,
                "{x}"
            );
            assert_eq!(
                div_magic(x as u32, 0x6666_6667, 1, false) as i32,
                x / 5,
                "{x}"
            );
            assert_eq!(
                div_magic(x as u32, 0x8888_8889, 7, true) as i32,
                x / 240,
                "{x}"
            );
        }
    }

    #[test]
    fn claims_only_while_module_2_is_loaded() {
        let mut m = Machine::default();
        m.set_gw(CONTEXT, 0x800b_d158);
        m.sw(0x800b_d158 + 0x38, 1);
        assert_eq!(dispatch(&mut m, 0x800d_98a0, [0; 4]), None);
        m.sw(0x800b_d158 + 0x38, 2);
        m.sw(BLOCK, 0x800f_9598);
        m.sh(0x800f_9598 + 0x50, 7);
        assert_eq!(dispatch(&mut m, 0x800d_98a0, [0; 4]), Some(2));
        assert_eq!(m.lhu(0x800b_d158 + 0x392), 7);
        assert!(m.take_fault().is_ok());
    }
}
