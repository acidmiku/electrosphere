//! Rasterizer oracle only: original packets, texture VRAM and DRAWENV inputs.
use ac3_graphics::{Frame, Vram, render_packets_with_transfers};
use serde_json::{Value, json};
use std::{fs, path::PathBuf};

#[test]
#[ignore = "requires original briefing GPU/VRAM captures"]
fn original_briefing_packets_match_pixels() {
    let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").unwrap());
    let capture = root.join("analysis/menu-runtime/briefing-scene-001");
    let mut reports = Vec::new();
    for (number, ot) in [
        (100, "ot-00099-001.json"),
        (700, "ot-00699-003.json"),
        (850, "ot-00849-005.json"),
    ] {
        let bytes = fs::read(capture.join(format!("frame-{number:05}.vram.bin"))).unwrap();
        let mut vram = Vram::from_le_bytes(&bytes[..1024 * 512 * 2]).unwrap();
        let value: Value = serde_json::from_slice(&fs::read(capture.join(ot)).unwrap()).unwrap();
        let packets: Vec<Vec<u32>> = value["packets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                p["words"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_u64().unwrap() as u32)
                    .collect()
            })
            .collect();
        let ram = fs::read(capture.join(format!("frame-{number:05}.ram.bin"))).unwrap();
        let word = |at| u32::from_le_bytes(ram[at..at + 4].try_into().unwrap());
        let fill = word(0xbb048);
        assert_eq!(fill >> 24, 2);
        assert_eq!(word(0xbb04c), 0);
        assert_eq!(word(0xbb050), 0x01ff0140);
        let mut frame = Frame::new(320, 512).unwrap();
        frame.draw_tile(
            0,
            0,
            320,
            511,
            [fill as u8, (fill >> 8) as u8, (fill >> 16) as u8],
            None,
        );
        let primitives = render_packets_with_transfers(&mut frame, &mut vram, &packets).unwrap();
        let original = fs::read(capture.join(format!("frame-{number:05}.screen.raw"))).unwrap();
        let expected: Vec<u16> = original
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes(b.try_into().unwrap()))
            .collect();
        assert_eq!(frame.words().len(), expected.len());
        let differences: Vec<_> = frame
            .words()
            .iter()
            .zip(&expected)
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .collect();
        eprintln!(
            "frame{number}: {} differences; first {:?}",
            differences.len(),
            differences
                .iter()
                .take(20)
                .map(|(i, (a, b))| (i % 320, i / 320, a, b))
                .collect::<Vec<_>>()
        );
        ac3_port::frontend::save_frame(
            &frame,
            &root.join(format!("analysis/native-mission/briefing-replay-{number}")),
        )
        .unwrap();
        reports.push(json!({"frame":number,"ot":ot,"pixels":frame.words().len(),"different_pixels":differences.len(),"primitives":primitives}));
    }
    let passed = reports.iter().all(|r| r["different_pixels"] == 0);
    fs::write(root.join("analysis/native-mission/briefing-pixel-validation.json"),serde_json::to_vec_pretty(&json!({"passed":passed,"frames":reports,"scope":"Rasterizer only: captured GPU packets, VRAM and DRAWENV. Does not validate native briefing packet generation or state evolution."})).unwrap()).unwrap();
    assert!(passed);
}
