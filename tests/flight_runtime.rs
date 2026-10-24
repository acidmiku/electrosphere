//! Connected host scheduling consumes only native state and original disc assets.
//! The captured upload payloads are independent expected output in this test.
use ac3_graphics::tim;
use ac3_port::{
    assets::GameData, flight_runtime::FlightRuntime, frontend::parse_inputs,
    menu_runtime::MenuRuntime, mission_runtime::MissionRuntime,
};
use std::{fs, path::PathBuf};

#[test]
#[ignore = "requires AC3_DATA_DIR and original mission-007 upload capture"]
fn connected_resident_preparation_installs_original_gpu_and_sound_data() {
    let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").unwrap());
    let data = GameData::load(&root.join("extracted/disc")).unwrap();
    let inputs = parse_inputs("40:confirm,110:start,160:confirm,200:down,220:confirm,390:right,410:left,450:confirm,650:start,750:confirm,850:confirm,950:confirm").unwrap();
    let mut menu = MenuRuntime::new(&data).unwrap();
    let mut mission: Option<MissionRuntime<'_>> = None;
    for frame in 1..=1029 {
        let bits = inputs.get(&frame).copied().unwrap_or(0);
        if let Some(mission) = &mut mission {
            mission.step(frame as i32, bits, bits).unwrap();
        } else {
            menu.step(frame as i32, bits, bits).unwrap();
            if menu.pending == Some("mission_transition_pending") {
                mission = Some(
                    MissionRuntime::new(
                        &data,
                        menu.game_context_bytes().unwrap(),
                        menu.context.language_id as usize,
                    )
                    .unwrap(),
                );
            }
        }
    }
    let mission = mission.unwrap();
    assert_eq!(mission.pending, Some("mission_resident_transition_pending"));
    assert_eq!(mission.random.state, 0x668a4953);
    let mut expected_vram = mission.scene.scene.vram.clone();
    let mut flight = FlightRuntime::from_mission(&data, &mission).unwrap();
    for frame in 1030..=1035 {
        let (image, trace) = flight.step(frame).unwrap();
        assert_eq!((image.frame.width(), image.frame.height()), (320, 256));
        assert_eq!(trace["unhandled_effects"].as_array().unwrap().len(), 0);
    }
    assert_eq!(
        flight.pending,
        Some("flight_gameplay_initialization_pending")
    );
    assert_eq!(flight.substate, 6);
    assert_eq!(flight.allocation_bytes, [420644, 23696]);
    assert_eq!(flight.arena_top, Some(444340));
    assert_eq!(flight.mission_assets.as_ref().unwrap().sections().len(), 11);
    assert_eq!(flight.aircraft_assets.as_ref().unwrap().sections().len(), 2);
    for (offset, count) in [(0xf80, 128), (0xf84, 12), (0xf88, 6)] {
        assert_eq!(
            u32::from_le_bytes(
                flight.game_context_bytes()[offset..offset + 4]
                    .try_into()
                    .unwrap()
            ),
            count
        );
    }
    let capture = root.join("analysis/menu-runtime/mission-007");
    let uploads: Vec<serde_json::Value> =
        serde_json::from_slice(&fs::read(capture.join("uploads.json")).unwrap()).unwrap();
    assert_eq!(uploads.len(), 598);
    for upload in uploads {
        let bytes = fs::read(capture.join(upload["payload"].as_str().unwrap())).unwrap();
        expected_vram
            .load_block(&tim::Block {
                x: upload["x"].as_u64().unwrap() as u16,
                y: upload["y"].as_u64().unwrap() as u16,
                width: upload["width"].as_u64().unwrap() as u16,
                height: upload["height"].as_u64().unwrap() as u16,
                data: &bytes,
            })
            .unwrap();
    }
    assert_eq!(flight.vram.words(), expected_vram.words());
    // The original SPU dump contains the same two replacement transfers.
    let state = fs::read(capture.join("frame-00400.sav")).unwrap();
    let spu = length_field(length_field(&state, 6), 1);
    for (start, len) in [(0x4a650, 199968), (0x43c90, 24688)] {
        assert_eq!(
            &flight.sound.samples[start..start + len],
            &spu[start..start + len]
        );
    }
    assert!(flight.sound.descriptors.contains_key(&84));
    assert!(flight.sound.descriptors.contains_key(&107));
    assert!(flight.step(1036).is_err());
}

fn varint(bytes: &[u8], at: &mut usize) -> u64 {
    let mut value = 0;
    for shift in (0..64).step_by(7) {
        let byte = bytes[*at];
        *at += 1;
        value |= u64::from(byte & 127) << shift;
        if byte & 128 == 0 {
            return value;
        }
    }
    panic!("invalid fixture varint")
}
fn length_field(bytes: &[u8], wanted: u64) -> &[u8] {
    let mut at = 0;
    while at < bytes.len() {
        let key = varint(bytes, &mut at);
        match key & 7 {
            0 => {
                varint(bytes, &mut at);
            }
            1 => at += 8,
            2 => {
                let len = varint(bytes, &mut at) as usize;
                if key >> 3 == wanted {
                    return &bytes[at..at + len];
                }
                at += len;
            }
            5 => at += 4,
            _ => panic!("invalid fixture wire type"),
        }
    }
    panic!("fixture field missing")
}
