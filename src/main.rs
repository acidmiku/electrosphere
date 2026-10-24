use ac3_port::{
    Result,
    assets::GameData,
    bootstrap::Bootstrap,
    machine_card::CardSource,
    settings::{self, Settings},
    validation,
};
use serde_json::json;
use std::{env, fs, path::PathBuf, process::ExitCode};

/// The shipped `ac3` binary never contains the development interpreter
/// fallback (the project's "no interpreter" rule): a build that would link
/// it into either translation crate, whatever enabled the feature, fails to
/// compile here. `ac3-dev` (`src/bin/ac3_dev.rs`) is this command line
/// built with it.
const _: () = assert!(
    !(ac3_flight::DEV_FALLBACK || ac3_flight_jp::DEV_FALLBACK)
        || same_name(env!("CARGO_BIN_NAME"), "ac3-dev"),
    "the dev-fallback interpreter must never be in the shipped ac3 binary: build `--bin ac3-dev`"
);
const fn same_name(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

const HELP: &str = "Ace Combat 3 (PAL and Japanese releases) — native Rust reconstruction\n\
Usage: ac3 [launcher|machine|settings|gamepad|dump-assets|export-movies|bootstrap|inspect|verify|menu] [options]\n\
  With no arguments, ac3 opens the launcher. Options without a command run bootstrap.\n\
launcher: a settings window for the settings file (display, controls, memory cards and\n\
  quick save). Save keeps the file's comments; Play saves and starts\n\
  `ac3 machine --settings FILE` (with --data and --disc when given). Enter plays, Esc quits.\n\
  --settings, --data and --disc apply.\n\
  --data PATH       Extracted disc directory (default: extracted/disc)\n\
  --module 1..5     Bootstrap module; 1 is normal startup, 2..5 are probes\n\
  --captures PATH   Redux captures directory (default: analysis/runtime)\n\
  --oracle PATH     Dispatcher oracle JSON (default: analysis/rust-oracle/state-vectors.json)\n\
  --report PATH     Save JSON result (in addition to console output)\n\
  --headless        Run native menu without a window\n\
  --frames N        Stop the menu after N frames (headless default 120)\n\
  --inputs LIST     Frame-indexed keys, e.g. 40:down,70:confirm\n\
  --output PATH     Save native menu PNG, raw pixels and execution trace\n\
machine options (translated resident code on the address-preserving machine):\n\
  --disc PATH       Raw disc image or cue sheet to boot: the PAL disc, or a Japanese disc\n\
                    (default: [paths] disc, else the PAL .bin in the working directory).\n\
                    The release is told from the disc; a Japanese disc's game files are\n\
                    read from the disc itself unless --data names extracted Japanese files\n\
  --disc2 PATH      The Japanese release's other disc (default: [paths] disc2, else the\n\
                    image next to --disc named with (Disc 2) / (Disc 1)). Both discs are\n\
                    mounted; when the game asks for the other disc it is changed for you\n\
  --vblanks N       Stop after N VBlanks (headless default 3000)\n\
  --pad LIST        Held buttons by VBlank, e.g. 300-304:cross,500-560:up (with the keys in a window)\n\
  --pad-type TYPE   Port 1 controller: auto (default: a DualShock when a gamepad is\n\
                    connected at start, else digital), digital or dualshock\n\
  --every N         Headless: save the displayed picture every N VBlanks (default 50)\n\
  --card1 PATH      Memory card 1: a raw 128 KiB .mcr image, created on the first save\n\
                    when missing; none: no card (default: a blank card in memory)\n\
  --card2 PATH      Memory card 2, likewise\n\
  --settings PATH   Settings file (default: ac3.toml next to the executable)\n\
  --window WxH      Window size, e.g. 1280x960\n\
  --mode MODE       windowed or borderless (fullscreen)\n\
  --scaling MODE    aspect (4:3), stretch or integer\n\
  --filter MODE     nearest or linear\n\
  --vsync on|off    Present in step with the monitor (game speed never depends on it)\n\
  --internal-res N  Internal resolution 1..8: above 1 the GPU renderer draws the game\n\
                    at N times the PlayStation's resolution on the graphics card\n\
  --renderer NAME   auto (software at 1x, GPU above), software, or gpu (1x too)\n\
  --upscaled-dither on|off  Dither shaded surfaces above 1x (off by default)\n\
  --subpixel on|off Sub-pixel 3D geometry above 1x, no vertex wobble (on by default)\n\
  --perspective on|off  Perspective-correct textures above 1x with sub-pixel geometry\n\
                    (on by default)\n\
  --texture-pack DIR  Draw the PNGs in DIR (named by replacement key, as --dump-textures\n\
                    writes them) instead of the game's textures; needs the GPU renderer.\n\
                    none: the game's own textures (default: [display] texture_pack,\n\
                    a textures folder next to the settings file). F9 reloads it.\n\
  --shader-preset FILE  Draw the picture through this RetroArch slang shader preset
                    (.slangp); none: the plain scaler (default: [display] shader_preset).
                    F2 toggles it, Shift+F2 loads it again. See docs/LIBRASHADER.md\n\
  --movie-pack DIR  Show the images in DIR (one folder per movie key, frames numbered as\n\
                    export-movies writes them) in place of the movies' frames, on the\n\
                    picture only; none: the game's frames (default: [paths] movie_pack).\n\
                    The hub's clips need the GPU renderer above 1x. F9 reloads it. See\n\
                    docs/MOVIES.md\n\
  --widescreen MODE off (default), 16:9, 16:10 or 21:9: flight shows a wider view at that\n\
                    shape, the HUD a centred 4:3 block; menus, hub and movies stay 4:3.\n\
                    Headless pictures stay 320 wide; the report gives each one's aspect\n\
  --effects on|off  Flight visual effects (Japanese release) with the GPU renderer above 1x\n\
                    (off by default; F6 toggles in the window)\n\
  --fx-debug MODE   off, depth or coverage: a debug view of the effects' depth over the\n\
                    flight world (with --effects on)\n\
  These override the settings file ([display], [effects], [paths] and [controls]).\n\
  Headless with the GPU renderer, the pictures are its N x ones, each with the software\n\
  renderer's beside it (-software.png); at 1x the report counts identical pixels.\n\
  --quick-save PATH Quick-save slot of F5 (save) and F8 (load) (default: the card 1 file\n\
                    as .ac3state, else quick.ac3state next to the executable)\n\
  --save-at N       Save a state to the quick-save slot at VBlank N\n\
  --load-state PATH Start from a save state\n\
  --dump-textures DIR  Write every texture the game uploads and draws (one PNG per\n\
                    palette, named by its replacement key) and a manifest to DIR\n\
settings: show the settings file in use and its values;\n\
  --write-default [PATH] writes a commented default file (--force replaces one)\n\
gamepad: show the connected gamepads, their input and a rumble test (30 s)\n\
dump-assets --output DIR: every image in the disc's resources as PNG, with a\n\
  manifest of its sources (see docs/ASSET_DUMP.md)\n\
export-movies --output DIR: every movie of the Japanese discs (--disc and --disc2, or\n\
  [paths]), decoded as the game shows it: lossless PNGs of each frame's picture (without\n\
  the letterbox) in DIR/<key>/, and DIR/manifest.json (see docs/MOVIES.md).\n\
  --only LIST       Movie keys or SPH resources, e.g. movie_09e6,800 (default: all)\n\
  --depth game|24|15  MDEC output depth (default: game, the depth the game's player uses)\n\
  --manifest-only   Write the manifest, no frames\n\
  --threads N       Movies decoded at once (default: the processor count)\n\
Machine controls: arrows, X/Space cross, C circle, Z square, S triangle, Q/W L1/R1, 1/2 L2/R2,\n\
  Enter start, Backspace select, Alt+Enter fullscreen, F5 quick save, F8 quick load,\n\
  F3 frame rate, F6 effects on/off, F7 change discs (Japanese release), F9 reload the\n\
  texture and movie packs, Esc quit.\n\
  An XInput gamepad works as a DualShock: A cross, B circle, X square, Y triangle, LB/RB L1/R1,\n\
  LT/RT L2/R2, View select, Menu start, sticks analog ([controls] in the settings remaps).\n\
Menu controls: arrows, Enter/Space to confirm/start, Backspace to cancel, Esc to close.\n\
menu runs the native front end, loading screen and first mission hub; briefing rendering remains in progress.\n\
bootstrap is the separate loader/dispatcher probe (ac3 bootstrap).\n\
Default paths are found from the working directory or the folders above the executable;\n\
supply explicit paths otherwise. No game data is bundled.";

/// A default workspace path: as given when it exists from the working
/// directory, else under the nearest folder above the executable that has it
/// (so `dist/windows/ac3.exe` finds the workspace's files from anywhere).
fn locate(default: &str) -> PathBuf {
    let path = PathBuf::from(default);
    if path.exists() {
        return path;
    }
    env::current_exe()
        .ok()
        .and_then(|exe| {
            exe.ancestors()
                .skip(1)
                .map(|folder| folder.join(default))
                .find(|candidate| candidate.exists())
        })
        .unwrap_or(path)
}

fn run() -> Result<bool> {
    // No arguments at all (a double-click): the launcher.
    let mut command = if env::args_os().len() <= 1 {
        "launcher"
    } else {
        "bootstrap"
    }
    .to_owned();
    let mut command_set = false;
    let mut data_dir = locate("extracted/disc");
    let mut captures = locate("analysis/runtime");
    let mut oracle = locate("analysis/rust-oracle/state-vectors.json");
    let mut report: Option<PathBuf> = None;
    let mut module = 1;
    let mut headless = false;
    let mut frames = None;
    let mut inputs = String::new();
    let mut output: Option<PathBuf> = None;
    let default_disc = locate("Ace Combat 3 - Electrosphere (Europe) (En,Fr,De,Es,It).bin");
    // --disc, --disc2 and --data as given (the settings file and the
    // defaults fill in the rest).
    let mut disc: Option<PathBuf> = None;
    let mut disc2: Option<PathBuf> = None;
    let mut data_given = false;
    let mut vblanks = None;
    let mut pad = String::new();
    let mut pad_type = None;
    let mut quick_save: Option<PathBuf> = None;
    let mut save_at = None;
    let mut load_state: Option<PathBuf> = None;
    let mut dump_textures: Option<PathBuf> = None;
    let mut every = 50;
    let mut cards: [Option<CardSource>; 2] = [None, None];
    let mut settings_path: Option<PathBuf> = None;
    let mut display = DisplayOverrides::default();
    // --effects and --fx-debug.
    let mut effects_on: Option<bool> = None;
    let mut fx_debug: Option<ac3_port::gpu_renderer::fx::DebugView> = None;
    let mut write_default: Option<Option<PathBuf>> = None;
    let mut force = false;
    // export-movies and --movie-pack.
    let mut movie_pack: Option<Option<PathBuf>> = None;
    let mut only: Vec<String> = Vec::new();
    let mut depth: Option<ac3_port::movie_disc::Depth> = None;
    let mut manifest_only = false;
    let mut threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    // ac3-dev: routines forced through the interpreter fallback.
    #[cfg(feature = "dev-fallback")]
    let mut interpret: Option<String> = None;
    let mut args = env::args().skip(1).peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!("{HELP}");
                return Ok(true);
            }
            "bootstrap" | "inspect" | "verify" | "menu" | "machine" | "settings" | "gamepad"
            | "dump-assets" | "export-movies" | "launcher"
                if !command_set =>
            {
                command = arg;
                command_set = true;
            }
            "--data" => {
                data_dir = args.next().ok_or("--data needs a path")?.into();
                data_given = true;
            }
            "--captures" => captures = args.next().ok_or("--captures needs a path")?.into(),
            "--oracle" => oracle = args.next().ok_or("--oracle needs a path")?.into(),
            "--report" => report = Some(args.next().ok_or("--report needs a path")?.into()),
            "--headless" => headless = true,
            "--frames" => {
                frames = Some(
                    args.next()
                        .ok_or("--frames needs a number")?
                        .parse::<u32>()?,
                )
            }
            "--inputs" => inputs = args.next().ok_or("--inputs needs frame:button entries")?,
            "--output" => output = Some(args.next().ok_or("--output needs a directory")?.into()),
            "--disc" => disc = Some(args.next().ok_or("--disc needs a path")?.into()),
            "--disc2" => disc2 = Some(args.next().ok_or("--disc2 needs a path")?.into()),
            "--vblanks" => {
                vblanks = Some(
                    args.next()
                        .ok_or("--vblanks needs a number")?
                        .parse::<u64>()?,
                )
            }
            "--pad" => pad = args.next().ok_or("--pad needs a schedule")?,
            "--pad-type" => {
                let name = args
                    .next()
                    .ok_or("--pad-type needs auto, digital or dualshock")?;
                pad_type = if name.eq_ignore_ascii_case("auto") {
                    None
                } else {
                    Some(ac3_port::machine_player::PadType::parse(&name)?)
                }
            }
            "--quick-save" => {
                quick_save = Some(args.next().ok_or("--quick-save needs a path")?.into())
            }
            "--save-at" => {
                save_at = Some(
                    args.next()
                        .ok_or("--save-at needs a VBlank number")?
                        .parse::<u64>()?,
                )
            }
            "--load-state" => {
                load_state = Some(args.next().ok_or("--load-state needs a path")?.into())
            }
            "--dump-textures" => {
                dump_textures = Some(
                    args.next()
                        .ok_or("--dump-textures needs a directory")?
                        .into(),
                )
            }
            "--card1" | "--card2" => {
                let path = args.next().ok_or("--card1/--card2 need a path or none")?;
                cards[usize::from(arg == "--card2")] = Some(CardSource::parse(&path));
            }
            "--settings" => {
                settings_path = Some(args.next().ok_or("--settings needs a path")?.into())
            }
            "--window" => {
                let size = args.next().ok_or("--window needs WIDTHxHEIGHT")?;
                let (w, h) = size
                    .split_once(['x', 'X'])
                    .ok_or("--window needs WIDTHxHEIGHT, e.g. 1280x960")?;
                display.size = Some((w.parse()?, h.parse()?));
            }
            "--mode" => {
                display.mode = Some(settings::WindowMode::parse(
                    &args.next().ok_or("--mode needs windowed or borderless")?,
                )?)
            }
            "--scaling" => {
                display.scaling = Some(settings::Scaling::parse(
                    &args
                        .next()
                        .ok_or("--scaling needs aspect, stretch or integer")?,
                )?)
            }
            "--filter" => {
                display.filter = Some(settings::Filter::parse(
                    &args.next().ok_or("--filter needs nearest or linear")?,
                )?)
            }
            "--vsync" => {
                display.vsync = Some(match args.next().as_deref() {
                    Some("on") => true,
                    Some("off") => false,
                    _ => return Err("--vsync needs on or off".into()),
                })
            }
            "--internal-res" => {
                let scale = args
                    .next()
                    .ok_or("--internal-res needs 1..8")?
                    .parse::<u32>()?;
                if !(1..=ac3_port::gpu_renderer::MAX_SCALE).contains(&scale) {
                    return Err("--internal-res needs 1..8".into());
                }
                display.internal_resolution = Some(scale);
            }
            "--renderer" => {
                display.renderer = Some(settings::Renderer::parse(
                    &args
                        .next()
                        .ok_or("--renderer needs auto, software or gpu")?,
                )?)
            }
            "--upscaled-dither" => {
                display.upscaled_dither = Some(match args.next().as_deref() {
                    Some("on") => true,
                    Some("off") => false,
                    _ => return Err("--upscaled-dither needs on or off".into()),
                })
            }
            "--subpixel" => {
                display.subpixel = Some(match args.next().as_deref() {
                    Some("on") => true,
                    Some("off") => false,
                    _ => return Err("--subpixel needs on or off".into()),
                })
            }
            "--perspective" => {
                display.perspective = Some(match args.next().as_deref() {
                    Some("on") => true,
                    Some("off") => false,
                    _ => return Err("--perspective needs on or off".into()),
                })
            }
            "--texture-pack" => {
                let folder = args.next().ok_or("--texture-pack needs a folder or none")?;
                display.texture_pack = Some(
                    (!folder.eq_ignore_ascii_case("none") && !folder.trim().is_empty())
                        .then(|| PathBuf::from(folder)),
                );
            }
            "--shader-preset" => {
                let file = args.next().ok_or("--shader-preset needs a file or none")?;
                display.shader_preset = Some(
                    (!file.eq_ignore_ascii_case("none") && !file.trim().is_empty())
                        .then(|| PathBuf::from(file)),
                );
            }
            "--movie-pack" => {
                let folder = args.next().ok_or("--movie-pack needs a folder or none")?;
                movie_pack = Some(
                    (!folder.eq_ignore_ascii_case("none") && !folder.trim().is_empty())
                        .then(|| PathBuf::from(folder)),
                );
            }
            "--only" => {
                let list = args.next().ok_or("--only needs a list of movies")?;
                only.extend(list.split(',').map(str::to_owned));
            }
            "--depth" => {
                depth = match args.next().as_deref() {
                    Some("game") => None,
                    Some("24") => Some(ac3_port::movie_disc::Depth::Rgb24),
                    Some("15") => Some(ac3_port::movie_disc::Depth::Rgb15),
                    _ => return Err("--depth needs game, 24 or 15".into()),
                }
            }
            "--manifest-only" => manifest_only = true,
            "--threads" => {
                threads = args
                    .next()
                    .ok_or("--threads needs a number")?
                    .parse::<usize>()?
            }
            "--effects" => {
                effects_on = Some(match args.next().as_deref() {
                    Some("on") => true,
                    Some("off") => false,
                    _ => return Err("--effects needs on or off".into()),
                })
            }
            "--fx-debug" => {
                let mode = args
                    .next()
                    .ok_or("--fx-debug needs off, depth or coverage")?;
                fx_debug = Some(ac3_port::gpu_renderer::fx::DebugView::parse(&mode)?);
            }
            "--widescreen" => {
                display.widescreen = Some(settings::Widescreen::parse(
                    &args
                        .next()
                        .ok_or("--widescreen needs off, 16:9, 16:10 or 21:9")?,
                )?)
            }
            "--write-default" => {
                write_default = Some(
                    args.next_if(|next| !next.starts_with("--"))
                        .map(PathBuf::from),
                )
            }
            "--force" => force = true,
            "--every" => {
                every = args
                    .next()
                    .ok_or("--every needs a number")?
                    .parse::<u64>()?
            }
            "--module" => {
                module = args
                    .next()
                    .ok_or("--module needs an ID")?
                    .parse::<usize>()?
            }
            #[cfg(feature = "dev-fallback")]
            "--interpret" => {
                interpret = Some(args.next().ok_or("--interpret needs a list of routines")?)
            }
            _ => return Err(format!("unknown argument {arg}; use --help").into()),
        }
    }
    if !(1..=5).contains(&module) {
        return Err("--module must be 1..5".into());
    }
    let settings_path = settings_path.unwrap_or_else(Settings::default_path);
    if command == "settings" {
        return settings_command(&settings_path, write_default, force);
    }
    if command == "gamepad" {
        // No game data needed.
        let settings = Settings::load(&settings_path)?;
        ac3_port::gamepad::monitor(&settings.controls, 30.0);
        return Ok(true);
    }
    if command == "launcher" {
        ac3_port::launcher::run(ac3_port::launcher::Options {
            settings: settings_path,
            data_dir,
            disc: disc.unwrap_or(default_disc),
            game_args: passed_on(env::args_os().skip(1).collect()),
        })?;
        return Ok(true);
    }
    if command == "export-movies" {
        // The discs as the machine finds them: the command line, then the
        // file, then the default; the other disc next to the first.
        let settings = Settings::load(&settings_path)?;
        let base = settings_path.parent().unwrap_or(std::path::Path::new(""));
        let disc = disc
            .or_else(|| settings.paths.disc(base))
            .unwrap_or(default_disc);
        let disc2 = disc2.or_else(|| settings.paths.disc2(base));
        let jp = ac3_port::machine_player::jp();
        let (discs, _) = ac3_port::machine_player::disc_images(jp, &disc, disc2.as_deref())?;
        let output = output.ok_or("export-movies needs --output DIR")?;
        let manifest = ac3_port::movie_export::run(&ac3_port::movie_export::ExportOptions {
            discs,
            output: &output,
            only,
            depth,
            manifest_only,
            threads,
        })?;
        println!(
            "Movie export: {} movies, {} frames ({} bytes) in {} ({:.1} s)",
            manifest["movies"].as_array().map_or(0, Vec::len),
            manifest["frames_written"],
            manifest["bytes_written"],
            output.display(),
            manifest["seconds"].as_f64().unwrap_or(0.0)
        );
        if let Some(path) = report {
            fs::write(&path, serde_json::to_string_pretty(&manifest)? + "\n")?;
            println!("Report: {}", path.display());
        }
        return Ok(true);
    }
    // The machine reads the settings file before its game files: the discs
    // come from the command line, then the file, then the default.
    let mut machine_settings = None;
    if command == "machine" {
        let settings = Settings::load(&settings_path)?;
        let base = settings_path.parent().unwrap_or(std::path::Path::new(""));
        disc = disc.or_else(|| settings.paths.disc(base));
        disc2 = disc2.or_else(|| settings.paths.disc2(base));
        machine_settings = Some(settings);
    }
    let disc = disc.unwrap_or(default_disc);
    // The machine plays the release of its disc; the asset dump reads
    // either release's files; the rest is PAL only.
    let data = match command.as_str() {
        "machine" => machine_data(&data_dir, data_given, &disc)?,
        "dump-assets" => GameData::load_any(&data_dir)?,
        _ => GameData::load(&data_dir)?,
    };
    let (result, passed) = match command.as_str() {
        "machine" => {
            let settings = machine_settings.take().unwrap_or_default();
            if settings_path.is_file() {
                eprintln!("ac3: settings: {}", settings_path.display());
            }
            // The command line first, then the file, then a blank card.
            let base = settings_path.parent().unwrap_or(std::path::Path::new(""));
            let cards = [0, 1].map(|slot| {
                cards[slot]
                    .clone()
                    .or_else(|| settings.paths.card(slot, base))
                    .unwrap_or(CardSource::Blank)
            });
            // The command line, then the file, then next to card 1.
            let quick_save = quick_save
                .or_else(|| settings.paths.quick_save(base))
                .unwrap_or_else(|| ac3_port::savestate::default_slot(&cards[0]));
            // ac3-dev: the fallback's extent tables of the release played.
            #[cfg(feature = "dev-fallback")]
            let fallback = ac3_fallback::start(
                match data.version {
                    ac3_port::version::Version::Pal => ac3_fallback::Version::Pal,
                    ac3_port::version::Version::Jp { .. } => ac3_fallback::Version::Jp,
                },
                interpret.as_deref(),
            )?;
            let result = ac3_port::machine_player::run(
                &data,
                ac3_port::machine_player::RunOptions {
                    disc: &disc,
                    disc2: disc2.as_deref(),
                    headless,
                    vblanks,
                    pad: ac3_port::machine_player::parse_pad_schedule(&pad)?,
                    pad_type,
                    snapshot_every: every,
                    output: output.as_deref(),
                    cards,
                    display: display.apply(settings.display, base),
                    effects: settings::Effects {
                        enabled: effects_on.unwrap_or(settings.effects.enabled),
                        debug: fx_debug.unwrap_or(settings.effects.debug),
                        ..settings.effects
                    },
                    controls: settings.controls,
                    quick_save,
                    save_at,
                    load_state: load_state.as_deref(),
                    dump_textures: dump_textures.as_deref(),
                    // The command line (from the working directory), else
                    // the file's (from its folder).
                    movie_pack: movie_pack
                        .clone()
                        .unwrap_or_else(|| settings.paths.movie_pack(base)),
                },
            )?;
            #[cfg(feature = "dev-fallback")]
            let result = ac3_fallback::finish(fallback, result);
            println!(
                "Native machine: {} VBlanks, state {} substate {}, {}",
                result["vblanks"], result["state"], result["substate"], result["status"]
            );
            if let Some(directory) = &dump_textures {
                let dump = &result["texture_dump"];
                println!(
                    "Texture dump: {} textures from {} uploads in {}",
                    dump["textures"],
                    dump["upload_contents"],
                    directory.display()
                );
            }
            (result, true)
        }
        "dump-assets" => {
            let directory = output.as_deref().ok_or("dump-assets needs --output DIR")?;
            let summary = ac3_port::asset_dump::dump_archive_images(&data, directory)?;
            println!(
                "Asset dump: {} images, {} PNGs in {}",
                summary["unique_images"],
                summary["pngs"],
                directory.display()
            );
            (summary, true)
        }
        "menu" => {
            let result =
                ac3_port::frontend::run_menu(&data, headless, frames, &inputs, output.as_deref())?;
            println!(
                "Native menu: {} frames, language ID{}, {}",
                result["frames"], result["language_id"], result["status"]
            );
            (result, true)
        }
        "inspect" => {
            let modules: Vec<_> = (1..=5)
                .map(|id| {
                    Ok(json!({"id":id,"size":data.overlay(id)?.len(),
                "sha256":ac3_port::sha256(data.overlay(id)?)}))
                })
                .collect::<Result<_>>()?;
            (
                json!({"build":"SCES-02066 PAL","resident_bytes":data.executable.payload.len(),
                "resident_entry":format!("0x{:08X}",data.executable.entry_pc),"archive_entries":data.directory.entries.len(),
                "state_count":data.states.len(),"overlays":modules}),
                true,
            )
        }
        "verify" => {
            let captures = validation::verify_captures(&data, &captures)?;
            let oracle = validation::verify_oracle(&data, &oracle)?;
            let passed = captures["passed"] == true && oracle["passed"] == true;
            println!(
                "Original memory comparisons: {}",
                if captures["passed"] == true {
                    "PASS"
                } else {
                    "FAIL"
                }
            );
            println!(
                "Original MIPS post-callback transitions: {}/{} cases match",
                oracle["matched"], oracle["cases"]
            );
            (
                json!({"passed":passed,"captures":captures,"dispatcher":oracle}),
                passed,
            )
        }
        _ => {
            let boot = Bootstrap::run(&data, module)?;
            for line in &boot.trace {
                println!("{line}");
            }
            (
                json!({"status":"stopped_before_unported_callback","mode":if module==1 {"normal_initial_state"} else {"controlled_module_probe"},
                "module":module,"loaded_bytes":boot.loaded_bytes,"next_callback":format!("0x{:08X}",boot.callback),
                "context":validation::context_json(boot.context),"trace":boot.trace}),
                true,
            )
        }
    };
    if let Some(path) = report {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, serde_json::to_string_pretty(&result)? + "\n")?;
        println!("Report: {}", path.display());
    } else if command == "inspect" {
        println!("{}", serde_json::to_string_pretty(&result)?);
    }
    Ok(passed)
}

/// The game files `machine` plays: for a Japanese disc, the extracted files
/// at `data_dir` when `--data` named Japanese ones (`ACE3.BIN`), else those
/// on the disc itself, so a Japanese disc needs nothing extracted; for
/// anything else the PAL files at `data_dir`, as before.
fn machine_data(
    data_dir: &std::path::Path,
    data_given: bool,
    disc: &std::path::Path,
) -> Result<GameData> {
    use ac3_port::{assets, version};
    match version::identify(disc) {
        Ok(identity) if matches!(identity.version, version::Version::Jp { .. }) => {
            if data_given && data_dir.join(assets::jp::RESIDENT).is_file() {
                GameData::load_jp(data_dir)
            } else {
                eprintln!(
                    "ac3: {}: {}; reading the game files from the disc",
                    disc.display(),
                    identity.version
                );
                GameData::load_jp_disc(disc)
            }
        }
        _ => GameData::load(data_dir),
    }
}

/// The launcher's `--data`, `--disc` and `--disc2` options, which its Play
/// button passes on to the game.
fn passed_on(args: Vec<std::ffi::OsString>) -> Vec<std::ffi::OsString> {
    args.windows(2)
        .filter(|pair| pair[0] == "--data" || pair[0] == "--disc" || pair[0] == "--disc2")
        .flat_map(|pair| pair.iter().cloned())
        .collect()
}

/// `[display]` values given on the command line.
#[derive(Default)]
struct DisplayOverrides {
    size: Option<(u32, u32)>,
    mode: Option<settings::WindowMode>,
    scaling: Option<settings::Scaling>,
    filter: Option<settings::Filter>,
    vsync: Option<bool>,
    internal_resolution: Option<u32>,
    renderer: Option<settings::Renderer>,
    upscaled_dither: Option<bool>,
    subpixel: Option<bool>,
    perspective: Option<bool>,
    /// `--texture-pack`: a folder (from the working directory), or `None`
    /// for none.
    texture_pack: Option<Option<PathBuf>>,
    widescreen: Option<settings::Widescreen>,
    /// `--shader-preset`: a file (from the working directory), or `None`
    /// for none.
    shader_preset: Option<Option<PathBuf>>,
}

impl DisplayOverrides {
    /// The file's `[display]` with the command line's values. The texture
    /// pack becomes a path the game can open as it is: the command line's
    /// as given, or the file's relative to `base` (its folder).
    fn apply(&self, file: settings::Display, base: &std::path::Path) -> settings::Display {
        let (window_width, window_height) =
            self.size.unwrap_or((file.window_width, file.window_height));
        let texture_pack = match &self.texture_pack {
            Some(given) => given.clone(),
            None => file.texture_pack_dir(base),
        };
        let shader_preset = match &self.shader_preset {
            Some(given) => given.clone(),
            None => file.shader_preset_path(base),
        };
        settings::Display {
            replace_textures: texture_pack.is_some(),
            texture_pack: texture_pack.map_or_else(String::new, |p| p.display().to_string()),
            window_width,
            window_height,
            mode: self.mode.unwrap_or(file.mode),
            scaling: self.scaling.unwrap_or(file.scaling),
            filter: self.filter.unwrap_or(file.filter),
            vsync: self.vsync.unwrap_or(file.vsync),
            show_fps: file.show_fps,
            internal_resolution: self.internal_resolution.unwrap_or(file.internal_resolution),
            renderer: self.renderer.unwrap_or(file.renderer),
            upscaled_dither: self.upscaled_dither.unwrap_or(file.upscaled_dither),
            subpixel: self.subpixel.unwrap_or(file.subpixel),
            perspective: self.perspective.unwrap_or(file.perspective),
            widescreen: self.widescreen.unwrap_or(file.widescreen),
            shader_preset: shader_preset.map_or_else(String::new, |p| p.display().to_string()),
            crt: file.crt,
            crt_preset: file.crt_preset,
            crt_curved: file.crt_curved,
            crt_folder: Some(settings::Display::crt_folder(base)),
        }
    }
}

/// `ac3 settings`: show the file in use and its values, or write the
/// commented default file.
fn settings_command(
    path: &std::path::Path,
    write: Option<Option<PathBuf>>,
    force: bool,
) -> Result<bool> {
    if let Some(target) = write {
        let target = target.unwrap_or_else(|| path.to_owned());
        Settings::write_default(&target, force)?;
        println!("Wrote default settings: {}", target.display());
        return Ok(true);
    }
    let settings = Settings::load(path)?;
    let state = if path.is_file() {
        ""
    } else {
        " (not found; defaults)"
    };
    println!("# Settings file: {}{state}", path.display());
    print!("{}", settings.to_toml()?);
    Ok(true)
}

pub(crate) fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("ac3: {error}");
            ExitCode::FAILURE
        }
    }
}
