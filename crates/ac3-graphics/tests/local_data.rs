use ac3_graphics::{BlendMode, Frame, Sprite, Vram, tim};
use std::{fs, path::PathBuf};

#[test]
#[ignore = "requires AC3_DATA_DIR and original-game menu VRAM capture"]
fn all_menu_tim_uploads_match_original_game_vram() {
    let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR is required"));
    let archives = root.join("extracted/archives");
    let mut paths: Vec<_> = fs::read_dir(&archives)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("bpb_0000")
        })
        .filter(|path| fs::read(path).unwrap().starts_with(&[16, 0, 0, 0]))
        .collect();
    paths.sort();
    let mut menu: Vec<_> = fs::read_dir(&archives)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("bpb_0069_0003_")
        })
        .collect();
    menu.sort();
    paths.extend(menu);
    assert_eq!(paths.len(), 118);
    let mut vram = Vram::new();
    let mut written = vec![false; 1024 * 512];
    for path in paths {
        let bytes = fs::read(path).unwrap();
        let image = tim::parse(&bytes).unwrap();
        assert_eq!(image.depth, tim::Depth::Indexed4);
        vram.load_tim(&image).unwrap();
        for block in [Some(image.pixels), image.clut].into_iter().flatten() {
            for row in 0..usize::from(block.height) {
                let start = (usize::from(block.y) + row) * 1024 + usize::from(block.x);
                written[start..start + usize::from(block.width)].fill(true);
            }
        }
    }
    let actual =
        fs::read(root.join("analysis/menu-runtime/baseline-001/frame-00240.vram.bin")).unwrap();
    // Redux has a 1024-row backing allocation; the retail 1-MiB bank is first.
    let actual = Vram::from_le_bytes(&actual[..1024 * 512 * 2]).unwrap();
    let mut count = 0;
    for (index, covered) in written.into_iter().enumerate() {
        if covered {
            assert_eq!(
                vram.words()[index],
                actual.words()[index],
                "VRAM ({},{})",
                index % 1024,
                index / 1024
            );
            count += 1;
        }
    }
    assert_eq!(count * 2, 296092);
    eprintln!("All 118 TIM uploads match {count} independently captured VRAM words");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and original-game menu RAM/VRAM capture"]
fn captured_sprite_packets_reproduce_original_frame() {
    let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR is required"));
    let capture = root.join("analysis/menu-runtime/baseline-001");
    let ram = fs::read(capture.join("frame-00240.ram.bin")).unwrap();
    let data = fs::read(capture.join("frame-00240.vram.bin")).unwrap();
    let vram = Vram::from_le_bytes(&data[..1024 * 512 * 2]).unwrap();
    let read = |address: u32| {
        let address = (address & 0x1f_ffff) as usize;
        u32::from_le_bytes(ram[address..address + 4].try_into().unwrap())
    };
    let mut address = read(read(0x800b_acd0) + 8) & 0xff_ffff;
    let mut visited = std::collections::HashSet::new();
    let mut frame = Frame::new(320, 512).unwrap();
    let mut tpage = 0;
    let mut rendered = 0;
    let mut all_packets = Vec::new();
    while address != 0xff_ffff {
        assert!(visited.insert(address), "cyclic ordering table");
        assert!(
            visited.len() < 4096,
            "ordering table exceeds expected bound"
        );
        let tag = read(address);
        let words: Vec<_> = (0..tag >> 24).map(|i| read(address + 4 + i * 4)).collect();
        if !words.is_empty() {
            all_packets.push(words.clone());
            let op = words[0] >> 24;
            match op {
                0xe1 => {
                    assert!(
                        words.len() == 1 || words[1] == 0,
                        "nonzero secondary draw-mode word"
                    );
                    tpage = words[0] as u16;
                }
                0x60 | 0x62 | 0x64 | 0x66 => {
                    let color = [
                        words[0] as u8,
                        (words[0] >> 8) as u8,
                        (words[0] >> 16) as u8,
                    ];
                    let (x, y) = (words[1] as i16, (words[1] >> 16) as i16);
                    let size = *words.last().unwrap();
                    let (width, height) = (size as u16, (size >> 16) as u16);
                    if op & 4 != 0 {
                        assert_eq!(words.len(), 4);
                        frame
                            .draw_sprite(
                                &vram,
                                Sprite {
                                    x,
                                    y,
                                    width,
                                    height,
                                    u: words[2] as u8,
                                    v: (words[2] >> 8) as u8,
                                    clut: (words[2] >> 16) as u16,
                                    tpage,
                                    color,
                                    raw: false,
                                    semi_transparent: op & 2 != 0,
                                },
                            )
                            .unwrap();
                    } else {
                        assert_eq!(words.len(), 3);
                        frame.draw_tile(
                            x,
                            y,
                            width,
                            height,
                            color,
                            (op & 2 != 0).then(|| BlendMode::from_tpage(tpage)),
                        );
                    }
                    rendered += 1;
                }
                0x80 => {
                    // The captured tail copies (0,0) to itself: a true no-op.
                    assert_eq!(words, [0x8000_0000, 0, 0, 0x1_0002]);
                }
                0 => assert!(words.iter().all(|&word| word == 0)),
                _ => panic!("unimplemented original packet {op:#x}"),
            }
        }
        address = tag & 0xff_ffff;
    }
    assert_eq!(rendered, 103);
    let mut parsed_frame = Frame::new(320, 512).unwrap();
    assert_eq!(
        ac3_graphics::render_packets(&mut parsed_frame, &vram, &all_packets).unwrap(),
        rendered
    );
    assert_eq!(parsed_frame.words(), frame.words());
    let expected = fs::read(capture.join("frame-00240.screen.raw")).unwrap();
    let expected: Vec<_> = expected
        .chunks_exact(2)
        .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
        .collect();
    let differences = frame
        .words()
        .iter()
        .zip(&expected)
        .filter(|(a, b)| a != b)
        .count();
    let rgb_differences = frame
        .words()
        .iter()
        .zip(&expected)
        .filter(|(a, b)| (*a & 0x7fff) != (*b & 0x7fff))
        .count();
    eprintln!(
        "RGB differences {rgb_differences}; samples {:?}",
        frame
            .words()
            .iter()
            .zip(&expected)
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .take(20)
            .map(|(i, (a, b))| (i % 320, i / 320, format!("{a:04x}"), format!("{b:04x}")))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        differences, 0,
        "native sprite rasterization differs at {differences} pixels"
    );
    eprintln!("Replayed {rendered} original sprites/tiles: all 163840 RGB555/STP pixels match");
}
