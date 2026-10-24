use ac3_mission::*;
use ac3_port::{
    aircraft_scene::AircraftScene, assets::GameData, menu_scene::LanguageScene,
    mission_assets::MissionAssets, sha256,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

fn unhex(s: &str) -> Vec<u8> {
    assert!(s.is_ascii() && s.len().is_multiple_of(2));
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}
fn list(v: &Value) -> &[Value] {
    if v == &json!({}) {
        &[]
    } else {
        v.as_array().unwrap()
    }
}
fn jsonl(p: &Path) -> Vec<Value> {
    fs::read_to_string(p)
        .unwrap()
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}
fn words(v: &Value) -> Vec<Vec<u32>> {
    list(v)
        .iter()
        .map(|p| list(p).iter().map(|w| w.as_u64().unwrap() as u32).collect())
        .collect()
}
fn provenance(capture: &Path) -> Value {
    let metadata: Value =
        serde_json::from_slice(&fs::read(capture.join("metadata.json")).unwrap()).unwrap();
    let addon: Value =
        serde_json::from_slice(&fs::read(capture.join("addon-metadata.json")).unwrap()).unwrap();
    assert_eq!(
        metadata["original_disc_sha256"],
        "5d59ef5bf2b905cde40cf8a14647b409228a4443eb94e9a0444351544ca87276"
    );
    assert_eq!(metadata["guest_code_patched"], false);
    assert_eq!(metadata["guest_ram_patched"], false);
    assert_eq!(
        metadata["engine_sha256"],
        sha256(&fs::read(capture.join("capture-engine.lua")).unwrap())
    );
    assert_eq!(
        addon["sha256"],
        sha256(&fs::read(capture.join("capture-addon.lua")).unwrap())
    );
    json!({"engine_sha256":metadata["engine_sha256"],"addon_sha256":addon["sha256"],"metadata_sha256":sha256(&fs::read(capture.join("metadata.json")).unwrap()),"emulator":metadata["emulator"]})
}

#[test]
#[ignore = "requires original assets and aircraft-visual-002 passive capture"]
fn native_aircraft_statistics_resources_and_draw_boundary_match_original() {
    let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").unwrap());
    let capture = root.join("analysis/menu-runtime/aircraft-visual-002");
    let proof = provenance(&capture);
    let wrappers = jsonl(&capture.join("aircraft-visual-wrappers.jsonl"));
    let draws = jsonl(&capture.join("aircraft-draw-vectors.jsonl"));
    let resources = jsonl(&capture.join("aircraft-resource-vectors.jsonl"));
    let data = GameData::load(&root.join("extracted/disc")).unwrap();
    let tables = AircraftTables::from_overlay(data.overlay(2).unwrap()).unwrap();
    let aircraft = AircraftScene::load(&data).unwrap();
    let mut scene =
        LanguageScene::from_mission(&data, MissionAssets::load(&data, 74).unwrap()).unwrap();
    let draw_by_sequence: BTreeMap<_, _> = draws
        .iter()
        .map(|d| (d["sequence"].as_u64().unwrap(), d))
        .collect();
    let mut packet_count = 0;
    let mut generated_uploads = Vec::new();
    let mut draw_count = 0;
    for case in &wrappers {
        let input = &case["input"];
        let expected = &case["expected"];
        let kind = case["kind"].as_str().unwrap();
        let state =
            AircraftSelectionState::from_bytes(&unhex(input["state_hex"].as_str().unwrap()))
                .unwrap();
        let context_bytes = unhex(input["context_hex"].as_str().unwrap());
        let pressed = input["pressed"].as_u64().unwrap() as u32;
        if kind.starts_with("launch") {
            let context = MissionContext::from_bytes(&context_bytes).unwrap();
            let update = if kind == "launch_init" {
                initialize_launch(context)
            } else {
                tick_launch(
                    LaunchState {
                        timer: input["launch_timer"].as_u64().unwrap() as i32,
                    },
                    context,
                    pressed,
                )
            };
            assert_eq!(
                update.state.timer as u32 as u64,
                expected["launch_timer"].as_u64().unwrap()
            );
            assert_eq!(
                update.callback_result as u64,
                case["callback_result"].as_u64().unwrap()
            );
            continue;
        }
        let context = AircraftContext::from_bytes(&context_bytes).unwrap();
        let update = if kind == "aircraft_init" {
            initialize_aircraft(state, context)
        } else {
            tick_aircraft(state, context, &tables, pressed).unwrap()
        };
        assert_eq!(
            update.state.to_bytes().as_slice(),
            unhex(expected["state_hex"].as_str().unwrap())
        );
        assert_eq!(
            update.callback_result as u64,
            case["callback_result"].as_u64().unwrap()
        );
        let sequence = case["sequence"].as_u64().unwrap();
        if let Some(draw) = update.draw_state {
            let observed = draw_by_sequence[&sequence];
            draw_count += 1;
            assert_eq!(
                draw.to_bytes().as_slice(),
                unhex(observed["state_hex"].as_str().unwrap()),
                "native draw boundary sequence {sequence}"
            );
            assert_eq!(
                observed["state_hex"], observed["expected_state_hex"],
                "original draw must preserve selection state"
            );
            let commands = aircraft.commands(&scene, &draw).unwrap();
            assert_eq!(
                commands.packets,
                words(&observed["packets"]),
                "statistics packets at sequence {sequence}"
            );
            packet_count += commands.packets.len();
            let mut visible = unhex(observed["input_visible_hex"].as_str().unwrap());
            for effect in commands.effects {
                match effect {
                    Effect::ClearTextVisibility => visible.fill(0),
                    Effect::SetTextVisibility { entry, value } => visible[entry as usize] = value,
                    _ => panic!("unexpected statistics visibility effect"),
                }
            }
            assert_eq!(
                visible,
                unhex(observed["expected_visible_hex"].as_str().unwrap())
            );
        } else {
            assert!(!draw_by_sequence.contains_key(&sequence));
        }
        for effect in update.effects {
            if let Effect::LoadAircraftResources { index, rectangle } = effect {
                let calls: Vec<_> = resources
                    .iter()
                    .filter(|r| r["sequence"].as_u64() == Some(sequence))
                    .collect();
                assert_eq!(calls.len(), 2);
                assert_eq!(calls[0]["child"], 1);
                assert_eq!(calls[1]["child"], 2);
                assert_eq!(calls[0]["index"], index);
                assert_eq!(calls[1]["index"], index);
                assert_eq!(calls[0]["capacity"], 0x3820);
                assert_eq!(calls[1]["capacity"], 0x828);
                assert_eq!(calls[0]["rectangle"], json!(rectangle));
                assert!(list(&calls[1]["rectangle"]).is_empty());
                assert_eq!(calls[0]["draw_sync"], 1);
                assert_eq!(calls[1]["draw_sync"], 1);
                generated_uploads.extend(
                    aircraft
                        .load_resources(index, rectangle, &mut scene.vram)
                        .unwrap(),
                );
            }
        }
    }
    let uploads: Value =
        serde_json::from_slice(&fs::read(capture.join("uploads.json")).unwrap()).unwrap();
    assert_eq!(generated_uploads.len(), list(&uploads).len());
    for (native, original) in generated_uploads.iter().zip(list(&uploads)) {
        for (key, value) in [
            ("x", native.x),
            ("y", native.y),
            ("width", native.width),
            ("height", native.height),
        ] {
            assert_eq!(original[key], value);
        }
        assert_eq!(original["sha256"], sha256(&native.data));
        assert_eq!(original["bytes"], native.data.len());
    }
    assert_eq!(wrappers.len(), 452);
    assert_eq!(draw_count, 398);
    assert_eq!(generated_uploads.len(), 4);
    let mut hashes = BTreeMap::new();
    for path in [
        "src/aircraft_scene.rs",
        "src/menu_scene.rs",
        "src/menu_fonts.rs",
        "crates/ac3-mission/src/aircraft.rs",
        "tests/aircraft_visual.rs",
    ] {
        hashes.insert(path, sha256(&fs::read(root.join(path)).unwrap()));
    }
    fs::create_dir_all(root.join("analysis/native-mission")).unwrap();
    fs::write(root.join("analysis/native-mission/aircraft-draw-validation.json"),serde_json::to_vec_pretty(&json!({"passed":true,"wrappers":wrappers.len(),"draws":draw_count,"native_packets":packet_count,"resource_calls":resources.len(),"uploads":generated_uploads.len(),"source_sha256":hashes,"provenance":proof,
  "scope":"Native aircraft/launch reducers from observed input, exact pre-weapon-interpolation draw boundary, all D8CB4 generated statistics packets and text-visibility mutations; native aircraft image/label TIM decoding and upload order. Shared page scripts, full scene pixels, background and streaming/audio excluded."})).unwrap()).unwrap();
}

#[test]
#[ignore = "requires original assets and aircraft-visual-002 passive capture"]
fn native_aircraft_shared_selection_and_launch_packets_match_original() {
    let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").unwrap());
    let capture = root.join("analysis/menu-runtime/aircraft-visual-002");
    let proof = provenance(&capture);
    let wrappers = jsonl(&capture.join("aircraft-visual-wrappers.jsonl"));
    let scripts = jsonl(&capture.join("aircraft-script-vectors.jsonl"));
    let texts = jsonl(&capture.join("aircraft-text-vectors.jsonl"));
    let scripts: BTreeMap<_, _> = scripts
        .iter()
        .map(|d| (d["sequence"].as_u64().unwrap(), d))
        .collect();
    let texts: BTreeMap<_, _> = texts
        .iter()
        .map(|d| (d["sequence"].as_u64().unwrap(), d))
        .collect();
    let data = GameData::load(&root.join("extracted/disc")).unwrap();
    let tables = AircraftTables::from_overlay(data.overlay(2).unwrap()).unwrap();
    let aircraft = AircraftScene::load(&data).unwrap();
    let mut scene =
        LanguageScene::from_mission(&data, MissionAssets::load(&data, 74).unwrap()).unwrap();
    let mut visual = scene.visual_state().unwrap();
    // Establish persistent shared state through the native hub intro/exit.
    // In particular, its final intro offset1 is (0,1), retained by aircraft.
    let mut hub = initialize_hub(
        MissionState::default(),
        MissionContext {
            options: 0x42,
            current_pair: [1, 0],
            ..Default::default()
        },
    );
    visual.apply_mission_visuals(&hub.effects).unwrap();
    scene.mission_commands(&mut visual, 0).unwrap();
    while hub.state.phase != 2 {
        hub = tick_hub(hub.state, hub.mission, hub.context, 0).unwrap();
        visual.apply_mission_visuals(&hub.effects).unwrap();
        scene.mission_commands(&mut visual, 0).unwrap();
    }
    hub = tick_hub(hub.state, hub.mission, hub.context, 0x10).unwrap();
    loop {
        visual.apply_mission_visuals(&hub.effects).unwrap();
        scene.mission_commands(&mut visual, 0).unwrap();
        if hub.callback_result != 0 {
            break;
        }
        hub = tick_hub(hub.state, hub.mission, hub.context, 0).unwrap();
    }
    let mut frames = 0;
    let mut packet_count = 0;
    for case in &wrappers {
        let input = &case["input"];
        let kind = case["kind"].as_str().unwrap();
        let sequence = case["sequence"].as_u64().unwrap();
        let context_bytes = unhex(input["context_hex"].as_str().unwrap());
        let pressed = input["pressed"].as_u64().unwrap() as u32;
        let (effects, draw_state) = if kind.starts_with("launch") {
            let context = MissionContext::from_bytes(&context_bytes).unwrap();
            let update = if kind == "launch_init" {
                initialize_launch(context)
            } else {
                tick_launch(
                    LaunchState {
                        timer: input["launch_timer"].as_u64().unwrap() as i32,
                    },
                    context,
                    pressed,
                )
            };
            (update.effects, None)
        } else {
            let state =
                AircraftSelectionState::from_bytes(&unhex(input["state_hex"].as_str().unwrap()))
                    .unwrap();
            let context = AircraftContext::from_bytes(&context_bytes).unwrap();
            let update = if kind == "aircraft_init" {
                initialize_aircraft(state, context)
            } else {
                tick_aircraft(state, context, &tables, pressed).unwrap()
            };
            (update.effects, update.draw_state)
        };
        visual
            .apply_mission_visuals(&[Effect::SetVisualStyle {
                slot: 0x78,
                style: u32::from(input["held"].as_u64().unwrap() & 0x10 != 0),
            }])
            .unwrap();
        for effect in &effects {
            match *effect {
                Effect::PendingOriginalCall {
                    address: 0x800d8cb4,
                } => {
                    let commands = aircraft.commands(&scene, &draw_state.unwrap()).unwrap();
                    visual.apply_mission_visuals(&commands.effects).unwrap();
                }
                Effect::LoadAircraftResources { index, rectangle } => {
                    aircraft
                        .load_resources(index, rectangle, &mut scene.vram)
                        .unwrap();
                }
                _ => visual
                    .apply_mission_visuals(std::slice::from_ref(effect))
                    .unwrap(),
            }
        }
        // Init retains the preceding briefing page. This test starts at the
        // aircraft boundary and intentionally excludes that inherited frame.
        if kind == "aircraft_init" || !scripts.contains_key(&sequence) {
            continue;
        }
        let original = scripts[&sequence];
        let color = original["frame_color"].as_u64().unwrap() as u32;
        visual.set_frame_color([color as u8, (color >> 8) as u8, (color >> 16) as u8]);
        assert_eq!(visual.page as u64, original["page"].as_u64().unwrap());
        let commands = scene
            .mission_commands(&mut visual, original["language"].as_u64().unwrap() as usize)
            .unwrap_or_else(|e| panic!("sequence{sequence}: {e}"));
        let expected = words(&original["packets"])
            .into_iter()
            .chain(words(&texts[&sequence]["packets"]).into_iter().rev())
            .collect::<Vec<_>>();
        let native = commands.into_packets();
        assert_eq!(
            native.len(),
            expected.len(),
            "shared packet count sequence{sequence}"
        );
        for (i, (a, b)) in native.iter().zip(&expected).enumerate() {
            assert_eq!(a, b, "shared packet {i} sequence{sequence}");
        }
        packet_count += native.len();
        frames += 1;
    }
    assert_eq!(frames, 450);
    let source_hashes: BTreeMap<_, _> = [
        "src/aircraft_scene.rs",
        "src/menu_scene.rs",
        "src/menu_fonts.rs",
        "crates/ac3-mission/src/aircraft.rs",
        "tests/aircraft_visual.rs",
    ]
    .into_iter()
    .map(|path| (path, sha256(&fs::read(root.join(path)).unwrap())))
    .collect();
    fs::write(root.join("analysis/native-mission/aircraft-shared-validation.json"), serde_json::to_vec_pretty(&json!({"passed":true,"frames":frames,"native_packets":packet_count,"provenance":proof,"source_sha256":source_hashes,"scope":"Aircraft/weapon/launch shared scripts and text, asset-initialized persistent visual state with native reducer effects; wrapper scalar inputs and per-frame background clear color observed. Inherited briefing init frame, background map and full pixels excluded."})).unwrap()).unwrap();
}
