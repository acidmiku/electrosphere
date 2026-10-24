//! Asset dump (Milestone 1 item 5) against the PAL disc data.
//!
//! Both tests need `AC3_DATA_DIR` (the workspace with `extracted/disc`,
//! `extracted/archives` and the disc image). Output goes to
//! `AC3_ASSET_DUMP_OUT` (default `target/asset-dump`).
//!
//! - `static_dump_counts_round_trip_and_determinism`: image counts by
//!   format, every sampled PNG decodes back to its TIM's indices and CLUT,
//!   every source reference finds the original bytes, and a second dump
//!   writes the same manifest and PNGs.
//! - `runtime_dump_over_the_scripted_path_into_flight`: `ac3 machine
//!   --dump-textures` over the scripted cold boot into flight records
//!   menu, HUD and terrain textures, keyed like the static dump, twice
//!   with the same result.
use ac3_graphics::tim;
use ac3_port::{
    asset_dump::{self, key::Key, png_io},
    assets::GameData,
    machine_card::CardSource,
    machine_player::{self, PadType, RunOptions},
    sha256,
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

fn root() -> PathBuf {
    PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR"))
}

fn out_dir(name: &str) -> PathBuf {
    let base = std::env::var_os("AC3_ASSET_DUMP_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/asset-dump"));
    let dir = base.join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn read_manifest(dir: &Path) -> (String, Value) {
    let text = fs::read_to_string(dir.join("manifest.json")).unwrap();
    let value = serde_json::from_str(&text).unwrap();
    (text, value)
}

/// The cold-boot pad path into flight (PREAMBLE_M1).
const INTO_FLIGHT: &str = "400-406:cross,550-556:start,700-706:cross,850-856:cross,\
1400-1406:cross,1550-1556:cross,1700-1706:cross,1850-1856:cross,2000-2006:cross,\
2150-2156:cross,2300-2306:cross,2450-2456:cross";

/// The original bytes behind a manifest source.
fn original_bytes(root: &Path, source: &Value, length: usize) -> Vec<u8> {
    let offset = source["offset_in_node"].as_u64().unwrap() as usize;
    let file = source["extracted"]
        .as_str()
        .expect("BPB sources name their extracted node");
    let node = fs::read(root.join(file)).unwrap();
    node[offset..offset + length].to_vec()
}

/// Check one manifest image: its first source holds the hashed bytes, and
/// every PNG decodes back to the pixel indices and that palette's entries.
fn check_round_trip(root: &Path, dir: &Path, image: &Value) {
    let length = image["bytes"].as_u64().unwrap() as usize;
    let source = &image["sources"][0];
    let bytes = original_bytes(root, source, length);
    assert_eq!(sha256(&bytes), image["sha256"], "{source}");
    let (depth, pixels, clut) = if source["found_by"] == "credits-table" {
        // Raw CLUT (16 entries) then 4-bit pixels.
        let vram = &image["vram"];
        let block = tim::Block {
            x: vram["x"].as_u64().unwrap() as u16,
            y: vram["y"].as_u64().unwrap() as u16,
            width: vram["w"].as_u64().unwrap() as u16,
            height: vram["h"].as_u64().unwrap() as u16,
            data: &bytes[0x20..],
        };
        let clut = tim::Block {
            x: 0,
            y: 0,
            width: 16,
            height: 1,
            data: &bytes[..0x20],
        };
        (tim::Depth::Indexed4, block, Some(clut))
    } else {
        let loaded = tim::parse_loaded(&bytes).unwrap();
        assert_eq!(loaded.lengths_match, image["stated_lengths_match"]);
        (loaded.image.depth, loaded.image.pixels, loaded.image.clut)
    };
    let pngs = image["pngs"].as_array().unwrap();
    assert!(!pngs.is_empty());
    for png in pngs {
        let file = fs::read(dir.join(png["file"].as_str().unwrap())).unwrap();
        assert_eq!(sha256(&file), png["sha256"]);
        let stem = Path::new(png["file"].as_str().unwrap())
            .file_stem()
            .unwrap()
            .to_str()
            .unwrap();
        let key = Key::from_stem(stem).unwrap();
        assert_eq!(key.to_string(), png["key"]);
        assert_eq!(key.texture, image["texture_hash"]);
        match png_io::decode(&file).unwrap() {
            png_io::Decoded::Indexed {
                bits,
                width,
                height,
                indices,
                palette,
            } => {
                assert_eq!(bits, depth.bits());
                assert_eq!(
                    (width, height),
                    (depth.texel_width(pixels.width), usize::from(pixels.height))
                );
                assert_eq!(
                    tim::pack_indices(&indices, depth).unwrap(),
                    pixels.data,
                    "{}",
                    png["file"]
                );
                let clut = clut.expect("indexed images here have a CLUT");
                let entries = depth.palette_entries();
                let first = png["palette"].as_u64().unwrap() as usize * entries;
                let words: Vec<u16> = clut
                    .data
                    .chunks_exact(2)
                    .map(|b| u16::from_le_bytes([b[0], b[1]]))
                    .collect();
                // The key covers the entries lo..=hi that the texture uses;
                // images sharing a key share one PNG (the first one's other
                // entries).
                let range = &png["index_range"];
                let (lo, hi) = (
                    range[0].as_u64().unwrap() as usize,
                    range[1].as_u64().unwrap() as usize,
                );
                let end = (hi + 1).min(words.len().saturating_sub(first));
                assert!(lo < end);
                assert_eq!(
                    &palette[lo..end],
                    &words[first + lo..first + end],
                    "{}",
                    png["file"]
                );
            }
            png_io::Decoded::Color15 { colors, .. } => {
                let words: Vec<u16> = pixels
                    .data
                    .chunks_exact(2)
                    .map(|b| u16::from_le_bytes([b[0], b[1]]))
                    .collect();
                assert_eq!(colors, words);
            }
            png_io::Decoded::Rgb24 { .. } => {}
        }
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR with the extracted PAL disc and archives"]
fn static_dump_counts_round_trip_and_determinism() {
    let root = root();
    let data = GameData::load(&root.join("extracted/disc")).unwrap();
    let first = out_dir("static-a");
    let started = std::time::Instant::now();
    let summary = asset_dump::dump_archive_images(&data, &first).unwrap();
    eprintln!(
        "static dump: {:.1} s, {}",
        started.elapsed().as_secs_f64(),
        serde_json::to_string_pretty(&summary).unwrap()
    );
    let (text, manifest) = read_manifest(&first);
    let images = manifest["images"].as_array().unwrap();
    assert_eq!(
        images.len() as u64,
        summary["unique_images"].as_u64().unwrap()
    );
    assert_eq!(manifest["errors"].as_array().unwrap().len(), 0);

    // Counts by format and finder (the PAL disc's inventory).
    let mut formats: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for image in images {
        let entry = formats
            .entry(image["format"].as_str().unwrap().to_owned())
            .or_default();
        entry.0 += 1;
        entry.1 += image["occurrences"].as_u64().unwrap();
    }
    eprintln!("formats (unique, occurrences): {formats:?}");
    eprintln!("finders: {}", summary["occurrences_by_finder"]);
    // The verified PAL disc: 4- and 8-bit images only (no 15/24-bit TIMs);
    // 11,371 TIM nodes (as tools/unpack_archives.py), 16 TIMs embedded in
    // other data (13 in bpb_0122, overlay 5, ACE.EXE, SCES_020.66) and
    // overlay 3's five credits images.
    assert_eq!(
        formats,
        BTreeMap::from([
            ("p4".to_owned(), (8739, 11302)),
            ("p8".to_owned(), (82, 90))
        ])
    );
    assert_eq!(
        summary["occurrences_by_finder"],
        serde_json::json!({"container": 11371, "credits-table": 5, "scan": 16})
    );
    assert_eq!(summary["nodes_visited"], 32792);
    assert_eq!(summary["ulz_streams"], 1362);
    assert_eq!(summary["stated_length_mismatches"], 6);
    assert_eq!(summary["pngs"], 6788);

    // Round trips: every multi-palette, 8-bit, short-length, scanned and
    // credits image, plus every 97th other one.
    let mut checked = 0;
    for (index, image) in images.iter().enumerate() {
        let special = image["pngs"].as_array().unwrap().len() > 1
            || image["format"] != "p4"
            || image["stated_lengths_match"] == false
            || image["sources"][0]["found_by"] != "container";
        let from_bpb = image["sources"][0]["extracted"].is_string();
        if from_bpb && (special || index % 97 == 0) {
            check_round_trip(&root, &first, image);
            checked += 1;
        }
    }
    eprintln!("round-tripped {checked} images");
    assert!(checked > 100);

    // Every PNG on disk is listed with its hash; names carry their keys.
    let mut listed = HashSet::new();
    for image in images {
        for png in image["pngs"].as_array().unwrap() {
            listed.insert(png["file"].as_str().unwrap().to_owned());
        }
    }
    assert_eq!(listed.len() as u64, summary["pngs"].as_u64().unwrap());

    // A second run writes the same manifest (and so the same PNG hashes).
    let second = out_dir("static-b");
    asset_dump::dump_archive_images(&data, &second).unwrap();
    let (again, _) = read_manifest(&second);
    assert!(text == again, "static manifests differ between runs");
    for file in listed.iter().take(500) {
        assert_eq!(
            fs::read(first.join(file)).unwrap(),
            fs::read(second.join(file)).unwrap()
        );
    }
}

fn runtime_dump(data: &GameData, dir: &Path) -> Value {
    let root = root();
    machine_player::run(
        data,
        RunOptions {
            disc: &root.join("Ace Combat 3 - Electrosphere (Europe) (En,Fr,De,Es,It).bin"),
            disc2: None,
            headless: true,
            vblanks: Some(2700),
            pad: machine_player::parse_pad_schedule(INTO_FLIGHT).unwrap(),
            pad_type: Some(PadType::Digital),
            snapshot_every: 2700,
            output: None,
            cards: [CardSource::Blank, CardSource::Blank],
            display: Default::default(),
            effects: Default::default(),
            controls: Default::default(),
            quick_save: dir.join("unused.ac3state"),
            save_at: None,
            load_state: None,
            dump_textures: Some(dir),
            movie_pack: None,
        },
    )
    .unwrap()
}

#[test]
#[ignore = "requires AC3_DATA_DIR with the extracted PAL disc and the disc image"]
fn runtime_dump_over_the_scripted_path_into_flight() {
    let root = root();
    let data = GameData::load(&root.join("extracted/disc")).unwrap();
    let first = out_dir("runtime-a");
    let started = std::time::Instant::now();
    let result = runtime_dump(&data, &first);
    eprintln!(
        "runtime dump: {:.1} s, state {}, {}",
        started.elapsed().as_secs_f64(),
        result["state"],
        serde_json::to_string_pretty(&result["texture_dump"]).unwrap()
    );
    assert_eq!(result["state"], 2, "the path reaches flight");
    let (text, manifest) = read_manifest(&first);
    let textures = manifest["textures"].as_array().unwrap();
    let in_state = |state: u64| textures.iter().filter(move |t| t["first_state"] == state);
    // Menus (state 4), the flight HUD (4-bit sprites first drawn in
    // flight) and terrain (opaque textured polygons in flight).
    let menus = in_state(4).count();
    let hud = in_state(2)
        .filter(|t| t["mode"] == "p4" && t["rectangles"].as_u64() > Some(0))
        .count();
    let terrain = in_state(2)
        .filter(|t| t["polygons"].as_u64() > Some(100) && t["semi_transparent_draws"] == 0)
        .count();
    eprintln!("menus {menus}, flight HUD sprites {hud}, flight opaque polygons {terrain}");
    assert!(menus >= 10 && hud >= 5 && terrain >= 1);
    for texture in textures {
        let file = fs::read(first.join(texture["file"].as_str().unwrap())).unwrap();
        assert_eq!(sha256(&file), texture["sha256"]);
        let (width, height) = match png_io::decode(&file).unwrap() {
            png_io::Decoded::Indexed { width, height, .. }
            | png_io::Decoded::Color15 { width, height, .. }
            | png_io::Decoded::Rgb24 { width, height, .. } => (width, height),
        };
        assert_eq!(width as u64, texture["width"].as_u64().unwrap());
        assert_eq!(height as u64, texture["height"].as_u64().unwrap());
        let stem = texture["key"].as_str().unwrap();
        assert_eq!(Key::from_stem(stem).unwrap().to_string(), stem);
    }

    // Static and runtime keys agree for textures loaded from TIMs.
    let statics = out_dir("runtime-static");
    asset_dump::dump_archive_images(&data, &statics).unwrap();
    let (_, static_manifest) = read_manifest(&statics);
    let static_keys: HashSet<String> = static_manifest["images"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|image| image["pngs"].as_array().unwrap().iter())
        .map(|png| png["key"].as_str().unwrap().to_owned())
        .collect();
    let shared = textures
        .iter()
        .filter(|t| static_keys.contains(t["key"].as_str().unwrap()))
        .count();
    eprintln!(
        "{shared} of {} runtime keys are static keys",
        textures.len()
    );
    assert!(
        shared >= 100,
        "TIM textures keep their static keys at run time"
    );
    // Every 4/8-bit texture drawn comes from a static image (same T); the
    // only other sources are the fog palette tables computed at load
    // (8005D490), drawn as 15-bit sprites into the CLUT area.
    let static_textures: HashSet<&str> = static_manifest["images"]
        .as_array()
        .unwrap()
        .iter()
        .map(|image| image["texture_hash"].as_str().unwrap())
        .collect();
    let foreign: Vec<&Value> = textures
        .iter()
        .filter(|t| !static_textures.contains(t["texture"].as_str().unwrap()))
        .collect();
    eprintln!("{} textures from non-static contents", foreign.len());
    assert!(
        foreign
            .iter()
            .all(|t| t["mode"] == "c16" && t["width"] == 16 && t["height"] == 64),
        "an indexed texture came from data the static dump does not find"
    );

    // A second run records the same textures.
    let second = out_dir("runtime-b");
    runtime_dump(&data, &second);
    let (again, _) = read_manifest(&second);
    assert!(text == again, "runtime manifests differ between runs");
}
