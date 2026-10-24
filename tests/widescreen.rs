//! Widescreen (`docs/MILESTONE_1.md` Phase 4 items 2 and 3) on the native
//! game: a cold boot of the disc image along the scripted pad path into the
//! first mission, run twice in lockstep, widescreen off and at 16:9.
//!
//! - [`gameplay_is_identical_with_widescreen`]: the flight (throttle up,
//!   lock-on, missiles, target switches, turns that sweep the targets across
//!   the screen edges) must give the same game logic every pass: every RAM
//!   word but the drawing data (the words of the GPU packets the frames
//!   submitted, the flight frame arenas: ordering tables and primitive
//!   buffers, the main loop's drawing buffers, the dead stack, the
//!   primitive cursor, the terrain frustum's outline minimum), the same
//!   audio and VBlanks. The scratchpad (the renderers' work area in flight)
//!   is only counted. It reports what differs where, and checks that it is
//!   not vacuous (targets on screen and crossing the edges, locks, wide
//!   pictures). [`gameplay_is_identical_at_21_9`] does the same at 21:9.
//! - [`golden_picture_at_16_9`]: the known headless run into flight (2700
//!   VBlanks) at 16:9 on the software renderer: the same audio as with
//!   widescreen off (`580973a9…`), the final picture shown at 16:9 and
//!   pinned, and both pictures written out.
//! - [`explore_differences`]: every RAM range that ever differs, with the
//!   first VBlank, for classifying them (`AC3_WIDESCREEN_EXPLORE=1`; pad
//!   schedule `AC3_WIDESCREEN_PAD`, length `AC3_WIDESCREEN_VBLANKS`).
//!
//! Output goes to `$AC3_WIDESCREEN_OUT` (default `target/widescreen`).
use ac3_flight::{Fault, Machine, widescreen::Aspect};
use ac3_graphics::Vram;
use ac3_port::assets::GameData;
use ac3_port::machine_card::{CardDevice, CardSource};
use ac3_port::machine_player::{Host, PadType, Player, parse_pad_schedule, shown_aspect};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::PathBuf;

/// The known headless run with widescreen off: final picture and PCM.
const OFF_PICTURE: &str = "72b41327";
const PCM: &str = "580973a9";
/// The same run at 16:9: its final picture (re-pin after a deliberate
/// change of the widescreen drawing).
const WIDE_PICTURE: &str = "82c0aa00";

const DISC: &str = "Ace Combat 3 - Electrosphere (Europe) (En,Fr,De,Es,It).bin";
/// The scripted pad path into flight (language, title, new game, hub,
/// briefing, aircraft select, launch).
const FLIGHT_PATH: &str = "400-406:cross,550-556:start,700-706:cross,850-856:cross,1400-1406:cross,1550-1556:cross,1700-1706:cross,1850-1856:cross,2000-2006:cross,2150-2156:cross,2300-2306:cross,2450-2456:cross";
/// In flight (from VBlank 2518): full throttle (R1); missiles (square) lock
/// on and fire; triangle switches targets; the turns sweep the targets
/// across both screen edges and out of view and back.
const FLIGHT: &str = "2600-5600:r1,2900-2906:square,3000-3006:square,3200-3206:triangle,3300-3306:square,3500-3506:square,3700-3900:left,4000-4006:triangle,4100-4300:right,4400-4406:square,4500-4700:left+down,4800-4806:triangle,4900-5100:right+up,5200-5206:square";
const VBLANKS: u64 = 5600;

/// `[gp+0x2D0]` the gameplay block (+0x14 primitive cursor), `[gp+0x2D4]`
/// the main loop's two drawing buffers (stride 0x1014, DrawOTag from +0x10).
const GAMEPLAY: u32 = ac3_flight::GP + 0x2d0;
const DRAW_BUFFERS: u32 = ac3_flight::GP + 0x2d4;
/// `gp+0x4FC/0x500`: the minimum x/z of the terrain frustum outline
/// (8005E140, read only by 8005FD68 for the terrain drawing). Widescreen
/// widens the frustum, so they differ.
const FRUSTUM_MINIMUM: [u32; 2] = [ac3_flight::GP + 0x4fc, ac3_flight::GP + 0x500];
/// The guest stack (dead between passes): `STACK_LIMIT..INITIAL_SP`.
const STACK: std::ops::Range<u32> = 0x801f_7000..0x801f_fff0;
/// 800C4B36: the locked target slot (0xFF: none).
const LOCKED_SLOT: u32 = 0x800c_4b36;
/// 800C4C98: the target record pointers of the 64 entity slots (8003ABAC:
/// +0x20 visibility flags, +0x24 frames on screen).
const TARGET_RECORDS: u32 = 0x800c_4c98;

struct Game {
    root: PathBuf,
    data: GameData,
    out: PathBuf,
}

fn game() -> Game {
    let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR"));
    let data = GameData::load(&root.join("extracted/disc")).unwrap();
    let out = std::env::var_os("AC3_WIDESCREEN_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/widescreen"));
    fs::create_dir_all(&out).unwrap();
    Game { root, data, out }
}

/// A player booted from the disc image as `machine_player::run` does, with
/// widescreen at `aspect`.
fn boot(game: &Game, schedule: &str, aspect: Option<Aspect>) -> Player {
    let disc = File::open(game.root.join(DISC)).unwrap();
    let mut host = Host::new(Vram::new(), Some(disc));
    host.card = CardDevice::from_sources(&[CardSource::Blank, CardSource::Blank]).unwrap();
    host.pad = PadType::Digital.ports();
    let mut player = Player::new(Machine::default(), host);
    player.machine.set_widescreen(aspect);
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

fn step(player: &mut Player) -> Vec<[i16; 2]> {
    let result: Result<(), Fault> = player.iteration();
    if let Err(fault) = result {
        panic!("VBlank {}: {fault}", player.vblanks());
    }
    let pcm = player.host.borrow_mut().take_audio();
    if let Some(error) = player.host.borrow().error() {
        panic!("VBlank {}: device: {error}", player.vblanks());
    }
    pcm
}

fn ram_word(ram: &[u8], address: u32) -> u32 {
    let at = (address & 0x1f_fffc) as usize;
    u32::from_le_bytes(ram[at..at + 4].try_into().unwrap())
}

/// Resident state and substate.
fn state(ram: &[u8]) -> (u32, u32) {
    let context = ram_word(ram, ac3_flight::GP + 0x2dc);
    (ram_word(ram, context + 0x2c), ram_word(ram, context + 0x30))
}

/// Targets inside the 4:3 screen (target record +0x20 flag 0x20000) and
/// with a valid projection (0x40000), over slots 2..63.
fn targets(ram: &[u8]) -> (u32, u32) {
    let (mut on_screen, mut projected) = (0, 0);
    for slot in 2..64 {
        let record = ram_word(ram, TARGET_RECORDS + slot * 4);
        if record & 0xff00_0000 != 0x8000_0000 {
            continue;
        }
        let flags = ram_word(ram, record + 0x20);
        on_screen += u32::from(flags & 0x2_0000 != 0);
        projected += u32::from(flags & 0x4_0000 != 0);
    }
    (on_screen, projected)
}

/// One bit per RAM word.
struct Words(Vec<u64>);
impl Words {
    fn new() -> Self {
        Self(vec![0; 0x20_0000 / 4 / 64])
    }
    fn index(address: u32) -> usize {
        ((address & 0x1f_ffff) >> 2) as usize
    }
    fn set(&mut self, address: u32) {
        let i = Self::index(address);
        self.0[i / 64] |= 1 << (i % 64);
    }
    fn get(&self, address: u32) -> bool {
        let i = Self::index(address);
        self.0[i / 64] >> (i % 64) & 1 != 0
    }
}

/// Mark every word of every GPU packet the two main-loop drawing buffers'
/// lists hold (DrawOTag from buffer +0x10: the main ordering table, the
/// flight one chained in, every primitive): the drawing data of this pass
/// and the last.
fn mark_packets(ram: &[u8], words: &mut Words) {
    let buffers = ram_word(ram, DRAW_BUFFERS);
    for index in 0..2u32 {
        let mut address = (buffers + index * 0x1014 + 0x10) & 0x00ff_ffff;
        for _ in 0..0x4_0000 {
            if address == 0x00ff_ffff || address >= 0x20_0000 {
                break;
            }
            let tag = ram_word(ram, address);
            for word in 0..=tag >> 24 {
                words.set(address + word * 4);
            }
            address = tag & 0x00ff_ffff;
        }
    }
}

/// Whether a differing RAM word is drawing data (see the module docs): a
/// word of a GPU packet the frames submitted, of the flight frame arenas
/// (the ordering tables gameplay +0x18/+0x1C, 0x4000 bytes, and the
/// primitive buffers +0x20/+0x24, 0x1B800 bytes, which also hold culled and
/// work packets), of the main loop's drawing buffers, the dead stack, the
/// primitive cursor or the terrain frustum minimum.
fn drawing_data(ram: &[u8], packets: &Words, address: u32) -> bool {
    let gameplay = ram_word(ram, GAMEPLAY);
    let arena = |offset: u32, bytes: u32| {
        let start = ram_word(ram, gameplay + offset);
        (start..start.wrapping_add(bytes)).contains(&address)
    };
    let buffers = ram_word(ram, DRAW_BUFFERS);
    packets.get(address)
        || arena(0x18, 0x4000)
        || arena(0x1c, 0x4000)
        || arena(0x20, 0x1_b800)
        || arena(0x24, 0x1_b800)
        || (buffers..buffers + 2 * 0x1014).contains(&address)
        || STACK.contains(&address)
        || address == gameplay + 0x14
        || FRUSTUM_MINIMUM.contains(&address)
}

/// A RAM word that differed outside the drawing data.
struct Difference {
    address: u32,
    first_vblank: u64,
    state: (u32, u32),
    off: u32,
    on: u32,
    passes: u64,
}

#[test]
#[ignore = "requires AC3_DATA_DIR"]
fn gameplay_is_identical_with_widescreen() {
    compare_gameplay(Aspect::Wide16x9);
}

/// The same at 21:9, the widest squeeze (k = 9/16).
#[test]
#[ignore = "requires AC3_DATA_DIR"]
fn gameplay_is_identical_at_21_9() {
    compare_gameplay(Aspect::Wide21x9);
}

/// The lockstep comparison of [`gameplay_is_identical_with_widescreen`].
fn compare_gameplay(aspect: Aspect) {
    let game = game();
    let schedule = format!("{FLIGHT_PATH},{FLIGHT}");
    let mut off = boot(&game, &schedule, None);
    let mut on = boot(&game, &schedule, Some(aspect));
    let mut packets = Words::new();
    let mut logic: BTreeMap<u32, Difference> = BTreeMap::new();
    let mut drawing_words = 0u64;
    let mut scratch_words = 0u64;
    let (mut flight, mut on_screen, mut edge, mut locked, mut wide) =
        (0u64, 0u64, 0u64, 0u64, 0u64);
    let mut locks = std::collections::BTreeSet::new();
    let mut previous_state = (0, 0);
    let mut previous_on_screen = 0;
    while off.vblanks() < VBLANKS {
        let pcm_off = step(&mut off);
        let pcm_on = step(&mut on);
        let vblank = off.vblanks();
        assert_eq!(vblank, on.vblanks(), "VBlank counts");
        assert_eq!(pcm_off, pcm_on, "audio at VBlank {vblank}");
        let (a, b) = (off.machine.ram(), on.machine.ram());
        // Drawing data of this run: reset when the resident state changes
        // (a new mission or state may put logic where packets were).
        let current = state(a);
        if current.0 != previous_state.0 {
            packets = Words::new();
        }
        previous_state = current;
        mark_packets(a, &mut packets);
        mark_packets(b, &mut packets);
        if a != b {
            for (i, (x, y)) in a.chunks_exact(4).zip(b.chunks_exact(4)).enumerate() {
                if x == y {
                    continue;
                }
                let address = 0x8000_0000 | (i as u32 * 4);
                if drawing_data(a, &packets, address) {
                    drawing_words += 1;
                    continue;
                }
                let difference = logic.entry(address).or_insert(Difference {
                    address,
                    first_vblank: vblank,
                    state: current,
                    off: u32::from_le_bytes(x.try_into().unwrap()),
                    on: u32::from_le_bytes(y.try_into().unwrap()),
                    passes: 0,
                });
                difference.passes += 1;
            }
        }
        // The scratchpad is the renderers' work area in flight; count it.
        scratch_words += off
            .machine
            .scratch()
            .chunks_exact(4)
            .zip(on.machine.scratch().chunks_exact(4))
            .filter(|(x, y)| x != y)
            .count() as u64;
        if current == (2, 7) {
            flight += 1;
            let (count, _) = targets(a);
            on_screen += u64::from(count > 0);
            // A target crossed the 4:3 edge (entered or left the screen).
            edge += u64::from(count != previous_on_screen);
            previous_on_screen = count;
            let slot = a[(LOCKED_SLOT & 0x1f_ffff) as usize];
            if slot != 0xff {
                locked += 1;
                locks.insert(slot);
            }
            wide += u64::from(shown_aspect(&on) == Some(aspect));
            assert_eq!(shown_aspect(&off), None, "4:3 with widescreen off");
        }
    }
    let mut text = format!(
        "{VBLANKS} VBlanks in lockstep, widescreen off and {name}; {flight} flight passes: \
         targets on screen in {on_screen}, {edge} changes of the on-screen count, a lock in \
         {locked} (slots {locks:?}), {wide} pictures shown at {name}.\n\
         Drawing data that differed: {drawing_words} RAM words over all passes (GPU packets, \
         dead stack, primitive cursor, terrain frustum minimum); scratchpad work area: \
         {scratch_words} words.\n\
         Game logic that differed: {} words.\n",
        logic.len(),
        name = aspect.name()
    );
    for difference in logic.values().take(64) {
        text += &format!(
            "  {:#010x}: off {:#010x}, on {:#010x}; first at VBlank {} in state {:?}, {} passes\n",
            difference.address,
            difference.off,
            difference.on,
            difference.first_vblank,
            difference.state,
            difference.passes
        );
    }
    eprint!("{text}");
    let file = format!("gameplay-{}.txt", aspect.name().replace(':', "x"));
    fs::write(game.out.join(file), &text).unwrap();
    assert!(logic.is_empty(), "game logic differs:\n{text}");
    // Not vacuous: the flight saw targets, a lock, targets crossing the
    // screen edges, and the widescreen picture; drawing data differed.
    assert!(flight > 1_000, "{flight} flight passes");
    assert!(on_screen > 500 && edge > 10, "{on_screen} {edge}");
    assert!(locked > 200 && locks.len() >= 2, "{locked} {locks:?}");
    assert!(wide * 10 > flight * 9, "{wide} of {flight}");
    assert!(drawing_words > 100_000, "{drawing_words}");
}

/// The displayed picture as `machine_player::run` saves `final.png`.
fn final_picture(player: &Player) -> Vec<u8> {
    let host = player.host.borrow();
    let [_, _, w, h] = host.gpu.display;
    let (w, h) = (w.max(1) as u32, h.max(1) as u32);
    let pixels = host.gpu.displayed().unwrap();
    let rgb: Vec<u8> = pixels
        .iter()
        .flat_map(|&p| {
            let [r, g, b, _] = ac3_graphics::rgba8(p);
            [r, g, b]
        })
        .collect();
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, w, h);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .unwrap()
        .write_image_data(&rgb)
        .unwrap();
    bytes
}

#[test]
#[ignore = "requires AC3_DATA_DIR"]
fn golden_picture_at_16_9() {
    let game = game();
    let mut pictures = Vec::new();
    for aspect in [None, Some(Aspect::Wide16x9)] {
        let mut player = boot(&game, FLIGHT_PATH, aspect);
        let mut pcm = Sha256::new();
        while player.vblanks() < 2700 {
            let audio = step(&mut player);
            pcm.update(
                audio
                    .iter()
                    .flatten()
                    .flat_map(|v| v.to_le_bytes())
                    .collect::<Vec<u8>>(),
            );
        }
        let pcm = format!("{:x}", pcm.finalize());
        assert!(pcm.starts_with(PCM), "{aspect:?}: audio {pcm}");
        assert_eq!(state(player.machine.ram()), (2, 7), "in flight");
        assert_eq!(shown_aspect(&player), aspect, "the picture's aspect");
        let picture = final_picture(&player);
        let name = match aspect {
            None => "final-off.png",
            Some(_) => "final-16x9.png",
        };
        fs::write(game.out.join(name), &picture).unwrap();
        pictures.push(format!("{:x}", Sha256::digest(&picture)));
    }
    eprintln!("final pictures: off {}, 16:9 {}", pictures[0], pictures[1]);
    assert!(pictures[0].starts_with(OFF_PICTURE), "{}", pictures[0]);
    assert!(pictures[1].starts_with(WIDE_PICTURE), "{}", pictures[1]);
}

#[test]
#[ignore = "requires AC3_DATA_DIR"]
fn explore_differences() {
    if std::env::var_os("AC3_WIDESCREEN_EXPLORE").is_none() {
        return;
    }
    let game = game();
    let schedule =
        std::env::var("AC3_WIDESCREEN_PAD").unwrap_or_else(|_| format!("{FLIGHT_PATH},{FLIGHT}"));
    let until: u64 = std::env::var("AC3_WIDESCREEN_VBLANKS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(VBLANKS);
    let mut off = boot(&game, &schedule, None);
    let mut on = boot(&game, &schedule, Some(Aspect::Wide16x9));
    // Word address -> (first VBlank, count of passes it differed).
    let mut seen: BTreeMap<u32, (u64, u64)> = BTreeMap::new();
    let mut scratch: BTreeMap<u32, (u64, u64)> = BTreeMap::new();
    let mut next_log = 0;
    while off.vblanks() < until {
        let a = step(&mut off);
        let b = step(&mut on);
        let vblank = off.vblanks();
        if vblank >= next_log {
            next_log = vblank + 50;
            let ram = off.machine.ram();
            let (on_screen, in_view) = targets(ram);
            eprintln!(
                "VBlank {vblank}: state {:?}, locked slot {:#x}, targets on screen {on_screen}, projected {in_view}",
                state(ram),
                ram[(LOCKED_SLOT & 0x1f_ffff) as usize]
            );
        }
        assert_eq!(vblank, on.vblanks(), "VBlank counts");
        assert_eq!(a, b, "PCM at VBlank {vblank}");
        let (ram_a, ram_b) = (off.machine.ram(), on.machine.ram());
        for (i, (x, y)) in ram_a.chunks(4).zip(ram_b.chunks(4)).enumerate() {
            if x != y {
                let entry = seen
                    .entry(0x8000_0000 | (i as u32 * 4))
                    .or_insert((vblank, 0));
                entry.1 += 1;
            }
        }
        for (i, (x, y)) in off
            .machine
            .scratch()
            .chunks(4)
            .zip(on.machine.scratch().chunks(4))
            .enumerate()
        {
            if x != y {
                let entry = scratch
                    .entry(0x1f80_0000 | (i as u32 * 4))
                    .or_insert((vblank, 0));
                entry.1 += 1;
            }
        }
    }
    // Merge into ranges (gaps under 64 bytes join).
    let mut ranges: Vec<(u32, u32, u64, u64)> = Vec::new();
    for (&address, &(first, count)) in &seen {
        match ranges.last_mut() {
            Some((_, end, f, c)) if address <= *end + 64 => {
                *end = address + 4;
                *f = (*f).min(first);
                *c = (*c).max(count);
            }
            _ => ranges.push((address, address + 4, first, count)),
        }
    }
    let ram = off.machine.ram();
    let gameplay = ram_word(ram, GAMEPLAY);
    let mut text = format!("gameplay {gameplay:#010x} state {:?}\n", state(ram));
    for offset in [0x18u32, 0x1c, 0x20, 0x24] {
        text += &format!(
            "  gameplay +{offset:#x} = {:#010x}\n",
            ram_word(ram, gameplay + offset)
        );
    }
    for gp in [0x2d4u32, 0x40c, 0x46c, 0x470, 0x4f4] {
        text += &format!(
            "  [gp+{gp:#x}] = {:#010x}\n",
            ram_word(ram, ac3_flight::GP + gp)
        );
    }
    for (start, end, first, count) in &ranges {
        text += &format!(
            "{start:#010x}..{end:#010x} ({:6} bytes) first VBlank {first}, up to {count} passes\n",
            end - start
        );
    }
    text += &format!("scratchpad: {} words differed\n", scratch.len());
    eprint!("{text}");
    fs::write(game.out.join("differences.txt"), &text).unwrap();
}
