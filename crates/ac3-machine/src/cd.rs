//! The host CD drive: [`CdDrive`], a drive model answering the libcd host
//! protocol (the CD host calls of [`crate::host_ids`]) from a [`Disc`], and
//! the disc sources it reads ([`RawImage`], [`Archives`], and [`DiscSet`],
//! the discs of a multi-disc game).
//!
//! **The lid.** The drive models its lid (shell) for disc changes
//! ([`Lid`], [`CdDrive::change_disc`]): while it is open the status byte has
//! bit 4 (shell open) and no motor bit, reading stops (an INT5 with error
//! 0x08, "door opened", when it was reading) and commands that need the
//! disc fail (INT5, 0x08). After it closes the drive spins the new disc up
//! ([`SPIN_UP_VBLANKS`]; commands needing the disc answer INT5 0x80, "not
//! ready yet") and turns the motor bit on. Bit 4 stays set until the first
//! Getstat after the close, as on the hardware (psx-spx, "CDROM Controller
//! Status": "Once shell open"). The game then re-identifies the disc and
//! searches the new disc's ISO directory itself (CdSearchFile goes to
//! `inner`, which the host points at the new image).
//!
//! **A game waiting for a disc change.** A game that wants another disc
//! stops the motor and polls Getstat until the lid opens (the Japanese
//! release's CD monitor, JP 8002A2C0 state 2 = PAL 80029E8C: Setmode 0,
//! Stop, then Getstat each pass). [`CdDrive::awaits_lid`] reports that:
//! [`LID_WAIT_POLLS`] Getstats after a Stop with nothing else in between,
//! the lid closed and the motor off. The host changes the disc then.
//!
//! Version-independent: the only game RAM it reads is the VBlank count it
//! paces sectors and the lid by, at the version's addresses
//! ([`VBlankClock`]).
use crate::host_ids;
use crate::machine::{Device, HostCall};
use std::collections::VecDeque;
use std::io::{Read, Seek, SeekFrom};

/// Bytes of a raw CD sector (sync, header, subheader, data, EDC/ECC).
pub const SECTOR_BYTES: usize = 2352;

/// Where the drive reads the guest's VBlank count: libetc's VBlank counter
/// (`counter`, advanced by its VBlank handler) and VSync's record of the
/// VBlank its last wait reached (`vsync_record`). The clock is the counter,
/// or the record when that is later (a host that does not deliver VBlank
/// interrupts). Each version crate names its own words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VBlankClock {
    pub counter: u32,
    pub vsync_record: u32,
}

impl VBlankClock {
    /// The clock in `ram`.
    pub fn read(self, ram: &[u8]) -> u32 {
        let counter = read_word(ram, self.counter);
        let waited = read_word(ram, self.vsync_record);
        if (waited.wrapping_sub(counter) as i32) > 0 {
            waited
        } else {
            counter
        }
    }
}

/// Raw sectors by logical block (LBA 0 = 00:02:00), of the disc in the
/// drive. A disc changer ([`DiscSet`]) also says which of its discs that
/// is; a single image is disc 0 of one.
pub trait Disc {
    /// The raw sector `lba`, or `None` past the end of the disc.
    fn sector(&mut self, lba: u32) -> Option<[u8; SECTOR_BYTES]>;
    /// Discs the drive can be given (1 for a single image).
    fn discs(&self) -> usize {
        1
    }
    /// The index of the disc in the drive (0: disc 1).
    fn inserted(&self) -> usize {
        0
    }
    /// Put disc `index` in the drive (with the lid open,
    /// [`CdDrive::change_disc`]); `false` when there is no such disc.
    fn insert(&mut self, index: usize) -> bool {
        index == 0
    }
}

/// The discs of a multi-disc game in one drive (a disc changer the host
/// operates): the inserted one answers. The host swaps discs with the lid
/// open ([`CdDrive::change_disc`]).
pub struct DiscSet<D> {
    discs: Vec<D>,
    inserted: usize,
}

impl<D> DiscSet<D> {
    /// The set of `discs` (disc 1 first) with disc index `inserted` in the
    /// drive; `None` for no discs or an index past them.
    pub fn new(discs: Vec<D>, inserted: usize) -> Option<Self> {
        (inserted < discs.len()).then_some(Self { discs, inserted })
    }
    /// One disc.
    pub fn single(disc: D) -> Self {
        Self {
            discs: vec![disc],
            inserted: 0,
        }
    }
    /// The disc the drive reads.
    pub fn current(&mut self) -> &mut D {
        &mut self.discs[self.inserted]
    }
}

impl<D: Disc> Disc for DiscSet<D> {
    fn sector(&mut self, lba: u32) -> Option<[u8; SECTOR_BYTES]> {
        self.current().sector(lba)
    }
    fn discs(&self) -> usize {
        self.discs.len()
    }
    fn inserted(&self) -> usize {
        self.inserted
    }
    fn insert(&mut self, index: usize) -> bool {
        let fits = index < self.discs.len();
        if fits {
            self.inserted = index;
        }
        fits
    }
}

/// A raw (2352-byte sector) image of the whole disc, such as the `.bin`.
pub struct RawImage<R>(pub R);
impl<R: Read + Seek> Disc for RawImage<R> {
    fn sector(&mut self, lba: u32) -> Option<[u8; SECTOR_BYTES]> {
        let mut sector = [0; SECTOR_BYTES];
        self.0
            .seek(SeekFrom::Start(u64::from(lba) * SECTOR_BYTES as u64))
            .ok()?;
        self.0.read_exact(&mut sector).ok()?;
        Some(sector)
    }
}

/// The disc as the game's two archives: ACE.BPB (2048-byte Mode 2 Form 1
/// user data from LBA `bpb_lba`; header and subheader rebuilt as on the
/// disc: file 0, channel 0, submode 0x08, 0x89 on the last sector) and
/// ACE.SPB.raw2352 (raw sectors from LBA `spb_lba`). Other sectors read as
/// zeros.
pub struct Archives<B, S> {
    pub bpb: B,
    pub bpb_lba: u32,
    pub bpb_sectors: u32,
    pub spb: S,
    pub spb_lba: u32,
    pub spb_sectors: u32,
}
impl<B: Read + Seek, S: Read + Seek> Disc for Archives<B, S> {
    fn sector(&mut self, lba: u32) -> Option<[u8; SECTOR_BYTES]> {
        let mut sector = [0; SECTOR_BYTES];
        if let Some(index) = lba
            .checked_sub(self.spb_lba)
            .filter(|&i| i < self.spb_sectors)
        {
            self.spb
                .seek(SeekFrom::Start(u64::from(index) * SECTOR_BYTES as u64))
                .ok()?;
            self.spb.read_exact(&mut sector).ok()?;
            return Some(sector);
        }
        sector[1..11].fill(0xff);
        let absolute = lba + 150;
        let bcd = |v: u32| (((v / 10) << 4) | (v % 10)) as u8;
        sector[12] = bcd(absolute / 4500);
        sector[13] = bcd(absolute / 75 % 60);
        sector[14] = bcd(absolute % 75);
        sector[15] = 2;
        if let Some(index) = lba
            .checked_sub(self.bpb_lba)
            .filter(|&i| i < self.bpb_sectors)
        {
            let submode = if index + 1 == self.bpb_sectors {
                0x89
            } else {
                0x08
            };
            sector[18] = submode;
            sector[22] = submode;
            self.bpb
                .seek(SeekFrom::Start(u64::from(index) * 2048))
                .ok()?;
            self.bpb.read_exact(&mut sector[24..24 + 2048]).ok()?;
        }
        Some(sector)
    }
}

/// Where the drive sends XA-ADPCM sectors and the CD audio mix.
pub trait CdAudio {
    /// A raw sector the drive plays (Setmode XA-ADPCM; with the filter
    /// only sectors of the Setfilter file and channel).
    fn xa_sector(&mut self, sector: &[u8; SECTOR_BYTES]);
    /// CdMix's attenuation (left to left, left to right, right to right,
    /// right to left; 0x80 = 100%).
    fn mix(&mut self, attenuation: [u8; 4]);
    /// Mute (0x0B) / Demute (0x0C).
    fn mute(&mut self, _muted: bool) {}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Response {
    kind: u8,
    count: u8,
    bytes: [u8; 8],
}
impl Response {
    fn new(kind: u8, bytes: &[u8]) -> Self {
        let mut all = [0; 8];
        all[..bytes.len()].copy_from_slice(bytes);
        Self {
            kind,
            count: bytes.len() as u8,
            bytes: all,
        }
    }
}

const STAT_MOTOR: u8 = 0x02;
const STAT_SHELL_OPEN: u8 = 0x10;
const STAT_READING: u8 = 0x20;
const STAT_ERROR: u8 = 0x01;
/// INT5 error codes (the second response byte; psx-spx "CDROM - Response
/// Error codes"): the door was opened, the drive is not ready yet.
const ERROR_DOOR_OPENED: u8 = 0x08;
const ERROR_NOT_READY: u8 = 0x80;

/// VBlanks the lid stays open in a disc change ([`CdDrive::change_disc`]).
pub const LID_OPEN_VBLANKS: u32 = 30;
/// VBlanks from closing the lid until the new disc spins (the motor bit).
/// The Japanese CD monitor gives the drive 180 VBlanks (JP 8002AD6C step 1).
pub const SPIN_UP_VBLANKS: u32 = 60;
/// Getstat polls after a Stop, with nothing else in between, that mean
/// the game waits for the lid to open ([`CdDrive::awaits_lid`]).
pub const LID_WAIT_POLLS: u32 = 4;

/// The drive's lid (shell).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Lid {
    /// Closed, with the disc as the commands left it.
    #[default]
    Closed,
    /// Open; in a disc change it closes again at VBlank `close_at`.
    Open { close_at: Option<u32> },
    /// Closed; the disc spins up and the motor is on from VBlank `ready`.
    SpinningUp { ready: u32 },
}

/// Mode bits (Setmode).
const MODE_SPEED: u8 = 0x80;
const MODE_XA_ADPCM: u8 = 0x40;
const MODE_WHOLE_SECTOR: u8 = 0x20;
const MODE_IGNORE: u8 = 0x10;
const MODE_FILTER: u8 = 0x08;

/// A host CD drive answering the libcd host protocol (the CD calls of
/// [`host_ids`]) from a [`Disc`]. Command responses: an acknowledge (INT3, the status
/// byte, plus the command's data) at the next poll, and for Standby, Stop,
/// Pause, Init, SetSession, SeekL/SeekP and GetID a completion (INT2) at
/// the poll after it. ReadN/ReadS read from the last Setloc position (or
/// continue) at 75 sectors per second, doubled with Setmode bit 7, paced by
/// the VBlank count ([`CdDrive::clock`]) at [`CdDrive::vblank_rate`] Hz after
/// [`CdDrive::seek_vblanks`]; with XA-ADPCM (Setmode bit 6) sectors whose
/// submode has audio, form 2 and real-time (0x64) go to [`CdDrive::audio`]
/// (with the filter, Setmode bit 3, only those of the Setfilter file and
/// channel) and raise no interrupt; every other sector raises data ready
/// (INT1, status 0x22) and becomes the sector buffer that CdGetSector reads
/// (from the header with Setmode bit 5, 0x924 bytes; else the 0x800 data
/// bytes). Unknown commands answer INT5 (error, 0x40). CdSearchFile,
/// CdRead, CdReadSync, CdControlB and every other call go to `inner` (for
/// example [`DiscDevice`](crate::disc::DiscDevice) on the same image). The
/// lid: see the module documentation.
pub struct CdDrive<D> {
    pub disc: D,
    pub audio: Option<Box<dyn CdAudio>>,
    pub inner: Option<Box<dyn Device>>,
    /// Where the guest's VBlank count is (the game version's words).
    /// Configuration, not drive state: save states leave it out.
    pub clock: VBlankClock,
    /// VBlank interrupts per second of the guest's VBlank counter.
    pub vblank_rate: u32,
    /// VBlanks from a read command to its first sector.
    pub seek_vblanks: u32,
    status: u8,
    mode: u8,
    filter: (u8, u8),
    target: u32,
    seek_pending: bool,
    position: u32,
    reading: bool,
    /// VBlank count when reading (re)started and sectors read since then.
    read_start: u32,
    read_count: u64,
    responses: VecDeque<Response>,
    /// Second responses, due after the acknowledge was polled.
    completions: VecDeque<Response>,
    buffer: Vec<u8>,
    cursor: usize,
    lid: Lid,
    /// Getstat commands since the last Stop with nothing else in between
    /// (`None`: another command came after it), for [`Self::awaits_lid`].
    stopped_polls: Option<u32>,
}

/// The drive's state for save states: everything but the disc, the audio
/// sink and `inner`, which stay attached to the drive they belong to, and
/// the clock words (configuration of the game version); plus which disc of
/// the drive's [`Disc`] is inserted.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CdDriveState {
    vblank_rate: u32,
    seek_vblanks: u32,
    status: u8,
    mode: u8,
    filter: (u8, u8),
    target: u32,
    seek_pending: bool,
    position: u32,
    reading: bool,
    read_start: u32,
    read_count: u64,
    responses: VecDeque<Response>,
    completions: VecDeque<Response>,
    buffer: Vec<u8>,
    cursor: usize,
    lid: Lid,
    stopped_polls: Option<u32>,
    /// [`Disc::inserted`]: the disc in the drive (0: disc 1).
    inserted: u32,
}

impl CdDriveState {
    /// The index of the disc in the drive (0: disc 1).
    pub fn inserted(&self) -> usize {
        self.inserted as usize
    }
}

impl<D: Disc> CdDrive<D> {
    /// The drive's state (see [`CdDriveState`]).
    pub fn snapshot(&self) -> CdDriveState {
        // Every field named: a new one must be placed here or excluded.
        let Self {
            disc,
            audio: _,
            inner: _,
            clock: _,
            vblank_rate,
            seek_vblanks,
            status,
            mode,
            filter,
            target,
            seek_pending,
            position,
            reading,
            read_start,
            read_count,
            responses,
            completions,
            buffer,
            cursor,
            lid,
            stopped_polls,
        } = self;
        CdDriveState {
            vblank_rate: *vblank_rate,
            seek_vblanks: *seek_vblanks,
            status: *status,
            mode: *mode,
            filter: *filter,
            target: *target,
            seek_pending: *seek_pending,
            position: *position,
            reading: *reading,
            read_start: *read_start,
            read_count: *read_count,
            responses: responses.clone(),
            completions: completions.clone(),
            buffer: buffer.clone(),
            cursor: *cursor,
            lid: *lid,
            stopped_polls: *stopped_polls,
            inserted: disc.inserted() as u32,
        }
    }
    /// Whether [`Self::restore`] can take `state`: its inserted disc is one
    /// of this drive's discs.
    pub fn fits(&self, state: &CdDriveState) -> Result<(), String> {
        let discs = self.disc.discs();
        if state.inserted() < discs {
            Ok(())
        } else {
            Err(format!(
                "the state has disc {} in the drive, this player has {discs} disc(s)",
                state.inserted() + 1
            ))
        }
    }
    /// Replace the drive's state, keeping its discs, audio sink, `inner`
    /// and clock words; the state's disc goes in (see [`Self::fits`]).
    pub fn restore(&mut self, state: CdDriveState) {
        let CdDriveState {
            vblank_rate,
            seek_vblanks,
            status,
            mode,
            filter,
            target,
            seek_pending,
            position,
            reading,
            read_start,
            read_count,
            responses,
            completions,
            buffer,
            cursor,
            lid,
            stopped_polls,
            inserted,
        } = state;
        self.vblank_rate = vblank_rate;
        self.seek_vblanks = seek_vblanks;
        self.status = status;
        self.mode = mode;
        self.filter = filter;
        self.target = target;
        self.seek_pending = seek_pending;
        self.position = position;
        self.reading = reading;
        self.read_start = read_start;
        self.read_count = read_count;
        self.responses = responses;
        self.completions = completions;
        self.buffer = buffer;
        self.cursor = cursor;
        self.lid = lid;
        self.stopped_polls = stopped_polls;
        self.disc.insert(inserted as usize);
    }

    /// A drive with `disc`, pacing sectors by the VBlank count at `clock`
    /// (50 VBlanks per second until [`Self::vblank_rate`] says otherwise).
    pub fn new(disc: D, clock: VBlankClock) -> Self {
        Self {
            disc,
            audio: None,
            inner: None,
            clock,
            vblank_rate: 50,
            seek_vblanks: 2,
            status: STAT_MOTOR,
            mode: 0,
            filter: (0, 0),
            target: 0,
            seek_pending: false,
            position: 0,
            reading: false,
            read_start: 0,
            read_count: 0,
            responses: VecDeque::new(),
            completions: VecDeque::new(),
            buffer: Vec::new(),
            cursor: 0,
            lid: Lid::Closed,
            stopped_polls: None,
        }
    }

    /// The lid.
    pub fn lid(&self) -> Lid {
        self.lid
    }

    /// Open the lid: reading stops (an INT5 "door opened" when it was
    /// reading), the motor stops, and the status reports the open shell.
    /// It stays open until [`Self::close_lid`].
    pub fn open_lid(&mut self) {
        if self.reading {
            let status = (self.status | STAT_SHELL_OPEN | STAT_ERROR) & !STAT_READING;
            self.responses
                .push_back(Response::new(5, &[status, ERROR_DOOR_OPENED]));
        }
        self.reading = false;
        self.status = (self.status | STAT_SHELL_OPEN) & !(STAT_MOTOR | STAT_READING);
        self.lid = Lid::Open { close_at: None };
        self.stopped_polls = None;
    }

    /// Close the lid (`ram`: the guest RAM the clock reads): the disc spins
    /// up and the motor is on [`SPIN_UP_VBLANKS`] later. No change when it
    /// is closed.
    pub fn close_lid(&mut self, ram: &[u8]) {
        if let Lid::Open { .. } = self.lid {
            let now = self.clock.read(ram);
            self.lid = Lid::SpinningUp {
                ready: now.wrapping_add(SPIN_UP_VBLANKS),
            };
        }
    }

    /// Change discs: open the lid now, put disc `index` of the drive's
    /// [`Disc`] in, and close the lid again [`LID_OPEN_VBLANKS`] later (by
    /// the clock in `ram`). `false` (no change) when there is no such disc.
    pub fn change_disc(&mut self, ram: &[u8], index: usize) -> bool {
        if index >= self.disc.discs() {
            return false;
        }
        let now = self.clock.read(ram);
        self.open_lid();
        self.disc.insert(index);
        self.lid = Lid::Open {
            close_at: Some(now.wrapping_add(LID_OPEN_VBLANKS)),
        };
        true
    }

    /// Whether the game waits for the lid to open: [`LID_WAIT_POLLS`]
    /// Getstats since a Stop with nothing else in between, the lid closed
    /// and the motor off (see the module documentation).
    pub fn awaits_lid(&self) -> bool {
        self.lid == Lid::Closed
            && self.status & STAT_MOTOR == 0
            && self
                .stopped_polls
                .is_some_and(|polls| polls >= LID_WAIT_POLLS)
    }

    /// The lid's timed steps due by `now`: a disc change's close, then the
    /// end of the spin-up.
    fn update_lid(&mut self, now: u32) {
        let due = |at: u32| (now.wrapping_sub(at) as i32) >= 0;
        if let Lid::Open {
            close_at: Some(close),
        } = self.lid
            && due(close)
        {
            self.lid = Lid::SpinningUp {
                ready: close.wrapping_add(SPIN_UP_VBLANKS),
            };
        }
        if let Lid::SpinningUp { ready } = self.lid
            && due(ready)
        {
            self.lid = Lid::Closed;
            self.status |= STAT_MOTOR;
        }
    }

    /// Commands that read the disc or its table of contents.
    fn needs_disc(command: u8) -> bool {
        matches!(
            command,
            0x03 | 0x06 | 0x13 | 0x14 | 0x15 | 0x16 | 0x1a | 0x1b | 0x1e
        )
    }

    fn acknowledge(&mut self, data: &[u8]) {
        let mut bytes = vec![self.status];
        bytes.extend_from_slice(data);
        self.responses.push_back(Response::new(3, &bytes));
    }

    fn start_reading(&mut self, ram: &[u8]) {
        if self.seek_pending {
            self.position = self.target;
            self.seek_pending = false;
        }
        self.reading = true;
        self.read_start = self.clock.read(ram).wrapping_add(self.seek_vblanks);
        self.read_count = 0;
    }

    fn command(&mut self, ram: &[u8], command: u8, count: u32, parameters: u32) {
        let parameter = |i: u32| (parameters >> (8 * i)) as u8;
        let decode = |byte: u8| u32::from(byte >> 4) * 10 + u32::from(byte & 0xf);
        if self.lid != Lid::Closed {
            self.update_lid(self.clock.read(ram));
        }
        // A game waiting for the lid: Getstats after a Stop, nothing else.
        self.stopped_polls = match command {
            0x08 => Some(0),
            0x01 => self.stopped_polls.map(|polls| polls.saturating_add(1)),
            _ => None,
        };
        if self.lid != Lid::Closed && Self::needs_disc(command) {
            let error = match self.lid {
                Lid::Open { .. } => ERROR_DOOR_OPENED,
                _ => ERROR_NOT_READY,
            };
            self.responses
                .push_back(Response::new(5, &[self.status | STAT_ERROR, error]));
            return;
        }
        match command {
            0x01 => {
                self.acknowledge(&[]);
                // The shell bit stays until a Getstat sees the lid closed.
                if !matches!(self.lid, Lid::Open { .. }) {
                    self.status &= !STAT_SHELL_OPEN;
                }
            }
            0x02 if count >= 3 => {
                let [minute, second, sector] = [0, 1, 2].map(|i| decode(parameter(i)));
                self.target = ((minute * 60 + second) * 75 + sector).wrapping_sub(150);
                self.seek_pending = true;
                self.acknowledge(&[]);
            }
            0x03 => {
                // Play (CD-DA): no audio tracks are modelled.
                self.acknowledge(&[]);
            }
            0x06 | 0x1b => {
                self.acknowledge(&[]);
                self.start_reading(ram);
                self.status |= STAT_READING;
            }
            0x07..=0x0a => {
                self.acknowledge(&[]);
                self.reading = false;
                self.status &= !STAT_READING;
                if command == 0x08 || matches!(self.lid, Lid::Open { .. }) {
                    self.status &= !STAT_MOTOR;
                } else {
                    self.status |= STAT_MOTOR;
                }
                if command == 0x0a {
                    self.mode = 0;
                }
                self.completions.push_back(Response::new(2, &[self.status]));
            }
            0x0b | 0x0c => {
                self.acknowledge(&[]);
                if let Some(audio) = &mut self.audio {
                    audio.mute(command == 0x0b);
                }
            }
            0x0d if count >= 2 => {
                self.filter = (parameter(0), parameter(1));
                self.acknowledge(&[]);
            }
            0x0e if count >= 1 => {
                self.mode = parameter(0);
                self.acknowledge(&[]);
            }
            0x0f => {
                let mode = self.mode;
                self.acknowledge(&[0, mode, 0, self.filter.0, self.filter.1]);
            }
            0x12 => {
                self.acknowledge(&[]);
                self.completions.push_back(Response::new(2, &[self.status]));
            }
            0x13 => self.acknowledge(&[1, 1]),
            0x14 => self.acknowledge(&[0, 2]),
            0x15 | 0x16 => {
                self.acknowledge(&[]);
                self.reading = false;
                self.status &= !STAT_READING;
                self.position = self.target;
                self.seek_pending = false;
                self.completions.push_back(Response::new(2, &[self.status]));
            }
            0x1a => {
                self.acknowledge(&[]);
                self.completions.push_back(Response::new(
                    2,
                    &[self.status, 0x00, 0x20, 0x00, b'S', b'C', b'E', b'E'],
                ));
            }
            _ => self
                .responses
                .push_back(Response::new(5, &[self.status | STAT_ERROR, 0x40])),
        }
    }

    /// Sectors due by `now` (VBlank count) since reading started.
    fn sectors_due(&self, now: u32) -> u64 {
        let elapsed = now.wrapping_sub(self.read_start);
        if (elapsed as i32) < 0 {
            return 0;
        }
        let speed = if self.mode & MODE_SPEED != 0 { 150 } else { 75 };
        u64::from(elapsed) * speed / u64::from(self.vblank_rate.max(1)) + 1
    }

    /// The next interrupt: a pending response, then a completion, then the
    /// next due sector that is not played as XA audio.
    fn next_interrupt(&mut self, ram: &[u8]) -> Option<Response> {
        if let Some(response) = self.responses.pop_front() {
            return Some(response);
        }
        if let Some(response) = self.completions.pop_front() {
            return Some(response);
        }
        if !self.reading {
            return None;
        }
        let now = self.clock.read(ram);
        while self.read_count < self.sectors_due(now) {
            let lba = self.position;
            self.position = self.position.wrapping_add(1);
            self.read_count += 1;
            let Some(sector) = self.disc.sector(lba) else {
                self.reading = false;
                self.status &= !STAT_READING;
                return Some(Response::new(5, &[self.status | STAT_ERROR, 0x04]));
            };
            let submode = sector[18];
            if self.mode & MODE_XA_ADPCM != 0 && submode & 0x64 == 0x64 {
                let wanted =
                    self.mode & MODE_FILTER == 0 || (sector[16], sector[17]) == self.filter;
                if wanted && let Some(audio) = &mut self.audio {
                    audio.xa_sector(&sector);
                }
                continue;
            }
            let (offset, size) = if self.mode & MODE_IGNORE != 0 {
                (24, 2328)
            } else if self.mode & MODE_WHOLE_SECTOR != 0 {
                (12, 2340)
            } else {
                (24, 2048)
            };
            self.buffer = sector[offset..offset + size].to_vec();
            self.cursor = 0;
            return Some(Response::new(1, &[self.status]));
        }
        None
    }

    fn transfer(&mut self, ram: &mut [u8], destination: u32, words: u32) {
        let bytes = (words as usize).saturating_mul(4).min(0x1_0000);
        for i in 0..bytes {
            let byte = self.buffer.get(self.cursor).copied().unwrap_or(0);
            self.cursor += 1;
            ram[(destination.wrapping_add(i as u32) & 0x1f_ffff) as usize] = byte;
        }
    }
}

fn read_word(ram: &[u8], address: u32) -> u32 {
    let at = (address & 0x1f_fffc) as usize;
    u32::from_le_bytes([ram[at], ram[at + 1], ram[at + 2], ram[at + 3]])
}

impl<D: Disc> Device for CdDrive<D> {
    fn call(&mut self, ram: &mut [u8], scratch: &mut [u8], call: HostCall) -> Option<u32> {
        let [a0, a1, a2, _] = call.args;
        match call.function {
            host_ids::CD_COMMAND => {
                self.command(ram, a0 as u8, a1, a2);
                Some(1)
            }
            host_ids::CD_INTERRUPT => {
                if let Some(response) = self.next_interrupt(ram) {
                    let frame = a1;
                    let mut write = |offset: u32, byte: u8| {
                        ram[(frame.wrapping_add(offset) & 0x1f_ffff) as usize] = byte;
                    };
                    write(0x10, response.kind);
                    write(0x11, response.count);
                    for (i, &byte) in response.bytes.iter().enumerate() {
                        write(0x18 + i as u32, byte);
                    }
                }
                Some(0)
            }
            host_ids::CD_GET_SECTOR | host_ids::CD_GET_SECTOR2 => {
                self.transfer(ram, a0, a1);
                Some(0)
            }
            host_ids::CD_MIX => {
                let attenuation =
                    std::array::from_fn(|i| ram[(a0.wrapping_add(i as u32) & 0x1f_ffff) as usize]);
                if let Some(audio) = &mut self.audio {
                    audio.mix(attenuation);
                }
                // CdMix returns 1 whatever the device says; let `inner`
                // (or the host results) see the call too.
                self.inner.as_mut()?.call(ram, scratch, call)
            }
            _ => self.inner.as_mut()?.call(ram, scratch, call),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Disc `n`: every sector's first data byte is `n`.
    struct Tagged(u8);
    impl Disc for Tagged {
        fn sector(&mut self, lba: u32) -> Option<[u8; SECTOR_BYTES]> {
            let mut sector = [0; SECTOR_BYTES];
            sector[18] = 0x08;
            sector[24] = self.0;
            sector[25..29].copy_from_slice(&lba.to_le_bytes());
            Some(sector)
        }
    }

    const CLOCK: VBlankClock = VBlankClock {
        counter: 0x8000_0100,
        vsync_record: 0x8000_0104,
    };

    fn ram(vblank: u32) -> Vec<u8> {
        let mut ram = vec![0; 0x20_0000];
        ram[0x100..0x104].copy_from_slice(&vblank.to_le_bytes());
        ram
    }

    /// Every response the drive has for the guest now.
    fn responses<D: Disc>(drive: &mut CdDrive<D>, ram: &[u8]) -> Vec<(u8, Vec<u8>)> {
        std::iter::from_fn(|| drive.next_interrupt(ram))
            .map(|r| (r.kind, r.bytes[..usize::from(r.count)].to_vec()))
            .collect()
    }

    fn two_discs() -> CdDrive<DiscSet<Tagged>> {
        CdDrive::new(DiscSet::new(vec![Tagged(1), Tagged(2)], 0).unwrap(), CLOCK)
    }

    #[test]
    fn a_disc_change_opens_swaps_closes_and_spins_up() {
        let mut drive = two_discs();
        let at = |n| ram(n);
        // The game's wait for the lid: Setmode 0, Stop, then Getstats.
        drive.command(&at(10), 0x0e, 1, 0);
        drive.command(&at(10), 0x08, 0, 0);
        assert_eq!(
            responses(&mut drive, &at(10)),
            [(3, vec![0x02]), (3, vec![0x02]), (2, vec![0x00])]
        );
        for poll in 1..=LID_WAIT_POLLS {
            assert!(!drive.awaits_lid());
            drive.command(&at(10 + poll), 0x01, 0, 0);
        }
        assert!(drive.awaits_lid());
        responses(&mut drive, &at(20));

        assert!(!drive.change_disc(&at(100), 2), "no third disc");
        assert!(drive.change_disc(&at(100), 1));
        assert_eq!(drive.disc.inserted(), 1);
        assert_eq!(
            drive.lid(),
            Lid::Open {
                close_at: Some(100 + LID_OPEN_VBLANKS)
            }
        );
        assert!(!drive.awaits_lid());
        // Open: the shell bit, no motor; reads fail with "door opened".
        drive.command(&at(110), 0x01, 0, 0);
        drive.command(&at(110), 0x06, 0, 0);
        assert_eq!(
            responses(&mut drive, &at(110)),
            [(3, vec![0x10]), (5, vec![0x11, 0x08])]
        );
        // Closed: the first Getstat still reports the shell, then it
        // clears; the disc is not ready until it spun up.
        let closed = 100 + LID_OPEN_VBLANKS;
        drive.command(&at(closed), 0x01, 0, 0);
        drive.command(&at(closed + 1), 0x01, 0, 0);
        drive.command(&at(closed + 1), 0x13, 0, 0);
        assert_eq!(
            responses(&mut drive, &at(closed + 1)),
            [(3, vec![0x10]), (3, vec![0x00]), (5, vec![0x01, 0x80])]
        );
        let ready = closed + SPIN_UP_VBLANKS;
        drive.command(&at(ready - 1), 0x01, 0, 0);
        drive.command(&at(ready), 0x01, 0, 0);
        assert_eq!(
            responses(&mut drive, &at(ready)),
            [(3, vec![0x00]), (3, vec![0x02])]
        );
        assert_eq!(drive.lid(), Lid::Closed);
        assert!(!drive.awaits_lid());
        // The new disc reads: Setloc 00:02:16 (LBA 16), ReadN.
        drive.command(&at(ready), 0x02, 3, 0x0016_0200);
        drive.command(&at(ready), 0x06, 0, 0);
        let later = at(ready + 10);
        let kinds: Vec<u8> = (0..3)
            .map(|_| drive.next_interrupt(&later).unwrap().kind)
            .collect();
        assert_eq!(kinds, [3, 3, 1]);
        assert_eq!(drive.buffer[..5], [2, 16, 0, 0, 0]);
    }

    #[test]
    fn opening_the_lid_while_reading_reports_the_door() {
        let mut drive = two_discs();
        drive.command(&ram(0), 0x06, 0, 0);
        responses(&mut drive, &ram(0));
        drive.open_lid();
        assert_eq!(
            responses(&mut drive, &ram(5)),
            [(5, vec![0x13, 0x08])],
            "shell open, error, no reading bit"
        );
        drive.close_lid(&ram(9));
        assert_eq!(
            drive.lid(),
            Lid::SpinningUp {
                ready: 9 + SPIN_UP_VBLANKS
            }
        );
        // Other commands keep the wait from counting.
        drive.command(&ram(100), 0x08, 0, 0);
        drive.command(&ram(100), 0x0e, 1, 0);
        for _ in 0..10 {
            drive.command(&ram(100), 0x01, 0, 0);
        }
        assert!(!drive.awaits_lid());
    }

    #[test]
    fn save_states_keep_the_lid_and_the_disc() {
        let mut drive = two_discs();
        drive.change_disc(&ram(40), 1);
        let state = drive.snapshot();
        assert_eq!(state.inserted(), 1);
        let mut other = two_discs();
        other.fits(&state).unwrap();
        other.restore(state.clone());
        assert_eq!(other.disc.inserted(), 1);
        assert_eq!(other.snapshot(), state);
        let single = CdDrive::new(Tagged(1), CLOCK);
        assert!(single.fits(&state).unwrap_err().contains("disc 2"));
        assert_eq!(single.snapshot().inserted(), 0);
    }
}
