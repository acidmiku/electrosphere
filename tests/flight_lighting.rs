//! Passive original 80032878..80032990 lighting comparison.
//! Captured RAM/GTE state is the expected result; native assets supply inputs.
use ac3_graphics::transform::SinCosTable;
use ac3_port::{assets::GameData, flight_assets::FlightMissionAssets, flight_lighting::*, sha256};
use serde_json::{Value, json};
use std::{fs, path::PathBuf};

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}
fn registers(v: &Value) -> Vec<u32> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_u64().unwrap().try_into().unwrap())
        .collect()
}
fn halves(values: &[i16]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}
fn matrix_bytes(matrix: [[i16; 3]; 3]) -> Vec<u8> {
    halves(matrix.as_flattened())
}
fn pair(low: i16, high: i16) -> u32 {
    u32::from(low as u16) | (u32::from(high as u16) << 16)
}

/// RTV0 effects: rotation controls, V0, IR1-3 and MAC1-3. FLAG is unchanged
/// here because the observed calls set no flag bits.
fn apply_gte(controls: &mut [u32], data: &mut [u32], call: &DirectionSetup, vector_word: u32) {
    let m = call.rotation;
    controls[0] = pair(m[0][0], m[0][1]);
    controls[1] = pair(m[0][2], m[1][0]);
    controls[2] = pair(m[1][1], m[1][2]);
    controls[3] = pair(m[2][0], m[2][1]);
    controls[4] = i32::from(m[2][2]) as u32;
    data[0] = pair(0, 0);
    data[1] = vector_word;
    for row in 0..3 {
        data[9 + row] = i32::from(call.transformed.saturated[row]) as u32;
        data[25 + row] = call.transformed.vector[row] as u32;
    }
    assert_eq!(call.transformed.flags, 0);
}

#[test]
#[ignore = "requires original assets and passive lighting-001 capture"]
fn original_lighting_block_and_helpers() {
    let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").unwrap());
    let capture = root.join("analysis/flight-runtime/lighting-001");
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
    let original = fs::read(capture.join("lighting.jsonl")).unwrap();
    let rows: Vec<Value> = std::str::from_utf8(&original)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();

    let data = GameData::load(&root.join("extracted/disc")).unwrap();
    let mission = FlightMissionAssets::parse(data.resource(7).unwrap()).unwrap();
    let trig =
        SinCosTable::from_le_bytes(&data.executable.payload[0xa4f84..0xa4f84 + 16384]).unwrap();
    let parameters = LightingParameters::from_assets(&mission).unwrap();
    let setup = initialize_lighting(parameters, &trig);

    let block = rows.iter().find(|r| r["kind"] == "lighting_block").unwrap();
    let section0 = &mission
        .sections()
        .iter()
        .find(|s| s.index == 0)
        .unwrap()
        .bytes()[..0x56];
    assert_eq!(
        section0,
        unhex(block["mission_hex"].as_str().unwrap()),
        "native section 0 lighting parameters"
    );

    // Helper calls in original order: rotation, apply, rotation, apply.
    let rotations: Vec<&Value> = rows
        .iter()
        .filter(|r| r["kind"] == "rotation_yxz")
        .collect();
    let applies: Vec<&Value> = rows
        .iter()
        .filter(|r| r["kind"] == "apply_matrix_sv")
        .collect();
    assert_eq!((rotations.len(), applies.len()), (2, 2));
    let mut vector_word = 0;
    for (index, call) in setup.calls.iter().enumerate() {
        let rotation = rotations[index];
        assert_eq!(
            unhex(rotation["angles_hex"].as_str().unwrap())[..6],
            halves(&parameters.angles[index])
        );
        let input = unhex(rotation["input_hex"].as_str().unwrap());
        let expected = unhex(rotation["expected_hex"].as_str().unwrap());
        assert_eq!(
            expected[..18],
            matrix_bytes(call.rotation),
            "rotation {index}"
        );
        assert_eq!(expected[18..], input[18..], "rotation {index} leaves tail");
        assert_eq!(rotation["input_controls"], rotation["expected_controls"]);
        assert_eq!(rotation["input_data"], rotation["expected_data"]);

        let apply = applies[index];
        assert_eq!(
            unhex(apply["matrix_hex"].as_str().unwrap())[..18],
            matrix_bytes(call.rotation)
        );
        let vector = unhex(apply["vector_hex"].as_str().unwrap());
        assert_eq!(vector[..6], halves(&[0, 0, 4096]));
        vector_word = u32::from_le_bytes(vector[4..8].try_into().unwrap());
        let expected = unhex(apply["expected_hex"].as_str().unwrap());
        assert_eq!(expected[..6], halves(&call.transformed.saturated));
        assert_eq!(
            apply["output"].as_u64().unwrap() - block["gameplay"].as_u64().unwrap(),
            (GAMEPLAY_LIGHTING_OFFSET + index * 6) as u64
        );
        let mut controls = registers(&apply["input_controls"]);
        let mut gte = registers(&apply["input_data"]);
        apply_gte(&mut controls, &mut gte, call, vector_word);
        assert_eq!(
            controls,
            registers(&apply["expected_controls"]),
            "apply {index}"
        );
        assert_eq!(gte, registers(&apply["expected_data"]), "apply {index}");
    }

    // Complete 700-byte gameplay block: native known fields over the input.
    let mut actual = unhex(block["input_hex"].as_str().unwrap());
    assert_eq!(actual.len(), 700);
    let fields = &setup.fields;
    for (i, (&value, &known)) in fields
        .scalar_bytes()
        .iter()
        .zip(fields.scalar_mask())
        .enumerate()
    {
        if known {
            actual[GAMEPLAY_LIGHTING_OFFSET + i] = value;
        }
    }
    let expected = unhex(block["expected_hex"].as_str().unwrap());
    assert_eq!(actual, expected, "all 700 gameplay bytes");
    let changed = actual
        .iter()
        .zip(unhex(block["input_hex"].as_str().unwrap()))
        .filter(|(a, b)| **a != *b)
        .count();
    let mut controls = registers(&block["input_controls"]);
    let mut gte = registers(&block["input_data"]);
    apply_gte(&mut controls, &mut gte, &setup.calls[1], vector_word);
    assert_eq!(controls, registers(&block["expected_controls"]));
    assert_eq!(gte, registers(&block["expected_data"]));
    assert_eq!(PENDING_SUBSYSTEM_CALL_PC, 0x80032990);
    assert_eq!(PENDING_SUBSYSTEM_FUNCTION, 0x8004af9c);

    let known = fields.scalar_mask().iter().filter(|&&v| v).count();
    let report = json!({"source":"passive original lighting-001; no guest code/RAM patching",
        "lighting_blocks":1,"gameplay_bytes_compared":700,"known_lighting_bytes":known,
        "bytes_changed_by_block":changed,"rotation_yxz_calls":2,"apply_matrix_sv_calls":2,
        "gte_registers_per_call":64,"directions":setup.directions,"colors":setup.colors,
        "background":setup.background,"capture_sha256":sha256(&original),
        "scope":"asset-derived 80032878..80032990; ends before 8004AF9C"});
    fs::create_dir_all(root.join("analysis/native-flight")).unwrap();
    fs::write(
        root.join("analysis/native-flight/lighting-validation.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
}
