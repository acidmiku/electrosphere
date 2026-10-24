//! The atmosphere in Japanese flight (`docs/EFFECTS_ATMOSPHERE.md` §8):
//! seven players start from the mission 1 flight save state with the GPU
//! renderer at 2x and sub-pixel geometry, and run in lockstep through the
//! cockpit view, the replay's external views and its explosion (VBlank
//! 50,000 to about 52,200):
//!
//! | Player | Effects | Atmosphere |
//! | --- | --- | --- |
//! | `plain` | configured, off | not registered (main as it is) |
//! | `off` | off (F6) | on, density 1, both halves |
//! | `noop` | on | off, both halves registered |
//! | `redirect` | on | the recorder half alone (the redirect, no post) |
//! | `density0` | on | on, density 0 |
//! | `density1`, `density3` | on | on, density 1 and 3 |
//!
//! [`atmosphere_in_jp_flight`] checks every main-loop pass:
//! - machine RAM is the same in all seven (the game never sees effects);
//! - `off` and `noop` show `plain`'s picture, pixel for pixel;
//! - `density0` shows `redirect`'s picture, pixel for pixel;
//!
//! and measures how often the redirect and the haze change the picture, the
//! pixels without a depth by class, and the coarse depth's error
//! ([`atmosphere_fx::coarse_depth`] against the view z of the terrain pixels
//! that have one). Pictures (`NNNNNN-<player>.png`) and `atmosphere.json`
//! go to `$AC3_EFFECTS_OUT` (default `target/effects-atmosphere`).
//!
//! Needs the Japanese disc 1 (`$AC3_JP_DISC`), the flight save state
//! (`$AC3_JP_FLIGHT_STATE`, default `~/ac3-jp/runs/present-work/at50000.ac3state`)
//! and a graphics adapter.
use ac3_graphics::Vram;
use ac3_port::assets::GameData;
use ac3_port::effects::atmosphere::{Haze, HazeInput};
use ac3_port::effects::atmosphere_fx::{self, Config, FrameData};
use ac3_port::gpu_renderer::execute::offscreen_device;
use ac3_port::gpu_renderer::fx::{self, Class, DebugView, SKY_Z};
use ac3_port::gpu_renderer::{DisplayArea, Executor, Frame, Options, Recorder, TablePrecision};
use ac3_port::machine_card::{CardDevice, CardSource};
use ac3_port::machine_player::{self, Host, PadType, Player};
use ac3_port::savestate::{self, GameId};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::rc::Rc;

const SCALE: u32 = 2;
/// VBlanks whose pictures are saved: the cockpit, the replay over the coast
/// and hills, the replay's explosion (fx-fog's frames).
const SAVED: [u64; 5] = [50_101, 50_701, 50_901, 51_101, 52_101];
/// The run ends at this VBlank.
const UNTIL: u64 = 52_160;
/// Every this many passes the depth snapshot is measured.
const MEASURE_EVERY: u64 = 10;

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
    let out = std::env::var_os("AC3_EFFECTS_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/effects-atmosphere"));
    fs::create_dir_all(&out).unwrap();
    out
}

/// One player's set-up.
struct Setup {
    name: &'static str,
    /// `[effects] enabled`.
    enabled: bool,
    /// The halves registered, and their configuration.
    record: Option<Config>,
    card: Option<Config>,
}

fn atmosphere(density: f32) -> Config {
    Config {
        on: true,
        density,
        edge: true,
    }
}

fn setups() -> Vec<Setup> {
    let off = Config::default();
    let setup = |name, enabled, record, card| Setup {
        name,
        enabled,
        record,
        card,
    };
    vec![
        setup("plain", false, None, None),
        setup("off", false, Some(atmosphere(1.0)), Some(atmosphere(1.0))),
        setup("noop", true, Some(off), Some(off)),
        setup("redirect", true, Some(atmosphere(0.0)), None),
        setup(
            "density0",
            true,
            Some(atmosphere(0.0)),
            Some(atmosphere(0.0)),
        ),
        setup(
            "density1",
            true,
            Some(atmosphere(1.0)),
            Some(atmosphere(1.0)),
        ),
        setup(
            "density3",
            true,
            Some(atmosphere(3.0)),
            Some(atmosphere(3.0)),
        ),
    ]
}

/// A JP player in flight from the save state with the GPU renderer at 2x
/// (the recorder as `Host::start_gpu_renderer` makes it, the atmosphere's
/// recorder half registered by `setup`).
fn player(data: &GameData, setup: &Setup) -> Player {
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
    let mut recorder = Recorder::new(
        Options {
            scale: SCALE,
            dither: false,
        },
        &host.gpu.gpu,
    );
    recorder.precision = host.vertex_precision.clone();
    recorder.perspective = true;
    recorder.set_effects(Some(fx::Config {
        enabled: setup.enabled,
        debug: DebugView::Off,
        x_scale: None,
    }));
    if let Some(config) = setup.record {
        atmosphere_fx::register_recorder(&mut recorder, config);
    }
    host.hw = Some(Box::new(recorder));
    let mut player = Player::new(ac3_flight::Machine::default(), host);
    player.machine.set_precision(Some(table));
    game.boot(&mut player.machine, &data.executable.payload);
    player.machine.take_fault().unwrap();
    savestate::load(&mut player, GameId::of(&data.executable.payload), &state()).unwrap();
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

fn differing(a: &[u32], b: &[u32]) -> usize {
    a.iter().zip(b).filter(|(x, y)| x != y).count()
}

/// Sorted samples, summarised.
#[derive(Default)]
struct Samples(Vec<f32>);

impl Samples {
    fn report(&mut self) -> serde_json::Value {
        let v = &mut self.0;
        v.sort_by(f32::total_cmp);
        let at = |q: f64| {
            v.get(((v.len() as f64 - 1.0) * q).round() as usize)
                .copied()
        };
        serde_json::json!({
            "count": v.len(),
            "median": at(0.5),
            "p90": at(0.9),
            "p99": at(0.99),
            "max": v.last(),
        })
    }
}

/// The depth snapshot of an effects frame, measured: pixels without a depth
/// by class (and how many get a coarse depth), and for terrain pixels with
/// one, the coarse depth's error.
#[derive(Default)]
struct Depths {
    /// Pixels without a view z, by class: (pixels, with a coarse depth).
    no_depth: BTreeMap<&'static str, (u64, u64)>,
    /// Terrain pixels with a view z, near (slot < 911: the faces the game
    /// subdivides, where the ones without a depth are) and all: relative
    /// error of the coarse depth, and its haze error (transmittance) at
    /// densities 1 and 3.
    near_relative: Samples,
    near_haze1: Samples,
    near_haze3: Samples,
    all_relative: Samples,
    /// The slot's depth alone (no sea plane), near.
    near_slot_only: Samples,
}

impl Depths {
    fn add(
        &mut self,
        frame: &fx::FxFrame,
        data: &FrameData,
        snapshot: &[[f32; 2]],
        rect: [u32; 4],
    ) {
        let scene = &frame.scene;
        let view = atmosphere_fx::scene_view(scene);
        let eye_y = scene.eye_units()[1];
        let haze = |density: f32| Haze {
            density: Haze::default().density * density,
            sea_level: data.sea_level,
            ..Haze::default()
        };
        let (haze1, haze3) = (haze(1.0), haze(3.0));
        for (i, &[z, order]) in snapshot.iter().enumerate() {
            if z >= SKY_Z * 0.5 {
                continue;
            }
            let entry = atmosphere_fx::order_entry(frame, order as u32);
            let class = Class::of((entry >> 16) as u8).map_or("none", Class::name);
            let slot = (entry & 0xffff) as u16;
            let (px, py) = (rect[0] + i as u32 % rect[2], rect[1] + i as u32 / rect[2]);
            let ray = view.to_world(atmosphere_fx::pixel_ray(scene, SCALE, px, py));
            let coarse = atmosphere_fx::coarse_depth(slot, ray[1], eye_y, Some(data.sea_level));
            if z <= 0.0 {
                let entry = self.no_depth.entry(class).or_default();
                entry.0 += 1;
                entry.1 += u64::from(coarse.is_some());
                continue;
            }
            if class != "terrain" {
                continue;
            }
            let Some(estimate) = coarse else {
                continue;
            };
            let relative = (estimate - z) / z;
            self.all_relative.0.push(relative.abs());
            if slot < 911 {
                self.near_relative.0.push(relative.abs());
                let slot_only = atmosphere_fx::coarse_depth(slot, ray[1], eye_y, None).unwrap();
                self.near_slot_only.0.push(((slot_only - z) / z).abs());
                let input = |depth: f32| HazeInput {
                    camera_y: eye_y,
                    offset: ray.map(|c| c * depth),
                    depth,
                };
                for (haze, samples) in [
                    (&haze1, &mut self.near_haze1),
                    (&haze3, &mut self.near_haze3),
                ] {
                    let error = (haze.transmittance(&input(estimate))
                        - haze.transmittance(&input(z)))
                    .abs();
                    samples.0.push(error);
                }
            }
        }
    }

    fn report(&mut self) -> serde_json::Value {
        let no_depth: serde_json::Map<String, serde_json::Value> = self
            .no_depth
            .iter()
            .map(|(class, (pixels, coarse))| {
                (
                    (*class).to_owned(),
                    serde_json::json!({ "pixels": pixels, "with_coarse_depth": coarse }),
                )
            })
            .collect();
        serde_json::json!({
            "no_depth": no_depth,
            "coarse_relative_error_near_terrain": self.near_relative.report(),
            "slot_only_relative_error_near_terrain": self.near_slot_only.report(),
            "coarse_transmittance_error_near_terrain_density1": self.near_haze1.report(),
            "coarse_transmittance_error_near_terrain_density3": self.near_haze3.report(),
            "coarse_relative_error_all_terrain": self.all_relative.report(),
        })
    }
}

#[test]
#[ignore = "requires the Japanese disc, its flight save state and a graphics adapter"]
fn atmosphere_in_jp_flight() {
    let data = GameData::load_jp_disc(&disc()).unwrap();
    let (device, queue) = offscreen_device().expect("a graphics adapter");
    let setups = setups();
    let index = |name: &str| setups.iter().position(|s| s.name == name).unwrap();
    let (plain, off, noop, redirect, density0, density1) = (
        index("plain"),
        index("off"),
        index("noop"),
        index("redirect"),
        index("density0"),
        index("density1"),
    );
    let mut players: Vec<Player> = setups.iter().map(|s| player(&data, s)).collect();
    let mut cards: Vec<Executor> = setups
        .iter()
        .map(|setup| {
            let mut executor = Executor::new(
                device.clone(),
                queue.clone(),
                Options {
                    scale: SCALE,
                    dither: false,
                },
                wgpu::TextureFormat::Rgba8Unorm,
            )
            .unwrap();
            if let Some(config) = setup.card {
                atmosphere_fx::register_card(&mut executor, config);
            }
            executor
        })
        .collect();
    let out = out();
    let mut saved = SAVED.iter().copied().peekable();
    let mut depths = Depths::default();
    let (mut passes, mut effects_frames, mut identical, mut redirect_same) =
        (0u64, 0u64, 0u64, 0u64);
    let (mut redirect_changed, mut haze_changed) = (0u64, 0u64);
    let mut sea_levels = BTreeMap::<String, u64>::new();
    let mut semi_terrain = 0u64;
    let mut changed_pixels = serde_json::Map::new();
    while players[0].vblanks() < UNTIL {
        passes += 1;
        let frames: Vec<Frame> = players.iter_mut().map(step).collect();
        let ram = players[0].machine.ram().to_vec();
        for (player, setup) in players.iter().zip(&setups).skip(1) {
            assert!(
                player.machine.ram() == ram,
                "pass {passes}: {}'s RAM",
                setup.name
            );
        }
        let vblank = players[0].vblanks();
        assert!(frames[plain].fx.is_none() && frames[off].fx.is_none());
        if let Some(fx) = frames[density1].fx.as_deref() {
            effects_frames += 1;
            if let Some(data) = fx
                .data
                .iter()
                .find(|(name, _)| *name == atmosphere_fx::NAME)
                .and_then(|(_, data)| data.downcast_ref::<FrameData>())
            {
                *sea_levels.entry(data.sea_level.to_string()).or_default() += 1;
                semi_terrain += u64::from(data.semi_terrain);
            }
        }
        for (card, frame) in cards.iter_mut().zip(&frames) {
            card.execute(frame);
        }
        if passes.is_multiple_of(MEASURE_EVERY)
            && let Some(fx) = frames[density1].fx.as_deref()
        {
            let data = fx
                .data
                .iter()
                .find_map(|(_, data)| data.downcast_ref::<FrameData>())
                .copied()
                .unwrap_or_default();
            let (snapshot, rect) = cards[density1].read_fx_snapshot().expect("a snapshot");
            depths.add(fx, &data, &snapshot, rect);
        }
        let Some(area) = display_area(&players[0].host.borrow()) else {
            continue;
        };
        let wanted = saved.peek().is_some_and(|&at| vblank >= at);
        let read: Vec<usize> = if wanted {
            (0..setups.len()).collect()
        } else {
            vec![plain, off, noop, redirect, density0, density1]
        };
        let mut pictures: Vec<Option<(Vec<u32>, usize, usize)>> = vec![None; setups.len()];
        for &i in &read {
            pictures[i] = cards[i].read_display(area);
        }
        let picture = |i: usize| &pictures[i].as_ref().unwrap().0;
        for other in [off, noop] {
            let n = differing(picture(plain), picture(other));
            assert_eq!(
                n, 0,
                "pass {passes} (VBlank {vblank}): {} differs",
                setups[other].name
            );
        }
        identical += 1;
        let n = differing(picture(redirect), picture(density0));
        assert_eq!(
            n, 0,
            "pass {passes} (VBlank {vblank}): density 0 is not the redirect"
        );
        redirect_same += 1;
        redirect_changed += u64::from(differing(picture(plain), picture(redirect)) > 0);
        haze_changed += u64::from(differing(picture(redirect), picture(density1)) > 0);
        if wanted {
            let at = saved.next().unwrap();
            let mut counts = serde_json::Map::new();
            for (i, setup) in setups.iter().enumerate() {
                let (rgb, width, height) = pictures[i].as_ref().unwrap();
                save_png(
                    &out.join(format!("{at:06}-{}.png", setup.name)),
                    rgb,
                    *width,
                    *height,
                );
                counts.insert(
                    setup.name.to_owned(),
                    serde_json::json!(differing(picture(plain), rgb)),
                );
            }
            changed_pixels.insert(format!("{at:06} (VBlank {vblank})"), counts.into());
        }
    }
    let stats = |i: usize| {
        players[i]
            .host
            .borrow()
            .hw
            .as_ref()
            .unwrap()
            .effects_stats()
            .cloned()
            .unwrap()
    };
    let report = serde_json::json!({
        "passes": passes,
        "effects_frames": effects_frames,
        "identical_pictures_plain_off_noop": identical,
        "density0_equals_redirect": redirect_same,
        "passes_the_redirect_changed": redirect_changed,
        "passes_the_haze_changed": haze_changed,
        "pixels_changed_from_plain": changed_pixels,
        "sea_levels": sea_levels,
        "semi_transparent_terrain_primitives": semi_terrain,
        "depths": depths.report(),
        "recorder_density1": stats(density1),
        "recorder_noop": stats(noop),
        "card_density1": cards[density1].effects_stats(),
    });
    let text = serde_json::to_string_pretty(&report).unwrap();
    fs::write(out.join("atmosphere.json"), &text).unwrap();
    eprintln!("{text}");
    assert!(
        effects_frames > passes / 2,
        "{effects_frames} effects frames"
    );
    assert!(redirect_changed > 0, "the redirect changed the picture");
    assert!(haze_changed > 0, "the haze changed the picture");
    assert_eq!(stats(noop).redirected, 0, "off: no redirect");
    assert!(stats(density1).redirected > 0);
    assert!(saved.next().is_none(), "every picture saved");
}
