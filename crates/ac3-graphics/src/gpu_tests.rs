//! Asset-free checks of `Gpu` edge rules that the original flight frames do
//! not exercise, or exercise only incidentally.

use crate::{Dithering, Error, Gpu, Vram};

fn xy(x: i32, y: i32) -> u32 {
    (x as u32 & 0xffff) | ((y as u32 & 0xffff) << 16)
}

fn gpu() -> Gpu {
    let mut gpu = Gpu::new(Vram::new());
    gpu.write_gp0_words(&[0xe300_0000, 0xe400_0000 | 1023 | (511 << 10)])
        .unwrap();
    gpu
}

fn at(gpu: &Gpu, x: usize, y: usize) -> u16 {
    gpu.vram().words()[y * 1024 + x]
}

fn put(gpu: &mut Gpu, x: i32, y: i32, pixels: &[u16]) {
    let mut words = vec![0xa000_0000, xy(x, y), (1 << 16) | pixels.len() as u32];
    words.extend(
        pixels
            .chunks(2)
            .map(|p| u32::from(p[0]) | (u32::from(*p.get(1).unwrap_or(&0)) << 16)),
    );
    gpu.write_gp0_words(&words).unwrap();
}

fn changed(gpu: &Gpu) -> usize {
    gpu.vram().words().iter().filter(|&&p| p != 0).count()
}

#[test]
fn fill_rounds_to_16_pixels_and_ignores_area_and_masks() {
    let mut gpu = gpu();
    gpu.write_gp0_words(&[
        0xe600_0003,
        0xe300_0000 | 10 | (10 << 10),
        0xe400_0000 | 12 | (12 << 10),
    ])
    .unwrap();
    gpu.write_gp0_words(&[0x0200_00f8, xy(0x13, 2), xy(1, 2)])
        .unwrap();
    for y in 0..6 {
        for x in 0..40 {
            let inside = (16..32).contains(&x) && (2..4).contains(&y);
            assert_eq!(at(&gpu, x, y), if inside { 31 } else { 0 }, "({x},{y})");
        }
    }
}

#[test]
fn upload_is_verbatim_ignores_masks_and_odd_padding_and_streams() {
    let mut gpu = gpu();
    put(&mut gpu, 5, 0, &[0x8000]);
    gpu.write_gp0(0xe600_0003).unwrap();
    for word in [0xa000_0000, xy(4, 0), xy(3, 1), 0x0002_0001, 0xdead_0003] {
        gpu.write_gp0(word).unwrap();
    }
    assert!(gpu.is_idle());
    assert_eq!(&gpu.vram().words()[4..8], &[1, 2, 3, 0]);
}

#[test]
fn copy_masks_identity_and_overlap_follow_reference_order() {
    let mut gpu = gpu();
    put(&mut gpu, 0, 0, &[1, 2, 3, 4, 5]);
    gpu.write_gp0_words(&[0xe600_0001, 0x8000_0000, xy(0, 0), xy(0, 1), xy(2, 1)])
        .unwrap();
    assert_eq!(&gpu.vram().words()[1024..1026], &[0x8001, 0x8002]);
    put(&mut gpu, 1, 2, &[0x8000]);
    gpu.write_gp0_words(&[0xe600_0002, 0x8000_0000, xy(0, 0), xy(0, 2), xy(2, 1)])
        .unwrap();
    assert_eq!(&gpu.vram().words()[2048..2050], &[1, 0x8000]);
    // Identity copies are skipped unless they set the mask bit.
    gpu.write_gp0_words(&[0xe600_0001, 0x8000_0000, 0, 0, xy(1, 1)])
        .unwrap();
    assert_eq!(at(&gpu, 0, 0), 0x8001);
    // Overlapping forward copies: even widths move pixel pairs, odd single pixels.
    gpu.write_gp0(0xe600_0000).unwrap();
    put(&mut gpu, 0, 3, &[1, 2, 3, 4, 5]);
    gpu.write_gp0_words(&[0x8000_0000, xy(0, 3), xy(1, 3), xy(4, 1)])
        .unwrap();
    assert_eq!(
        &gpu.vram().words()[3 * 1024..3 * 1024 + 5],
        &[1, 1, 2, 2, 4]
    );
    put(&mut gpu, 0, 4, &[1, 2, 3, 4, 5]);
    gpu.write_gp0_words(&[0x8000_0000, xy(0, 4), xy(1, 4), xy(3, 1)])
        .unwrap();
    assert_eq!(
        &gpu.vram().words()[4 * 1024..4 * 1024 + 5],
        &[1, 1, 1, 1, 5]
    );
}

#[test]
fn pair_and_single_writes_differ_for_quarter_and_subtract_like_the_reference() {
    let mut gpu = gpu();
    // Direct 15-bit page at (0,0), quarter blending.
    put(&mut gpu, 0, 0, &[0x80c0, 0x80c0, 0x8060, 0x8060]);
    gpu.write_gp0(0xe100_0160).unwrap();
    gpu.write_gp0_words(&[0x66ff_ffff, xy(100, 10), 0, xy(2, 1)])
        .unwrap();
    gpu.write_gp0_words(&[0x66ff_ffff, xy(200, 10), 0, xy(1, 1)])
        .unwrap();
    // Pair: floor(6/4)*255>>7 = 1. Single: ((6<<5)>>2)*255>>7 keeps the fraction: 2.
    assert_eq!(at(&gpu, 100, 10), 0x8020);
    assert_eq!(at(&gpu, 101, 10), 0x8020);
    assert_eq!(at(&gpu, 200, 10), 0x8040);
    // Subtraction: single pixels subtract at native bit position and borrow.
    put(&mut gpu, 100, 20, &[0x80, 0x80]);
    put(&mut gpu, 200, 20, &[0x80]);
    gpu.write_gp0(0xe100_0140).unwrap();
    gpu.write_gp0_words(&[0x66aa_aaaa, xy(100, 20), 2, xy(2, 1)])
        .unwrap();
    gpu.write_gp0_words(&[0x66aa_aaaa, xy(200, 20), 2, xy(1, 1)])
        .unwrap();
    assert_eq!(at(&gpu, 100, 20), 0x8020);
    assert_eq!(at(&gpu, 200, 20), 0x8000);
}

#[test]
fn polylines_end_on_terminator_and_reject_dangling_colors() {
    let mut gpu = gpu();
    gpu.write_gp0_words(&[0x4800_00ff, xy(0, 30), xy(4, 30), xy(4, 32), 0x5555_5555])
        .unwrap();
    for x in 0..=4 {
        assert_eq!(at(&gpu, x, 30), 31);
    }
    assert_eq!(at(&gpu, 4, 31), 31);
    assert_eq!(at(&gpu, 4, 32), 31);
    assert_eq!(changed(&gpu), 7);
    // Gouraud polyline whose terminator replaces a vertex after a color.
    gpu.write_gp0_words(&[
        0x5800_00ff,
        xy(0, 40),
        0xff00,
        xy(9, 40),
        0xff_0000,
        0x5000_5000,
    ])
    .unwrap();
    assert!(gpu.is_idle());
    assert_eq!(changed(&gpu), 7);
}

#[test]
fn fixed_rectangle_sizes_raw_sprites_and_texture_window() {
    let mut gpu = gpu();
    gpu.write_gp0_words(&[0x7800_00ff, xy(0, 40)]).unwrap();
    gpu.write_gp0_words(&[0x7000_00ff, xy(20, 40)]).unwrap();
    gpu.write_gp0_words(&[0x6800_00ff, xy(40, 40)]).unwrap();
    assert_eq!(changed(&gpu), 256 + 64 + 1);
    let texels: Vec<u16> = (1..=16).collect();
    put(&mut gpu, 0, 100, &texels);
    gpu.write_gp0(0xe100_0100).unwrap();
    // Raw 8x8 sprite ignores the command color.
    gpu.write_gp0_words(&[0x7500_0000, xy(0, 60), 100 << 8])
        .unwrap();
    assert_eq!(&gpu.vram().words()[60 * 1024..60 * 1024 + 8], &texels[..8]);
    // Window: mask 8 and offset 8 force texel bit 3 on.
    gpu.write_gp0_words(&[0xe200_0401, 0x6580_8080, xy(0, 70), 100 << 8, xy(4, 1)])
        .unwrap();
    assert_eq!(
        &gpu.vram().words()[70 * 1024..70 * 1024 + 4],
        &texels[8..12]
    );
}

#[test]
fn quad_culling_skips_the_v0_v3_diagonal_and_offsets_wrap_only_polygons() {
    let mut gpu = gpu();
    gpu.write_gp0_words(&[0x2800_00ff, xy(-500, 50), xy(0, 40), xy(0, 60), xy(600, 50)])
        .unwrap();
    assert_eq!(at(&gpu, 300, 50), 31);
    let mut gpu = self::gpu();
    gpu.write_gp0(0xe500_0000 | 0x7e8).unwrap();
    gpu.write_gp0_words(&[0x2000_ff00, xy(1030, 0), xy(1040, 0), xy(1030, 10)])
        .unwrap();
    gpu.write_gp0_words(&[0x6000_00ff, xy(1030, 0), xy(10, 10)])
        .unwrap();
    assert_eq!(at(&gpu, 1008, 2), 31 << 5);
    assert_eq!(gpu.vram().words().iter().filter(|&&p| p == 31).count(), 0);
}

#[test]
fn draw_area_edges_for_lines_and_degenerate_areas() {
    let mut gpu = gpu();
    gpu.write_gp0(0xe400_0000 | 9 | (9 << 10)).unwrap();
    gpu.write_gp0_words(&[0x4000_00ff, xy(0, 0), xy(9, 9)])
        .unwrap();
    gpu.write_gp0_words(&[0x4000_ff00, xy(0, 5), xy(9, 5)])
        .unwrap();
    for k in 0..9 {
        if k != 5 {
            assert_eq!(at(&gpu, k, k), 31);
        }
    }
    assert_eq!(
        at(&gpu, 9, 9),
        0,
        "octant lines exclude the last draw column/row"
    );
    assert_eq!(at(&gpu, 9, 5), 31 << 5, "axis lines include it");
    // One-row draw areas reject polygons but not rectangles.
    let mut gpu = self::gpu();
    gpu.write_gp0(0xe400_0000 | 100).unwrap();
    gpu.write_gp0_words(&[0x2000_00ff, xy(0, 0), xy(50, 0), xy(0, 5)])
        .unwrap();
    assert_eq!(changed(&gpu), 0);
    gpu.write_gp0_words(&[0x6000_00ff, xy(0, 0), xy(4, 4)])
        .unwrap();
    assert_eq!(changed(&gpu), 4);
}

#[test]
fn gouraud_dithering_follows_e1_and_policy() {
    let quad = [
        0x3880_8080,
        0,
        0x80_8080,
        xy(4, 0),
        0x80_8080,
        xy(0, 4),
        0x80_8080,
        xy(4, 4),
    ];
    let mut plain = gpu();
    plain.write_gp0_words(&quad).unwrap();
    let mut dithered = gpu();
    dithered.write_gp0(0xe100_0200).unwrap();
    dithered.write_gp0_words(&quad).unwrap();
    let mut forced_off = gpu();
    forced_off.set_dithering(Dithering::Off);
    forced_off.write_gp0(0xe100_0200).unwrap();
    forced_off.write_gp0_words(&quad).unwrap();
    let dither = [-4, 0, -3, 1, 2, -2, 3, -1, -3, 1, -4, 0, 3, -1, 2, -2];
    for y in 0..4 {
        for x in 0..4 {
            let c = ((128 + dither[y * 4 + x]) >> 3) as u16;
            assert_eq!(at(&dithered, x, y), c | (c << 5) | (c << 10));
            assert_eq!(at(&plain, x, y), 16 | (16 << 5) | (16 << 10));
            assert_eq!(at(&forced_off, x, y), at(&plain, x, y));
        }
    }
}

#[test]
fn undithered_gouraud_texture_pairs_share_the_first_pixel_shade() {
    let mut gpu = gpu();
    for row in 0..8 {
        put(&mut gpu, 64, row, &[0x1f; 8]);
    }
    let quad = |dither: bool| {
        let mut gpu = gpu.clone();
        gpu.write_gp0(0xe100_0000 | if dither { 0x200 } else { 0 })
            .unwrap();
        gpu.write_gp0_words(&[
            0x3c00_0040,
            xy(0, 0),
            0,
            0xc0,
            xy(4, 0),
            4 | (0x101 << 16),
            0x40,
            xy(0, 4),
            4 << 8,
            0xc0,
            xy(4, 4),
            4 | (4 << 8),
        ])
        .unwrap();
        gpu.vram().words()[..4].to_vec()
    };
    // Shades 64,96,128,160: pairs use 64 and 128 for both of their pixels.
    assert_eq!(quad(false), [15, 15, 31, 31]);
    // The dithered path shades every pixel itself (31*shade>>4, dither, >>3).
    let dithered: Vec<u16> = [64, 96, 128, 160]
        .iter()
        .zip([-4, 0, -3, 1])
        .map(|(&s, d): (&i32, i32)| ((((31 * s) >> 4).min(255) + d).clamp(0, 255) >> 3) as u16)
        .collect();
    assert_eq!(quad(true), dithered);
}

#[test]
fn display_readback_and_explicit_unsupported_inputs() {
    let mut gpu = gpu();
    put(&mut gpu, 8, 16, &[7, 8]);
    gpu.write_gp1(0x0500_0000 | 8 | (16 << 10)).unwrap();
    assert_eq!(gpu.display_pixels(2, 1).unwrap(), [7, 8]);
    gpu.write_gp0_words(&[0xc000_0000, 0, xy(1, 1)]).unwrap();
    assert!(gpu.is_idle());
    assert_eq!(
        gpu.write_gp0_words(&[0x8000_0000, 0, xy(0, 600), xy(1, 1)]),
        Err(Error::Unsupported("VRAM copy coordinates outside VRAM"))
    );
    assert!(gpu.write_gp1(0x0900_0001).is_err());
    assert!(
        gpu.write_gp0_words(&[0xa000_0000, xy(1020, 0), xy(8, 1)])
            .is_err()
    );
}
