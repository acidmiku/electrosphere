//! Anamorphic widescreen in flight (`docs/MILESTONE_1.md` Phase 4 items 2
//! and 3; the plan is `docs/RECOMP_ENHANCEMENTS.md` §4.2): the PAL side.
//! The machinery (the setting, the GTE squeeze, the DrawOTag transform) is
//! [`ac3_machine::widescreen`], re-exported here; this module says when it
//! runs in the PAL game ([`begin_pass`], [`world_end`], [`before_draw`]) and
//! which PAL routines squeeze what.
//!
//! The frame keeps its 320 columns but holds a wider view: the GTE squeezes
//! the X of every projected vertex by k = (4/3) / aspect
//! ([`ac3_gte::Gte::set_projection_x_scale`]), and the presenter shows the
//! picture stretched to the aspect. Off by default: with the setting off
//! (the [`Machine`] default) nothing here runs, and the game is
//! bit-identical.
//!
//! **When.** [`begin_pass`] (each main-loop pass) notes whether the pass runs
//! in flight: `[gp+0x2DC]+0x2C` (resident state 2) with the setting on. In
//! such a pass the flight frame body 80033748 ([`crate::frame::frame`])
//! squeezes only its world section: from [`world_begin`] (after the update
//! and the camera, before 8003308C, 80033140, 80033194 and 800331D4) to
//! [`world_end`] (before the HUD 80039D84, 80033524, 80033544, 8003357C).
//! The update and the HUD keep the original 4:3 projection, so the logic
//! that reads screen positions (target projection and lock-on in 80039D84,
//! for one) is unchanged. State 9 (the music player's background flight),
//! the loading substates and every menu never squeeze.
//!
//! **The world section.** Everything projected through RTPS/RTPT (terrain,
//! sky, aircraft, smoke tubes, glint rays, cloud quads, the sun disc) is
//! squeezed by the GTE and needs nothing else. Screen-space culling there
//! compares GTE results with ±160 and so already uses the wider view. The
//! routines that build shapes in screen space around a projected point
//! scale their X extents by k ([`squeeze_x`]):
//!
//! | Routine | What |
//! | --- | --- |
//! | 8005E140 `terrain::view_frustum_body` | horizontal extents with H·k ([`x_scale`]); the vertical ones and the clipped horizon's unprojection keep H for y |
//! | 800931B8 `terrain::sky_bands`, the type-2 sky bands (`draw_cloud_sky_bands`), the sun glow band (`draw_sun_body`) | band edges and the glow's angular span from H·k, so the sky still covers the screen edges (at 21:9 it did not) |
//! | 8008BCBC `render::draw_glow_sprite` | glow sprite width |
//! | 8008BE4C `render::draw_sparkle` | cross, diamond and core X offsets |
//! | 8008C5A8 `render::build_trail_quad`, `trail_corner` | trail widths (8008F388/8008F660/8008FB00 trails, ambient streaks 8008C7C8, lock trail): the direction taken unsqueezed, the corner offsets squeezed |
//! | 8008F940 `render::draw_sparks` | spark width |
//! | 800841AC `render::sprite_corners` | object sprites (flak 80084324, smoke 8008451C, sparks 8008473C, glows 80084934): offset and half widths |
//! | 80090AFC → 80090478 explosion glows, 8008FE38/80090200 | RTPS used as a 2D transform (depth H, translation a screen point): the squeezed centre goes into TRX unsqueezed ([`unsqueeze_x`]), the radius comes out squeezed |
//! | 800908F8 `render::clip_segment` | side planes at ±0xB8 / k |
//! | 8008A310 `render::draw_sun` | corona (2D transform as above); brightness, flash state and flare strength from the 4:3 position, so the flash state is the original's |
//! | 8008A030 `render::lens_flare` | element widths |
//! | 80089BA0 `render::draw_moon` | disc offsets |
//! | 800887D4/80089480 debris stars and embers (`ray_point`) | ray offsets |
//! | 8008D948 `render::draw_motes` | mote width |
//! | 8008F040 `render::lock_burst` | burst width |
//!
//! Checked and left alone: smoke trails 8007A990 (3D rings; H only sets the
//! LOD distances), glints 8008B6D0 and callbacks 8008B914/8008BA18/8008BB1C
//! (3D billboards; H only sets the size cut-off), ribbon trails, shock
//! rings, tracers, exhausts, afterburner and engine glow, leaves, clouds,
//! stars, the sky bands, the terrain (`project_vector` 8005E030 maps to the
//! ground grid, not the screen).
//!
//! **Everything drawn after it** is fixed 4:3 screen space: the HUD
//! (80039D84 target projection, lock-on, panel and message state and the
//! mission fade 8008E42C; 80033524 popups; 80033544 radio text lines;
//! 8003357C radar and gauges 8003BDF8, target markers 8003E828, instruments
//! 80062680 with the boresight, pitch ladder, horizon ring, brackets
//! 8006492C, target arrow, gun pipper and impact marker, panel 8005CBF4,
//! training guidance 80041FF0), the pause menu 80034BB0, the result and
//! replay captions and the replay fade bar. At the frame's DrawOTag
//! ([`before_draw`], [`after_draw`]) every packet linked after the world
//! section is squeezed toward the screen centre, so the HUD shows
//! undistorted as a centred 4:3 block. Elements placed from projected
//! positions (target boxes and markers, the ladder, the brackets, built on
//! the 4:3 projection) land exactly where the squeezed world puts their
//! targets. Elements that span the whole width (fades, flashes, letterbox
//! bars) and drawing areas reaching the screen edges stay as they are.
//! Sprites, which the GPU cannot scale, become textured quads in the free
//! tail of the frame's primitive buffer (with the current texture page).
//! VRAM copies onto the screen (the radio text lines of 8002CF08) keep
//! their size, centred where their squeezed centre falls; fills stay.
//! The list is put back as the game wrote it once the GPU has read it, so
//! RAM after the pass is untouched by this step.
//!
//! **Gameplay.** The update, the HUD state and every routine that reads
//! screen positions for logic (8003ABAC "inside 320x256" and the sighting
//! scripts it starts, lock-on) run on the original projection; the world
//! section writes only drawing data, except the terrain frustum outline
//! minimum `gp+0x4FC/0x500` (read only by the terrain drawing 8005FD68).
//! `tests/widescreen.rs` checks this over a flight with locks, missiles and
//! targets crossing the screen edges: no other RAM word ever differs.
//!
//! In-engine sequences (80039598 with the letterbox 80092164) narrow the
//! drawing area and draw their bars over the full width in the update, so
//! they stay full width; nothing of theirs is squeezed.
//!
//! [`shown_aspect`] tells the presenter at which aspect the picture at a
//! display origin was drawn.
use crate::{
    common::{CONTEXT, GAMEPLAY},
    machine::Machine,
};
use ac3_machine::widescreen as machinery;
pub use ac3_machine::widescreen::{
    Aspect, Picture, Stats, Undo, Widescreen, after_draw, shown_aspect, squeeze_about, squeeze_x,
    unsqueeze_x, world_begin, x_scale,
};

/// Bytes of each flight primitive buffer (80032268).
const PRIMITIVE_BUFFER: u32 = 0x1_b800;

/// The resident state word `[[gp+0x2DC]+0x2C]`, read without side effects.
fn resident_state(m: &Machine) -> u32 {
    let context = m.peek(crate::GP + CONTEXT);
    m.peek(context.wrapping_add(0x2c))
}

/// Whether the game is in flight: resident state 2 (`[[gp+0x2DC]+0x2C]`),
/// read without side effects.
pub fn in_flight(m: &Machine) -> bool {
    resident_state(m) == 2
}

/// Main-loop pass start: decide whether this pass is a widescreen flight
/// pass (resident state 2 with the setting on). The GTE projects normally
/// until [`world_begin`].
pub fn begin_pass(m: &mut Machine) {
    if m.widescreen().aspect().is_none() {
        return;
    }
    let flight = in_flight(m);
    machinery::begin_pass(m, flight);
}

/// 80033748, after the world drawers: the original projection again, and
/// note every packet linked so far (the world, from the ordering table at
/// context +0x10); what is linked later is screen-space overlay.
pub fn world_end(m: &mut Machine) {
    if x_scale(m).is_none() {
        return;
    }
    let list = m.peek(m.peek(crate::GP + CONTEXT).wrapping_add(0x10));
    machinery::world_end(m, list);
}

/// 80025C20, before DrawOTag(`ot`) of the frame drawn with drawing
/// environment `draw` (buffer `index`): remember the picture's aspect, and
/// in a pass that drew its world squeezed, squeeze the overlay packets (see
/// the module documentation). Rebuilt sprites go to the free tail of this
/// frame's primitive buffer (gameplay +0x20/+0x24, filled up to the cursor
/// +0x14). Returns what [`after_draw`] puts back.
pub fn before_draw(m: &mut Machine, ot: u32, draw: u32, index: u32) -> Undo {
    machinery::before_draw(m, ot, draw, index, |m| {
        let gameplay = m.peek(crate::GP + GAMEPLAY);
        let base = m.peek(gameplay.wrapping_add(0x20 + ((index & 1) << 2)));
        let cursor = m.peek(gameplay.wrapping_add(0x14));
        let end = base.wrapping_add(PRIMITIVE_BUFFER);
        if (base..end).contains(&cursor) {
            (cursor.next_multiple_of(4), end)
        } else {
            (0, 0)
        }
    })
}
