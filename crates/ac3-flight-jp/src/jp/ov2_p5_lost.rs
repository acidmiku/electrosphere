//! JP overlay 2, part 5: state-3 substates 2..4 (connection lost, the
//! loading bar), the resource loaders of the phase tables at 0x80111710 /
//! 0x80111730 (800FB2A0..800FC148), and the 3D scene set-up of the hub
//! (800FC148..800FC5A8).
//!
//! | Substate | Callback | Rust |
//! | ---: | --- | --- |
//! | 2 | `800FBC48` | [`lost_initialize`] |
//! | 3 | `800FBD74` | [`lost_update`]: "CONNECTION LOST" / "REBOOT" |
//! | 4 | `800FBF70` | [`load_update`]: the loader phases and the progress bar |
//!
//! The loader phases request a resource (8002C63C/8002C82C into an arena
//! buffer, the resource block `[gp+0x2DC]` +0x60), wait for it (8002CA2C,
//! the byte counts in the hub block +0x1C8/+0x1CC/+0x1D0 drive the bar) and
//! unpack it.
use super::ov2_p5::{MODULE, alloc, call, ctx, div, framed, hub, lbu, lh, lw, sb, sh, slt, sw};
use crate::common::{ENTITIES, GAMEPLAY, RESOURCES, loaded_module};
use crate::machine::{Machine, SmallData};

/// `[gp+0x2D8]` (0x800BCE4C): the 32 object records of the hub scene.
const OBJECTS: u32 = 0x2d8;
/// The resource loader's completion callback (8009B904).
const LOADED: u32 = 0x8009_b904;

fn resources(m: &mut Machine) -> u32 {
    m.gw(RESOURCES)
}

/// 800FB2A0: the story chapter of mission `a0`: 0 marks the hub block
/// (+0x80 = 1) and returns 1; else 0 (before 20 or from 0x3B), 1, 2 or 3
/// by mission range.
pub fn chapter(m: &mut Machine, a0: u32) -> u32 {
    if a0 == 0 {
        let hub = hub(m);
        sw(m, hub, 0x80, 1);
        return 1;
    }
    let n = a0 as i32;
    match n {
        _ if n < 0x14 => 0,
        _ if n < 0x23 => 1,
        _ if n < 0x28 => 3,
        _ if n < 0x2d => 2,
        _ if n < 0x31 => 3,
        _ if n < 0x36 => 2,
        0x36 => 1,
        _ if n < 0x39 => 2,
        _ if n < 0x3b => 3,
        _ => 0,
    }
}

/// 800FB330: the loading bar: a POLY_G4 from (0xC0, 0x1A4) of width
/// 115 * done / total (clamped to 0..0x72) into packet `a0`, and the six
/// lamps (800DEF94(n, lit)) lit up to 7 * done / total. Returns the next
/// packet.
pub fn loading_bar(m: &mut Machine, a0: u32) -> u32 {
    framed(m, 0x48, |m, sp| {
        let a1 = sp + 0x10;
        for offset in [0x10, 0x11, 0x14, 0x15, 0x18, 0x19, 0x1c, 0x1d] {
            sb(m, a1, offset, 0xe5);
        }
        sb(m, a1, 0x13, 0);
        for offset in [0x12, 0x16, 0x1a, 0x1e] {
            sb(m, a1, offset, 0xd5);
        }
        let hub = hub(m);
        let done = lw(m, hub, 0x1cc);
        let total = lw(m, hub, 0x1c8);
        let scaled =
            ((((done << 3).wrapping_sub(done) << 2).wrapping_add(done)) << 2).wrapping_sub(done);
        let (mut width, _) = div(m, scaled, total, 0x800f_b3c0, 0x800f_b3d8);
        if !slt(width, 0x73) {
            width = 0x72;
        } else if (width as i32) < 0 {
            width = 0;
        }
        sh(m, a1, 0, 0xc0);
        sh(m, a1, 8, 0xc0);
        let hub = self::hub(m);
        let right = width.wrapping_add(0xc0);
        sh(m, a1, 2, 0x1a4);
        sh(m, a1, 6, 0x1a4);
        sh(m, a1, 4, right);
        sh(m, a1, 0xa, 0x1aa);
        sh(m, a1, 0xc, right);
        sh(m, a1, 0xe, 0x1aa);
        let ot = lw(m, hub, 8);
        let next = call(m, 0x800f_7244, [a1, a0, ot, 0]);
        for lamp in 0..6u32 {
            let hub = self::hub(m);
            let done = lw(m, hub, 0x1cc);
            let total = lw(m, hub, 0x1c8);
            let (lit, _) = div(
                m,
                (done << 3).wrapping_sub(done),
                total,
                0x800f_b480,
                0x800f_b498,
            );
            let on = u32::from(slt(lamp, lit));
            call(m, 0x800d_ef94, [lamp + 1, on, 0, 0]);
        }
        next
    })
}

/// Request resource `id` into a fresh arena buffer and mark the context
/// busy (+0x44 = 1): 8002C63C(block + 0x60, 1) gives its size,
/// 8002C82C(block + 0x60, 1, buffer, 8009B904) starts it. `before(m,
/// size)` runs between the size query and the allocation, `after(m, hub,
/// buffer)` before the request.
fn request(
    m: &mut Machine,
    id: u32,
    before: impl FnOnce(&mut Machine, u32),
    after: impl FnOnce(&mut Machine, u32, u32),
) {
    let block = resources(m);
    sw(m, block, 0x60, id);
    let size = call(m, 0x8002_c63c, [block.wrapping_add(0x60), 1, 0, 0]);
    before(m, size);
    let buffer = alloc(m, size);
    let block = resources(m);
    let hub = hub(m);
    let context = ctx(m);
    after(m, hub, buffer);
    sw(m, context, 0x44, 1);
    call(
        m,
        0x8002_c82c,
        [block.wrapping_add(0x60), 1, buffer, LOADED],
    );
}

/// 800FB4F0 (JP; PAL 800CB8DC in overlay 1, changed): request resource
/// 0x71, then reset the progress counts (+0x1C8 = 1, +0x1CC = 0). Returns 1.
pub fn request_71(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        request(m, 0x71, |_, _| {}, |_, _, _| {});
        let hub = hub(m);
        sw(m, hub, 0x1c8, 1);
        sw(m, hub, 0x1cc, 0);
        1
    })
}

/// 800FB574 (JP; PAL 800CB94C in overlay 1, exact): 1 when the resource
/// request is done (context +0x44 == 0).
pub fn request_done(m: &mut Machine) -> u32 {
    let context = ctx(m);
    u32::from(lw(m, context, 0x44) == 0)
}

/// 800FB58C: unpack resource 0x71: its parts by chapter (800ECDA8), the
/// sound bank (8002D96C, then freed), the chapter lamps and the chapter's
/// sound request (`[0x80111700 + 4 * chapter]`, its handle at hub +0x130).
/// Returns 1.
pub fn unpack_71(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        let block = resources(m);
        let id = lw(m, block, 0x60);
        let s0 = call(m, 0x8002_7338, [id, 0, 0, 0]);
        let part = lw(m, s0, 0);
        call(m, 0x800e_cda8, [part, 0, 0, 0]);
        for (chapter, offset) in [(0u32, 8u32), (1, 0xc), (2, 0x10), (3, 0x14)] {
            let hub = hub(m);
            if lw(m, hub, 0x80) == chapter {
                let part = lw(m, s0, offset);
                call(m, 0x800e_cda8, [part, 0, 0, 0]);
            }
        }
        let bank = lw(m, s0, 4);
        call(m, 0x8002_d96c, [bank, 0, 0, 0]);
        let bank = lw(m, s0, 4);
        call(m, 0x8002_7538, [bank, 0, 0, 0]);
        let hub = hub(m);
        if lh(m, hub, 0x1c6) == 0 {
            let chapter = lw(m, hub, 0x80).wrapping_add(2);
            call(m, 0x800d_edf0, [2, (1u32 << (chapter & 31)) | 2, 0, 0]);
        }
        let hub = self::hub(m);
        let chapter = lw(m, hub, 0x80);
        let sound = lw(m, chapter << 2, 0x8011_1700);
        let handle = call(m, 0x8002_db4c, [sound, 0, 0, 0]);
        let hub = self::hub(m);
        sw(m, hub, 0x130, handle);
        1
    })
}

/// 800FB6E8: request the mission's resource (`[0x80111688 + 2 * mission]`),
/// adding its size to the progress total (+0x1C8) and keeping the buffer at
/// +0x1D0. Returns 1.
pub fn request_mission(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        let context = ctx(m);
        let mission = lh(m, context, 0x38c);
        let id = lh(m, mission << 1, 0x8011_1688);
        request(
            m,
            id,
            |m, size| {
                let hub = hub(m);
                let total = lw(m, hub, 0x1c8).wrapping_add(size);
                sw(m, hub, 0x1c8, total);
            },
            |m, hub, buffer| {
                sw(m, hub, 0x1d0, buffer);
                sw(m, hub, 0x1cc, 0);
            },
        );
        1
    })
}

/// The progress step of 800FB7A4/800FB9C4: 8002CA2C(&count) and add the
/// new bytes to the hub's done count. Returns (hub, context).
fn progress(m: &mut Machine, sp: u32) -> (u32, u32) {
    call(m, 0x8002_ca2c, [sp + 0x10, 0, 0, 0]);
    let hub = hub(m);
    let count = lw(m, sp, 0x10);
    let previous = lw(m, hub, 0x1d0);
    sw(m, hub, 0x1d0, count);
    let delta = count.wrapping_sub(previous);
    let done = lw(m, hub, 0x1cc).wrapping_add(delta);
    let context = ctx(m);
    sw(m, hub, 0x1cc, done);
    (hub, context)
}

/// 800FB7A4: count the loaded bytes; 1 when the request is done.
pub fn wait_mission(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, sp| {
        let (_, context) = progress(m, sp);
        u32::from(lw(m, context, 0x44) == 0)
    })
}

/// 800FB7F4: unpack the mission resource: its directory at hub +0x20
/// (+0x1C0 and `[0x80112DC8]` from it), the parts +0x1C and +0x20, the
/// stream part of the mission's stream entry (0x8010E674 table column +1:
/// 0 part +0x10, 1 part +0x14; 800ECF88), the part +0x1C freed and the part
/// +4 at hub +0x120. Returns 1.
pub fn unpack_mission(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let block = resources(m);
        let id = lw(m, block, 0x60);
        let directory = call(m, 0x8002_7338, [id, 0, 0, 0]);
        let hub = hub(m);
        sw(m, hub, 0x20, directory);
        let v0 = lw(m, directory, 8);
        sw(m, hub, 0x1c0, v0);
        let directory = lw(m, hub, 0x20);
        let part = lw(m, directory, 0x1c);
        let first = lw(m, directory, 0);
        m.sw(0x8011_2dc8, first);
        call(m, 0x800e_cda8, [part, 0, 0, 0]);
        let hub = self::hub(m);
        let directory = lw(m, hub, 0x20);
        let part = lw(m, directory, 0x20);
        call(m, 0x800e_cda8, [part, 0, 0, 0]);
        let context = ctx(m);
        let mission = lh(m, context, 0x38c);
        let column = lbu(m, context, 0x956);
        let entry = ((mission << 2).wrapping_add(column) << 1).wrapping_add(0x8010_e674);
        let kind = lbu(m, entry, 1);
        let offset = match kind {
            0 => Some(0x10),
            1 => Some(0x14),
            _ => None,
        };
        if let Some(offset) = offset {
            let hub = self::hub(m);
            let directory = lw(m, hub, 0x20);
            let part = lw(m, directory, offset);
            let v0 = call(m, 0x8002_7338, [part, 0, 0, 0]);
            let a0 = lw(m, v0, 0);
            call(m, 0x800e_cf88, [a0, 0, 0, 0]);
        }
        let hub = self::hub(m);
        let directory = lw(m, hub, 0x20);
        let part = lw(m, directory, 0x1c);
        call(m, 0x8002_7538, [part, 0, 0, 0]);
        let hub = self::hub(m);
        let directory = lw(m, hub, 0x20);
        let part = lw(m, directory, 4);
        let v0 = call(m, 0x8002_7338, [part, 0, 0, 0]);
        let hub = self::hub(m);
        sw(m, hub, 0x120, v0);
        1
    })
}

/// 800FB948: request resource 0x72 (buffer at hub +0x1D0). Returns 1.
pub fn request_72(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        request(
            m,
            0x72,
            |_, _| {},
            |m, hub, buffer| sw(m, hub, 0x1d0, buffer),
        );
        1
    })
}

/// 800FB9C4: count the loaded bytes; when the request is done, start the
/// chapter sound (8002DDA8(hub +0x130)) and return 1, else 0.
pub fn wait_72(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, sp| {
        let (hub, context) = progress(m, sp);
        if lw(m, context, 0x44) != 0 {
            return 0;
        }
        let handle = lw(m, hub, 0x130);
        call(m, 0x8002_dda8, [handle, 0, 0, 0]);
        1
    })
}

/// 800FBA30: unpack resource 0x72: its directory at hub +0x18, the sound
/// bank (+0x20), the part +0x1C (unpacked, then freed), +0x14 to hub +0xAC,
/// buffers of 0x80 (+0x154) and 0x10 (+0x1BC, filled by 800ECD4C from
/// directory +0x10), parts +0xC (+0x158) and +8 (+0x15C, 800DE908(.., 0x1B,
/// 0)), 800E38DC(directory, +4) and 80101EE4. Returns 1.
pub fn unpack_72(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let block = resources(m);
        let id = lw(m, block, 0x60);
        let directory = call(m, 0x8002_7338, [id, 0, 0, 0]);
        let hub = hub(m);
        sw(m, hub, 0x18, directory);
        let bank = lw(m, directory, 0x20);
        call(m, 0x8002_d96c, [bank, 0, 0, 0]);
        for routine in [0x800e_cda8u32, 0x8002_7538] {
            let hub = self::hub(m);
            let directory = lw(m, hub, 0x18);
            let part = lw(m, directory, 0x1c);
            call(m, routine, [part, 0, 0, 0]);
        }
        let hub = self::hub(m);
        let directory = lw(m, hub, 0x18);
        let v0 = lw(m, directory, 0x14);
        sw(m, hub, 0xac, v0);
        let buffer = alloc(m, 0x80);
        let hub = self::hub(m);
        sw(m, hub, 0x154, buffer);
        let buffer = alloc(m, 0x10);
        let hub = self::hub(m);
        let directory = lw(m, hub, 0x18);
        sw(m, hub, 0x1bc, buffer);
        call(m, 0x800e_cd4c, [directory.wrapping_add(0x10), buffer, 0, 0]);
        let hub = self::hub(m);
        let directory = lw(m, hub, 0x18);
        let part = lw(m, directory, 0xc);
        let v0 = call(m, 0x8002_7338, [part, 0, 0, 0]);
        let hub = self::hub(m);
        let directory = lw(m, hub, 0x18);
        sw(m, hub, 0x158, v0);
        let part = lw(m, directory, 8);
        let a0 = call(m, 0x8002_7338, [part, 0, 0, 0]);
        let hub = self::hub(m);
        sw(m, hub, 0x15c, a0);
        call(m, 0x800d_e908, [a0, 0x1b, 0, 0]);
        let hub = self::hub(m);
        let directory = lw(m, hub, 0x18);
        let a1 = lw(m, directory, 4);
        call(m, 0x800e_38dc, [directory, a1, 0, 0]);
        call(m, 0x8010_1ee4, [0; 4]);
        1
    })
}

/// 800FBB74: wait for the fade (800DEE84(1) == 2) or a confirm press
/// (context +0x12C bit 4 without +0x128 bits 0x2F): then 800DEDF0(2, 0)
/// and return 2; else 0.
pub fn wait_fade(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        if call(m, 0x800d_ee84, [1, 0, 0, 0]) != 2 {
            let context = ctx(m);
            if lw(m, context, 0x12c) & 0x10 == 0 || lw(m, context, 0x128) & 0x2f != 0 {
                return 0;
            }
        }
        call(m, 0x800d_edf0, [2, 0, 0, 0]);
        2
    })
}

/// 800FBBE0 (JP; PAL 800D7230, changed): start the fade 800DEE08(1, 1, 0,
/// 10). Returns 2.
pub fn start_fade(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        call(m, 0x800d_ee08, [1, 1, 0, 0xa]);
        2
    })
}

/// 800FBC0C (JP; PAL 800DCF70, changed): 3 once the fade is over
/// (800DEE84(1) == 2), else 0.
pub fn fade_over(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        if call(m, 0x800d_ee84, [1, 0, 0, 0]) == 2 {
            3
        } else {
            0
        }
    })
}

/// 800FBC48, state 3 substate 2: reset the loader phase (hub +0x7C/+0x7D),
/// pick the packet buffer, set up the backdrop (0x8010ED18) and the fades;
/// after a lost connection (context +0x957) or a reboot (+0x96C bit 0) wait
/// 120 frames, adding the mission time to the totals for a lost connection.
/// Returns NEXT_SUBSTATE.
pub fn lost_initialize(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        let hub = hub(m);
        sb(m, hub, 0x7c, 0);
        let hub = self::hub(m);
        sb(m, hub, 0x7d, 0);
        let context = ctx(m);
        let buffer = lw(m, context, 4);
        let hub = self::hub(m);
        let packets = lw(m, hub.wrapping_add(buffer << 2), 0x278);
        sw(m, hub, 0x274, packets);
        let v0 = call(m, 0x8002_7338, [0x8010_ed18, 0, 0, 0]);
        call(m, 0x800d_e908, [v0, 8, 2, 0]);
        call(m, 0x800d_edf0, [2, 0, 0, 0]);
        call(m, 0x800d_ee08, [1, 1, 0, 0x78]);
        let context = ctx(m);
        if lbu(m, context, 0x957) == 1 {
            call(m, 0x800d_edf0, [2, 0x40, 0, 0]);
            let hub = self::hub(m);
            let a0 = ctx(m);
            sw(m, hub, 0x50, 0x78);
            let mission = lh(m, a0, 0x38c);
            let time = lh(m, a0, 0x958);
            let slot = a0.wrapping_add(mission << 2);
            let total = lw(m, slot, 0xf3c).wrapping_add(time);
            sw(m, slot, 0xf3c, total);
            let time = lh(m, a0, 0x958);
            let total = lw(m, a0, 0x1034).wrapping_add(time);
            sw(m, a0, 0x1034, total);
        }
        let context = ctx(m);
        if lw(m, context, 0x96c) & 1 != 0 {
            call(m, 0x800d_edf0, [2, 0x80, 0, 0]);
            let hub = self::hub(m);
            sw(m, hub, 0x50, 0x78);
        }
        0x0400_0000
    })
}

/// Clear the ordering table of this frame's buffer and link the context's
/// display list around it (the sequence of 800FBD74 and 800FBF70).
fn link_display(m: &mut Machine) -> (u32, u32) {
    let context = ctx(m);
    let buffer = lw(m, context, 4);
    let hub = hub(m);
    let ot = lw(m, hub, buffer << 2);
    sw(m, hub, 8, ot);
    call(m, 0x800a_8680, [ot, 0x10, 0, 0]);
    let a1 = self::hub(m);
    let a0 = ctx(m);
    let a3 = lw(m, a1, 8);
    let list = lw(m, a0, 8);
    let v1 = lw(m, a3, 0);
    let v0 = lw(m, list, 0);
    sw(m, a3, 0, (v1 & 0xff00_0000) | (v0 & 0x00ff_ffff));
    let v0 = lw(m, a1, 8);
    let a3 = lw(m, a0, 8);
    let v1 = lw(m, a3, 0);
    sw(
        m,
        a3,
        0,
        (v1 & 0xff00_0000) | (v0.wrapping_add(0x3c) & 0x00ff_ffff),
    );
    (a1, a0)
}

/// 800FBD74, state 3 substate 3: the "CONNECTION LOST" / "REBOOT" screen.
/// Counts hub +0x50 down (drawing the backdrop, 800E2494) unless confirm is
/// pressed; returns NEXT_SUBSTATE when it is over or not wanted, else 0.
pub fn lost_update(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        let context = ctx(m);
        let flags = lw(m, context, 0x96c);
        if flags & 8 != 0 {
            return 0x0400_0000;
        }
        if lbu(m, context, 0x957) == 0 && flags & 1 == 0 {
            return 0x0400_0000;
        }
        let (a1, a0) = link_display(m);
        let buffer = lw(m, a0, 4);
        let ot = lw(m, a1, 8);
        let packets = lw(m, a1.wrapping_add(buffer << 2), 0x278);
        sw(m, a1, 0x274, packets);
        call(m, 0x800f_3db4, [ot, 0, 0, 0]);
        call(m, 0x800f_35b8, [7, 0, 0, 0]);
        call(m, 0x800f_33f8, [0xffff_ffff, 0, 0, 0]);
        call(m, 0x800f_34c8, [1, 1, 1, 0]);
        call(m, 0x800f_34b0, [1, 0, 0, 0]);
        call(m, 0x800f_33d0, [0x18, 0xb8, 0xd0, 0]);
        let context = ctx(m);
        if lbu(m, context, 0x957) == 1 {
            call(m, 0x800f_33a8, [0xca, 0x9b, 0, 0]);
            call(m, 0x800f_4ae0, [0x800c_d678, 0, 0, 0]);
        }
        let context = ctx(m);
        if lw(m, context, 0x96c) & 1 != 0 {
            call(m, 0x800f_33a8, [0xe9, 0x91, 0, 0]);
            call(m, 0x800f_4ae0, [0x800c_d688, 0, 0, 0]);
        }
        let v1 = ctx(m);
        if lw(m, v1, 0x12c) & 0x10 != 0 && lw(m, v1, 0x128) & 0x2f == 0 {
            return 0x0400_0000;
        }
        let hub = hub(m);
        let count = lw(m, hub, 0x50);
        if (count as i32) <= 0 {
            return 0x0400_0000;
        }
        let packets = lw(m, hub, 0x274);
        sw(m, hub, 0x50, count.wrapping_sub(1));
        let v0 = call(m, 0x800e_2494, [packets, 0, 0, 0]);
        let hub = self::hub(m);
        sw(m, hub, 0x274, v0);
        0
    })
}

/// 800FBF70, state 3 substate 4: clear the lost-connection flags, then run
/// the loader phase `[0x80111730 + 4 * hub+0x7D]` and draw the progress
/// bar: phase result 1 goes to the next phase, 2 ends the loading (phase
/// +0x1C6 = 1, NEXT_SUBSTATE). Returns NEXT_SUBSTATE when the loading is
/// over or not wanted, else 0.
pub fn load_update(m: &mut Machine, a2: u32, a3: u32) -> u32 {
    framed(m, 0x28, |m, _| {
        let a0 = ctx(m);
        let buffer = lw(m, a0, 4);
        let hub = hub(m);
        let packets = lw(m, hub.wrapping_add(buffer << 2), 0x278);
        sw(m, hub, 0x274, packets);
        sb(m, a0, 0x957, 0);
        let v1 = ctx(m);
        let flags = lw(m, v1, 0x96c);
        let a0 = self::hub(m);
        sw(m, v1, 0x96c, flags & !1);
        if lh(m, a0, 0x1c6) != 0 {
            sh(m, a0, 0x88, 0);
            sh(m, a0, 0x8a, 0);
            return 0x0400_0000;
        }
        if flags & 8 != 0
            || lbu(m, v1, 0x95f) == 1
            || lbu(m, v1, 0x95e) != 0
            || lh(m, v1, 0x38c) == 0x3b
        {
            return 0x0400_0000;
        }
        let phase = lbu(m, a0, 0x7d);
        let routine = lw(m, phase << 2, 0x8011_1730);
        // the phase routines take no arguments; a0 (hub), a1 (the flags) and
        // the caller's a2/a3 are what the registers hold
        let result = call(m, routine, [a0, flags, a2, a3]);
        let context = ctx(m);
        let buffer = lw(m, context, 4);
        let a1 = self::hub(m);
        let ot = lw(m, a1.wrapping_add(buffer << 2), 0);
        sw(m, a1, 8, ot);
        call(m, 0x800a_8680, [ot, 0x10, 0, 0]);
        let a1 = self::hub(m);
        let a3 = ctx(m);
        let a2 = lw(m, a1, 8);
        let list = lw(m, a3, 8);
        let v1 = lw(m, a2, 0);
        let v0 = lw(m, list, 0);
        sw(m, a2, 0, (v1 & 0xff00_0000) | (v0 & 0x00ff_ffff));
        let v0 = lw(m, a1, 8);
        let a2 = lw(m, a3, 8);
        let v1 = lw(m, a2, 0);
        sw(
            m,
            a2,
            0,
            (v1 & 0xff00_0000) | (v0.wrapping_add(0x3c) & 0x00ff_ffff),
        );
        let packets = lw(m, a1, 0x274);
        let next = loading_bar(m, packets);
        let hub = self::hub(m);
        sw(m, hub, 0x274, next);
        let v0 = call(m, 0x800e_2494, [next, 0, 0, 0]);
        let a0 = self::hub(m);
        sw(m, a0, 0x274, v0);
        match result {
            1 => {
                let phase = lbu(m, a0, 0x7d).wrapping_add(1);
                sb(m, a0, 0x7d, phase);
                0
            }
            2 => {
                sh(m, a0, 0x88, 0);
                sh(m, a0, 0x8a, 0);
                sh(m, a0, 0x1c6, 1);
                0x0400_0000
            }
            _ => 0,
        }
    })
}

// ---- the hub scene ------------------------------------------------------------------

/// 800FC148: allocate the hub scene: the sprite list (hub +0xD0/+0xD4, an
/// 0x3F0,0xF8 16x8 area), two ordering tables and packet buffers in the
/// gameplay block (+0x18/+0x20, current at +0x10/+0x14), the object
/// pointer tables `[gp+0x2E8]` (2 of 0x58 and 30 of 0x5C bytes) and
/// `[gp+0x2D8]` (32 of 0x94 bytes), and 64 records of 0x1C bytes at
/// gameplay +0x250. Returns 0.
pub fn scene_create(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        let hub = hub(m);
        sh(m, hub, 0xd8, 0x3f0);
        sh(m, hub, 0xda, 0xf8);
        sh(m, hub, 0xdc, 0x10);
        sh(m, hub, 0xde, 8);
        let v0 = alloc(m, 0x100);
        let hub = self::hub(m);
        let rows = lh(m, hub, 0xde);
        sw(m, hub, 0xd0, v0);
        let v0 = alloc(m, rows << 5);
        let hub = self::hub(m);
        sw(m, hub, 0xd4, v0);
        for s1 in 0..2u32 {
            let ot = alloc(m, 0x100);
            let gameplay = m.gw(GAMEPLAY);
            sw(m, gameplay.wrapping_add(s1 << 2), 0x18, ot);
            let packets = alloc(m, 0x4800);
            let gameplay = m.gw(GAMEPLAY);
            sw(m, gameplay.wrapping_add(s1 << 2), 0x20, packets);
        }
        let a2 = ctx(m);
        let buffer = lw(m, a2, 4);
        let a1 = m.gw(GAMEPLAY);
        let ot = lw(m, a1.wrapping_add(buffer << 2), 0x18);
        sw(m, a1, 0x10, ot);
        let buffer = lw(m, a2, 4);
        let packets = lw(m, a1.wrapping_add(buffer << 2), 0x20);
        sw(m, a1, 0x14, packets);
        let tables = alloc(m, 0x100);
        m.set_gw(ENTITIES, tables);
        m.set_gw(OBJECTS, tables.wrapping_add(0x80));
        for s1 in 0..0x20u32 {
            let record = alloc(m, if s1 < 2 { 0x58 } else { 0x5c });
            let table = m.gw(ENTITIES);
            sw(m, table, s1 << 2, record);
            sw(m, record, 0, 0);
        }
        for s1 in 0..0x20u32 {
            let table = m.gw(ENTITIES);
            let record = lw(m, table, s1 << 2);
            sb(m, record, 5, 0);
            let table = m.gw(ENTITIES);
            let record = lw(m, table, s1 << 2);
            sh(m, record, 6, 0);
        }
        for s1 in 0..0x20u32 {
            let record = alloc(m, 0x94);
            let table = m.gw(OBJECTS);
            sw(m, table, s1 << 2, record);
            sw(m, record, 0, 0);
        }
        let v0 = alloc(m, 4);
        let gameplay = m.gw(GAMEPLAY);
        sw(m, gameplay, 0x250, v0);
        for s1 in 0..0x40u32 {
            let record = alloc(m, 0x1c);
            let gameplay = m.gw(GAMEPLAY);
            let table = lw(m, gameplay, 0x250);
            sw(m, table, s1 << 2, record);
        }
        0
    })
}

/// 800FC37C: reset the gameplay block's light set-up: the colour matrix
/// +0x8C (0x800 diagonal), the light matrix +0x6C (zero) and its rows from
/// (0, 0, 0x1000) rotated by RotMatrixYXZ(0, 0, 0) (ApplyMatrixSV into +0x6C
/// and +0x72), and the ambient +0xAC (0x54, 0x54, 0x54). Returns the last
/// call's v0.
pub fn scene_lights(m: &mut Machine) -> u32 {
    framed(m, 0x58, |m, sp| {
        let (s3, s2) = (sp + 0x18, sp + 0x20);
        sh(m, sp, 0x14, 0x1000);
        let s1 = m.gw(GAMEPLAY);
        sh(m, sp, 0x10, 0);
        sh(m, sp, 0x12, 0);
        let colour = s1.wrapping_add(0x8c);
        let s0 = s1.wrapping_add(0x6c);
        sh(m, s1, 0x8c, 0x800);
        sh(m, colour, 6, 0x800);
        sh(m, colour, 0xc, 0x800);
        for offset in [2, 8, 0xe, 4, 0xa, 0x10] {
            sh(m, colour, offset, 0);
        }
        let ambient = s1.wrapping_add(0xac);
        sh(m, s1, 0x6c, 0);
        for offset in [2, 4, 6, 8, 0xa, 0xc, 0xe, 0x10] {
            sh(m, s0, offset, 0);
        }
        sw(m, s1, 0xac, 0x54);
        sw(m, ambient, 4, 0x54);
        sw(m, ambient, 8, 0x54);
        let mut v0 = 0;
        for target in [s0, s1.wrapping_add(0x72)] {
            sh(m, sp, 0x18, 0);
            sh(m, sp, 0x1a, 0);
            sh(m, sp, 0x1c, 0);
            call(m, 0x800a_7718, [s3, s2, 0, 0]);
            v0 = call(m, 0x800a_6e78, [s2, sp + 0x10, target, 0]);
        }
        v0
    })
}

/// 800FC488: set up the hub camera: the view record at gameplay +0xB8
/// (gameplay +0x28; 80077880(view, 0xC8, 0x1F40, 0x6D60, zero[8])), the
/// lights (800FC37C), the camera position (0, -0x200000, 0) and angles
/// (-0x1000, 0, 0), the matrices +0x2C (80030778) and +0x4C (its
/// transpose, 80102120), SetGeomOffset(0, 0) and SetGeomScreen(view +0x24).
/// Returns SetGeomScreen's v0.
pub fn scene_camera(m: &mut Machine) -> u32 {
    framed(m, 0x28, |m, sp| {
        call(m, 0x800a_dd18, [sp + 0x18, 0, 8, 0]);
        let v1 = m.gw(GAMEPLAY);
        sw(m, v1, 0x28, v1.wrapping_add(0xb8));
        sw(m, sp, 0x10, sp + 0x18);
        let view = lw(m, v1, 0x28);
        call(m, 0x8007_7880, [view, 0xc8, 0x1f40, 0x6d60]);
        scene_lights(m);
        let v0 = m.gw(GAMEPLAY);
        let view = lw(m, v0, 0x28);
        sw(m, view, 4, 0);
        let view = lw(m, v0, 0x28);
        sw(m, view, 8, 0xffe0_0000);
        let view = lw(m, v0, 0x28);
        sw(m, view, 0xc, 0);
        let view = lw(m, v0, 0x28);
        sh(m, view, 0x14, 0xf000);
        let view = lw(m, v0, 0x28);
        sh(m, view, 0x16, 0);
        let view = lw(m, v0, 0x28);
        sh(m, view, 0x18, 0);
        let view = lw(m, v0, 0x28);
        for offset in [0x48, 0x44, 0x40, 0x68, 0x64, 0x60] {
            sw(m, v0, offset, 0);
        }
        call(
            m,
            0x8003_0778,
            [view.wrapping_add(0x14), v0.wrapping_add(0x2c), 0, 0],
        );
        let a1 = m.gw(GAMEPLAY);
        call(
            m,
            0x800a_7448,
            [a1.wrapping_add(0x2c), a1.wrapping_add(0x4c), 0, 0],
        );
        let a0 = m.gw(GAMEPLAY);
        call(m, 0x8010_2120, [a0.wrapping_add(0x4c), 0, 0, 0]);
        call(m, 0x800a_7118, [0, 0, 0, 0]);
        let v0 = m.gw(GAMEPLAY);
        let view = lw(m, v0, 0x28);
        let h = lh(m, view, 0x24);
        call(m, 0x800a_7138, [h, 0, 0, 0])
    })
}

/// The routines of this file by JP address, while module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    let [a0, _, a2, a3] = args;
    Some(match target {
        0x800f_b2a0 => chapter(m, a0),
        0x800f_b330 => loading_bar(m, a0),
        0x800f_b4f0 => request_71(m),
        0x800f_b574 => request_done(m),
        0x800f_b58c => unpack_71(m),
        0x800f_b6e8 => request_mission(m),
        0x800f_b7a4 => wait_mission(m),
        0x800f_b7f4 => unpack_mission(m),
        0x800f_b948 => request_72(m),
        0x800f_b9c4 => wait_72(m),
        0x800f_ba30 => unpack_72(m),
        0x800f_bb74 => wait_fade(m),
        0x800f_bbe0 => start_fade(m),
        0x800f_bc0c => fade_over(m),
        0x800f_bc48 => lost_initialize(m),
        0x800f_bd74 => lost_update(m),
        0x800f_bf70 => load_update(m, a2, a3),
        0x800f_c148 => scene_create(m),
        0x800f_c37c => scene_lights(m),
        0x800f_c488 => scene_camera(m),
        _ => return None,
    })
}
