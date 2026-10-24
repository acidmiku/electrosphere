use ac3_mission::{FullBriefingState, apply_briefing_draw_outcomes};
use ac3_port::{briefing_scene, mission_map};
use briefing_scene::*;
use serde_json::Value;
fn bytes(v: &Value, key: &str) -> Vec<u8> {
    v[key]
        .as_str()
        .unwrap()
        .as_bytes()
        .chunks_exact(2)
        .map(|b| u8::from_str_radix(std::str::from_utf8(b).unwrap(), 16).unwrap())
        .collect()
}

fn original_packets(c: &Value) -> Vec<Vec<u32>> {
    let raw = bytes(c, "packet_hex");
    let words: Vec<u32> = raw
        .chunks_exact(4)
        .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
        .collect();
    let mut packets = Vec::new();
    let mut at = 0;
    while at < words.len() {
        let count = (words[at] >> 24) as usize;
        let mut packet = words[at + 1..at + count + 1].to_vec();
        if packet.first().is_some_and(|v| v >> 24 == 0x2c) {
            packet[6] &= 65535;
            packet[8] &= 65535;
        }
        packets.push(packet);
        at += count + 1;
    }
    packets
}

fn normalize_packet(mut packet: Vec<u32>) -> Vec<u32> {
    if matches!(packet[0] >> 24, 0x2c | 0x2e) {
        packet[6] &= 65535;
        packet[8] &= 65535;
    }
    packet
}
fn composition_reference(
    root: &std::path::Path,
    fixture: &str,
    frame: u64,
) -> Option<(String, Vec<Vec<u32>>)> {
    if fixture != "briefing-wrapper-normal-002" {
        return None;
    }
    let file = match frame {
        98 => "ot-00099-001.json",
        698 => "ot-00699-003.json",
        848 => "ot-00849-005.json",
        _ => return None,
    };
    let original: Value = serde_json::from_slice(
        &std::fs::read(root.join(format!("analysis/menu-runtime/briefing-scene-001/{file}")))
            .unwrap(),
    )
    .unwrap();
    let packets: Vec<Vec<u32>> = original["packets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            normalize_packet(
                p["words"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_u64().unwrap() as u32)
                    .collect(),
            )
        })
        .collect();
    let start = packets
        .iter()
        .position(|p| p[0] >> 24 == 0x60 && p[1] == 0x1ff0000 && p[2] == 0x40140)
        .unwrap();
    Some((file.to_owned(), packets[start..].to_vec()))
}

struct OriginalCallback {
    input: Vec<u8>,
    outer_zero: Vec<Vec<u32>>,
    outer_one: Vec<Vec<u32>>,
    menu_five: Vec<Vec<u32>>,
}
fn callback_references(root: &std::path::Path, fixture: &str) -> Option<Vec<OriginalCallback>> {
    let suffix = match fixture {
        "briefing-wrapper-normal-002" => "001",
        "briefing-wrapper-fast-001" => "fast-001",
        _ => return None,
    };
    let text = std::fs::read_to_string(root.join(format!(
        "analysis/menu-runtime/briefing-ui-{suffix}/briefing-ui.jsonl"
    )))
    .unwrap();
    let mut results = Vec::new();
    let mut outer_zero = Vec::new();
    for line in text.lines() {
        let c: Value = serde_json::from_str(line).unwrap();
        match c["kind"].as_str().unwrap() {
            "connector" | "map_text" => outer_zero.extend(original_packets(&c)),
            "font" if c["caller"].as_u64().unwrap() < 0x800eea20 => {
                outer_zero.extend(original_packets(&c))
            }
            "minimap" => results.push(OriginalCallback {
                input: Vec::new(),
                outer_zero: std::mem::take(&mut outer_zero),
                outer_one: Vec::new(),
                menu_five: original_packets(&c),
            }),
            _ => (),
        }
    }
    assert!(outer_zero.is_empty());
    let text = std::fs::read_to_string(root.join(format!(
        "analysis/menu-runtime/briefing-scene-{suffix}/briefing-scene.jsonl"
    )))
    .unwrap();
    let mut terrain = Vec::new();
    let mut index = 0;
    for line in text.lines() {
        let c: Value = serde_json::from_str(line).unwrap();
        match c["kind"].as_str().unwrap() {
            "terrain" => terrain = original_packets(&c),
            "scene" => {
                let packets = original_packets(&c);
                assert!(packets.starts_with(&terrain));
                let row = &mut results[index];
                index += 1;
                row.input = bytes(&c, "input_state_hex")[..FullBriefingState::BYTE_LEN].to_vec();
                row.outer_zero.extend_from_slice(&packets[terrain.len()..]);
                row.outer_one = std::mem::take(&mut terrain);
            }
            _ => (),
        }
    }
    assert_eq!(index, results.len());
    Some(results)
}

#[test]
#[ignore = "requires original assets and independent briefing UI capture"]
fn original_briefing_ui() {
    let root = std::path::PathBuf::from(std::env::var("AC3_DATA_DIR").unwrap());
    let data = ac3_port::assets::GameData::load(&root.join("extracted/disc")).unwrap();
    let ui = BriefingUi::new(&data, 74).unwrap();
    let fonts = ac3_port::menu_scene::LanguageScene::from_mission(
        &data,
        ac3_port::mission_assets::MissionAssets::load(&data, 74).unwrap(),
    )
    .unwrap();
    for fixture in ["briefing-ui-001", "briefing-ui-fast-001"] {
        let text = std::fs::read_to_string(
            root.join(format!("analysis/menu-runtime/{fixture}/briefing-ui.jsonl")),
        )
        .unwrap();
        let mut counts = std::collections::BTreeMap::new();
        for line in text.lines() {
            let c: Value = serde_json::from_str(line).unwrap();
            let kind = c["kind"].as_str().unwrap();
            if kind == "draw_env" {
                continue;
            }
            let input = bytes(&c, "input_state_hex");
            let state =
                FullBriefingState::from_bytes(&input, c["script_base"].as_u64().unwrap() as u32)
                    .unwrap();
            let lang = c["language"].as_u64().unwrap() as usize;
            let packets = match kind {
                "connector" => ui
                    .connector(
                        &state,
                        c["label_index"].as_u64().unwrap() as usize,
                        std::array::from_fn(|i| c["visual_offset"][i].as_i64().unwrap() as i16),
                    )
                    .unwrap(),
                "minimap" => ui.minimap(&state).unwrap(),
                "map_text" => {
                    let requests = ui
                        .map_text(
                            lang,
                            c["mission"].as_i64().unwrap() as i16,
                            state.script.selected_label,
                            [
                                (state.script.camera[0] / 128 + 128) as i16,
                                (state.script.camera[2] / 128 + 128) as i16,
                            ],
                        )
                        .unwrap();
                    requests
                        .iter()
                        .flat_map(|r| r.packets(&fonts).unwrap())
                        .collect()
                }
                "font" => {
                    let pos = std::array::from_fn(|i| c["position"][i].as_i64().unwrap() as i16);
                    assert_eq!(c["font"].as_u64().unwrap(), 16);
                    assert_eq!(c["rgb"].as_u64().unwrap(), 0x808080);
                    let text = bytes(&c, "text_hex");
                    if c["caller"].as_u64().unwrap() < 0x800eea20 {
                        let index = state.text_index - state.text_index % 3
                            + (i32::from(pos[1]) - 320) / 20;
                        let request = ui.text_line(lang, index, pos.map(i32::from)).unwrap();
                        assert_eq!(request.text, text, "asset text line {index}");
                        assert_eq!(ui.text_line_count(lang).unwrap() as i32, state.text_count);
                    }
                    BriefingText {
                        text,
                        position: pos,
                    }
                    .packets(&fonts)
                    .unwrap()
                }
                _ => panic!("unknown original UI kind {kind}"),
            };
            assert_eq!(packets, original_packets(&c), "{kind} frame {}", c["frame"]);
            *counts.entry(kind.to_owned()).or_insert(0usize) += 1;
        }
        eprintln!("{fixture} exact briefing UI observations: {counts:?}");
        assert!(counts.get("minimap").copied().unwrap_or_default() > 0);
        if std::env::var_os("AC3_BRIEFING_SCENE_WRITE_REPORT").is_some() {
            std::fs::write(root.join(format!("analysis/menu-runtime/{fixture}/native-briefing-ui-{}.json",std::env::consts::OS)),serde_json::to_vec_pretty(&serde_json::json!({
                "passed":true,"platform":std::env::consts::OS,"calls":counts,
                "fixture_sha256":ac3_port::sha256(text.as_bytes()),"native_source_sha256":ac3_port::sha256(include_bytes!("../src/briefing_scene.rs")),
                "scope":"Original leaf state inputs; native asset-resolved text, connector and minimap packets; exact payloads excluding DMA links and unused FT4 UV halves."
            })).unwrap()).unwrap();
        }
    }
}
fn half(b: &[u8], at: usize) -> i16 {
    i16::from_le_bytes(b[at..at + 2].try_into().unwrap())
}
fn word(b: &[u8], at: usize) -> i32 {
    i32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
#[test]
#[ignore = "requires original assets and independent briefing draw capture"]
fn original_briefing_camera_objects() {
    let fixtures = std::env::var("AC3_BRIEFING_SCENE_FIXTURE")
        .map(|s| vec![s])
        .unwrap_or_else(|_| {
            vec![
                "briefing-scene-001".to_owned(),
                "briefing-scene-fast-001".to_owned(),
            ]
        });
    for fixture in fixtures {
        validate_fixture(&fixture);
    }
}
fn validate_fixture(fixture: &str) {
    let root = std::path::PathBuf::from(std::env::var("AC3_DATA_DIR").unwrap());
    let data = ac3_port::assets::GameData::load(&root.join("extracted/disc")).unwrap();
    let renderer = BriefingScene::new(&data.executable.payload).unwrap();
    let mission = ac3_formats::dot1::parse(data.resource(74).unwrap()).unwrap();
    let terrain =
        BriefingTerrain::new(mission.child(0).unwrap(), data.overlay(2).unwrap()).unwrap();
    let fonts = ac3_port::menu_scene::LanguageScene::from_mission(
        &data,
        ac3_port::mission_assets::MissionAssets::load(&data, 74).unwrap(),
    )
    .unwrap();
    let text = std::fs::read_to_string(root.join(format!(
        "analysis/menu-runtime/{fixture}/briefing-scene.jsonl"
    )))
    .unwrap();
    let (mut cameras, mut objects, mut scenes, mut packets, mut terrain_calls) = (0, 0, 0, 0, 0);
    let (mut scene_packets, mut scene_words) = (0, 0);
    for line in text.lines() {
        let c: Value = serde_json::from_str(line).unwrap();
        let kind = c["kind"].as_str().unwrap();
        let input = bytes(&c, "input_state_hex");
        let expected = bytes(&c, "expected_state_hex");
        let mut state =
            FullBriefingState::from_bytes(&input, c["script_base"].as_u64().unwrap() as u32)
                .unwrap();
        if kind == "camera" {
            let actual = renderer.camera(&state).unwrap();
            let rotation = std::array::from_fn(|r| {
                std::array::from_fn(|j| half(&expected, 0x554 + (r * 3 + j) * 2))
            });
            let translation = std::array::from_fn(|i| word(&expected, 0x568 + i * 4));
            assert_eq!(
                actual.matrix,
                mission_map::Matrix {
                    rotation,
                    translation
                },
                "camera frame {}",
                c["frame"]
            );
            assert_eq!(
                actual.projection.offset,
                [
                    c["expected_controls"][24].as_u64().unwrap() as i32,
                    c["expected_controls"][25].as_u64().unwrap() as i32
                ]
            );
            assert_eq!(
                u64::from(actual.projection.distance),
                c["expected_controls"][26].as_u64().unwrap()
            );
            cameras += 1;
        } else if kind == "terrain" {
            let actual = terrain
                .draw(&state, renderer.camera(&state).unwrap())
                .unwrap();
            let raw = bytes(&c, "packet_hex");
            let words: Vec<u32> = raw
                .chunks_exact(4)
                .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
                .collect();
            let mut expected = Vec::new();
            let mut at = 0;
            while at < words.len() {
                let n = (words[at] >> 24) as usize;
                expected.push(words[at + 1..at + 1 + n].to_vec());
                at += n + 1;
            }
            assert_eq!(actual, expected, "terrain frame {}", c["frame"]);
            terrain_calls += 1;
        } else if kind == "objects" || kind == "scene" {
            let result = renderer.draw_objects(&state).unwrap();
            state.script = apply_briefing_draw_outcomes(state.script, &result.outcomes).unwrap();
            let mut actual_bytes = input[..FullBriefingState::BYTE_LEN].to_vec();
            state
                .write_into(&mut actual_bytes, c["script_base"].as_u64().unwrap() as u32)
                .unwrap();
            assert_eq!(
                actual_bytes,
                expected[..FullBriefingState::BYTE_LEN],
                "{kind} complete state bytes, frame {}",
                c["frame"]
            );
            assert_eq!(
                state,
                FullBriefingState::from_bytes(&expected, c["script_base"].as_u64().unwrap() as u32)
                    .unwrap(),
                "{} state frame {}",
                kind,
                c["frame"]
            );
            if kind == "objects" {
                let raw = bytes(&c, "packet_hex");
                let words: Vec<u32> = raw
                    .chunks_exact(4)
                    .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
                    .collect();
                let mut wanted = Vec::new();
                let mut at = 0;
                while at < words.len() {
                    let n = (words[at] >> 24) as usize;
                    assert_eq!(n, 9);
                    let mut p: [u32; 9] = words[at + 1..at + 10].try_into().unwrap();
                    p[6] &= 65535;
                    p[8] &= 65535;
                    wanted.push(p);
                    at += 10;
                }
                assert_eq!(result.packets, wanted, "objects frame {}", c["frame"]);
                objects += 1;
                packets += wanted.len();
            } else {
                let original = FullBriefingState::from_bytes(
                    &input,
                    c["script_base"].as_u64().unwrap() as u32,
                )
                .unwrap();
                let draw = renderer.draw(&original, &terrain).unwrap();
                let mut actual = draw.terrain_packets;
                if let Some(marker) = draw.marker {
                    let mut text = marker.text.into_bytes();
                    text.push(0);
                    fonts
                        .font_text(
                            u16::from(marker.font),
                            &text,
                            marker.position[0] as i16,
                            marker.position[1] as i16,
                            &mut actual,
                        )
                        .unwrap();
                }
                actual.extend(draw.objects.packets.iter().map(|p| p.to_vec()));
                let raw = bytes(&c, "packet_hex");
                let words: Vec<u32> = raw
                    .chunks_exact(4)
                    .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
                    .collect();
                let mut expected = Vec::new();
                let mut at = 0;
                while at < words.len() {
                    let n = (words[at] >> 24) as usize;
                    let mut p = words[at + 1..at + 1 + n].to_vec();
                    if p.first().is_some_and(|x| x >> 24 == 0x2c) {
                        p[6] &= 65535;
                        p[8] &= 65535;
                    }
                    expected.push(p);
                    at += n + 1;
                }
                assert_eq!(
                    actual, expected,
                    "full DrawScene packets frame {}",
                    c["frame"]
                );
                scene_packets += actual.len();
                scene_words += actual.iter().map(Vec::len).sum::<usize>();
                scenes += 1;
            }
        }
    }
    eprintln!(
        "{fixture}: {cameras} camera matrices, {objects} object draws, {scenes} scene mutations, {packets} object packets, {terrain_calls} terrain calls exact"
    );
    assert!(scenes > 0 && cameras > 0 && packets > 0);
    assert_eq!(cameras, objects);
    assert_eq!(terrain_calls, cameras);
    if std::env::var_os("AC3_BRIEFING_SCENE_WRITE_REPORT").is_some() {
        let report = serde_json::json!({
            "fixture": fixture,
            "platform": std::env::consts::OS,
            "native_source_sha256": ac3_port::sha256(include_bytes!("../src/briefing_scene.rs")),
            "fixture_sha256": ac3_port::sha256(text.as_bytes()),
            "camera_matrices": cameras,
            "terrain_calls": terrain_calls,
            "object_calls": objects,
            "object_packets": packets,
            "scene_calls": scenes,
            "scene_packets": scene_packets,
            "scene_gp0_words": scene_words,
            "state_bytes_per_call": FullBriefingState::BYTE_LEN,
            "match": true,
            "normalization": "DMA links omitted; unused upper halves of FT4 UV2/UV3 zeroed",
            "input_scope": "Original callback state; asset height map, palette, fonts and native camera math",
            "excluded": ["pixel rasterization", "whole-wrapper temporal integration", "drawing outside ED834"]
        });
        std::fs::write(
            root.join(format!(
                "analysis/menu-runtime/{fixture}/native-briefing-scene-{}.json",
                std::env::consts::OS
            )),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
    }
}

#[test]
#[ignore = "requires original assets and whole briefing callback captures"]
fn original_briefing_control_with_native_draw_outcomes() {
    use ac3_mission::{
        BriefingFrameInput, BriefingScript, finish_briefing_render, initialize_briefing,
        prepare_briefing_tick,
    };
    let root = std::path::PathBuf::from(std::env::var("AC3_DATA_DIR").unwrap());
    let data = ac3_port::assets::GameData::load(&root.join("extracted/disc")).unwrap();
    let renderer = BriefingRenderer::new(&data, 74).unwrap();
    let math = ac3_port::mission_math::NativeBriefingMath::load(&data).unwrap();
    let mission = ac3_formats::dot1::parse(data.resource(74).unwrap()).unwrap();
    let script = BriefingScript::parse(mission.child(1).unwrap()).unwrap();
    let fonts = ac3_port::menu_scene::LanguageScene::from_mission(
        &data,
        ac3_port::mission_assets::MissionAssets::load(&data, 74).unwrap(),
    )
    .unwrap();
    for fixture in [
        "briefing-wrapper-normal-002",
        "briefing-wrapper-fast-001",
        "briefing-wrapper-confirm-001",
        "briefing-wrapper-early-cancel-001",
    ] {
        let text = std::fs::read_to_string(root.join(format!(
            "analysis/menu-runtime/{fixture}/briefing-wrapper-vectors.jsonl"
        )))
        .unwrap();
        let mut visual = fonts.visual_state().unwrap();
        // Isolated compositor inputs retain the previous hub's style bytes.
        // Production receives the continuously maintained native visual state.
        let checkpoint =
            std::fs::read(root.join("analysis/menu-runtime/briefing-ui-001/checkpoint.ram.bin"))
                .unwrap();
        let visual_address = word(&checkpoint, 0xf9e14) as usize & 0x1fffff;
        visual
            .apply_mission_visuals(
                &(0..128)
                    .map(|slot| ac3_mission::Effect::SetVisualStyle {
                        slot,
                        style: u32::from(checkpoint[visual_address + 0x4a4 + slot as usize]),
                    })
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        visual
            .apply_mission_visuals(
                &(0..32)
                    .map(|slot| ac3_mission::Effect::SetVisualOffset {
                        slot,
                        x: i32::from(half(
                            &checkpoint,
                            visual_address + 0x298 + slot as usize * 4,
                        )),
                        y: i32::from(half(
                            &checkpoint,
                            visual_address + 0x29a + slot as usize * 4,
                        )),
                    })
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        let mut cases = 0;
        let references = callback_references(&root, fixture);
        let mut render_index = 0;
        let mut callback_visual = fonts.visual_state().unwrap();
        for line in text.lines() {
            let c: Value = serde_json::from_str(line).unwrap();
            let input = bytes(&c, "input_state_hex");
            let base = c["script_base"].as_u64().unwrap() as u32;
            let update = if c["kind"] == "init" {
                initialize_briefing(
                    FullBriefingState::from_bytes_for_initialization(&input).unwrap(),
                    &script,
                    renderer.ui.text_line_count(0).unwrap() as i32,
                    &math,
                )
                .unwrap()
            } else {
                let offset = bytes(&c, "visual_offset2_hex");
                prepare_briefing_tick(
                    FullBriefingState::from_bytes(&input, base).unwrap(),
                    &script,
                    BriefingFrameInput {
                        pressed: c["pressed"].as_u64().unwrap() as u32,
                        visual_address: c["visual_address"].as_u64().unwrap() as u32,
                        visual_offset2: [half(&offset, 0), half(&offset, 2)],
                    },
                    &math,
                )
                .unwrap()
            };
            // Outer CBB88 sets this from held input before the nested callback.
            // The normal capture holds no buttons during the checked frame range.
            if fixture == "briefing-wrapper-normal-002" {
                visual
                    .apply_mission_visuals(&[ac3_mission::Effect::SetVisualStyle {
                        slot: 120,
                        style: 0,
                    }])
                    .unwrap();
            }
            if update.render_pending
                && let Some(rows) = &references
            {
                let expected = &rows[render_index];
                render_index += 1;
                let callback = renderer
                    .callback_commands(&fonts, &mut callback_visual, &update, 0, 1)
                    .unwrap();
                let mut before = input.clone();
                update.state.write_into(&mut before, base).unwrap();
                assert_eq!(before, expected.input, "{fixture} original draw boundary");
                assert_eq!(
                    callback.outer_zero, expected.outer_zero,
                    "{fixture} outer0 callback {}",
                    c["frame"]
                );
                assert_eq!(
                    callback.outer_one, expected.outer_one,
                    "{fixture} outer1 callback {}",
                    c["frame"]
                );
                assert_eq!(
                    callback.menu_bins[5], expected.menu_five,
                    "{fixture} menu5 callback {}",
                    c["frame"]
                );
            }
            let reference = composition_reference(&root, fixture, c["frame"].as_u64().unwrap());
            if let Some((_, expected)) = &reference {
                let color = expected[0][0];
                visual.set_frame_color([color as u8, (color >> 8) as u8, (color >> 16) as u8]);
            }
            let commands = renderer
                .commands(&fonts, &mut visual, &update, 0, 1)
                .unwrap();
            if let Some((file, expected)) = reference {
                let native: Vec<_> = commands
                    .execution_order()
                    .into_iter()
                    .map(normalize_packet)
                    .collect();
                assert_eq!(
                    native.len() + 3,
                    expected.len(),
                    "{file} native geometry and trailing engine packets"
                );
                for (i, (actual, original)) in native.iter().zip(&expected).enumerate() {
                    assert_eq!(actual, original, "{file} native packet {i}");
                }
                eprintln!(
                    "{file}: complete briefing subsequence {} packets exact",
                    native.len()
                );
            }
            let actual = finish_briefing_render(update, &commands.outcomes).unwrap();
            let mut output = input.clone();
            actual.state.write_into(&mut output, base).unwrap();
            assert_eq!(
                output,
                bytes(&c, "expected_state_hex"),
                "{fixture} frame {} state",
                c["frame"]
            );
            assert_eq!(
                actual.callback_result,
                c["callback_result"].as_u64().unwrap() as u32
            );
            assert!(!actual.render_pending);
            cases += 1;
        }
        eprintln!("{fixture}: {cases} whole callbacks exact with native draw outcomes and math");
        if let Some(rows) = &references {
            assert_eq!(render_index, rows.len());
            eprintln!("{fixture}: all {render_index} callback-owned packet streams exact");
        }
        assert!(cases > 0);
        if std::env::var_os("AC3_BRIEFING_SCENE_WRITE_REPORT").is_some() {
            std::fs::write(root.join(format!("analysis/menu-runtime/{fixture}/native-briefing-render-{}.json",std::env::consts::OS)),serde_json::to_vec_pretty(&serde_json::json!({
                "passed":true,"platform":std::env::consts::OS,"callback_cases":cases,"state_bytes_per_case":FullBriefingState::BYTE_LEN,
                "complete_callback_packet_streams":render_index,
                "composed_subsequence_packets":if fixture=="briefing-wrapper-normal-002" {vec![186,479,505]}else{Vec::<i32>::new()},
                "fixture_sha256":ac3_port::sha256(text.as_bytes()),"native_source_sha256":ac3_port::sha256(include_bytes!("../src/briefing_scene.rs")),
                "scope":"Original callback input state; all control, draw outcomes and math native. Separate compositor boundary retains observed initial hub styles/offsets and observed frame color; continuous initialization has its own test."
            })).unwrap()).unwrap();
        }
    }
}
