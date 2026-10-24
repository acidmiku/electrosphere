//! JP overlay 2: the story scene's sequencing (its script, phases and
//! set-up), translated from the JP instructions.
//!
//! The scene runs phase lists from 0x80112BE0 by phase `+0x9D4` and step
//! `+0x9D5` (80106A34): phase 0 = [80106000, 8010606C] (the opening camera
//! until hub frame 0x79 or a button), phase 1 = [801062B8, 8010655C,
//! 801065C8] (the timed script), phase 2 = [8010673C, 80106780]. The
//! script (`+0x9C8`, 16-byte commands {time, opcode, slot, arguments})
//! runs through the command table 0x80112B9C at the hub frame
//! (`[0x80112DC4]+0x50`) of each command.
use super::ov2_p6::{HUB, SCENE, call, framed, lwc2, swc2};
use super::ov2_p6_scene as scene;
use crate::{
    arena,
    common::CONTEXT,
    machine::{Machine, SmallData},
    sdk,
};

/// The script command routines by opcode.
pub const COMMANDS: u32 = 0x8011_2b9c;
/// The phase lists by phase.
pub const PHASES: u32 = 0x8011_2be0;
/// 80102650 and 80105E18: the 8-byte `jr ra; nop` stubs of opcodes 0 and
/// 8, which the JP function map lacks (JP_PORT_PLAN §3, uncovered stubs):
/// calling them has no effect.
const NO_OP_STUBS: [u32; 2] = [0x8010_2650, 0x8010_5e18];
/// 8002DB4C: start a sound.
const START_SOUND: u32 = 0x8002_db4c;
/// 800DEE08 / 800DEF94 / 800DECA8 / 800DECD4 (another part): visual slots.
const SET_VISUAL: u32 = 0x800d_ee08;
const SET_VISUAL_STYLE: u32 = 0x800d_ef94;
const VISUAL_FADE: u32 = 0x800d_eca8;
const VISUAL_CLOSE: u32 = 0x800d_ecd4;
/// 800F33A8 / 800F33D0 / 800F4AE0 (another part): text position, color,
/// formatted print.
const TEXT_POSITION: u32 = 0x800f_33a8;
const TEXT_COLOR: u32 = 0x800f_33d0;
const TEXT_PRINT: u32 = 0x800f_4ae0;
/// The marker's format string.
const MARKER_FORMAT: u32 = 0x800c_d77c;

fn hub(m: &mut Machine) -> u32 {
    m.lw(HUB)
}
fn block(m: &mut Machine) -> u32 {
    m.lw(SCENE)
}
fn hub_frame(m: &mut Machine) -> u32 {
    let hub = hub(m);
    m.lw(hub.wrapping_add(0x50))
}
/// A `jalr` of a command or phase routine (the map's missing no-op stubs
/// have no effect).
fn call_routine(m: &mut Machine, target: u32, args: [u32; 4]) -> u32 {
    if NO_OP_STUBS.contains(&target) {
        return target;
    }
    call(m, target, args)
}
/// Whether a confirm press (pressed bit 0x10, none of the held bits 0x2F)
/// happened: (decision, the original's last `v0`).
fn confirm(m: &mut Machine) -> (bool, u32) {
    let context = m.gw(CONTEXT);
    let pressed = m.lw(context.wrapping_add(0x12c)) & 0x10;
    if pressed == 0 {
        return (false, 0);
    }
    let held = m.lw(context.wrapping_add(0x128)) & 0x2f;
    (held == 0, held)
}

// ---- 80105C20 .. 80105E20: the marker, the pan and the script ------------------------------

/// 80105C20: the marker (+0x9C4 >= 0): RTPS of its position (+0x9BC)
/// with the view matrix +0x50, then its time as minutes and seconds
/// (format 0x800CD77C) at the projected position (x - 10) in white.
/// Returns the packet pointer unchanged.
pub fn draw_marker(m: &mut Machine, packet: u32) -> u32 {
    framed(m, 0x38, |m, sp| {
        let block = block(m);
        if (m.lw(block.wrapping_add(0x9c4)) as i32) < 0 {
            return packet;
        }
        super::ov2_p6::load_rot_trans(m, block.wrapping_add(0x50));
        lwc2(m, 0, block.wrapping_add(0x9bc));
        lwc2(m, 1, block.wrapping_add(0x9c0));
        m.gte.rtps(true, false);
        swc2(m, 14, sp + 0x10);
        swc2(m, 8, sp + 0x18);
        let flag = m.gte.cfc2(31);
        m.sw(sp + 0x1c, flag);
        let sz = m.gte.mfc2(19) as i32 >> 2;
        m.sw(sp + 0x20, sz as u32);
        let x = (m.lh(sp + 0x10) as i32 as u32).wrapping_sub(0xa);
        let y = m.lh(sp + 0x12) as i32 as u32;
        call(m, TEXT_POSITION, [x, y, 0, 0]);
        call(m, TEXT_COLOR, [0x80, 0x80, 0x80, 0]);
        let block = self::block(m);
        let time = m.lw(block.wrapping_add(0x9c4)) as i32;
        let high = ((i64::from(time) * i64::from(0x8888_8889u32 as i32)) >> 32) as i32;
        let minutes = (high.wrapping_add(time) >> 5).wrapping_sub(time >> 31);
        let seconds = time.wrapping_sub(minutes.wrapping_mul(60));
        call(
            m,
            TEXT_PRINT,
            [MARKER_FORMAT, minutes as u32, seconds as u32, 0],
        );
        packet
    })
}

/// 80105D54 (PAL 800EBC14, changed): move the looked-at point one frame
/// along the pan heading while the pan lasts. Returns 1 when it moved,
/// else 0.
pub fn tick_pan(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        let block = block(m);
        if (m.lw(block.wrapping_add(0x78)) as i32) <= 0 {
            return 0;
        }
        let angle = m.lh(block.wrapping_add(0x7e)) as i32 as u32;
        let sine = sdk::rsin(m, angle);
        let block = self::block(m);
        let speed = m.lw(block.wrapping_add(0x74));
        let dx = super::ov2_p6::divide_pow2(speed.wrapping_mul(sine) as i32, 12) as u32;
        let angle = m.lh(block.wrapping_add(0x7e)) as i32 as u32;
        let cosine = sdk::rcos(m, angle);
        let block = self::block(m);
        let speed = m.lw(block.wrapping_add(0x74));
        let dz = super::ov2_p6::divide_pow2(speed.wrapping_mul(cosine) as i32, 12) as u32;
        let x = m.lw(block.wrapping_add(0x20));
        let frames = m.lw(block.wrapping_add(0x78));
        m.sw(block.wrapping_add(0x20), x.wrapping_add(dx));
        let z = m.lw(block.wrapping_add(0x28));
        m.sw(block.wrapping_add(0x78), frames.wrapping_sub(1));
        m.sw(block.wrapping_add(0x28), z.wrapping_add(dz));
        1
    })
}

/// 80105E20: the script step: every command due at the hub frame runs
/// (command table 0x80112B9C, `a0` its time; `a1` 0x80110000 and `a2`/`a3`
/// as the original leaves them for the first), then the pan (80105D54)
/// and the objects (801030E0) advance and the hub frame counts. Returns
/// the new hub frame.
pub fn run_script(m: &mut Machine, a2: u32, a3: u32) -> u32 {
    framed(m, 0x20, |m, _| {
        let block = block(m);
        let cmd = m.lw(block.wrapping_add(0x9c8));
        let time = m.lh(cmd) as i32 as u32;
        if time == hub_frame(m) {
            loop {
                let block = self::block(m);
                let cmd = m.lw(block.wrapping_add(0x9c8));
                let opcode = u32::from(m.lbu(cmd.wrapping_add(2)));
                let time = m.lh(cmd) as i32 as u32;
                let routine = m.lw((opcode << 2).wrapping_add(COMMANDS));
                call_routine(m, routine, [time, 0x8011_0000, a2, a3]);
                let block = self::block(m);
                let cmd = m.lw(block.wrapping_add(0x9c8));
                m.sw(block.wrapping_add(0x9c8), cmd.wrapping_add(0x10));
                let next = m.lh(cmd.wrapping_add(0x10)) as i32 as u32;
                if next != hub_frame(m) {
                    break;
                }
            }
        }
        tick_pan(m);
        scene::step_objects(m);
        let hub = hub(m);
        let frame = m.lw(hub.wrapping_add(0x50)).wrapping_add(1);
        m.sw(hub.wrapping_add(0x50), frame);
        frame
    })
}

// ---- 80105F04 .. 80106000: the set-up ------------------------------------------------------

/// 80105F04: allocate the 32 objects (0x20 bytes each; frames 0, state
/// and screen position -1) into +0x9DC. Returns 0 (the loop test).
pub fn allocate_objects(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        for slot in 0..0x20u32 {
            let object = arena::alloc(m, 0x20);
            let block = block(m);
            m.sw(block.wrapping_add(slot << 2).wrapping_add(0x9dc), object);
            m.sh(object.wrapping_add(4), 0);
            m.sh(object.wrapping_add(6), 0);
            m.sh(object.wrapping_add(2), 0xffff);
            m.sh(object.wrapping_add(0x1c), 0xffff);
            m.sh(object.wrapping_add(0x1e), 0xffff);
        }
        0
    })
}

/// 80105F78: no labels (+0x9CC = 0); allocate the 8 label records (4
/// bytes each) into +0xA5C. Returns 0 (the loop test).
pub fn allocate_labels(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        let block = block(m);
        m.sh(block.wrapping_add(0x9cc), 0);
        for index in 0..8u32 {
            let record = arena::alloc(m, 4);
            let block = self::block(m);
            m.sw(block.wrapping_add(index << 2).wrapping_add(0xa5c), record);
        }
        0
    })
}

/// 80105FD8 (PAL 800DC938, changed): free the arena back to the scene
/// block (80027538). Returns its v0.
pub fn free_scene(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let block = block(m);
        arena::reset(m, block)
    })
}

/// 80106000 (phase 0, step 0): step 1, hub frame 0; visual slot 1 fades
/// in (800DEE08(1, 1, 0, 120)), slots 2..4 get style 0. Returns 0 (the
/// loop test).
pub fn phase_open(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let block = block(m);
        m.sb(block.wrapping_add(0x9d5), 1);
        let hub = hub(m);
        m.sw(hub.wrapping_add(0x50), 0);
        call(m, SET_VISUAL, [1, 1, 0, 0x78]);
        for slot in 2..5u32 {
            call(m, SET_VISUAL_STYLE, [slot, 0, 0, 0]);
        }
        0
    })
}

/// 8010606C (phase 0, step 1): the opening camera (80102184); +0x9D9 by
/// the hub frame (1 before 0x28, 2 before 0x10E, else 0) and the title
/// style +0x9D8 (0 before 0x1E, 1..4 every 5 frames up to 0x37, then 0)
/// into visual slot 1; at hub frame 0x79, or on a confirm press, the next
/// phase (a press also fades slots out and sets the hub's +0x1A4 to
/// 0x190). Returns the new hub frame.
pub fn phase_opening(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        scene::update_camera(m);
        let frame = hub_frame(m) as i32;
        let wide = if frame < 0x28 {
            1
        } else if frame < 0x10e {
            2
        } else {
            0
        };
        let block = block(m);
        m.sb(block.wrapping_add(0x9d9), wide);
        let frame = hub_frame(m) as i32;
        let title = match frame {
            f if f < 0x1e => 0,
            f if f < 0x23 => 1,
            f if f < 0x2d => 2,
            f if f < 0x32 => 3,
            f if f < 0x37 => 4,
            _ => 0,
        };
        let block = self::block(m);
        m.sb(block.wrapping_add(0x9d8), title);
        let block = self::block(m);
        let style = u32::from(m.lbu(block.wrapping_add(0x9d8)));
        call(m, SET_VISUAL_STYLE, [1, style, 0, 0]);
        let mut check = true;
        if (hub_frame(m) as i32) >= 0x79 {
            let block = self::block(m);
            let phase = m.lbu(block.wrapping_add(0x9d4));
            m.sb(block.wrapping_add(0x9d4), phase.wrapping_add(1));
            let block = self::block(m);
            m.sb(block.wrapping_add(0x9d5), 0);
            check = (hub_frame(m) as i32) < 0x79;
        }
        if check && confirm(m).0 {
            let block = self::block(m);
            let phase = m.lbu(block.wrapping_add(0x9d4));
            m.sb(block.wrapping_add(0x9d4), phase.wrapping_add(1));
            let block = self::block(m);
            m.sb(block.wrapping_add(0x9d5), 0);
            call(m, VISUAL_FADE, [0, 5, 0, 0]);
            call(m, VISUAL_FADE, [0, 7, 0, 0]);
            call(m, SET_VISUAL, [1, 2, 0x78, 0x78]);
            call(m, SET_VISUAL_STYLE, [1, 0, 0, 0]);
            call(m, SET_VISUAL_STYLE, [2, 0, 0, 0]);
            let hub = hub(m);
            m.sw(hub.wrapping_add(0x1a4), 0x190);
        }
        let hub = hub(m);
        let frame = m.lw(hub.wrapping_add(0x50)).wrapping_add(1);
        m.sw(hub.wrapping_add(0x50), frame);
        frame
    })
}

/// 801062B8 (phase 1, step 0): the script phase's set-up: 0x80112E2C/E,
/// the placed mask and the hub frame cleared, the next step, no title;
/// 800F7524(1, the context's scene number +0x38C); every object's
/// POLY_FT4 in both hub tables (code 0x2C, CLUT 0xF... tpage 0xF, gray
/// 0x80); per display buffer the label panel's three DR_MODEs (tpage 0,
/// 0x4C, 0xB) and the label sprites (SPRT 0x66 at (0x3C, 0x45) and SPRT
/// 0x64 at (0x38, 0xC)); then the camera, the objects and the labels.
/// Returns the labels' v0.
pub fn phase_setup(m: &mut Machine) -> u32 {
    framed(m, 0x40, |m, sp| {
        let block = block(m);
        let hub = hub(m);
        m.sh(0x8011_2e2e, 0);
        m.sh(0x8011_2e2c, 0);
        m.sw(block.wrapping_add(0x9d0), 0);
        m.sw(hub.wrapping_add(0x50), 0);
        let step = m.lbu(block.wrapping_add(0x9d5));
        m.sb(block.wrapping_add(0x9d5), step.wrapping_add(1));
        let block = self::block(m);
        m.sb(block.wrapping_add(0x9d8), 0);
        let context = m.gw(CONTEXT);
        let number = m.lh(context.wrapping_add(0x38c)) as i32 as u32;
        call(m, 0x800f_7524, [1, number, 0, 0]);
        for table in 0..2u32 {
            for slot in 0..0x20u32 {
                let hub = self::hub(m);
                let base = m.lw(hub.wrapping_add(table << 2).wrapping_add(0x38));
                let prim = base.wrapping_add(slot.wrapping_mul(40));
                let quad = prim.wrapping_add(0xb8c);
                m.sb(quad.wrapping_add(3), 9);
                m.sb(quad.wrapping_add(7), 0x2c);
                m.sh(prim.wrapping_add(0xba2), 0xf);
                m.sb(quad.wrapping_add(4), 0x80);
                m.sb(quad.wrapping_add(5), 0x80);
                m.sb(quad.wrapping_add(6), 0x80);
            }
        }
        let (mut sprites, mut modes) = (0x8d4u32, 0x924u32);
        for _ in 0..2 {
            let block = self::block(m);
            m.sw(sp + 0x10, 0);
            let mut sprite = block.wrapping_add(sprites);
            let mode = block.wrapping_add(modes);
            sdk::set_draw_mode(m, mode, 0, 0, 0, 0);
            let mode = mode.wrapping_add(0xc);
            m.sw(sp + 0x10, 0);
            sdk::set_draw_mode(m, mode, 0, 0, 0x4c, 0);
            m.sw(sp + 0x10, 0);
            sdk::set_draw_mode(m, mode.wrapping_add(0xc), 0, 0, 0xb, 0);
            for k in 0..2 {
                if k == 1 {
                    sprite = sprite.wrapping_add(0x14);
                }
                sprite_header(m, sprite, 0x66, 0x3c, 0x45);
            }
            modes = modes.wrapping_add(0x24);
            sprites = sprites.wrapping_add(0x28);
        }
        let mut offset = 0x96cu32;
        for _ in 0..2 {
            let block = self::block(m);
            let sprite = block.wrapping_add(offset);
            sprite_header(m, sprite, 0x64, 0x38, 0xc);
            sprite_header(m, sprite.wrapping_add(0x14), 0x64, 0x38, 0xc);
            offset = offset.wrapping_add(0x28);
        }
        scene::update_camera(m);
        scene::step_draw_objects(m);
        scene::draw_labels(m)
    })
}

/// A label sprite's fixed words: length 4, code `code`, gray 0x80, size
/// (`w`, `h`).
fn sprite_header(m: &mut Machine, sprite: u32, code: u8, w: u16, h: u16) {
    m.sb(sprite.wrapping_add(3), 4);
    m.sb(sprite.wrapping_add(7), code);
    m.sb(sprite.wrapping_add(4), 0x80);
    m.sb(sprite.wrapping_add(5), 0x80);
    m.sb(sprite.wrapping_add(6), 0x80);
    m.sh(sprite.wrapping_add(0x10), w);
    m.sh(sprite.wrapping_add(0x12), h);
}

/// 8010655C (phase 1, step 1): 800F7524(1, the scene number), the camera,
/// the objects and the labels; the hub's +0x14C = 180; the next step.
/// Returns the new step.
pub fn phase_start(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let context = m.gw(CONTEXT);
        let number = m.lh(context.wrapping_add(0x38c)) as i32 as u32;
        call(m, 0x800f_7524, [1, number, 0, 0]);
        scene::update_camera(m);
        scene::step_draw_objects(m);
        scene::draw_labels(m);
        let hub = hub(m);
        let block = block(m);
        m.sw(hub.wrapping_add(0x14c), 0xb4);
        let step = u32::from(m.lbu(block.wrapping_add(0x9d5))) + 1;
        m.sb(block.wrapping_add(0x9d5), step as u8);
        step
    })
}

/// 801065C8 (phase 1, step 2): the camera, the script step (80105E20),
/// the objects and the labels; while 8002B59C reports nothing and the
/// hub's +0x14C counts down, wait; else the hub text (8002D3F4 into
/// `+0x274`) and 800F787C/8002CAE4: at its end (-1), or on a confirm
/// press, the next phase with the labels hidden (+0x9D6), visual slots
/// closed and sound 0x5B (and 0x3C on a press). Returns the original's
/// last `v0`.
pub fn phase_script(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        scene::update_camera(m);
        // a2/a3 are whatever the camera's callees left; no command reads them
        run_script(m, 0, 0);
        scene::step_draw_objects(m);
        scene::draw_labels(m);
        let busy = call(m, 0x8002_b59c, [0; 4]);
        if busy == 0 {
            let hub = hub(m);
            let left = m.lw(hub.wrapping_add(0x14c));
            if (left as i32) >= 0 {
                m.sw(hub.wrapping_add(0x14c), left.wrapping_sub(1));
                return 0;
            }
        }
        let hub = hub(m);
        let target = m.lw(hub.wrapping_add(8));
        let cursor = m.lw(hub.wrapping_add(0x274));
        let cursor = call(m, 0x8002_d3f4, [target, cursor, 0, 0]);
        let hub = self::hub(m);
        m.sw(hub.wrapping_add(0x274), cursor);
        let state = call(m, 0x800f_787c, [0; 4]);
        let ended = call(m, 0x8002_cae4, [state, 0, 0, 0]) == u32::MAX;
        if !ended {
            let (pressed, v0) = confirm(m);
            if !pressed {
                return v0;
            }
        }
        let block = block(m);
        let phase = m.lbu(block.wrapping_add(0x9d4));
        m.sb(block.wrapping_add(0x9d4), phase.wrapping_add(1));
        let block = self::block(m);
        m.sb(block.wrapping_add(0x9d5), 0);
        let block = self::block(m);
        m.sb(block.wrapping_add(0x9d6), 1);
        call(m, VISUAL_CLOSE, [0, 5, 0, 0]);
        call(m, VISUAL_CLOSE, [0, 7, 0, 0]);
        call(m, SET_VISUAL, [1, 3, u32::MAX, u32::MAX]);
        call(m, START_SOUND, [0x5b, 0, 0, 0]);
        let (pressed, v0) = confirm(m);
        if !pressed {
            return v0;
        }
        call(m, START_SOUND, [0x3c, 0, 0, 0])
    })
}

/// 8010673C (PAL 800D725C, changed; phase 2, step 0): the next step, then
/// the camera, the objects and the labels. Returns the labels' v0.
pub fn phase_close(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let block = block(m);
        let step = m.lbu(block.wrapping_add(0x9d5));
        m.sb(block.wrapping_add(0x9d5), step.wrapping_add(1));
        scene::update_camera(m);
        scene::step_draw_objects(m);
        scene::draw_labels(m)
    })
}

/// 80106780 (PAL 800EE860, changed; phase 2, step 1): the camera, the
/// objects and the labels. Returns the labels' v0.
pub fn phase_idle(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        scene::update_camera(m);
        scene::step_draw_objects(m);
        scene::draw_labels(m)
    })
}

// ---- 801067B0 / 80106A34: the scene's load and frame ----------------------------------------

/// `[0x801127AC + n*12]`: the story scenes (+0 the scene's resource, -1
/// none).
const SCENES: u32 = 0x8011_27ac;
/// `[0x80112AAC + n*4]`: per scene +2: 1 stop the streams and 8002D6E0,
/// 0 stop the streams.
const SCENE_STREAMS: u32 = 0x8011_2aac;

/// 801067B0 (the story state's initialization): for the context's scene
/// number (+0x38C) with a scene resource: the scene block, its objects,
/// labels and pan (801020C0, 80105F04, 80105F78, 801020F4), the scene
/// resources (800F7514), the stream stops per 0x80112AAC (8002CC44(2, 0,
/// 0); 8002D6E0, an 8-byte stub returning 0 that the JP map lacks), the
/// hub's script (800ECEC8 of `[[+0x20]+0xC]`, the script `[+0x1C0]` into
/// +0x9C8, its leading set-up commands run), the colors, phase 0 and the
/// visual and text state. Without a scene the hub's +0x84 counts on.
/// Returns the original's last `v0`.
pub fn load_scene(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        let context = m.gw(CONTEXT);
        let number = m.lh(context.wrapping_add(0x38c)) as i32 as u32;
        let resource = m.lh(number.wrapping_mul(12).wrapping_add(SCENES)) as i32;
        if resource == -1 {
            let hub = hub(m);
            let next = m.lhu(hub.wrapping_add(0x84)).wrapping_add(1);
            m.sh(hub.wrapping_add(0x84), next);
            return 0;
        }
        scene::allocate_scene(m);
        allocate_objects(m);
        allocate_labels(m);
        scene::reset_pan(m);
        call(m, 0x800f_7514, [SCENES, 0, 0, 0]);
        let context = m.gw(CONTEXT);
        let number = m.lh(context.wrapping_add(0x38c)) as i32 as u32;
        let streams = m.lh((number << 2).wrapping_add(SCENE_STREAMS).wrapping_add(2)) as i32;
        if streams == 1 {
            call(m, 0x8002_cc44, [2, 0, 0, 0]);
            let hub = hub(m);
            let resources = m.lw(hub.wrapping_add(0x20));
            let _stream = m.lw(resources.wrapping_add(0x18));
            // 8002D6E0 (`jr ra; move v0, zero`, not in the JP map): no effect
        }
        let context = m.gw(CONTEXT);
        let number = m.lh(context.wrapping_add(0x38c)) as i32 as u32;
        if m.lh((number << 2).wrapping_add(SCENE_STREAMS).wrapping_add(2)) == 0 {
            call(m, 0x8002_cc44, [2, 0, 0, 0]);
        }
        let hub = hub(m);
        let resources = m.lw(hub.wrapping_add(0x20));
        let script = m.lw(resources.wrapping_add(0xc));
        call(m, 0x800e_cec8, [script, 0, 0, 0]);
        let hub = self::hub(m);
        let block = block(m);
        let script = m.lw(hub.wrapping_add(0x1c0));
        m.sw(block.wrapping_add(0x9c8), script);
        if m.lh(script) == -1 {
            loop {
                let block = self::block(m);
                let cmd = m.lw(block.wrapping_add(0x9c8));
                let opcode = u32::from(m.lbu(cmd.wrapping_add(2)));
                let time = m.lh(cmd) as i32 as u32;
                let routine = m.lw((opcode << 2).wrapping_add(COMMANDS));
                call_routine(m, routine, [time, 0, 0, 0]);
                let block = self::block(m);
                let cmd = m.lw(block.wrapping_add(0x9c8));
                m.sw(block.wrapping_add(0x9c8), cmd.wrapping_add(0x10));
                if m.lh(cmd.wrapping_add(0x10)) != -1 {
                    break;
                }
            }
        }
        let block = self::block(m);
        m.sb(block.wrapping_add(0x9ce), 0);
        scene::set_colors(m);
        let hub = self::hub(m);
        let block = self::block(m);
        let count = m.lhu(hub.wrapping_add(0x86)).wrapping_add(1);
        m.sh(hub.wrapping_add(0x86), count);
        m.sb(block.wrapping_add(0x9d4), 0);
        let block = self::block(m);
        m.sb(block.wrapping_add(0x9d5), 0);
        let block = self::block(m);
        m.sb(block.wrapping_add(0x9d6), 0);
        call(m, SET_VISUAL, [2, 0, 0, 0]);
        call(m, SET_VISUAL, [3, 0, 0, 0]);
        call(m, SET_VISUAL, [4, 0, 0, 0]);
        call(m, 0x800f_35b8, [7, 0, 0, 0]);
        call(m, 0x800f_33f8, [u32::MAX, 0, 0, 0]);
        call(m, 0x800f_34c8, [1, 1, 1, 0]);
        call(m, 0x800f_34b0, [1, 0, 0, 0]);
        call(m, 0x800f_3218, [0x100, 0, 0, 0])
    })
}

/// Copy the 8-byte RECT at `from` to `to` (the unaligned-copy idiom on
/// aligned words).
fn copy_rect(m: &mut Machine, from: u32, to: u32) {
    let first = m.lw(from);
    let second = m.lw(from.wrapping_add(4));
    m.sw(to, first);
    m.sw(to.wrapping_add(4), second);
}

/// A drawing area (SetDrawArea of the RECT `rect`) at the hub's packet
/// (`+0x274`, advanced by 0xC), added at the hub's entry `[+0x14]`.
fn draw_area(m: &mut Machine, rect: u32) {
    let hub = hub(m);
    let area = m.lw(hub.wrapping_add(0x274));
    let entry = m.lw(hub.wrapping_add(0x14));
    m.sw(hub.wrapping_add(0x274), area.wrapping_add(0xc));
    call(m, 0x800a_8f20, [area, rect, 0, 0]);
    super::ov2_p6::add_prim_masked(m, entry, area);
}

/// 80106A34 (the story state's frame; it runs on to 801071B4, so the map's
/// 80107154 is its tail): the phase step (phase lists 0x80112BE0 by
/// +0x9D4/+0x9D5; `a0` the scene block), the terrain (801048F0, unless
/// +0x9D6) and the marker (80105C20) into the hub's packet `+0x274`; a
/// drawing area (0x800CD784) and, during phases 1+ past step 0, the
/// minimap: a 3x6 TILE per placed object of kinds 0..8 and 10 (colors by
/// kind) around the looked-at point and the map POLY_FT4 (CLUT 0x5714,
/// tpage 5); a second drawing area (0x800CD78C); a skip press (pressed
/// 0x20, none of 0x5F held) stops the streams and leaves (the stars
/// appear, the hub's +0x1A4 eases, sounds 0x5B and 0x3B, +0x9D6 = 2), or
/// on the hub object's state 2 sets it to 3; in state 3 the scene ends
/// (800F7524(3, scene), the hub's +0x84/+0x86 or +0x7C..+0x7E and
/// 800DEBB8(0, 1), then 80105FD8). Returns the original's last `v0`.
pub fn story_frame(m: &mut Machine, a1: u32, a2: u32, a3: u32) -> u32 {
    framed(m, 0x40, |m, sp| {
        let block = block(m);
        let phase = u32::from(m.lbu(block.wrapping_add(0x9d4)));
        let list = m.lw((phase << 2).wrapping_add(PHASES));
        let step = u32::from(m.lbu(block.wrapping_add(0x9d5)));
        let routine = m.lw((step << 2).wrapping_add(list));
        call_routine(m, routine, [block, a1, a2, a3]);
        let block = self::block(m);
        if m.lbu(block.wrapping_add(0x9d6)) == 0 {
            let hub = hub(m);
            let packet = m.lw(hub.wrapping_add(0x274));
            let packet = super::ov2_p6_terrain::draw_terrain(m, packet);
            let hub = self::hub(m);
            m.sw(hub.wrapping_add(0x274), packet);
        }
        let hub = self::hub(m);
        let packet = m.lw(hub.wrapping_add(0x274));
        let packet = draw_marker(m, packet);
        let hub = self::hub(m);
        m.sw(hub.wrapping_add(0x274), packet);
        copy_rect(m, 0x800c_d784, sp + 0x10);
        draw_area(m, sp + 0x10);
        let block = self::block(m);
        if m.lbu(block.wrapping_add(0x9d4)) != 0 && m.lbu(block.wrapping_add(0x9d5)) != 0 {
            minimap(m, block);
        }
        copy_rect(m, 0x800c_d78c, sp + 0x18);
        draw_area(m, sp + 0x18);
        let context = m.gw(CONTEXT);
        if m.lw(context.wrapping_add(0x12c)) & 0x20 != 0
            && m.lw(context.wrapping_add(0x128)) & 0x5f == 0
        {
            skip(m);
        }
        let hub = self::hub(m);
        let current = m.lw(hub.wrapping_add(0x16c));
        if m.lh(current.wrapping_add(0x46)) != 3 {
            return 3;
        }
        let block = self::block(m);
        if m.lbu(block.wrapping_add(0x9d6)) == 1 {
            let context = m.gw(CONTEXT);
            let number = m.lh(context.wrapping_add(0x38c)) as i32 as u32;
            call(m, 0x800f_7524, [3, number, 0, 0]);
            let hub = self::hub(m);
            m.sh(hub.wrapping_add(0x84), 2);
            m.sh(hub.wrapping_add(0x86), 0);
        }
        let block = self::block(m);
        if m.lbu(block.wrapping_add(0x9d6)) == 2 {
            for offset in [0x7cu32, 0x7d, 0x7e] {
                let hub = self::hub(m);
                m.sb(hub.wrapping_add(offset), 0);
            }
            call(m, 0x800d_ebb8, [0, 1, 0, 0]);
            let context = m.gw(CONTEXT);
            let number = m.lh(context.wrapping_add(0x38c)) as i32 as u32;
            call(m, 0x800f_7524, [3, number, 0, 0]);
        }
        free_scene(m)
    })
}

/// 80106B90..80106EC0: the minimap of the placed objects.
fn minimap(m: &mut Machine, block: u32) {
    use super::ov2_p6::{add_prim_masked, divide_pow2, half};
    let hub = hub(m);
    let x = m.lw(block.wrapping_add(0x20)) as i32;
    let mut prim = m.lw(hub.wrapping_add(0x274));
    let entry = m.lw(hub.wrapping_add(0x14));
    let z = m.lw(block.wrapping_add(0x28)) as i32;
    let cx = divide_pow2(x, 7).wrapping_add(0x80);
    let cz = divide_pow2(z.wrapping_neg(), 7).wrapping_add(0x80);
    prim = prim.wrapping_add(0x10);
    let left = i32::from(cx as i16).wrapping_sub(0x80);
    for slot in 0..0x20u32 {
        let block = self::block(m);
        let mask = m.lw(block.wrapping_add(0x9d0));
        if ((mask as i32) >> (slot & 31)) & 1 == 0 {
            continue;
        }
        let object = m.lw(block.wrapping_add(slot << 2).wrapping_add(0x9dc));
        let kind = i32::from(m.lh(object));
        if kind == 9 || kind >= 0xb {
            continue;
        }
        let ox = divide_pow2(i32::from(m.lh(object.wrapping_add(0xc))), 7);
        let oz = i32::from(m.lh(object.wrapping_add(0x10))).wrapping_neg();
        let dx = half(ox.wrapping_sub(left));
        let tile = prim;
        m.sb(tile.wrapping_add(3), 3);
        m.sb(tile.wrapping_add(7), 0x60);
        m.sh(tile.wrapping_add(8), dx.wrapping_add(0x113) as u16);
        let y = divide_pow2(oz, 7).wrapping_sub(cz).wrapping_add(0x124);
        m.sh(tile.wrapping_add(0xa), y as u16);
        m.sh(tile.wrapping_add(0xc), 3);
        m.sh(tile.wrapping_add(0xe), 6);
        let kind = m.lh(object) as i32 as u32;
        prim = prim.wrapping_add(0x10);
        let color = match kind {
            0 | 1 | 10 => Some((0x38, 0x60, 0xc8)),
            2 | 3 => Some((0xff, 0xff, 0xff)),
            4 | 5 => Some((0xe8, 0xd0, 0x68)),
            6 | 7 => Some((0xf0, 0x50, 0x68)),
            8 => Some((0x90, 0xd0, 0xd0)),
            _ => None,
        };
        if let Some((r, g, b)) = color {
            m.sb(tile.wrapping_add(4), r);
            m.sb(tile.wrapping_add(5), g);
            m.sb(tile.wrapping_add(6), b);
        }
        let block = self::block(m);
        if m.lbu(block.wrapping_add(0x9d6)) == 0 {
            add_prim_masked(m, entry, tile);
        }
    }
    let hub = self::hub(m);
    let quad = prim;
    m.sw(hub.wrapping_add(0x274), prim);
    m.sb(quad.wrapping_add(3), 9);
    m.sb(quad.wrapping_add(7), 0x2c);
    let shift = half(i32::from(cx as i16)) as u32;
    let x0 = 0x113u32.wrapping_sub(shift);
    let x1 = 0x193u32.wrapping_sub(shift);
    let y0 = 0xa4u32.wrapping_sub(cz as u32);
    m.sh(quad.wrapping_add(0xa), y0 as u16);
    m.sh(quad.wrapping_add(0x12), y0 as u16);
    let y1 = 0x1a4u32.wrapping_sub(cz as u32);
    m.sh(quad.wrapping_add(0x1a), y1 as u16);
    m.sh(quad.wrapping_add(0x22), y1 as u16);
    m.sh(quad.wrapping_add(0x16), 5);
    for (offset, value) in [
        (4u32, 0x80u8),
        (5, 0x80),
        (6, 0x80),
        (0xc, 0),
        (0xd, 0),
        (0x14, 0x80),
        (0x15, 0),
        (0x1c, 0),
        (0x1d, 0x80),
        (0x24, 0x80),
        (0x25, 0x80),
    ] {
        m.sb(quad.wrapping_add(offset), value);
    }
    let block = self::block(m);
    m.sh(quad.wrapping_add(8), x0 as u16);
    m.sh(quad.wrapping_add(0x10), x1 as u16);
    m.sh(quad.wrapping_add(0x18), x0 as u16);
    m.sh(quad.wrapping_add(0x20), x1 as u16);
    m.sh(quad.wrapping_add(0xe), 0x5714);
    if m.lbu(block.wrapping_add(0x9d6)) == 0 {
        add_prim_masked(m, entry, quad);
    }
    let hub = self::hub(m);
    m.sw(hub.wrapping_add(0x274), prim.wrapping_add(0x28));
}

/// 80106F78..801070D0: the skip.
fn skip(m: &mut Machine) {
    call(m, 0x8002_cc44, [0xff, 0, 0, 0]);
    let hub = hub(m);
    let current = m.lw(hub.wrapping_add(0x16c));
    if m.lh(current.wrapping_add(0x46)) == 2 {
        m.sh(current.wrapping_add(0x46), 3);
        call(m, SET_VISUAL, [1, 0, 0, 0]);
    } else {
        super::ov2_p6_map::stars_appear(m);
        call(m, SET_VISUAL, [1, 3, u32::MAX, u32::MAX]);
        let block = block(m);
        if m.lbu(block.wrapping_add(0x9d4)) != 0 {
            let hub = self::hub(m);
            m.sw(hub.wrapping_add(0x1a4), 0x1d);
        } else {
            let hub = self::hub(m);
            let current = m.lw(hub.wrapping_add(0x16c));
            match m.lh(current.wrapping_add(0x46)) {
                0 => m.sw(hub.wrapping_add(0x1a4), 0),
                1 => {
                    let value = m.lw(hub.wrapping_add(0x1a4)).wrapping_sub(0x102);
                    let eased = super::ov2_p6::divide_pow2(value.wrapping_mul(29) as i32, 6) as u32;
                    m.sw(hub.wrapping_add(0x1a4), eased);
                    if (eased as i32) < 0 {
                        m.sw(hub.wrapping_add(0x1a4), 0);
                    }
                    let hub = self::hub(m);
                    if (m.lw(hub.wrapping_add(0x1a4)) as i32) >= 0x1e {
                        m.sw(hub.wrapping_add(0x1a4), 0x1d);
                    }
                }
                _ => m.sw(hub.wrapping_add(0x1a4), 0x1d),
            }
        }
        call(m, VISUAL_CLOSE, [0, 5, 0, 0]);
        call(m, VISUAL_CLOSE, [0, 7, 0, 0]);
        call(m, START_SOUND, [0x5b, 0, 0, 0]);
        call(m, START_SOUND, [0x3b, 0, 0, 0]);
    }
    let block = block(m);
    m.sb(block.wrapping_add(0x9d6), 2);
}
