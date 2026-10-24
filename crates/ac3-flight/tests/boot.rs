//! Resident boot (`boot.rs`) against cold-boot captures of the original.
//!
//! - `analysis/flight-runtime/boot-001`: RAM at resident entry 8002521C and
//!   at the first 80025720 call (inside 80025934), with the last-writer PC of
//!   every byte.
//! - `analysis/flight-runtime/objects-boot-002`: the same entry image, RAM at
//!   main's first loop call of 80025720 (80025D30), writers and the full call
//!   trace (`boot-calls.jsonl`: every resident call with a0..a3 and return).
//! - `analysis/flight-runtime/objects-boot-003`: entry/return snapshots of the
//!   boot's game routines (`snapshots.jsonl`).
//!
//! RAM comparisons cover everything outside the stack except the ranges in
//! [`EXCLUDED`], each with its reason.
use ac3_flight::{
    Machine,
    boot::{self, BootStop},
    dispatch,
    machine::RAM_BYTES,
    snapshot::{self, Snapshot},
};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::OnceLock,
};

fn data() -> PathBuf {
    PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR"))
}
fn capture(name: &str) -> PathBuf {
    data().join("analysis/flight-runtime").join(name)
}
fn payload() -> Vec<u8> {
    let payload = fs::read(data().join("extracted/programs/ACE.unpacked.bin")).unwrap();
    assert_eq!(payload.len(), boot::PAYLOAD_BYTES);
    payload
}
fn disc() -> Box<ac3_flight::cdstream::CdDrive<ac3_flight::cdstream::RawImage<fs::File>>> {
    let open = || {
        fs::File::open(data().join("Ace Combat 3 - Electrosphere (Europe) (En,Fr,De,Es,It).bin"))
            .unwrap()
    };
    // The libcd drive (commands, interrupts, sectors) in front of the boot
    // device that answers CdSearchFile/CdRead.
    let mut drive = ac3_flight::cdstream::CdDrive::new(
        ac3_flight::cdstream::RawImage(open()),
        ac3_flight::cdstream::CLOCK,
    );
    drive.inner = Some(Box::new(boot::DiscDevice::new(open(), boot::CD_FILE_CACHE)));
    Box::new(drive)
}

/// A machine from an image with the disc attached as its host device.
fn machine(ram: &[u8]) -> Machine {
    let mut m = Machine::from_images(ram, &[0; 0x400]).unwrap();
    m.device = Some(disc());
    m
}

/// RAM never compared, with the reason (inclusive-exclusive). Everything in
/// these ranges is written only by SDK or kernel code in the captures
/// (`sdk_ranges_hold_no_game_state` checks the writers), and only SDK code
/// reads it: the device routines are host calls in the native boot.
const EXCLUDED: &[(u32, u32, &str)] = &[
    (
        0x8000_0000,
        0x8001_0000,
        "kernel area (BIOS/kernel data, kernel patches)",
    ),
    (
        0x801f_e000,
        0x8020_0000,
        "stack (excluded from every machine comparison)",
    ),
    (
        0x800b_acb0,
        0x800b_acb4,
        "stup1's saved BIOS Exec return (0xBFC03D60), read only if main returns",
    ),
    (0x800b_2504, 0x800b_258c, "libpad driver state"),
    (0x800b_25cc, 0x800b_26b0, "libspu driver state"),
    (0x800b_2ec4, 0x800b_2ecc, "libetc VSync state"),
    (
        0x800b_2f6c,
        0x800b_2f70,
        "libetc interrupt manager's setjmp buffer: the caller's fp (ResetCallback's RAM is sdk.rs's)",
    ),
    (
        0x800b_2f74,
        0x800b_2f90,
        "libetc interrupt manager's setjmp buffer: the caller's s1..s7",
    ),
    (
        0x800b_2f94,
        0x800b_3f44,
        "libetc interrupt handlers' stack (interrupt frames; the host's)",
    ),
    (
        0x800b_3ff4,
        0x800b_3ff8,
        "libetc VBlank counter (VBlanks the host delivered)",
    ),
    (
        0x800b_4010,
        0x800b_4014,
        "DMA callback 2: libgpu installs 800A86AC around queued transfers (the host's complete at once)",
    ),
    (
        0x800b_4018,
        0x800b_401c,
        "DMA callback 4: libspu's transfer callback 8009DAE8 (SpuInit is a host call)",
    ),
    (
        0x800b_4038,
        0x800b_4100,
        "libcd state (CdInit, callbacks, debug level, command bytes)",
    ),
    (0x800b_43b8, 0x800b_4440, "libcd file and read state"),
    (
        0x800b_97ec,
        0x800b_97f0,
        "libgpu submission flag (set by the original's GPU transfers; LoadImage is a host call)",
    ),
    (
        0x800b_990c,
        0x800b_9928,
        "libgpu queue indices, saved interrupt masks and DMA timeout (queued GPU transfers, ResetGraph's mask save)",
    ),
    (0x800b_a964, 0x800b_a96c, "libc stdout state"),
    (0x800c_59d0, 0x800c_5cbc, "libpad, libspu and libcd BSS"),
    (
        0x800c_5cd0,
        0x800c_6304,
        "libcd file cache and ISO9660 directory state",
    ),
    (0x800c_78d0, 0x800c_80d0, "libcd directory sector buffer"),
    (0x800c_8128, 0x800c_9a30, "libgpu command queue"),
    (0x800c_9a50, 0x800c_9a54, "libcard state"),
    (0x800c_9d40, 0x800c_9d60, "libc putchar buffer"),
    (
        0x800c_9dc4,
        0x800c_9dc8,
        "kernel InitHeap free-list header (the game never uses the kernel heap)",
    ),
];

/// Pad receive buffers: after PadStartCom the pad driver rewrites them from
/// controller input in VSync interrupts (host input in the native player).
/// Excluded wherever interrupts ran during the compared interval.
const PAD_INPUT: &[(u32, u32, &str)] = &[(
    0x800b_e1e0,
    0x800b_e224,
    "pad receive buffers (controller input, written in VSync interrupts)",
)];

fn excluded(address: u32, extra: &[(u32, u32, &'static str)]) -> Option<&'static str> {
    EXCLUDED
        .iter()
        .chain(extra)
        .find(|(start, end, _)| (*start..*end).contains(&address))
        .map(|&(_, _, reason)| reason)
}

fn ram(path: &Path) -> Vec<u8> {
    let ram = fs::read(path).unwrap();
    assert_eq!(ram.len(), RAM_BYTES);
    ram
}

/// Last-writer PC per RAM byte, when the capture has one.
fn writers(directory: &Path) -> Option<Vec<u32>> {
    let bytes = fs::read(directory.join("boot-writers.bin")).ok()?;
    Some(
        bytes
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
    )
}

/// Differing byte ranges outside [`EXCLUDED`], with the original writer of
/// each range's first byte.
fn differences(
    actual: &[u8],
    expected: &[u8],
    writers: Option<&[u32]>,
    extra: &[(u32, u32, &'static str)],
) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < RAM_BYTES {
        let address = 0x8000_0000 + i as u32;
        if actual[i] != expected[i] && excluded(address, extra).is_none() {
            let start = i;
            while i < RAM_BYTES
                && (actual[i] != expected[i] || i < start + 8)
                && excluded(0x8000_0000 + i as u32, extra).is_none()
            {
                i += 1;
            }
            let writer = writers.map_or(0, |w| w[start]);
            out.push(format!(
                "{address:08x}..{:08x} writer {writer:08x}: got {:02x?} want {:02x?}",
                0x8000_0000 + i as u32,
                &actual[start..i.min(start + 12)],
                &expected[start..i.min(start + 12)]
            ));
        }
        i += 1;
    }
    out
}

fn assert_ram(
    actual: &[u8],
    directory: &Path,
    image: &str,
    extra: &[(u32, u32, &'static str)],
) -> usize {
    let expected = ram(&directory.join(image));
    let writers = writers(directory);
    let diffs = differences(actual, &expected, writers.as_deref(), extra);
    assert!(
        diffs.is_empty(),
        "{} differing ranges:\n{}",
        diffs.len(),
        diffs
            .iter()
            .take(80)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
    let entry = ram(&directory.join("entry.ram.bin"));
    (0..RAM_BYTES)
        .filter(|&i| entry[i] != expected[i] && excluded(0x8000_0000 + i as u32, extra).is_none())
        .count()
}

/// Host-call arities (arguments compared) of the SDK/kernel routines the
/// game calls directly during boot.
fn arity(function: u32) -> Option<usize> {
    Some(match function {
        boot::INIT_HEAP => 2,
        boot::RESET_CALLBACK | boot::CD_INIT | boot::SS_UT_REVERB_OFF => 0,
        boot::START_CARD | boot::BU_INIT | boot::PAD_START_COM => 0,
        boot::EXIT_CRITICAL_SECTION => 0,
        boot::CD_SET_DEBUG | boot::VSYNC | boot::RESET_GRAPH | boot::SET_DISP_MASK => 1,
        boot::INIT_CARD | boot::PAD_ENABLE_COM | boot::START_RCNT => 1,
        boot::SPU_SET_COMMON_ATTR | boot::CD_MIX | boot::SPU_SET_REVERB_MODE_PARAM => 1,
        boot::CD_SYNC_CALLBACK | boot::CD_DATA_CALLBACK | boot::CD_READY_CALLBACK => 1,
        boot::PRINTF => 1,
        boot::PAD_INIT_DIRECT | boot::CD_SEARCH_FILE | boot::CD_READ_SYNC => 2,
        boot::LOAD_IMAGE => 2,
        boot::SET_RCNT | boot::CD_CONTROL_B | boot::CD_READ => 3,
        _ => return None,
    })
}

/// The resident call trace of `objects-boot-002` (`boot-calls.jsonl`).
struct Trace {
    events: Vec<Event>,
    functions: Vec<u32>,
}

enum Event {
    Call {
        f: u32,
        ra: u32,
        sp: u32,
        a: [u32; 4],
    },
    Return {
        pc: u32,
        sp: u32,
        v0: u32,
    },
}

fn trace() -> &'static Trace {
    static TRACE: OnceLock<Trace> = OnceLock::new();
    TRACE.get_or_init(|| {
        let text =
            fs::read_to_string(capture("objects-boot-002").join("boot-calls.jsonl")).unwrap();
        let number = |v: &Value| v.as_u64().unwrap() as u32;
        let events = text
            .lines()
            .map(|line| {
                let e: Value = serde_json::from_str(line).unwrap();
                if e["e"] == "call" {
                    let a = e["a"].as_array().unwrap();
                    Event::Call {
                        f: number(&e["f"]),
                        ra: number(&e["ra"]),
                        sp: number(&e["sp"]),
                        a: std::array::from_fn(|i| number(&a[i])),
                    }
                } else {
                    Event::Return {
                        pc: number(&e["pc"]),
                        sp: number(&e["sp"]),
                        v0: number(&e["v0"]),
                    }
                }
            })
            .collect();
        let mut functions: Vec<u32> =
            fs::read_to_string(data().join("analysis/main/functions.tsv"))
                .unwrap()
                .lines()
                .filter_map(|l| u32::from_str_radix(l.split('\t').next()?, 16).ok())
                .collect();
        functions.sort_unstable();
        Trace { events, functions }
    })
}

impl Trace {
    /// Whether the call returning to `ra` was made from resident game code.
    fn called_from_game(&self, ra: u32) -> bool {
        let pc = ra.wrapping_sub(8);
        let caller = match self.functions.binary_search(&pc) {
            Ok(i) => self.functions[i],
            Err(0) => 0,
            Err(i) => self.functions[i - 1],
        };
        (0x8001_0000..0x8009_9000).contains(&caller)
    }
}

/// The original's game-level host calls (callee in [`arity`], called from
/// resident game code) up to the first call of `stop` whose return address
/// is `stop_ra` (any if 0).
fn traced_host_calls(stop: u32, stop_ra: u32) -> Vec<(u32, Vec<u32>)> {
    let trace = trace();
    let mut calls = Vec::new();
    for event in &trace.events {
        let Event::Call { f, ra, a, .. } = *event else {
            continue;
        };
        if f == stop && (stop_ra == 0 || ra == stop_ra) {
            return calls;
        }
        if let Some(n) = arity(f)
            && trace.called_from_game(ra)
        {
            calls.push((f, a[..n].to_vec()));
        }
    }
    panic!("stop {stop:08x} not reached in the trace");
}

/// Host routines whose return value the translation consumes
/// (`Machine::host_call_result`) and that the disc device does not answer.
const RESULTS: &[u32] = &[
    boot::RESET_GRAPH,
    boot::CD_INIT,
    boot::EXIT_CRITICAL_SECTION,
    boot::PAD_ENABLE_COM,
    boot::START_RCNT,
    boot::CD_MIX,
    boot::LOAD_IMAGE,
    boot::SPU_SET_REVERB_MODE_PARAM,
    boot::PRINTF,
];

/// The original's return values of the [`RESULTS`] calls made from game code
/// inside the `occurrence`th call of `callee` (0 where the trace has no
/// return), in call order: what the host supplies.
fn traced_results(callee: u32, occurrence: usize) -> Vec<u32> {
    let trace = trace();
    let mut seen = 0;
    let mut open: Vec<(u32, u32, Option<usize>)> = Vec::new();
    let mut results = Vec::new();
    let mut inside: Option<(u32, u32)> = None;
    for event in &trace.events {
        match *event {
            Event::Call { f, ra, sp, .. } => {
                if inside.is_none() && f == callee {
                    if seen == occurrence {
                        inside = Some((ra, sp));
                    }
                    seen += 1;
                    continue;
                }
                if inside.is_some() {
                    let slot = (RESULTS.contains(&f) && trace.called_from_game(ra)).then(|| {
                        results.push(0);
                        results.len() - 1
                    });
                    open.push((ra, sp, slot));
                }
            }
            Event::Return { pc, sp, v0 } => {
                let Some((ra, entry_sp)) = inside else {
                    continue;
                };
                if pc == ra && sp == entry_sp {
                    return results;
                }
                if let Some(i) = open.iter().rposition(|&(r, s, _)| r == pc && s == sp) {
                    if let Some(slot) = open[i].2 {
                        results[slot] = v0;
                    }
                    open.truncate(i);
                }
            }
        }
    }
    panic!("{callee:08x} #{occurrence}: no return in the trace");
}

/// BIOS puts, through which libcd prints its messages.
const PUTS: u32 = 0x800a_d618;

fn native_host_calls(m: &Machine) -> Vec<(u32, Vec<u32>)> {
    // The trace keeps only calls made from game code. Translated libcd's
    // CD_init (800A2AD8) makes its own ResetCallback right after printing
    // "CD_init:"; the original's comes from SDK code, so it is not traced.
    let libcd_message = |c: &ac3_flight::HostCall| {
        (c.function == PUTS || c.function == boot::PRINTF) && LIBCD_STRINGS.contains(&c.args[0])
    };
    m.host_calls
        .iter()
        .enumerate()
        .filter(|(i, c)| {
            !(c.function == boot::RESET_CALLBACK
                && i.checked_sub(1)
                    .is_some_and(|p| libcd_message(&m.host_calls[p])))
        })
        .filter_map(|(_, c)| Some((c.function, c.args[..arity(c.function)?].to_vec())))
        .collect()
}

/// libcd's code and its message strings. libcd is translated
/// (`mainloop::pending_sdk`, `cdstream`; validated by their own snapshot
/// tests): the original records its API calls as device calls, the native
/// boot only its drive commands and its own debug prints. Both are left out
/// of the sequence comparison; their RAM effects are still compared.
const LIBCD_CODE: std::ops::Range<u32> = 0x800a_1228..0x800a_45c8;
const LIBCD_STRINGS: std::ops::Range<u32> = 0x8002_4600..0x8002_4b54;

/// Compares the host-call sequences, ignoring `skip` (VSync waits whose
/// count depends on device timing) and libcd ([`LIBCD_CODE`]).
fn assert_host_calls(m: &Machine, stop: u32, stop_ra: u32, skip: &[u32]) -> usize {
    let keep = |calls: Vec<(u32, Vec<u32>)>| -> Vec<(u32, Vec<u32>)> {
        calls
            .into_iter()
            .filter(|(f, _)| !skip.contains(f) && !LIBCD_CODE.contains(f))
            .filter(|(f, args)| {
                !(*f == boot::PRINTF && args.first().is_some_and(|a| LIBCD_STRINGS.contains(a)))
            })
            .collect()
    };
    let traced = keep(traced_host_calls(stop, stop_ra));
    let native = keep(native_host_calls(m));
    let first = traced
        .iter()
        .zip(&native)
        .position(|(a, b)| a != b)
        .unwrap_or(traced.len().min(native.len()));
    assert!(
        traced.len() == native.len() && first == traced.len(),
        "host calls differ at #{first} (native {}, traced {}): native {:x?} traced {:x?}",
        native.len(),
        traced.len(),
        native.get(first),
        traced.get(first)
    );
    traced.len()
}

/// Boot from the original entry image (payload already in place, the boot
/// loader's leftovers above the BSS kept) to the first 80025720 call.
#[test]
#[ignore = "requires AC3_DATA_DIR, the disc image and analysis/flight-runtime/boot-001"]
fn boot_to_first_present_boot_001() {
    let directory = capture("boot-001");
    let entry = ram(&directory.join("entry.ram.bin"));
    let mut m = machine(&entry);
    let payload = payload();
    assert!(boot::boot_until(&mut m, &payload, BootStop::FirstPresent));
    m.take_fault().unwrap();
    let changed = assert_ram(m.ram(), &directory, "main-loop.ram.bin", &[]);
    let calls = assert_host_calls(&m, boot::PRESENT, 0, &[]);
    eprintln!("boot-001: {changed} changed bytes reproduced, {calls} host calls");
    assert!(changed > 40_000);
}

/// The native image from a zero machine equals the entry-image run on the
/// payload, the BSS and everything the boot writes; only the boot loader's
/// leftovers (never written by the boot) differ.
#[test]
#[ignore = "requires AC3_DATA_DIR, the disc image and analysis/flight-runtime/boot-001"]
fn native_image_matches_entry_image_run() {
    let directory = capture("boot-001");
    let entry = ram(&directory.join("entry.ram.bin"));
    let payload = payload();
    let mut original = machine(&entry);
    boot::boot_until(&mut original, &payload, BootStop::FirstPresent);
    let mut native = Machine::default();
    native.device = Some(disc());
    boot::boot_until(&mut native, &payload, BootStop::FirstPresent);
    native.take_fault().unwrap();
    let writers = writers(&directory).unwrap();
    let expected = ram(&directory.join("main-loop.ram.bin"));
    let mut leftovers = 0;
    for i in 0x1_0000..0x1f_e000 {
        let address = 0x8000_0000 + i as u32;
        if native.ram()[i] != original.ram()[i] && excluded(address, &[]).is_none() {
            assert_eq!(writers[i], 0, "{address:08x} written by the boot");
            assert_eq!(original.ram()[i], expected[i]);
            leftovers += 1;
        }
    }
    eprintln!("{leftovers} boot-loader leftover bytes differ (zero in the native image)");
}

/// Every excluded SDK range holds only bytes written by SDK or kernel code
/// (or cleared by `start`) in the full boot capture, apart from the libcd
/// result bytes 0x800B3ECC..0x800B3ED7 that the stream code (mainloop.rs,
/// 80029A40) writes after the boot reaches its resource load.
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/objects-boot-002"]
fn sdk_ranges_hold_no_game_state() {
    let writers = writers(&capture("objects-boot-002")).unwrap();
    for &(start, end, reason) in &EXCLUDED[3..] {
        for address in start..end {
            let pc = writers[(address & 0x1f_ffff) as usize];
            let game = (0x8001_0000..0x8009_9000).contains(&pc) && pc != 0x8002_522c;
            let stream = (0x800b_3ecc..0x800b_3ed8).contains(&address);
            assert!(
                !game || stream,
                "{address:08x} ({reason}) written by game code {pc:08x}"
            );
        }
    }
}

/// Entry/return snapshot of one boot routine with the comparison above.
fn check(
    callee: u32,
    occurrence: usize,
    native: impl FnOnce(&mut Machine, &Snapshot) -> u32,
) -> usize {
    check_with(callee, occurrence, true, native)
}

/// [`check`]; `supply` = false leaves `host_results` empty (every host call
/// takes its default, which then must be the original's value), for routines
/// that call `sdk::vsync`, whose results the trace does not record.
fn check_with(
    callee: u32,
    occurrence: usize,
    supply: bool,
    native: impl FnOnce(&mut Machine, &Snapshot) -> u32,
) -> usize {
    let index = capture("objects-boot-003").join("snapshots.jsonl");
    let directory = index.parent().unwrap();
    let all = snapshot::read_index(&index).unwrap();
    let entry = all
        .iter()
        .filter(|s| s.callee == callee && s.stage == "entry")
        .nth(occurrence)
        .unwrap_or_else(|| panic!("{callee:08x}: no entry #{occurrence}"));
    let exit = all
        .iter()
        .find(|s| {
            s.callee == callee
                && s.stage == "return"
                && s.sequence > entry.sequence
                && s.ra == entry.ra
                && s.registers.gpr[29] == entry.registers.gpr[29]
        })
        .unwrap();
    let entry = Snapshot::load(directory, entry).unwrap();
    let exit = Snapshot::load(directory, exit).unwrap();
    let mut m = entry.machine();
    m.device = Some(disc());
    if supply {
        m.host_results.extend(traced_results(callee, occurrence));
    }
    let v0 = native(&mut m, &entry);
    if let Err(fault) = m.take_fault() {
        panic!("{callee:08x}: {fault}");
    }
    let diffs = differences(m.ram(), &exit.ram, None, PAD_INPUT);
    assert!(
        diffs.is_empty(),
        "{callee:08x}: {}",
        diffs
            .iter()
            .take(40)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
    let scratch = snapshot::compare(&m, &exit);
    assert!(
        scratch.scratch.is_empty() && scratch.gte_data.is_empty() && scratch.gte_control.is_empty(),
        "{callee:08x}: {}",
        scratch.summary()
    );
    assert_eq!(v0, exit.gpr(2), "{callee:08x}: v0");
    assert_eq!(m.sp, entry.gpr(29), "{callee:08x}: frames balanced");
    assert!(
        m.host_results.is_empty(),
        "{callee:08x}: unused host results"
    );
    let changed = (0..0x1f_e000)
        .filter(|&i| {
            entry.ram[i] != exit.ram[i] && excluded(0x8000_0000 + i as u32, PAD_INPUT).is_none()
        })
        .count();
    eprintln!("{callee:08x} #{occurrence}: {changed} changed bytes, v0 {v0:#x}");
    changed
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/objects-boot-003"]
fn init_globals_800252d8() {
    assert!(check(0x8002_52d8, 0, |m, _| boot::init_globals(m)) > 100);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/objects-boot-003"]
fn init_sound_channels_80028720() {
    assert!(
        check(0x8002_8720, 0, |m, e| boot::init_sound_channels(
            m,
            e.gpr(4)
        )) > 100
    );
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/objects-boot-003"]
fn init_devices_80025610() {
    check_with(0x8002_5610, 0, false, |m, _| boot::init_devices(m));
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/objects-boot-003"]
fn init_pads_800260ac() {
    assert!(
        check(0x8002_60ac, 0, |m, e| {
            boot::init_pads(m, e.gpr(4), e.gpr(5), e.gpr(6), e.gpr(7))
        }) > 40
    );
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/objects-boot-003"]
fn init_audio_attributes_80025990() {
    check(0x8002_5990, 0, |m, _| boot::init_audio_attributes(m));
}

#[test]
#[ignore = "requires AC3_DATA_DIR, the disc image and analysis/flight-runtime/objects-boot-003"]
fn init_resources_8002b8b0() {
    assert!(check(0x8002_b8b0, 0, |m, _| boot::init_resources(m)) > 8000);
}

#[test]
#[ignore = "requires AC3_DATA_DIR, the disc image and analysis/flight-runtime/objects-boot-003"]
fn search_files_8002a8ac() {
    assert!(
        check(0x8002_a8ac, 0, |m, e| {
            boot::search_files(m, e.gpr(4), e.gpr(5), e.gpr(6), e.gpr(7))
        }) > 16
    );
}

#[test]
#[ignore = "requires AC3_DATA_DIR, the disc image and analysis/flight-runtime/objects-boot-003"]
fn read_sectors_8002a968() {
    for occurrence in 0..2 {
        let changed = check(0x8002_a968, occurrence, |m, e| {
            boot::read_sectors(m, e.gpr(4), e.gpr(5), e.gpr(6))
        });
        assert!(changed > 4000);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/objects-boot-003"]
fn upload_tim_80026d00() {
    for occurrence in 0..2 {
        check(0x8002_6d00, occurrence, |m, e| {
            boot::upload_tim(m, e.gpr(4))
        });
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/objects-boot-003"]
fn init_display_lists_80027128() {
    assert!(check(0x8002_7128, 0, |m, _| boot::init_display_lists(m)) >= 8);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/objects-boot-003"]
fn init_sound_8002d334() {
    assert!(check(0x8002_d334, 0, |m, _| boot::init_sound(m)) > 200);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/objects-boot-003"]
fn set_master_volume_8002d458() {
    assert!(check(0x8002_d458, 0, |m, e| boot::set_master_volume(m, e.gpr(4))) > 2);
}

/// The traced boot reaches main's loop only through mainloop.rs's 80025720
/// and 8002C0B4; until they are claimed, the native boot stops with
/// `Fault::Untranslated` at the first of them. Once both exist this compares
/// the whole main-loop image and the host calls (VSync waits excepted; the
/// request pump's device-timing state may then need its own exclusions).
#[test]
#[ignore = "requires AC3_DATA_DIR, the disc image and analysis/flight-runtime/objects-boot-002"]
fn boot_to_main_loop_reaches_owned_routines() {
    let directory = capture("objects-boot-002");
    let entry = ram(&directory.join("entry.ram.bin"));
    let mut m = machine(&entry);
    let payload = payload();
    boot::boot(&mut m, &payload);
    match m.take_fault() {
        Ok(()) => {
            assert_ram(m.ram(), &directory, "main-loop.ram.bin", PAD_INPUT);
            assert_host_calls(&m, boot::PRESENT, 0x8002_5d38, &[boot::VSYNC]);
        }
        Err(fault) => {
            assert!(
                [boot::PRESENT, boot::PUMP_REQUESTS]
                    .map(|address| ac3_flight::Fault::Untranslated { address })
                    .contains(&fault),
                "only mainloop.rs routines may be missing: {fault}"
            );
        }
    }
}

/// Routines that ran before the first-present point and again after the
/// boot (state code), so their bytes may differ at the first overlay
/// callback: 80026F5C resets the arena on state transitions.
const REWRITTEN_LATER: &[u32] = &[0x8002_6f5c];

/// The flight engine's inputs (`inputs-002`, bytes first loaded between
/// state-2 entry and 20 flight callbacks) in the native first-present image
/// (from a zero machine) against the original at its first overlay callback
/// 800CB2BC (`writers-001/boot`). Every differing byte must have been
/// written, in the original, by a routine that runs after the first-present
/// point (later boot steps, the main loop, menus) or be a boot-loader
/// leftover. Bytes last written by SDK routines outside the excluded
/// internals (environment pad bytes, draw packets) are counted separately:
/// the main loop calls those routines again. SDK internals and pad input are
/// not compared.
#[test]
#[ignore = "requires AC3_DATA_DIR, the disc image and analysis/flight-runtime/{inputs-002,writers-001,objects-boot-002}"]
fn flight_inputs_at_first_overlay_callback() {
    let payload = payload();
    let mut m = Machine::default();
    m.device = Some(disc());
    assert!(boot::boot_until(&mut m, &payload, BootStop::FirstPresent));
    m.take_fault().unwrap();
    let directory = capture("writers-001/boot");
    let overlay = ram(&directory.join("main-loop.ram.bin"));
    let writers = writers(&directory).unwrap();
    let trace = trace();
    let function = |pc: u32| match trace.functions.binary_search(&pc) {
        Ok(i) => trace.functions[i],
        Err(0) => pc,
        Err(i) if (0x8001_0000..0x800c_9dc0).contains(&pc) => trace.functions[i - 1],
        Err(_) => pc,
    };
    let (mut before, mut after) = (
        std::collections::HashSet::new(),
        std::collections::HashSet::new(),
    );
    let mut stopped = false;
    for event in &trace.events {
        if let Event::Call { f, .. } = *event {
            stopped |= f == boot::PRESENT;
            if stopped {
                after.insert(f)
            } else {
                before.insert(f)
            };
        }
    }
    let text = fs::read_to_string(capture("inputs-002").join("input-set.jsonl")).unwrap();
    let (mut compared, mut equal, mut later, mut sdk, mut leftovers) = (0, 0, 0, 0, 0);
    for line in text.lines() {
        let range: Value = serde_json::from_str(line).unwrap();
        let start = range["start"].as_u64().unwrap() as u32;
        for address in start..start + range["length"].as_u64().unwrap() as u32 {
            if !(0x8001_0000..0x801f_e000).contains(&address)
                || excluded(address, PAD_INPUT).is_some()
            {
                continue;
            }
            compared += 1;
            let i = (address & 0x1f_ffff) as usize;
            if m.ram()[i] == overlay[i] {
                equal += 1;
                continue;
            }
            let writer = writers[i];
            if writer == 0 {
                assert!(
                    address >= boot::BSS_END,
                    "{address:08x}: unwritten payload byte differs"
                );
                leftovers += 1;
                continue;
            }
            let f = function(writer);
            if (0x8001_0000..0x8009_9000).contains(&f) {
                assert!(
                    !before.contains(&f) || after.contains(&f) || REWRITTEN_LATER.contains(&f),
                    "{address:08x}: written by {f:08x}, which runs only before the first present"
                );
                later += 1;
            } else {
                sdk += 1;
            }
        }
    }
    eprintln!(
        "{compared} flight-input bytes: {equal} equal at the first present; differing: {later} written later by game code, {sdk} by SDK routines the main loop calls again, {leftovers} loader leftovers"
    );
    assert!(equal >= 3415, "regression: {equal} equal");
}

/// The boot steps after 80025934 that do not depend on mainloop.rs, run on
/// the first-present image in original order: 80025990, 8002B8B0, then
/// (skipping 800259F8, whose resource load needs the request pump)
/// 8002D334, 8002C7A4 and 8002D458(1). Every byte the original's main-loop
/// image last received from these routines (or from the CD header reads they
/// issue) must match.
#[test]
#[ignore = "requires AC3_DATA_DIR, the disc image and analysis/flight-runtime/objects-boot-002"]
fn boot_continuation_matches_main_loop() {
    let directory = capture("objects-boot-002");
    let entry = ram(&directory.join("entry.ram.bin"));
    let mut m = machine(&entry);
    let payload = payload();
    assert!(boot::boot_until(&mut m, &payload, BootStop::FirstPresent));
    boot::init_audio_attributes(&mut m);
    boot::init_resources(&mut m);
    boot::init_sound(&mut m);
    dispatch::call(&mut m, 0x8002_c7a4, [0; 4]);
    boot::set_master_volume(&mut m, 1);
    m.take_fault().unwrap();
    let expected = ram(&directory.join("main-loop.ram.bin"));
    let writers = writers(&directory).unwrap();
    let trace = trace();
    let mine = [
        0x8002_5990,
        0x8002_b8b0,
        0x8002_ac0c,
        0x8002_95b8,
        0x8002_a9e4,
        0x8002_a8ac,
        0x8002_a968,
        0x8002_d334,
        0x8002_e1e8,
        0x8002_d374,
        0x8002_ed24,
        0x8002_ed5c,
        0x8002_d5f8,
        0x8002_ee38,
        0x8002_c7a4,
        0x8002_d458,
    ];
    let mut compared = 0;
    for i in 0x1_0000..0x1f_e000 {
        let address = 0x8000_0000 + i as u32;
        let pc = writers[i];
        let function = match trace.functions.binary_search(&pc) {
            Ok(k) => trace.functions[k],
            Err(0) => 0,
            Err(k) => trace.functions[k - 1],
        };
        // CD header reads (CD_getsector) and the Setloc status bytes.
        let headers = (0x800b_f1d0..0x800c_31d0).contains(&address)
            || (0x800b_f178..0x800b_f180).contains(&address);
        if (mine.contains(&function) || headers) && excluded(address, PAD_INPUT).is_none() {
            compared += 1;
            assert_eq!(m.ram()[i], expected[i], "{address:08x} (writer {pc:08x})");
        }
    }
    eprintln!("{compared} main-loop bytes from the continuation routines match");
    assert!(compared > 16_000);
}
