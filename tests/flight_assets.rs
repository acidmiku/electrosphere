//! Independent resident-loader oracles. Captures are only read by ignored tests.
use ac3_port::{
    assets::GameData,
    flight_assets::{
        FlightAircraftAssets, FlightMissionAssets, RecordPointer, ResidentSection, Upload,
    },
};
use std::{fs, path::PathBuf};

fn root() -> PathBuf {
    PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("set AC3_DATA_DIR"))
}
fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}
fn write_word(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn relocated_section(section: &ResidentSection, address: u32) -> Vec<u8> {
    let mut bytes = section.bytes().to_vec();
    if let Some(directory) = &section.directory {
        for (index, &offset) in directory.slots.iter().enumerate() {
            write_word(&mut bytes, 4 + index * 4, address + offset as u32);
        }
        for relocation in &directory.relocations {
            let value = match relocation.target {
                RecordPointer::SectionOffset(offset) => address + offset as u32,
                RecordPointer::Encoded(value) => value,
            };
            write_word(&mut bytes, relocation.field_offset, value);
        }
    }
    bytes
}

fn assert_uploads(expected: &[Upload], original: &[serde_json::Value], capture: &std::path::Path) {
    assert_eq!(expected.len(), original.len());
    for (index, (native, original)) in expected.iter().zip(original).enumerate() {
        let rectangle =
            ["x", "y", "width", "height"].map(|key| original[key].as_u64().unwrap() as u16);
        assert_eq!(
            native.rectangle, rectangle,
            "upload rectangle {index}: {:?}",
            native.source
        );
        let payload = fs::read(capture.join(original["payload"].as_str().unwrap())).unwrap();
        assert_eq!(
            native.bytes, payload,
            "upload payload {index}: {:?}",
            native.source
        );
        assert_eq!(
            ac3_port::sha256(&payload),
            original["sha256"].as_str().unwrap()
        );
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and original mission-007 LoadImage/RAM oracles"]
fn first_mission_and_aircraft_match_all_uploads_and_resident_data() {
    let root = root();
    let game = GameData::load(&root.join("extracted/disc")).unwrap();
    // These resource IDs come from resident tables, not the captured RAM.
    assert_eq!(game.executable.payload[0x800af748 - 0x80010000 + 2], 7);
    assert_eq!(game.executable.payload[0x800af79c - 0x80010000 + 2], 48);
    let mut mission = FlightMissionAssets::parse(game.resource(7).unwrap()).unwrap();
    let aircraft = FlightAircraftAssets::parse(game.resource(48).unwrap()).unwrap();
    assert_eq!(mission.sections().len(), 11);
    assert_eq!(
        mission
            .texture_groups()
            .iter()
            .map(|v| (v.section, v.image_count))
            .collect::<Vec<_>>(),
        [(15, 2), (14, 3), (13, 4), (12, 3), (11, 281)]
    );
    assert_eq!(aircraft.texture_groups()[0].image_count, 2);
    assert_eq!(
        mission.sections()[2]
            .directory
            .as_ref()
            .unwrap()
            .slots
            .len(),
        432
    );
    assert_eq!(
        mission.sections()[3]
            .directory
            .as_ref()
            .unwrap()
            .slots
            .len(),
        128
    );
    assert_eq!(
        mission.sections()[4]
            .directory
            .as_ref()
            .unwrap()
            .slots
            .len(),
        16
    );
    assert_eq!(
        mission.sections()[5]
            .directory
            .as_ref()
            .unwrap()
            .slots
            .len(),
        6
    );
    assert_eq!(
        aircraft.sections()[0]
            .directory
            .as_ref()
            .unwrap()
            .slots
            .len(),
        12
    );
    // Native initialization starts at zero in this fixture; inherited source
    // cells were independently observed zero. Runtime callers supply live VRAM.
    let graphics = mission
        .prepare_graphics(&game.executable.payload, &ac3_graphics::Vram::new())
        .unwrap();
    let aircraft_graphics = aircraft.prepare_graphics(&graphics.vram).unwrap();
    let capture = root.join("analysis/menu-runtime/mission-007");
    let uploads: Vec<serde_json::Value> =
        serde_json::from_slice(&fs::read(capture.join("uploads.json")).unwrap()).unwrap();
    assert_eq!(uploads.len(), 598);
    assert_uploads(&graphics.uploads, &uploads[..594], &capture);
    assert_uploads(&aircraft_graphics.uploads, &uploads[594..], &capture);
    for (resource, sections) in [(7, mission.sections()), (48, aircraft.sections())] {
        for section in sections {
            let expanded = fs::read(root.join(format!(
                "extracted/archives/bpb_{resource:04}_{:04}_dec.bin",
                section.index
            )))
            .unwrap();
            assert_eq!(
                section.bytes(),
                expanded,
                "expanded section {resource}/{}",
                section.index
            );
        }
    }
    // The saved frame is after gameplay initialization. Its sole extra section
    // mutation is the aircraft directory assignment at 80032510, explicitly
    // checked below; the asset loader deliberately does not perform that action.
    let ram = fs::read(capture.join("frame-00400.ram.bin")).unwrap();
    let context = word(&ram, 0xbacc0) as usize & 0x1fffff;
    let mission_slots = [
        (0x18, 0),
        (0x1c, 0),
        (0x20, 0),
        (0x24, 4),
        (0x28, 4),
        (0x2c, 4),
        (0x30, 0),
        (0x34, 0),
        (0x38, 0),
        (0x3c, 4),
        (0x40, 4),
    ];
    let addresses: Vec<u32> = mission_slots
        .iter()
        .map(|&(slot, bias)| word(&ram, context + slot) - bias)
        .collect();
    let aircraft_addresses = [word(&ram, context + 8) - 4, word(&ram, context + 12)];
    let mission_base = addresses[0] - mission.sections()[0].allocation_offset as u32;
    let aircraft_base = aircraft_addresses[0] - aircraft.sections()[0].allocation_offset as u32;
    assert_eq!(
        mission_base + mission.resident_bytes() as u32,
        aircraft_base
    );
    assert_eq!(
        aircraft_base + aircraft.resident_bytes() as u32,
        word(&ram, context + 0x5c)
    );
    for section in mission.sections() {
        let address = addresses[section.index];
        assert_eq!(address, mission_base + section.allocation_offset as u32);
        let mut expected = relocated_section(section, address);
        if section.index == 3 {
            for (index, record) in mission.object_slots().iter().enumerate() {
                write_word(
                    &mut expected,
                    4 + index * 4,
                    addresses[record.section] + record.offset as u32,
                );
            }
            let actual_slot3 = word(&ram, (address as usize & 0x1fffff) + 16);
            assert_eq!(actual_slot3, aircraft_addresses[0] + 4);
            write_word(&mut expected, 16, aircraft_addresses[0] + 4);
        }
        let start = address as usize & 0x1fffff;
        assert_eq!(
            expected,
            ram[start..start + expected.len()],
            "resident mission section {}",
            section.index
        );
    }
    for section in aircraft.sections() {
        let address = aircraft_addresses[section.index];
        assert_eq!(address, aircraft_base + section.allocation_offset as u32);
        let expected = relocated_section(section, address);
        let start = address as usize & 0x1fffff;
        assert_eq!(
            expected,
            ram[start..start + expected.len()],
            "resident aircraft section {}",
            section.index
        );
    }
}

// Minimal protobuf traversal of the original uncompressed SaveState. No state
// restoration or capture-derived runtime assets are involved.
fn length_field(bytes: &[u8], wanted: u64) -> &[u8] {
    fn varint(bytes: &[u8], at: &mut usize) -> u64 {
        let mut value = 0;
        for shift in (0..70).step_by(7) {
            let byte = bytes[*at];
            *at += 1;
            value |= u64::from(byte & 127) << shift;
            if byte < 128 {
                return value;
            }
        }
        panic!("invalid oracle protobuf varint")
    }
    let mut at = 0;
    while at < bytes.len() {
        let tag = varint(bytes, &mut at);
        match tag & 7 {
            0 => {
                varint(bytes, &mut at);
            }
            1 => at += 8,
            2 => {
                let size = varint(bytes, &mut at) as usize;
                let data = &bytes[at..at + size];
                if tag >> 3 == wanted {
                    return data;
                }
                at += size;
            }
            5 => at += 4,
            _ => panic!("unsupported oracle wire type"),
        }
    }
    panic!("missing original protobuf field {wanted}")
}

#[test]
#[ignore = "requires AC3_DATA_DIR and original mission-007 SPU/RAM savestate"]
fn both_banks_match_original_spu_transfers_and_descriptor_installation() {
    let root = root();
    let game = GameData::load(&root.join("extracted/disc")).unwrap();
    let mission = FlightMissionAssets::parse(game.resource(7).unwrap()).unwrap();
    let aircraft = FlightAircraftAssets::parse(game.resource(48).unwrap()).unwrap();
    let capture = root.join("analysis/menu-runtime/mission-007");
    let state = fs::read(capture.join("frame-00400.sav")).unwrap();
    let spu = length_field(length_field(&state, 6), 1);
    assert_eq!(spu.len(), ac3_audio::SPU_RAM_BYTES);
    let ram = fs::read(capture.join("frame-00400.ram.bin")).unwrap();
    for (bytes, first, count, address, len) in [
        (mission.sound_bank_bytes(), 88, 20, 0x4a650, 199968),
        (aircraft.sound_bank_bytes(), 84, 2, 0x43c90, 24688),
    ] {
        let bank = ac3_audio::Bank::parse(bytes).unwrap();
        assert_eq!(
            (
                bank.first_sound_id(),
                bank.sound_count(),
                bank.spu_address(),
                bank.sample_bytes().len()
            ),
            (first, count, address, len)
        );
        assert_eq!(
            bank.sample_bytes(),
            &spu[address as usize..address as usize + len]
        );
        let descriptors = &bytes[12..12 + usize::from(count) * 20];
        let at = 0xc37d8 + usize::from(first) * 20;
        assert_eq!(descriptors, &ram[at..at + descriptors.len()]);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR; runs the native menu-to-launch route before checking inherited palettes"]
fn native_launch_vram_generates_the_original_flight_palettes() {
    use ac3_port::{
        frontend::parse_inputs, menu_runtime::MenuRuntime, mission_runtime::MissionRuntime,
    };
    let root = root();
    let game = GameData::load(&root.join("extracted/disc")).unwrap();
    let inputs = parse_inputs("40:confirm,110:start,160:confirm,200:down,220:confirm,390:right,410:left,450:confirm,650:start,750:confirm,850:confirm,950:confirm").unwrap();
    let mut menu = MenuRuntime::new(&game).unwrap();
    let mut mission: Option<MissionRuntime<'_>> = None;
    let mut boundary = None;
    for frame in 1..=1100 {
        let input = inputs.get(&frame).copied().unwrap_or(0);
        if let Some(runtime) = &mut mission {
            runtime.step(frame as i32, input, input).unwrap();
            if let Some(pending) = runtime.pending {
                assert_eq!(pending, "mission_resident_transition_pending");
                boundary = Some(frame);
                break;
            }
        } else {
            menu.step(frame as i32, input, input).unwrap();
            if let Some(pending) = menu.pending {
                assert_eq!(pending, "mission_transition_pending");
                mission = Some(
                    MissionRuntime::new(
                        &game,
                        menu.game_context_bytes().unwrap(),
                        menu.context.language_id as usize,
                    )
                    .unwrap(),
                );
            }
        }
    }
    assert_eq!(boundary, Some(1029));
    let mut assets = FlightMissionAssets::parse(game.resource(7).unwrap()).unwrap();
    let original_vram = &mission.as_ref().unwrap().scene.scene.vram;
    let inherited = original_vram.words().to_vec();
    let graphics = assets
        .prepare_graphics(&game.executable.payload, original_vram)
        .unwrap();
    assert_eq!(
        inherited,
        original_vram.words(),
        "preparation does not mutate caller VRAM"
    );
    let capture = root.join("analysis/menu-runtime/mission-007");
    let uploads: Vec<serde_json::Value> =
        serde_json::from_slice(&fs::read(capture.join("uploads.json")).unwrap()).unwrap();
    assert_uploads(&graphics.uploads[586..], &uploads[586..594], &capture);
}
