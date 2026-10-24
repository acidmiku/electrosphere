use ac3_port::{
    assets::GameData,
    flight_assets::{FlightAircraftAssets, FlightMissionAssets},
    flight_initialization::{self as init, InitializedFields, ObjectSlot, PrefixInputs, Reference},
};
use std::{fs, path::PathBuf};

fn root() -> PathBuf {
    PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("set AC3_DATA_DIR"))
}
fn word(bytes: &[u8], address: u32) -> u32 {
    let at = address as usize & 0x1fffff;
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}
fn put(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
fn json_word(value: &serde_json::Value, name: &str) -> u32 {
    value[name].as_u64().unwrap().try_into().unwrap()
}

fn overlay(fields: &InitializedFields, initial: &mut [u8], resolve: &impl Fn(Reference) -> u32) {
    assert_eq!(fields.len(), initial.len());
    for ((byte, &native), &known) in initial
        .iter_mut()
        .zip(fields.scalar_bytes())
        .zip(fields.scalar_mask())
    {
        if known {
            *byte = native;
        }
    }
    for reference in fields.references() {
        put(initial, reference.offset, resolve(reference.target));
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and passive initialization-002 checkpoints"]
fn original_prefix_matches_all_allocated_bytes_and_preserves_unknown_fields() {
    let root = root();
    let data = GameData::load(&root.join("extracted/disc")).unwrap();
    let mission = FlightMissionAssets::parse(data.resource(7).unwrap()).unwrap();
    let aircraft = FlightAircraftAssets::parse(data.resource(48).unwrap()).unwrap();
    let capture = root.join("analysis/menu-runtime/initialization-002");
    let events: Vec<serde_json::Value> = fs::read_to_string(capture.join("initialization.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let before = events
        .iter()
        .find(|v| v["stage"] == "after_loading_poll")
        .unwrap();
    let after = events
        .iter()
        .find(|v| v["stage"] == "camera_entry")
        .unwrap();
    let input = fs::read(capture.join(before["ram_file"].as_str().unwrap())).unwrap();
    let output = fs::read(capture.join(after["ram_file"].as_str().unwrap())).unwrap();
    assert_eq!(
        &input[0x32510..0x32c80],
        &data.executable.payload[0x22510..0x22c80],
        "original initializer instructions are unchanged"
    );
    let context = json_word(before, "context");
    let display = json_word(before, "display");
    let resources = json_word(before, "resources");
    let arena = word(&input, resources + 0x5c);
    let prefix = init::initialize_after_loading_poll(
        &mission,
        &aircraft,
        PrefixInputs {
            display_flags: word(&input, display),
            context_flags: word(&input, context),
            transition_words: [word(&input, context + 0x3b4), word(&input, context + 0x3b8)],
            trailing_resource_words: std::array::from_fn(|i| {
                word(&input, resources + 0x44 + i as u32 * 4)
            }),
            available_arena_bytes: (0x801fb700 - arena) as usize,
        },
    )
    .unwrap();
    assert_eq!(json_word(after, "gameplay"), arena);
    assert_eq!(json_word(after, "entity_table"), arena + 700);
    assert_eq!(json_word(after, "second_entity_table"), arena + 700 + 128);
    assert_eq!(word(&output, context + 0x1c), arena + 8628);
    let mission_starts: Vec<_> = [
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
    ]
    .into_iter()
    .map(|(field, bias)| word(&input, resources + field) - bias)
    .collect();
    let aircraft_starts = [
        word(&input, resources + 8) - 4,
        word(&input, resources + 12),
    ];
    let resolve = |reference| match reference {
        Reference::MissionSection { section, offset } => mission_starts[section] + offset as u32,
        Reference::AircraftSection { section, offset } => aircraft_starts[section] + offset as u32,
        Reference::Gameplay { offset } | Reference::Arena { offset } => arena + offset as u32,
        Reference::Entity { index, offset } => {
            arena + prefix.entities[index].allocation_offset as u32 + offset as u32
        }
    };
    let start = arena as usize & 0x1fffff;
    let mut expected = input[start..start + prefix.allocated_bytes].to_vec();
    overlay(&prefix.gameplay, &mut expected[..700], &resolve);
    overlay(&prefix.entity_table, &mut expected[700..956], &resolve);
    for entity in &prefix.entities {
        let at = entity.allocation_offset;
        overlay(
            &entity.fields,
            &mut expected[at..at + entity.layout.bytes()],
            &resolve,
        );
    }
    let actual = &output[start..start + expected.len()];
    let differences: Vec<_> = expected
        .iter()
        .zip(actual)
        .enumerate()
        .filter_map(|(i, (a, b))| (a != b).then_some(i))
        .collect();
    assert!(
        differences.is_empty(),
        "allocation mismatches: {:?}",
        &differences[..differences.len().min(24)]
    );
    // Unknown fields really contain inherited nonzero bytes in this run. A
    // fabricated all-zero allocation would fail this independent comparison.
    assert!(
        input[start..start + 700]
            .iter()
            .zip(prefix.gameplay.scalar_mask())
            .any(|(&byte, &known)| !known && byte != 0)
    );
    let object_table = word(&input, resources + 0x24);
    for (index, slot) in prefix.object_slots.iter().enumerate() {
        let value = match *slot {
            ObjectSlot::MissionRecord(record) => {
                mission_starts[record.section] + record.offset as u32
            }
            ObjectSlot::AircraftDirectory { section, offset } => {
                aircraft_starts[section] + offset as u32
            }
        };
        assert_eq!(word(&output, object_table + index as u32 * 4), value);
    }
    assert_ne!(
        word(&input, object_table + 12),
        word(&output, object_table + 12)
    );
    assert_eq!(word(&output, display), prefix.display.flags);
    assert_eq!(word(&output, display + 4), prefix.display.field_04);
    assert_eq!(word(&output, display + 0x14), prefix.display.field_14);
    assert_eq!(word(&output, context), prefix.context_flags);
    assert_eq!(
        word(&output, resources + 0x70),
        prefix.resource_transition_words[0]
    );
    assert_eq!(
        word(&output, resources + 0x74),
        prefix.resource_transition_words[1]
    );
    let pending = prefix.pending_camera;
    let arguments: Vec<u32> = after["arguments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap() as u32)
        .collect();
    assert_eq!(
        arguments,
        [
            resolve(pending.destination),
            pending.projection_distance as u32,
            pending.mission_parameter_0c as i32 as u32,
            pending.mission_parameter_0e as i32 as u32
        ]
    );
    assert_eq!(
        json_word(after, "stack_argument0"),
        resolve(pending.aircraft_parameters)
    );
}

#[test]
#[ignore = "requires AC3_DATA_DIR; checks native asset bindings without any emulator state"]
fn native_assets_define_layout_and_capacity_errors_without_snapshot_inputs() {
    let root = root();
    let data = GameData::load(&root.join("extracted/disc")).unwrap();
    let mission = FlightMissionAssets::parse(data.resource(7).unwrap()).unwrap();
    let aircraft = FlightAircraftAssets::parse(data.resource(48).unwrap()).unwrap();
    let input = PrefixInputs {
        display_flags: 0xa0,
        context_flags: 0x02000040,
        transition_words: [0x12, 0x34],
        trailing_resource_words: [11, 22, 33, 44, 55, 66],
        available_arena_bytes: 8628,
    };
    let prefix = init::initialize_after_loading_poll(&mission, &aircraft, input).unwrap();
    assert_eq!(prefix.display.flags, 0xa5);
    assert_eq!(prefix.context_flags, 0x12000040);
    assert_eq!(prefix.resource_transition_words, [0x12, 0x34]);
    assert_eq!(
        prefix
            .entities
            .iter()
            .map(|e| e.layout.bytes())
            .sum::<usize>(),
        7672
    );
    assert_eq!(prefix.entities.len(), 64);
    for pair in prefix.entities.windows(2) {
        assert_eq!(
            pair[1].allocation_offset,
            pair[0].allocation_offset + pair[0].layout.bytes()
        );
    }
    assert_eq!(
        prefix.gameplay.scalar(0x284, 4).unwrap(),
        66_u32.to_le_bytes()
    );
    assert_eq!(
        prefix.object_slots[3],
        ObjectSlot::AircraftDirectory {
            section: 0,
            offset: 4
        }
    );
    assert_eq!(
        mission.object_slots()[3].section,
        3,
        "owned source asset remains unmodified"
    );
    assert_eq!(prefix.pending_camera.mission_parameter_0c, 16395);
    assert_eq!(prefix.pending_camera.mission_parameter_0e, 28000);
    assert!(
        init::initialize_after_loading_poll(
            &mission,
            &aircraft,
            PrefixInputs {
                available_arena_bytes: 8627,
                ..input
            }
        )
        .is_err()
    );
}
