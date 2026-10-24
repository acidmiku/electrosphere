//! Sub-pixel vertex positions (`ac3_flight::precision`, the "wobble fix" of
//! `docs/MILESTONE_1.md` item 6) on the native game: a cold boot of the disc
//! image along the scripted pad path into flight, with the player built as
//! `machine_player::run` builds it (two blank cards, a digital pad).
//!
//! - [`tracking_leaves_the_game_bit_identical`]: RAM (every 100 VBlanks),
//!   scratchpad, GTE, VRAM, the final picture and the mixed PCM are the same
//!   with tracking on and off, and equal the known headless run; the run
//!   time of both, overall and in flight. Then again further into flight in
//!   the external view (SELECT at 2800), where the aircraft is drawn.
//! - [`coverage_of_submitted_vertex_words`]: every DMA list the game submits
//!   (on the external-view path to 3300) is walked; each vertex word is
//!   looked up, every position found must floor to its word, and the
//!   fraction found is reported by scene and, in flight, by the frame
//!   routine that wrote the packet. The flight frame runs through a traced
//!   copy of 80033C58/80033748 that records the primitive cursor around each
//!   routine; a reference player without tracking runs in lockstep with the
//!   real callback, and every pass must match it exactly (so the copy and
//!   the tracking change nothing). It also rasterizes one flight frame at 4×
//!   from the integer and the sub-pixel positions.
//! - [`coverage_by_window`]: the same coverage for other window sizes and
//!   without following loads (about what explicit `swc2` store sites alone
//!   would reach).
//!
//! Output goes to `$AC3_PRECISION_OUT` (default `target/precision`).
use ac3_flight::precision::{Options, PrecisionTable, SharedPrecision, VertexPrecision};
use ac3_flight::{Device, Fault, HostCall, Machine, dispatch, mainloop};
use ac3_graphics::Vram;
use ac3_port::assets::GameData;
use ac3_port::flight_host::{self, LIST_END, packet_at};
use ac3_port::machine_card::{CardDevice, CardSource};
use ac3_port::machine_player::{Host, PadType, Player, SharedHost, parse_pad_schedule};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Instant;

const DISC: &str = "Ace Combat 3 - Electrosphere (Europe) (En,Fr,De,Es,It).bin";
/// The scripted pad path into flight (language, title, new game, hub,
/// briefing, aircraft select, launch).
const FLIGHT_PATH: &str = "400-406:cross,550-556:start,700-706:cross,850-856:cross,1400-1406:cross,1550-1556:cross,1700-1706:cross,1850-1856:cross,2000-2006:cross,2150-2156:cross,2300-2306:cross,2450-2456:cross";
/// The known headless run: 2700 VBlanks, final picture and PCM.
const VBLANKS: u64 = 2700;
const FINAL_PICTURE: &str = "72b41327";
const PCM: &str = "580973a9";
const PCM_FRAMES: u64 = 2_381_125;
/// Coverage runs go further into flight, and switch from the cockpit to the
/// external view (SELECT) so the player's aircraft is drawn.
const COVERAGE_VBLANKS: u64 = 3300;
const EXTERNAL_VIEW: &str = "2800-2806:select";
/// The flight frame rasterized for the demonstration.
const DEMO_VBLANK: u64 = 3000;
/// `[gp+0x2DC]` the resident context (state +0x2C), `[gp+0x2D0]` gameplay
/// (primitive cursor +0x14).
const CONTEXT: u32 = ac3_flight::GP + 0x2dc;
const GAMEPLAY: u32 = ac3_flight::GP + 0x2d0;

struct Game {
    root: PathBuf,
    data: GameData,
    out: PathBuf,
}

fn game() -> Game {
    let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR"));
    let data = GameData::load(&root.join("extracted/disc")).unwrap();
    let out = std::env::var_os("AC3_PRECISION_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/precision"));
    fs::create_dir_all(&out).unwrap();
    Game { root, data, out }
}

/// A player booted from the disc image as `machine_player::run` does.
fn boot(game: &Game, schedule: &str) -> Player {
    let disc = File::open(game.root.join(DISC)).unwrap();
    let mut host = Host::new(Vram::new(), Some(disc));
    host.card = CardDevice::from_sources(&[CardSource::Blank, CardSource::Blank]).unwrap();
    host.pad = PadType::Digital.ports();
    let mut player = Player::new(Machine::default(), host);
    let schedule = parse_pad_schedule(schedule).unwrap();
    player.host.borrow_mut().pad_input = Some(Box::new(move |vblank| {
        schedule
            .iter()
            .filter(|(first, last, _)| (*first..=*last).contains(&vblank))
            .fold(0, |held, (_, _, b)| held | b)
    }));
    ac3_flight::boot::boot(&mut player.machine, &game.data.executable.payload);
    player.machine.take_fault().unwrap();
    player
}

/// After one main-loop pass: its PCM, with the device error check.
fn finish_pass(player: &mut Player, result: Result<(), Fault>) -> Vec<[i16; 2]> {
    if let Err(fault) = result {
        panic!("VBlank {}: {fault}", player.vblanks());
    }
    let pcm = player.host.borrow_mut().take_audio();
    if let Some(error) = player.host.borrow().error() {
        panic!("VBlank {}: device: {error}", player.vblanks());
    }
    pcm
}
fn step(player: &mut Player) -> Vec<[i16; 2]> {
    let result = player.iteration();
    finish_pass(player, result)
}

fn hex(bytes: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn pcm_bytes(pcm: &[[i16; 2]]) -> Vec<u8> {
    pcm.iter().flatten().flat_map(|v| v.to_le_bytes()).collect()
}
fn gte_words(m: &Machine) -> Vec<u8> {
    m.gte
        .data
        .iter()
        .chain(&m.gte.control)
        .flat_map(|w| w.to_le_bytes())
        .collect()
}
fn vram_bytes(player: &Player) -> Vec<u8> {
    let host = player.host.borrow();
    host.gpu
        .gpu
        .vram()
        .words()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect()
}
fn ram_word(ram: &[u8], address: u32) -> u32 {
    let at = (address & 0x1f_ffff) as usize;
    u32::from_le_bytes(ram[at..at + 4].try_into().unwrap())
}

fn png_bytes(width: usize, height: usize, rgb: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, width as u32, height as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .unwrap()
        .write_image_data(rgb)
        .unwrap();
    bytes
}
/// The displayed picture as `machine_player::run` saves `final.png`.
fn final_picture(player: &Player) -> Vec<u8> {
    let host = player.host.borrow();
    let [_, _, w, h] = host.gpu.display;
    let (w, h) = (w.max(1) as usize, h.max(1) as usize);
    assert_eq!(host.gpu.gpu.display().mode & 0x10, 0, "a 15-bit picture");
    let pixels = host.gpu.displayed().unwrap();
    let rgb: Vec<u8> = pixels
        .iter()
        .flat_map(|&p| {
            let [r, g, b, _] = ac3_graphics::rgba8(p);
            [r, g, b]
        })
        .collect();
    png_bytes(w, h, &rgb)
}

// ---- bit identity and cost --------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    /// RAM hash every 100 VBlanks.
    checkpoints: Vec<String>,
    ram: String,
    scratch: String,
    gte: String,
    vram: String,
    picture: String,
    pcm: String,
    frames: u64,
}

/// Wall time of a run: all passes, and the flight (state 2) passes.
#[derive(Clone, Copy, Default)]
struct Timing {
    seconds: f64,
    flight_seconds: f64,
    flight_passes: u64,
}

fn identity_run(
    game: &Game,
    schedule: &str,
    vblanks: u64,
    precise: bool,
) -> (Outcome, Timing, u64) {
    let mut player = boot(game, schedule);
    let table = precise.then(PrecisionTable::shared);
    player.machine.set_precision(table.clone());
    let mut pcm = Sha256::new();
    let (mut frames, mut checkpoints, mut next) = (0, Vec::new(), 100);
    let mut timing = Timing::default();
    let started = Instant::now();
    while player.vblanks() < vblanks {
        let pass = Instant::now();
        let audio = step(&mut player);
        if scene(player.machine.ram()) == "flight" {
            timing.flight_seconds += pass.elapsed().as_secs_f64();
            timing.flight_passes += 1;
        }
        frames += audio.len() as u64;
        pcm.update(pcm_bytes(&audio));
        if player.vblanks() >= next {
            checkpoints.push(hex(player.machine.ram()));
            next += 100;
        }
    }
    timing.seconds = started.elapsed().as_secs_f64();
    let recorded = table.map_or(0, |t| t.borrow().stats().recorded);
    let outcome = Outcome {
        checkpoints,
        ram: hex(player.machine.ram()),
        scratch: hex(player.machine.scratch()),
        gte: hex(gte_words(&player.machine)),
        vram: hex(vram_bytes(&player)),
        picture: hex(final_picture(&player)),
        pcm: format!("{:x}", pcm.finalize()),
        frames,
    };
    (outcome, timing, recorded)
}

#[test]
#[ignore = "requires AC3_DATA_DIR"]
fn tracking_leaves_the_game_bit_identical() {
    let game = game();
    let change = |on: f64, off: f64| (on / off - 1.0) * 100.0;
    let report = |name: &str, off: Timing, on: Timing, recorded: u64| {
        eprintln!(
            "{name}: precision off {:.2} s, on {:.2} s ({:+.1}%); {} flight passes {:.3} s, {:.3} s ({:+.1}%); {recorded} positions recorded",
            off.seconds,
            on.seconds,
            change(on.seconds, off.seconds),
            on.flight_passes,
            off.flight_seconds,
            on.flight_seconds,
            change(on.flight_seconds, off.flight_seconds)
        );
    };
    // The known headless run into flight (cockpit view).
    let (plain, off, _) = identity_run(&game, FLIGHT_PATH, VBLANKS, false);
    let (precise, on, recorded) = identity_run(&game, FLIGHT_PATH, VBLANKS, true);
    report("path into flight", off, on, recorded);
    eprintln!(
        "final picture {}, PCM {} ({} frames), RAM {}, VRAM {}",
        precise.picture, precise.pcm, precise.frames, precise.ram, precise.vram
    );
    assert!(
        plain.picture.starts_with(FINAL_PICTURE),
        "{}",
        plain.picture
    );
    assert!(plain.pcm.starts_with(PCM), "{}", plain.pcm);
    assert_eq!(plain.frames, PCM_FRAMES);
    assert_eq!(plain, precise);
    assert!(recorded > 100_000, "not vacuous: {recorded}");
    // Further into flight in the external view (the aircraft drawn).
    let path = format!("{FLIGHT_PATH},{EXTERNAL_VIEW}");
    let (plain, off, _) = identity_run(&game, &path, COVERAGE_VBLANKS, false);
    let (precise, on, recorded) = identity_run(&game, &path, COVERAGE_VBLANKS, true);
    report("external view to 3300", off, on, recorded);
    assert_eq!(plain, precise);
    assert!(on.flight_passes > 300, "{}", on.flight_passes);
}

// ---- the traced flight frame --------------------------------------------------------------

/// A copy of the flight state callback 80033C58 and the frame body 80033748
/// (`ac3_flight::frame`) that records the primitive cursor `[gameplay+0x14]`
/// around each routine. The lockstep reference proves it exact.
mod traced {
    use super::{GAMEPLAY, ram_word};
    use ac3_flight::common::{CONTEXT, GAMEPLAY as GAMEPLAY_OFFSET};
    use ac3_flight::{Machine, SmallData, dispatch, frame, sdk};

    pub const FLIGHT_CALLBACK: u32 = 0x8003_3c58;
    const DRAW_BUFFERS: u32 = 0x2d4;
    const OT_ENTRIES: u32 = 0x1000;
    const LOW24: u32 = 0x00ff_ffff;
    const HIGH8: u32 = 0xff00_0000;
    const UPDATE_REGULAR: u32 = 0x8003_2ca4;

    /// RAM offsets [start, end) of the packets a routine wrote.
    pub type Range = (u32, u32, &'static str);

    fn cursor(m: &Machine) -> u32 {
        ram_word(m.ram(), ram_word(m.ram(), GAMEPLAY) + 0x14)
    }
    fn range(start: u32, end: u32, category: &'static str) -> Range {
        (start & 0x1f_ffff, end & 0x1f_ffff, category)
    }
    fn call(m: &mut Machine, address: u32, args: [u32; 4]) -> u32 {
        dispatch::call(m, address, args)
    }
    fn traced(
        m: &mut Machine,
        trace: &mut Vec<Range>,
        category: &'static str,
        address: u32,
        args: [u32; 4],
    ) {
        let start = cursor(m);
        call(m, address, args);
        trace.push(range(start, cursor(m), category));
    }
    fn add_prim(m: &mut Machine, slot: u32, packet: u32) {
        let tag = m.lw(packet);
        let link = m.lw(slot);
        m.sw(packet, (tag & HIGH8) | (link & LOW24));
        let link = m.lw(slot);
        m.sw(slot, (link & HIGH8) | (packet & LOW24));
    }
    fn draw_offset_packet(m: &mut Machine, slot: u32, packet: u32, x: i16, y: i16) {
        let command = ((y as i32 as u32) << 11) | (x as i32 as u32) | 0xe500_0000;
        m.sw(packet + 4, command);
        m.sw(packet + 8, 0);
        let link = m.lw(slot);
        m.sw(packet, link | 0x0200_0000);
        m.sw(slot, (packet << 8) >> 8);
    }

    /// 80033C58 (`frame::flight_callback`).
    pub fn flight_callback(m: &mut Machine, trace: &mut Vec<Range>) -> u32 {
        m.push_frame(0x18);
        let gameplay = m.gw(GAMEPLAY_OFFSET);
        let flags = m.lw(gameplay);
        m.sw(gameplay, flags | 2);
        frame_body(m, UPDATE_REGULAR, trace);
        let context = m.gw(CONTEXT);
        let flags = m.lw(context);
        if flags & 0x2000_0000 != 0 {
            traced(m, trace, "transition", 0x8003_4428, [0; 4]);
            let context = m.gw(CONTEXT);
            let gameplay = m.gw(GAMEPLAY_OFFSET);
            let a0 = m.lw(context + 0xc);
            let a1 = m.lw(gameplay + 0x14);
            let primitives = call(m, 0x8003_4640, [a0, a1, 0, 0]);
            trace.push(range(a1, primitives, "overlay"));
            let gameplay = m.gw(GAMEPLAY_OFFSET);
            m.sw(gameplay + 0x14, primitives);
            let context = m.gw(CONTEXT);
            if m.lw(context) & 0x0800_0000 != 0 {
                call(m, 0x8002_c2cc, [0, 0, 0, 0]);
            }
        } else if !flags & 0x1000_0000 != 0 {
            let buttons = m.lhu(context + 0x156);
            let mut pause = false;
            if buttons & 1 != 0 {
                m.sh(context + 0x156, buttons & 0xfffe);
                pause = true;
            }
            let context = m.gw(CONTEXT);
            if m.lw(context + 0x12c) & 0x40 != 0 {
                pause = true;
            }
            if pause {
                let flags = m.lw(context);
                m.sw(context, flags | 0x0400_0000);
                call(m, 0x8002_c2cc, [1, 0, 0, 0]);
            }
        }
        let v0 = frame::frame_tail(m);
        m.pop_frame(0x18);
        v0
    }

    /// 80033748 (`frame::frame`).
    fn frame_body(m: &mut Machine, update: u32, trace: &mut Vec<Range>) {
        m.push_frame(0x20);
        let context = m.gw(CONTEXT);
        let gameplay = m.gw(GAMEPLAY_OFFSET);
        let buffer = m.lw(context + 4);
        let slot = gameplay + (buffer << 2);
        let ot = m.lw(slot + 0x18);
        m.sw(gameplay + 0x10, ot);
        let primitives = m.lw(slot + 0x20);
        m.sw(gameplay + 0x14, primitives);
        sdk::clear_otag_r(m, ot, OT_ENTRIES);

        let gameplay = m.gw(GAMEPLAY_OFFSET);
        let context = m.gw(CONTEXT);
        let ot = m.lw(gameplay + 0x10);
        let chain = m.lw(context + 8);
        let head = m.lw(ot);
        let next = m.lw(chain);
        m.sw(ot, (head & HIGH8) | (next & LOW24));
        let last = m.lw(gameplay + 0x10).wrapping_add(0x3ffc);
        let chain = m.lw(context + 8);
        let word = m.lw(chain);
        m.sw(chain, (word & HIGH8) | (last & LOW24));
        for offset in [0x28c, 0x28e, 0x290, 0x292] {
            m.sh(gameplay + offset, 0);
        }
        traced(m, trace, "update", update, [gameplay, chain, context, 0]);

        let gameplay = m.gw(GAMEPLAY_OFFSET);
        let camera = m.lw(gameplay + 0x28);
        traced(m, trace, "camera", 0x8007_9da4, [camera, 0, 0, 0]);
        let gameplay = m.gw(GAMEPLAY_OFFSET);
        let camera = m.lw(gameplay + 0x28);
        traced(
            m,
            trace,
            "camera",
            0x8003_304c,
            [camera, gameplay + 0x2c, gameplay + 0x4c, 0],
        );

        let context = m.gw(CONTEXT);
        let buffer = m.lw(context + 4);
        let gameplay = m.gw(GAMEPLAY_OFFSET);
        let table = m.gw(DRAW_BUFFERS).wrapping_add(buffer.wrapping_mul(0x1014));
        let env = m.lw(table);
        let mut packet = m.lw(gameplay + 0x14);
        let ofs_x = m.lh(env + 8);
        let ofs_y = m.lh(env + 0xa);
        let width = m.lhu(env + 4);
        let height = m.lhu(env + 6);
        let shake = [0x28c, 0x28e, 0x290, 0x292].map(|offset| m.lhu(gameplay + offset));
        m.sh(gameplay + 0x294, shake[0].wrapping_add(ofs_x as u16));
        m.sh(gameplay + 0x296, shake[1].wrapping_add(ofs_y as u16));
        let center_x = (ofs_x as i32 + ((u32::from(width) << 16) as i32 >> 17)) as u16;
        let center_y = (ofs_y as i32 + ((u32::from(height) << 16) as i32 >> 17)) as u16;
        m.sh(gameplay + 0x29a, shake[3].wrapping_add(center_y));
        m.sh(gameplay + 0x298, shake[2].wrapping_add(center_x));
        let full_width = ((u32::from(width) << 16) as i32 >> 16) as u16;
        for (index, offset) in [0x290_u32, 0x292].into_iter().enumerate() {
            let gameplay = m.gw(GAMEPLAY_OFFSET);
            if m.lh(gameplay + offset) == 0 {
                continue;
            }
            m.sb(packet + 3, 3);
            m.sb(packet + 7, 0x60);
            for byte in 4..7 {
                m.sb(packet + byte, 0);
            }
            let gameplay = m.gw(GAMEPLAY_OFFSET);
            m.sh(packet + 8, 0);
            m.sh(packet + 0xa, 0);
            if index == 0 {
                let bar = m.lhu(gameplay + 0x290);
                m.sh(packet + 0xe, 0x100);
                m.sh(packet + 0xc, bar);
            } else {
                m.sh(packet + 0xc, full_width);
                let bar = m.lhu(gameplay + 0x292);
                m.sh(packet + 0xe, bar);
            }
            let ot = m.lw(gameplay + 0x10);
            add_prim(m, ot, packet);
            packet += 0x10;
        }

        let gameplay = m.gw(GAMEPLAY_OFFSET);
        m.sw(gameplay + 0x14, packet);
        let ot = m.lw(gameplay + 0x10);
        draw_offset_packet(m, ot, packet, ofs_x, ofs_y);
        let gameplay = m.gw(GAMEPLAY_OFFSET);
        let x = m.lh(gameplay + 0x294);
        let y = m.lh(gameplay + 0x296);
        let second = packet + 0xc;
        m.sw(gameplay + 0x14, second);
        let ot = m.lw(gameplay + 0x10);
        draw_offset_packet(m, ot + 4, second, x, y);
        m.sw(gameplay + 0x14, packet + 0x18);
        sdk::set_geom_offset(m, 0, 0);

        for (routine, category) in [
            (0x8003_308c, "trails+objects"),
            (0x8003_3140, "effects"),
            (0x8003_3194, "entities"),
            (0x8003_31d4, "terrain+sky"),
        ] {
            traced(m, trace, category, routine, [0; 4]);
        }

        let gameplay = m.gw(GAMEPLAY_OFFSET);
        let packet = m.lw(gameplay + 0x14);
        let x = m.lh(gameplay + 0x298);
        let y = m.lh(gameplay + 0x29a);
        let ot = m.lw(gameplay + 0x10);
        draw_offset_packet(m, ot + 0x3ffc, packet, x, y);
        m.sw(gameplay + 0x14, packet + 0xc);
        sdk::set_geom_offset(m, 0xa0, 0x80);

        for routine in [0x8003_9d84, 0x8003_3524, 0x8003_3544] {
            traced(m, trace, "hud", routine, [0; 4]);
        }
        let gameplay = m.gw(GAMEPLAY_OFFSET);
        if !m.lw(gameplay) & 1 != 0 {
            traced(m, trace, "hud", 0x8003_357c, [0; 4]);
        }
        m.pop_frame(0x20);
    }
}

// ---- walking the submitted lists --------------------------------------------------------------

/// One GP0 command of a packet.
enum Command {
    /// A polygon, line or rectangle: its class, kind (e.g. `GT4`), the RAM
    /// address and word of each vertex, the first colour and semi-transparency.
    Primitive {
        class: &'static str,
        kind: String,
        vertices: Vec<(u32, u32)>,
        color: u32,
        semi: bool,
    },
    /// E3 drawing area top left, E5 drawing offset.
    Area(i32, i32),
    Offset(i32, i32),
}

fn sext11(value: u32) -> i32 {
    ((value << 21) as i32) >> 21
}

/// The commands of a packet whose first word is at `base`.
fn commands(words: &[u32], base: u32) -> Vec<Command> {
    let mut out = Vec::new();
    let at = |i: usize| base + 4 * i as u32;
    let mut i = 0;
    while i < words.len() {
        let word = words[i];
        let code = word >> 24;
        let semi = code & 2 != 0;
        let color = word & 0x00ff_ffff;
        let vertex = |out: &mut Vec<(u32, u32)>, j: usize| {
            if let Some(&w) = words.get(j) {
                out.push((at(j), w));
            }
        };
        let length = match code {
            0x20..=0x3f => {
                let (gouraud, quad, textured) = (code & 0x10 != 0, code & 8 != 0, code & 4 != 0);
                let n = if quad { 4 } else { 3 };
                let stride = 1 + usize::from(textured) + usize::from(gouraud);
                let mut vertices = Vec::new();
                for k in 0..n {
                    vertex(&mut vertices, i + 1 + k * stride);
                }
                let kind = format!(
                    "{}{}{n}",
                    if gouraud { "G" } else { "F" },
                    if textured { "T" } else { "" }
                );
                out.push(Command::Primitive {
                    class: "polygon",
                    kind,
                    vertices,
                    color,
                    semi,
                });
                1 + n * (1 + usize::from(textured)) + if gouraud { n - 1 } else { 0 }
            }
            0x40..=0x5f => {
                let (gouraud, poly) = (code & 0x10 != 0, code & 8 != 0);
                let mut vertices = Vec::new();
                vertex(&mut vertices, i + 1);
                let mut j = i + 2;
                let mut count = 1;
                loop {
                    if j >= words.len() || (!poly && count == 2) {
                        break;
                    }
                    if poly && words[j] & 0xf000_f000 == 0x5000_5000 {
                        j += 1;
                        break;
                    }
                    if gouraud {
                        j += 1;
                    }
                    vertex(&mut vertices, j);
                    j += 1;
                    count += 1;
                }
                out.push(Command::Primitive {
                    class: "line",
                    kind: if gouraud { "LG" } else { "LF" }.to_owned(),
                    vertices,
                    color,
                    semi,
                });
                j - i
            }
            0x60..=0x7f => {
                let mut vertices = Vec::new();
                vertex(&mut vertices, i + 1);
                out.push(Command::Primitive {
                    class: "rect",
                    kind: "RECT".to_owned(),
                    vertices,
                    color,
                    semi,
                });
                2 + usize::from(code & 4 != 0) + usize::from(code >> 3 & 3 == 0)
            }
            0x02 | 0xc0..=0xdf => 3,
            0x80..=0x9f => 4,
            0xa0..=0xbf => {
                let size = words.get(i + 2).copied().unwrap_or(0);
                let pixels = (size & 0xffff) as usize * (size >> 16) as usize;
                3 + pixels.div_ceil(2)
            }
            0xe3 => {
                out.push(Command::Area(
                    (word & 0x3ff) as i32,
                    (word >> 10 & 0x3ff) as i32,
                ));
                1
            }
            0xe5 => {
                out.push(Command::Offset(sext11(word), sext11(word >> 11)));
                1
            }
            _ => 1,
        };
        i += length.max(1);
    }
    out
}

fn scene(ram: &[u8]) -> String {
    let context = ram_word(ram, CONTEXT);
    match ram_word(ram, context + 0x2c) {
        2 => "flight".to_owned(),
        3 => "hub/briefing".to_owned(),
        4 => "menus".to_owned(),
        state => format!("state {state}"),
    }
}

/// Words and positions found.
#[derive(Clone, Copy, Default, Debug)]
struct Count {
    words: u64,
    precise: u64,
}
impl Count {
    fn add(&mut self, precise: bool) {
        self.words += 1;
        self.precise += u64::from(precise);
    }
    fn percent(self) -> f64 {
        if self.words == 0 {
            0.0
        } else {
            self.precise as f64 * 100.0 / self.words as f64
        }
    }
}

/// A polygon of the demonstration frame: screen coordinates (drawing offset
/// applied, drawing area origin removed) from the integer words and from
/// the positions found (the integer where none).
struct DemoPolygon {
    integer: Vec<[f32; 2]>,
    precise: Vec<[f32; 2]>,
    color: [u8; 3],
    semi: bool,
    found: bool,
    category: &'static str,
}

#[derive(Default)]
struct Report {
    /// (scene, category, class) -> count.
    counts: BTreeMap<(String, &'static str, &'static str), Count>,
    /// (scene, category, polygon kind) -> count.
    kinds: BTreeMap<(String, &'static str, String), Count>,
    lists: u64,
    /// Positions checked against their word, and the failures.
    rounding: u64,
    failures: Vec<String>,
    /// The demonstration frame's polygons and display size.
    demo: Option<(Vec<DemoPolygon>, [usize; 2])>,
    seconds: f64,
}

/// Packet ranges of the last two traced flight frames.
type Frames = Rc<RefCell<VecDeque<Vec<traced::Range>>>>;

/// Device wrapper: walks each submitted DMA list before the host's GPU
/// executes it.
struct Observer {
    host: SharedHost,
    table: SharedPrecision,
    frames: Frames,
    demo_from: Option<u64>,
    report: Report,
}

impl Observer {
    fn observe(&mut self, ram: &[u8], start: u32) {
        let scene = scene(ram);
        let vblank = self.host.0.borrow().vblanks;
        let capture = scene == "flight"
            && self.report.demo.is_none()
            && self.demo_from.is_some_and(|from| vblank >= from);
        let table = self.table.borrow();
        let view = table.view(ram);
        let frames = self.frames.borrow();
        let category = |address: u32| -> &'static str {
            let offset = address & 0x1f_ffff;
            frames
                .iter()
                .rev()
                .flatten()
                .find(|&&(start, end, _)| (start..end).contains(&offset))
                .map_or("other", |&(_, _, category)| category)
        };
        let mut demo = Vec::new();
        let (mut offset, mut area) = ((0, 0), (0, 0));
        let mut address = start & LIST_END;
        self.report.lists += 1;
        for _ in 0..flight_host::MAX_LIST_PACKETS {
            let Ok((words, next)) = packet_at(ram, address) else {
                return;
            };
            let category = if scene == "flight" {
                category(address)
            } else {
                "all"
            };
            for command in commands(&words, address + 4) {
                let (class, kind, vertices, color, semi) = match command {
                    Command::Area(x, y) => {
                        area = (x, y);
                        continue;
                    }
                    Command::Offset(x, y) => {
                        offset = (x, y);
                        continue;
                    }
                    Command::Primitive {
                        class,
                        kind,
                        vertices,
                        color,
                        semi,
                    } => (class, kind, vertices, color, semi),
                };
                let mut polygon = DemoPolygon {
                    integer: Vec::new(),
                    precise: Vec::new(),
                    color: [color as u8, (color >> 8) as u8, (color >> 16) as u8],
                    semi,
                    found: false,
                    category,
                };
                for &(vertex, word) in &vertices {
                    let position = view.precise_vertex(vertex);
                    let (x, y) = (word as u16 as i16, (word >> 16) as u16 as i16);
                    if let Some([px, py, _]) = position {
                        self.report.rounding += 1;
                        if px.floor() as i16 != x || py.floor() as i16 != y {
                            self.report.failures.push(format!(
                                "VBlank {vblank} {vertex:#x}: {px},{py} for ({x},{y})"
                            ));
                        }
                    }
                    let found = position.is_some();
                    self.report
                        .counts
                        .entry((scene.clone(), category, class))
                        .or_default()
                        .add(found);
                    if class == "polygon" {
                        self.report
                            .kinds
                            .entry((scene.clone(), category, kind.clone()))
                            .or_default()
                            .add(found);
                    }
                    let origin = |v: f32, o: i32, a: i32| v + (o - a) as f32;
                    let screen = |vx: f32, vy: f32| {
                        [origin(vx, offset.0, area.0), origin(vy, offset.1, area.1)]
                    };
                    polygon.integer.push(screen(f32::from(x), f32::from(y)));
                    let [px, py] = position.map_or([f32::from(x), f32::from(y)], |p| [p[0], p[1]]);
                    polygon.precise.push(screen(px, py));
                    polygon.found |= found;
                }
                if capture && class == "polygon" {
                    demo.push(polygon);
                }
            }
            if next == LIST_END {
                break;
            }
            address = next;
        }
        // The frame's main list, not an environment or transfer list.
        if capture && demo.len() >= 100 {
            let [_, _, w, h] = self.host.0.borrow().gpu.display;
            self.report.demo = Some((demo, [w.max(1) as usize, h.max(1) as usize]));
        }
    }
}

struct ObserverDevice(Rc<RefCell<Observer>>);
impl Device for ObserverDevice {
    fn call(&mut self, ram: &mut [u8], scratch: &mut [u8], call: HostCall) -> Option<u32> {
        if call.function == flight_host::sdk::DMA_LIST {
            self.0.borrow_mut().observe(ram, call.args[0]);
        }
        let mut host = self.0.borrow().host.clone();
        host.call(ram, scratch, call)
    }
}

/// One coverage run to `until`, with a lockstep reference when `lockstep`.
fn coverage_run(game: &Game, options: Options, until: u64, lockstep: bool, demo: bool) -> Report {
    let table = Rc::new(RefCell::new(PrecisionTable::new(options)));
    let path = format!("{FLIGHT_PATH},{EXTERNAL_VIEW}");
    let mut player = boot(game, &path);
    player.machine.set_precision(Some(table.clone()));
    let frames = Rc::new(RefCell::new(VecDeque::new()));
    let observer = Rc::new(RefCell::new(Observer {
        host: SharedHost(player.host.clone()),
        table: table.clone(),
        frames: frames.clone(),
        demo_from: demo.then_some(DEMO_VBLANK),
        report: Report::default(),
    }));
    player.machine.device = Some(Box::new(ObserverDevice(observer.clone())));
    let mut reference = lockstep.then(|| boot(game, &path));
    let mut flight_frames = 0;
    let started = Instant::now();
    while player.vblanks() < until {
        player.machine.host_calls.clear();
        let mut invoke = |m: &mut Machine, target: u32, args: [u32; 4]| -> u32 {
            if target != traced::FLIGHT_CALLBACK {
                return dispatch::call(m, target, args);
            }
            let mut trace = Vec::new();
            let v0 = traced::flight_callback(m, &mut trace);
            let mut frames = frames.borrow_mut();
            frames.push_back(trace);
            if frames.len() > 2 {
                frames.pop_front();
            }
            flight_frames += 1;
            v0
        };
        mainloop::main_loop_iteration_with(&mut player.machine, &mut invoke);
        if player.machine.fault().is_none() {
            ac3_flight::cdstream::cd_service(&mut player.machine);
        }
        let result = player.machine.take_fault();
        let pcm = finish_pass(&mut player, result);
        if let Some(reference) = reference.as_mut() {
            let expected = step(reference);
            let vblank = player.vblanks();
            assert_eq!(reference.vblanks(), vblank);
            assert!(
                reference.machine.ram() == player.machine.ram(),
                "RAM at VBlank {vblank}"
            );
            assert!(
                reference.machine.scratch() == player.machine.scratch(),
                "scratchpad at {vblank}"
            );
            assert_eq!(
                reference.machine.gte.data, player.machine.gte.data,
                "VBlank {vblank}"
            );
            assert_eq!(reference.machine.gte.control, player.machine.gte.control);
            assert_eq!(expected, pcm, "PCM at VBlank {vblank}");
            if vblank % 100 < 2 {
                assert!(
                    vram_bytes(reference) == vram_bytes(&player),
                    "VRAM at {vblank}"
                );
            }
        }
    }
    assert!(flight_frames > 100, "{flight_frames} traced flight frames");
    player.machine.device = None;
    let mut report = std::mem::take(&mut observer.borrow_mut().report);
    report.seconds = started.elapsed().as_secs_f64();
    // Every recorded position floors to its word.
    for (address, word, [x, y, _]) in table.borrow().entries() {
        report.rounding += 1;
        if x.floor() as i16 != word as u16 as i16 || y.floor() as i16 != (word >> 16) as u16 as i16
        {
            report
                .failures
                .push(format!("entry {address:#x}: {x},{y} for {word:#010x}"));
        }
    }
    report
}

fn summary(report: &Report) -> String {
    let mut lines = String::new();
    for ((scene, category, class), count) in &report.counts {
        lines += &format!(
            "  {scene:<12} {category:<15} {class:<8} {:>9} words {:>9} precise {:6.2}%\n",
            count.words,
            count.precise,
            count.percent()
        );
    }
    lines += "  polygon kinds:\n";
    for ((scene, category, kind), count) in &report.kinds {
        lines += &format!(
            "  {scene:<12} {category:<15} {kind:<5} {:>9} words {:6.2}%\n",
            count.words,
            count.percent()
        );
    }
    lines
}

fn count(report: &Report, scene: &str, category: &str, class: &str) -> Count {
    report
        .counts
        .get(&(scene.to_owned(), category, class))
        .copied()
        .unwrap_or_default()
}

#[test]
#[ignore = "requires AC3_DATA_DIR"]
fn coverage_of_submitted_vertex_words() {
    let game = game();
    let report = coverage_run(&game, Options::default(), COVERAGE_VBLANKS, true, true);
    let text = format!(
        "coverage to VBlank {COVERAGE_VBLANKS} ({} lists, {:.1} s, window {}):\n{}",
        report.lists,
        report.seconds,
        Options::default().window,
        summary(&report)
    );
    eprintln!("{text}");
    fs::write(game.out.join("coverage.txt"), &text).unwrap();
    assert!(
        report.failures.is_empty(),
        "{} of {} positions do not floor to their word: {:?}",
        report.failures.len(),
        report.rounding,
        &report.failures[..report.failures.len().min(8)]
    );
    assert!(report.rounding > 100_000, "{}", report.rounding);
    let (demo, size) = report.demo.as_ref().expect("a flight frame was captured");
    write_demo(&game.out, demo, *size);
    // The GTE geometry: the player's aircraft (entity models), the terrain
    // meshes, the object systems and the briefing map's terrain.
    let polygon = |scene, category| count(&report, scene, category, "polygon");
    let kind = |scene: &str, category, kind: &str| {
        report
            .kinds
            .get(&(scene.to_owned(), category, kind.to_owned()))
            .copied()
            .unwrap_or_default()
    };
    let at_least = |count: Count, percent: f64, what: &str| {
        assert!(count.words > 1_000, "{what}: {count:?}");
        assert!(count.percent() >= percent, "{what}: {count:?}");
    };
    at_least(polygon("flight", "entities"), 99.9, "aircraft");
    at_least(kind("flight", "terrain+sky", "FT4"), 99.9, "terrain FT4");
    at_least(kind("flight", "terrain+sky", "GT4"), 99.9, "terrain GT4");
    at_least(polygon("flight", "terrain+sky"), 95.0, "terrain and sky");
    at_least(
        polygon("flight", "trails+objects"),
        80.0,
        "trails and objects",
    );
    at_least(
        kind("hub/briefing", "all", "G4"),
        99.9,
        "briefing map terrain",
    );
    at_least(polygon("hub/briefing", "all"), 85.0, "hub and briefing");
    // Typed in screen space: menus, and the HUD's and the hub's sprites.
    for (scene, category, class) in [
        ("menus", "all", "polygon"),
        ("menus", "all", "rect"),
        ("flight", "other", "rect"),
    ] {
        let count = count(&report, scene, category, class);
        assert!(count.words > 1_000, "{scene} {category} {class}: {count:?}");
        assert_eq!(count.precise, 0, "{scene} {category} {class}");
    }
    assert!(count(&report, "hub/briefing", "all", "rect").percent() < 0.1);
}

#[test]
#[ignore = "requires AC3_DATA_DIR"]
fn coverage_by_window() {
    let game = game();
    let mut text = String::new();
    for (window, follow_loads) in [
        (3, false),
        (32, false),
        (256, false),
        (8, true),
        (16, true),
        (32, true),
        (64, true),
        (256, true),
    ] {
        let options = Options {
            window,
            follow_loads,
        };
        let report = coverage_run(&game, options, COVERAGE_VBLANKS, false, false);
        assert!(report.failures.is_empty(), "{:?}", &report.failures[..1]);
        let line = |scene: &str, category: &str, class: &str| {
            format!("{:.2}", count(&report, scene, category, class).percent())
        };
        let hub = report
            .counts
            .iter()
            .filter(|((scene, _, class), _)| scene == "hub/briefing" && *class == "polygon")
            .fold(Count::default(), |sum, (_, c)| Count {
                words: sum.words + c.words,
                precise: sum.precise + c.precise,
            });
        let row = format!(
            "window {window:>3} loads {follow_loads:<5}: hub/briefing {:.2} | flight terrain {} entities {} trails+objects {} effects {} hud {} other {} | {:.1} s\n",
            hub.percent(),
            line("flight", "terrain+sky", "polygon"),
            line("flight", "entities", "polygon"),
            line("flight", "trails+objects", "polygon"),
            line("flight", "effects", "polygon"),
            line("flight", "hud", "polygon"),
            line("flight", "other", "polygon"),
            report.seconds
        );
        eprint!("{row}");
        text += &row;
    }
    fs::write(game.out.join("coverage-by-window.txt"), &text).unwrap();
}

// ---- the demonstration pictures --------------------------------------------------------------

const SCALE: f32 = 4.0;

struct Canvas {
    width: usize,
    height: usize,
    rgb: Vec<u8>,
}
impl Canvas {
    fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            rgb: vec![0; width * height * 3],
        }
    }
    fn blend(&mut self, x: usize, y: usize, color: [u8; 3], semi: bool) {
        let at = (y * self.width + x) * 3;
        for (c, &value) in self.rgb[at..at + 3].iter_mut().zip(&color) {
            *c = if semi {
                ((u16::from(*c) + u16::from(value)) / 2) as u8
            } else {
                value
            };
        }
    }
    /// Fill the triangle (pixel centres inside, either winding).
    fn triangle(&mut self, p: [[f32; 2]; 3], color: [u8; 3], semi: bool) {
        let edge = |a: [f32; 2], b: [f32; 2], c: [f32; 2]| {
            (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
        };
        let area = edge(p[0], p[1], p[2]);
        if area == 0.0 {
            return;
        }
        let min = |i: usize| p.iter().map(|v| v[i]).fold(f32::MAX, f32::min);
        let max = |i: usize| p.iter().map(|v| v[i]).fold(f32::MIN, f32::max);
        let x0 = min(0).floor().max(0.0) as usize;
        let y0 = min(1).floor().max(0.0) as usize;
        let x1 = (max(0).ceil() as usize).min(self.width);
        let y1 = (max(1).ceil() as usize).min(self.height);
        for y in y0..y1 {
            for x in x0..x1 {
                let c = [x as f32 + 0.5, y as f32 + 0.5];
                let w = [
                    edge(p[1], p[2], c),
                    edge(p[2], p[0], c),
                    edge(p[0], p[1], c),
                ];
                if w.iter().all(|&w| w * area >= 0.0) {
                    self.blend(x, y, color, semi);
                }
            }
        }
    }
    fn line(&mut self, a: [f32; 2], b: [f32; 2], color: [u8; 3]) {
        let steps = (b[0] - a[0]).abs().max((b[1] - a[1]).abs()).ceil().max(1.0) as usize;
        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            let (x, y) = (a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t);
            if x >= 0.0 && y >= 0.0 && (x as usize) < self.width && (y as usize) < self.height {
                let at = (y as usize * self.width + x as usize) * 3;
                for (c, &value) in self.rgb[at..at + 3].iter_mut().zip(&color) {
                    *c = c.saturating_add(value);
                }
            }
        }
    }
    fn save(&self, path: &Path) {
        fs::write(path, png_bytes(self.width, self.height, &self.rgb)).unwrap();
    }
}

fn scaled(points: &[[f32; 2]], scale: f32, origin: [f32; 2]) -> Vec<[f32; 2]> {
    points
        .iter()
        .map(|p| [(p[0] - origin[0]) * scale, (p[1] - origin[1]) * scale])
        .collect()
}
/// PlayStation polygon order: triangles (0, 1, 2) and (1, 2, 3); outline
/// 0-1-3-2.
fn fill(canvas: &mut Canvas, v: &[[f32; 2]], color: [u8; 3], semi: bool) {
    canvas.triangle([v[0], v[1], v[2]], color, semi);
    if v.len() == 4 {
        canvas.triangle([v[1], v[2], v[3]], color, semi);
    }
}
fn outline(canvas: &mut Canvas, v: &[[f32; 2]], color: [u8; 3]) {
    let order: &[usize] = if v.len() == 4 {
        &[0, 1, 3, 2]
    } else {
        &[0, 1, 2]
    };
    for (i, &a) in order.iter().enumerate() {
        canvas.line(v[a], v[order[(i + 1) % order.len()]], color);
    }
}

/// Flat-shaded fills from the integer and the sub-pixel positions at 4×,
/// their wireframes overlaid (integer red, sub-pixel green, both yellow),
/// and a 16× crop of the overlay.
fn write_demo(out: &Path, polygons: &[DemoPolygon], [width, height]: [usize; 2]) {
    let (w, h) = (
        (width as f32 * SCALE) as usize,
        (height as f32 * SCALE) as usize,
    );
    let mut integer = Canvas::new(w, h);
    let mut precise = Canvas::new(w, h);
    let mut overlay = Canvas::new(w, h);
    for polygon in polygons {
        let color = if polygon.color == [0, 0, 0] {
            [40, 40, 40]
        } else {
            polygon.color
        };
        fill(
            &mut integer,
            &scaled(&polygon.integer, SCALE, [0.0, 0.0]),
            color,
            polygon.semi,
        );
        fill(
            &mut precise,
            &scaled(&polygon.precise, SCALE, [0.0, 0.0]),
            color,
            polygon.semi,
        );
        if polygon.found {
            outline(
                &mut overlay,
                &scaled(&polygon.integer, SCALE, [0.0, 0.0]),
                [200, 0, 0],
            );
            outline(
                &mut overlay,
                &scaled(&polygon.precise, SCALE, [0.0, 0.0]),
                [0, 200, 0],
            );
        }
    }
    let shots = out.join("shots");
    fs::create_dir_all(&shots).unwrap();
    integer.save(&shots.join("flight-integer-4x.png"));
    precise.save(&shots.join("flight-subpixel-4x.png"));
    overlay.save(&shots.join("flight-overlay-4x.png"));
    // 16× crops of an 80 x 60 pixel window around the aircraft (the entity
    // models' centre, else the screen centre): the wireframes overlaid, and
    // the integer and sub-pixel fills side by side.
    let entity: Vec<[f32; 2]> = polygons
        .iter()
        .filter(|p| p.category == "entities")
        .flat_map(|p| p.integer.iter().copied())
        .collect();
    let centre = if entity.is_empty() {
        [width as f32 / 2.0, height as f32 / 2.0]
    } else {
        let n = entity.len() as f32;
        [
            entity.iter().map(|v| v[0]).sum::<f32>() / n,
            entity.iter().map(|v| v[1]).sum::<f32>() / n,
        ]
    };
    let origin = [(centre[0] - 40.0).round(), (centre[1] - 30.0).round()];
    let scale = 16.0;
    let mut crop = Canvas::new(80 * 16, 60 * 16);
    let mut fills = [Canvas::new(80 * 16, 60 * 16), Canvas::new(80 * 16, 60 * 16)];
    for polygon in polygons {
        let (integer, precise) = (
            scaled(&polygon.integer, scale, origin),
            scaled(&polygon.precise, scale, origin),
        );
        let color = if polygon.color == [0, 0, 0] {
            [40, 40, 40]
        } else {
            polygon.color
        };
        fill(&mut fills[0], &integer, color, polygon.semi);
        fill(&mut fills[1], &precise, color, polygon.semi);
        if polygon.found {
            outline(&mut crop, &integer, [200, 0, 0]);
            outline(&mut crop, &precise, [0, 200, 0]);
        }
    }
    crop.save(&shots.join("flight-overlay-16x-crop.png"));
    let mut side = Canvas::new(2 * 80 * 16 + 16, 60 * 16);
    for (half, canvas) in fills.iter().enumerate() {
        for y in 0..canvas.height {
            let row = &canvas.rgb[y * canvas.width * 3..(y + 1) * canvas.width * 3];
            let at = (y * side.width + half * (canvas.width + 16)) * 3;
            side.rgb[at..at + row.len()].copy_from_slice(row);
        }
    }
    side.save(&shots.join("flight-integer-vs-subpixel-16x.png"));
    let found = polygons.iter().filter(|p| p.found).count();
    eprintln!(
        "demo frame: {} polygons, {found} with sub-pixel positions, pictures in {}",
        polygons.len(),
        shots.display()
    );
}
