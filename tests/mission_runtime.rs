use ac3_port::{assets::GameData, frontend::run_menu};
use std::path::PathBuf;

#[test]
#[ignore = "requires verified original assets"]
fn aircraft_weapon_choice_and_launch_load_resident_flight_assets() {
    let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").unwrap());
    let data = GameData::load(&root.join("extracted/disc")).unwrap();
    let output = run_menu(&data,true,Some(1200),
        "40:confirm,110:start,160:confirm,200:down,220:confirm,390:right,410:left,450:confirm,650:start,750:confirm,850:confirm,950:confirm",None).unwrap();
    assert_eq!(output["status"], "flight_gameplay_initialization_pending");
    assert_eq!(output["frames"], 1035);
    assert_eq!(output["resident_state"], 2);
    assert_eq!(output["resident_substate"], 6);
    let trace = output["trace"].as_array().unwrap();
    assert_eq!(trace[777]["stream_request"]["resource"], 0x81d);
    assert_eq!(trace[778]["aircraft_id"], 1);
    assert_eq!(trace[778]["texture_uploads"], 4);
    assert_eq!(trace[903]["aircraft_phase"], 1);
    assert_eq!(trace[903]["aircraft_substate"], 2);
    assert_eq!(trace[976]["stream_request"]["status"], "stopped");
    assert_eq!(trace[977]["begin_launch_background_fade"], true);
    assert_eq!(trace[1028]["resident_result"], 0x60000002_u32);
    let resident = &trace[1029..];
    assert_eq!(resident[0]["random_state"], 0x668a4953_u32);
    assert_eq!(resident[0]["reset_resident_voices"], true);
    assert!(resident.iter().all(|t| t["background_audio"].is_null()));
    assert!(
        resident
            .iter()
            .all(|t| t["frame_width"] == 320 && t["frame_height"] == 256)
    );
    assert_eq!(resident[2]["preparation"]["texture_uploads"], 594);
    assert_eq!(resident[2]["preparation"]["gpu_syncs"], 5);
    assert_eq!(resident[2]["preparation"]["sound_bank"]["bytes"], 199968);
    assert_eq!(resident[5]["preparation"]["texture_uploads"], 4);
    assert_eq!(resident[5]["preparation"]["gpu_syncs"], 1);
    assert_eq!(resident[5]["preparation"]["sound_bank"]["bytes"], 24688);
    assert_eq!(resident[5]["render_flags"], 13);
    assert!(
        trace
            .iter()
            .filter_map(|t| t["unhandled_effects"].as_array())
            .all(|v| v.is_empty())
    );
    assert_eq!(output["stream_audio"]["frames"], (300 + 199) * 882);
    assert_eq!(output["stream_audio"]["stream"]["starts"], 2);
    assert_eq!(output["stream_audio"]["stream"]["active"], false);
    let sounds = output["sound_events"].as_array().unwrap();
    assert!(sounds.iter().all(|s| s["status"] == "rendered"));
    assert_eq!(
        sounds.iter().find(|s| s["id"] == 0x63).unwrap()["frames"],
        315382
    );
}

#[test]
#[ignore = "requires verified original assets"]
fn native_briefing_fast_forward_and_confirm_reach_aircraft() {
    let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").unwrap());
    let data = GameData::load(&root.join("extracted/disc")).unwrap();
    let output = run_menu(&data, true, Some(777),
        "40:confirm,110:start,160:confirm,200:down,220:confirm,390:right,410:left,450:confirm,650:start,750:confirm", None).unwrap();
    assert_eq!(output["status"], "frame_limit");
    assert_eq!(output["frames"], 777);
    assert_eq!(output["mission_state"], 2);
    let trace = output["trace"].as_array().unwrap();
    assert_eq!(trace[476]["stream_request"]["resource"], 0x827);
    assert_eq!(trace[476]["stream_request"]["status"], "started");
    assert_eq!(trace[776]["stream_request"]["action"], "stop");
    assert_eq!(trace[776]["stream_request"]["status"], "stopped");
    assert_eq!(output["stream_audio"]["frames"], 300 * 882);
    assert_eq!(output["stream_audio"]["stream"]["starts"], 1);
    assert_eq!(output["stream_audio"]["stream"]["active"], false);
    assert_eq!(output["stream_audio"]["stream"]["source_verified"], true);
    assert!(trace[..476].iter().all(|t| t["stream_audio"].is_null()));
    assert!(
        trace[476..776]
            .iter()
            .all(|t| t["stream_audio"]["frames"] == 882)
    );
    assert!(trace[776]["stream_audio"].is_null());
    assert_eq!(trace[720]["briefing_phase"], 2);
    assert!(
        trace
            .iter()
            .filter_map(|t| t["unhandled_effects"].as_array())
            .all(|v| v.is_empty())
    );
    let sounds = output["sound_events"].as_array().unwrap();
    assert!(sounds.iter().all(|s| s["status"] == "rendered"));
    for id in [0x64, 0x65, 0x66] {
        assert!(sounds.iter().any(|s| s["id"] == id));
    }
}

#[test]
#[ignore = "requires verified original assets"]
fn new_game_loads_hub_and_routes_mission_selection() {
    let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").unwrap());
    let data = GameData::load(&root.join("extracted/disc")).unwrap();
    let output = run_menu(
        &data,
        true,
        Some(476),
        "40:confirm,110:start,160:confirm,200:down,220:confirm,390:right,410:left,450:confirm",
        None,
    )
    .unwrap();
    assert_eq!(output["status"], "frame_limit");
    assert_eq!(output["mission_state"], 1);
    let trace = output["trace"].as_array().unwrap();
    let mission: Vec<_> = trace
        .iter()
        .filter(|t| t.get("mission_state").is_some())
        .collect();
    assert!(!mission.is_empty());
    assert_eq!(mission[0]["mission_state"], 14);
    assert!(
        mission
            .iter()
            .all(|t| t["unhandled_effects"].as_array().unwrap().is_empty())
    );
    assert_eq!(
        mission
            .iter()
            .map(|t| t["texture_uploads"].as_u64().unwrap())
            .sum::<u64>(),
        712
    );
    let ready = mission.iter().find(|t| t["mission_state"] == 0).unwrap();
    assert_eq!(ready["loader_total_bytes"], 722660);
    assert_eq!(ready["loader_loaded_bytes"], 722660);
    // Loading duration follows real readiness and ten-TIM queue polls. The
    // fixture's 187 inactive callbacks are deliberately not a production delay.
    assert_eq!(ready["map_frame"], 58);
    assert_eq!(trace[389]["hub_selection"], 1);
    assert_eq!(trace[409]["hub_selection"], 0);
    let bank_frame = mission
        .iter()
        .find(|t| t["load_mission_sound_bank"] == true)
        .unwrap()["frame"]
        .as_u64()
        .unwrap();
    let sounds = output["sound_events"].as_array().unwrap();
    assert!(sounds.iter().all(|e| e["status"] == "rendered"));
    let loading_cue = sounds.iter().find(|e| e["id"] == 0x56).unwrap();
    assert!(loading_cue["frame"].as_u64().unwrap() < bank_frame);
    let init_frame = mission
        .iter()
        .find(|t| t["initialize_background_voices"] == true)
        .unwrap()["frame"]
        .as_u64()
        .unwrap();
    let starts: Vec<_> = mission
        .iter()
        .filter(|t| t["background_audio"]["started"] == true)
        .collect();
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0]["frame"], init_frame + 1);
    assert!(
        starts[0]["background_audio"]["volume_writes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|w| w["gain"] == 0)
    );
    let writes: Vec<_> = mission
        .iter()
        .filter_map(|t| t["background_audio"]["volume_writes"].as_array())
        .flatten()
        .collect();
    assert_eq!(writes.last().unwrap()["gain"], 125);
    assert_eq!(
        output["background_audio"]["frames"],
        (trace.len() as u64 - bank_frame + 1) * 882
    );
    assert!(
        sounds
            .iter()
            .any(|e| e["id"] == 0x5a && e["frame"].as_u64().unwrap() > bank_frame)
    );
}
