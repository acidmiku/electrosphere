//! Host device for the BIOS kernel's events and the memory cards
//! (`ac3_flight::bios`).
//!
//! [`CardDevice`] answers the kernel's host calls: event control blocks
//! (OpenEvent .. DeliverEvent, TestEvent, WaitEvent), critical sections,
//! the `bu00:`/`bu10:` file system (open, lseek, read, write, close,
//! firstfile, nextfile) and libcard's card requests (`_card_info`,
//! `_card_load`, `_card_clear`, `_card_format`), on two [`MemoryCard`]s in
//! the raw 128 KiB `.mcr` format.
//!
//! Files: PAL's `BESCESP02066AC3PAL` is one block, moved in one 0x11FC-byte
//! read or write from the start. The Japanese `BISLPSP02020AC3ES` is two
//! blocks (created with mode 0x20200: overlay 1 800D5EC4, overlay 2's call
//! at 800E47FC), used up to 0x2B80: a 0x280-byte head, a 0x80-byte record at
//! 0x280, three 0x480-byte records from 0x300 and six 0x180-byte records
//! per account from 0x1080 (0x900 apart), each moved by its own open
//! (0x8001 / 0x8002), `lseek(fd, offset, 0)` and one transfer. A
//! descriptor keeps its position: `lseek` sets it, and each transfer starts
//! there and advances it by the whole sectors it moved. The two games'
//! card images are not interchangeable (different files).
//!
//! The kernel's card work is asynchronous. A request occupies its port and
//! completes after a delay (below); completion performs the transfer (RAM ⇄
//! card image, saved to the card's file after every write) and delivers
//! the request's event: class `SW_CARD` for `_card_info`, `_card_load` and
//! asynchronous `read`/`write`, `HW_CARD` for `_card_clear`; spec `EV_IOE`
//! when done, `EV_TIMEOUT` with no card, `EV_ERROR` for `_card_load` on an
//! unformatted card, `EV_NEW` for a card flagged new. Delivery marks every
//! enabled event of that class and spec (no-interrupt mode) ready;
//! TestEvent returns 1 once and re-arms it, as the kernel does.
//!
//! Time is libetc's VBlank counter (`0x800B3FF4`, advanced by the player's
//! VBlank delivery) and, for busy waits that never let a VBlank through,
//! the number of TestEvent polls. Calibrated on the original (captures
//! `ov1-010`, `ov1-012`, `ov1-019`, `ov2hub-008`..`012`; frames are
//! VBlanks; the delays seen in parentheses): `_card_info` is ready after
//! one VBlank (1: 295 times, 2: 5), `_card_load` after
//! [`VBLANKS_PER_SECTOR`] per sector of 0..35 (72: 20 times; 2 when the
//! header says unformatted), `read` / `write` after two per sector and one
//! more (35 sectors: 71, 72); `_card_clear`'s one-sector write finishes
//! within the frame, after [`POLLS_PER_SECTOR`] polls (the front end's
//! 370th round of three TestEvents; the save screen's busy wait saw it
//! after 1041..1047 polls) or at the next VBlank.
//!
//! What the original's kernel and libraries did, as observed:
//! - read / write move whole sectors: 0x11FC bytes are 35 sectors, the
//!   last 0x7C bytes stay untouched (`ov1-019`, `ov2hub-008`);
//! - `open` with create (`0x200`, blocks in bits 16..) leaves one file
//!   descriptor allocated for good and returns the next (`ov2hub-008`:
//!   the create and every later open got 3, with 2 never freed);
//! - firstfile fills the DIRENTRY's name (up to its NUL), attributes 0x50,
//!   size and head (the first block's first sector), not `next` or
//!   `system` (`ov2hub-012`); libc's wrapper keeps the pattern's device
//!   name ("bu") at `0x800C9D68` and the kernel's patched entry at
//!   `0x800C9D60` (kernel state, not reproduced);
//! - `_card_format` (libcard, synchronous) writes sectors 1..15 from its
//!   templates (0xA0, size 0, next 0xFFFF), 16..35 from its broken-sector
//!   words (0xFFFFFFFF) and the "MC" header, each with its XOR byte, reads
//!   sector 0 back into its buffer and loads the directory: its RAM
//!   (`0x800C9A70..0x800C9D30`) is reproduced, and only the load's
//!   `SW_CARD` event is left pending (`ov2hub-011`; its `_card_status`
//!   waits take the hardware completions). The original's format took 146
//!   VBlanks of game time; here it completes within the call.
//!
//! Event slot 0 is libspu's transfer event (`0xF1000000`, opened by
//! SpuInit, which the SPU device answers): TestEvent / WaitEvent on events
//! not opened here are left to the other devices.
use ac3_flight::{Device, HostCall, bios};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// libetc's VBlank counter of the PAL release: the default clock of the
/// card work ([`CardDevice::with_clock`] takes the release's, its
/// `Layout::vblank_counter`; JP 0x800B6174).
pub const VBLANK_COUNTER: u32 = 0x800b_3ff4;
pub const CARD_BYTES: usize = 0x2_0000;
pub const SECTOR_BYTES: usize = 0x80;
pub const BLOCK_BYTES: usize = 0x2000;
const SECTORS_PER_BLOCK: u32 = (BLOCK_BYTES / SECTOR_BYTES) as u32;
/// Event control blocks (`SYSTEM.CNF` EVENT = 0x10) and file descriptors.
const EVENTS: usize = 16;
const FILES: usize = 16;
/// VBlanks per sector of the kernel's card work.
pub const VBLANKS_PER_SECTOR: u32 = 2;
/// Polls per sector transfer in a busy wait.
pub const POLLS_PER_SECTOR: u64 = 3 * 369;
/// `_card_load`: header, 15 directory frames, 20 broken-sector frames.
const LOAD_SECTORS: u32 = 36;
/// Directory frame states: in use (first, middle, last block), free.
const FIRST: u32 = 0x51;
const MIDDLE: u32 = 0x52;
const LAST: u32 = 0x53;
const FREE: u32 = 0xa0;

/// Event status words (libapi `kernel.h`).
pub mod status {
    pub const UNUSED: u32 = 0x0000;
    pub const WAIT: u32 = 0x1000;
    pub const ACTIVE: u32 = 0x2000;
    pub const READY: u32 = 0x4000;
}

fn index(address: u32) -> usize {
    (address & 0x1f_ffff) as usize
}
fn read_u32(ram: &[u8], address: u32) -> u32 {
    let at = index(address);
    u32::from_le_bytes(std::array::from_fn(|i| ram[(at + i) & 0x1f_ffff]))
}
fn read_bytes(ram: &[u8], address: u32, length: usize) -> Vec<u8> {
    (0..length)
        .map(|i| ram[index(address.wrapping_add(i as u32))])
        .collect()
}
fn write_bytes(ram: &mut [u8], address: u32, bytes: &[u8]) {
    for (i, &byte) in bytes.iter().enumerate() {
        ram[index(address.wrapping_add(i as u32))] = byte;
    }
}
/// A NUL-terminated string (at most 64 bytes).
fn read_string(ram: &[u8], address: u32) -> Vec<u8> {
    read_bytes(ram, address, 64)
        .into_iter()
        .take_while(|&b| b != 0)
        .collect()
}
/// `buP0:NAME` → (port P, NAME); other devices (and multitap slots) are
/// not ours.
fn bu_path(path: &[u8]) -> Option<(usize, Vec<u8>)> {
    match path {
        [b'b', b'u', port @ (b'0' | b'1'), b'0', b':', name @ ..] => {
            Some((usize::from(port - b'0'), name.to_vec()))
        }
        _ => None,
    }
}
/// The kernel's name match: `?` any one character, `*` the rest.
fn name_matches(pattern: &[u8], name: &[u8]) -> bool {
    let mut i = 0;
    for &p in pattern {
        match p {
            b'*' => return true,
            b'?' if i < name.len() => {}
            _ if name.get(i) == Some(&p) => {}
            _ => return false,
        }
        i += 1;
    }
    i == name.len()
}

/// One directory frame (sectors 1..15) of a card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub state: u32,
    pub size: u32,
    pub next: u16,
    pub name: Vec<u8>,
}

/// A memory card: the raw 128 KiB image (1024 sectors of 128 bytes:
/// sector 0 the "MC" header, 1..15 the directory, 16..35 the broken-sector
/// list, 63 the write test, blocks 1..15 of 64 sectors from sector 64) and
/// the file it is saved to.
#[derive(Clone, Debug)]
pub struct MemoryCard {
    image: Vec<u8>,
    path: Option<PathBuf>,
    /// The card's "new card" flag (set on insertion, cleared by a write):
    /// `_card_info` and `_card_load` report `EV_NEW` while it is set. The
    /// original's cards (PCSX-Redux) never reported it; cards start clear.
    pub new_card: bool,
}

impl MemoryCard {
    /// A formatted, empty card in the BIOS layout: the header, free
    /// directory frames (0xA0, next 0xFFFF), empty broken-sector frames
    /// (0xFFFFFFFF, next 0xFFFF), each with its XOR byte.
    pub fn blank() -> Self {
        let mut card = Self::zeroed();
        card.format(true);
        card
    }
    /// An unformatted card (all zeros).
    pub fn zeroed() -> Self {
        Self {
            image: vec![0; CARD_BYTES],
            path: None,
            new_card: false,
        }
    }
    /// Write the header, the directory and the broken-sector list;
    /// `bios_layout` adds the broken frames' next 0xFFFF, which libcard's
    /// `_card_format` leaves 0.
    fn format(&mut self, bios_layout: bool) {
        let header = self.sector_mut(0);
        header.fill(0);
        header[..2].copy_from_slice(b"MC");
        self.checksum(0);
        for frame in 1..16 {
            self.set_frame(
                frame,
                &Frame {
                    state: FREE,
                    size: 0,
                    next: 0xffff,
                    name: Vec::new(),
                },
            );
        }
        for sector in 16..36 {
            let bytes = self.sector_mut(sector);
            bytes.fill(0);
            bytes[..4].fill(0xff);
            if bios_layout {
                bytes[8..10].fill(0xff);
            }
            self.checksum(sector);
        }
    }
    /// A raw image (exactly 128 KiB).
    pub fn from_image(image: Vec<u8>) -> io::Result<Self> {
        if image.len() != CARD_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "memory card image is {} bytes, expected {CARD_BYTES} (raw .mcr)",
                    image.len()
                ),
            ));
        }
        Ok(Self {
            image,
            path: None,
            new_card: false,
        })
    }
    /// The card saved at `path`: loaded when the file exists, else a blank
    /// card the first write creates.
    pub fn open(path: &Path) -> io::Result<Self> {
        let mut card = match fs::read(path) {
            Ok(image) => Self::from_image(image)
                .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Self::blank(),
            Err(error) => return Err(error),
        };
        card.path = Some(path.to_owned());
        Ok(card)
    }
    pub fn image(&self) -> &[u8] {
        &self.image
    }
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
    /// Write the image to its file, if it has one.
    pub fn save(&self) -> io::Result<()> {
        match &self.path {
            Some(path) => fs::write(path, &self.image),
            None => Ok(()),
        }
    }
    pub fn formatted(&self) -> bool {
        self.image.starts_with(b"MC")
    }
    pub fn sector(&self, sector: u32) -> &[u8] {
        let at = sector as usize * SECTOR_BYTES;
        &self.image[at..at + SECTOR_BYTES]
    }
    fn sector_mut(&mut self, sector: u32) -> &mut [u8] {
        let at = sector as usize * SECTOR_BYTES;
        &mut self.image[at..at + SECTOR_BYTES]
    }
    /// XOR of the sector's first 127 bytes into its last.
    fn checksum(&mut self, sector: u32) {
        let bytes = self.sector_mut(sector);
        bytes[0x7f] = bytes[..0x7f].iter().fold(0, |x, b| x ^ b);
    }
    /// Directory frame `frame` (1..15).
    pub fn frame(&self, frame: u32) -> Frame {
        let s = self.sector(frame);
        Frame {
            state: u32::from_le_bytes([s[0], s[1], s[2], s[3]]),
            size: u32::from_le_bytes([s[4], s[5], s[6], s[7]]),
            next: u16::from_le_bytes([s[8], s[9]]),
            name: s[0x0a..0x1f]
                .iter()
                .take_while(|&&b| b != 0)
                .copied()
                .collect(),
        }
    }
    fn set_frame(&mut self, frame: u32, value: &Frame) {
        let s = self.sector_mut(frame);
        s.fill(0);
        s[0..4].copy_from_slice(&value.state.to_le_bytes());
        s[4..8].copy_from_slice(&value.size.to_le_bytes());
        s[8..10].copy_from_slice(&value.next.to_le_bytes());
        let name = &value.name[..value.name.len().min(20)];
        s[0x0a..0x0a + name.len()].copy_from_slice(name);
        self.checksum(frame);
    }
    /// The blocks of the file starting at directory frame `first`, in
    /// order (the frames' next links, 0-based; at most 15).
    pub fn chain(&self, first: u32) -> Vec<u32> {
        let mut blocks = vec![first];
        let mut frame = self.frame(first);
        while frame.next != 0xffff && blocks.len() < 15 {
            let next = u32::from(frame.next) + 1;
            if !(1..16).contains(&next) || blocks.contains(&next) {
                break;
            }
            blocks.push(next);
            frame = self.frame(next);
        }
        blocks
    }
    /// The first frame of the file `name`.
    pub fn find(&self, name: &[u8]) -> Option<u32> {
        (1..16).find(|&f| {
            let frame = self.frame(f);
            frame.state == FIRST && frame.name == name
        })
    }
    /// The files: (first frame, name, size).
    pub fn files(&self) -> Vec<(u32, Vec<u8>, u32)> {
        (1..16)
            .map(|f| (f, self.frame(f)))
            .filter(|(_, frame)| frame.state == FIRST)
            .map(|(f, frame)| (f, frame.name, frame.size))
            .collect()
    }
    /// Allocate `blocks` free frames for `name` (first 0x51, middle 0x52,
    /// last 0x53, size in the first); the first frame, or `None` when the
    /// name exists or the card is full.
    pub fn create(&mut self, name: &[u8], blocks: u32) -> Option<u32> {
        if self.find(name).is_some() || name.is_empty() || name.len() > 20 || blocks == 0 {
            return None;
        }
        let free: Vec<u32> = (1..16)
            .filter(|&f| (self.frame(f).state & 0xf0) == FREE)
            .take(blocks as usize)
            .collect();
        if free.len() != blocks as usize {
            return None;
        }
        for (i, &frame) in free.iter().enumerate() {
            let next = free.get(i + 1).map_or(0xffff, |&n| (n - 1) as u16);
            let state = match (i, i + 1 == free.len()) {
                (0, _) => FIRST,
                (_, true) => LAST,
                _ => MIDDLE,
            };
            self.set_frame(
                frame,
                &Frame {
                    state,
                    size: if i == 0 {
                        blocks * BLOCK_BYTES as u32
                    } else {
                        0
                    },
                    next,
                    name: if i == 0 { name.to_vec() } else { Vec::new() },
                },
            );
        }
        Some(free[0])
    }
    /// Delete the file `name` (frames marked free-deleted 0xA1..0xA3). No
    /// game stub reaches the kernel's `erase`; this is for the host.
    pub fn erase(&mut self, name: &[u8]) -> bool {
        let Some(first) = self.find(name) else {
            return false;
        };
        for block in self.chain(first) {
            let mut frame = self.frame(block);
            frame.state = 0xa0 | (frame.state & 0x0f);
            self.set_frame(block, &frame);
        }
        true
    }
}

/// A memory-card slot as configured: a blank card kept in memory, no card,
/// or a card saved to a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CardSource {
    Blank,
    Absent,
    File(PathBuf),
}

impl CardSource {
    /// `none` → no card, else a file path.
    pub fn parse(text: &str) -> Self {
        if text.eq_ignore_ascii_case("none") {
            Self::Absent
        } else {
            Self::File(text.into())
        }
    }
    pub fn card(&self) -> io::Result<Option<MemoryCard>> {
        Ok(match self {
            Self::Blank => Some(MemoryCard::blank()),
            Self::Absent => None,
            Self::File(path) => Some(MemoryCard::open(path)?),
        })
    }
}

/// One kernel event control block.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Event {
    pub class: u32,
    pub spec: u32,
    pub mode: u32,
    pub handler: u32,
    pub status: u32,
    /// Opened outside this device (libspu's event 0).
    pub foreign: bool,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct OpenFile {
    port: usize,
    first: u32,
    size: u32,
    position: u32,
    mode: u32,
    /// The descriptor a create leaves allocated (not a usable file).
    dead: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum Work {
    Info,
    Load,
    /// `_card_clear`: sector 0x3F from RAM address 0.
    Clear,
    FileRead {
        sectors: Vec<u32>,
        buffer: u32,
    },
    FileWrite {
        sectors: Vec<u32>,
        buffer: u32,
    },
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Pending {
    port: usize,
    work: Work,
    due_vblank: u32,
    due_poll: u64,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Search {
    port: usize,
    pattern: Vec<u8>,
    next: u32,
}

/// The kernel's events, files and card driver on two cards; see the module
/// documentation.
pub struct CardDevice {
    pub cards: [Option<MemoryCard>; 2],
    pub events: [Event; EVENTS],
    files: [Option<OpenFile>; FILES],
    pending: Vec<Pending>,
    critical: bool,
    polls: u64,
    search: Option<Search>,
    /// Requests completed and events delivered (for reports).
    pub completed: u64,
    pub delivered: u64,
    error: Option<String>,
    /// libetc's VBlank counter of the release (the card work's clock).
    /// Configuration, not device state.
    vblank_counter: u32,
}

/// The kernel and card driver state of a [`CardDevice`] for save states:
/// events, open files, requests in progress, the critical section, the poll
/// count, the directory search, the counters and the error. The cards
/// themselves are not part of it (a load does not roll them back).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct CardState {
    events: [Event; EVENTS],
    files: [Option<OpenFile>; FILES],
    pending: Vec<Pending>,
    critical: bool,
    polls: u64,
    search: Option<Search>,
    completed: u64,
    delivered: u64,
    error: Option<String>,
}

impl CardState {
    /// Whether a card request (transfer) is in progress.
    pub fn busy(&self) -> bool {
        !self.pending.is_empty()
    }
}

impl Default for CardDevice {
    /// Two blank cards kept in memory.
    fn default() -> Self {
        Self::new([Some(MemoryCard::blank()), Some(MemoryCard::blank())])
    }
}

/// `true` for the host calls [`CardDevice`] answers.
pub fn handles(function: u32) -> bool {
    use bios::*;
    matches!(
        function,
        DELIVER_EVENT
            | OPEN_EVENT
            | CLOSE_EVENT
            | WAIT_EVENT
            | TEST_EVENT
            | ENABLE_EVENT
            | DISABLE_EVENT
            | ENTER_CRITICAL_SECTION
            | EXIT_CRITICAL_SECTION
            | OPEN
            | LSEEK
            | READ
            | WRITE
            | CLOSE
            | NEXT_FILE
            | FIRST_FILE
            | BU_INIT
            | CARD_INFO
            | CARD_LOAD
            | CARD_CLEAR
            | CARD_FORMAT
            | INIT_CARD
            | START_CARD
            | STOP_CARD
    )
}

impl CardDevice {
    pub fn new(cards: [Option<MemoryCard>; 2]) -> Self {
        let mut events = [Event::default(); EVENTS];
        events[0] = Event {
            class: 0xf000_0009,
            spec: 0x20,
            mode: bios::EV_MODE_NO_INTERRUPT,
            handler: 0,
            status: status::WAIT,
            foreign: true,
        };
        Self {
            cards,
            events,
            files: Default::default(),
            pending: Vec::new(),
            critical: false,
            polls: 0,
            search: None,
            completed: 0,
            delivered: 0,
            error: None,
            vblank_counter: VBLANK_COUNTER,
        }
    }
    /// The device timing its card work by the VBlank counter at `counter`
    /// (the release's `Layout::vblank_counter`; PAL [`VBLANK_COUNTER`] by
    /// default).
    pub fn with_clock(mut self, counter: u32) -> Self {
        self.vblank_counter = counter;
        self
    }
    /// The two slots as configured.
    pub fn from_sources(sources: &[CardSource; 2]) -> io::Result<Self> {
        Ok(Self::new([sources[0].card()?, sources[1].card()?]))
    }
    /// First error (saving a card file), if any.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
    /// Whether a request is in progress.
    pub fn busy(&self) -> bool {
        !self.pending.is_empty()
    }
    /// The device's state without the cards (see [`CardState`]).
    pub fn snapshot(&self) -> CardState {
        // Every field named: a new one must be placed here or excluded.
        let Self {
            cards: _,
            events,
            files,
            pending,
            critical,
            polls,
            search,
            completed,
            delivered,
            error,
            vblank_counter: _,
        } = self;
        CardState {
            events: *events,
            files: files.clone(),
            pending: pending.clone(),
            critical: *critical,
            polls: *polls,
            search: search.clone(),
            completed: *completed,
            delivered: *delivered,
            error: error.clone(),
        }
    }
    /// Replace the device's state, keeping the cards as they are.
    pub fn restore(&mut self, state: CardState) {
        let CardState {
            events,
            files,
            pending,
            critical,
            polls,
            search,
            completed,
            delivered,
            error,
        } = state;
        self.events = events;
        self.files = files;
        self.pending = pending;
        self.critical = critical;
        self.polls = polls;
        self.search = search;
        self.completed = completed;
        self.delivered = delivered;
        self.error = error;
    }

    fn event_index(descriptor: u32) -> Option<usize> {
        let index = (descriptor & 0xffff) as usize;
        (descriptor & 0xffff_0000 == 0xf100_0000 && index < EVENTS).then_some(index)
    }
    /// An event opened here.
    fn own_event(&self, descriptor: u32) -> Option<usize> {
        Self::event_index(descriptor)
            .filter(|&i| self.events[i].status != status::UNUSED && !self.events[i].foreign)
    }

    /// DeliverEvent: every enabled event of `class`/`spec` in no-interrupt
    /// mode becomes ready. Handlers (interrupt mode) are guest code and are
    /// not run.
    pub fn deliver(&mut self, class: u32, spec: u32) {
        for event in &mut self.events {
            if event.status == status::ACTIVE
                && event.class == class
                && event.spec == spec
                && event.mode == bios::EV_MODE_NO_INTERRUPT
            {
                event.status = status::READY;
                self.delivered += 1;
            }
        }
    }

    fn port_busy(&self, port: usize) -> bool {
        self.pending.iter().any(|p| p.port == port)
    }
    fn schedule(&mut self, ram: &[u8], port: usize, work: Work, vblanks: u32, polls: u64) {
        let now = read_u32(ram, self.vblank_counter);
        self.pending.push(Pending {
            port,
            work,
            due_vblank: now.wrapping_add(vblanks),
            due_poll: self.polls + polls,
        });
    }
    /// Complete the requests that are due.
    fn advance(&mut self, ram: &mut [u8]) {
        let now = read_u32(ram, self.vblank_counter);
        let mut i = 0;
        while i < self.pending.len() {
            let p = &self.pending[i];
            if now.wrapping_sub(p.due_vblank) as i32 >= 0 || self.polls >= p.due_poll {
                let p = self.pending.remove(i);
                self.complete(ram, p);
            } else {
                i += 1;
            }
        }
    }
    /// Complete every request now (WaitEvent).
    fn finish_all(&mut self, ram: &mut [u8]) {
        while !self.pending.is_empty() {
            let p = self.pending.remove(0);
            self.complete(ram, p);
        }
    }
    fn complete(&mut self, ram: &mut [u8], p: Pending) {
        use bios::{EV_ERROR, EV_IOE, EV_NEW, EV_TIMEOUT, HW_CARD, SW_CARD};
        self.completed += 1;
        let class = if p.work == Work::Clear {
            HW_CARD
        } else {
            SW_CARD
        };
        let Some(card) = self.cards[p.port].as_mut() else {
            self.deliver(class, EV_TIMEOUT);
            return;
        };
        let mut saved = false;
        let spec = match p.work {
            Work::Info if card.new_card => EV_NEW,
            Work::Info => EV_IOE,
            Work::Load if !card.formatted() => EV_ERROR,
            Work::Load if card.new_card => EV_NEW,
            Work::Load => EV_IOE,
            Work::Clear => {
                let data = read_bytes(ram, 0, SECTOR_BYTES);
                card.sector_mut(0x3f).copy_from_slice(&data);
                card.new_card = false;
                saved = true;
                EV_IOE
            }
            Work::FileRead {
                ref sectors,
                buffer,
            } => {
                for (k, &sector) in sectors.iter().enumerate() {
                    let at = buffer.wrapping_add((k * SECTOR_BYTES) as u32);
                    write_bytes(ram, at, card.sector(sector));
                }
                EV_IOE
            }
            Work::FileWrite {
                ref sectors,
                buffer,
            } => {
                for (k, &sector) in sectors.iter().enumerate() {
                    let at = buffer.wrapping_add((k * SECTOR_BYTES) as u32);
                    let data = read_bytes(ram, at, SECTOR_BYTES);
                    card.sector_mut(sector).copy_from_slice(&data);
                }
                saved = true;
                EV_IOE
            }
        };
        if saved {
            self.save(p.port);
        }
        self.deliver(class, spec);
    }
    fn save(&mut self, port: usize) {
        if let Some(card) = &self.cards[port]
            && let Err(error) = card.save()
        {
            let path = card.path().map(|p| p.display().to_string());
            self.error
                .get_or_insert(format!("memory card {}: {error}", path.unwrap_or_default()));
        }
    }

    // ---- card requests --------------------------------------------------------------

    /// The port of a channel (0x00 / 0x10); multitap slots are refused.
    fn port(channel: u32) -> Option<usize> {
        (channel & !0x10 == 0).then_some((channel >> 4) as usize)
    }
    fn card_request(&mut self, ram: &[u8], channel: u32, work: Work) -> u32 {
        let Some(port) = Self::port(channel).filter(|&p| !self.port_busy(p)) else {
            return 0;
        };
        let formatted = self.cards[port].as_ref().is_some_and(MemoryCard::formatted);
        let sectors = match work {
            Work::Load if formatted => LOAD_SECTORS,
            Work::Load => 1,
            _ => 0,
        };
        let vblanks = (sectors * VBLANKS_PER_SECTOR).max(1);
        let polls = u64::from(sectors.max(1)) * POLLS_PER_SECTOR;
        self.schedule(ram, port, work, vblanks, polls);
        1
    }
    /// libcard's `_card_format`; see the module documentation.
    fn format(&mut self, ram: &mut [u8], channel: u32) -> u32 {
        let Some(port) = Self::port(channel).filter(|&p| !self.port_busy(p)) else {
            return 0;
        };
        let mut template = [0u8; 0x20];
        template[0] = 0xa0;
        template[8..10].fill(0xff);
        for frame in 0..15 {
            write_bytes(ram, bios::FORMAT_FRAMES + frame * 0x20, &template);
        }
        write_bytes(ram, bios::FORMAT_BROKEN, &[0xff; 20 * 4]);
        let header = match self.cards[port].as_mut() {
            Some(card) => {
                card.format(false);
                card.new_card = false;
                card.sector(0).to_vec()
            }
            None => vec![0; SECTOR_BYTES],
        };
        write_bytes(ram, bios::FORMAT_BUFFER, &header);
        self.completed += 1;
        if self.cards[port].is_some() {
            self.save(port);
            self.deliver(bios::SW_CARD, bios::EV_IOE);
        } else {
            self.deliver(bios::SW_CARD, bios::EV_TIMEOUT);
        }
        1
    }

    // ---- files ----------------------------------------------------------------------

    fn free_descriptor(&self) -> Option<usize> {
        (2..FILES).find(|&fd| self.files[fd].is_none())
    }
    fn open(&mut self, ram: &[u8], name: u32, mode: u32) -> Option<u32> {
        let (port, name) = bu_path(&read_string(ram, name))?;
        let Some(card) = self.cards[port].as_mut().filter(|c| c.formatted()) else {
            return Some(u32::MAX);
        };
        let create = mode & bios::O_CREATE != 0;
        let first = if create {
            card.create(&name, (mode >> 16).max(1))
        } else {
            card.find(&name)
        };
        let Some(first) = first else {
            return Some(u32::MAX);
        };
        let size = card.frame(first).size;
        let file = OpenFile {
            port,
            first,
            size,
            position: 0,
            mode,
            dead: false,
        };
        if create {
            self.save(port);
            if let Some(fd) = self.free_descriptor() {
                self.files[fd] = Some(OpenFile {
                    dead: true,
                    ..file.clone()
                });
            }
        }
        let Some(fd) = self.free_descriptor() else {
            return Some(u32::MAX);
        };
        self.files[fd] = Some(file);
        Some(fd as u32)
    }
    /// The card sectors of `count` sectors of `file` from its position.
    fn file_sectors(card: &MemoryCard, file: &OpenFile, count: u32) -> Vec<u32> {
        let chain = card.chain(file.first);
        (0..count)
            .filter_map(|k| {
                let offset = (file.position + k * SECTOR_BYTES as u32) as usize;
                let block = *chain.get(offset / BLOCK_BYTES)?;
                Some(block * SECTORS_PER_BLOCK + (offset % BLOCK_BYTES / SECTOR_BYTES) as u32)
            })
            .collect()
    }
    /// read / write: whole sectors (`bytes / 128`, within the file).
    fn transfer(&mut self, ram: &mut [u8], args: [u32; 4], write: bool) -> Option<u32> {
        let [fd, buffer, bytes, _] = args;
        let file = self.files.get(fd as usize)?.as_ref();
        let Some(file) = file.cloned() else {
            return (fd >= 2).then_some(u32::MAX);
        };
        let allowed = if write { bios::O_WRITE } else { bios::O_READ };
        if file.dead || file.mode & allowed == 0 || self.port_busy(file.port) {
            return Some(u32::MAX);
        }
        let Some(card) = self.cards[file.port].as_ref() else {
            return Some(u32::MAX);
        };
        let count = (bytes >> 7).min(file.size.saturating_sub(file.position) >> 7);
        let sectors = Self::file_sectors(card, &file, count);
        let moved = (sectors.len() * SECTOR_BYTES) as u32;
        if let Some(open) = self.files[fd as usize].as_mut() {
            open.position += moved;
        }
        let work = if write {
            Work::FileWrite { sectors, buffer }
        } else {
            Work::FileRead { sectors, buffer }
        };
        if file.mode & bios::O_ASYNC != 0 {
            let n = moved >> 7;
            self.schedule(
                ram,
                file.port,
                work,
                n * VBLANKS_PER_SECTOR + 1,
                u64::from(n.max(1)) * POLLS_PER_SECTOR,
            );
            Some(0)
        } else {
            // A synchronous transfer completes before returning; the driver
            // delivers its event all the same.
            let pending = Pending {
                port: file.port,
                work,
                due_vblank: 0,
                due_poll: 0,
            };
            self.complete(ram, pending);
            Some(moved)
        }
    }
    /// lseek: the descriptor's position, `offset` bytes from the start or
    /// from where it is; the new position, -1 for a descriptor that is not
    /// open (or the one a create leaves) or another origin. The position is
    /// not checked here: a transfer moves only the whole sectors inside the
    /// file from it.
    fn lseek(&mut self, fd: u32, offset: u32, origin: u32) -> Option<u32> {
        let slot = self.files.get_mut(fd as usize)?;
        let Some(file) = slot.as_mut().filter(|f| !f.dead) else {
            return (fd >= 2).then_some(u32::MAX);
        };
        let position = match origin {
            bios::SEEK_SET => offset,
            bios::SEEK_CUR => file.position.wrapping_add(offset),
            _ => return Some(u32::MAX),
        };
        file.position = position;
        Some(position)
    }
    fn close(&mut self, fd: u32) -> Option<u32> {
        if fd == u32::MAX {
            return Some(u32::MAX);
        }
        let slot = self.files.get_mut(fd as usize)?;
        if fd < 2 {
            return None;
        }
        Some(if slot.take().is_some() { fd } else { u32::MAX })
    }
    /// firstfile / nextfile: fill the DIRENTRY fields the kernel writes for
    /// the next match (see the module documentation).
    fn find_next(&mut self, ram: &mut [u8], entry: u32) -> u32 {
        let Some(search) = self.search.as_mut() else {
            return 0;
        };
        let Some(card) = self.cards[search.port].as_ref().filter(|c| c.formatted()) else {
            return 0;
        };
        while search.next < 16 {
            let frame_index = search.next;
            search.next += 1;
            let frame = card.frame(frame_index);
            if frame.state != FIRST || !name_matches(&search.pattern, &frame.name) {
                continue;
            }
            let mut name = frame.name.clone();
            name.truncate(19);
            name.push(0);
            write_bytes(ram, entry, &name);
            write_bytes(ram, entry + 20, &(frame.state & 0xf0).to_le_bytes());
            write_bytes(ram, entry + 24, &frame.size.to_le_bytes());
            write_bytes(
                ram,
                entry + 32,
                &(frame_index * SECTORS_PER_BLOCK).to_le_bytes(),
            );
            return entry;
        }
        0
    }
    /// libc's firstfile wrapper: the pattern's device name (its leading
    /// characters at or above ';') into its static, then the kernel's.
    fn first_file(&mut self, ram: &mut [u8], pattern: u32, entry: u32) -> u32 {
        let path = read_string(ram, pattern);
        let device: Vec<u8> = path
            .iter()
            .copied()
            .take_while(|&b| (b as i8) >= 0x3b)
            .chain([0])
            .collect();
        write_bytes(ram, bios::FIRST_FILE_STATICS + 8, &device);
        let Some((port, pattern)) = bu_path(&path) else {
            self.search = None;
            return 0;
        };
        self.search = Some(Search {
            port,
            pattern,
            next: 1,
        });
        self.find_next(ram, entry)
    }
}

impl Device for CardDevice {
    fn call(&mut self, ram: &mut [u8], _scratch: &mut [u8], call: HostCall) -> Option<u32> {
        use bios::*;
        let [a0, a1, a2, a3] = call.args;
        if matches!(call.function, TEST_EVENT | WAIT_EVENT) {
            self.polls += 1;
        }
        self.advance(ram);
        match call.function {
            DELIVER_EVENT => {
                self.deliver(a0, a1);
                None
            }
            OPEN_EVENT => {
                let free = self.events.iter().position(|e| e.status == status::UNUSED);
                Some(free.map_or(u32::MAX, |i| {
                    self.events[i] = Event {
                        class: a0,
                        spec: a1,
                        mode: a2,
                        handler: a3,
                        status: status::WAIT,
                        foreign: false,
                    };
                    0xf100_0000 | i as u32
                }))
            }
            CLOSE_EVENT => {
                if let Some(i) = Self::event_index(a0) {
                    self.events[i] = Event::default();
                }
                Some(1)
            }
            ENABLE_EVENT | DISABLE_EVENT => {
                if let Some(i) = Self::event_index(a0)
                    && self.events[i].status != status::UNUSED
                {
                    self.events[i].status = if call.function == ENABLE_EVENT {
                        status::ACTIVE
                    } else {
                        status::WAIT
                    };
                }
                Some(1)
            }
            TEST_EVENT | WAIT_EVENT => {
                let i = self.own_event(a0)?;
                if call.function == WAIT_EVENT && self.events[i].status == status::ACTIVE {
                    self.finish_all(ram);
                }
                let ready = self.events[i].status == status::READY;
                if ready {
                    self.events[i].status = status::ACTIVE;
                }
                Some(u32::from(ready))
            }
            ENTER_CRITICAL_SECTION => {
                let enabled = !self.critical;
                self.critical = true;
                Some(u32::from(enabled))
            }
            EXIT_CRITICAL_SECTION => {
                self.critical = false;
                None
            }
            OPEN => self.open(ram, a0, a1),
            LSEEK => self.lseek(a0, a1, a2),
            READ => self.transfer(ram, call.args, false),
            WRITE => self.transfer(ram, call.args, true),
            CLOSE => self.close(a0),
            FIRST_FILE => Some(self.first_file(ram, a0, a1)),
            NEXT_FILE => Some(self.find_next(ram, a0)),
            CARD_INFO => Some(self.card_request(ram, a0, Work::Info)),
            CARD_LOAD => Some(self.card_request(ram, a0, Work::Load)),
            CARD_CLEAR => Some(self.card_request(ram, a0, Work::Clear)),
            CARD_FORMAT => Some(self.format(ram, a0)),
            START_CARD | STOP_CARD => Some(0),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ac3_flight::{Machine, sdk::test_event};
    use std::{cell::RefCell, rc::Rc};

    #[derive(Clone)]
    pub(super) struct Shared(pub(super) Rc<RefCell<CardDevice>>);
    impl Device for Shared {
        fn call(&mut self, ram: &mut [u8], scratch: &mut [u8], call: HostCall) -> Option<u32> {
            self.0.borrow_mut().call(ram, scratch, call)
        }
    }
    fn machine(device: CardDevice) -> (Machine, Rc<RefCell<CardDevice>>) {
        let shared = Rc::new(RefCell::new(device));
        let mut m = Machine::default();
        m.device = Some(Box::new(Shared(shared.clone())));
        (m, shared)
    }
    fn vblank(m: &mut Machine, n: u32) {
        let count = m.lw(VBLANK_COUNTER);
        m.sw(VBLANK_COUNTER, count + n);
    }
    /// The seven card events of the menus, opened and enabled.
    pub(super) const CARD_EVENTS: [(u32, u32); 7] = [
        (bios::SW_CARD, bios::EV_IOE),
        (bios::SW_CARD, bios::EV_ERROR),
        (bios::SW_CARD, bios::EV_TIMEOUT),
        (bios::SW_CARD, bios::EV_NEW),
        (bios::HW_CARD, bios::EV_IOE),
        (bios::HW_CARD, bios::EV_ERROR),
        (bios::HW_CARD, bios::EV_TIMEOUT),
    ];
    fn open_card_events(m: &mut Machine) -> [u32; 7] {
        let events = CARD_EVENTS.map(|(class, spec)| bios::open_event(m, class, spec, 0x2000, 0));
        for event in events {
            bios::enable_event(m, event);
        }
        events
    }
    fn name(m: &mut Machine, text: &str) -> u32 {
        let at = 0x8010_0000;
        m.write_bytes(at, text.as_bytes());
        m.sb(at + text.len() as u32, 0);
        at
    }

    #[test]
    fn blank_card_has_the_bios_layout() {
        let card = MemoryCard::blank();
        assert!(card.formatted());
        assert_eq!(card.sector(0)[0x7f], b'M' ^ b'C');
        for frame in 1..16 {
            assert_eq!(card.frame(frame).state, FREE);
            assert_eq!(card.frame(frame).next, 0xffff);
            assert_eq!(card.sector(frame)[0x7f], 0xa0);
        }
        assert_eq!(
            &card.sector(16)[..10],
            &[0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0, 0xff, 0xff]
        );
        assert!(card.files().is_empty());
        assert!(!MemoryCard::zeroed().formatted());
    }

    #[test]
    fn events_open_after_libspus_and_report_once() {
        let (mut m, _) = machine(CardDevice::default());
        let events = open_card_events(&mut m);
        assert_eq!(events, std::array::from_fn(|i| 0xf100_0001 + i as u32));
        // Not delivered yet; libspu's event 0 is not answered here.
        assert_eq!(test_event(&mut m, events[0]), 0);
        assert_eq!(test_event(&mut m, 0xf100_0000), 1);
        bios::deliver_event(&mut m, bios::SW_CARD, bios::EV_IOE);
        assert_eq!(test_event(&mut m, events[0]), 1);
        assert_eq!(test_event(&mut m, events[0]), 0);
        // A disabled event misses the delivery.
        bios::disable_event(&mut m, events[0]);
        bios::deliver_event(&mut m, bios::SW_CARD, bios::EV_IOE);
        bios::enable_event(&mut m, events[0]);
        assert_eq!(test_event(&mut m, events[0]), 0);
        bios::close_event(&mut m, events[6]);
        assert_eq!(
            bios::open_event(&mut m, bios::HW_CARD, bios::EV_NEW, 0x2000, 0),
            events[6]
        );
        assert_eq!(bios::enter_critical_section(&mut m), 1);
        assert_eq!(bios::enter_critical_section(&mut m), 0);
        assert_eq!(bios::exit_critical_section(&mut m, 7), 7);
        assert_eq!(bios::enter_critical_section(&mut m), 1);
        assert!(m.take_fault().is_ok());
    }

    #[test]
    fn card_requests_complete_after_their_vblanks_or_polls() {
        let (mut m, device) = machine(CardDevice::new([Some(MemoryCard::blank()), None]));
        let events = open_card_events(&mut m);
        assert_eq!(bios::card_info(&mut m, 0), 1);
        assert_eq!(bios::card_info(&mut m, 0), 0, "port busy");
        assert_eq!(bios::card_info(&mut m, 0x01), 0, "multitap slot");
        assert_eq!(test_event(&mut m, events[0]), 0);
        vblank(&mut m, 1);
        assert_eq!(test_event(&mut m, events[0]), 1);
        // No card in port 2: the probe times out.
        assert_eq!(bios::card_info(&mut m, 0x10), 1);
        vblank(&mut m, 1);
        assert_eq!(test_event(&mut m, events[2]), 1);
        // _card_load: 36 sectors, 72 VBlanks.
        assert_eq!(bios::card_load(&mut m, 0), 1);
        vblank(&mut m, 71);
        assert_eq!(test_event(&mut m, events[0]), 0);
        vblank(&mut m, 1);
        assert_eq!(test_event(&mut m, events[0]), 1);
        // _card_clear: the front end's busy wait sees it after 370 rounds.
        m.write_bytes(0, &[0x5a; 0x80]);
        assert_eq!(bios::card_clear(&mut m, 0), 1);
        let mut rounds = 0;
        loop {
            rounds += 1;
            let done = [4, 5, 6].map(|e| test_event(&mut m, events[e]));
            if done[0] == 1 {
                break;
            }
            assert!(rounds < 1000);
        }
        assert_eq!(rounds, 370);
        let device = device.borrow();
        assert_eq!(
            device.cards[0].as_ref().unwrap().sector(0x3f),
            &[0x5a; 0x80]
        );
        assert!(m.take_fault().is_ok());
    }

    #[test]
    fn the_card_work_follows_the_release_clock() {
        // The Japanese VBlank counter (Layout::vblank_counter of JP).
        const JP_COUNTER: u32 = 0x800b_6174;
        let device = CardDevice::new([Some(MemoryCard::blank()), None]).with_clock(JP_COUNTER);
        let (mut m, _) = machine(device);
        let events = open_card_events(&mut m);
        assert_eq!(bios::card_info(&mut m, 0), 1);
        // PAL's counter moving does not complete it; the JP one does.
        vblank(&mut m, 1);
        assert_eq!(test_event(&mut m, events[0]), 0);
        let count = m.lw(JP_COUNTER);
        m.sw(JP_COUNTER, count + 1);
        assert_eq!(test_event(&mut m, events[0]), 1);
        assert!(m.take_fault().is_ok());
    }

    #[test]
    fn files_create_write_read_back_and_list() {
        let (mut m, _) = machine(CardDevice::default());
        let events = open_card_events(&mut m);
        let path = name(&mut m, "bu10:BESCESP02066AC3PAL");
        assert_eq!(bios::open(&mut m, path, 0x8001), u32::MAX, "no file yet");
        // The create leaves descriptor 2 allocated and returns 3.
        assert_eq!(bios::open(&mut m, path, 0x2_0200), 3);
        assert_eq!(bios::close(&mut m, 3), 3);
        assert_eq!(bios::close(&mut m, 3), u32::MAX);
        let fd = bios::open(&mut m, path, bios::O_WRITE);
        assert_eq!(fd, 3);
        let data: Vec<u8> = (0..0x200).map(|i| i as u8).collect();
        m.write_bytes(0x8011_0000, &data);
        assert_eq!(
            bios::write(&mut m, fd, 0x8011_0000, 0x200),
            0x200,
            "synchronous"
        );
        // The driver delivers its completion even for a synchronous write.
        assert_eq!(test_event(&mut m, events[0]), 1);
        assert_eq!(
            bios::read(&mut m, fd, 0x8012_0000, 0x80),
            u32::MAX,
            "write-only"
        );
        assert_eq!(bios::close(&mut m, fd), 3);
        // The menus' read: asynchronous, whole sectors only.
        let fd = bios::open(&mut m, path, 0x8001);
        assert_eq!(fd, 3);
        assert_eq!(bios::read(&mut m, fd, 0x8012_0000, 0x1fc), 0);
        vblank(&mut m, 6);
        assert_eq!(test_event(&mut m, events[0]), 0);
        vblank(&mut m, 1);
        assert_eq!(test_event(&mut m, events[0]), 1);
        assert_eq!(m.read_bytes(0x8012_0000, 0x180), data[..0x180]);
        assert_eq!(m.lw(0x8012_0180), 0, "the partial sector is not read");
        assert_eq!(bios::close(&mut m, fd), 3);
        // Directory listing: the fields the kernel fills.
        let pattern = name(&mut m, "bu10:BESC*");
        let entry = 0x8013_0000;
        m.write_bytes(entry, &[0xee; 40]);
        assert_eq!(bios::first_file(&mut m, pattern, entry), entry);
        assert_eq!(&m.read_bytes(entry, 19)[..], b"BESCESP02066AC3PAL\0");
        assert_eq!(m.lbu(entry + 19), 0xee);
        assert_eq!(m.lw(entry + 20), 0x50);
        assert_eq!(m.lw(entry + 24), 0x4000);
        assert_eq!(m.lw(entry + 28), 0xeeee_eeee, "next is not written");
        assert_eq!(m.lw(entry + 32), 0x40);
        assert_eq!(&m.read_bytes(bios::FIRST_FILE_STATICS + 8, 3)[..], b"bu\0");
        assert_eq!(bios::next_file(&mut m, entry), 0);
        let other = name(&mut m, "bu00:*.*");
        assert_eq!(bios::first_file(&mut m, other, entry), 0);
        assert!(m.take_fault().is_ok());
    }

    /// The Japanese file's records (offset, bytes) as overlay 1 and 2 move
    /// them: the head, the 0x80 record, three 0x480 records, then six 0x180
    /// records for each of the three accounts.
    fn jp_records() -> Vec<(u32, u32)> {
        let mut records = vec![(0, 0x280), (0x280, 0x80)];
        records.extend((0..3).map(|n| (0x300 + 0x480 * n, 0x480)));
        for account in 0..3 {
            records.extend((0..6).map(|k| (0x1080 + 0x900 * account + 0x180 * k, 0x180)));
        }
        records
    }
    /// One asynchronous record transfer as the Japanese overlays make it:
    /// open, seek (not for the head), read or write, wait for `SW_CARD`
    /// done, close. Returns the VBlanks it took.
    fn jp_transfer(
        m: &mut Machine,
        done: u32,
        path: u32,
        (offset, bytes): (u32, u32),
        buffer: u32,
        write: bool,
    ) -> u32 {
        let mode = if write { 0x8002 } else { 0x8001 };
        let fd = bios::open(m, path, mode);
        // 3 after the create (which keeps 2), 2 on a card loaded afresh.
        assert!(matches!(fd, 2 | 3), "descriptor {fd:#x}");
        if offset != 0 {
            assert_eq!(bios::lseek(m, fd, offset, bios::SEEK_SET), offset);
        }
        let started = if write {
            bios::write(m, fd, buffer, bytes)
        } else {
            bios::read(m, fd, buffer, bytes)
        };
        assert_eq!(started, 0, "asynchronous");
        let mut frames = 0;
        while test_event(m, done) == 0 {
            vblank(m, 1);
            frames += 1;
            assert!(frames < 100);
        }
        assert_eq!(bios::close(m, fd), fd);
        frames
    }

    #[test]
    fn japanese_file_records_move_at_their_offsets() {
        let (mut m, device) = machine(CardDevice::default());
        let events = open_card_events(&mut m);
        let path = name(&mut m, "bu00:BISLPSP02020AC3ES");
        // The create (two blocks), closed at once: descriptor 2 stays taken.
        assert_eq!(bios::open(&mut m, path, 0x2_0200), 3);
        assert_eq!(bios::close(&mut m, 3), 3);
        let records = jp_records();
        let end = records.iter().map(|(o, b)| o + b).max().unwrap();
        assert_eq!(end, 0x2b80);
        assert!(
            end as usize <= 2 * BLOCK_BYTES,
            "two blocks hold every record"
        );
        // Write every record from its own buffer pattern, then read each
        // back into a clean buffer.
        let pattern = |offset: u32, i: u32| ((offset >> 4) as u8) ^ (i as u8).wrapping_mul(7);
        for &(offset, bytes) in &records {
            let data: Vec<u8> = (0..bytes).map(|i| pattern(offset, i)).collect();
            m.write_bytes(0x8011_0000, &data);
            let frames = jp_transfer(&mut m, events[0], path, (offset, bytes), 0x8011_0000, true);
            assert_eq!(frames, (bytes >> 7) * VBLANKS_PER_SECTOR + 1);
        }
        let card = device.borrow().cards[0].clone().unwrap();
        let first = card.find(b"BISLPSP02020AC3ES").unwrap();
        assert_eq!(card.frame(first).size, 0x4000);
        let chain = card.chain(first);
        assert_eq!(chain.len(), 2);
        let file: Vec<u8> = chain
            .iter()
            .flat_map(|&b| card.image()[b as usize * BLOCK_BYTES..][..BLOCK_BYTES].to_vec())
            .collect();
        for &(offset, bytes) in &records {
            let expected: Vec<u8> = (0..bytes).map(|i| pattern(offset, i)).collect();
            assert_eq!(&file[offset as usize..][..bytes as usize], &expected[..]);
        }
        assert!(file[0x2b80..].iter().all(|&b| b == 0), "past the records");
        for &(offset, bytes) in records.iter().rev() {
            m.write_bytes(0x8012_0000, &vec![0xee; bytes as usize + 0x80]);
            jp_transfer(&mut m, events[0], path, (offset, bytes), 0x8012_0000, false);
            let expected: Vec<u8> = (0..bytes).map(|i| pattern(offset, i)).collect();
            assert_eq!(m.read_bytes(0x8012_0000, bytes as usize), expected);
            assert_eq!(m.lbu(0x8012_0000 + bytes), 0xee, "only the record");
        }
        // The card image round trip: the saved image reads the same.
        let image = card.image().to_vec();
        let reloaded = MemoryCard::from_image(image.clone()).unwrap();
        let (mut m, _) = machine(CardDevice::new([Some(reloaded), None]));
        let events = open_card_events(&mut m);
        let path = name(&mut m, "bu00:BISLPSP02020AC3ES");
        for &(offset, bytes) in &records {
            jp_transfer(&mut m, events[0], path, (offset, bytes), 0x8012_0000, false);
            let expected: Vec<u8> = (0..bytes).map(|i| pattern(offset, i)).collect();
            assert_eq!(m.read_bytes(0x8012_0000, bytes as usize), expected);
        }
        assert!(m.take_fault().is_ok());
    }

    #[test]
    fn lseek_origins_and_bad_descriptors() {
        let (mut m, device) = machine(CardDevice::default());
        let path = name(&mut m, "bu10:BISLPSP02020AC3ES");
        assert_eq!(bios::open(&mut m, path, 0x2_0200 | bios::O_WRITE), 3);
        assert_eq!(bios::lseek(&mut m, 3, 0x280, bios::SEEK_SET), 0x280);
        assert_eq!(bios::lseek(&mut m, 3, 0x80, bios::SEEK_CUR), 0x300);
        assert_eq!(
            bios::lseek(&mut m, 3, (-0x100i32) as u32, bios::SEEK_CUR),
            0x200
        );
        assert_eq!(bios::lseek(&mut m, 3, 0, 2), u32::MAX, "no SEEK_END");
        assert_eq!(bios::lseek(&mut m, 2, 0, 0), u32::MAX, "the create's own");
        assert_eq!(bios::lseek(&mut m, 4, 0, 0), u32::MAX, "not open");
        assert_eq!(bios::lseek(&mut m, 99, 0, 0), u32::MAX);
        // A synchronous write lands at the position and advances it.
        m.write_bytes(0x8011_0000, &[0x5a; 0x100]);
        assert_eq!(bios::write(&mut m, 3, 0x8011_0000, 0x100), 0x100);
        assert_eq!(bios::lseek(&mut m, 3, 0, bios::SEEK_CUR), 0x300);
        // At the file's end nothing moves.
        assert_eq!(bios::lseek(&mut m, 3, 0x4000, bios::SEEK_SET), 0x4000);
        assert_eq!(bios::write(&mut m, 3, 0x8011_0000, 0x80), 0);
        let card = device.borrow().cards[1].clone().unwrap();
        let first = card.find(b"BISLPSP02020AC3ES").unwrap();
        let at = first as usize * BLOCK_BYTES;
        assert!(card.image()[at..at + 0x200].iter().all(|&b| b == 0));
        assert!(
            card.image()[at + 0x200..at + 0x300]
                .iter()
                .all(|&b| b == 0x5a)
        );
        assert!(card.image()[at + 0x300..at + 0x400].iter().all(|&b| b == 0));
        assert!(m.take_fault().is_ok());
    }

    #[test]
    fn format_writes_libcards_layout_and_buffers() {
        let (mut m, device) = machine(CardDevice::new([Some(MemoryCard::zeroed()), None]));
        let events = open_card_events(&mut m);
        assert_eq!(bios::card_format(&mut m, 0), 1);
        assert_eq!(test_event(&mut m, events[0]), 1, "the directory load");
        assert_eq!(test_event(&mut m, events[4]), 0);
        let card = device.borrow().cards[0].clone().unwrap();
        assert!(card.formatted());
        assert_eq!(card.frame(15).state, FREE);
        assert_eq!(
            &card.sector(35)[..10],
            &[0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(card.sector(35)[0x7f], 0);
        assert_eq!(m.lw(bios::FORMAT_FRAMES + 14 * 0x20), 0xa0);
        assert_eq!(m.lhu(bios::FORMAT_FRAMES + 14 * 0x20 + 8), 0xffff);
        assert_eq!(m.lw(bios::FORMAT_BROKEN + 19 * 4), u32::MAX);
        assert_eq!(m.read_bytes(bios::FORMAT_BUFFER, 0x80), card.sector(0));
        // No card: accepted, the load times out.
        assert_eq!(bios::card_format(&mut m, 0x10), 1);
        assert_eq!(test_event(&mut m, events[2]), 1);
        assert!(m.take_fault().is_ok());
    }

    #[test]
    fn card_files_are_saved_after_writes() {
        let directory = std::env::temp_dir().join(format!("ac3-card-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("card1.mcr");
        let _ = fs::remove_file(&path);
        let source = CardSource::parse(path.to_str().unwrap());
        assert_eq!(CardSource::parse("NONE"), CardSource::Absent);
        let device = CardDevice::from_sources(&[source, CardSource::Absent]).unwrap();
        let (mut m, device) = machine(device);
        let file = name(&mut m, "bu00:TEST");
        let fd = bios::open(&mut m, file, (1 << 16) | bios::O_CREATE | bios::O_WRITE);
        assert_eq!(fd, 3);
        m.write_bytes(0x8011_0000, &[7; 0x80]);
        assert_eq!(bios::write(&mut m, fd, 0x8011_0000, 0x80), 0x80);
        assert!(device.borrow().error().is_none());
        let mut saved = MemoryCard::open(&path).unwrap();
        let first = saved.find(b"TEST").unwrap();
        assert_eq!(saved.sector(first * SECTORS_PER_BLOCK), &[7; 0x80]);
        assert!(saved.erase(b"TEST"));
        assert!(saved.find(b"TEST").is_none());
        assert_eq!(saved.frame(first).state, 0xa1);
        let _ = fs::remove_dir_all(&directory);
        assert!(m.take_fault().is_ok());
    }
}

/// Closed loops on the original captures (need `AC3_DATA_DIR`): the
/// translated card layers of the front end (overlay 1) and the save screen
/// (overlay 2) run against this device, from captured states, and must
/// reach the original's results: the card block the menus read, the
/// routines' results and RAM, and the card images the original's kernel
/// wrote (the capture profiles' `memcard*.mcd` after the run; sector 63,
/// the write test of `_card_clear` from kernel RAM, aside).
#[cfg(test)]
mod captures {
    use super::tests::{CARD_EVENTS, Shared};
    use super::*;
    use ac3_flight::{
        Machine, dispatch, ov1, ov1_card, ov1_load,
        ov2_hub::card as hub,
        snapshot::{self, Snapshot, SnapshotInfo},
    };
    use std::{cell::RefCell, rc::Rc};

    fn directory(name: &str) -> PathBuf {
        PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR"))
            .join("analysis/flight-runtime")
            .join(name)
    }

    struct Capture {
        directory: PathBuf,
        index: Vec<SnapshotInfo>,
    }

    impl Capture {
        fn new(name: &str) -> Self {
            let directory = directory(name);
            let index = snapshot::read_index(&directory.join("snapshots.jsonl")).expect("index");
            Self { directory, index }
        }
        fn load(&self, info: &SnapshotInfo) -> Snapshot {
            Snapshot::load(&self.directory, info).expect("images")
        }
        /// The return snapshot of `entry`.
        fn exit(&self, entry: &SnapshotInfo) -> &SnapshotInfo {
            self.index
                .iter()
                .find(|s| {
                    s.callee == entry.callee
                        && s.stage == "return"
                        && s.sequence > entry.sequence
                        && s.ra == entry.ra
                        && s.registers.gpr[29] == entry.registers.gpr[29]
                })
                .expect("return")
        }
        /// The `nth` entry of `callee`.
        fn entry(&self, callee: u32, nth: usize) -> &SnapshotInfo {
            self.index
                .iter()
                .filter(|s| s.callee == callee && s.stage == "entry")
                .nth(nth)
                .expect("entry")
        }
        /// The innermost captured routine whose window holds snapshot
        /// sequence `after`: its entry and return.
        fn window(&self, after: u64) -> (&SnapshotInfo, &SnapshotInfo) {
            self.index
                .iter()
                .filter(|s| s.stage == "entry" && s.sequence <= after)
                .map(|e| (e, self.exit(e)))
                .rfind(|(_, x)| x.sequence > after)
                .expect("window")
        }
        /// The original's BIOS / libcard calls (`bios.jsonl`).
        fn bios_calls(&self) -> Vec<Logged> {
            let text = fs::read_to_string(self.directory.join("bios.jsonl")).expect("bios.jsonl");
            text.lines()
                .map(|l| {
                    let v: serde_json::Value = serde_json::from_str(l).unwrap();
                    let int = |x: &serde_json::Value| x.as_u64().unwrap() as u32;
                    Logged {
                        after: v["after"].as_u64().unwrap(),
                        frame: int(&v["frame"]),
                        function: int(&v["function"]),
                        args: std::array::from_fn(|i| int(&v["args"][i])),
                        v0: int(&v["v0"]),
                    }
                })
                .collect()
        }
        /// The `nth` logged call of `function`.
        fn bios_call(&self, function: u32, nth: usize) -> Logged {
            self.bios_calls()
                .into_iter()
                .filter(|c| c.function == function)
                .nth(nth)
                .expect("bios call")
        }
        fn card(&self, n: u32) -> MemoryCard {
            let path = self.directory.join(format!("profile/memcard{n}.mcd"));
            MemoryCard::from_image(fs::read(path).expect("card")).unwrap()
        }
    }

    /// One original BIOS / libcard call: the snapshot sequence before it,
    /// its frame (VBlank), routine, argument registers and result.
    #[derive(Clone, Debug)]
    struct Logged {
        after: u64,
        frame: u32,
        function: u32,
        args: [u32; 4],
        v0: u32,
    }

    /// A device with `cards` and the menus' seven card events open (and
    /// enabled): descriptors 0xF1000001..7, as the captured card blocks
    /// hold them.
    fn primed(cards: [Option<MemoryCard>; 2], enabled: bool) -> Rc<RefCell<CardDevice>> {
        let mut device = CardDevice::new(cards);
        let mut ram = vec![0u8; 0x20_0000];
        let mut scratch = vec![0u8; 0x400];
        for (class, spec) in CARD_EVENTS {
            let call = HostCall {
                function: bios::OPEN_EVENT,
                args: [class, spec, 0x2000, 0],
            };
            let event = device.call(&mut ram, &mut scratch, call).unwrap();
            if enabled {
                let call = HostCall {
                    function: bios::ENABLE_EVENT,
                    args: [event, 0, 0, 0],
                };
                device.call(&mut ram, &mut scratch, call);
            }
        }
        Rc::new(RefCell::new(device))
    }

    fn machine(snapshot: &Snapshot, device: &Rc<RefCell<CardDevice>>) -> Machine {
        let mut m = snapshot.machine();
        m.device = Some(Box::new(Shared(device.clone())));
        m
    }
    fn next_vblank(m: &mut Machine) {
        let count = m.lw(VBLANK_COUNTER);
        m.sw(VBLANK_COUNTER, count.wrapping_add(1));
    }

    /// RAM written in interrupt context during any window (kernel RAM,
    /// libpad's driver, libetc's interrupt manager and stack, the libpad /
    /// libspu / libcd BSS and the pad buffers), as `tests/ov2_hub/states.rs`.
    const INTERRUPT_CONTEXT: [(u32, u32); 5] = [
        (0x8000_0000, 0x8001_0000),
        (0x800b_2504, 0x800b_258c),
        (0x800b_2f2c, 0x800b_4034),
        (0x800c_59d0, 0x800c_5cbc),
        (0x800b_e1e0, 0x800b_e224),
    ];

    /// All RAM (below the stack) and scratchpad differences, except the
    /// interrupt context and `ignore`.
    fn differences(m: &Machine, exit: &Snapshot, ignore: &[(u32, u32)]) -> Vec<u32> {
        let d = snapshot::compare(m, exit);
        d.ram
            .into_iter()
            .chain(d.scratch)
            .filter(|a| {
                !INTERRUPT_CONTEXT
                    .iter()
                    .chain(ignore)
                    .any(|(s, e)| (*s..*e).contains(a))
            })
            .collect()
    }

    /// Differences of the card block (`block`, 0x14E4 bytes), except
    /// `ignore` (offsets): offset, ours, the original's.
    fn block_differences(
        m: &Machine,
        exit: &Snapshot,
        block: u32,
        ignore: &[u32],
    ) -> Vec<(u32, u8, u8)> {
        (0..0x14e4u32)
            .filter(|o| !ignore.contains(o))
            .map(|o| {
                let i = ((block + o) & 0x1f_ffff) as usize;
                (o, m.ram()[i], exit.ram[i])
            })
            .filter(|(_, a, b)| a != b)
            .collect()
    }

    /// Sectors where two images differ, the write test (63) aside.
    fn sector_differences(actual: &MemoryCard, expected: &MemoryCard) -> Vec<u32> {
        (0..1024)
            .filter(|&s| s != 0x3f && actual.sector(s) != expected.sector(s))
            .collect()
    }

    /// Front end: one boot frame's card half: poll both slots, then the
    /// card step (`0x800D63D0[state0 +4]`); the step's result.
    fn card_frame(m: &mut Machine, poll: bool) -> u32 {
        const STATE0: u32 = 0x800d_8744;
        const CARD_STEPS: u32 = 0x800d_63d0;
        if poll {
            ov1_load::poll_slots(m);
        }
        let state = m.lw(STATE0);
        let step = m.lw(state + 4);
        let callback = m.lw(CARD_STEPS + 4 * step);
        let result = dispatch::call(m, callback, [0; 4]);
        let state = m.lw(STATE0);
        if result & 1 != 0 {
            m.sw(state + 4, step + 1);
        } else if result & 4 != 0 {
            m.sw(state + 4, 7);
        }
        result
    }

    /// Run the front end's card probe frame by frame until its sequence
    /// ends (result bit 1 or 2); the result and the frames run.
    fn run_card_sequence(m: &mut Machine, first_poll: bool) -> (u32, u32) {
        let mut result = card_frame(m, first_poll);
        let mut frames = 1;
        while result & 6 == 0 {
            next_vblank(m);
            result = card_frame(m, true);
            frames += 1;
            assert!(m.fault().is_none(), "{:?}", m.fault());
            assert!(frames < 2000, "the card sequence does not end");
        }
        (result, frames)
    }

    /// Card block offsets that count time: the probe's timeout (+0x14C4).
    const TIMING: [u32; 4] = [0x14c4, 0x14c5, 0x14c6, 0x14c7];
    /// The front end's probe keeps cycling over both slots in the
    /// background (+0x14CC, +0x14CD: the slots' steps): where it stands when
    /// the card sequence ends depends on the requests' timing (one step
    /// apart in `ov1-010` and `ov1-019`); with the timeout, the only bytes
    /// left out.
    const PROBE: [u32; 6] = [0x14c4, 0x14c5, 0x14c6, 0x14c7, 0x14cc, 0x14cd];

    /// ov1-010: the cold start with two empty cards: the card events
    /// (800CCCB8 initialize, exact), then the probe of both slots from
    /// card step 0 until it gives up on the save (800CB828): the card block
    /// as the original's.
    #[test]
    #[ignore = "requires AC3_DATA_DIR and ov1-010"]
    fn front_end_cold_start_ov1_010() {
        let capture = Capture::new("ov1-010");
        // The kernel at this capture's start: only libspu's event is open.
        let cards = [Some(capture.card(1)), Some(capture.card(1))];
        let device = Rc::new(RefCell::new(CardDevice::new(cards)));
        let entry = capture.entry(0x800c_ccb8, 0);
        let exit = capture.exit(entry);
        let entry = capture.load(entry);
        let args = [entry.gpr(4), entry.gpr(5), entry.gpr(6), entry.gpr(7)];
        let mut m = machine(&entry, &device);
        dispatch::call(&mut m, 0x800c_ccb8, args);
        assert!(m.take_fault().is_ok());
        let exit = capture.load(exit);
        let d = differences(&m, &exit, &[]);
        assert!(d.is_empty(), "initialize: {:08x?}", &d[..d.len().min(16)]);
        let entry = capture.entry(0x800c_b690, 0);
        let mut m = machine(&capture.load(entry), &device);
        let (result, frames) = run_card_sequence(&mut m, false);
        assert_eq!(result & 4, 4, "no save: the card sequence is abandoned");
        let end = capture.entry(0x800c_b828, 0);
        let end = capture.load(capture.exit(end));
        let card = m.lw(ov1::CARD);
        let d = block_differences(&m, &end, card, &PROBE);
        assert!(d.is_empty(), "card block +{:x?}", &d[..d.len().min(16)]);
        eprintln!(
            "ov1-010: card sequence over {frames} frames, {} requests",
            device.borrow().completed
        );
    }

    /// ov1-019: the cold start with a card holding two saves: the probe
    /// from the first poll, the save read (35 sectors of the 0x11FC-byte
    /// image), its checks and the options applied (800CB7CC): the card
    /// block as the original's.
    #[test]
    #[ignore = "requires AC3_DATA_DIR and ov1-019"]
    fn front_end_saved_game_ov1_019() {
        let capture = Capture::new("ov1-019");
        let device = primed([Some(capture.card(1)), Some(capture.card(2))], true);
        let entry = capture.entry(0x800c_bdac, 0);
        let mut m = machine(&capture.load(entry), &device);
        // The _card_info of the frame before, still pending.
        assert_eq!(bios::card_info(&mut m, 0), 1);
        ov1_card::poll(&mut m);
        let resource = m.lw(ov1::RESOURCE);
        let (first, _) = ov1_card::slot_state(&mut m, 0);
        m.sw(resource + 4, first);
        let (second, _) = ov1_card::slot_state(&mut m, 1);
        m.sw(resource + 8, second);
        let (result, frames) = run_card_sequence(&mut m, false);
        assert_eq!(result & 2, 2, "the save is read");
        let end = capture.entry(0x800c_b7cc, 0);
        let end = capture.load(capture.exit(end));
        let card = m.lw(ov1::CARD);
        let d = block_differences(&m, &end, card, &PROBE);
        assert!(d.is_empty(), "card block +{:x?}", &d[..d.len().min(16)]);
        eprintln!(
            "ov1-019: card sequence over {frames} frames, {} requests",
            device.borrow().completed
        );
    }

    /// The save screen (overlay 2) around the `nth` original call of
    /// `function`: a device with the capture's cards brought to the
    /// original's kernel state by the original's earlier calls (each at its
    /// VBlank, strings and buffers from the window's entry RAM), then the
    /// innermost captured routine around the call, run natively against it
    /// and compared with its return (all RAM and scratchpad but the interrupt
    /// context, `ignore`, and the timeout of the card block). Returns the
    /// machine, the device and the routine's arguments.
    fn save_screen_window(
        capture: &Capture,
        cards: [Option<MemoryCard>; 2],
        function: u32,
        nth: usize,
        ignore: &[(u32, u32)],
    ) -> (Machine, Rc<RefCell<CardDevice>>, u32, [u32; 4]) {
        let call = capture.bios_call(function, nth);
        let (entry, exit) = capture.window(call.after);
        let callee = entry.callee;
        let entry = capture.load(entry);
        let exit = capture.load(exit);
        let device = replayed(capture, cards, &entry);
        let mut m = machine(&entry, &device);
        let args = [entry.gpr(4), entry.gpr(5), entry.gpr(6), entry.gpr(7)];
        dispatch::call(&mut m, callee, args);
        assert!(m.take_fault().is_ok());
        let card = m.lw(hub::CARD);
        let timing: Vec<(u32, u32)> = vec![(card + 0x14c4, card + 0x14c8)];
        let ignore: Vec<(u32, u32)> = ignore.iter().copied().chain(timing).collect();
        let d = differences(&m, &exit, &ignore);
        let shown: Vec<String> = d
            .iter()
            .take(16)
            .map(|&a| {
                let i = (a & 0x1f_ffff) as usize;
                format!("{a:08x}: {:02x} vs {:02x}", m.ram()[i], exit.ram[i])
            })
            .collect();
        assert!(
            d.is_empty(),
            "{callee:08x} around {function:08x}: {shown:?}"
        );
        (m, device, callee, args)
    }

    /// A device with `cards` brought to the original's kernel state at
    /// `entry` by the original's calls before it (each at its VBlank,
    /// strings and buffers from `entry`'s RAM).
    fn replayed(
        capture: &Capture,
        cards: [Option<MemoryCard>; 2],
        entry: &Snapshot,
    ) -> Rc<RefCell<CardDevice>> {
        let mut device = CardDevice::new(cards);
        let base = read_u32(&entry.ram, VBLANK_COUNTER).wrapping_sub(entry.info.frame as u32);
        let mut ram = entry.ram.to_vec();
        let mut scratch = vec![0u8; 0x400];
        for c in capture
            .bios_calls()
            .iter()
            .filter(|c| c.after < entry.info.sequence)
        {
            let counter = c.frame.wrapping_add(base).to_le_bytes();
            write_bytes(&mut ram, VBLANK_COUNTER, &counter);
            let call = HostCall {
                function: c.function,
                args: c.args,
            };
            device.call(&mut ram, &mut scratch, call);
        }
        Rc::new(RefCell::new(device))
    }

    /// Run the save screen's routine `callee` once per VBlank until the
    /// device has no request in progress, then `extra` frames more.
    fn run_frames(
        m: &mut Machine,
        device: &Rc<RefCell<CardDevice>>,
        callee: u32,
        args: [u32; 4],
        extra: u32,
    ) {
        let mut left = extra;
        for _ in 0..1000 {
            next_vblank(m);
            dispatch::call(m, callee, args);
            assert!(m.fault().is_none(), "{:?}", m.fault());
            if !device.borrow().busy() {
                if left == 0 {
                    return;
                }
                left -= 1;
            }
        }
        panic!("the request does not complete");
    }

    /// ov2hub-008: the save screen on the profile's new card (Redux's
    /// formatted card, as `ov1-012`'s), the file created (descriptor 3: the
    /// create leaves 2 allocated): the save (800CDED4, port 0, finish 1)
    /// from the frame it opens the file and starts the write (its entry was
    /// not captured: from its first callee's entry, the state it saw), once
    /// per VBlank until done: the card block as the original's after it and
    /// the card the original's kernel wrote.
    #[test]
    #[ignore = "requires AC3_DATA_DIR, ov1-012 and ov2hub-008"]
    fn save_screen_new_file_ov2hub_008() {
        let capture = Capture::new("ov2hub-008");
        let blank = Capture::new("ov1-012").card(1);
        let create = capture.bios_call(bios::OPEN, 4);
        assert_eq!((create.args[1], create.v0), (0x1_0200, 3));
        let write = capture.bios_call(bios::WRITE, 0);
        let seal = capture
            .index
            .iter()
            .rfind(|s| s.callee == 0x800c_dd50 && s.stage == "entry" && s.sequence <= write.after)
            .expect("seal");
        let start = capture.load(seal);
        let device = replayed(&capture, [Some(blank.clone()), Some(blank)], &start);
        let mut m = machine(&start, &device);
        let mut result = 0;
        for _ in 0..1000 {
            result = hub::save_cded4(&mut m, 0, 1);
            assert!(m.fault().is_none(), "{:?}", m.fault());
            if result != 0 {
                break;
            }
            next_vblank(&mut m);
        }
        assert_eq!(result, 1);
        let block = m.lw(hub::CARD);
        assert_eq!(m.lw(block + 0x122c), 3, "the descriptor");
        let close = capture.bios_call(bios::CLOSE, 7);
        let (_, end) = capture.window(close.after + 1);
        let end = capture.load(end);
        let d = block_differences(&m, &end, block, &TIMING);
        assert!(d.is_empty(), "card block +{:x?}", &d[..d.len().min(16)]);
        let written = device.borrow().cards[0].clone().unwrap();
        let d = sector_differences(&written, &capture.card(1));
        assert!(d.is_empty(), "card sectors {d:?}");
    }

    /// ov2hub-009: the card ov2hub-008 wrote: the frame that opens the save
    /// and starts the read, frames until it is done (the 35 sectors in the
    /// card block, checks), then the frame that starts the overwrite and
    /// frames until it is done: the card the original wrote.
    #[test]
    #[ignore = "requires AC3_DATA_DIR, ov2hub-008 and ov2hub-009"]
    fn save_screen_load_and_overwrite_ov2hub_009() {
        let capture = Capture::new("ov2hub-009");
        let card = Capture::new("ov2hub-008").card(1);
        let cards = [Some(card.clone()), Some(capture.card(2))];
        let (mut m, device, callee, args) =
            save_screen_window(&capture, cards.clone(), bios::READ, 0, &[]);
        run_frames(&mut m, &device, callee, args, 0);
        let block = m.lw(hub::CARD);
        let first = card.find(b"BESCESP02066AC3PAL").unwrap();
        let loaded = m.read_bytes(block, 0x1180);
        assert_eq!(
            &loaded[..],
            &card.image()[first as usize * BLOCK_BYTES..][..0x1180]
        );
        let (mut m, device, callee, args) =
            save_screen_window(&capture, cards, bios::WRITE, 0, &[]);
        run_frames(&mut m, &device, callee, args, 0);
        let written = device.borrow().cards[0].clone().unwrap();
        let d = sector_differences(&written, &capture.card(1));
        assert!(d.is_empty(), "card sectors {d:?}");
    }

    /// ov2hub-011: an all-zero card: the frame of the format (libcard's
    /// buffers, the result) and the card the original formatted.
    #[test]
    #[ignore = "requires AC3_DATA_DIR and ov2hub-011"]
    fn save_screen_format_ov2hub_011() {
        let capture = Capture::new("ov2hub-011");
        assert_eq!(capture.bios_call(bios::CARD_FORMAT, 0).v0, 1);
        let cards = [Some(MemoryCard::zeroed()), Some(capture.card(2))];
        let (_, device, _, _) = save_screen_window(&capture, cards, bios::CARD_FORMAT, 0, &[]);
        let written = device.borrow().cards[0].clone().unwrap();
        let d = sector_differences(&written, &capture.card(1));
        assert!(d.is_empty(), "card sectors {d:?}");
    }

    /// ov2hub-012 / ov2hub-008: the frame of the card listing (firstfile /
    /// nextfile) on a card with another product's file and on an empty
    /// card (all RAM but the kernel entry libc saves at 0x800C9D60).
    #[test]
    #[ignore = "requires AC3_DATA_DIR, ov2hub-008 and ov2hub-012"]
    fn save_screen_listing_ov2hub_012() {
        // ov2hub-012's run only read its card; ov2hub-008 started from
        // Redux's formatted card (as ov1-012's).
        let blank = Capture::new("ov1-012").card(1);
        for (name, found) in [("ov2hub-012", 0x801f_ff08), ("ov2hub-008", 0)] {
            let capture = Capture::new(name);
            assert_eq!(capture.bios_call(bios::FIRST_FILE, 0).v0, found);
            let first = if found == 0 {
                blank.clone()
            } else {
                capture.card(1)
            };
            let cards = [Some(first), Some(capture.card(2))];
            save_screen_window(
                &capture,
                cards,
                bios::FIRST_FILE,
                0,
                &[(0x800c_9d60, 0x800c_9d64)],
            );
        }
    }
}
