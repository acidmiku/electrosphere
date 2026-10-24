//! Native window and deterministic batch driver. No emulator/BIOS is used.
use crate::flight_runtime::FlightRuntime;
use crate::mission_audio::MissionAudioEvent;
use crate::mission_runtime::MissionRuntime;
use crate::{Result, assets::GameData, menu_audio::MenuAudio, menu_runtime::MenuRuntime, sha256};
use ac3_graphics::Frame;
use minifb::{Key, KeyRepeat, ScaleMode, Window, WindowOptions};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, io::BufWriter, path::Path};

pub fn save_frame(frame: &Frame, directory: &Path) -> Result<()> {
    fs::create_dir_all(directory)?;
    let raw: Vec<u8> = frame.words().iter().flat_map(|w| w.to_le_bytes()).collect();
    fs::write(directory.join("frame.raw"), raw)?;
    let writer = BufWriter::new(fs::File::create(directory.join("frame.png"))?);
    let mut encoder =
        png::Encoder::new(writer, u32::from(frame.width()), u32::from(frame.height()));
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&frame.rgba8())?;
    Ok(())
}

/// A frame-indexed processed-input sequence, e.g. `40:down,70:up,90:confirm`.
pub fn parse_inputs(text: &str) -> Result<BTreeMap<u32, u32>> {
    let mut result = BTreeMap::new();
    for item in text.split(',').filter(|s| !s.is_empty()) {
        let (frame, keys) = item.split_once(':').ok_or("input needs frame:button")?;
        let frame = frame.parse::<u32>()?;
        if frame == 0 {
            return Err("input frames start at 1".into());
        }
        let mut bits = 0;
        for key in keys.split('+') {
            bits |= match key {
                "up" => 1,
                "down" => 2,
                "left" => 4,
                "right" => 8,
                "confirm" => 0x10,
                "cancel" => 0x20,
                "start" => 0x40,
                _ => return Err(format!("unknown input {key}").into()),
            };
        }
        *result.entry(frame).or_default() |= bits;
    }
    Ok(result)
}

pub fn run_menu(
    data: &GameData,
    headless: bool,
    max_frames: Option<u32>,
    inputs: &str,
    output: Option<&Path>,
) -> Result<Value> {
    let inputs = parse_inputs(inputs)?;
    let mut runtime = MenuRuntime::new(data)?;
    let mut mission: Option<MissionRuntime<'_>> = None;
    let mut flight: Option<FlightRuntime<'_>> = None;
    let mut audio = MenuAudio::new(data, headless)?;
    let mut sound_events = Vec::new();
    let mut window = if headless {
        None
    } else {
        let mut window = Window::new(
            "Ace Combat 3 — Rust",
            640,
            480,
            WindowOptions {
                resize: true,
                scale_mode: ScaleMode::Stretch,
                ..WindowOptions::default()
            },
        )?;
        window.set_target_fps(50);
        Some(window)
    };
    let limit = max_frames.unwrap_or(if headless { 120 } else { u32::MAX });
    if limit == 0 {
        return Err("--frames must be positive".into());
    }
    let mut trace = Vec::new();
    let mut final_frame = None;
    let mut status = "frame_limit";
    for frame_number in 1..=limit {
        let mut bits = inputs.get(&frame_number).copied().unwrap_or(0);
        if let Some(window) = &window {
            if !window.is_open() || window.is_key_down(Key::Escape) {
                status = "window_closed";
                break;
            }
            if window.is_key_pressed(Key::Up, KeyRepeat::Yes) {
                bits |= 1;
            }
            if window.is_key_pressed(Key::Down, KeyRepeat::Yes) {
                bits |= 2;
            }
            if window.is_key_pressed(Key::Left, KeyRepeat::Yes) {
                bits |= 4;
            }
            if window.is_key_pressed(Key::Right, KeyRepeat::Yes) {
                bits |= 8;
            }
            if window.is_key_pressed(Key::Enter, KeyRepeat::No)
                || window.is_key_pressed(Key::Space, KeyRepeat::No)
            {
                bits |= 0x50;
            }
            if window.is_key_pressed(Key::Backspace, KeyRepeat::No) {
                bits |= 0x20;
            }
        }
        let (image, mut step) = if let Some(flight) = &mut flight {
            flight.step(frame_number)?
        } else if let Some(mission) = &mut mission {
            mission.step(frame_number as i32, bits, bits)?
        } else {
            runtime.step(frame_number as i32, bits, bits)?
        };
        if step["load_mission_sound_bank"] == true {
            audio.load_mission_bank(data)?;
        }
        if step["reset_resident_voices"] == true {
            audio.stop_resident_voices();
        }
        let mut background_events = Vec::new();
        for (key, event) in [
            (
                "initialize_background_voices",
                MissionAudioEvent::InitBackgroundVoices,
            ),
            (
                "begin_hub_background_fade",
                MissionAudioEvent::BeginHubBackgroundFade,
            ),
            (
                "begin_launch_background_fade",
                MissionAudioEvent::BeginLaunchBackgroundFade,
            ),
            (
                "begin_briefing_background_fade",
                MissionAudioEvent::BeginBriefingBackgroundFade,
            ),
            (
                "resume_background_fast",
                MissionAudioEvent::ResumeBackgroundFast,
            ),
        ] {
            if step[key] == true {
                background_events.push(event);
            }
        }
        if let Some(background) = audio.background_tick(&background_events, output)? {
            step["background_audio"] = background;
        }
        if !step["stream_request"].is_null() {
            step["stream_request"] = audio.stream_request(&step["stream_request"])?;
        }
        if let Some(stream) = audio.stream_tick(output)? {
            step["stream_audio"] = stream;
        }
        for request in step["sound_requests"]
            .as_array()
            .ok_or("missing sound requests")?
        {
            let id = u16::try_from(request.as_u64().ok_or("invalid sound ID")?)?;
            let mut event = audio.request(id, output)?;
            event["frame"] = json!(frame_number);
            sound_events.push(event);
        }
        if let Some(window) = &mut window {
            let pixels: Vec<u32> = image
                .frame
                .rgba8()
                .chunks_exact(4)
                .map(|c| (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2]))
                .collect();
            window.update_with_buffer(
                &pixels,
                usize::from(image.frame.width()),
                usize::from(image.frame.height()),
            )?;
        }
        trace.push(step);
        final_frame = Some(image.frame);
        if let Some(flight) = &flight {
            if let Some(pending) = flight.pending {
                status = pending;
                break;
            }
        } else if let Some(mission) = &mission {
            if let Some(pending) = mission.pending {
                if pending == "mission_resident_transition_pending" {
                    flight = Some(FlightRuntime::from_mission(data, mission)?);
                } else {
                    status = pending;
                    break;
                }
            }
        } else if let Some(pending) = runtime.pending {
            if pending == "mission_transition_pending" {
                mission = Some(MissionRuntime::new(
                    data,
                    runtime.game_context_bytes()?,
                    usize::try_from(runtime.context.language_id)?,
                )?);
                runtime.pending = None;
            } else {
                status = pending;
                break;
            }
        }
    }
    let digest = if let Some(frame) = final_frame {
        if let Some(directory) = output {
            save_frame(&frame, directory)?;
        }
        Some(sha256(
            &frame
                .words()
                .iter()
                .flat_map(|w| w.to_le_bytes())
                .collect::<Vec<_>>(),
        ))
    } else {
        None
    };
    let result = json!({"status":status,"frames":trace.len(),"headless":headless,"language_id":runtime.context.language_id,
        "menu_state":runtime.menu.state,"mission_state":mission.as_ref().map(|m|m.state.state),
        "resident_state":flight.as_ref().map(|_|2),"resident_substate":flight.as_ref().map(|f|f.substate),"frame_sha256":digest,"trace":trace,
        "audio_device":audio.device_status,"sound_events":sound_events,"background_audio":audio.finish_background()?,"stream_audio":audio.finish_stream()?,
        "scope":"Native language/title/main/difficulty, resource-loading UI, first mission hub, briefing, aircraft/weapon selection, launch and mode1 resident mission/aircraft loading using original assets. Finite cues, five persistent background voices and briefing/aircraft XA music are connected; resident loading installs both new sound banks. Gameplay initialization, flight and saves remain under development. Host audio uses a nominal50-Hz block schedule without original CD latency or final SPU voice mixing; resident voice key-off stops host sources without reproducing release tails."});
    if let Some(directory) = output {
        fs::create_dir_all(directory)?;
        fs::write(
            directory.join("run.json"),
            serde_json::to_vec_pretty(&result)?,
        )?;
    }
    Ok(result)
}
