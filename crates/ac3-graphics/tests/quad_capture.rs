use ac3_graphics::{Frame, Vram, render_packets};
use std::{fs, path::PathBuf};

#[test]
#[ignore = "requires AC3_DATA_DIR and original menu continuation captures"]
fn original_main_and_difficulty_packets_match() {
    let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR required"));
    let capture = root.join("analysis/menu-runtime/continuation-001");
    let mut total_differences = 0;
    for (frame_number, ot_name) in [
        (100, "ot-00099-003.json"),
        (160, "ot-00159-005.json"),
        (240, "ot-00239-007.json"),
        (400, "ot-00399-011.json"),
        (600, "ot-00599-013.json"),
        (900, "ot-00899-015.json"),
    ] {
        let bytes = fs::read(capture.join(format!("frame-{frame_number:05}.vram.bin"))).unwrap();
        let vram = Vram::from_le_bytes(&bytes[..1024 * 512 * 2]).unwrap();
        let json: serde_json::Value =
            serde_json::from_slice(&fs::read(capture.join(ot_name)).unwrap()).unwrap();
        let packets: Vec<Vec<u32>> = json["packets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|packet| {
                packet["words"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_u64().unwrap() as u32)
                    .collect()
            })
            .collect();
        let mut frame = Frame::new(320, 512).unwrap();
        render_packets(&mut frame, &vram, &packets).unwrap();
        let expected =
            fs::read(capture.join(format!("frame-{frame_number:05}.screen.raw"))).unwrap();
        let expected: Vec<u16> = expected
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect();
        let differences: Vec<_> = frame
            .words()
            .iter()
            .zip(&expected)
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .collect();
        eprintln!(
            "frame {frame_number}: {} differences; first {:?}",
            differences.len(),
            differences
                .iter()
                .take(20)
                .map(|(i, (a, b))| (i % 320, i / 320, format!("{a:04x}"), format!("{b:04x}")))
                .collect::<Vec<_>>()
        );
        total_differences += differences.len();
    }
    assert_eq!(total_differences, 0);
}
