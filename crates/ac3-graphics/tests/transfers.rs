use ac3_graphics::{Frame, Vram, render_packets, render_packets_with_transfers};

#[test]
fn palette_uploads_affect_only_later_draws_and_ignore_odd_pixel_padding() {
    let mut vram = Vram::new();
    let mut frame = Frame::new(2, 1).unwrap();
    let packets = vec![
        vec![0xa0000000, 64, 0x00010001, 0xdead1111],
        vec![0xa0000000, 0, 0x00010002, 0x001f0000],
        vec![0xe1000001, 0x65808080, 0, 0, 0x00010001],
        vec![0xa0000000, 1, 0x00010001, 0xbeef03e0, 0x01000000],
        vec![0x65808080, 1, 0, 0x00010001],
    ];
    assert_eq!(
        render_packets_with_transfers(&mut frame, &mut vram, &packets).unwrap(),
        2
    );
    assert_eq!(frame.words(), &[0x001f, 0x03e0]);
    assert_eq!(vram.words()[2], 0);
    assert_eq!(vram.words()[65], 0);
    assert!(render_packets(&mut frame, &vram, &packets).is_err());
}

#[test]
fn truncated_upload_fails_before_mutating_vram() {
    let mut vram = Vram::new();
    let mut frame = Frame::new(2, 2).unwrap();
    assert!(
        render_packets_with_transfers(
            &mut frame,
            &mut vram,
            &[vec![0xa0000000, 0, 0x00010003, 0xffffffff]]
        )
        .is_err()
    );
    assert!(vram.words().iter().all(|&v| v == 0));
}

#[test]
fn flat_quad_covers_rectangle_once_including_shared_triangle_edge() {
    let mut frame = Frame::new(4, 4).unwrap();
    frame.clear(0x001f);
    render_packets(
        &mut frame,
        &Vram::new(),
        &[vec![0x2a000000, 0, 3, 3 << 16, (3 << 16) | 3]],
    )
    .unwrap();
    for y in 0..4 {
        for x in 0..4 {
            assert_eq!(
                frame.words()[y * 4 + x],
                if x < 3 && y < 3 { 15 } else { 31 },
                "{x},{y}"
            );
        }
    }
}
