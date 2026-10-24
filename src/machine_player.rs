//! Native player host for the address-preserving machine (`ac3_machine`).
//!
//! The game version is a [`Game`] (`ac3_flight::game::GAME` for the
//! European release): [`run`] picks it when it starts ([`game_for`], the
//! version selection point), and everything version-specific the host does
//! goes through it: the boot, each main-loop pass, the CD interrupt work,
//! libpad's VBlank handler, the field rate, and the RAM words the host
//! reads and writes as the interrupts would ([`Layout`]).
//!
//! The translated resident code logs every device-facing SDK call as a
//! [`HostCall`], by its canonical id ([`host_ids`]). [`Host`] is the device
//! side of all of them: the GPU (ordering tables, environments, VRAM
//! transfers), the disc ([`DiscDevice`]), and the interrupt-maintained RAM
//! the SDK routines read. The VBlank handler's work happens when the
//! translated `VSync(n)` waits: the host delivers every VBlank the wait
//! spans (libetc's VBlank counter and libpad's VBlank handler,
//! [`Game::pad_vblank`], which exchanges with the controllers and fills the
//! receive buffers), as the hardware interrupts would during the original's
//! busy wait.
//!
//! The SPU ([`SpuDevice`]) answers the libspu calls and mixes one field of
//! 44.1 kHz audio at each delivered VBlank ([`Host::take_audio`]; PAL 881.9
//! samples, NTSC 735.735, [`FieldLength`]).
//!
//! **Releases.** [`game_for`] picks the [`Game`] from the disc identity:
//! PAL ([`pal`]) or the Japanese release ([`jp`], the `ac3-flight-jp`
//! crate). The host devices that read or write the game's SDK data in RAM
//! take that release's words ([`HostTables`]: libspu's state, libpress's
//! default MDEC tables, libetc's VBlank counter for the card work) and its
//! field length (the SPU mixer and busy waits).
//!
//! **Discs.** A release of two discs has both mounted ([`Host::with_discs`]):
//! the CD drive holds a [`DiscSet`] and models its lid. When the game stops
//! the drive and waits for the lid to open (the Japanese "insert disc 2"
//! prompt; [`CdDrive::awaits_lid`]), the host changes to the other disc on
//! its own: lid open, swap, lid closed, as a player would
//! ([`Host::auto_change`]). F7 in the window changes discs by hand. The
//! boot's disc device (CdSearchFile, CdRead) follows the drive's disc.
//!
//! [`Player`] owns the machine and steps the resident main loop
//! ([`Game::main_loop_pass`]) one pass at a time. [`run`] drives it headless
//! (PNGs and a WAV file) or in a window (winit + wgpu through
//! [`crate::present`], paced to the game's field rate: 50 VBlanks per
//! second of wall time for PAL, 60000/1001 for the Japanese release).
//!
//! The GPU renderer ([`crate::gpu_renderer`], `[display]
//! internal_resolution` above 1) runs beside the software GPU: [`Host::hw`]
//! records each pass from the same GPU stream, and the window (or the
//! headless loop) runs it on the card and shows its N x picture. The
//! software GPU stays authoritative for everything the game reads.
//!
//! Texture replacement (`[display] texture_pack`) rides on the GPU
//! renderer: [`Host::start_gpu_renderer`] opens the pack for its recorder,
//! F9 reloads it ([`Host::reload_texture_pack`]). Only the GPU picture
//! changes. The movie pack's hub clips take the same path above 1x: each
//! clip frame complete in VRAM goes to the recorder after its last slice's
//! upload (`Host::load_image`), with or without a texture pack.
use crate::flight_host::GpuDevice;
use crate::machine_audio::{self, AudioOutput, FieldLength, LibSpu, SpuDevice};
use crate::machine_card::{self, CardDevice, CardSource};
use crate::machine_mdec::{self, DefaultTables, MdecDevice};
use crate::machine_movie::{BusyClock, XaPlayer, XaQueue, XaSink};
use ac3_graphics::Vram;
use ac3_machine::{
    Device, Fault, Game, HostCall, Machine,
    cd::{CdDrive, Disc, DiscSet, RawImage},
    disc::DiscDevice,
    game::Layout,
    host_ids,
    sio::{Controller, Sio},
};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    fs::File,
    path::{Path, PathBuf},
    rc::Rc,
};

/// SDK entry points handled here in addition to [`crate::flight_host::sdk`]
/// (canonical ids, [`host_ids`]).
pub mod calls {
    pub use ac3_machine::host_ids::{LOAD_IMAGE, STORE_IMAGE, VSYNC};
}

/// The PAL game: the reference version, and the one [`Host::new`] runs.
pub fn pal() -> &'static dyn Game {
    &ac3_flight::game::GAME
}

/// The Japanese release's game (`ac3-flight-jp`: the generated crate and
/// its patch layer).
pub fn jp() -> &'static dyn Game {
    &ac3_flight_jp::jp::game::GAME
}

/// What the host devices take from the release beyond its [`Layout`]: the
/// RAM words of SDK data they read and write, and the field length of the
/// SPU mixer and busy waits. Configuration, never game state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostTables {
    /// libspu's state words ([`SpuDevice`]).
    pub libspu: LibSpu,
    /// libpress's default MDEC blocks ([`MdecDevice`]).
    pub mdec: DefaultTables,
    /// CPU cycles per field ([`SpuDevice::mix_field`], [`BusyClock`]).
    pub field: FieldLength,
}

impl HostTables {
    pub const PAL: Self = Self {
        libspu: LibSpu::PAL,
        mdec: DefaultTables::PAL,
        field: FieldLength::PAL,
    };
    pub const JP: Self = Self {
        libspu: LibSpu::JP,
        mdec: DefaultTables::JP,
        field: FieldLength::NTSC,
    };

    /// The tables of `game`, by its release name (each version crate's
    /// `NAME`).
    ///
    /// # Panics
    ///
    /// For a [`Game`] this host has no tables for: a new version crate
    /// needs its row here.
    pub fn of(game: &dyn Game) -> Self {
        let tables = match game.name() {
            ac3_flight::game::NAME => Self::PAL,
            ac3_flight_jp::jp::game::NAME => Self::JP,
            other => panic!("the host has no device tables for the {other} release"),
        };
        debug_assert_eq!(tables.field, FieldLength::of(game.video()));
        tables
    }
}

/// The version selection point: the [`Game`] of the release on the disc
/// image at `disc` ([`crate::version::identify`]: its volume ID and
/// `SYSTEM.CNF`), which must be the release of the loaded files `data`. The
/// rest of the player only ever sees the returned [`Game`]: the PAL
/// translation ([`pal`]) or the Japanese one ([`jp`]). An image the
/// identification does not know (a modified disc) plays as the release of
/// the loaded files.
pub fn game_for(
    data: &crate::assets::GameData,
    disc: &std::path::Path,
) -> crate::Result<&'static dyn Game> {
    use crate::version::{Version, VersionError, identify};
    let release = match identify(disc) {
        Ok(identity) => identity.version,
        Err(VersionError::Io { .. }) => data.version,
        Err(error) => {
            eprintln!("ac3: {error}; playing the release of the game files");
            data.version
        }
    };
    let same = |a: Version, b: Version| {
        matches!(
            (a, b),
            (Version::Pal, Version::Pal) | (Version::Jp { .. }, Version::Jp { .. })
        )
    };
    if !same(release, data.version) {
        return Err(format!(
            "{}: a disc of {release}, but the game files are {}",
            disc.display(),
            data.version
        )
        .into());
    }
    Ok(match release {
        Version::Pal => pal(),
        Version::Jp { .. } => jp(),
    })
}

/// The disc images a run of `game` mounts, in disc order, and the index of
/// `disc` (the one the game boots from) among them. A one-disc release has
/// `disc` alone. For a release of more discs: `disc` with `disc2` when
/// given (both must be the release's discs), else with the disc
/// [`crate::version::DiscSet::find`] finds next to it. When it finds none
/// the run goes on with `disc` alone, and says so: the game then waits at
/// its disc change.
pub fn disc_images(
    game: &dyn Game,
    disc: &Path,
    disc2: Option<&Path>,
) -> crate::Result<(Vec<PathBuf>, usize)> {
    use crate::version::{DiscSet, Version, identify};
    if game.discs() <= 1 {
        return Ok((vec![disc.to_owned()], 0));
    }
    let set = match disc2 {
        Some(other) => DiscSet::from_paths(&[disc.to_owned(), other.to_owned()])?,
        None => match DiscSet::find(disc) {
            Ok(set) => set,
            Err(error) => {
                eprintln!(
                    "ac3: {error}; playing with {} only (the game will wait at its \
                     disc change; give the other disc with --disc2)",
                    disc.display()
                );
                return Ok((vec![disc.to_owned()], 0));
            }
        },
    };
    let booted = match identify(disc) {
        Ok(identity) => match identity.version {
            Version::Jp { disc } => usize::from(disc.saturating_sub(1)),
            Version::Pal => 0,
        },
        Err(_) => 0,
    };
    let booted = booted.min(set.discs.len() - 1);
    Ok((set.discs, booted))
}

fn index(address: u32) -> usize {
    (address & 0x1f_ffff) as usize
}
fn read_u16(ram: &[u8], address: u32) -> u16 {
    let at = index(address);
    u16::from_le_bytes([ram[at], ram[(at + 1) & 0x1f_ffff]])
}
fn read_u32(ram: &[u8], address: u32) -> u32 {
    u32::from(read_u16(ram, address)) | (u32::from(read_u16(ram, address.wrapping_add(2))) << 16)
}
/// The game context's pad state (80028C24's `pad`) and its DualShock
/// analog record's calibration (`config` +0x15C, class 4's record +0xB8,
/// its {low, centre, high, dead zone} bytes at +0x40): the same offsets in
/// both releases.
const CONTEXT_PAD: u32 = 0x50;
const CONTEXT_ANALOG_CALIBRATION: u32 = 0x15c + 0xb8 + 0x40;

/// The game's calibration of port 1's four stick axes while it reads the
/// controller as a DualShock in analog mode (pad type 0x73, mode 7: its
/// input class 4, whose record 8002895C applies); `None` before the
/// context exists or for any other controller.
fn analog_calibration(ram: &[u8], layout: &Layout) -> Option<[crate::gamepad::AxisCalibration; 4]> {
    let context = read_u32(ram, layout.context);
    if context & 0xffe0_0000 != 0x8000_0000 {
        return None;
    }
    let pad = context + CONTEXT_PAD;
    if ram[index(pad + 0xc)] != 0x73 || read_u16(ram, pad + 0xa) != 7 {
        return None;
    }
    let record = context + CONTEXT_ANALOG_CALIBRATION;
    Some(std::array::from_fn(|axis| {
        let byte = |offset: u32| ram[index(record + axis as u32 * 4 + offset)];
        crate::gamepad::AxisCalibration {
            low: byte(0),
            centre: byte(1),
            high: byte(2),
            dead: byte(3),
        }
    }))
}

fn write_bytes(ram: &mut [u8], address: u32, bytes: &[u8]) {
    for (i, &byte) in bytes.iter().enumerate() {
        ram[index(address.wrapping_add(i as u32))] = byte;
    }
}

/// A RECT {x, y, w, h} in RAM, as the libgpu transfer routines take it.
fn rect(ram: &[u8], address: u32) -> [u16; 4] {
    std::array::from_fn(|i| read_u16(ram, address.wrapping_add(i as u32 * 2)))
}

/// Device side of every host call; see the module documentation.
pub struct Host {
    /// The game version whose RAM words and libpad handler the interrupt
    /// work uses.
    pub game: &'static dyn Game,
    /// `game`'s RAM words (copied: read at every VBlank).
    layout: Layout,
    /// `game`'s SDK data words and field length for the devices.
    pub tables: HostTables,
    pub gpu: GpuDevice,
    /// The GPU renderer's recorder, while that renderer draws. It sees the
    /// GPU stream and changes nothing; save states leave it out (a load
    /// re-seeds it, [`Host::reseed_gpu_renderer`]).
    pub hw: Option<Box<crate::gpu_renderer::Recorder>>,
    /// Sub-pixel vertex positions for the GPU renderer, handed to its
    /// recorder when one starts ([`Host::start_gpu_renderer`]).
    pub vertex_precision: Option<Rc<dyn crate::gpu_renderer::VertexPrecision>>,
    /// Perspective-correct textures from `vertex_precision`'s depths, for
    /// the GPU renderer's recorder when one starts.
    pub perspective: bool,
    /// The texture pack folder, opened for the GPU renderer's recorder
    /// when it starts.
    pub texture_pack: Option<std::path::PathBuf>,
    /// Flight effects (`[effects]`) for the GPU renderer's recorder when it
    /// starts above 1x on the Japanese release (`None`: none; F6 has
    /// nothing to toggle).
    pub effects: Option<crate::gpu_renderer::fx::Config>,
    /// The `[effects]` settings: the effects' own keys, for their halves
    /// when the GPU renderer starts (`effects` is the infrastructure's).
    pub effect_settings: crate::settings::Effects,
    /// The boot's disc device (CdSearchFile, CdRead): the image of the disc
    /// in the drive.
    pub disc: Option<DiscDevice<File>>,
    /// The CD drive behind libcd (commands, interrupts, sectors, the lid)
    /// with every disc of the release; calls it does not answer
    /// (CdSearchFile, CdRead, ...) go to `disc`.
    pub cd: Option<CdDrive<DiscSet<RawImage<File>>>>,
    /// The disc images in disc order, for `disc` after a disc change.
    disc_images: Vec<File>,
    /// Change discs when the game waits for the lid ([`Self::auto_change`]);
    /// on by default.
    pub auto_disc_change: bool,
    /// A disc change asked for by hand (F7), done at the next host call.
    disc_change_requested: bool,
    /// Disc changes made.
    pub disc_changes: u64,
    /// The sound processor (libspu calls, sound RAM, voices, mixer).
    pub spu: SpuDevice,
    /// The MDEC behind libpress (DecDCTReset/DecDCTin/DecDCTout).
    pub mdec: MdecDevice,
    /// XA-ADPCM sectors the drive played, decoded into the SPU's CD input
    /// at each delivered VBlank.
    pub xa: Rc<RefCell<XaQueue>>,
    pub xa_player: XaPlayer,
    /// Guest time of the movie decoder's busy waits (`states::busy`).
    pub busy: BusyClock,
    /// The kernel's events and the two memory cards (`bios` calls).
    pub card: CardDevice,
    /// Mixed audio of the VBlanks delivered since the last [`take_audio`](Self::take_audio).
    pub audio: Vec<[i16; 2]>,
    /// Port 1 buttons (libpad active-high order: bit 0 SELECT .. bit 15
    /// SQUARE), sampled at each delivered VBlank.
    pub buttons: u16,
    /// Port 1 stick positions (right X, right Y, left X, left Y; 0x80
    /// centre, left/up 0x00), sampled with `buttons` at each delivered
    /// VBlank. Only a DualShock in analog mode reports them.
    pub sticks: [u8; 4],
    /// Carry each moved stick axis past the game's own dead zone before
    /// the controller sends it (`[controls] compensate_deadzone`,
    /// [`analog_calibration`]); off (the default here): `sticks` as they
    /// are.
    pub compensate_deadzone: bool,
    /// The controller ports behind libpad (port 1 a digital pad, port 2
    /// empty by default).
    pub pad: Sio,
    /// Whether libpad's VBlank handler runs: from PadStartCom (host call
    /// 8009B200) until PadStopCom (8009B220); not before PadStartCom once
    /// PadInitDirect (8009C2A0) is seen. `None` (a machine started from a
    /// snapshot): when libpad is set up in RAM.
    pub pad_running: Option<bool>,
    /// Optional pad source, called with the index of the VBlank being
    /// delivered (0 = first); it replaces `buttons` when present.
    pub pad_input: Option<Box<dyn FnMut(u64) -> u16>>,
    /// VBlanks delivered since the host was created.
    pub vblanks: u64,
    /// Every host call seen, by SDK entry point.
    pub calls: BTreeMap<u32, u64>,
    /// What the screen shows: the field after the latest VBlank.
    scanout: Scanout,
    /// The movie pack (`[paths] movie_pack`): its frame tracker sees the
    /// MDEC and LoadImage calls (reading only), and the presentation shows
    /// its images over the movie frames (the hub's clips through the GPU
    /// renderer above 1x). Never game state, not saved.
    pub movies: Option<Box<crate::movie_pack::MovieReplacer>>,
    error: Option<String>,
}

/// What the screen shows: the field a TV scans out after the latest VBlank.
/// That is VRAM as the VBlank left it, with the display the game sets
/// right after it (frame end's PutDispEnv runs in the vertical blank), so
/// the host keeps a copy at the first VRAM write after the VBlank. Until
/// that write the display now is the field.
///
/// A double-buffered screen shows the same picture either way. A
/// single-buffered 480i screen differs: the JP credits (state 7) draw
/// their table before the VSync wait and frame end's PutDrawEnv clears the
/// buffer after it, so the display after the pass is black while the field
/// shows the credits; screens that draw after the clear (the hub, the
/// terminal) show the previous pass's picture, as a real field does.
/// Presentation only: never game state, not saved.
#[derive(Default)]
struct Scanout {
    /// A VBlank came and VRAM has not been written since.
    armed: bool,
    /// `vram`, `display` and `size` hold the field (`false`: the display
    /// now is the field).
    held: bool,
    vram: Vec<u16>,
    /// The GP1 display state and the display's width and height
    /// ([`GpuDevice::display`]).
    display: ac3_graphics::gpu::Display,
    size: [i16; 2],
}

impl Host {
    /// The host of the PAL game ([`pal`]); see [`Self::with_game`].
    pub fn new(vram: Vram, disc: Option<File>) -> Self {
        Self::with_game(pal(), vram, disc)
    }
    /// The host of `game` on `vram`, with the disc image `disc` (the CD
    /// drive and the boot's disc device read it).
    pub fn with_game(game: &'static dyn Game, vram: Vram, disc: Option<File>) -> Self {
        Self::with_discs(game, vram, disc.into_iter().collect(), 0)
    }
    /// The host of `game` on `vram` with the disc images `discs` (disc
    /// order) mounted and disc index `inserted` in the drive. The devices
    /// take the release's tables ([`HostTables::of`]).
    pub fn with_discs(
        game: &'static dyn Game,
        vram: Vram,
        discs: Vec<File>,
        inserted: usize,
    ) -> Self {
        let xa = Rc::new(RefCell::new(XaQueue::default()));
        let layout = game.layout();
        let tables = HostTables::of(game);
        let images = discs
            .iter()
            .filter_map(|file| file.try_clone().ok().map(RawImage))
            .collect();
        let inserted = inserted.min(discs.len().saturating_sub(1));
        let mut spu = SpuDevice::new();
        spu.set_release(tables.libspu, tables.field);
        let mut mdec = MdecDevice::new();
        mdec.set_release(tables.mdec);
        Self {
            game,
            layout,
            tables,
            gpu: GpuDevice::new(vram),
            hw: None,
            vertex_precision: None,
            perspective: false,
            texture_pack: None,
            effects: None,
            effect_settings: crate::settings::Effects::default(),
            cd: DiscSet::new(images, inserted).map(|set| {
                let mut drive = CdDrive::new(set, layout.vblank_clock());
                drive.vblank_rate = game.field_rate().rounded_hz();
                drive.audio = Some(Box::new(XaSink(xa.clone())));
                drive
            }),
            disc: discs
                .get(inserted)
                .and_then(|file| file.try_clone().ok())
                .map(|file| DiscDevice::new(file, layout.cd_file_cache)),
            disc_images: discs,
            auto_disc_change: true,
            disc_change_requested: false,
            disc_changes: 0,
            spu,
            mdec,
            xa,
            xa_player: XaPlayer::default(),
            busy: BusyClock::default(),
            card: CardDevice::default().with_clock(layout.vblank_counter),
            audio: Vec::new(),
            buttons: 0,
            sticks: [0x80; 4],
            compensate_deadzone: false,
            pad: Sio::default(),
            pad_running: None,
            pad_input: None,
            vblanks: 0,
            calls: BTreeMap::new(),
            scanout: Scanout::default(),
            movies: None,
            error: None,
        }
    }
    /// Start the GPU renderer's recorder, seeded from the software GPU,
    /// with the texture pack when one is set (logged either way).
    pub fn start_gpu_renderer(&mut self, options: crate::gpu_renderer::Options) {
        let mut recorder = crate::gpu_renderer::Recorder::new(options, &self.gpu.gpu);
        recorder.precision = self.vertex_precision.clone();
        recorder.perspective = self.perspective && recorder.precision.is_some();
        if options.scale > 1 {
            let state = |on: bool| if on { "on" } else { "off" };
            eprintln!(
                "ac3: gpu renderer: sub-pixel geometry {}, perspective-correct textures {}",
                state(recorder.precision.is_some()),
                state(recorder.perspective)
            );
        }
        // Effects: the Japanese release's flight above 1x (its scene words);
        // configured even when off, so F6 can turn them on.
        if let Some(config) = self.effects {
            let jp = self.game.name() == ac3_flight_jp::jp::game::NAME;
            if options.scale > 1 && jp {
                recorder.set_effects(Some(config));
                crate::effects::atmosphere_fx::register_recorder(
                    &mut recorder,
                    crate::effects::atmosphere_fx::Config::from_settings(&self.effect_settings),
                );
                crate::effects::far_terrain::register_recorder(
                    &mut recorder,
                    crate::effects::far_terrain::Settings::from_settings(&self.effect_settings),
                );
                crate::effects::sun::register_recorder(&mut recorder, &self.effect_settings);
                eprintln!(
                    "ac3: effects: {} (F6 toggles){}{}",
                    if config.enabled { "on" } else { "off" },
                    match config.debug {
                        crate::gpu_renderer::fx::DebugView::Off => String::new(),
                        view => format!(", debug view {}", view.name()),
                    },
                    if recorder.precision.is_none() {
                        "; without sub-pixel geometry nothing has a depth"
                    } else {
                        ""
                    }
                );
            } else if config.enabled {
                eprintln!(
                    "ac3: effects: not used; they need the GPU renderer above 1x and the \
                     Japanese release"
                );
            }
        }
        if let Some(folder) = &self.texture_pack {
            if folder.is_dir() {
                match crate::gpu_renderer::TexturePack::open(folder) {
                    Ok(pack) => {
                        eprintln!("ac3: texture pack: {}", pack.summary());
                        recorder.set_texture_pack(Some(pack), self.gpu.gpu.vram());
                    }
                    Err(error) => eprintln!("ac3: texture pack: {error}"),
                }
            } else {
                eprintln!(
                    "ac3: texture pack: no folder {} (the game's own textures are drawn)",
                    folder.display()
                );
            }
        }
        // The movie pack's hub clips: textures, drawn from the pack above 1x.
        if let Some(movies) = self.movies.as_deref() {
            if options.scale > 1 {
                recorder.set_movie_images(Some(Box::new(movies.hub_images())), self.gpu.gpu.vram());
                eprintln!("ac3: movie pack: the hub's clips are drawn from the pack");
            } else {
                eprintln!(
                    "ac3: movie pack: the hub's clips need the GPU renderer above 1x (the game's \
                     are drawn)"
                );
            }
        }
        self.hw = Some(Box::new(recorder));
    }
    /// F9: read the texture pack folder again (or for the first time);
    /// returns the note to show.
    pub fn reload_texture_pack(&mut self) -> String {
        let Some(hw) = self.hw.as_deref_mut() else {
            return "Texture replacement needs the GPU renderer".to_owned();
        };
        let Some(folder) = &self.texture_pack else {
            return "No texture pack is set".to_owned();
        };
        let vram = self.gpu.gpu.vram();
        let result = match hw.reload_texture_pack(vram) {
            Some(result) => result,
            None => crate::gpu_renderer::TexturePack::open(folder).map(|pack| {
                let summary = pack.summary();
                hw.set_texture_pack(Some(pack), vram);
                summary
            }),
        };
        match result {
            Ok(summary) => {
                eprintln!("ac3: texture pack: {summary}");
                let count = hw.replacer().map_or(0, |replacer| replacer.pack().len());
                format!("Texture pack loaded: {count} replacements")
            }
            Err(error) => {
                eprintln!("ac3: texture pack: {error}");
                format!("Texture pack: {error}")
            }
        }
    }
    /// F6: effects on or off (from the next frame); returns the note to
    /// show.
    pub fn toggle_effects(&mut self) -> String {
        match self
            .hw
            .as_deref_mut()
            .and_then(|hw| hw.effects_config_mut())
        {
            Some(config) => {
                config.enabled = !config.enabled;
                let note = format!("Effects {}", if config.enabled { "on" } else { "off" });
                eprintln!("ac3: {note}");
                note
            }
            None => "Effects need the GPU renderer above 1x (Japanese release)".to_owned(),
        }
    }

    /// Turn effects off for good when the card cannot run them (`error`).
    pub fn effects_failed(&mut self, error: &str) {
        if let Some(config) = self
            .hw
            .as_deref_mut()
            .and_then(|hw| hw.effects_config_mut())
            .filter(|config| config.enabled)
        {
            config.enabled = false;
            eprintln!("ac3: effects off: {error}");
        }
    }

    /// After a load: the screen shows the loaded display until the next
    /// VBlank ([`Scanout`]).
    pub fn reset_scanout(&mut self) {
        self.scanout.armed = false;
        self.scanout.held = false;
        // The loaded VRAM holds no frame the movie tracker saw.
        if let Some(movies) = self.movies.as_deref_mut() {
            movies.reset();
        }
    }

    /// The movie pack's image for the movie frame in the shown field, if
    /// any, waiting for its decoding until `deadline` (`None`: as the pack
    /// says). Call once after each pass.
    pub fn movie_update(
        &mut self,
        deadline: Option<std::time::Instant>,
    ) -> Option<crate::movie_pack::MovieLayer> {
        let shown = {
            let (vram, display, [w, h]) = self.shown();
            self.movies.as_deref()?.tracker.shown(
                vram,
                (display.start_x, display.start_y),
                (w.max(1) as u16, h.max(1) as u16),
                display.mode & 0x10 != 0,
            )
        };
        let vblanks = self.vblanks;
        self.movies.as_deref_mut()?.update(shown, vblanks, deadline)
    }

    /// F9: scan the movie pack folder again; the note to show, if a pack is
    /// in use.
    pub fn reload_movie_pack(&mut self) -> Option<String> {
        let note = self.movies.as_deref_mut()?.reload();
        eprintln!("ac3: {note}");
        Some(note)
    }
    /// Before a VRAM write: the first one after a VBlank keeps the field the
    /// screen shows ([`Scanout`]), and marks the same point in the GPU
    /// renderer's pass.
    fn before_vram_write(&mut self) {
        if !std::mem::take(&mut self.scanout.armed) {
            return;
        }
        self.count_coverage();
        let scanout = &mut self.scanout;
        scanout.vram.clear();
        scanout.vram.extend_from_slice(self.gpu.gpu.vram().words());
        scanout.display = self.gpu.gpu.display();
        scanout.size = [self.gpu.display[2], self.gpu.display[3]];
        scanout.held = true;
        let area = display_area(scanout.display, scanout.size);
        if let Some(hw) = self.hw.as_deref_mut() {
            hw.show(area, self.gpu.gpu.vram());
        }
    }
    /// `--dump-textures`: the field the screen shows ([`Scanout`]) is the
    /// display now; count it in the dump's screen coverage.
    fn count_coverage(&mut self) {
        let [x, y, w, h] = self.gpu.display.map(i32::from);
        let movie = self.gpu.gpu.display().mode & 0x10 != 0;
        if let Some(recorder) = self.gpu.texture_dump.as_deref_mut() {
            recorder.scan_out([x, y, w, h], movie);
        }
    }
    /// After a load: the GPU renderer starts over from the software GPU.
    pub fn reseed_gpu_renderer(&mut self) {
        if let Some(hw) = self.hw.as_deref_mut() {
            hw.reseed(&self.gpu.gpu);
        }
    }
    /// The pass the GPU renderer recorded since the last call, if it runs.
    pub fn take_gpu_frame(&mut self) -> Option<crate::gpu_renderer::Frame> {
        let vram = self.gpu.gpu.vram();
        self.hw.as_deref_mut().map(|hw| hw.take_frame(vram))
    }
    /// First device error (GPU DMA, VRAM transfer), if any.
    pub fn error(&self) -> Option<&str> {
        self.error
            .as_deref()
            .or(self.gpu.error())
            .or(self.card.error())
    }
    fn fail(&mut self, message: String) {
        self.error.get_or_insert(message);
    }
    /// Mix `frames` more samples now (outside the VBlank clock).
    pub fn audio_frames(&mut self, frames: usize) -> Vec<[i16; 2]> {
        self.spu.mix(frames)
    }
    /// The audio mixed at the VBlanks delivered so far (44.1 kHz stereo).
    pub fn take_audio(&mut self) -> Vec<[i16; 2]> {
        std::mem::take(&mut self.audio)
    }
    /// Port 1's DualShock motors as the game last drove them: the data
    /// bytes of the last poll through the controller's 0x4D actuator map
    /// (both off for a digital pad or an empty port).
    pub fn motors(&self) -> crate::gamepad::Motors {
        self.pad.ports[0]
            .as_ref()
            .map_or_else(Default::default, |controller| {
                crate::gamepad::Motors::from_actuators(&controller.motors, &controller.actuators)
            })
    }

    /// The release's tables back on the devices (after a save state replaced
    /// them: the tables are configuration and the state's devices decode
    /// with PAL's).
    pub fn apply_release(&mut self) {
        self.spu.set_release(self.tables.libspu, self.tables.field);
        self.mdec.set_release(self.tables.mdec);
    }

    /// Discs mounted, and the index of the one in the drive (0: disc 1).
    pub fn discs(&self) -> (usize, usize) {
        self.cd
            .as_ref()
            .map_or((0, 0), |cd| (cd.disc.discs(), cd.disc.inserted()))
    }

    /// F7: change to the next disc at the next host call (the game is
    /// between two main-loop passes now). Returns the note to show.
    pub fn request_disc_change(&mut self) -> String {
        let (discs, inserted) = self.discs();
        if discs < 2 {
            return "Only one disc is mounted".to_owned();
        }
        self.disc_change_requested = true;
        format!("Changing to disc {}", (inserted + 1) % discs + 1)
    }

    /// The automatic disc change: after a CD command, when the release has
    /// more discs and the game waits for the lid to open
    /// ([`CdDrive::awaits_lid`]: the drive stopped, Getstat polls and
    /// nothing else), open the lid, put the other disc in and close it
    /// again ([`CdDrive::change_disc`]). The game then re-identifies the
    /// disc and reads the new one's files.
    fn auto_change(&mut self, ram: &[u8]) {
        let waiting = self
            .cd
            .as_ref()
            .is_some_and(|cd| self.auto_disc_change && cd.disc.discs() > 1 && cd.awaits_lid());
        if waiting {
            self.change_disc(ram, "the game waits for a disc change");
        }
    }

    /// Change to the next disc (lid open now, closed a moment later), and
    /// point the boot's disc device at it.
    fn change_disc(&mut self, ram: &[u8], reason: &str) {
        let Some(cd) = self.cd.as_mut() else {
            return;
        };
        let (discs, from) = (cd.disc.discs(), cd.disc.inserted());
        let to = (from + 1) % discs.max(1);
        if to == from || !cd.change_disc(ram, to) {
            return;
        }
        self.disc_changes += 1;
        eprintln!(
            "ac3: disc change: disc {} out, disc {} in ({reason}; VBlank {})",
            from + 1,
            to + 1,
            self.vblanks
        );
        self.sync_disc_device();
    }

    /// Point the boot's disc device at the image of the disc in the drive
    /// (after a disc change or a load).
    pub fn sync_disc_device(&mut self) {
        let (discs, inserted) = self.discs();
        if discs == 0 {
            return;
        }
        let Some(device) = self.disc.as_mut() else {
            return;
        };
        match self.disc_images.get(inserted).map(File::try_clone) {
            Some(Ok(file)) => {
                device.set_image(file);
            }
            Some(Err(error)) => self.fail(format!("disc {}: {error}", inserted + 1)),
            None => self.fail(format!("disc {} is not mounted", inserted + 1)),
        }
    }

    /// One VBlank interrupt: libetc's counter, then libpad's VBlank handler
    /// (both ports' exchanges, the receive buffers, the new-data flag)
    /// while libpad runs.
    fn vblank(&mut self, ram: &mut [u8]) {
        let layout = self.layout;
        let counter = read_u32(ram, layout.vblank_counter).wrapping_add(1);
        write_bytes(ram, layout.vblank_counter, &counter.to_le_bytes());
        self.xa_player.feed(&self.xa, &mut self.spu);
        let field = self.spu.mix_field();
        self.audio.extend(field);
        if let Some(input) = &mut self.pad_input {
            self.buttons = input(self.vblanks);
        }
        let running = self.pad_running.unwrap_or_else(|| {
            read_u32(ram, layout.pad_state) == 1
                && read_u32(ram, layout.pad_records_pointer) == layout.pad_records
        });
        if running {
            let sticks = match self
                .compensate_deadzone
                .then(|| analog_calibration(ram, &layout))
                .flatten()
            {
                Some(calibration) => crate::gamepad::compensate_sticks(self.sticks, &calibration),
                None => self.sticks,
            };
            if let Some(controller) = &mut self.pad.ports[0] {
                controller.buttons = self.buttons;
                controller.sticks = sticks;
            }
            self.game.pad_vblank(ram, &mut self.pad);
            if let Some(error) = self.pad.error.take() {
                self.fail(error);
            }
        }
        // No VRAM write since the last VBlank: its field was shown again.
        if self.scanout.armed {
            self.count_coverage();
        }
        // A new field: the display now is what it shows.
        self.scanout.armed = true;
        self.scanout.held = false;
        if let Some(hw) = self.hw.as_deref_mut() {
            hw.vblank();
        }
        self.vblanks += 1;
    }

    /// VSync(mode) for waiting modes: deliver VBlanks until the counter
    /// passes the target (VSync's record + mode - 1, mode 0: the record;
    /// PAL 800B2EC8), then one more, as libetc's VSync waits (see
    /// `ac3_flight::sdk::vsync`).
    fn vsync(&mut self, ram: &mut [u8], mode: i32) {
        if mode < 0 || mode == 1 {
            return;
        }
        let (counter_word, record) = (self.layout.vblank_counter, self.layout.vsync_record);
        let last = read_u32(ram, record);
        let target = if mode > 0 {
            last.wrapping_sub(1).wrapping_add(mode as u32)
        } else {
            last
        };
        let counter = read_u32(ram, counter_word);
        let reached = if (counter as i32) < (target as i32) {
            target
        } else {
            counter
        };
        while (read_u32(ram, counter_word) as i32) < (reached.wrapping_add(1) as i32) {
            self.vblank(ram);
        }
    }

    /// LoadImage(rect, pixels): a CPU→VRAM transfer of `w * h` halfwords.
    /// The last slice of a hub clip's frame hands the frame to the GPU
    /// renderer after the upload ([`crate::gpu_renderer::Recorder::movie_frame`]).
    fn load_image(&mut self, ram: &[u8], area: u32, pixels: u32) {
        let [x, y, w, h] = rect(ram, area);
        let movie_frame = self
            .movies
            .as_deref_mut()
            .and_then(|movies| movies.on_load_image(ram, [x, y, w, h], pixels));
        let count = usize::from(w) * usize::from(h);
        let mut words = vec![
            0xa000_0000,
            (u32::from(y) << 16) | u32::from(x),
            (u32::from(h) << 16) | u32::from(w),
        ];
        words.extend(
            (0..count.div_ceil(2)).map(|i| read_u32(ram, pixels.wrapping_add(i as u32 * 4))),
        );
        if count % 2 == 1 {
            // The DMA sends whole words; the GPU ignores the excess half.
            let last = words.len() - 1;
            words[last] &= 0xffff;
        }
        if let Some(recorder) = &mut self.gpu.texture_dump {
            let pixels: Vec<u16> = (0..count)
                .map(|i| (words[3 + i / 2] >> (16 * (i & 1))) as u16)
                .collect();
            recorder.on_upload([x, y, w, h], &pixels);
        }
        if let Some(hw) = self.hw.as_deref_mut() {
            for &word in &words {
                hw.gp0(word, 0, self.gpu.gpu.vram());
            }
            if let Some(frame) = movie_frame {
                hw.movie_frame(frame);
            }
        }
        if let Err(error) = self.gpu.gpu.write_gp0_words(&words) {
            self.fail(format!("LoadImage {x},{y} {w}x{h}: {error}"));
        }
    }
    /// StoreImage(rect, pixels): VRAM→RAM, row by row with VRAM wrap.
    fn store_image(&mut self, ram: &mut [u8], area: u32, pixels: u32) {
        let [x, y, w, h] = rect(ram, area);
        let vram = self.gpu.gpu.vram().words();
        let mut address = pixels;
        for row in 0..h {
            for column in 0..w {
                let vx = usize::from(x.wrapping_add(column)) & 0x3ff;
                let vy = usize::from(y.wrapping_add(row)) & 0x1ff;
                write_bytes(ram, address, &vram[vy * 1024 + vx].to_le_bytes());
                address = address.wrapping_add(2);
            }
        }
    }
}

impl Device for Host {
    fn call(&mut self, ram: &mut [u8], scratch: &mut [u8], call: HostCall) -> Option<u32> {
        *self.calls.entry(call.function).or_default() += 1;
        if std::mem::take(&mut self.disc_change_requested) {
            self.change_disc(ram, "F7");
        }
        // The calls that write VRAM: GP0 lists (drawing, PutDrawEnv's clear)
        // and LoadImage.
        if matches!(call.function, host_ids::DMA_LIST | host_ids::LOAD_IMAGE) {
            self.before_vram_write();
        }
        let [a0, a1, ..] = call.args;
        match call.function {
            function if machine_card::handles(function) => self.card.call(ram, scratch, call),
            function if machine_audio::handles(function) => self.spu.call(ram, scratch, call),
            function if machine_mdec::handles(function) => {
                let result = self.mdec.call(ram, scratch, call);
                if let Some(movies) = self.movies.as_deref_mut() {
                    movies.on_mdec(ram, call.function, call.args);
                }
                result
            }
            host_ids::PAD_INIT_DIRECT => {
                // Set-up before PadStartCom; a registered handler keeps running.
                self.pad_running.get_or_insert(false);
                None
            }
            host_ids::PAD_STOP_COM => {
                self.pad_running = Some(false);
                None
            }
            host_ids::PAD_START_COM => {
                self.pad_running = Some(true);
                None
            }
            host_ids::BUSY_WAIT => {
                // One iteration of a busy-wait loop: its cycles of guest
                // time, delivering the VBlanks that fall due.
                for _ in 0..self.busy.advance(a1, self.tables.field) {
                    self.vblank(ram);
                }
                Some(0)
            }
            host_ids::VSYNC => {
                self.vsync(ram, a0 as i32);
                None
            }
            host_ids::LOAD_IMAGE => {
                self.load_image(ram, a0, a1);
                None
            }
            host_ids::STORE_IMAGE => {
                self.store_image(ram, a0, a1);
                None
            }
            host_ids::DMA_LIST | host_ids::GP1 if self.hw.is_some() => {
                // Each DMA list goes to the GPU renderer's effects first
                // (with RAM: the flight ordering table's markers and the
                // scene; nothing with effects off), and its end after.
                let list = call.function == host_ids::DMA_LIST;
                if list && let Some(hw) = self.hw.as_deref_mut() {
                    let movie = self.gpu.gpu.display().mode & 0x10 != 0;
                    hw.list_begin(ram, a0, self.gpu.gpu.vram(), movie);
                }
                let result = self.gpu.call_with(ram, call, self.hw.as_deref_mut());
                if list && let Some(hw) = self.hw.as_deref_mut() {
                    hw.list_end(self.gpu.gpu.vram());
                }
                result
            }
            _ => {
                let result = self
                    .gpu
                    .call(ram, scratch, call)
                    .or_else(|| self.cd.as_mut()?.call(ram, scratch, call))
                    .or_else(|| self.disc.as_mut()?.call(ram, scratch, call));
                if call.function == host_ids::CD_COMMAND {
                    self.auto_change(ram);
                }
                result
            }
        }
    }
}

/// Shared handle: the machine owns the device hook, the player keeps the host.
#[derive(Clone)]
pub struct SharedHost(pub Rc<RefCell<Host>>);
impl Device for SharedHost {
    fn call(&mut self, ram: &mut [u8], scratch: &mut [u8], call: HostCall) -> Option<u32> {
        self.0.borrow_mut().call(ram, scratch, call)
    }
}

/// The machine and its host, stepped one main-loop pass at a time.
pub struct Player {
    /// The game version (the host's).
    pub game: &'static dyn Game,
    pub machine: Machine,
    pub host: Rc<RefCell<Host>>,
}

impl Player {
    /// The player of `host`'s game on `machine`.
    pub fn new(mut machine: Machine, host: Host) -> Self {
        let game = host.game;
        let host = Rc::new(RefCell::new(host));
        machine.device = Some(Box::new(SharedHost(host.clone())));
        Self {
            game,
            machine,
            host,
        }
    }
    /// One pass of the resident main loop. Host calls are logged only for
    /// the pass (the log is cleared first). A fault stops the pass where it
    /// occurred.
    pub fn iteration(&mut self) -> Result<(), Fault> {
        self.machine.host_calls.clear();
        self.game.main_loop_pass(&mut self.machine);
        // The CD interrupt work of the pass (libcd's handler; the drive
        // paces sectors by the VBlank count).
        if self.machine.fault().is_none() {
            self.game.cd_service(&mut self.machine);
        }
        self.machine.take_fault()
    }
    pub fn vblanks(&self) -> u64 {
        self.host.borrow().vblanks
    }
    pub fn set_buttons(&mut self, buttons: u16) {
        self.host.borrow_mut().buttons = buttons;
    }
    /// Port 1's analog stick bytes (see [`Host::sticks`]).
    pub fn set_sticks(&mut self, sticks: [u8; 4]) {
        self.host.borrow_mut().sticks = sticks;
    }
}

/// libpad button bits (active-high).
pub mod buttons {
    pub const SELECT: u16 = 1 << 0;
    pub const START: u16 = 1 << 3;
    pub const UP: u16 = 1 << 4;
    pub const RIGHT: u16 = 1 << 5;
    pub const DOWN: u16 = 1 << 6;
    pub const LEFT: u16 = 1 << 7;
    pub const L2: u16 = 1 << 8;
    pub const R2: u16 = 1 << 9;
    pub const L1: u16 = 1 << 10;
    pub const R1: u16 = 1 << 11;
    pub const TRIANGLE: u16 = 1 << 12;
    pub const CIRCLE: u16 = 1 << 13;
    pub const CROSS: u16 = 1 << 14;
    pub const SQUARE: u16 = 1 << 15;

    pub fn by_name(name: &str) -> Option<u16> {
        Some(match name.to_ascii_lowercase().as_str() {
            "select" => SELECT,
            "start" => START,
            "up" => UP,
            "right" => RIGHT,
            "down" => DOWN,
            "left" => LEFT,
            "l2" => L2,
            "r2" => R2,
            "l1" => L1,
            "r1" => R1,
            "triangle" => TRIANGLE,
            "circle" => CIRCLE,
            "cross" => CROSS,
            "square" => SQUARE,
            _ => return None,
        })
    }
}

/// Held-button schedule: `FIRST-LAST:button+button,...`
/// (VBlank numbers since boot, inclusive), e.g. `300-304:cross,500-560:up`.
pub fn parse_pad_schedule(text: &str) -> crate::Result<Vec<(u64, u64, u16)>> {
    let mut schedule = Vec::new();
    for entry in text.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        let (range, names) = entry
            .split_once(':')
            .ok_or_else(|| format!("pad entry {entry}: expected FIRST-LAST:buttons"))?;
        let (first, last) = range.split_once('-').unwrap_or((range, range));
        let mut held = 0;
        for name in names.split('+') {
            held |= buttons::by_name(name).ok_or_else(|| format!("unknown button {name}"))?;
        }
        schedule.push((first.parse()?, last.parse()?, held));
    }
    Ok(schedule)
}

impl Host {
    /// The field the screen shows ([`Scanout`]): its VRAM, GP1 display
    /// state and display size.
    fn shown(&self) -> (&[u16], ac3_graphics::gpu::Display, [i16; 2]) {
        let scanout = &self.scanout;
        if scanout.held {
            (&scanout.vram, scanout.display, scanout.size)
        } else {
            let [_, _, w, h] = self.gpu.display;
            (self.gpu.gpu.vram().words(), self.gpu.gpu.display(), [w, h])
        }
    }
}

/// The picture the screen shows ([`Scanout`]) as 0RGB words, and its size.
fn shown_rgb(host: &Host) -> Option<(Vec<u32>, usize, usize)> {
    let (vram, display, size) = host.shown();
    picture_rgb(vram, display, size)
}

/// The picture of `display` (the GP1 state) at `size` (width, height) in
/// `vram` as 0RGB words, and its size; `None` for a 15-bit area crossing
/// the VRAM edge, which shows nothing.
fn picture_rgb(
    vram: &[u16],
    display: ac3_graphics::gpu::Display,
    [w, h]: [i16; 2],
) -> Option<(Vec<u32>, usize, usize)> {
    use ac3_graphics::{VRAM_HEIGHT, VRAM_WIDTH};
    let (w, h) = (w.max(1) as usize, h.max(1) as usize);
    let (x, y) = (usize::from(display.start_x), usize::from(display.start_y));
    if display.mode & 0x10 != 0 {
        // 24-bit display (the movies): three bytes per pixel.
        let rgb = crate::machine_movie::display_rgb(vram, (x, y), w, h, true);
        return Some((rgb, w, h));
    }
    if x + w > VRAM_WIDTH || y + h > VRAM_HEIGHT {
        return None;
    }
    let rgb = (y..y + h)
        .flat_map(|row| &vram[row * VRAM_WIDTH + x..row * VRAM_WIDTH + x + w])
        .map(|&p| {
            let [r, g, b, _] = ac3_graphics::rgba8(p);
            (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
        })
        .collect();
    Some((rgb, w, h))
}

/// The aspect the shown picture was drawn for: a widescreen aspect for
/// a picture of widescreen flight, `None` for 4:3 (menus, the hub, every
/// 24-bit movie, and everything with widescreen off).
pub fn shown_aspect(player: &Player) -> Option<ac3_machine::widescreen::Aspect> {
    let host = player.host.borrow();
    let (_, display, _) = host.shown();
    if display.mode & 0x10 != 0 {
        return None;
    }
    ac3_machine::widescreen::shown_aspect(&player.machine, display.start_x, display.start_y)
}

/// The name of a picture's aspect in the report.
fn aspect_name(aspect: Option<ac3_machine::widescreen::Aspect>) -> &'static str {
    aspect.map_or("4:3", ac3_machine::widescreen::Aspect::name)
}

/// The GPU renderer's display area of `display` (the GP1 state) at `size`,
/// or `None` when the software picture is shown instead (24-bit movies, or
/// an area crossing the VRAM edge, which shows nothing).
fn display_area(
    display: ac3_graphics::gpu::Display,
    [w, h]: [i16; 2],
) -> Option<crate::gpu_renderer::DisplayArea> {
    let area = crate::gpu_renderer::DisplayArea {
        x: u32::from(display.start_x),
        y: u32::from(display.start_y),
        width: w.max(1) as u32,
        height: h.max(1) as u32,
    };
    (display.mode & 0x10 == 0 && area.fits()).then_some(area)
}

/// The GPU renderer's display area of the field the screen shows; the
/// executor shows its copy of the area when the recorder took one
/// ([`crate::gpu_renderer::Recorder::show`]).
fn shown_display(host: &Host) -> Option<crate::gpu_renderer::DisplayArea> {
    let (_, display, size) = host.shown();
    display_area(display, size)
}

/// The picture of a headless GPU-renderer run: the card's N x display
/// area, else the software picture.
fn gpu_picture(
    host: &Host,
    executor: &mut crate::gpu_renderer::Executor,
) -> Option<(Vec<u32>, usize, usize)> {
    shown_display(host)
        .and_then(|area| executor.read_display(area))
        .or_else(|| shown_rgb(host))
}

fn save_png(path: &std::path::Path, rgb: &[u32], w: usize, h: usize) -> crate::Result<()> {
    let writer = std::io::BufWriter::new(File::create(path)?);
    let mut encoder = png::Encoder::new(writer, w as u32, h as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let bytes: Vec<u8> = rgb
        .iter()
        .flat_map(|&p| [(p >> 16) as u8, (p >> 8) as u8, p as u8])
        .collect();
    encoder.write_header()?.write_image_data(&bytes)?;
    Ok(())
}

/// The controller on port 1.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PadType {
    /// A standard digital pad (SCPH-1080), PCSX-Redux's default.
    #[default]
    Digital,
    /// A DualShock, powered on in digital mode; the game switches it to
    /// analog (PadSetMainMode) and sets up its actuators.
    DualShock,
}

impl PadType {
    pub fn parse(name: &str) -> crate::Result<Self> {
        match name.to_ascii_lowercase().as_str() {
            "digital" => Ok(Self::Digital),
            "dualshock" => Ok(Self::DualShock),
            _ => Err(format!("unknown pad type {name}; use digital or dualshock").into()),
        }
    }
    /// libpad's port hardware with this controller on port 1, port 2 empty.
    pub fn ports(self) -> Sio {
        let controller = match self {
            Self::Digital => Controller::digital(),
            Self::DualShock => Controller::dualshock(false),
        };
        Sio::new([Some(controller), None])
    }
}

/// Options of [`run`].
pub struct RunOptions<'a> {
    /// The disc the game boots from (an image or its cue sheet).
    pub disc: &'a std::path::Path,
    /// A two-disc release's other disc; `None`: found next to `disc`
    /// ([`disc_images`]).
    pub disc2: Option<&'a std::path::Path>,
    pub headless: bool,
    /// Stop after this many VBlanks (headless default 3000).
    pub vblanks: Option<u64>,
    /// Held buttons by VBlank (`parse_pad_schedule`), with a window's keys.
    pub pad: Vec<(u64, u64, u16)>,
    /// The controller on port 1. `None`: `controls.pad_type` decides (with
    /// `auto`, a DualShock when a window run finds a gamepad at start, else
    /// a digital pad).
    pub pad_type: Option<PadType>,
    /// Headless: save the displayed picture every this many VBlanks.
    pub snapshot_every: u64,
    pub output: Option<&'a std::path::Path>,
    /// Memory cards 1 and 2.
    pub cards: [CardSource; 2],
    /// The window (`[display]` of the settings); unused when headless.
    pub display: crate::settings::Display,
    /// Flight effects (`[effects]`), with the GPU renderer above 1x.
    pub effects: crate::settings::Effects,
    /// The gamepad (`[controls]` of the settings); read by window runs only.
    pub controls: crate::gamepad::Bindings,
    /// The quick-save slot: F5 saves, F8 loads (`savestate`).
    pub quick_save: std::path::PathBuf,
    /// Save a state to the quick-save slot at the first main-loop boundary
    /// at or after this VBlank (scripted checks).
    pub save_at: Option<u64>,
    /// Start from this save state instead of the cold boot's VBlank 0.
    pub load_state: Option<&'a std::path::Path>,
    /// `--dump-textures DIR`: record every uploaded texture the game draws
    /// and write them (PNG and manifest) to DIR when the run ends.
    pub dump_textures: Option<&'a std::path::Path>,
    /// The movie pack folder (`--movie-pack`, `[paths] movie_pack`):
    /// replacement movie frames, shown on the picture only.
    pub movie_pack: Option<PathBuf>,
}

/// A run in progress: the player, its audio and the loop's bookkeeping,
/// shared by the headless loop and the window.
struct Session {
    player: Player,
    audio: AudioOutput,
    /// Why the run stopped (`None`: still running).
    status: Option<String>,
    /// Stop after this many VBlanks.
    limit: u64,
    /// Wall-clock start of the loop: VBlank `n` is due `n` fields of the
    /// game's field rate after it (20 ms each for PAL).
    started: std::time::Instant,
    /// Wall-clock start of the run, for the report's seconds: unlike
    /// `started`, a stall or a quick load does not restart it.
    opened: std::time::Instant,
    /// When the loop stopped (before the window and audio are torn down).
    finished: Option<std::time::Instant>,
    iterations: u64,
    /// Buttons held by the window's keys and gamepad, read by the pad source
    /// at every VBlank.
    held: Rc<std::cell::Cell<u16>>,
    /// The quick-save slot and the game its states belong to.
    game: crate::savestate::GameId,
    quick: crate::savestate::QuickSlot,
    /// `--save-at`: save to the slot at this VBlank (once).
    save_at: Option<u64>,
    /// The VBlank count at `started` (a quick load restarts the clock).
    base: u64,
}

impl Session {
    fn running(&self) -> bool {
        self.status.is_none() && self.player.vblanks() < self.limit
    }

    /// One main-loop pass, its audio and the device error check.
    fn step(&mut self) -> crate::Result<()> {
        self.scheduled_save();
        {
            let vblank = self.player.vblanks();
            let state = self.player.game.states(&self.player.machine).state;
            if let Some(recorder) = self.player.host.borrow_mut().gpu.texture_dump.as_mut() {
                recorder.set_frame(vblank, state);
            }
        }
        if let Err(fault) = self.player.iteration() {
            self.status = Some(format!("VBlank {}: {fault}", self.player.vblanks()));
        }
        self.iterations += 1;
        let pcm = self.player.host.borrow_mut().take_audio();
        self.audio.submit(&pcm)?;
        if let Some(error) = self.player.host.borrow().error() {
            self.status = Some(format!("VBlank {}: device: {error}", self.player.vblanks()));
        }
        Ok(())
    }

    /// The field clock: when the next main-loop pass is due (VBlanks at
    /// the game's field rate in wall time, 50 per second for PAL).
    fn due(&self) -> std::time::Instant {
        let fields = self.player.vblanks().saturating_sub(self.base);
        self.started + self.player.game.field_rate().duration(fields)
    }

    /// Restart the field clock at the current VBlank: the next pass is due
    /// now (after a stall, the passes it missed are not caught up).
    fn rebase(&mut self) {
        self.started = std::time::Instant::now();
        self.base = self.player.vblanks();
    }

    /// `--save-at`: save to the slot at the first main-loop boundary at or
    /// after its VBlank. A busy memory card retries at the next one.
    fn scheduled_save(&mut self) {
        let Some(at) = self.save_at.filter(|&at| self.player.vblanks() >= at) else {
            return;
        };
        match crate::savestate::save(&self.player, self.game, self.quick.path()) {
            Ok(saved) => {
                eprintln!(
                    "ac3: saved {} (VBlank {})",
                    self.quick.path().display(),
                    saved.vblanks
                );
                self.save_at = None;
            }
            Err(error) => eprintln!("ac3: save at VBlank {at}: {error}"),
        }
    }

    /// F5 (save) or F8 (`load`) at a main-loop boundary. A load restarts
    /// the audio output and the field clock at the state's VBlank. Returns
    /// the note to show.
    fn quick(&mut self, load: bool) -> String {
        let event = self.quick.act(&mut self.player, load);
        if let crate::savestate::QuickEvent::Loaded(loaded) = &event {
            self.audio.flush();
            self.started = std::time::Instant::now();
            self.base = loaded.vblanks;
            self.player.host.borrow_mut().reseed_gpu_renderer();
        }
        let note = event.to_string();
        eprintln!("ac3: {note}");
        note
    }
}

/// The native player on the machine model: translated resident boot from
/// the disc image, then the translated main loop, presented from VRAM
/// (window, or PNGs when headless). Stops at the first fault (for example
/// a routine that is not translated yet) and reports where.
pub fn run(
    data: &crate::assets::GameData,
    options: RunOptions<'_>,
) -> crate::Result<serde_json::Value> {
    use serde_json::json;
    // The game version: everything below goes through it.
    let version = game_for(data, options.disc)?;
    let payload = &data.executable.payload;
    let expected = version.executable().bytes;
    if payload.len() != expected {
        return Err(format!(
            "resident payload is {} bytes, expected {expected} ({})",
            payload.len(),
            version.name()
        )
        .into());
    }
    // Every disc of the release, in disc order (raw images: a cue sheet
    // names its image).
    let (images, booted) = disc_images(version, options.disc, options.disc2)?;
    let mut discs = Vec::with_capacity(images.len());
    for path in &images {
        let image = crate::version::image_path(path).unwrap_or_else(|_| path.clone());
        discs.push(File::open(&image).map_err(|e| {
            format!(
                "{}: {e}; expected the raw {} disc image",
                image.display(),
                version.name()
            )
        })?);
    }
    if images.len() > 1 {
        for (index, path) in images.iter().enumerate() {
            let state = if index == booted {
                " (in the drive)"
            } else {
                ""
            };
            eprintln!("ac3: disc {}: {}{state}", index + 1, path.display());
        }
    }
    // Window runs read gamepads; headless runs never touch them.
    let pads = (!options.headless).then(|| crate::gamepad::Gamepads::new(&options.controls));
    let pad_type = options.pad_type.unwrap_or_else(|| {
        let connected = pads.as_ref().is_some_and(|pads| pads.connected());
        options.controls.pad_type.resolve(connected)
    });
    // Headless runs with the GPU renderer draw offscreen (a window run
    // starts it with the window, on the window's device).
    let gpu_options = options.display.gpu_renderer().filter(|_| options.headless);
    let mut executor = match gpu_options {
        Some(gpu_options) => {
            let executor = crate::gpu_renderer::Executor::offscreen(gpu_options)
                .map_err(|e| format!("GPU renderer: {e}"))?;
            eprintln!("ac3: gpu renderer: {}", executor.description);
            Some(executor)
        }
        None => None,
    };
    let mut host = Host::with_discs(version, Vram::new(), discs, booted);
    host.card =
        CardDevice::from_sources(&options.cards)?.with_clock(version.layout().vblank_counter);
    host.pad = pad_type.ports();
    host.compensate_deadzone = options.controls.compensate_deadzone;
    // The texture pack (the caller resolved the folder), for the GPU
    // renderer's recorder. Without that renderer nothing is replaced.
    host.texture_pack = options.display.texture_pack_dir(std::path::Path::new(""));
    if let Some(folder) = &host.texture_pack
        && options.display.gpu_renderer().is_none()
        && folder.is_dir()
    {
        eprintln!(
            "ac3: texture pack {}: not used, it needs the GPU renderer (internal_resolution \
             above 1, or renderer = \"gpu\")",
            folder.display()
        );
    }
    // The movie pack (display only; the Japanese release's players).
    if let Some(folder) = &options.movie_pack {
        if version.name() == ac3_flight_jp::jp::game::NAME {
            host.movies = Some(Box::new(crate::movie_pack::MovieReplacer::open(
                folder,
                images.clone(),
                options.headless,
            )));
        } else {
            eprintln!(
                "ac3: movie pack {}: not used, movie packs are for the Japanese release",
                folder.display()
            );
        }
    }
    // Sub-pixel vertex positions for the GPU renderer above 1x (the wobble
    // fix): the machine follows the GTE's screen words into this table and
    // the renderer reads it while recording. Never game state.
    let precision = options
        .display
        .gpu_renderer()
        .filter(|gpu| gpu.scale > 1 && options.display.subpixel)
        .map(|_| ac3_machine::precision::PrecisionTable::shared());
    host.vertex_precision = precision.clone().map(|table| {
        Rc::new(crate::gpu_renderer::TablePrecision(table))
            as Rc<dyn crate::gpu_renderer::VertexPrecision>
    });
    // Perspective-correct textures from the table's depths (display only).
    host.perspective = options.display.perspective_textures();
    // Flight effects (display only; off by default, F6 toggles), with
    // widescreen's squeeze for their projection.
    host.effects = Some(options.effects.config(options.display.widescreen.aspect()));
    host.effect_settings = options.effects;
    if let Some(executor) = executor.as_mut() {
        crate::effects::atmosphere_fx::register_card(
            executor,
            crate::effects::atmosphere_fx::Config::from_settings(&options.effects),
        );
        crate::effects::far_terrain::register_card(executor);
        crate::effects::sun::register_card(executor);
    }
    let reserved = options.effects.unimplemented_set();
    if !reserved.is_empty() {
        eprintln!(
            "ac3: effects: {} not implemented yet (they draw nothing)",
            reserved.join(", ")
        );
    }
    if let Some(gpu_options) = gpu_options {
        host.start_gpu_renderer(gpu_options);
    }
    if options.dump_textures.is_some() {
        host.gpu.texture_dump = Some(Box::default());
    }
    let mut player = Player::new(Machine::default(), host);
    // Widescreen (off by default): host configuration, never game state.
    player
        .machine
        .set_widescreen(options.display.widescreen.aspect());
    if let Some(table) = precision {
        player.machine.set_precision(Some(table));
    }
    // The schedule's buttons, plus the keys held in the window.
    let schedule = options.pad.clone();
    let held = Rc::new(std::cell::Cell::new(0u16));
    let held_keys = held.clone();
    player.host.borrow_mut().pad_input = Some(Box::new(move |vblank| {
        schedule
            .iter()
            .filter(|(first, last, _)| (*first..=*last).contains(&vblank))
            .fold(held_keys.get(), |held, (_, _, b)| held | b)
    }));
    if let Some(directory) = options.output {
        std::fs::create_dir_all(directory)?;
    }
    // Window runs play through the default device; headless runs write
    // audio.wav next to the pictures.
    let wav = options
        .output
        .filter(|_| options.headless)
        .map(|directory| directory.join("audio.wav"));
    let audio = AudioOutput::new(!options.headless, wav.as_deref())?;
    version.boot(&mut player.machine, payload);
    let status = match player.machine.take_fault() {
        Ok(()) => None,
        Err(fault) => Some(format!("boot: {fault}")),
    };
    let game = crate::savestate::GameId::of(payload);
    let mut base = 0;
    if let Some(path) = options.load_state {
        let loaded = crate::savestate::load(&mut player, game, path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        eprintln!("ac3: loaded {} (VBlank {})", path.display(), loaded.vblanks);
        base = loaded.vblanks;
        player.host.borrow_mut().reseed_gpu_renderer();
    }
    let limit = options
        .vblanks
        .unwrap_or(if options.headless { 3000 } else { u64::MAX });
    let mut session = Session {
        player,
        audio,
        status,
        limit,
        started: std::time::Instant::now(),
        opened: std::time::Instant::now(),
        finished: None,
        iterations: 0,
        held,
        game,
        quick: crate::savestate::QuickSlot::new(options.quick_save.clone(), game),
        save_at: options.save_at,
        base,
    };
    let mut saved = Vec::new();
    // Widescreen: the aspect each saved picture was drawn for.
    let mut aspects = serde_json::Map::new();
    let mut window_report = None;
    let mut compared = Vec::new();
    if options.headless {
        let mut next_snapshot = options.snapshot_every.max(1);
        while session.running() {
            session.step()?;
            if let Some(executor) = executor.as_mut() {
                let frame = session.player.host.borrow_mut().take_gpu_frame();
                if let Some(frame) = frame {
                    executor.execute(&frame);
                }
                if let Some(error) = executor.effects_failed() {
                    session.player.host.borrow_mut().effects_failed(error);
                }
            }
            // The movie pack: which movie frame the shown field holds.
            let movies = session.player.host.borrow().movies.is_some();
            if movies {
                session.player.host.borrow_mut().movie_update(None);
            }
            let vblanks = session.player.vblanks();
            if let Some(directory) = options.output
                && vblanks >= next_snapshot
            {
                next_snapshot = vblanks + options.snapshot_every.max(1);
                let name = format!("vblank-{vblanks:06}.png");
                let aspect = aspect_name(shown_aspect(&session.player));
                match executor.as_mut() {
                    None => {
                        let picture = shown_rgb(&session.player.host.borrow());
                        if let Some(picture) = picture {
                            let host = &mut session.player.host.borrow_mut();
                            let (rgb, w, h) = movie_picture(host, picture, vblanks);
                            save_png(&directory.join(&name), &rgb, w, h)?;
                            aspects.insert(name.clone(), json!(aspect));
                            saved.push(name);
                        }
                    }
                    Some(executor) => {
                        let picture = gpu_picture(&session.player.host.borrow(), executor);
                        if let Some(picture) = picture {
                            let picture = movie_picture(
                                &mut session.player.host.borrow_mut(),
                                picture,
                                vblanks,
                            );
                            let host = session.player.host.borrow();
                            save_gpu_picture(directory, &name, &picture, &host, &mut compared)?;
                            aspects.insert(name.clone(), json!(aspect));
                            saved.push(name);
                        }
                    }
                }
            }
        }
        session.finished = Some(std::time::Instant::now());
        if let Some(executor) = executor.as_mut() {
            executor.finish();
        }
    } else {
        window_report = Some(window::play(
            &mut session,
            &options.display,
            options.controls.keys.bindings(),
            pads,
            pad_type,
        )?);
    }
    let audio = session.audio.finish()?;
    let final_aspect = aspect_name(shown_aspect(&session.player));
    let player = &mut session.player;
    let states = version.states(&player.machine);
    let vblanks = player.vblanks();
    let final_picture = match executor.as_mut() {
        None => shown_rgb(&player.host.borrow()),
        Some(executor) => gpu_picture(&player.host.borrow(), executor),
    }
    .map(|picture| movie_picture(&mut player.host.borrow_mut(), picture, vblanks));
    let host = player.host.borrow();
    if let (Some(directory), Some(picture)) = (options.output, final_picture) {
        match executor.as_mut() {
            None => save_png(
                &directory.join("final.png"),
                &picture.0,
                picture.1,
                picture.2,
            )?,
            Some(_) => save_gpu_picture(directory, "final.png", &picture, &host, &mut compared)?,
        }
    }
    let texture_dump = match (options.dump_textures, host.gpu.texture_dump.as_deref()) {
        (Some(directory), Some(recorder)) => Some(recorder.write(directory)?),
        _ => None,
    };
    let calls: serde_json::Map<String, serde_json::Value> = host
        .calls
        .iter()
        .map(|(f, n)| (format!("{f:08x}"), json!(n)))
        .collect();
    let mut report = json!({
        "status": session.status.clone().unwrap_or_else(|| "vblank limit".to_owned()),
        "vblanks": player.vblanks(),
        "iterations": session.iterations,
        // Wall time of the loop; the field rate (PAL 50 VBlanks per
        // second, JP 59.94) is real time.
        "seconds": session
            .finished
            .unwrap_or_else(std::time::Instant::now)
            .duration_since(session.opened)
            .as_secs_f64(),
        "game": version.name(),
        "state": states.state, "substate": states.substate,
        "requested_module": states.requested_module as i32,
        "loaded_module": states.loaded_module as i32,
        "pictures": saved,
        "pad_type": format!("{pad_type:?}"),
        "compensate_deadzone": host.compensate_deadzone,
        "memory_cards": {
            "requests": host.card.completed,
            "events": host.card.delivered,
        },
        "discs": {
            "mounted": host.discs().0,
            "in_drive": host.discs().1 + 1,
            "changes": host.disc_changes,
        },
        "field_rate_hz": version.field_rate().hz(),
        "audio": audio,
        "host_calls": calls,
    });
    if let Some(window) = window_report {
        report["window"] = window;
    }
    if let Some(aspect) = options.display.widescreen.aspect() {
        // Pictures stay 320 wide (anamorphic); this says how each is shown.
        let stats = player.machine.widescreen().stats();
        report["widescreen"] = json!({
            "setting": aspect.name(),
            "final": final_aspect,
            "wide_pictures": aspects.values().filter(|a| *a != "4:3").count(),
            "pictures": aspects,
            "last_overlay": {
                "packets": stats.packets,
                "overlay_packets": stats.overlay,
                "squeezed": stats.squeezed,
                "sprites_rebuilt": stats.rebuilt,
                "kept_full_width": stats.full_width,
                "no_room": stats.no_room,
            },
        });
    }
    if let Some(summary) = texture_dump {
        report["texture_dump"] = summary;
    }
    if let Some(movies) = host.movies.as_deref() {
        report["movie_pack"] = movies.report();
        if let Some(replacer) = host
            .hw
            .as_deref()
            .and_then(|hw| hw.replacer())
            .filter(|replacer| replacer.has_movie_images())
        {
            report["movie_pack"]["hub_clips"] = json!(replacer.movie_stats);
        }
    }
    if let (Some(executor), Some(hw)) = (executor.as_ref(), host.hw.as_deref()) {
        let identical = compared
            .iter()
            .filter(|c: &&serde_json::Value| c["differing_pixels"] == 0)
            .count();
        let lowest = compared
            .iter()
            .filter_map(|c| c["identical_percent"].as_f64())
            .fold(100.0f64, f64::min);
        report["gpu_renderer"] = json!({
            "adapter": executor.description,
            "scale": hw.options().scale,
            "dither": hw.options().dither,
            "subpixel": hw.precision.is_some(),
            "perspective": hw.perspective,
            "recorder": hw.stats(),
            "texture_pack": hw.replacer().filter(|r| !r.pack().is_none()).map(|replacer| json!({
                "folder": replacer.pack().root().display().to_string(),
                "replacements": replacer.pack().len(),
                "stats": replacer.stats,
            })),
            "executor": executor.stats,
            "effects": hw.effects_config().map(|config| json!({
                "enabled": config.enabled,
                "debug": config.debug.name(),
                "recorder": hw.effects_stats(),
                "card": executor.effects_stats(),
            })),
            "pictures_compared": compared.len(),
            "pictures_identical": identical,
            "lowest_identical_percent": lowest,
            "compared": compared,
        });
    }
    Ok(report)
}

/// A headless picture with the movie pack's image (for VBlank `vblank`)
/// over the movie frame, at the image's resolution; unchanged without one.
fn movie_picture(
    host: &mut Host,
    (rgb, w, h): (Vec<u32>, usize, usize),
    vblank: u64,
) -> (Vec<u32>, usize, usize) {
    match host.movies.as_deref_mut().and_then(|m| m.at_vblank(vblank)) {
        Some(layer) => {
            let scale = crate::movie_pack::picture_scale(w, &layer);
            crate::movie_pack::composite(&rgb, w, h, &layer, scale)
        }
        None => (rgb, w, h),
    }
}

/// Save a headless GPU-renderer picture as `name`, and beside it the
/// software renderer's (`name` with `-software`); at the same size (1x),
/// note how many pixels are identical.
fn save_gpu_picture(
    directory: &std::path::Path,
    name: &str,
    (rgb, w, h): &(Vec<u32>, usize, usize),
    host: &Host,
    compared: &mut Vec<serde_json::Value>,
) -> crate::Result<()> {
    save_png(&directory.join(name), rgb, *w, *h)?;
    let Some((software, sw, sh)) = shown_rgb(host) else {
        return Ok(());
    };
    let software_name = name.replace(".png", "-software.png");
    save_png(&directory.join(&software_name), &software, sw, sh)?;
    if (sw, sh) == (*w, *h) {
        let differing = rgb.iter().zip(&software).filter(|(a, b)| a != b).count();
        compared.push(serde_json::json!({
            "picture": name,
            "size": [sw, sh],
            "differing_pixels": differing,
            "identical_percent": 100.0 * (rgb.len() - differing) as f64 / rgb.len().max(1) as f64,
        }));
    }
    Ok(())
}

/// The game window: winit's event loop drives the main loop at the game's
/// field rate (PAL 50 Hz) and shows each displayed frame through
/// [`Presenter`](crate::present::Presenter).
///
/// Game time follows the wall clock only: a pass runs when its first VBlank
/// is due ([`Session::due`]), whatever the monitor rate or vsync. If a
/// pass finishes already late for the next one, its frame is not presented
/// (at most [`MAX_SKIPPED`](window::MAX_SKIPPED) in a row), so a blocking
/// present cannot slow the game. A pass more than 200 ms late (a stall) is
/// not caught up: the clock restarts there, so the game neither
/// fast-forwards nor queues the missed passes' audio (a lasting delay).
mod window {
    use super::{PadType, Session, shown_display, shown_rgb};
    use crate::{
        gamepad::{self, Gamepads, PadState},
        present::Presenter,
        settings,
    };
    use std::time::{Duration, Instant};
    use winit::{
        application::ApplicationHandler,
        event::{ElementState, KeyEvent, WindowEvent},
        event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
        keyboard::{KeyCode, PhysicalKey},
        window::WindowId,
    };

    const TITLE: &str = "Ace Combat 3 — native machine";
    /// Most frames left unpresented in a row while behind the clock.
    pub const MAX_SKIPPED: u32 = 3;
    /// Lateness past which the clock restarts instead of catching up.
    const STALL: Duration = Duration::from_millis(200);
    /// How long the quick save / load notes stay up.
    const NOTE: Duration = Duration::from_secs(2);

    /// The most key bindings the window follows (one bit each).
    const MAX_KEYS: usize = 64;

    /// The buttons held by the bindings (`[controls.keys]`,
    /// [`crate::keyboard::KeyMap::bindings`]) whose bits are set in `keys`.
    pub fn held_buttons(bindings: &[(KeyCode, u16)], keys: u64) -> u16 {
        bindings
            .iter()
            .take(MAX_KEYS)
            .enumerate()
            .filter(|(index, _)| keys & (1 << index) != 0)
            .fold(0, |held, (_, (_, button))| held | button)
    }

    /// F5 / F8, handled at the next main-loop boundary.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Quick {
        Save,
        Load,
    }

    struct WindowLoop<'a> {
        session: &'a mut Session,
        display: &'a settings::Display,
        presenter: Option<Presenter>,
        /// The keyboard's bindings, and the held ones (one bit each).
        bindings: Vec<(KeyCode, u16)>,
        keys: u64,
        /// Gamepads (`None` if gilrs could not start) and port 1's
        /// controller, which decides how keys and sticks combine.
        pads: Option<Gamepads>,
        pad_type: PadType,
        /// Input and vibration only while the window has the focus.
        focused: bool,
        quick: Option<Quick>,
        /// F9: reload the texture pack at the next main-loop boundary.
        reload_textures: bool,
        /// F7: change discs (a two-disc release).
        disc_change: bool,
        /// F6: effects on or off at the next main-loop boundary.
        toggle_effects: bool,
        /// Frames left unpresented in a row.
        skipped: u32,
        /// Frames presented and left unpresented in all.
        presented_total: u64,
        skipped_total: u64,
        /// The latest a pass started after its due time.
        most_late: Duration,
        /// Stalls the clock restarted after ([`STALL`]).
        stalls: u64,
        /// The frame-rate line (F3): on or off, and the passes (game
        /// frames) and presented frames counted since `fps_since`.
        show_fps: bool,
        fps_since: Instant,
        fps_counts: (u32, u32),
        /// The movie pack's frame on the screen, for the images between
        /// passes of a pack with several images a frame.
        movie: Option<MovieTiming>,
        error: Option<Box<dyn std::error::Error + Send + Sync>>,
    }

    /// When the shown movie frame appeared, and its images over its time.
    #[derive(Clone, Copy, Debug)]
    struct MovieTiming {
        generation: u64,
        since: Instant,
        /// The frame's time at the movie's rate.
        frame: Duration,
        images: u32,
        /// The image shown (0..images).
        shown: u32,
    }

    impl MovieTiming {
        /// The image due at `now`.
        fn due(&self, now: Instant) -> u32 {
            let elapsed = now.saturating_duration_since(self.since).as_secs_f64();
            let image = elapsed * f64::from(self.images) / self.frame.as_secs_f64().max(1e-3);
            (image as u32).min(self.images - 1)
        }
        /// When the next image is due (`None`: the last is shown).
        fn next(&self) -> Option<Instant> {
            (self.shown + 1 < self.images).then(|| {
                self.since
                    + self
                        .frame
                        .mul_f64(f64::from(self.shown + 1) / f64::from(self.images))
            })
        }
    }

    impl WindowLoop<'_> {
        fn close(&mut self, event_loop: &ActiveEventLoop) {
            if self.session.running() {
                self.session.status = Some("window closed".to_owned());
            }
            event_loop.exit();
        }

        fn fail(
            &mut self,
            event_loop: &ActiveEventLoop,
            error: Box<dyn std::error::Error + Send + Sync>,
        ) {
            self.error = Some(error);
            event_loop.exit();
        }

        /// One main-loop pass at its due time, then its frame.
        fn advance(&mut self) -> crate::Result<()> {
            let Some(presenter) = self.presenter.as_mut() else {
                return Ok(());
            };
            let late = Instant::now().saturating_duration_since(self.session.due());
            self.most_late = self.most_late.max(late);
            // A stall (a slow first GPU frame, or Windows' modal loop while
            // the window is moved or resized) is not caught up: the clock
            // restarts at this pass, as after a quick load. Catching up would
            // run the missed passes back to back, fast-forward the picture,
            // and queue their audio behind the device, delaying every later
            // sound by the stall's length.
            if late > STALL {
                self.session.rebase();
                self.stalls += 1;
            }
            // Quick save / quick load. This is a main-loop boundary: no game
            // code is running, so the whole game state is data here.
            if let Some(request) = self.quick.take() {
                let note = self.session.quick(request == Quick::Load);
                presenter.show_message(note, NOTE);
            }
            if std::mem::take(&mut self.reload_textures) {
                let mut host = self.session.player.host.borrow_mut();
                let mut notes = Vec::new();
                if host.texture_pack.is_some() || host.movies.is_none() {
                    notes.push(host.reload_texture_pack());
                }
                notes.extend(host.reload_movie_pack());
                drop(host);
                presenter.show_message(notes.join("; "), NOTE);
            }
            if std::mem::take(&mut self.disc_change) {
                let note = self.session.player.host.borrow_mut().request_disc_change();
                eprintln!("ac3: {note}");
                presenter.show_message(note, NOTE);
            }
            if std::mem::take(&mut self.toggle_effects) {
                let note = self.session.player.host.borrow_mut().toggle_effects();
                presenter.show_message(note, NOTE);
            }
            // The gamepad (polled even unfocused, so its events do not pile
            // up) and the keys, combined for port 1's controller.
            let pad = match self.pads.as_mut() {
                Some(pads) => Some(pads.poll()).filter(|_| self.focused),
                None => None,
            };
            let (held, sticks) = gamepad::port_input(
                held_buttons(&self.bindings, self.keys),
                pad.unwrap_or(PadState::default()),
                self.pad_type,
            );
            self.session.held.set(held);
            self.session.player.set_sticks(sticks);
            self.session.step()?;
            if let Some(pads) = self.pads.as_mut() {
                if self.focused {
                    pads.rumble(self.session.player.host.borrow().motors());
                } else {
                    pads.stop_rumble();
                }
            }
            // When the next pass is due (the movie pack waits at most until then).
            let next_due = self.session.due();
            let mut host = self.session.player.host.borrow_mut();
            match host.take_gpu_frame() {
                // The GPU renderer: its pass runs on the card, and its N x
                // display area is shown (the software picture for movies).
                Some(frame) => {
                    presenter.execute(&frame);
                    if let Some(error) = presenter.effects_failed() {
                        host.effects_failed(&error);
                    }
                    match shown_display(&host) {
                        Some(area) => presenter.set_vram_frame(area),
                        None => {
                            if let Some((rgb, w, h)) = shown_rgb(&host) {
                                presenter.set_frame(&rgb, w, h);
                            }
                        }
                    }
                }
                None => {
                    if let Some((rgb, w, h)) = shown_rgb(&host) {
                        presenter.set_frame(&rgb, w, h);
                    }
                }
            }
            // The movie pack's image over the movie frame, waiting for its
            // decoding at most until the next pass is due.
            if host.movies.is_some() {
                let now = Instant::now();
                let deadline = next_due.clamp(now, now + crate::movie_pack::MAX_WAIT);
                match host.movie_update(Some(deadline)) {
                    Some(layer) => {
                        let movies = host.movies.as_deref_mut().expect("checked");
                        let field = 1.0 / self.session.player.game.field_rate().hz();
                        let timing = match self.movie {
                            Some(timing) if timing.generation == layer.generation => timing,
                            _ => MovieTiming {
                                generation: layer.generation,
                                since: now,
                                frame: Duration::from_secs_f64(movies.frame_vblanks() * field),
                                images: movies.subframes(),
                                shown: 0,
                            },
                        };
                        let due = timing.due(now);
                        let later = (due > 0).then(|| movies.subframe(due)).flatten();
                        presenter.set_movie(Some(later.as_ref().unwrap_or(&layer)));
                        let shown = if later.is_some() { due } else { 0 };
                        self.movie = Some(MovieTiming { shown, ..timing });
                    }
                    None => {
                        self.movie = None;
                        presenter.set_movie(None);
                    }
                }
            }
            drop(host);
            // Widescreen flight shows at its aspect; everything else 4:3.
            presenter.set_aspect(super::shown_aspect(&self.session.player));
            self.fps_counts.0 += 1;
            if Instant::now() < self.session.due() || self.skipped >= MAX_SKIPPED {
                presenter.render();
                self.skipped = 0;
                self.presented_total += 1;
                self.fps_counts.1 += 1;
            } else {
                self.skipped += 1;
                self.skipped_total += 1;
            }
            self.update_fps();
            Ok(())
        }

        /// Once a second, the frame-rate line: the game's frames (main-loop
        /// passes) per second, and how many reached the screen when fewer.
        fn update_fps(&mut self) {
            let elapsed = self.fps_since.elapsed();
            if elapsed < Duration::from_secs(1) {
                return;
            }
            let rate = |count: u32| (f64::from(count) / elapsed.as_secs_f64()).round();
            let (game, shown) = (rate(self.fps_counts.0), rate(self.fps_counts.1));
            self.fps_since = Instant::now();
            self.fps_counts = (0, 0);
            if let (true, Some(presenter)) = (self.show_fps, self.presenter.as_mut()) {
                presenter.set_status(Some(if shown < game {
                    format!("{game} fps ({shown} shown)")
                } else {
                    format!("{game} fps")
                }));
            }
        }

        /// F3: the frame-rate line on or off.
        fn toggle_fps(&mut self) {
            self.show_fps = !self.show_fps;
            if let Some(presenter) = self.presenter.as_mut() {
                presenter.set_status(self.show_fps.then(|| "... fps".to_owned()));
            }
        }

        fn key(&mut self, event_loop: &ActiveEventLoop, event: &KeyEvent) {
            let PhysicalKey::Code(code) = event.physical_key else {
                return;
            };
            let pressed = event.state == ElementState::Pressed;
            match code {
                KeyCode::Escape if pressed => self.close(event_loop),
                KeyCode::F5 if pressed && !event.repeat => self.quick = Some(Quick::Save),
                KeyCode::F8 if pressed && !event.repeat => self.quick = Some(Quick::Load),
                KeyCode::F3 if pressed && !event.repeat => self.toggle_fps(),
                KeyCode::F9 if pressed && !event.repeat => self.reload_textures = true,
                KeyCode::F7 if pressed && !event.repeat => self.disc_change = true,
                KeyCode::F6 if pressed && !event.repeat => self.toggle_effects = true,
                _ => {
                    for (index, (key, _)) in self.bindings.iter().take(MAX_KEYS).enumerate() {
                        if *key == code {
                            if pressed {
                                self.keys |= 1 << index;
                            } else {
                                self.keys &= !(1 << index);
                            }
                        }
                    }
                }
            }
        }
    }

    impl ApplicationHandler for WindowLoop<'_> {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            if self.presenter.is_some() {
                return;
            }
            match Presenter::new(event_loop, self.display, TITLE) {
                Ok(mut presenter) => {
                    // The GPU renderer records from here on, seeded from the
                    // software GPU (the game booted before the window).
                    let mut host = self.session.player.host.borrow_mut();
                    match presenter.gpu_renderer() {
                        Some(options) => {
                            host.start_gpu_renderer(options);
                            if let Some(executor) = presenter.executor_mut() {
                                crate::effects::atmosphere_fx::register_card(
                                    executor,
                                    crate::effects::atmosphere_fx::Config::from_settings(
                                        &host.effect_settings,
                                    ),
                                );
                                crate::effects::far_terrain::register_card(executor);
                                crate::effects::sun::register_card(executor);
                            }
                        }
                        None => {
                            if let Some(folder) = &host.texture_pack
                                && self.display.gpu_renderer().is_some()
                                && folder.is_dir()
                            {
                                eprintln!(
                                    "ac3: texture pack {}: not used without the GPU renderer",
                                    folder.display()
                                );
                            }
                        }
                    }
                    drop(host);
                    self.presenter = Some(presenter);
                    // The clock starts with the window, not with its set-up.
                    self.session.started = Instant::now();
                    self.session.opened = self.session.started;
                    self.fps_since = Instant::now();
                    if self.show_fps {
                        self.show_fps = false;
                        self.toggle_fps();
                    }
                }
                Err(error) => self.fail(event_loop, format!("window: {error}").into()),
            }
        }

        fn window_event(
            &mut self,
            event_loop: &ActiveEventLoop,
            window: WindowId,
            event: WindowEvent,
        ) {
            let Some(presenter) = self.presenter.as_mut() else {
                return;
            };
            if window != presenter.window_id() || presenter.window_event(&event) {
                return;
            }
            match event {
                WindowEvent::CloseRequested => self.close(event_loop),
                WindowEvent::Focused(focused) => {
                    self.focused = focused;
                    // Keys released while unfocused never arrive.
                    if !focused {
                        self.keys = 0;
                    }
                }
                WindowEvent::KeyboardInput { event, .. } => self.key(event_loop, &event),
                _ => {}
            }
        }

        fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
            if self.presenter.is_none() || self.error.is_some() {
                return;
            }
            if !self.session.running() {
                event_loop.exit();
                return;
            }
            let due = self.session.due();
            if Instant::now() >= due {
                if let Err(error) = self.advance() {
                    return self.fail(event_loop, error);
                }
                if !self.session.running() {
                    event_loop.exit();
                    return;
                }
            }
            // A movie pack with several images a frame: the next image
            // between passes (one not decoded yet is skipped).
            if let Some(timing) = self.movie
                && timing.next().is_some_and(|at| Instant::now() >= at)
            {
                let due = timing.due(Instant::now());
                let layer = self
                    .session
                    .player
                    .host
                    .borrow_mut()
                    .movies
                    .as_deref_mut()
                    .and_then(|movies| movies.subframe(due));
                if let (Some(layer), Some(presenter)) = (layer, self.presenter.as_mut()) {
                    presenter.set_movie(Some(&layer));
                    presenter.render();
                }
                self.movie = Some(MovieTiming {
                    shown: due,
                    ..timing
                });
            }
            let wake = match self.movie.and_then(|timing| timing.next()) {
                Some(at) => at.min(self.session.due()),
                None => self.session.due(),
            };
            event_loop.set_control_flow(ControlFlow::WaitUntil(wake));
        }
    }

    /// Run `session` in a window until it stops or the window closes.
    /// Returns the window's part of the report: the display set-up and how
    /// many frames were presented or skipped.
    pub fn play(
        session: &mut Session,
        display: &settings::Display,
        bindings: Vec<(KeyCode, u16)>,
        pads: Option<Gamepads>,
        pad_type: PadType,
    ) -> crate::Result<serde_json::Value> {
        let event_loop = EventLoop::new().map_err(|e| format!("window: {e}"))?;
        let gamepads = pads.as_ref().map(|pads| pads.names()).unwrap_or_default();
        let mut app = WindowLoop {
            session,
            display,
            presenter: None,
            bindings,
            keys: 0,
            pads,
            pad_type,
            focused: true,
            quick: None,
            reload_textures: false,
            disc_change: false,
            toggle_effects: false,
            skipped: 0,
            presented_total: 0,
            skipped_total: 0,
            most_late: Duration::ZERO,
            stalls: 0,
            show_fps: display.show_fps,
            fps_since: Instant::now(),
            fps_counts: (0, 0),
            movie: None,
            error: None,
        };
        let result = event_loop.run_app(&mut app);
        app.session.finished = Some(Instant::now());
        result.map_err(|e| format!("window: {e}"))?;
        if let Some(error) = app.error {
            return Err(error);
        }
        Ok(serde_json::json!({
            "display": app.presenter.as_ref().map(|p| p.description.clone()),
            "presented": app.presented_total,
            "skipped": app.skipped_total,
            "most_late_ms": app.most_late.as_secs_f64() * 1000.0,
            "stalls": app.stalls,
            "gamepads": gamepads,
        }))
    }

    #[cfg(test)]
    mod tests {
        use super::super::buttons;
        use super::*;

        #[test]
        fn held_keys_hold_their_buttons() {
            let bindings = crate::keyboard::KeyMap::default().bindings();
            let bit = |code| 1 << bindings.iter().position(|(key, _)| *key == code).unwrap();
            assert_eq!(held_buttons(&bindings, 0), 0);
            assert_eq!(held_buttons(&bindings, bit(KeyCode::KeyW)), buttons::R1);
            // Space and J both hold square; with up.
            let keys = bit(KeyCode::Space) | bit(KeyCode::KeyJ) | bit(KeyCode::ArrowUp);
            assert_eq!(held_buttons(&bindings, keys), buttons::SQUARE | buttons::UP);
            assert_eq!(
                held_buttons(&bindings, bit(KeyCode::Enter) | bit(KeyCode::Backspace)),
                buttons::START | buttons::SELECT
            );
            let all = held_buttons(&bindings, u64::MAX);
            assert_eq!(all, 0xfff9, "all fourteen buttons (L3/R3 unbound)");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ac3_flight::{
        SmallData,
        mainloop::{self, libpad},
    };

    const VBLANK_COUNTER: u32 = ac3_flight::game::LAYOUT.vblank_counter;
    const VSYNC_LAST_COUNT: u32 = ac3_flight::game::LAYOUT.vsync_record;

    #[test]
    fn vsync_delivers_the_waited_vblanks_and_pad_data() {
        let mut m = Machine::default();
        m.set_gw(mainloop::PAD_BLOCK, 0x800b_e000);
        // libpad's data (step table, register pointers, port 2's multitap
        // counter) and boot's set-up: PadInitDirect, PadStartCom and
        // PadEnableCom's RAM effects.
        let steps = [
            0x8009_abb0u32,
            0x8009_abf8,
            0x8009_acd0,
            0x8009_ad7c,
            0x8009_ae40,
        ];
        for (i, step) in steps.into_iter().enumerate() {
            m.sw(0x800b_258c + i as u32 * 4, step);
        }
        m.sw(0x800b_2564, libpad::I_STAT);
        for pointer in [0x800b_2568, 0x800b_2580, 0x800b_25ac] {
            m.sw(pointer, libpad::SIO_DATA);
        }
        m.sw(0x800b_2560, u32::MAX);
        ac3_flight::boot::pad_init_direct(&mut m, 0x800b_e000, 0x800b_e022);
        libpad::pad_start_com(&mut m);
        assert_eq!(libpad::pad_enable_com(&mut m, 3), 1);
        m.sw(VSYNC_LAST_COUNT, 10);
        m.sw(VBLANK_COUNTER, 10);
        let mut player = Player::new(m, Host::new(Vram::new(), None));
        player.host.borrow_mut().pad_running = Some(true);
        player.set_buttons(0x0010);
        // VSync(2) from count 10: target 11, then one more.
        let elapsed = ac3_flight::sdk::vsync(&mut player.machine, 2);
        assert_eq!(elapsed, 0);
        let m = &mut player.machine;
        assert_eq!(m.lw(VBLANK_COUNTER), 12);
        assert_eq!(
            m.lw(VSYNC_LAST_COUNT),
            12,
            "VSync's record matches the counter"
        );
        assert_eq!(ac3_flight::sdk::vsync(m, u32::MAX), 12);
        // The second VBlank stores the first one's reply.
        assert_eq!(m.lbu(0x800b_e000), 0);
        assert_eq!(m.lbu(0x800b_e001), 0x41);
        assert_eq!(m.lhu(0x800b_e002), !0x0010);
        assert_eq!(m.lbu(0x800b_e022), 0xff, "port 2 empty");
        assert_eq!(m.lw(libpad::VSYNC_FLAG), 1);
        assert!(m.take_fault().is_ok());
        assert_eq!(player.vblanks(), 2);
        assert_eq!(player.host.borrow().error(), None);
        // Detection gives up on configuration mode and settles on a digital
        // pad: state 2, controller ID 4.
        for _ in 0..8 {
            ac3_flight::sdk::vsync(&mut player.machine, 2);
        }
        let m = &mut player.machine;
        assert_eq!(mainloop::pending_sdk::pad_get_state(m, 0), 2);
        assert_eq!(mainloop::pending_sdk::pad_info_mode(m, 0, 1, 0), 4);
    }

    #[test]
    fn pad_schedule_parses_ranges_and_chords() {
        let schedule = parse_pad_schedule("300-304:cross, 500:up+l1").unwrap();
        assert_eq!(
            schedule,
            vec![
                (300, 304, buttons::CROSS),
                (500, 500, buttons::UP | buttons::L1)
            ]
        );
        assert!(parse_pad_schedule("1-2:jump").is_err());
    }

    #[test]
    fn each_release_has_its_device_tables() {
        assert_eq!(HostTables::of(pal()), HostTables::PAL);
        assert_eq!(HostTables::of(jp()), HostTables::JP);
        let host = Host::with_game(jp(), Vram::new(), None);
        assert_eq!(host.spu.release(), (LibSpu::JP, FieldLength::NTSC));
        assert_eq!(host.mdec.release(), DefaultTables::JP);
        assert_eq!(host.discs(), (0, 0));
        let host = Host::new(Vram::new(), None);
        assert_eq!(host.spu.release(), (LibSpu::PAL, FieldLength::PAL));
        assert_eq!(host.mdec.release(), DefaultTables::PAL);
        // One-disc releases mount the disc given, whatever is next to it.
        let (images, booted) = disc_images(pal(), Path::new("x.bin"), None).unwrap();
        assert_eq!((images, booted), (vec![PathBuf::from("x.bin")], 0));
    }

    #[test]
    fn a_two_disc_host_changes_discs_when_the_game_waits_for_the_lid() {
        use ac3_machine::cd::LID_WAIT_POLLS;
        // Two raw images: every sector's first data byte is the disc number.
        let folder = std::env::temp_dir().join(format!("ac3-discs-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let files: Vec<File> = (1..=2u8)
            .map(|disc| {
                let path = folder.join(format!("disc{disc}.bin"));
                let mut raw = vec![0u8; 32 * 2352];
                for sector in raw.chunks_mut(2352) {
                    sector[24] = disc;
                }
                std::fs::write(&path, raw).unwrap();
                File::open(path).unwrap()
            })
            .collect();
        let mut player = Player::new(
            Machine::default(),
            Host::with_discs(jp(), Vram::new(), files, 0),
        );
        let read = |m: &mut Machine| {
            // CdControlB(Setloc 00:02:05), CdRead(1 sector): the boot's device.
            m.write_bytes(0x8010_0000, &[0x00, 0x02, 0x05]);
            m.host_call(host_ids::CD_CONTROL_B, [2, 0x8010_0000, 0, 0]);
            m.host_call(host_ids::CD_READ, [1, 0x8010_1000, 0, 0]);
            m.lbu(0x8010_1000)
        };
        assert_eq!(read(&mut player.machine), 1);
        // The Japanese CD monitor's wait: Setmode 0, Stop, then Getstats.
        let m = &mut player.machine;
        m.host_call(host_ids::CD_COMMAND, [0x0e, 1, 0, 0]);
        m.host_call(host_ids::CD_COMMAND, [0x08, 0, 0, 0]);
        for _ in 0..LID_WAIT_POLLS - 1 {
            m.host_call(host_ids::CD_COMMAND, [0x01, 0, 0, 0]);
        }
        assert_eq!(player.host.borrow().disc_changes, 0);
        player
            .machine
            .host_call(host_ids::CD_COMMAND, [0x01, 0, 0, 0]);
        {
            let host = player.host.borrow();
            assert_eq!(host.disc_changes, 1);
            assert_eq!(host.discs(), (2, 1));
            let lid = host.cd.as_ref().unwrap().lid();
            assert!(matches!(lid, ac3_machine::cd::Lid::Open { .. }), "{lid:?}");
        }
        assert_eq!(read(&mut player.machine), 2, "the new disc's sectors");
        // F7: back to disc 1 at the next host call.
        let note = player.host.borrow_mut().request_disc_change();
        assert_eq!(note, "Changing to disc 1");
        assert_eq!(read(&mut player.machine), 1);
        assert_eq!(player.host.borrow().disc_changes, 2);
        assert!(player.machine.take_fault().is_ok());
        assert_eq!(player.host.borrow().error(), None);
        let single = Host::with_game(jp(), Vram::new(), None).request_disc_change();
        assert_eq!(single, "Only one disc is mounted");
        std::fs::remove_dir_all(&folder).unwrap();
    }

    /// The JP credits (state 7): a pass draws the single 480i buffer before
    /// its VSync wait, and frame end's PutDrawEnv clears it after. The
    /// screen shows the field after the VBlank (the credits), not the
    /// display after the pass (black), in both renderers.
    #[test]
    fn the_screen_shows_the_field_after_the_vblank() {
        use crate::gpu_renderer::{Op, Options, Shown, tiles::Rect};
        let host = Host::with_game(jp(), Vram::new(), None);
        let mut player = Player::new(Machine::default(), host);
        player.host.borrow_mut().start_gpu_renderer(Options {
            scale: 2,
            dither: false,
        });
        // 320x480 interlaced at (0, 0): the display start, 240 lines, mode.
        for word in [0x0500_0000, 0x0704_0010, 0x0800_0025] {
            player.machine.host_call(host_ids::GP1, [word, 0, 0, 0]);
        }
        // Two one-packet lists, full-screen fills: red (the credits) and
        // black (the clear).
        let (credits, clear) = (0x8010_0000, 0x8010_0100);
        for (list, rgb) in [(credits, 0xff), (clear, 0)] {
            for (i, word) in [0x03ff_ffff, 0x0200_0000 | rgb, 0, 0x01e0_0140]
                .into_iter()
                .enumerate()
            {
                player.machine.sw(list + 4 * i as u32, word);
            }
        }
        let draw = |player: &mut Player, list: u32| {
            player
                .machine
                .host_call(host_ids::DMA_LIST, [list, 0, 0, 0]);
        };
        let vsync = |player: &mut Player| {
            player.machine.host_call(host_ids::VSYNC, [0, 0, 0, 0]);
        };
        // The shown picture's one colour.
        let shown = |player: &Player| {
            let (rgb, w, h) = shown_rgb(&player.host.borrow()).expect("picture");
            assert_eq!((w, h), (320, 480));
            assert!(rgb.iter().all(|&p| p == rgb[0]), "one colour");
            rgb[0]
        };
        const RED: u32 = 0x00ff_0000;
        // The credits pass: drawn, the VBlank, the clear.
        draw(&mut player, credits);
        vsync(&mut player);
        draw(&mut player, clear);
        assert_eq!(shown(&player), RED, "the field: the credits");
        {
            let host = player.host.borrow();
            let [_, _, w, h] = host.gpu.display;
            let (now, ..) =
                picture_rgb(host.gpu.gpu.vram().words(), host.gpu.gpu.display(), [w, h])
                    .expect("display");
            assert!(now.iter().all(|&p| p == 0), "the display after the pass");
        }
        let frame = player.host.borrow_mut().take_gpu_frame().expect("recorder");
        assert_eq!(frame.shown, Shown::Copy);
        assert!(frame.ops.contains(&Op::Show(Rect::new(0, 0, 320, 480))));
        // The next pass alike, then a VBlank with no write after it: the
        // display as it is, not the earlier field.
        draw(&mut player, credits);
        vsync(&mut player);
        draw(&mut player, clear);
        assert_eq!(shown(&player), RED);
        vsync(&mut player);
        assert_eq!(shown(&player), 0, "the display now");
        let frame = player.host.borrow_mut().take_gpu_frame().expect("recorder");
        assert_eq!(frame.shown, Shown::Live);
        // A screen drawn after the clear (the hub, the terminal) shows the
        // previous pass's picture: the field the VBlank left.
        vsync(&mut player);
        draw(&mut player, clear);
        draw(&mut player, credits);
        assert_eq!(shown(&player), 0, "the previous pass's picture");
        vsync(&mut player);
        draw(&mut player, clear);
        draw(&mut player, credits);
        assert_eq!(shown(&player), RED);
        // After a load the screen shows the loaded display.
        vsync(&mut player);
        draw(&mut player, clear);
        assert_eq!(shown(&player), RED);
        player.host.borrow_mut().reset_scanout();
        assert_eq!(shown(&player), 0);
        assert!(player.machine.take_fault().is_ok());
        assert_eq!(player.host.borrow().error(), None);
    }

    #[test]
    fn image_transfers_round_trip_through_vram() {
        let mut m = Machine::default();
        // RECT (8, 4, 3, 1) and three pixels.
        for (i, half) in [8u16, 4, 3, 1].into_iter().enumerate() {
            m.sh(0x8010_0000 + i as u32 * 2, half);
        }
        m.sh(0x8010_0100, 0x1234);
        m.sh(0x8010_0102, 0x0567);
        m.sh(0x8010_0104, 0x7fff);
        let mut player = Player::new(m, Host::new(Vram::new(), None));
        let m = &mut player.machine;
        m.host_call(calls::LOAD_IMAGE, [0x8010_0000, 0x8010_0100, 0, 0]);
        m.host_call(calls::STORE_IMAGE, [0x8010_0000, 0x8010_0200, 0, 0]);
        assert_eq!(m.lhu(0x8010_0200), 0x1234);
        assert_eq!(m.lhu(0x8010_0202), 0x0567);
        assert_eq!(m.lhu(0x8010_0204), 0x7fff);
        assert_eq!(player.host.borrow().error(), None);
        let host = player.host.borrow();
        assert_eq!(host.gpu.gpu.vram().words()[4 * 1024 + 9], 0x0567);
    }
}
