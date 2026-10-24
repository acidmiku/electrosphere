//! Far terrain's unit tests: the map data, the backdrop strips, the
//! recorder with effects off, the projection, and the card against the CPU
//! reference on a synthetic mission map (`AC3_REQUIRE_GPU` makes the GPU
//! tests fail instead of skipping without an adapter).
use super::card::{ring_cells, uniform};
use super::*;
use crate::effects::terrain_map::{Source, TerrainMap};
use crate::gpu_renderer::execute::offscreen_device;
use crate::gpu_renderer::fx::{self, FxFrame};
use crate::gpu_renderer::record::VERTEX_WORDS;
use crate::gpu_renderer::tiles::Rect;
use crate::gpu_renderer::{Executor, Frame, Op, Options, Recorder};
use ac3_graphics::{Gpu, Vram};
use std::sync::{Mutex, MutexGuard, OnceLock};

const SEA: i16 = -1024;
const HILL: i16 = -3000;
const GRID: u32 = 0x8014_0000;
const DIRECTORY: u32 = 0x8016_0000;
const MESHES: [u32; 2] = [0x8016_1000, 0x8016_2000];
const TPAGE: u32 = 0x2b;
const CLUT: u32 = (100 << 6) | 3;
/// Mission colours (RGB555): the sea (blue at least red: the sea rule)
/// and the hills.
const SEA_COLOUR: u16 = 6 | (10 << 5) | (14 << 10);
const HILL_COLOUR: u16 = 10 | (14 << 5) | (4 << 10);

fn put(ram: &mut [u8], address: u32, value: u32) {
    let at = (address & 0x1f_ffff) as usize;
    ram[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn get(ram: &[u8], address: u32) -> u32 {
    let at = (address & 0x1f_ffff) as usize;
    u32::from_le_bytes(ram[at..at + 4].try_into().unwrap())
}

/// One tile mesh at `mesh`: vertices (x, y, z), faces (four indices and
/// four uvs), lowest and highest points.
fn put_mesh(
    ram: &mut [u8],
    mesh: u32,
    vertices: &[[i16; 3]],
    faces: &[([u8; 4], [[u8; 2]; 4])],
    floor: i16,
    top: i16,
) {
    let list = mesh + 0x100;
    let flat = mesh + 0x400;
    let gouraud = mesh + 0x800;
    put(ram, mesh + 4, list);
    put(ram, mesh + 0xc, flat);
    put(ram, mesh + 0x10, gouraud);
    put(
        ram,
        mesh + 0x14,
        u32::from(floor as u16) | (u32::from(top as u16) << 16),
    );
    put(ram, list, vertices.len() as u32);
    for (k, v) in vertices.iter().enumerate() {
        let group = list + 4 + 20 * (k as u32 / 3);
        let i = k as u32 % 3;
        let half = |ram: &mut [u8], address: u32, value: i16| {
            let at = (address & 0x1f_ffff) as usize;
            ram[at..at + 2].copy_from_slice(&value.to_le_bytes());
        };
        half(ram, group + 4 * i, v[0]);
        half(ram, group + 4 * i + 2, v[1]);
        half(ram, group + 12 + 2 * i, v[2]);
    }
    put(ram, flat, faces.len() as u32);
    for (f, (indices, uv)) in faces.iter().enumerate() {
        let face = flat + 4 + 0x24 * f as u32;
        put(ram, face + 0x10, u32::from_le_bytes(*indices));
        let uv_word = |k: usize| u32::from(uv[k][0]) | (u32::from(uv[k][1]) << 8);
        put(ram, face + 0x14, uv_word(0) | (CLUT << 16));
        put(ram, face + 0x18, uv_word(1) | (TPAGE << 16));
        put(ram, face + 0x1c, uv_word(2) | (uv_word(3) << 16));
        put(ram, face + 0x20, 0x2c80_8080);
    }
    put(ram, gouraud, 0);
}

/// A flight RAM image (the `fx::scene` tests' words) with a synthetic
/// mission map: tile type 1 flat sea at y = SEA, tile type 2 a pyramid hill
/// rising to HILL, the grid all sea but `hills`; and the VRAM its textures
/// read (a 4-bit page at (704, 0), palette at (48, 100)).
fn synthetic(hills: &[(usize, usize)]) -> (Vec<u8>, Vec<u16>) {
    let mut ram = fx::scene::tests::flight_ram();
    let world = get(&ram, ac3_flight_jp::GP + ac3_flight_jp::common::GAMEPLAY);
    put(&mut ram, world + 0x248, GRID);
    put(&mut ram, world + 0x24c, DIRECTORY);
    for z in 0..256u32 {
        for x in 0..256u32 {
            let tile = if hills.contains(&(x as usize, z as usize)) {
                2
            } else {
                1
            };
            let at = ((GRID + 2 * (z * 256 + x)) & 0x1f_ffff) as usize;
            ram[at..at + 2].copy_from_slice(&(tile as u16).to_le_bytes());
        }
    }
    put(&mut ram, DIRECTORY, 2);
    put(&mut ram, DIRECTORY + 4, MESHES[0]);
    put(&mut ram, DIRECTORY + 8, MESHES[1]);
    let corners = |y: i16| {
        [
            [-1024, y, -1024],
            [1024, y, -1024],
            [-1024, y, 1024],
            [1024, y, 1024],
        ]
    };
    let sea_uv = [[0, 0], [63, 0], [0, 63], [63, 63]];
    put_mesh(
        &mut ram,
        MESHES[0],
        &corners(SEA),
        &[([2, 3, 0, 1], sea_uv)],
        SEA,
        SEA,
    );
    let mut pyramid = corners(SEA).to_vec();
    pyramid.push([0, HILL, 0]);
    let hill_uv = [[0, 64], [63, 64], [32, 127], [32, 127]];
    let faces = [[2, 3, 4, 4], [3, 1, 4, 4], [1, 0, 4, 4], [0, 2, 4, 4]].map(|i| (i, hill_uv));
    put_mesh(&mut ram, MESHES[1], &pyramid, &faces, SEA, HILL);
    let mut vram = vec![0u16; 1024 * 512];
    for v in 0..128 {
        for w in 0..16 {
            vram[v * 1024 + 704 + w] = if v < 64 { 0x1111 } else { 0x2222 };
        }
    }
    vram[100 * 1024 + 48 + 1] = SEA_COLOUR;
    vram[100 * 1024 + 48 + 2] = HILL_COLOUR;
    (ram, vram)
}

fn maps(ram: &[u8], vram: &[u16]) -> (TerrainMap, TerrainMap, MapData) {
    let baked = TerrainMap::build(&Source { ram, vram }).unwrap();
    let patched = neutral_ram(ram, &baked);
    let neutral = TerrainMap::build(&Source {
        ram: &patched,
        vram,
    })
    .unwrap();
    let data = MapData::new(&baked, &neutral);
    (baked, neutral, data)
}

#[test]
fn the_map_data_has_levels_tops_and_neutral_colours() {
    let (ram, vram) = synthetic(&[(130, 140)]);
    let (baked, neutral, data) = maps(&ram, &vram);
    assert_eq!(data.sea_level, Some(f32::from(SEA)));
    assert_eq!(
        data.vertices.len(),
        baked.mesh.vertices.len() * VERTEX_BYTES
    );
    // The sea is flat at every level, its colour the texel times the
    // vertex colour / 128 (baked) or 0x83 / 128 (neutral).
    let channel = |c: u16, s: u16| f32::from((c >> s) & 31) * 255.0 / 31.0;
    for level in 0..LEVELS {
        let at = level_index(1, 0, 0, level);
        assert_eq!(data.heights[level][at], f32::from(SEA), "level {level}");
        let red = f32::from(data.baked[level][at][0]);
        assert!((red - channel(SEA_COLOUR, 0)).abs() <= 1.0, "{red}");
        let red = f32::from(data.neutral[level][at][0]);
        assert!(
            (red - channel(SEA_COLOUR, 0) * 131.0 / 128.0).abs() <= 1.0,
            "{red}"
        );
    }
    // The hill: highest near its centre, means at coarser levels.
    let hill = data.tiles[2];
    assert!(hill.height_top < f32::from(SEA) - 1800.0, "{hill:?}");
    assert_eq!((hill.top, hill.floor), (f32::from(HILL), f32::from(SEA)));
    let centre = data.heights[0][level_index(2, 16, 16, 0)];
    let mean = data.heights[5][level_index(2, 0, 0, 5)];
    assert!(centre < mean && mean < f32::from(SEA), "{centre} {mean}");
    // Cell tops: the hill's cell and its neighbours, the sea elsewhere.
    let top = |x: i32, z: i32| data.tops[0][((z + 16) * TOPS_SIDE as i32 + x + 16) as usize];
    for (x, z) in [(130, 140), (129, 139), (131, 141)] {
        assert_eq!(top(x, z), hill.height_top, "({x}, {z})");
    }
    assert_eq!(top(132, 140), f32::from(SEA));
    // Blocks of 8 x 8 cells (grid block b at level 3 texel b + 2).
    let block = |bx: usize, bz: usize| data.tops[3][(bz + 2) * (TOPS_SIDE >> 3) + bx + 2];
    assert_eq!(block(16, 17), hill.height_top);
    assert_eq!(block(0, 0), f32::from(SEA));
    assert_eq!(
        block(15, 17),
        f32::from(SEA),
        "cells 120..127, the hill at 130"
    );
    assert_eq!(top(-16, -16), f32::from(SEA));
    assert_eq!(data.top, f32::from(HILL));
    // The neutral RAM patched every face colour, nothing else.
    let patched = neutral_ram(&ram, &baked);
    let face = MESHES[1] + 0x400 + 4 + 0x20;
    assert_eq!(get(&patched, face), 0x2c83_8383);
    let differing = ram.iter().zip(&patched).filter(|(a, b)| a != b).count();
    assert_eq!(differing, 3 * 5, "one sea face and four hill faces");
    // The vertices: positions and both colours.
    let first = baked.tiles[2].vertices.start as usize * VERTEX_BYTES;
    let v = &data.vertices[first..first + VERTEX_BYTES];
    let x = f32::from_le_bytes(v[0..4].try_into().unwrap());
    assert_eq!(x, baked.mesh.vertices[first / VERTEX_BYTES].position[0]);
    assert_eq!(
        &v[12..15],
        &baked.mesh.vertices[first / VERTEX_BYTES].color[..3]
    );
    assert_eq!(
        &v[16..19],
        &neutral.mesh.vertices[first / VERTEX_BYTES].color[..3]
    );
}

#[test]
fn levels_and_heights_follow_the_shaders_lookup() {
    let (ram, vram) = synthetic(&[(128, 128)]);
    let (_, _, data) = maps(&ram, &vram);
    // Cell (128, 128) holds world 0..2048: the hill, its apex at 1024.
    let apex = data.height_at(1024.0, 1024.0, 0).unwrap();
    assert!(apex < -2800.0, "{apex}");
    assert_eq!(data.height_at(5000.0, 5000.0, 0), Some(f64::from(SEA)));
    // Beyond the map the wrap repeats the border.
    assert_eq!(data.height_at(-1.0e6, 3.0e6, 3), Some(f64::from(SEA)));
    assert_eq!(level_index(17, 1, 2, 0), (32 + 2) * 512 + 32 + 1);
    assert_eq!(level_index(17, 1, 2, 1), (16 + 2) * 256 + 16 + 1);
    assert_eq!(level_size(5), (16, 32));
}

#[test]
fn the_projection_and_its_rays_agree() {
    let mut scene = Scene {
        eye: [0x0100_0000, -0x0040_0000, 0x0200_0000],
        h: 296,
        centre: [160, 376],
        x_scale: (3, 4),
        ..Scene::default()
    };
    // A camera yawed and pitched (rows orthonormal to 1.3.12).
    let (yaw, pitch) = (0.7f64, -0.2f64);
    let rows = [
        [yaw.cos(), 0.0, -yaw.sin()],
        [
            yaw.sin() * pitch.sin(),
            pitch.cos(),
            yaw.cos() * pitch.sin(),
        ],
        [
            yaw.sin() * pitch.cos(),
            -pitch.sin(),
            yaw.cos() * pitch.cos(),
        ],
    ];
    scene.view = rows.map(|row| row.map(|v| (v * 4096.0).round() as i16));
    let view = View::of(&scene);
    assert_eq!(view.k, 0.75);
    assert_eq!(view.eye, [4096.0, -1024.0, 8192.0]);
    for rel in [
        [100.0, 50.0, 3000.0],
        [-4000.0, 900.0, 20000.0],
        [7.0, -3.0, 40.0],
    ] {
        let rel = {
            // In front of the camera: view (x, y, z) to world.
            let mut w = [0.0; 3];
            for (r, &v) in view.rows.iter().zip(&rel) {
                for k in 0..3 {
                    w[k] += r[k] * v;
                }
            }
            w
        };
        let (s, z) = view.project(rel).unwrap();
        let d = view.ray(s);
        let length = (rel[0] * rel[0] + rel[1] * rel[1] + rel[2] * rel[2]).sqrt();
        for k in 0..3 {
            assert!((d[k] - rel[k] / length).abs() < 1e-9, "{d:?} {rel:?}");
        }
        assert!((z - view.to_view(rel)[2]).abs() < 1e-9);
    }
    assert_eq!(earth_drop(1000.0, 28_000.0), 0.0);
    let d = earth_drop(128_000.0, 28_000.0);
    assert!((d - 1.0e10 / (2.0 * EARTH_RADIUS)).abs() < 1e-6, "{d}");
}

#[test]
fn settings_need_far_terrain_and_the_atmosphere() {
    let mut effects = crate::settings::Effects::default();
    assert_eq!(effects.far_terrain_tiles, Ring(DEFAULT_TILES));
    assert_eq!(effects.far_terrain_quality, Quality::Medium);
    assert!(!Settings::from_settings(&effects).on);
    effects.far_terrain = true;
    assert!(
        !Settings::from_settings(&effects).on,
        "without the atmosphere"
    );
    effects.atmosphere = true;
    effects.far_terrain_tiles = Ring(500);
    effects.far_terrain_quality = Quality::High;
    let settings = Settings::from_settings(&effects);
    assert!(settings.on && !settings.blocking);
    assert_eq!(settings.ring.tiles(), MAX_TILES);
    assert_eq!(settings.quality, Quality::High);
    assert_eq!(Ring(3).tiles(), MIN_TILES);
    for quality in Quality::ALL {
        let (step, steps) = quality.march();
        assert!(step > 0.0 && steps >= 512, "{}", quality.name());
    }
}

// ---- the recorder -----------------------------------------------------------------------

/// Far terrain on (the default settings, as `far_terrain` and `atmosphere`
/// set).
fn on() -> Settings {
    Settings {
        on: true,
        ..Settings::default()
    }
}

/// The flight list of `packets` in `ram`, through `recorder` and `gpu` as
/// the host does; returns the frame.
fn record(
    recorder: &mut Recorder,
    gpu: &mut Gpu,
    ram: &mut [u8],
    packets: &[(u32, Vec<u32>)],
) -> Frame {
    let start = fx::walk::tests::flight_list_in(ram, packets);
    recorder.list_begin(ram, start, gpu.vram(), false);
    for (address, words) in crate::flight_host::walk_list_addressed(ram, start).unwrap() {
        for (i, &word) in words.iter().enumerate() {
            recorder.gp0(word, address + 4 * (i as u32 + 1), gpu.vram());
            gpu.write_gp0(word).unwrap();
        }
    }
    recorder.list_end(gpu.vram());
    recorder.take_frame(gpu.vram())
}

/// A flight frame: the sky slot with the draw area, a band, a backdrop
/// strip quad and the sun's corona (another palette); a terrain tile; the
/// flare and the HUD.
fn flight_packets() -> Vec<(u32, Vec<u32>)> {
    let ft4 = |clut: u32, tpage: u32| {
        vec![
            0x2e65_6565,
            10 | (20 << 16),
            (clut << 16) | 0x80c0,
            60 | (20 << 16),
            (tpage << 16) | 0x80ff,
            10 | (40 << 16),
            0x9fc0,
            60 | (40 << 16),
            0x9fff,
        ]
    };
    let mut sky = vec![0xe300_0000, 0xe403_bd3f, 0xe500_0000];
    sky.extend([0x2840_6080, 0, 320, 120 << 16, 320 | (120 << 16)]);
    sky.extend(ft4(u32::from(STRIP_CLUT), 0x39));
    sky.extend(ft4(0x4dd6, 0x39));
    vec![
        (4095, sky),
        (4094, vec![0x6000_8000, 10 | (10 << 16), 16 | (16 << 16)]),
        (1, vec![0xe500_0000]),
        (0, vec![0x6000_ffff, 230 << 16, 320 | (2 << 16)]),
    ]
}

fn effects_config(enabled: bool) -> fx::Config {
    fx::Config {
        enabled,
        ..fx::Config::default()
    }
}

fn gpu_with(vram: &[u16]) -> Gpu {
    let bytes: Vec<u8> = vram.iter().flat_map(|w| w.to_le_bytes()).collect();
    Gpu::new(Vram::from_le_bytes(&bytes).unwrap())
}

#[test]
fn backdrop_strips_are_dropped_only_while_far_terrain_draws() {
    let options = Options {
        scale: 2,
        dither: false,
    };
    let run = |ram: &[u8], vram: &[u16], far: bool| {
        let mut ram = ram.to_vec();
        let mut gpu = gpu_with(vram);
        let mut recorder = Recorder::new(options, &gpu);
        recorder.set_effects(Some(effects_config(true)));
        let mut stats = None;
        if far {
            let effect = FarTerrain::new(on().with_blocking(true));
            stats = Some(effect.stats());
            recorder.add_effect(Box::new(effect));
        }
        let frame = record(&mut recorder, &mut gpu, &mut ram, &flight_packets());
        let dropped = recorder.effects_stats().unwrap().dropped;
        let handed = frame
            .fx
            .as_ref()
            .unwrap()
            .data
            .iter()
            .any(|(name, data)| *name == NAME && data.downcast_ref::<FarFrame>().is_some());
        (dropped, handed, stats.map(|s| s.lock().unwrap().clone()))
    };
    let (ram, vram) = synthetic(&[(130, 140)]);
    // Off: nothing dropped.
    assert_eq!(run(&ram, &vram, false).0, 0);
    // On with a map: the strip quad (not the corona) dropped, the map
    // handed to the card.
    let (dropped, handed, stats) = run(&ram, &vram, true);
    assert_eq!((dropped, handed), (1, true));
    let stats = stats.unwrap();
    assert_eq!((stats.builds, stats.dropped), (1, 1));
    // On without a map (no grid): nothing dropped, nothing handed over.
    let mut broken = ram.clone();
    let world = get(&broken, ac3_flight_jp::GP + ac3_flight_jp::common::GAMEPLAY);
    put(&mut broken, world + 0x248, 0x9000_0000);
    let (dropped, handed, stats) = run(&broken, &vram, true);
    assert_eq!((dropped, handed), (0, false));
    assert_eq!(stats.unwrap().failed_builds, 1);
}

#[test]
fn the_strip_rule_matches_only_the_strip() {
    let words = flight_packets()[0].1.clone();
    let strip = &words[8..17];
    let corona = &words[17..26];
    fn primitive(words: &[u32], phase: Phase) -> fx::Primitive<'_> {
        fx::Primitive {
            phase,
            command: (words[0] >> 24) as u8,
            words,
            addresses: &[],
            offset: [0, 0],
            area: Rect::default(),
            semi: true,
            texture: None,
        }
    }
    assert!(is_backdrop_strip(&primitive(strip, Phase::Sky)));
    assert!(
        !is_backdrop_strip(&primitive(strip, Phase::World)),
        "leaves"
    );
    assert!(
        !is_backdrop_strip(&primitive(corona, Phase::Sky)),
        "the sun"
    );
    let mut subtractive = strip.to_vec();
    subtractive[4] = (0x59 << 16) | (subtractive[4] & 0xffff);
    assert!(is_backdrop_strip(&primitive(&subtractive, Phase::Sky)));
    let mut gouraud = strip.to_vec();
    gouraud[0] = 0x3e65_6565;
    assert!(
        !is_backdrop_strip(&primitive(&gouraud, Phase::Sky)),
        "clouds, dome"
    );
}

#[test]
fn effects_off_record_exactly_as_before_with_far_terrain_registered() {
    let (ram, vram) = synthetic(&[(130, 140)]);
    let options = Options {
        scale: 2,
        dither: false,
    };
    let frames = |configure: &dyn Fn(&mut Recorder)| {
        let mut ram = ram.clone();
        let mut gpu = gpu_with(&vram);
        let mut recorder = Recorder::new(options, &gpu);
        configure(&mut recorder);
        (0..3)
            .map(|_| record(&mut recorder, &mut gpu, &mut ram, &flight_packets()))
            .collect::<Vec<_>>()
    };
    let plain = frames(&|_| {});
    let registered = frames(&|recorder| {
        recorder.set_effects(Some(effects_config(false)));
        recorder.add_effect(Box::new(FarTerrain::new(on().with_blocking(true))));
    });
    for (a, b) in plain.iter().zip(&registered) {
        assert_eq!(a.ops, b.ops);
        assert_eq!(a.vertices, b.vertices);
        assert_eq!(a.data, b.data);
        assert!(b.fx.is_none() && b.fx_vertices.is_empty());
    }
    // Effects on, far terrain registered but off: what effects on records
    // without it, and nothing handed to the card.
    let on_plain = frames(&|recorder| recorder.set_effects(Some(effects_config(true))));
    let on_off = frames(&|recorder| {
        recorder.set_effects(Some(effects_config(true)));
        register_recorder(recorder, Settings::default());
    });
    for (a, b) in on_plain.iter().zip(&on_off) {
        assert_eq!(a.ops, b.ops);
        assert_eq!(a.vertices, b.vertices);
        assert_eq!(a.fx_vertices, b.fx_vertices);
        let (a, b) = (a.fx.as_ref().unwrap(), b.fx.as_ref().unwrap());
        assert_eq!(a.classes, b.classes);
        assert!(b.data.is_empty());
    }
}

#[test]
fn the_worker_map_arrives_in_the_same_frame_every_run() {
    let (ram, vram) = synthetic(&[(130, 140)]);
    let options = Options {
        scale: 2,
        dither: false,
    };
    for _ in 0..2 {
        let mut ram = ram.clone();
        let mut gpu = gpu_with(&vram);
        let mut recorder = Recorder::new(options, &gpu);
        recorder.set_effects(Some(effects_config(true)));
        register_recorder(&mut recorder, on());
        let handed: Vec<bool> = (0..6)
            .map(|_| {
                let frame = record(&mut recorder, &mut gpu, &mut ram, &flight_packets());
                !frame.fx.unwrap().data.is_empty()
            })
            .collect();
        let first = BUILD_FRAMES as usize;
        assert_eq!(handed, (0..6).map(|k| k >= first).collect::<Vec<_>>());
    }
}

// ---- the card ---------------------------------------------------------------------------

fn device() -> Option<&'static (wgpu::Device, wgpu::Queue)> {
    static DEVICE: OnceLock<Option<(wgpu::Device, wgpu::Queue)>> = OnceLock::new();
    DEVICE
        .get_or_init(|| match offscreen_device() {
            Ok(device) => Some(device),
            Err(error) => {
                assert!(
                    std::env::var_os("AC3_REQUIRE_GPU").is_none(),
                    "AC3_REQUIRE_GPU: {error}"
                );
                eprintln!("skipped: {error}");
                None
            }
        })
        .as_ref()
}

fn serial() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// A camera at `eye` (world units) yawed and pitched (radians; pitch
/// positive looks down), H 296, the projection centre `centre`.
fn scene(eye: [f64; 3], yaw: f64, pitch: f64, centre: [i32; 2], k: (u32, u32)) -> Scene {
    // View = pitch about x after yaw about y; rows map world to view.
    let rows = [
        [yaw.cos(), 0.0, -yaw.sin()],
        [
            -yaw.sin() * pitch.sin(),
            pitch.cos(),
            -yaw.cos() * pitch.sin(),
        ],
        [
            yaw.sin() * pitch.cos(),
            pitch.sin(),
            yaw.cos() * pitch.cos(),
        ],
    ];
    Scene {
        eye: eye.map(|v| (v * 4096.0) as i32),
        h: 296,
        near: 16_395,
        far: 28_000,
        view: rows.map(|row| row.map(|v| (v * 4096.0).round() as i16)),
        centre,
        sun: [3705, -144, -1739],
        sun_colour: [1761, 1679, 1597],
        x_scale: k,
        ..Scene::default()
    }
}

/// Run one far-terrain frame on the card: the z target over `area` (view
/// z, order) at N x and the N x VRAM.
fn draw(executor: &mut Executor, scene: Scene, area: Rect, frame: FarFrame) -> Vec<[f32; 2]> {
    let fx_frame = FxFrame {
        scene,
        world_area: area,
        data: vec![(NAME, Arc::new(frame) as Arc<dyn Any + Send + Sync>)],
        ..FxFrame::default()
    };
    let frame = Frame {
        ops: vec![Op::FxBegin(area), Op::FxPost(area)],
        vertices: vec![0; VERTEX_WORDS],
        fx_vertices: vec![0; fx::FX_VERTEX_WORDS],
        fx: Some(Box::new(fx_frame)),
        ..Frame::default()
    };
    executor.execute(&frame);
    executor.read_fx_snapshot().expect("a snapshot").0
}

#[derive(Debug, Default)]
struct Compared {
    samples: usize,
    /// Relative view z errors where both hit.
    errors: Vec<f64>,
    mismatched: usize,
}

impl Compared {
    fn worst(&self) -> f64 {
        self.errors.iter().copied().fold(0.0, f64::max)
    }
    /// The share of errors below `bound`.
    fn within(&self, bound: f64) -> f64 {
        self.errors.iter().filter(|&&e| e < bound).count() as f64 / self.errors.len().max(1) as f64
    }
}

/// The card's view z against [`reference_march`] marched from the eye, at
/// every `step`-th N x pixel of the area. `beyond`: only samples whose
/// reference hit lies beyond the ring (the ray march's part; the meshes'
/// exact geometry differs from the heightmap near ridges), else every
/// sample (a flat map: meshes and heightmap agree at every level).
#[allow(clippy::too_many_arguments)]
fn compare(
    data: &MapData,
    scene: &Scene,
    area: Rect,
    scale: u32,
    settings: &Settings,
    z: &[[f32; 2]],
    step: usize,
    beyond: bool,
) -> Compared {
    let view = View::of(scene);
    let mut params = Params::new(scene, data, settings);
    let ring = params.march_start + 2048.0;
    params.march_start = 0.0;
    let (w, h) = (
        (area.w as u32 * scale) as usize,
        (area.h as u32 * scale) as usize,
    );
    let mut out = Compared::default();
    let debug = std::env::var_os("AC3_FAR_DEBUG").is_some();
    if debug {
        // The horizon in the middle column: the first row with terrain.
        let i = w / 2;
        let gpu = (0..h).find(|&j| z[j * w + i][0] > 0.0);
        let cpu = (0..h).find(|&j| {
            let s = [
                f64::from(area.x) + (i as f64 + 0.5) / f64::from(scale),
                f64::from(area.y) + (j as f64 + 0.5) / f64::from(scale),
            ];
            reference_march(data, &view, &params, view.ray(s), view.h * f64::from(scale)).is_some()
        });
        eprintln!(
            "horizon, middle column: card row {gpu:?} (z {:?}), reference row {cpu:?}",
            gpu.map(|j| z[j * w + i][0])
        );
    }
    for j in (step / 2..h).step_by(step) {
        let mut line = String::new();
        for i in (step / 2..w).step_by(step) {
            if debug {
                let s = [
                    f64::from(area.x) + (i as f64 + 0.5) / f64::from(scale),
                    f64::from(area.y) + (j as f64 + 0.5) / f64::from(scale),
                ];
                let d = view.ray(s);
                let hit = reference_march(data, &view, &params, d, view.h * f64::from(scale));
                let forward =
                    d[0] * view.rows[2][0] + d[1] * view.rows[2][1] + d[2] * view.rows[2][2];
                let got = f64::from(z[j * w + i][0]);
                line.push(match hit {
                    Some(t) if got > 0.0 => {
                        let e = (got - t * forward).abs() / (t * forward);
                        if e < 1e-3 {
                            '.'
                        } else if e < 1e-2 {
                            'o'
                        } else if got > t * forward {
                            '>'
                        } else {
                            '<'
                        }
                    }
                    None if got > 0.0 => 'g',
                    Some(_) => 'c',
                    None => ' ',
                });
                if i == w / 2 && j % (step * 6) == step / 2 {
                    eprintln!("row {j}: gpu {got} cpu {:?}", hit.map(|t| t * forward));
                }
            }
            let s = [
                f64::from(area.x) + (i as f64 + 0.5) / f64::from(scale),
                f64::from(area.y) + (j as f64 + 0.5) / f64::from(scale),
            ];
            let d = view.ray(s);
            let hit = reference_march(data, &view, &params, d, view.h * f64::from(scale));
            if beyond && hit.is_none_or(|t| t * d[0].hypot(d[2]) < ring) {
                continue;
            }
            let forward = d[0] * view.rows[2][0] + d[1] * view.rows[2][1] + d[2] * view.rows[2][2];
            let [got, order] = z[j * w + i];
            assert_eq!(order, 0.0, "draw order 0");
            out.samples += 1;
            match (hit, got > 0.0) {
                (Some(t), true) => {
                    let e = t * forward;
                    out.errors.push((f64::from(got) - e).abs() / e);
                }
                (None, false) => {}
                _ => out.mismatched += 1,
            }
        }
        if debug && !line.is_empty() {
            eprintln!("|{line}|");
        }
    }
    out
}

#[test]
fn the_card_matches_the_cpu_reference() {
    let _serial = serial();
    let Some((device, queue)) = device() else {
        return;
    };
    let hills: Vec<(usize, usize)> = (0..40)
        .map(|k| (100 + (k * 7) % 60, 60 + (k * 13) % 50))
        .collect();
    // (scale, k, centre, area, hills, eye y, pitch, ring, quality)
    let top = Rect {
        x: 0,
        y: 0,
        w: 320,
        h: 240,
    };
    let bottom = Rect {
        x: 0,
        y: 256,
        w: 320,
        h: 240,
    };
    for (scale, k, centre, area, hills, eye_y, pitch, ring, quality, eye_xz, yaw) in [
        (
            2,
            (1, 1),
            [160, 120],
            top,
            &[][..],
            -5_000.0,
            0.12,
            16,
            Quality::High,
            [-40_000.0, -150_000.0],
            0.4,
        ),
        (
            3,
            (3, 4),
            [163, 377],
            bottom,
            &hills[..],
            -5_000.0,
            0.12,
            16,
            Quality::High,
            [-40_000.0, -150_000.0],
            0.4,
        ),
        // High above the sea, the horizon far away: the default ring and step.
        (
            2,
            (1, 1),
            [160, 120],
            top,
            &[][..],
            -6_672.0,
            0.02,
            44,
            Quality::Medium,
            [-40_000.0, -150_000.0],
            0.4,
        ),
        // North across the map edge (the wrap repeats its border rows).
        (
            2,
            (1, 1),
            [160, 120],
            top,
            &[][..],
            -6_672.0,
            0.02,
            44,
            Quality::Medium,
            [-90_845.0, -90_737.0],
            2.976,
        ),
    ] {
        let (ram, vram) = synthetic(hills);
        let (_, _, data) = maps(&ram, &vram);
        let data = Arc::new(data);
        let mut executor = Executor::new(
            device.clone(),
            queue.clone(),
            Options {
                scale,
                dither: false,
            },
            wgpu::TextureFormat::Rgba8Unorm,
        )
        .unwrap();
        let card = FarTerrainCard::new();
        let stats = card.stats();
        executor.add_effect(Box::new(card));
        let settings = Settings {
            on: true,
            ring: Ring(ring),
            quality,
            blocking: false,
        };
        // Over the coast of hills, 4,000 units above the sea, looking a
        // little down toward the hills.
        let scene = scene([eye_xz[0], eye_y, eye_xz[1]], yaw, pitch, centre, k);
        let frame = FarFrame {
            map: data.clone(),
            version: 7,
            changes: None,
            settings,
        };
        let z = draw(&mut executor, scene, area, frame);
        let flat = hills.is_empty();
        let compared = compare(&data, &scene, area, scale, &settings, &z, 7, !flat);
        eprintln!(
            "scale {scale}, k {k:?}, {} hills: {} samples, {} both hit, {} mismatched, worst relative z error {:.2e}, {:.1}% within 1e-3",
            hills.len(),
            compared.samples,
            compared.errors.len(),
            compared.mismatched,
            compared.worst(),
            100.0 * compared.within(1e-3)
        );
        let card = stats.lock().unwrap().clone();
        assert_eq!(card.full_uploads, 1, "{card:?}");
        assert!(card.cells > 0 && card.culled > 0, "{card:?}");
        assert!(compared.errors.len() > compared.samples / 4, "{compared:?}");
        if flat {
            // The projection: meshes and march alike, exactly.
            assert!(compared.worst() < 1e-3, "worst {}", compared.worst());
            assert!(
                compared.mismatched * 100 <= compared.samples,
                "{} of {}",
                compared.mismatched,
                compared.samples
            );
        } else {
            // Hills: the card's coarser steps may meet a slope a little
            // later or miss a thin crest.
            assert!(compared.within(1e-2) > 0.97, "{:.3}", compared.within(1e-2));
            assert!(
                compared.mismatched * 100 <= 3 * compared.samples,
                "{} of {}",
                compared.mismatched,
                compared.samples
            );
        }
    }
}

#[test]
fn uploads_are_whole_once_then_only_what_changed() {
    let _serial = serial();
    let Some((device, queue)) = device() else {
        return;
    };
    let (ram, vram) = synthetic(&[(128, 125)]);
    let (_, _, data) = maps(&ram, &vram);
    let map = Arc::new(data);
    let mut executor = Executor::new(
        device.clone(),
        queue.clone(),
        Options {
            scale: 2,
            dither: false,
        },
        wgpu::TextureFormat::Rgba8Unorm,
    )
    .unwrap();
    let card = FarTerrainCard::new();
    let stats = card.stats();
    executor.add_effect(Box::new(card));
    let area = Rect {
        x: 0,
        y: 0,
        w: 320,
        h: 240,
    };
    let scene = scene([1000.0, -4000.0, -20_000.0], 0.0, 0.15, [160, 120], (1, 1));
    let frame = |version, changes| FarFrame {
        map: map.clone(),
        version,
        changes,
        settings: Settings::default(),
    };
    let changes = |base| {
        Some(Changes {
            base,
            grid: true,
            tiles: vec![2],
        })
    };
    draw(&mut executor, scene, area, frame(10, None));
    draw(&mut executor, scene, area, frame(10, None));
    draw(&mut executor, scene, area, frame(11, changes(10)));
    draw(&mut executor, scene, area, frame(13, changes(12)));
    let stats = stats.lock().unwrap().clone();
    assert_eq!(
        (stats.full_uploads, stats.partial_uploads),
        (2, 1),
        "{stats:?}"
    );
    assert_eq!(stats.frames, 4);
}

#[test]
fn the_uniform_and_the_ring_follow_the_scene() {
    let (ram, vram) = synthetic(&[]);
    let (_, _, data) = maps(&ram, &vram);
    let scene = scene([0.0, -3000.0, 0.0], 0.0, 0.0, [160, 376], (3, 4));
    let settings = Settings::default();
    let params = Params::new(&scene, &data, &settings);
    assert_eq!(params.drop_start, 28_000.0);
    assert_eq!(params.march_start, f64::from(DEFAULT_TILES) * 2048.0);
    assert!(params.march_end > 300_000.0, "{params:?}");
    assert_eq!(params.cue, Some((16_395.0, 28_000.0)));
    let words = uniform(&scene, &params, &data, 4, [0, 256, 320, 240]);
    let f = |i: usize| f32::from_bits(words[i]);
    assert_eq!((f(3), f(7), f(11)), (296.0, 0.75, 4.0));
    assert_eq!((f(16), f(17), f(18), f(19)), (160.0, 120.0, 320.0, 240.0));
    assert_eq!((f(20), f(21)), (0.0, 1024.0));
    assert_eq!((words[44], words[45]), (1024, 8));
    // Level flight looking along +z: the ring keeps the cells ahead.
    let view = View::of(&scene);
    let (bytes, draws, culled) = ring_cells(&data, &view, &params, 20, [0, 256, 320, 240]);
    let cells = bytes.len() / 32;
    assert_eq!(draws.iter().map(|d| d.count as usize).sum::<usize>(), cells);
    assert_eq!(cells as u64 + culled, 41 * 41);
    assert!(cells > 100 && culled > 600, "{cells} kept, {culled} culled");
    // Every kept cell lies ahead (cell z >= the eye's row).
    for cell in bytes.chunks_exact(32) {
        let z = i32::from_le_bytes(cell[20..24].try_into().unwrap());
        assert!(z >= -1, "{z}");
    }
}
