//! Original 80032548..80032990 as one composed native span.
//! Snapshot 02 plus native writes must reproduce snapshot 06 in all of RAM
//! outside the stack. Guest addresses appear only in this comparison adapter.
use ac3_graphics::transform::SinCosTable;
use ac3_port::{
    assets::GameData,
    flight_assets::{FlightAircraftAssets, FlightMissionAssets},
    flight_gameplay::{self, GameplayProgress},
    flight_initialization::{InitializedFields, ObjectSlot, PrefixInputs, Reference},
    sha256,
};
use serde_json::{Value, json};
use std::{fs, path::PathBuf};

const STACK_START: usize = 0x1fe000;

fn word(bytes: &[u8], address: u32) -> u32 {
    let at = address as usize & 0x1fffff;
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}
fn put(bytes: &mut [u8], address: u32, value: u32) {
    let at = address as usize & 0x1fffff;
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}
fn json_word(value: &Value, name: &str) -> u32 {
    value[name].as_u64().unwrap().try_into().unwrap()
}

struct Resolver {
    arena: u32,
    mission: Vec<u32>,
    aircraft: [u32; 2],
    entities: Vec<u32>,
}
impl Resolver {
    fn address(&self, reference: Reference) -> u32 {
        match reference {
            Reference::MissionSection { section, offset } => self.mission[section] + offset as u32,
            Reference::AircraftSection { section, offset } => {
                self.aircraft[section] + offset as u32
            }
            Reference::Gameplay { offset } | Reference::Arena { offset } => {
                self.arena + offset as u32
            }
            Reference::Entity { index, offset } => self.entities[index] + offset as u32,
        }
    }
    fn overlay(&self, ram: &mut [u8], base: u32, fields: &InitializedFields) {
        let start = base as usize & 0x1fffff;
        for (i, (&value, &known)) in fields
            .scalar_bytes()
            .iter()
            .zip(fields.scalar_mask())
            .enumerate()
        {
            if known {
                ram[start + i] = value;
            }
        }
        for reference in fields.references() {
            put(
                ram,
                base + reference.offset as u32,
                self.address(reference.target),
            );
        }
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and passive initialization-002 snapshots"]
fn composed_span_reproduces_original_ram_before_world_setup() {
    let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").unwrap());
    let data = GameData::load(&root.join("extracted/disc")).unwrap();
    let mission = FlightMissionAssets::parse(data.resource(7).unwrap()).unwrap();
    let aircraft = FlightAircraftAssets::parse(data.resource(48).unwrap()).unwrap();
    let trig =
        SinCosTable::from_le_bytes(&data.executable.payload[0xa4f84..0xa4f84 + 16384]).unwrap();
    let capture = root.join("analysis/menu-runtime/initialization-002");
    let metadata: Value =
        serde_json::from_slice(&fs::read(capture.join("metadata.json")).unwrap()).unwrap();
    assert_eq!(metadata["guest_code_patched"], false);
    assert_eq!(metadata["guest_ram_patched"], false);
    let events: Vec<Value> = fs::read_to_string(capture.join("initialization.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let stage = |name: &str| events.iter().find(|v| v["stage"] == name).unwrap();
    let before = stage("after_loading_poll");
    let after = stage("world_setup_call");
    let input = fs::read(capture.join(before["ram_file"].as_str().unwrap())).unwrap();
    let output = fs::read(capture.join(after["ram_file"].as_str().unwrap())).unwrap();
    assert_eq!(
        &input[0x32510..0x32c80],
        &data.executable.payload[0x22510..0x22c80]
    );
    let context = json_word(before, "context");
    let display = json_word(before, "display");
    let resources = json_word(before, "resources");
    let arena = word(&input, resources + 0x5c);
    let inputs = PrefixInputs {
        display_flags: word(&input, display),
        context_flags: word(&input, context),
        transition_words: [word(&input, context + 0x3b4), word(&input, context + 0x3b8)],
        trailing_resource_words: std::array::from_fn(|i| {
            word(&input, resources + 0x44 + i as u32 * 4)
        }),
        available_arena_bytes: (0x801fb700 - arena) as usize,
    };
    // Values the native player must supply from its own state at this point.
    assert_eq!(inputs.display_flags, 0x40);
    assert_eq!(inputs.context_flags, 0);
    assert_eq!(inputs.transition_words, [0, 0]);
    assert_eq!(inputs.trailing_resource_words, [0; 6]);
    let mode_byte = input[(context as usize & 0x1fffff) + 0x3a3];
    assert_eq!(mode_byte, 0);
    let progress: GameplayProgress =
        flight_gameplay::initialize_through_lighting(&mission, &aircraft, inputs, mode_byte, &trig)
            .unwrap();
    let prefix = &progress.prefix;

    let mission_starts = [
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
    let resolver = Resolver {
        arena,
        mission: mission_starts,
        aircraft: [
            word(&input, resources + 8) - 4,
            word(&input, resources + 12),
        ],
        entities: prefix
            .entities
            .iter()
            .map(|e| arena + e.allocation_offset as u32)
            .collect(),
    };
    let mut expected = input.clone();
    resolver.overlay(&mut expected, arena, &prefix.gameplay);
    resolver.overlay(
        &mut expected,
        arena + prefix.entity_table_allocation.offset as u32,
        &prefix.entity_table,
    );
    for entity in &prefix.entities {
        resolver.overlay(
            &mut expected,
            arena + entity.allocation_offset as u32,
            &entity.fields,
        );
    }
    resolver.overlay(
        &mut expected,
        arena + progress.depth_allocation.offset as u32,
        &progress.depth,
    );
    let object_table = word(&input, resources + 0x24);
    for (index, slot) in prefix.object_slots.iter().enumerate() {
        let value = match *slot {
            ObjectSlot::MissionRecord(record) => resolver.address(Reference::MissionSection {
                section: record.section,
                offset: record.offset,
            }),
            ObjectSlot::AircraftDirectory { section, offset } => {
                resolver.address(Reference::AircraftSection { section, offset })
            }
        };
        put(&mut expected, object_table + index as u32 * 4, value);
    }
    put(&mut expected, 0x800bacc4, arena);
    put(
        &mut expected,
        0x800baccc,
        arena + prefix.entity_table_allocation.offset as u32,
    );
    put(
        &mut expected,
        0x800bacbc,
        arena + prefix.entity_table_allocation.offset as u32 + 4 * 32,
    );
    put(&mut expected, display, prefix.display.flags);
    put(&mut expected, display + 4, prefix.display.field_04);
    put(&mut expected, display + 0x14, prefix.display.field_14);
    put(&mut expected, context, prefix.context_flags);
    put(
        &mut expected,
        context + 0x1c,
        arena + progress.allocated_bytes as u32,
    );
    put(
        &mut expected,
        resources + 0x70,
        prefix.resource_transition_words[0],
    );
    put(
        &mut expected,
        resources + 0x74,
        prefix.resource_transition_words[1],
    );

    let differences: Vec<String> = (0..STACK_START)
        .filter(|&i| expected[i] != output[i])
        .take(24)
        .map(|i| format!("{:08x}", 0x80000000 + i))
        .collect();
    assert!(
        differences.is_empty(),
        "RAM mismatches outside stack: {differences:?}"
    );
    let changed = (0..STACK_START).filter(|&i| input[i] != output[i]).count();
    assert!(changed > 4000, "comparison covers the real span");
    assert_eq!(progress.allocated_bytes, 8664);
    assert_eq!(progress.pending_call, 0x8004af9c);

    let report = json!({"source":"passive original initialization-002 snapshots 02 -> 06",
        "compared_ram_bytes":STACK_START,"changed_original_bytes":changed,
        "excluded":"stack 0x801FE000..0x80200000",
        "span":"80032548..80032990: prefix, camera, zero words, projection/depth, depth allocation, lighting",
        "allocated_bytes":progress.allocated_bytes,"pending_call":"8004AF9C",
        "snapshot02_sha256":sha256(&input),"snapshot06_sha256":sha256(&output)});
    fs::create_dir_all(root.join("analysis/native-flight")).unwrap();
    fs::write(
        root.join("analysis/native-flight/gameplay-span-validation.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
}
