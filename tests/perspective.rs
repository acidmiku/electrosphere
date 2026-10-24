//! Perspective-correct textures (`docs/RECOMP_ENHANCEMENTS.md` §4.1) on the
//! native game: a cold boot of the disc image along the scripted pad path
//! (language, title, hub, briefing, aircraft select, flight, then the
//! external view), built as `machine_player::run` builds it, with the GPU
//! renderer at 4x and sub-pixel geometry. Two players run in lockstep, one
//! with perspective off and one with it on.
//!
//! [`perspective_changes_only_its_own_triangles`]:
//! - every pass, the two recorded frames are word for word the same but for
//!   the PERSPECTIVE bit and w of textured polygons whose vertices all have
//!   a depth (the game, and every other primitive, is untouched);
//! - at a hub, a briefing-map, an aircraft-select and two flight pictures,
//!   the pictures differ only where those triangles drew (their rectangles,
//!   followed through VRAM copies and 15-bit textures until a fill or an
//!   upload covers them); a diff mask and the pictures are saved;
//! - the 4x pictures with perspective off are those of the build before it
//!   (pixel hashes of the baseline run; re-pinned when polygons above 1x
//!   moved onto the 1x pixel grid, `src/gpu_renderer/draw.wgsl`), and those
//!   with it on are pinned (golden) on an RTX 3090 (other adapters only
//!   print theirs);
//! - crops of the aircraft, the ground, the aircraft-select model and the
//!   hub's tunnel, off and on side by side, are saved.
//!
//! Output goes to `$AC3_PERSPECTIVE_OUT` (default `target/perspective`).
use ac3_flight::Machine;
use ac3_flight::precision::PrecisionTable;
use ac3_graphics::Vram;
use ac3_port::assets::GameData;
use ac3_port::gpu_renderer::record::{VERTEX_WORDS, mode};
use ac3_port::gpu_renderer::{DisplayArea, Executor, Frame, Op, Options, TablePrecision};
use ac3_port::machine_card::{CardDevice, CardSource};
use ac3_port::machine_player::{Host, PadType, Player, parse_pad_schedule};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::rc::Rc;

const DISC: &str = "Ace Combat 3 - Electrosphere (Europe) (En,Fr,De,Es,It).bin";
/// The scripted pad path into flight, then the external view (SELECT).
const PATH: &str = "400-406:cross,550-556:start,700-706:cross,850-856:cross,1400-1406:cross,1550-1556:cross,1700-1706:cross,1850-1856:cross,2000-2006:cross,2150-2156:cross,2300-2306:cross,2450-2456:cross,2800-2806:select";
const VBLANKS: u64 = 3230;
const SCALE: u32 = 4;
/// The pictures compared (pass ends of the path, as `--every 50` saves
/// them): name, VBlank, and the SHA-256 of the 4x picture's RGB bytes with
/// perspective off (the baseline build's) and on (golden, RTX 3090).
const PICTURES: [(&str, u64, &str, &str); 5] = [
    (
        "hub",
        1124,
        "240ac81c64a4207ea9868e95cce5258e0c554a434f800e9c8c6a90045b99c658",
        "4891f64c9808eaf1f05ee26c36a5f4c85ea2a187685c01a9101dfb46360d5f50",
    ),
    (
        "briefing-map",
        1674,
        "a55dfd69651bce3775223099a34b17c420608b9972d67cc0a57bf9805b37d733",
        "bfe228f52ebb7dec9a661e846e03b61ee236cfbdf41a4ded8ed4887d0f95f55b",
    ),
    (
        "aircraft-select",
        2124,
        "b199ce6d0b5eb4ce3a7f8e391a9b3d4014508d87b25ecafc217ca9e5143ca0ac",
        "56c6fdfa88258f28333559a28e2ec6c2906f4d97b0a655c00697051e46b6d32e",
    ),
    (
        "flight-cockpit",
        2674,
        "ca44472fdff9948c00fbc882e767f12889b79607c09e570a67dd006a4e5db547",
        "5b581a429daf2e07072c2c0642456919b969234667aaae9a306323207070325b",
    ),
    (
        "flight-external",
        3224,
        "2fb9ead615c91fb387bae712871b08b9e4a6d0ee45e5f11671b117d87f6dc9f2",
        "63bd6e5da25a8145b6bac80c40d116a9677fa590d7ab54ebffa4dd1731fd29b0",
    ),
];
/// Crops saved off and on (picture, name, x, y, width, height at 4x).
const CROPS: [(&str, &str, u32, u32, u32, u32); 4] = [
    ("flight-external", "aircraft", 200, 580, 900, 400),
    ("flight-cockpit", "ground", 0, 640, 1280, 384),
    ("aircraft-select", "aircraft-model", 160, 260, 400, 520),
    ("hub", "tunnel", 600, 640, 640, 620),
];

struct Game {
    root: PathBuf,
    data: GameData,
    out: PathBuf,
}

fn game() -> Game {
    let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR"));
    let data = GameData::load(&root.join("extracted/disc")).unwrap();
    let out = std::env::var_os("AC3_PERSPECTIVE_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/perspective"));
    fs::create_dir_all(&out).unwrap();
    Game { root, data, out }
}

/// A player with the GPU renderer at 4x and sub-pixel geometry, as
/// `machine_player::run` builds it, perspective `on` or off, and its card.
fn boot(game: &Game, on: bool) -> (Player, Executor) {
    let options = Options {
        scale: SCALE,
        dither: false,
    };
    let executor = Executor::offscreen(options).expect("an offscreen graphics adapter");
    let disc = File::open(game.root.join(DISC)).unwrap();
    let mut host = Host::new(Vram::new(), Some(disc));
    host.card = CardDevice::from_sources(&[CardSource::Blank, CardSource::Blank]).unwrap();
    host.pad = PadType::Digital.ports();
    let table = PrecisionTable::shared();
    host.vertex_precision = Some(Rc::new(TablePrecision(table.clone())));
    host.perspective = on;
    host.start_gpu_renderer(options);
    let mut player = Player::new(Machine::default(), host);
    player.machine.set_precision(Some(table));
    let schedule = parse_pad_schedule(PATH).unwrap();
    player.host.borrow_mut().pad_input = Some(Box::new(move |vblank| {
        schedule
            .iter()
            .filter(|(first, last, _)| (*first..=*last).contains(&vblank))
            .fold(0, |held, (_, _, b)| held | b)
    }));
    ac3_flight::boot::boot(&mut player.machine, &game.data.executable.payload);
    player.machine.take_fault().unwrap();
    (player, executor)
}

fn step(player: &mut Player) {
    if let Err(fault) = player.iteration() {
        panic!("VBlank {}: {fault}", player.vblanks());
    }
    player.host.borrow_mut().take_audio();
    if let Some(error) = player.host.borrow().error() {
        panic!("VBlank {}: device: {error}", player.vblanks());
    }
}

/// The displayed area of the GPU renderer's VRAM (as `machine_player`).
fn display_area(host: &Host) -> Option<DisplayArea> {
    let display = host.gpu.gpu.display();
    let [_, _, w, h] = host.gpu.display;
    let area = DisplayArea {
        x: u32::from(display.start_x),
        y: u32::from(display.start_y),
        width: w.max(1) as u32,
        height: h.max(1) as u32,
    };
    (display.mode & 0x10 == 0 && area.fits()).then_some(area)
}

/// The 1x VRAM pixels whose card colour may depend on a perspective
/// triangle: drawn by one, or copied from such pixels since (a VRAM copy, a
/// 15-bit texture of them). Fills and uploads (the software VRAM's pixels)
/// clear them; every other draw keeps them, as it may blend with them.
/// Followed through each frame in the order the card draws.
struct Taint(Vec<bool>);

impl Taint {
    fn new() -> Self {
        Self(vec![false; 1024 * 512])
    }
    fn get(&self, x: i32, y: i32) -> bool {
        (0..1024).contains(&x) && (0..512).contains(&y) && self.0[(y * 1024 + x) as usize]
    }
    /// Pixels `x0..x1` x `y0..y1`, clamped to VRAM.
    fn rows([x0, y0, x1, y1]: [i32; 4]) -> impl Iterator<Item = std::ops::Range<usize>> {
        let (x0, x1) = (x0.clamp(0, 1024) as usize, x1.clamp(0, 1024) as usize);
        (y0.clamp(0, 512)..y1.clamp(0, 512))
            .map(move |y| y as usize * 1024 + x0..y as usize * 1024 + x1.max(x0))
    }
    fn set(&mut self, rect: [i32; 4], value: bool) {
        for row in Self::rows(rect) {
            self.0[row].fill(value);
        }
    }
    fn any(&self, rect: [i32; 4]) -> bool {
        Self::rows(rect).any(|row| self.0[row].iter().any(|&bit| bit))
    }

    fn follow(&mut self, frame: &Frame) {
        for op in &frame.ops {
            let Op::Draw { first, count, .. } = op else {
                continue;
            };
            let words = *first as usize * VERTEX_WORDS..(*first + *count) as usize * VERTEX_WORDS;
            for triangle in frame.vertices[words].chunks_exact(3 * VERTEX_WORDS) {
                let corner =
                    |i: usize, axis: usize| f32::from_bits(triangle[i * VERTEX_WORDS + axis]);
                let low = |axis| {
                    (0..3)
                        .map(|i| corner(i, axis))
                        .fold(f32::INFINITY, f32::min)
                };
                let high = |axis| {
                    (0..3)
                        .map(|i| corner(i, axis))
                        .fold(f32::NEG_INFINITY, f32::max)
                };
                // A whole-pixel rectangle's half: its corners are half a
                // pixel outside its pixels. Anything else: its box, grown.
                let exact = [low(0), low(1), high(0), high(1)].map(|v| (v + 0.5).round() as i32);
                let grown = [
                    low(0).floor() as i32 - 1,
                    low(1).floor() as i32 - 1,
                    high(0).ceil() as i32 + 2,
                    high(1).ceil() as i32 + 2,
                ];
                let (bits, flat) = (triangle[5], &triangle[6..VERTEX_WORDS - 1]);
                match bits & 7 {
                    mode::KIND_FILL | mode::KIND_MIRROR => self.set(exact, false),
                    mode::KIND_COPY => {
                        let (sx, sy) = ((flat[0] & 0xffff) as i32, (flat[0] >> 16) as i32);
                        let [x0, y0, x1, y1] = exact;
                        let source = [sx, sy, sx + x1 - x0, sy + y1 - y0];
                        if self.any(source) {
                            self.set(exact, true);
                        } else if flat[2] & 2 == 0 {
                            self.set(exact, false);
                        }
                    }
                    _ => {
                        let tex = flat[0];
                        let direct = bits & mode::TEXTURED != 0 && (tex >> 5) & 3 >= 2;
                        let (px, py) = ((tex & 15) as i32 * 64, ((tex >> 4) & 1) as i32 * 256);
                        let page = [px, py, px + 256, py + 256];
                        if bits & mode::PERSPECTIVE != 0 || (direct && self.any(page)) {
                            self.set(grown, true);
                        }
                    }
                }
            }
        }
    }
}

/// What the two frames of one pass differ in.
#[derive(Default, Debug)]
struct Counts {
    vertices: u64,
    perspective_vertices: u64,
    perspective_triangles: u64,
}

/// Check that `on` is `off` but for the perspective bit and w of textured
/// polygons whose three vertices all carry it.
fn compare_frames(off: &Frame, on: &Frame, counts: &mut Counts) {
    assert_eq!(off.ops, on.ops);
    assert_eq!(off.data, on.data);
    assert_eq!(off.vertices.len(), on.vertices.len());
    let a: Vec<&[u32]> = off.vertices.chunks_exact(VERTEX_WORDS).collect();
    let b: Vec<&[u32]> = on.vertices.chunks_exact(VERTEX_WORDS).collect();
    counts.vertices += a.len() as u64;
    let w = |v: &[u32]| f32::from_bits(v[VERTEX_WORDS - 1]);
    for (index, (a, b)) in a.iter().zip(&b).enumerate() {
        assert_eq!(w(a), 1.0, "vertex {index}");
        assert_eq!(a[5] & mode::PERSPECTIVE, 0);
        if b[5] & mode::PERSPECTIVE == 0 {
            assert_eq!(a, b, "vertex {index}");
            continue;
        }
        counts.perspective_vertices += 1;
        assert_eq!(b[5] & 7, mode::KIND_POLY);
        assert_ne!(b[5] & mode::TEXTURED, 0);
        assert_eq!(b[5] & mode::EXACT, 0);
        assert!(w(b) > 0.0 && w(b).is_finite(), "w {}", w(b));
        assert_eq!(a[5], b[5] & !mode::PERSPECTIVE);
        assert_eq!(a[..5], b[..5]);
        assert_eq!(a[6..VERTEX_WORDS - 1], b[6..VERTEX_WORDS - 1]);
    }
    for corners in b.chunks_exact(3) {
        let marked = corners
            .iter()
            .filter(|v| v[5] & mode::PERSPECTIVE != 0)
            .count();
        assert!(marked == 0 || marked == 3, "{marked} of a triangle");
        counts.perspective_triangles += u64::from(marked == 3);
    }
}

fn rgb_bytes(rgb: &[u32]) -> Vec<u8> {
    rgb.iter()
        .flat_map(|&p| [(p >> 16) as u8, (p >> 8) as u8, p as u8])
        .collect()
}

fn save_png(path: &Path, rgb: &[u32], width: usize, height: usize) {
    let mut encoder = png::Encoder::new(
        std::io::BufWriter::new(File::create(path).unwrap()),
        width as u32,
        height as u32,
    );
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .unwrap()
        .write_image_data(&rgb_bytes(rgb))
        .unwrap();
}

/// A captured picture pair.
struct Capture {
    name: &'static str,
    off: Vec<u32>,
    on: Vec<u32>,
    width: usize,
    height: usize,
}

/// `capture`'s pictures, a diff mask (differing pixels magenta over the
/// picture darkened, yellow where not tainted), and how many differ where
/// no perspective triangle drew.
fn report(game: &Game, capture: &Capture, area: DisplayArea, taint: &Taint) -> [usize; 3] {
    let (width, height) = (capture.width, capture.height);
    let mut mask = Vec::with_capacity(width * height);
    let (mut differing, mut outside, mut tainted) = (0, 0, 0);
    for (index, (&a, &b)) in capture.off.iter().zip(&capture.on).enumerate() {
        let (x, y) = ((index % width) as u32, (index / width) as u32);
        let vram = ((area.x + x / SCALE) as i32, (area.y + y / SCALE) as i32);
        tainted += usize::from(taint.get(vram.0, vram.1));
        if a == b {
            let grey = ((a >> 16 & 255) + (a >> 8 & 255) + (a & 255)) / 9;
            mask.push(grey * 0x01_0101);
            continue;
        }
        differing += 1;
        if taint.get(vram.0, vram.1) {
            mask.push(0xff_00ff);
        } else {
            // Yellow: outside every perspective triangle.
            mask.push(0xff_ff00);
            outside += 1;
        }
    }
    let path = |kind: &str| game.out.join(format!("{}-{kind}.png", capture.name));
    save_png(&path("off"), &capture.off, width, height);
    save_png(&path("on"), &capture.on, width, height);
    save_png(&path("diff"), &mask, width, height);
    [differing, outside, tainted]
}

/// Off and on side by side, each `zoom` times, of `capture`'s rectangle.
fn save_crop(game: &Game, capture: &Capture, name: &str, [x, y, w, h]: [u32; 4]) {
    let zoom = 2;
    let (w, h) = (
        w.min(capture.width as u32 - x),
        h.min(capture.height as u32 - y),
    );
    let out_width = (2 * w * zoom + 8) as usize;
    let mut rgb = vec![0xff_ffff; out_width * (h * zoom) as usize];
    for (side, picture) in [&capture.off, &capture.on].into_iter().enumerate() {
        for row in 0..h * zoom {
            for column in 0..w * zoom {
                let source =
                    (y + row / zoom) as usize * capture.width + (x + column / zoom) as usize;
                let left = side as u32 * (w * zoom + 8);
                rgb[row as usize * out_width + (left + column) as usize] = picture[source];
            }
        }
    }
    save_png(
        &game.out.join(format!("crop-{name}.png")),
        &rgb,
        out_width,
        (h * zoom) as usize,
    );
}

fn hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[test]
#[ignore = "requires AC3_DATA_DIR and a graphics adapter"]
fn perspective_changes_only_its_own_triangles() {
    let game = game();
    let (mut off, mut off_card) = boot(&game, false);
    let (mut on, mut on_card) = boot(&game, true);
    let adapter = on_card.description.clone();
    eprintln!("adapter: {adapter}");
    let golden_adapter = adapter.contains("RTX 3090");
    let mut counts = Counts::default();
    // Where the perspective triangles drew.
    let mut taint = Taint::new();
    let mut captures = Vec::new();
    let mut next = 0;
    while off.vblanks() < VBLANKS {
        step(&mut off);
        step(&mut on);
        assert_eq!(off.vblanks(), on.vblanks());
        assert_eq!(
            off.machine.ram(),
            on.machine.ram(),
            "VBlank {}",
            on.vblanks()
        );
        let a = off.host.borrow_mut().take_gpu_frame().unwrap();
        let b = on.host.borrow_mut().take_gpu_frame().unwrap();
        compare_frames(&a, &b, &mut counts);
        taint.follow(&b);
        off_card.execute(&a);
        on_card.execute(&b);
        let Some(&(name, vblank, baseline, golden)) = PICTURES.get(next) else {
            continue;
        };
        if on.vblanks() < vblank {
            continue;
        }
        next += 1;
        assert_eq!(on.vblanks(), vblank, "{name}: not a pass end");
        let area = display_area(&on.host.borrow()).expect("a 15-bit picture");
        let (off_rgb, width, height) = off_card.read_display(area).unwrap();
        let (on_rgb, _, _) = on_card.read_display(area).unwrap();
        let capture = Capture {
            name,
            off: off_rgb,
            on: on_rgb,
            width,
            height,
        };
        let [differing, outside, tainted] = report(&game, &capture, area, &taint);
        let (off_hash, on_hash) = (hex(&rgb_bytes(&capture.off)), hex(&rgb_bytes(&capture.on)));
        eprintln!(
            "{name} (VBlank {vblank}, {width}x{height}): {differing} pixels differ ({:.2}%), {outside} where no perspective triangle drew ({:.2}% of the picture is where they drew); off {off_hash}, on {on_hash}",
            100.0 * differing as f64 / (width * height) as f64,
            100.0 * tainted as f64 / (width * height) as f64
        );
        assert_eq!(
            outside, 0,
            "{name}: a change outside the perspective triangles"
        );
        if !baseline.is_empty() {
            assert_eq!(
                off_hash, baseline,
                "{name}: perspective off is not the baseline"
            );
        }
        if golden_adapter && !golden.is_empty() {
            assert_eq!(on_hash, golden, "{name}: the golden picture changed");
        }
        captures.push(capture);
    }
    assert_eq!(next, PICTURES.len(), "every picture captured");
    eprintln!("{counts:?}");
    assert!(counts.perspective_triangles > 100_000, "{counts:?}");
    for (picture, name, x, y, w, h) in CROPS {
        let capture = captures.iter().find(|c| c.name == picture).unwrap();
        save_crop(&game, capture, name, [x, y, w, h]);
    }
    // The flight pictures change; the hub's tunnel too.
    for capture in &captures {
        if capture.name.starts_with("flight") || capture.name == "hub" {
            assert!(capture.off != capture.on, "{}: no change", capture.name);
        }
    }
}
