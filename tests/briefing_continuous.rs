//! Complete native hub -> briefing visual progression from original assets.
use ac3_mission::*;
use ac3_port::{
    assets::GameData,
    briefing_scene::BriefingRenderer,
    mission_assets::MissionAssets,
    mission_hub::{HubLayers, HubScene},
    mission_map::Random,
    mission_math::NativeBriefingMath,
};
use serde_json::{Value, json};
use std::{fs, path::PathBuf};

fn normalize(mut p: Vec<u32>) -> Vec<u32> {
    if matches!(p[0] >> 24, 0x2c | 0x2e) {
        p[6] &= 65535;
        p[8] &= 65535;
    }
    p
}

#[test]
#[ignore = "requires original assets, complete briefing OTs and screenshots"]
fn continuous_native_briefing_packets_and_pixels() {
    let root = PathBuf::from(std::env::var("AC3_DATA_DIR").unwrap());
    let data = GameData::load(&root.join("extracted/disc")).unwrap();
    let mut scene = HubScene::new(
        &data,
        MissionAssets::load(&data, 74).unwrap(),
        &mut Random { state: 0 },
    )
    .unwrap();
    // These are the independently established elapsed counts of mission-002's
    // checkpoint. No captured state, packets, textures or pixels seed generation.
    for _ in 0..187 {
        assert!(scene.map.tick([0; 3]).unwrap().emitted.is_empty());
    }
    scene.set_load_progress(712, 712).unwrap();
    let mut hub = initialize_hub(
        MissionState::default(),
        MissionContext {
            options: 0x42,
            current_pair: [1, 0],
            ..Default::default()
        },
    );
    scene.layers(&hub.effects, 0, 0, 0).unwrap();
    scene.map.tick(scene.color.rgb).unwrap();
    for elapsed in 1..305 {
        hub = tick_hub(hub.state, hub.mission, hub.context, 0).unwrap();
        scene.layers(&hub.effects, elapsed, 0, 0).unwrap();
        scene.map.tick(scene.color.rgb).unwrap();
    }
    assert_eq!(scene.map.state.frame, 492);
    let clock: Value = serde_json::from_slice(
        &fs::read(
            root.join("analysis/menu-runtime/hub-render-001/render-00000-callback-entry.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let clock_start = clock["context_frame"].as_i64().unwrap() as i32;
    let mut mission = MissionState {
        substate: 1,
        ..hub.mission
    };
    let mut context = hub.context;
    let renderer = BriefingRenderer::new(&data, 74).unwrap();
    let math = NativeBriefingMath::load(&data).unwrap();
    let mission_data = ac3_formats::dot1::parse(data.resource(74).unwrap()).unwrap();
    let script = BriefingScript::parse(mission_data.child(1).unwrap()).unwrap();
    let mut briefing = FullBriefingState::default();
    let mut reports = Vec::new();
    let mut init_frame = None;
    for frame in 0..=848 {
        // Original hub_vectors records Cross as pressed0x810 in callback2.
        // Physical press/release occurs at Vsync1/5; PAD polling adds one field.
        let pressed = if frame == 2 { 0x810 } else { 0 };
        let held = if (2..6).contains(&frame) { 0x811 } else { 1 };
        let layers = if mission.state == 0 {
            hub = tick_hub(hub.state, mission, context, pressed).unwrap();
            mission = hub.mission;
            context = hub.context;
            let post = apply_callback_result(mission, context, hub.callback_result).unwrap();
            mission = post.state;
            context = post.context;
            let mut effects = hub.effects.clone();
            effects.extend(post.effects);
            scene
                .layers(&effects, clock_start + frame, 0, held)
                .unwrap()
        } else {
            assert_eq!(
                mission.state, 1,
                "briefing progression at observer frame{frame}"
            );
            let clear_color = scene.begin_frame(held).unwrap();
            let update = if mission.substate == 0 {
                init_frame = Some(frame);
                initialize_briefing(
                    briefing,
                    &script,
                    renderer.ui.text_line_count(0).unwrap() as i32,
                    &math,
                )
                .unwrap()
            } else {
                prepare_briefing_tick(
                    briefing,
                    &script,
                    BriefingFrameInput {
                        pressed,
                        visual_address: 0,
                        visual_offset2: scene.visual.offset(2).unwrap(),
                    },
                    &math,
                )
                .unwrap()
            };
            scene.visual.set_frame_color(scene.color.rgb);
            let commands = renderer
                .commands(&scene.scene, &mut scene.visual, &update, 0, 1)
                .unwrap();
            let update = finish_briefing_render(update, &commands.outcomes).unwrap();
            briefing = update.state;
            let post = apply_callback_result(mission, context, update.callback_result).unwrap();
            mission = post.state;
            context = post.context;
            for effect in &commands.remaining_effects {
                match effect {
                    BriefingEffect::Common(Effect::PendingOriginalCall {
                        address: 0x800e983c,
                    }) => scene.map.set_object_phase(3).unwrap(),
                    BriefingEffect::Common(Effect::PendingOriginalCall {
                        address: 0x800e9824,
                    }) => scene.map.set_object_phase(1).unwrap(),
                    BriefingEffect::Common(
                        Effect::PlaySound { .. }
                        | Effect::StartStream { .. }
                        | Effect::StopStream
                        | Effect::PendingOriginalCallWithArgs {
                            address: 0x800e7a18,
                            ..
                        },
                    ) => (),
                    _ => panic!("unhandled visual effect {effect:?}"),
                }
            }
            assert!(post.effects.is_empty());
            HubLayers {
                packets: commands.execution_order(),
                clear_color,
                remaining_effects: Vec::new(),
            }
        };
        let depth = scene.map.tick(scene.color.rgb).unwrap();
        let depth: Vec<_> = depth
            .execution_order()
            .iter()
            .map(|p| p.words.clone())
            .collect();
        let target = match frame {
            98 => Some((100, "ot-00099-001.json")),
            698 => Some((700, "ot-00699-003.json")),
            848 => Some((850, "ot-00849-005.json")),
            _ => None,
        };
        if let Some((screenshot, ot)) = target {
            let image = scene.render(&layers, &depth).unwrap();
            let capture = root.join("analysis/menu-runtime/briefing-scene-001");
            let original: Value =
                serde_json::from_slice(&fs::read(capture.join(ot)).unwrap()).unwrap();
            let packets: Vec<Vec<u32>> = original["packets"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| {
                    normalize(
                        p["words"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|v| v.as_u64().unwrap() as u32)
                            .collect(),
                    )
                })
                .collect();
            assert_eq!(
                image.packets.len() + 3,
                packets.len(),
                "frame{frame} complete scene plus engine footer"
            );
            for (i, (a, b)) in image
                .packets
                .iter()
                .cloned()
                .map(normalize)
                .zip(&packets)
                .enumerate()
            {
                assert_eq!(&a, b, "frame{frame} native packet{i}");
            }
            let actual: Vec<u8> = image
                .frame
                .words()
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect();
            let expected =
                fs::read(capture.join(format!("frame-{screenshot:05}.screen.raw"))).unwrap();
            assert_eq!(actual.len(), expected.len());
            let differences = actual
                .chunks_exact(2)
                .zip(expected.chunks_exact(2))
                .filter(|(a, b)| a != b)
                .count();
            ac3_port::frontend::save_frame(
                &image.frame,
                &root.join(format!(
                    "analysis/native-mission/briefing-native-{screenshot}"
                )),
            )
            .unwrap();
            assert_eq!(differences, 0, "frame{frame} native pixel differences");
            reports.push(json!({"callback":frame,"screenshot":screenshot,"packets":image.packets.len(),"pixels":320*512,"different_pixels":differences,"sha256":ac3_port::sha256(&actual)}));
        }
    }
    assert_eq!(init_frame, Some(29));
    assert_eq!(reports.len(), 3);
    fs::write(root.join("analysis/native-mission/briefing-continuous-validation.json"),serde_json::to_vec_pretty(&json!({"passed":true,"platform":std::env::consts::OS,"frames":reports,"inactive_ticks":187,"hub_active_ticks_before_capture":305,"clock_start":clock_start,"briefing_init_observer_frame":init_frame,"scope":"Asset initialized hub, background, persistent visual state, briefing control and drawing; native math/outcomes; only elapsed counts, frame clock and controller inputs supplied. No captured state/VRAM/commands/pixels used for generation. Audio/CD timing excluded.","normalization":"DMA links and unused FT4 upper UV2/UV3 halves omitted; three resident housekeeping packets excluded."})).unwrap()).unwrap();
}
