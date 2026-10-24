//! The sun, its lens flare and bloom (`docs/EFFECTS_SUN.md`, Milestone 1
//! Phase 4 item 7).
//!
//! - **The card side on synthetic frames** (a graphics adapter; skipped
//!   without one unless `AC3_REQUIRE_GPU` is set): without its data it
//!   leaves VRAM alone; the disc goes on the sky only, white at the
//!   projected position, keeping every mask bit; an occluded sun draws no
//!   disc and no flare; bloom spreads bright light and leaves dark pixels
//!   far from it exact. [`sun_gpu_time_at_4x`] (`--ignored`) measures the
//!   card side's GPU time with timestamp queries.
//! - **Japanese flight** (`--ignored`: the JP disc and its save states):
//!   - [`jp_sun_drops_exactly_the_games_sun_and_flare`]: every pass of two
//!     runs (mission 1 facing the sun, [`sun_state`]; and from
//!     `at50000.ac3state`: the cockpit, the crash, the replay), the
//!     classification of every primitive outside the world, and the sun's
//!     projection against the game's own sun sprite (1 x pixels);
//!   - [`jp_sun_identity_and_pictures`]: eight players in lockstep over
//!     four runs ([`RUNS`]): effects off with the sun configured; effects
//!     on without the sun's card half (main as it was); on with the sun off
//!     but its card half registered; the sun; the atmosphere; with the sun;
//!     the atmosphere and far terrain; with the sun. RAM identical in all
//!     eight; the sun off with its card half registered gives main's
//!     picture pixel for pixel; the sun changes the picture. The sun's
//!     visibility is taken from each depth snapshot (all three on), and
//!     pictures (off, sun, atmo, atmo-sun, far, far-sun) are saved at
//!     chosen passes and where the sun is partly hidden, to `$AC3_SUN_OUT`
//!     (default `target/effects-sun`), at the scales of `$AC3_SUN_SCALES`
//!     (default 2).
//!
//! The runs facing the sun start from `at30000.ac3state` (the hub) with the
//! story pad schedule (`$AC3_JP_PAD`, default
//! `~/ac3-jp/runs/pad-boot-story-flight.txt`) up to VBlank 49,700, then
//! [`SUN_PAD`] instead of its dive: pull up, roll, level out; from about
//! VBlank 50,500 the low sun is ahead over the coast. The state at VBlank
//! 50,450 is saved once to `$AC3_SUN_OUT/sun-facing.ac3state` (or
//! `$AC3_SUN_STATE` is used).
use ac3_graphics::Vram;
use ac3_port::assets::GameData;
use ac3_port::effects::sun::{self, GameSun, SunCard, SunFrame};
use ac3_port::effects::{atmosphere_fx, far_terrain};
use ac3_port::gpu_renderer::execute::{VRAM_FORMAT, offscreen_device};
use ac3_port::gpu_renderer::fx::card::ZORDER_FORMAT;
use ac3_port::gpu_renderer::fx::{
    self, CardEffect, Context, FxFrame, Phase, Primitive, RecordEffect, Route, Scene,
};
use ac3_port::gpu_renderer::tiles::Rect;
use ac3_port::gpu_renderer::{DisplayArea, Executor, Frame, Options, TablePrecision};
use ac3_port::machine_card::{CardDevice, CardSource};
use ac3_port::machine_player::{self, Host, PadType, Player};
use ac3_port::savestate::{self, GameId};
use ac3_port::settings::Effects;
use serde::Serialize;
use std::cell::RefCell;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::rc::Rc;

// ---- the card side on synthetic frames ---------------------------------------------------

/// The scale of the synthetic frames, and their world area (1 x).
const N: u32 = 2;
const AREA: Rect = Rect {
    x: 0,
    y: 0,
    w: 320,
    h: 240,
};

fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
    match offscreen_device() {
        Ok(pair) => Some(pair),
        Err(error) if std::env::var_os("AC3_REQUIRE_GPU").is_none() => {
            eprintln!("skipped: {error}");
            None
        }
        Err(error) => panic!("AC3_REQUIRE_GPU: {error}"),
    }
}

/// A synthetic effects frame on the card: N x VRAM and its shadow, the z
/// target, the depth snapshot of [`AREA`].
struct Synthetic {
    /// The scale.
    n: u32,
    device: wgpu::Device,
    queue: wgpu::Queue,
    vram: wgpu::Texture,
    shadow: wgpu::Texture,
    zorder: wgpu::Texture,
    snapshot: wgpu::Texture,
    scene_buffer: wgpu::Buffer,
    scene: Scene,
    frame: FxFrame,
}

impl Synthetic {
    fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        Self::scaled(device, queue, N)
    }

    /// The N x VRAM's size.
    fn size(&self) -> (u32, u32) {
        (1024 * self.n, 512 * self.n)
    }

    fn scaled(device: wgpu::Device, queue: wgpu::Queue, n: u32) -> Self {
        use wgpu::TextureUsages as U;
        let texture = |label, (w, h): (u32, u32), format, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let all = U::RENDER_ATTACHMENT | U::TEXTURE_BINDING | U::COPY_SRC | U::COPY_DST;
        let size = (1024 * n, 512 * n);
        let vram = texture("vram", size, VRAM_FORMAT, all);
        let shadow = texture("shadow", size, VRAM_FORMAT, all);
        let zorder = texture("zorder", size, ZORDER_FORMAT, all);
        let snapshot = texture(
            "snapshot",
            (AREA.w as u32 * n, AREA.h as u32 * n),
            ZORDER_FORMAT,
            U::TEXTURE_BINDING | U::COPY_DST,
        );
        let scene_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("scene"),
            size: 256,
            usage: wgpu::BufferUsages::UNIFORM,
            mapped_at_creation: false,
        });
        // Identity view, H 192, the projection centre at (160, 120); a
        // warm white light.
        let scene = Scene {
            h: 192,
            view: [[4096, 0, 0], [0, 4096, 0], [0, 0, 4096]],
            centre: [160, 120],
            sun: [0, 0, 4096],
            sun_colour: [1800, 1700, 1600],
            x_scale: (1, 1),
            ..Default::default()
        };
        let frame = FxFrame {
            scene,
            world_area: AREA,
            ..Default::default()
        };
        Self {
            n,
            device,
            queue,
            vram,
            shadow,
            zorder,
            snapshot,
            scene_buffer,
            scene,
            frame,
        }
    }

    /// Fill the N x VRAM with `pixel(x, y)` (N x pixels of the area; the
    /// rest 0).
    fn fill(&self, pixel: impl Fn(u32, u32) -> u16) {
        let (w, h) = self.size();
        let mut bytes = vec![0u8; (w * h * 2) as usize];
        for y in 0..AREA.h as u32 * self.n {
            for x in 0..AREA.w as u32 * self.n {
                let at = ((y * w + x) * 2) as usize;
                bytes[at..at + 2].copy_from_slice(&pixel(x, y).to_le_bytes());
            }
        }
        self.queue.write_texture(
            self.vram.as_image_copy(),
            &bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * 2),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        self.sync();
    }

    /// The snapshot: `(z, order)` per N x pixel of the area.
    fn depths(&self, depth: impl Fn(u32, u32) -> (f32, f32)) {
        let (w, h) = (AREA.w as u32 * self.n, AREA.h as u32 * self.n);
        let mut bytes = Vec::with_capacity((w * h * 8) as usize);
        for y in 0..h {
            for x in 0..w {
                let (z, order) = depth(x, y);
                bytes.extend_from_slice(&z.to_le_bytes());
                bytes.extend_from_slice(&order.to_le_bytes());
            }
        }
        self.queue.write_texture(
            self.snapshot.as_image_copy(),
            &bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * 8),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Copy VRAM into the shadow (what the executor's layering does before
    /// the late step).
    fn sync(&self) {
        let mut encoder = self.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_texture(
            self.vram.as_image_copy(),
            self.shadow.as_image_copy(),
            self.vram.size(),
        );
        self.queue.submit([encoder.finish()]);
    }

    /// Run the card's post (`late`: its late step) with `data`.
    fn run(&self, card: &mut SunCard, data: Option<&SunFrame>, late: bool) {
        let mut encoder = self.device.create_command_encoder(&Default::default());
        self.encode(&mut encoder, card, data, late);
        self.queue.submit([encoder.finish()]);
    }

    /// Encode the card's post (`late`: its late step) with `data`.
    fn encode(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        card: &mut SunCard,
        data: Option<&SunFrame>,
        late: bool,
    ) {
        let vram = self.vram.create_view(&Default::default());
        let shadow = self.shadow.create_view(&Default::default());
        let zorder = self.zorder.create_view(&Default::default());
        let snapshot = self.snapshot.create_view(&Default::default());
        let mut ctx = Context {
            device: &self.device,
            queue: &self.queue,
            encoder,
            scale: self.n,
            area: AREA,
            pixels: [0, 0, AREA.w as u32 * self.n, AREA.h as u32 * self.n],
            vram: &vram,
            vram_texture: &self.vram,
            shadow: &shadow,
            shadow_texture: &self.shadow,
            zorder: &zorder,
            snapshot: Some(&snapshot),
            scene: &self.scene,
            frame: &self.frame,
            scene_buffer: &self.scene_buffer,
            debug: fx::DebugView::Off,
            data: data.map(|frame| frame as &(dyn std::any::Any + Send + Sync)),
        };
        if late {
            card.late(&mut ctx);
        } else {
            card.post(&mut ctx);
        }
    }

    /// The N x VRAM over the area, rows of N x pixels.
    fn read(&self) -> Vec<u16> {
        let (w, h) = self.size();
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("read"),
            size: u64::from(w * h * 2),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            self.vram.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(w * 2),
                    rows_per_image: None,
                },
            },
            self.vram.size(),
        );
        self.queue.submit([encoder.finish()]);
        buffer.map_async(wgpu::MapMode::Read, .., |result| result.unwrap());
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        let bytes = buffer.get_mapped_range(..).unwrap().to_vec();
        let words: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|p| u16::from_le_bytes([p[0], p[1]]))
            .collect();
        let (aw, ah) = (AREA.w as u32 * self.n, AREA.h as u32 * self.n);
        (0..ah)
            .flat_map(|y| (0..aw).map(move |x| (y, x)))
            .map(|(y, x)| words[(y * w + x) as usize])
            .collect()
    }

    /// The sun's data for this scene: its disc and flare on, bloom
    /// `bloom`, one flare element.
    fn sun(&self, bloom: f32) -> SunFrame {
        let mut elements = [sun::FlareElement::default(); sun::FLARE_ELEMENTS];
        elements[0] = sun::FlareElement {
            kind: 2,
            threshold: 20,
            size: 12,
            scale: -4096,
            profile: [[0.6, 0.4, 0.2]; sun::PROFILE],
            ..Default::default()
        };
        SunFrame {
            // Without the veiling glare (its own test below).
            settings: sun::Settings {
                bloom,
                glare: 0.0,
                ..Default::default()
            },
            view: sun::project(&self.scene),
            word: 0x0202,
            elements: Some(elements),
            game: Default::default(),
        }
    }
}

fn rgb(word: u16) -> [u16; 3] {
    [word & 31, (word >> 5) & 31, (word >> 10) & 31]
}

/// The area's N x pixel (x, y) in a [`Synthetic::read`] picture.
fn at(picture: &[u16], x: u32, y: u32) -> u16 {
    picture[(y * AREA.w as u32 * N + x) as usize]
}

/// A dark blue sky (5, 8, 14), the mask bit set on every other column.
fn sky(x: u32, _y: u32) -> u16 {
    5 | (8 << 5) | (14 << 10) | if x.is_multiple_of(2) { 0x8000 } else { 0 }
}

#[test]
fn without_its_data_the_sun_leaves_vram_alone() {
    let Some((device, queue)) = gpu() else {
        return;
    };
    let synthetic = Synthetic::new(device, queue);
    synthetic.fill(sky);
    synthetic.depths(|_, _| (fx::SKY_Z, 0.0));
    let before = synthetic.read();
    let mut card = SunCard::default();
    synthetic.run(&mut card, None, false);
    synthetic.run(&mut card, None, true);
    assert!(synthetic.read() == before);
    assert_eq!(card.stats, Default::default());
}

#[test]
fn the_disc_goes_on_the_sky_at_the_projected_sun() {
    let Some((device, queue)) = gpu() else {
        return;
    };
    let mut synthetic = Synthetic::new(device, queue);
    // The sun 40 pixels right of and 20 above the centre (H 192).
    synthetic.scene.sun = {
        let v = [40.0f32 / 192.0, -20.0 / 192.0, 1.0];
        let l = (v[0] * v[0] + v[1] * v[1] + 1.0).sqrt();
        v.map(|c| (c / l * 4096.0).round() as i16)
    };
    synthetic.fill(sky);
    // Terrain (order 7) below y 140 (1 x), and a pillar over x 60..80.
    let ground = |x: u32, y: u32| y >= 140 * N || (60 * N..80 * N).contains(&x);
    synthetic.depths(|x, y| {
        if ground(x, y) {
            (900.0, 7.0)
        } else {
            (fx::SKY_Z, 0.0)
        }
    });
    let before = synthetic.read();
    let mut card = SunCard::default();
    let data = synthetic.sun(0.0);
    let [sx, sy] = data.view.screen.unwrap();
    assert!(
        (sx - 40.0).abs() < 0.1 && (sy + 20.0).abs() < 0.1,
        "{sx} {sy}"
    );
    synthetic.run(&mut card, Some(&data), false);
    let after = synthetic.read();
    // White at the projected sun, (200, 100) at 1 x.
    let centre = at(&after, 200 * N, 100 * N);
    assert_eq!(rgb(centre), [31, 31, 31]);
    // The disc is small: 10 pixels away it is the aureole, brighter than
    // the sky, not white.
    let near = rgb(at(&after, 210 * N, 100 * N));
    assert!(
        near.iter().sum::<u16>() > 5 + 8 + 14 && near[0] < 31,
        "{near:?}"
    );
    let mut changed = 0;
    for (i, (&new, &old)) in after.iter().zip(&before).enumerate() {
        let (x, y) = (
            i as u32 % (AREA.w as u32 * N),
            i as u32 / (AREA.w as u32 * N),
        );
        // Every mask bit kept; the ground never touched at the post.
        assert_eq!(new & 0x8000, old & 0x8000, "({x}, {y})");
        if ground(x, y) {
            assert_eq!(new, old, "({x}, {y}) is ground");
        }
        changed += usize::from(new != old);
    }
    assert!(changed > 1000, "{changed}");
    assert_eq!((card.stats.posts, card.stats.discs), (1, 1));
    // The late step without bloom: the lens flare only (its element sits at
    // the mirror image of the sun, (120, 140)); far from both nothing
    // changes.
    synthetic.sync();
    let before_late = synthetic.read();
    synthetic.run(&mut card, Some(&data), true);
    let late = synthetic.read();
    assert!(at(&late, 120 * N, 140 * N - 6) != at(&before_late, 120 * N, 140 * N - 6));
    assert_eq!(
        at(&late, 10 * N, 230 * N),
        at(&before_late, 10 * N, 230 * N)
    );
    assert!(
        late.iter()
            .zip(&before_late)
            .all(|(new, old)| new & 0x8000 == old & 0x8000)
    );
    assert_eq!(
        (card.stats.lates, card.stats.flares, card.stats.blooms),
        (1, 1, 0)
    );
}

#[test]
fn an_occluded_sun_draws_no_disc_and_no_flare() {
    let Some((device, queue)) = gpu() else {
        return;
    };
    let synthetic = Synthetic::new(device, queue);
    synthetic.fill(sky);
    // A hill of far terrain (draw order 0, a depth) over the sun (at the
    // centre) and 16 pixels around it.
    let hill = |x: u32, y: u32| {
        let (dx, dy) = (x as f32 / N as f32 - 160.0, y as f32 / N as f32 - 120.0);
        dx.abs() < 16.0 && dy.abs() < 16.0
    };
    synthetic.depths(|x, y| {
        if hill(x, y) {
            (50_000.0, 0.0)
        } else {
            (fx::SKY_Z, 0.0)
        }
    });
    let mut card = SunCard::default();
    let data = synthetic.sun(0.0);
    synthetic.run(&mut card, Some(&data), false);
    let post = synthetic.read();
    synthetic.sync();
    synthetic.run(&mut card, Some(&data), true);
    let late = synthetic.read();
    // No white pixel anywhere (no disc), the hill unchanged; the flare's
    // element (at the centre's mirror, the centre itself) and the halo
    // are not drawn: the late step changes nothing.
    assert!(post.iter().all(|&p| rgb(p) != [31, 31, 31]));
    assert_eq!(at(&post, 160 * N, 120 * N), sky(160 * N, 120 * N));
    assert!(late == post, "the flare of an occluded sun");
}

#[test]
fn bloom_spreads_bright_light_and_leaves_dark_pixels_exact() {
    let Some((device, queue)) = gpu() else {
        return;
    };
    let mut synthetic = Synthetic::new(device, queue);
    // No sun in front: bloom only. Black, with a white 8 x 8 block at
    // (40, 40) and a mid grey (15) block at (240, 40), under the bright
    // pass's threshold.
    synthetic.scene.sun = [0, 0, -4096];
    let white = |x: u32, y: u32| (40 * N..48 * N).contains(&x) && (40 * N..48 * N).contains(&y);
    let grey = |x: u32, y: u32| (240 * N..248 * N).contains(&x) && (40 * N..48 * N).contains(&y);
    synthetic.fill(|x, y| {
        if white(x, y) {
            0x7fff
        } else if grey(x, y) {
            15 | (15 << 5) | (15 << 10)
        } else {
            0
        }
    });
    synthetic.depths(|_, _| (1000.0, 1.0));
    let before = synthetic.read();
    let mut card = SunCard::default();
    let data = synthetic.sun(sun::Bloom::DEFAULT as f32);
    assert_eq!(data.view.screen, None);
    synthetic.run(&mut card, Some(&data), false);
    assert!(synthetic.read() == before, "nothing at the post");
    synthetic.run(&mut card, Some(&data), true);
    let after = synthetic.read();
    assert_eq!((card.stats.blooms, card.stats.flares), (1, 0));
    // Beside the white block: brighter. Around the grey one, far from the
    // white: exact. The white block stays white.
    let beside = rgb(at(&after, 49 * N, 44 * N));
    assert!(beside[0] > 0, "{beside:?}");
    assert_eq!(at(&after, 44 * N, 44 * N), 0x7fff);
    for y in 30 * N..60 * N {
        for x in 230 * N..260 * N {
            assert_eq!(at(&after, x, y), at(&before, x, y), "({x}, {y})");
        }
    }
    // The glow falls off: 4 pixels out brighter than 16 out.
    let near = rgb(at(&after, 52 * N, 44 * N))[0];
    let far = rgb(at(&after, 64 * N, 44 * N))[0];
    assert!(near >= far, "{near} {far}");
}

/// One frame of the card side with `data` from the dark sky: the post, the
/// shadow brought up to date, the late step.
fn frame_with(synthetic: &Synthetic, data: &SunFrame) -> Vec<u16> {
    let mut card = SunCard::default();
    synthetic.fill(sky);
    synthetic.run(&mut card, Some(data), false);
    synthetic.sync();
    synthetic.run(&mut card, Some(data), true);
    synthetic.read()
}

fn brightness(word: u16) -> u16 {
    rgb(word).iter().sum()
}

#[test]
fn the_glare_veils_the_picture_by_visibility_and_the_star_can_go() {
    let Some((device, queue)) = gpu() else {
        return;
    };
    let mut synthetic = Synthetic::new(device, queue);
    // The sun 40 pixels right of the centre, level: at (200, 120).
    synthetic.scene.sun = {
        let v = [40.0f32 / 192.0, 0.0, 1.0];
        let l = (v[0] * v[0] + 1.0).sqrt();
        v.map(|c| (c / l * 4096.0).round() as i16)
    };
    synthetic.depths(|_, _| (fx::SKY_Z, 0.0));
    let plain = synthetic.sun(0.0);
    let mut glare = plain.clone();
    glare.settings.glare = 1.0;
    let (without, with) = (
        frame_with(&synthetic, &plain),
        frame_with(&synthetic, &glare),
    );
    // The veil reaches the far corner, more near the sun; mask bits kept.
    let corner = |p: &[u16]| brightness(at(p, 10 * N, 230 * N));
    let near = |p: &[u16]| brightness(at(p, 230 * N, 150 * N));
    assert!(
        corner(&with) > corner(&without),
        "{} {}",
        corner(&with),
        corner(&without)
    );
    assert!(near(&with) - near(&without) >= corner(&with) - corner(&without));
    assert!(
        with.iter()
            .zip(&without)
            .all(|(a, b)| a & 0x8000 == b & 0x8000)
    );
    // Twice the strength veils more.
    let mut double = glare.clone();
    double.settings.glare = 2.0;
    assert!(corner(&frame_with(&synthetic, &double)) > corner(&with));
    // The sun behind far terrain (16 pixels around it): no veil at all.
    synthetic.depths(|x, y| {
        let (dx, dy) = (x as f32 / N as f32 - 200.0, y as f32 / N as f32 - 120.0);
        if dx.abs() < 16.0 && dy.abs() < 16.0 {
            (50_000.0, 0.0)
        } else {
            (fx::SKY_Z, 0.0)
        }
    });
    assert!(frame_with(&synthetic, &glare) == frame_with(&synthetic, &plain));
    // The star off: its rays go, nothing else changes. The rays turn by
    // the game's glow angle, `4 x + 8 y` 4096ths of a turn.
    synthetic.depths(|_, _| (fx::SKY_Z, 0.0));
    let mut no_star = plain.clone();
    no_star.settings.star = false;
    let (star, plain_star) = (
        frame_with(&synthetic, &plain),
        frame_with(&synthetic, &no_star),
    );
    let a = sun::star_angle([40.0, 0.0]);
    let ray = |d: f32| {
        let (x, y) = (200.0 + d * a.cos(), 120.0 + d * a.sin());
        ((x * N as f32) as u32, (y * N as f32) as u32)
    };
    let (rx, ry) = ray(12.0);
    assert!(brightness(at(&star, rx, ry)) > brightness(at(&plain_star, rx, ry)));
    let off = a + std::f32::consts::FRAC_PI_4;
    let (ox, oy) = (
        ((200.0 + 12.0 * off.cos()) * N as f32) as u32,
        ((120.0 + 12.0 * off.sin()) * N as f32) as u32,
    );
    assert_eq!(at(&star, ox, oy), at(&plain_star, ox, oy));
    let changed = star.iter().zip(&plain_star).filter(|(a, b)| a != b).count();
    assert!(changed > 0 && changed < star.len() / 20, "{changed}");
}

/// The card side's GPU time at 4x (timestamp queries; `--ignored`, needs
/// an adapter with timestamps inside encoders): a frame half sky, half
/// terrain, the sun in view with ten flare elements, bloom at the default.
/// Prints the post's and the late step's times over `$AC3_SUN_FRAMES`
/// (300) frames and writes `gpu-time-4x.json`.
#[test]
#[ignore = "a measurement: needs a graphics adapter with timestamp queries"]
fn sun_gpu_time_at_4x() {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY.with_env(),
        ..wgpu::InstanceDescriptor::new_without_display_handle_from_env()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    }))
    .expect("an adapter");
    let wanted = wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
    assert!(
        adapter.features().contains(wanted),
        "no timestamps on {:?}",
        adapter.get_info()
    );
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("sun timing"),
        required_features: wanted,
        required_limits:
            wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits()),
        ..Default::default()
    }))
    .unwrap();
    let mut synthetic = Synthetic::scaled(device.clone(), queue.clone(), 4);
    synthetic.scene.h = 296;
    synthetic.scene.sun = [600, -300, 4050];
    synthetic.fill(|x, y| {
        let t = (y / 4).min(239) as u16;
        if y < 140 * 4 {
            (18 + t / 20) | ((20 + t / 20) << 5) | (24 << 10) | ((x as u16 & 1) << 15)
        } else {
            (6 + (x as u16 / 8) % 5) | ((9 + (y as u16 / 8) % 4) << 5) | (5 << 10)
        }
    });
    synthetic.depths(|_, y| {
        if y < 140 * 4 {
            (fx::SKY_Z, 0.0)
        } else {
            (2000.0 + y as f32, 1.0 + (y / 4) as f32)
        }
    });
    let mut data = synthetic.sun(sun::Bloom::DEFAULT as f32);
    let mut elements = [sun::FlareElement::default(); sun::FLARE_ELEMENTS];
    for (i, element) in elements.iter_mut().enumerate() {
        *element = sun::FlareElement {
            kind: (i % 4) as u8,
            threshold: 10,
            size: 8 + 4 * i as i16,
            scale: 4096 - 1000 * i as i16,
            profile: [[0.5, 0.4, 0.3]; sun::PROFILE],
            ..Default::default()
        };
    }
    data.elements = Some(elements);
    assert!(data.view.screen.is_some());
    let frames: usize = std::env::var("AC3_SUN_FRAMES")
        .ok()
        .and_then(|text| text.parse().ok())
        .unwrap_or(300);
    let queries = device.create_query_set(&wgpu::QuerySetDescriptor {
        label: Some("sun timing"),
        ty: wgpu::QueryType::Timestamp,
        count: 3,
    });
    let resolve = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("resolve"),
        size: 24,
        usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let read = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("read"),
        size: 24,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let period = f64::from(queue.get_timestamp_period());
    let mut card = SunCard::default();
    let (mut post, mut late) = (Vec::new(), Vec::new());
    for frame in 0..frames + 10 {
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.write_timestamp(&queries, 0);
        synthetic.encode(&mut encoder, &mut card, Some(&data), false);
        encoder.write_timestamp(&queries, 1);
        synthetic.encode(&mut encoder, &mut card, Some(&data), true);
        encoder.write_timestamp(&queries, 2);
        encoder.resolve_query_set(&queries, 0..3, &resolve, 0);
        encoder.copy_buffer_to_buffer(&resolve, 0, &read, 0, 24);
        queue.submit([encoder.finish()]);
        read.map_async(wgpu::MapMode::Read, .., |result| result.unwrap());
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let ticks: Vec<u64> = read
            .get_mapped_range(..)
            .unwrap()
            .chunks_exact(8)
            .map(|b| u64::from_le_bytes(b.try_into().unwrap()))
            .collect();
        read.unmap();
        // The first frames make the resources.
        if frame >= 10 {
            post.push((ticks[1] - ticks[0]) as f64 * period / 1e6);
            late.push((ticks[2] - ticks[1]) as f64 * period / 1e6);
        }
    }
    let stats = |v: &mut Vec<f64>| {
        v.sort_by(|a, b| a.total_cmp(b));
        let mean = v.iter().sum::<f64>() / v.len() as f64;
        serde_json::json!({ "mean": mean, "median": v[v.len() / 2], "p95": v[v.len() * 95 / 100] })
    };
    let report = serde_json::json!({
        "adapter": adapter.get_info().name,
        "backend": format!("{:?}", adapter.get_info().backend),
        "scale": 4,
        "frames": frames,
        "post_ms": stats(&mut post),
        "late_ms": stats(&mut late),
    });
    eprintln!("{}", serde_json::to_string_pretty(&report).unwrap());
    fs::write(
        out().join("gpu-time-4x.json"),
        serde_json::to_string_pretty(&report).unwrap(),
    )
    .unwrap();
}

// ---- Japanese flight ---------------------------------------------------------------------

fn home(path: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").expect("HOME")).join(path)
}

fn disc() -> PathBuf {
    std::env::var_os("AC3_JP_DISC").map_or_else(
        || home("ac3-jp/discs/Ace Combat 3 - Electrosphere (Japan, Asia) (Disc 1) (Rev 1).cue"),
        PathBuf::from,
    )
}

fn flight_state() -> PathBuf {
    std::env::var_os("AC3_JP_FLIGHT_STATE").map_or_else(
        || home("ac3-jp/runs/present-work/at50000.ac3state"),
        PathBuf::from,
    )
}

fn out() -> PathBuf {
    let out = std::env::var_os("AC3_SUN_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/effects-sun"));
    fs::create_dir_all(&out).unwrap();
    out
}

/// After the story schedule's flight start (VBlank 49,700): pull up, roll
/// right, pull up, roll left; then nothing (the sun ahead from about
/// 50,500).
pub const SUN_PAD: &str = "49750-49900:down,49900-50000:right,50000-50060:down,50070-50110:left";
/// The state facing the sun is saved here.
const SUN_STATE_AT: u64 = 50_450;

fn host() -> Host {
    let game = machine_player::jp();
    let (images, booted) = machine_player::disc_images(game, &disc(), None).unwrap();
    let discs = images
        .iter()
        .map(|path| {
            let image = ac3_port::version::image_path(path).unwrap_or_else(|_| path.clone());
            File::open(image).unwrap()
        })
        .collect();
    let mut host = Host::with_discs(game, Vram::new(), discs, booted);
    host.card = CardDevice::from_sources(&[CardSource::Blank, CardSource::Blank])
        .unwrap()
        .with_clock(game.layout().vblank_counter);
    host.pad = PadType::Digital.ports();
    host
}

fn boot(data: &GameData, host: Host, state: &Path) -> Player {
    let game = machine_player::jp();
    let mut player = Player::new(ac3_flight::Machine::default(), host);
    game.boot(&mut player.machine, &data.executable.payload);
    player.machine.take_fault().unwrap();
    savestate::load(&mut player, GameId::of(&data.executable.payload), state).unwrap();
    player
}

/// The pad schedule (`--pad` syntax) as `(first, last, buttons)`.
fn parse(text: &str) -> Vec<(u64, u64, u16)> {
    machine_player::parse_pad_schedule(text).unwrap()
}

/// Hold the buttons `schedule` gives for the player's next VBlank.
fn hold(player: &mut Player, schedule: &[(u64, u64, u16)]) {
    let vblank = player.vblanks() + 1;
    let buttons = schedule
        .iter()
        .filter(|(first, last, _)| (*first..=*last).contains(&vblank))
        .fold(0, |held, (_, _, b)| held | b);
    player.set_buttons(buttons);
}

/// The save state facing the sun (see the module documentation).
fn sun_state(data: &GameData) -> PathBuf {
    if let Some(path) = std::env::var_os("AC3_SUN_STATE") {
        return PathBuf::from(path);
    }
    let path = out().join("sun-facing.ac3state");
    if path.exists() {
        return path;
    }
    let hub = std::env::var_os("AC3_JP_HUB_STATE").map_or_else(
        || home("ac3-jp/runs/present-work/at30000.ac3state"),
        PathBuf::from,
    );
    let story = std::env::var_os("AC3_JP_PAD").map_or_else(
        || home("ac3-jp/runs/pad-boot-story-flight.txt"),
        PathBuf::from,
    );
    let story = fs::read_to_string(story).unwrap();
    let mut schedule: Vec<(u64, u64, u16)> = parse(story.trim())
        .into_iter()
        .filter(|(first, _, _)| (30_000..49_700).contains(first))
        .collect();
    schedule.extend(parse(SUN_PAD));
    let mut player = boot(data, host(), &hub);
    while player.vblanks() < SUN_STATE_AT {
        hold(&mut player, &schedule);
        if let Err(fault) = player.iteration() {
            panic!("VBlank {}: {fault}", player.vblanks());
        }
        player.host.borrow_mut().take_audio();
    }
    savestate::save(&player, GameId::of(&data.executable.payload), &path).unwrap();
    eprintln!("saved {} (VBlank {})", path.display(), player.vblanks());
    path
}

/// The `[effects]` settings with the sun, the atmosphere and far terrain on
/// or off (haze density 1, the default ring and quality).
fn settings(sun: bool, atmosphere: bool, far_terrain: bool) -> Effects {
    Effects {
        sun,
        atmosphere,
        far_terrain,
        ..Effects::default()
    }
}

/// A JP player from `state` with the GPU renderer at `scale` and sub-pixel
/// geometry, `effects` and the effects' `settings` (their recorder halves
/// registered as `machine_player` does), and the survey after them.
fn player(
    data: &GameData,
    state: &Path,
    scale: u32,
    effects: fx::Config,
    settings: Effects,
    survey: Option<Survey>,
) -> Player {
    let mut host = host();
    let table = ac3_flight::precision::PrecisionTable::shared();
    host.vertex_precision = Some(Rc::new(TablePrecision(table.clone())));
    host.perspective = true;
    host.effects = Some(effects);
    host.effect_settings = settings;
    host.start_gpu_renderer(Options {
        scale,
        dither: false,
    });
    if let Some(survey) = survey {
        // After the sun: it sees what the sun drops, too.
        host.hw.as_mut().unwrap().add_effect(Box::new(survey));
    }
    let mut player = boot(data, host, state);
    player.machine.set_precision(Some(table));
    player.host.borrow_mut().reseed_gpu_renderer();
    player
}

fn step(player: &mut Player) -> Frame {
    if let Err(fault) = player.iteration() {
        panic!("VBlank {}: {fault}", player.vblanks());
    }
    player.host.borrow_mut().take_audio();
    if let Some(error) = player.host.borrow().error() {
        panic!("VBlank {}: device: {error}", player.vblanks());
    }
    player.host.borrow_mut().take_gpu_frame().unwrap()
}

fn display_area(host: &Host) -> Option<DisplayArea> {
    let display = host.gpu.gpu.display();
    let [_, _, w, h] = host.gpu.display;
    let area = DisplayArea {
        x: u32::from(display.start_x),
        y: u32::from(display.start_y),
        width: w.max(1) as u32,
        height: h.max(1) as u32,
    };
    (display.mode & 0x10 == 0 && area.fits()).then_some(area)
}

fn save_png(path: &Path, rgb: &[u32], width: usize, height: usize) {
    let mut encoder = png::Encoder::new(
        std::io::BufWriter::new(File::create(path).unwrap()),
        width as u32,
        height as u32,
    );
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let bytes: Vec<u8> = rgb
        .iter()
        .flat_map(|&p| [(p >> 16) as u8, (p >> 8) as u8, p as u8])
        .collect();
    encoder
        .write_header()
        .unwrap()
        .write_image_data(&bytes)
        .unwrap();
}

fn sun_frame(frame: &Frame) -> Option<&SunFrame> {
    let fx = frame.fx.as_deref()?;
    fx.data
        .iter()
        .find(|(name, _)| *name == sun::NAME)
        .and_then(|(_, data)| data.downcast_ref::<SunFrame>())
}

fn effects(enabled: bool) -> fx::Config {
    fx::Config {
        enabled,
        debug: fx::DebugView::Off,
        x_scale: None,
    }
}

// ---- the survey of the sky, flare and HUD slots ------------------------------------------

/// A drawing command outside the world as the survey saw it.
#[derive(Clone, Debug, Serialize)]
struct Seen {
    phase: Phase,
    command: u8,
    semi: bool,
    textured: bool,
    page: Option<[i32; 2]>,
    clut: Option<[i32; 2]>,
    /// A textured polygon's blend mode (its texture page word's bits 5..6:
    /// 0 average, 1 add, 2 subtract, 3 add a quarter).
    blend: Option<u32>,
    /// The packet's RAM address (0: none).
    address: u32,
    first_vertex: [i32; 2],
    kind: Option<GameSun>,
}

/// Logs every drawing command of an effects frame outside the world, and
/// the world's that the sun would classify (there must be none).
#[derive(Clone, Default)]
struct Survey(Rc<RefCell<Vec<Seen>>>);

impl RecordEffect for Survey {
    fn name(&self) -> &'static str {
        "survey"
    }
    fn primitive(&mut self, primitive: &Primitive<'_>) -> Route {
        let kind = sun::classify(primitive);
        if primitive.phase != Phase::World || kind.is_some() {
            let vertex = primitive.words.get(1).copied().unwrap_or(0);
            self.0.borrow_mut().push(Seen {
                phase: primitive.phase,
                command: primitive.command,
                semi: primitive.semi,
                textured: primitive.texture.is_some(),
                page: primitive.texture.map(|t| t.page),
                clut: primitive.texture.map(|t| t.clut),
                blend: (primitive.texture.is_some() && (0x20..0x40).contains(&primitive.command))
                    .then(|| primitive.words.get(4).map_or(0, |word| (word >> 21) & 3)),
                address: primitive.addresses.first().copied().unwrap_or(0),
                first_vertex: [i32::from(vertex as i16), i32::from((vertex >> 16) as i16)],
                kind,
            });
        }
        Route::Draw
    }
}

#[derive(Debug, Default, Serialize)]
struct PassReport {
    run: &'static str,
    vblank: u64,
    word: u16,
    ours: Option<[f32; 2]>,
    game: Option<[f32; 2]>,
    /// Ours less the game's (which truncates: ours lies in [game, game + 1)).
    error: Option<[f32; 2]>,
    counts: [u32; 5],
    sky_kept_textured: u32,
    flare_kept: u32,
    hud: u32,
}

/// Survey `passes` passes of a player from `state` (`pad` held).
fn survey_run(
    data: &GameData,
    run: &'static str,
    state: &Path,
    pad: &str,
    passes: u64,
    reports: &mut Vec<PassReport>,
    kinds: &mut std::collections::BTreeMap<String, serde_json::Value>,
) {
    let survey = Survey::default();
    let log = survey.0.clone();
    let mut player = player(
        data,
        state,
        2,
        effects(true),
        settings(true, false, false),
        Some(survey),
    );
    let pad = parse(pad);
    for _ in 0..passes {
        hold(&mut player, &pad);
        let frame = step(&mut player);
        let seen: Vec<Seen> = std::mem::take(&mut *log.borrow_mut());
        let vblank = player.vblanks();
        let Some(sun_frame) = sun_frame(&frame) else {
            assert!(
                seen.is_empty(),
                "VBlank {vblank}: a survey outside effects frames"
            );
            continue;
        };
        let mut report = PassReport {
            run,
            vblank,
            word: sun_frame.word,
            ours: sun_frame.view.screen,
            game: sun_frame.game.position(),
            counts: sun_frame.game.counts,
            ..Default::default()
        };
        for item in &seen {
            match (item.phase, item.kind) {
                (Phase::World, kind) => panic!("VBlank {vblank}: a world primitive is {kind:?}"),
                (Phase::Sky, None) if item.textured => {
                    // Textured and kept in the sky: the cloud layers
                    // (8008D9F4) share the sun's page, palettes (352,
                    // 311..314).
                    report.sky_kept_textured += 1;
                    if item.page == Some(sun::SUN_PAGE) {
                        let row = item.clut.map_or(0, |clut| clut[1]);
                        assert!((311..=314).contains(&row), "VBlank {vblank}: {item:?}");
                    }
                }
                (Phase::Flare, None) => {
                    report.flare_kept += 1;
                    assert!(!item.textured && !item.semi, "VBlank {vblank}: {item:?}");
                }
                (Phase::Hud, kind) => {
                    report.hud += 1;
                    assert_eq!(kind, None, "VBlank {vblank}: the HUD keeps everything");
                }
                _ => {}
            }
            let key = format!(
                "{:?} {:02x} semi {} blend {:?} page {:?} clut {:?} -> {}",
                item.phase,
                item.command,
                item.semi,
                item.blend,
                item.page,
                item.clut,
                item.kind.map_or("kept", GameSun::name)
            );
            kinds.entry(key).or_insert_with(
                || serde_json::json!({ "run": run, "vblank": vblank, "example": item }),
            );
        }
        let classified = seen.iter().filter(|item| item.kind.is_some()).count() as u32;
        assert_eq!(
            report.counts.iter().sum::<u32>(),
            classified,
            "VBlank {vblank}"
        );
        if let (Some(ours), Some(game)) = (report.ours, report.game) {
            report.error = Some([ours[0] - game[0], ours[1] - game[1]]);
        }
        if std::env::var_os("AC3_SUN_VERBOSE").is_some() {
            eprintln!(
                "{run} {vblank} ours {:?} game {:?}",
                report.ours, report.game
            );
        }
        reports.push(report);
    }
}

#[test]
#[ignore = "requires the Japanese disc and its save states"]
fn jp_sun_drops_exactly_the_games_sun_and_flare() {
    let data = GameData::load_jp_disc(&disc()).unwrap();
    let mut reports = Vec::new();
    let mut kinds = std::collections::BTreeMap::new();
    let facing = sun_state(&data);
    // `$AC3_SUN_PAD`: another pad schedule for the run facing the sun.
    let pad = std::env::var("AC3_SUN_PAD").unwrap_or_default();
    survey_run(&data, "sun", &facing, &pad, 400, &mut reports, &mut kinds);
    survey_run(
        &data,
        "at50000",
        &flight_state(),
        "",
        1150,
        &mut reports,
        &mut kinds,
    );
    let errors: Vec<[f32; 2]> = reports.iter().filter_map(|r| r.error).collect();
    let worst = errors
        .iter()
        .flat_map(|e| e.map(|v| (v - 0.5).abs()))
        .fold(0.0f32, f32::max);
    let mean =
        [0, 1].map(|i| errors.iter().map(|e| e[i]).sum::<f32>() / errors.len().max(1) as f32);
    let totals: Vec<(&str, u32)> = GameSun::ALL
        .iter()
        .enumerate()
        .map(|(i, kind)| (kind.name(), reports.iter().map(|r| r.counts[i]).sum()))
        .collect();
    let report = serde_json::json!({
        "passes": reports.len(),
        "compared": errors.len(),
        "mean_ours_less_game": mean,
        "worst_distance_from_the_truncated_cell_centre": worst,
        "dropped": totals,
        "kinds": kinds,
        "per_pass": reports,
    });
    fs::write(
        out().join("classification.json"),
        serde_json::to_string_pretty(&report).unwrap(),
    )
    .unwrap();
    eprintln!(
        "{} passes, {} compared, mean {mean:?}, worst {worst:.3}; dropped {totals:?}",
        reports.len(),
        errors.len()
    );
    for key in kinds.keys().filter(|key| !key.starts_with("Hud")) {
        eprintln!("  {key}");
    }
    // The game truncates its projection: ours lies within the game's pixel
    // (half a pixel from its centre), with a quarter pixel for the GTE's
    // division.
    assert!(
        errors.len() >= 20,
        "the game's sun in {} passes",
        errors.len()
    );
    assert!(worst <= 0.75, "the projection is {worst} pixels off");
    // Mission 1's sun word is 0x0202: a disc, and flash mode 2 (no screen
    // fill), so there is no flash to drop.
    for (name, count) in &totals {
        assert_eq!(*count > 0, *name != "flash", "{name}: {count} dropped");
    }
}

// ---- the glare, measured -----------------------------------------------------------------

/// Leaves the game's sun primitives of the given kinds out of the picture
/// (the glare's decomposition).
struct DropKinds(Vec<GameSun>);

impl RecordEffect for DropKinds {
    fn name(&self) -> &'static str {
        "drop-kinds"
    }
    fn primitive(&mut self, primitive: &Primitive<'_>) -> Route {
        match sun::classify(primitive) {
            Some(kind) if self.0.contains(&kind) => Route::Drop,
            _ => Route::Draw,
        }
    }
}

/// Luma (0..255) of a picture's pixels: Rec. 709 weights of the 8-bit
/// channels as shown.
fn luma(pixel: u32) -> f32 {
    let r = ((pixel >> 16) & 0xff) as f32;
    let g = ((pixel >> 8) & 0xff) as f32;
    let b = (pixel & 0xff) as f32;
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// The glare's numbers of one picture with the sun at `sun` (its pixels;
/// `n` the scale): the whole screen's mean, median, 90th and 99th
/// percentile luma and its share near white (luma 230 and up); around the
/// sun, in the ring of 8 to 60 PlayStation pixels (the disc left out), the
/// mean luma, its standard deviation, the detail left in it (the RMS of
/// luma less its 9 x 9-pixel mean: how well an aircraft or an edge stands
/// out, whatever the glow's smooth gradient) and the share near white; and
/// the mean out to 120 pixels.
#[derive(Clone, Copy, Debug, Default, Serialize)]
struct GlareNumbers {
    screen_mean: f64,
    screen_p50: f64,
    screen_p90: f64,
    screen_p99: f64,
    screen_white: f64,
    near_mean: f64,
    near_std: f64,
    near_detail: f64,
    near_white: f64,
    wide_mean: f64,
}

impl GlareNumbers {
    fn of(rgb: &[u32], width: usize, height: usize, sun: [f32; 2], n: f32) -> Self {
        let mut all: Vec<f32> = rgb.iter().map(|&p| luma(p)).collect();
        let screen_mean = all.iter().map(|&y| f64::from(y)).sum::<f64>() / all.len() as f64;
        let screen_white = all.iter().filter(|&&y| y >= 230.0).count() as f64 / all.len() as f64;
        // An integral image for the local means.
        let mut integral = vec![0.0f64; (width + 1) * (height + 1)];
        for y in 0..height {
            let mut row = 0.0;
            for x in 0..width {
                row += f64::from(all[y * width + x]);
                integral[(y + 1) * (width + 1) + x + 1] = integral[y * (width + 1) + x + 1] + row;
            }
        }
        let half = (4.0 * n) as usize;
        let local = |x: usize, y: usize| {
            let (x0, y0) = (x.saturating_sub(half), y.saturating_sub(half));
            let (x1, y1) = ((x + half + 1).min(width), (y + half + 1).min(height));
            let sum = integral[y1 * (width + 1) + x1]
                - integral[y0 * (width + 1) + x1]
                - integral[y1 * (width + 1) + x0]
                + integral[y0 * (width + 1) + x0];
            sum / ((x1 - x0) * (y1 - y0)) as f64
        };
        let (mut near, mut wide, mut detail) = (Vec::new(), Vec::new(), 0.0f64);
        for y in 0..height {
            for x in 0..width {
                let dx = (x as f32 + 0.5 - sun[0]) / n;
                let dy = (y as f32 + 0.5 - sun[1]) / n;
                let r = (dx * dx + dy * dy).sqrt();
                let value = f64::from(all[y * width + x]);
                if (8.0..=60.0).contains(&r) {
                    near.push(value);
                    detail += (value - local(x, y)).powi(2);
                }
                if r <= 120.0 {
                    wide.push(value);
                }
            }
        }
        let near_detail = (detail / near.len().max(1) as f64).sqrt();
        all.sort_by(|a, b| a.total_cmp(b));
        let at = |q: f64| f64::from(all[((all.len() - 1) as f64 * q) as usize]);
        let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len().max(1) as f64;
        let near_mean = mean(&near);
        let near_std = (near.iter().map(|v| (v - near_mean).powi(2)).sum::<f64>()
            / near.len().max(1) as f64)
            .sqrt();
        Self {
            screen_mean,
            screen_p50: at(0.5),
            screen_p90: at(0.9),
            screen_p99: at(0.99),
            screen_white,
            near_mean,
            near_std,
            near_detail,
            near_white: near.iter().filter(|&&v| v >= 230.0).count() as f64
                / near.len().max(1) as f64,
            wide_mean: mean(&wide),
        }
    }

    fn add(&mut self, other: &Self) {
        self.screen_mean += other.screen_mean;
        self.screen_p50 += other.screen_p50;
        self.screen_p90 += other.screen_p90;
        self.screen_p99 += other.screen_p99;
        self.screen_white += other.screen_white;
        self.near_mean += other.near_mean;
        self.near_std += other.near_std;
        self.near_detail += other.near_detail;
        self.near_white += other.near_white;
        self.wide_mean += other.wide_mean;
    }

    fn scaled(&self, by: f64) -> Self {
        Self {
            screen_mean: self.screen_mean * by,
            screen_p50: self.screen_p50 * by,
            screen_p90: self.screen_p90 * by,
            screen_p99: self.screen_p99 * by,
            screen_white: self.screen_white * by,
            near_mean: self.near_mean * by,
            near_std: self.near_std * by,
            near_detail: self.near_detail * by,
            near_white: self.near_white * by,
            wide_mean: self.wide_mean * by,
        }
    }
}

/// The glare variants of `$AC3_SUN_SHAPES`: `halo,width,veil,range,floor`
/// lists separated by `;` (tuning), besides the old (no glare) and the
/// calibrated defaults.
fn glare_variants() -> Vec<(String, sun::Settings)> {
    let mut variants = vec![
        (
            "old".to_owned(),
            sun::Settings {
                glare: 0.0,
                ..Default::default()
            },
        ),
        ("new".to_owned(), sun::Settings::default()),
    ];
    if let Ok(text) = std::env::var("AC3_SUN_SHAPES") {
        for (i, shape) in text.split(';').filter(|s| !s.trim().is_empty()).enumerate() {
            let v: Vec<f32> = shape
                .split(',')
                .map(|x| x.trim().parse().unwrap())
                .collect();
            let shape = sun::GlareShape {
                halo: v[0],
                width: v[1],
                veil: v[2],
                range: v[3],
                floor: v[4],
            };
            variants.push((
                format!("v{i}"),
                sun::Settings {
                    shape,
                    ..Default::default()
                },
            ));
        }
    }
    variants
}

/// The game's glare against ours, measured (`docs/EFFECTS_SUN.md` §6): the
/// runs facing the sun ([`RUNS`] but the crash), players in lockstep at
/// `$AC3_SUN_SCALE` (default 2):
/// - the game; the game without its horizon strip; without its disc and
///   glow; without its flare (and flash); without all of them;
/// - the atmosphere and far terrain with the game's sun;
/// - the atmosphere, far terrain and our sun: without the glare (the first
///   defaults) and with it (the calibrated ones), and `$AC3_SUN_SHAPES`.
///
/// Every third pass where our sun is on the picture and at least half
/// visible, [`GlareNumbers`] of each picture around the sun; the game's sky
/// glare strength (sun state `+4`, `[[gp+0x41C]+0x5CC]`) too. Writes
/// `glare.json` and prints the table; checks that the calibrated glare
/// veils at least as much as the game's (near the sun: as bright, no more
/// contrast; over the screen: as bright).
#[test]
#[ignore = "requires the Japanese disc, its save states and a graphics adapter"]
fn jp_sun_glare_against_the_games() {
    let data = GameData::load_jp_disc(&disc()).unwrap();
    let (device, queue) = offscreen_device().expect("a graphics adapter");
    let scale: u32 = std::env::var("AC3_SUN_SCALE")
        .ok()
        .and_then(|text| text.parse().ok())
        .unwrap_or(2);
    let facing = sun_state(&data);
    let variants = glare_variants();
    let mut names: Vec<String> = [
        "game",
        "game-strip",
        "game-disc-glow",
        "game-flare",
        "game-none",
        "atmo-far",
    ]
    .map(str::to_owned)
    .to_vec();
    names.extend(variants.iter().map(|(name, _)| format!("sun-{name}")));
    let drops: [&[GameSun]; 4] = [
        &[GameSun::Strip],
        &[GameSun::Disc, GameSun::Glow],
        &[GameSun::Flare, GameSun::Flash],
        &GameSun::ALL,
    ];
    let mut totals: Vec<(String, Vec<GlareNumbers>, u64)> = Vec::new();
    let mut per_pass = Vec::new();
    for (run, _, pad, passes, _) in RUNS.iter().filter(|run| run.1) {
        let pad = parse(pad);
        let mut players: Vec<Player> = Vec::new();
        let mut cards: Vec<Executor> = Vec::new();
        for (i, _) in names.iter().enumerate() {
            let (on, chosen) = match i {
                0 => (false, settings(false, false, false)),
                1..=4 => (true, settings(false, false, false)),
                _ => (true, settings(false, true, true)),
            };
            let player = player(&data, &facing, scale, effects(on), chosen, None);
            {
                let mut host = player.host.borrow_mut();
                let recorder = host.hw.as_mut().unwrap();
                if (1..=4).contains(&i) {
                    recorder.add_effect(Box::new(DropKinds(drops[i - 1].to_vec())));
                }
                if i >= 6 {
                    recorder.add_effect(Box::new(sun::SunRecord::new(variants[i - 6].1)));
                }
            }
            players.push(player);
            let mut executor = Executor::new(
                device.clone(),
                queue.clone(),
                Options {
                    scale,
                    dither: false,
                },
                wgpu::TextureFormat::Rgba8Unorm,
            )
            .unwrap();
            atmosphere_fx::register_card(
                &mut executor,
                atmosphere_fx::Config::from_settings(&chosen),
            );
            far_terrain::register_card(&mut executor);
            sun::register_card(&mut executor);
            cards.push(executor);
        }
        let mut sums = vec![GlareNumbers::default(); names.len()];
        let mut counted = 0u64;
        let new = 7;
        for pass in 1..=*passes {
            for player in &mut players {
                hold(player, &pad);
            }
            let frames: Vec<Frame> = players.iter_mut().map(step).collect();
            let ram = players[0].machine.ram().to_vec();
            for (player, name) in players.iter().zip(&names).skip(1) {
                assert!(
                    player.machine.ram() == ram,
                    "{run} pass {pass}: {name}'s RAM"
                );
            }
            for (card, frame) in cards.iter_mut().zip(&frames) {
                card.execute(frame);
            }
            if pass % 3 != 0 {
                continue;
            }
            // Our sun on the picture and at least half visible.
            let (Some(fx), Some(ours)) = (frames[new].fx.as_deref(), sun_frame(&frames[new]))
            else {
                continue;
            };
            let Some(screen) = ours.view.screen else {
                continue;
            };
            let Some((sky, inside)) = cards[new]
                .read_fx_snapshot()
                .and_then(|(values, rect)| visibility(fx, ours, &values, rect))
            else {
                continue;
            };
            let n = scale as f32;
            let at = [
                (fx.scene.centre[0] as f32 + screen[0] - fx.world_area.x as f32) * n,
                (fx.scene.centre[1] as f32 + screen[1] - fx.world_area.y as f32) * n,
            ];
            let area = display_area(&players[0].host.borrow()).expect("a 15-bit picture");
            let on_picture = at[0] >= 0.0
                && at[1] >= 0.0
                && at[0] < (area.width * scale) as f32
                && at[1] < (area.height * scale) as f32;
            if !on_picture || inside < 64 || sky < 32 {
                continue;
            }
            counted += 1;
            // The game's sky glare (8008BA8C's flash mode 2, read by
            // 80094D10 next frame): flag 2 and strength at sun state +4.
            let systems = fx::scene::word(&ram, ac3_flight_jp::GP + 0x41c).unwrap_or(0);
            let flags = fx::scene::word(&ram, systems + 0x5c8).unwrap_or(0);
            let strength = fx::scene::word(&ram, systems + 0x5cc).unwrap_or(0);
            let numbers: Vec<GlareNumbers> = cards
                .iter_mut()
                .map(|card| {
                    let (rgb, width, height) = card.read_display(area).unwrap();
                    GlareNumbers::of(&rgb, width, height, at, n)
                })
                .collect();
            for (sum, numbers) in sums.iter_mut().zip(&numbers) {
                sum.add(numbers);
            }
            per_pass.push(serde_json::json!({
                "run": run, "vblank": players[0].vblanks(), "sun": screen,
                "visibility": f64::from(sky) / 64.0,
                "sky_glare": { "flag": flags & 2 != 0, "strength": strength },
                "numbers": names.iter().zip(&numbers)
                    .map(|(name, n)| (name.clone(), serde_json::to_value(n).unwrap()))
                    .collect::<serde_json::Map<_, _>>(),
            }));
        }
        let means: Vec<GlareNumbers> = sums
            .iter()
            .map(|sum| sum.scaled(1.0 / counted.max(1) as f64))
            .collect();
        totals.push(((*run).to_owned(), means, counted));
    }
    // All runs together, weighted by their frames.
    let frames: u64 = totals.iter().map(|(_, _, c)| c).sum();
    let mut overall = vec![GlareNumbers::default(); names.len()];
    for (_, means, counted) in &totals {
        for (sum, mean) in overall.iter_mut().zip(means) {
            sum.add(&mean.scaled(*counted as f64));
        }
    }
    let overall: Vec<GlareNumbers> = overall
        .iter()
        .map(|sum| sum.scaled(1.0 / frames.max(1) as f64))
        .collect();
    let table = |title: &str, means: &[GlareNumbers]| {
        eprintln!("{title}");
        eprintln!(
            "  {:<16} {:>7} {:>6} {:>6} {:>6} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7}",
            "picture",
            "screen",
            "p50",
            "p90",
            "p99",
            "white%",
            "near",
            "near_sd",
            "detail",
            "nwhite%",
            "wide"
        );
        for (name, m) in names.iter().zip(means) {
            eprintln!(
                "  {:<16} {:>7.1} {:>6.1} {:>6.1} {:>6.1} {:>7.2} {:>7.1} {:>7.1} {:>7.2} {:>7.2} {:>7.1}",
                name,
                m.screen_mean,
                m.screen_p50,
                m.screen_p90,
                m.screen_p99,
                m.screen_white * 100.0,
                m.near_mean,
                m.near_std,
                m.near_detail,
                m.near_white * 100.0,
                m.wide_mean
            );
        }
    };
    for (run, means, counted) in &totals {
        table(&format!("{run}: {counted} frames"), means);
    }
    table(&format!("all: {frames} frames"), &overall);
    let report = serde_json::json!({
        "scale": scale,
        "names": names,
        "variants": variants.iter().map(|(name, s)| serde_json::json!({ "name": name, "settings": s })).collect::<Vec<_>>(),
        "runs": totals.iter().map(|(run, means, counted)| serde_json::json!({
            "run": run, "frames": counted, "means": means })).collect::<Vec<_>>(),
        "all": overall,
        "per_pass": per_pass,
    });
    fs::write(
        out().join(format!("glare-{scale}x.json")),
        serde_json::to_string_pretty(&report).unwrap(),
    )
    .unwrap();
    // The calibrated glare veils at least as much as the game's, in every
    // run: the screen, the sun's surroundings and the ring around it as
    // bright (within a luma step), with no more detail left in the ring.
    for (run, means, counted) in &totals {
        assert!(*counted > 10, "{run}: {counted} frames with the sun");
        let (game, ours) = (&means[0], &means[7]);
        assert!(
            ours.screen_mean >= game.screen_mean - 1.0,
            "{run}: screen {ours:?} {game:?}"
        );
        assert!(
            ours.wide_mean >= game.wide_mean - 1.0,
            "{run}: wide {ours:?} {game:?}"
        );
        assert!(
            ours.near_mean >= game.near_mean - 1.0,
            "{run}: near {ours:?} {game:?}"
        );
        assert!(
            ours.near_detail <= game.near_detail + 0.1,
            "{run}: detail {ours:?} {game:?}"
        );
    }
}

// ---- identity and pictures ---------------------------------------------------------------

/// The runs of [`jp_sun_identity_and_pictures`]: name, from the state facing
/// the sun (else `at50000`), the pad held, passes, the passes whose pictures
/// are saved.
const RUNS: [(&str, bool, &str, u64, &[u64]); 4] = [
    // Banked, the sun high on the screen, sinking toward the coast.
    ("banked", true, "", 300, &[60, 160, 260]),
    // Rolled level: the sun low ahead over the horizon (51,050 to 51,900).
    ("level", true, LEVEL_PAD, 730, &[150, 300, 450, 625, 725]),
    // The external view, climbing: the sun passes behind the aircraft's
    // left wing (51,150 to 51,300).
    ("external", true, EXTERNAL_PAD, 480, &[126, 276, 351, 426]),
    // The crash at 50,250 (explosion, smoke).
    ("crash", false, "", 150, &[125, 130, 140]),
];
/// From the state facing the sun: roll level.
pub const LEVEL_PAD: &str = "50460-50500:right";
/// From the state facing the sun: roll level, the external view, turn
/// left, pull up.
pub const EXTERNAL_PAD: &str =
    "50460-50500:right,50520-50526:select,50560-50574:left,50600-50640:down";

/// The sun's visibility samples (as `fs_visibility`: a Vogel disc of 64,
/// radius as the uniform's) in a depth snapshot (`values` rows of `rect`'s
/// width, N x): those that show sky, and those inside the world area; `None`
/// behind the eye.
fn visibility(
    frame: &FxFrame,
    sun: &SunFrame,
    values: &[[f32; 2]],
    rect: [u32; 4],
) -> Option<(u32, u32)> {
    sun.view.screen?;
    let scene = &frame.scene;
    let rows = sun::uniform(sun, scene, rect[2] / frame.world_area.w.max(1) as u32, rect);
    let [x, y, _, _] = rows[3];
    let radius = rows[5][3];
    let k = scene.k();
    let (mut seen, mut inside) = (0, 0);
    for i in 0..64 {
        let r = radius * ((i as f32 + 0.5) / 64.0).sqrt();
        let a = i as f32 * 2.399_963_2;
        let px = x + a.cos() * k * r - rect[0] as f32;
        let py = y + a.sin() * r - rect[1] as f32;
        if px < 0.0 || py < 0.0 || px >= rect[2] as f32 || py >= rect[3] as f32 {
            continue;
        }
        let at = py as usize * rect[2] as usize + px as usize;
        inside += 1;
        seen += u32::from(values[at][0] >= fx::SKY_Z * 0.5);
    }
    Some((seen, inside))
}

#[test]
#[ignore = "requires the Japanese disc, its save states and a graphics adapter"]
fn jp_sun_identity_and_pictures() {
    let data = GameData::load_jp_disc(&disc()).unwrap();
    let (device, queue) = offscreen_device().expect("a graphics adapter");
    let scales: Vec<u32> = std::env::var("AC3_SUN_SCALES")
        .unwrap_or_else(|_| "2".to_owned())
        .split(',')
        .map(|s| s.trim().parse().unwrap())
        .collect();
    let facing = sun_state(&data);
    let out = out();
    let mut summary = Vec::new();
    for &scale in &scales {
        for (run, from_facing, pad, passes, saved) in RUNS {
            let state = if from_facing {
                facing.clone()
            } else {
                flight_state()
            };
            let pad = parse(pad);
            // Effects off (the sun configured); on, without the sun's card
            // half (main as it was); on, the sun off with its card half
            // registered; the sun; the atmosphere; with the sun; the
            // atmosphere and far terrain; with the sun.
            let names = [
                "off", "main", "nosun", "sun", "atmo", "atmo-sun", "far", "far-sun",
            ];
            let chosen = [
                (false, settings(true, false, false), true),
                (true, settings(false, false, false), false),
                (true, settings(false, false, false), true),
                (true, settings(true, false, false), true),
                (true, settings(false, true, false), true),
                (true, settings(true, true, false), true),
                (true, settings(false, true, true), true),
                (true, settings(true, true, true), true),
            ];
            let mut players: Vec<Player> = chosen
                .iter()
                .map(|&(on, chosen, _)| player(&data, &state, scale, effects(on), chosen, None))
                .collect();
            let mut cards: Vec<Executor> = chosen
                .iter()
                .map(|(_, chosen, sun_card)| {
                    let mut executor = Executor::new(
                        device.clone(),
                        queue.clone(),
                        Options {
                            scale,
                            dither: false,
                        },
                        wgpu::TextureFormat::Rgba8Unorm,
                    )
                    .unwrap();
                    // As machine_player: the atmosphere's card half, far
                    // terrain's, then the sun's.
                    atmosphere_fx::register_card(
                        &mut executor,
                        atmosphere_fx::Config::from_settings(chosen),
                    );
                    far_terrain::register_card(&mut executor);
                    if *sun_card {
                        sun::register_card(&mut executor);
                    }
                    executor
                })
                .collect();
            let (mut identical, mut changed) = (0u64, 0u64);
            let mut seen = Vec::new();
            let mut partial: Vec<u64> = Vec::new();
            let mut saved_pictures = Vec::new();
            for pass in 1..=passes {
                for player in &mut players {
                    hold(player, &pad);
                }
                let frames: Vec<Frame> = players.iter_mut().map(step).collect();
                let ram = players[0].machine.ram().to_vec();
                for (player, name) in players.iter().zip(names).skip(1) {
                    assert!(
                        player.machine.ram() == ram,
                        "{run} pass {pass}: {name}'s RAM"
                    );
                }
                assert!(frames[0].fx.is_none());
                for i in [1, 2, 4, 6] {
                    assert!(sun_frame(&frames[i]).is_none(), "{}", names[i]);
                }
                for (card, frame) in cards.iter_mut().zip(&frames) {
                    card.execute(frame);
                }
                // The sun's visibility with the atmosphere and far terrain.
                let visible = match (frames[7].fx.as_deref(), sun_frame(&frames[7])) {
                    (Some(fx), Some(sun)) => cards[7]
                        .read_fx_snapshot()
                        .and_then(|(values, rect)| visibility(fx, sun, &values, rect)),
                    _ => None,
                };
                let vblank = players[0].vblanks();
                if let Some((sky, inside)) = visible {
                    seen.push((vblank, sky, inside));
                }
                let area = display_area(&players[0].host.borrow()).expect("a 15-bit picture");
                let pictures: Vec<(Vec<u32>, usize, usize)> = cards
                    .iter_mut()
                    .map(|card| card.read_display(area).unwrap())
                    .collect();
                // The sun off, its card half registered: main's picture.
                if pictures[1].0 != pictures[2].0 {
                    let width = pictures[1].1;
                    let differ: Vec<(usize, usize, u32, u32)> = pictures[1]
                        .0
                        .iter()
                        .zip(&pictures[2].0)
                        .enumerate()
                        .filter(|(_, (a, b))| a != b)
                        .map(|(i, (a, b))| (i % width, i / width, *a, *b))
                        .collect();
                    panic!(
                        "{run} {scale}x pass {pass}: the sun off changed {} pixels: {:x?}",
                        differ.len(),
                        &differ[..differ.len().min(12)]
                    );
                }
                identical += 1;
                changed += u64::from(pictures[3].0 != pictures[1].0);
                // Partly hidden (on screen, partly behind terrain or an
                // aircraft): up to three, 40 passes apart.
                let hidden = visible
                    .is_some_and(|(sky, inside)| inside == 64 && (6..58).contains(&sky))
                    && partial.len() < 3
                    && partial.last().is_none_or(|last| pass >= last + 40);
                if hidden {
                    partial.push(pass);
                }
                if saved.contains(&pass) || hidden {
                    // Where the sun is in the pictures (their pixels; the
                    // frame drew into the world area, which is shown next).
                    let at =
                        frames[3]
                            .fx
                            .as_deref()
                            .zip(sun_frame(&frames[3]))
                            .and_then(|(fx, sun)| {
                                let s = sun.view.screen?;
                                let n = scale as f32;
                                Some([
                                    (fx.scene.centre[0] as f32 + s[0] - fx.world_area.x as f32) * n,
                                    (fx.scene.centre[1] as f32 + s[1] - fx.world_area.y as f32) * n,
                                ])
                            });
                    saved_pictures.push(serde_json::json!({
                        "file": format!("{run}-{scale}x-{vblank:06}"),
                        "pass": pass,
                        "partly_hidden": hidden,
                        "sun": at,
                        "visibility": visible,
                    }));
                    for ((rgb, width, height), name) in pictures.iter().zip(names) {
                        if name == "main" || name == "nosun" {
                            continue;
                        }
                        let file = format!("{run}-{scale}x-{vblank:06}-{name}.png");
                        save_png(&out.join(file), rgb, *width, *height);
                    }
                }
            }
            eprintln!(
                "{run} {scale}x: {identical} identical, {changed} changed by the sun, \
                 partly hidden at passes {partial:?}"
            );
            summary.push(serde_json::json!({
                "run": run, "scale": scale, "passes": passes,
                "identical_main_and_sun_off": identical, "changed_by_the_sun": changed,
                "partly_hidden_passes": partial,
                "pictures": saved_pictures,
                "visibility": seen,
                "card": cards.iter().map(|card| card.effects_stats()).collect::<Vec<_>>(),
            }));
            assert!(
                changed > passes / 2,
                "{run}: the sun changed {changed} pictures"
            );
        }
    }
    fs::write(
        out.join("identity.json"),
        serde_json::to_string_pretty(&summary).unwrap(),
    )
    .unwrap();
}
