use ac3_graphics::{Frame, Vram, render_packets, tim};

fn xy(x: u16, y: u16) -> u32 {
    u32::from(x) | (u32::from(y) << 16)
}

#[test]
fn e5_moves_each_supported_primitive_in_framebuffer_space() {
    let mut vram = Vram::new();
    vram.load_block(&tim::Block {
        x: 0,
        y: 0,
        width: 8,
        height: 8,
        data: &[255; 128],
    })
    .unwrap();
    let a = xy(1, 1);
    let b = xy(5, 1);
    let c = xy(1, 5);
    let d = xy(5, 5);
    let primitives = [
        vec![0x600000ff, a, xy(3, 2)],
        vec![0x65000000, a, 0, xy(3, 2)],
        vec![0x75000000, a, 0],
        vec![0x400000ff, a, d],
        vec![0x200000ff, a, b, c],
        vec![0x280000ff, a, b, c, d],
        vec![
            0x2d000000, a, 0, b, 0x01000004, c, 0x00000400, d, 0x00000404,
        ],
        vec![0x300000ff, a, 0x0000ff00, b, 0x00ff0000, c],
        vec![0x380000ff, a, 0x0000ff00, b, 0x00ff0000, c, 0x00ffffff, d],
    ];
    for primitive in primitives {
        let mut original = Frame::new(16, 16).unwrap();
        render_packets(&mut original, &vram, &[vec![0xe1000100], primitive.clone()]).unwrap();
        assert!(original.words().iter().any(|&v| v != 0));
        let mut moved = Frame::new(16, 16).unwrap();
        render_packets(
            &mut moved,
            &vram,
            &[vec![0xe1000100, 0xe5001003], primitive.clone()],
        )
        .unwrap();
        for y in 0..16 {
            for x in 0..16 {
                let expected = if x >= 3 && y >= 2 {
                    original.words()[(y - 2) * 16 + x - 3]
                } else {
                    0
                };
                assert_eq!(
                    moved.words()[y * 16 + x],
                    expected,
                    "primitive {:08x}, {x},{y}",
                    primitive[0]
                );
            }
        }
    }
}

#[test]
fn e5_signed_coordinate_addition_wraps_and_draw_area_stays_absolute() {
    let vram = Vram::new();
    let mut frame = Frame::new(8, 8).unwrap();
    // (-1024,-1024) plus (-1024,-1024) wraps to (0,0) in signed11.
    render_packets(
        &mut frame,
        &vram,
        &[vec![0xe5200400], vec![0x600000ff, xy(1024, 1024), xy(1, 1)]],
    )
    .unwrap();
    assert_eq!(frame.words()[0], 31);
    // Offset(-3,+4): geometry(4,0) becomes(1,4), then the absolute x>=2
    // draw-area clips its leftmost pixel without moving the clipping rectangle.
    render_packets(
        &mut frame,
        &vram,
        &[
            vec![0xe50027fd, 0xe3000002, 0xe4001c07],
            vec![0x6000ff00, xy(4, 0), xy(3, 1)],
        ],
    )
    .unwrap();
    assert_eq!(frame.words()[4 * 8 + 1], 0);
    assert_eq!(&frame.words()[4 * 8 + 2..4 * 8 + 4], &[31 << 5; 2]);
}

#[test]
fn e5_zero_restores_coordinates_for_later_commands() {
    let vram = Vram::new();
    let mut frame = Frame::new(8, 8).unwrap();
    render_packets(
        &mut frame,
        &vram,
        &[
            vec![0xe5001003],
            vec![0x600000ff, 0, xy(1, 1)],
            vec![0xe5000000],
            vec![0x6000ff00, 0, xy(1, 1)],
        ],
    )
    .unwrap();
    assert_eq!(frame.words()[2 * 8 + 3], 31);
    assert_eq!(frame.words()[0], 31 << 5);
}
