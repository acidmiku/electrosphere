//! Patches of the `hud` lock-on group (generated `hud::tree_39d84`) whose JP
//! instructions differ from PAL (REVIEW.md §2, §3), part 3 of the resident
//! wave: the target markers' screen pass 8003B34C, the lock-on update
//! 8003EF40, the lock cursor 800409D4 and 80040A24 and the fade 8008FC9C.
//! Each is listed in [`REPLACES`](super::REPLACES) under its generated path;
//! the functions keep the generated names.
use crate::hud::tree_39d84::{
    CURSOR, EVENT_FLAGS, LOCK_BLOCK, SCREEN_RECORDS, TARGET_RECORDS, lock_clear_40160,
    lock_states_3eb14, marker_size_3afb4,
};
use crate::hud::{self, LINK, context, entities};
use crate::machine::{Machine, SmallData};
use crate::{script, sdk, shared};
use ac3_gte::{ctrl, reg};

/// `gp+0x384`/`gp+0x388`: script tables of slots 0..31 (0x40-byte
/// entries) and 32..63 (0x28-byte entries).
const SCRIPTS_LOW: u32 = 0x384;

const SCRIPTS_HIGH: u32 = 0x388;

/// The camera-relative coordinate `(position - camera) >> 12` when it
/// fits a signed halfword (the original's `sltu 0xFFFF, rel + 0x8000`).
fn relative(position: u32, camera: u32) -> Option<u16> {
    let rel = (position.wrapping_sub(camera) as i32) >> 12;
    ((rel as u32).wrapping_add(0x8000) <= 0xffff).then_some(rel as u16)
}

/// One-shot script of slot `index`: slots below 32 use the 0x40-byte
/// table at `[gp+0x384]` (word `low`), the others the 0x28-byte table at
/// `[gp+0x388]` (word `high`).
fn slot_script(m: &mut Machine, index: u32, low: u32, high: u32) -> u32 {
    if index < 32 {
        let table = m.gw(SCRIPTS_LOW);
        m.lw(table.wrapping_add(index * 0x40).wrapping_add(low))
    } else {
        let table = m.gw(SCRIPTS_HIGH);
        m.lw(table.wrapping_add((index - 32) * 0x28).wrapping_add(high))
    }
}

/// 800A60F8 `rsin`: no `sdk.rs` function yet.
fn rsin(m: &mut Machine, angle: u32) -> u32 {
    crate::dispatch::call(m, 0x800a_60f8, [angle, 0, 0, 0])
}

/// Little-endian `lwl rt, 0(address)`: the bytes from the aligned word
/// up to `address` replace the high bytes of `rt`.
fn lwl(m: &mut Machine, address: u32, rt: u32) -> u32 {
    let word = m.lw(address & !3);
    let shift = (3 - (address & 3)) * 8;
    let keep = (1u32 << shift).wrapping_sub(1);
    (rt & keep) | (word << shift)
}

/// Little-endian `swl rt, 0(address)`: the high bytes of `rt` replace
/// the bytes from the aligned word up to `address`.
fn swl(m: &mut Machine, address: u32, rt: u32) {
    let aligned = address & !3;
    let word = m.lw(aligned);
    let shift = (3 - (address & 3)) * 8;
    let keep = !(u32::MAX >> shift);
    m.sw(aligned, (word & keep) | (rt >> shift));
}

/// 8002DBE0: start sound effect `id` (unobserved on the captured paths).
fn sound_2d7b8(m: &mut Machine, id: u32) {
    crate::dispatch::call(m, 0x8002_dbe0, [id, 0, 0, 0]);
}

/// One axis of the cursor approach: move `value` by `delta`, at most
/// `speed` (the current size) per frame.
fn approach(value: u16, delta: i32, speed: i16, speed_bits: u16) -> u16 {
    let speed = i32::from(speed);
    if speed < delta {
        value.wrapping_add(speed_bits)
    } else if delta < speed.wrapping_neg() {
        value.wrapping_sub(speed_bits)
    } else {
        value.wrapping_add(delta as u16)
    }
}

/// Differences (x, y, size) between the owner slot's screen record and
/// the cursor.
fn cursor_deltas(m: &mut Machine, record: u32) -> (i32, i32, i32) {
    let dy = i32::from(m.lh(record.wrapping_add(0xa))).wrapping_sub(i32::from(m.lh(CURSOR + 6)));
    let dx = i32::from(m.lh(record.wrapping_add(8))).wrapping_sub(i32::from(m.lh(CURSOR + 4)));
    let ds = i32::from(m.lh(record.wrapping_add(6))).wrapping_sub(i32::from(m.lh(CURSOR + 2)));
    (dx, dy, ds)
}

/// The JP mission number: the halfword at context +0x38C (PAL reads the
/// byte 0x800BAEB4).
fn jp_mission(m: &mut Machine) -> i32 {
    let ctx = context(m);
    i32::from(m.lh(ctx + 0x38c))
}

/// 8003B34C (JP; PAL 8003ABAC, near, `imm:5 ins:1 op:2`): project the target markers of slots 2..63 and run their
/// sighting scripts. Pass 1: for each live entity (word 0 bit 0x4000)
/// with both records, the camera-relative position (`>> 12`, each
/// component a signed halfword) goes through RotTransPers into the
/// screen record (+8 x/y, +6 size from [`marker_size_3afb4`]); the
/// target record's +0x20 flags get 0x10000 (in range), 0x40000 (valid
/// projection) and 0x20000 (inside 320x240). Pass 2: on-screen targets
/// count frames in +0x24 while HUD bit 4 is on (reset otherwise); after
/// 90 frames (0 in mission 6, the JP mission halfword
/// context +0x38C; PAL: the byte 0x800BAEB4) the slot's first script runs once
/// (event bit 0x1000000) and, while `[gameplay+8] > 0`, the second one
/// (event bit 0x2000000). The original returns 0.
pub fn screen_targets_3abac(m: &mut Machine) {
    let frame = m.push_frame(0x68);
    let gameplay = hud::gameplay(m);
    sdk::set_rot_matrix(m, gameplay.wrapping_add(0x4c));
    m.sw(frame + 0x2c, 0);
    m.sw(frame + 0x28, 0);
    m.sw(frame + 0x24, 0);
    sdk::set_trans_matrix(m, frame + 0x10);

    for index in 2..64u32 {
        let entities = hud::entities(m);
        let entity = m.lw(entities.wrapping_add(index * 4));
        let target = m.lw(TARGET_RECORDS + index * 4);
        let status = m.lw(entity);
        let screen = m.lw(SCREEN_RECORDS + index * 4);
        if status & 0x4000 == 0 || target == 0 || screen == 0 {
            continue;
        }
        let gameplay = hud::gameplay(m);
        m.sw(target.wrapping_add(0x20), 0);
        let mut in_range = true;
        for (k, (position, camera)) in [(8u32, 4u32), (0xc, 8), (0x10, 0xc)]
            .into_iter()
            .enumerate()
        {
            let camera_block = m.lw(gameplay.wrapping_add(0x28));
            let position = m.lw(entity.wrapping_add(position));
            let camera = m.lw(camera_block.wrapping_add(camera));
            match relative(position, camera) {
                Some(rel) => m.sh(frame + 0x30 + k as u32 * 2, rel),
                None => {
                    in_range = false;
                    break;
                }
            }
        }
        if !in_range {
            continue;
        }
        let flags = m.lw(target.wrapping_add(0x20)) | 0x1_0000;
        m.sw(target.wrapping_add(0x20), flags);
        let depth = sdk::rot_trans_pers(
            m,
            frame + 0x30,
            screen.wrapping_add(8),
            frame + 0x38,
            frame + 0x3c,
        );
        let size = marker_size_3afb4(depth);
        let gte_flag = m.lw(frame + 0x3c);
        m.sh(screen.wrapping_add(6), size as u16);
        if (gte_flag as i32) < 0 {
            continue;
        }
        let flags = m.lw(target.wrapping_add(0x20)) | 0x4_0000;
        m.sw(target.wrapping_add(0x20), flags);
        if m.lhu(screen.wrapping_add(8)) >= 0x141 {
            continue;
        }
        let y = m.lh(screen.wrapping_add(0xa));
        if !(0..=0xf0).contains(&y) {
            continue;
        }
        m.sw(target.wrapping_add(0x20), flags | 0x2_0000);
    }

    for index in 2..64u32 {
        let event = EVENT_FLAGS + index * 4;
        let entities = hud::entities(m);
        let entity = m.lw(entities.wrapping_add(index * 4));
        let status = m.lw(entity);
        let target = m.lw(TARGET_RECORDS + index * 4);
        if status & 0x4000 == 0 {
            continue;
        }
        let on_screen = m.lw(target.wrapping_add(0x20)) & 0x2_0000 != 0;
        let gameplay = hud::gameplay(m);
        if !on_screen || m.lw(gameplay.wrapping_add(4)) & 4 == 0 {
            m.sw(target.wrapping_add(0x24), 0);
            continue;
        }
        let context = hud::context(m);
        if m.lw(context) & 0x2000_0000 == 0 {
            let frames = m.lw(target.wrapping_add(0x24)).wrapping_add(1);
            m.sw(target.wrapping_add(0x24), frames);
        }
        let delay = if jp_mission(m) == 6 { 0 } else { 0x5a };
        let word = m.lw(event);
        if word & 0x100_0000 == 0 && (m.lw(target.wrapping_add(0x24)) as i32) >= delay {
            m.sw(event, word | 0x100_0000);
            let script = slot_script(m, index, 0x38, 0x20);
            if script != 0 {
                script::interpret(m, script);
            }
        }
        let gameplay = hud::gameplay(m);
        if m.lw(gameplay.wrapping_add(8)) as i32 <= 0 {
            continue;
        }
        let word = m.lw(event);
        if word & 0x200_0000 != 0 || (m.lw(target.wrapping_add(0x24)) as i32) < delay {
            continue;
        }
        m.sw(event, word | 0x200_0000);
        let script = slot_script(m, index, 0x3c, 0x24);
        if script != 0 {
            script::interpret(m, script);
        }
    }
    m.pop_frame(0x68);
}

/// 8003EF40 (JP; PAL 8003E780, changed, `call-:1 del:12 imm:1 ins:10 op:4
/// reg:1`): lock-on update. The lock-suppress byte (`LOCK_BLOCK + 0x10`) is
/// set for weapon modes 5, 6 and 9 of context byte +0x3AA, or, when
/// 8003A5C0 returns 1, of context byte +0x3AB (PAL: the mode from 8003A62C,
/// or context +0x3AE/+0x3AF chosen by 8003A5C0); then the slot lock states
/// (8003F2CC) and the lock cursor (80040A24) step.
pub fn lockon_3e780(m: &mut Machine) {
    m.push_frame(0x18);
    let suppressed = |mode: u8| mode.wrapping_sub(5) < 2 || mode == 9;
    let ctx = context(m);
    let mode = m.lbu(ctx + 0x3aa);
    let suppress = if suppressed(mode) {
        true
    } else if shared::f_8003a08c(m, [0; 4]) != 1 {
        false
    } else {
        let ctx = context(m);
        suppressed(m.lbu(ctx + 0x3ab))
    };
    m.sb(LOCK_BLOCK + 0x10, u8::from(suppress));
    lock_states_3eb14(m);
    lock_cursor_4016c(m);
    m.pop_frame(0x18);
}

/// 800409D4 (JP; PAL 8004011C, near, `imm:1`): start the lock cursor on slot
/// `index` unless it is already running: state 1, size 2 at the screen
/// centre (160, 120; PAL 128). Returns 1 when started, else 0.
pub fn lock_start_4011c(m: &mut Machine, index: u32) -> u32 {
    if m.lbu(CURSOR) != 0 {
        return 0;
    }
    m.sb(CURSOR, 1);
    m.sh(CURSOR + 4, 0xa0);
    m.sh(CURSOR + 6, 0x78);
    m.sb(CURSOR + 1, index as u8);
    m.sh(CURSOR + 2, 2);
    1
}

/// 80040A24 (JP; PAL 8004016C, changed, `call+:3 del:3 imm:11 ins:74 op:7
/// reg:2`): lock cursor animation. While running (state 1) the cursor
/// moves toward the owner slot's marker (x/y by at most its size, size by
/// at most 4 per frame); on arrival it becomes locked (state 2, following
/// the marker exactly). It stops when the owner leaves the lockable state;
/// sounds 0x2D/0x32 play every 8th frame (8002DBE0). On arrival JP runs
/// the banner 8004CEB0(0xE) (PAL: 8004B760(0xE)) in missions 4, 8, 15, 30
/// (the halfword context +0x38C) for an entity with bit 0x100, and the
/// radio cue 80048C0C(8, 0, cue) of the owner's script entry
/// (`[[gp+0x384] + 0x40 * owner + 0x10] + 3`) when its event word has bit
/// 0x800000.
pub fn lock_cursor_4016c(m: &mut Machine) {
    m.push_frame(0x20);
    lock_cursor_body(m);
    m.pop_frame(0x20);
}

/// Body of [`lock_cursor_4016c`] in its frame.
fn lock_cursor_body(m: &mut Machine) {
    let slot = u32::from(m.lbu(CURSOR + 1)) * 4;
    let record = m.lw(SCREEN_RECORDS + slot);
    if record == 0 {
        return;
    }
    let state = m.lbu(CURSOR);
    if state == 0 {
        return;
    }
    if m.lbu(record) == 0 {
        lock_clear_40160(m);
        return;
    }
    if m.lw(EVENT_FLAGS + slot) & 1 == 0 {
        m.sb(CURSOR, 0);
        return;
    }
    let target = m.lw(TARGET_RECORDS + slot);
    if target == 0 {
        return;
    }
    if m.lw(target) & 8 == 0 {
        m.sb(CURSOR, 0);
        return;
    }
    if state == 1 {
        let gameplay = hud::gameplay(m);
        if m.lw(gameplay.wrapping_add(0xc)) & 7 == 0 {
            sound_2d7b8(m, 0x2d);
        }
    }
    let current = m.lbu(CURSOR);
    if current == 2 {
        let gameplay = hud::gameplay(m);
        if m.lw(gameplay.wrapping_add(0xc)) & 7 == 0 {
            sound_2d7b8(m, 0x32);
        }
        if m.lbu(CURSOR) == current {
            let x = m.lhu(record.wrapping_add(8));
            m.sh(CURSOR + 4, x);
            let y = m.lhu(record.wrapping_add(0xa));
            m.sh(CURSOR + 6, y);
            let size = m.lhu(record.wrapping_add(6));
            m.sh(CURSOR + 2, size);
            return;
        }
    }
    let (dx, dy, ds) = cursor_deltas(m, record);
    if dx != 0 || dy != 0 || ds != 0 {
        let speed = m.lh(CURSOR + 2);
        let speed_bits = m.lhu(CURSOR + 2);
        let x = approach(m.lhu(CURSOR + 4), dx, speed, speed_bits);
        m.sh(CURSOR + 4, x);
        let speed = m.lh(CURSOR + 2);
        let speed_bits = m.lhu(CURSOR + 2);
        let y = approach(m.lhu(CURSOR + 6), dy, speed, speed_bits);
        m.sh(CURSOR + 6, y);
        let size = m.lhu(CURSOR + 2);
        let size = if ds >= 5 {
            size.wrapping_add(4)
        } else if ds < -4 {
            size.wrapping_sub(4)
        } else {
            size.wrapping_add(ds as u16)
        };
        m.sh(CURSOR + 2, size);
        let (dx, dy, ds) = cursor_deltas(m, record);
        if dx != 0 || dy != 0 || ds != 0 {
            return;
        }
    }
    // JP: the banner 8004CEB0 (PAL 8004B760), then the owner's radio cue.
    if m.lbu(CURSOR) != 2 && matches!(jp_mission(m), 4 | 8 | 0xf | 0x1e) {
        let owner = u32::from(m.lbu(CURSOR + 1));
        let table = entities(m);
        let entity = m.lw(table.wrapping_add(owner * 4));
        if m.lw(entity) & 0x100 != 0 {
            crate::jp::res_p3_resident::banner_4ceb0(m, 0xe);
        }
    }
    let owner = u32::from(m.lbu(CURSOR + 1));
    if m.lw(EVENT_FLAGS + (owner << 2)) & 0x0080_0000 != 0 {
        let table = m.gw(SCRIPTS_LOW);
        let entry = m.lw(table.wrapping_add(owner << 6).wrapping_add(0x10));
        let cue = u32::from(m.lbu(entry + 3));
        crate::jp::res_p3_resident::radio_cue_48c0c(m, 8, 0, cue, 0);
    }
    m.sb(CURSOR, 2);
}

/// 8008FC9C (JP; PAL 8008E42C, near, `imm:15`): queue the full-screen fade at
/// `level` (0..0x1000; negative draws nothing) into ordering-table slot
/// `ot`, building primitives at `prim`: a DR_MODE, a 320x240 TILE (PAL
/// 320x256; DPCS-faded semi-transparent white, or opaque black at full
/// level), four textured 160x120 quads (PAL 160x128) with their JP texel
/// words and a DPCS-faded color when the level is partial, and a closing
/// DR_MODE. Returns the next free primitive address (`prim` when nothing
/// is drawn).
pub fn fade_8e42c(m: &mut Machine, ot: u32, prim: u32, level: u32) -> u32 {
    let mut level = level as i32;
    if level < 0 {
        return prim;
    }
    if level >= 0x1001 {
        level = 0x1000;
    }
    m.gte.ctc2(ctrl::RFC, 0);
    m.gte.ctc2(ctrl::GFC, 0);
    m.gte.ctc2(ctrl::BFC, 0);
    m.gte.mtc2(reg::RGBC, 0x62ff_ffff);
    let sine = rsin(m, (level >> 1) as u32) as i32 >> 1;
    let depth = if level < 0x800 {
        0x1000i32.wrapping_sub(sine)
    } else {
        sine
    };
    m.gte.mtc2(reg::IR0, depth as u32);
    let context = hud::context(m);
    let tpage_bit = u32::from(m.lw(context.wrapping_add(4)) == 0) << 8;
    m.gte.dpcs(true, false);

    let quads = prim.wrapping_add(0x1c);
    let tile = prim.wrapping_add(0xc);
    m.sw(prim, (tile & LINK) | 0x0200_0000);
    m.sw(prim.wrapping_add(0xc), (quads & LINK) | 0x0300_0000);
    m.sw(prim.wrapping_add(4), 0xe600_0001);
    m.sw(prim.wrapping_add(8), 0xe100_0340);
    m.sw(tile.wrapping_add(8), 0);
    m.sw(tile.wrapping_add(0xc), 0x00f0_0140);
    if level < 0x1000 {
        let color = m.gte.mfc2(reg::RGB2);
        m.sw(tile.wrapping_add(4), color);
    } else {
        m.sw(tile.wrapping_add(4), 0x6000_0000);
    }

    let mut tail = quads;
    if level & 0xfff != 0 {
        let sine = rsin(m, (level >> 1) as u32);
        m.gte.mtc2(reg::IR0, 0x1000u32.wrapping_sub(sine));
        m.gte.mtc2(reg::RGBC, 0x2e7c_7f7d);
        tail = quads.wrapping_add(0xa0);
        m.gte.dpcs(true, false);
        let tpage_x = tpage_bit >> 4;
        for i in 0..4u32 {
            let quad = quads.wrapping_add(i * 0x28);
            m.sw(quad, (quad.wrapping_add(0x28) & LINK) | 0x0900_0000);
            m.sh(quad.wrapping_add(0x16), ((i & 2) | 0x120 | tpage_x) as u16);
            let left: u16 = if i & 2 != 0 { 0xa0 } else { 0 };
            m.sh(quad.wrapping_add(0x18), left);
            m.sh(quad.wrapping_add(0x8), left);
            m.sh(quad.wrapping_add(0x20), left + 0xa0);
            m.sh(quad.wrapping_add(0x10), left + 0xa0);
            let top: u16 = if i & 1 != 0 { 0x78 } else { 0 };
            m.sh(quad.wrapping_add(0x12), top);
            m.sh(quad.wrapping_add(0xa), top);
            m.sh(quad.wrapping_add(0x22), top + 0x78);
            m.sh(quad.wrapping_add(0x1a), top + 0x78);
        }
        let color = m.gte.mfc2(reg::RGB2);
        for offset in [0x7c, 0x54, 0x2c, 0x04] {
            m.sw(quads.wrapping_add(offset), color);
        }
        for (offset, uv) in [
            (0x0c, 0x0203u16),
            (0x14, 0x03a1),
            (0x1c, 0x7905),
            (0x24, 0x79a1),
            (0x34, 0x7705),
            (0x3c, 0x77a1),
            (0x44, 0xee03),
            (0x4c, 0xeda1),
            (0x5c, 0x031f),
            (0x64, 0x02bd),
            (0x6c, 0x791f),
            (0x74, 0x79bb),
            (0x84, 0x771f),
            (0x8c, 0x77bb),
            (0x94, 0xed1f),
            (0x9c, 0xeebd),
        ] {
            m.sh(quads.wrapping_add(offset), uv);
        }
    }

    let next = tail.wrapping_add(0xc);
    m.sw(tail, (next & LINK) | 0x0200_0000);
    m.sw(tail.wrapping_add(4), 0xe600_0000);
    m.sw(tail.wrapping_add(8), 0xe100_0220);
    // addPrims(ot, prim, tail) as compiled: `lwl/swl` at offset 2.
    let link = lwl(m, ot.wrapping_add(2), 0x0200_0000);
    swl(m, tail.wrapping_add(2), link);
    swl(m, ot.wrapping_add(2), prim << 8);
    next
}
