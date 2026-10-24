use ac3_graphics::{Frame, Vram, render_packets};

fn texture(fill: impl Fn(usize, usize) -> u16) -> Vram {
    let mut data = vec![0; 1024 * 512 * 2];
    for y in 0..32 {
        for x in 0..32 {
            let offset = (y * 1024 + x) * 2;
            data[offset..offset + 2].copy_from_slice(&fill(x, y).to_le_bytes());
        }
    }
    Vram::from_le_bytes(&data).unwrap()
}
fn quad(op: u8, xy: [(i16, i16); 4], uv: [(u8, u8); 4], tpage: u16) -> Vec<u32> {
    let mut packet = vec![(u32::from(op) << 24) | 0x808080];
    for i in 0..4 {
        packet.push(u32::from(xy[i].0 as u16) | (u32::from(xy[i].1 as u16) << 16));
        packet.push(
            u32::from(uv[i].0)
                | (u32::from(uv[i].1) << 8)
                | if i == 1 { u32::from(tpage) << 16 } else { 0 },
        );
    }
    packet
}

#[test]
fn textured_quad_top_left_coverage_and_shared_diagonal() {
    let vram = texture(|_, _| 0x8001);
    let packet = quad(
        0x2f,
        [(1, 1), (7, 1), (1, 6), (7, 6)],
        [(0, 0), (6, 0), (0, 5), (6, 5)],
        0x120,
    );
    let mut frame = Frame::new(8, 8).unwrap();
    frame.clear(3);
    assert_eq!(render_packets(&mut frame, &vram, &[packet]).unwrap(), 1);
    for y in 0..8 {
        for x in 0..8 {
            assert_eq!(
                frame.words()[y * 8 + x],
                if (1..7).contains(&x) && (1..6).contains(&y) {
                    0x8004
                } else {
                    3
                }
            );
        }
    }
}

#[test]
fn affine_texture_rounding_uses_truncated_edge_and_span_steps() {
    let vram = texture(|u, v| (u + 1) as u16 | (((v + 1) as u16) << 5));
    let packet = quad(
        0x2d,
        [(13, 1), (19, 27), (0, 12), (6, 38)],
        [(0, 0), (15, 0), (0, 15), (15, 15)],
        0x100,
    );
    let mut frame = Frame::new(32, 40).unwrap();
    render_packets(&mut frame, &vram, &[packet]).unwrap();
    // Exact rational interpolation gives (3.75, 7.5). Fixed edge/span steps
    // select row 7, not 8; this catches half-texel rounding regressions.
    assert_eq!(frame.words()[13 * 32 + 8], 5 | (8 << 5));
    assert_eq!(frame.words()[0], 0);
}

#[test]
fn quad_clipping_modulation_and_stp_match_sprite_rules() {
    let vram = texture(|_, _| 0x801e);
    for op in [0x2c, 0x2d, 0x2e, 0x2f] {
        let mut packet = quad(
            op,
            [(-2, -2), (2, -2), (-2, 2), (2, 2)],
            [(0, 0), (4, 0), (0, 4), (4, 4)],
            0x100,
        );
        packet[0] = (u32::from(op) << 24) | 0x404040;
        let mut frame = Frame::new(4, 4).unwrap();
        frame.clear(8);
        render_packets(&mut frame, &vram, &[packet]).unwrap();
        let front = if op & 1 == 0 { 15 } else { 30 };
        let expected = 0x8000 | if op & 2 == 0 { front } else { (8 + front) >> 1 };
        for y in 0..4 {
            for x in 0..4 {
                assert_eq!(
                    frame.words()[y * 4 + x],
                    if x < 2 && y < 2 { expected } else { 8 }
                );
            }
        }
    }
}

#[test]
fn degenerate_oversized_and_truncated_quads_are_bounded() {
    let vram = texture(|_, _| 31);
    let uv = [(0, 0); 4];
    let mut frame = Frame::new(4, 4).unwrap();
    for xy in [[(1, 1); 4], [(-1024, 0), (1023, 0), (-1024, 3), (1023, 3)]] {
        render_packets(&mut frame, &vram, &[quad(0x2c, xy, uv, 0x100)]).unwrap();
        assert!(frame.words().iter().all(|p| *p == 0));
    }
    let packet = quad(0x2c, [(0, 0), (1, 0), (0, 1), (1, 1)], uv, 0x100);
    for len in 1..9 {
        assert!(render_packets(&mut frame, &vram, &[packet[..len].to_vec()]).is_err());
    }
}
