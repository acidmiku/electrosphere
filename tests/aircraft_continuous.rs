//! Continuous asset-initialized loading -> hub -> briefing -> aircraft/launch.
//! The original captures form one uninterrupted game timeline despite checkpoint
//! boundaries. Only controller inputs and independently established elapsed time
//! enter generation; original RAM, packets and pixels are comparison evidence.
use ac3_mission::MissionContext;
use ac3_port::{assets::GameData, mission_runtime::MissionRuntime};
use serde_json::{Value, json};
use std::{fs, path::PathBuf};

fn normalize(mut p: Vec<u32>) -> Vec<u32> {
    if matches!(p[0] >> 24, 0x2c | 0x2e) {
        p[6] &= 65535;
        p[8] &= 65535;
    }
    p
}
#[test]
#[ignore = "requires original assets and aircraft-visual-002 capture"]
fn continuous_native_aircraft_packets_and_pixels() {
    let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").unwrap());
    let capture = root.join("analysis/menu-runtime/aircraft-visual-002");
    let metadata: Value =
        serde_json::from_slice(&fs::read(capture.join("metadata.json")).unwrap()).unwrap();
    let addon: Value =
        serde_json::from_slice(&fs::read(capture.join("addon-metadata.json")).unwrap()).unwrap();
    assert_eq!(
        metadata["original_disc_sha256"],
        "5d59ef5bf2b905cde40cf8a14647b409228a4443eb94e9a0444351544ca87276"
    );
    assert_eq!(metadata["guest_code_patched"], false);
    assert_eq!(metadata["guest_ram_patched"], false);
    assert_eq!(
        metadata["engine_sha256"],
        ac3_port::sha256(&fs::read(capture.join("capture-engine.lua")).unwrap())
    );
    assert_eq!(
        addon["sha256"],
        ac3_port::sha256(&fs::read(capture.join("capture-addon.lua")).unwrap())
    );
    let data = GameData::load(&root.join("extracted/disc")).unwrap();
    let mut bytes = vec![0; 0xf70];
    ac3_menu::reset_new_game_progress(&mut bytes).unwrap();
    bytes[0x390..0x392].copy_from_slice(&1i16.to_le_bytes());
    MissionContext {
        options: 0x42,
        progress_flags: 0x13,
        current_pair: [1, 1],
        ..Default::default()
    }
    .write_into(&mut bytes)
    .unwrap();
    let mut runtime = MissionRuntime::new(&data, bytes, 0).unwrap();
    let mut loader_ticks = 0;
    while runtime.state.state == 14 {
        runtime.step(loader_ticks, 0, 1).unwrap();
        loader_ticks += 1;
    }
    assert_eq!(runtime.state.state, 0);
    // Native synchronous file reads complete earlier than original CD transfers.
    // Align only the independent background clock with this original checkpoint.
    let native_inactive = runtime.scene.map.state.frame;
    while runtime.scene.map.state.frame < 187 {
        assert!(runtime.scene.map.tick([0; 3]).unwrap().emitted.is_empty());
    }
    for frame in 0..305 {
        runtime.step(frame, 0, 1).unwrap();
    }
    assert_eq!(runtime.scene.map.state.frame, 492);
    let clock: Value = serde_json::from_slice(
        &fs::read(
            root.join("analysis/menu-runtime/hub-render-001/render-00000-callback-entry.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let clock_start = clock["context_frame"].as_i64().unwrap() as i32;
    let mut reports = Vec::new();
    let mut aircraft_init = None;
    for frame in 0..=1678 {
        // +0x124 remains1 in this normal controller run; +0x12C is the
        // original processed press/release word, including upper repeat bits.
        let pressed = match frame {
            2 | 1202 | 1522 | 1602 | 1642 => 0x810,
            602 => 0x50,
            1442 => 0x4000008,
            1482 => 0x2000004,
            1562 => 0x20,
            1566 => 0x200000,
            _ => 0,
        };
        let held = 1;
        if runtime.state.state == 2 && runtime.state.substate == 0 {
            aircraft_init = Some(frame);
        }
        let (image, trace) = runtime
            .step(clock_start + frame, pressed, held)
            .unwrap_or_else(|e| panic!("frame{frame}: {e}"));
        assert!(
            trace["unhandled_effects"].as_array().unwrap().is_empty(),
            "frame{frame}: {trace}"
        );
        let local_frame = frame - 1200;
        let screenshot = local_frame + 2;
        if ![40, 100, 240, 330, 360, 400, 440, 480].contains(&screenshot) {
            continue;
        }
        let otfile = fs::read_dir(&capture)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(&format!("ot-{:05}-", local_frame + 1))
            })
            .unwrap();
        let original: Value = serde_json::from_slice(&fs::read(&otfile).unwrap()).unwrap();
        let packets: Vec<Vec<u32>> = original["packets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                normalize(
                    p["words"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|v| v.as_u64().unwrap() as u32)
                        .collect(),
                )
            })
            .collect();
        assert_eq!(
            image.packets.len() + 3,
            packets.len(),
            "frame{frame} packet count"
        );
        for (i, (a, b)) in image
            .packets
            .iter()
            .cloned()
            .map(normalize)
            .zip(&packets)
            .enumerate()
        {
            assert_eq!(&a, b, "frame{frame} packet{i}");
        }
        let actual: Vec<u8> = image
            .frame
            .words()
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let expected = fs::read(capture.join(format!("frame-{screenshot:05}.screen.raw"))).unwrap();
        assert_eq!(actual.len(), expected.len());
        let differences = actual
            .chunks_exact(2)
            .zip(expected.chunks_exact(2))
            .filter(|(a, b)| a != b)
            .count();
        ac3_port::frontend::save_frame(
            &image.frame,
            &root.join(format!(
                "analysis/native-mission/aircraft-native-{screenshot}"
            )),
        )
        .unwrap();
        assert_eq!(differences, 0, "frame{frame} native pixel differences");
        reports.push(json!({"callback":local_frame,"screenshot":screenshot,"packets":image.packets.len(),"pixels":320*512,"different_pixels":differences,"sha256":ac3_port::sha256(&actual)}));
    }
    assert_eq!(aircraft_init, Some(1230));
    assert_eq!(reports.len(), 8);
    let sources: std::collections::BTreeMap<_, _> = [
        "src/aircraft_scene.rs",
        "src/mission_runtime.rs",
        "src/menu_scene.rs",
        "src/mission_map.rs",
        "src/mission_hub.rs",
        "src/briefing_scene.rs",
        "crates/ac3-mission/src/aircraft.rs",
        "tests/aircraft_continuous.rs",
    ]
    .into_iter()
    .map(|path| (path, ac3_port::sha256(&fs::read(root.join(path)).unwrap())))
    .collect();
    fs::write(root.join("analysis/native-mission/aircraft-continuous-validation.json"),serde_json::to_vec_pretty(&json!({"passed":true,"platform":std::env::consts::OS,"source_sha256":sources,"capture_metadata_sha256":ac3_port::sha256(&fs::read(capture.join("metadata.json")).unwrap()),"frames":reports,"native_loader_ticks":loader_ticks,"native_inactive_ticks":native_inactive,"aligned_inactive_ticks":187,"hub_active_ticks":305,"aircraft_init_frame":aircraft_init,"scope":"Continuous native resource loading, hub, briefing, aircraft and launch generation from game assets plus normal controller input and original elapsed-count alignment. No captured state/VRAM/commands/pixels used for generation; original packets/pixels are expectations only. Audio/CD timing excluded."})).unwrap()).unwrap();
}
