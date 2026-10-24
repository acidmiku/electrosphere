//! CD reads and streaming (`cdstream.rs`) and libcd (`mainloop::pending_sdk`)
//! on original snapshots, plus the host drive model [`CdDrive`].
//!
//! Captures (`analysis/flight-runtime`, both from the mission-006 checkpoint
//! with CROSS on fields 1..5 and 81..85):
//! * `mainloop-cd-001`: fields 82..306, the state-2 loading reads: the CD
//!   stop at field 82 (blocking Pause), the read request 8002B9EC from field
//!   83 (Setloc, Setmode 0xA0, ReadN through 80029DCC/80029A40) and its
//!   sectors (80029624, fields 103..110), with every libcd interrupt the
//!   handler 800A3018 read (pc 800A1A70: kind, response count and bytes).
//! * `mainloop-cd-002`: probe, fields 300..1100: the in-flight stream fade
//!   (8002BEBC, field 491) and start (8002BABC/8002A34C kind 2, field 559).
//! * `mainloop-cd-003`: fields 489..640: the fade-out and stop (8002BEBC,
//!   8002A80C), the XA audio stream start (8002BABC, 8002A34C, 800299C8,
//!   80029BB0 states through ReadS), its data sectors (8002993C/800297B8)
//!   and the stream service 8002A064.
//!
//! Every comparison is exact over RAM outside the stack, the scratchpad,
//! GTE registers and v0, with the drive replayed from the capture
//! ([`Replay`]): each original handler invocation's interrupts in order,
//! sector data from the disc image. Excluded: the interrupt stack below the
//! entry `sp` for calls inside the CD interrupt (the original saves
//! registers there; the translation keeps them in Rust) and, for a main
//! program call during which the CD interrupt ran, the interrupt context
//! libetc and the BIOS keep ([`IRQ_CONTEXT`]).
use ac3_flight::{
    Device, Machine,
    cdstream::{self, Archives, CdAudio, CdDrive, Disc, RawImage, SECTOR_BYTES},
    machine::HostCall,
    mainloop::{self, pending_sdk},
    sdk,
    snapshot::{self, Snapshot, SnapshotInfo},
};
use std::{cell::RefCell, collections::VecDeque, fs::File, path::PathBuf, rc::Rc};

const GP: u32 = ac3_flight::GP;
const DISC: &str = "Ace Combat 3 - Electrosphere (Europe) (En,Fr,De,Es,It).bin";
/// The CD interrupt handler's entry `sp` in the captures (interrupt stack).
const IRQ_SP: u32 = 0x800b_3f1c;
/// Interrupt context written outside the CD handler when it runs during a
/// main-program call: BIOS kernel RAM of its exception handler
/// (0x80008500..0x80008600 and the exception frame 0x8000E200..0x8000E290)
/// and libetc's dispatcher frame and state on the interrupt stack
/// (0x800B3D00..0x800B3F40: saved registers and its loop count).
const IRQ_CONTEXT: &[(u32, u32)] = &[
    (0x8000_8500, 0x8000_8600),
    (0x8000_e200, 0x8000_e290),
    (0x800b_3d00, 0x800b_3f40),
];
/// The native host runs the CD interrupt on the main program's stack (the
/// original's interrupt stack lies below `Machine`'s stack region), so
/// calls captured inside the interrupt run from here.
const HOST_IRQ_SP: u32 = 0x801f_f000;

fn data() -> PathBuf {
    PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR"))
}
fn disc() -> RawImage<File> {
    RawImage(File::open(data().join(DISC)).expect("disc image"))
}

struct Capture {
    directory: PathBuf,
    index: Vec<SnapshotInfo>,
}

fn capture(name: &str) -> Capture {
    let directory = data().join("analysis/flight-runtime").join(name);
    let index = snapshot::read_index(&directory.join("snapshots.jsonl")).expect("index");
    Capture { directory, index }
}

impl Capture {
    fn load(&self, info: &SnapshotInfo) -> Snapshot {
        Snapshot::load(&self.directory, info).expect("snapshot image")
    }
    /// Every entry of `callee` with its matching return (same ra and sp).
    fn calls(&self, callee: u32) -> Vec<(&SnapshotInfo, &SnapshotInfo)> {
        self.index
            .iter()
            .filter(|s| s.callee == callee && s.stage == "entry")
            .map(|entry| {
                let exit = self
                    .index
                    .iter()
                    .find(|s| {
                        s.callee == callee
                            && s.stage == "return"
                            && s.sequence > entry.sequence
                            && s.ra == entry.ra
                            && s.registers.gpr[29] == entry.registers.gpr[29]
                    })
                    .expect("matching return");
                (entry, exit)
            })
            .collect()
    }
    /// Snapshots strictly between two sequence numbers.
    fn between(&self, from: u64, to: u64) -> impl Iterator<Item = &SnapshotInfo> {
        self.index
            .iter()
            .filter(move |s| s.sequence > from && s.sequence < to)
    }
    /// The interrupts each CD handler invocation between `from` and `to`
    /// read, in order.
    fn interrupt_groups(&self, from: u64, to: u64) -> VecDeque<VecDeque<Event>> {
        let handlers: Vec<_> = self
            .calls(pending_sdk::CD_INTERRUPT_HANDLER)
            .into_iter()
            .filter(|(entry, exit)| entry.sequence >= from && exit.sequence <= to)
            .collect();
        handlers
            .iter()
            .map(|(entry, exit)| {
                self.between(entry.sequence, exit.sequence)
                    .filter(|s| s.callee == 0x800a_1a70 && s.stage == "pc")
                    .map(|s| event(self, s))
                    .collect()
            })
            .collect()
    }
}

fn word(snapshot: &Snapshot, address: u32) -> u32 {
    let i = (address & 0x1f_ffff) as usize;
    u32::from_le_bytes(snapshot.ram[i..i + 4].try_into().unwrap())
}
fn byte(snapshot: &Snapshot, address: u32) -> u8 {
    snapshot.ram[(address & 0x1f_ffff) as usize]
}

/// One interrupt the original's 800A1988 read (its pc snapshot at
/// 800A1A70: kind at sp+0x10, response count in s0, bytes at sp+0x18).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Event {
    kind: u8,
    count: u8,
    bytes: [u8; 8],
}

/// What a drive writes for one interrupt into 800A1988's frame (CD host
/// protocol): kind at +0x10, response count at +0x11, bytes at +0x18.
fn put_interrupt(ram: &mut [u8], frame: u32, kind: u8, response: &[u8]) {
    let mut write =
        |offset: u32, byte: u8| ram[(frame.wrapping_add(offset) & 0x1f_ffff) as usize] = byte;
    write(0x10, kind);
    write(0x11, response.len() as u8);
    for (i, &byte) in response.iter().enumerate() {
        write(0x18 + i as u32, byte);
    }
}

fn event(capture: &Capture, info: &SnapshotInfo) -> Event {
    let snapshot = capture.load(info);
    let sp = info.registers.gpr[29];
    Event {
        kind: byte(&snapshot, sp + 0x10),
        count: info.registers.gpr[16] as u8,
        bytes: std::array::from_fn(|i| byte(&snapshot, sp + 0x18 + i as u32)),
    }
}

/// The original's drive, replayed: interrupts per handler invocation (a
/// handler-site poll returns the group's next interrupt, then 0 once and
/// moves on), the sectors of data-ready interrupts from the disc (`lbas`
/// in order; from the header with Setmode bit 5), and `polls` for blocking
/// commands. Commands are recorded.
struct Replay {
    groups: VecDeque<VecDeque<Event>>,
    current: Option<VecDeque<Event>>,
    lbas: VecDeque<u32>,
    disc: RawImage<File>,
    buffer: Vec<u8>,
    cursor: usize,
    polls: u32,
    commands: Rc<RefCell<Vec<[u32; 4]>>>,
}

impl Replay {
    fn new(groups: VecDeque<VecDeque<Event>>, lbas: Vec<u32>, polls: u32) -> Self {
        Self {
            groups,
            current: None,
            lbas: lbas.into(),
            disc: disc(),
            buffer: Vec::new(),
            cursor: 0,
            polls,
            commands: Rc::default(),
        }
    }
    /// The sector the drive holds when a captured ready callback starts
    /// (its data-ready interrupt was read before the entry snapshot).
    fn latch(&mut self, lba: u32, mode: u8) {
        let sector = self.disc.sector(lba).expect("disc sector");
        let start = if mode & 0x20 != 0 { 12 } else { 24 };
        self.buffer = sector[start..].to_vec();
        self.cursor = 0;
    }
}

impl Device for Replay {
    fn call(&mut self, ram: &mut [u8], _: &mut [u8], call: HostCall) -> Option<u32> {
        let [a0, a1, ..] = call.args;
        match call.function {
            pending_sdk::CD_COMMAND => {
                self.commands.borrow_mut().push(call.args);
                Some(self.polls)
            }
            pending_sdk::CD_INTERRUPT => {
                if a0 != pending_sdk::SITE_HANDLER {
                    return Some(0);
                }
                let group = self
                    .current
                    .get_or_insert_with(|| self.groups.pop_front().unwrap_or_default());
                let Some(event) = group.pop_front() else {
                    self.current = None;
                    return Some(0);
                };
                put_interrupt(ram, a1, event.kind, &event.bytes[..event.count as usize]);
                if event.kind == 1 {
                    let lba = self.lbas.pop_front().expect("a sector for each data ready");
                    let sector = self.disc.sector(lba).expect("disc sector");
                    let mode = ram[(pending_sdk::CD_LAST_MODE & 0x1f_ffff) as usize];
                    let start = if mode & 0x20 != 0 { 12 } else { 24 };
                    self.buffer = sector[start..].to_vec();
                    self.cursor = 0;
                }
                Some(0)
            }
            pending_sdk::CD_GET_SECTOR | pending_sdk::CD_GET_SECTOR2 => {
                for i in 0..a1 as usize * 4 {
                    let value = self.buffer.get(self.cursor).copied().unwrap_or(0);
                    self.cursor += 1;
                    ram[(a0.wrapping_add(i as u32) & 0x1f_ffff) as usize] = value;
                }
                Some(0)
            }
            _ => None,
        }
    }
}

/// The driver state `[gp+0x38]` of a snapshot.
fn driver(snapshot: &Snapshot) -> u32 {
    word(snapshot, GP + 0x38)
}

/// The sector the drive delivers to the CPU next for the driver in
/// `snapshot`: after the last one read (+0x24; the start +0x10 before the
/// first), skipping XA-ADPCM sectors (submode 0x64) a stream plays.
fn next_cpu_sector(snapshot: &Snapshot, disc: &mut RawImage<File>) -> u32 {
    let d = driver(snapshot);
    let last = word(snapshot, d + 0x24);
    let mut lba = if last == 0 {
        word(snapshot, d + 0x10)
    } else {
        last + 1
    };
    let streaming = word(snapshot, d) & 0x1000 != 0;
    for _ in 0..64 {
        let sector = disc.sector(lba).unwrap();
        if !streaming || sector[18] & 0x64 != 0x64 {
            return lba;
        }
        lba += 1;
    }
    panic!("no CPU sector after {lba}");
}

/// Exact comparison outside the stack and `excluded`.
fn verify(
    actual: &mut Machine,
    expected: &Snapshot,
    excluded: &[(u32, u32)],
) -> Result<(), String> {
    actual.take_fault().map_err(|f| f.to_string())?;
    let differences = snapshot::compare(actual, expected);
    let ram: Vec<u32> = differences
        .ram
        .iter()
        .copied()
        .filter(|a| !excluded.iter().any(|(lo, hi)| (*lo..*hi).contains(a)))
        .collect();
    if ram.is_empty()
        && differences.scratch.is_empty()
        && differences.gte_data.is_empty()
        && differences.gte_control.is_empty()
    {
        return Ok(());
    }
    let mut ranges = Vec::new();
    let mut i = 0;
    while i < ram.len() && ranges.len() < 24 {
        let (start, mut end) = (ram[i], ram[i]);
        while i + 1 < ram.len() && ram[i + 1] == end + 1 {
            i += 1;
            end += 1;
        }
        ranges.push(format!("{start:08x}+{}", end - start + 1));
        i += 1;
    }
    Err(format!(
        "ram {} [{}] scratch {:?}",
        ram.len(),
        ranges.join(" "),
        differences.scratch
    ))
}

/// The original's CD_cw calls between two sequence numbers: command code,
/// parameter count and bytes, async (the CD_COMMAND arguments).
fn commands_between(capture: &Capture, from: u64, to: u64) -> Vec<[u32; 4]> {
    capture
        .calls(pending_sdk::CD_COMMAND)
        .into_iter()
        .filter(|(entry, _)| entry.sequence >= from && entry.sequence < to)
        .map(|(entry, _)| {
            let snapshot = capture.load(entry);
            let [_, _, _, _, a0, a1, _, a3] = entry.registers.gpr[..8].try_into().unwrap();
            let count = word(&snapshot, 0x800b_4320 + (a0 & 0xff) * 4);
            let bytes = (0..count.min(4)).fold(0, |acc, i| {
                acc | u32::from(byte(&snapshot, a1 + i)) << (8 * i)
            });
            [a0 & 0xff, count, bytes, a3]
        })
        .collect()
}

struct Checked {
    calls: usize,
    changed: usize,
}

/// Run `native` on every captured call of `callee` that `select` accepts,
/// with the replayed drive, and compare with the original's return.
fn check_all(
    capture: &Capture,
    callee: u32,
    select: impl Fn(&SnapshotInfo, &SnapshotInfo) -> bool,
    native: impl Fn(&mut Machine, &Snapshot) -> Option<u32>,
) -> Checked {
    let mut disc = disc();
    let mut checked = Checked {
        calls: 0,
        changed: 0,
    };
    for (entry_info, exit_info) in capture.calls(callee) {
        if !select(entry_info, exit_info) {
            continue;
        }
        let entry = capture.load(entry_info);
        let exit = capture.load(exit_info);
        let groups = capture.interrupt_groups(entry_info.sequence, exit_info.sequence);
        let sectors = groups.iter().flatten().filter(|e| e.kind == 1).count();
        let mut lbas = Vec::new();
        if sectors > 0 {
            let first = next_cpu_sector(&entry, &mut disc);
            lbas.push(first);
            for _ in 1..sectors {
                lbas.push(lbas.last().unwrap() + 1);
            }
        }
        let polls = word(&exit, sdk::CD_TIMEOUT_POLLS);
        let mut replay = Replay::new(groups, lbas, polls);
        if [0x8002_9624, 0x8002_993c, 0x8002_97b8].contains(&callee) {
            let lba = next_cpu_sector(&entry, &mut disc);
            replay.latch(lba, byte(&entry, pending_sdk::CD_LAST_MODE));
        }
        let commands = replay.commands.clone();
        let mut m = entry.machine();
        m.device = Some(Box::new(replay));
        let inside_irq = entry_info.registers.gpr[29] < IRQ_SP + 4;
        if inside_irq {
            m.sp = HOST_IRQ_SP;
        }
        let v0 = native(&mut m, &entry);
        let interrupted = capture
            .between(entry_info.sequence, exit_info.sequence)
            .any(|s| s.callee == pending_sdk::CD_INTERRUPT_HANDLER);
        let mut excluded = Vec::new();
        if inside_irq {
            excluded.push((
                entry_info.registers.gpr[29] - 0x200,
                entry_info.registers.gpr[29],
            ));
        }
        if interrupted && !inside_irq {
            excluded.extend_from_slice(IRQ_CONTEXT);
        }
        verify(&mut m, &exit, &excluded).unwrap_or_else(|e| {
            panic!(
                "{callee:08x} seq {} field {}: {e}",
                entry_info.sequence, entry_info.frame
            )
        });
        if let Some(v0) = v0 {
            assert_eq!(
                v0,
                exit.gpr(2),
                "{callee:08x} seq {}: v0",
                entry_info.sequence
            );
        }
        assert_eq!(
            *commands.borrow(),
            commands_between(capture, entry_info.sequence, exit_info.sequence),
            "{callee:08x} seq {}: drive commands",
            entry_info.sequence
        );
        checked.calls += 1;
        checked.changed += snapshot::changed_bytes(&entry, &exit);
    }
    println!(
        "{callee:08x}: {} calls, {} original bytes changed",
        checked.calls, checked.changed
    );
    checked
}

fn args(entry: &Snapshot) -> [u32; 4] {
    [4, 5, 6, 7].map(|r| entry.gpr(r))
}

fn all(_: &SnapshotInfo, _: &SnapshotInfo) -> bool {
    true
}

// ---- libcd ---------------------------------------------------------------------------

#[test]
#[ignore = "requires AC3_DATA_DIR captures and the disc image"]
fn interrupt_handler_800a3018() {
    let mut total = 0;
    for name in ["mainloop-cd-001", "mainloop-cd-003"] {
        let capture = capture(name);
        let checked = check_all(&capture, pending_sdk::CD_INTERRUPT_HANDLER, all, |m, _| {
            pending_sdk::cd_interrupt_handler(m);
            None
        });
        total += checked.calls;
    }
    assert_eq!(total, 52, "40 + 12 handler invocations");
}

#[test]
#[ignore = "requires AC3_DATA_DIR captures and the disc image"]
fn cd_command_800a2434() {
    let mut total = 0;
    for name in ["mainloop-cd-001", "mainloop-cd-003"] {
        let capture = capture(name);
        let checked = check_all(&capture, pending_sdk::CD_COMMAND, all, |m, entry| {
            let [a0, a1, a2, a3] = args(entry);
            Some(pending_sdk::cd_command(m, a0, a1, a2, a3))
        });
        total += checked.calls;
    }
    assert_eq!(
        total, 14,
        "blocking Pause (2), async commands in and out of callbacks"
    );
}

// ---- engine ----------------------------------------------------------------------------

#[test]
#[ignore = "requires AC3_DATA_DIR captures and the disc image"]
fn request_handlers_8002b9ec_8002babc_8002bebc() {
    let loading = capture("mainloop-cd-001");
    let flight = capture("mainloop-cd-003");
    let read = check_all(&loading, 0x8002_b9ec, all, |m, entry| {
        Some(cdstream::read_file_request(m, entry.gpr(4)))
    });
    let read_flight = check_all(&flight, 0x8002_b9ec, all, |m, entry| {
        Some(cdstream::read_file_request(m, entry.gpr(4)))
    });
    let play = check_all(&flight, 0x8002_babc, all, |m, entry| {
        Some(cdstream::play_stream_request(m, entry.gpr(4)))
    });
    let fade = check_all(&flight, 0x8002_bebc, all, |m, entry| {
        Some(cdstream::fade_out_request(m, entry.gpr(4)))
    });
    assert_eq!(read.calls + read_flight.calls, 12);
    assert_eq!([play.calls, fade.calls], [20, 12]);
    assert!(play.changed > 0 && fade.changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR captures and the disc image"]
fn start_read_8002a34c_seek_800299c8() {
    let mut calls = [0; 2];
    for name in ["mainloop-cd-001", "mainloop-cd-003"] {
        let capture = capture(name);
        calls[0] += check_all(&capture, 0x8002_a34c, all, |m, entry| {
            let [a0, a1, a2, a3] = args(entry);
            let flags = m.lw(entry.gpr(29) + 0x10);
            Some(cdstream::start_read(m, a0, a1, a2, a3, flags))
        })
        .calls;
        calls[1] += check_all(&capture, 0x8002_99c8, all, |m, _| {
            cdstream::seek_start(m);
            None
        })
        .calls;
    }
    assert_eq!(
        calls,
        [3, 3],
        "data reads at fields 83 and 281, the XA stream at 559"
    );
}

#[test]
#[ignore = "requires AC3_DATA_DIR captures and the disc image"]
fn command_states_80029dcc_80029a40_80029bb0() {
    let loading = capture("mainloop-cd-001");
    let flight = capture("mainloop-cd-003");
    let mut calls = 0;
    for capture in [&loading, &flight] {
        calls += check_all(capture, 0x8002_9dcc, all, |m, entry| {
            cdstream::sync_callback(m, entry.gpr(4), entry.gpr(5));
            None
        })
        .calls;
    }
    let data = check_all(&loading, 0x8002_9a40, all, |m, _| {
        cdstream::data_states(m);
        None
    });
    let stream = check_all(&flight, 0x8002_9bb0, all, |m, _| {
        cdstream::stream_states(m);
        None
    });
    assert_eq!([calls, data.calls, stream.calls], [12, 8, 4]);
}

#[test]
#[ignore = "requires AC3_DATA_DIR captures and the disc image"]
fn data_ready_80029624() {
    let capture = capture("mainloop-cd-001");
    let checked = check_all(&capture, 0x8002_9624, all, |m, entry| {
        cdstream::data_ready(m, entry.gpr(4), entry.gpr(5));
        None
    });
    assert_eq!(checked.calls, 24);
    assert!(checked.changed >= 24 * 0x400, "sector data transferred");
}

#[test]
#[ignore = "requires AC3_DATA_DIR captures and the disc image"]
fn stream_sectors_8002993c_800297b8() {
    let capture = capture("mainloop-cd-003");
    let ready = check_all(&capture, 0x8002_993c, all, |m, entry| {
        cdstream::stream_ready(m, entry.gpr(4), entry.gpr(5));
        None
    });
    let sector = check_all(&capture, 0x8002_97b8, all, |m, _| {
        cdstream::stream_sector(m);
        None
    });
    assert_eq!([ready.calls, sector.calls], [6, 6]);
}

#[test]
#[ignore = "requires AC3_DATA_DIR captures and the disc image"]
fn stream_service_8002a064() {
    let capture = capture("mainloop-cd-003");
    let checked = check_all(&capture, 0x8002_a064, all, |m, entry| {
        Some(cdstream::stream_service(m, entry.gpr(4)))
    });
    assert_eq!(checked.calls, 20);
}

#[test]
#[ignore = "requires AC3_DATA_DIR captures and the disc image"]
fn queue_monitor_and_stop_8002c0b4_80029e8c_8002a80c() {
    let loading = capture("mainloop-cd-001");
    let flight = capture("mainloop-cd-003");
    let mut queue = 0;
    for capture in [&loading, &flight] {
        queue += check_all(capture, 0x8002_c0b4, all, |m, _| {
            mainloop::process_cd_queue(m);
            None
        })
        .calls;
    }
    let monitor = check_all(&flight, 0x8002_9e8c, all, |m, _| {
        mainloop::cd_monitor(m);
        None
    });
    let stop = check_all(&flight, 0x8002_a80c, all, |m, entry| {
        Some(mainloop::cd_stop(m, entry.gpr(4)))
    });
    assert_eq!([queue, monitor.calls, stop.calls], [27, 10, 1]);
}

// ---- the drive model -----------------------------------------------------------------------

/// Records the XA sectors the drive plays: (LBA from the header, file,
/// channel).
#[derive(Default)]
struct Played(Rc<RefCell<Vec<(u32, u8, u8)>>>);
impl CdAudio for Played {
    fn xa_sector(&mut self, sector: &[u8; SECTOR_BYTES]) {
        let bcd = |b: u8| u32::from(b >> 4) * 10 + u32::from(b & 0xf);
        let lba = (bcd(sector[12]) * 60 + bcd(sector[13])) * 75 + bcd(sector[14]) - 150;
        self.0.borrow_mut().push((lba, sector[16], sector[17]));
    }
    fn mix(&mut self, _: [u8; 4]) {}
}

/// Records the drive's commands while forwarding to it.
struct Logged<D> {
    drive: D,
    commands: Rc<RefCell<Vec<[u32; 4]>>>,
}
impl<D: Device> Device for Logged<D> {
    fn call(&mut self, ram: &mut [u8], scratch: &mut [u8], call: HostCall) -> Option<u32> {
        if call.function == pending_sdk::CD_COMMAND {
            self.commands.borrow_mut().push(call.args);
        }
        self.drive.call(ram, scratch, call)
    }
}

fn advance_vblank(m: &mut Machine) {
    let count = m.lw(sdk::VSYNC_COUNTER);
    m.sw(sdk::VSYNC_COUNTER, count.wrapping_add(1));
}

#[test]
#[ignore = "requires AC3_DATA_DIR captures and the disc image"]
fn drive_completes_the_loading_read() {
    // Field 83's request 2 (phase 0) on the drive model: run it, then one
    // CD interrupt service per VBlank and the request each main-loop pass
    // (every VBlank in loading), until it reports done.
    let capture = capture("mainloop-cd-001");
    let (entry_info, _) = capture.calls(0x8002_b9ec)[0];
    let entry = capture.load(entry_info);
    let request = entry.gpr(4);
    let mut m = entry.machine();
    let commands = Rc::default();
    m.device = Some(Box::new(Logged {
        drive: CdDrive::new(disc(), cdstream::CLOCK),
        commands: Rc::clone(&commands),
    }));
    let file = sext16(m.lh(request + 2));
    let destination = m.lw(request + 4);
    let record = ac3_flight::dispatch::call(&mut m, 0x8002_953c, [file, 0, 0, 0]);
    let words = m.lw(record) & 0x7_ffff;
    let start = m.lw(record + 4);
    let mut done = cdstream::read_file_request(&mut m, request);
    let mut vblanks = 0;
    while done == 0 && vblanks < 3000 {
        advance_vblank(&mut m);
        cdstream::cd_service(&mut m);
        done = cdstream::read_file_request(&mut m, request);
        vblanks += 1;
        assert_eq!(m.take_fault(), Ok(()));
    }
    assert_eq!(done, 1, "read finished after {vblanks} VBlanks");
    let sectors = words.div_ceil(0x200);
    println!("file {file:#x}: {words:#x} words from {start}, {vblanks} VBlanks, {sectors} sectors");
    let mut disc = disc();
    for i in 0..sectors {
        let raw = disc.sector(start + i).unwrap();
        let length = (words - i * 0x200).min(0x200) as usize * 4;
        let ram = m.read_bytes(destination + i * 0x800, length);
        assert_eq!(&ram[..], &raw[24..24 + length], "sector {i}");
    }
    let d = m.lw(GP + 0x38);
    assert_eq!(m.lw(d + 0x14), 0, "no words left");
    assert_eq!(m.lw(d + 0x24), start + sectors - 1, "last sector read");
    assert_eq!(m.lw(d) & 0x8000_0000, 0, "driver closed");
    let queue = m.lw(GP + 0x48);
    assert_eq!([m.lw(queue + 0x168), m.lw(queue + 0x16c)], [1, 0]);
    assert_eq!(m.lw(queue + 0x174), u32::MAX);
    let codes: Vec<u32> = commands.borrow().iter().map(|c| c[0]).collect();
    assert_eq!(codes, [2, 0xe, 6, 9], "Setloc, Setmode 0xA0, ReadN, Pause");
    assert_eq!(commands.borrow()[1][2], 0xa0);
    assert_eq!(
        [
            m.lw(pending_sdk::CD_SYNC_CALLBACK_SLOT),
            m.lw(pending_sdk::CD_READY_CALLBACK_SLOT)
        ],
        [0, 0]
    );
}

fn sext16(value: i16) -> u32 {
    value as i32 as u32
}

#[test]
#[ignore = "requires AC3_DATA_DIR captures and the disc image"]
fn drive_plays_the_flight_xa_stream() {
    // Field 559's stream start (request 3, phase 0) on the drive model, then
    // the CD monitor (with the stream service hook) and the request every
    // two VBlanks, as the flight main loop runs them.
    let capture = capture("mainloop-cd-003");
    let (entry_info, _) = capture.calls(0x8002_babc)[0];
    let entry = capture.load(entry_info);
    let request = entry.gpr(4);
    let mut m = entry.machine();
    let played = Played::default();
    let log = Rc::clone(&played.0);
    let mut drive = CdDrive::new(disc(), cdstream::CLOCK);
    drive.audio = Some(Box::new(played));
    let commands = Rc::default();
    m.device = Some(Box::new(Logged {
        drive,
        commands: Rc::clone(&commands),
    }));
    assert_eq!(cdstream::play_stream_request(&mut m, request), 0);
    // The data sectors' positions as the original read them (800297B8).
    let original: Vec<(u32, u32)> = capture
        .calls(0x8002_97b8)
        .into_iter()
        .map(|(_, exit)| {
            let exit = capture.load(exit);
            let d = driver(&exit);
            (word(&exit, d + 0x24), word(&exit, d + 0x14))
        })
        .collect();
    let d = m.lw(GP + 0x38);
    let start = m.lw(d + 0x10);
    // The engine's clock is the context's VBlank count (+0x20), which the
    // main loop stores from VSync(-1) each pass.
    let clock = m.lw(GP + 0x2dc) + 0x20;
    let mut positions = Vec::new();
    let mut last = 0;
    for vblank in 0..400 {
        advance_vblank(&mut m);
        cdstream::cd_service(&mut m);
        if vblank % 2 == 1 {
            let count = m.lw(sdk::VSYNC_COUNTER);
            m.sw(clock, count);
            mainloop::cd_monitor(&mut m);
            cdstream::play_stream_request(&mut m, request);
        }
        assert_eq!(m.take_fault(), Ok(()));
        let d = m.lw(GP + 0x38);
        let position = m.lw(d + 0x24);
        if position != last && position >= start {
            positions.push((position, m.lw(d + 0x14)));
            last = position;
        }
    }
    let codes: Vec<[u32; 4]> = commands.borrow().clone();
    assert_eq!(
        codes,
        commands_between(&capture, 87, 129),
        "Setloc, Setfilter, Setmode, ReadS as the original"
    );
    let d = m.lw(GP + 0x38);
    assert_eq!(m.lw(d + 0x20), 0x40, "streaming");
    assert_eq!(
        positions[..original.len()],
        original[..],
        "data sectors (position, sectors left) as the original"
    );
    let played = log.borrow();
    assert!(played.len() > 50, "{} XA sectors played", played.len());
    assert!(
        played
            .iter()
            .all(|&(_, file, channel)| (file, channel) == (1, 0x10))
    );
    assert!(
        played
            .iter()
            .all(|&(lba, ..)| (lba - start).is_multiple_of(8)),
        "every 8th sector"
    );
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the extracted archives"]
fn archives_rebuild_the_disc_sectors() {
    let root = data().join("extracted/disc");
    let bpb = File::open(root.join("ACE.BPB")).unwrap();
    let spb = File::open(root.join("ACE.SPB.raw2352")).unwrap();
    let bpb_sectors = (bpb.metadata().unwrap().len() / 2048) as u32;
    let spb_sectors = (spb.metadata().unwrap().len() / SECTOR_BYTES as u64) as u32;
    let mut archives = Archives {
        bpb,
        bpb_lba: 469,
        bpb_sectors,
        spb,
        spb_lba: 17388,
        spb_sectors,
    };
    let mut image = disc();
    for lba in [469, 470, 1198, 469 + bpb_sectors - 1, 17388, 93840, 93868] {
        let rebuilt = archives.sector(lba).unwrap();
        let original = image.sector(lba).unwrap();
        // Header, subheader and user data (EDC/ECC are not rebuilt).
        assert_eq!(rebuilt[..24 + 2048], original[..24 + 2048], "LBA {lba}");
    }
}

// ---- unit tests (no data) ----------------------------------------------------------------

/// libetc's interrupt table (0x800B3F94, resident data) and its pointer:
/// CD_init's ResetCallback and InterruptCallback go through it.
fn libetc_table(m: &mut Machine) {
    let table = [
        0x8002_45e4u32,
        0,
        0x800a_0bb4,
        0x800a_090c,
        0x800a_0cfc,
        0,
        0x800a_0d9c,
        0x800b_2f2c,
    ];
    for (i, word) in table.into_iter().enumerate() {
        m.sw(0x800b_3f94 + i as u32 * 4, word);
    }
    m.sw(0x800b_3fb4, 0x800b_3f94);
}

/// libcd's per-command tables (resident data, as in the captures), with
/// libetc's interrupt table.
fn libcd_tables(m: &mut Machine) {
    libetc_table(m);
    for command in 0..32u32 {
        let set = |list: &[u32]| u32::from(list.contains(&command));
        let at = command * 4;
        m.sw(0x800b_4054 + at, set(&[3, 6, 0x15, 0x16, 0x1b]));
        m.sw(
            0x800b_41a0 + at,
            set(&[7, 8, 9, 0xa, 0x12, 0x15, 0x16, 0x1a]),
        );
        m.sw(0x800b_4220 + at, set(&[3, 4, 5, 6, 0x15, 0x16, 0x1b]));
        m.sw(0x800b_42a0 + at, 1 - set(&[0x10, 0x11]));
        let count = match command {
            2 => 3,
            0xd => 2,
            0xe | 0x12 | 0x14 => 1,
            _ => 0,
        };
        m.sw(0x800b_4320 + at, count);
    }
}

/// A drive that only raises the queued interrupts (kind, response).
/// Queued interrupts: kind and response bytes.
type Queue = Rc<RefCell<VecDeque<(u8, Vec<u8>)>>>;

#[derive(Clone, Default)]
struct Interrupts(Queue);
impl Device for Interrupts {
    fn call(&mut self, ram: &mut [u8], _: &mut [u8], call: HostCall) -> Option<u32> {
        if call.function == pending_sdk::CD_COMMAND {
            return Some(1);
        }
        if call.function != pending_sdk::CD_INTERRUPT {
            return None;
        }
        if let Some((kind, response)) = self.0.borrow_mut().pop_front() {
            put_interrupt(ram, call.args[1], kind, &response);
        }
        Some(0)
    }
}

/// Attach an [`Interrupts`] drive to `m` and return its queue.
fn interrupts(m: &mut Machine) -> Interrupts {
    let queue = Interrupts::default();
    m.device = Some(Box::new(queue.clone()));
    queue
}

fn queue_interrupt(queue: &Interrupts, kind: u8, response: &[u8]) {
    queue.0.borrow_mut().push_back((kind, response.to_vec()));
}

fn unit_machine() -> Machine {
    let mut m = Machine::default();
    libcd_tables(&mut m);
    m.sb(pending_sdk::CD_SYNC_STATUS, 2);
    m
}

#[test]
fn interrupt_kinds_update_libcd_state() {
    use pending_sdk::*;
    let mut m = unit_machine();
    let q = interrupts(&mut m);
    // Acknowledge of Pause (a second response follows): status 3, no
    // callback due; then the completion.
    m.sb(CD_LAST_COMMAND, 9);
    queue_interrupt(&q, 3, &[0x22]);
    assert_eq!(cd_get_interrupt(&mut m, SITE_HANDLER), 1);
    assert_eq!([m.lbu(CD_SYNC_STATUS), m.lbu(CD_SYNC_RESULT)], [3, 0x22]);
    assert_eq!(m.lw(CD_STATUS), 0x22);
    queue_interrupt(&q, 2, &[0x02]);
    assert_eq!(cd_get_interrupt(&mut m, SITE_HANDLER), 2);
    assert_eq!(m.lbu(CD_SYNC_STATUS), 2);
    // An acknowledge with error bits; the shell newly open is counted.
    queue_interrupt(&q, 3, &[0x13, 0x40]);
    assert_eq!(cd_get_interrupt(&mut m, SITE_HANDLER), 2);
    assert_eq!(
        [
            m.lbu(CD_SYNC_STATUS),
            m.lw(CD_SHELL_OPENED) as u8,
            m.lw(CD_STATUS2) as u8
        ],
        [5, 1, 0x40]
    );
    // GetlocL's acknowledge leaves the status words.
    m.sb(CD_LAST_COMMAND, 0x10);
    queue_interrupt(&q, 3, &[0, 1, 2, 3, 4, 5, 6, 7]);
    assert_eq!(cd_get_interrupt(&mut m, SITE_HANDLER), 2);
    assert_eq!(m.lw(CD_STATUS), 0x13);
    assert_eq!(m.read_bytes(CD_SYNC_RESULT, 8), [0, 1, 2, 3, 4, 5, 6, 7]);
    // Data ready: a lone status byte with error bits is not an error.
    queue_interrupt(&q, 1, &[0x23]);
    assert_eq!(cd_get_interrupt(&mut m, SITE_HANDLER), 4);
    assert_eq!(m.lbu(CD_READY_STATUS), 1);
    queue_interrupt(&q, 1, &[0x23, 0x04]);
    cd_get_interrupt(&mut m, SITE_HANDLER);
    assert_eq!(m.lbu(CD_READY_STATUS), 5);
    // Data end and error.
    queue_interrupt(&q, 4, &[0x02]);
    assert_eq!(cd_get_interrupt(&mut m, SITE_HANDLER), 4);
    assert_eq!(
        [
            m.lbu(CD_READY_STATUS),
            m.lbu(CD_END_STATUS),
            m.lbu(CD_END_RESULT)
        ],
        [4, 4, 2]
    );
    queue_interrupt(&q, 5, &[0x03, 0x80]);
    assert_eq!(cd_get_interrupt(&mut m, SITE_HANDLER), 6);
    assert_eq!([m.lbu(CD_READY_STATUS), m.lbu(CD_SYNC_STATUS)], [5, 5]);
    assert_eq!(m.read_bytes(CD_READY_RESULT, 2), [0x03, 0x80]);
    // None pending.
    assert_eq!(cd_get_interrupt(&mut m, SITE_HANDLER), 0);
    assert_eq!(m.take_fault(), Ok(()));
}

#[test]
fn handler_runs_the_due_callbacks() {
    use pending_sdk::*;
    let mut m = unit_machine();
    let q = interrupts(&mut m);
    // 8002B988 as both callbacks: it records (done, status) in the queue.
    m.sw(GP + 0x48, 0x8010_0000);
    m.sw(CD_READY_CALLBACK_SLOT, 0x8002_b988);
    m.sb(CD_LAST_COMMAND, 6);
    queue_interrupt(&q, 1, &[0x22]);
    queue_interrupt(&q, 3, &[0x22]);
    assert_eq!(cd_interrupt_handler(&mut m), 2);
    assert_eq!([m.lw(0x8010_0168), m.lw(0x8010_016c)], [1, 1], "ready(1)");
    m.sw(CD_READY_CALLBACK_SLOT, 0);
    m.sw(CD_SYNC_CALLBACK_SLOT, 0x8002_b988);
    queue_interrupt(&q, 5, &[0x03, 0x80]);
    assert_eq!(cd_interrupt_handler(&mut m), 1);
    assert_eq!(m.lw(0x8010_016c), 5, "sync(5)");
    assert!(q.0.borrow().is_empty());
    assert_eq!(m.take_fault(), Ok(()));
}

#[test]
fn commands_check_parameters_and_retry() {
    use pending_sdk::*;
    let mut m = unit_machine();
    interrupts(&mut m);
    // A command needing parameters without any: -2, nothing sent.
    assert_eq!(cd_command(&mut m, 2, 0, 0, 1), -2i32 as u32);
    assert!(m.host_calls.iter().all(|c| c.function != CD_COMMAND));
    // Setloc records the position, Setmode the mode.
    m.write_bytes(0x8010_0000, &[0x00, 0x17, 0x73, 0x80]);
    assert_eq!(cd_command(&mut m, 2, 0x8010_0000, 0, 1), 0);
    assert_eq!(m.read_bytes(CD_LAST_POSITION, 4), [0x00, 0x17, 0x73, 0x80]);
    assert_eq!(m.lbu(CD_SYNC_STATUS), 0, "sent");
    let last = m.host_calls.last().unwrap().args;
    assert_eq!(last, [2, 3, 0x0073_1700, 1]);
    // With the shell reported open, CdControlF first sends Getstat
    // (blocking), then the command; a ReadN with a position Setloc first.
    let mut m = Machine::default();
    m.device = Some(Box::new(CdDrive::new(Empty, cdstream::CLOCK)));
    libcd_tables(&mut m);
    m.sb(CD_SYNC_STATUS, 2);
    m.sw(CD_STATUS, 0x12);
    m.write_bytes(0x8010_0000, &[0x00, 0x02, 0x10]);
    assert_eq!(cd_control_f(&mut m, 6, 0x8010_0000), 1);
    let sent: Vec<[u32; 4]> = m
        .host_calls
        .iter()
        .filter(|c| c.function == CD_COMMAND)
        .map(|c| c.args)
        .collect();
    assert_eq!(sent, [[1, 0, 0, 0], [2, 3, 0x10_0200, 0], [6, 0, 0, 1]]);
    assert_eq!(m.lbu(CD_READY_STATUS), 0, "ReadN clears the ready status");
    assert_eq!(m.take_fault(), Ok(()));
    // A drive that never interrupts: the blocking wait times out
    // (untranslated); no drive at all: the command itself faults.
    let mut m = unit_machine();
    interrupts(&mut m);
    assert_eq!(cd_control(&mut m, 9, 0, 0), 0);
    assert_eq!(
        m.take_fault(),
        Err(ac3_flight::Fault::Untranslated {
            address: 0x800a_2694
        })
    );
    let mut m = unit_machine();
    assert_eq!(cd_control_f(&mut m, 9, 0), 0);
    assert_eq!(
        m.take_fault(),
        Err(ac3_flight::Fault::Untranslated {
            address: CD_COMMAND
        })
    );
}

#[test]
fn positions_and_reset() {
    use pending_sdk::*;
    let mut m = Machine::default();
    for sector in [0u32, 1198, 13269, 93840, 333_000] {
        cd_int_to_pos(&mut m, sector, 0x8010_0000);
        assert_eq!(sdk::cd_pos_to_int(&mut m, 0x8010_0000), sector);
    }
    cd_int_to_pos(&mut m, 1198, 0x8010_0000);
    assert_eq!(m.read_bytes(0x8010_0000, 3), [0x00, 0x17, 0x73]);
    assert_eq!(cd_int_to_pos(&mut m, 93840, 0x8010_0010), 0x8010_0010);
    assert_eq!(m.read_bytes(0x8010_0010, 3), [0x20, 0x53, 0x15]);
    // CdReset(0) without a drive: CD_init's Getstat finds no device.
    libetc_table(&mut m);
    assert_eq!(cd_reset(&mut m, 0), 0);
    assert_eq!(
        m.take_fault(),
        Err(ac3_flight::Fault::Untranslated {
            address: CD_COMMAND
        })
    );
    assert_eq!(cd_sync_callback(&mut m, 0x8002_9dcc), 0);
    assert_eq!(cd_sync_callback(&mut m, 0), 0x8002_9dcc);
    assert_eq!(cd_ready_callback(&mut m, 0x8002_9624), 0);
    assert_eq!(cd_ready_callback(&mut m, 0), 0x8002_9624);
}

/// A synthetic disc: data sectors (submode 0x08) except that sectors whose
/// LBA is a multiple of 4 plus 1 are XA audio of file 1, channel LBA % 3.
struct Synthetic;
impl Disc for Synthetic {
    fn sector(&mut self, lba: u32) -> Option<[u8; SECTOR_BYTES]> {
        let mut sector = [0; SECTOR_BYTES];
        let absolute = lba + 150;
        let bcd = |v: u32| (((v / 10) << 4) | (v % 10)) as u8;
        sector[12..16].copy_from_slice(&[
            bcd(absolute / 4500),
            bcd(absolute / 75 % 60),
            bcd(absolute % 75),
            2,
        ]);
        if lba % 4 == 1 {
            sector[16..20].copy_from_slice(&[1, (lba % 3) as u8, 0x64, 0]);
        } else {
            sector[16..20].copy_from_slice(&[0, 0, 0x08, 0]);
        }
        sector[24..28].copy_from_slice(&lba.to_le_bytes());
        Some(sector)
    }
}

struct Empty;
impl Disc for Empty {
    fn sector(&mut self, _: u32) -> Option<[u8; SECTOR_BYTES]> {
        None
    }
}

#[test]
fn drive_paces_reads_and_routes_xa_audio() {
    use pending_sdk::*;
    let mut m = unit_machine();
    let played = Played::default();
    let log = Rc::clone(&played.0);
    let mut drive = CdDrive::new(Synthetic, cdstream::CLOCK);
    drive.audio = Some(Box::new(played));
    drive.seek_vblanks = 1;
    m.device = Some(Box::new(drive));
    // XA-ADPCM with the filter on file 1, channel 2, double speed.
    m.write_bytes(0x8010_0000, &[0x00, 0x02, 0x10, 0, 0xe8, 0, 0, 0, 1, 2]);
    assert_eq!(cd_control_b(&mut m, 2, 0x8010_0000, 0x8010_0040), 1);
    assert_eq!(cd_control_b(&mut m, 0xd, 0x8010_0008, 0x8010_0040), 1);
    assert_eq!(cd_control_b(&mut m, 0xe, 0x8010_0004, 0x8010_0040), 1);
    // Count data-ready interrupts with 8002B988 as the ready callback.
    m.sw(GP + 0x48, 0x8010_1000);
    m.sw(CD_READY_CALLBACK_SLOT, 0x8002_b988);
    assert_eq!(cd_control_f(&mut m, 0x1b, 0), 1);
    let mut ready = Vec::new();
    for _ in 0..5 {
        cdstream::cd_service(&mut m);
        ready.push(m.lw(0x8010_1168));
        m.sw(0x8010_1168, 0);
        let count = m.lw(sdk::VSYNC_COUNTER);
        m.sw(sdk::VSYNC_COUNTER, count + 1);
    }
    assert_eq!(
        ready,
        [0, 1, 1, 1, 1],
        "first sector one VBlank after ReadS"
    );
    // From LBA 10 (00:02:10), one sector at the first VBlank, then 3 per
    // VBlank at double speed: 10..=19. Audio sectors 13 and 17 (channels
    // 13 % 3 = 1, 17 % 3 = 2): only 17 matches the filter.
    assert_eq!(*log.borrow(), [(17, 1, 2)]);
    // The last data sector's bytes, from the header (whole-sector mode).
    cd_get_sector(&mut m, 0x8010_2000, 4);
    assert_eq!(m.read_bytes(0x8010_200c, 4), 19u32.to_le_bytes());
    // Pause: acknowledge and completion, then no more sectors.
    m.sw(CD_SYNC_CALLBACK_SLOT, 0);
    assert_eq!(cd_control_b(&mut m, 9, 0, 0x8010_0040), 1);
    let count = m.lw(sdk::VSYNC_COUNTER);
    m.sw(sdk::VSYNC_COUNTER, count + 10);
    assert_eq!(cdstream::cd_service(&mut m), 0);
    // An unknown command is an error.
    assert_eq!(cd_control(&mut m, 0x1f, 0, 0x8010_0040), 0);
    assert_eq!(m.lbu(0x8010_0041), 0x40);
    assert_eq!(m.take_fault(), Ok(()));
}

#[test]
fn ring_assembles_frames() {
    // Two 2-sector frames into a ring of 3 slots (0x800 bytes each): the
    // second frame does not fit after the first and wraps to the start.
    let mut m = Machine::default();
    m.sw(GP + 0x40, 0x8010_0000);
    let buffer = 0x8011_0000;
    assert_eq!(cdstream::ring_init(&mut m, buffer, 3 * 0x800, 0), buffer);
    cdstream::ring_clear(&mut m);
    let ring = 0x8010_0000;
    assert_eq!(
        [0, 4, 0xc, 0x14, 0x24].map(|o| m.lw(ring + o)),
        [8, 0x1f8, 3, buffer + 3 * 32, u32::MAX]
    );
    // A host that serves each sector: 8 header words, then 0x1F8 data words.
    struct Sectors(VecDeque<Vec<u8>>);
    impl Device for Sectors {
        fn call(&mut self, ram: &mut [u8], _: &mut [u8], call: HostCall) -> Option<u32> {
            if call.function != pending_sdk::CD_GET_SECTOR {
                return None;
            }
            let [destination, words, ..] = call.args;
            let data = self.0.front_mut()?;
            for i in 0..words as usize * 4 {
                ram[(destination as usize + i) & 0x1f_ffff] = data[i];
            }
            data.drain(..words as usize * 4);
            if data.is_empty() {
                self.0.pop_front();
            }
            Some(0)
        }
    }
    let sector = |frame: u16, index: u8| {
        let mut data = vec![0u8; 0x20 + 0x7e0];
        data[0] = 1;
        data[1] = index;
        data[2] = 2;
        data[6..8].copy_from_slice(&frame.to_le_bytes());
        data[0x20] = 0xa0 + index + frame as u8 * 0x10;
        data
    };
    m.device = Some(Box::new(Sectors(
        [sector(1, 0), sector(1, 1), sector(2, 0), sector(2, 1)].into(),
    )));
    cdstream::ring_sector(&mut m);
    cdstream::ring_sector(&mut m);
    assert_eq!(
        [m.lbu(buffer), m.lbu(buffer + 0x20)],
        [2, 2],
        "frame 1 complete"
    );
    // The consumer takes frame 1 (frees its slots); frame 2 wraps.
    m.sb(buffer, 0);
    m.sb(buffer + 0x20, 0);
    cdstream::ring_sector(&mut m);
    cdstream::ring_sector(&mut m);
    assert_eq!(m.take_fault(), Ok(()));
    let data = buffer + 3 * 32;
    assert_eq!(m.lw(ring + 0x34), 2, "two frames");
    assert_eq!(m.lbu(buffer), 2, "frame 2 overwrote slot 0 after wrapping");
    assert_eq!(m.lbu(buffer + 0x40), 4, "slot 2 marked wrapped");
    assert_eq!([m.lbu(data), m.lbu(data + 0x7e0)], [0xc0, 0xc1]);
    assert_eq!(m.lw(ring + 0x18), 2, "next frame at slot 2");
}

#[test]
fn cd_init_with_a_drive() {
    use pending_sdk::*;
    let mut m = Machine::default();
    libcd_tables(&mut m);
    m.sb(CD_LAST_MODE, 0xa0);
    let commands = Rc::default();
    m.device = Some(Box::new(Logged {
        drive: CdDrive::new(Empty, cdstream::CLOCK),
        commands: Rc::clone(&commands),
    }));
    assert_eq!(cd_init(&mut m), 1);
    let codes: Vec<u32> = commands.borrow().iter().map(|c| c[0]).collect();
    assert_eq!(codes, [1, 0xa, 0xc], "Getstat, Init, Demute");
    assert_eq!(
        [
            m.lbu(CD_SYNC_STATUS),
            m.lbu(CD_READY_STATUS),
            m.lbu(CD_LAST_MODE)
        ],
        [2, 0, 0]
    );
    assert_eq!(
        [
            m.lw(CD_SYNC_CALLBACK_SLOT),
            m.lw(CD_READY_CALLBACK_SLOT),
            m.lw(0x800b_4044)
        ],
        [0x800a_12f4, 0x800a_131c, 0x800a_1344]
    );
    assert!(m.host_calls.iter().any(|c| c.function == CD_INIT_VOLUME));
    assert_eq!(m.take_fault(), Ok(()));
    // CdReset(1) runs the same initialisation.
    assert_eq!(cd_reset(&mut m, 1), 1);
    assert_eq!(m.take_fault(), Ok(()));
}

#[test]
fn drive_answers_commands() {
    use pending_sdk::*;
    let mut m = unit_machine();
    m.device = Some(Box::new(CdDrive::new(Empty, cdstream::CLOCK)));
    // GetID: acknowledge, then the ID (complete).
    assert_eq!(cd_control_b(&mut m, 0x1a, 0, 0x8010_0000), 1);
    assert_eq!(m.read_bytes(0x8010_0000, 8), b"\x02\x00\x20\x00SCEE");
    // GetTN: first and last track.
    assert_eq!(cd_control(&mut m, 0x13, 0, 0x8010_0010), 1);
    assert_eq!(m.read_bytes(0x8010_0010, 3), [2, 1, 1]);
    // Reading past the disc's end raises an error interrupt.
    m.write_bytes(0x8010_0020, &[0x00, 0x02, 0x00]);
    assert_eq!(cd_control_f(&mut m, 6, 0x8010_0020), 1);
    let count = m.lw(sdk::VSYNC_COUNTER);
    m.sw(sdk::VSYNC_COUNTER, count + 5);
    assert_eq!(
        cdstream::cd_service(&mut m),
        2,
        "ReadN acknowledge, read error"
    );
    assert_eq!([m.lbu(CD_READY_STATUS), m.lbu(CD_SYNC_STATUS)], [5, 5]);
    assert_eq!(m.take_fault(), Ok(()));
}
