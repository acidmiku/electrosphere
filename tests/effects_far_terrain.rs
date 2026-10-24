//! Far terrain (`docs/EFFECTS_TERRAIN.md`, Phase 4 item 6) on Japanese
//! flight, from the mission 1 save state at VBlank 50000: players in
//! lockstep with the GPU renderer and sub-pixel geometry.
//!
//! [`far_terrain_in_jp_flight`], at 2x and 4x (`AC3_FAR_SCALES`), every
//! effect registered as `machine_player` registers it (`effect_settings`):
//! - **as is** (no effects), **redirect** (the atmosphere at haze density
//!   0: the game's fog removed), **redirect + far terrain**, **haze 1 + far
//!   terrain**, **haze 3 + far terrain**; at 2x also **haze 1** alone (its
//!   edge fade) and **far terrain alone** (no redirect: the fog wall);
//! - identity players (2x): effects configured but off with the atmosphere
//!   and far terrain set, and effects on with both registered but off: every
//!   picture of every pass identical to as-is;
//! - every pass: RAM identical in all players (display only);
//! - `AC3_FAR_PAD` holds buttons in every player (`--pad` syntax);
//! - at the saved VBlanks (`AC3_FAR_SAVE`, default the cockpit at 50101 and
//!   the external view at 50701, 50901, 51101): pictures, a contact sheet,
//!   seam close-ups (where the game's terrain meets far terrain), and the
//!   checks: far terrain's view z against [`reference_march`] on sample
//!   pixels, no sky where the reference sees terrain (gaps at the seam),
//!   the backdrop strip dropped, the draw order 0.
//!
//! [`far_terrain_gpu_time`]: the GPU time of the far terrain's passes at
//! 4x (timestamp queries), over the flight.
//!
//! [`jp_sky_slot_primitives`]: what the sky slot draws in mission 1 (the
//! backdrop strip's identification), with its texture.
//!
//! Needs the Japanese disc 1 (`$AC3_JP_DISC`), the state
//! (`$AC3_JP_FLIGHT_STATE`) and a graphics adapter; pictures go to
//! `$AC3_FAR_OUT` (default `target/effects-far`).
use ac3_graphics::Vram;
use ac3_port::assets::GameData;
use ac3_port::effects::atmosphere_fx::{self, HazeDensity};
use ac3_port::effects::far_terrain::{
    self, FarFrame, FarTerrainCard, MapData, Params, Quality, Ring, Settings, View, reference_march,
};
use ac3_port::effects::terrain_map::{TextureKey, sample_texture};
use ac3_port::gpu_renderer::execute::offscreen_device;
use ac3_port::gpu_renderer::fx::{self, Class, Phase, Primitive, Route, SKY_Z};
use ac3_port::gpu_renderer::{DisplayArea, Executor, Frame, Options, TablePrecision};
use ac3_port::machine_card::{CardDevice, CardSource};
use ac3_port::machine_player::{self, Host, PadType, Player};
use ac3_port::savestate::{self, GameId};
use ac3_port::settings::Effects;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::rc::Rc;

fn home(path: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").expect("HOME")).join(path)
}

fn disc() -> PathBuf {
    std::env::var_os("AC3_JP_DISC").map_or_else(
        || home("ac3-jp/discs/Ace Combat 3 - Electrosphere (Japan, Asia) (Disc 1) (Rev 1).cue"),
        PathBuf::from,
    )
}

fn state() -> PathBuf {
    std::env::var_os("AC3_JP_FLIGHT_STATE").map_or_else(
        || home("ac3-jp/runs/present-work/at50000.ac3state"),
        PathBuf::from,
    )
}

fn out() -> PathBuf {
    let out = std::env::var_os("AC3_FAR_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/effects-far"));
    fs::create_dir_all(&out).unwrap();
    out
}

fn env_list(name: &str, default: &str) -> Vec<u64> {
    std::env::var(name)
        .unwrap_or_else(|_| default.to_owned())
        .split(',')
        .filter_map(|v| v.trim().parse().ok())
        .collect()
}

/// How a player is set up.
#[derive(Clone, Copy, Debug)]
struct Setup {
    name: &'static str,
    /// `None`: no effects configured at all (the game as is); else the
    /// master switch.
    effects: Option<bool>,
    /// The `[effects]` settings the host registers its effects from (the
    /// atmosphere's and far terrain's halves, as `machine_player::run`).
    settings: Effects,
    /// Far terrain on although the settings leave it off (it needs the
    /// atmosphere): the fog wall, for the pictures.
    far_by_hand: bool,
}

/// `[effects]` with the atmosphere at `haze` density (`None`: off) and far
/// terrain.
fn effects(haze: Option<f64>, far_terrain: bool) -> Effects {
    Effects {
        enabled: true,
        atmosphere: haze.is_some(),
        haze_density: HazeDensity(haze.unwrap_or(1.0)),
        far_terrain,
        far_terrain_tiles: Ring::default(),
        far_terrain_quality: Quality::Medium,
        ..Effects::default()
    }
}

fn setup(name: &'static str, haze: Option<f64>, far: bool) -> Setup {
    Setup {
        name,
        effects: Some(true),
        settings: effects(haze, far),
        far_by_hand: false,
    }
}

/// A JP player in flight from the save state, as `machine_player::run`
/// builds one, and its executor (`device`), the effects registered as the
/// host registers them.
fn player(
    data: &GameData,
    device: &(wgpu::Device, wgpu::Queue),
    scale: u32,
    setup: Setup,
) -> (Player, Executor) {
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
    let table = ac3_flight::precision::PrecisionTable::shared();
    host.vertex_precision = Some(Rc::new(TablePrecision(table.clone())));
    host.perspective = true;
    host.effects = setup.effects.map(|enabled| fx::Config {
        enabled,
        debug: fx::DebugView::Off,
        x_scale: None,
    });
    host.effect_settings = setup.settings;
    host.start_gpu_renderer(Options {
        scale,
        dither: false,
    });
    if setup.far_by_hand {
        let settings = Settings {
            on: true,
            ..Settings::from_settings(&setup.settings)
        };
        far_terrain::register_recorder(host.hw.as_mut().unwrap(), settings);
    }
    let mut executor = Executor::new(
        device.0.clone(),
        device.1.clone(),
        Options {
            scale,
            dither: false,
        },
        wgpu::TextureFormat::Rgba8Unorm,
    )
    .unwrap();
    atmosphere_fx::register_card(
        &mut executor,
        atmosphere_fx::Config::from_settings(&setup.settings),
    );
    far_terrain::register_card(&mut executor);
    let mut player = Player::new(ac3_flight::Machine::default(), host);
    player.machine.set_precision(Some(table));
    game.boot(&mut player.machine, &data.executable.payload);
    player.machine.take_fault().unwrap();
    savestate::load(&mut player, GameId::of(&data.executable.payload), &state()).unwrap();
    player.host.borrow_mut().reseed_gpu_renderer();
    (player, executor)
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

/// An RGB picture.
#[derive(Clone)]
struct Picture {
    rgb: Vec<u32>,
    width: usize,
    height: usize,
}

impl Picture {
    fn blank(width: usize, height: usize) -> Self {
        Self {
            rgb: vec![0; width * height],
            width,
            height,
        }
    }

    fn save(&self, path: &Path) {
        let mut encoder = png::Encoder::new(
            std::io::BufWriter::new(File::create(path).unwrap()),
            self.width as u32,
            self.height as u32,
        );
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let bytes: Vec<u8> = self
            .rgb
            .iter()
            .flat_map(|&p| [(p >> 16) as u8, (p >> 8) as u8, p as u8])
            .collect();
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&bytes)
            .unwrap();
    }

    /// Box-filtered down by `factor`.
    fn shrink(&self, factor: usize) -> Self {
        if factor <= 1 {
            return self.clone();
        }
        let (w, h) = (self.width / factor, self.height / factor);
        let mut out = Self::blank(w, h);
        for y in 0..h {
            for x in 0..w {
                let mut sum = [0u32; 3];
                for dy in 0..factor {
                    for dx in 0..factor {
                        let p = self.rgb[(y * factor + dy) * self.width + x * factor + dx];
                        sum[0] += (p >> 16) & 255;
                        sum[1] += (p >> 8) & 255;
                        sum[2] += p & 255;
                    }
                }
                let n = (factor * factor) as u32;
                out.rgb[y * w + x] = ((sum[0] / n) << 16) | ((sum[1] / n) << 8) | (sum[2] / n);
            }
        }
        out
    }

    /// The rectangle at (x, y) of w x h, enlarged `zoom` times.
    fn crop(&self, x: usize, y: usize, w: usize, h: usize, zoom: usize) -> Self {
        let mut out = Self::blank(w * zoom, h * zoom);
        for j in 0..h * zoom {
            for i in 0..w * zoom {
                let (sx, sy) = (
                    (x + i / zoom).min(self.width - 1),
                    (y + j / zoom).min(self.height - 1),
                );
                out.rgb[j * w * zoom + i] = self.rgb[sy * self.width + sx];
            }
        }
        out
    }

    /// `pictures` side by side (rows of `columns`), 4-pixel gaps.
    fn sheet(pictures: &[Picture], columns: usize) -> Self {
        let (w, h) = (pictures[0].width, pictures[0].height);
        let rows = pictures.len().div_ceil(columns);
        let mut out = Self::blank(columns * (w + 4) - 4, rows * (h + 4) - 4);
        for (k, picture) in pictures.iter().enumerate() {
            let (cx, cy) = ((k % columns) * (w + 4), (k / columns) * (h + 4));
            for y in 0..h.min(picture.height) {
                let at = (cy + y) * out.width + cx;
                out.rgb[at..at + w.min(picture.width)].copy_from_slice(
                    &picture.rgb[y * picture.width..y * picture.width + w.min(picture.width)],
                );
            }
        }
        out
    }
}

fn read(executor: &mut Executor, area: DisplayArea) -> Picture {
    let (rgb, width, height) = executor.read_display(area).unwrap();
    Picture { rgb, width, height }
}

/// The checks of one saved frame of the far-terrain player.
#[derive(Debug, Default, serde::Serialize)]
struct FrameChecks {
    vblank: u64,
    /// Pixels of the world area: game terrain, other game primitives, far
    /// terrain, sky.
    terrain: u64,
    objects: u64,
    far: u64,
    sky: u64,
    /// Far terrain samples against the reference, where the reference's
    /// surface is the meshes' too: the ray march's part (beyond the ring)
    /// and the sea within it. Land within the ring is drawn from the exact
    /// meshes (walls, roofs), which the heightmap only approximates.
    march: ErrorStats,
    sea: ErrorStats,
    /// Far terrain samples where the card met a mesh within the ring and
    /// the reference the heightmap beyond it, or the other way round.
    ring_silhouettes: usize,
    /// Far terrain samples without a reference hit.
    no_reference: usize,
    /// Sky samples where the reference sees terrain even 2 pixels higher
    /// (inside the terrain, not at its horizon): gaps.
    gaps: usize,
    sky_samples: usize,
    /// Sky samples the reference hits at all (gaps or the grazing horizon).
    sky_reference_hits: usize,
    /// Pixels where game terrain meets far terrain (the seam).
    seam: u64,
}

/// Relative view z errors against the reference.
#[derive(Debug, Default, serde::Serialize)]
struct ErrorStats {
    compared: usize,
    median: f64,
    p99: f64,
    within_1_percent: f64,
}

impl ErrorStats {
    fn of(mut errors: Vec<f64>) -> Self {
        errors.sort_by(f64::total_cmp);
        if errors.is_empty() {
            return Self::default();
        }
        let n = errors.len();
        Self {
            compared: n,
            median: errors[n / 2],
            p99: errors[(n * 99 / 100).min(n - 1)],
            within_1_percent: errors.iter().filter(|&&e| e < 0.01).count() as f64 / n as f64,
        }
    }

    /// The median close, and nearly all close when there are enough
    /// samples to say (a handful of shore samples can meet a wall).
    fn good(&self) -> bool {
        self.compared == 0
            || (self.median < 1e-3 && (self.compared < 20 || self.within_1_percent > 0.97))
    }
}

/// The snapshot's classes as a picture: game terrain green, other game
/// primitives blue, far terrain orange (darker farther), the sky black;
/// and the reference's sample grid over it (white: the reference sees
/// terrain where the picture has sky; red: no reference hit where far
/// terrain drew).
fn class_picture(kinds: &[u8], snapshot: &[[f32; 2]], w: usize, h: usize) -> Picture {
    let rgb = kinds
        .iter()
        .zip(snapshot)
        .map(|(&k, &[z, _])| match k {
            0 => 0x2a_8a_2a,
            1 => 0x30_50_c0,
            2 => {
                let shade = (255.0 - 40.0 * (f64::from(z) / 20_000.0).max(1.0).log2())
                    .clamp(60.0, 255.0) as u32;
                (shade << 16) | ((shade * 6 / 10) << 8)
            }
            _ => 0,
        })
        .collect();
    Picture {
        rgb,
        width: w,
        height: h,
    }
}

/// Classify the snapshot and check far terrain against the reference.
fn check_frame(
    fx_frame: &fx::FxFrame,
    map: &MapData,
    settings: &Settings,
    scale: u32,
    snapshot: &[[f32; 2]],
    vblank: u64,
) -> (FrameChecks, Vec<u8>, Picture) {
    let area = fx_frame.world_area;
    let (w, h) = (
        (area.w as u32 * scale) as usize,
        (area.h as u32 * scale) as usize,
    );
    let kind = |[z, order]: [f32; 2]| -> u8 {
        let order = order as usize;
        if order == 0 {
            if z >= SKY_Z { 3 } else { 2 }
        } else if fx_frame.classes.get(order - 1).and_then(|&c| Class::of(c))
            == Some(Class::Terrain)
        {
            0
        } else {
            1
        }
    };
    let kinds: Vec<u8> = snapshot.iter().map(|&p| kind(p)).collect();
    let mut checks = FrameChecks {
        vblank,
        ..FrameChecks::default()
    };
    for &k in &kinds {
        match k {
            0 => checks.terrain += 1,
            1 => checks.objects += 1,
            2 => checks.far += 1,
            _ => checks.sky += 1,
        }
    }
    // The seam: game terrain next to far terrain.
    let mut seam = vec![0u8; w * h];
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            if kinds[y * w + x] == 0
                && [
                    y * w + x - 1,
                    y * w + x + 1,
                    (y - 1) * w + x,
                    (y + 1) * w + x,
                ]
                .iter()
                .any(|&n| kinds[n] == 2)
            {
                seam[y * w + x] = 1;
                checks.seam += 1;
            }
        }
    }
    // The reference, on a grid of samples.
    let scene = &fx_frame.scene;
    let view = View::of(scene);
    let mut params = Params::new(scene, map, settings);
    let ring = params.march_start + 2048.0;
    params.march_start = 0.0;
    let sea = map.sea_level.map(f64::from);
    let (mut march, mut water) = (Vec::new(), Vec::new());
    let mut picture = class_picture(&kinds, snapshot, w, h);
    let mut mark = |i: usize, j: usize, colour: u32| {
        for y in j.saturating_sub(1)..(j + 2).min(h) {
            for x in i.saturating_sub(1)..(i + 2).min(w) {
                picture.rgb[y * w + x] = colour;
            }
        }
    };
    let stride = 4 * scale as usize;
    for j in (stride / 2..h).step_by(stride) {
        for i in (stride / 2..w).step_by(stride) {
            let k = kinds[j * w + i];
            if k != 2 && k != 3 {
                continue;
            }
            let s = [
                f64::from(area.x) + (i as f64 + 0.5) / f64::from(scale),
                f64::from(area.y) + (j as f64 + 0.5) / f64::from(scale),
            ];
            let d = view.ray(s);
            let hit = reference_march(map, &view, &params, d, view.h * f64::from(scale));
            let forward = d[0] * view.rows[2][0] + d[1] * view.rows[2][1] + d[2] * view.rows[2][2];
            if k == 2 {
                match hit {
                    Some(t) => {
                        let e = t * forward;
                        let error = (f64::from(snapshot[j * w + i][0]) - e).abs() / e;
                        let p = [view.eye[0] + d[0] * t, view.eye[2] + d[2] * t];
                        let flat_sea = |x: f64, z: f64| {
                            map.height_at(x, z, 0)
                                .zip(sea)
                                .is_some_and(|(h, level)| (h - level).abs() < 0.5)
                        };
                        // The card's own hit, from its z: within the ring it
                        // drew the exact meshes, which the heightmap only
                        // approximates (walls, roofs, crests).
                        let card_t = f64::from(snapshot[j * w + i][0]) / forward;
                        let dxz = d[0].hypot(d[2]);
                        if t * dxz > ring && card_t * dxz > ring {
                            march.push(error);
                        } else if (t * dxz > ring) != (card_t * dxz > ring) {
                            checks.ring_silhouettes += 1;
                        } else if [-64.0, 0.0, 64.0]
                            .iter()
                            .all(|&o| flat_sea(p[0] + o, p[1]) && flat_sea(p[0], p[1] + o))
                        {
                            water.push(error);
                        }
                    }
                    None => {
                        checks.no_reference += 1;
                        mark(i, j, 0xff_00_00);
                    }
                }
            } else {
                checks.sky_samples += 1;
                if hit.is_some() {
                    checks.sky_reference_hits += 1;
                    mark(i, j, 0xff_ff_ff);
                    // A gap: the reference still hits 2 pixels higher (up
                    // in the world), so this is not the horizon's edge.
                    let tilt = 2.0 / (view.h * f64::from(scale));
                    let up = [d[0], d[1] - tilt, d[2]];
                    let length = (up[0] * up[0] + up[1] * up[1] + up[2] * up[2]).sqrt();
                    let up = up.map(|c| c / length);
                    if let Some(u) =
                        reference_march(map, &view, &params, up, view.h * f64::from(scale))
                    {
                        if checks.gaps < 3 {
                            let t = hit.unwrap_or(0.0);
                            let p = [
                                view.eye[0] + d[0] * t,
                                view.eye[1] + d[1] * t,
                                view.eye[2] + d[2] * t,
                            ];
                            let below = (1..6)
                                .map(|k| snapshot[(j + k).min(h - 1) * w + i][0])
                                .collect::<Vec<_>>();
                            eprintln!(
                                "gap at ({i}, {j}): reference t {t:.0} at {p:?} (height {:?}), 2 px up t {u:.0}; eye {:?}, d {d:?}; march end {:.0}; z below {below:?}",
                                map.height_at(p[0], p[2], 0),
                                view.eye,
                                params.march_end
                            );
                        }
                        checks.gaps += 1;
                        mark(i, j, 0xff_00_ff);
                    }
                }
            }
        }
    }
    checks.march = ErrorStats::of(march);
    checks.sea = ErrorStats::of(water);
    (checks, seam, picture)
}

/// The densest `cw` x `ch` window of seam pixels (N x), stepping by a
/// quarter window.
fn seam_window(
    seam: &[u8],
    w: usize,
    h: usize,
    cw: usize,
    ch: usize,
) -> Option<(usize, usize, usize)> {
    let mut best = None;
    for y in (0..h.saturating_sub(ch)).step_by(ch / 4) {
        for x in (0..w.saturating_sub(cw)).step_by(cw / 4) {
            let count: usize = (y..y + ch)
                .map(|r| {
                    seam[r * w + x..r * w + x + cw]
                        .iter()
                        .map(|&s| usize::from(s))
                        .sum::<usize>()
                })
                .sum();
            if best.is_none_or(|(_, _, c)| count > c) {
                best = Some((x, y, count));
            }
        }
    }
    best.filter(|(_, _, c)| *c > 0)
}

#[test]
#[ignore = "requires the Japanese disc, its flight save state and a graphics adapter"]
fn far_terrain_in_jp_flight() {
    let data = GameData::load_jp_disc(&disc()).unwrap();
    let device = offscreen_device().expect("a graphics adapter");
    let out = out();
    let saves = env_list("AC3_FAR_SAVE", "50101,50701,50901,51101");
    // Held buttons by VBlank, the same in every player (`AC3_FAR_PAD`, as
    // `--pad`: e.g. "50001-50100:down" pulls up toward the hills).
    let pad = std::env::var("AC3_FAR_PAD")
        .ok()
        .map(|text| machine_player::parse_pad_schedule(&text).expect("a pad schedule"))
        .unwrap_or_default();
    let last = *saves.iter().max().unwrap();
    let mut report = serde_json::Map::new();
    let mut failures = Vec::new();
    for scale in env_list("AC3_FAR_SCALES", "2,4") {
        let scale = scale as u32;
        let as_is = Setup {
            name: "as-is",
            effects: None,
            settings: Effects::default(),
            far_by_hand: false,
        };
        let mut setups = vec![
            as_is,
            // The palette redirect alone (the atmosphere without a haze).
            setup("redirect", Some(0.0), false),
            setup("redirect-far", Some(0.0), true),
            // The atmosphere's haze over far terrain, light and dense.
            setup("haze1-far", Some(1.0), true),
            setup("haze3-far", Some(3.0), true),
        ];
        if scale == 2 {
            // The atmosphere alone (its edge fade at the draw distance).
            setups.push(setup("haze1", Some(1.0), false));
            // Far terrain without the redirect: the fog wall.
            setups.push(Setup {
                far_by_hand: true,
                ..setup("far-only", None, true)
            });
            // Identity: effects configured but off, everything registered
            // and set; effects on, everything registered but off.
            setups.push(Setup {
                effects: Some(false),
                ..setup("off-registered", Some(1.0), true)
            });
            setups.push(setup("on-far-off", None, false));
        }
        let mut players: Vec<(Player, Executor)> = setups
            .iter()
            .map(|&s| player(&data, &device, scale, s))
            .collect();
        let names: Vec<&str> = setups.iter().map(|s| s.name).collect();
        let index = |name: &str| names.iter().position(|n| *n == name).unwrap();
        let mut pictures: BTreeMap<(u64, &str), Picture> = BTreeMap::new();
        let mut checks = Vec::new();
        let mut identical = 0u64;
        let mut passes = 0u64;
        let mut next = 0;
        while players[0].0.vblanks() < last {
            let now = players[0].0.vblanks();
            let held = pad
                .iter()
                .filter(|(first, end, _)| (*first..=*end).contains(&now))
                .fold(0, |held, (_, _, buttons)| held | buttons);
            if !pad.is_empty() {
                for (player, _) in &mut players {
                    player.set_buttons(held);
                }
            }
            let frames: Vec<Frame> = players.iter_mut().map(|(p, _)| step(p)).collect();
            passes += 1;
            let ram = players[0].0.machine.ram().to_vec();
            for ((player, _), name) in players.iter().zip(&names).skip(1) {
                assert!(
                    player.machine.ram() == ram,
                    "pass {passes}: {name}'s RAM differs"
                );
            }
            for ((_, executor), frame) in players.iter_mut().zip(&frames) {
                executor.execute(frame);
            }
            let vblank = players[0].0.vblanks();
            let area = display_area(&players[0].0.host.borrow()).expect("a 15-bit picture");
            if scale == 2 {
                let base = read(&mut players[0].1, area);
                let off = read(&mut players[index("off-registered")].1, area);
                assert!(
                    off.rgb == base.rgb,
                    "pass {passes}: effects off with far terrain registered changed the picture"
                );
                let on = read(&mut players[index("on-far-off")].1, area);
                assert!(
                    on.rgb == base.rgb,
                    "pass {passes}: far_terrain = false changed the picture"
                );
                identical += 1;
            }
            let saving = next < saves.len() && vblank >= saves[next];
            if !saving {
                continue;
            }
            next += 1;
            for (k, (_, executor)) in players.iter_mut().enumerate() {
                let picture = read(executor, area);
                picture.save(&out.join(format!("{vblank:06}-{scale}x-{}.png", names[k])));
                pictures.insert((vblank, names[k]), picture);
            }
            // The far-terrain player's frame: checks and the seam.
            let far = index("redirect-far");
            let fx_frame = frames[far].fx.as_deref().expect("an effects frame");
            let far_frame = fx_frame
                .data
                .iter()
                .find(|(name, _)| *name == far_terrain::NAME)
                .and_then(|(_, data)| data.downcast_ref::<FarFrame>())
                .expect("far terrain's data");
            let (snapshot, _) = players[far].1.read_fx_snapshot().expect("a snapshot");
            let (frame_checks, seam, classes) = check_frame(
                fx_frame,
                &far_frame.map,
                &far_frame.settings,
                scale,
                &snapshot,
                vblank,
            );
            eprintln!("{scale}x VBlank {vblank}: {frame_checks:?}");
            classes.save(&out.join(format!("classes-{vblank:06}-{scale}x.png")));
            let (w, h) = (
                (fx_frame.world_area.w as u32 * scale) as usize,
                (fx_frame.world_area.h as u32 * scale) as usize,
            );
            let (cw, ch) = (80 * scale as usize, 60 * scale as usize);
            if let Some((x, y, _)) = seam_window(&seam, w, h, cw, ch) {
                let zoom = (8 / scale as usize).max(2);
                // The world area is the display area in flight.
                let crops: Vec<Picture> = ["as-is", "redirect", "redirect-far", "haze1-far"]
                    .iter()
                    .map(|name| pictures[&(vblank, *name)].crop(x, y, cw, ch, zoom))
                    .collect();
                Picture::sheet(&crops, 4).save(&out.join(format!("seam-{vblank:06}-{scale}x.png")));
            }
            let dropped = |name: &str| {
                players[index(name)]
                    .0
                    .host
                    .borrow()
                    .hw
                    .as_ref()
                    .unwrap()
                    .effects_stats()
                    .unwrap()
                    .dropped
            };
            assert!(
                dropped("redirect-far") > dropped("redirect"),
                "the backdrop strip is dropped with far terrain only"
            );
            if !(frame_checks.far > 0
                && frame_checks.march.good()
                && frame_checks.sea.good()
                && frame_checks.gaps == 0)
            {
                failures.push(format!("{scale}x {frame_checks:?}"));
            }
            checks.push(frame_checks);
        }
        // The contact sheet: rows the saved frames, columns the setups shown.
        let shown: Vec<&str> = names
            .iter()
            .copied()
            .filter(|n| !matches!(*n, "off-registered" | "on-far-off"))
            .collect();
        let shrink = if scale > 2 { scale as usize / 2 } else { 1 };
        let cells: Vec<Picture> = saves
            .iter()
            .flat_map(|v| shown.iter().map(move |n| (*v, *n)))
            .filter_map(|key| pictures.get(&key).map(|p| p.shrink(shrink)))
            .collect();
        if !cells.is_empty() {
            Picture::sheet(&cells, shown.len()).save(&out.join(format!("sheet-{scale}x.png")));
        }
        let far = index("redirect-far");
        let card = players[far]
            .0
            .host
            .borrow()
            .hw
            .as_ref()
            .unwrap()
            .effects_stats()
            .cloned()
            .unwrap();
        report.insert(
            format!("{scale}x"),
            serde_json::json!({
                "passes": passes,
                "identical_pictures_checked": identical,
                "columns": shown,
                "frames": checks,
                "recorder": card,
            }),
        );
    }
    let text = serde_json::to_string_pretty(&serde_json::Value::Object(report)).unwrap();
    fs::write(out.join("far-terrain.json"), &text).unwrap();
    eprintln!("{text}");
    assert!(
        failures.is_empty(),
        "far terrain against the reference:\n{}",
        failures.join("\n")
    );
}

/// The GPU time of far terrain at 4x (`AC3_FAR_TIME_SCALE`), every pass
/// from the state to VBlank 51101, on a device with timestamp queries.
#[test]
#[ignore = "requires the Japanese disc, its flight save state and a graphics adapter"]
fn far_terrain_gpu_time() {
    let data = GameData::load_jp_disc(&disc()).unwrap();
    let scale = env_list("AC3_FAR_TIME_SCALE", "4")[0] as u32;
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY.with_env(),
        ..wgpu::InstanceDescriptor::new_without_display_handle_from_env()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    }))
    .expect("an adapter");
    eprintln!("adapter: {:?}", adapter.get_info());
    let features = adapter.features() & wgpu::Features::TIMESTAMP_QUERY;
    assert!(!features.is_empty(), "the adapter has no timestamp queries");
    let device = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("ac3 far terrain timing"),
        required_features: features,
        required_limits:
            wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits()),
        ..Default::default()
    }))
    .unwrap();
    // The atmosphere alone first: the whole frame's time without far
    // terrain.
    for (far, quality) in [
        (false, Quality::Medium),
        (true, Quality::Medium),
        (true, Quality::Low),
        (true, Quality::High),
    ] {
        let mut setup = setup("timing", Some(1.0), far);
        setup.settings.far_terrain_quality = quality;
        let (mut player, _) = player(&data, &device, scale, setup);
        // The card `player` registered has no timer handle: a fresh
        // executor with the atmosphere's card and a timed far terrain card.
        let mut executor = Executor::new(
            device.0.clone(),
            device.1.clone(),
            Options {
                scale,
                dither: false,
            },
            wgpu::TextureFormat::Rgba8Unorm,
        )
        .unwrap();
        atmosphere_fx::register_card(
            &mut executor,
            atmosphere_fx::Config::from_settings(&setup.settings),
        );
        let card = FarTerrainCard::new();
        let timer = card.timer();
        let card_stats = card.stats();
        executor.add_effect(Box::new(card));
        let mut times: Vec<far_terrain::PassTimes> = Vec::new();
        let mut frame_times = Vec::new();
        while player.vblanks() < 51_101 {
            let frame = step(&mut player);
            let started = std::time::Instant::now();
            executor.execute(&frame);
            executor.finish();
            frame_times.push(started.elapsed().as_secs_f64() * 1e3);
            if far
                && frame.fx.is_some()
                && let Some(t) = timer.read()
            {
                times.push(t);
                if std::env::var_os("AC3_FAR_TIME_TRACE").is_some()
                    && times.len().is_multiple_of(10)
                {
                    eprintln!(
                        "VBlank {}: {:.3} ms (meshes {:.3}, march {:.3}, composite {:.3})",
                        player.vblanks(),
                        t.total(),
                        t.meshes,
                        t.march,
                        t.composite
                    );
                }
            }
        }
        let sorted = |f: &dyn Fn(&far_terrain::PassTimes) -> f64| {
            let mut v: Vec<f64> = times.iter().map(f).collect();
            v.sort_by(f64::total_cmp);
            v
        };
        frame_times.sort_by(f64::total_cmp);
        let pick = |v: &[f64], q: f64| {
            v.get(((v.len().max(1) - 1) as f64 * q) as usize)
                .copied()
                .unwrap_or(0.0)
        };
        if !far {
            eprintln!(
                "{scale}x atmosphere alone: whole frame (CPU submit to finish) median {:.3} ms",
                pick(&frame_times, 0.5)
            );
            continue;
        }
        let total = sorted(&|t| t.total());
        let line = |name: &str, v: &[f64]| {
            format!(
                "{name} median {:.3}, 90% {:.3}, max {:.3}",
                pick(v, 0.5),
                pick(v, 0.9),
                pick(v, 1.0)
            )
        };
        eprintln!(
            "{scale}x {}: far terrain GPU time over {} frames, ms: {}; {}; {}; {}; whole frame (CPU submit to finish) median {:.3}",
            quality.name(),
            times.len(),
            line("all", &total),
            line("meshes", &sorted(&|t| t.meshes)),
            line("march", &sorted(&|t| t.march)),
            line("composite", &sorted(&|t| t.composite)),
            pick(&frame_times, 0.5)
        );
        let card = card_stats.lock().unwrap().clone();
        eprintln!(
            "{scale}x {}: card side CPU per frame: mean {:.3} ms, max {:.3} ms (the first: the whole map's upload); last frame {} cells drawn, {} culled, {} draws",
            quality.name(),
            card.cpu_micros as f64 / card.frames.max(1) as f64 / 1e3,
            card.cpu_micros_max as f64 / 1e3,
            card.cells,
            card.culled,
            card.draws
        );
        assert!(pick(&total, 0.5) < 5.0, "a few ms at most");
    }
}

// ---- the sky slot ----------------------------------------------------------------------------

#[derive(Default)]
struct Probe {
    seen: BTreeMap<String, u64>,
    keys: BTreeMap<(u16, u16), u64>,
    strips: u64,
    frames: u64,
}

struct ProbeEffect(Rc<RefCell<Probe>>);

impl fx::RecordEffect for ProbeEffect {
    fn name(&self) -> &'static str {
        "probe"
    }
    fn kick(&mut self, _scene: &fx::Scene, _ram: &[u8], _vram: &Vram) {
        self.0.borrow_mut().frames += 1;
    }
    fn primitive(&mut self, p: &Primitive<'_>) -> Route {
        if p.phase != Phase::Sky {
            return Route::Draw;
        }
        let mut probe = self.0.borrow_mut();
        let key = format!(
            "GP0 {:02x}, semi {}, {} words, texture {:?}",
            p.command,
            p.semi,
            p.words.len(),
            p.texture
        );
        *probe.seen.entry(key).or_default() += 1;
        if (0x2c..=0x2f).contains(&p.command) && p.words.len() >= 9 {
            let clut = (p.words[2] >> 16) as u16;
            let tpage = (p.words[4] >> 16) as u16;
            *probe.keys.entry((tpage, clut)).or_default() += 1;
        }
        if far_terrain::is_backdrop_strip(p) {
            probe.strips += 1;
        }
        Route::Draw
    }
}

/// What mission 1's sky slot draws: bands (GP0 20/28/30/38), the fog strip
/// 16 fill (60), stars (6A), the sun's corona (2E, palette (352, 311/312))
/// and the backdrop strip (2E, palette 0x4ED6 = (352, 315), page 0x39), a
/// chain of six or so quads along the horizon. Writes each FT4 key's
/// texture.
#[test]
#[ignore = "requires the Japanese disc and its flight save state"]
fn jp_sky_slot_primitives() {
    let data = GameData::load_jp_disc(&disc()).unwrap();
    let device = offscreen_device().expect("a graphics adapter");
    let setup = setup("probe", None, false);
    let (mut player, _) = player(&data, &device, 2, setup);
    let probe = Rc::new(RefCell::new(Probe::default()));
    player
        .host
        .borrow_mut()
        .hw
        .as_mut()
        .unwrap()
        .add_effect(Box::new(ProbeEffect(probe.clone())));
    while player.vblanks() < 50_400 {
        step(&mut player);
    }
    let probe = probe.borrow();
    eprintln!("{} effects frames; sky slot primitives:", probe.frames);
    for (key, count) in &probe.seen {
        eprintln!("{count:6}  {key}");
    }
    eprintln!("backdrop strip quads: {}", probe.strips);
    assert!(probe.strips >= probe.frames, "a strip every frame");
    let host = player.host.borrow();
    let vram = host.gpu.gpu.vram().words();
    for &(tpage, clut) in probe.keys.keys() {
        let key = TextureKey { tpage, clut };
        let rgb: Vec<u32> = (0..=255u8)
            .flat_map(|v| (0..=255u8).map(move |u| (u, v)))
            .map(|(u, v)| {
                let t = sample_texture(vram, key, u, v).unwrap_or(0);
                let c = |s: u16| u32::from((t >> s) & 31) * 255 / 31;
                (c(0) << 16) | (c(5) << 8) | c(10)
            })
            .collect();
        let picture = Picture {
            rgb,
            width: 256,
            height: 256,
        };
        picture.save(&out().join(format!("sky-key-{tpage:04x}-{clut:04x}.png")));
        eprintln!(
            "FT4 key: page {tpage:04x}, palette {clut:04x}: {} quads",
            probe.keys[&(tpage, clut)]
        );
    }
}
