use ac3_graphics::{BlendMode, Frame, Sprite, Vram, tim};

fn upload(vram: &mut Vram, x: u16, y: u16, words: &[u16]) {
    let bytes: Vec<_> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
    vram.load_block(&tim::Block {
        x,
        y,
        width: words.len() as u16,
        height: 1,
        data: &bytes,
    })
    .unwrap();
}

fn tim_fixture() -> Vec<u8> {
    let mut bytes = Vec::new();
    for word in [0x10u32, 8, 44, 0x0001_0010, 0x0001_0010] {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    for color in 0u16..16 {
        bytes.extend_from_slice(&color.to_le_bytes());
    }
    for word in [16u32, 0x0100_0040, 0x0002_0001, 0x7654_3210] {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    bytes
}

#[test]
fn tim_parses_palettes_words_and_all_prefixes_safely() {
    let bytes = tim_fixture();
    let image = tim::parse(&bytes).unwrap();
    assert_eq!(image.depth, tim::Depth::Indexed4);
    assert_eq!(
        (
            image.pixels.x,
            image.pixels.y,
            image.pixels.width,
            image.pixels.height
        ),
        (64, 256, 1, 2)
    );
    assert_eq!(image.clut.unwrap().data.len(), 32);
    assert_eq!(image.bytes_consumed, bytes.len());
    for len in 0..bytes.len() {
        assert!(tim::parse(&bytes[..len]).is_err());
    }
    let mut bad = bytes.clone();
    bad[12..14].copy_from_slice(&1020u16.to_le_bytes());
    assert!(tim::parse(&bad).is_err());
    let mut bad = bytes;
    bad[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(tim::parse(&bad).is_err());
}

#[test]
fn texture_sampling_handles_depths_page_offsets_and_cluts() {
    let mut vram = Vram::new();
    vram.load_tim(&tim::parse(&tim_fixture()).unwrap()).unwrap();
    for u in 0..4 {
        assert_eq!(vram.sample(0x11, 65, u, 0).unwrap(), u16::from(u));
    }
    assert_eq!(vram.sample(0x11, 65, 3, 1).unwrap(), 7);
    upload(&mut vram, 128, 0, &[0x0100]);
    upload(&mut vram, 0, 2, &[0x1234, 0x5678]);
    assert_eq!(vram.sample(0x82, 128, 0, 0).unwrap(), 0x1234);
    assert_eq!(vram.sample(0x82, 128, 1, 0).unwrap(), 0x5678);
    assert_eq!(vram.sample(0x102, 0, 0, 0).unwrap(), 0x0100);
    assert!(vram.sample(0x180, 0, 0, 0).is_err());
}

fn sprite() -> Sprite {
    Sprite {
        x: 0,
        y: 0,
        width: 3,
        height: 1,
        u: 0,
        v: 0,
        tpage: 0x100,
        clut: 0,
        color: [128; 3],
        raw: false,
        semi_transparent: false,
    }
}

#[test]
fn sprite_clipping_preserves_uv_and_zero_texel_transparency() {
    let mut vram = Vram::new();
    upload(&mut vram, 0, 0, &[0x1f, 0, 0x8000]);
    let mut frame = Frame::new(2, 1).unwrap();
    frame.clear(0x1234);
    frame
        .draw_sprite(&vram, Sprite { x: -1, ..sprite() })
        .unwrap();
    assert_eq!(frame.words(), [0x1234, 0x8000]);
}

#[test]
fn modulation_and_blending_stay_in_rgb555() {
    let mut vram = Vram::new();
    upload(&mut vram, 0, 0, &[0x001f, 0x801f, 0x83e0]);
    let mut frame = Frame::new(3, 1).unwrap();
    frame
        .draw_sprite(
            &vram,
            Sprite {
                color: [64, 255, 128],
                ..sprite()
            },
        )
        .unwrap();
    assert_eq!(frame.words(), [15, 0x800f, 0x83e0]);
    frame.clear(0x0c63); // Every background channel is 3.
    frame
        .draw_sprite(
            &vram,
            Sprite {
                semi_transparent: true,
                tpage: 0x140,
                ..sprite()
            },
        )
        .unwrap();
    assert_eq!(frame.words(), [31, 0x8c60, 0x8c03]); // Subtraction only on STP texels.
    frame.clear(0x7fff);
    frame.draw_tile(0, 0, 3, 1, [255; 3], Some(BlendMode::Add));
    assert_eq!(frame.words(), [0x7fff; 3]);
    frame.draw_tile(1, 0, 1, 1, [255; 3], Some(BlendMode::Subtract));
    assert_eq!(frame.words(), [0x7fff, 0, 0x7fff]);
}

#[test]
fn frame_bounds_and_presentation_are_explicit() {
    assert!(Frame::new(0, 512).is_err());
    assert!(Frame::new(320, 513).is_err());
    let mut frame = Frame::new(1, 1).unwrap();
    frame.clear(0x801f);
    assert_eq!(frame.rgba8(), [255, 0, 0, 255]);
    assert!(Vram::from_le_bytes(&[0; 32]).is_err());
}

#[test]
fn gp0_packets_apply_texture_window_and_reject_unknown_commands() {
    let mut vram = Vram::new();
    upload(&mut vram, 0, 0, &[1, 2, 3, 4, 5, 6, 7, 8, 0x1234]);
    let mut frame = Frame::new(1, 1).unwrap();
    let packets = vec![
        vec![0xe100_0100, 0xe200_0401],
        vec![0x6580_8080, 0, 0, 0x0001_0001],
    ];
    assert_eq!(
        ac3_graphics::render_packets(&mut frame, &vram, &packets).unwrap(),
        1
    );
    assert_eq!(frame.words(), [0x1234]);
    assert!(ac3_graphics::render_packets(&mut frame, &vram, &[vec![0x2000_0000]]).is_err());
    assert!(ac3_graphics::render_packets(&mut frame, &vram, &[vec![0x6400_0000, 0]]).is_err());
    assert!(
        ac3_graphics::render_packets(&mut frame, &vram, &[vec![0x8000_0000, 0, 1, 0x10001]])
            .is_err()
    );
}

#[test]
fn average_blending_adds_before_rounding_as_original_menu_requires() {
    let mut frame = Frame::new(1, 1).unwrap();
    frame.clear(17);
    frame.draw_tile(0, 0, 1, 1, [17 << 3, 0, 0], Some(BlendMode::Average));
    assert_eq!(frame.words(), [17]);
}
