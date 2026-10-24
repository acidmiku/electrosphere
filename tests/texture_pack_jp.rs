//! Texture packs on the Japanese release (`docs/UI_UPSCALE.md`, "Round
//! trip"): the textures the game draws, dumped and used as their own
//! replacements, give the GPU renderer's picture unchanged, pixel for pixel.
//!
//! For three stretches of the scripted run (the pad schedule from boot):
//! - the cold boot through the title and the account menus (480i);
//! - "CONNECTION LOST" after the failed mission back into the hub, which
//!   loads its textures again (`$AC3_JP_HUB_STATE`);
//! - weapon select, the mission load and the first flight seconds
//!   (`$AC3_JP_FLIGHT_LOAD_STATE`),
//!
//! a run with the runtime dump writes the textures, then three identity
//! packs are made from them:
//! - every dumped PNG upscaled 2x nearest-neighbour under its exact key
//!   (indexed PNGs drawn in their own palettes), played at 2x;
//! - the same upscaled 4x, played at 4x;
//! - each 4/8-bit texture once as an indexed `-any-` PNG upscaled 2x (drawn
//!   through the live palette), with the 15-bit ones, played at 4x.
//!
//! Each pack runs in lockstep with a run without one: every pass, machine
//! RAM is the same (the game never sees a pack) and the displayed pictures
//! are byte-identical, while the pack replaces most textured primitives.
//!
//! Needs the Japanese disc 1 (`$AC3_JP_DISC`), the pad schedule
//! (`$AC3_JP_PAD`, default `~/ac3-jp/runs/pad-boot-story-flight.txt`), the
//! two save states (defaults in `~/ac3-jp/runs/upscale/states/`) and a
//! graphics adapter. Pictures of the first difference, if any, go to
//! `$AC3_TEXPACK_OUT` (default `target/texture-pack-jp`).
use ac3_graphics::Vram;
use ac3_port::asset_dump::key::{Key, PaletteKey};
use ac3_port::asset_dump::png_io::{self, Decoded, Pixels};
use ac3_port::assets::GameData;
use ac3_port::gpu_renderer::execute::offscreen_device;
use ac3_port::gpu_renderer::{DisplayArea, Executor, Frame, Options};
use ac3_port::machine_card::{CardDevice, CardSource};
use ac3_port::machine_player::{self, Host, PadType, Player};
use ac3_port::savestate::{self, GameId};
use std::fs::{self, File};
use std::path::{Path, PathBuf};

fn home(path: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").expect("HOME")).join(path)
}

fn env_path(name: &str, default: &str) -> PathBuf {
    std::env::var_os(name).map_or_else(|| home(default), PathBuf::from)
}

fn disc() -> PathBuf {
    env_path(
        "AC3_JP_DISC",
        "ac3-jp/discs/Ace Combat 3 - Electrosphere (Japan, Asia) (Disc 1) (Rev 1).cue",
    )
}

fn out() -> PathBuf {
    let out = std::env::var_os("AC3_TEXPACK_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/texture-pack-jp"));
    fs::create_dir_all(&out).unwrap();
    out
}

/// A stretch of the scripted run: from the cold boot or a save state, to
/// a VBlank.
struct Stretch {
    name: &'static str,
    state: Option<PathBuf>,
    end: u64,
}

fn stretches() -> Vec<Stretch> {
    vec![
        Stretch {
            name: "menus",
            state: None,
            end: 1500,
        },
        Stretch {
            name: "hub",
            state: Some(env_path(
                "AC3_JP_HUB_STATE",
                "ac3-jp/runs/upscale/states/hub51800.ac3state",
            )),
            end: 52900,
        },
        Stretch {
            name: "flight",
            state: Some(env_path(
                "AC3_JP_FLIGHT_LOAD_STATE",
                "ac3-jp/runs/upscale/states/flight48260.ac3state",
            )),
            end: 49300,
        },
    ]
}

/// A JP player at the stretch's start, as `machine_player::run` builds
/// one: the GPU renderer at `scale` (none: software only) with the texture
/// pack `pack`, or the runtime dump when `dump`.
fn player(
    data: &GameData,
    stretch: &Stretch,
    scale: Option<u32>,
    pack: Option<&Path>,
    dump: bool,
) -> Player {
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
    let schedule = machine_player::parse_pad_schedule(
        &fs::read_to_string(env_path(
            "AC3_JP_PAD",
            "ac3-jp/runs/pad-boot-story-flight.txt",
        ))
        .unwrap(),
    )
    .unwrap();
    host.pad_input = Some(Box::new(move |vblank| {
        schedule
            .iter()
            .filter(|(first, last, _)| (*first..=*last).contains(&vblank))
            .fold(0, |held, (_, _, buttons)| held | buttons)
    }));
    host.texture_pack = pack.map(Path::to_owned);
    if let Some(scale) = scale {
        host.start_gpu_renderer(Options {
            scale,
            dither: false,
        });
    }
    if dump {
        host.gpu.texture_dump = Some(Box::default());
    }
    let mut player = Player::new(ac3_flight::Machine::default(), host);
    game.boot(&mut player.machine, &data.executable.payload);
    player.machine.take_fault().unwrap();
    if let Some(state) = &stretch.state {
        savestate::load(&mut player, GameId::of(&data.executable.payload), state).unwrap();
        player.host.borrow_mut().reseed_gpu_renderer();
    }
    player
}

/// One main-loop pass, as `machine_player`'s session runs it (the dump's
/// time stamp first); the GPU renderer's frame, if it runs.
fn step(player: &mut Player) -> Option<Frame> {
    {
        let vblank = player.vblanks();
        let state = player.game.states(&player.machine).state;
        if let Some(recorder) = player.host.borrow_mut().gpu.texture_dump.as_mut() {
            recorder.set_frame(vblank, state);
        }
    }
    if let Err(fault) = player.iteration() {
        panic!("VBlank {}: {fault}", player.vblanks());
    }
    player.host.borrow_mut().take_audio();
    if let Some(error) = player.host.borrow().error() {
        panic!("VBlank {}: device: {error}", player.vblanks());
    }
    player.host.borrow_mut().take_gpu_frame()
}

/// The displayed area (the display now; both players agree on it).
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

/// The dump of the stretch: run it with the runtime dump into `dir`;
/// returns the number of textures.
fn dump(data: &GameData, stretch: &Stretch, dir: &Path) -> u64 {
    let mut player = player(data, stretch, None, None, true);
    while player.vblanks() < stretch.end {
        step(&mut player);
    }
    let host = player.host.borrow();
    let summary = host.gpu.texture_dump.as_ref().unwrap().write(dir).unwrap();
    summary["textures"].as_u64().unwrap()
}

/// Upscale a dump PNG `factor` times, nearest neighbour, keeping its kind.
fn upscale(png: &[u8], factor: usize) -> Vec<u8> {
    let scale = |pixels: &[u16], width: usize, height: usize| -> Vec<u16> {
        (0..height * factor)
            .flat_map(|y| (0..width * factor).map(move |x| (x / factor, y / factor)))
            .map(|(x, y)| pixels[y * width + x])
            .collect()
    };
    match png_io::decode(png).unwrap() {
        Decoded::Indexed {
            bits,
            width,
            height,
            indices,
            palette,
        } => {
            let wide: Vec<u16> = indices.iter().map(|&i| u16::from(i)).collect();
            let indices: Vec<u8> = scale(&wide, width, height)
                .into_iter()
                .map(|i| i as u8)
                .collect();
            png_io::encode(
                width * factor,
                height * factor,
                Pixels::Indexed {
                    bits,
                    indices: &indices,
                    palette: &palette,
                },
            )
            .unwrap()
        }
        Decoded::Color15 {
            width,
            height,
            colors,
        } => png_io::encode(
            width * factor,
            height * factor,
            Pixels::Color15(&scale(&colors, width, height)),
        )
        .unwrap(),
        other => panic!("unexpected dump PNG {other:?}"),
    }
}

/// An identity pack from the dump in `dump_dir`: every PNG upscaled
/// `factor` times under its key, or (`any`) one indexed `-any-` PNG per
/// 4/8-bit upload and mode, with the 15-bit ones. Returns the file count.
fn identity_pack(dump_dir: &Path, pack: &Path, factor: usize, any: bool) -> usize {
    let manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dump_dir.join("manifest.json")).unwrap()).unwrap();
    let _ = fs::remove_dir_all(pack);
    fs::create_dir_all(pack).unwrap();
    let mut written = std::collections::BTreeSet::new();
    for texture in manifest["textures"].as_array().unwrap() {
        let file = texture["file"].as_str().unwrap();
        let mut key = Key::from_stem(texture["key"].as_str().unwrap()).unwrap();
        if any && key.mode.indexed() {
            key.palette = PaletteKey::Any;
        }
        let name = format!("{key}.png");
        if written.insert(name.clone()) {
            let png = fs::read(dump_dir.join(file)).unwrap();
            fs::write(pack.join(name), upscale(&png, factor)).unwrap();
        }
    }
    written.len()
}

/// A pack against no pack at `scale`, in lockstep over the stretch:
/// returns (passes compared, primitives replaced, textured primitives
/// drawn as the original).
fn compare(data: &GameData, stretch: &Stretch, scale: u32, pack: &Path, label: &str) -> [u64; 3] {
    let (device, queue) = offscreen_device().expect("a graphics adapter");
    let mut cards: Vec<Executor> = (0..2)
        .map(|_| {
            Executor::new(
                device.clone(),
                queue.clone(),
                Options {
                    scale,
                    dither: false,
                },
                wgpu::TextureFormat::Rgba8Unorm,
            )
            .unwrap()
        })
        .collect();
    let mut players = [
        player(data, stretch, Some(scale), None, false),
        player(data, stretch, Some(scale), Some(pack), false),
    ];
    let mut passes = 0;
    while players[0].vblanks() < stretch.end {
        let frames: Vec<Frame> = players.iter_mut().map(|p| step(p).unwrap()).collect();
        assert!(
            players[0].machine.ram() == players[1].machine.ram(),
            "{label}: VBlank {}: the pack changed RAM",
            players[0].vblanks()
        );
        for (card, frame) in cards.iter_mut().zip(&frames) {
            card.execute(frame);
        }
        let Some(area) = display_area(&players[0].host.borrow()) else {
            continue;
        };
        let plain = cards[0].read_display(area).unwrap();
        let packed = cards[1].read_display(area).unwrap();
        if plain.0 != packed.0 {
            let out = out();
            for (name, (rgb, width, height)) in [("plain", &plain), ("pack", &packed)] {
                let image = image_rgb(rgb, *width, *height);
                fs::write(out.join(format!("{label}-{name}.png")), image).unwrap();
            }
            let differing = plain
                .0
                .iter()
                .zip(&packed.0)
                .filter(|(a, b)| a != b)
                .count();
            panic!(
                "{label}: VBlank {}: {differing} pixels differ (pictures in {})",
                players[0].vblanks(),
                out.display()
            );
        }
        passes += 1;
    }
    let host = players[1].host.borrow();
    let stats = &host.hw.as_ref().unwrap().replacer().unwrap().stats;
    [passes, stats.replaced, stats.originals]
}

/// A 0RGB picture as an RGB PNG.
fn image_rgb(rgb: &[u32], width: usize, height: usize) -> Vec<u8> {
    let mut output = Vec::new();
    let mut encoder = png::Encoder::new(&mut output, width as u32, height as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let bytes: Vec<u8> = rgb
        .iter()
        .flat_map(|&p| [(p >> 16) as u8, (p >> 8) as u8, p as u8])
        .collect();
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(&bytes).unwrap();
    writer.finish().unwrap();
    output
}

#[test]
#[ignore = "requires the Japanese disc, the pad schedule, two save states and a graphics adapter"]
fn identity_packs_leave_jp_pictures_unchanged() {
    let data = GameData::load_jp_disc(&disc()).unwrap();
    let work = std::env::temp_dir().join(format!("ac3-texture-pack-jp-{}", std::process::id()));
    let mut report = serde_json::Map::new();
    for stretch in stretches() {
        let dump_dir = work.join(stretch.name).join("dump");
        let textures = dump(&data, &stretch, &dump_dir);
        assert!(textures > 0, "{}: nothing dumped", stretch.name);
        let mut runs = Vec::new();
        for (scale, factor, any) in [(2, 2, false), (4, 4, false), (4, 2, true)] {
            let label = format!(
                "{}-{scale}x-{}{factor}x",
                stretch.name,
                if any { "any-" } else { "exact-" }
            );
            let pack = work.join(stretch.name).join(&label);
            let files = identity_pack(&dump_dir, &pack, factor, any);
            let [passes, replaced, originals] = compare(&data, &stretch, scale, &pack, &label);
            assert!(passes > 0, "{label}: no picture compared");
            assert!(
                replaced > originals,
                "{label}: only {replaced} primitives replaced, {originals} drawn as the original"
            );
            eprintln!(
                "{label}: {files} PNGs; {passes} pictures identical; {replaced} primitives \
                 replaced, {originals} original"
            );
            runs.push(serde_json::json!({
                "label": label, "files": files, "identical_pictures": passes,
                "replaced": replaced, "originals": originals,
            }));
        }
        report.insert(
            stretch.name.to_owned(),
            serde_json::json!({"textures": textures, "runs": runs}),
        );
    }
    fs::write(
        out().join("report.json"),
        serde_json::to_string_pretty(&report).unwrap() + "\n",
    )
    .unwrap();
    let _ = fs::remove_dir_all(&work);
}
