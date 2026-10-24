//! Atmosphere (`src/effects/atmosphere.rs`, `docs/EFFECTS_ATMOSPHERE.md`).
//!
//! - Unit tests of the pure functions: the fog strips' layout, levels and
//!   slots, the palette interpolation, the redirect rule, the sky bins, the
//!   haze integral and phase function, the write-back helpers.
//! - Ignored (the JP disc and `~/ac3-jp/runs/present-work/at50000`):
//!   - [`jp_fog_mechanism_and_redirect_on_captured_frames`]: GPU traces of
//!     mission 1 flight passes (`tests/support/fog_trace.rs`) prove the
//!     documented mechanism word for word and palette for palette, prove
//!     the trace re-renders the pass exactly, and prove the redirect rule:
//!     the redirected pass equals the pass with the game's strips removed;
//!   - [`jp_atmosphere_mockups`]: before/after pictures;
//!   - [`jp_depth_cue_report`]: per-polygon colours, for the IR0 = 0
//!     experiment (`tools/effects/compare_depth_cue.py`).
//!
//! Pictures go to `$AC3_FX_FOG_OUT` (default `target/effects-atmosphere`).
#[path = "support/fog_trace.rs"]
mod fog_trace;

use ac3_port::effects::atmosphere::{self as atm, Haze, HazeInput, HazeScene, Phase, SkyTable};
use fog_trace::{
    DepthBuffer, Packet, PassTrace, Replay, State, Step, TracedPlayer, clut_word_index, commands,
    crop, jp_disc, jp_flight_state, save_png, side_by_side, stacked, tpage_word_index, upscale,
};
use std::path::PathBuf;

// ---- unit tests ------------------------------------------------------------------------

#[test]
fn strips_follow_the_fog_routine() {
    // Even strips: 16x64 blocks at (320..368, 384/448); odd strips: 64x16
    // rows at (0..192, 240/496); strip 16: the fill (800943A8).
    for strip in 0..16 {
        let i = strip as u16;
        let expected = if strip % 2 == 0 {
            atm::StripSource::Block {
                x: 320 + 16 * ((i / 2) % 4),
                y: 384 + 64 * (i / 8),
            }
        } else {
            atm::StripSource::Rows {
                x: 64 * (((i - 1) / 2) % 4),
                y: 240 + 256 * (i / 8),
            }
        };
        assert_eq!(atm::strip_source(strip), expected, "strip {strip}");
    }
    assert_eq!(atm::strip_source(16), atm::StripSource::Fill);
    assert_eq!(
        atm::strip_source(0),
        atm::StripSource::Block {
            x: atm::UNFOGGED_PALETTES[0],
            y: atm::UNFOGGED_PALETTES[1]
        }
    );
    // The levels: rsin(64 i), rising to the fill's 4096.
    for (i, &level) in atm::FOG_LEVELS.iter().enumerate() {
        let exact = 4096.0 * (std::f64::consts::PI * i as f64 / 32.0).sin();
        assert!((f64::from(level) - exact).abs() <= 1.0, "level {i}");
    }
    assert_eq!(atm::strip_level(0), 0);
    assert_eq!(atm::strip_level(16), 4096);
}

#[test]
fn bucket_slots_of_mission_one() {
    // Mission 1's section 0 near/far 16395/28000 and the RAM table + 1 of
    // the trace at VBlank 50107.
    let slots = atm::bucket_slots(16395, 28000);
    let ram = [
        2140, 2230, 2321, 2412, 2502, 2593, 2684, 2774, 2865, 2956, 3046, 3137, 3228, 3318, 3409,
        3500, 4094,
    ];
    assert_eq!(slots, ram.map(|bucket| bucket + 1));
    // Degenerate and negative ranges keep to 0..=0xFFD (+1).
    let flat = atm::bucket_slots(100, 100);
    assert!(flat[..16].iter().all(|&slot| slot == (100 >> 3) + 1));
    let negative = atm::bucket_slots(-800, -100);
    assert!(negative[..16].iter().all(|&slot| slot == 1));
}

#[test]
fn a_slot_reads_the_lowest_strip_at_or_above_it() {
    let slots = atm::bucket_slots(16395, 28000);
    assert_eq!(atm::strip_for_slot(2, &slots), 0);
    assert_eq!(atm::strip_for_slot(2141, &slots), 0);
    assert_eq!(atm::strip_for_slot(2142, &slots), 1);
    assert_eq!(atm::strip_for_slot(2231, &slots), 1);
    assert_eq!(atm::strip_for_slot(3501, &slots), 15);
    assert_eq!(atm::strip_for_slot(3502, &slots), 16);
    assert_eq!(atm::strip_for_slot(4094, &slots), 16);
    // Coinciding strips: the lower one runs last.
    let mut equal = slots;
    equal[3] = equal[2];
    assert_eq!(atm::strip_for_slot(equal[2], &equal), 2);
    // The palette fog of a depth: none before strip 0, all past strip 15.
    assert_eq!(atm::palette_fog(16000.0, &slots), 0.0);
    assert_eq!(atm::palette_fog(29000.0, &slots), 1.0);
    let middle = atm::palette_fog(22000.0, &slots);
    assert!(middle > 0.5 && middle < 0.9, "{middle}");
}

#[test]
fn palette_interpolation_keeps_its_ends() {
    let fog = [0x5b, 0x63, 0x5b];
    for source in [0x0001u16, 0x7fff, 0x8000, 0x8421, 0x1234, 0x7c1f] {
        // Level 0 is the source itself: step 0 is unfogged.
        assert_eq!(atm::fog_palette_color(source, fog, 0), source);
        // Full level is the fog colour (5-bit), with the STP bit kept.
        let full = atm::fog_palette_color(source, fog, 4096);
        assert_eq!(
            full & 0x7fff,
            (0x5b >> 3) | ((0x63 >> 3) << 5) | ((0x5b >> 3) << 10)
        );
        assert_eq!(full & 0x8000, source & 0x8000);
    }
    // Transparent stays transparent at every level.
    for level in atm::FOG_LEVELS {
        assert_eq!(atm::fog_palette_color(0, fog, level), 0);
    }
    // In between: each channel moves monotonically toward the fog.
    let mut previous = 0x7fffu16;
    for level in atm::FOG_LEVELS {
        let value = atm::fog_palette_color(0x7fff, fog, level);
        assert!(value & 0x1f <= previous & 0x1f);
        previous = value;
    }
}

#[test]
fn the_redirect_rule() {
    use Phase::*;
    // Every terrain palette in the world phase reads step 0.
    for row in 0..64 {
        assert_eq!(
            atm::clut_redirect([320, 256 + row], 16, World),
            Some([320, 384 + row])
        );
    }
    // Outside the block, or 8-bit palettes that leave it: untouched.
    for clut in [
        [320, 255],
        [320, 320],
        [336, 256],
        [304, 256],
        [320, 384],
        [352, 311],
    ] {
        assert_eq!(atm::clut_redirect(clut, 16, World), None, "{clut:?}");
    }
    assert_eq!(atm::clut_redirect([320, 256], 256, World), None);
    // Only the world phase.
    for phase in [Sky, Flare, Hud, Other] {
        assert_eq!(atm::clut_redirect([320, 300], 16, phase), None);
    }
    // The GP0 half-word form.
    let word = atm::clut_word([320, 270]);
    assert_eq!(atm::clut_position(word), [320, 270]);
    assert_eq!(
        atm::clut_word_redirect(word, 16, World),
        Some(atm::clut_word([320, 398]))
    );
    assert_eq!(atm::phase_of_slot(Some(4095)), Sky);
    assert_eq!(atm::phase_of_slot(Some(4094)), World);
    assert_eq!(atm::phase_of_slot(Some(2)), World);
    assert_eq!(atm::phase_of_slot(Some(1)), Flare);
    assert_eq!(atm::phase_of_slot(Some(0)), Hud);
    assert_eq!(atm::phase_of_slot(None), Other);
}

fn level_view(h: f32) -> atm::View {
    atm::View {
        rotation: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        h,
        position: [0.0, -2000.0, 0.0],
    }
}

#[test]
fn sky_bins_cover_every_elevation() {
    let mut previous = -2.0;
    for bin in 0..atm::SKY_BINS {
        let elevation = atm::bin_elevation(bin);
        assert!(elevation > previous);
        assert_eq!(atm::sky_bin(elevation), bin);
        previous = elevation;
    }
    assert_eq!(atm::sky_bin(-std::f32::consts::FRAC_PI_2), 0);
    assert_eq!(atm::sky_bin(std::f32::consts::FRAC_PI_2), atm::SKY_BINS - 1);
    // Half the bins are within 15 degrees of the horizon.
    let near = (0..atm::SKY_BINS)
        .filter(|&bin| atm::bin_elevation(bin).abs() < 15f32.to_radians())
        .count();
    assert!(near >= atm::SKY_BINS / 2, "{near}");
    // A level view: screen y is elevation.
    let view = level_view(296.0);
    assert!(atm::elevation(view.direction(0.0, -100.0)) > 0.3);
    assert!(atm::elevation(view.direction(0.0, 100.0)) < -0.3);
    assert!(atm::elevation(view.direction(50.0, 0.0)).abs() < 1e-6);
}

#[test]
fn the_sky_model_bins_a_gradient() {
    // A Gouraud quad over the screen: blue at the top, the fog colour at
    // the bottom, and a flat fog band below the horizon.
    let (top, bottom) = ([40, 80, 200], [91, 99, 91]);
    let vertex = |x: f32, y: f32, color| atm::SkyVertex { x, y, color };
    let polygons = vec![
        atm::SkyPolygon {
            vertices: vec![
                vertex(-160.0, -120.0, top),
                vertex(160.0, -120.0, top),
                vertex(-160.0, 0.0, bottom),
                vertex(160.0, 0.0, bottom),
            ],
        },
        atm::SkyPolygon {
            vertices: vec![
                vertex(-160.0, 0.0, bottom),
                vertex(160.0, 0.0, bottom),
                vertex(-160.0, 120.0, bottom),
                vertex(160.0, 120.0, bottom),
            ],
        },
    ];
    let view = level_view(296.0);
    let table = atm::bin_sky(&polygons, &view, 2.0).expect("samples");
    let fog = atm::unit_color(bottom);
    // Below the horizon: the band colour, also in the bins off screen.
    for bin in 0..atm::SKY_BINS / 2 - 1 {
        let color = table.colors[bin];
        assert!(
            (0..3).all(|c| (color[c] - fog[c]).abs() < 0.02),
            "bin {bin}: {color:?}"
        );
    }
    // Above: bluer with elevation, and the top bins take the top sample.
    let blue: Vec<f32> = (atm::SKY_BINS / 2..atm::SKY_BINS)
        .map(|bin| table.colors[bin][2])
        .collect();
    assert!(
        blue.windows(2).all(|pair| pair[1] >= pair[0] - 1e-4),
        "{blue:?}"
    );
    assert!(blue[blue.len() - 1] > 0.6);
    assert!(!table.sampled[atm::SKY_BINS - 1]);
    assert!(table.sampled[atm::SKY_BINS / 2]);
    // Sampling between bin centres interpolates.
    let horizon = table.sample(0.0);
    assert!((horizon[0] - fog[0]).abs() < 0.05);
    assert!(atm::bin_sky(&[], &view, 2.0).is_none());
    let uniform = SkyTable::uniform([0.5; 3]);
    assert_eq!(uniform.sample(0.3), [0.5; 3]);
}

#[test]
fn sky_polygons_come_from_gp0() {
    // POLY_G3 of the trace (VBlank 50107) and POLY_F4.
    let g3 = [
        0x306b_7a6b,
        0x0078_ffbd,
        0x0069_6f5c,
        0x0078_ff60,
        0x006b_7a6b,
        0xffe4_ff60,
    ];
    let polygon = atm::sky_polygon(&g3).expect("a polygon");
    assert_eq!(polygon.vertices.len(), 3);
    assert_eq!(polygon.vertices[0].x, -67.0);
    assert_eq!(polygon.vertices[0].y, 120.0);
    assert_eq!(polygon.vertices[0].color, [0x6b, 0x7a, 0x6b]);
    assert_eq!(polygon.vertices[1].color, [0x5c, 0x6f, 0x69]);
    assert_eq!(polygon.vertices[2].y, -28.0);
    let f4 = [
        0x285b_635b,
        0xff88_ff60,
        0x0078_fffe,
        0xffa5_ff60,
        0x0078_ffe5,
    ];
    let polygon = atm::sky_polygon(&f4).expect("a polygon");
    assert_eq!(polygon.vertices.len(), 4);
    assert!(
        polygon
            .vertices
            .iter()
            .all(|v| v.color == [0x5b, 0x63, 0x5b])
    );
    // Textured (the sun strip), semi-transparent, rectangles: not sky bands.
    assert!(atm::sky_polygon(&[0x2e69_6969, 0, 0, 0, 0, 0, 0, 0, 0]).is_none());
    assert!(atm::sky_polygon(&[0x2a00_0000, 0, 0, 0, 0]).is_none());
    assert!(atm::sky_polygon(&[0x605b_635b, 0, 0x0040_0010]).is_none());
    assert!(atm::sky_polygon(&[0x3800_0000, 0]).is_none());
}

#[test]
fn the_fog_colour_word() {
    assert_eq!(atm::fog_color(0x005b_635b), [0x5b, 0x63, 0x5b]);
    let words = |address: u32| match address {
        atm::jp::WORLD => 0x8013_fb58,
        0x8013_fd9c => 0x800c_c000,
        0x800c_c008 => 0x005b_635b,
        _ => 0,
    };
    assert_eq!(atm::read_fog_color(words), [0x5b, 0x63, 0x5b]);
}

#[test]
fn the_depth_cue_of_the_gte_set_up() {
    let (near, far) = (16395.0, 28000.0);
    assert_eq!(atm::depth_cue(near, near, far), 0.0);
    assert!((atm::depth_cue(far, near, far) - 1.0).abs() < 1e-6);
    assert_eq!(atm::depth_cue(5000.0, near, far), 0.0);
    assert_eq!(atm::depth_cue(40000.0, near, far), 1.0);
    // Linear in 1/z: halfway in 1/z is 0.5.
    let z = 2.0 / (1.0 / near + 1.0 / far);
    assert!((atm::depth_cue(z, near, far) - 0.5).abs() < 1e-5);
}

#[test]
fn the_haze_integral_is_exact() {
    let haze = Haze::default();
    // Against a midpoint sum along the ray.
    for (from, to, distance) in [
        (2000.0, 0.0, 30000.0),
        (500.0, 4000.0, 60000.0),
        (3000.0, 3000.0, 100000.0),
        (0.0, 20000.0, 200000.0),
    ] {
        let steps = 20000;
        let mut sum = 0.0f64;
        for i in 0..steps {
            let t = (f64::from(i) + 0.5) / f64::from(steps);
            let altitude = f64::from(from) + t * f64::from(to - from);
            sum += f64::from(haze.density) * (-altitude / f64::from(haze.scale_height)).exp();
        }
        let numeric = sum * f64::from(distance) / f64::from(steps);
        let analytic = f64::from(haze.optical_depth(from, to, distance));
        assert!(
            (numeric - analytic).abs() / numeric < 1e-4,
            "{from} {to} {distance}: {numeric} {analytic}"
        );
    }
    // 10 km at sea level: 1/110,000 per unit, 37% haze.
    let through = (-haze.optical_depth(0.0, 0.0, 50_000.0)).exp();
    assert!((through - (-50_000.0f32 / 110_000.0).exp()).abs() < 1e-6);
    // The drop: 785 m at 100 km.
    assert!((haze.earth_drop(500_000.0) / 5.0 - 785.0).abs() < 2.0);
    // Altitudes: y down, the point above the curved sea.
    let input = HazeInput {
        camera_y: -3000.0,
        offset: [0.0, 1000.0, 40000.0],
        depth: 40000.0,
    };
    let (camera, point) = haze.altitudes(&input);
    assert_eq!(camera, 3000.0);
    assert!((point - (2000.0 + haze.earth_drop(40000.0))).abs() < 1e-3);
    // Farther is hazier.
    let far = HazeInput {
        offset: [0.0, 1000.0, 80000.0],
        ..input
    };
    assert!(haze.transmittance(&far) < haze.transmittance(&input));
}

#[test]
fn the_sun_phase_function_is_normalised() {
    for g in [0.0f32, 0.3, 0.6, 0.85] {
        // The integral over the sphere: 2 pi times the integral over cos.
        let steps = 200_000;
        let sum: f64 = (0..steps)
            .map(|i| {
                let c = -1.0 + 2.0 * (f64::from(i) + 0.5) / f64::from(steps);
                f64::from(atm::henyey_greenstein(c as f32, g))
            })
            .sum::<f64>()
            * 2.0
            / f64::from(steps);
        assert!(
            (sum * 2.0 * std::f64::consts::PI - 1.0).abs() < 1e-3,
            "g {g}: {sum}"
        );
    }
    assert!((atm::henyey_greenstein(0.3, 0.0) - 1.0 / (4.0 * std::f32::consts::PI)).abs() < 1e-7);
    assert!(atm::henyey_greenstein(1.0, 0.6) > 10.0 * atm::henyey_greenstein(0.0, 0.6));
}

#[test]
fn shading_without_haze_is_the_colour() {
    let sky = SkyTable::uniform([0.4, 0.5, 0.8]);
    let scene = HazeScene {
        sky: &sky,
        fog: [0.36, 0.39, 0.36],
        sun: [0.0, -1.0, 0.0],
        sun_color: [1.0; 3],
    };
    let clear = Haze {
        density: 0.0,
        ..Haze::default()
    };
    let input = HazeInput {
        camera_y: -2000.0,
        offset: [3000.0, 1500.0, 20000.0],
        depth: 20000.0,
    };
    let color = [0.2, 0.6, 0.1];
    let shaded = clear.shade(color, &input, &scene);
    assert!((0..3).all(|c| (shaded[c] - color[c]).abs() < 1e-6));
    // Thick haze is the haze colour; the sun term adds toward the sun.
    let thick = Haze {
        density: 1.0,
        sun_strength: 0.0,
        ..Haze::default()
    };
    let hazed = thick.shade(color, &input, &scene);
    let expected = [
        0.4 * 0.65 + 0.36 * 0.35,
        0.5 * 0.65 + 0.39 * 0.35,
        0.8 * 0.65 + 0.36 * 0.35,
    ];
    assert!(
        (0..3).all(|c| (hazed[c] - expected[c]).abs() < 1e-4),
        "{hazed:?}"
    );
    let toward = HazeScene {
        sun: [0.0, 0.0, 1.0],
        ..scene
    };
    let haze = Haze::default();
    let sunny = haze.haze_color(&toward, [0.0, 0.0, 1.0]);
    let away = haze.haze_color(&toward, [0.0, 0.0, -1.0]);
    assert!(sunny[0] > away[0] + 0.5);
    // The edge: at the game's draw distance the point is the sky behind.
    let edged = Haze {
        edge: Some([10000.0, 20000.0]),
        ..clear
    };
    let at_edge = edged.shade(color, &input, &scene);
    assert!((0..3).all(|c| (at_edge[c] - sky.colors[0][c]).abs() < 1e-6));
}

#[test]
fn write_back_helpers() {
    for pixel in [0u16, 0x7fff, 0x1234, 0x4210, 0x03e0] {
        assert_eq!(atm::to_rgb555(atm::from_rgb555(pixel), 0.0), pixel);
    }
    let mean: f32 = (0..4)
        .flat_map(|y| (0..4).map(move |x| atm::dither(x, y)))
        .sum::<f32>()
        / 16.0;
    assert!(mean.abs() < 1e-6);
    assert!((0..16).all(|i| atm::dither(i % 4, i / 4).abs() < 0.5));
}

#[test]
fn the_haze_uniform_rows() {
    let mut sky = SkyTable::uniform([0.0; 3]);
    sky.colors[5] = [0.1, 0.2, 0.3];
    let scene = HazeScene {
        sky: &sky,
        fog: [0.4, 0.5, 0.6],
        sun: [0.0, -1.0, 0.0],
        sun_color: [0.7, 0.8, 0.9],
    };
    let haze = Haze {
        edge: Some([100.0, 200.0]),
        ..Haze::default()
    };
    let rows = haze.uniform(&scene, -3300.0);
    assert_eq!(rows.len(), 38);
    assert_eq!(
        rows[0],
        [
            haze.density,
            haze.scale_height,
            haze.sea_level,
            haze.earth_radius
        ]
    );
    assert_eq!(rows[1], [0.35, 0.6, haze.sun_strength, -3300.0]);
    assert_eq!(rows[2], [100.0, 200.0, 0.0, 0.0]);
    assert_eq!(rows[3], [0.4, 0.5, 0.6, 0.0]);
    assert_eq!(rows[4], [0.0, -1.0, 0.0, 0.0]);
    assert_eq!(rows[5], [0.7, 0.8, 0.9, 0.0]);
    assert_eq!(rows[6 + 5], [0.1, 0.2, 0.3, 0.0]);
    let none = Haze::default().uniform(&scene, 0.0);
    assert!(none[2][1] <= none[2][0], "no edge");
    assert!(atm::HAZE_WGSL.contains("fn haze_shade("));
}

/// A compute device for the WGSL check: skipped without an adapter unless
/// `AC3_REQUIRE_GPU` is set (`WGPU_ADAPTER_NAME` picks one by name).
fn compute_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY.with_env(),
        ..wgpu::InstanceDescriptor::new_without_display_handle_from_env()
    });
    let adapter = match std::env::var("WGPU_ADAPTER_NAME") {
        Ok(name) => {
            let wanted = name.to_lowercase();
            pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()))
                .into_iter()
                .find(|adapter| adapter.get_info().name.to_lowercase().contains(&wanted))
        }
        Err(_) => pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        }))
        .ok(),
    };
    let device = adapter.and_then(|adapter| {
        eprintln!("haze check adapter: {}", adapter.get_info().name);
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("haze check"),
            required_limits: wgpu::Limits::downlevel_defaults(),
            ..Default::default()
        }))
        .ok()
    });
    if device.is_none() {
        assert!(
            std::env::var_os("AC3_REQUIRE_GPU").is_none(),
            "AC3_REQUIRE_GPU: no wgpu device"
        );
        eprintln!("skipped: no graphics adapter");
    }
    device
}

fn float_bytes(values: impl IntoIterator<Item = f32>) -> Vec<u8> {
    values.into_iter().flat_map(f32::to_le_bytes).collect()
}

/// `haze.wgsl` on the GPU gives `Haze::shade` and `Haze::transmittance`.
#[test]
fn haze_wgsl_matches_the_rust_model() {
    let Some((device, queue)) = compute_device() else {
        return;
    };
    let mut sky = SkyTable::uniform([0.0; 3]);
    for (bin, color) in sky.colors.iter_mut().enumerate() {
        let t = bin as f32 / 31.0;
        *color = [0.30 + 0.2 * t, 0.35 + 0.25 * t, 0.36 + 0.5 * t];
    }
    let sun = [0.9045f32, -0.0352, -0.4246];
    let length = (sun[0] * sun[0] + sun[1] * sun[1] + sun[2] * sun[2]).sqrt();
    let scene = HazeScene {
        sky: &sky,
        fog: atm::unit_color([91, 99, 91]),
        sun: sun.map(|c| c / length),
        sun_color: [0.9, 0.85, 0.7],
    };
    let camera_y = -3300.0;
    let cases: Vec<([f32; 3], [f32; 3], f32)> = (0..512u32)
        .map(|i| {
            let x = ((i % 16) as f32 - 8.0) * 1500.0;
            let y = (((i / 16) % 8) as f32 - 2.0) * 400.0 + 300.0;
            let z = 1000.0 + (i / 16) as f32 * 1100.0;
            let color = [
                (i * 37 % 256) as f32 / 255.0,
                (i * 91 % 256) as f32 / 255.0,
                (i * 53 % 256) as f32 / 255.0,
            ];
            (color, [x, y, z], z)
        })
        .collect();
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("haze check"),
        source: wgpu::ShaderSource::Wgsl(
            format!(
                "{}\n{}",
                atm::HAZE_WGSL,
                "@group(0) @binding(0) var<uniform> haze_u: HazeUniform;
struct Case { color: vec4<f32>, offset: vec4<f32> }
@group(0) @binding(1) var<storage, read> cases: array<Case>;
@group(0) @binding(2) var<storage, read_write> results: array<vec4<f32>>;
@compute @workgroup_size(64)
fn check(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if (i >= arrayLength(&cases)) { return; }
    let c = cases[i];
    let shaded = haze_shade(haze_u, c.color.rgb, c.offset.xyz, c.offset.w);
    results[i] = vec4<f32>(shaded, haze_transmittance(haze_u, c.offset.xyz));
}"
            )
            .into(),
        ),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("haze check"),
        layout: None,
        module: &module,
        entry_point: Some("check"),
        compilation_options: Default::default(),
        cache: None,
    });
    let case_bytes = float_bytes(cases.iter().flat_map(|(color, offset, depth)| {
        [
            color[0], color[1], color[2], 0.0, offset[0], offset[1], offset[2], *depth,
        ]
    }));
    let result_size = (cases.len() * 16) as u64;
    let buffer = |label, size, usage| {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size,
            usage,
            mapped_at_creation: false,
        })
    };
    use wgpu::BufferUsages as Usage;
    let uniform = buffer(
        "haze uniform",
        (atm::HAZE_UNIFORM_ROWS * 16) as u64,
        Usage::UNIFORM | Usage::COPY_DST,
    );
    let inputs = buffer(
        "haze cases",
        case_bytes.len() as u64,
        Usage::STORAGE | Usage::COPY_DST,
    );
    let outputs = buffer(
        "haze results",
        result_size,
        Usage::STORAGE | Usage::COPY_SRC,
    );
    let readback = buffer(
        "haze readback",
        result_size,
        Usage::MAP_READ | Usage::COPY_DST,
    );
    queue.write_buffer(&inputs, 0, &case_bytes);
    let layout = pipeline.get_bind_group_layout(0);
    for haze in [
        Haze {
            sea_level: -1020.0,
            edge: Some([22400.0, 28000.0]),
            ..Haze::default()
        },
        Haze {
            density: 1.0 / 30_000.0,
            sea_level: -1020.0,
            ..Haze::default()
        },
    ] {
        let rows = haze.uniform(&scene, camera_y);
        queue.write_buffer(&uniform, 0, &float_bytes(rows.iter().flatten().copied()));
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("haze check"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: inputs.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: outputs.as_entire_binding(),
                },
            ],
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("haze check"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups((cases.len() as u32).div_ceil(64), 1, 1);
        }
        encoder.copy_buffer_to_buffer(&outputs, 0, &readback, 0, result_size);
        queue.submit([encoder.finish()]);
        readback.map_async(wgpu::MapMode::Read, .., |result| {
            result.expect("readback");
        });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("the GPU");
        let gpu: Vec<f32> = {
            let mapped = readback.get_mapped_range(..).expect("mapped");
            mapped
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect()
        };
        readback.unmap();
        let mut worst = 0.0f32;
        for (index, (color, offset, depth)) in cases.iter().enumerate() {
            let input = HazeInput {
                camera_y,
                offset: *offset,
                depth: *depth,
            };
            let cpu = haze.shade(*color, &input, &scene);
            let through = haze.transmittance(&input);
            let got = &gpu[index * 4..index * 4 + 4];
            for (a, b) in cpu.iter().chain([&through]).zip(got) {
                worst = worst.max((a - b).abs());
            }
        }
        eprintln!("haze.wgsl vs Haze::shade: largest difference {worst:e}");
        assert!(worst < 1e-3, "{worst}");
    }
}

// ---- traces of the game -----------------------------------------------------------

fn out_dir() -> PathBuf {
    let out = std::env::var_os("AC3_FX_FOG_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/effects-atmosphere"));
    std::fs::create_dir_all(&out).expect("output folder");
    out
}

/// The VBlanks to trace (`$AC3_FX_FOG_FRAMES`, comma-separated).
fn frames(default: &[u64]) -> Vec<u64> {
    std::env::var("AC3_FX_FOG_FRAMES")
        .ok()
        .map(|list| {
            list.split(',')
                .filter_map(|v| v.trim().parse().ok())
                .collect()
        })
        .unwrap_or_else(|| default.to_vec())
}

/// A traced player, with `$AC3_FX_FOG_PAD` (a `--pad` schedule) held.
fn player() -> TracedPlayer {
    let player = TracedPlayer::new(&jp_disc(), &jp_flight_state());
    if let Ok(text) = std::env::var("AC3_FX_FOG_PAD") {
        let schedule = ac3_port::machine_player::parse_pad_schedule(&text).expect("a pad schedule");
        player.player.host.borrow_mut().pad_input = Some(Box::new(move |vblank| {
            schedule
                .iter()
                .filter(|(first, last, _)| (*first..=*last).contains(&vblank))
                .fold(0, |held, (_, _, b)| held | b)
        }));
    }
    player
}

/// The RAM inputs of a traced flight pass.
struct Scene {
    view: atm::View,
    fog: [u8; 3],
    fog_range: [i16; 2],
    cue_range: [i16; 2],
    /// The strips' slots from the RAM bucket table (+1).
    slots: [u16; atm::STRIPS],
    sun: [f32; 3],
    sun_color: [f32; 3],
    flags: u32,
    animated: u32,
    state: u32,
}

fn scene(trace: &PassTrace) -> Scene {
    let ram = &trace.ram;
    let world = ram.w(atm::jp::WORLD);
    let camera = ram.w(world + atm::jp::CAMERA);
    let mission = ram.w(world + atm::jp::MISSION);
    let matrix = std::array::from_fn(|i| ram.sh(world + atm::jp::VIEW + 2 * i as u32));
    let position = std::array::from_fn(|i| ram.w(camera + 4 + 4 * i as u32) as i32);
    let buckets = ram.w(world + atm::jp::FOG_BUCKETS);
    let context = ram.w(atm::jp::CONTEXT);
    Scene {
        view: atm::View::from_gte(matrix, ram.sh(camera + 0x24), position),
        fog: atm::fog_color(ram.w(mission + atm::jp::FOG_COLOR)),
        fog_range: [
            ram.sh(mission + atm::jp::FOG_NEAR),
            ram.sh(mission + atm::jp::FOG_FAR),
        ],
        cue_range: [ram.sh(camera + 0x28), ram.sh(camera + 0x2a)],
        slots: std::array::from_fn(|i| ram.h(buckets + 2 * i as u32) + 1),
        sun: std::array::from_fn(|i| {
            f32::from(ram.sh(world + atm::jp::SUN + 2 * i as u32)) / 4096.0
        }),
        sun_color: [0u32, 6, 12].map(|offset| {
            (f32::from(ram.sh(world + atm::jp::LIGHT_COLOR + offset)) / 4096.0).clamp(0.0, 1.0)
        }),
        flags: ram.w(world + atm::jp::WORLD_FLAGS),
        animated: ram.w(mission + 0x74),
        state: ram.w(context + 0x2c),
    }
}

/// The texture depth a textured command samples with (polygons: their
/// page word; rectangles: the page in effect, `page`).
fn texture_entries(words: &[u32], at: usize, page: &mut u32) -> u16 {
    if let Some(index) = tpage_word_index(words[at]).filter(|&i| at + i < words.len()) {
        *page = words[at + index] >> 16;
    }
    if words[at] >> 24 == 0xe1 {
        *page = words[at] & 0xffff;
    }
    match (*page >> 7) & 3 {
        0 => 16,
        1 => 256,
        _ => 0,
    }
}

/// The redirect rule applied to a packet's commands (the edit the GPU
/// renderer's CLUT hook makes at decode time), tracking the texture page.
fn redirect(packet: &Packet, page: &mut u32, redirected: &mut usize) -> Step {
    let phase = atm::phase_of_slot(packet.slot);
    let mut words = packet.words.clone();
    let mut changed = false;
    for (at, len) in commands(&packet.words) {
        let entries = texture_entries(&packet.words, at, page);
        let Some(index) = clut_word_index(packet.words[at]).filter(|&i| i < len) else {
            continue;
        };
        let word = packet.words[at + index];
        if entries == 0 {
            continue;
        }
        if let Some(clut) = atm::clut_word_redirect((word >> 16) as u16, entries, phase) {
            words[at + index] = (word & 0xffff) | (u32::from(clut) << 16);
            changed = true;
            *redirected += 1;
        }
    }
    if changed {
        Step::Edit(words)
    } else {
        Step::Run
    }
}

/// The game's strips removed (an independent way to an unfogged pass):
/// GP0 80 copies into the palette block and the fill drawn into it become
/// NOPs.
fn without_strips(packet: &Packet, state: &State<'_>, removed: &mut usize) -> Step {
    let mut words = packet.words.clone();
    let mut changed = false;
    let mut area = state.area;
    for (at, len) in commands(&packet.words) {
        let command = packet.words[at];
        let op = command >> 24;
        if op == 0xe3 {
            area[0] = (command & 0x3ff) as i32;
            area[1] = ((command >> 10) & 0x3ff) as i32;
        }
        let palette = atm::FOG_PALETTES;
        let into_block = match op {
            0x80 if len == 4 => {
                let dest = packet.words[at + 2];
                palette.contains(dest as u16, (dest >> 16) as u16)
            }
            0x60..=0x63 => area[0] == i32::from(palette.x) && area[1] == i32::from(palette.y),
            _ => false,
        };
        if into_block {
            words[at..at + len].fill(0);
            changed = true;
            *removed += 1;
        }
    }
    if changed {
        Step::Edit(words)
    } else {
        Step::Run
    }
}

/// A 16-colour palette row of VRAM.
fn palette(words: &[u16], x: u16, y: u16) -> [u16; 16] {
    std::array::from_fn(|i| words[usize::from(y) * 1024 + usize::from(x) + i])
}

/// The frame's draw area and drawing offset (at its first world polygon).
fn frame_area(trace: &PassTrace) -> ([i32; 4], [i32; 2]) {
    let mut found = None;
    let mut replay = Replay::new(trace);
    replay.run(&mut |packet, state| {
        let polygon = commands(&packet.words)
            .iter()
            .any(|&(at, _)| (0x20..0x40).contains(&(packet.words[at] >> 24)));
        if atm::phase_of_slot(packet.slot) == Phase::World && polygon {
            found = Some((state.area, state.offset));
            Step::Stop
        } else {
            Step::Run
        }
    });
    found.expect("a world polygon")
}

/// The sky slot's band polygons.
fn sky_polygons(trace: &PassTrace) -> Vec<atm::SkyPolygon> {
    trace
        .world_packets()
        .filter(|packet| packet.slot == Some(4095))
        .flat_map(|packet| {
            commands(&packet.words)
                .into_iter()
                .filter_map(|(at, len)| atm::sky_polygon(&packet.words[at..at + len]))
                .collect::<Vec<_>>()
        })
        .collect()
}

fn rgb555(color: [u8; 3]) -> u16 {
    u16::from(color[0] >> 3) | (u16::from(color[1] >> 3) << 5) | (u16::from(color[2] >> 3) << 10)
}

#[test]
#[ignore = "needs the JP disc and the mission 1 flight save state (AC3_JP_DIR)"]
fn jp_fog_mechanism_and_redirect_on_captured_frames() {
    let mut player = player();
    let mut report = Vec::new();
    let mut sky_fog_vertices = 0;
    for at in frames(&[50100, 50300, 50500]) {
        player.run_to(at);
        let trace = player.trace_world();
        let actual = player.player.host.borrow().gpu.gpu.vram().words().to_vec();
        let scene = scene(&trace);
        let ram = &trace.ram;
        let vblank = trace.vblank;
        assert_eq!(scene.state, 2, "VBlank {vblank}: flight");
        assert_eq!(scene.flags & atm::jp::NO_FOG, 0, "VBlank {vblank}: fog on");

        // The layout constants are the game's.
        let source: Vec<i16> = (0..4)
            .map(|i| ram.sh(atm::jp::PALETTE_SOURCE + 2 * i))
            .collect();
        let area = atm::FOG_PALETTES;
        assert_eq!(source, [area.x, area.y, area.w, area.h].map(|v| v as i16));
        for (step, &level) in atm::FOG_LEVELS.iter().enumerate() {
            assert_eq!(ram.h(atm::jp::RSIN_TABLE + 2 * 64 * step as u32), level);
        }
        // The strips' slots: the RAM table is 8005EAB8's of section 0.
        assert_eq!(
            scene.slots,
            atm::bucket_slots(scene.fog_range[0], scene.fog_range[1])
        );

        // Every strip, word for word, in its slot (800943A8).
        let mask = 0xe600_0000;
        for strip in 0..16 {
            let slot = scene.slots[strip];
            let packets: Vec<&Packet> = trace
                .world_packets()
                .filter(|p| p.slot == Some(slot) && p.words.first() == Some(&0xe600_0000))
                .filter(|p| p.words.get(2) == Some(&0x8000_0000))
                .collect();
            match atm::strip_source(strip) {
                atm::StripSource::Block { x, y } => {
                    assert_eq!(packets.len(), 1, "VBlank {vblank} strip {strip}");
                    let expected = [
                        0xe600_0000,
                        0x0100_0000,
                        0x8000_0000,
                        u32::from(x) | (u32::from(y) << 16),
                        0x0100_0140,
                        0x0040_0010,
                        mask,
                    ];
                    assert_eq!(packets[0].words, expected, "strip {strip}");
                }
                atm::StripSource::Rows { x, y } => {
                    assert_eq!(packets.len(), 2, "VBlank {vblank} strip {strip}");
                    let mut copies = Vec::new();
                    for packet in &packets {
                        for (at, _) in commands(&packet.words) {
                            if packet.words[at] == 0x8000_0000 {
                                copies.push(packet.words[at + 1..at + 4].to_vec());
                            }
                        }
                    }
                    copies.sort();
                    let mut expected: Vec<Vec<u32>> = (0..4u32)
                        .map(|k| {
                            vec![
                                (u32::from(x) + 16 * k) | (u32::from(y) << 16),
                                0x0100_0140 + 0x10_0000 * k,
                                0x0010_0010,
                            ]
                        })
                        .collect();
                    expected.sort();
                    assert_eq!(copies, expected, "strip {strip}");
                }
                atm::StripSource::Fill => unreachable!(),
            }
        }
        // Strip 16 in the sky slot: draw area (320,256)..(335,319), offset
        // (320,256), a 16x64 TILE of the fog colour with the mask bit set.
        let fog_word = u32::from_le_bytes([scene.fog[0], scene.fog[1], scene.fog[2], 0]);
        let sky: Vec<&Packet> = trace
            .world_packets()
            .filter(|p| p.slot == Some(4095))
            .collect();
        let at = sky
            .iter()
            .position(|p| p.words == [0xe304_0140, 0xe404_fd4f, 0xe508_0140, 0])
            .expect("strip 16's draw area");
        assert_eq!(
            sky[at + 1].words,
            [0xe600_0001, 0x6000_0000 | fog_word, 0, 0x0040_0010, mask]
        );
        // The sky's lowest bands are in the fog colour too (when on screen).
        let polygons = sky_polygons(&trace);
        let fog_vertices = polygons
            .iter()
            .flat_map(|p| &p.vertices)
            .filter(|v| v.color == scene.fog)
            .count();
        sky_fog_vertices += fog_vertices;

        // VRAM: every step is the palette builder's interpolation of step 0
        // (all 1,024 colours): step 0 is the unfogged palette.
        let vram = trace.gpu.vram().words();
        let step0: Vec<[u16; 16]> = (0..64)
            .map(|row| {
                palette(
                    vram,
                    atm::UNFOGGED_PALETTES[0],
                    atm::UNFOGGED_PALETTES[1] + row,
                )
            })
            .collect();
        for step in 0..16 {
            let level = atm::strip_level(step);
            for (row, source) in step0.iter().enumerate() {
                let row = row as u16;
                let stored: [u16; 16] = match atm::strip_source(step) {
                    atm::StripSource::Block { x, y } => palette(vram, x, y + row),
                    atm::StripSource::Rows { x, y } => {
                        palette(vram, x + 16 * (row / 16), y + row % 16)
                    }
                    atm::StripSource::Fill => unreachable!(),
                };
                let expected = source.map(|c| atm::fog_palette_color(c, scene.fog, level));
                assert_eq!(stored, expected, "VBlank {vblank} step {step} row {row}");
            }
        }

        // The trace re-renders the pass exactly.
        let mut replay = Replay::new(&trace);
        let mut checked = 0usize;
        let mut terrain_by_strip = [0usize; atm::STRIPS];
        let mut other_fog_reads = 0usize;
        replay.finish(&mut |packet, state| {
            // Each terrain primitive reads the palette of its slot's strip.
            for (at, len) in commands(&packet.words) {
                let Some(index) = clut_word_index(packet.words[at]).filter(|&i| i < len) else {
                    continue;
                };
                let [x, y] = atm::clut_position((packet.words[at + index] >> 16) as u16);
                if !area.contains(x, y) {
                    continue;
                }
                if atm::phase_of_slot(packet.slot) != Phase::World {
                    other_fog_reads += 1;
                    continue;
                }
                let strip = atm::strip_for_slot(packet.slot.expect("slot"), &scene.slots);
                let row = usize::from(y - area.y);
                let expected = if strip == 16 {
                    [rgb555(scene.fog) | 0x8000; 16]
                } else {
                    step0[row]
                        .map(|c| atm::fog_palette_color(c, scene.fog, atm::strip_level(strip)))
                };
                assert_eq!(
                    palette(state.gpu.vram().words(), x, y),
                    expected,
                    "VBlank {vblank} slot {:?} strip {strip}",
                    packet.slot
                );
                terrain_by_strip[strip] += 1;
                checked += 1;
            }
            Step::Run
        });
        assert!(
            replay.gpu.vram().words() == actual.as_slice(),
            "VBlank {vblank}: replay"
        );
        assert!(
            checked > 100,
            "VBlank {vblank}: {checked} terrain primitives"
        );

        // The redirect: equal to the pass with the strips removed (and the
        // block preloaded with step 0), everywhere but the block itself.
        let mut page = 0;
        let mut redirected = 0;
        let mut redirect_replay = Replay::new(&trace);
        redirect_replay.finish(&mut |packet, _| redirect(packet, &mut page, &mut redirected));
        let mut stripped = Replay::new(&trace);
        let mut preloaded = vram.to_vec();
        for (row, colors) in step0.iter().enumerate() {
            let at = (usize::from(area.y) + row) * 1024 + usize::from(area.x);
            preloaded[at..at + 16].copy_from_slice(colors);
        }
        stripped.set_vram(&preloaded);
        let mut removed = 0;
        stripped.finish(&mut |packet, state| without_strips(packet, state, &mut removed));
        assert_eq!(removed, 16 + 8 * 3 + 1, "VBlank {vblank}: strip commands");
        let (a, b) = (
            redirect_replay.gpu.vram().words(),
            stripped.gpu.vram().words(),
        );
        let mut differing = 0;
        for y in 0..512u16 {
            for x in 0..1024u16 {
                let index = usize::from(y) * 1024 + usize::from(x);
                if a[index] != b[index] && !area.contains(x, y) {
                    differing += 1;
                }
            }
        }
        assert_eq!(differing, 0, "VBlank {vblank}: redirect vs strips removed");
        // It changed the picture where fogged terrain drew, not elsewhere.
        let ([x0, y0, x1, y1], _) = frame_area(&trace);
        let changed = (y0..=y1)
            .flat_map(|y| (x0..=x1).map(move |x| (y * 1024 + x) as usize))
            .filter(|&index| a[index] != actual[index])
            .count();
        let fogged = terrain_by_strip[1..].iter().sum::<usize>();
        assert_eq!(
            redirected, checked,
            "VBlank {vblank}: every terrain read redirected"
        );
        assert!(fogged == 0 || changed > 0, "VBlank {vblank}");
        report.push(serde_json::json!({
            "vblank": vblank,
            "terrain_primitives": checked,
            "by_strip": terrain_by_strip,
            "fog_block_reads_outside_world": other_fog_reads,
            "redirected": redirected,
            "pixels_changed": changed,
            "fog": scene.fog,
            "fog_range": scene.fog_range,
            "depth_cue_range": scene.cue_range,
            "strip_slots": scene.slots,
            "animated_entries": scene.animated,
            "sky_fog_colour_vertices": fog_vertices,
        }));
    }
    assert!(sky_fog_vertices > 0, "the sky's fog-colour bands");
    let out = out_dir();
    std::fs::write(
        out.join("mechanism.json"),
        serde_json::to_string_pretty(&report).expect("json"),
    )
    .expect("report");
    println!("{}", serde_json::to_string_pretty(&report).expect("json"));
}

// ---- mock-ups ----------------------------------------------------------------------------

/// A re-rendered pass: VRAM and what the haze used.
struct Rendered {
    vram: Vec<u16>,
    depth: DepthBuffer,
    /// 1 - transmittance per frame pixel (0 where nothing was hazed).
    haze: Vec<f32>,
}

/// Re-render `trace`: `redirect_on` applies the rule; `haze` adds the post
/// pass at world end (before the flare and HUD slots) on the depth rebuilt
/// from the traced polygons, written back dithered.
fn render(trace: &PassTrace, scene: &Scene, redirect_on: bool, haze: Option<&Haze>) -> Rendered {
    let (area, _) = frame_area(trace);
    let (w, h) = (area[2] - area[0] + 1, area[3] - area[1] + 1);
    let mut depth = DepthBuffer::new(area[0], area[1], w, h);
    let mut replay = Replay::new(trace);
    let mut page = 0;
    let mut redirected = 0;
    let mut world_end = None;
    let mut world_begin: Option<Vec<u16>> = None;
    let stopped = replay.run(&mut |packet, state| {
        if haze.is_some() && matches!(atm::phase_of_slot(packet.slot), Phase::Flare | Phase::Hud) {
            world_end = Some(state.offset);
            return Step::Stop;
        }
        if atm::phase_of_slot(packet.slot) == Phase::World {
            if world_begin.is_none() {
                world_begin = Some(state.gpu.vram().words().to_vec());
            }
            depth.add(packet, state.offset);
        }
        if redirect_on {
            redirect(packet, &mut page, &mut redirected)
        } else {
            Step::Run
        }
    });
    let mut amounts = vec![0.0f32; (w * h) as usize];
    if let (true, Some(haze), Some(offset)) = (stopped, haze, world_end) {
        let polygons = sky_polygons(trace);
        let sky = atm::bin_sky(&polygons, &scene.view, 2.0)
            .unwrap_or_else(|| SkyTable::uniform(atm::unit_color(scene.fog)));
        let haze_scene = HazeScene {
            sky: &sky,
            fog: atm::unit_color(scene.fog),
            sun: scene.sun,
            sun_color: scene.sun_color,
        };
        let mut words = replay.gpu.vram().words().to_vec();
        // Pixels the world drew that the rebuilt depth missed (its float
        // coverage differs from the GPU's at polygon edges) take the
        // farthest depth next to them: the renderer's z target has no gaps.
        if let Some(begin) = &world_begin {
            for _ in 0..3 {
                let previous = depth.z.clone();
                for y in 0..h {
                    for x in 0..w {
                        let index = (y * w + x) as usize;
                        let vram_index = ((y + area[1]) * 1024 + x + area[0]) as usize;
                        if previous[index] > 0.0 || words[vram_index] == begin[vram_index] {
                            continue;
                        }
                        let mut best = 0.0f32;
                        for (dx, dy) in [
                            (-1, 0),
                            (1, 0),
                            (0, -1),
                            (0, 1),
                            (-1, -1),
                            (1, 1),
                            (-1, 1),
                            (1, -1),
                        ] {
                            let (nx, ny) = (x + dx, y + dy);
                            if nx >= 0 && ny >= 0 && nx < w && ny < h {
                                best = best.max(previous[(ny * w + nx) as usize]);
                            }
                        }
                        if best > 0.0 {
                            depth.z[index] = best;
                        }
                    }
                }
            }
        }
        for py in area[1]..=area[3] {
            for px in area[0]..=area[2] {
                let z = depth.at(px, py);
                if z <= 0.0 {
                    continue;
                }
                let (sx, sy) = ((px - offset[0]) as f32, (py - offset[1]) as f32);
                let input = HazeInput {
                    camera_y: scene.view.position[1],
                    offset: scene.view.offset(sx, sy, z),
                    depth: z,
                };
                let index = (py * 1024 + px) as usize;
                let color = atm::from_rgb555(words[index]);
                let shaded = haze.shade(color, &input, &haze_scene);
                let bias = atm::dither(px as u32, py as u32);
                words[index] = atm::to_rgb555(shaded, bias) | (words[index] & 0x8000);
                amounts[((py - area[1]) * w + (px - area[0])) as usize] =
                    1.0 - haze.transmittance(&input);
            }
        }
        replay.set_vram(&words);
    }
    replay.finish(&mut |packet, _| {
        if redirect_on {
            redirect(packet, &mut page, &mut redirected)
        } else {
            Step::Run
        }
    });
    Rendered {
        vram: replay.gpu.vram().words().to_vec(),
        depth,
        haze: amounts,
    }
}

fn picture(vram: &[u16], area: [i32; 4]) -> Vec<u8> {
    let bytes: Vec<u8> = vram.iter().flat_map(|w| w.to_le_bytes()).collect();
    let vram = ac3_graphics::Vram::from_le_bytes(&bytes).expect("vram");
    crop(
        &vram,
        area[0],
        area[1],
        area[2] - area[0] + 1,
        area[3] - area[1] + 1,
    )
}

/// A 0..1 map as grey.
fn grey(values: &[f32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|&v| {
            let g = (v.clamp(0.0, 1.0) * 255.0) as u8;
            [g, g, g]
        })
        .collect()
}

/// The sea level of mission 1's frames: the lowest flat terrain (the most
/// common world y among the lowest tenth of terrain vertices).
fn sea_level(trace: &PassTrace, scene: &Scene) -> Option<f32> {
    let mut heights = Vec::new();
    for packet in trace.world_packets() {
        if atm::phase_of_slot(packet.slot) != Phase::World {
            continue;
        }
        for (at, len) in commands(&packet.words) {
            let Some(index) = clut_word_index(packet.words[at]).filter(|&i| i < len) else {
                continue;
            };
            let [x, y] = atm::clut_position((packet.words[at + index] >> 16) as u16);
            if !atm::FOG_PALETTES.contains(x, y) {
                continue;
            }
            for vertex in fog_trace::polygon_vertices(packet.words[at]) {
                if let Some(Some([sx, sy, z])) = packet.precise.get(at + vertex)
                    && *z > 0.0
                {
                    let offset = scene.view.offset(*sx, *sy, *z);
                    heights.push(((scene.view.position[1] + offset[1]) / 4.0).round() as i32 * 4);
                }
            }
        }
    }
    let lowest = *heights.iter().max()?;
    let count = heights.iter().filter(|&&h| (lowest - h).abs() <= 8).count();
    (count * 20 >= heights.len()).then_some(lowest as f32)
}

#[test]
#[ignore = "needs the JP disc and the mission 1 flight save state (AC3_JP_DIR)"]
fn jp_atmosphere_mockups() {
    // Mission 1's sea (the water of the replay frames, world y).
    const SEA: f32 = -1020.0;
    let out = out_dir();
    let mut player = player();
    let scale = 3;
    let mut rows = Vec::new();
    let mut summary = Vec::new();
    // In flight (cockpit), then the replay after the crash (external views
    // of the coast and the hills, and the crash's explosion).
    for at in frames(&[50100, 50700, 50900, 51100, 52100]) {
        player.run_to(at);
        let trace = player.trace_world();
        let scene = scene(&trace);
        let vblank = trace.vblank;
        let measured = sea_level(&trace, &scene);
        let (area, _) = frame_area(&trace);
        let (w, h) = (
            (area[2] - area[0] + 1) as u32,
            (area[3] - area[1] + 1) as u32,
        );
        let far = f32::from(scene.cue_range[1]);
        let haze = Haze {
            sea_level: SEA,
            edge: Some([0.8 * far, far]),
            ..Haze::default()
        };
        let no_edge = Haze { edge: None, ..haze };
        // Three times the density: haze closer to the game's own depth.
        let dense = Haze {
            density: 3.0 / 110_000.0,
            ..haze
        };
        let before = render(&trace, &scene, false, None);
        let unfogged = render(&trace, &scene, true, None);
        let after = render(&trace, &scene, true, Some(&haze));
        let after_no_edge = render(&trace, &scene, true, Some(&no_edge));
        let after_dense = render(&trace, &scene, true, Some(&dense));
        let pictures = [
            ("before", picture(&before.vram, area)),
            ("unfogged", picture(&unfogged.vram, area)),
            ("after", picture(&after.vram, area)),
            ("after-no-edge", picture(&after_no_edge.vram, area)),
            ("after-dense", picture(&after_dense.vram, area)),
        ];
        for (name, rgb) in &pictures {
            let big = upscale(rgb, w, h, scale);
            save_png(
                &out.join(format!("{vblank:06}-{name}.png")),
                &big,
                w * scale,
                h * scale,
            );
        }
        // The fog amounts: the game's (its strip by slot) and the haze's.
        let slots = &scene.slots;
        let game_fog: Vec<f32> = after
            .depth
            .slot
            .iter()
            .zip(&after.depth.z)
            .map(|(&slot, &z)| {
                if z > 0.0 {
                    f32::from(atm::strip_level(atm::strip_for_slot(slot, slots))) / 4096.0
                } else {
                    0.0
                }
            })
            .collect();
        let depth_view: Vec<f32> = after
            .depth
            .z
            .iter()
            .map(|&z| {
                if z > 0.0 {
                    1.0 - (z / 30000.0).sqrt()
                } else {
                    0.0
                }
            })
            .collect();
        let maps = [grey(&depth_view), grey(&game_fog), grey(&after.haze)];
        let (maps, maps_w) = side_by_side(
            &[
                (maps[0].as_slice(), w),
                (maps[1].as_slice(), w),
                (maps[2].as_slice(), w),
            ],
            h,
            4,
        );
        save_png(&out.join(format!("{vblank:06}-maps.png")), &maps, maps_w, h);
        // The sky model of the frame: 32 swatches.
        let polygons = sky_polygons(&trace);
        let table = atm::bin_sky(&polygons, &scene.view, 2.0);
        if let Some(table) = &table {
            let mut swatches = Vec::new();
            for _ in 0..16 {
                for bin in (0..atm::SKY_BINS).rev() {
                    let c = table.colors[bin].map(|v| (v * 255.0) as u8);
                    for _ in 0..16 {
                        swatches.extend_from_slice(&c);
                    }
                }
            }
            save_png(
                &out.join(format!("{vblank:06}-sky-table.png")),
                &swatches,
                16 * atm::SKY_BINS as u32,
                16,
            );
        }
        let [before_rgb, unfogged_rgb, after_rgb, dense_rgb] = [
            &pictures[0].1,
            &pictures[1].1,
            &pictures[2].1,
            &pictures[4].1,
        ]
        .map(|rgb| upscale(rgb, w, h, 2));
        let (row, row_w) = side_by_side(
            &[
                (before_rgb.as_slice(), w * 2),
                (unfogged_rgb.as_slice(), w * 2),
                (after_rgb.as_slice(), w * 2),
                (dense_rgb.as_slice(), w * 2),
            ],
            h * 2,
            8,
        );
        rows.push((row, row_w, h * 2));
        summary.push(serde_json::json!({
            "vblank": vblank,
            "camera": scene.view.position,
            "h": scene.view.h,
            "fog": scene.fog,
            "sun": scene.sun,
            "sun_color": scene.sun_color,
            "sea_level": haze.sea_level,
            "lowest_flat_terrain": measured,
            "edge": haze.edge,
            "depth_polygons": [after.depth.precise, after.depth.estimated],
            "sky_polygons": polygons.len(),
            "sky_table": table.map(|t| t.colors.to_vec()),
        }));
    }
    if let Some(&(_, width, _)) = rows.first() {
        let parts: Vec<(&[u8], u32)> = rows
            .iter()
            .map(|(rgb, _, h)| (rgb.as_slice(), *h))
            .collect();
        let (sheet, height) = stacked(&parts, width, 8);
        save_png(
            &out.join("sheet-before-unfogged-after-dense.png"),
            &sheet,
            width,
            height,
        );
    }
    std::fs::write(
        out.join("mockups.json"),
        serde_json::to_string_pretty(&summary).expect("json"),
    )
    .expect("summary");
    println!("{}", serde_json::to_string_pretty(&summary).expect("json"));
}

/// Per-polygon colours of the world for the IR0 = 0 experiment: run once
/// as is and once with the GTE's IR0 forced to 0 after RTPS/RTPT (a
/// throw-away build; `tools/effects/compare_depth_cue.py` compares them).
#[test]
#[ignore = "needs the JP disc and the mission 1 flight save state (AC3_JP_DIR)"]
fn jp_depth_cue_report() {
    let label = std::env::var("AC3_FX_FOG_LABEL").unwrap_or_else(|_| "normal".to_owned());
    let out = out_dir().join(format!("depth-cue-{label}"));
    std::fs::create_dir_all(&out).expect("output folder");
    let mut player = player();
    for at in frames(&[50100, 50300, 50500]) {
        player.run_to(at);
        let trace = player.trace_world();
        let scene = scene(&trace);
        let vblank = trace.vblank;
        let (area, _) = frame_area(&trace);
        let (w, h) = (
            (area[2] - area[0] + 1) as u32,
            (area[3] - area[1] + 1) as u32,
        );
        let rendered = render(&trace, &scene, false, None);
        save_png(
            &out.join(format!("{vblank:06}.png")),
            &upscale(&picture(&rendered.vram, area), w, h, 3),
            w * 3,
            h * 3,
        );
        let unfogged = render(&trace, &scene, true, None);
        save_png(
            &out.join(format!("{vblank:06}-unfogged.png")),
            &upscale(&picture(&unfogged.vram, area), w, h, 3),
            w * 3,
            h * 3,
        );
        let mut polygons = Vec::new();
        for packet in trace.world_packets() {
            for (at, len) in commands(&packet.words) {
                let op = packet.words[at] >> 24;
                if !(0x20..=0x3f).contains(&op) {
                    continue;
                }
                let vertices = fog_trace::polygon_vertices(packet.words[at]);
                if vertices.iter().any(|&i| i >= len) {
                    continue;
                }
                let gouraud = op & 0x10 != 0;
                let colors: Vec<u32> = (0..vertices.len())
                    .map(|v| {
                        if gouraud && v > 0 {
                            packet.words[at + vertices[v] - 1] & 0xff_ffff
                        } else {
                            packet.words[at] & 0xff_ffff
                        }
                    })
                    .collect();
                let z = vertices
                    .iter()
                    .filter_map(|&i| packet.precise[at + i].map(|p| p[2]))
                    .fold(0.0f32, f32::max);
                let clut = clut_word_index(packet.words[at])
                    .filter(|&i| i < len)
                    .map(|i| atm::clut_position((packet.words[at + i] >> 16) as u16));
                polygons.push(serde_json::json!({
                    "address": packet.address + 4 * (at as u32 + 1),
                    "slot": packet.slot,
                    "op": op,
                    "z": z,
                    "colors": colors,
                    "clut": clut,
                }));
            }
        }
        let report = serde_json::json!({
            "vblank": vblank,
            "depth_cue_range": scene.cue_range,
            "fog": scene.fog,
            "polygons": polygons,
        });
        std::fs::write(
            out.join(format!("{vblank:06}.json")),
            serde_json::to_string(&report).expect("json"),
        )
        .expect("report");
    }
}
