use ac3_port::assets::GameData;
use ac3_port::flight_loader::*;
use ac3_port::mission_map::Random;
use serde_json::Value;
use std::{collections::BTreeMap, path::PathBuf};

fn bytes(v: &Value) -> Vec<u8> {
    let s = v.as_str().unwrap();
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}
fn word(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn half(b: &[u8], o: usize) -> i16 {
    i16::from_le_bytes(b[o..o + 2].try_into().unwrap())
}
fn context(b: &[u8]) -> FlightLoadingContext {
    FlightLoadingContext {
        mission: half(b, 0x390),
        aircraft: half(b, 0x392),
        language: word(b, 0x38c),
        difficulty: word(b, 0x398),
        frame_clock: word(b, 0x20),
        replay: b[0x3a9],
        replay_index: b[0x3aa],
        restart_count: half(b, 0x3b0) as u16,
        exit_reason: b[0x95b],
        status: half(b, 0x95c) as u16,
    }
}
fn render(b: &[u8]) -> LoadingRenderStatus {
    LoadingRenderStatus {
        flags: word(b, 0),
        phase: word(b, 4),
        background: b[16..20].try_into().unwrap(),
        parameter: word(b, 20),
    }
}
fn packets(v: &Value) -> Vec<Vec<u32>> {
    let b = bytes(v);
    let mut o = 0;
    let mut out = vec![];
    while o < b.len() {
        let count = (word(&b, o) >> 24) as usize;
        assert!(count > 0);
        out.push((0..count).map(|i| word(&b, o + 4 + i * 4)).collect());
        o += (count + 1) * 4;
    }
    assert_eq!(o, b.len());
    out
}
fn num(v: &Value, key: &str) -> u32 {
    v[key].as_u64().unwrap() as u32
}
fn observation(c: &Value) -> FlightLoadObservation {
    FlightLoadObservation {
        active_resource: c["resource"].as_u64().unwrap_or(u64::from(u32::MAX)) as u32 as i32,
        progress: c["progress"].as_u64().unwrap_or(0) as i16,
        completed: false,
    }
}

#[test]
fn loading_noise_repeats_original_rng_and_chain_order() {
    let mut random = Random { state: 0 };
    let p = FlightLoader::aperture([0, 0, 33, 49], 4096, &mut random).unwrap();
    assert_eq!(p.len(), 6);
    assert_eq!(p[1][0], 0x64838383);
    assert_eq!(p[1][2], 0x46d60000);
    assert_eq!(p[2][3], 0x00300001);
    assert_eq!(p[4][3], 0x00010001);
    let mut out = FlightLoadingCommands::default();
    out.groups.push(p.clone());
    out.groups.push(FlightLoader::fade(0));
    assert_eq!(out.execution_order()[0], vec![0xe6000001, 0]);
    assert_eq!(&out.execution_order()[4..], p);
}

#[test]
fn loading_context_bridge_preserves_other_fields() {
    let mut bytes = vec![0xa5; 0xa05];
    let original = bytes.clone();
    let mut context = FlightLoadingContext::from_bytes(&bytes).unwrap();
    context.exit_reason = 0;
    context.status = 0;
    context.restart_count = 0;
    context.write_into(&mut bytes).unwrap();
    for (i, (before, after)) in original.iter().zip(&bytes).enumerate() {
        if !matches!(i,0x3b0..=0x3b1|0x95b..=0x95d) {
            assert_eq!(before, after);
        }
    }
    assert_eq!(FlightLoadingContext::from_bytes(&bytes).unwrap(), context);
    assert!(context.write_into(&mut bytes[..100]).is_err());
}

#[test]
#[ignore = "requires original assets and natural RNG initialization capture"]
fn original_inactive_trails_preserve_rng_initialization() {
    let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").unwrap());
    let data = GameData::load(&root.join("extracted/disc")).unwrap();
    let assets = ac3_port::mission_assets::MissionAssets::load(&data, 74).unwrap();
    let mut random = Random { state: 0 };
    ac3_port::mission_map::MapRenderer::new(
        &data.executable.payload,
        data.overlay(2).unwrap(),
        &assets.vram,
        &mut random,
    )
    .unwrap();
    let rows: Vec<Value> =
        std::fs::read_to_string(root.join("analysis/flight-runtime/rng-002/random.jsonl"))
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
    let original = rows
        .iter()
        .find(|c| c["kind"] == "map_objects_return")
        .unwrap();
    assert_eq!(random.state, num(original, "seed"));
    let trails = initialize_inactive_trails(&mut random);
    let original = rows
        .iter()
        .find(|c| c["kind"] == "scene_init_return")
        .unwrap();
    assert_eq!(random.state, num(original, "seed"));
    for c in rows
        .iter()
        .filter(|c| c["kind"] == "alternate_object_return")
    {
        let slot = num(c, "slot") as usize;
        let positions = bytes(&c["positions_hex"]);
        let angles = bytes(&c["angles_hex"]);
        for i in 0..15 {
            for axis in 0..3 {
                assert_eq!(
                    trails[slot].positions[i][axis],
                    word(&positions, i * 16 + axis * 4) as i32
                );
                assert_eq!(
                    trails[slot].angles[i][axis],
                    half(&angles, i * 8 + axis * 2)
                );
            }
        }
    }
}

#[test]
#[ignore = "requires original loading progress, OTs and screenshots"]
fn continuous_native_loading_packets_and_pixels() {
    let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").unwrap());
    let data = GameData::load(&root.join("extracted/disc")).unwrap();
    let assets = ac3_port::mission_assets::MissionAssets::load(&data, 74).unwrap();
    let mut random = Random { state: 0 };
    ac3_port::mission_map::MapRenderer::new(
        &data.executable.payload,
        data.overlay(2).unwrap(),
        &assets.vram,
        &mut random,
    )
    .unwrap();
    let _trails = initialize_inactive_trails(&mut random);
    let mut vram = assets.vram;
    let loader = FlightLoader::new(&data.executable.payload).unwrap();
    let mut state = FlightLoadingState::default();
    let mut render = LoadingRenderStatus::default();
    let mut context = FlightLoadingContext {
        mission: 1,
        aircraft: 1,
        difficulty: 1,
        ..Default::default()
    };
    let capture = root.join("analysis/flight-runtime/loading-002");
    let rows: Vec<Value> = std::fs::read_to_string(capture.join("flight-loader.jsonl"))
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    let mut polls: Vec<_> = rows
        .iter()
        .enumerate()
        .filter(|(_, c)| c["kind"] == "poll")
        .collect();
    polls.sort_by_key(|(_, c)| num(c, "id"));
    let mut completion_frames: Vec<_> = rows
        .iter()
        .enumerate()
        .filter(|(_, c)| c["kind"] == "completion")
        .map(|(i, _)| i)
        .collect();
    completion_frames.sort();
    let mut next_completion = 0;
    let mut reports = vec![];
    for (capture_index, c) in polls {
        let frame = num(c, "frame");
        // The clock is an explicit external input. Long synchronous decode calls
        // retain their entry clock while Vsync advances, until the next callback.
        context.frame_clock = word(&bytes(&c["input"]["context_hex"]), 0x20);
        let mut io = observation(c);
        if next_completion < completion_frames.len()
            && completion_frames[next_completion] < capture_index
        {
            io.completed = true;
            next_completion += 1;
        }
        let update = if frame == 82 {
            loader.initialize(&mut state, &mut context, &mut render, io, &mut random)
        } else if frame == 281 {
            loader.queue_aircraft(&mut state, &context, &mut render, io, &mut random)
        } else {
            loader.poll(&mut state, &context, &mut render, io, &mut random)
        }
        .unwrap();
        assert_eq!(
            state.to_bytes().as_slice(),
            bytes(&c["expected"]["state_hex"]),
            "continuous state frame{frame}"
        );
        assert_eq!(
            random.state,
            num(&c["expected"], "random"),
            "continuous RNG frame{frame}"
        );
        let shot = match frame {
            98 => 101,
            99 => 102,
            118 => 121,
            198 => 201,
            304 => 307,
            _ => continue,
        };
        let packets = update.commands.execution_order();
        let ot_path = std::fs::read_dir(&capture)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(&format!("ot-{:05}-", shot - 2))
            })
            .unwrap();
        let ot: Value = serde_json::from_slice(&std::fs::read(&ot_path).unwrap()).unwrap();
        let original: Vec<Vec<u32>> = ot["packets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                p["words"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|w| w.as_u64().unwrap() as u32)
                    .collect()
            })
            .collect();
        assert_eq!(
            &original[..packets.len()],
            packets,
            "continuous packet order frame{frame}"
        );
        let mut image = ac3_graphics::Frame::new(320, 256).unwrap();
        let rgb = render.clear_rgb();
        image.clear(
            u16::from(rgb[0] >> 3) | (u16::from(rgb[1] >> 3) << 5) | (u16::from(rgb[2] >> 3) << 10),
        );
        ac3_graphics::render_packets_with_transfers(&mut image, &mut vram, &packets).unwrap();
        let actual: Vec<u8> = image.words().iter().flat_map(|w| w.to_le_bytes()).collect();
        let expected = std::fs::read(capture.join(format!("frame-{shot:05}.screen.raw"))).unwrap();
        assert_eq!(actual.len(), expected.len());
        let differences = actual
            .chunks_exact(2)
            .zip(expected.chunks_exact(2))
            .filter(|(a, b)| a != b)
            .count();
        ac3_port::frontend::save_frame(
            &image,
            &root.join(format!("analysis/native-flight/loading-{shot}")),
        )
        .unwrap();
        assert_eq!(differences, 0, "native loading pixels screen{shot}");
        reports.push(serde_json::json!({"callback":frame,"screenshot":shot,"ot":ot_path.file_name().unwrap().to_string_lossy(),"packets":packets.len(),"pixels":image.words().len(),"different_pixels":differences,"sha256":ac3_port::sha256(&actual)}));
    }
    assert_eq!(reports.len(), 5);
    std::fs::create_dir_all(root.join("analysis/native-flight")).unwrap();
    std::fs::write(root.join(format!("analysis/native-flight/loading-continuous-{}.json",std::env::consts::OS)),serde_json::to_vec_pretty(&serde_json::json!({"passed":true,"platform":std::env::consts::OS,"frames":reports,"scope":"Native default loading state, original asset textures/tables, seed0 map+inactive-trail initialization, then continuous native callbacks/RNG/packets. Original resource progress/completion, frame clock and callback schedule are explicit external inputs. No captured state/VRAM/commands seed generation; mission/aircraft resource preparation remains separate."})).unwrap()).unwrap();
}

#[test]
#[ignore = "requires original resident and Linux runtime fixture"]
fn original_flight_loading_control_and_packets() {
    let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR"));
    let data = GameData::load(&root.join("extracted/disc")).unwrap();
    let loader = FlightLoader::new(&data.executable.payload).unwrap();
    let path = root.join("analysis/flight-runtime/loading-002/flight-loader.jsonl");
    let rows: Vec<Value> = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let mut counts = BTreeMap::<String, usize>::new();
    let mut packet_count = 0;
    let mut word_count = 0;
    for c in &rows {
        let kind = c["kind"].as_str().unwrap();
        if matches!(kind, "draw_env" | "completion") {
            continue;
        }
        let input = &c["input"];
        let expected = &c["expected"];
        let mut state = FlightLoadingState::from_bytes(&bytes(&input["state_hex"])).unwrap();
        let mut ctx = context(&bytes(&input["context_hex"]));
        let mut gpu = render(&bytes(&input["gpu_hex"]));
        let mut random = Random {
            state: num(input, "random"),
        };
        let args: Vec<u32> = c["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap() as u32)
            .collect();
        let mut commands = FlightLoadingCommands::default();
        let mut result = None;
        match kind {
            "initialize" | "queue_aircraft" => {
                let poll = rows
                    .iter()
                    .filter(|p| {
                        p["kind"] == "poll"
                            && num(p, "id") > num(c, "id")
                            && num(p, "frame") == num(c, "frame")
                    })
                    .min_by_key(|p| num(p, "id"))
                    .unwrap();
                let update = if kind == "initialize" {
                    loader.initialize(
                        &mut state,
                        &mut ctx,
                        &mut gpu,
                        observation(poll),
                        &mut random,
                    )
                } else {
                    loader.queue_aircraft(
                        &mut state,
                        &ctx,
                        &mut gpu,
                        observation(poll),
                        &mut random,
                    )
                }
                .unwrap();
                commands = update.commands;
                result = Some(update.callback_result);
                assert!(
                    update
                        .effects
                        .iter()
                        .any(|e| matches!(e, FlightLoadingEffect::QueueResource { .. }))
                );
            }
            "poll" => {
                let update = loader
                    .poll(&mut state, &ctx, &mut gpu, observation(c), &mut random)
                    .unwrap();
                commands = update.commands;
                result = Some(update.callback_result);
            }
            "visual_initialize" | "mode1_initialize" => {
                let saved = state.visual_status;
                FlightLoader::visual_initialize(&mut state, &mut gpu).unwrap();
                if kind == "mode1_initialize" {
                    state.visual_status = saved;
                }
            }
            "mode1_draw" | "visual_draw" => {
                commands = loader.mode1_draw(&mut state, &ctx, &mut random).unwrap()
            }
            "text" => commands = loader.text(&state, &ctx).unwrap(),
            "fade" => commands.groups.push(FlightLoader::fade(args[2] as i32)),
            "aperture" => {
                let b = bytes(&c["rect_hex"]);
                let rect = std::array::from_fn(|i| half(&b, i * 2));
                if let Some(g) = FlightLoader::aperture(rect, args[3] as i32, &mut random) {
                    commands.groups.push(g);
                }
            }
            "noise_tiles" => {
                let rect = [
                    args[2] as i16,
                    (args[2] >> 16) as i16,
                    args[3] as i16,
                    (args[3] >> 16) as i16,
                ];
                commands.groups.push(FlightLoader::noise_tiles(
                    rect,
                    args[1] as u16,
                    num(c, "rgb"),
                    num(c, "ir0") as i16,
                    &mut random,
                ));
            }
            "fixed_font" | "variable_font" => {
                let mut text = bytes(&c["text_hex"]);
                assert_eq!(text.pop(), Some(0));
                if kind == "fixed_font" {
                    FlightLoader::fixed_font(
                        &text,
                        [args[2] as i16, args[3] as i16],
                        args[4],
                        &mut commands,
                    );
                } else {
                    loader
                        .variable_font(
                            &text,
                            [args[2] as i16, args[3] as i16],
                            args[4],
                            &mut commands,
                        )
                        .unwrap();
                }
            }
            "difficulty" => loader
                .difficulty(&ctx, args[2] as i16, args[3] as i16, &mut commands)
                .unwrap(),
            _ => panic!("unknown capture {kind}"),
        }
        let label = format!("{kind} frame{} id{}", c["frame"], c["id"]);
        assert_eq!(
            state.to_bytes().as_slice(),
            bytes(&expected["state_hex"]),
            "state {label}"
        );
        assert_eq!(
            ctx,
            context(&bytes(&expected["context_hex"])),
            "context {label}"
        );
        assert_eq!(
            gpu,
            render(&bytes(&expected["gpu_hex"])),
            "render status {label}"
        );
        assert_eq!(random.state, num(expected, "random"), "random {label}");
        if let Some(result) = result {
            assert_eq!(result, num(c, "result"), "return {label}");
        }
        if c.get("packet_hex").is_some() {
            let original = packets(&c["packet_hex"]);
            let generated = commands.emission_order();
            assert_eq!(generated, original, "packets {label}");
            packet_count += generated.len();
            word_count += generated.iter().map(Vec::len).sum::<usize>();
        }
        *counts.entry(kind.into()).or_default() += 1;
    }
    println!("flight loader {counts:?}; {packet_count} packets, {word_count} words exact");
    if std::env::var_os("AC3_FLIGHT_LOADER_WRITE_REPORT").is_some() {
        let report = serde_json::json!({"passed":true,"platform":std::env::consts::OS,
        "observations":counts,"packets":packet_count,"words":word_count,"source_sha256":ac3_port::sha256(include_bytes!("../src/flight_loader.rs")),
        "fixture_sha256":ac3_port::sha256(&std::fs::read(path).unwrap()),"scope":"Original state60 bytes, modeled context/render fields, RNG, callback returns and generated packet payloads. I/O progress/completion are explicit captured observations; no flight simulation."});
        std::fs::write(
            root.join(format!(
                "analysis/flight-runtime/loading-002/native-flight-loader-{}.json",
                std::env::consts::OS
            )),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
    }
}
