//! Read-only original whole-callback preparation oracle; generated commands use
//! native parsed assets. Guest addresses occur only in this comparison adapter.
use ac3_port::{assets::GameData, flight_assets, flight_resource_loader, sha256};
use flight_assets::{FlightAircraftAssets, FlightMissionAssets, ResidentSection, TextureGroup};
use flight_resource_loader::*;
use serde_json::{Value, json};
use std::{fs, path::PathBuf};
fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}
fn word(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn set(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}
fn uint(v: &Value) -> u32 {
    v.as_u64().unwrap().try_into().unwrap()
}

#[test]
#[ignore = "requires original assets and preparation-001 passive capture"]
fn original_preparation_order_bindings_counts_and_callback_results() {
    let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").unwrap());
    let capture = root.join("analysis/flight-runtime/preparation-001");
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
    let rows: Vec<Value> = fs::read_to_string(capture.join("preparation.jsonl"))
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert_eq!(rows.len(), 2);
    let data = GameData::load(&root.join("extracted/disc")).unwrap();
    let mission = FlightMissionAssets::parse(data.resource(7).unwrap()).unwrap();
    let aircraft = FlightAircraftAssets::parse(data.resource(48).unwrap()).unwrap();
    let mut reports = Vec::new();
    for row in &rows {
        let (plan, sections, groups): (PreparationPlan, &[ResidentSection], &[TextureGroup]) =
            if row["kind"] == "mission" {
                (
                    mission_preparation(&mission).unwrap(),
                    mission.sections(),
                    mission.texture_groups(),
                )
            } else {
                (
                    aircraft_preparation(&aircraft).unwrap(),
                    aircraft.sections(),
                    aircraft.texture_groups(),
                )
            };
        assert_eq!(plan.kind.original_callback(), uint(&row["entry"]));
        assert_eq!(plan.callback_result, uint(&row["callback_result"]));
        let base = uint(&row["resource_address"]);
        let offsets: Vec<u32> = row["resource_offsets"]
            .as_array()
            .unwrap()
            .iter()
            .map(uint)
            .collect();
        let calls = row["calls"].as_array().unwrap();
        let mut at = 0usize;
        let mut fields = unhex(row["input"]["resource_state_hex"].as_str().unwrap());
        let mut counts = unhex(row["input"]["context_counts_hex"].as_str().unwrap());
        let mut arena = uint(&row["input"]["arena_base"]);
        let mut render = unhex(row["input"]["render_hex"].as_str().unwrap());
        let mut polls = 0;
        let mut temporary = 0;
        let address = |s: usize, offset: usize| {
            base + u32::try_from(sections[s].allocation_offset + offset).unwrap()
        };
        let mut next = |pc: u32, args: &[u32]| -> &Value {
            let c = &calls[at];
            assert_eq!(uint(&c["address"]), pc, "call{at}");
            for (i, arg) in args.iter().enumerate() {
                assert_eq!(uint(&c["arguments"][i]), *arg, "call{at} argument{i}");
            }
            at += 1;
            c
        };
        for command in &plan.commands {
            use PreparationCommand as C;
            match *command {
                C::ResolveResourceDirectory => {
                    assert_eq!(uint(&next(0x80026e98, &[base])["result"]), base + 4);
                }
                C::BindSoundBank { child } => {
                    next(0x8002d544, &[base + offsets[child]]);
                }
                C::UploadTextureGroup { child } => {
                    let group = groups.iter().find(|g| g.section == child).unwrap();
                    let call = next(
                        0x80026e50,
                        &[base + offsets[child], base + offsets[child + 1]],
                    );
                    assert_eq!(
                        uint(&call["result"]),
                        base + offsets[child + 1] + group.bytes.len() as u32
                    );
                }
                C::SynchronizeGpu => {
                    next(0x800a6664, &[0]);
                }
                C::PreserveCompressedPrefix {
                    until_child,
                    pointer_count,
                } => {
                    assert_eq!(pointer_count, sections.len());
                    let bytes = (offsets[until_child] - 4) & !3;
                    temporary = 0x801fb700 - bytes;
                    assert_eq!(
                        uint(&next(0x800270fc, &[temporary, base + 4, bytes / 4])["result"]),
                        0x801fb700
                    );
                }
                C::ExpandSection { section } => {
                    let call = next(
                        0x800ae118,
                        &[temporary + offsets[section] - 4, address(section, 0)],
                    );
                    assert_eq!(
                        uint(&call["result"]),
                        address(section, sections[section].bytes().len())
                    );
                }
                C::BindResource {
                    offset,
                    address: target,
                } => set(
                    &mut fields,
                    usize::from(offset),
                    address(target.section, target.offset),
                ),
                C::SetContextCount { offset, count } => {
                    set(&mut counts, usize::from(offset) - 0xf80, count)
                }
                C::RelocateDirectory { section } => {
                    assert_eq!(
                        uint(&next(0x80026e98, &[address(section, 0)])["result"]),
                        address(section, 4)
                    );
                }
                C::RelocateRecords {
                    section,
                    fields,
                    logical_records,
                } => {
                    let directory = sections[section].directory.as_ref().unwrap();
                    assert_eq!(logical_records, directory.slots.len());
                    for offset in &directory.slots {
                        let start = address(section, *offset);
                        assert_eq!(
                            uint(&next(0x80026ee0, &[start, start + 4, fields as u32])["result"]),
                            start
                        );
                    }
                }
                C::ReplaceObjectSlots {
                    first,
                    source_section,
                    count,
                } => {
                    let dir = sections[source_section].directory.as_ref().unwrap();
                    assert_eq!(dir.slots.len(), count);
                    for (i, offset) in dir.slots.iter().enumerate() {
                        assert_eq!(
                            mission.object_slots()[first + i],
                            flight_assets::RecordRef {
                                section: source_section,
                                offset: *offset
                            }
                        );
                    }
                }
                C::PrepareMissionPalettes => {
                    next(
                        0x8005d490,
                        &[
                            address(0, 8),
                            address(0, 0x78),
                            word(sections[0].bytes(), 0x74),
                        ],
                    );
                }
                C::SetArenaEnd { allocation_bytes } => {
                    arena = base + allocation_bytes as u32;
                    assert_eq!(uint(&next(0x80027098, &[arena])["result"]), arena);
                }
                C::RecordArenaTop {
                    resource_offset,
                    allocation_bytes,
                } => {
                    assert_eq!(arena, base + allocation_bytes as u32);
                    let end = uint(&next(0x80026fd8, &[0])["result"]);
                    assert_eq!(end, arena);
                    set(&mut fields, usize::from(resource_offset), end);
                }
                C::AdvanceLoadingScreen => {
                    let poll = next(0x800354b8, &[]);
                    polls += 1;
                    render = unhex(poll["expected"]["render_hex"].as_str().unwrap());
                }
                C::ResetRenderStatus { flags_or } => {
                    let flags = word(&render, 0) | flags_or;
                    set(&mut render, 0, flags);
                    set(&mut render, 4, 0);
                    set(&mut render, 20, 0);
                }
            }
        }
        assert_eq!(at, calls.len());
        assert_eq!(polls, 1);
        assert_eq!(
            fields,
            unhex(row["expected"]["resource_state_hex"].as_str().unwrap())
        );
        assert_eq!(
            counts,
            unhex(row["expected"]["context_counts_hex"].as_str().unwrap())
        );
        assert_eq!(arena, uint(&row["expected"]["arena_base"]));
        assert_eq!(
            row["input"]["arena_current"],
            row["expected"]["arena_current"]
        );
        assert_eq!(
            render,
            unhex(row["expected"]["render_hex"].as_str().unwrap())
        );
        reports.push(json!({"kind":row["kind"],"frame":row["frame"],"original_direct_calls":at,"commands":plan.commands.len(),"polls":polls,"resident_bytes":arena-base,"callback_result":plan.callback_result}));
    }
    fs::create_dir_all(root.join("analysis/native-flight")).unwrap();
    let hashes: std::collections::BTreeMap<_, _> = [
        "src/flight_resource_loader.rs",
        "src/flight_assets.rs",
        "tests/flight_resource_loader.rs",
    ]
    .into_iter()
    .map(|p| (p, sha256(&fs::read(root.join(p)).unwrap())))
    .collect();
    fs::write(root.join("analysis/native-flight/preparation-validation.json"),serde_json::to_vec_pretty(&json!({"passed":true,"cases":reports,"source_sha256":hashes,"capture_sha256":sha256(&fs::read(capture.join("preparation.jsonl")).unwrap()),"provenance":metadata,"scope":"Ordered preparation commands, all direct original calls/meaningful arguments, allocation ends, resident resource fields/counts and fixedcallback return. The loading-screen poll's render result is an explicit observed dependency here; FlightLoader validates its native implementation separately. No gameplay initialization or frame-budget timing inferred."})).unwrap()).unwrap();
}
