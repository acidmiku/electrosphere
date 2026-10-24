//! Resident state-2 loading (`ac3_flight::loading`) against original
//! entry/return snapshots, plus the end-to-end arena check.
//!
//! Captures (normal CROSS inputs from `menu-runtime/mission-006/final.sav`):
//! - `loading-m002`: window 800355F0 (frame 82) and its callees.
//! - `loading-m003`: 18 of the first 160 800354B8 windows (frames 82..305) and
//!   two snapshots of every loading-screen leaf.
//! - `loading-m004`: window 80035730 (frames 216..280) and its callees.
//! - `loading-m005`: window 80035C10 (frame 281).
//! - `loading-m006`: window 80035C90 (frames 298..304) and its callees.
//!
//! Callbacks that run without interrupts are compared exactly with
//! `snapshot::check_call` (all RAM outside the stack, scratchpad, GTE, v0).
//! The long callbacks (SPU reverb clear, sample upload, texture uploads and
//! expansion spanning many VBlank, sound-timer, DMA and CD interrupts) are
//! compared with the same strictness except for the RAM listed in
//! [`INTERRUPT_OWNED`], which only interrupt handlers, the BIOS and the
//! timing-dependent libgpu queue write.
use ac3_flight::{
    Machine, dispatch, loading,
    machine::{Device, HostCall},
    snapshot::{self, Snapshot},
};
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR"))
}
fn capture(name: &str) -> PathBuf {
    root()
        .join("analysis/flight-runtime")
        .join(name)
        .join("snapshots.jsonl")
}

/// RAM written during the long callbacks by code other than the callbacks'
/// own call tree. Each range was identified from the capture call tree
/// (`loading-m001`) and the routines that address it:
const INTERRUPT_OWNED: &[(u32, u32, &str)] = &[
    // BIOS exception dispatch, event control blocks and the DeliverEvent /
    // WaitEvent / TestEvent services (BIOS ROM code; kernel RAM).
    (0x8000_0000, 0x8001_0000, "BIOS kernel RAM"),
    // Set by the sound-timer interrupt handler 80099F8C (80099FCC), cleared
    // by the sound driver (8009A12C).
    (0x800b_256c, 0x800b_2570, "sound interrupt flag"),
    // libetc interrupt manager 800A090C/800A09E4: context at 800B2F2C and
    // its private interrupt stack up to 800B3F44 (handler frames).
    (
        0x800b_2f2c,
        0x800b_3f44,
        "libetc interrupt context and stack",
    ),
    // VBlank counter incremented by the VBlank handler 800A0E90 (800A0EBC).
    (0x800b_3ff4, 0x800b_3ff8, "VSync counter"),
    // libetc DMA-2 (GPU) completion callback, installed by the libgpu
    // enqueue path (800A07D8(2, 800A86AC)) and removed when the queue drains.
    (0x800b_4010, 0x800b_4014, "GPU DMA callback slot"),
    // libgpu queue indices: the original enqueued transfers issued while a
    // previous DMA was still running and drained them from the DMA
    // interrupt; the host GPU finishes each transfer before returning.
    (0x800b_990c, 0x800b_9914, "libgpu queue head/tail"),
    // libgpu timeout (VSync counter at the last arm + 240, advanced by the
    // VBlank interrupt meanwhile) and DrawSync's wait-loop count.
    (0x800b_9920, 0x800b_9928, "libgpu timeout"),
    // Sound driver state updated on every sound-timer tick (8009A768/8009CDDC
    // store 800C5C70; 800C5BCC lies in the driver's voice state).
    (0x800c_5bcc, 0x800c_5bce, "sound driver tick state"),
    (0x800c_5c70, 0x800c_5c72, "sound driver tick state"),
    // libgpu queue entries (64 x 0x60 from 800C8230) for the queued transfers.
    (0x800c_8230, 0x800c_9a30, "libgpu queue entries"),
];
fn owner(address: u32) -> Option<&'static str> {
    INTERRUPT_OWNED
        .iter()
        .find(|(start, end, _)| (*start..*end).contains(&address))
        .map(|(_, _, name)| *name)
}

struct Pair {
    entry: Snapshot,
    exit: Snapshot,
}
fn pair(index: &Path, callee: u32, occurrence: usize) -> Pair {
    let directory = index.parent().unwrap();
    let all = snapshot::read_index(index).unwrap();
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
        .unwrap_or_else(|| panic!("{callee:08x}: no return"));
    Pair {
        entry: Snapshot::load(directory, entry).unwrap(),
        exit: Snapshot::load(directory, exit).unwrap(),
    }
}
fn arguments(entry: &Snapshot) -> [u32; 4] {
    std::array::from_fn(|i| entry.gpr(4 + i))
}

/// Exact check of the routine `callee` through `dispatch::call` (with the
/// test [`Host`] device attached).
fn exact(index: &Path, callee: u32, occurrence: usize, void: bool) -> usize {
    snapshot::check_call(index, callee, occurrence, |m, entry| {
        attach_host(m);
        let v0 = dispatch::call(m, callee, arguments(entry));
        (!void).then_some(v0)
    })
    .unwrap_or_else(|e| panic!("{callee:08x}#{occurrence}: {e}"))
}

struct Tolerant {
    /// Bytes the original changed (entry vs return, outside the stack).
    changed: usize,
    /// Differing bytes inside [`INTERRUPT_OWNED`], per owner.
    excluded: Vec<(&'static str, usize)>,
}
/// Same comparison as `check_call`, except that differences inside
/// [`INTERRUPT_OWNED`] are counted instead of failing. `prepare` may supply
/// a device (StoreImage VRAM) before the run; `check_v0` compares v0.
fn tolerant(
    index: &Path,
    callee: u32,
    occurrence: usize,
    check_v0: bool,
    prepare: impl FnOnce(&mut Machine),
) -> Tolerant {
    let p = pair(index, callee, occurrence);
    let mut m = p.entry.machine();
    prepare(&mut m);
    let v0 = dispatch::call(&mut m, callee, arguments(&p.entry));
    m.take_fault()
        .unwrap_or_else(|f| panic!("{callee:08x}#{occurrence}: {f}"));
    let d = snapshot::compare(&m, &p.exit);
    assert!(
        d.scratch.is_empty() && d.gte_data.is_empty() && d.gte_control.is_empty(),
        "{callee:08x}#{occurrence}: {}",
        d.summary()
    );
    let foreign: Vec<u32> = d
        .ram
        .iter()
        .copied()
        .filter(|&a| owner(a).is_none())
        .collect();
    assert!(
        foreign.is_empty(),
        "{callee:08x}#{occurrence}: {} bytes outside interrupt-owned RAM: {:08x?}",
        foreign.len(),
        &foreign[..foreign.len().min(16)]
    );
    // The translation itself never wrote these bytes (they keep their entry
    // value), except the libgpu timeout it arms from the VSync counter.
    let touched: Vec<u32> = d
        .ram
        .iter()
        .copied()
        .filter(|&a| {
            let i = (a & 0x1f_ffff) as usize;
            owner(a) != Some("libgpu timeout") && m.ram()[i] != p.entry.ram[i]
        })
        .collect();
    assert!(
        touched.is_empty(),
        "{callee:08x}#{occurrence}: translation wrote interrupt-owned RAM {:08x?}",
        &touched[..touched.len().min(16)]
    );
    if check_v0 {
        assert_eq!(v0, p.exit.gpr(2), "{callee:08x}#{occurrence}: v0");
    }
    let mut excluded: Vec<(&'static str, usize)> = vec![];
    for name in d.ram.iter().filter_map(|&a| owner(a)) {
        match excluded.iter_mut().find(|(n, _)| *n == name) {
            Some((_, count)) => *count += 1,
            None => excluded.push((name, 1)),
        }
    }
    Tolerant {
        changed: snapshot::changed_bytes(&p.entry, &p.exit),
        excluded,
    }
}
fn report(name: &str, t: &Tolerant) {
    println!(
        "{name}: original changed {} bytes; interrupt-owned differences {:?}",
        t.changed, t.excluded
    );
}

/// The VRAM rectangle the original StoreImage (800A6970 inside 8005D490)
/// delivered: its filled stack buffer 801FEF78 at the first 8005D3F8 entry.
fn palette_source() -> Vec<u8> {
    let p = pair(&capture("loading-m004"), 0x8005_d3f8, 0);
    let at = (0x801f_ef78u32 & 0x1f_ffff) as usize;
    p.entry.ram[at..at + 0x800].to_vec()
}
/// Test device: SetIntrMask (800A08F4) returns the previous interrupt mask
/// (initially the one libgpu last saved at 800B9914, which it restores
/// after every submission), and StoreImage optionally receives the
/// original's VRAM words.
struct Host {
    mask: u32,
    vram: Option<Vec<u8>>,
}
impl Device for Host {
    fn call(&mut self, ram: &mut [u8], _: &mut [u8], call: HostCall) -> Option<u32> {
        match call.function {
            0x800a_08f4 => {
                let previous = self.mask;
                self.mask = call.args[0];
                Some(previous)
            }
            0x800a_6970 => {
                if let Some(vram) = &self.vram {
                    let at = (call.args[1] & 0x1f_ffff) as usize;
                    ram[at..at + vram.len()].copy_from_slice(vram);
                }
                None
            }
            _ => None,
        }
    }
}
fn attach_host(m: &mut Machine) {
    let mask = m.lw(0x800b_9914);
    m.device = Some(Box::new(Host { mask, vram: None }));
}
fn attach_vram(m: &mut Machine) {
    let mask = m.lw(0x800b_9914);
    m.device = Some(Box::new(Host {
        mask,
        vram: Some(palette_source()),
    }));
}

// ---- substate 0 ----------------------------------------------------------------

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/loading-m002"]
fn initialize_800355f0_and_callees() {
    let index = capture("loading-m002");
    // Leaves without interrupts: exact.
    assert!(exact(&index, 0x8002_c280, 0, true) > 0, "queue reset");
    assert_eq!(exact(&index, 0x8002_c214, 0, false), 0, "size (v0 checked)");
    assert!(
        exact(&index, 0x8002_c404, 0, false) > 0,
        "queue mission read"
    );
    assert!(exact(&index, 0x8003_6668, 0, true) > 0);
    assert!(exact(&index, 0x8003_6d00, 0, true) > 0);
    assert!(exact(&index, 0x8003_54b8, 0, false) > 0, "first poll");
    // Sound reset: the reverb-area clear waits for 19 SPU DMA interrupts.
    let t = tolerant(&index, 0x8002_d6cc, 0, false, attach_host);
    report("8002D6CC", &t);
    let t = tolerant(&index, 0x8003_55f0, 0, true, attach_host);
    report("800355F0", &t);
    assert!(t.changed > 100);
}

// ---- substates 1 and 4 ---------------------------------------------------------------

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/loading-m003"]
fn poll_800354b8_every_captured_window() {
    let index = capture("loading-m003");
    let mut frames = vec![];
    for occurrence in 0..18 {
        let p = pair(&index, 0x8003_54b8, occurrence);
        frames.push((p.entry.info.frame, p.exit.gpr(2)));
        let changed = exact(&index, 0x8003_54b8, occurrence, false);
        assert!(changed > 0, "poll #{occurrence}");
    }
    println!("poll frames/results: {frames:x?}");
    // Waiting, completion (frame 215), the tails inside 35730/35C10, the
    // aircraft stage and its completion (frame 297) are all covered.
    assert!(frames.iter().any(|&(f, v)| f == 215 && v == 0x0400_0000));
    assert!(frames.iter().any(|&(f, v)| f == 297 && v == 0x0400_0000));
    assert!(frames.iter().any(|&(f, v)| f == 286 && v == 0));
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/loading-m003"]
fn loading_screen_leaves() {
    let index = capture("loading-m003");
    let mut total = 0;
    for (callee, void, count) in [
        (0x8003_6d98, true, 2),
        (0x8003_6b20, false, 2),
        (0x8003_64d0, false, 2),
        (0x8003_5144, false, 2),
        (0x8003_4f7c, false, 2),
        (0x8003_53d8, false, 2),
        (0x8003_52bc, false, 2),
        (0x8003_5384, false, 2),
        (0x8003_43d0, false, 2),
        (0x8008_3160, false, 2),
        (0x8008_2cd0, false, 2),
        (0x8003_5f5c, false, 2),
        (0x8003_6d00, true, 1),
        (0x8003_6668, true, 1),
        (0x8002_c604, false, 2),
        (0x8002_c6a8, false, 2),
    ] {
        for occurrence in 0..count {
            total += exact(&index, callee, occurrence, void);
        }
    }
    println!("loading-screen leaves: {total} original bytes reproduced");
    assert!(total > 3000);
}

// ---- substate 2 -------------------------------------------------------------------

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/loading-m004"]
fn prepare_mission_callees() {
    let index = capture("loading-m004");
    let mut total = 0;
    for (callee, void, occurrence) in [
        (0x8002_6e98, false, 0),
        (0x8002_6e98, false, 1),
        (0x800a_e118, false, 1),
        (0x800a_6910, false, 0),
        (0x800a_9218, false, 0),
        (0x800a_9218, false, 1),
        (0x800a_9228, false, 0),
        (0x8002_6ee0, false, 0),
        (0x8002_6ee0, false, 1),
        (0x800a_6970, false, 0),
        (0x800a_45c8, false, 0),
        (0x800a_45c8, false, 1),
        (0x8005_d3f8, true, 0),
        (0x8005_d3f8, true, 1),
        (0x8003_54b8, false, 0),
    ] {
        total += exact(&index, callee, occurrence, void);
    }
    println!("mission leaves (exact): {total} original bytes reproduced");
    // Interrupts (VBlank, sound timer, GPU DMA) inside these.
    for (callee, check_v0, occurrence) in [
        (0x8002_d4e8, true, 0),
        (0x8002_d544, true, 0),
        (0x800a_e118, true, 0),
        (0x8002_6d8c, false, 0),
        (0x8002_6d8c, false, 1),
        (0x8002_6e50, true, 0),
        (0x8002_6e50, true, 1),
        // The original queued this CLUT upload behind a running DMA, so its
        // LoadImage returned the queue depth 1; the host transfer returns 0.
        (0x800a_6910, false, 1),
        (0x800a_9228, true, 1),
        (0x8002_70fc, true, 0),
    ] {
        let t = tolerant(&index, callee, occurrence, check_v0, attach_host);
        report(&format!("{callee:08X}#{occurrence}"), &t);
    }
    let t = tolerant(&index, 0x8005_d490, 0, false, attach_vram);
    report("8005D490", &t);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/loading-m004"]
fn prepare_mission_80035730() {
    let index = capture("loading-m004");
    let t = tolerant(&index, 0x8003_5730, 0, true, attach_vram);
    report("80035730", &t);
    assert!(t.changed > 800_000);
}

// ---- substates 3 and 5 ------------------------------------------------------------

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/loading-m005"]
fn queue_aircraft_80035c10() {
    let index = capture("loading-m005");
    assert_eq!(exact(&index, 0x8002_c214, 0, false), 0);
    assert!(exact(&index, 0x8002_c404, 0, false) > 0);
    assert!(exact(&index, 0x8003_54b8, 0, false) > 0);
    let changed = exact(&index, 0x8003_5c10, 0, false);
    println!("80035C10: {changed} original bytes reproduced");
    assert!(changed > 100);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/loading-m006"]
fn prepare_aircraft_80035c90() {
    let index = capture("loading-m006");
    let mut total = 0;
    for (callee, occurrence) in [
        (0x8002_6e98, 0),
        (0x8002_6e98, 1),
        (0x8002_70fc, 0),
        (0x8002_6ee0, 0),
        (0x8002_6ee0, 1),
        (0x8003_54b8, 0),
    ] {
        total += exact(&index, callee, occurrence, false);
    }
    println!("aircraft leaves (exact): {total} original bytes reproduced");
    for (callee, occurrence) in [
        (0x8002_d544, 0),
        (0x800a_e118, 0),
        (0x800a_e118, 1),
        (0x8002_6e50, 0),
    ] {
        let t = tolerant(&index, callee, occurrence, true, attach_host);
        report(&format!("{callee:08X}#{occurrence}"), &t);
    }
    let t = tolerant(&index, 0x8003_5c90, 0, true, attach_host);
    report("80035C90", &t);
    assert!(t.changed > 60_000);
}

// ---- end to end ---------------------------------------------------------------------

const ARENA_BASE: u32 = 0x800c_9d98;
const ARENA_END: u32 = 0x8013_654c;
fn ram_range(ram: &[u8], start: u32, end: u32) -> &[u8] {
    &ram[(start & 0x1f_ffff) as usize..(end & 0x1f_ffff) as usize]
}
/// Byte-exact range comparison listing the first differing addresses.
fn assert_same(native: &[u8], original: &[u8], start: u32, end: u32, what: &str) {
    let (a, b) = (
        ram_range(native, start, end),
        ram_range(original, start, end),
    );
    let differing: Vec<u32> = (0..a.len())
        .filter(|&i| a[i] != b[i])
        .map(|i| start + i as u32)
        .collect();
    assert!(
        differing.is_empty(),
        "{what}: {} of {} bytes differ, first {:08x?}",
        differing.len(),
        a.len(),
        &differing[..differing.len().min(24)]
    );
}

/// From the original state at 800355F0 entry (arena overwritten with a
/// garbage pattern), run substates 0, 2, 3 and 5 natively, delivering both
/// CD reads with `complete_cd_read` from ACE.BPB where the original's data
/// arrives. The read bytes, the completion state and the final state-2
/// arena must equal the original's.
#[test]
#[ignore = "requires AC3_DATA_DIR, extracted/disc/ACE.BPB and loading-m002/m004/m005/m006"]
fn state2_arena_built_from_files() {
    let bpb = std::fs::read(root().join("extracted/disc/ACE.BPB")).unwrap();
    let init = pair(&capture("loading-m002"), 0x8003_55f0, 0);
    let mission = pair(&capture("loading-m004"), 0x8003_5730, 0);
    let queue = pair(&capture("loading-m005"), 0x8003_5c10, 0);
    let aircraft = pair(&capture("loading-m006"), 0x8003_5c90, 0);
    let mut m = init.entry.machine();
    for address in (ARENA_BASE..0x801f_b700).step_by(4) {
        m.sw(address, 0xa5a5_5a5a);
    }

    assert_eq!(loading::initialize(&mut m), loading::ADVANCE);
    let read = loading::complete_cd_read(&mut m, &bpb).unwrap();
    println!("mission read {read:x?}");
    assert_eq!(
        (read.file, read.destination, read.size),
        (7, ARENA_BASE, 0xa_299c)
    );
    // ACE.BPH lists resource 7 at ACE.BPB sector 729; ACE.BPB starts at 469.
    assert_eq!((read.sector, read.archive_sector), (1198, 469));
    let end = read.destination + read.size;
    assert_same(
        m.ram(),
        &mission.entry.ram,
        read.destination,
        end,
        "mission file bytes at 80035730 entry",
    );
    // Completion state as the original left it for 80035730.
    for (address, length) in [
        (0x800c_3420, 4),    // queue head
        (0x800c_34dc, 0x14), // the read entry (type 2, started)
        (0x800c_3568, 4),    // control request retired
        (0x800c_3588, 0x1c), // done flag, status, active file, extent
        (0x800b_f198, 4),    // driver position at the extent end
        (0x800c_4670, 4),    // loading state: pending cleared by 800354AC
    ] {
        assert_eq!(
            ram_range(m.ram(), address, address + length),
            ram_range(&mission.entry.ram, address, address + length),
            "completion state at {address:08x}"
        );
    }
    attach_vram(&mut m);
    assert_eq!(loading::prepare_mission(&mut m), loading::ADVANCE);
    assert_eq!(
        ram_range(m.ram(), ARENA_BASE, 0x8013_08bc),
        ram_range(&mission.exit.ram, ARENA_BASE, 0x8013_08bc),
        "expanded mission resource"
    );

    assert_eq!(loading::queue_aircraft(&mut m), loading::ADVANCE);
    assert_eq!(
        ram_range(m.ram(), 0x800c_3420, 0x800c_3424 + 4),
        ram_range(&queue.exit.ram, 0x800c_3420, 0x800c_3424 + 4)
    );
    let read = loading::complete_cd_read(&mut m, &bpb).unwrap();
    println!("aircraft read {read:x?}");
    assert_eq!(
        (read.file, read.destination, read.size),
        (0x30, 0x8013_08bc, 0xe19c)
    );
    let end = read.destination + read.size;
    assert_same(
        m.ram(),
        &aircraft.entry.ram,
        read.destination,
        end,
        "aircraft file bytes at 80035C90 entry",
    );
    assert_eq!(loading::prepare_aircraft(&mut m), loading::ADVANCE);
    m.take_fault().unwrap();

    let native = ram_range(m.ram(), ARENA_BASE, ARENA_END);
    let original = ram_range(&aircraft.exit.ram, ARENA_BASE, ARENA_END);
    let differing: Vec<u32> = (0..native.len())
        .filter(|&i| native[i] != original[i])
        .map(|i| ARENA_BASE + i as u32)
        .collect();
    println!(
        "state-2 arena {ARENA_BASE:08x}..{ARENA_END:08x}: {} bytes, {} differ",
        native.len(),
        differing.len()
    );
    assert!(
        differing.is_empty(),
        "arena differs at {:08x?}",
        &differing[..differing.len().min(16)]
    );
    // The bindings that point into it, and the arena pointers.
    for (address, length, what) in [
        (0x800b_e168 + 8, 0x3c, "resource bindings +08..+40"),
        (
            0x800b_e168 + 0x5c,
            8,
            "resource arena top +5C, file list +60",
        ),
        (0x800b_d158 + 0x18, 8, "arena base/current"),
        (0x800b_d158 + 0xf80, 0xc, "context counts F80/F84/F88"),
    ] {
        assert_eq!(
            ram_range(m.ram(), address, address + length),
            ram_range(&aircraft.exit.ram, address, address + length),
            "{what}"
        );
    }
}

// ---- capture-independent paths ------------------------------------------------------

fn machine_with_queue() -> Machine {
    let mut m = Machine::default();
    let gp = ac3_flight::GP;
    m.sw(gp + 0x48, 0x800c_3420);
    m.sw(gp + 0x34, 0x800b_f150);
    m.sw(gp + 0x38, 0x800b_f180);
    m.sw(gp + 0x150, 0x800c_4668);
    m.sw(0x800b_f150 + 0x18, 0x800b_f1e4);
    // File 2: 3 words at sector 1.
    m.sw(0x800b_f1e4 + 16, 3);
    m.sw(0x800b_f1e4 + 20, 1);
    m
}

#[test]
fn cd_read_request_and_host_completion() {
    let mut m = machine_with_queue();
    assert_eq!(
        loading::pending_cd_read(&mut m),
        Err(loading::CdReadError::Idle)
    );
    m.sw(0x800c_4668 + 8, 1);
    m.sw(0x8010_0000, 2);
    let end = loading::queue_files(&mut m, 0x8010_0000, 1, 0x8011_0000, loading::READ_COMPLETE);
    assert_eq!(end, 0x8011_000c);
    assert_eq!(
        m.lw(0x8010_0000),
        0x8011_0000,
        "list slot replaced by destination"
    );
    assert_eq!(
        m.host_calls.last().unwrap().args,
        [2, 0x8011_0000, loading::READ_COMPLETE, 12]
    );
    let mut bpb = vec![0u8; 4096];
    bpb[2048..2060].copy_from_slice(b"ABCDEFGHIJKL");
    let read = loading::complete_cd_read(&mut m, &bpb).unwrap();
    assert_eq!((read.sector, read.size), (1, 12));
    assert_eq!(m.read_bytes(0x8011_0000, 12), b"ABCDEFGHIJKL");
    assert_eq!(m.lw(0x800c_4668 + 8), 0, "800354AC cleared pending");
    assert_eq!(m.lw(0x800c_3420), 1, "head advanced");
    assert_eq!(m.lw(0x800c_3420 + 0x174), u32::MAX);
    assert_eq!(m.lw(0x800b_f180 + 0x18), 0x8011_000c);
    assert_eq!(
        loading::pending_cd_read(&mut m),
        Err(loading::CdReadError::Idle)
    );
    // A sector beyond the body and a stream file are rejected.
    m.sw(0x800b_f1e4 + 20, 9);
    loading::queue_read(&mut m, 2, 0x8011_0000, 0);
    assert!(matches!(
        loading::complete_cd_read(&mut m, &bpb),
        Err(loading::CdReadError::OutsideBody { .. })
    ));
    let mut m = machine_with_queue();
    loading::queue_read(&mut m, 0x802, 0x8011_0000, 0);
    assert_eq!(
        loading::pending_cd_read(&mut m),
        Err(loading::CdReadError::StreamFile { file: 0x802 })
    );
}

#[test]
fn progress_is_clamped_and_guards_empty_extents() {
    let mut m = machine_with_queue();
    m.sw(0x800c_3420 + 0x17c, 0x8011_0000);
    m.sw(0x800c_3420 + 0x180, 0x8011_0000);
    m.sw(0x800b_f180 + 0x18, 0x8011_0100);
    assert_eq!(loading::read_progress(&mut m, 0), 0, "empty extent");
    m.sw(0x800c_3420 + 0x180, 0x8011_0400);
    assert_eq!(loading::read_progress(&mut m, 0x8012_0000), 0x400);
    assert_eq!(m.lw(0x8012_0000), 0x8011_0100);
    m.sw(0x800b_f180 + 0x18, 0x8011_0800);
    assert_eq!(loading::read_progress(&mut m, 0), 0x1000, "clamped");
}

/// Kind-0/1/2 blocks built by hand: literals, overlapping back-references
/// and the word-buffered literals of kind 2.
#[test]
fn decompressor_kinds() {
    /// `(kind, offset bits, byte count)`, flag words, literals, pairs.
    fn block(
        m: &mut Machine,
        at: u32,
        (kind, bits, header4): (u8, u8, u32),
        flags: &[u32],
        literals: &[u8],
        pairs: &[u16],
    ) {
        let literal_at = 0x10 + 4 * flags.len() as u32;
        let pair_at = (literal_at + literals.len() as u32 + 3) & !3;
        m.sw(at + 4, header4 | (u32::from(kind) << 24));
        m.sw(at + 8, literal_at | (u32::from(bits) << 24));
        m.sw(at + 0xc, pair_at);
        for (i, f) in flags.iter().enumerate() {
            m.sw(at + 0x10 + 4 * i as u32, *f);
        }
        m.write_bytes(at + literal_at, literals);
        for (i, p) in pairs.iter().enumerate() {
            m.sh(at + pair_at + 2 * i as u32, *p);
        }
    }
    let mut m = Machine::default();
    // Kind 0: literal 'a', 'b', then copy distance 2 length 4 ("abab"),
    // literal 'c'; flags 1,1,0,1 + sentinel, then a zero word.
    // The resident kind table (800AE140).
    for (i, handler) in [0x800a_e150u32, 0x800a_e1f4, 0x800a_e2a0]
        .into_iter()
        .enumerate()
    {
        m.sw(0x800a_e140 + 4 * i as u32, handler);
    }
    block(
        &mut m,
        0x8001_0000,
        (0, 4, 0),
        &[0b1_1011 << 27, 0],
        b"abc",
        &[(1 << 4) | 1],
    );
    let end = loading::decompress(&mut m, 0x8001_0000, 0x8002_0000);
    assert_eq!(
        m.read_bytes(0x8002_0000, (end - 0x8002_0000) as usize),
        b"abababc"
    );
    // Kind 1: halfword literal 0x1234, then copy distance 2 bytes, 3 halfwords.
    block(
        &mut m,
        0x8003_0000,
        (1, 5, 0),
        &[0b101 << 29, 0],
        &[0x34, 0x12],
        &[1 << 4],
    );
    let end = loading::decompress(&mut m, 0x8003_0000, 0x8004_0000);
    assert_eq!(end - 0x8004_0000, 8);
    assert_eq!(
        m.read_bytes(0x8004_0000, 8),
        [0x34, 0x12, 0x34, 0x12, 0x34, 0x12, 0x34, 0x12]
    );
    // Kind 2: six literals spanning two literal words, then a 3-byte copy;
    // the byte count (9) ends the stream mid flag word.
    block(
        &mut m,
        0x8005_0000,
        (2, 4, 9),
        &[0b1111_1100 << 24],
        b"uvwxyz\0\0",
        &[2],
    );
    let end = loading::decompress(&mut m, 0x8005_0000, 0x8006_0000);
    assert_eq!(
        m.read_bytes(0x8006_0000, (end - 0x8006_0000) as usize),
        b"uvwxyzxyz"
    );
    assert!(m.take_fault().is_ok());
}

#[test]
fn unclaimed_modes_and_dispatch() {
    let mut m = Machine::default();
    assert!(
        loading::dispatch(&mut m, 0x8003_6724, [0; 4]).is_none(),
        "mode 0 is not translated"
    );
    assert!(loading::dispatch(&mut m, 0x8003_5730, [0; 4]).is_some());
    // Unknown decompressor kinds fault instead of guessing.
    m.sw(0x800a_e140 + 12, 0);
    m.sb(0x8001_0007, 3);
    loading::decompress(&mut m, 0x8001_0000, 0x8002_0000);
    assert!(m.take_fault().is_err());
}

/// Paths the first mission does not take: a subtracting fade, an empty
/// aperture rectangle and the replay index wrap.
#[test]
fn screen_and_replay_edge_paths() {
    let mut m = Machine::default();
    let ot = 0x8010_0000;
    m.sw(ot, 0x00ff_ffff);
    let end = loading::fade(&mut m, ot, 0x8011_0000, (-0x20i32) as u32);
    assert_eq!(end, 0x8011_0030);
    assert_eq!(m.lw(0x8011_0010), 0xe100_0240, "subtractive blend");
    assert_eq!(m.lw(0x8011_0018), 0x6220_2020, "magnitude grey");
    // ot -> E6 on -> E1 -> TILE -> E6 off -> previous link.
    assert_eq!(m.lw(ot) & 0xff_ffff, 0x11_0000);
    assert_eq!(m.lw(0x8011_0000) & 0xff_ffff, 0x11_000c);
    assert_eq!(m.lw(0x8011_000c) & 0xff_ffff, 0x11_0014);
    assert_eq!(m.lw(0x8011_0014) & 0xff_ffff, 0x11_0024);
    assert_eq!(m.lw(0x8011_0024), 0x02ff_ffff);
    m.sh(0x8012_0004, 0);
    m.sh(0x8012_0006, 10);
    assert_eq!(
        loading::draw_aperture(&mut m, ot, 0x8013_0000, 0x8012_0000, 0),
        0x8013_0000,
        "empty rectangle"
    );
    m.sw(ac3_flight::GP + 0x2dc, 0x800b_d158);
    m.sb(0x800b_d158 + 0x3aa, 16);
    m.sw(0x800a_f7c8 + 4, 0x0203_0000);
    loading::select_replay(&mut m);
    assert_eq!(m.lbu(0x800b_d158 + 0x3aa), 1, "index 16 restarts at 1");
    assert_eq!(
        (m.lh(0x800b_d158 + 0x390), m.lh(0x800b_d158 + 0x392)),
        (3, 2)
    );
    assert!(m.take_fault().is_ok());
}
