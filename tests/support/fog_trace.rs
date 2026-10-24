//! GPU command traces of Japanese flight passes, and their offline
//! re-rendering (`docs/EFFECTS_ATMOSPHERE.md`).
//!
//! A [`Tracer`] sits between the machine and its host as the machine's
//! device. For a traced pass it records, before the host executes them:
//! every GP0 DMA list (each packet with its RAM address, the ordering-table
//! slot it is linked under and the sub-pixel position of each word), every
//! LoadImage and every GP1 write; plus the software GPU as the pass found
//! it and machine RAM at the world list's DrawOTag. That is everything the
//! software GPU saw, so [`Replay`] re-renders the pass exactly, or with
//! edited commands (the CLUT redirect), and stops at any packet (the end of
//! the world, for a post pass).
//!
//! Used by `tests/effects_atmosphere.rs` (compiled into it with `#[path]`),
//! run by `tools/effects/atmosphere-mockups.sh`. It never changes the game:
//! the host gets every call unchanged.
#![allow(dead_code)]

use ac3_graphics::{Gpu, Vram};
use ac3_machine::precision::{PrecisionTable, SharedPrecision};
use ac3_machine::{Device, HostCall, Machine, host_ids};
use ac3_port::assets::GameData;
use ac3_port::machine_card::{CardDevice, CardSource};
use ac3_port::machine_player::{Host, PadType, Player, SharedHost, disc_images, game_for};
use std::cell::RefCell;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// JP gp and the world (gameplay) block pointer `[gp+0x2E0]`.
pub const GP: u32 = 0x800b_cb74;
pub const WORLD: u32 = GP + 0x2e0;
/// The state context `[gp+0x2EC]` (+0x2C: the state; 2 is flight).
pub const CONTEXT: u32 = GP + 0x2ec;
/// Ordering-table entries per buffer.
pub const OT_ENTRIES: u32 = 4096;

/// The first disc of the Japanese release (`$AC3_JP_DISC`, else under
/// `~/ac3-jp/discs`).
pub fn jp_disc() -> PathBuf {
    if let Some(path) = std::env::var_os("AC3_JP_DISC") {
        return path.into();
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join("ac3-jp/discs/Ace Combat 3 - Electrosphere (Japan, Asia) (Disc 1) (Rev 1).cue")
}

/// The mission 1 flight save state of the JP runs.
pub fn jp_flight_state() -> PathBuf {
    if let Some(path) = std::env::var_os("AC3_JP_STATE") {
        return path.into();
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join("ac3-jp/runs/present-work/at50000.ac3state")
}

fn word(ram: &[u8], address: u32) -> u32 {
    let at = (address & 0x1f_fffc) as usize;
    u32::from_le_bytes(ram[at..at + 4].try_into().expect("four bytes"))
}
fn half(ram: &[u8], address: u32) -> u16 {
    let at = (address & 0x1f_fffe) as usize;
    u16::from_le_bytes([ram[at], ram[at + 1]])
}

/// RAM reads of a snapshot (KSEG0 addresses).
#[derive(Clone)]
pub struct Ram(pub Vec<u8>);
impl Ram {
    pub fn w(&self, address: u32) -> u32 {
        word(&self.0, address)
    }
    pub fn h(&self, address: u32) -> u16 {
        half(&self.0, address)
    }
    pub fn sh(&self, address: u32) -> i16 {
        half(&self.0, address) as i16
    }
    pub fn b(&self, address: u32) -> u8 {
        self.0[(address & 0x1f_ffff) as usize]
    }
}

/// One non-empty DMA packet.
#[derive(Clone, Debug)]
pub struct Packet {
    /// KSEG0 address of its header; GP0 word `i` is at `address + 4 (i + 1)`.
    pub address: u32,
    pub words: Vec<u32>,
    /// The world ordering-table slot it is linked under (`None` outside the
    /// world list).
    pub slot: Option<u16>,
    /// The sub-pixel position (x, y before the drawing offset, z the GTE's
    /// unrounded SZ) recorded for each word, when there is one.
    pub precise: Vec<Option<[f32; 3]>>,
}

#[derive(Clone, Debug)]
pub enum Event {
    /// A GP0 DMA list from `start`; `world`: the world ordering table.
    List {
        start: u32,
        world: bool,
        packets: Vec<Packet>,
    },
    /// A LoadImage: GP0 A0, the rectangle and the pixel words.
    Upload(Vec<u32>),
    Gp1(u32),
}

/// One traced main-loop pass.
#[derive(Clone)]
pub struct PassTrace {
    /// The VBlank count before the pass.
    pub vblank: u64,
    /// The software GPU as the pass found it.
    pub gpu: Gpu,
    pub events: Vec<Event>,
    /// Machine RAM at the world list's DrawOTag (empty without one).
    pub ram: Ram,
    /// The world ordering table (its first entry).
    pub ot: u32,
}

impl PassTrace {
    /// The world list's packets.
    pub fn world_packets(&self) -> impl Iterator<Item = &Packet> {
        self.events.iter().flat_map(|event| match event {
            Event::List {
                world: true,
                packets,
                ..
            } => packets.as_slice(),
            _ => &[],
        })
    }
}

#[derive(Default)]
struct Recording {
    events: Vec<Event>,
    ram: Option<Vec<u8>>,
    ot: u32,
}

/// The machine's device while tracing: records, then hands every call to
/// the host unchanged.
pub struct Tracer {
    host: SharedHost,
    precision: Option<SharedPrecision>,
    recording: Rc<RefCell<Option<Recording>>>,
}

impl Tracer {
    fn list(&self, ram: &[u8], start: u32) -> (bool, Vec<Packet>, u32) {
        let world = word(ram, WORLD);
        let tables = [word(ram, world + 0x18), word(ram, world + 0x1c)];
        let slot_of = |address: u32| -> Option<(u32, u16)> {
            tables.iter().find_map(|&table| {
                let offset = (address & 0x1f_ffff).wrapping_sub(table & 0x1f_ffff);
                (offset < OT_ENTRIES * 4 && offset.is_multiple_of(4))
                    .then_some((table, (offset / 4) as u16))
            })
        };
        let precision = self.precision.as_ref().map(|table| table.borrow());
        let mut packets = Vec::new();
        let mut address = start & 0x00ff_ffff;
        // The slot of the world table the walk last passed: the root list
        // of the frame links the world table in.
        let mut slot = None;
        let mut table = 0;
        for _ in 0..(1 << 20) {
            let header = word(ram, address);
            let count = header >> 24;
            let kseg0 = 0x8000_0000 | (address & 0x1f_ffff);
            if let Some((base, entry)) = slot_of(address) {
                slot = Some(entry);
                table = 0x8000_0000 | (base & 0x1f_ffff);
            }
            if count > 0 {
                let words: Vec<u32> = (1..=count).map(|i| word(ram, address + 4 * i)).collect();
                let precise = words
                    .iter()
                    .enumerate()
                    .map(|(i, &w)| {
                        precision
                            .as_ref()
                            .and_then(|table| table.position(kseg0 + 4 * (i as u32 + 1), w))
                    })
                    .collect();
                packets.push(Packet {
                    address: kseg0,
                    words,
                    slot,
                    precise,
                });
            }
            let next = header & 0x00ff_ffff;
            if next == 0x00ff_ffff {
                break;
            }
            address = next;
        }
        (table != 0, packets, table)
    }
}

impl Device for Tracer {
    fn call(&mut self, ram: &mut [u8], scratch: &mut [u8], call: HostCall) -> Option<u32> {
        let recording_active = self.recording.borrow().is_some();
        if recording_active {
            let [a0, a1, ..] = call.args;
            let event = match call.function {
                host_ids::DMA_LIST => {
                    let (world, packets, table) = self.list(ram, a0);
                    if world {
                        let mut recording = self.recording.borrow_mut();
                        let recording = recording.as_mut().expect("recording");
                        recording.ram = Some(ram.to_vec());
                        recording.ot = table;
                    }
                    Some(Event::List {
                        start: a0,
                        world,
                        packets,
                    })
                }
                host_ids::LOAD_IMAGE => {
                    let x = half(ram, a0);
                    let y = half(ram, a0 + 2);
                    let w = half(ram, a0 + 4);
                    let h = half(ram, a0 + 6);
                    let count = usize::from(w) * usize::from(h);
                    let mut words = vec![
                        0xa000_0000,
                        (u32::from(y) << 16) | u32::from(x),
                        (u32::from(h) << 16) | u32::from(w),
                    ];
                    words.extend((0..count.div_ceil(2)).map(|i| word(ram, a1 + 4 * i as u32)));
                    if count % 2 == 1 {
                        let last = words.len() - 1;
                        words[last] &= 0xffff;
                    }
                    Some(Event::Upload(words))
                }
                host_ids::GP1 => Some(Event::Gp1(a0)),
                _ => None,
            };
            if let Some(event) = event {
                let mut recording = self.recording.borrow_mut();
                recording.as_mut().expect("recording").events.push(event);
            }
        }
        self.host.call(ram, scratch, call)
    }
}

/// A JP player at the flight save state, with the tracer as its device.
pub struct TracedPlayer {
    pub player: Player,
    pub precision: SharedPrecision,
    recording: Rc<RefCell<Option<Recording>>>,
}

impl TracedPlayer {
    /// Boot the JP disc as `ac3 machine` does (blank cards, a digital pad),
    /// then load `state`. Sub-pixel positions are recorded as with the GPU
    /// renderer above 1x.
    pub fn new(disc: &Path, state: &Path) -> Self {
        let data = GameData::load_jp_disc(disc).expect("the JP game files");
        let version = game_for(&data, disc).expect("the JP release");
        let (images, booted) = disc_images(version, disc, None).expect("the JP discs");
        let discs = images
            .iter()
            .map(|path| {
                let image = ac3_port::version::image_path(path).unwrap_or_else(|_| path.clone());
                File::open(&image).expect("a disc image")
            })
            .collect();
        let mut host = Host::with_discs(version, Vram::new(), discs, booted);
        host.card = CardDevice::from_sources(&[CardSource::Blank, CardSource::Blank])
            .expect("blank cards")
            .with_clock(version.layout().vblank_counter);
        host.pad = PadType::Digital.ports();
        let mut player = Player::new(Machine::default(), host);
        let precision = PrecisionTable::shared();
        player.machine.set_precision(Some(precision.clone()));
        let payload = &data.executable.payload;
        version.boot(&mut player.machine, payload);
        player.machine.take_fault().expect("boot");
        let game = ac3_port::savestate::GameId::of(payload);
        ac3_port::savestate::load(&mut player, game, state).expect("the save state");
        precision.borrow_mut().clear();
        let recording = Rc::new(RefCell::new(None));
        player.machine.device = Some(Box::new(Tracer {
            host: SharedHost(player.host.clone()),
            precision: Some(precision.clone()),
            recording: recording.clone(),
        }));
        Self {
            player,
            precision,
            recording,
        }
    }

    pub fn vblanks(&self) -> u64 {
        self.player.vblanks()
    }

    /// One main-loop pass, untraced.
    pub fn step(&mut self) {
        if let Err(fault) = self.player.iteration() {
            panic!("VBlank {}: {fault}", self.player.vblanks());
        }
        self.player.host.borrow_mut().take_audio();
        if let Some(error) = self.player.host.borrow().error() {
            panic!("VBlank {}: device: {error}", self.player.vblanks());
        }
    }

    /// Run passes until the VBlank count reaches `vblank`.
    pub fn run_to(&mut self, vblank: u64) {
        while self.vblanks() < vblank {
            self.step();
        }
    }

    /// One main-loop pass, traced.
    pub fn trace(&mut self) -> PassTrace {
        let vblank = self.vblanks();
        let gpu = self.player.host.borrow().gpu.gpu.clone();
        *self.recording.borrow_mut() = Some(Recording::default());
        self.step();
        let recording = self.recording.borrow_mut().take().expect("recording");
        PassTrace {
            vblank,
            gpu,
            events: recording.events,
            ram: Ram(recording.ram.unwrap_or_default()),
            ot: recording.ot,
        }
    }

    /// Trace passes until one has a world list (flight).
    pub fn trace_world(&mut self) -> PassTrace {
        for _ in 0..16 {
            let trace = self.trace();
            if !trace.ram.0.is_empty() {
                return trace;
            }
        }
        panic!("no world list in 16 passes at VBlank {}", self.vblanks());
    }
}

// ---- GP0 commands ------------------------------------------------------------------

/// The GP0 commands of a packet: (first word index, word count). A packet
/// holds whole commands (the game never splits one across packets).
pub fn commands(words: &[u32]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let op = (words[i] >> 24) as u8;
        let len = match op {
            0x02 => 3,
            0x20..=0x3f => {
                let vertices = if op & 8 != 0 { 4 } else { 3 };
                let textured = usize::from(op & 4 != 0);
                let gouraud = op & 0x10 != 0;
                1 + vertices * (1 + textured) + if gouraud { vertices - 1 } else { 0 }
            }
            0x40..=0x5f if op & 8 == 0 => {
                if op & 0x10 != 0 {
                    4
                } else {
                    3
                }
            }
            0x40..=0x5f => {
                let mut n = 1 + if op & 0x10 != 0 { 3 } else { 2 };
                while i + n < words.len() && words[i + n] & 0xf000_f000 != 0x5000_5000 {
                    n += 1;
                }
                n + 1
            }
            0x60..=0x7f => {
                let textured = usize::from(op & 4 != 0);
                2 + textured + usize::from(op & 0x18 == 0)
            }
            0x80..=0x9f => 4,
            0xa0..=0xdf => 3,
            _ => 1,
        };
        let len = len.min(words.len() - i);
        out.push((i, len));
        i += len;
    }
    out
}

/// The word index (within a command) of the CLUT half-word of a textured
/// polygon or rectangle, if it has one.
pub fn clut_word_index(command: u32) -> Option<usize> {
    let op = (command >> 24) as u8;
    match op {
        0x20..=0x3f if op & 4 != 0 => Some(2),
        0x60..=0x7f if op & 4 != 0 => Some(2),
        _ => None,
    }
}

/// The texture page word of a textured polygon (the second UV word), if any.
pub fn tpage_word_index(command: u32) -> Option<usize> {
    let op = (command >> 24) as u8;
    match op {
        0x20..=0x3f if op & 4 != 0 => Some(if op & 0x10 != 0 { 5 } else { 4 }),
        _ => None,
    }
}

// ---- replay --------------------------------------------------------------------------

/// What a replay does with the next packet.
pub enum Step {
    /// Execute it as traced.
    Run,
    /// Execute these words instead.
    Edit(Vec<u32>),
    /// Stop before it ([`Replay::run`] returns; the next run starts with it).
    Stop,
}

/// The replay's state before a packet.
pub struct State<'g> {
    /// The software GPU (its VRAM is what the packet will read).
    pub gpu: &'g Gpu,
    /// The drawing offset (E5) and draw area (E3/E4, inclusive) in effect.
    pub offset: [i32; 2],
    pub area: [i32; 4],
}

/// The software GPU re-rendering a [`PassTrace`].
pub struct Replay<'a> {
    pub trace: &'a PassTrace,
    pub gpu: Gpu,
    event: usize,
    packet: usize,
    pub offset: [i32; 2],
    pub area: [i32; 4],
}

pub fn sign_extend_11(value: u32) -> i32 {
    ((value << 21) as i32) >> 21
}

impl<'a> Replay<'a> {
    pub fn new(trace: &'a PassTrace) -> Self {
        Self {
            trace,
            gpu: trace.gpu.clone(),
            event: 0,
            packet: 0,
            offset: [0; 2],
            area: [0; 4],
        }
    }

    fn note(&mut self, words: &[u32]) {
        for &(at, _) in &commands(words) {
            let command = words[at];
            match command >> 24 {
                0xe3 => {
                    self.area[0] = (command & 0x3ff) as i32;
                    self.area[1] = ((command >> 10) & 0x3ff) as i32;
                }
                0xe4 => {
                    self.area[2] = (command & 0x3ff) as i32;
                    self.area[3] = ((command >> 10) & 0x3ff) as i32;
                }
                0xe5 => {
                    self.offset = [sign_extend_11(command), sign_extend_11(command >> 11)];
                }
                _ => {}
            }
        }
    }

    /// Run the pass, asking `step` about each packet first. Returns true
    /// when `step` stopped it, false at the pass's end.
    pub fn run(&mut self, step: &mut dyn FnMut(&Packet, &State<'_>) -> Step) -> bool {
        while self.event < self.trace.events.len() {
            match &self.trace.events[self.event] {
                Event::List { packets, .. } => {
                    while self.packet < packets.len() {
                        let packet = &packets[self.packet];
                        let state = State {
                            gpu: &self.gpu,
                            offset: self.offset,
                            area: self.area,
                        };
                        let edited = match step(packet, &state) {
                            Step::Stop => return true,
                            Step::Run => None,
                            Step::Edit(words) => Some(words),
                        };
                        let words = edited.as_deref().unwrap_or(&packet.words);
                        self.note(words);
                        self.gpu.write_gp0_words(words).expect("GP0 replay");
                        self.packet += 1;
                    }
                }
                Event::Upload(words) => self.gpu.write_gp0_words(words).expect("upload replay"),
                Event::Gp1(word) => self.gpu.write_gp1(*word).expect("GP1 replay"),
            }
            self.event += 1;
            self.packet = 0;
        }
        false
    }

    /// Run to the end.
    pub fn finish(&mut self, step: &mut dyn FnMut(&Packet, &State<'_>) -> Step) {
        while self.run(step) {}
    }

    /// Replace the software GPU's VRAM words (a post pass's write-back).
    pub fn set_vram(&mut self, words: &[u16]) {
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        self.gpu
            .set_vram(Vram::from_le_bytes(&bytes).expect("a whole VRAM"));
    }
}

// ---- depth -------------------------------------------------------------------------

/// The vertex word indices of a polygon command (within the command).
pub fn polygon_vertices(command: u32) -> Vec<usize> {
    let op = command >> 24;
    let count = if op & 8 != 0 { 4 } else { 3 };
    let textured = op & 4 != 0;
    let gouraud = op & 0x10 != 0;
    let mut at = 1;
    let mut out = Vec::with_capacity(count);
    for index in 0..count {
        if gouraud && index > 0 {
            at += 1;
        }
        out.push(at);
        at += 1;
        if textured {
            at += 1;
        }
    }
    out
}

/// A per-pixel view depth of the world, rebuilt from the traced
/// polygons' sub-pixel depths: each opaque world-phase polygon writes its
/// perspective-interpolated z where it covers, in drawing order (the game
/// draws back to front), so the last write is the visible surface. A
/// polygon with a vertex lacking a depth writes its slot's depth (8 per
/// slot) instead. 0: nothing (the sky).
pub struct DepthBuffer {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub z: Vec<f32>,
    pub slot: Vec<u16>,
    /// Polygons drawn with sub-pixel depths, and with slot depths.
    pub precise: usize,
    pub estimated: usize,
}

impl DepthBuffer {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        let size = (w * h) as usize;
        Self {
            x,
            y,
            w,
            h,
            z: vec![0.0; size],
            slot: vec![0; size],
            precise: 0,
            estimated: 0,
        }
    }

    /// The z at VRAM pixel (`px`, `py`) (0: none or outside).
    pub fn at(&self, px: i32, py: i32) -> f32 {
        let (x, y) = (px - self.x, py - self.y);
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            return 0.0;
        }
        self.z[(y * self.w + x) as usize]
    }

    /// Add the polygons of `packet` (drawn with drawing offset `offset`).
    pub fn add(&mut self, packet: &Packet, offset: [i32; 2]) {
        let Some(slot) = packet.slot.filter(|slot| (2..=4094).contains(slot)) else {
            return;
        };
        for (at, len) in commands(&packet.words) {
            let op = packet.words[at] >> 24;
            if !(0x20..=0x3f).contains(&op) || op & 2 != 0 {
                continue;
            }
            let indices = polygon_vertices(packet.words[at]);
            if indices.iter().any(|&i| i >= len) {
                continue;
            }
            let fallback = f32::from(slot) * 8.0;
            let all = indices
                .iter()
                .all(|&i| packet.precise[at + i].is_some_and(|p| p[2] > 0.0));
            let points: Vec<[f32; 3]> = indices
                .iter()
                .map(|&i| {
                    let word = packet.words[at + i];
                    match packet.precise[at + i].filter(|_| all) {
                        Some([x, y, z]) => [x + offset[0] as f32, y + offset[1] as f32, z],
                        None => [
                            (sign_extend_11(word) + offset[0]) as f32,
                            (sign_extend_11(word >> 16) + offset[1]) as f32,
                            fallback,
                        ],
                    }
                })
                .collect();
            if all {
                self.precise += 1;
            } else {
                self.estimated += 1;
            }
            self.triangle(points[0], points[1], points[2], slot);
            if points.len() == 4 {
                self.triangle(points[1], points[3], points[2], slot);
            }
        }
    }

    fn triangle(&mut self, a: [f32; 3], b: [f32; 3], c: [f32; 3], slot: u16) {
        let area = (b[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (b[1] - a[1]);
        if area.abs() < 1e-6 {
            return;
        }
        let x0 = (a[0].min(b[0]).min(c[0]).floor() as i32).max(self.x);
        let x1 = (a[0].max(b[0]).max(c[0]).ceil() as i32).min(self.x + self.w - 1);
        let y0 = (a[1].min(b[1]).min(c[1]).floor() as i32).max(self.y);
        let y1 = (a[1].max(b[1]).max(c[1]).ceil() as i32).min(self.y + self.h - 1);
        for py in y0..=y1 {
            for px in x0..=x1 {
                let (x, y) = (px as f32, py as f32);
                let wa = ((b[0] - x) * (c[1] - y) - (c[0] - x) * (b[1] - y)) / area;
                let wb = ((c[0] - x) * (a[1] - y) - (a[0] - x) * (c[1] - y)) / area;
                let wc = 1.0 - wa - wb;
                if wa < -1e-4 || wb < -1e-4 || wc < -1e-4 {
                    continue;
                }
                let inverse = wa / a[2] + wb / b[2] + wc / c[2];
                if inverse <= 0.0 {
                    continue;
                }
                let index = ((py - self.y) * self.w + (px - self.x)) as usize;
                self.z[index] = 1.0 / inverse;
                self.slot[index] = slot;
            }
        }
    }
}

// ---- pictures ------------------------------------------------------------------------

/// RGB555 to 8-bit RGB as the presenter shows it (`c << 3 | c >> 2`).
pub fn rgb8(pixel: u16) -> [u8; 3] {
    let c = |shift: u16| {
        let v = ((pixel >> shift) & 0x1f) as u8;
        (v << 3) | (v >> 2)
    };
    [c(0), c(5), c(10)]
}

/// A rectangle of VRAM as 8-bit RGB.
pub fn crop(vram: &Vram, x: i32, y: i32, w: i32, h: i32) -> Vec<u8> {
    let words = vram.words();
    let mut out = Vec::with_capacity((w * h * 3) as usize);
    for row in y..y + h {
        for column in x..x + w {
            let index = ((row & 511) * 1024 + (column & 1023)) as usize;
            out.extend_from_slice(&rgb8(words[index]));
        }
    }
    out
}

pub fn save_png(path: &Path, rgb: &[u8], w: u32, h: u32) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("output folder");
    }
    let file = File::create(path).expect("png file");
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("png header");
    writer.write_image_data(rgb).expect("png data");
}

/// Scale an RGB picture up by an integer factor (nearest).
pub fn upscale(rgb: &[u8], w: u32, h: u32, factor: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgb.len() * (factor * factor) as usize);
    for y in 0..h * factor {
        for x in 0..w * factor {
            let at = (((y / factor) * w + x / factor) * 3) as usize;
            out.extend_from_slice(&rgb[at..at + 3]);
        }
    }
    out
}

/// Pictures side by side (same height), with a gap.
pub fn side_by_side(pictures: &[(&[u8], u32)], h: u32, gap: u32) -> (Vec<u8>, u32) {
    let total: u32 = pictures.iter().map(|(_, w)| w).sum::<u32>()
        + gap * (pictures.len() as u32).saturating_sub(1);
    let mut out = vec![0x20u8; (total * h * 3) as usize];
    let mut left = 0;
    for (rgb, w) in pictures {
        for y in 0..h {
            let from = (y * w * 3) as usize;
            let to = ((y * total + left) * 3) as usize;
            out[to..to + (w * 3) as usize].copy_from_slice(&rgb[from..from + (w * 3) as usize]);
        }
        left += w + gap;
    }
    (out, total)
}

/// Pictures stacked (same width), with a gap.
pub fn stacked(pictures: &[(&[u8], u32)], w: u32, gap: u32) -> (Vec<u8>, u32) {
    let mut out = Vec::new();
    let mut total = 0;
    for (index, (rgb, h)) in pictures.iter().enumerate() {
        if index > 0 {
            out.extend(std::iter::repeat_n(0x20u8, (gap * w * 3) as usize));
            total += gap;
        }
        out.extend_from_slice(&rgb[..(w * h * 3) as usize]);
        total += h;
    }
    (out, total)
}
