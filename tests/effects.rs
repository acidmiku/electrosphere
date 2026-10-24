//! The effects infrastructure (`docs/EFFECTS.md`, Milestone 1 Phase 4 item
//! 4) on Japanese flight: four players start from the same flight save
//! state with the GPU renderer at 2x and sub-pixel geometry, and run in
//! lockstep: effects off, effects on (every effect a no-op), and effects on
//! with the depth and the coverage debug views.
//!
//! [`effects_in_jp_flight`] checks, every main-loop pass:
//! - machine RAM is the same in all four (the game never sees effects);
//! - the effects-off frames are not effects frames;
//! - each effects frame's markers fired at the flight ordering table's
//!   entries 4095 (sky), 4094 (world begin), 1 (world end) and 0 (HUD), in
//!   that order, of the table the pass built (world `+0x10`), which is one of
//!   the pair at world `+0x18`/`+0x1C`;
//! - the pictures with effects on and off are identical, pixel for pixel
//!   (deferred transparency with a no-op post is the PlayStation's picture);
//!
//! and measures the depth coverage: of the world area's pixels at world
//! end, by the class of the primitive that drew each last (terrain: a
//! palette in the fog's block; other polygons, aircraft among them;
//! sprites; lines; fills and copies; the sky: no world primitive), the
//! share with a view z. Pictures (off, on, depth, coverage) and
//! `coverage.json` go to `$AC3_EFFECTS_OUT` (default `target/effects`).
//!
//! Needs the Japanese disc 1 (`$AC3_JP_DISC`, default
//! `~/ac3-jp/discs/... (Disc 1) (Rev 1).cue`), the flight save state
//! (`$AC3_JP_FLIGHT_STATE`, default
//! `~/ac3-jp/runs/present-work/at50000.ac3state`) and a graphics adapter.
use ac3_graphics::Vram;
use ac3_port::assets::GameData;
use ac3_port::gpu_renderer::execute::offscreen_device;
use ac3_port::gpu_renderer::fx::{self, Class, DebugView, Marker, SKY_Z};
use ac3_port::gpu_renderer::{DisplayArea, Executor, Frame, Options, TablePrecision};
use ac3_port::machine_card::{CardDevice, CardSource};
use ac3_port::machine_player::{self, Host, PadType, Player};
use ac3_port::savestate::{self, GameId};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::rc::Rc;

const SCALE: u32 = 2;
/// Main-loop passes run (JP flight draws a frame every two VBlanks).
const PASSES: u64 = 120;
/// Passes whose pictures are saved.
const SAVED: [u64; 3] = [10, 60, 110];
/// SELECT (the external view) is held for these passes; coverage is
/// counted apart before (cockpit) and after (xternal).
const SELECT: std::ops::RangeInclusive<u64> = 62..=64;
const EXTERNAL_FROM: u64 = 75;

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
        .unwrap_or_else(|| PathBuf::from("target/effects"));
    fs::create_dir_all(&out).unwrap();
    out
}

/// A JP player in flight from the save state, as `machine_player::run`
/// builds one, with the GPU renderer at 2x and `effects`.
fn player(data: &GameData, effects: fx::Config) -> Player {
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
    host.effects = Some(effects);
    host.start_gpu_renderer(Options {
        scale: SCALE,
        dither: false,
    });
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

/// The displayed area (as `machine_player` shows it, without the held
/// field: the four players agree on it either way).
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

/// Pixels of the world area, and those with a view z, by the class of the
/// primitive that drew each last.
#[derive(Default, Debug)]
struct Coverage(BTreeMap<&'static str, (u64, u64)>);

impl Coverage {
    fn add(&mut self, frame: &fx::FxFrame, snapshot: &[[f32; 2]]) {
        for &[z, order] in snapshot {
            let order = order as u32;
            let class = if order == 0 {
                "sky"
            } else {
                frame
                    .classes
                    .get(order as usize - 1)
                    .and_then(|&class| Class::of(class))
                    .map_or("unknown", Class::name)
            };
            let entry = self.0.entry(class).or_default();
            entry.0 += 1;
            if z > 0.0 {
                entry.1 += 1;
            }
            if order == 0 {
                assert_eq!(z, SKY_Z, "the sky keeps the clear value");
            }
        }
    }

    fn report(&self) -> serde_json::Value {
        let total: u64 = self.0.values().map(|(pixels, _)| pixels).sum();
        let mut report = serde_json::Map::new();
        for (class, (pixels, with_z)) in &self.0 {
            report.insert(
                (*class).to_owned(),
                serde_json::json!({
                    "pixels": pixels,
                    "share_of_picture_percent": 100.0 * *pixels as f64 / total.max(1) as f64,
                    "with_z": with_z,
                    "with_z_percent": 100.0 * *with_z as f64 / (*pixels).max(1) as f64,
                }),
            );
        }
        serde_json::Value::Object(report)
    }

    fn percent(&self, class: &str) -> f64 {
        self.0.get(class).map_or(100.0, |(pixels, with_z)| {
            100.0 * *with_z as f64 / (*pixels).max(1) as f64
        })
    }
}

#[test]
#[ignore = "requires the Japanese disc, its flight save state and a graphics adapter"]
fn effects_in_jp_flight() {
    let data = GameData::load_jp_disc(&disc()).unwrap();
    let (device, queue) = offscreen_device().expect("a graphics adapter");
    let executor = || {
        Executor::new(
            device.clone(),
            queue.clone(),
            Options {
                scale: SCALE,
                dither: false,
            },
            wgpu::TextureFormat::Rgba8Unorm,
        )
        .unwrap()
    };
    let config = |enabled, debug| fx::Config {
        enabled,
        debug,
        x_scale: None,
    };
    let names = ["off", "on", "depth", "coverage"];
    let configs = [
        config(false, DebugView::Off),
        config(true, DebugView::Off),
        config(true, DebugView::Depth),
        config(true, DebugView::Coverage),
    ];
    let mut players: Vec<Player> = configs.iter().map(|&c| player(&data, c)).collect();
    let mut cards: Vec<Executor> = configs.iter().map(|_| executor()).collect();
    let out = out();
    let (mut cockpit, mut external) = (Coverage::default(), Coverage::default());
    let (mut effects_frames, mut identical) = (0, 0);
    for pass in 1..=PASSES {
        let held = if SELECT.contains(&pass) {
            machine_player::buttons::SELECT
        } else {
            0
        };
        for player in &mut players {
            player.set_buttons(held);
        }
        let frames: Vec<Frame> = players.iter_mut().map(step).collect();
        let ram = players[0].machine.ram().to_vec();
        for (player, name) in players.iter().zip(names).skip(1) {
            assert!(player.machine.ram() == ram, "pass {pass}: {name}'s RAM");
        }
        assert!(frames[0].fx.is_none() && frames[0].fx_vertices.is_empty());
        if let Some(fx) = frames[1].fx.as_deref() {
            effects_frames += 1;
            let scene = &fx.scene;
            let ot = fx.markers[0].1 - 4095 * 4;
            assert!(
                scene.ots.contains(&ot),
                "pass {pass}: {ot:08x} {:08x?}",
                scene.ots
            );
            assert_eq!(
                ot, scene.current_ot,
                "pass {pass}: the table built this pass"
            );
            let expected: Vec<(Marker, u32)> = Marker::ALL
                .iter()
                .map(|&marker| (marker, ot + marker.entry() * 4))
                .collect();
            assert_eq!(fx.markers, expected, "pass {pass}");
            for other in &frames[2..] {
                assert_eq!(other.fx.as_deref().unwrap().markers, fx.markers);
            }
        }
        for (card, frame) in cards.iter_mut().zip(&frames) {
            card.execute(frame);
        }
        if let Some(fx) = frames[1].fx.as_deref() {
            let (snapshot, _) = cards[1].read_fx_snapshot().expect("a snapshot");
            if pass < *SELECT.start() {
                cockpit.add(fx, &snapshot);
            } else if pass >= EXTERNAL_FROM {
                external.add(fx, &snapshot);
            }
        }
        let area = display_area(&players[0].host.borrow()).expect("a 15-bit picture");
        let pictures: Vec<(Vec<u32>, usize, usize)> = cards
            .iter_mut()
            .map(|card| card.read_display(area).unwrap())
            .collect();
        assert!(
            pictures[0].0 == pictures[1].0,
            "pass {pass}: effects on (no-op) changed the picture: {} pixels",
            pictures[0]
                .0
                .iter()
                .zip(&pictures[1].0)
                .filter(|(a, b)| a != b)
                .count()
        );
        identical += 1;
        if SAVED.contains(&pass) {
            for ((rgb, width, height), name) in pictures.iter().zip(names) {
                save_png(
                    &out.join(format!("flight-{pass:03}-{name}.png")),
                    rgb,
                    *width,
                    *height,
                );
            }
        }
    }
    let stats = players[1]
        .host
        .borrow()
        .hw
        .as_ref()
        .unwrap()
        .effects_stats()
        .cloned()
        .unwrap();
    let card = cards[1].effects_stats().unwrap();
    let report = serde_json::json!({
        "passes": PASSES,
        "effects_frames": effects_frames,
        "identical_pictures": identical,
        "coverage_cockpit": cockpit.report(),
        "coverage_external": external.report(),
        "recorder": stats,
        "card": card,
    });
    fs::write(
        out.join("coverage.json"),
        serde_json::to_string_pretty(&report).unwrap(),
    )
    .unwrap();
    eprintln!("{}", serde_json::to_string_pretty(&report).unwrap());
    assert!(
        effects_frames >= PASSES - 2,
        "{effects_frames} effects frames"
    );
    assert!(stats.deferred > 0 && stats.replayed == stats.deferred);
    assert_eq!(card.posts, effects_frames);
    assert!(cockpit.percent("terrain") > 50.0, "{cockpit:?}");
    assert!(external.percent("terrain") > 50.0, "{external:?}");
}
