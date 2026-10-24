use ac3_graphics::{Frame, Vram, render_packets, tim};

#[test]
fn loading_fade_mask_write_and_protected_destination_are_ordered() {
    let mut frame = Frame::new(4, 2).unwrap();
    render_packets(
        &mut frame,
        &Vram::new(),
        &[
            vec![0xe6000001, 0],
            vec![0x60000040, 0, 2 | (2 << 16)],
            vec![0xe6000002],
            vec![0x60004000, 0, 4 | (2 << 16)],
        ],
    )
    .unwrap();
    assert_eq!(
        frame.words(),
        [0x8008, 0x8008, 0x100, 0x100, 0x8008, 0x8008, 0x100, 0x100]
    );
    render_packets(
        &mut frame,
        &Vram::new(),
        &[vec![0xe6000000], vec![0x60000080, 0, 1 | (1 << 16)]],
    )
    .unwrap();
    assert_eq!(frame.words()[0], 16);
    assert_eq!(frame.words()[1], 0x8008);
}

#[test]
fn fixed_loading_glyph_keeps_uv_word_and_clips_eight_pixel_extent() {
    let mut vram = Vram::new();
    let pixels: Vec<u8> = (0..64_u16)
        .flat_map(|v| (0x8000 | (v % 31 + 1)).to_le_bytes())
        .collect();
    vram.load_block(&tim::Block {
        x: 3,
        y: 4,
        width: 8,
        height: 8,
        data: &pixels,
    })
    .unwrap();
    let mut frame = Frame::new(12, 12).unwrap();
    frame.clear(2);
    // Raw, additive SPRT_8 starts at(-1,2); following TILE in the same packet
    // proves parsing consumes three glyph words, never a nonexistent size word.
    let count = render_packets(
        &mut frame,
        &vram,
        &[
            vec![0xe1000120],
            vec![
                0x77000000,
                0x0002ffff,
                0x0403,
                0x600000f8,
                11 | (11 << 16),
                1 | (1 << 16),
            ],
        ],
    )
    .unwrap();
    assert_eq!(count, 2);
    for y in 0..12 {
        for x in 0..12 {
            let expected = if x == 11 && y == 11 {
                31
            } else if x < 7 && (2..10).contains(&y) {
                0x8000 | (((((y - 2) * 8 + x + 1) % 31 + 1) as u16) + 2).min(31)
            } else {
                2
            };
            assert_eq!(frame.words()[y * 12 + x], expected, "({x},{y})");
        }
    }
}
