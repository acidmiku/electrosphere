//! Quick save and quick load: the whole running game in one file.
//!
//! A state is taken between two [`Player::iteration`] calls, at a main-loop
//! boundary. The Rust call stack is empty there, so the game is all data:
//!
//! - the machine ([`MachineState`]): RAM, scratchpad, GTE, guest stack
//!   pointer, queued host results and the fault;
//! - every device of the [`Host`]: the GPU (VRAM, drawing and display state,
//!   a partial command or transfer), the SPU (sound RAM, registers, voices,
//!   the CD input and the mixer's clock), the CD drive (position, pending
//!   responses and sectors, the sector buffer, the lid, and which disc of
//!   a two-disc release is in it), the boot disc device's seek
//!   position, the MDEC, the XA queue and decoder, the busy-wait clock, the
//!   kernel's events and the memory-card driver (requests in progress, open
//!   files), the controller ports, plus the held buttons, libpad's running
//!   flag, the VBlank count and the audio mixed but not yet taken.
//!
//! Not part of a state: the audio output device's buffer (the caller
//! flushes it), the window, settings, [`Host::calls`] statistics, the
//! machine's host-call log of the last pass, the pad input source (it maps
//! the restored VBlank count to buttons), and the memory-card files. As in
//! emulators, a load does not roll the cards back. While a card request is
//! in flight, [`capture`] (and so [`save`]) refuses with [`Error::CardBusy`]:
//! the kernel's view of the card would not match the card after a later
//! load.
//!
//! [`restore`] decodes and checks the whole state before it changes the
//! player, and keeps every link: the machine's device hook, the CD drive's
//! XA sink (which feeds the host's shared [`XaQueue`]), the disc files and
//! the pad input source. The only thread-local state of the translated code,
//! `ov2_briefing`'s external replay hook, is a test fixture the player never
//! installs.
//!
//! # File format
//!
//! Little-endian, 64-byte header, then the payload:
//!
//! | Offset | Bytes | Field |
//! | --- | --- | --- |
//! | 0 | 8 | [`MAGIC`] `AC3STATE` |
//! | 8 | 4 | [`FORMAT_VERSION`] |
//! | 12 | 32 | the game: SHA-256 of the resident payload ([`GameId`]) |
//! | 44 | 8 | VBlank count |
//! | 52 | 4 | compression: 1 zlib (miniz_oxide), 0 none |
//! | 56 | 8 | payload bytes before compression |
//! | 64 | .. | payload |
//!
//! The payload is a postcard-encoded list of named sections (`machine`,
//! `host.gpu`, `host.spu`, ...), each the postcard encoding of that part.
//! Loading refuses another magic, another version and another game before it
//! decompresses anything; every section must be present, decode completely
//! and be used. The encoding follows the devices' field layouts, so
//! [`FORMAT_VERSION`] changes with any serialized device structure (the
//! `layout_is_pinned` test fails until it does). [`save`] writes a
//! temporary file next to the target and renames it over the target.
//!
//! Measured on the scripted cold boot (`tests/savestate.rs`): files of
//! 1.2 MB (language screen, attract movie) to 2.1 MB (flight), from 3.4 to
//! 3.9 MB of payload; a save takes 35 to 51 ms, a load 18 to 20 ms.
use crate::flight_host::GpuDevice;
use crate::machine_audio::{SPU_RAM_BYTES, SpuDevice};
use crate::machine_card::CardState;
use crate::machine_mdec::MdecDevice;
use crate::machine_movie::{BusyClock, XaPlayer, XaQueue};
use crate::machine_player::{Host, Player};
use ac3_graphics::{VRAM_HEIGHT, VRAM_WIDTH};
use ac3_machine::{
    cd::CdDriveState,
    disc::DiscDeviceState,
    machine::{MachineState, RAM_BYTES, SCRATCH_BYTES},
    sio::Sio,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    collections::BTreeMap,
    fmt, fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

/// File magic.
pub const MAGIC: [u8; 8] = *b"AC3STATE";
/// Version of the file layout and of every serialized device structure.
///
/// - 1: Milestone 1.
/// - 2: the CD drive's state gained the lid (open, closing in a disc
///   change, spinning up), the Getstat polls after a Stop (the game waiting
///   for the lid) and the disc in the drive (the Japanese release's two
///   discs). Version 1 states do not load.
pub const FORMAT_VERSION: u32 = 2;
/// Header bytes before the payload.
pub const HEADER_BYTES: usize = 64;
const COMPRESSION_NONE: u32 = 0;
const COMPRESSION_ZLIB: u32 = 1;
/// zlib level of [`State::to_bytes`] (miniz_oxide: 0 none .. 10 best).
const ZLIB_LEVEL: u8 = 1;
/// Largest payload accepted (a state is 3.4 to 3.9 MB before compression).
const MAX_PAYLOAD: u64 = 64 << 20;

/// The game data a state belongs to: SHA-256 of the resident payload.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct GameId(pub [u8; 32]);

impl GameId {
    /// The identity of `payload` (`GameData::executable.payload`).
    pub fn of(payload: &[u8]) -> Self {
        use sha2::{Digest, Sha256};
        Self(Sha256::digest(payload).into())
    }
}
impl fmt::Display for GameId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
    }
}
impl fmt::Debug for GameId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "GameId({self})")
    }
}

/// Why a state was not taken, written or loaded.
#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    /// The file does not start with [`MAGIC`].
    NotAState,
    /// The file has another format version.
    Version {
        found: u32,
        supported: u32,
    },
    /// The state belongs to other game data.
    WrongGame {
        found: GameId,
        expected: GameId,
    },
    /// A memory-card request is in progress; save again in a moment.
    CardBusy,
    /// A device has failed; the game is stopping.
    Device(String),
    /// The player cannot take this state (a disc drive in one and not in
    /// the other) or is borrowed.
    Player(String),
    /// The state does not decode: a damaged file or another build's layout.
    Corrupt(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "save state file: {error}"),
            Self::NotAState => write!(f, "not an AC3 save state"),
            Self::Version { found, supported } => write!(
                f,
                "save state format version {found}, this build reads version {supported}"
            ),
            Self::WrongGame { found, expected } => write!(
                f,
                "save state of other game data (resident payload {found}, running {expected})"
            ),
            Self::CardBusy => write!(
                f,
                "memory card busy (a card request is in progress); save again in a moment"
            ),
            Self::Device(error) => write!(f, "device error, not saving: {error}"),
            Self::Player(error) => write!(f, "save state does not fit this player: {error}"),
            Self::Corrupt(error) => write!(f, "damaged save state: {error}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// A captured state in memory: the game, the VBlank count and the encoded
/// sections. [`restore`] can apply it any number of times. Two states are
/// equal when every part of the game is.
#[derive(Clone, PartialEq, Eq)]
pub struct State {
    game: GameId,
    vblanks: u64,
    sections: Vec<Section>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Section {
    name: String,
    data: Vec<u8>,
}

impl State {
    pub fn game(&self) -> GameId {
        self.game
    }
    /// VBlanks delivered when the state was taken.
    pub fn vblanks(&self) -> u64 {
        self.vblanks
    }
    /// Section names and encoded sizes, in order.
    pub fn sections(&self) -> impl Iterator<Item = (&str, usize)> {
        self.sections
            .iter()
            .map(|s| (s.name.as_str(), s.data.len()))
    }
    /// Names of the sections that differ from `other`'s (or that only one
    /// of the two has): which parts of two runs diverged.
    pub fn differing_sections(&self, other: &Self) -> Vec<String> {
        let theirs: BTreeMap<&str, &[u8]> = other
            .sections
            .iter()
            .map(|s| (s.name.as_str(), s.data.as_slice()))
            .collect();
        let mut names: Vec<String> = self
            .sections
            .iter()
            .filter(|s| theirs.get(s.name.as_str()) != Some(&s.data.as_slice()))
            .map(|s| s.name.clone())
            .collect();
        names.extend(
            other
                .sections
                .iter()
                .filter(|s| !self.sections.iter().any(|mine| mine.name == s.name))
                .map(|s| s.name.clone()),
        );
        names
    }

    /// The file image: header and zlib-compressed payload.
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        let payload = postcard::to_allocvec(&self.sections)
            .map_err(|e| Error::Corrupt(format!("encoding sections: {e}")))?;
        let compressed = miniz_oxide::deflate::compress_to_vec_zlib(&payload, ZLIB_LEVEL);
        let mut bytes = Vec::with_capacity(HEADER_BYTES + compressed.len());
        bytes.extend_from_slice(&MAGIC);
        bytes.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        bytes.extend_from_slice(&self.game.0);
        bytes.extend_from_slice(&self.vblanks.to_le_bytes());
        bytes.extend_from_slice(&COMPRESSION_ZLIB.to_le_bytes());
        bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        debug_assert_eq!(bytes.len(), HEADER_BYTES);
        bytes.extend_from_slice(&compressed);
        Ok(bytes)
    }

    /// Read a file image, refusing another magic, version or game first.
    pub fn from_bytes(bytes: &[u8], game: GameId) -> Result<Self, Error> {
        let field = |at: usize, n: usize| bytes.get(at..at + n);
        let truncated = || Error::Corrupt(format!("{} bytes: truncated header", bytes.len()));
        if field(0, 8) != Some(&MAGIC[..]) {
            return Err(Error::NotAState);
        }
        let u32_at = |at| field(at, 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
        let u64_at = |at| field(at, 8).map(|b| u64::from_le_bytes(b.try_into().unwrap()));
        let version = u32_at(8).ok_or_else(truncated)?;
        if version != FORMAT_VERSION {
            return Err(Error::Version {
                found: version,
                supported: FORMAT_VERSION,
            });
        }
        let header = field(0, HEADER_BYTES).ok_or_else(truncated)?;
        let found = GameId(header[12..44].try_into().unwrap());
        if found != game {
            return Err(Error::WrongGame {
                found,
                expected: game,
            });
        }
        let vblanks = u64_at(44).ok_or_else(truncated)?;
        let compression = u32_at(52).ok_or_else(truncated)?;
        let length = u64_at(56).ok_or_else(truncated)?;
        if length > MAX_PAYLOAD {
            return Err(Error::Corrupt(format!("payload of {length} bytes")));
        }
        let body = &bytes[HEADER_BYTES..];
        let payload = match compression {
            COMPRESSION_ZLIB => {
                miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(body, length as usize)
                    .map_err(|e| Error::Corrupt(format!("decompressing: {:?}", e.status)))?
            }
            COMPRESSION_NONE => body.to_vec(),
            other => return Err(Error::Corrupt(format!("compression {other}"))),
        };
        if payload.len() as u64 != length {
            return Err(Error::Corrupt(format!(
                "payload is {} bytes, header says {length}",
                payload.len()
            )));
        }
        let sections = decode_all::<Vec<Section>>("sections", &payload)?;
        Ok(Self {
            game,
            vblanks,
            sections,
        })
    }
}

/// `data` as one complete value.
fn decode_all<T: DeserializeOwned>(name: &str, data: &[u8]) -> Result<T, Error> {
    let (value, rest) = postcard::take_from_bytes::<T>(data)
        .map_err(|e| Error::Corrupt(format!("section {name}: {e}")))?;
    if !rest.is_empty() {
        return Err(Error::Corrupt(format!(
            "section {name}: {} bytes left over",
            rest.len()
        )));
    }
    Ok(value)
}

#[derive(Default)]
struct Writer(Vec<Section>);
impl Writer {
    fn put<T: Serialize + ?Sized>(&mut self, name: &str, value: &T) -> Result<(), Error> {
        let data = postcard::to_allocvec(value)
            .map_err(|e| Error::Corrupt(format!("encoding {name}: {e}")))?;
        self.0.push(Section {
            name: name.to_owned(),
            data,
        });
        Ok(())
    }
}

struct Reader<'a>(BTreeMap<&'a str, &'a [u8]>);
impl<'a> Reader<'a> {
    fn new(sections: &'a [Section]) -> Result<Self, Error> {
        let mut map = BTreeMap::new();
        for section in sections {
            if map
                .insert(section.name.as_str(), section.data.as_slice())
                .is_some()
            {
                return Err(Error::Corrupt(format!("section {} twice", section.name)));
            }
        }
        Ok(Self(map))
    }
    fn take<T: DeserializeOwned>(&mut self, name: &str) -> Result<T, Error> {
        let data = self
            .0
            .remove(name)
            .ok_or_else(|| Error::Corrupt(format!("no section {name}")))?;
        decode_all(name, data)
    }
    /// Every section was used.
    fn finish(self) -> Result<(), Error> {
        match self.0.keys().next() {
            Some(name) => Err(Error::Corrupt(format!("unknown section {name}"))),
            None => Ok(()),
        }
    }
}

/// The [`Host`] fields a state holds whole, one section each
/// (`host.<field>`). A new Host field that affects the game's future goes
/// in this list; fields with files or shared links (`disc`, `cd`, `xa`,
/// `card`) are handled by [`capture`] and [`restore`] themselves.
macro_rules! whole_host_fields {
    ($($field:ident: $type:ty),* $(,)?) => {
        struct WholeFields {
            $($field: $type,)*
        }
        impl WholeFields {
            fn put(host: &Host, writer: &mut Writer) -> Result<(), Error> {
                $(writer.put(concat!("host.", stringify!($field)), &host.$field)?;)*
                Ok(())
            }
            fn take(reader: &mut Reader<'_>) -> Result<Self, Error> {
                Ok(Self {
                    $($field: reader.take(concat!("host.", stringify!($field)))?,)*
                })
            }
            fn apply(self, host: &mut Host) {
                $(host.$field = self.$field;)*
            }
        }
    };
}

whole_host_fields! {
    gpu: GpuDevice,
    spu: SpuDevice,
    mdec: MdecDevice,
    xa_player: XaPlayer,
    busy: BusyClock,
    audio: Vec<[i16; 2]>,
    buttons: u16,
    sticks: [u8; 4],
    pad: Sio,
    pad_running: Option<bool>,
    vblanks: u64,
}

/// Take the player's state (between two [`Player::iteration`] calls).
/// Refuses while a memory-card request is in progress
/// ([`Error::CardBusy`]) or after a device error.
pub fn capture(player: &Player, game: GameId) -> Result<State, Error> {
    let host = player
        .host
        .try_borrow()
        .map_err(|_| Error::Player("the host is in use".into()))?;
    if let Some(error) = host.error() {
        return Err(Error::Device(error.to_owned()));
    }
    if host.card.busy() {
        return Err(Error::CardBusy);
    }
    let mut writer = Writer::default();
    writer.put("machine", &player.machine.snapshot())?;
    WholeFields::put(&host, &mut writer)?;
    writer.put("host.disc", &host.disc.as_ref().map(|d| d.snapshot()))?;
    writer.put("host.cd", &host.cd.as_ref().map(|d| d.snapshot()))?;
    let xa = host
        .xa
        .try_borrow()
        .map_err(|_| Error::Player("the XA queue is in use".into()))?;
    writer.put("host.xa", &*xa)?;
    writer.put("host.card", &host.card.snapshot())?;
    Ok(State {
        game,
        vblanks: host.vblanks,
        sections: writer.0,
    })
}

/// Replace the player's state with `state` (between two
/// [`Player::iteration`] calls). The whole state is decoded and checked
/// first; on an error the player is unchanged. The memory cards, the pad
/// input source, the host-call statistics and every device link stay.
pub fn restore(player: &mut Player, state: &State, game: GameId) -> Result<(), Error> {
    if state.game != game {
        return Err(Error::WrongGame {
            found: state.game,
            expected: game,
        });
    }
    let mut reader = Reader::new(&state.sections)?;
    let machine: MachineState = reader.take("machine")?;
    let whole = WholeFields::take(&mut reader)?;
    let disc: Option<DiscDeviceState> = reader.take("host.disc")?;
    let cd: Option<CdDriveState> = reader.take("host.cd")?;
    let xa: XaQueue = reader.take("host.xa")?;
    let card: CardState = reader.take("host.card")?;
    reader.finish()?;

    let sizes = [
        ("RAM", machine.ram.len(), RAM_BYTES),
        ("scratchpad", machine.scratch.len(), SCRATCH_BYTES),
        (
            "VRAM",
            whole.gpu.gpu.vram().words().len(),
            VRAM_WIDTH * VRAM_HEIGHT,
        ),
        ("sound RAM", whole.spu.sound_ram().len(), SPU_RAM_BYTES),
    ];
    for (name, found, expected) in sizes {
        if found != expected {
            return Err(Error::Corrupt(format!(
                "{name} is {found} bytes or words, expected {expected}"
            )));
        }
    }
    if whole.vblanks != state.vblanks {
        return Err(Error::Corrupt(format!(
            "VBlank count {} in the header, {} in the host",
            state.vblanks, whole.vblanks
        )));
    }
    let mut host = player
        .host
        .try_borrow_mut()
        .map_err(|_| Error::Player("the host is in use".into()))?;
    let fits = |name: &str, saved: bool, here: bool| match (saved, here) {
        (true, false) => Err(Error::Player(format!(
            "the state has a {name}, the player not"
        ))),
        (false, true) => Err(Error::Player(format!(
            "the player has a {name}, the state not"
        ))),
        _ => Ok(()),
    };
    fits("disc device", disc.is_some(), host.disc.is_some())?;
    fits("CD drive", cd.is_some(), host.cd.is_some())?;
    // The disc in the state's drive must be one this player has mounted.
    if let (Some(drive), Some(state)) = (host.cd.as_ref(), cd.as_ref()) {
        drive.fits(state).map_err(Error::Player)?;
    }
    let xa_queue = host.xa.clone();
    let mut queue = xa_queue
        .try_borrow_mut()
        .map_err(|_| Error::Player("the XA queue is in use".into()))?;

    // Checked; nothing below fails.
    player.machine.restore(machine).map_err(Error::Corrupt)?;
    // A running texture dump is host-side observation; it survives a load.
    let texture_dump = host.gpu.texture_dump.take();
    whole.apply(&mut host);
    host.gpu.texture_dump = texture_dump;
    // The screen shows the loaded display until the next VBlank.
    host.reset_scanout();
    // The release's device tables are configuration, not state.
    host.apply_release();
    if let (Some(device), Some(state)) = (host.disc.as_mut(), disc) {
        device.restore(state);
    }
    if let (Some(drive), Some(state)) = (host.cd.as_mut(), cd) {
        drive.restore(state);
        // The state's disc goes in; the boot's disc device reads it too.
        host.sync_disc_device();
    }
    // In place: the CD drive's sink shares this queue.
    *queue = xa;
    host.card.restore(card);
    Ok(())
}

/// What [`save`] wrote or [`load`] read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Summary {
    /// VBlank count of the state.
    pub vblanks: u64,
    /// File size.
    pub file_bytes: u64,
    /// Payload size before compression.
    pub payload_bytes: u64,
}

fn summary(bytes: &[u8]) -> Summary {
    let u64_at = |at: usize| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
    Summary {
        vblanks: u64_at(44),
        file_bytes: bytes.len() as u64,
        payload_bytes: u64_at(56),
    }
}

/// [`capture`] the player and write the state to `path` atomically (a
/// temporary file next to it, then a rename over it). Creates the folder.
pub fn save(player: &Player, game: GameId, path: &Path) -> Result<Summary, Error> {
    let bytes = capture(player, game)?.to_bytes()?;
    if let Some(folder) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(folder)?;
    }
    write_atomically(path, &bytes)?;
    Ok(summary(&bytes))
}

/// Read the state at `path` and [`restore`] it into the player. The caller
/// flushes its audio output (the device buffer is not part of a state).
pub fn load(player: &mut Player, game: GameId, path: &Path) -> Result<Summary, Error> {
    let bytes = fs::read(path)?;
    let state = State::from_bytes(&bytes, game)?;
    restore(player, &state, game)?;
    Ok(summary(&bytes))
}

fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut name = path.as_os_str().to_owned();
    name.push(".tmp");
    let temporary = PathBuf::from(name);
    let written = (|| {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    written
}

/// The one quick-save slot behind the save key (F5) and the load key (F8).
/// [`QuickSlot::poll`] acts once per key press.
pub struct QuickSlot {
    path: PathBuf,
    game: GameId,
    held: [bool; 2],
}

/// What a quick-save key press did.
#[derive(Debug)]
pub enum QuickEvent {
    Saved(Summary),
    /// The state is loaded: flush the audio output and restart frame
    /// pacing from its VBlank count.
    Loaded(Summary),
    SaveFailed(Error),
    LoadFailed(Error),
}

impl fmt::Display for QuickEvent {
    /// A short note for the screen or the log.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Saved(saved) => write!(f, "quick save (VBlank {})", saved.vblanks),
            Self::Loaded(loaded) => write!(f, "quick load (VBlank {})", loaded.vblanks),
            Self::SaveFailed(error) => write!(f, "quick save failed: {error}"),
            Self::LoadFailed(Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                write!(f, "quick load: no quick save yet")
            }
            Self::LoadFailed(error) => write!(f, "quick load failed: {error}"),
        }
    }
}

impl QuickSlot {
    pub fn new(path: PathBuf, game: GameId) -> Self {
        Self {
            path,
            game,
            held: [false; 2],
        }
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// Call between two [`Player::iteration`] calls with whether the save
    /// and the load key are held: a key that went down saves or loads (a
    /// save when both did).
    pub fn poll(
        &mut self,
        player: &mut Player,
        save_key: bool,
        load_key: bool,
    ) -> Option<QuickEvent> {
        let pressed = [save_key && !self.held[0], load_key && !self.held[1]];
        self.held = [save_key, load_key];
        if pressed[0] {
            Some(self.act(player, false))
        } else if pressed[1] {
            Some(self.act(player, true))
        } else {
            None
        }
    }
    /// Save now (or load, with `loading`), for a caller that detects the
    /// key presses itself. Call between two [`Player::iteration`] calls.
    pub fn act(&self, player: &mut Player, loading: bool) -> QuickEvent {
        if loading {
            match load(player, self.game, &self.path) {
                Ok(loaded) => QuickEvent::Loaded(loaded),
                Err(error) => QuickEvent::LoadFailed(error),
            }
        } else {
            match save(player, self.game, &self.path) {
                Ok(saved) => QuickEvent::Saved(saved),
                Err(error) => QuickEvent::SaveFailed(error),
            }
        }
    }
}

/// The default quick-save slot: next to memory card 1's file
/// (`card1.mcr` → `card1.ac3state`), else `quick.ac3state` in the player's
/// folder (next to the executable; Application Support for a macOS app).
pub fn default_slot(card1: &crate::machine_card::CardSource) -> PathBuf {
    match card1 {
        crate::machine_card::CardSource::File(card) => card.with_extension("ac3state"),
        _ => crate::settings::player_folder().join("quick.ac3state"),
    }
}

/// Serde for fixed-size arrays of any length (`#[serde(with = "...")]`;
/// serde's own implementations stop at 32 elements).
pub mod array {
    use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error};

    pub fn serialize<S: Serializer, T: Serialize, const N: usize>(
        array: &[T; N],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(array)
    }
    pub fn deserialize<'de, D: Deserializer<'de>, T: Deserialize<'de>, const N: usize>(
        deserializer: D,
    ) -> Result<[T; N], D::Error> {
        let items = Vec::<T>::deserialize(deserializer)?;
        let length = items.len();
        items
            .try_into()
            .map_err(|_| D::Error::invalid_length(length, &"the array's length"))
    }
}

/// Serde for `[[T; N]; M]` of any sizes.
pub mod array_of_arrays {
    use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error};

    pub fn serialize<S: Serializer, T: Serialize, const N: usize, const M: usize>(
        rows: &[[T; N]; M],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(rows.iter().map(|row| row.as_slice()))
    }
    pub fn deserialize<
        'de,
        D: Deserializer<'de>,
        T: Deserialize<'de>,
        const N: usize,
        const M: usize,
    >(
        deserializer: D,
    ) -> Result<[[T; N]; M], D::Error> {
        let rows = Vec::<Vec<T>>::deserialize(deserializer)?
            .into_iter()
            .map(|row| {
                let length = row.len();
                row.try_into()
                    .map_err(|_| D::Error::invalid_length(length, &"the row's length"))
            })
            .collect::<Result<Vec<[T; N]>, D::Error>>()?;
        let length = rows.len();
        rows.try_into()
            .map_err(|_| D::Error::invalid_length(length, &"the row count"))
    }
}

/// Serde for a queue of boxed fixed-size arrays (XA sectors).
pub mod boxed_arrays {
    use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error};
    use std::collections::VecDeque;

    pub fn serialize<S: Serializer, T: Serialize, const N: usize>(
        items: &VecDeque<Box<[T; N]>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(items.iter().map(|item| item.as_slice()))
    }
    pub fn deserialize<'de, D: Deserializer<'de>, T: Deserialize<'de>, const N: usize>(
        deserializer: D,
    ) -> Result<VecDeque<Box<[T; N]>>, D::Error> {
        Vec::<Vec<T>>::deserialize(deserializer)?
            .into_iter()
            .map(|item| {
                let length = item.len();
                item.into_boxed_slice()
                    .try_into()
                    .map_err(|_| D::Error::invalid_length(length, &"the item's length"))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine_card::CardDevice;
    use ac3_flight::bios;
    use ac3_graphics::Vram;
    use ac3_machine::{Device, HostCall, Machine};
    use std::rc::Rc;

    const GAME: GameId = GameId([7; 32]);

    /// A player without a disc whose machine and devices are not at their
    /// power-on state.
    fn player() -> Player {
        let mut machine = Machine::default();
        machine.sw(0x8001_0000, 0x1234_5678);
        machine.sh(0x1f80_0010, 0xbeef);
        machine.gte.data[3] = 9;
        machine.host_results.push_back(5);
        let mut host = Host::new(Vram::new(), None);
        host.vblanks = 17;
        host.buttons = 0x4000;
        host.pad_running = Some(true);
        host.audio = vec![[1, -1]; 3];
        host.xa.borrow_mut().muted = true;
        host.xa
            .borrow_mut()
            .sectors
            .push_back(Box::new([3; ac3_machine::cd::SECTOR_BYTES]));
        host.busy
            .advance(1234, crate::machine_audio::FieldLength::PAL);
        Player::new(machine, host)
    }

    #[test]
    fn states_round_trip_in_memory_and_through_bytes() {
        let source = player();
        let state = capture(&source, GAME).unwrap();
        assert_eq!(state.vblanks(), 17);
        let bytes = state.to_bytes().unwrap();
        assert_eq!(&bytes[..8], &MAGIC);
        let decoded = State::from_bytes(&bytes, GAME).unwrap();
        let mut target = Player::new(Machine::default(), Host::new(Vram::new(), None));
        let queue = target.host.borrow().xa.clone();
        restore(&mut target, &decoded, GAME).unwrap();
        assert_eq!(target.machine.ram(), source.machine.ram());
        assert_eq!(target.machine.scratch(), source.machine.scratch());
        assert_eq!(target.machine.gte, source.machine.gte);
        assert_eq!(target.machine.host_results, source.machine.host_results);
        let (a, b) = (source.host.borrow(), target.host.borrow());
        assert_eq!(
            (b.vblanks, b.buttons, b.pad_running, &b.audio),
            (a.vblanks, a.buttons, a.pad_running, &a.audio)
        );
        assert!(Rc::ptr_eq(&queue, &b.xa), "the shared XA queue stays");
        assert!(b.xa.borrow().muted);
        assert_eq!(b.xa.borrow().sectors.len(), 1);
        assert_eq!(b.busy, a.busy);
        // The same bytes again (deterministic encoding).
        assert_eq!(capture(&target, GAME).unwrap().to_bytes().unwrap(), bytes);
    }

    #[test]
    fn refusals_name_the_reason() {
        let source = player();
        let bytes = capture(&source, GAME).unwrap().to_bytes().unwrap();
        let mut wrong = bytes.clone();
        wrong[0] ^= 1;
        assert!(matches!(
            State::from_bytes(&wrong, GAME),
            Err(Error::NotAState)
        ));
        let mut wrong = bytes.clone();
        wrong[8..12].copy_from_slice(&(FORMAT_VERSION + 1).to_le_bytes());
        assert!(matches!(
            State::from_bytes(&wrong, GAME),
            Err(Error::Version { found, .. }) if found == FORMAT_VERSION + 1
        ));
        assert!(matches!(
            State::from_bytes(&bytes, GameId([8; 32])),
            Err(Error::WrongGame { .. })
        ));
        let mut damaged = bytes.clone();
        let last = damaged.len() - 1;
        damaged[last] ^= 0x55;
        assert!(matches!(
            State::from_bytes(&damaged, GAME),
            Err(Error::Corrupt(_))
        ));
        assert!(matches!(
            State::from_bytes(&bytes[..40], GAME),
            Err(Error::Corrupt(_))
        ));
        // In memory too, leaving the player unchanged.
        let state = State::from_bytes(&bytes, GAME).unwrap();
        let mut other = Player::new(Machine::default(), Host::new(Vram::new(), None));
        assert!(matches!(
            restore(&mut other, &state, GameId([8; 32])),
            Err(Error::WrongGame { .. })
        ));
        assert_eq!(other.vblanks(), 0, "unchanged after a refusal");
    }

    #[test]
    fn a_card_request_in_flight_refuses_the_save() {
        let source = player();
        let mut ram = vec![0u8; RAM_BYTES];
        let mut scratch = vec![0u8; SCRATCH_BYTES];
        let mut call = |function, ram: &mut [u8]| {
            let call = HostCall {
                function,
                args: [0, 0, 0, 0],
            };
            source.host.borrow_mut().card.call(ram, &mut scratch, call)
        };
        source.host.borrow_mut().card = CardDevice::default();
        // _card_info on port 0: done one VBlank later.
        assert_eq!(call(bios::CARD_INFO, &mut ram), Some(1));
        assert!(matches!(capture(&source, GAME), Err(Error::CardBusy)));
        // The VBlank passes; the driver completes it at its next call.
        ram[0x0b_3ff4] = 1;
        call(bios::TEST_EVENT, &mut ram);
        assert!(capture(&source, GAME).is_ok());
    }

    #[test]
    fn the_quick_slot_acts_once_per_key_press() {
        let folder = std::env::temp_dir().join(format!("ac3-quick-slot-{}", std::process::id()));
        let mut slot = QuickSlot::new(folder.join("slot/quick.ac3state"), GAME);
        let mut player = player();
        let event = slot.poll(&mut player, false, true).unwrap();
        assert_eq!(event.to_string(), "quick load: no quick save yet");
        assert!(slot.poll(&mut player, false, true).is_none(), "held");
        assert!(matches!(
            slot.poll(&mut player, true, false),
            Some(QuickEvent::Saved(Summary { vblanks: 17, .. }))
        ));
        assert!(slot.poll(&mut player, true, false).is_none(), "held");
        assert!(slot.path().exists());
        player.host.borrow_mut().vblanks = 99;
        player.machine.sw(0x8001_0000, 0);
        let event = slot.poll(&mut player, false, true).unwrap();
        assert_eq!(event.to_string(), "quick load (VBlank 17)");
        assert_eq!(player.vblanks(), 17);
        assert_eq!(player.machine.lw(0x8001_0000), 0x1234_5678);
        // A card request in flight: the save is refused, the slot kept.
        let mut ram = vec![0u8; RAM_BYTES];
        let call = HostCall {
            function: bios::CARD_INFO,
            args: [0, 0, 0, 0],
        };
        player
            .host
            .borrow_mut()
            .card
            .call(&mut ram, &mut [0; SCRATCH_BYTES], call);
        let event = slot.poll(&mut player, true, false).unwrap();
        assert!(matches!(event, QuickEvent::SaveFailed(Error::CardBusy)));
        assert!(
            event
                .to_string()
                .starts_with("quick save failed: memory card busy")
        );
        assert_eq!(summary(&fs::read(slot.path()).unwrap()).vblanks, 17);
        fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn the_default_slot_sits_next_to_memory_card_1() {
        use crate::machine_card::CardSource;
        let card = CardSource::File(PathBuf::from("saves/card1.mcr"));
        assert_eq!(default_slot(&card), PathBuf::from("saves/card1.ac3state"));
        let slot = default_slot(&CardSource::Blank);
        assert_eq!(slot.file_name().unwrap(), "quick.ac3state");
    }

    /// Sections of power-on devices. Any change to a serialized device
    /// structure changes these bytes: bump [`FORMAT_VERSION`] and pin the
    /// new hash.
    #[test]
    fn layout_is_pinned() {
        let player = Player::new(Machine::default(), Host::new(Vram::new(), None));
        let state = capture(&player, GAME).unwrap();
        let mut bytes = Vec::new();
        for section in &state.sections {
            bytes.extend_from_slice(section.name.as_bytes());
            bytes.extend_from_slice(&section.data);
        }
        let image = || std::io::Cursor::new(Vec::<u8>::new());
        let layout = ac3_flight::game::LAYOUT;
        let drive = ac3_machine::cd::CdDrive::new(
            ac3_machine::cd::RawImage(image()),
            layout.vblank_clock(),
        );
        bytes.extend_from_slice(&postcard::to_allocvec(&drive.snapshot()).unwrap());
        let disc = ac3_machine::disc::DiscDevice::new(image(), layout.cd_file_cache);
        bytes.extend_from_slice(&postcard::to_allocvec(&disc.snapshot()).unwrap());
        let layout = GameId::of(&bytes).to_string();
        assert_eq!(
            (FORMAT_VERSION, layout.as_str()),
            // 2: the CD drive's lid, its wait-for-the-lid count and the disc
            // in the drive (the Japanese release's two discs).
            (
                2,
                "e5fab038af9c343616bf88282245fd54593a0149a63945813f2a18adbfa79f2c"
            ),
            "a serialized structure changed: bump FORMAT_VERSION and pin the new layout hash"
        );
    }
}
