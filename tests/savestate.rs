//! Quick save and quick load (`src/savestate.rs`) on the native machine,
//! from a cold boot of the disc image with the player built as
//! `machine_player::run` builds it (two blank memory cards, a digital pad,
//! a scripted pad schedule).
//!
//! At each point, player A saves a state file at the first main-loop
//! boundary at or after VBlank N and runs on to N + 300, recording RAM,
//! scratchpad, GTE, VRAM, sound RAM and the mixed PCM of the window. A
//! separately booted player B loads the file and runs to N + 300: every
//! hash must be the same, and so must the whole state (every section of a
//! capture) right after the load and at the end. Then A itself, far ahead,
//! restores the same moment from memory and replays the window again.
//!
//! Points: the language screen (VBlank 400, as CROSS is pressed), the hub
//! with its XA music (1750), flight loading (2440) and flight (2650) on the
//! scripted path into flight; the attract movie (2000, MDEC and XA
//! streaming) on a path that lets the title wait. Plus: a save refused
//! while the boot's memory-card check is in flight, and files of another
//! format, version or game refused without touching the player.
//!
//! State files go to `$AC3_SAVESTATE_OUT` (default `target/savestate`).
use ac3_flight::Machine;
use ac3_graphics::Vram;
use ac3_port::assets::GameData;
use ac3_port::machine_card::{CardDevice, CardSource};
use ac3_port::machine_player::{Host, PadType, Player, parse_pad_schedule};
use ac3_port::savestate::{self, Error, GameId, State};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::path::PathBuf;
use std::time::Instant;

const DISC: &str = "Ace Combat 3 - Electrosphere (Europe) (En,Fr,De,Es,It).bin";
/// The scripted pad path into flight (language, title, new game, hub,
/// briefing, aircraft select, launch).
const FLIGHT_PATH: &str = "400-406:cross,550-556:start,700-706:cross,850-856:cross,1400-1406:cross,1550-1556:cross,1700-1706:cross,1850-1856:cross,2000-2006:cross,2150-2156:cross,2300-2306:cross,2450-2456:cross";
/// The language screen only: the title waits and the attract movie starts
/// (state 5 from VBlank 1458).
const MOVIE_PATH: &str = "400-406:cross";
/// VBlanks run after each save.
const WINDOW: u64 = 300;
/// `[gp+0x2DC]`: the resident context (state +0x2C, substate +0x30).
const CONTEXT: u32 = ac3_flight::GP + 0x2dc;

struct Game {
    root: PathBuf,
    data: GameData,
    id: GameId,
    out: PathBuf,
}

fn game() -> Game {
    let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR"));
    let data = GameData::load(&root.join("extracted/disc")).unwrap();
    let id = GameId::of(&data.executable.payload);
    let out = std::env::var_os("AC3_SAVESTATE_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/savestate"));
    fs::create_dir_all(&out).unwrap();
    Game {
        root,
        data,
        id,
        out,
    }
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

/// One main-loop pass as `run` makes it; returns the PCM it mixed.
fn step(player: &mut Player) -> Vec<[i16; 2]> {
    if let Err(fault) = player.iteration() {
        panic!("VBlank {}: {fault}", player.vblanks());
    }
    let pcm = player.host.borrow_mut().take_audio();
    if let Some(error) = player.host.borrow().error() {
        panic!("VBlank {}: device: {error}", player.vblanks());
    }
    pcm
}

fn hex(bytes: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// (state, substate) of the resident main loop.
fn game_state(player: &mut Player) -> (u32, u32) {
    let context = player.machine.lw(CONTEXT);
    (
        player.machine.lw(context + 0x2c),
        player.machine.lw(context + 0x30),
    )
}

/// What one window produced.
#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    vblanks: u64,
    passes: u64,
    state: (u32, u32),
    ram: String,
    scratch: String,
    gte: String,
    vram: String,
    sound_ram: String,
    pcm: String,
    pcm_frames: usize,
}

/// A window being recorded: the PCM since the save, and the passes.
struct Window {
    end: u64,
    pcm: Sha256,
    frames: usize,
    passes: u64,
}

impl Window {
    fn new(start: u64) -> Self {
        Self {
            end: start + WINDOW,
            pcm: Sha256::new(),
            frames: 0,
            passes: 0,
        }
    }
    fn feed(&mut self, pcm: &[[i16; 2]]) {
        for frame in pcm {
            self.pcm.update(frame[0].to_le_bytes());
            self.pcm.update(frame[1].to_le_bytes());
        }
        self.frames += pcm.len();
        self.passes += 1;
    }
    fn outcome(self, player: &mut Player) -> Outcome {
        let state = game_state(player);
        let host = player.host.borrow();
        let vram: Vec<u8> = host
            .gpu
            .gpu
            .vram()
            .words()
            .iter()
            .flat_map(|w| w.to_le_bytes())
            .collect();
        Outcome {
            vblanks: host.vblanks,
            passes: self.passes,
            state,
            ram: hex(player.machine.ram()),
            scratch: hex(player.machine.scratch()),
            gte: hex(format!("{:?}", player.machine.gte)),
            vram: hex(vram),
            sound_ram: hex(host.spu.sound_ram()),
            pcm: format!("{:x}", self.pcm.finalize()),
            pcm_frames: self.frames,
        }
    }
}

/// Run `player` to the end of `window`, recording it.
fn run_window(player: &mut Player, mut window: Window) -> Outcome {
    while player.vblanks() < window.end {
        let pcm = step(player);
        window.feed(&pcm);
    }
    window.outcome(player)
}

/// One save point: the file, the same moment in memory, and what A did.
struct Saved {
    name: &'static str,
    path: PathBuf,
    state: State,
    vblanks: u64,
    outcome: Option<Outcome>,
    end_state: Option<State>,
}

/// Player A on `schedule`: save at each point and record the window after
/// it. `check` sees the player at each save.
fn record(
    game: &Game,
    schedule: &str,
    points: &[(&'static str, u64)],
    mut check: impl FnMut(&str, &mut Player),
) -> (Player, Vec<Saved>) {
    let mut a = boot(game, schedule);
    let mut saved: Vec<Saved> = Vec::new();
    let mut windows: Vec<Option<Window>> = Vec::new();
    loop {
        for (i, (name, at)) in points.iter().enumerate() {
            if i == saved.len() && a.vblanks() >= *at {
                check(name, &mut a);
                let path = game.out.join(format!("{name}.ac3state"));
                let started = Instant::now();
                let summary = savestate::save(&a, game.id, &path).unwrap();
                let save_time = started.elapsed();
                let state = savestate::capture(&a, game.id).unwrap();
                println!(
                    "{name}: saved at VBlank {} (state {:?}): file {} bytes, payload {} bytes, save {:.1} ms",
                    summary.vblanks,
                    game_state(&mut a),
                    summary.file_bytes,
                    summary.payload_bytes,
                    save_time.as_secs_f64() * 1e3,
                );
                saved.push(Saved {
                    name,
                    path,
                    state,
                    vblanks: summary.vblanks,
                    outcome: None,
                    end_state: None,
                });
                windows.push(Some(Window::new(summary.vblanks)));
            }
        }
        if saved.len() == points.len() && windows.iter().all(Option::is_none) {
            return (a, saved);
        }
        let pcm = step(&mut a);
        for (i, slot) in windows.iter_mut().enumerate() {
            let Some(window) = slot else { continue };
            window.feed(&pcm);
            if a.vblanks() >= window.end {
                let window = slot.take().unwrap();
                saved[i].outcome = Some(window.outcome(&mut a));
                saved[i].end_state = Some(savestate::capture(&a, game.id).unwrap());
            }
        }
    }
}

/// Every saved point replayed by a freshly booted player B from its file,
/// and by A from memory.
fn replay(game: &Game, schedule: &str, a: &mut Player, saved: &[Saved]) {
    for point in saved {
        let expected = point.outcome.as_ref().unwrap();
        let end_state = point.end_state.as_ref().unwrap();
        let mut b = boot(game, schedule);
        let started = Instant::now();
        let summary = savestate::load(&mut b, game.id, &point.path).unwrap();
        let load_time = started.elapsed();
        assert_eq!(summary.vblanks, point.vblanks);
        assert_eq!(b.vblanks(), point.vblanks);
        let loaded = savestate::capture(&b, game.id).unwrap();
        assert!(
            loaded == point.state,
            "{}: the loaded player differs in {:?}",
            point.name,
            loaded.differing_sections(&point.state)
        );
        let outcome = run_window(&mut b, Window::new(point.vblanks));
        println!(
            "{}: loaded in {:.1} ms; B at VBlank {} after {} passes, {} PCM frames, state {:?}",
            point.name,
            load_time.as_secs_f64() * 1e3,
            outcome.vblanks,
            outcome.passes,
            outcome.pcm_frames,
            outcome.state
        );
        assert_eq!(&outcome, expected, "{}: B after the load", point.name);
        let end = savestate::capture(&b, game.id).unwrap();
        assert!(
            &end == end_state,
            "{}: B's state at the end differs in {:?}",
            point.name,
            end.differing_sections(end_state)
        );

        // A, which ran on, rewinds to the same moment from memory.
        savestate::restore(a, &point.state, game.id).unwrap();
        let outcome = run_window(a, Window::new(point.vblanks));
        assert_eq!(&outcome, expected, "{}: A after rewinding", point.name);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR"]
fn quick_load_replays_menus_hub_and_flight_exactly() {
    let game = game();
    let points = [
        ("language", 400),
        ("hub", 1750),
        ("flight-loading", 2440),
        ("flight", 2650),
    ];
    let mut busy = Vec::new();
    let (mut a, saved) = record(&game, FLIGHT_PATH, &points, |name, player| {
        let expected = match name {
            "language" => 4,
            "hub" => 3,
            _ => 2,
        };
        assert_eq!(game_state(player).0, expected, "{name}: game state");
    });
    // The scripted path again, for the boundaries where the memory card
    // is busy (the boot's card check): there a save is refused and the
    // file left alone.
    let mut p = boot(&game, FLIGHT_PATH);
    let path = game.out.join("busy.ac3state");
    let _ = fs::remove_file(&path);
    while p.vblanks() < 400 {
        step(&mut p);
        if p.host.borrow().card.busy() {
            busy.push(p.vblanks());
            assert!(matches!(
                savestate::save(&p, game.id, &path),
                Err(Error::CardBusy)
            ));
        }
    }
    assert!(!path.exists(), "a refused save writes nothing");
    println!(
        "memory card busy at {} boundaries before VBlank 400: VBlanks {}..={}",
        busy.len(),
        busy.first().unwrap(),
        busy.last().unwrap()
    );
    assert!(!busy.is_empty(), "the boot's card check");
    replay(&game, FLIGHT_PATH, &mut a, &saved);
}

#[test]
#[ignore = "requires AC3_DATA_DIR"]
fn quick_load_replays_the_attract_movie_exactly() {
    let game = game();
    let mut streaming = (0, 0);
    let (mut a, saved) = record(&game, MOVIE_PATH, &[("movie", 2000)], |_, player| {
        assert_eq!(game_state(player).0, 5, "the attract movie");
        let host = player.host.borrow();
        streaming = (host.mdec.macroblocks, host.xa_player.sectors);
        assert!(host.spu.cd_audio_queued() > 0, "XA audio queued in the SPU");
    });
    let host = a.host.borrow();
    println!(
        "movie: MDEC macroblocks {} -> {}, XA sectors {} -> {}",
        streaming.0, host.mdec.macroblocks, streaming.1, host.xa_player.sectors
    );
    assert!(streaming.0 > 0 && host.mdec.macroblocks > streaming.0 + 10_000);
    assert!(streaming.1 > 0 && host.xa_player.sectors > streaming.1 + 50);
    drop(host);
    replay(&game, MOVIE_PATH, &mut a, &saved);
}

#[test]
#[ignore = "requires AC3_DATA_DIR"]
fn quick_load_refuses_other_files() {
    let game = game();
    let mut a = boot(&game, FLIGHT_PATH);
    while a.vblanks() < 400 {
        step(&mut a);
    }
    let path = game.out.join("refusals.ac3state");
    savestate::save(&a, game.id, &path).unwrap();
    let bytes = fs::read(&path).unwrap();

    let mut b = boot(&game, FLIGHT_PATH);
    let before = savestate::capture(&b, game.id).unwrap();
    let try_load = |b: &mut Player, name: &str, bytes: &[u8]| {
        let path = game.out.join(name);
        fs::write(&path, bytes).unwrap();
        savestate::load(b, game.id, &path)
    };
    let mut wrong = bytes.clone();
    wrong[..8].copy_from_slice(b"NOTSTATE");
    let refused = try_load(&mut b, "magic.ac3state", &wrong).unwrap_err();
    println!("wrong magic: {refused}");
    assert!(matches!(refused, Error::NotAState));

    let mut wrong = bytes.clone();
    wrong[8..12].copy_from_slice(&(savestate::FORMAT_VERSION + 1).to_le_bytes());
    let refused = try_load(&mut b, "version.ac3state", &wrong).unwrap_err();
    println!("wrong version: {refused}");
    assert!(matches!(refused, Error::Version { found, supported }
        if found == savestate::FORMAT_VERSION + 1 && supported == savestate::FORMAT_VERSION));

    let mut wrong = bytes.clone();
    let other = GameId::of(b"another resident payload");
    wrong[12..44].copy_from_slice(&other.0);
    let refused = try_load(&mut b, "game.ac3state", &wrong).unwrap_err();
    println!("wrong game: {refused}");
    assert!(matches!(refused, Error::WrongGame { found, expected }
        if found == other && expected == game.id));
    let state = State::from_bytes(&bytes, game.id).unwrap();
    assert!(matches!(
        savestate::restore(&mut b, &state, other),
        Err(Error::WrongGame { .. })
    ));

    let refused = try_load(&mut b, "short.ac3state", &bytes[..1000]).unwrap_err();
    println!("truncated: {refused}");
    assert!(matches!(refused, Error::Corrupt(_)));
    let missing = savestate::load(&mut b, game.id, &game.out.join("missing.ac3state"));
    assert!(matches!(missing, Err(Error::Io(_))));

    assert!(
        savestate::capture(&b, game.id).unwrap() == before,
        "refused loads leave the player alone"
    );
    // The good file still loads.
    savestate::load(&mut b, game.id, &path).unwrap();
    assert_eq!(b.vblanks(), a.vblanks());
}

/// A state taken twice at one moment encodes to the same bytes.
#[test]
#[ignore = "requires AC3_DATA_DIR"]
fn quick_save_is_deterministic() {
    let game = game();
    let mut a = boot(&game, FLIGHT_PATH);
    while a.vblanks() < 2650 {
        step(&mut a);
    }
    let first = savestate::capture(&a, game.id).unwrap().to_bytes().unwrap();
    let second = savestate::capture(&a, game.id).unwrap().to_bytes().unwrap();
    assert_eq!(hex(&first), hex(&second));
    let state = savestate::capture(&a, game.id).unwrap();
    let sizes: Vec<String> = state
        .sections()
        .map(|(name, bytes)| format!("{name} {bytes}"))
        .collect();
    println!("flight state sections: {}", sizes.join(", "));
}
