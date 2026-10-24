use super::*;

fn pair(low: i16, high: i16) -> u32 {
    u32::from(low as u16) | (u32::from(high as u16) << 16)
}
fn load_rotation(gte: &mut Gte, m: [[i16; 3]; 3]) {
    gte.ctc2(ctrl::R11R12, pair(m[0][0], m[0][1]));
    gte.ctc2(ctrl::R13R21, pair(m[0][2], m[1][0]));
    gte.ctc2(ctrl::R22R23, pair(m[1][1], m[1][2]));
    gte.ctc2(ctrl::R31R32, pair(m[2][0], m[2][1]));
    gte.ctc2(ctrl::R33, m[2][2] as u16 as u32);
}

/// Values from the original 80032878 lighting call (capture lighting-001):
/// RTV0 after ldv0 of [0,0,4096] with a padding word, then three mfc2 reads.
#[test]
fn rtv0_matches_original_apply_matrix_sv_registers() {
    let mut gte = Gte::default();
    gte.data[reg::IR1] = 0xffff_0030;
    gte.data[reg::IR2] = 0x30;
    load_rotation(
        &mut gte,
        [[-1740, 130, 3705], [0, 4093, -144], [-3708, -62, -1739]],
    );
    gte.mtc2(reg::VXY0, 0);
    gte.mtc2(reg::VZ0, 0x0e8f_1000);
    gte.command(0x0486012).unwrap();
    let ir = [9, 10, 11].map(|r| gte.mfc2(r));
    assert_eq!(ir, [0xe79, 0xffff_ff70, 0xffff_f935]);
    assert_eq!(
        [25, 26, 27].map(|r| gte.data[r]),
        [0xe79, 0xffff_ff70, 0xffff_f935]
    );
    assert_eq!(gte.data[reg::VZ0], 0x0e8f_1000, "raw word retained");
    assert_eq!(gte.control[ctrl::R33], 0xffff_f935, "ctc2 sign extension");
    assert_eq!(gte.flag(), 0);
}

#[test]
fn rtps_agrees_with_oracle_validated_projection() {
    use ac3_graphics::projection::{ProjectionControls, project};
    let mut seed = 0x1234_5678_u32;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    for case in 0..20_000 {
        let m: [[i16; 3]; 3] = std::array::from_fn(|_| std::array::from_fn(|_| next() as i16));
        let t: [i32; 3] = std::array::from_fn(|_| (next() as i32) >> (next() % 24));
        let v: [i16; 3] = std::array::from_fn(|_| next() as i16);
        let controls = ProjectionControls {
            offset: [(next() as i32) >> 4, (next() as i32) >> 4],
            distance: next() as u16 >> (next() % 8),
            depth_scale: next() as i16,
            depth_offset: next() as i32 >> (next() % 8),
        };
        let expected = project(m, t, controls, v);
        let mut gte = Gte::default();
        load_rotation(&mut gte, m);
        for (i, value) in t.into_iter().enumerate() {
            gte.ctc2(ctrl::TRX + i, value as u32);
        }
        gte.ctc2(ctrl::OFX, controls.offset[0] as u32);
        gte.ctc2(ctrl::OFY, controls.offset[1] as u32);
        gte.ctc2(ctrl::H, u32::from(controls.distance));
        gte.ctc2(ctrl::DQA, controls.depth_scale as u16 as u32);
        gte.ctc2(ctrl::DQB, controls.depth_offset as u32);
        gte.mtc2(reg::VXY0, pair(v[0], v[1]));
        gte.mtc2(reg::VZ0, v[2] as u16 as u32);
        gte.rtps(true, false);
        let sxy = gte.data[reg::SXY2];
        assert_eq!(
            [sxy as u16 as i16, (sxy >> 16) as u16 as i16],
            expected.screen[0],
            "case {case}"
        );
        assert_eq!(gte.sz(3), expected.depth[0], "case {case}");
        assert_eq!(gte.ir(0) as u16, expected.ir0, "case {case}");
        assert_eq!(gte.mac(0), expected.mac0, "case {case}");
        assert_eq!(
            [gte.mac(1), gte.mac(2), gte.mac(3)],
            expected.last_vector,
            "case {case}"
        );
        if (-32768..=32767).contains(&expected.last_vector[2]) {
            assert_eq!(gte.flag(), expected.flags, "case {case}");
        }
    }
}

#[test]
fn transfers_follow_redux_storage_rules() {
    let mut gte = Gte::default();
    gte.mtc2(reg::SXYP, 1);
    gte.mtc2(reg::SXYP, 2);
    gte.mtc2(reg::SXYP, 3);
    assert_eq!(&gte.data[12..16], &[1, 2, 3, 3]);
    gte.data[reg::SXY2] = 9;
    assert_eq!(gte.mfc2(reg::SXYP), 9);
    gte.mtc2(reg::IRGB, 0x7fff);
    assert_eq!([gte.ir(1), gte.ir(2), gte.ir(3)], [0xf80, 0xf80, 0xf80]);
    assert_eq!(gte.mfc2(reg::ORGB), 0x7fff);
    gte.mtc2(reg::LZCS, 0xffff_0000);
    assert_eq!(gte.data[reg::LZCR], 16);
    gte.mtc2(reg::LZCS, 0);
    assert_eq!(gte.data[reg::LZCR], 32);
    gte.mtc2(reg::LZCR, 5);
    assert_eq!(gte.data[reg::LZCR], 32, "LZCR is read-only");
    gte.data[reg::SZ1] = 0xdead_0001;
    assert_eq!(gte.mfc2(reg::SZ1), 1);
    gte.ctc2(ctrl::FLAG, 0xffff_ffff);
    assert_eq!(gte.flag(), 0xffff_f000);
    gte.ctc2(ctrl::FLAG, 0x1000);
    assert_eq!(gte.flag(), 0x1000, "IR0 saturation is not an error bit");
}

#[test]
fn average_z_and_nclip() {
    let mut gte = Gte::default();
    gte.ctc2(ctrl::ZSF3, 0x155);
    for (i, z) in [10_u32, 100, 200, 300].into_iter().enumerate() {
        gte.data[reg::SZ0 + i] = z;
    }
    gte.avsz3();
    assert_eq!(gte.mac(0), 0x155 * 600);
    assert_eq!(gte.data[reg::OTZ] & 0xffff, (0x155 * 600) >> 12);
    gte.data[reg::SXY0] = pair(0, 0);
    gte.data[reg::SXY1] = pair(10, 0);
    gte.data[reg::SXY2] = pair(0, 10);
    gte.nclip();
    assert_eq!(gte.mac(0), 100);
}

#[test]
fn color_pipeline_saturates_into_fifo() {
    let mut gte = Gte::default();
    gte.data[reg::RGBC] = 0x2a80_ff40;
    gte.mtc2(reg::IR0, 0x800);
    gte.ctc2(ctrl::RFC, 0x100);
    gte.dpcs(true, false);
    let rgb2 = gte.data[reg::RGB2].to_le_bytes();
    assert_eq!(rgb2[3], 0x2a, "code byte copied");
    assert_eq!(gte.flag() & flag::COLOR_R_SAT, 0);
    // Halfway from RGB (0x40,0xFF,0x80) toward FC (0x10,0,0): 0x28, 0x7F, 0x40.
    assert_eq!(rgb2, [0x28, 0x7f, 0x40, 0x2a]);
    gte.sqr(false, true);
    assert!(gte.flag() & flag::IR1_SAT != 0);
}

#[test]
fn unknown_command_is_explicit() {
    assert_eq!(Gte::default().command(0x3a), Err(UnknownCommand(0x3a)));
    let mut gte = Gte::default();
    let write = pipeline::PendingWrite {
        control: false,
        index: reg::IR1,
        value: 7,
        distance: 0,
    };
    assert_eq!(
        gte.command_after_writes(0x3a, &[write]),
        Err(UnknownCommand(0x3a))
    );
    assert_eq!(gte, Gte::default(), "no write lands for an unknown command");
}

/// Redux probe (headless `e1693b7f`): `mtc2 t1(=7), IR1` 0, 1 and 2
/// instructions before SQR (sf=0) with IR1-3 = 0xBEEF0003, 0x00010004, 5, and
/// IR3 written right before SQR. IR1 needs two instructions for SQR, IR3 none.
#[test]
fn write_to_use_matches_redux_sqr_probe() {
    let start = || {
        let mut gte = Gte::default();
        gte.data[reg::IR0] = 0xdead_0002;
        gte.data[reg::IR1] = 0xbeef_0003;
        gte.data[reg::IR2] = 0x0001_0004;
        gte.data[reg::IR3] = 5;
        gte
    };
    let sqr = |index, distance| {
        let mut gte = start();
        let write = pipeline::PendingWrite {
            control: false,
            index,
            value: 7,
            distance,
        };
        gte.command_after_writes(op::SQR, &[write]).unwrap();
        let ir = |i: usize| gte.data[reg::IR0 + i];
        ([ir(1), ir(3)], [gte.mac(1), gte.mac(3)])
    };
    // Old IR1 read; the write by the previous instruction lands over the result.
    assert_eq!(sqr(reg::IR1, 0), ([7, 0x19], [9, 25]));
    // Old IR1 read; the older write lands before the result, which stays.
    assert_eq!(sqr(reg::IR1, 1), ([0xbeef_0009, 0x19], [9, 25]));
    // Landed: the new full word is read and keeps its zero upper half.
    assert_eq!(sqr(reg::IR1, 2), ([0x31, 0x19], [49, 25]));
    // IR3 needs no separation even from the previous instruction.
    assert_eq!(sqr(reg::IR3, 0), ([0xbeef_0009, 0x31], [9, 49]));
    assert_eq!(pipeline::write_need(op::SQR, reg::IR1), 2);
    assert_eq!(pipeline::write_need(op::SQR, reg::IR3), 0);
}

#[test]
fn mvmva_write_need_follows_register_roles() {
    let mvmva = |mx: u32, v: u32, cv: u32| mx << 17 | v << 15 | cv << 13 | op::MVMVA;
    // Vector x needs 2 (VXY covers x and y), z none; IR1-3 for v=3.
    assert_eq!(pipeline::write_need(mvmva(0, 1, 0), reg::VXY1), 2);
    assert_eq!(pipeline::write_need(mvmva(0, 1, 0), reg::VZ1), 0);
    assert_eq!(pipeline::write_need(mvmva(0, 1, 0), reg::VXY0), 0);
    assert_eq!(pipeline::write_need(mvmva(0, 3, 0), reg::IR1), 2);
    assert_eq!(pipeline::write_need(mvmva(0, 3, 0), reg::IR2), 0);
    // Matrix pairs 2, 1, 0, 0, 0 of the selected matrix only.
    assert_eq!(pipeline::write_need(mvmva(1, 0, 3), 32 + ctrl::L11L12), 2);
    assert_eq!(
        pipeline::write_need(mvmva(1, 0, 3), 32 + ctrl::L11L12 + 1),
        1
    );
    assert_eq!(pipeline::write_need(mvmva(0, 0, 3), 32 + ctrl::L11L12), 0);
    assert_eq!(pipeline::write_need(mvmva(3, 0, 3), 32 + ctrl::R11R12), 0);
    // Translation x, y, z: 2, 1, 0 of the selected vector; none for cv=3.
    assert_eq!(pipeline::write_need(mvmva(0, 0, 1), 32 + ctrl::RBK), 2);
    assert_eq!(pipeline::write_need(mvmva(0, 0, 2), 32 + ctrl::RFC + 1), 1);
    assert_eq!(pipeline::write_need(mvmva(0, 0, 0), 32 + ctrl::TRZ), 0);
    assert_eq!(pipeline::write_need(mvmva(0, 0, 3), 32 + ctrl::TRX), 0);
}

#[test]
fn late_writes_to_registers_a_command_does_not_write_take_effect() {
    // RTPS R31R32 needs 2: the command projects with the old row 3, but the
    // control register itself ends up holding the written value.
    let mut gte = Gte::default();
    gte.ctc2(ctrl::H, 100);
    gte.ctc2(ctrl::R33, 0x1000);
    gte.mtc2(reg::VZ0, 400);
    let write = pipeline::PendingWrite {
        control: true,
        index: ctrl::R31R32,
        value: 0x0000_1000,
        distance: 1,
    };
    let mut reference = gte.clone();
    reference.rtps(true, false);
    gte.command_after_writes(op::RTPS | 1 << 19, &[write])
        .unwrap();
    assert_eq!(gte.data, reference.data, "old R31 used");
    assert_eq!(gte.control[ctrl::R31R32], 0x1000);
    // GPL MAC1 (need 2) is also a result: MAC1 = 100 + 2 * 3 from the old value.
    for (distance, mac1, ir1) in [(0, 7, 106), (1, 106, 106), (2, 13, 13)] {
        let mut gte = Gte::default();
        gte.data[reg::IR0] = 2;
        gte.data[reg::IR1] = 3;
        gte.data[reg::MAC1] = 100;
        let write = pipeline::PendingWrite {
            control: false,
            index: reg::MAC1,
            value: 7,
            distance,
        };
        gte.command_after_writes(op::GPL, &[write]).unwrap();
        assert_eq!((gte.mac(1), gte.ir(1)), (mac1, ir1), "distance {distance}");
    }
}

fn screen(word: u32) -> (i32, i32) {
    (
        i32::from(word as u16 as i16),
        i32::from((word >> 16) as u16 as i16),
    )
}

/// The sub-pixel window: off by default and invisible to `==`; when on, RTPS
/// and RTPT leave every register bit-identical, and each projected vertex's
/// unrounded position floors to the SXY word and the SZ value it wrote.
#[test]
fn subpixel_positions_floor_to_the_integer_results() {
    let mut seed = 0x9e37_79b9_u32;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    let (mut positions, mut fractional, mut saturated) = (0, 0, 0);
    for case in 0..40_000 {
        let mut plain = Gte::default();
        let m: [[i16; 3]; 3] =
            std::array::from_fn(|_| std::array::from_fn(|_| (next() as i16) >> (next() % 4)));
        load_rotation(&mut plain, m);
        for i in 0..3 {
            plain.ctc2(ctrl::TRX + i, ((next() as i32) >> (next() % 24)) as u32);
        }
        plain.ctc2(ctrl::OFX, ((next() as i32) >> (6 + next() % 8)) as u32);
        plain.ctc2(ctrl::OFY, ((next() as i32) >> (6 + next() % 8)) as u32);
        plain.ctc2(ctrl::H, next() >> (16 + next() % 8));
        plain.ctc2(ctrl::DQA, next() & 0xffff);
        plain.ctc2(ctrl::DQB, next());
        for v in 0..3 {
            plain.mtc2(reg::VXY0 + 2 * v, next());
            plain.mtc2(reg::VZ0 + 2 * v, next() & 0xffff);
        }
        let rtpt = case % 2 == 0;
        let code = if rtpt { op::RTPT } else { op::RTPS } | (next() & 1) << 19 | (next() & 1) << 10;
        let mut precise = plain.clone();
        precise.enable_subpixel(subpixel::DEFAULT_CAPACITY);
        assert!(plain.subpixel().is_none());
        assert_eq!(plain, precise, "the window is not state");
        plain.command(code).unwrap();
        precise.command(code).unwrap();
        assert_eq!(plain.data, precise.data, "case {case}");
        assert_eq!(plain.control, precise.control, "case {case}");
        let window = precise.subpixel().unwrap();
        let vertices = if rtpt { 3 } else { 1 };
        assert_eq!(window.projected(), vertices);
        assert_eq!(window.len() as u64, vertices);
        // Newest first: the last vertex is SXY2/SZ3.
        for (back, (word, position)) in window.entries().enumerate() {
            assert_eq!(word, precise.data[reg::SXY2 - back], "case {case}");
            let (x, y) = screen(word);
            // The divide overflowed for this vertex: H >= 2 * its SZ.
            let h = u32::from(precise.control[ctrl::H] as u16);
            let overflow = h >= 2 * u32::from(precise.sz(3 - back));
            let Some([px, py, pz]) = position else {
                saturated += 1;
                assert!(
                    overflow || [x, y].iter().any(|v| [-0x400, 0x3ff].contains(v)),
                    "case {case}: a vertex without a position saturated or overflowed"
                );
                if !rtpt && !overflow {
                    assert_ne!(precise.flag() & (1 << 14 | 1 << 13), 0);
                }
                continue;
            };
            assert!(
                !overflow,
                "case {case}: no position after a divide overflow"
            );
            positions += 1;
            assert_eq!(px.floor() as i32, x, "case {case}: x {px}");
            assert_eq!(py.floor() as i32, y, "case {case}: y {py}");
            assert_eq!(
                pz.floor() as u16,
                precise.sz(3 - back),
                "case {case}: z {pz}"
            );
            fractional += usize::from(px.fract() != 0.0 || py.fract() != 0.0);
        }
    }
    // Not vacuous: most cases keep a fraction, and both outcomes occur.
    assert!(
        positions > 20_000 && saturated > 1_000,
        "{positions} {saturated}"
    );
    assert!(
        fractional * 10 > positions * 9,
        "{fractional} of {positions}"
    );
}

/// A vertex moving by 1/10 pixel per step: the integer SX jumps a whole
/// pixel every ten steps, the unrounded position follows each step.
#[test]
fn subpixel_positions_move_smoothly() {
    let mut gte = Gte::default();
    load_rotation(&mut gte, [[0x1000, 0, 0], [0, 0x1000, 0], [0, 0, 0x1000]]);
    gte.ctc2(ctrl::TRZ, 2560);
    gte.ctc2(ctrl::OFX, 160 << 16);
    gte.ctc2(ctrl::OFY, 120 << 16);
    gte.ctc2(ctrl::H, 256);
    gte.enable_subpixel(4);
    let mut previous: Option<f32> = None;
    let mut integers = std::collections::BTreeSet::new();
    for x in 0..100_u32 {
        gte.mtc2(reg::VXY0, x | 7 << 16);
        gte.mtc2(reg::VZ0, 0);
        gte.rtps(true, false);
        let (word, position) = gte.subpixel().unwrap().newest().unwrap();
        assert_eq!(word, gte.data[reg::SXY2]);
        let [px, py, pz] = position.unwrap();
        let (sx, sy) = screen(word);
        integers.insert(sx);
        assert_eq!(px.floor() as i32, sx);
        assert_eq!(py.floor() as i32, sy);
        assert!((px - (160.0 + x as f32 * 0.1)).abs() < 1e-3, "x {x}: {px}");
        assert!((py - 120.7).abs() < 1e-3, "{py}");
        assert_eq!(pz, 2560.0);
        if let Some(previous) = previous {
            assert!(
                (px - previous - 0.1).abs() < 1e-3,
                "step {x}: {previous} -> {px}"
            );
        }
        previous = Some(px);
    }
    assert_eq!(integers.len(), 10, "ten whole pixels: {integers:?}");
}

/// The widescreen X scale: with `None` (and by default) RTPS/RTPT are the
/// exact Redux commands; with a fraction only SX changes, to
/// `OFX + floor(IR1 * ratio * num / den)` (the reciprocal `ratio` read back
/// through MAC0 with DQA 1, DQB 0), saturated as usual, and the sub-pixel
/// position still floors to it. The scale is not state.
#[test]
fn projection_x_scale_squeezes_only_sx() {
    let mut seed = 0x2545_f491_u32;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    let mut changed = 0;
    for case in 0..40_000 {
        let mut plain = Gte::default();
        assert_eq!(plain.projection_x_scale(), None);
        let m: [[i16; 3]; 3] =
            std::array::from_fn(|_| std::array::from_fn(|_| (next() as i16) >> (next() % 4)));
        load_rotation(&mut plain, m);
        for i in 0..3 {
            plain.ctc2(ctrl::TRX + i, ((next() as i32) >> (next() % 24)) as u32);
        }
        plain.ctc2(ctrl::OFX, ((next() as i32) >> (6 + next() % 8)) as u32);
        plain.ctc2(ctrl::OFY, ((next() as i32) >> (6 + next() % 8)) as u32);
        plain.ctc2(ctrl::H, next() >> (16 + next() % 8));
        plain.ctc2(ctrl::DQA, 1);
        plain.ctc2(ctrl::DQB, 0);
        plain.mtc2(reg::VXY0, next());
        plain.mtc2(reg::VZ0, next() & 0xffff);
        let code = op::RTPS | (next() & 1) << 19 | (next() & 1) << 10;
        let (num, den) = [(3, 4), (5, 6), (9, 16), (4, 7)][case % 4];
        let mut scaled = plain.clone();
        scaled.set_projection_x_scale(Some((num, den)));
        scaled.enable_subpixel(4);
        assert_eq!(plain, scaled, "the scale is not state");
        let mut unscaled = plain.clone();
        unscaled.set_projection_x_scale(Some((num, den)));
        unscaled.set_projection_x_scale(None);
        plain.command(code).unwrap();
        unscaled.command(code).unwrap();
        scaled.command(code).unwrap();
        assert_eq!(plain.data, unscaled.data, "case {case}: None is exact");
        assert_eq!(plain.control, unscaled.control, "case {case}");
        // Everything but SXY2 and the SX/MAC0 flags is unchanged.
        for r in (0..32).filter(|&r| r != reg::SXY2) {
            assert_eq!(plain.data[r], scaled.data[r], "case {case}: register {r}");
        }
        let sx_flags = flag::SX_SAT | flag::MAC0_POS | flag::MAC0_NEG;
        assert_eq!(
            plain.flag() & !sx_flags & !flag::ERROR,
            scaled.flag() & !sx_flags & !flag::ERROR,
            "case {case}"
        );
        let (_, y) = screen(plain.data[reg::SXY2]);
        let (x, scaled_y) = screen(scaled.data[reg::SXY2]);
        assert_eq!(y, scaled_y, "case {case}: SY unchanged");
        let ratio = i64::from(plain.mac(0));
        let offset = (i64::from(plain.ir(1)) * ratio * i64::from(num)).div_euclid(i64::from(den));
        let expected =
            ((i64::from(plain.control[ctrl::OFX] as i32) + offset) >> 16).clamp(-0x400, 0x3ff);
        assert_eq!(i64::from(x), expected, "case {case}");
        changed += usize::from(plain.data[reg::SXY2] != scaled.data[reg::SXY2]);
        if let Some((word, Some([px, _, _]))) = scaled.subpixel().unwrap().newest() {
            assert_eq!(word, scaled.data[reg::SXY2]);
            assert_eq!(px.floor() as i32, x, "case {case}: sub-pixel x {px}");
        }
    }
    // Many random vertices saturate both ways; thousands still move.
    assert!(changed > 5_000, "not vacuous: {changed}");
    // A serialized GTE drops the scale.
    let mut gte = Gte::default();
    gte.set_projection_x_scale(Some((3, 4)));
    assert_eq!(gte.projection_x_scale(), Some((3, 4)));
    gte.set_projection_x_scale(Some((0, 4)));
    assert_eq!(gte.projection_x_scale(), None, "zero numerator");
}

/// A centred vertex 100 units right at depth H: SX 100 from the centre
/// unscaled, 75 at 3/4, and 56.25 at 9/16 (the 21:9 setting).
#[test]
fn projection_x_scale_of_a_known_vertex() {
    let mut gte = Gte::default();
    load_rotation(&mut gte, [[0x1000, 0, 0], [0, 0x1000, 0], [0, 0, 0x1000]]);
    gte.ctc2(ctrl::OFX, 160 << 16);
    gte.ctc2(ctrl::OFY, 128 << 16);
    gte.ctc2(ctrl::H, 256);
    gte.enable_subpixel(4);
    for (scale, sx, precise) in [
        (None, 260, 260.0),
        (Some((3, 4)), 235, 235.0),
        (Some((9, 16)), 216, 216.25),
    ] {
        gte.set_projection_x_scale(scale);
        gte.mtc2(reg::VXY0, 100 | 30 << 16);
        gte.mtc2(reg::VZ0, 256);
        gte.rtps(true, false);
        let (x, y) = screen(gte.data[reg::SXY2]);
        assert_eq!((x, y), (sx, 158), "{scale:?}");
        let [px, _, _] = gte.subpixel().unwrap().newest().unwrap().1.unwrap();
        assert!((px - precise).abs() < 1e-3, "{scale:?}: {px}");
    }
}

#[test]
fn subpixel_window_keeps_the_newest_entries() {
    let mut window = subpixel::Window::new(3);
    assert_eq!(window.find(1), None);
    for word in 1..=5_u32 {
        window.push(word, Some([word as f32, 0.0, 0.0]));
    }
    assert_eq!(window.len(), 3);
    assert_eq!(window.find(2), None, "dropped out");
    assert_eq!(window.find(3), Some([3.0, 0.0, 0.0]));
    window.push(4, None);
    assert_eq!(
        window.find(4),
        None,
        "the newest entry for 4 has no position"
    );
    window.push(4, Some([4.5, 0.0, 0.0]));
    assert_eq!(window.find(4), Some([4.5, 0.0, 0.0]));
    assert_eq!(
        window.entries().map(|(w, _)| w).collect::<Vec<_>>(),
        [4, 4, 5]
    );
    window.clear();
    assert!(window.is_empty());
    assert_eq!(window.find(4), None);
}
