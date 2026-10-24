//! Passive original camera entry/return and projection/depth comparison.
//! Guest addresses are used only to resolve the typed pointer in this test.
use ac3_port::{assets::GameData, flight_assets, flight_camera, sha256};
use flight_assets::{FlightAircraftAssets, FlightMissionAssets};
use flight_camera::*;
use serde_json::{Value, json};
use std::{fs, path::PathBuf};

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}
fn uint(v: &Value) -> u32 {
    v.as_u64().unwrap().try_into().unwrap()
}
fn word(b: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(b[offset..offset + 4].try_into().unwrap())
}

#[test]
#[ignore = "requires original assets and passive camera-001 capture"]
fn original_camera_and_projection_depth_setup() {
    let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").unwrap());
    let capture = root.join("analysis/flight-runtime/camera-001");
    let metadata: Value =
        serde_json::from_slice(&fs::read(capture.join("metadata.json")).unwrap()).unwrap();
    let addon: Value =
        serde_json::from_slice(&fs::read(capture.join("addon-metadata.json")).unwrap()).unwrap();
    assert_eq!(metadata["guest_code_patched"], false);
    assert_eq!(metadata["guest_ram_patched"], false);
    assert_eq!(
        metadata["original_disc_sha256"],
        "5d59ef5bf2b905cde40cf8a14647b409228a4443eb94e9a0444351544ca87276"
    );
    assert_eq!(
        metadata["engine_sha256"],
        sha256(&fs::read(capture.join("capture-engine.lua")).unwrap())
    );
    assert_eq!(
        addon["sha256"],
        sha256(&fs::read(capture.join("capture-addon.lua")).unwrap())
    );
    let original = fs::read(capture.join("camera.jsonl")).unwrap();
    let rows: Vec<Value> = std::str::from_utf8(&original)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let camera = rows.iter().find(|row| row["kind"] == "camera").unwrap();
    let data = GameData::load(&root.join("extracted/disc")).unwrap();
    let mission = FlightMissionAssets::parse(data.resource(7).unwrap()).unwrap();
    let aircraft = FlightAircraftAssets::parse(data.resource(48).unwrap()).unwrap();
    let parameters = CameraParameters::from_assets(
        &mission,
        &aircraft,
        uint(&camera["mode_byte"]) as u8,
        uint(&camera["entity_flags"]),
    )
    .unwrap();
    assert_eq!(
        i32::from(parameters.projection_distance) as u32,
        uint(&camera["arguments"][1])
    );
    assert_eq!(
        i32::from(parameters.near) as u32,
        uint(&camera["arguments"][2])
    );
    assert_eq!(
        i32::from(parameters.far) as u32,
        uint(&camera["arguments"][3])
    );
    let parameter_bytes: Vec<u8> = parameters
        .aircraft_offsets
        .into_iter()
        .flat_map(i16::to_le_bytes)
        .collect();
    assert_eq!(
        parameter_bytes,
        unhex(camera["aircraft_parameters"].as_str().unwrap())
    );
    let fields = initialize_camera(parameters);
    let mut actual = unhex(camera["input_hex"].as_str().unwrap());
    assert_eq!(actual.len(), CAMERA_BYTES);
    for (i, (&value, &known)) in fields
        .scalar_bytes()
        .iter()
        .zip(fields.scalar_mask())
        .enumerate()
    {
        if known {
            actual[i] = value;
        }
    }
    let target = fields.target;
    assert_eq!(target.entity_index, 0);
    let entity_position = uint(&camera["entity_address"]) + target.position_offset as u32;
    actual[target.field_offset..target.field_offset + 4]
        .copy_from_slice(&entity_position.to_le_bytes());
    assert_eq!(
        actual,
        unhex(camera["expected_hex"].as_str().unwrap()),
        "all 352 camera bytes, preserving untouched allocator data"
    );
    assert_eq!(camera["input_controls"], camera["expected_controls"]);

    let geometry = geometry_setup(parameters).unwrap();
    let buckets = depth_buckets(i32::from(parameters.near), i32::from(parameters.far));
    let mut helpers = 0;
    let mut divisions = 0;
    for row in &rows {
        let mut controls: Vec<u32> = row["input_controls"]
            .as_array()
            .unwrap()
            .iter()
            .map(uint)
            .collect();
        match row["kind"].as_str().unwrap() {
            "geometry_offset" => {
                assert_eq!(row["arguments"][0], 0);
                assert_eq!(row["arguments"][1], 0);
                controls[24] = geometry.offset[0] as u32;
                controls[25] = geometry.offset[1] as u32;
            }
            "geometry_screen" => {
                controls[26] = u32::from(geometry.screen);
            }
            "depth_cue" => {
                controls[27] = i32::from(geometry.dqa) as u32;
                controls[28] = geometry.dqb as u32;
                assert_eq!(geometry.dqb as u32, uint(&row["result"][0]));
            }
            "depth_table" => {
                let mut expected = unhex(row["input_hex"].as_str().unwrap());
                assert_eq!(expected.len(), DEPTH_ALLOCATION_BYTES);
                for (index, value) in buckets.into_iter().enumerate() {
                    expected[index * 2..index * 2 + 2].copy_from_slice(&value.to_le_bytes());
                }
                assert_eq!(expected, unhex(row["expected_hex"].as_str().unwrap()));
                assert_eq!(row["result"][0], 4094);
            }
            "divide64" => {
                let args: Vec<u32> = row["arguments"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(uint)
                    .collect();
                let numerator = ((u64::from(args[1]) << 32) | u64::from(args[0])) as i64;
                let denominator = ((u64::from(args[3]) << 32) | u64::from(args[2])) as i64;
                let result = numerator.checked_div(denominator).unwrap() as u64;
                assert_eq!(result as u32, uint(&row["result"][0]));
                assert_eq!((result >> 32) as u32, uint(&row["result"][1]));
                divisions += 1;
                continue;
            }
            _ => continue,
        }
        assert_eq!(
            controls,
            row["expected_controls"]
                .as_array()
                .unwrap()
                .iter()
                .map(uint)
                .collect::<Vec<_>>(),
            "all 32 GTE controls for {}",
            row["kind"]
        );
        helpers += 1;
    }
    assert_eq!(helpers, 4);
    assert_eq!(divisions, 5);
    assert_eq!(GAMEPLAY_CAMERA_OFFSET, 0xb8);
    assert_eq!(POST_CAMERA_ZERO_WORDS, [0x48, 0x44, 0x40, 0x68, 0x64, 0x60]);
    assert_eq!(GAMEPLAY_DEPTH_OFFSET, 0x288);
    assert_eq!(PENDING_LIGHTING_PC, 0x80032878);
    assert_eq!(word(&actual, 0), 0x500);
    let report = json!({"source":"passive original camera-001; no guest code/RAM patching",
        "camera_wrappers":1,"camera_bytes":352,"known_scalar_bytes":fields.scalar_mask().iter().filter(|&&v|v).count(),
        "typed_entity_references":1,"projection_and_depth_helpers":helpers,"gte_control_registers_per_helper":32,
        "signed_64_bit_divisions":divisions,"depth_allocation_bytes_compared":36,"depth_bytes_written":34,
        "dqa":geometry.dqa,"dqb":geometry.dqb,"depth_buckets":buckets,
        "capture_sha256":sha256(&original),"scope":"asset-derived camera arguments; unknown allocator holes preserved; stops before lighting/gameplay initialization"});
    fs::create_dir_all(root.join("analysis/native-flight")).unwrap();
    fs::write(
        root.join("analysis/native-flight/camera-validation.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
}
