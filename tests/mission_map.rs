use ac3_graphics::{Vram, scalar::ScalarMath, transform::SinCosTable};
use ac3_port::{mission_background, mission_map};
use mission_background::PaletteGradient;
use mission_map::*;
use serde_json::Value;
fn bytes(v: &Value, field: &str) -> Vec<u8> {
    v[field]
        .as_str()
        .unwrap()
        .as_bytes()
        .chunks_exact(2)
        .map(|s| u8::from_str_radix(std::str::from_utf8(s).unwrap(), 16).unwrap())
        .collect()
}
fn i16at(b: &[u8], a: usize) -> i16 {
    i16::from_le_bytes(b[a..a + 2].try_into().unwrap())
}
fn i32at(b: &[u8], a: usize) -> i32 {
    i32::from_le_bytes(b[a..a + 4].try_into().unwrap())
}
fn matrix(b: &[u8], a: usize) -> Matrix {
    Matrix {
        rotation: std::array::from_fn(|r| std::array::from_fn(|c| i16at(b, a + (r * 3 + c) * 2))),
        translation: std::array::from_fn(|i| i32at(b, a + 20 + i * 4)),
    }
}
fn state(v: &Value) -> MapState {
    let s = bytes(v, "state_hex");
    let a = bytes(v, "angles_hex");
    let t = bytes(v, "translations_hex");
    let vs = bytes(v, "vertices_hex");
    let oi = bytes(v, "object_indices_hex");
    let os = bytes(v, "objects_hex");
    MapState {
        frame: i32at(&s, 12) as u32,
        tube_phase: i32at(&s, 0x14),
        object_phase: i32at(&s, 0x18),
        tube_progress: i32at(&s, 0x20),
        object_progress: i32at(&s, 0x24),
        world: matrix(&s, 0x2c),
        camera_angles: std::array::from_fn(|i| i16at(&s, 0x88 + i * 2)),
        angle_counter: i16at(&s, 0x6c),
        speed: i16at(&s, 0x70),
        fraction: i16at(&s, 0x72),
        step: i16at(&s, 0x74),
        ring_phase: i32at(&s, 0x7c) as u32,
        angles: std::array::from_fn(|i| std::array::from_fn(|j| i16at(&a, i * 8 + j * 2))),
        translations: std::array::from_fn(|i| std::array::from_fn(|j| i32at(&t, i * 16 + j * 4))),
        vertices: std::array::from_fn(|i| std::array::from_fn(|j| i16at(&vs, i * 8 + j * 2))),
        object_indices: std::array::from_fn(|i| i16at(&oi, i * 2)),
        objects: std::array::from_fn(|i| std::array::from_fn(|j| i32at(&os, i * 16 + j * 4))),
    }
}
fn texture(v: &Value, offset: usize) -> Texture {
    let b = bytes(v, "textures_hex");
    Texture {
        origin: [i16at(&b, offset), i16at(&b, offset + 2)],
        clut: [i16at(&b, offset + 8) as u16, i16at(&b, offset + 10) as u16],
    }
}
#[test]
#[ignore = "requires the original game and independent runtime fixtures"]
fn original_map_packets() {
    let root = std::path::PathBuf::from(std::env::var("AC3_DATA_DIR").unwrap());
    let data = ac3_port::assets::GameData::load(&root.join("extracted/disc")).unwrap();
    let trig = SinCosTable::from_le_bytes(&data.executable.payload[0xa4f84..0xa8f84]).unwrap();
    let math = ScalarMath::from_resident(&data.executable.payload, 0x80010000).unwrap();
    let fixtures = if let Ok(fixture) = std::env::var("AC3_MAP_FIXTURE") {
        vec![fixture]
    } else {
        [
            "map-packets-001",
            "map-init-001",
            "map-reveal-001",
            "map-exit-001",
            "map-launch-001",
        ]
        .map(|name| format!("analysis/menu-runtime/{name}/map-packets.jsonl"))
        .to_vec()
    };
    for fixture in fixtures {
        let fixture_path = root.join(fixture);
        let text = std::fs::read_to_string(&fixture_path).unwrap();
        let mut count = 0;
        let mut word_count = 0;
        let mut updates = 0;
        let mut initializations = 0;
        for line in text.lines() {
            let case: Value = serde_json::from_str(line).unwrap();
            let kind = case["kind"].as_str().unwrap();
            if kind == "update" {
                let mut s = state(&case["input"]);
                s.update(&trig, &math).unwrap();
                assert_eq!(
                    s,
                    state_from_expected(&case),
                    "update frame {}",
                    case["frame"]
                );
                updates += 1;
                continue;
            }
            if kind == "init_objects" {
                let mut s = state(&case["input"]);
                let mut random = Random {
                    state: case["random_before"].as_u64().unwrap() as u32,
                };
                s.initialize_objects(&math, &mut random);
                assert_eq!(s, state_from_expected(&case));
                assert_eq!(random.state, case["random_after"].as_u64().unwrap() as u32);
                initializations += 1;
                continue;
            }
            if kind == "init" {
                let mut random = Random {
                    state: case["random_before"].as_u64().unwrap() as u32,
                };
                let s = MapState::new(&math, &mut random);
                let e = state_from_expected(&case);
                assert_eq!(s.angles, e.angles);
                assert_eq!(s.translations, e.translations);
                assert_eq!(s.vertices, e.vertices);
                assert_eq!(s.object_indices, e.object_indices);
                assert_eq!(s.objects, e.objects);
                assert_eq!(
                    (
                        s.frame,
                        s.tube_phase,
                        s.object_phase,
                        s.angle_counter,
                        s.speed,
                        s.fraction,
                        s.ring_phase
                    ),
                    (
                        e.frame,
                        e.tube_phase,
                        e.object_phase,
                        e.angle_counter,
                        e.speed,
                        e.fraction,
                        e.ring_phase
                    )
                );
                initializations += 1;
                continue;
            }
            if kind == "init_tube" {
                continue;
            }
            let input = &case["input"];
            let mut state = state(input);
            let raw = bytes(input, "state_hex");
            let mut vram = vec![0; 1024 * 512 * 2];
            vram[(63 * 1024 + 432) * 2..(63 * 1024 + 448) * 2].copy_from_slice(&raw[0xbc..0xdc]);
            let palette =
                PaletteGradient::from_vram(&Vram::from_le_bytes(&vram).unwrap(), [432, 63])
                    .unwrap();
            let rgb: [u8; 3] = bytes(input, "far_rgb_hex").try_into().unwrap();
            let actual = match kind {
                "near" => state
                    .draw_near(&trig, texture(input, 0), &palette, rgb)
                    .unwrap(),
                "far" => state.draw_far(&trig, texture(input, 0)),
                "objects" => state.draw_objects(&trig, texture(input, 16), rgb).unwrap(),
                _ => panic!(),
            };
            let packet_bytes = bytes(&case, "packet_hex");
            let words: Vec<u32> = packet_bytes
                .chunks_exact(4)
                .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
                .collect();
            let mut at = 0;
            let mut expected = Vec::new();
            let start = case["packet_start"].as_u64().unwrap() as u32;
            while at < words.len() {
                let count = (words[at] >> 24) as usize;
                expected.push((
                    start + at as u32 * 4,
                    words[at + 1..at + 1 + count].to_vec(),
                ));
                at += count + 1;
            }
            assert_eq!(
                actual.emitted.len(),
                expected.len(),
                "{} frame {} packet count",
                kind,
                case["frame"]
            );
            let ot = bytes(&case, "ot_hex");
            let mut buckets = std::collections::HashMap::new();
            for bucket in 0..256 {
                let mut link = i32at(&ot, bucket * 4) as u32 & 0xffffff;
                while let Some((_, packet)) =
                    expected.iter().find(|(addr, _)| addr & 0xffffff == link)
                {
                    buckets.insert(link, bucket as u8);
                    let offset = ((link - (start & 0xffffff)) / 4) as usize;
                    link = words[offset] & 0xffffff;
                    let _ = packet;
                }
            }
            for (index, (actual, (address, mut expected))) in
                actual.emitted.iter().zip(expected).enumerate()
            {
                if kind == "objects" {
                    expected[6] &= 65535;
                    expected[8] &= 65535;
                }
                assert_eq!(
                    actual.words, expected,
                    "{} frame {} packet {}",
                    kind, case["frame"], index
                );
                assert_eq!(
                    Some(&actual.bucket),
                    buckets.get(&(address & 0xffffff)),
                    "{} frame {} packet {} bucket",
                    kind,
                    case["frame"],
                    index
                );
                word_count += actual.words.len();
            }
            assert_eq!(
                state,
                state_from_expected(&case),
                "{} frame {} post-state",
                kind,
                case["frame"]
            );
            count += 1;
        }
        eprintln!(
            "{count} draw boundaries, {initializations} initializations and {word_count} GP0 words exact"
        );
        assert!(count >= 12 || initializations >= 2);
        if std::env::var_os("AC3_MAP_WRITE_REPORT").is_some() {
            let report = serde_json::json!({"status":"passed","platform":std::env::consts::OS,
            "fixture_sha256":ac3_port::sha256(text.as_bytes()),"native_source_sha256":ac3_port::sha256(include_bytes!("../src/mission_map.rs")),
            "draw_boundaries":count,"update_boundaries":updates,"initializations":initializations,"gp0_words":word_count,
            "checks":["emitted payloads","depth buckets","complete typed post-state"],
            "normalization":"Object UV words 6 and 8 have unused high halves cleared; guest DMA pointers excluded."});
            std::fs::write(
                fixture_path.parent().unwrap().join(format!(
                    "native-map-validation-{}.json",
                    std::env::consts::OS
                )),
                serde_json::to_vec_pretty(&report).unwrap(),
            )
            .unwrap();
        }
    }
}
fn state_from_expected(case: &Value) -> MapState {
    state(&case["expected"])
}

#[test]
#[ignore = "requires original assets and the independent reveal fixture"]
fn asset_initialized_continuous_reveal() {
    let root = std::path::PathBuf::from(std::env::var("AC3_DATA_DIR").unwrap());
    let data = ac3_port::assets::GameData::load(&root.join("extracted/disc")).unwrap();
    let assets = ac3_port::mission_assets::MissionAssets::load(&data, 74).unwrap();
    let mut random = Random { state: 0 };
    let mut renderer = MapRenderer::new(
        &data.executable.payload,
        data.overlay(2).unwrap(),
        &assets.vram,
        &mut random,
    )
    .unwrap();
    assert_eq!(random.state, 2025337184);
    let text = std::fs::read_to_string(
        root.join("analysis/menu-runtime/map-reveal-001/map-packets.jsonl"),
    )
    .unwrap();
    let cases: Vec<Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let updates: Vec<&Value> = cases.iter().filter(|c| c["kind"] == "update").collect();
    let first = state(&updates[0]["input"]);
    // The inactive interval is 187 callback ticks; no state is copied from RAM.
    for _ in 0..187 {
        assert!(renderer.tick([0; 3]).unwrap().emitted.is_empty());
    }
    renderer.set_tube_phase(1).unwrap();
    renderer.set_object_phase(1).unwrap();
    assert_eq!(renderer.state.angles, first.angles);
    assert_eq!(renderer.state.translations, first.translations);
    assert_eq!(renderer.state.objects, first.objects);
    assert_eq!(renderer.state.object_indices, first.object_indices);
    let mut total = 0;
    for update in updates {
        let frame = update["frame"].as_u64().unwrap();
        let input = &update["input"];
        assert_eq!(renderer.tube_texture, texture(input, 0));
        assert_eq!(renderer.object_texture, texture(input, 16));
        let rgb = bytes(input, "far_rgb_hex").try_into().unwrap();
        let actual = renderer.tick(rgb).unwrap();
        let mut expected = Vec::new();
        for case in cases.iter().filter(|c| {
            c["frame"] == frame
                && (c["kind"] == "near" || c["kind"] == "far" || c["kind"] == "objects")
        }) {
            let raw = bytes(case, "packet_hex");
            let words: Vec<u32> = raw
                .chunks_exact(4)
                .map(|w| u32::from_le_bytes(w.try_into().unwrap()))
                .collect();
            let mut at = 0;
            while at < words.len() {
                let n = (words[at] >> 24) as usize;
                let mut p = words[at + 1..at + 1 + n].to_vec();
                if case["kind"] == "objects" {
                    p[6] &= 65535;
                    p[8] &= 65535;
                }
                expected.push(p);
                at += n + 1;
            }
        }
        assert_eq!(
            actual
                .emitted
                .iter()
                .map(|p| p.words.clone())
                .collect::<Vec<_>>(),
            expected,
            "continuous frame {frame}"
        );
        assert_eq!(actual.execution_order().len(), actual.emitted.len());
        total += actual.emitted.len();
        let last = cases
            .iter()
            .rev()
            .find(|c| c["frame"] == frame && c["kind"] == "objects")
            .unwrap();
        let mut expected_state = state(&last["expected"]);
        expected_state.frame += 1;
        assert_eq!(
            renderer.state, expected_state,
            "continuous state frame {frame}"
        );
    }
    eprintln!(
        "{total} packets matched from original assets and translated initialization; no captured geometry input"
    );
    if std::env::var_os("AC3_MAP_WRITE_REPORT").is_some() {
        let report = serde_json::json!({"status":"passed","platform":std::env::consts::OS,"frames":85,"packets":total,
            "native_source_sha256":ac3_port::sha256(include_bytes!("../src/mission_map.rs")),"fixture_sha256":ac3_port::sha256(text.as_bytes()),
            "initialization":"Original resident tables, original overlay descriptors, original loaded TIM palette, translated constants, PRNG seed 0",
            "captured_geometry_inputs":false,"caller_inputs":"187 inactive ticks, phase activation, observed per-frame far RGB"});
        std::fs::write(
            root.join(format!(
                "analysis/menu-runtime/map-reveal-001/native-map-continuous-{}.json",
                std::env::consts::OS
            )),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
    }
}
