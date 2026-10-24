use ac3_port::{assets::GameData, mission_assets::MissionAssets, sha256};
use std::{fs, path::PathBuf};

#[test]
#[ignore = "requires original mission text-draw trace and assets"]
fn native_mission_font_matches_original_packets() {
    use ac3_formats::dot1;
    use ac3_port::menu_scene::LanguageScene;
    let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").unwrap());
    let data = GameData::load(&root.join("extracted/disc")).unwrap();
    let assets = MissionAssets::load(&data, 74).unwrap();
    let text = dot1::parse(assets.strings.child(0).unwrap())
        .unwrap()
        .child(185)
        .unwrap();
    let scene = LanguageScene::from_mission(&data, assets).unwrap();
    let mut native = Vec::new();
    scene.font_text(16, text, 195, 56, &mut native).unwrap();
    let manifest: serde_json::Value = serde_json::from_slice(
        &fs::read(root.join("analysis/menu-runtime/hub-render-001/render-manifest.json")).unwrap(),
    )
    .unwrap();
    let call = &manifest["text"]["observed_draws"][0];
    assert_eq!(call["text"], "Mission ");
    assert_eq!(call["font"], 16);
    assert_eq!(call["x"], 195);
    assert_eq!(call["y"], 56);
    // This normal-frame trace records the output pointer before the original
    // text routine. The observer's next RAM dump retains its generated list.
    let ram =
        fs::read(root.join("analysis/menu-runtime/hub-render-001/frame-00001.ram.bin")).unwrap();
    let mut cursor = call["packet"].as_u64().unwrap() as usize & 0x1fffff;
    for (index, packet) in native.iter().enumerate() {
        let tag = u32::from_le_bytes(ram[cursor..cursor + 4].try_into().unwrap());
        let length = (tag >> 24) as usize;
        assert_eq!(length, packet.len(), "packet {index} length");
        let original: Vec<u32> = ram[cursor + 4..cursor + 4 + length * 4]
            .chunks_exact(4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
            .collect();
        assert_eq!(packet, &original, "packet {index}");
        cursor += 4 + length * 4;
    }
    assert_eq!(native.len(), 9);
}

#[test]
#[ignore = "requires original mission hub packets and VRAM"]
fn mission_hub_gpu_packets_match_original_pixels() {
    let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").unwrap());
    let capture = root.join("analysis/menu-runtime/mission-002");
    let bytes = fs::read(capture.join("frame-00600.vram.bin")).unwrap();
    let mut vram = ac3_graphics::Vram::from_le_bytes(&bytes[..1024 * 512 * 2]).unwrap();
    let ot: serde_json::Value =
        serde_json::from_slice(&fs::read(capture.join("ot-00599-011.json")).unwrap()).unwrap();
    let packets: Vec<Vec<u32>> = ot["packets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            p["words"]
                .as_array()
                .unwrap()
                .iter()
                .map(|w| w.as_u64().unwrap() as u32)
                .collect()
        })
        .collect();
    let mut frame = ac3_graphics::Frame::new(320, 512).unwrap();
    // PutDrawEnv submits a direct fill outside DrawOTag. The separate
    // hub-render trace identifies the DRAWENV at800BB010, retained in this RAM.
    let ram = fs::read(capture.join("frame-00600.ram.bin")).unwrap();
    let environment: Vec<u32> = ram[0xbb030..0xbb054]
        .chunks_exact(4)
        .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
        .collect();
    assert_eq!(environment[6] >> 24, 2);
    assert_eq!(environment[7], 0);
    assert_eq!(environment[8], 0x01ff0140);
    let rgb = environment[6];
    frame.draw_tile(
        0,
        0,
        320,
        511,
        [rgb as u8, (rgb >> 8) as u8, (rgb >> 16) as u8],
        None,
    );
    let primitives =
        ac3_graphics::render_packets_with_transfers(&mut frame, &mut vram, &packets).unwrap();
    let actual: Vec<u8> = frame.words().iter().flat_map(|w| w.to_le_bytes()).collect();
    let expected = fs::read(capture.join("frame-00600.screen.raw")).unwrap();
    assert_eq!(actual.len(), expected.len());
    let differences: Vec<_> = actual
        .chunks_exact(2)
        .zip(expected.chunks_exact(2))
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .collect();
    fs::create_dir_all(root.join("analysis/native-mission")).unwrap();
    fs::write(
        root.join("analysis/native-mission/hub-packet-replay.raw"),
        &actual,
    )
    .unwrap();
    fs::write(root.join("analysis/native-mission/packet-validation.json"),serde_json::to_vec_pretty(&serde_json::json!({"scope":"Renderer only: captured GPU commands/VRAM, not native mission scene", "primitives":primitives,"pixels":frame.words().len(),"different_pixels":differences.len()})).unwrap()).unwrap();
    eprintln!(
        "first differing pixels: {:?}",
        differences
            .iter()
            .take(20)
            .map(|(i, (a, b))| (i % 320, i / 320, a, b))
            .collect::<Vec<_>>()
    );
    assert_eq!(differences.len(), 0);
}

#[test]
#[ignore = "requires original first-mission resource upload capture"]
fn native_mission_texture_loads_match_original_uploads() {
    let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").unwrap());
    let data = GameData::load(&root.join("extracted/disc")).unwrap();
    let assets = MissionAssets::load(&data, 74).unwrap();
    let captured: Vec<serde_json::Value> = serde_json::from_slice(
        &fs::read(root.join("analysis/menu-runtime/mission-002/uploads.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(assets.uploads.len(), 712);
    assert_eq!(captured.len(), assets.uploads.len());
    let mut bytes = 0;
    for (index, (native, original)) in assets.uploads.iter().zip(captured.iter()).enumerate() {
        for (key, value) in [
            ("x", native.x),
            ("y", native.y),
            ("width", native.width),
            ("height", native.height),
        ] {
            assert_eq!(original[key], value, "upload {index} {key}");
        }
        assert_eq!(
            original["bytes"],
            native.data.len(),
            "upload {index} length"
        );
        assert_eq!(
            original["sha256"],
            sha256(&native.data),
            "upload {index} payload"
        );
        bytes += native.data.len();
    }
    let report = serde_json::json!({"passed":true,"uploads":assets.uploads.len(),"payload_bytes":bytes,
        "original":"analysis/menu-runtime/mission-002/uploads.json","mission_resource":74,"common_resource":72,
        "scope":"Native TIM/Ulz payloads, coordinates and order; dynamic map transfers and final mission frame remain separate."});
    fs::create_dir_all(root.join("analysis/native-mission")).unwrap();
    fs::write(
        root.join("analysis/native-mission/texture-validation.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
}
