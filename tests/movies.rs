//! Movie export and movie packs on the Japanese release (`docs/MOVIES.md`).
//!
//! - [`intro_export_and_packs`] exports the intro movie (`movie_09e6`), then
//!   runs the cold boot through it headless three times, every picture
//!   saved: without a pack, with the export as its own pack (identity), and
//!   with a marked pack (each image carries its frame index in its first
//!   row). It checks that the identity pack changes no picture, the audio
//!   or the game; that every movie picture of the marked run shows the
//!   image of exactly the frame the game shows (first and last included);
//!   and that START ends the replacement with the movie.
//! - [`hub_clips_from_the_pack`] plays the archive's news replay (the hub
//!   clip `movie_0816` in the picture window) with the GPU renderer at 4x,
//!   in lockstep without a pack and with a marked pack (each image is the
//!   game's frame 4x nearest-neighbour, with its clip and index in a band
//!   of cells). Every pass: RAM is the same; the pictures are identical
//!   except in the band, whose cells read the clip and index of an image
//!   the renderer drew in that pass or the one before; the indices follow
//!   each other one by one.
//! - [`pack_decode_throughput`] measures image decoding for packs in
//!   `$AC3_MOVIE_BENCH` (one folder per format and size).
//!
//! Needs the Japanese disc 1 (`$AC3_JP_DISC`, default
//! `~/ac3-jp/discs/... (Disc 1) (Rev 1).cue`, disc 2 next to it). Output in
//! `$AC3_MOVIES_OUT` (default `target/movies`). The hub test also needs the
//! pad schedule (`$AC3_JP_PAD`, default
//! `~/ac3-jp/runs/pad-boot-story-flight.txt`), a save state before the
//! replay (`$AC3_JP_ARCHIVE_STATE`, default
//! `~/ac3-jp/runs/hubclips/states/archive38600.ac3state`; made from
//! `~/ac3-jp/runs/upscale/states/hub28100.ac3state` when missing) and a
//! graphics adapter.
use ac3_graphics::Vram;
use ac3_port::assets::GameData;
use ac3_port::gpu_renderer::execute::offscreen_device;
use ac3_port::gpu_renderer::record::{VERTEX_WORDS, mode};
use ac3_port::gpu_renderer::{DisplayArea, Executor, Frame, Options, replace};
use ac3_port::machine_card::{CardDevice, CardSource};
use ac3_port::machine_player::{self, Host, PadType, Player, RunOptions};
use ac3_port::movie_export::{self, ExportOptions, encode_png};
use ac3_port::movie_pack::{self, Loader, MoviePack, MovieReplacer};
use ac3_port::savestate::{self, GameId};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
use std::fs::{self, File};
use std::path::{Path, PathBuf};

fn home(path: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").expect("HOME")).join(path)
}

fn disc() -> PathBuf {
    std::env::var_os("AC3_JP_DISC").map_or_else(
        || home("ac3-jp/discs/Ace Combat 3 - Electrosphere (Japan, Asia) (Disc 1) (Rev 1).cue"),
        PathBuf::from,
    )
}

fn out() -> PathBuf {
    let out = std::env::var_os("AC3_MOVIES_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/movies"));
    fs::create_dir_all(&out).unwrap();
    out
}

/// The intro movie plays from the cold boot until about VBlank 7100.
const VBLANKS: u64 = 7200;
const INTRO: &str = "movie_09e6";
/// The movie frame's rows on the 320x240 screen.
const TOP: usize = 32;

/// A headless run of the cold boot, every pass's picture saved in `output`.
fn run(data: &GameData, output: &Path, pack: Option<&Path>, pad: &str, vblanks: u64) -> Value {
    let _ = fs::remove_dir_all(output);
    machine_player::run(
        data,
        RunOptions {
            disc: &disc(),
            disc2: None,
            headless: true,
            vblanks: Some(vblanks),
            pad: machine_player::parse_pad_schedule(pad).unwrap(),
            pad_type: Some(PadType::Digital),
            snapshot_every: 1,
            output: Some(output),
            cards: [CardSource::Blank, CardSource::Blank],
            display: Default::default(),
            effects: Default::default(),
            controls: Default::default(),
            quick_save: output.with_extension("ac3state"),
            save_at: None,
            load_state: None,
            dump_textures: None,
            movie_pack: pack.map(Path::to_owned),
        },
    )
    .unwrap()
}

/// A PNG as 0RGB words and its size.
fn read_png(path: &Path) -> (Vec<u32>, usize, usize) {
    let bytes = fs::read(path).unwrap();
    let image = movie_pack::decode_image(&bytes, "png").unwrap();
    let rgb = image
        .pixels
        .chunks_exact(4)
        .map(|p| (u32::from(p[0]) << 16) | (u32::from(p[1]) << 8) | u32::from(p[2]))
        .collect();
    (rgb, image.width as usize, image.height as usize)
}

/// The run's pictures by name (`vblank-NNNNNN.png`).
fn pictures(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.starts_with("vblank-") && n.ends_with(".png"))
        .collect();
    names.sort();
    names
}

/// Bit 15 of the marker is always set.
const MARK: u32 = 1 << 15;

/// The marked pack: frame `n` with its first 16 pixels of row 0 holding
/// `n | MARK` (white 1, black 0).
fn marked_pack(export: &Path, pack: &Path) {
    let folder = pack.join(INTRO);
    let _ = fs::remove_dir_all(pack);
    fs::create_dir_all(&folder).unwrap();
    for entry in fs::read_dir(export).unwrap() {
        let path = entry.unwrap().path();
        let Some(index) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        let (mut rgb, w, h) = read_png(&path);
        for (bit, pixel) in rgb.iter_mut().enumerate().take(16) {
            *pixel = if (index | MARK) >> bit & 1 != 0 {
                0x00ff_ffff
            } else {
                0
            };
        }
        let png = encode_png(&rgb, w as u32, h as u32).unwrap();
        fs::write(folder.join(path.file_name().unwrap()), png).unwrap();
    }
}

/// The marker of a 320x240 picture's movie frame, if it shows one.
fn marker(rgb: &[u32]) -> Option<u32> {
    let row = &rgb[TOP * 320..TOP * 320 + 16];
    let bit = |b: usize| row[b] & 0xff > 127;
    bit(15).then(|| (0..15).filter(|&b| bit(b)).map(|b| 1 << b).sum())
}

#[test]
#[ignore = "requires the Japanese disc images"]
fn intro_export_and_packs() {
    let out = out();
    let data = GameData::load_jp_disc(&disc()).unwrap();
    let export = out.join("export");
    let _ = fs::remove_dir_all(&export);
    let manifest = movie_export::run(&ExportOptions {
        discs: vec![disc()],
        output: &export,
        only: vec![INTRO.to_owned()],
        depth: None,
        manifest_only: false,
        threads: 4,
    })
    .unwrap();
    let movie = &manifest["movies"][0];
    assert_eq!(movie["key"], INTRO);
    assert_eq!(movie["frames"], 1589);
    assert_eq!(movie["first_number"], 1588);
    assert_eq!(movie["frame_size"], serde_json::json!([320, 176]));
    assert_eq!(movie["depth"], 24);
    assert_eq!(movie["fps"], 15.0);
    let frames = export.join(INTRO);
    // The frames by content (the frame's rows without row 0, where the
    // marker goes), for what a picture shows.
    let mut by_content: HashMap<Vec<u32>, Vec<u32>> = HashMap::new();
    for index in 0..1589u32 {
        let (rgb, w, h) = read_png(&frames.join(format!("{index:06}.png")));
        assert_eq!((w, h), (320, 176));
        by_content
            .entry(rgb[320..].to_vec())
            .or_default()
            .push(index);
    }

    // No pack, the identity pack, the marked pack.
    let plain = out.join("run-plain");
    let plain_report = run(&data, &plain, None, "", VBLANKS);
    let identity = out.join("run-identity");
    let identity_report = run(&data, &identity, Some(&export), "", VBLANKS);
    let marked_dir = out.join("pack-marked");
    marked_pack(&frames, &marked_dir);
    let marked = out.join("run-marked");
    let marked_report = run(&data, &marked, Some(&marked_dir), "", VBLANKS);

    // The game is the same (host calls, audio, cards, the final state).
    for report in [&identity_report, &marked_report] {
        for key in [
            "vblanks",
            "iterations",
            "state",
            "substate",
            "host_calls",
            "audio",
        ] {
            assert_eq!(report[key], plain_report[key], "{key}");
        }
    }
    assert_eq!(
        fs::read(plain.join("audio.wav")).unwrap(),
        fs::read(identity.join("audio.wav")).unwrap()
    );
    let replacer = &identity_report["movie_pack"]["replacer"];
    assert_eq!(replacer["frames_replaced"], 1588, "{replacer}");
    assert_eq!(replacer["missing"], 0);
    assert_eq!(replacer["late"], 0);

    let names = pictures(&plain);
    assert_eq!(names, pictures(&identity));
    let (mut movie_pictures, mut marked_pictures) = (0, 0);
    let mut shown = Vec::new();
    for name in &names {
        let (game, w, h) = read_png(&plain.join(name));
        // The identity pack: the same picture.
        assert_eq!(read_png(&identity.join(name)).0, game, "{name}: identity");
        if (w, h) != (320, 240) {
            continue;
        }
        let content = &game[(TOP + 1) * 320..(TOP + 176) * 320];
        let Some(indices) = by_content.get(content) else {
            continue;
        };
        movie_pictures += 1;
        let (ours, ..) = read_png(&marked.join(name));
        let Some(index) = marker(&ours) else {
            // Before and after the movie: black pictures equal its black
            // frames, and no pack image is shown.
            assert!(content.iter().all(|&p| p == 0), "{name}: not replaced");
            continue;
        };
        marked_pictures += 1;
        assert!(
            indices.contains(&index),
            "{name}: image {index}, frame {indices:?}"
        );
        assert_eq!(&ours[(TOP + 1) * 320..(TOP + 176) * 320], content, "{name}");
        // Outside the frame: the game's picture.
        assert_eq!(&ours[..TOP * 320], &game[..TOP * 320], "{name}");
        assert_eq!(
            &ours[(TOP + 176) * 320..],
            &game[(TOP + 176) * 320..],
            "{name}"
        );
        if shown.last() != Some(&index) {
            shown.push(index);
        }
    }
    // Every frame the game shows (all but number 0, which ends the movie),
    // in order, each with its own image.
    assert_eq!(
        shown,
        (0..1588).collect::<Vec<u32>>(),
        "{movie_pictures} movie pictures"
    );
    assert!(marked_pictures >= 1588);

    // START ends the movie: the pack's images stop with it.
    let skipped = out.join("run-start");
    let report = run(&data, &skipped, Some(&marked_dir), "3000-3006:start", 3200);
    let replaced = report["movie_pack"]["replacer"]["frames_replaced"]
        .as_u64()
        .unwrap();
    assert!((400..700).contains(&replaced), "{replaced}");
    for name in pictures(&skipped) {
        let vblank: u64 = name[7..13].parse().unwrap();
        let (rgb, w, h) = read_png(&skipped.join(&name));
        let mark = ((w, h) == (320, 240)).then(|| marker(&rgb)).flatten();
        if vblank > 3010 {
            assert_eq!(mark, None, "{name}: after the skip");
        }
    }
    eprintln!(
        "intro: {marked_pictures} pictures replaced and in step, frames {}..{}",
        shown[0],
        shown[shown.len() - 1]
    );
}

// ---- the hub's clips ---------------------------------------------------------------------

fn env_path(name: &str, default: &str) -> PathBuf {
    std::env::var_os(name).map_or_else(|| home(default), PathBuf::from)
}

/// The clip of the archive's news replay, and where the test plays it.
const HUB_CLIP: &str = "movie_0816";
const HUB_START: u64 = 38600;
const HUB_END: u64 = 41300;
const HUB_SCALE: u32 = 4;
/// The marker band: texel rows 44..52 of the 128x96 frame, 32 cells of 4
/// texels (bit 0 first); a cell is red for 1 and blue for 0.
const BAND: std::ops::Range<usize> = 44..52;
const CELLS: usize = 32;

/// The marker of frame `index` of clip `resource`: the index in bits
/// 0..12, the resource's low 5 bits in 12..17, bit 31 set.
fn hub_marker(resource: u16, index: u32) -> u32 {
    index | (u32::from(resource & 0x1f) << 12) | (1 << 31)
}

/// The cell colour of `bit`, as 0RGB of 5-bit channels expanded; another
/// shade where the frame's own texel has that colour, so every band pixel
/// differs from the game's.
fn cell_colour(bit: bool, texel: u32) -> u32 {
    let expand = |v: u32| (v << 3) | (v >> 2);
    let colour = |v: u32| if bit { expand(v) << 16 } else { expand(v) };
    if colour(31) == texel {
        colour(20)
    } else {
        colour(31)
    }
}

/// The marked pack of the exported frames in `export` (128x96, 15-bit
/// colours): each frame 4x nearest-neighbour, with its marker in the band.
/// Returns the frame count.
fn marked_hub_pack(export: &Path, pack: &Path, resource: u16) -> u32 {
    let folder = pack.join(HUB_CLIP);
    let _ = fs::remove_dir_all(pack);
    fs::create_dir_all(&folder).unwrap();
    let mut frames: Vec<(u32, PathBuf)> = fs::read_dir(export)
        .unwrap()
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            let index = path.file_stem()?.to_str()?.parse().ok()?;
            Some((index, path))
        })
        .collect();
    frames.sort();
    std::thread::scope(|scope| {
        for chunk in frames.chunks(frames.len().div_ceil(8).max(1)) {
            let folder = &folder;
            scope.spawn(move || {
                for (index, path) in chunk {
                    let (rgb, w, h) = read_png(path);
                    assert_eq!((w, h), (128, 96));
                    let marker = hub_marker(resource, *index);
                    let texel = |x: usize, y: usize| {
                        let t = rgb[y * 128 + x];
                        if BAND.contains(&y) {
                            cell_colour(marker >> (x / 4) & 1 != 0, t)
                        } else {
                            t
                        }
                    };
                    let big: Vec<u32> = (0..96 * 4)
                        .flat_map(|y| (0..128 * 4).map(move |x| texel(x / 4, y / 4)))
                        .collect();
                    let png = encode_png(&big, 512, 384).unwrap();
                    fs::write(folder.join(path.file_name().unwrap()), png).unwrap();
                }
            });
        }
    });
    frames.len() as u32
}

/// A JP player at save state `state` with the scripted pad, as
/// `machine_player::run` builds one: the GPU renderer at `scale` (none:
/// software only) with the movie pack `pack` (headless: it waits for
/// every image).
fn hub_player(data: &GameData, state: &Path, scale: Option<u32>, pack: Option<&Path>) -> Player {
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
    host.movies = pack.map(|pack| Box::new(MovieReplacer::open(pack, images.clone(), true)));
    if let Some(scale) = scale {
        host.start_gpu_renderer(Options {
            scale,
            dither: false,
        });
    }
    let mut player = Player::new(ac3_flight::Machine::default(), host);
    game.boot(&mut player.machine, &data.executable.payload);
    player.machine.take_fault().unwrap();
    savestate::load(&mut player, GameId::of(&data.executable.payload), state).unwrap();
    player.host.borrow_mut().reseed_gpu_renderer();
    player
}

/// One main-loop pass; the GPU renderer's frame, if it runs.
fn hub_step(player: &mut Player) -> Option<Frame> {
    if let Err(fault) = player.iteration() {
        panic!("VBlank {}: {fault}", player.vblanks());
    }
    player.host.borrow_mut().take_audio();
    if let Some(error) = player.host.borrow().error() {
        panic!("VBlank {}: device: {error}", player.vblanks());
    }
    player.host.borrow_mut().take_gpu_frame()
}

/// The save state before the replay: `$AC3_JP_ARCHIVE_STATE`, or one made
/// from the hub state at VBlank 28100 (software only) in the output folder.
fn hub_state(data: &GameData) -> PathBuf {
    let state = env_path(
        "AC3_JP_ARCHIVE_STATE",
        "ac3-jp/runs/hubclips/states/archive38600.ac3state",
    );
    if state.exists() {
        return state;
    }
    let made = out().join("archive38600.ac3state");
    if !made.exists() {
        let hub = env_path(
            "AC3_JP_HUB28100_STATE",
            "ac3-jp/runs/upscale/states/hub28100.ac3state",
        );
        let mut player = hub_player(data, &hub, None, None);
        while player.vblanks() < HUB_START {
            hub_step(&mut player);
        }
        savestate::save(&player, GameId::of(&data.executable.payload), &made).unwrap();
    }
    made
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

/// The movie images (`movie_0816 image 12`) `frame` draws with, following
/// the movie slots' images in `slots`.
fn drawn_images(frame: &Frame, slots: &mut HashMap<u32, String>) -> BTreeSet<String> {
    for image in &frame.replacements {
        slots.insert(image.slot, image.key.clone());
    }
    frame
        .vertices
        .chunks_exact(VERTEX_WORDS)
        .filter(|v| v[5] & mode::TEXTURED != 0 && v[5] >> mode::SLOT_SHIFT != 0)
        .map(|v| (v[5] >> mode::SLOT_SHIFT) - 1)
        .filter(|&slot| replace::is_movie_slot(slot))
        .map(|slot| slots[&slot].clone())
        .collect()
}

/// The marker in `packed` where it differs from `plain` (`width` wide):
/// `None` when they are the same; an error unless the differing pixels
/// fill exactly one rectangle.
fn read_band(plain: &[u32], packed: &[u32], width: usize) -> Result<Option<u32>, String> {
    let differing: Vec<(usize, usize)> = plain
        .iter()
        .zip(packed)
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .map(|(at, _)| (at % width, at / width))
        .collect();
    if differing.is_empty() {
        return Ok(None);
    }
    let x0 = differing.iter().map(|p| p.0).min().unwrap();
    let x1 = differing.iter().map(|p| p.0).max().unwrap() + 1;
    let y0 = differing.iter().map(|p| p.1).min().unwrap();
    let y1 = differing.iter().map(|p| p.1).max().unwrap() + 1;
    if differing.len() != (x1 - x0) * (y1 - y0) {
        return Err(format!(
            "{} pixels differ, in {}x{} at ({x0}, {y0})",
            differing.len(),
            x1 - x0,
            y1 - y0
        ));
    }
    let y = (y0 + y1) / 2;
    let value = (0..CELLS)
        .map(|cell| {
            let x = x0 + (2 * cell + 1) * (x1 - x0) / (2 * CELLS);
            let p = packed[y * width + x];
            u32::from((p >> 16) & 0xff > p & 0xff) << cell
        })
        .sum();
    Ok(Some(value))
}

#[test]
#[ignore = "requires the Japanese discs, the pad schedule, a save state and a graphics adapter"]
fn hub_clips_from_the_pack() {
    let out = out();
    let data = GameData::load_jp_disc(&disc()).unwrap();
    // The clip at the game's depth (15-bit), and the marked pack.
    let export = out.join("hub-export");
    let _ = fs::remove_dir_all(&export);
    let manifest = movie_export::run(&ExportOptions {
        discs: vec![disc()],
        output: &export,
        only: vec![HUB_CLIP.to_owned()],
        depth: None,
        manifest_only: false,
        threads: 8,
    })
    .unwrap();
    let movie = &manifest["movies"][0];
    assert_eq!(movie["key"], HUB_CLIP);
    assert_eq!(movie["frame_size"], serde_json::json!([128, 96]));
    assert_eq!(movie["depth"], 15);
    let resource = u16::from_str_radix(&HUB_CLIP[6..], 16).unwrap();
    let pack = out.join("hub-pack");
    let frames = marked_hub_pack(&export.join(HUB_CLIP), &pack, resource);
    assert_eq!(u64::from(frames), movie["frames"].as_u64().unwrap());

    // Without a pack and with the marked one, in lockstep.
    let state = hub_state(&data);
    let (device, queue) = offscreen_device().expect("a graphics adapter");
    let mut cards: Vec<Executor> = (0..2)
        .map(|_| {
            Executor::new(
                device.clone(),
                queue.clone(),
                Options {
                    scale: HUB_SCALE,
                    dither: false,
                },
                wgpu::TextureFormat::Rgba8Unorm,
            )
            .unwrap()
        })
        .collect();
    let mut players = [
        hub_player(&data, &state, Some(HUB_SCALE), None),
        hub_player(&data, &state, Some(HUB_SCALE), Some(&pack)),
    ];
    let mut slots = HashMap::new();
    let mut drawn_before = BTreeSet::new();
    let (mut passes, mut marked, mut drawn_passes) = (0u64, 0u64, 0u64);
    let mut shown: Vec<u32> = Vec::new();
    while players[0].vblanks() < HUB_END {
        let frames: Vec<Frame> = players.iter_mut().map(|p| hub_step(p).unwrap()).collect();
        let vblank = players[0].vblanks();
        assert!(
            players[0].machine.ram() == players[1].machine.ram(),
            "VBlank {vblank}: the pack changed RAM"
        );
        assert!(drawn_images(&frames[0], &mut HashMap::new()).is_empty());
        let drawn = drawn_images(&frames[1], &mut slots);
        assert!(drawn.len() <= 1, "VBlank {vblank}: {drawn:?}");
        drawn_passes += u64::from(!drawn.is_empty());
        for (card, frame) in cards.iter_mut().zip(&frames) {
            card.execute(frame);
        }
        let Some(area) = display_area(&players[0].host.borrow()) else {
            drawn_before = drawn;
            continue;
        };
        let plain = cards[0].read_display(area).unwrap();
        let packed = cards[1].read_display(area).unwrap();
        passes += 1;
        let value = match read_band(&plain.0, &packed.0, plain.1) {
            Ok(value) => value,
            Err(error) => {
                for (name, (rgb, w, h)) in [("plain", &plain), ("pack", &packed)] {
                    let png = encode_png(rgb, *w as u32, *h as u32).unwrap();
                    fs::write(out.join(format!("hub-{vblank}-{name}.png")), png).unwrap();
                }
                panic!("VBlank {vblank}: {error} (pictures in {})", out.display());
            }
        };
        if let Some(value) = value {
            let index = value & 0xfff;
            assert_eq!(
                value,
                hub_marker(resource, index),
                "VBlank {vblank}: marker {value:#x}"
            );
            let name = format!("{HUB_CLIP} image {index}");
            assert!(
                drawn.contains(&name) || drawn_before.contains(&name),
                "VBlank {vblank}: the picture shows {name}, the renderer drew {drawn:?} after \
                 {drawn_before:?}"
            );
            marked += 1;
            if shown.last() != Some(&index) {
                shown.push(index);
            }
        }
        drawn_before = drawn;
    }
    // Every frame the game showed, one after another, from the pack.
    assert!(shown.len() > 200, "{} frames shown", shown.len());
    for pair in shown.windows(2) {
        assert_eq!(pair[1], pair[0] + 1, "{shown:?}");
    }
    assert!(*shown.last().unwrap() < frames);
    let host = players[1].host.borrow();
    let movies = host.movies.as_deref().unwrap();
    let stats = host.hw.as_deref().unwrap().replacer().unwrap().movie_stats;
    assert_eq!((stats.late, stats.no_slot), (0, 0), "{stats:?}");
    assert_eq!(movies.stats.hub_missing, 0);
    assert_eq!(movies.stats.hub_frames, stats.frames);
    assert!(stats.images >= shown.len() as u64, "{stats:?}");
    eprintln!(
        "hub clips: {passes} pictures compared, {marked} with the pack's image (frames {}..{} of \
         {HUB_CLIP}, {} drawn passes); {stats:?}",
        shown[0],
        shown[shown.len() - 1],
        drawn_passes
    );
}

/// Decoding speed of the images in each folder of `$AC3_MOVIE_BENCH`: one
/// thread, then the pack loader's threads.
#[test]
#[ignore = "needs $AC3_MOVIE_BENCH (folders of frame images)"]
fn pack_decode_throughput() {
    let Some(root) = std::env::var_os("AC3_MOVIE_BENCH").map(PathBuf::from) else {
        eprintln!("skipped: no AC3_MOVIE_BENCH");
        return;
    };
    let mut folders: Vec<PathBuf> = fs::read_dir(&root)
        .unwrap()
        .filter_map(|e| Some(e.ok()?.path()))
        .filter(|p| p.is_dir())
        .collect();
    folders.sort();
    let threads = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .min(8);
    for folder in folders {
        // Read as a one-movie pack.
        let pack = MoviePack::open(&folder).unwrap();
        let Some((key, movie)) = pack.movies.iter().next() else {
            continue;
        };
        let files: Vec<&movie_pack::Source> = movie.files.values().take(96).collect();
        let bytes: u64 = files.iter().map(|f| f.bytes()).sum();
        let started = std::time::Instant::now();
        let mut size = (0, 0);
        for file in files.iter().take(24) {
            let image = file.load().unwrap();
            size = (image.width, image.height);
        }
        let single = started.elapsed().as_secs_f64() / files.len().min(24) as f64;
        let loader = Loader::new(threads, 1 << 34);
        let key: std::sync::Arc<str> = key.as_str().into();
        let started = std::time::Instant::now();
        for (n, file) in files.iter().enumerate() {
            loader.request_source(key.clone(), n as u32, file);
        }
        for n in 0..files.len() {
            loader.wait_for(&key, n as u32).unwrap().unwrap();
        }
        let parallel = files.len() as f64 / started.elapsed().as_secs_f64();
        eprintln!(
            "{}: {} images {}x{}, {:.2} MB each; one thread {:.1} ms an image; {threads} threads \
             {:.1} images/s",
            folder.file_name().unwrap().to_string_lossy(),
            files.len(),
            size.0,
            size.1,
            bytes as f64 / files.len() as f64 / 1e6,
            single * 1000.0,
            parallel
        );
    }
}
