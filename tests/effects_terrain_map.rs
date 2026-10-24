//! The far-terrain map (`src/effects/terrain_map.rs`) of the Japanese
//! release's mission 1, built from a flight state (VBlank 50000), and its
//! pictures: the atlas (colour, height, flags), the whole map top-down
//! (colour, relief, classes), and views of the map ray-marched from the
//! game's camera next to the game's own picture of the same frame.
//!
//! Needs the JP disc 1 image and the state: `AC3_JP_DISC` (default
//! `~/ac3-jp/discs/... (Disc 1) (Rev 1).cue`) and `AC3_TERRAIN_STATE`
//! (default `~/ac3-jp/runs/present-work/at50000.ac3state`). Pictures go to
//! `AC3_TERRAIN_OUT` (default `target/effects-terrain`).
use ac3_graphics::Vram;
use ac3_machine::Machine;
use ac3_port::assets::GameData;
use ac3_port::effects::terrain_map::{
    self, ATLAS_HEIGHT, ATLAS_WIDTH, Atlas, CELL_TEXELS, Classifier, GRID_SIZE, MapCamera, NO_KEY,
    Source, TerrainMap, TextureKey, flags,
};
use ac3_port::machine_card::{CardDevice, CardSource};
use ac3_port::machine_player::{self, Host, PadType, Player, parse_pad_schedule};
use ac3_port::savestate::{self, GameId};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Pull up (hold down) for the first 250 VBlanks after the state: the
/// aircraft levels out over the sea next to the coast instead of diving
/// into the forest.
const PULL_UP: &str = "50001-50250:down";
/// The JP mission 1 sea: tile type 309, flat at y -1024, 4-bit page (704,
/// 0) with CLUT (320, 314).
const SEA_TILE: u16 = 309;
const SEA_LEVEL: f32 = -1024.0;
const SEA_KEY: TextureKey = TextureKey {
    tpage: 0x2b,
    clut: 0x4e94,
};

const fn key(tpage: u16, clut: u16) -> TextureKey {
    TextureKey { tpage, clut }
}

/// The actual forest keys of JP mission 1, labelled by eye on the key
/// sheet (`tools/effects/terrain_keys_sheet.py`): canopy textures.
const FOREST_KEYS: [TextureKey; 4] = [
    key(0x2c, 0x4ed4),
    key(0x2d, 0x4f54),
    key(0x2e, 0x4e14),
    key(0x28, 0x4954),
];
/// Mixed keys: trees with a road, a canal or buildings in one texture (a
/// park, a canal bank, a highway through forest, forest edges).
const MIXED_KEYS: [TextureKey; 5] = [
    key(0x29, 0x4894),
    key(0x2d, 0x4854),
    key(0x2a, 0x4894),
    key(0x2c, 0x4f14),
    key(0x2d, 0x4f14),
];
// Every other land key of mission 1 is built: city blocks, roads, rails,
// pavement, roofs and walls.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Label {
    Water,
    Forest,
    Built,
    Mixed,
}

fn label(key: TextureKey) -> Label {
    if key == SEA_KEY {
        Label::Water
    } else if FOREST_KEYS.contains(&key) {
        Label::Forest
    } else if MIXED_KEYS.contains(&key) {
        Label::Mixed
    } else {
        Label::Built
    }
}

/// The mission 1 key table for every key of `map`: the pure keys by their
/// label; the mixed keys keep the colour rules.
fn mission_1_classifier(map: &TerrainMap) -> Classifier {
    let mut classifier = Classifier::default();
    for stat in &map.keys {
        let flag = match label(stat.key) {
            Label::Water => flags::WATER,
            Label::Forest => flags::FOREST,
            Label::Built => flags::BUILT,
            Label::Mixed => continue,
        };
        classifier.keys.push((stat.key, flag));
    }
    classifier
}

/// Map area (tiles) by actual label (water, forest, built, mixed) and flag
/// (water, forest, built, none), over the covered texels.
struct Evaluation {
    table: [[f64; 4]; 4],
}

impl Evaluation {
    /// The share of `label`'s area flagged `column`.
    fn share(&self, label: Label, column: usize) -> f64 {
        let row = self.table[label as usize];
        let total: f64 = row.iter().sum();
        if total == 0.0 {
            0.0
        } else {
            row[column] / total
        }
    }
}

fn evaluate(map: &TerrainMap) -> Evaluation {
    let mut table = [[0.0; 4]; 4];
    let texel_area = 1.0 / (CELL_TEXELS * CELL_TEXELS) as f64;
    for (t, tile) in map.tiles.iter().enumerate().skip(1) {
        let area = texel_area * f64::from(tile.cells);
        for j in 0..CELL_TEXELS {
            for i in 0..CELL_TEXELS {
                let at = Atlas::index(t as u16, i, j);
                let k = map.atlas.keys[at];
                if k == NO_KEY {
                    continue;
                }
                let f = map.atlas.flags[at];
                let column = if f & flags::WATER != 0 {
                    0
                } else if f & flags::FOREST != 0 {
                    1
                } else if f & flags::BUILT != 0 {
                    2
                } else {
                    3
                };
                let actual = label(map.keys[usize::from(k)].key);
                table[actual as usize][column] += area;
            }
        }
    }
    Evaluation { table }
}

fn print_evaluation(name: &str, evaluation: &Evaluation) {
    println!("{name}: area in tiles by actual key label -> flagged water / forest / built / none");
    for label in [Label::Water, Label::Forest, Label::Built, Label::Mixed] {
        let row = evaluation.table[label as usize];
        let total: f64 = row.iter().sum();
        println!(
            "  {label:?}: {total:.0} -> {:.0} / {:.0} / {:.0} / {:.0} ({:.1}% / {:.1}% / {:.1}% / {:.1}%)",
            row[0],
            row[1],
            row[2],
            row[3],
            100.0 * row[0] / total.max(1e-9),
            100.0 * row[1] / total.max(1e-9),
            100.0 * row[2] / total.max(1e-9),
            100.0 * row[3] / total.max(1e-9)
        );
    }
}

/// Percentiles (area-weighted) of the texel luma, blue/green ratio and
/// sample luma deviation per actual label: the data the default
/// [`Classifier`] thresholds come from.
fn print_distributions(map: &TerrainMap) {
    let mut by: Vec<Vec<([f32; 3], f64)>> = vec![Vec::new(); 4];
    for (t, tile) in map.tiles.iter().enumerate().skip(1) {
        for j in 0..CELL_TEXELS {
            for i in 0..CELL_TEXELS {
                let at = Atlas::index(t as u16, i, j);
                let k = map.atlas.keys[at];
                if k == NO_KEY || tile.cells == 0 {
                    continue;
                }
                let [r, g, b, _] = map.atlas.colors[at].map(f32::from);
                let y = 0.299 * r + 0.587 * g + 0.114 * b;
                let stats = [y, b / g.max(1.0), map.atlas.luma_std[at]];
                by[label(map.keys[usize::from(k)].key) as usize]
                    .push((stats, f64::from(tile.cells)));
            }
        }
    }
    let labels = [Label::Water, Label::Forest, Label::Built, Label::Mixed];
    for (label, mut texels) in labels.into_iter().zip(by) {
        let total: f64 = texels.iter().map(|e| e.1).sum();
        for (name, pick) in [("luma", 0), ("blue/green", 1), ("deviation", 2)] {
            texels.sort_by(|a, b| a.0[pick].total_cmp(&b.0[pick]));
            let mut acc = 0.0;
            let mut out = Vec::new();
            let mut quantiles = [0.02, 0.1, 0.25, 0.5, 0.75, 0.9, 0.98]
                .into_iter()
                .peekable();
            for (stats, area) in &texels {
                acc += area;
                while let Some(q) = quantiles.next_if(|&q| acc >= q * total) {
                    out.push(format!("{q}:{:.2}", stats[pick]));
                }
            }
            println!("  {label:?} {name}: {}", out.join(" "));
        }
    }
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

fn state_path() -> PathBuf {
    std::env::var_os("AC3_TERRAIN_STATE")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join("ac3-jp/runs/present-work/at50000.ac3state"))
}

fn disc_path() -> PathBuf {
    std::env::var_os("AC3_JP_DISC")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            home().join(
                "ac3-jp/discs/Ace Combat 3 - Electrosphere (Japan, Asia) (Disc 1) (Rev 1).cue",
            )
        })
}

fn out_dir() -> PathBuf {
    let out = std::env::var_os("AC3_TERRAIN_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/effects-terrain"));
    fs::create_dir_all(&out).unwrap();
    out
}

/// The JP player as `machine_player::run` builds it, with the state loaded.
fn load_flight(pad: &str) -> Player {
    let disc = disc_path();
    let data = GameData::load_jp_disc(&disc).unwrap();
    let game = machine_player::jp();
    let (images, booted) = machine_player::disc_images(game, &disc, None).unwrap();
    let files = images
        .iter()
        .map(|p| {
            File::open(ac3_port::version::image_path(p).unwrap_or_else(|_| p.clone())).unwrap()
        })
        .collect();
    let mut host = Host::with_discs(game, Vram::new(), files, booted);
    host.card = CardDevice::from_sources(&[CardSource::Blank, CardSource::Blank])
        .unwrap()
        .with_clock(game.layout().vblank_counter);
    host.pad = PadType::Digital.ports();
    let mut player = Player::new(Machine::default(), host);
    let schedule = parse_pad_schedule(pad).unwrap();
    player.host.borrow_mut().pad_input = Some(Box::new(move |vblank| {
        schedule
            .iter()
            .filter(|(first, last, _)| (*first..=*last).contains(&vblank))
            .fold(0, |held, (_, _, b)| held | b)
    }));
    game.boot(&mut player.machine, &data.executable.payload);
    player.machine.take_fault().unwrap();
    let id = GameId::of(&data.executable.payload);
    savestate::load(&mut player, id, &state_path()).unwrap();
    player
}

/// RAM and VRAM of the player.
fn snapshot(player: &Player) -> (Vec<u8>, Vec<u16>) {
    let vram = player.host.borrow().gpu.gpu.vram().words().to_vec();
    (player.machine.ram().to_vec(), vram)
}

fn build(ram: &[u8], vram: &[u16]) -> TerrainMap {
    TerrainMap::build(&Source { ram, vram }).unwrap()
}

// ---- pictures ------------------------------------------------------------------------

fn write_png(path: &Path, width: usize, height: usize, rgb: &[[u8; 3]]) {
    assert_eq!(rgb.len(), width * height);
    let writer = std::io::BufWriter::new(File::create(path).unwrap());
    let mut encoder = png::Encoder::new(writer, width as u32, height as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let bytes: Vec<u8> = rgb.iter().flatten().copied().collect();
    encoder
        .write_header()
        .unwrap()
        .write_image_data(&bytes)
        .unwrap();
}

fn class_color(flag: u8) -> [u8; 3] {
    if flag & flags::WATER != 0 {
        [40, 90, 210]
    } else if flag & flags::FOREST != 0 {
        [30, 150, 40]
    } else if flag & flags::BUILT != 0 {
        [230, 60, 50]
    } else if flag & flags::COVERED != 0 {
        [150, 150, 140]
    } else {
        [0, 0, 0]
    }
}

/// Height as grey: the map's highest point white, the sea level dark.
fn height_grey(y: f32, top: f32, bottom: f32) -> u8 {
    (255.0 * (bottom - y) / (bottom - top).max(1.0)).clamp(0.0, 255.0) as u8
}

fn atlas_pictures(map: &TerrainMap, out: &Path) {
    let (top, bottom) = height_range(map);
    let colors: Vec<[u8; 3]> = map
        .atlas
        .colors
        .iter()
        .map(|c| [c[0], c[1], c[2]])
        .collect();
    write_png(
        &out.join("atlas-color.png"),
        ATLAS_WIDTH,
        ATLAS_HEIGHT,
        &colors,
    );
    let heights: Vec<[u8; 3]> = map
        .atlas
        .heights
        .iter()
        .zip(&map.atlas.flags)
        .map(|(&h, &f)| {
            if f & flags::COVERED == 0 {
                [0, 0, 60]
            } else {
                [height_grey(h, top, bottom); 3]
            }
        })
        .collect();
    write_png(
        &out.join("atlas-height.png"),
        ATLAS_WIDTH,
        ATLAS_HEIGHT,
        &heights,
    );
    let classes: Vec<[u8; 3]> = map.atlas.flags.iter().map(|&f| class_color(f)).collect();
    write_png(
        &out.join("atlas-flags.png"),
        ATLAS_WIDTH,
        ATLAS_HEIGHT,
        &classes,
    );
}

/// Highest (smallest y) and lowest covered heights in use on the grid.
fn height_range(map: &TerrainMap) -> (f32, f32) {
    let mut top = f32::INFINITY;
    let mut bottom = f32::NEG_INFINITY;
    for (t, tile) in map.tiles.iter().enumerate() {
        if t == 0 || tile.cells == 0 {
            continue;
        }
        top = top.min(f32::from(tile.top));
        bottom = bottom.max(f32::from(tile.floor));
    }
    (top, bottom.min(SEA_LEVEL + 200.0))
}

/// A square of the grid in a picture: `cells` cells from cell `(x0, z0)`,
/// `scale` pixels per cell (x right, z down).
#[derive(Clone, Copy)]
struct Region {
    x0: i32,
    z0: i32,
    cells: usize,
    scale: usize,
}

impl Region {
    /// The whole grid, 8 pixels per cell.
    const WHOLE: Self = Self {
        x0: 0,
        z0: 0,
        cells: GRID_SIZE,
        scale: 8,
    };

    /// `cells` cells around the camera at the atlas resolution.
    fn around(camera: &MapCamera, cells: usize) -> Self {
        let [cx, cz] = camera.cell();
        let half = cells as i32 / 2;
        Self {
            x0: cx - half,
            z0: cz - half,
            cells,
            scale: CELL_TEXELS,
        }
    }

    fn side(&self) -> usize {
        self.cells * self.scale
    }

    /// World `(x, z)` to a pixel.
    fn pixel(&self, x: f64, z: f64) -> (f64, f64) {
        let cell = f64::from(terrain_map::TILE_UNITS);
        let scale = self.scale as f64;
        (
            (x / cell + 128.0 - f64::from(self.x0)) * scale,
            (z / cell + 128.0 - f64::from(self.z0)) * scale,
        )
    }
}

/// The map top-down over `region`, each pixel the mean (or the majority
/// class) of its atlas texels; outside the grid through the edge wrap.
fn map_picture(map: &TerrainMap, region: Region, kind: &str) -> Vec<[u8; 3]> {
    let side = region.side();
    let scale = region.scale;
    let per = CELL_TEXELS / scale;
    let (top, bottom) = height_range(map);
    let mut picture = vec![[0u8; 3]; side * side];
    for (py, row) in picture.chunks_mut(side).enumerate() {
        for (px, pixel) in row.iter_mut().enumerate() {
            let tile = map.tile_at(
                region.x0 + (px / scale) as i32,
                region.z0 + (py / scale) as i32,
            );
            if tile == 0 {
                continue;
            }
            let (i0, j0) = ((px % scale) * per, (py % scale) * per);
            let mut sum = [0u32; 3];
            let mut height = 0.0;
            let mut votes = [0u32; 4];
            for j in j0..j0 + per {
                for i in i0..i0 + per {
                    let at = Atlas::index(tile, i, j);
                    let c = map.atlas.colors[at];
                    for (s, v) in sum.iter_mut().zip(c) {
                        *s += u32::from(v);
                    }
                    height += map.atlas.heights[at];
                    let f = map.atlas.flags[at];
                    let class = if f & flags::WATER != 0 {
                        0
                    } else if f & flags::FOREST != 0 {
                        1
                    } else if f & flags::BUILT != 0 {
                        2
                    } else {
                        3
                    };
                    votes[class] += 1;
                }
            }
            let n = (per * per) as u32;
            *pixel = match kind {
                "color" => sum.map(|s| (s / n) as u8),
                "height" => [height_grey(height / n as f32, top, bottom); 3],
                _ => {
                    let best = (0..4).max_by_key(|&k| votes[k]).unwrap_or(3);
                    class_color([flags::WATER, flags::FOREST, flags::BUILT, flags::COVERED][best])
                }
            };
        }
    }
    picture
}

/// Hill shading of the heights over `region` (light from the upper left),
/// sea level blue.
fn relief(map: &TerrainMap, region: Region) -> Vec<[u8; 3]> {
    let side = region.side();
    let step = f64::from(terrain_map::TILE_UNITS) / region.scale as f64;
    let cell = f64::from(terrain_map::TILE_UNITS);
    let height = |px: usize, py: usize| {
        let x = (f64::from(region.x0) - 128.0) * cell + (px as f64 + 0.5) * step;
        let z = (f64::from(region.z0) - 128.0) * cell + (py as f64 + 0.5) * step;
        f64::from(map.height_at(x, z).unwrap_or(SEA_LEVEL))
    };
    let mut picture = vec![[0u8; 3]; side * side];
    for py in 1..side - 1 {
        for px in 1..side - 1 {
            let dx = height(px + 1, py) - height(px - 1, py);
            let dz = height(px, py + 1) - height(px, py - 1);
            // y down: ground rising towards the light (-x, -z) is lit.
            let light = (0.6 + (dx + dz) / (4.0 * step)).clamp(0.05, 1.0);
            let h = height(px, py);
            let base = if (h - f64::from(SEA_LEVEL)).abs() < 1.0 {
                [60.0, 90.0, 160.0]
            } else {
                [200.0, 190.0, 170.0]
            };
            picture[py * side + px] = base.map(|c| (c * light) as u8);
        }
    }
    picture
}

/// Draw the camera: a dot and its view direction on the ground.
fn mark_camera(picture: &mut [[u8; 3]], region: Region, camera: &MapCamera, color: [u8; 3]) {
    let side = region.side();
    let forward = camera.rotation[2];
    let (cx, cz) = region.pixel(camera.position[0], camera.position[2]);
    let length = forward[0].hypot(forward[2]).max(1e-3);
    let mut put = |x: f64, y: f64| {
        if x >= 0.0 && y >= 0.0 && (x as usize) < side && (y as usize) < side {
            picture[y as usize * side + x as usize] = color;
        }
    };
    let reach = (side / 16).max(40);
    for k in 0..reach {
        let t = k as f64;
        put(
            cx + t * f64::from(forward[0] / length),
            cz + t * f64::from(forward[2] / length),
        );
    }
    for dy in -3..=3 {
        for dx in -3..=3 {
            put(cx + f64::from(dx), cz + f64::from(dy));
        }
    }
}

/// The map seen from `camera` (320 x 240 about the screen centre, the
/// game's projection), ray-marched through the heightmap: what the
/// renderer's far-terrain pass draws, here on the CPU.
fn ray_march(map: &TerrainMap, camera: &MapCamera, width: usize, height: usize) -> Vec<[u8; 3]> {
    let r = camera.rotation;
    let h = f64::from(camera.h);
    let c = camera.position;
    let ground = |x: f64, z: f64| f64::from(map.height_at(x, z).unwrap_or(SEA_LEVEL));
    let mut picture = vec![[0u8; 3]; width * height];
    for py in 0..height {
        for px in 0..width {
            let view = [
                (px as f64 + 0.5 - width as f64 / 2.0) / h,
                (py as f64 + 0.5 - height as f64 / 2.0) / h,
                1.0,
            ];
            // World direction: R^T view.
            let mut d = [0.0f64; 3];
            for (row, v) in r.iter().zip(view) {
                for k in 0..3 {
                    d[k] += f64::from(row[k]) * v;
                }
            }
            let norm = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            let d = d.map(|v| v / norm);
            let at = |t: f64| [c[0] + t * d[0], c[1] + t * d[1], c[2] + t * d[2]];
            let mut t = 50.0;
            let mut previous = 0.0;
            let mut hit = None;
            while t < 400_000.0 {
                let p = at(t);
                if p[1] >= ground(p[0], p[2]) {
                    let (mut lo, mut hi) = (previous, t);
                    for _ in 0..10 {
                        let mid = 0.5 * (lo + hi);
                        let q = at(mid);
                        if q[1] >= ground(q[0], q[2]) {
                            hi = mid;
                        } else {
                            lo = mid;
                        }
                    }
                    hit = Some(hi);
                    break;
                }
                previous = t;
                t += (t * 0.004).max(16.0);
            }
            picture[py * width + px] = match hit {
                Some(t) => {
                    let p = at(t);
                    let color = map
                        .texel_index(p[0], p[2])
                        .map(|i| map.atlas.colors[i])
                        .unwrap_or([0, 0, 0, 0]);
                    // Slope shading, so relief reads.
                    let e = 32.0;
                    let dx = ground(p[0] + e, p[2]) - ground(p[0] - e, p[2]);
                    let dz = ground(p[0], p[2] + e) - ground(p[0], p[2] - e);
                    let shade = (1.0 + (dx - dz) / (6.0 * e)).clamp(0.5, 1.4);
                    [0, 1, 2].map(|k| (f64::from(color[k]) * shade).min(255.0) as u8)
                }
                None => [150, 165, 180],
            };
        }
    }
    picture
}

/// The game's current display buffer (320 x 240 at the GP1 start).
fn game_picture(player: &Player) -> Vec<[u8; 3]> {
    let host = player.host.borrow();
    let display = host.gpu.gpu.display();
    let vram = host.gpu.gpu.vram().words();
    let (x0, y0) = (usize::from(display.start_x), usize::from(display.start_y));
    let mut picture = Vec::with_capacity(320 * 240);
    for y in 0..240 {
        for x in 0..320 {
            let w = vram[((y0 + y) & 511) * 1024 + ((x0 + x) & 1023)];
            let c = |s: u16| (((w >> s) & 31) * 255 / 31) as u8;
            picture.push([c(0), c(5), c(10)]);
        }
    }
    picture
}

fn side_by_side(left: &[[u8; 3]], right: &[[u8; 3]], w: usize, h: usize) -> Vec<[u8; 3]> {
    let mut picture = Vec::with_capacity(2 * w * h);
    for y in 0..h {
        picture.extend_from_slice(&left[y * w..(y + 1) * w]);
        picture.extend_from_slice(&right[y * w..(y + 1) * w]);
    }
    picture
}

// ---- tests ---------------------------------------------------------------------------

/// Mission 1 from the state: layouts, sea, classification, timing and the
/// pictures.
#[test]
#[ignore = "requires the JP disc and the mission 1 flight state"]
fn mission_1_map_from_the_flight_state() {
    let player = load_flight("");
    assert_eq!(player.vblanks(), 50001);
    let (ram, vram) = snapshot(&player);
    let mut times = Vec::new();
    let mut map = None;
    for _ in 0..3 {
        let started = Instant::now();
        map = Some(build(&ram, &vram));
        times.push(started.elapsed());
    }
    let map = map.unwrap();
    let camera = MapCamera::read(&ram).unwrap();
    map_pictures(&map, &camera);
    // The keys by area, for tools/effects/terrain_keys_sheet.py; with
    // AC3_TERRAIN_DUMP, the RAM and VRAM it reads.
    let out = out_dir();
    let keys: String = map
        .keys
        .iter()
        .map(|k| format!("{:04x} {:04x} {:.1}\n", k.key.tpage, k.key.clut, k.area))
        .collect();
    fs::write(out.join("keys.txt"), keys).unwrap();
    if std::env::var_os("AC3_TERRAIN_DUMP").is_some() {
        fs::write(out.join("at50000.ram"), &ram).unwrap();
        let bytes: Vec<u8> = vram.iter().flat_map(|w| w.to_le_bytes()).collect();
        fs::write(out.join("at50000.vram"), bytes).unwrap();
    }
    let best = times.iter().min().unwrap();
    println!(
        "build: {} tile types, {} faces, {} triangles; {:.1} ms (best of 3: {:?}), of which shading {:.1} ms",
        map.stats.tiles,
        map.stats.faces,
        map.stats.triangles,
        best.as_secs_f64() * 1e3,
        times
            .iter()
            .map(|t| format!("{:.1}", t.as_secs_f64() * 1e3))
            .collect::<Vec<_>>(),
        map.stats.shading.as_secs_f64() * 1e3
    );
    let started = Instant::now();
    let mut reclassified = map.clone();
    let cloned = started.elapsed();
    let started = Instant::now();
    reclassified.classify(Classifier::default());
    println!(
        "classify alone: {:.1} ms (clone {:.1} ms)",
        started.elapsed().as_secs_f64() * 1e3,
        cloned.as_secs_f64() * 1e3
    );

    // Layouts: 432 tile types, every grid cell names one of them.
    assert_eq!(map.tiles.len(), 433);
    assert!(map.grid.iter().all(|&t| (1..=432).contains(&t)));
    assert_eq!(map.tiles.iter().map(|t| t.cells).sum::<u32>(), 65536);
    let used = map.tiles.iter().filter(|t| t.cells > 0).count();
    println!("tile types on the grid: {used}");
    let on_grid = || map.tiles.iter().skip(1).filter(|t| t.cells > 0);
    println!(
        "heights on the grid: highest y {}, lowest y {}",
        on_grid().map(|t| t.top).min().unwrap(),
        on_grid().map(|t| t.floor).max().unwrap()
    );
    // Every mesh header: floor >= top (y down); the vertex ranges follow
    // each other through the whole mesh, three vertices per triangle.
    let mut end = 0;
    for (t, tile) in map.tiles.iter().enumerate().skip(1) {
        assert!(
            tile.floor >= tile.top,
            "tile {t}: floor {} top {}",
            tile.floor,
            tile.top
        );
        assert!(!tile.vertices.is_empty(), "tile {t} has no triangles");
        assert_eq!(tile.vertices.start, end, "tile {t}");
        assert_eq!(tile.vertices.len() % 3, 0, "tile {t}");
        end = tile.vertices.end;
    }
    assert_eq!(end as usize, map.mesh.vertices.len());
    // Atlas heights stay within each tile's own floor and top.
    for (t, tile) in map.tiles.iter().enumerate().skip(1) {
        for j in 0..CELL_TEXELS {
            for i in 0..CELL_TEXELS {
                let at = Atlas::index(t as u16, i, j);
                if map.atlas.flags[at] & flags::COVERED != 0 {
                    let h = map.atlas.heights[at];
                    assert!(
                        h >= f32::from(tile.top) - 0.5 && h <= f32::from(tile.floor) + 0.5,
                        "tile {t} texel ({i}, {j}): {h}"
                    );
                }
            }
        }
    }

    // The sea: detected by the heuristic, and the same as the actual key.
    let sea = map.sea.clone().expect("a sea");
    println!(
        "sea: tile {} level {} cells {} keys {:?}",
        sea.tile, sea.level, sea.cells, sea.keys
    );
    assert_eq!(sea.tile, SEA_TILE);
    assert_eq!(sea.level, SEA_LEVEL);
    assert_eq!(sea.keys, vec![SEA_KEY]);
    let water_tile = &map.tiles[usize::from(SEA_TILE)];
    println!(
        "sea tile: mean colour {:?}, luma std {:.1}, coverage {:.3}",
        water_tile.mean_color, water_tile.luma_std, water_tile.coverage
    );
    // "Bluish" in the strict sense (blue the largest channel) fails here:
    // the mission 1 sea is a dark grey-teal, green highest.
    let [r, g, b, _] = water_tile.mean_color;
    println!(
        "sea colour: blue largest {}, blue >= red {}",
        b >= r && b >= g,
        b >= r
    );

    println!("texel statistics by actual key label (area percentiles):");
    print_distributions(&map);
    // The colour heuristics against the actual keys of mission 1.
    let heuristic = evaluate(&map);
    print_evaluation("colour heuristics", &heuristic);
    let water_area: f64 = heuristic.table[0].iter().sum();
    println!(
        "water: {water_area:.0} tiles ({} as the sea tile, {:.0} on shore tiles)",
        sea.cells,
        water_area - f64::from(sea.cells)
    );
    assert!(water_area >= f64::from(sea.cells));
    // Every sea-key texel at sea level is water, nothing else is.
    // (A few texels straddle the shore: the sea's key in the majority
    // but a mean height off the sea level.)
    assert!(heuristic.share(Label::Water, 0) > 0.9999);
    for label in [Label::Forest, Label::Built, Label::Mixed] {
        assert_eq!(
            heuristic.table[label as usize][0], 0.0,
            "{label:?} as water"
        );
    }
    // Forest and built by colour: most of the area right.
    assert!(heuristic.share(Label::Forest, 1) > 0.98);
    assert!(heuristic.share(Label::Built, 2) > 0.85);
    // The key table of mission 1: exact for the pure keys, the colour
    // rules for the mixed ones.
    let mut table = map.clone();
    table.classify(mission_1_classifier(&map));
    let keyed = evaluate(&table);
    print_evaluation("mission 1 key table", &keyed);
    assert_eq!(keyed.share(Label::Forest, 1), 1.0);
    assert_eq!(keyed.share(Label::Built, 2), 1.0);
    let whole = Region::WHOLE;
    let mut classes = map_picture(&table, whole, "class");
    mark_camera(&mut classes, whole, &camera, [255, 255, 0]);
    write_png(
        &out.join("map-class-keys.png"),
        whole.side(),
        whole.side(),
        &classes,
    );
    println!("texture keys by area (tiles), mean colour, flagged water/forest/built:");
    for stat in map.keys.iter().take(24) {
        println!(
            "  tpage {:04x} clut {:04x} ({:>3},{:>3}): {:>6} faces {:>3} tiles area {:>8.1}  rgb {:?}  w {:.1} f {:.1} b {:.1}",
            stat.key.tpage,
            stat.key.clut,
            (stat.key.clut & 63) * 16,
            stat.key.clut >> 6,
            stat.faces,
            stat.tiles,
            stat.area,
            stat.mean_color,
            stat.water,
            stat.forest,
            stat.built
        );
    }
    // The sea's key is one of the two largest (forest is the other), and
    // nothing else at sea level is flagged water.
    assert!(map.keys[..2].iter().any(|k| k.key == SEA_KEY));
    for stat in &map.keys {
        if stat.key != SEA_KEY {
            assert_eq!(stat.water, 0.0, "{:?}", stat.key);
        }
    }

    // The camera: above the ground of its cell.
    let [cx, cz] = camera.cell();
    let ground = map
        .height_at(camera.position[0], camera.position[2])
        .unwrap();
    println!(
        "camera {:?} cell ({cx}, {cz}) tile {} ground y {ground:.0}, H {}",
        camera.position,
        map.tile_at(cx, cz),
        camera.h
    );
    assert_eq!([cx, cz], [91, 85]);
    assert!(
        (camera.position[1] as f32) < ground,
        "the camera is above the ground"
    );
    // Instances around the camera: the +-44 square of their ring.
    let ring = map.cells_around(cx, cz, 44);
    assert_eq!(ring.len(), 89 * 89);

    // Upload sizes.
    println!(
        "upload: grid {} B, atlas heights {} B, colours {} B, flags {} B, mesh {} B",
        map.grid.len() * 2,
        map.atlas.height_bytes().len(),
        map.atlas.color_bytes().len(),
        map.atlas.flags.len(),
        map.mesh.to_bytes().len()
    );
}

/// The atlas and map pictures of `map`, the camera marked.
fn map_pictures(map: &TerrainMap, camera: &MapCamera) {
    let out = out_dir();
    atlas_pictures(map, &out);
    let whole = Region::WHOLE;
    let side = whole.side();
    let mut color = map_picture(map, whole, "color");
    mark_camera(&mut color, whole, camera, [255, 255, 0]);
    write_png(&out.join("map-color.png"), side, side, &color);
    let height = map_picture(map, whole, "height");
    write_png(&out.join("map-height.png"), side, side, &height);
    let mut classes = map_picture(map, whole, "class");
    mark_camera(&mut classes, whole, camera, [255, 255, 0]);
    write_png(&out.join("map-class.png"), side, side, &classes);
    let coarse = Region { scale: 4, ..whole };
    let relief = relief(map, coarse);
    write_png(
        &out.join("map-relief.png"),
        coarse.side(),
        coarse.side(),
        &relief,
    );
    // The atlas resolution around the camera: 48 x 48 cells.
    let near = Region::around(camera, 48);
    let mut crop = map_picture(map, near, "color");
    mark_camera(&mut crop, near, camera, [255, 255, 0]);
    write_png(
        &out.join("map-camera-crop.png"),
        near.side(),
        near.side(),
        &crop,
    );
    println!("pictures in {}", out.display());
}

/// The map from the camera of the running flight, next to the game's
/// picture; [`TerrainMap::update`] every pass (grid and textures).
#[test]
#[ignore = "requires the JP disc and the mission 1 flight state"]
fn map_views_match_the_flight_pictures() {
    let mut player = load_flight(PULL_UP);
    let (ram, vram) = snapshot(&player);
    let mut map = build(&ram, &vram);
    let out = out_dir();
    let whole = Region::WHOLE;
    let side = whole.side();
    let mut track = map_picture(&map, whole, "color");
    let mut updates: Vec<Duration> = Vec::new();
    let mut changed_cells = 0;
    let mut changed_tiles = 0;
    let mut sheet = Vec::new();
    let mut next = 50_100;
    while player.vblanks() < 51_300 {
        player.iteration().unwrap();
        let (ram, vram) = snapshot(&player);
        let started = Instant::now();
        let update = map
            .update(&Source {
                ram: &ram,
                vram: &vram,
            })
            .unwrap();
        updates.push(started.elapsed());
        assert!(!update.rebuilt, "VBlank {}: same map", player.vblanks());
        changed_cells += update.cells.len();
        changed_tiles += update.tiles.len();
        if player.vblanks() >= next {
            next += 100;
            let camera = MapCamera::read(&ram).unwrap();
            mark_camera(&mut track, whole, &camera, [255, 255, 0]);
            let view = ray_march(&map, &camera, 320, 240);
            let game = game_picture(&player);
            let pair = side_by_side(&game, &view, 320, 240);
            let name = format!("view-{:06}.png", player.vblanks());
            write_png(&out.join(&name), 640, 240, &pair);
            sheet.push(pair);
            println!(
                "{name}: camera ({:.0}, {:.0}, {:.0}) cell {:?} forward {:?}",
                camera.position[0],
                camera.position[1],
                camera.position[2],
                camera.cell(),
                camera.rotation[2]
            );
        }
    }
    // A contact sheet of the pairs, two columns.
    let rows = sheet.len().div_ceil(2);
    let mut contact = vec![[0u8; 3]; 1280 * 240 * rows];
    for (k, pair) in sheet.iter().enumerate() {
        let (col, row) = (k % 2, k / 2);
        for y in 0..240 {
            let at = (row * 240 + y) * 1280 + col * 640;
            contact[at..at + 640].copy_from_slice(&pair[y * 640..(y + 1) * 640]);
        }
    }
    write_png(&out.join("views.png"), 1280, 240 * rows, &contact);
    write_png(&out.join("map-track.png"), side, side, &track);
    updates.sort();
    let mean = updates.iter().sum::<Duration>() / updates.len() as u32;
    println!(
        "update over {} passes: median {:.3} ms, mean {:.3} ms, max {:.3} ms; {changed_cells} grid cells and {changed_tiles} tile types changed",
        updates.len(),
        updates[updates.len() / 2].as_secs_f64() * 1e3,
        mean.as_secs_f64() * 1e3,
        updates.last().unwrap().as_secs_f64() * 1e3
    );
}

/// Change detection on edited copies of the state: a grid rewrite as
/// 8005E460 makes it, a CLUT change, and another directory.
#[test]
#[ignore = "requires the JP disc and the mission 1 flight state"]
fn rewrites_are_detected() {
    let player = load_flight("");
    let (mut ram, mut vram) = snapshot(&player);
    let mut map = build(&ram, &vram);
    let reference = map.clone();
    let unchanged = map
        .update(&Source {
            ram: &ram,
            vram: &vram,
        })
        .unwrap();
    assert!(unchanged.is_empty(), "{unchanged:?}");

    // 8005E460(column 10, row 20, 41): the low 9 bits replaced, the high
    // bits kept. The flags alone changing is no tile change.
    let word = |ram: &[u8], a: u32| {
        let o = (a & 0x1f_ffff) as usize;
        u32::from_le_bytes(ram[o..o + 4].try_into().unwrap())
    };
    let world = word(&ram, ac3_flight_jp::GP + ac3_flight_jp::common::GAMEPLAY);
    let grid = word(&ram, world + 0x248);
    let cell = |x: u32, z: u32| ((grid + 2 * (z * 256 + x)) & 0x1f_ffff) as usize;
    let at = cell(10, 20);
    let old = u16::from_le_bytes([ram[at], ram[at + 1]]);
    let new = (old & 0xfe00) | 41;
    assert_ne!(old & 0x1ff, 41);
    ram[at..at + 2].copy_from_slice(&new.to_le_bytes());
    let at = cell(11, 20);
    let flagged = u16::from_le_bytes([ram[at], ram[at + 1]]) ^ 0x0200;
    ram[at..at + 2].copy_from_slice(&flagged.to_le_bytes());
    let started = Instant::now();
    let update = map
        .update(&Source {
            ram: &ram,
            vram: &vram,
        })
        .unwrap();
    println!("grid rewrite: {update:?} in {:?}", started.elapsed());
    assert_eq!(update.cells, vec![[10, 20]]);
    assert!(!update.rebuilt && update.tiles.is_empty());
    assert_eq!(map.tile_at(10, 20), 41);
    assert_eq!(map.tiles[41].cells, reference.tiles[41].cells + 1);

    // The sea's CLUT changed (in its unfogged source, 128 rows below the
    // fog block): the tile types using it rebuild.
    let clut_row =
        (usize::from(SEA_KEY.clut >> 6) + 128) * 1024 + usize::from(SEA_KEY.clut & 63) * 16;
    for entry in &mut vram[clut_row..clut_row + 16] {
        if *entry != 0 {
            *entry ^= 0x0421; // the low bit of each channel
        }
    }
    let started = Instant::now();
    let update = map
        .update(&Source {
            ram: &ram,
            vram: &vram,
        })
        .unwrap();
    println!(
        "sea CLUT change: {} tile types rebuilt in {:?}",
        update.tiles.len(),
        started.elapsed()
    );
    assert!(update.tiles.contains(&SEA_TILE));
    assert!(update.cells.is_empty() && !update.rebuilt);
    // The update equals a fresh build from the changed VRAM.
    let fresh = build(&ram, &vram);
    let sea = usize::from(SEA_TILE);
    println!(
        "sea tile mean colour {:?} -> {:?}",
        reference.tiles[sea].mean_color, map.tiles[sea].mean_color
    );
    assert_ne!(map.tiles[sea].mean_color, reference.tiles[sea].mean_color);
    assert_eq!(map.tiles[sea].mean_color, fresh.tiles[sea].mean_color);
    assert_eq!(map.atlas.colors, fresh.atlas.colors);
    assert_eq!(map.atlas.flags, fresh.atlas.flags);
    assert_eq!(map.mesh.vertices, fresh.mesh.vertices);
    // The vertex ranges stay; only colours change.
    assert_eq!(map.mesh.vertices.len(), reference.mesh.vertices.len());

    // Another tile directory (here: two mesh pointers swapped): a full
    // rebuild.
    let directory = (word(&ram, world + 0x24c) & 0x1f_ffff) as usize;
    let mut moved = ram.clone();
    let (a, b) = (directory + 4, directory + 8);
    let (pa, pb) = (moved[a..a + 4].to_vec(), moved[b..b + 4].to_vec());
    moved[a..a + 4].copy_from_slice(&pb);
    moved[b..b + 4].copy_from_slice(&pa);
    let update = map
        .update(&Source {
            ram: &moved,
            vram: &vram,
        })
        .unwrap();
    assert!(update.rebuilt);
    assert_eq!(map.tiles[1].address, reference.tiles[2].address);
}
