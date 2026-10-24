use ac3_graphics::{Frame, Vram, render_packets};

#[test]
fn flat_triangle_strip_edge_and_additive_color() {
    let mut frame = Frame::new(6, 6).unwrap();
    frame.clear(2);
    render_packets(
        &mut frame,
        &Vram::new(),
        &[
            vec![0xe1000020],
            vec![0x22000040, 1 | (1 << 16), 5 | (1 << 16), 1 | (5 << 16)],
        ],
    )
    .unwrap();
    for y in 0..6 {
        for x in 0..6 {
            assert_eq!(
                frame.words()[y * 6 + x],
                if x >= 1 && y >= 1 && x + y < 6 { 10 } else { 2 }
            );
        }
    }
}

#[test]
fn gouraud_gradient_respects_draw_area_and_its_reset() {
    let vram = Vram::new();
    let mut frame = Frame::new(5, 5).unwrap();
    let quad = vec![0x38000000, 0, 0x80, 4, 0, 4 << 16, 0x80, (4 << 16) | 4];
    render_packets(
        &mut frame,
        &vram,
        &[
            vec![0xe3000000 | 1 | (1 << 10), 0xe4000000 | 2 | (2 << 10)],
            quad,
        ],
    )
    .unwrap();
    for y in 0..5 {
        for x in 0..5 {
            assert_eq!(
                frame.words()[y * 5 + x],
                if (1..=2).contains(&x) && (1..=2).contains(&y) {
                    (x * 4) as u16
                } else {
                    0
                }
            );
        }
    }
    render_packets(
        &mut frame,
        &vram,
        &[
            vec![0xe3000000, 0xe4000000 | 4 | (4 << 10)],
            vec![0x6000ff00, 4 | (4 << 16), 1 | (1 << 16)],
        ],
    )
    .unwrap();
    assert_eq!(frame.words()[24], 31 << 5);
}

#[test]
fn gouraud_dither_and_shared_diagonal_have_single_coverage() {
    let vram = Vram::new();
    let mut frame = Frame::new(4, 4).unwrap();
    // Additive shading makes a double-covered strip diagonal observable.
    let quad = vec![
        0x3a808080,
        0,
        0x808080,
        4,
        0x808080,
        4 << 16,
        0x808080,
        (4 << 16) | 4,
    ];
    render_packets(&mut frame, &vram, &[vec![0xe1000220], quad]).unwrap();
    let dither = [-4, 0, -3, 1, 2, -2, 3, -1, -3, 1, -4, 0, 3, -1, 2, -2];
    for (index, bias) in dither.into_iter().enumerate() {
        let c = ((128 + bias) >> 3) as u16;
        assert_eq!(frame.words()[index], c | c << 5 | c << 10);
    }
}

#[test]
fn flat_lines_include_endpoints_and_reverse_consistently() {
    let vram = Vram::new();
    let mut first = Frame::new(10, 10).unwrap();
    let mut second = Frame::new(10, 10).unwrap();
    render_packets(
        &mut first,
        &vram,
        &[vec![0x400000ff, 1 | (1 << 16), 7 | (3 << 16)]],
    )
    .unwrap();
    render_packets(
        &mut second,
        &vram,
        &[vec![0x400000ff, 7 | (3 << 16), 1 | (1 << 16)]],
    )
    .unwrap();
    assert_eq!(first.words(), second.words());
    assert_eq!(first.words().iter().filter(|&&v| v != 0).count(), 7);
    assert_eq!(first.words()[11], 31);
    assert_eq!(first.words()[37], 31);
}
