//! Original read/queue timings are validation inputs. Native visual state,
//! scripts, decoded textures and packets evolve continuously from initialization.
use ac3_formats::dot1;
use ac3_graphics::Frame;
use ac3_mission::*;
use ac3_port::{
    assets::GameData,
    menu_scene::LanguageScene,
    mission_assets::{MissionAssets, MissionTextureQueue},
    sha256,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::PathBuf};

fn unhex(text: &str) -> Vec<u8> {
    assert!(text.is_ascii() && text.len().is_multiple_of(2));
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}
fn rows(value: &Value) -> &[Value] {
    if value == &json!({}) {
        &[]
    } else {
        value.as_array().unwrap()
    }
}
fn normalize(packet: &mut [u32]) {
    if matches!(packet[0] >> 24, 0x2c..=0x2f) {
        packet[6] &= 0xffff;
        packet[8] &= 0xffff;
    }
}
fn packets(value: &Value) -> Vec<Vec<u32>> {
    rows(value)
        .iter()
        .map(|p| {
            let mut words: Vec<u32> = rows(p).iter().map(|w| w.as_u64().unwrap() as u32).collect();
            normalize(&mut words);
            words
        })
        .collect()
}
fn verify_provenance(capture: &std::path::Path) -> Value {
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
    json!({"metadata_sha256":sha256(&fs::read(capture.join("metadata.json")).unwrap()),"engine_sha256":metadata["engine_sha256"],"addon_sha256":addon["sha256"],"emulator":metadata["emulator"]})
}

#[test]
#[ignore = "requires original PAL assets and passive loader-visual-001 capture"]
fn continuous_native_loading_scripts_match_original_packets_and_pixels() {
    let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").unwrap());
    let capture = root.join("analysis/menu-runtime/loader-visual-001");
    let proof = verify_provenance(&capture);
    let raw = fs::read(capture.join("loader-vectors.jsonl")).unwrap();
    let script_raw = fs::read(capture.join("loader-script-vectors.jsonl")).unwrap();
    let cases: Vec<Value> = raw
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_slice(l).unwrap())
        .collect();
    let scripts: Vec<Value> = script_raw
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_slice(l).unwrap())
        .collect();
    assert_eq!(cases.len(), 187);
    assert_eq!(scripts.len(), cases.len());
    let data = GameData::load(&root.join("extracted/disc")).unwrap();
    let tables = AircraftTables::from_overlay(data.overlay(2).unwrap()).unwrap();
    let mut scene =
        LanguageScene::from_mission(&data, MissionAssets::load(&data, 74).unwrap()).unwrap();
    scene.vram = LanguageScene::load(&data).unwrap().vram;
    let mut visual = scene.blank_visual_state();
    let mut state = MissionLoaderState::default();
    let mut context =
        LoaderContext::from_bytes(&unhex(cases[0]["input_context_hex"].as_str().unwrap())).unwrap();
    let mut queue: Option<MissionTextureQueue<'_>> = None;
    let mut packet_count = 0;
    let mut uploads = 0;
    let mut frames = BTreeMap::new();
    let mut page_counts = BTreeMap::new();
    for (index, (case, script)) in cases.iter().zip(&scripts).enumerate() {
        let mut input = LoaderInputs::default();
        for dep in rows(&case["dependencies"]) {
            match dep["kind"].as_str().unwrap() {
                "size" => match dep["resource"].as_u64().unwrap() {
                    74 => input.mission_resource_bytes = data.resource(74).unwrap().len() as i32,
                    72 => input.common_resource_bytes = data.resource(72).unwrap().len() as i32,
                    _ => panic!("unexpected first-mission resource"),
                },
                "transfer" => {
                    input.transferred_total_bytes =
                        dep["transferred_total_bytes"].as_i64().unwrap() as i32;
                    input.read_complete = dep["read_complete"] == 1;
                }
                "textures" => {
                    // Decode/upload the real batch; the captured return only checks it.
                    let result = queue.as_mut().unwrap().poll(&mut scene.vram).unwrap();
                    uploads += result.uploads.len();
                    input.texture_poll_result = i32::from(result.complete);
                    assert_eq!(
                        i64::from(input.texture_poll_result),
                        dep["result"].as_i64().unwrap()
                    );
                }
                other => panic!("unknown dependency {other}"),
            }
        }
        let update = if case["kind"] == "init" {
            initialize_mission_loader(state, context)
        } else {
            tick_mission_loader(state, context, input, &tables).unwrap()
        };
        state = update.state;
        context = update.context;
        assert_eq!(
            state,
            MissionLoaderState::from_bytes(
                &unhex(case["expected_state_hex"].as_str().unwrap()),
                &unhex(case["expected_loader_hex"].as_str().unwrap())
            )
            .unwrap(),
            "loader case {index}"
        );
        assert_eq!(
            update.callback_result as u64,
            case["callback_result"].as_u64().unwrap()
        );
        let mut progress = None;
        for effect in update.effects {
            match effect {
                LoaderEffect::Common(effect) => visual.apply_mission_visuals(&[effect]).unwrap(),
                LoaderEffect::BindBuiltinScripts { .. } => {
                    scene.use_mission_loading_scripts(true).unwrap()
                }
                LoaderEffect::BindCommonScripts {
                    resource: 72,
                    child: 3,
                } => scene.use_mission_loading_scripts(false).unwrap(),
                LoaderEffect::BindMissionResource {
                    resource,
                    texture_child,
                    batch_size,
                }
                | LoaderEffect::BindCommonResource {
                    resource,
                    texture_child,
                    batch_size,
                } => {
                    let container = dot1::parse(data.resource(resource as usize).unwrap()).unwrap();
                    queue = Some(
                        MissionTextureQueue::new(
                            container.child(texture_child as usize).unwrap(),
                            batch_size as usize,
                        )
                        .unwrap(),
                    );
                }
                LoaderEffect::DrawProgressBar { width } => {
                    progress = Some(vec![0x60d5e5e5, 0x01a400c0, 0x00060000 | u32::from(width)])
                }
                _ => (), // No script mutation: I/O lifetime, audio handles, palette backup.
            }
        }
        // The outer callback's held-confirm style and frame-color are zero in this no-input run.
        visual
            .apply_mission_visuals(&[Effect::SetVisualStyle {
                slot: 0x78,
                style: 0,
            }])
            .unwrap();
        assert_eq!(script["frame_color"], 0);
        visual.set_frame_color([0; 3]);
        if index != 0 {
            assert_eq!(visual.page as u64, script["page"].as_u64().unwrap());
        }
        // Original initialization preserves an arbitrary page while the script binding is disabled.
        // The native blank state's empty page0 yields the same two frame masks.
        let native = scene
            .mission_commands(&mut visual, script["language"].as_u64().unwrap() as usize)
            .unwrap()
            .into_packets();
        let original = packets(&script["packets"]);
        assert_eq!(
            native, original,
            "loader script index {index}, frame {}",
            script["frame"]
        );
        packet_count += native.len();
        *page_counts.entry(visual.page).or_insert(0_usize) += 1;
        let mut output = native.clone();
        if let Some(packet) = progress {
            output.push(packet);
        }
        let mut frame = Frame::new(320, 512).unwrap();
        frame.draw_tile(0, 0, 320, 511, [0; 3], None);
        ac3_graphics::render_packets(&mut frame, &scene.vram, &output).unwrap();
        let pixels: Vec<u8> = frame.words().iter().flat_map(|w| w.to_le_bytes()).collect();
        frames.insert(script["frame"].as_u64().unwrap(), (output, pixels));
    }
    assert_eq!(uploads, 712);
    let mut image_reports = Vec::new();
    let mut pixel_count = 0;
    // F1BB8 at F -> DrawOTag at F+1 -> displayed screen capture at F+2.
    // 243 is deliberately excluded: the texture worker spans Vsyncs there.
    for screen_frame in [49_u64, 50, 60, 100, 180, 200, 250, 254, 255] {
        let callback_frame = screen_frame - 2;
        let (native, actual) = frames.get(&callback_frame).unwrap();
        let ot_file = fs::read_dir(&capture)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(&format!("ot-{:05}-", screen_frame - 1))
            })
            .unwrap();
        let ot: Value = serde_json::from_slice(&fs::read(&ot_file).unwrap()).unwrap();
        let original: Vec<_> = rows(&ot["packets"])
            .iter()
            .map(|p| p["words"].clone())
            .collect();
        let original = packets(&Value::Array(original));
        assert_eq!(
            original.len(),
            native.len() + 3,
            "original outer trailer must contain exactly three packets"
        );
        assert_eq!(
            &original[..native.len()],
            native,
            "ordering table timing at screen {screen_frame}"
        );
        // Resident outer trailer: draw mode, a 2x1 self-copy at (0,0), and NOPs.
        assert_eq!(
            &original[native.len()..],
            &[
                vec![0xe1000015],
                vec![0x80000000, 0, 0, 0x00010002],
                vec![0, 0, 0, 0]
            ]
        );
        let expected =
            fs::read(capture.join(format!("frame-{screen_frame:05}.screen.raw"))).unwrap();
        assert_eq!(actual.len(), expected.len());
        let differences = actual
            .chunks_exact(2)
            .zip(expected.chunks_exact(2))
            .filter(|(a, b)| a != b)
            .count();
        assert_eq!(
            differences, 0,
            "native loading pixels at screen {screen_frame}, callback {callback_frame}"
        );
        pixel_count += actual.len() / 2;
        image_reports.push(json!({"callback_frame":callback_frame,"ot_frame":screen_frame-1,"screen_frame":screen_frame,"pixels":actual.len()/2,"different_pixels":differences,"sha256":sha256(actual)}));
    }
    let mut sources = BTreeMap::new();
    for path in [
        "tests/loader_visual.rs",
        "src/menu_scene.rs",
        "src/mission_assets.rs",
        "src/menu_transform.rs",
        "crates/ac3-mission/src/loader.rs",
    ] {
        sources.insert(path, sha256(&fs::read(root.join(path)).unwrap()));
    }
    let report = json!({"passed":true,"loader_callbacks":cases.len(),"script_renders":scripts.len(),"native_script_packets":packet_count,
        "native_texture_uploads":uploads,"pages":page_counts,"screens":image_reports,"pixels_compared":pixel_count,
        "scope":"Continuous native blank visual state, loader reducers, script-bank switches, scripts, texture decoding/batching and packets. Captured asynchronous read byte counts/readiness are explicit validation inputs. Nine original display screenshots and preceding OT submissions checked with exact two-frame timing. Captured script state, textures, pixels and commands are never injected into the native scene. Audio playback and native I/O timing excluded.",
        "normalization":"Address-dependent DMA links omitted; FT4 GPU-ignored UV2/UV3 upper halfwords masked. Three verified resident outer trailer packets do not alter these screenshots.",
        "loader_trace_sha256":sha256(&raw),"script_trace_sha256":sha256(&script_raw),"source_sha256":sources,"provenance":proof});
    fs::create_dir_all(root.join("analysis/native-mission")).unwrap();
    fs::write(
        root.join("analysis/native-mission/loader-visual-validation.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
}
