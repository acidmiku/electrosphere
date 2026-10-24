//! JP overlay 2: the aircraft model viewer and the fade steps around it,
//! translated from the JP instructions.
//!
//! The viewer block `[0x80112E30]` (0x4008 bytes): +0 the model data, +0x4000
//! the model number (0..0x30, the table 0x80112C08 of 8-byte records: +0 its
//! resource, +2 its entity slot, +4 its scene resource or 0), +0x4001 a model
//! load pending, +0x4003 the leave request (1 back, 2 out), +0x4004 the
//! scene's arena mark. `[0x80112DDC]` +1 is the sub-mode's step; the hub's
//! +0xFC..+0x104 the view distance and angles.
use super::ov2_p6::{DISPLAY, HUB, MAP, add_prim_masked, call, divide_pow2, framed};
use crate::{
    arena,
    common::{CONTEXT, ENTITIES, GAMEPLAY, RESOURCES},
    machine::{Machine, SmallData},
};

/// `[0x80112E30]`: the viewer block.
pub const VIEWER: u32 = 0x8011_2e30;
/// `[0x80112DDC]`: the sub-mode state (+1 its step).
pub const MODE: u32 = 0x8011_2ddc;
/// `0x80112BF4`: the fade counter.
pub const FADE: u32 = 0x8011_2bf4;
/// The viewer's models (8-byte records).
pub const MODELS: u32 = 0x8011_2c08;
/// 8009B904: the resource loader's completion callback.
const LOADED: u32 = 0x8009_b904;
/// 800DEE08 / 800DEF94 / 800DEE84 / 800DECA8 / 800DEDF0 / 800DEBB8
/// (another part): visual slots.
const SET_VISUAL: u32 = 0x800d_ee08;
const SET_VISUAL_STYLE: u32 = 0x800d_ef94;
const VISUAL_MODE: u32 = 0x800d_ee84;
const VISUAL_VALUE: u32 = 0x800d_eea8;
const VISUAL_FADE: u32 = 0x800d_eca8;
const VISUAL_FLAGS: u32 = 0x800d_edf0;
const VISUAL_RESET: u32 = 0x800d_ebb8;

fn viewer(m: &mut Machine) -> u32 {
    m.lw(VIEWER)
}
fn hub(m: &mut Machine) -> u32 {
    m.lw(HUB)
}
/// Advance the sub-mode step by one.
fn next_step(m: &mut Machine) {
    let mode = m.lw(MODE);
    let step = m.lbu(mode.wrapping_add(1)).wrapping_add(1);
    m.sb(mode.wrapping_add(1), step);
}
fn set_step(m: &mut Machine, step: u8) {
    let mode = m.lw(MODE);
    m.sb(mode.wrapping_add(1), step);
}
/// The current model's record in 0x80112C08.
fn record(m: &mut Machine) -> u32 {
    let viewer = viewer(m);
    let number = u32::from(m.lbu(viewer.wrapping_add(0x4000)));
    (number << 3).wrapping_add(MODELS)
}
fn entity(m: &mut Machine, slot: u32) -> u32 {
    let table = m.gw(ENTITIES);
    m.lw((slot << 2).wrapping_add(table))
}
/// Visual slots `slots` set to `(mode, a2, a3)` in order.
fn set_visuals(m: &mut Machine, slots: &[u32], mode: u32, a2: u32, a3: u32) {
    for &slot in slots {
        call(m, SET_VISUAL, [slot, mode, a2, a3]);
    }
}
/// Whether a press of `pressed` happened with none of `held` held: (the
/// decision, the original's last `v0`).
fn press(m: &mut Machine, pressed: u32, held: u32) -> (bool, u32) {
    let context = m.gw(CONTEXT);
    if m.lw(context.wrapping_add(0x12c)) & pressed == 0 {
        return (false, 0);
    }
    let blocked = m.lw(context.wrapping_add(0x128)) & held;
    (blocked == 0, blocked)
}

// ---- 801071B8 .. 80107358: the fade steps ----------------------------------------------------

/// 801071B8 (not reached): the next step; visual slot 15 fades in
/// (800DEE08(15, 1, 0, 20)); 800FF19C(3). Returns its v0.
pub fn open_step(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        next_step(m);
        call(m, SET_VISUAL, [0xf, 1, 0, 0x14]);
        call(m, 0x800f_f19c, [3, 0, 0, 0])
    })
}

/// 80107200 (not reached): once the map layer is off, the next step and
/// the fade counter (1 when the background red is below 0x8F, else 0x20).
/// Returns the counter set, or 0x80110000 while the map layer is on.
pub fn fade_wait(m: &mut Machine) -> u32 {
    let map = m.lw(MAP);
    if m.lw(map.wrapping_add(0x14)) != 0 {
        return 0x8011_0000;
    }
    next_step(m);
    let display = m.gw(DISPLAY);
    let value = if m.lbu(display.wrapping_add(0x10)) < 0x8f {
        1
    } else {
        0x20
    };
    m.sw(FADE, value);
    value
}

/// 80107270 (not reached): count the fade down (the next step at 0); the
/// background color fades with it (143, 135, 83 * counter / 32) and the
/// display request gets flag 8. Returns the flags.
pub fn fade_step(m: &mut Machine) -> u32 {
    let count = m.lw(FADE).wrapping_sub(1);
    m.sw(FADE, count);
    if count == 0 {
        next_step(m);
    }
    for (offset, factor) in [(0x10u32, 143u32), (0x11, 135), (0x12, 83)] {
        let count = m.lw(FADE);
        let display = m.gw(DISPLAY);
        let value = divide_pow2(count.wrapping_mul(factor) as i32, 5);
        m.sb(display.wrapping_add(offset), value as u8);
    }
    let display = m.gw(DISPLAY);
    let flags = m.lw(display) | 8;
    m.sw(display, flags);
    flags
}

/// 80107358 (not reached): the hub's +0x7F = 1, visual slot 2 off, the
/// context's options +0x96C |= 2. Returns the options.
pub fn leave_hub(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let hub = hub(m);
        m.sb(hub.wrapping_add(0x7f), 1);
        call(m, SET_VISUAL, [2, 0, 0, 0]);
        let context = m.gw(CONTEXT);
        let options = m.lw(context.wrapping_add(0x96c)) | 2;
        m.sw(context.wrapping_add(0x96c), options);
        options
    })
}

// ---- 80108298 .. 80108874: the model -------------------------------------------------------

/// Request the resource `resource` (`[gp+0x2DC]+0x60`, 8002C63C for its
/// size, an arena buffer, 8002C82C with the completion 8009B904; the
/// context's +0x44 = 1 while loading).
fn request(m: &mut Machine, resource: u32) {
    let resources = m.gw(RESOURCES);
    m.sw(resources.wrapping_add(0x60), resource);
    let size = call(m, 0x8002_c63c, [resources.wrapping_add(0x60), 1, 0, 0]);
    let buffer = arena::alloc(m, size);
    let resources = m.gw(RESOURCES);
    let context = m.gw(CONTEXT);
    m.sw(context.wrapping_add(0x44), 1);
    call(
        m,
        0x8002_c82c,
        [resources.wrapping_add(0x60), 1, buffer, LOADED],
    );
}

/// 80108298 (not reached): load the current model's resource; a load is
/// pending (+0x4001). Returns the viewer block.
pub fn load_model(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        let record = record(m);
        let resource = m.lh(record) as i32 as u32;
        request(m, resource);
        let viewer = viewer(m);
        m.sb(viewer.wrapping_add(0x4001), 1);
        viewer
    })
}

/// 80108334 (not reached): finish a pending model load once the loader is
/// idle (context +0x44) and visual slot 2 has settled: decompress the
/// model (8002 7338 twice, 800ED0D4, 800AFD08 into the viewer block), make
/// it the gameplay model (+0x250 +0x20, 80027380), reset the view (hub
/// +0xFC = -0x100000, angles 0xC00/0x2800/0) and show slots 2 and 8; free
/// the buffer. Returns 1 when done, 0 while loading, -1 when nothing is
/// pending or the slot is busy.
pub fn finish_model(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        let viewer = viewer(m);
        if m.lbu(viewer.wrapping_add(0x4001)) == 0 {
            return u32::MAX;
        }
        let context = m.gw(CONTEXT);
        if m.lw(context.wrapping_add(0x44)) == 1 {
            return 0;
        }
        if call(m, VISUAL_MODE, [2, 0, 0, 0]) != 0 {
            return u32::MAX;
        }
        let resources = m.gw(RESOURCES);
        let buffer = m.lw(resources.wrapping_add(0x60));
        let entry = call(m, 0x8002_7338, [buffer, 0, 0, 0]);
        let first = m.lw(entry);
        let files = call(m, 0x8002_7338, [first, 0, 0, 0]);
        let header = m.lw(files);
        call(m, 0x800e_d0d4, [header, 0, 0, 0]);
        let packed = m.lw(files.wrapping_add(4));
        let viewer = self::viewer(m);
        call(m, 0x800a_fd08, [packed, viewer, 0, 0]);
        let gameplay = m.gw(GAMEPLAY);
        let viewer = self::viewer(m);
        let models = m.lw(gameplay.wrapping_add(0x250));
        m.sw(models.wrapping_add(0x20), viewer);
        let models = m.lw(gameplay.wrapping_add(0x250));
        let model = m.lw(models.wrapping_add(0x20));
        call(m, 0x8002_7380, [model, model.wrapping_add(4), 6, 0]);
        let hub = hub(m);
        let viewer = self::viewer(m);
        m.sw(hub.wrapping_add(0xfc), 0xfff0_0000);
        m.sh(hub.wrapping_add(0x100), 0xc00);
        m.sh(hub.wrapping_add(0x102), 0x2800);
        m.sh(hub.wrapping_add(0x104), 0);
        m.sb(viewer.wrapping_add(0x4001), 0);
        set_visuals(m, &[2, 8], 1, 0, 0x19);
        arena::reset(m, buffer);
        1
    })
}

/// 80108478 (not reached): LoadImage of the hub's image `which` (0: +0xD0,
/// 1: +0xD4) to the rectangle +0xD8. Returns its v0 (1 for another
/// `which`).
pub fn upload_image(m: &mut Machine, which: u32) -> u32 {
    framed(m, 0x18, |m, _| {
        let offset = match which {
            0 => 0xd0,
            1 => 0xd4,
            _ => return 1,
        };
        let hub = hub(m);
        let image = m.lw(hub.wrapping_add(offset));
        call(m, 0x800a_8440, [hub.wrapping_add(0xd8), image, 0, 0])
    })
}

/// 801084D0 (not reached): turn the model with the pad: R2/L2 (held 0x200
/// / 0x100) move the view distance (hub +0xFC) in 0x1000 steps within
/// -0x200000..-0x80000; UP/DOWN/LEFT/RIGHT (8/4/1/2) add 0x80 steps to the
/// view angles (80030F64 of hub +0x100 with the SVECTOR on the stack).
/// Returns its v0.
pub fn turn_model(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, sp| {
        call(m, 0x800a_dd18, [sp + 0x10, 0, 8, 0]);
        let context = m.gw(CONTEXT);
        if m.lw(context.wrapping_add(0x128)) & 0x200 != 0 {
            let hub = hub(m);
            let distance = m.lw(hub.wrapping_add(0xfc));
            if (0xffe0_0000u32 as i32) < distance as i32 {
                m.sw(hub.wrapping_add(0xfc), distance.wrapping_sub(0x1000));
            }
        }
        let context = m.gw(CONTEXT);
        if m.lw(context.wrapping_add(0x128)) & 0x100 != 0 {
            let hub = hub(m);
            let distance = m.lw(hub.wrapping_add(0xfc));
            if (distance as i32) < (0xfff8_0000u32 as i32) {
                m.sw(hub.wrapping_add(0xfc), distance.wrapping_add(0x1000));
            }
        }
        let context = m.gw(CONTEXT);
        for (bit, offset, value) in [
            (8u32, 0x12u32, 0x80u16),
            (4, 0x12, 0xff80),
            (1, 0x10, 0xff80),
            (2, 0x10, 0x80),
        ] {
            if m.lw(context.wrapping_add(0x128)) & bit != 0 {
                m.sh(sp + offset, value);
            }
        }
        let hub = hub(m);
        call(m, 0x8003_0f64, [hub.wrapping_add(0x100), sp + 0x10, 0, 0])
    })
}

/// `-325 * (25 - slide)^2 / 625 * 256` as compiled (multiply-high by
/// 0x68DB8BAD, `sra 8`): the slide-in offset of the viewer panels.
fn slide_offset(slide: u32) -> u32 {
    let left = 0x19u32.wrapping_sub(slide);
    let square = left.wrapping_mul(left.wrapping_mul(325).wrapping_neg());
    super::ov2_p6::magic_quotient(square as i32, 0x68db_8bad, 8) as u32
}

/// 801085F8: place the model and the camera: `mode` 0 the current model
/// at rest (geometry offset x 0x50 + the panel slide of visual slot 2, y
/// 0x84), 1 the model turned by the pad (801084D0) in the middle of the
/// screen with the hub's angles and view distance, 2 entity 2 at rest;
/// the camera at (0, -0x100000 or the view distance, 0). Returns the
/// camera (2 for another mode).
pub fn place_model(m: &mut Machine, mode: u32) -> u32 {
    framed(m, 0x28, |m, _| {
        let slide = call(m, VISUAL_VALUE, [2, 0, 0, 0]);
        let record = record(m);
        let empty = u32::from(m.lh(record.wrapping_add(4)) == 0) << 1;
        let finish = |m: &mut Machine, distance: u32| {
            let gameplay = m.gw(GAMEPLAY);
            let camera = m.lw(gameplay.wrapping_add(0x28));
            m.sw(camera.wrapping_add(4), 0);
            let camera = m.lw(gameplay.wrapping_add(0x28));
            m.sw(camera.wrapping_add(8), distance);
            let camera = m.lw(gameplay.wrapping_add(0x28));
            m.sw(camera.wrapping_add(0xc), 0);
            camera
        };
        match mode {
            0 => {
                let x = slide_offset(slide).wrapping_add(0x50);
                call(m, 0x800a_7118, [x, 0x84, 0, 0]);
                for (offset, value) in [(0x18u32, 0u16), (0x1a, 0x1000), (0x1c, 0)] {
                    let record = self::record(m);
                    let slot = m.lh(record.wrapping_add(2)) as i32 as u32;
                    let entity = entity(m, slot);
                    m.sh(entity.wrapping_add(offset), value);
                }
                finish(m, 0xfff0_0000)
            }
            1 => {
                turn_model(m);
                call(m, 0x800a_7118, [0xa0, 0xf0, 0, 0]);
                let hub = hub(m);
                for offset in [0x18u32, 0x1a, 0x1c] {
                    let entity = entity(m, empty);
                    let angle = m.lhu(hub.wrapping_add(0x100 + offset - 0x18));
                    m.sh(entity.wrapping_add(offset), angle);
                }
                let distance = m.lw(hub.wrapping_add(0xfc));
                finish(m, distance)
            }
            2 => {
                let x = slide_offset(slide).wrapping_add(0x50);
                call(m, 0x800a_7118, [x, 0x84, 0, 0]);
                for (offset, value) in [(0x18u32, 0u16), (0x1a, 0x1000), (0x1c, 0)] {
                    let entity = entity(m, 2);
                    m.sh(entity.wrapping_add(offset), value);
                }
                finish(m, 0xfff0_0000)
            }
            _ => 2,
        }
    })
}

/// The DR_AREA and DR_MODE links of 80108874: `prim` at the gameplay
/// cursor (+0x14, advanced by 0xC) into the gameplay ordering table
/// `[+0x10] + entry`.
fn link_gameplay(m: &mut Machine, prim: u32, entry: u32) -> u32 {
    let gameplay = m.gw(GAMEPLAY);
    let ot = m.lw(gameplay.wrapping_add(0x10)).wrapping_add(entry);
    add_prim_masked(m, ot, prim)
}

/// 80108874: draw the viewer: the light and view matrices (gameplay +0x4C,
/// +0x8C, back color +0xAC..+0xB4); the camera's screen 320x480 at H 250;
/// in `mode` 1 a drawing area beside the panel slide (visual slot 1) in
/// entry 63; a DR_MODE in entry 1, the model pass (8010817C of gameplay
/// +0x244 into the gameplay table and cursor), a DR_MODE (dither) in entry
/// 0 and in mode 1 the full-screen drawing area there too. Returns the
/// original's last `v0`.
pub fn draw_viewer(m: &mut Machine, mode: u32) -> u32 {
    framed(m, 0x258, |m, sp| {
        let view = m.gw(GAMEPLAY).wrapping_add(0x4c);
        call(m, 0x800a_7048, [view, 0, 0, 0]);
        let view = m.gw(GAMEPLAY).wrapping_add(0x4c);
        call(m, 0x800a_70a8, [view, 0, 0, 0]);
        let gameplay = m.gw(GAMEPLAY);
        let r = m.lw(gameplay.wrapping_add(0xac));
        let g = m.lw(gameplay.wrapping_add(0xb0));
        let b = m.lw(gameplay.wrapping_add(0xb4));
        call(m, 0x800a_70f8, [r, g, b, 0]);
        let colors = m.gw(GAMEPLAY).wrapping_add(0x8c);
        call(m, 0x800a_7078, [colors, 0, 0, 0]);
        let view = m.gw(GAMEPLAY).wrapping_add(0x4c);
        call(m, 0x800a_7048, [view, 0, 0, 0]);
        let view = m.gw(GAMEPLAY).wrapping_add(0x4c);
        call(m, 0x800a_70a8, [view, 0, 0, 0]);
        let gameplay = m.gw(GAMEPLAY);
        for (offset, value) in [(0x20u32, 0x140u16), (0x22, 0x1e0), (0x24, 0xfa)] {
            let camera = m.lw(gameplay.wrapping_add(0x28));
            m.sh(camera.wrapping_add(offset), value);
        }
        if mode == 1 {
            let slide = call(m, VISUAL_VALUE, [1, 0, 0, 0]);
            let gameplay = m.gw(GAMEPLAY);
            let area = m.lw(gameplay.wrapping_add(0x14));
            m.sw(gameplay.wrapping_add(0x14), area.wrapping_add(0xc));
            let rect = sp + 0x220;
            m.sh(rect + 2, 0x3a);
            m.sh(rect + 4, 0x9b);
            m.sh(rect + 6, 0x93);
            m.sh(rect, slide_offset(slide).wrapping_add(0x17) as u16);
            call(m, 0x800a_8f20, [area, rect, 0, 0]);
            link_gameplay(m, area, 0xfc);
        }
        let gameplay = m.gw(GAMEPLAY);
        let prim = m.lw(gameplay.wrapping_add(0x14));
        m.sw(gameplay.wrapping_add(0x14), prim.wrapping_add(0xc));
        m.sw(sp + 0x10, 0);
        call(m, 0x800a_c7f8, [prim, 0, 0, 0]);
        link_gameplay(m, prim, 4);
        let gameplay = m.gw(GAMEPLAY);
        let ot = m.lw(gameplay.wrapping_add(0x10));
        let cursor = m.lw(gameplay.wrapping_add(0x14));
        let prim = super::ov2_p6_models::model_pass(
            m,
            gameplay.wrapping_add(0x244),
            ot.wrapping_add(4),
            cursor,
            sp + 0x18,
        );
        let gameplay = m.gw(GAMEPLAY);
        m.sw(gameplay.wrapping_add(0x14), prim.wrapping_add(0xc));
        m.sw(sp + 0x10, 0);
        call(m, 0x800a_c7f8, [prim, 0, 1, 0]);
        let mut v0 = link_gameplay(m, prim, 0);
        if mode == 1 {
            let gameplay = m.gw(GAMEPLAY);
            let area = m.lw(gameplay.wrapping_add(0x14));
            m.sw(gameplay.wrapping_add(0x14), area.wrapping_add(0xc));
            let rect = sp + 0x228;
            m.sh(rect, 0);
            m.sh(rect + 2, 0);
            m.sh(rect + 4, 0x140);
            m.sh(rect + 6, 0x1e0);
            call(m, 0x800a_8f20, [area, rect, 0, 0]);
            v0 = link_gameplay(m, area, 0);
        }
        v0
    })
}

// ---- 80108B7C .. 80109660: the viewer's steps ----------------------------------------------

/// The viewer's ordering table (gameplay +0x10, 64 entries) cleared and
/// spliced into the hub's entry `[[0x80112DC4]+8] + 0xC`.
fn splice_table(m: &mut Machine) {
    let gameplay = m.gw(GAMEPLAY);
    let ot = m.lw(gameplay.wrapping_add(0x10));
    call(m, 0x800a_8680, [ot, 0x40, 0, 0]);
    let gameplay = m.gw(GAMEPLAY);
    let hub = hub(m);
    let ot = m.lw(gameplay.wrapping_add(0x10));
    let entry = m.lw(hub.wrapping_add(8)).wrapping_add(0xc);
    let first = m.lw(ot);
    let link = m.lw(entry);
    m.sw(ot, (first & 0xff00_0000) | (link & 0x00ff_ffff));
    let ot = m.lw(gameplay.wrapping_add(0x10)).wrapping_add(0xfc);
    let entry = m.lw(hub.wrapping_add(8)).wrapping_add(0xc);
    let link = m.lw(entry);
    m.sw(entry, (link & 0xff00_0000) | (ot & 0x00ff_ffff));
}

/// 80108B7C (not reached): open the viewer: its block (0x4008 bytes, model
/// 0, a load pending), 800FC148 and 800FC488, the first model (80108298),
/// the hub's backdrop (800ECE38), the panels' visual slots, step 1;
/// entities 0 and 2 reset (kind 2, entity 0 lit, at rest) and the camera
/// on entity 1 (+0xC4). Returns 1.
pub fn open_viewer(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let block = arena::alloc(m, 0x4008);
        m.sw(VIEWER, block);
        m.sb(block.wrapping_add(0x4000), 0);
        for (offset, value) in [(0x4001u32, 1u8), (0x4002, 0), (0x4003, 0)] {
            let block = viewer(m);
            m.sb(block.wrapping_add(offset), value);
        }
        call(m, 0x800f_c148, [0; 4]);
        call(m, 0x800f_c488, [0; 4]);
        load_model(m);
        let hub = hub(m);
        let backdrop = m.lw(hub.wrapping_add(0xa4));
        call(m, 0x800e_ce38, [backdrop, 0x1_0000, 0, 0]);
        call(m, VISUAL_FADE, [2, 0x14, 0, 0]);
        call(m, VISUAL_FLAGS, [2, 0x10_0000, 0, 0]);
        for kind in [9u32, 0xc, 0xa, 0xe, 8] {
            call(m, VISUAL_FADE, [0, kind, 0, 0]);
        }
        call(m, VISUAL_FLAGS, [0, 0x5701, 0, 0]);
        set_step(m, 1);
        call(m, SET_VISUAL, [2, 0, 0, 0]);
        call(m, SET_VISUAL_STYLE, [1, 0, 0, 0]);
        let first = entity(m, 0);
        m.sw(first, 0);
        let third = entity(m, 2);
        m.sw(third, 0);
        let first = entity(m, 0);
        m.sb(first.wrapping_add(4), 2);
        let third = entity(m, 2);
        m.sb(third.wrapping_add(4), 2);
        let first = entity(m, 0);
        let side = m.lbu(first.wrapping_add(5)) | 1;
        m.sb(first.wrapping_add(5), side);
        for slot in [0u32, 2] {
            for offset in [8u32, 0xc, 0x10] {
                let entity = entity(m, slot);
                m.sw(entity.wrapping_add(offset), 0);
            }
            for (offset, value) in [(0x18u32, 0u16), (0x1a, 0x1000), (0x1c, 0x1000)] {
                let entity = entity(m, slot);
                m.sh(entity.wrapping_add(offset), value);
            }
        }
        let gameplay = m.gw(GAMEPLAY);
        let camera = m.lw(gameplay.wrapping_add(0x28));
        m.sw(camera.wrapping_add(0xc4), 1);
        1
    })
}

/// 80108D90 (not reached): wait for the model (80108334); when it is in,
/// step 2 and the panels shown; entity 2 visible (flags 0x1C000), entity
/// 0 off, the panels' styles cleared. Returns the last call's v0.
pub fn viewer_loading(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        if finish_model(m) == 1 {
            set_step(m, 2);
            set_visuals(m, &[1, 3, 0xb, 5, 8], 1, 0, 0x19);
        }
        let third = entity(m, 2);
        let flags = m.lw(third) | 0x1_c000;
        m.sw(third, flags);
        let first = entity(m, 0);
        m.sw(first, 0);
        let mut v0 = 0;
        for slot in [1u32, 3, 2, 4] {
            v0 = call(m, SET_VISUAL_STYLE, [slot, 0, 0, 0]);
        }
        v0
    })
}

/// 80108E88 (not reached): the viewer's input: the panels' styles by the
/// held LEFT/RIGHT and the hub object's +0x6A; UP/DOWN pressed pick the
/// next/previous model (0..0x30, reloaded); a confirm press loads the
/// model's scene (when it has one) and leaves (+0x4003 = 1, step 3); then
/// the load (80108334), the spliced table, the model (801085F8(0),
/// 80108874(1)); a cancel press leaves (+0x4003 = 2, step 3). Returns the
/// original's last `v0`.
pub fn viewer_select(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        call(m, SET_VISUAL_STYLE, [1, 0, 0, 0]);
        let context = m.gw(CONTEXT);
        let left = u32::from(m.lw(context.wrapping_add(0x128)) & 4 != 0);
        call(m, SET_VISUAL_STYLE, [2, left, 0, 0]);
        let context = m.gw(CONTEXT);
        let right = u32::from(m.lw(context.wrapping_add(0x128)) & 8 != 0);
        call(m, SET_VISUAL_STYLE, [3, right, 0, 0]);
        let hub = hub(m);
        let current = m.lw(hub.wrapping_add(0x16c));
        let two = u32::from(m.lh(current.wrapping_add(0x6a)) == 2);
        call(m, SET_VISUAL_STYLE, [4, two, 0, 0]);
        let context = m.gw(CONTEXT);
        if m.lw(context.wrapping_add(0x12c)) & 8 != 0 {
            let viewer = viewer(m);
            let number = m.lbu(viewer.wrapping_add(0x4000));
            if number < 0x30 {
                m.sb(viewer.wrapping_add(0x4000), number + 1);
            }
            load_model(m);
            set_visuals(m, &[2, 8], 3, u32::MAX, u32::MAX);
        }
        let context = m.gw(CONTEXT);
        if m.lw(context.wrapping_add(0x12c)) & 4 != 0 {
            let viewer = viewer(m);
            let number = m.lbu(viewer.wrapping_add(0x4000));
            if number != 0 {
                m.sb(viewer.wrapping_add(0x4000), number - 1);
            }
            load_model(m);
            set_visuals(m, &[2, 8], 3, u32::MAX, u32::MAX);
        }
        if press(m, 0x10, 0x2f).0 {
            let record = record(m);
            let scene = m.lh(record.wrapping_add(4)) as i32 as u32;
            if scene != 0 {
                request(m, scene);
            }
            set_step(m, 3);
            set_visuals(m, &[2, 8, 1], 3, u32::MAX, u32::MAX);
            call(m, SET_VISUAL, [3, 3, u32::MAX, u32::MAX]);
            set_visuals(m, &[0xb, 5], 3, u32::MAX, u32::MAX);
            let viewer = viewer(m);
            m.sb(viewer.wrapping_add(0x4003), 1);
        }
        finish_model(m);
        splice_table(m);
        place_model(m, 0);
        draw_viewer(m, 1);
        let (pressed, v0) = press(m, 0x20, 0x5f);
        if !pressed {
            return v0;
        }
        set_step(m, 3);
        set_visuals(m, &[8, 2, 1], 3, u32::MAX, u32::MAX);
        call(m, SET_VISUAL, [3, 3, u32::MAX, u32::MAX]);
        set_visuals(m, &[0xb, 5], 3, u32::MAX, u32::MAX);
        let viewer = viewer(m);
        m.sb(viewer.wrapping_add(0x4003), 2);
        2
    })
}

/// 801092BC (not reached): leaving: while visual slot 2 still moves, keep
/// drawing (the spliced table, 801085F8(0), 80108874(1)); then back (step
/// 4, slot 2 flags 0x200000) or out (800DEBB8(0, 1), the sub-mode state
/// cleared, the viewer block freed). Returns the last call's v0 (2 when
/// neither).
pub fn viewer_leave(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        if call(m, VISUAL_MODE, [2, 0, 0, 0]) != 0 {
            splice_table(m);
            place_model(m, 0);
            return draw_viewer(m, 1);
        }
        let viewer = viewer(m);
        match m.lbu(viewer.wrapping_add(0x4003)) {
            1 => {
                set_step(m, 4);
                call(m, VISUAL_FLAGS, [2, 0x20_0000, 0, 0])
            }
            2 => {
                call(m, VISUAL_RESET, [0, 1, 0, 0]);
                let mode = m.lw(MODE);
                m.sb(mode, 0);
                let mode = m.lw(MODE);
                m.sb(mode.wrapping_add(1), 0);
                let viewer = self::viewer(m);
                arena::reset(m, viewer)
            }
            _ => 2,
        }
    })
}

/// 801093E8 (not reached): the model's scene: without one, image 0
/// (80108478) and step 5; once loaded (context +0x44 = 0), its files
/// (80027338: +0 packed data, +4 header, +8 backdrop, +0xC arena mark)
/// unpacked into a 0x10000-byte buffer and relocated (80027380 of each
/// entry), the backdrop (800ECE38), the gameplay model table, entity 0
/// visible, entity 2 off, step 5. Returns the original's last `v0`.
pub fn viewer_scene(m: &mut Machine) -> u32 {
    framed(m, 0x28, |m, _| {
        let record = record(m);
        if m.lh(record.wrapping_add(4)) == 0 {
            set_step(m, 5);
            return upload_image(m, 0);
        }
        let context = m.gw(CONTEXT);
        let loading = m.lw(context.wrapping_add(0x44));
        if loading != 0 {
            return loading;
        }
        let resources = m.gw(RESOURCES);
        let buffer = m.lw(resources.wrapping_add(0x60));
        let viewer = viewer(m);
        m.sw(viewer.wrapping_add(0x4004), buffer);
        let files = call(m, 0x8002_7338, [buffer, 0, 0, 0]);
        let mark = m.lw(files.wrapping_add(0xc));
        let resources = m.gw(RESOURCES);
        arena::reset(m, mark);
        let backdrop = m.lw(files.wrapping_add(8));
        call(m, 0x800e_ce38, [backdrop, 0x1_0000, 0, 0]);
        let backdrop = m.lw(files.wrapping_add(8));
        arena::reset(m, backdrop);
        let unpacked = arena::alloc(m, 0x1_0000);
        let packed = m.lw(files);
        call(m, 0x800a_fd08, [packed, unpacked, 0, 0]);
        let table = call(m, 0x8002_7338, [unpacked, 0, 0, 0]);
        m.sw(resources.wrapping_add(8), table);
        let models = resources.wrapping_add(8);
        let mut index = 0u32;
        if (m.lw(unpacked) as i32) > 0 {
            loop {
                let table = m.lw(models);
                let entry = m.lw((index << 2).wrapping_add(table));
                call(m, 0x8002_7380, [entry, entry.wrapping_add(4), 6, 0]);
                index += 1;
                if (index as i32) >= (m.lw(unpacked) as i32) {
                    break;
                }
            }
        }
        let header = m.lw(files.wrapping_add(4));
        let resources = m.gw(RESOURCES);
        m.sw(models.wrapping_add(4), header);
        let gameplay = m.gw(GAMEPLAY);
        let table = m.gw(ENTITIES);
        let target = m.lw(gameplay.wrapping_add(0x250));
        let set = m.lw(resources.wrapping_add(8));
        m.sw(target.wrapping_add(0xc), set);
        let first = m.lw(table);
        let flags = m.lw(first) | 0x1_c000;
        m.sw(first, flags);
        let third = m.lw(table.wrapping_add(8));
        m.sw(third, 0);
        set_step(m, 5);
        5
    })
}

/// 80109588 (not reached): the scene view: the spliced table, then a
/// cancel press goes to step 6, else the model is drawn (801085F8(1),
/// 80108874(0)). Returns the original's last `v0`.
pub fn viewer_idle(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        splice_table(m);
        if press(m, 0x20, 0x5f).0 {
            set_step(m, 6);
            return 6;
        }
        place_model(m, 1);
        draw_viewer(m, 0)
    })
}

/// 80109660 (not reached): back from the scene view: slot 2 flags
/// 0x100000; with a scene, entity 0 off and entity 2 visible, the scene
/// freed, the view reset and the model reloaded (step 1); else the panels
/// shown (step 2). Returns the step.
pub fn viewer_back(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        call(m, VISUAL_FLAGS, [2, 0x10_0000, 0, 0]);
        let viewer = viewer(m);
        let record = record(m);
        if m.lh(record.wrapping_add(4)) != 0 {
            let first = entity(m, 0);
            m.sw(first, 0);
            let third = entity(m, 2);
            let flags = m.lw(third) | 0x1_c000;
            m.sw(third, flags);
            let mark = m.lw(viewer.wrapping_add(0x4004));
            arena::reset(m, mark);
            let hub = hub(m);
            m.sw(hub.wrapping_add(0xfc), 0xfff0_0000);
            m.sh(hub.wrapping_add(0x100), 0xc00);
            m.sh(hub.wrapping_add(0x102), 0x2800);
            m.sh(hub.wrapping_add(0x104), 0);
            load_model(m);
            set_step(m, 1);
            1
        } else {
            set_visuals(m, &[2, 1, 3, 0xb, 5, 8], 1, 0, 0x19);
            set_step(m, 2);
            2
        }
    })
}
