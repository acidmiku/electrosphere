//! Resident main-loop routines (`mainloop.rs`) on original flight-frame
//! snapshots, and unit tests of paths flight frames do not reach.
//!
//! Captures (`analysis/flight-runtime`):
//! * `mainloop-001`: frames 307..315, every main-loop routine bracketed after
//!   the first flight callback, plus pc snapshots of the loop top 80025D30
//!   and the state callback's entry 80025EFC / return 80025F04.
//! * `mainloop-002`: frames 444..486 and 524..548 with UP held (400..466)
//!   and START at 470 (pause) and 530 (resume): loop tops, callback entry and
//!   return, and the routines on the input and pause paths.
//!
//! Comparisons are exact over RAM outside the stack, the scratchpad, GTE
//! registers and v0. The SDK routines are `sdk.rs`'s RAM-exact translations;
//! their device results come from the captures. Whole iterations wait for
//! VBlank: interrupt handlers then change their own RAM (VBlank counter,
//! libpad's VSync receive, BIOS interrupt context). A test [`Device`] applies
//! those bytes from the original at the `VSync(n)` wait, where the original's
//! handlers ran, so the game code after it (context +0x20 = `VSync(-1)`)
//! reads what the original read.
use ac3_flight::{
    Device, Machine,
    cdstream::{CdDrive, Disc, SECTOR_BYTES},
    machine::HostCall,
    mainloop::{self, pending_sdk},
    script, sdk, shared,
    snapshot::{self, Snapshot, SnapshotInfo},
};
use std::path::PathBuf;

// ---- snapshot helpers ---------------------------------------------------------------

struct Capture {
    directory: PathBuf,
    index: Vec<SnapshotInfo>,
}

fn capture(name: &str) -> Capture {
    let directory = PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR"))
        .join("analysis/flight-runtime")
        .join(name);
    let index = snapshot::read_index(&directory.join("snapshots.jsonl")).expect("index");
    Capture { directory, index }
}

impl Capture {
    fn load(&self, info: &SnapshotInfo) -> Snapshot {
        Snapshot::load(&self.directory, info).expect("snapshot image")
    }
    /// The `occurrence`th entry of `callee` and its matching return (same
    /// ra and sp), as `snapshot::check_call` pairs them.
    fn call(&self, callee: u32, occurrence: usize) -> (&SnapshotInfo, &SnapshotInfo) {
        let entry = self
            .index
            .iter()
            .filter(|s| s.callee == callee && s.stage == "entry")
            .nth(occurrence)
            .unwrap_or_else(|| panic!("{callee:08x}: no entry #{occurrence}"));
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
            .unwrap_or_else(|| panic!("{callee:08x}: no return"));
        (entry, exit)
    }
    /// First snapshot of `callee` at `stage` after sequence `after`.
    fn next(&self, callee: u32, stage: &str, after: u64) -> &SnapshotInfo {
        self.index
            .iter()
            .find(|s| s.callee == callee && s.stage == stage && s.sequence > after)
            .unwrap_or_else(|| panic!("{callee:08x} {stage} after {after}"))
    }
    fn pcs(&self, pc: u32) -> Vec<&SnapshotInfo> {
        self.index
            .iter()
            .filter(|s| s.callee == pc && s.stage == "pc")
            .collect()
    }
}

fn word(snapshot: &Snapshot, address: u32) -> u32 {
    let i = (address & 0x1f_ffff) as usize;
    u32::from_le_bytes(snapshot.ram[i..i + 4].try_into().unwrap())
}

fn ranges(addresses: &[u32]) -> String {
    let mut out = Vec::new();
    let mut i = 0;
    while i < addresses.len() && out.len() < 48 {
        let start = addresses[i];
        let mut end = start;
        while i + 1 < addresses.len() && addresses[i + 1] == end + 1 {
            i += 1;
            end += 1;
        }
        out.push(format!("{start:08x}+{}", end - start + 1));
        i += 1;
    }
    out.join(" ")
}

/// Require `actual` to equal `expected` outside the stack, without faults.
fn verify(actual: &mut Machine, expected: &Snapshot) -> Result<(), String> {
    actual.take_fault().map_err(|fault| fault.to_string())?;
    let differences = snapshot::compare(actual, expected);
    if differences.is_empty() {
        return Ok(());
    }
    Err(format!(
        "ram {} [{}] scratch {} [{}] gte {:?}/{:?}",
        differences.ram.len(),
        ranges(&differences.ram),
        differences.scratch.len(),
        ranges(&differences.scratch),
        differences.gte_data,
        differences.gte_control
    ))
}

struct Outcome {
    /// Bytes outside the stack the original changed.
    changed: usize,
    host_calls: Vec<HostCall>,
}

/// Run `native` on a captured call with the original's device `results`
/// and compare with its return (RAM, scratchpad, GTE, v0).
fn check(
    capture: &Capture,
    callee: u32,
    occurrence: usize,
    results: &[u32],
    native: impl FnOnce(&mut Machine, &Snapshot) -> Option<u32>,
) -> Outcome {
    let (entry, exit) = capture.call(callee, occurrence);
    let entry = capture.load(entry);
    let exit = capture.load(exit);
    let mut m = entry.machine();
    m.host_results.extend(results.iter().copied());
    let v0 = native(&mut m, &entry);
    verify(&mut m, &exit).unwrap_or_else(|e| panic!("{callee:08x} #{occurrence}: {e}"));
    if let Some(v0) = v0 {
        assert_eq!(v0, exit.gpr(2), "{callee:08x} #{occurrence}: v0");
    }
    assert!(
        m.host_results.is_empty(),
        "{callee:08x}: unused host results"
    );
    let changed = snapshot::changed_bytes(&entry, &exit);
    println!(
        "{callee:08x} #{occurrence} frame {}: {changed} bytes",
        entry.info.frame
    );
    Outcome {
        changed,
        host_calls: m.host_calls,
    }
}

/// The 24 key states SpuGetAllKeysStatus stored (its return snapshot's
/// output buffer, a0 at entry).
fn key_states(capture: &Capture, after: u64) -> Vec<u32> {
    let entry = capture.next(0x8009_fcdc, "entry", after);
    let exit = capture.next(0x8009_fcdc, "return", entry.sequence);
    let buffer = entry.registers.gpr[4];
    let exit = capture.load(exit);
    (0..24)
        .map(|i| u32::from(exit.ram[((buffer + i) & 0x1f_ffff) as usize]))
        .collect()
}

/// The interrupt mask PutDrawEnv/DrawOTag saved (SetIntrMask results).
fn submit_masks(exit: &Snapshot) -> [u32; 4] {
    let mask = word(exit, sdk::GPU_SAVED_MASK);
    [mask, 0, mask, 0]
}

/// RAM owned by interrupt handlers that run while an iteration waits for
/// VBlank: BIOS kernel interrupt context, libetc's VBlank counter
/// (INTR_VB_OBJ_58), libpad's VSync receive (PADMAIN and its port state
/// machines: variables 0x800B2504..0x800B25B0 including the new-data flag,
/// port work areas 0x800C59D0..0x800C5C78, and the receive buffers at
/// `[gp+0x10]`).
fn interrupt_ranges(pads: u32) -> [(u32, u32); 5] {
    [
        (0x8000_e200, 0x8000_e290),
        (sdk::VSYNC_COUNTER, sdk::VSYNC_COUNTER + 4),
        (0x800b_2504, 0x800b_25b0),
        (0x800c_59d0, 0x800c_5c78),
        (pads, pads + 0x44),
    ]
}

/// Applies the original's interrupt-owned bytes at the iteration's
/// `VSync(n)` wait (where its VBlank handlers ran).
struct VBlank {
    bytes: Vec<(usize, u8)>,
    applied: bool,
}

impl Device for VBlank {
    fn call(&mut self, ram: &mut [u8], _: &mut [u8], call: HostCall) -> Option<u32> {
        if call.function == 0x800a_0568 && (call.args[0] as i32) >= 0 {
            for &(i, value) in &self.bytes {
                ram[i] = value;
            }
            self.applied = true;
        }
        None
    }
}

/// A state-callback substitute: checks the target and a0/a1 of the
/// original jalr (pc snapshot 80025EFC), that the machine equals the
/// original there, then takes the original callback's result state (pc
/// snapshot 80025F04) and v0. The attached device is kept.
fn callback<'a>(
    capture: &'a Capture,
    entry: &'a SnapshotInfo,
    exit: &'a SnapshotInfo,
    calls: &'a mut usize,
) -> impl FnMut(&mut Machine, u32, [u32; 4]) -> u32 + 'a {
    move |m, target, args| {
        *calls += 1;
        let gpr = &entry.registers.gpr;
        assert_eq!(target, gpr[2], "callback address");
        assert_eq!([args[0], args[1]], [gpr[4], gpr[5]], "callback a0/a1");
        let before = capture.load(entry);
        verify(m, &before).unwrap_or_else(|e| panic!("before the callback: {e}"));
        let after = capture.load(exit);
        let host_calls = std::mem::take(&mut m.host_calls);
        let host_results = std::mem::take(&mut m.host_results);
        let device = m.device.take();
        let sp = m.sp;
        *m = after.machine();
        m.host_calls = host_calls;
        m.host_results = host_results;
        m.device = device;
        m.sp = sp;
        after.gpr(2)
    }
}

const MAINLOOP: &str = "mainloop-001";
const PAUSE: &str = "mainloop-002";

// ---- snapshot tests ---------------------------------------------------------------------

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/mainloop-001"]
fn display_requests_80025720() {
    let capture = capture(MAINLOOP);
    let mut changed = 0;
    for occurrence in 0..3 {
        let outcome = check(&capture, 0x8002_5720, occurrence, &[], |m, _| {
            mainloop::display_requests(m);
            None
        });
        changed += outcome.changed;
    }
    assert!(changed > 0, "a display request was processed");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/mainloop-001"]
fn swap_buffers_80025bb0() {
    let capture = capture(MAINLOOP);
    for occurrence in 0..3 {
        let (_, exit) = capture.call(0x8002_5bb0, occurrence);
        // ClearOTagR's DMA poll count (its timeout poll word afterwards).
        let polls = word(&capture.load(exit), sdk::GPU_TIMEOUT_POLLS);
        let outcome = check(&capture, 0x8002_5bb0, occurrence, &[polls], |m, _| {
            Some(mainloop::swap_buffers(m))
        });
        assert!(outcome.changed > 0);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/mainloop-001"]
fn poll_pads_800269e4() {
    let capture = capture(MAINLOOP);
    for occurrence in 0..3 {
        let outcome = check(&capture, 0x8002_69e4, occurrence, &[], |m, _| {
            mainloop::poll_pads(m);
            None
        });
        assert!(outcome.changed > 0);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/mainloop-001"]
fn pad_update_80026704() {
    let capture = capture(MAINLOOP);
    for occurrence in 0..2 {
        check(&capture, 0x8002_6704, occurrence, &[], |m, entry| {
            mainloop::pad_update(m, entry.gpr(4), entry.gpr(5));
            None
        });
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/mainloop-001"]
fn map_pad_input_80028784() {
    let capture = capture(MAINLOOP);
    for occurrence in 0..3 {
        check(&capture, 0x8002_8784, occurrence, &[], |m, entry| {
            mainloop::map_pad_input(m, entry.gpr(4), entry.gpr(5), entry.gpr(6));
            None
        });
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/mainloop-001"]
fn mix_vibration_80029314() {
    let capture = capture(MAINLOOP);
    for occurrence in 0..2 {
        check(&capture, 0x8002_9314, occurrence, &[], |m, entry| {
            mainloop::mix_vibration(m, entry.gpr(4));
            None
        });
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/mainloop-001"]
fn update_sound_8002ddcc() {
    let capture = capture(MAINLOOP);
    for occurrence in 0..3 {
        let (entry, _) = capture.call(0x8002_ddcc, occurrence);
        let states = key_states(&capture, entry.sequence);
        let outcome = check(&capture, 0x8002_ddcc, occurrence, &states, |m, _| {
            mainloop::update_sound(m);
            None
        });
        assert!(outcome.changed > 0);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/mainloop-001"]
fn voice_attributes_8002dca8_8002db70() {
    // The SpuVoiceAttr each builds in its stack frame (sp - 0x58 + 0x10,
    // 0x40 bytes) is what the host reads at SpuSetVoiceAttr: it must equal
    // the original's frame bytes too (the comparison otherwise skips the
    // stack).
    let capture = capture(MAINLOOP);
    for occurrence in 0..2 {
        for (callee, key_on) in [(0x8002_dca8, true), (0x8002_db70, false)] {
            let (entry, exit) = capture.call(callee, occurrence);
            let attr = entry.registers.gpr[29] - 0x58 + 0x10;
            let exit = capture.load(exit);
            let mut seen = Vec::new();
            check(&capture, callee, occurrence, &[], |m, entry| {
                if key_on {
                    mainloop::key_on_voice(m, entry.gpr(4));
                } else {
                    mainloop::update_voice_volume(m, entry.gpr(4));
                }
                seen = m.read_bytes(attr, 0x40);
                None
            });
            let i = (attr & 0x1f_ffff) as usize;
            assert_eq!(
                seen,
                exit.ram[i..i + 0x40],
                "{callee:08x} #{occurrence} attr"
            );
        }
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/mainloop-001"]
fn dispatch_state_80025eb0() {
    let capture = capture(MAINLOOP);
    for occurrence in 0..2 {
        let (entry, _) = capture.call(0x8002_5eb0, occurrence);
        let before = capture.next(0x8002_5efc, "pc", entry.sequence);
        let after = capture.next(0x8002_5f04, "pc", before.sequence);
        let mut calls = 0;
        let mut invoke = callback(&capture, before, after, &mut calls);
        check(&capture, 0x8002_5eb0, occurrence, &[], |m, _| {
            Some(mainloop::dispatch_state_with(m, &mut invoke))
        });
        drop(invoke);
        assert_eq!(calls, 1);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/mainloop-001"]
fn process_cd_queue_8002c0b4() {
    let capture = capture(MAINLOOP);
    for occurrence in 0..3 {
        let outcome = check(&capture, 0x8002_c0b4, occurrence, &[], |m, _| {
            mainloop::process_cd_queue(m);
            None
        });
        assert!(outcome.changed > 0, "CdSync timeout words");
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/mainloop-001"]
fn link_sprites_80027408() {
    let capture = capture(MAINLOOP);
    for occurrence in 0..3 {
        let outcome = check(&capture, 0x8002_7408, occurrence, &[], |m, entry| {
            Some(mainloop::link_sprites(m, entry.gpr(4), entry.gpr(5)))
        });
        assert!(outcome.changed > 0);
    }
}

fn frame_end_checks(capture: &Capture, occurrences: usize) {
    for occurrence in 0..occurrences {
        let (_, exit) = capture.call(0x8002_5c20, occurrence);
        let masks = submit_masks(&capture.load(exit));
        let outcome = check(capture, 0x8002_5c20, occurrence, &masks, |m, _| {
            Some(mainloop::frame_end(m))
        });
        assert!(outcome.changed > 0);
        let submitted = outcome
            .host_calls
            .iter()
            .filter(|c| c.function == 0x800a_8360)
            .count();
        assert_eq!(submitted, 2, "DR_ENV packet and ordering table");
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/mainloop-001"]
fn frame_end_80025c20() {
    frame_end_checks(&capture(MAINLOOP), 3);
}

/// Run every consecutive loop-top pair (one iteration, two fields apart)
/// of `name` with the state callback substituted; returns the iterations
/// checked and the bytes the original changed.
fn iterations(name: &str) -> (usize, usize) {
    let capture = capture(name);
    let tops = capture.pcs(0x8002_5d30);
    let (mut checked, mut changed) = (0, 0);
    for pair in tops.windows(2) {
        let (top, next) = (pair[0], pair[1]);
        if next.frame != top.frame + 2 {
            continue;
        }
        // The state callback's entry and return inside this iteration.
        let inside = |pc: u32, after: u64| {
            capture
                .index
                .iter()
                .find(|s| s.callee == pc && s.stage == "pc" && s.sequence > after)
                .filter(|s| s.sequence < next.sequence)
        };
        let Some(before) = inside(0x8002_5efc, top.sequence) else {
            continue;
        };
        let Some(after) = inside(0x8002_5f04, before.sequence) else {
            continue;
        };
        let start = capture.load(top);
        let end = capture.load(next);
        let entry = capture.load(before);
        let context = word(&end, ac3_flight::GP + 0x2dc);
        // Device results in call order: ClearOTagR polls (its poll word at
        // the callback), the key states, GetRCnt, DrawSync polls (any count
        // below the timeout: DrawOTag re-arms the poll word), GetRCnt,
        // VSync(n) (counter at entry, VBlank count and counter after the
        // wait), PutDrawEnv and DrawOTag SetIntrMask results.
        let mut results = vec![word(&entry, sdk::GPU_TIMEOUT_POLLS)];
        results.extend(key_states(&capture, top.sequence));
        results.extend([
            word(&end, context + 0x48),
            0,
            word(&end, context + 0x4c),
            word(&start, sdk::VSYNC_LAST_HCOUNT),
            word(&end, sdk::VSYNC_LAST_COUNT),
            word(&end, sdk::VSYNC_LAST_HCOUNT),
        ]);
        results.extend(submit_masks(&end));
        let pads = word(&start, ac3_flight::GP + 0x10);
        let bytes = interrupt_ranges(pads)
            .into_iter()
            .flat_map(|(from, to)| (from..to).map(|a| (a & 0x1f_ffff) as usize))
            .map(|i| (i, end.ram[i]))
            .collect();
        let mut m = start.machine();
        m.host_results.extend(results);
        m.device = Some(Box::new(VBlank {
            bytes,
            applied: false,
        }));
        let mut calls = 0;
        {
            let mut invoke = callback(&capture, before, after, &mut calls);
            mainloop::main_loop_iteration_with(&mut m, &mut invoke);
        }
        assert_eq!(calls, 1);
        assert!(m.host_results.is_empty(), "unused host results");
        m.device = None;
        verify(&mut m, &end).unwrap_or_else(|e| panic!("{name} frame {}: {e}", top.frame));
        let bytes = snapshot::changed_bytes(&start, &end);
        println!("{name} frame {}: {bytes} bytes changed", top.frame);
        checked += 1;
        changed += bytes;
    }
    (checked, changed)
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/mainloop-001"]
fn main_loop_iteration_80025d30() {
    let (checked, changed) = iterations(MAINLOOP);
    assert_eq!(checked, 2);
    assert!(changed > 10_000);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/mainloop-002"]
fn main_loop_iterations_input_and_pause() {
    let (checked, changed) = iterations(PAUSE);
    assert!(checked >= 28, "{checked} iterations");
    assert!(changed > 100_000);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/mainloop-002"]
fn routines_on_input_and_pause() {
    let capture = capture(PAUSE);
    for occurrence in 0..12 {
        check(&capture, 0x8002_8784, occurrence, &[], |m, entry| {
            mainloop::map_pad_input(m, entry.gpr(4), entry.gpr(5), entry.gpr(6));
            None
        });
        let (entry, _) = capture.call(0x8002_ddcc, occurrence);
        let states = key_states(&capture, entry.sequence);
        check(&capture, 0x8002_ddcc, occurrence, &states, |m, _| {
            mainloop::update_sound(m);
            None
        });
    }
    frame_end_checks(&capture, 12);
    for occurrence in 0..6 {
        check(&capture, 0x8002_69e4, occurrence, &[], |m, _| {
            mainloop::poll_pads(m);
            None
        });
    }
}

// ---- unit tests -------------------------------------------------------------------------
// Paths flight frames do not reach, on synthetic machines. Expected values
// are derived from the original instructions by hand.

const GP: u32 = ac3_flight::GP;
const CONTEXT: u32 = 0x800b_d158;
const STATUS: u32 = 0x800b_b0a8;
const BUFFERS: u32 = 0x800b_b130;
const ENVS: u32 = 0x800b_af58;
const PADS: u32 = 0x8010_0000;
const VOICES: u32 = 0x800c_35f8;
const SOUNDS: u32 = 0x800c_37d8;
const MONITOR: u32 = 0x8010_4000;
const STREAM: u32 = 0x8010_4100;
const QUEUE: u32 = 0x8010_4200;

fn machine() -> Machine {
    let mut m = Machine::default();
    for (offset, value) in [
        (0x2dc, CONTEXT),
        (0x2c4, STATUS),
        (0x2d4, BUFFERS),
        (0x10, PADS),
        (0x24, 0x8010_3800),
        (0x30, 0x8010_3900),
        (0x34, MONITOR),
        (0x38, STREAM),
        (0x3c, 0x8010_4180),
        (0x48, QUEUE),
    ] {
        m.sw(GP + offset, value);
    }
    // libgpu's low-level table (the standard routines) and a complete CD
    // command (libcd's status byte, set by its interrupt handler).
    m.sw(0x800b_97dc, 0x8010_8800);
    for (offset, routine) in [
        (8, 0x800a_83fc),
        (0x10, 0x800a_82e8),
        (0x18, 0x800a_8360),
        (0x2c, 0x800a_7b1c),
        (0x3c, 0x800a_8a5c),
    ] {
        m.sw(0x8010_8800 + offset, routine);
    }
    m.sb(sdk::CD_SYNC_STATUS, 2);
    // libpad's port-record and busy-test function pointers, libcd's
    // command tables.
    m.sw(0x800b_2518, 0x8009_c8dc);
    m.sw(0x800b_2520, 0x8009_cd88);
    libcd_tables(&mut m);
    m
}

/// libcd's per-command tables (resident data): Setloc first for commands
/// with a position (0x800B4054), a second response follows the acknowledge
/// (0x800B41A0), the command clears the ready status (0x800B4220), the
/// acknowledge updates the status (0x800B42A0), parameter bytes
/// (0x800B4320).
fn libcd_tables(m: &mut Machine) {
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

/// libpad's record of port 0x10 (the second record, 0x800C5B80).
const PORT: u32 = pending_sdk::PAD_PORTS + 0xf0;

/// Port 0x10's libpad record: `state`, controller and extended IDs, a mode
/// table and 5-byte actuator records, idle and taking requests.
fn pad_port(
    m: &mut Machine,
    state: u8,
    id: u8,
    extended: u16,
    modes: &[u16],
    actuators: &[[u8; 5]],
) {
    m.sb(PORT + 0x49, state);
    m.sw(PORT + 0x10, PORT);
    m.sw(PORT + 0x30, 0x8010_5200);
    m.sb(PORT + 0xe8, id);
    m.sh(PORT + 0xe6, extended);
    m.sb(PORT + 0xe3, modes.len() as u8);
    m.sw(PORT, 0x8010_5000);
    for (i, &mode) in modes.iter().enumerate() {
        m.sh(0x8010_5000 + i as u32 * 2, mode);
    }
    m.sb(PORT + 0xe9, actuators.len() as u8);
    m.sw(PORT + 4, 0x8010_5100);
    for (i, record) in actuators.iter().enumerate() {
        m.write_bytes(0x8010_5100 + i as u32 * 5, record);
    }
    m.sb(PORT + 0x46, 0xff);
}

/// A disc without readable sectors (drive commands only).
struct NoDisc;
impl Disc for NoDisc {
    fn sector(&mut self, _: u32) -> Option<[u8; SECTOR_BYTES]> {
        None
    }
}

fn functions(m: &Machine) -> Vec<u32> {
    m.host_calls.iter().map(|c| c.function).collect()
}

fn no_fault(m: &mut Machine) {
    assert_eq!(m.take_fault(), Ok(()));
}

#[test]
fn pad_mode_index_table() {
    let expected = [0xffff_ffff, 3, 2, 5, 1, 4, 7, 6, 0xffff_ffff];
    for (id, value) in expected.into_iter().enumerate() {
        assert_eq!(mainloop::pad_mode_index(id as u32), value);
    }
}

#[test]
fn display_requests_every_bit_and_phase() {
    let mut m = machine();
    m.sw(STATUS, 0x7f);
    m.sw(STATUS + 4, 1);
    m.sh(STATUS + 8, 0x12);
    m.sh(STATUS + 0xc, 0x34);
    for (i, value) in [1, 2, 3].into_iter().enumerate() {
        m.sb(STATUS + 0x10 + i as u32, value);
    }
    mainloop::display_requests(&mut m);
    // 0x40, 1, 0x10, 0x20, 2, 4, 8 in order: 0x10/0x20 re-request bit 0 and
    // bit 0 requests 0x40 for the next frame.
    assert_eq!(m.lw(STATUS), 0x41);
    assert_eq!(m.lw(BUFFERS), STATUS + 0x2c);
    assert_eq!(m.lw(BUFFERS + 0x1014), STATUS + 0x2c);
    assert_eq!(m.lw(BUFFERS + 4), STATUS + 0x18);
    assert_eq!(m.lw(BUFFERS + 0x1018), STATUS + 0x18);
    for display in [0x114, 0x128, 0x13c] {
        assert_eq!(
            [m.lhu(ENVS + display + 8), m.lhu(ENVS + display + 0xa)],
            [0x12, 0x34]
        );
    }
    for draw in [0, 0x5c, 0xb8] {
        let bytes: Vec<u8> = (0x18..0x1c).map(|o| m.lbu(ENVS + draw + o)).collect();
        assert_eq!(bytes, [1, 1, 2, 3]);
    }
    m.sw(STATUS + 4, 0);
    mainloop::display_requests(&mut m);
    assert_eq!(m.lw(STATUS), 0x40);
    assert_eq!(
        [m.lw(BUFFERS + 4), m.lw(BUFFERS + 0x1018)],
        [ENVS + 0x114, ENVS + 0x128]
    );
    assert_eq!([m.lw(BUFFERS), m.lw(BUFFERS + 0x1014)], [ENVS, ENVS + 0x5c]);
    m.sw(STATUS + 4, 1);
    m.sw(STATUS, 0x41);
    mainloop::display_requests(&mut m);
    assert_eq!(
        [m.lw(BUFFERS + 4), m.lw(BUFFERS + 0x1018)],
        [ENVS + 0x13c, ENVS + 0x13c]
    );
    assert_eq!(
        [m.lw(BUFFERS), m.lw(BUFFERS + 0x1014)],
        [ENVS + 0xb8, ENVS + 0xb8]
    );
    // Other phases change no pointer; the bits are still consumed.
    m.sw(STATUS + 4, 2);
    m.sw(STATUS, 0x40 | 4);
    mainloop::display_requests(&mut m);
    assert_eq!(m.lw(STATUS), 0);
    assert_eq!(m.lw(BUFFERS + 4), ENVS + 0x13c);
    for draw in [0, 0x5c, 0xb8] {
        assert_eq!(m.lbu(ENVS + draw + 0x18), 0);
    }
    no_fault(&mut m);
}

/// State table: state s has callbacks 0x80200000 + s*0x100 + substate,
/// module s (state 3: -2), load address 0x80110000 + s, limit
/// 0x80120000 + s*0x100.
fn dispatch_machine() -> Machine {
    let mut m = machine();
    for state in 0..4u32 {
        let record = 0x800a_e544 + state * 16;
        let table = 0x8010_5000 + state * 0x20;
        m.sw(record, table);
        m.sw(record + 4, if state == 3 { 0xffff_fffe } else { state });
        m.sw(record + 8, 0x8011_0000 + state);
        m.sw(record + 0xc, 0x8012_0000 + state * 0x100);
        for substate in 0..8 {
            m.sw(table + substate * 4, 0x8020_0000 + state * 0x100 + substate);
        }
    }
    m.sw(0x800b_24f4, 0x8009_9bc8);
    m.sw(0x800b_24f8, 0x8009_9cd8);
    for (offset, value) in [
        (0x2c, 1),
        (0x30, 2),
        (0x34, 1),
        (0x38, 1),
        (0x3c, 0x8011_0001),
        (0x40, 0x8012_0100),
        (0x18, 0x8013_0000),
        (0x1c, 0x8013_0040),
    ] {
        m.sw(CONTEXT + offset, value);
    }
    m
}

fn run_dispatch(m: &mut Machine, result: u32) -> (u32, [u32; 4]) {
    let mut seen = (0, [0; 4]);
    {
        let mut invoke = |_: &mut Machine, target: u32, args: [u32; 4]| {
            seen = (target, args);
            result
        };
        assert_eq!(mainloop::dispatch_state_with(m, &mut invoke), 0);
    }
    seen
}

fn context_words(m: &mut Machine) -> [u32; 9] {
    [0, 0x18, 0x1c, 0x2c, 0x30, 0x34, 0x38, 0x3c, 0x40].map(|o| m.lw(CONTEXT + o))
}

#[test]
fn dispatcher_transitions() {
    use mainloop::transition::*;
    let mut m = dispatch_machine();
    let before = context_words(&mut m);
    assert_eq!(run_dispatch(&mut m, 0), (0x8020_0102, [1, CONTEXT, 0, 0]));
    assert_eq!(context_words(&mut m), before, "result 0 changes nothing");
    assert_eq!(run_dispatch(&mut m, LOAD_COMPLETE).0, 0x8020_0102);
    assert_eq!(
        context_words(&mut m),
        before,
        "LOAD_COMPLETE outside loading"
    );

    let base = [0, 0x8013_0000, 0x8013_0040];
    let cases: [(u32, [u32; 6]); 6] = [
        (
            SET_STATE | 0x103,
            [3, 0, 0xffff_fffe, 0xffff_ffff, 0x8011_0003, 0x8012_0300],
        ),
        (NEXT_STATE, [2, 0, 2, 1, 0x8011_0002, 0x8012_0200]),
        (SET_SUBSTATE | 0x107, [1, 7, 1, 1, 0x8011_0001, 0x8012_0100]),
        (NEXT_SUBSTATE, [1, 3, 1, 1, 0x8011_0001, 0x8012_0100]),
        (RESET_ARENA, [1, 2, 1, 1, 0x8011_0001, 0x8012_0100]),
        // Priority SET_STATE > NEXT_STATE > SET_SUBSTATE > NEXT_SUBSTATE.
        (
            SET_STATE | NEXT_STATE | SET_SUBSTATE | NEXT_SUBSTATE | RESET_ARENA | 2,
            [2, 0, 2, 1, 0x8011_0002, 0x8012_0200],
        ),
    ];
    for (result, fields) in cases {
        let mut m = dispatch_machine();
        run_dispatch(&mut m, result);
        let words = context_words(&mut m);
        assert_eq!(words[3..], fields, "result {result:#x}");
        if result & RESET_ARENA != 0 {
            // The arena resets to the (new) limit and DrawSync is requested.
            assert_eq!(words[..3], [8, fields[5], fields[5]], "result {result:#x}");
        } else {
            assert_eq!(words[..3], base, "result {result:#x}");
        }
    }

    // Loading path: module 2 requested, module 1 loaded, substate 1.
    let mut m = dispatch_machine();
    m.sw(CONTEXT + 0x34, 2);
    m.sw(CONTEXT + 0x30, 1);
    let seen = run_dispatch(&mut m, LOAD_COMPLETE | NEXT_SUBSTATE);
    assert_eq!(seen, (0x8009_9cd8, [2, 0x8011_0001, 0, 0]));
    assert_eq!(m.lw(CONTEXT + 0x38), 2, "module loaded");
    assert_eq!(m.lw(CONTEXT + 0x30), 1, "substate reset, then advanced");
    let mut m = dispatch_machine();
    m.sw(CONTEXT + 0x34, 2);
    m.sw(CONTEXT + 0x30, 0);
    assert_eq!(run_dispatch(&mut m, 0).0, 0x8009_9bc8);
    assert_eq!(m.lw(CONTEXT + 0x38), 1);
    no_fault(&mut m);
}

/// A machine on which a whole loop iteration runs on zeroed data: no pads,
/// no voices playing, an idle CD monitor and queue.
fn loop_machine() -> Machine {
    let mut m = machine();
    m.write_bytes(PADS, &mainloop::no_pad());
    m.write_bytes(PADS + 0x22, &mainloop::no_pad());
    for record in 0..8 {
        m.sw(PADS + 0x44 + record * 0x10 + 4, 0x8010_1000 + record * 0x20);
    }
    m.sw(CONTEXT + 0x14, 0x8010_6000);
    m.sw(STATUS + 0x14, 2);
    for buffer in [BUFFERS, BUFFERS + 0x1014] {
        m.sw(buffer, 0x8010_8000);
        m.sw(buffer + 4, 0x8010_9000);
    }
    for voice in 0..24 {
        // Free voices at full level: no volume update.
        m.sb(VOICES + voice * 0x14, 0xff);
        m.sb(VOICES + voice * 0x14 + 0x10, 0x80);
        m.sb(VOICES + voice * 0x14 + 0x11, 0x80);
    }
    m.sb(0x800c_4338 + 1, 0xff);
    m.sh(0x800c_4338 + 2, 0xffff);
    for slot in 0..8 {
        m.sh(0x800c_4318 + slot * 4 + 2, 0xffff);
    }
    m
}

fn run_iteration(m: &mut Machine, results: &[u32]) -> usize {
    m.host_results.extend(results.iter().copied());
    let mut calls = 0;
    {
        let mut invoke = |_: &mut Machine, target: u32, _: [u32; 4]| {
            assert_eq!(target, 0);
            calls += 1;
            0
        };
        mainloop::main_loop_iteration_with(m, &mut invoke);
    }
    no_fault(m);
    calls
}

#[test]
fn iteration_device_order_and_flags() {
    use mainloop::flags::*;
    let mut m = loop_machine();
    m.sw(CONTEXT, PAUSE_REQUEST | DRAW_SYNC);
    m.sw(sdk::VSYNC_COUNTER, 333);
    // ClearOTagR polls, key states, GetRCnt, DrawSync polls, GetRCnt,
    // VSync(2) (counter at entry, VBlank count and counter after), the
    // SetIntrMask pairs of PutDrawEnv and DrawOTag, DrawSync polls.
    let mut results = vec![0];
    results.extend([0; 24]);
    results.extend([111, 0, 222, 0, 334, 0, 0, 0, 0, 0, 0]);
    assert_eq!(run_iteration(&mut m, &results), 1);
    assert!(m.host_results.is_empty());
    assert_eq!(m.lw(CONTEXT), PAUSED, "pause request taken, DrawSync done");
    assert_eq!(
        [
            m.lw(CONTEXT + 0x48),
            m.lw(CONTEXT + 0x4c),
            m.lw(CONTEXT + 0x20)
        ],
        [111, 222, 333]
    );
    let timing = [
        0x800a_d934,
        0x800a_8a5c,
        0x800a_0568,
        0x800a_d9d0,
        0x800a_8360,
    ];
    let order: Vec<(u32, u32)> = m
        .host_calls
        .iter()
        .map(|c| (c.function, c.args[0]))
        .filter(|(f, _)| timing.contains(f))
        .collect();
    assert_eq!(
        order,
        [
            (0x800a_d934, 0xf200_0001),
            (0x800a_8a5c, 0),
            (0x800a_d934, 0xf200_0001),
            (0x800a_0568, 2),
            (0x800a_d9d0, 0xf200_0001),
            (0x800a_8360, 0x8010_8000 + 0x1c),
            (0x800a_8360, BUFFERS + 0x1014 + 0x10),
            (0x800a_8a5c, 0),
        ]
    );
    assert_eq!(m.lw(CONTEXT + 0x24), 1);
    assert_eq!(
        m.lw(CONTEXT + 0x28),
        1,
        "the pause starts after this frame end"
    );
    // Paused frame: no +0x28 count, master volume 0.
    run_iteration(&mut m, &[]);
    assert_eq!([m.lw(CONTEXT + 0x24), m.lw(CONTEXT + 0x28)], [2, 1]);
    assert_eq!(m.lhu(0x800c_35c8 + 4), 0);
    // Resume.
    m.sw(CONTEXT, PAUSED | RESUME_REQUEST);
    run_iteration(&mut m, &[]);
    assert_eq!(m.lw(CONTEXT), 0);
    assert_eq!(
        m.lhu(0x800c_35c8 + 4),
        0,
        "paused at the start of this frame"
    );
    // The lock discards pause/resume requests and the pause state.
    m.sw(
        CONTEXT,
        PAUSE_LOCK | PAUSED | PAUSE_REQUEST | RESUME_REQUEST | SKIP_COUNT,
    );
    run_iteration(&mut m, &[]);
    assert_eq!(
        m.lw(CONTEXT),
        PAUSE_LOCK | SKIP_COUNT,
        "a paused frame keeps SKIP_COUNT"
    );
    let counted = m.lw(CONTEXT + 0x28);
    run_iteration(&mut m, &[]);
    assert_eq!(m.lw(CONTEXT), PAUSE_LOCK);
    assert_eq!(m.lw(CONTEXT + 0x28), counted, "skipped count");
    assert_eq!(m.lhu(0x800c_35c8 + 4), 0x3fff);
    // NO_DRAW: no DrawOTag.
    m.sw(CONTEXT, NO_DRAW);
    m.host_calls.clear();
    run_iteration(&mut m, &[]);
    let submitted: Vec<u32> = m
        .host_calls
        .iter()
        .filter(|c| c.function == 0x800a_8360)
        .map(|c| c.args[0])
        .collect();
    assert_eq!(submitted, [0x8010_8000 + 0x1c], "no DrawOTag");
}

fn pad_machine(kind: u8, raw: &[u8]) -> (Machine, u32, u32, u32) {
    let mut m = machine();
    let (record, state, buffer) = (0x8010_0044, 0x8010_1000, 0x8010_0000);
    m.sb(record, 0x10);
    m.sw(record + 4, state);
    m.sb(state + 0xc, kind);
    m.write_bytes(buffer, raw);
    (m, record, state, buffer)
}

fn pad_halves(m: &mut Machine, state: u32) -> [u16; 6] {
    [0xe, 0x10, 0x12, 0x14, 0x16, 0x18].map(|o| m.lhu(state + o))
}

#[test]
fn pad_update_device_types() {
    // DualShock analog: buttons active low, analog bytes 4..7.
    let (mut m, record, state, raw) =
        pad_machine(0x73, &[0, 0x73, 0xfe, 0xff, 0x11, 0x22, 0x33, 0x44]);
    mainloop::pad_update(&mut m, raw, record);
    assert_eq!(pad_halves(&mut m, state), [1, 1, 0x22, 0x11, 0x44, 0x33]);
    assert_eq!([m.lbu(state + 0xc), m.lbu(state + 0xd)], [0x73, 0]);
    mainloop::pad_update(&mut m, raw, record);
    assert_eq!(m.lhu(state + 0x10), 0, "held, not newly pressed");
    // Mouse: signed deltas accumulated.
    let (mut m, record, state, raw) = pad_machine(0x12, &[0, 0x12, 0xff, 0xff, 0xfd, 0x07]);
    m.sh(state + 0x16, 100);
    m.sh(state + 0x18, 5);
    mainloop::pad_update(&mut m, raw, record);
    assert_eq!(pad_halves(&mut m, state), [0, 0, 7, 0xfffd, 107, 2]);
    // Gun and jog.
    let (mut m, record, state, raw) =
        pad_machine(0x63, &[0, 0x63, 0xff, 0xff, 0x34, 0x12, 0x78, 0x56]);
    m.sh(state + 0x16, 9);
    mainloop::pad_update(&mut m, raw, record);
    assert_eq!(pad_halves(&mut m, state)[2..], [0x1234, 0x5678, 0, 0]);
    let (mut m, record, state, raw) = pad_machine(0xe3, &[0, 0xe3, 0xff, 0xff, 0x10, 0x10, 5, 0]);
    m.sh(state + 0x12, 0x1000);
    mainloop::pad_update(&mut m, raw, record);
    assert_eq!(pad_halves(&mut m, state)[2..], [0x1010, 5, 0x10, 0]);
    // Digital: analog fields cleared.
    let (mut m, record, state, raw) = pad_machine(0x41, &[0, 0x41, 0xff, 0xff]);
    m.sh(state + 0x12, 7);
    mainloop::pad_update(&mut m, raw, record);
    assert_eq!(pad_halves(&mut m, state)[2..], [0, 0, 0, 0]);
    // Removed: event 8 (1 if no type was known) and a cleared state.
    for (kind, event) in [(0x41, 8), (0, 1)] {
        let (mut m, record, state, raw) = pad_machine(kind, &[0xff, 0, 0, 0]);
        m.sb(record + 1, 2);
        m.write_bytes(state, &[9; 0x1a]);
        m.sb(state + 0xc, kind);
        mainloop::pad_update(&mut m, raw, record);
        assert_eq!(m.lbu(state + 0xd), event);
        assert_eq!(m.lbu(record + 1), 0);
        let cleared: Vec<u8> = (0..0x1a)
            .filter(|&o| o != 0xd)
            .map(|o| m.lbu(state + o))
            .collect();
        assert_eq!(cleared, [0; 0x19]);
        assert!(m.host_calls.is_empty());
    }
    // Mode change: libpad reports another mode, the type is reset.
    let (mut m, record, state, raw) = pad_machine(0x73, &[0, 0x73, 0xff, 0xff, 1, 2, 3, 4]);
    m.sh(state + 0xa, 7);
    pad_port(&mut m, 6, 7, 5, &[], &[]);
    mainloop::pad_update(&mut m, raw, record);
    assert!(m.host_calls.is_empty(), "libpad is RAM-exact");
    assert_eq!([m.lbu(state + 0xc), m.lbu(state + 0xd)], [0, 4]);
    assert_eq!([m.lbu(record + 1), m.lbu(record + 0xe)], [1, 1]);
    assert_eq!(pad_halves(&mut m, state)[2..], [0, 0, 0, 0]);
    // Detection: a new pad runs 800263BC; a disconnected state reports 2.
    let (mut m, record, state, raw) = pad_machine(0, &[0, 0x41, 0xfe, 0xff]);
    mainloop::pad_update(&mut m, raw, record);
    assert_eq!(m.lbu(state + 0xd), 2);
    assert!(m.host_calls.is_empty());
    no_fault(&mut m);
}

#[test]
fn poll_pads_single_and_multitap() {
    let mut m = machine();
    for record in 0..8 {
        m.sw(PADS + 0x44 + record * 0x10 + 4, 0x8010_1000 + record * 0x20);
        m.sb(0x8010_1000 + record * 0x20 + 0xc, 0x41);
    }
    let mut port1 = mainloop::digital_pad(0x0001);
    port1[8..10].copy_from_slice(&[0x12, 0x34]);
    // Port 2: multitap, sub-pads at +2/+10/+18/+26.
    let mut port2 = [0u8; 0x22];
    port2[1] = 0x80;
    for (i, buttons) in [0x10u16, 0x20, 0x40, 0x80].into_iter().enumerate() {
        let sub = 2 + i * 8;
        port2[sub + 1] = 0x41;
        port2[sub + 2..sub + 4].copy_from_slice(&(!buttons).to_le_bytes());
    }
    mainloop::write_pad_buffers(&mut m, &port1, &port2);
    assert_eq!(m.lw(mainloop::PAD_VSYNC_FLAG), 1);
    mainloop::poll_pads(&mut m);
    let held: Vec<u16> = (0..8)
        .map(|r| m.lhu(0x8010_1000 + r * 0x20 + 0xe))
        .collect();
    assert_eq!(held, [1, 0, 0, 0, 0x10, 0x20, 0x40, 0x80]);
    assert_eq!(
        m.lhu(PADS + 8),
        0xffff,
        "a single pad marks sub-buffer 2 empty"
    );
    assert_eq!(m.lbu(0x8010_1000 + 0x20 + 0xd), 8, "sub-pad 2 removed");
    // No new data (PadChkVsync cleared the flag): nothing changes.
    assert_eq!(m.lw(mainloop::PAD_VSYNC_FLAG), 0);
    m.sh(PADS + 2, 0);
    mainloop::poll_pads(&mut m);
    assert_eq!(m.lhu(0x8010_1000 + 0xe), 1);
    no_fault(&mut m);
}

fn setup_machine(phase: u8) -> (Machine, u32, u32) {
    let mut m = machine();
    let (record, state) = (0x8010_0044, 0x8010_1000);
    m.sb(record, 0x10);
    m.sb(record + 1, phase);
    m.sw(record + 4, state);
    m.sb(PADS + 0xcc, 1);
    // Mode records for 80026188 and the mode table for 80026260/800262B4.
    m.sw(0x800a_e5e4, 0x20);
    m.sw(0x800a_e5e8, 0x8010_2000);
    m.sw(0x800a_e5ec, 0x30);
    m.sw(0x800a_e5f0, 0x8010_2010);
    for (i, value) in [0x100u16, 0x300, 0].into_iter().enumerate() {
        m.sh(0x8010_2000 + i as u32 * 2, value);
    }
    m.sh(0x8010_2010, 0x300);
    m.sw(PADS + 0xc4, 2);
    m.sw(PADS + 0xc8, 0x8010_3000);
    m.write_bytes(0x8010_3000, &[0x10, 1, 2, 0x20, 3, 4]);
    // Actuator kinds for 80026350.
    m.write_bytes(0x800a_e5f4, &[1, 1, 1, 1, 2, 1, 2, 2, 4, 0, 0, 0]);
    (m, record, state)
}

#[test]
fn pad_setup_phases() {
    // Phase 0, pad found without extended modes: actuator bytes sent.
    let (mut m, record, state) = setup_machine(0);
    pad_port(&mut m, 2, 4, 9, &[], &[]);
    assert_eq!(mainloop::pad_setup(&mut m, record), 4);
    assert_eq!(
        [m.lbu(state), m.lbu(state + 1), m.lbu(record + 2)],
        [1, 1, 2]
    );
    assert_eq!(m.lhu(state + 0xa), 9);
    assert_eq!(
        [m.lw(PORT + 0x28), u32::from(m.lbu(PORT + 0x34))],
        [state + 2, 2]
    );
    // Phase 0, stable: read the mode table, select the main mode.
    let (mut m, record, state) = setup_machine(0);
    pad_port(&mut m, 6, 7, 1, &[0x100, 0x300, 0], &[]);
    m.sb(PORT + 0xe4, 3);
    assert_eq!(mainloop::pad_setup(&mut m, record), 0);
    assert_eq!(
        [0x46, 0x51, 0x52, 0x53].map(|o| m.lbu(PORT + o)),
        [1, 3, 4, 1],
        "PadSetMainMode(0x10, 3, 4) requested"
    );
    assert_eq!(
        [m.lw(PORT + 0x14), m.lw(PORT + 0x18)],
        [0x8009_c140, 0x8009_c194]
    );
    assert_eq!(
        [m.lbu(state), m.lbu(state + 1), m.lbu(record + 1)],
        [0x20, 0, 1]
    );
    // A port without extended modes refuses the request.
    let (mut m, record, _) = setup_machine(0);
    pad_port(&mut m, 6, 7, 0, &[0x100, 0x300, 0], &[]);
    mainloop::pad_setup(&mut m, record);
    assert_eq!(m.lbu(PORT + 0x46), 0xff);
    // Phase 1: actuator table, alignment.
    let (mut m, record, state) = setup_machine(1);
    pad_port(&mut m, 6, 7, 1, &[], &[[1, 1, 1, 0, 0], [1, 2, 2, 0, 0]]);
    assert_eq!(mainloop::pad_setup(&mut m, record), 0);
    let align: Vec<u8> = (8..0xe).map(|o| m.lbu(record + o)).collect();
    assert_eq!(align, [0, 1, 0xff, 0xff, 0xff, 0xff]);
    assert_eq!([m.lbu(state + 1), m.lbu(record + 1)], [3, 2]);
    assert_eq!(
        [m.lw(PORT + 0x14), m.lw(PORT + 0x18), m.lw(PORT + 0x20)],
        [0x8009_bfc4, 0x8009_bfe0, record + 8],
        "PadSetActAlign(0x10, record + 8)"
    );
    // Phase 2: start the actuators (three present).
    let (mut m, record, state) = setup_machine(2);
    m.sb(state + 1, 0b10_0101);
    pad_port(&mut m, 6, 7, 9, &[], &[]);
    assert_eq!(mainloop::pad_setup(&mut m, record), 7);
    assert_eq!(
        [m.lw(PORT + 0x28), u32::from(m.lbu(PORT + 0x34))],
        [state + 2, 3]
    );
    assert_eq!(m.lhu(state + 0xa), 9);
    // Not stable yet (with a request in flight state 6 reads as 4) /
    // disconnected / unknown phase.
    let (mut m, record, _) = setup_machine(2);
    pad_port(&mut m, 6, 7, 9, &[], &[]);
    m.sb(PORT + 0x37, 1);
    assert_eq!(pending_sdk::pad_get_state(&mut m, 0x10), 4);
    assert_eq!(mainloop::pad_setup(&mut m, record), 0);
    assert!(m.host_calls.is_empty());
    let (mut m, record, state) = setup_machine(1);
    m.write_bytes(state, &[5; 0xc]);
    assert_eq!(mainloop::pad_setup(&mut m, record), 0);
    assert_eq!(
        [m.lbu(state), m.lbu(state + 1), m.lbu(record + 1)],
        [0, 0, 0]
    );
    assert_eq!(m.lhu(state + 0xa), 0);
    let (mut m, record, _) = setup_machine(5);
    pad_port(&mut m, 6, 7, 9, &[], &[]);
    assert_eq!(mainloop::pad_setup(&mut m, record), 0);
    assert_eq!(m.lbu(record + 1), 0);
    no_fault(&mut m);
}

#[test]
fn libpad_queries() {
    use pending_sdk::*;
    let mut m = machine();
    pad_port(&mut m, 3, 0x73, 0x123, &[0x401, 0x702], &[[1, 2, 3, 4, 5]]);
    m.sb(PORT + 0xe4, 1);
    m.sw(PORT + 0x4c, 0xdead_beef);
    assert_eq!(pad_get_state(&mut m, 0x10), 3, "idle: the state itself");
    m.sw(PORT + 0x10, 0);
    m.sb(PORT + 0x39, 1);
    assert_eq!(
        pad_get_state(&mut m, 0x10),
        1,
        "sub-port request: 3 reads 1"
    );
    assert_eq!(
        [1, 2, 3].map(|term| pad_info_mode(&mut m, 0x10, term, 0)),
        [0x73, 0x123, 1]
    );
    assert_eq!(pad_info_mode(&mut m, 0x10, 4, u32::MAX), 2);
    assert_eq!(pad_info_mode(&mut m, 0x10, 4, 1), 0x702);
    assert_eq!(pad_info_mode(&mut m, 0x10, 4, 2), 0);
    assert_eq!(pad_info_mode(&mut m, 0x10, 0x64, 0), 0xdead_beef);
    assert_eq!(pad_info_mode(&mut m, 0x10, 5, 0), 0);
    assert_eq!(pad_info_act(&mut m, 0x10, u32::MAX, 0), 1);
    assert_eq!(
        [1, 2, 3, 4, 5, 6].map(|term| pad_info_act(&mut m, 0x10, 0, term)),
        [1, 2, 3, 4, 5, 0]
    );
    assert_eq!(pad_info_act(&mut m, 0x10, 1, 1), 0);
    assert_eq!(pad_port_record(0), PAD_PORTS);
    assert_eq!(pad_set_act(&mut m, 0x10, 0x8010_6000, 0x106), PORT);
    assert_eq!(m.lbu(PORT + 0x34), 6);
    // Busy: a request pending (+0x46 not 0xFF).
    m.sb(PORT + 0x46, 1);
    assert_eq!(pad_set_main_mode(&mut m, 0x10, 0, 0), 0);
    assert_eq!(pad_set_act_align(&mut m, 0x10, 0), 0);
    assert!(m.host_calls.is_empty());
    no_fault(&mut m);
}

fn input_machine(kind: u8) -> (Machine, u32, u32, u32) {
    let mut m = machine();
    let (pad, input, config) = (CONTEXT + 0x50, CONTEXT + 0x120, CONTEXT + 0x15c);
    m.sb(pad + 0xc, kind);
    (m, pad, input, config)
}

fn input_words(m: &mut Machine, input: u32) -> [u32; 5] {
    [4, 8, 0xc, 0x10, 0x14].map(|o| m.lw(input + o))
}

#[test]
fn input_mapping_digital_repeat_and_tap_hold() {
    let (mut m, pad, input, config) = input_machine(0x41);
    m.sh(config, 0x0010);
    m.sh(config + 21 * 2, 0x0040);
    m.sh(pad + 0xe, 0x0010);
    mainloop::map_pad_input(&mut m, pad, input, config);
    assert_eq!(input_words(&mut m, input), [1, 1, 1, 1, 0]);
    assert_eq!(m.lbu(input + 0x38), 0);
    mainloop::map_pad_input(&mut m, pad, input, config);
    assert_eq!(input_words(&mut m, input), [1, 1, 0, 0, 0]);
    assert_eq!(m.lbu(input + 0x38), 1);
    // Direction repeat: every 4th frame from count 0x17.
    m.sb(input + 0x38, 0x16);
    mainloop::map_pad_input(&mut m, pad, input, config);
    assert_eq!(m.lw(input + 0x10), 0, "count 0x17");
    m.sb(input + 0x38, 0x1b);
    mainloop::map_pad_input(&mut m, pad, input, config);
    assert_eq!(m.lw(input + 0x10), 1, "count 0x1C");
    // Release, with pad event 2 (bit 3).
    m.sh(pad + 0xe, 0);
    m.sb(pad + 0xd, 2);
    mainloop::map_pad_input(&mut m, pad, input, config);
    assert_eq!(input_words(&mut m, input), [1, 0, 0, 0, 1]);
    assert_eq!(m.lhu(input + 0x36), 8);
    // Tap/hold on bit 21 (repeat flags 0x10): a short press reports the bit
    // on release, a long one bit 0x2000 instead.
    m.sb(pad + 0xd, 0);
    m.sw(config + 0x58, 0x10);
    for _ in 0..3 {
        m.sh(pad + 0xe, 0x0040);
        mainloop::map_pad_input(&mut m, pad, input, config);
        assert_eq!(m.lw(input + 8), 0);
    }
    m.sh(pad + 0xe, 0);
    mainloop::map_pad_input(&mut m, pad, input, config);
    assert_eq!(m.lw(input + 8), 0x0020_0000);
    assert_eq!(m.lbu(input + 0x39), 0);
    m.sh(pad + 0xe, 0x0040);
    for frame in 1..=8 {
        mainloop::map_pad_input(&mut m, pad, input, config);
        let expected = if frame >= 7 { 0x2000 } else { 0 };
        assert_eq!(m.lw(input + 8), expected, "frame {frame}");
    }
    m.sh(pad + 0xe, 0);
    mainloop::map_pad_input(&mut m, pad, input, config);
    assert_eq!(m.lw(input + 8), 0, "no tap after a hold");
    no_fault(&mut m);
}

#[test]
fn input_mapping_analog_types() {
    // DualShock in mode 7 (+0xB8 table): calibrated axes and stick directions.
    let (mut m, pad, input, config) = input_machine(0x73);
    m.sh(pad + 0xa, 7);
    let table = config + 0xb8;
    for axis in 0..4 {
        m.write_bytes(table + 0x40 + axis * 4, &[0, 0x80, 0xff, 0x10]);
    }
    m.write_bytes(table + 0x52, &[0, 1, 2, 3, 0xff, 0xff]);
    for (i, value) in [0x80u16, 0xff, 0x00, 0xc0].into_iter().enumerate() {
        m.sh(pad + 0x12 + i as u32 * 2, value);
    }
    m.sw(input + 0x30, 99);
    mainloop::map_pad_input(&mut m, pad, input, config);
    assert_eq!(m.lw(input + 4), 4);
    assert_eq!(m.lw(input + 0x18), 0xf);
    let axes: Vec<i32> = (0..6).map(|i| m.lw(input + 0x1c + i * 4) as i32).collect();
    // 0x7F above centre: (0x6F << 13) / (0x6F << 1) - 1; 0x40 above:
    // (0x30 << 13) / 222 - 1; 0x80 below: -(0x70 << 13) / (0x70 << 1).
    assert_eq!(axes, [0, 0xfff, -0x1000, 1770, 0, 0]);
    assert_eq!(m.lw(input + 8) & 0xf, 1, "left stick x < -0xEAA");
    // Mode 0 DualShock uses +0x5C and the left stick, the analog joystick
    // +0x114 and the right stick (limit 0x600).
    for (kind, class, offset, directions) in [(0x73, 2, 0x5c, 5), (0x53, 8, 0x114, 2)] {
        let (mut m, pad, input, config) = input_machine(kind);
        m.write_bytes(config + offset + 0x52, &[0xff; 6]);
        for axis in 0..4 {
            m.write_bytes(config + offset + 0x40 + axis * 4, &[0, 0x80, 0xff, 0]);
        }
        m.sh(pad + 0x12, 0xff);
        m.sh(pad + 0x14, 0x80);
        mainloop::map_pad_input(&mut m, pad, input, config);
        assert_eq!(m.lw(input + 4), class);
        assert_eq!(m.lw(input + 8) & 0xf, directions, "type {kind:#x}");
    }
    // neGcon: buttons synthesized from the analog values; axis inversion.
    let (mut m, pad, input, config) = input_machine(0x23);
    let table = config + 0x170;
    for axis in 0..4 {
        m.write_bytes(table + 0x40 + axis * 4, &[0, 0x80, 0xff, 0]);
    }
    m.write_bytes(table + 0x52, &[0, 0xff, 0xff, 0xff, 0xff, 0xff]);
    m.sh(table + 0x50, 4);
    m.sh(table + 14 * 2, 0x4000);
    m.sh(table + 15 * 2, 0x0100);
    m.sh(pad + 0x12, 0xff);
    m.sh(pad + 0x16, 0xff);
    mainloop::map_pad_input(&mut m, pad, input, config);
    assert_eq!(m.lw(input + 4), 0x10);
    assert_eq!(m.lw(input + 8), 0xc000, "0x4000 and 0x100 (from 0x400)");
    assert_eq!(m.lw(input + 0x1c) as i32, -0xfff, "inverted axis 0");
    // Unknown and absent types clear the block.
    for (kind, class) in [(0x42, 0x20), (0, 0)] {
        let (mut m, pad, input, config) = input_machine(kind);
        m.write_bytes(input + 8, &[7; 0x34]);
        mainloop::map_pad_input(&mut m, pad, input, config);
        assert_eq!(m.lw(input + 4), class);
        let block: Vec<u8> = (8..0x3c).map(|o| m.lbu(input + o)).collect();
        assert_eq!(block, [0; 0x34]);
    }
    // Curve flags reach the rsin curve (sdk::rsin, translated).
    let (mut m, pad, input, config) = input_machine(0x73);
    m.write_bytes(config + 0x5c + 0x52, &[0, 0xff, 0xff, 0xff, 0xff, 0xff]);
    m.sh(config + 0x5c + 0x50, 1);
    mainloop::map_pad_input(&mut m, pad, input, config);
    no_fault(&mut m);
}

fn set_slot(m: &mut Machine, slot: u32, frames: u8, kind: u8, level: u8) {
    m.write_bytes(0x8010_3900 + slot * 4, &[frames, kind, level]);
}

#[test]
fn vibration_mix_and_output() {
    let slots = 0x8010_3900;
    let mut m = machine();
    set_slot(&mut m, 0, 1, 0x14, 0x40);
    set_slot(&mut m, 1, 10, 0xb4, 0x90);
    set_slot(&mut m, 2, 5, 0xb4, 0x10);
    set_slot(&mut m, 3, 0x40, 0xfa, 0xa0);
    set_slot(&mut m, 4, 2, 5, 0xff);
    set_slot(&mut m, 5, 0, 3, 0xff);
    mainloop::mix_vibration(&mut m, 0x8010_3a00);
    assert_eq!([m.lbu(0x8010_3a00), m.lbu(0x8010_3a01)], [0xa0, 0xff]);
    let slot = |m: &mut Machine, i: u32| [0, 1, 2].map(|o| m.lbu(slots + i * 4 + o));
    assert_eq!(
        slot(&mut m, 0),
        [0, 0, 0x3c],
        "expired, kind cleared, decay >> 4"
    );
    assert_eq!(slot(&mut m, 1), [9, 0xb4, 0x90]);
    assert_eq!(slot(&mut m, 2), [4, 0xb4, 0x0f]);
    assert_eq!(slot(&mut m, 3), [0x3f, 0xfa, 0xa0]);
    assert_eq!(slot(&mut m, 5), [0, 3, 0xff], "idle slot untouched");
    let mut m = machine();
    let kinds = [
        (1, 0x30),
        (2, 0x40),
        (9, 0x50),
        (0x10, 0x20),
        (0x1e, 0x80),
        (4, 0xff),
    ];
    for (i, (kind, level)) in kinds.into_iter().enumerate() {
        set_slot(&mut m, i as u32, 3, kind, level);
    }
    mainloop::mix_vibration(&mut m, 0x8010_3a00);
    assert_eq!([m.lbu(0x8010_3a00), m.lbu(0x8010_3a01)], [0x50, 0x80]);
    let levels: Vec<u8> = (0..6).map(|i| m.lbu(slots + i * 4 + 2)).collect();
    assert_eq!(levels, [0x30, 0x30, 0x50, 0x1c, 0x7c, 0xff]);

    // Motor bytes: small motor on/off from the pattern table, large level.
    let (mut m, pad, input, config) = input_machine(0x41);
    m.sw(input, 1);
    m.sb(pad + 1, 3);
    m.sw(CONTEXT + 0x24, 5);
    m.sw(0x800a_e760 + 8 * 4, 0xb555_aaad);
    set_slot(&mut m, 0, 2, 1, 0x80);
    set_slot(&mut m, 1, 3, 0x10, 0x40);
    mainloop::map_pad_input(&mut m, pad, input, config);
    assert_eq!([m.lbu(input + 0x34), m.lbu(input + 0x35)], [0x80, 0x40]);
    let bytes: Vec<u8> = (2..8).map(|o| m.lbu(pad + o)).collect();
    assert_eq!(bytes, [0x40, 1, 0x40, 0, 0, 0]);
    // Paused: all actuator bytes cleared.
    m.sw(CONTEXT, mainloop::flags::PAUSED);
    mainloop::map_pad_input(&mut m, pad, input, config);
    let bytes: Vec<u8> = (2..8).map(|o| m.lbu(pad + o)).collect();
    assert_eq!(bytes, [0; 6]);
    no_fault(&mut m);
}

fn define_sound(m: &mut Machine, id: u32, fixed: u8) {
    let definition = SOUNDS + id * 0x14;
    m.sb(definition, fixed);
    m.sh(definition + 4, 0x1111);
    m.sh(definition + 6, 0x2222);
    m.sh(definition + 8, 0x3c00);
}

#[test]
fn sound_voices_and_sequences() {
    let mut m = machine();
    for voice in 0..24 {
        m.sb(VOICES + voice * 0x14, 0xff);
    }
    define_sound(&mut m, 5, 3);
    define_sound(&mut m, 6, 0xff);
    define_sound(&mut m, 0x2e, 0xff);
    assert_eq!(script::play_sound_8002d724(&mut m, 5), 3);
    let v = VOICES + 3 * 0x14;
    let bytes: Vec<u8> = (0..4).map(|o| m.lbu(v + o)).collect();
    assert_eq!(bytes, [5, 1, 0x10, 1]);
    let halves = [4, 6, 8, 0xa, 0xc, 0xe, 0x12].map(|o| m.lhu(v + o));
    assert_eq!(halves, [0x1111, 0x2222, 0x1111, 0x2222, 0x3c00, 0x3c00, 0]);
    assert_eq!([m.lbu(v + 0x10), m.lbu(v + 0x11)], [0x80, 0x80]);
    // Free voices come from 12..23.
    assert_eq!(script::play_sound_8002d724(&mut m, 6), 12);
    for voice in 13..24 {
        m.sb(VOICES + voice * 0x14 + 3, 1);
    }
    m.sb(VOICES + 20 * 0x14 + 3, 0);
    m.sb(VOICES + 15 * 0x14, 0x2e);
    // 0x2E restarts its playing voice (8002D7B8) and starts another.
    assert_eq!(script::play_sound_8002d724(&mut m, 0x2e), 20);
    assert_eq!(m.lbu(VOICES + 15 * 0x14 + 1), 1, "voice 15 restarted");
    assert_eq!(
        script::play_sound_8002d724(&mut m, 6),
        0xff,
        "no voice left"
    );
    assert_eq!(shared::release_channel(&mut m, 24), 0xff);
    assert_eq!(shared::release_channel(&mut m, 4), 4);
    assert_eq!(m.lbu(VOICES + 4 * 0x14 + 1), 2);
    assert_eq!(shared::reset_channel(&mut m, 0xff), 0xff);
    assert_eq!(shared::reset_channel(&mut m, 3), 3);
    let bytes = [0, 1, 2, 3, 0x10, 0x11].map(|o| m.lbu(v + o));
    assert_eq!(bytes, [0xff, 0, 0, 0, 0x80, 0x80]);

    // Delayed sounds: frames 1 plays now and clears the slot.
    for i in 0..8 {
        m.sh(0x800c_4318 + i * 4 + 2, 0xffff);
    }
    let slot = 0x800c_4318 + 2 * 4;
    m.sb(slot, 5);
    m.sh(slot + 2, 1);
    mainloop::update_delayed_sounds(&mut m);
    assert_eq!([m.lbu(slot), m.lhu(slot + 2) as u8], [0xff, 0xff]);
    assert_eq!(m.lbu(v), 5, "sound 5 started on voice 3");

    // Sequence: a step with frames 0 plays, then waits for its voice.
    let step = 0x800c_4338 + 4 * 4;
    m.sb(GP + 0x314, 4);
    m.sb(step, 5);
    m.sb(step + 1, 0xff);
    m.sh(step + 2, 0);
    mainloop::update_sequence(&mut m);
    assert_eq!([m.lbu(step + 1), m.lhu(step + 2) as u8], [3, 0xff]);
    mainloop::update_sequence(&mut m);
    assert_eq!(m.lbu(GP + 0x314), 4, "voice 3 still in use");
    m.sb(v + 3, 0);
    mainloop::update_sequence(&mut m);
    assert_eq!(m.lbu(GP + 0x314), 5);
    assert_eq!([m.lbu(step), m.lbu(step + 1)], [0xff, 0xff]);
    let next = 0x800c_4338 + 5 * 4;
    m.sb(next + 1, 0xff);
    m.sh(next + 2, 0xffff);
    mainloop::update_sequence(&mut m);
    assert_eq!(m.lbu(GP + 0x315), 5, "end marker");
    no_fault(&mut m);
}

#[test]
fn sound_fade_and_release() {
    let mut m = machine();
    for voice in 0..24 {
        let v = VOICES + voice * 0x14;
        m.sb(v, 0xff);
        m.sb(v + 0x10, 0x80);
        m.sb(v + 0x11, 0x80);
    }
    m.sb(0x800c_4338 + 1, 0xff);
    m.sh(0x800c_4338 + 2, 0xffff);
    for slot in 0..8 {
        m.sh(0x800c_4318 + slot * 4 + 2, 0xffff);
    }
    let v = VOICES + 2 * 0x14;
    m.sb(v + 0x10, 0x40);
    m.sh(v + 0x12, 4);
    m.sb(v + 3, 1);
    let r = VOICES + 7 * 0x14;
    m.sb(r + 2, 1);
    m.sb(r + 3, 1);
    let mut states = vec![0; 24];
    states[2] = 1;
    states[7] = 3;
    m.host_results.extend(states);
    mainloop::update_sound(&mut m);
    assert_eq!([m.lbu(v + 0x11), m.lbu(v + 0x12)], [0x70, 3]);
    assert_eq!(m.lbu(v + 2), 8, "key on: release delay 8");
    assert_eq!(m.lbu(r + 1), 2, "envelope ended: key off");
    let keys: Vec<[u32; 2]> = m
        .host_calls
        .iter()
        .filter(|c| c.function == 0x8009_f3c8)
        .map(|c| [c.args[0], c.args[1]])
        .collect();
    assert_eq!(keys, [[1, 0], [0, 1 << 7]]);
    let updates = m
        .host_calls
        .iter()
        .filter(|c| c.function == 0x8009_fd68)
        .count();
    assert_eq!(updates, 1, "only the fading voice updates its volume");
    no_fault(&mut m);
}

#[test]
fn cd_queue_and_commands() {
    let mut m = machine();
    m.sw(0x800a_e7b0, 0x8002_b99c);
    m.sw(0x800a_e7b0 + 6 * 4, 0x8002_bff4);
    // Queued no-op with a completion routine (8002C6A8, read-only).
    m.sw(QUEUE, 0x13);
    m.sw(QUEUE + 4, 0x14);
    let entry = QUEUE + 3 * 0x14 + 8;
    m.sw(entry + 0x10, 0x8002_c6a8);
    mainloop::process_cd_queue(&mut m);
    assert_eq!(m.lw(QUEUE), 0x14);
    mainloop::process_cd_queue(&mut m);
    assert_eq!(m.lw(QUEUE), 0x14, "queue empty");
    // Pending command 6: stream pause request.
    m.sw(QUEUE + 0x148, 1);
    m.sb(QUEUE + 0x14c, 6);
    m.sw(QUEUE + 0x150, 1);
    m.sw(STREAM, 0x8000_0000);
    mainloop::process_cd_queue(&mut m);
    assert_eq!(m.lw(QUEUE + 0x148), 0);
    assert_eq!(m.lw(QUEUE + 0x178), 1);
    assert_eq!(m.lw(STREAM), 0x8000_0040);
    // Resume of a paused stream: restart time now + 2 + length / 3.
    m.sw(STREAM, 0xc000_0000);
    m.sw(STREAM + 0x14, 31);
    m.sw(MONITOR + 0x20, 0x8010_4300);
    m.sw(0x8010_4300, 1000);
    assert_eq!(mainloop::stream_pause(&mut m, 0), 1);
    assert_eq!(m.lw(STREAM + 0x3c), 1012);
    m.sw(STREAM, 0);
    assert_eq!(mainloop::stream_pause(&mut m, 1), 0xffff_ffff, "no stream");
    no_fault(&mut m);
}

#[test]
fn cd_monitor_states() {
    use pending_sdk::*;
    let drive =
        || Some(Box::new(CdDrive::new(NoDisc, ac3_flight::cdstream::CLOCK)) as Box<dyn Device>);
    // State 0, lid open: stop to standby (a blocking CdControl 7: the drive
    // acknowledges and completes), recovery from state 2.
    let mut m = machine();
    m.device = drive();
    m.sb(MONITOR + 0x28, 0x10);
    m.write_bytes(STREAM, &[0xaa; 0x44]);
    m.sw(CD_SYNC_CALLBACK_SLOT, 0x8002_9dcc);
    m.sw(CD_READY_CALLBACK_SLOT, 0x8002_993c);
    mainloop::cd_monitor(&mut m);
    assert_eq!(
        [m.lw(MONITOR), m.lw(MONITOR + 4), m.lw(MONITOR + 0xc)],
        [2, 0, 4]
    );
    let stream = [0, 4, 0x20, 0x40].map(|o| m.lw(STREAM + o));
    assert_eq!(stream, [0; 4]);
    assert_eq!(
        [m.lw(CD_SYNC_CALLBACK_SLOT), m.lw(CD_READY_CALLBACK_SLOT)],
        [0, 0]
    );
    let commands: Vec<[u32; 4]> = m
        .host_calls
        .iter()
        .filter(|c| c.function == CD_COMMAND)
        .map(|c| c.args)
        .collect();
    assert_eq!(commands, [[7, 0, 0, 0]]);
    assert_eq!(
        functions(&m)[..3],
        [0x8009_f888, 0x800a_14bc, 0x800a_14bc],
        "CD mix off, CdSync(1), CD_cw's CdSync(0)"
    );
    assert_eq!(m.lbu(CD_SYNC_STATUS), 2, "acknowledged (3), then complete");
    assert_eq!(m.lbu(MONITOR + 0x28), 2, "result: motor on");
    assert_eq!(m.lbu(CD_LAST_COMMAND), 7);
    no_fault(&mut m);
    // State 0, stream retries run out: pause it (CdControlF: no wait).
    let mut m = machine();
    m.device = drive();
    m.sw(STREAM, 0x10);
    m.sw(STREAM + 0x30, 0);
    mainloop::cd_monitor(&mut m);
    assert_eq!(
        [m.lw(STREAM), m.lw(STREAM + 0x20), m.lw(STREAM + 0x30)],
        [0x20, 0xff, 0xffff_ffff]
    );
    let last = *m.host_calls.last().unwrap();
    assert_eq!((last.function, last.args), (CD_COMMAND, [9, 0, 0, 1]));
    assert_eq!(m.lbu(CD_SYNC_STATUS), 0, "sent");
    m.sb(CD_SYNC_STATUS, 2);
    m.sw(STREAM, 0x10);
    m.sw(STREAM + 0x30, 2);
    m.host_calls.clear();
    mainloop::cd_monitor(&mut m);
    assert_eq!(m.lw(STREAM + 0x30), 1);
    assert_eq!(functions(&m), [0x800a_14bc]);
    // State 2: setmode, then stop; an open lid moves on to state 3.
    let mut m = machine();
    m.device = drive();
    m.sw(MONITOR, 2);
    mainloop::cd_monitor(&mut m);
    assert_eq!([m.lw(MONITOR), m.lw(MONITOR + 4)], [2, 1]);
    let command = m
        .host_calls
        .iter()
        .find(|c| c.function == CD_COMMAND)
        .unwrap();
    assert_eq!(command.args, [0xe, 1, 0, 0], "Setmode 0 (gp+0x2EC)");
    assert_eq!(m.lbu(CD_LAST_MODE), 0);
    mainloop::cd_monitor(&mut m);
    assert_eq!(m.lw(MONITOR + 4), 2);
    assert_eq!(m.lbu(MONITOR + 0x28), 0, "stopped: motor off");
    m.sb(MONITOR + 0x28, 0x10);
    mainloop::cd_monitor(&mut m);
    assert_eq!([m.lw(MONITOR), m.lw(MONITOR + 4)], [3, 0]);
    // State 3: a drive error returns to state 2 (+0xC = 5); step 7 moves to
    // state 4 three ticks after its start.
    m.sb(MONITOR + 0x28, 1);
    m.sb(MONITOR + 0x29, 0x40);
    mainloop::cd_monitor(&mut m);
    assert_eq!([m.lw(MONITOR), m.lw(MONITOR + 0xc)], [2, 5]);
    m.sw(MONITOR, 3);
    m.sb(MONITOR + 0x28, 0);
    m.sb(MONITOR + 0x29, 0);
    m.sw(MONITOR + 4, 7);
    m.sw(MONITOR + 0x20, 0x8010_4300);
    m.sw(MONITOR + 8, 100);
    m.sw(0x8010_4300, 103);
    mainloop::cd_monitor(&mut m);
    assert_eq!(m.lw(MONITOR), 3);
    m.sw(0x8010_4300, 104);
    mainloop::cd_monitor(&mut m);
    assert_eq!([m.lw(MONITOR), m.lw(MONITOR + 4)], [4, 0]);
    // State 4: header files not found (boot's 8002A9E4): error 5, back to
    // state 2.
    mainloop::cd_monitor(&mut m);
    assert_eq!([m.lw(MONITOR), m.lw(MONITOR + 0xc)], [2, 5]);
    assert!(functions(&m).contains(&ac3_flight::boot::CD_SEARCH_FILE));
    no_fault(&mut m);
}

#[test]
fn cursor_sprites_and_frame_end() {
    let mut m = machine();
    let (queue, ot, packet) = (0x8010_3800, 0x8010_7000, 0x8010_6000);
    m.sw(queue, 2);
    m.sw(queue + 4, 0x0010_0020);
    m.sw(queue + 8, 0x7f00_1234);
    m.sw(queue + 0xc, 0x0030_0040);
    m.sw(queue + 0x10, 0x7f00_5678);
    m.sw(ot, 0x00ff_ffff);
    assert_eq!(mainloop::link_sprites(&mut m, ot, packet), packet + 0x30);
    let words: Vec<u32> = (0..12).map(|i| m.lw(packet + i * 4)).collect();
    assert_eq!(
        words,
        [
            0x04ff_ffff,
            0x6480_8080,
            0x0010_0020,
            0x7f00_1234,
            0x0006_0006,
            0x0400_0000 | (packet & 0xff_ffff),
            0x6480_8080,
            0x0030_0040,
            0x7f00_5678,
            0x0006_0006,
            0x0100_0000 | ((packet + 0x14) & 0xff_ffff),
            0xe100_0015,
        ]
    );
    assert_eq!(m.lw(ot), (packet + 0x28) & 0xff_ffff);
    assert_eq!([m.lw(queue), m.lw(GP + 0x2e4)], [0, 0x0010_0010]);

    // Frame end on buffer 1.
    use mainloop::flags::*;
    m.sw(CONTEXT + 4, 1);
    m.sw(BUFFERS + 0x1014, 0x8010_8000);
    m.sw(BUFFERS + 0x1018, 0x8010_9000);
    for (word, counted, after) in [
        (0, 1, 0),
        (SKIP_COUNT, 1, 0),
        (PAUSED | SKIP_COUNT, 1, PAUSED | SKIP_COUNT),
        (NO_DRAW, 2, NO_DRAW),
    ] {
        m.sw(CONTEXT, word);
        m.host_calls.clear();
        mainloop::frame_end(&mut m);
        assert_eq!(m.lw(CONTEXT), after);
        assert_eq!(m.lw(CONTEXT + 0x28), counted, "flags {word:#x}");
        let submitted: Vec<u32> = m
            .host_calls
            .iter()
            .filter(|c| c.function == 0x800a_8360)
            .map(|c| c.args[0])
            .collect();
        let expected: &[u32] = if word & NO_DRAW != 0 {
            &[0x8010_801c]
        } else {
            &[0x8010_801c, BUFFERS + 0x1014 + 0x10]
        };
        assert_eq!(submitted, expected, "flags {word:#x}");
    }
    assert_eq!(mainloop::frame_end(&mut m), 5);
    assert_eq!(m.lw(sdk::GPU_DRAW_ENV), m.lw(0x8010_8000), "DRAWENV copied");
    no_fault(&mut m);
}

#[test]
fn dispatch_claims_translated_routines() {
    let mut m = machine();
    m.sw(QUEUE + 0x174, 0x1234);
    assert_eq!(
        ac3_flight::dispatch::call(&mut m, 0x8002_c6a8, [0; 4]),
        0x1234
    );
    assert_eq!(
        ac3_flight::dispatch::call(&mut m, 0x8002_61f4, [3, 0, 0, 0]),
        5
    );
    assert_eq!(ac3_flight::dispatch::call(&mut m, 0x8002_b99c, [0; 4]), 1);
    assert_eq!(
        ac3_flight::dispatch::call(&mut m, 0x8002_5ba4, [0; 4]),
        0x6000_0004,
        "initial state: set state 4, reset the arena"
    );
    assert_eq!(
        mainloop::dispatch(&mut m, 0x8002_5d08, [0; 4]),
        None,
        "main never returns"
    );
    no_fault(&mut m);
}
