//! Bridge diagnostics: the native front end's game context at the resident
//! state-2 hand-over, compared with the original at 800355F0 entry
//! (`analysis/flight-runtime/inputs-002`). Writes a report of every
//! differing byte and whether the flight engine reads it before writing.
use ac3_port::{
    assets::GameData, frontend::parse_inputs, menu_runtime::MenuRuntime,
    mission_runtime::MissionRuntime,
};
use serde_json::{Value, json};
use std::{collections::BTreeSet, fs, path::PathBuf};

const CONTEXT: usize = 0x0bd158;

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/inputs-002"]
fn native_context_at_state_two_entry() {
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
    let native = mission.game_context_bytes();
    let capture = root.join("analysis/flight-runtime/inputs-002");
    let ram = fs::read(capture.join("input-start.ram.bin")).unwrap();
    let original = &ram[CONTEXT..CONTEXT + native.len()];
    let mut read_first = BTreeSet::new();
    for line in fs::read_to_string(capture.join("input-set.jsonl"))
        .unwrap()
        .lines()
    {
        let row: Value = serde_json::from_str(line).unwrap();
        let start = row["start"].as_u64().unwrap() as usize & 0x1fffff;
        for i in 0..row["length"].as_u64().unwrap() as usize {
            read_first.insert(start + i);
        }
    }
    let differing: Vec<usize> = (0..native.len())
        .filter(|&i| native[i] != original[i])
        .collect();
    let inputs_differing: Vec<usize> = differing
        .iter()
        .copied()
        .filter(|&i| read_first.contains(&(CONTEXT + i)))
        .collect();
    let describe = |offsets: &[usize]| -> Vec<Value> {
        offsets
            .iter()
            .map(|&i| json!({"offset": format!("{i:#x}"), "native": native[i], "original": original[i]}))
            .collect()
    };
    println!(
        "context bytes {}, differing {}, differing flight inputs {}: {:?}",
        native.len(),
        differing.len(),
        inputs_differing.len(),
        inputs_differing
            .iter()
            .map(|i| format!("{i:#x}"))
            .collect::<Vec<_>>()
    );
    let report = json!({"context_bytes": native.len(), "differing": describe(&differing),
        "differing_flight_inputs": describe(&inputs_differing),
        "source": "native front end at frame 1029 vs original at 800355F0 entry (inputs-002)"});
    fs::create_dir_all(root.join("analysis/native-flight")).unwrap();
    fs::write(
        root.join("analysis/native-flight/bridge-context.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
}
