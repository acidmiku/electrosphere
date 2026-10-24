//! Independent original game pixels. Native output uses assets, never snapshots.
use ac3_menu::{Effect, language_rows};
use ac3_port::{assets::GameData, menu_scene::LanguageScene};
use std::{fs, path::PathBuf};

#[test]
#[ignore = "requires local PAL data and menu runtime capture"]
fn native_language_commands_reproduce_original_frame() {
    let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").unwrap());
    let data = GameData::load(&root.join("extracted/disc")).unwrap();
    let scene = LanguageScene::load(&data).unwrap();
    let image = scene
        .render(&[Effect::DrawLanguageRows {
            rows: language_rows(0, 0),
        }])
        .unwrap();
    let expected =
        fs::read(root.join("analysis/menu-runtime/baseline-001/frame-00240.screen.raw")).unwrap();
    let actual: Vec<u8> = image
        .frame
        .words()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect();
    fs::create_dir_all(root.join("analysis/native-menu")).unwrap();
    fs::write(root.join("analysis/native-menu/frame.raw"), &actual).unwrap();
    fs::write(
        root.join("analysis/native-menu/packets.json"),
        serde_json::to_vec(&image.packets).unwrap(),
    )
    .unwrap();
    let differences = actual
        .chunks_exact(2)
        .zip(expected.chunks_exact(2))
        .filter(|(a, b)| a != b)
        .count();
    assert_eq!(actual.len(), expected.len());
    assert_eq!(
        differences, 0,
        "native frame differs at {differences} pixels"
    );
}

#[test]
#[ignore = "requires original menu input captures"]
fn native_language_logic_matches_original_controller_sequences() {
    let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").unwrap());
    for run in ["navigation-001", "edges-001"] {
        let path = root.join(format!("analysis/menu-runtime/{run}/language-vectors.json"));
        let report = ac3_port::menu_validation::verify_language_trace(&path).unwrap();
        assert_eq!(report["passed"], true, "{report:#}");
    }
    for run in ["continuation-001", "continuation-edges-001", "mission-002"] {
        let report = ac3_port::menu_validation::verify_front_end_trace(
            &root.join(format!("analysis/menu-runtime/{run}/menu-vectors.json")),
        )
        .unwrap();
        assert_eq!(report["passed"], true, "{report:#}");
    }
}

#[test]
#[ignore = "requires original title/main/difficulty frame captures"]
fn native_front_end_frames_reproduce_originals() {
    use ac3_menu::*;
    let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").unwrap());
    let data = GameData::load(&root.join("extracted/disc")).unwrap();
    let scene = LanguageScene::load(&data).unwrap();
    let mut failures = Vec::new();
    for (capture, screen) in [(100, "title"), (240, "main"), (400, "difficulty")] {
        let mut visual = scene.visual_state().unwrap();
        let mut effects = title_progress_effects(&vec![0; 0xf70]).unwrap();
        effects.extend(initialize_title(MenuContext::default()).effects);
        effects.extend(
            tick_title(TitleState::default(), MenuContext::default(), 0)
                .unwrap()
                .effects,
        );
        visual.apply(&effects, 0).unwrap();
        let context = MenuContext {
            progress_flags: 1,
            ..Default::default()
        };
        if screen != "title" {
            visual
                .apply(
                    &initialize_main_menu(MenuState::default(), context).effects,
                    0,
                )
                .unwrap();
        }
        if screen == "difficulty" {
            visual
                .apply(
                    &initialize_difficulty(DifficultyState::default(), context).effects,
                    0,
                )
                .unwrap();
            visual.apply(&difficulty_styles(0), 0).unwrap();
        }
        let ram = fs::read(root.join(format!(
            "analysis/menu-runtime/continuation-001/frame-{capture:05}.ram.bin"
        )))
        .unwrap();
        let counter = i32::from_le_bytes(ram[0xbd178..0xbd17c].try_into().unwrap()) - 1;
        let effects = if screen == "title" {
            vec![Effect::PulseVisualColor {
                slot: 0x78,
                from: [0xf8, 0x98, 0x18],
                to: [0xf8, 0xf8, 0xf0],
            }]
        } else {
            vec![]
        };
        scene
            .render_menu(&mut visual, 0, counter - 1, &effects)
            .unwrap();
        let actual = scene
            .render_menu(&mut visual, 0, counter, &effects)
            .unwrap();
        let out = root.join(format!("analysis/native-menu/{screen}"));
        ac3_port::frontend::save_frame(&actual.frame, &out).unwrap();
        fs::write(
            out.join("packets.json"),
            serde_json::to_vec(&actual.packets).unwrap(),
        )
        .unwrap();
        let expected = fs::read(root.join(format!(
            "analysis/menu-runtime/continuation-001/frame-{capture:05}.screen.raw"
        )))
        .unwrap();
        let expected: Vec<u16> = expected
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect();
        assert_eq!(actual.frame.words().len(), expected.len());
        let different = actual
            .frame
            .words()
            .iter()
            .zip(expected.iter())
            .filter(|(a, b)| a != b)
            .count();
        if different != 0 {
            failures.push(format!("{screen}: {different} pixels"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join(", "));
}
