//! Validation of the patch-layer routines of part `resident-part2` (JP
//! resident routines whose PAL counterparts are in `ai`, `boot`, `entities`,
//! `init`, `init_38818`, `loading`, `render` and `script`).
//!
//! * `patched_routines_match_the_original_on_captured_entries`: every
//!   patched routine with a JP capture runs from its captured entry states
//!   as the original JP instructions (the oracle, `support::oracle`) and as
//!   the translation; calls out of the routine run natively in both, so the
//!   routine is compared in isolation from callees that other parts own.
//! * `patched_routines_reproduce_their_captures` (also with every other
//!   routine as the original instructions): the whole captured call against
//!   the return snapshot (as `tests/captures.rs`), with the development
//!   fallback interpreting untranslated callees (feature `dev-fallback`), the
//!   JP disc attached for the CD routines, and the bytes only kernel,
//!   SDK-driver and interrupt code own excluded and listed
//!   ([`DEVICE_STATE`]).
//! * The `oracle_*` tests: routines no capture reaches (and captured ones
//!   whose capture takes one path), against the oracle on JP RAM images with
//!   the fields that select their paths varied.
//!
//! JP data from `AC3_JP_DIR` (default `~/ac3-jp`), captures from
//! `AC3_JP_CAPTURES` (default `AC3_JP_DIR/analysis/redux`). Run with
//! `cargo test --release -p ac3-flight-jp --features dev-fallback --test
//! res_p2 -- --include-ignored`.
mod support;

use ac3_flight_jp::{
    Fault, Machine, boot, cdstream, dispatch,
    snapshot::{self, Snapshot},
};
use std::path::PathBuf;
use support::{oracle, scan};

fn captures() -> PathBuf {
    std::env::var_os("AC3_JP_CAPTURES")
        .map(PathBuf::from)
        .unwrap_or_else(|| scan::jp_dir().join("analysis/redux"))
}

/// JP disc 1 (Rev 1), the raw 2352-byte-sector image.
fn disc_path() -> PathBuf {
    scan::jp_dir().join("discs/Ace Combat 3 - Electrosphere (Japan, Asia) (Disc 1) (Rev 1).bin")
}

/// The libcd drive (commands, interrupts, sectors) in front of the boot
/// device that answers CdSearchFile/CdRead, both on JP disc 1 (the PAL
/// `tests/boot.rs` setup).
fn disc() -> Box<cdstream::CdDrive<cdstream::RawImage<std::fs::File>>> {
    let open = || std::fs::File::open(disc_path()).expect("JP disc 1");
    let mut drive = cdstream::CdDrive::new(cdstream::RawImage(open()), cdstream::CLOCK);
    // NTSC: 60 VBlanks per second.
    drive.vblank_rate = 60;
    drive.inner = Some(Box::new(boot::DiscDevice::new(open(), boot::CD_FILE_CACHE)));
    Box::new(drive)
}

/// With `dev-fallback`, routines nobody translated yet (other parts' JP-only
/// routines) run interpreted on this thread, in the original and the
/// translation alike. `RES_P2_FORCE` (a `Fallback::force` spec, e.g.
/// `owner:hud`) also runs translated routines interpreted: a debug aid to
/// tell this part's differences from its callees'.
fn install_fallback() {
    #[cfg(feature = "dev-fallback")]
    {
        let mut fallback = ac3_fallback::Fallback::new(ac3_fallback::Version::Jp);
        if let Ok(spec) = std::env::var("RES_P2_FORCE") {
            let n = fallback.force(&spec).expect("RES_P2_FORCE");
            eprintln!("RES_P2_FORCE {spec}: {n} routines interpreted");
        }
        ac3_fallback::install(fallback);
    }
}

/// RAM that only kernel, SDK-driver and interrupt code write (inclusive-
/// exclusive, with the reason): the PAL `tests/boot.rs` list at its JP
/// addresses (the gp-area run +0x2180, the BSS runs +0x2190 and +0x2228).
/// The translation makes those device routines host calls and the native
/// host delivers no interrupts inside a routine, so their internal state is
/// not produced; only SDK code reads it.
const DEVICE_STATE: &[(u32, u32, &str)] = &[
    (
        0x8000_0000,
        0x8001_0000,
        "kernel area (BIOS/kernel data, kernel patches)",
    ),
    (0x800b_4684, 0x800b_470c, "libpad driver state"),
    (0x800b_474c, 0x800b_4830, "libspu driver state"),
    (0x800b_5044, 0x800b_504c, "libetc VSync state"),
    (
        0x800b_50ec,
        0x800b_50f0,
        "libetc interrupt manager's setjmp buffer: the caller's fp",
    ),
    (
        0x800b_50f4,
        0x800b_5110,
        "libetc interrupt manager's setjmp buffer: the caller's s1..s7",
    ),
    (
        0x800b_5114,
        0x800b_60c4,
        "libetc interrupt handlers' stack (interrupt frames; the host's)",
    ),
    (
        0x800b_6174,
        0x800b_6178,
        "libetc VBlank counter (VBlanks the host delivered)",
    ),
    (
        0x800b_6190,
        0x800b_6194,
        "DMA callback 2 (libgpu's around queued transfers)",
    ),
    (
        0x800b_6198,
        0x800b_619c,
        "DMA callback 4 (libspu's transfers)",
    ),
    (
        0x800b_61b8,
        0x800b_6280,
        "libcd state (CdInit, callbacks, debug level, command bytes)",
    ),
    (0x800b_6538, 0x800b_65c0, "libcd file and read state"),
    (0x800b_b96c, 0x800b_b970, "libgpu submission flag"),
    (
        0x800b_ba8c,
        0x800b_baa8,
        "libgpu queue indices, saved interrupt masks and DMA timeout",
    ),
    (0x800b_cae4, 0x800b_caec, "libc stdout state"),
    (0x800c_7bf8, 0x800c_7ee4, "libpad, libspu and libcd BSS"),
    (
        0x800c_7ef8,
        0x800c_852c,
        "libcd file cache and ISO9660 directory state",
    ),
    (0x800c_9af8, 0x800c_a2f8, "libcd directory sector buffer"),
    (0x800c_a350, 0x800c_bc58, "libgpu command queue"),
    (0x800c_bc78, 0x800c_bc7c, "libcard state"),
    (0x800c_bf68, 0x800c_bf88, "libc putchar buffer"),
    (
        0x800c_0418,
        0x800c_045c,
        "pad receive buffers (`[gp+0x10]`: controller input, written in VSync interrupts)",
    ),
];

fn excluded(address: u32, ranges: &[(u32, u32, &'static str)]) -> Option<&'static str> {
    ranges
        .iter()
        .find(|(start, end, _)| (*start..*end).contains(&address))
        .map(|&(_, _, reason)| reason)
}

/// The routines of part `resident-part2`: (JP address, size in bytes).
const PART: &[(u32, u32)] = &[
    (0x8002_57e8, 792),
    (0x8002_5b00, 200),
    (0x8002_ae0c, 552),
    (0x8002_d79c, 112),
    (0x8002_f9ac, 1844),
    (0x8003_2754, 1988),
    (0x8003_57c0, 312),
    (0x8003_58f8, 320),
    (0x8003_5a38, 1248),
    (0x8003_5f18, 128),
    (0x8003_5f98, 408),
    (0x8003_62a0, 272),
    (0x8003_6440, 132),
    (0x8003_64c4, 312),
    (0x8003_65fc, 552),
    (0x8003_6824, 420),
    (0x8003_6a84, 1024),
    (0x8003_6e84, 492),
    (0x8003_7e50, 516),
    (0x8003_8054, 36),
    (0x8003_809c, 100),
    (0x8003_8100, 468),
    (0x8003_83ac, 1680),
    (0x8003_8b2c, 252),
    (0x8003_aa60, 396),
    (0x8003_ac8c, 296),
    (0x8004_6600, 596),
    (0x8004_69ec, 1040),
    (0x8004_701c, 1108),
    (0x8004_7a54, 172),
    (0x8004_7d68, 1492),
    (0x8004_856c, 236),
    (0x8004_8a80, 84),
    (0x8004_986c, 432),
    (0x8004_a678, 368),
    (0x8004_aa3c, 596),
    (0x8004_b108, 512),
    (0x8004_b308, 192),
    (0x8004_b4d8, 524),
    (0x8004_ba7c, 668),
    (0x8004_dc38, 1064),
    (0x8004_f0c8, 1040),
    (0x8004_f570, 1464),
    (0x8004_fc04, 236),
    (0x8005_0184, 336),
    (0x8005_02d4, 608),
    (0x8005_0de8, 448),
    (0x8005_1010, 756),
    (0x8005_1304, 212),
    (0x8005_13d8, 60),
    (0x8005_3bac, 500),
    (0x8005_4ebc, 3472),
    (0x8005_5f3c, 104),
    (0x8005_60e4, 100),
    (0x8005_6198, 156),
    (0x8006_7144, 572),
    (0x8006_7380, 276),
    (0x8007_7880, 328),
    (0x8007_961c, 100),
    (0x8007_affc, 1084),
    (0x8007_b438, 2128),
    (0x8007_cfa4, 2108),
    (0x8007_d7e0, 572),
    (0x8008_3fe0, 824),
    (0x8008_488c, 236),
    (0x8008_4f6c, 364),
    (0x8008_50d8, 300),
    (0x8008_54b8, 1124),
    (0x8008_6320, 1220),
    (0x8008_6a0c, 1536),
    (0x8008_9f44, 1752),
    (0x8008_abf0, 1112),
    (0x8008_b310, 1180),
    (0x8008_ba8c, 1556),
    (0x8008_c644, 912),
    (0x8008_e038, 1216),
    (0x8008_e4f8, 1632),
    (0x8008_f1b8, 968),
    (0x8008_f580, 976),
    (0x8008_f950, 344),
    (0x8008_ffac, 248),
    (0x8009_0a74, 388),
    (0x8009_0bf8, 728),
    (0x8009_0ed0, 736),
    (0x8009_11b0, 448),
    (0x8009_1370, 740),
    (0x8009_2168, 516),
    (0x8009_28f4, 452),
];

fn size_of(start: u32) -> u32 {
    PART.iter()
        .find(|(a, _)| *a == start)
        .map(|(_, s)| *s)
        .unwrap_or_else(|| panic!("{start:08x} is not a routine of the part"))
}

/// Every captured call (label, entry, exit) of `callee` in the module-0
/// indexes of all runs, at most `limit` per run.
fn captured(callee: u32, limit: usize) -> Vec<(String, Snapshot, Snapshot)> {
    let mut out = Vec::new();
    let Ok(runs) = std::fs::read_dir(captures()) else {
        return out;
    };
    let mut runs: Vec<PathBuf> = runs.flatten().map(|r| r.path()).collect();
    runs.sort();
    for run in runs {
        let index_file = run.join("snapshots-module0.jsonl");
        let Ok(index) = snapshot::read_index(&index_file) else {
            continue;
        };
        let name = run.file_name().unwrap().to_string_lossy().into_owned();
        for entry in index
            .iter()
            .filter(|s| s.callee == callee && s.stage == "entry")
            .take(limit)
        {
            let Some(exit) = index.iter().find(|s| {
                s.callee == callee
                    && s.stage == "return"
                    && s.sequence > entry.sequence
                    && s.ra == entry.ra
                    && s.registers.gpr[29] == entry.registers.gpr[29]
            }) else {
                continue;
            };
            out.push((
                format!("{name} #{}", entry.sequence),
                Snapshot::load(&run, entry).unwrap(),
                Snapshot::load(&run, exit).unwrap(),
            ));
        }
    }
    out
}

/// The prefix of a result that waits on another part: without
/// `dev-fallback`, a call that reaches a routine nobody translated yet
/// (another part's JP-only routine) faults, and such a case is reported as
/// pending on that routine, not failed. With the fallback every routine
/// runs, so nothing is pending.
const PENDING: &str = "pending: reaches the untranslated routine";

fn pending_on(fault: &Result<(), Fault>) -> Option<String> {
    match fault {
        Err(Fault::Untranslated { address, .. }) if !cfg!(feature = "dev-fallback") => {
            Some(format!("{PENDING} {address:08x}"))
        }
        _ => None,
    }
}

fn panic_text(e: &(dyn std::any::Any + Send)) -> String {
    e.downcast_ref::<String>()
        .cloned()
        .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default()
}

/// The scratchpad words where 80086320 and 80086A0C save their caller's
/// s0..s5 and `ra` (0x1F8003D8..0x1F8003F4: their frame on 8008700C's
/// scratchpad stack). The oracle starts a routine with those registers zero
/// (and `ra` its return sentinel), so it saves other values than the
/// caller 8008700C had, which the translation saves (and the captures
/// hold); outside this oracle comparison they are compared.
fn caller_spills(start: u32) -> std::ops::Range<usize> {
    if matches!(start, 0x8008_6320 | 0x8008_6a0c) {
        0x3d8..0x3f8
    } else {
        0..0
    }
}

/// What the original did in one oracle case: the RAM bytes it changed and a
/// digest of its result (`v0`, the changed RAM and scratchpad bytes), to
/// count the distinct outcomes of varied cases.
struct Outcome {
    changed: usize,
    digest: u64,
}

/// FNV-1a over the little-endian bytes of `value`.
fn fnv(digest: &mut u64, value: u32) {
    for byte in value.to_le_bytes() {
        *digest ^= u64::from(byte);
        *digest = digest.wrapping_mul(0x100_0000_01b3);
    }
}

/// Run the original JP instructions of `range` (the oracle) and the
/// translation (`dispatch::call`) from clones of `base`, each with a fresh
/// JP disc device when `with_disc`, and compare RAM outside the stack below
/// the entry `sp` and the `skip` ranges, scratchpad, GTE, `sp`, fault and
/// `v0` (when `has_result`).
fn compare_with_oracle(
    base: &Machine,
    range: (u32, u32),
    args: [u32; 4],
    has_result: bool,
    with_disc: bool,
    skip: &[(u32, u32, &'static str)],
) -> Result<Outcome, String> {
    let sp = base.sp;
    let mut original = base.clone();
    let mut native = base.clone();
    if with_disc {
        original.device = Some(disc());
        native.device = Some(disc());
    }
    let v0 = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        oracle::run(&mut original, range.0, range.1, args)
    }))
    .map_err(|e| format!("oracle: {}", panic_text(&*e)))?;
    let nv0 = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        dispatch::call(&mut native, range.0, args)
    }))
    .map_err(|e| format!("native: {}", panic_text(&*e)))?;
    let (fo, fnat) = (original.take_fault(), native.take_fault());
    if let Some(pending) = pending_on(&fo).or_else(|| pending_on(&fnat)) {
        return Err(pending);
    }
    if fo != fnat {
        return Err(format!("faults: original {fo:?}, native {fnat:?}"));
    }
    let stack = snapshot::STACK_START..(sp & 0x1f_ffff) as usize;
    let skipped = |i: usize| stack.contains(&i) || excluded(0x8000_0000 + i as u32, skip).is_some();
    let ram: Vec<u32> = (0..0x20_0000usize)
        .filter(|&i| !skipped(i) && original.ram()[i] != native.ram()[i])
        .map(|i| 0x8000_0000 + i as u32)
        .collect();
    let spills = caller_spills(range.0);
    let scratch = (0..0x400)
        .filter(|&i| !spills.contains(&i) && original.scratch()[i] != native.scratch()[i])
        .count();
    if !ram.is_empty() || scratch != 0 || original.gte != native.gte || original.sp != native.sp {
        return Err(format!(
            "ram {:08x?} ({} bytes) scratch {scratch} gte equal {} sp {:x}/{:x}",
            &ram[..ram.len().min(12)],
            ram.len(),
            original.gte == native.gte,
            original.sp,
            native.sp
        ));
    }
    if has_result && v0 != nv0 {
        return Err(format!("v0: original {v0:#x}, native {nv0:#x}"));
    }
    let mut digest = 0xcbf2_9ce4_8422_2325u64;
    fnv(&mut digest, v0);
    let mut changed = 0;
    for i in (0..0x20_0000usize).filter(|&i| !skipped(i) && base.ram()[i] != original.ram()[i]) {
        changed += 1;
        fnv(&mut digest, i as u32);
        fnv(&mut digest, u32::from(original.ram()[i]));
    }
    for i in
        (0..0x400).filter(|&i| !spills.contains(&i) && base.scratch()[i] != original.scratch()[i])
    {
        fnv(&mut digest, 0x1f80_0000 + i as u32);
        fnv(&mut digest, u32::from(original.scratch()[i]));
    }
    Ok(Outcome { changed, digest })
}

/// Every routine of the part that the patch layer replaces runs from each of
/// its captured entry states (up to four per run) twice: the original JP
/// instructions (the oracle) and the translation (`dispatch::call`), with
/// every call out of the routine native in both. They must leave the same
/// RAM, scratchpad, GTE, `sp`, fault and `v0` (for routines with a result).
/// This isolates each patched routine from its callees, which other parts
/// own; [`patched_routines_reproduce_their_captures`] checks whole calls.
///
/// 80025B00 is not compared here: the oracle cannot run its SDK device calls
/// (CdSetDebug, InitCARD, ...), which the translation makes host calls; it
/// is validated by [`init_devices_reproduces_its_capture`].
#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn patched_routines_match_the_original_on_captured_entries() {
    install_fallback();
    let replaced = scan::patch_table("REPLACES");
    let taken = scan::patch_table("TAKES_OVER");
    let (_, void) = scan::jp_resident_claims();
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let (mut cases, mut failures, mut routines) = (0, Vec::new(), 0);
    for &(start, size) in PART {
        if !(replaced.contains(&start) || taken.contains(&start)) || start == 0x8002_5b00 {
            continue;
        }
        let calls = captured(start, 4);
        if calls.is_empty() {
            continue;
        }
        routines += 1;
        let with_disc = start == 0x8002_ae0c;
        for (label, entry, _) in calls {
            let base = entry.machine();
            let args = [entry.gpr(4), entry.gpr(5), entry.gpr(6), entry.gpr(7)];
            let label = format!("{start:08x} {label}");
            let result = compare_with_oracle(
                &base,
                (start, start + size),
                args,
                !void.contains(&start),
                with_disc,
                &[],
            );
            cases += 1;
            match result {
                Ok(outcome) => eprintln!("{label}: equal, {} bytes changed", outcome.changed),
                Err(e) => failures.push(format!("{label}: {e}")),
            }
        }
    }
    std::panic::set_hook(hook);
    eprintln!("{routines} patched routines, {cases} captured entry states compared");
    let (pending, failures): (Vec<_>, Vec<_>) = failures
        .into_iter()
        .partition(|line| line.contains(PENDING));
    for line in &pending {
        eprintln!("{line}");
    }
    assert!(cases > 0);
    assert!(failures.is_empty(), "{failures:#?}");
}

/// GTE data registers a whole captured call may leave differently, by JP
/// address, with the reason.
const GTE_ALLOWED: &[(u32, &[usize], &str)] = &[(
    0x8003_5a38,
    &[20, 21, 28, 29],
    "JP 8005EBB8 reads the mission CLUTs back from VRAM (StoreImage2, a host \
     call without a VRAM here) before INTPL-darkening them: the colour FIFO \
     RGB0/RGB1 and IRGB/ORGB of its last INTPL",
)];

/// Replay one captured call through `dispatch::call` (the disc attached when
/// `with_disc`) and compare it with the return snapshot outside `skip` (and
/// the [`GTE_ALLOWED`] registers): `Ok(bytes the original changed outside
/// skip)`.
fn replay(
    entry: &Snapshot,
    exit: &Snapshot,
    with_disc: bool,
    has_result: bool,
    skip: &[(u32, u32, &'static str)],
) -> Result<usize, String> {
    let mut m = entry.machine();
    if with_disc {
        m.device = Some(disc());
    }
    let callee = entry.info.callee;
    let args = [entry.gpr(4), entry.gpr(5), entry.gpr(6), entry.gpr(7)];
    let v0 = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        dispatch::call(&mut m, callee, args)
    }))
    .map_err(|e| format!("panic: {}", panic_text(&*e)))?;
    let fault = m.take_fault();
    if let Some(pending) = pending_on(&fault) {
        return Err(pending);
    }
    if let Err(fault) = fault {
        return Err(format!("fault: {fault}"));
    }
    let ram: Vec<u32> = (0..snapshot::STACK_START)
        .filter(|&i| m.ram()[i] != exit.ram[i])
        .map(|i| 0x8000_0000 + i as u32)
        .filter(|&a| excluded(a, skip).is_none())
        .collect();
    let d = snapshot::compare(&m, exit);
    let allowed = GTE_ALLOWED
        .iter()
        .find(|(a, _, _)| *a == callee)
        .map_or(&[][..], |(_, r, _)| *r);
    let gte_data: Vec<usize> = d
        .gte_data
        .iter()
        .copied()
        .filter(|r| !allowed.contains(r))
        .collect();
    if !ram.is_empty() || !d.scratch.is_empty() || !gte_data.is_empty() || !d.gte_control.is_empty()
    {
        return Err(format!(
            "ram {:08x?} ({} bytes) scratch {:08x?} gte {:?}/{:?}",
            &ram[..ram.len().min(12)],
            ram.len(),
            &d.scratch[..d.scratch.len().min(8)],
            gte_data,
            d.gte_control
        ));
    }
    if has_result && v0 != exit.gpr(2) {
        return Err(format!("v0 {v0:#x}, original {:#x}", exit.gpr(2)));
    }
    if m.sp != entry.gpr(29) {
        return Err(format!("sp {:x}, entry {:x}", m.sp, entry.gpr(29)));
    }
    Ok((0..snapshot::STACK_START)
        .filter(|&i| {
            entry.ram[i] != exit.ram[i] && excluded(0x8000_0000 + i as u32, skip).is_none()
        })
        .count())
}

/// The routines of the part the patch layer claims (REPLACES, TAKES_OVER).
fn patched() -> Vec<u32> {
    let replaced = scan::patch_table("REPLACES");
    let taken = scan::patch_table("TAKES_OVER");
    PART.iter()
        .map(|&(a, _)| a)
        .filter(|a| replaced.contains(a) || taken.contains(a))
        .collect()
}

/// Replay every captured call of every patched routine of the part:
/// (cases, reproduced, failures).
fn replay_all() -> (usize, usize, Vec<(u32, String)>) {
    let (_, void) = scan::jp_resident_claims();
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let (mut cases, mut passes, mut failures) = (0, 0, Vec::new());
    for start in patched() {
        let with_disc = matches!(start, 0x8002_5b00 | 0x8002_ae0c);
        for (label, entry, exit) in captured(start, 4) {
            cases += 1;
            let result = replay(
                &entry,
                &exit,
                with_disc,
                !void.contains(&start),
                DEVICE_STATE,
            );
            let label = format!("{start:08x} {label}");
            match result {
                Ok(changed) => {
                    passes += 1;
                    eprintln!("{label}: reproduced, {changed} bytes changed");
                }
                Err(e) => failures.push((start, format!("{label}: {e}"))),
            }
        }
    }
    std::panic::set_hook(hook);
    (cases, passes, failures)
}

/// Every captured call of every patched routine of the part, whole, against
/// its return snapshot, with every other game routine running as the
/// original JP instructions (the development fallback forced on everything
/// but this part's patched routines; feature `dev-fallback`): this part's
/// translations against Redux, independent of the state of the other
/// parts. CD routines run with the JP disc attached; the [`DEVICE_STATE`]
/// bytes are excluded (the captures span VBlank, CD and SPU interrupts, the
/// native call has none), and the [`GTE_ALLOWED`] registers allowed.
#[cfg(feature = "dev-fallback")]
#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn patched_routines_reproduce_their_captures_among_original_callees() {
    let mut fallback = ac3_fallback::Fallback::new(ac3_fallback::Version::Jp);
    let spec = std::iter::once("game".to_string())
        .chain(patched().iter().map(|a| format!("-{a:08x}")))
        .collect::<Vec<_>>()
        .join(",");
    let forced = fallback.force(&spec).expect("force spec");
    ac3_fallback::install(fallback);
    eprintln!("{forced} routines forced to run as the original instructions");
    let (cases, passes, failures) = replay_all();
    ac3_fallback::uninstall();
    eprintln!("{passes} of {cases} captured calls reproduced");
    assert!(passes > 0);
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Captured calls that the current translations of other parts do not let
/// reproduce yet: (JP address, the dependency). They are replayed and
/// reported, not required. None since resident part 3 (the loading tiles'
/// HUD drawers, 800566F0, 8005EBB8, 80049438, ...) is in the crate; the
/// loading-screen captures of 80032754, 80035A38, 80035F18, 80035F98 and
/// 80036A84 waited on it.
const DEPENDS_ON_OTHER_PARTS: &[(u32, &str)] = &[];

/// Every captured call of every patched routine of the part, whole, as the
/// crate stands: untranslated callees interpreted by the fallback (with
/// `dev-fallback`; without it, calls reaching them are reported as
/// [`PENDING`]), translated ones native. The calls listed in
/// [`DEPENDS_ON_OTHER_PARTS`] are reported, the rest required.
#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn patched_routines_reproduce_their_captures() {
    install_fallback();
    let (cases, passes, failures) = replay_all();
    eprintln!("{passes} of {cases} captured calls reproduced");
    let (untranslated, failures): (Vec<_>, Vec<_>) = failures
        .into_iter()
        .partition(|(_, line)| line.contains(PENDING));
    for (_, line) in &untranslated {
        eprintln!("{line}");
    }
    let (pending, failures): (Vec<_>, Vec<_>) = failures
        .into_iter()
        .partition(|(a, _)| DEPENDS_ON_OTHER_PARTS.iter().any(|(d, _)| d == a));
    for (a, line) in &pending {
        let why = DEPENDS_ON_OTHER_PARTS
            .iter()
            .find(|(d, _)| d == a)
            .unwrap()
            .1;
        eprintln!("pending: {line} (depends on {why})");
    }
    assert!(passes > 0);
    assert!(failures.is_empty(), "{failures:#?}");
}

/// 80025B00 (device start-up) on its `boot-002` capture with the JP disc:
/// the RAM outside [`DEVICE_STATE`], the scratchpad and the GTE (InitGeom's
/// controls), and `v0`, as the PAL `tests/boot.rs` checks 80025610.
#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures and disc 1)"]
fn init_devices_reproduces_its_capture() {
    let calls = captured(0x8002_5b00, 1);
    assert!(!calls.is_empty(), "no capture of 80025B00");
    for (label, entry, exit) in calls {
        let changed = replay(&entry, &exit, true, true, DEVICE_STATE)
            .unwrap_or_else(|e| panic!("80025b00 {label}: {e}"));
        eprintln!("80025b00 {label}: reproduced, {changed} bytes changed");
        assert!(changed > 0);
    }
}

/// Debug aid: `PROBE=run:callee:occurrence[:disc]` replays one capture and
/// prints every difference (bytes in [`DEVICE_STATE`] marked `x`).
#[test]
#[ignore = "debug aid: PROBE=run:callee:occurrence[:disc]"]
fn probe() {
    let Ok(spec) = std::env::var("PROBE") else {
        return;
    };
    install_fallback();
    let parts: Vec<&str> = spec.split(':').collect();
    let (run, callee, occurrence) = (
        parts[0],
        u32::from_str_radix(parts[1], 16).unwrap(),
        parts[2].parse::<usize>().unwrap(),
    );
    let (_, entry, exit) = captured(callee, occurrence + 1)
        .into_iter()
        .filter(|(label, _, _)| label.starts_with(run))
        .nth(occurrence)
        .expect("capture");
    let mut m = entry.machine();
    if parts.get(3) == Some(&"disc") {
        m.device = Some(disc());
    }
    let args = [entry.gpr(4), entry.gpr(5), entry.gpr(6), entry.gpr(7)];
    let v0 = dispatch::call(&mut m, callee, args);
    eprintln!("fault {:?}", m.take_fault());
    eprintln!("v0 {v0:#x}, original {:#x}", exit.gpr(2));
    let mut last = 0;
    for i in 0..snapshot::STACK_START {
        if m.ram()[i] != exit.ram[i] {
            let a = 0x8000_0000 + i as u32;
            if a != last + 1 {
                eprintln!();
            }
            eprint!(
                "{a:08x}:{:02x}/{:02x}{} ",
                m.ram()[i],
                exit.ram[i],
                if excluded(a, DEVICE_STATE).is_some() {
                    "x"
                } else {
                    ""
                }
            );
            last = a;
        }
    }
    eprintln!();
    eprintln!("{}", snapshot::compare(&m, &exit).summary());
    for r in 0..32 {
        let (native, original) = (m.gte.data[r], exit.info.registers.cp2d[r]);
        if native != original {
            eprintln!(
                "gte data {r}: native {native:08x}, original {original:08x}, entry {:08x}",
                entry.info.registers.cp2d[r]
            );
        }
    }
    #[cfg(feature = "dev-fallback")]
    if let Some(fallback) = ac3_fallback::active() {
        eprintln!("{}", fallback.summary());
        let report = fallback.report();
        for r in report["queue"].as_array().into_iter().flatten().take(20) {
            eprintln!(
                "  interpreted {} {} calls {} instructions {}",
                r["address"], r["class"], r["calls"], r["instructions"]
            );
        }
        eprintln!("blocked {} faults {}", report["blocked"], report["faults"]);
    }
    for call in m.host_calls.iter().take(64) {
        eprintln!("host {:08x} {:08x?}", call.function, call.args);
    }
    let _ = size_of(callee);
}

// ---- oracle cases: the routines of the part no capture reaches --------------

/// The JP global pointer.
const GP: u32 = ac3_flight_jp::GP;
/// 800C6FD0: the per-slot status words.
const SLOT_FLAGS: u32 = 0x800c_6fd0;
/// Scratchpad base.
const SCRATCH: u32 = 0x1f80_0000;

fn gp_word(m: &mut Machine, offset: u32) -> u32 {
    m.lw(GP + offset)
}
fn context(m: &mut Machine) -> u32 {
    gp_word(m, 0x2ec)
}
fn gameplay(m: &mut Machine) -> u32 {
    gp_word(m, 0x2e0)
}
fn entity(m: &mut Machine, slot: u32) -> u32 {
    let table = gp_word(m, 0x2e8);
    m.lw(table + slot * 4)
}
fn slot_record(m: &mut Machine, slot: u32) -> u32 {
    gp_word(m, 0x34c) + slot * 0xb8
}
fn slot_info(m: &mut Machine, slot: u32) -> u32 {
    gp_word(m, 0x384) + slot * 0x40
}
fn in_ram(address: u32) -> bool {
    (0x8000_0000..0x8020_0000).contains(&address)
}
fn pick(rng: &mut oracle::Rng, values: &[u32]) -> u32 {
    values[rng.next() as usize % values.len()]
}
fn coin(rng: &mut oracle::Rng) -> bool {
    rng.next() & 0x100 != 0
}
/// A value in `-range..range`.
fn signed(rng: &mut oracle::Rng, range: u32) -> u32 {
    (rng.next() % (2 * range)).wrapping_sub(range)
}

/// One family of oracle cases: a routine of the part that no capture
/// reaches, run from the first captured entry state of `base` (a routine
/// that runs in the same context), with `setup` varying the fields its paths
/// depend on (`varied`) and returning the arguments.
struct Family {
    start: u32,
    base: u32,
    cases: usize,
    setup: fn(&mut Machine, &Snapshot, &mut oracle::Rng) -> [u32; 4],
    varied: &'static str,
}

/// Run every case of every family as the original JP instructions (the
/// oracle) and as the translation ([`compare_with_oracle`]); report the
/// states and the distinct outcomes (a family of several cases must have at
/// least two).
fn run_families(families: &[Family]) {
    install_fallback();
    let (_, void) = scan::jp_resident_claims();
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let (mut report, mut failures) = (Vec::new(), Vec::new());
    for family in families {
        let start = family.start;
        let Some((_, entry, _)) = captured(family.base, 1).into_iter().next() else {
            failures.push(format!(
                "{start:08x}: no capture of the base {:08x}",
                family.base
            ));
            continue;
        };
        let image = entry.machine();
        let mut rng = oracle::Rng(start);
        let range = (start, start + size_of(start));
        let (mut outcomes, mut changed, mut pending) = (std::collections::BTreeSet::new(), 0, 0);
        let verbose = std::env::var_os("RES_P2_VERBOSE").is_some();
        for case in 0..family.cases {
            let mut base = image.clone();
            let args = (family.setup)(&mut base, &entry, &mut rng);
            if verbose {
                eprintln!("{start:08x} case {case} {args:08x?}");
            }
            match compare_with_oracle(&base, range, args, !void.contains(&start), false, &[]) {
                Ok(outcome) => {
                    outcomes.insert(outcome.digest);
                    changed += outcome.changed;
                }
                Err(e) if e.contains(PENDING) => pending += 1,
                Err(e) => failures.push(format!("{start:08x} case {case} {args:08x?}: {e}")),
            }
        }
        report.push(format!(
            "{start:08x}: {} states from the {:08x} capture, {} distinct outcomes, {changed} \
             bytes changed in all ({}){}",
            family.cases,
            family.base,
            outcomes.len(),
            family.varied,
            if pending > 0 {
                format!(", {pending} pending (untranslated callees)")
            } else {
                String::new()
            }
        ));
        if family.cases > 1 && outcomes.len() < 2 && pending == 0 {
            failures.push(format!("{start:08x}: a single outcome"));
        }
    }
    std::panic::set_hook(hook);
    for line in &report {
        eprintln!("{line}");
    }
    // Printed as well: tests running in parallel swap the panic hook.
    for line in &failures {
        eprintln!("FAILED {line}");
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Mission indexes the AI routines compare (and two others).
const MISSIONS: &[u32] = &[
    0, 3, 6, 7, 0x17, 0x1a, 0x1c, 0x1f, 0x20, 0x24, 0x29, 0x2c, 0x2f, 0x32, 0x35, 0x37,
];
/// AI record bytes the movement and action states branch on, with the
/// values they compare.
const RECORD_BYTES: &[(u32, &[u32])] = &[
    (0x1c, &[3, 8, 0xa, 0xff]),
    (0x1d, &[0, 1]),
    (0x3d, &[0, 3, 0x17, 0xff]),
    (0x3e, &[0, 1, 2, 3, 7, 0x10]),
    (0x63, &[0, 0x32, 0xff]),
    (0x64, &[0, 1, 2, 3, 4, 5]),
];
/// AI record halves (timers, counters, angles).
const RECORD_HALVES: &[(u32, &[u32])] = &[
    (0x40, &[0, 1, 2, 0x50, 0xffff]),
    (0x5e, &[0, 1, 0xe, 0xf, 0x1d, 0x1e, 0x3b, 0x3c, 0x77, 0x78]),
    (0x66, &[0, 0x3c, 0x5a, 0xa8c, 0x1c20, 0x1d50]),
    (0x80, &[0, 0x100]),
    (0x84, &[0, 0x100]),
    (0xb4, &[0, 0x7fff]),
];

fn live_slots(m: &mut Machine) -> Vec<u32> {
    (1..32).filter(|&slot| in_ram(entity(m, slot))).collect()
}

/// The mission index (context half +0x38C) and the difficulty (+0x394).
fn vary_mission(m: &mut Machine, rng: &mut oracle::Rng) {
    let context = context(m);
    let mission = pick(rng, MISSIONS);
    m.sh(context + 0x38c, mission as u16);
    m.sw(context + 0x394, rng.next() & 3);
}

/// A live slot, with the mission, the difficulty, gameplay flags, the
/// slot's AI record, status word, entity kind and flags, type kind, pilot
/// and weapon fields and the shots mask varied.
fn vary_slot(m: &mut Machine, rng: &mut oracle::Rng) -> u32 {
    let slots = live_slots(m);
    let slot = if slots.is_empty() {
        1
    } else {
        pick(rng, &slots)
    };
    vary_mission(m, rng);
    let gameplay = gameplay(m);
    for (offset, bits) in [(4u32, 0x2804u32), (8, 1), (0xc, 0xff)] {
        if coin(rng) {
            let value = m.lw(gameplay + offset) ^ (rng.next() & bits);
            m.sw(gameplay + offset, value);
        }
    }
    let record = slot_record(m, slot);
    for &(offset, values) in RECORD_BYTES {
        if coin(rng) {
            let value = pick(rng, values);
            m.sb(record + offset, value as u8);
        }
    }
    for &(offset, values) in RECORD_HALVES {
        if coin(rng) {
            let value = pick(rng, values);
            m.sh(record + offset, value as u16);
        }
    }
    if coin(rng) {
        let value = m.lw(record) ^ 0x40_0000;
        m.sw(record, value);
    }
    let targets: Vec<u32> = std::iter::once(0).chain(slots.iter().copied()).collect();
    for offset in [0x1f, 0x3f, 0x60] {
        if coin(rng) {
            let target = pick(rng, &targets);
            m.sb(record + offset, target as u8);
        }
    }
    let status = SLOT_FLAGS + slot * 4;
    if coin(rng) {
        let value = m.lw(status) ^ (rng.next() & 0x0090_0420);
        m.sw(status, value);
    }
    let mover = entity(m, slot);
    if coin(rng) {
        let kind = pick(rng, &[1, 0x14, 0x1a, 0x1b, 0x2c, 0x3e]);
        m.sh(mover + 0x48, kind as u16);
    }
    if coin(rng) {
        let value = m.lw(mover) ^ (rng.next() & 0xc0);
        m.sw(mover, value);
    }
    let info = slot_info(m, slot);
    let params = m.lw(info + 0x14);
    if in_ram(params) && coin(rng) {
        m.sw(params, rng.next() % 9);
    }
    let pilot = m.lw(info + 0x18);
    if in_ram(pilot) {
        if coin(rng) {
            m.sb(pilot, (rng.next() & 3) as u8);
        }
        if coin(rng) {
            let interval = pick(rng, &[0x10, 0x3c, 0x78]);
            m.sh(pilot + 0x16, interval as u16);
        }
        if coin(rng) {
            let time = pick(rng, &[0, 0x20, 0x7fff]);
            m.sh(pilot + 0x12, time as u16);
        }
    }
    let weapon = m.lw(info + 0x1c);
    if in_ram(weapon) && coin(rng) {
        m.sh(weapon, (1 + rng.next() % 9) as u16);
        m.sw(weapon + 8, rng.next());
    }
    let shots = m.lw(0x800b_d080);
    if in_ram(shots) && coin(rng) {
        m.sw(shots, rng.next() << 8);
    }
    slot
}

fn slot_case(m: &mut Machine, _: &Snapshot, rng: &mut oracle::Rng) -> [u32; 4] {
    [vary_slot(m, rng), 0, 0, 0]
}

/// 80051010: a slot whose spawn record's route (+2) selects a keyframe
/// track (800C6E90) of 3..6 random 20-byte keys and the end key (-1),
/// written below the stack, at a random step (+0x38); a slot without a
/// spawn record gets one there too.
fn keyframes_case(m: &mut Machine, entry: &Snapshot, rng: &mut oracle::Rng) -> [u32; 4] {
    let slot = vary_slot(m, rng);
    let info = slot_info(m, slot);
    let track = (entry.gpr(29) - 0x4000) & !3;
    let mut spawn = m.lw(info + 0x30);
    if !in_ram(spawn) {
        spawn = track - 0x40;
        m.sw(info + 0x30, spawn);
        m.sb(spawn + 2, (rng.next() % 8) as u8);
    }
    let route = u32::from(m.lbu(spawn + 2));
    m.sw(0x800c_6e90 + route * 4, track);
    let keys = 3 + rng.next() % 4;
    for key in 0..keys {
        let at = track + key * 20;
        for word in 0..3 {
            let value = signed(rng, 0x100_0000);
            m.sw(at + word * 4, value);
        }
        for half in 0..3 {
            let angle = rng.next() & 0xffff;
            m.sh(at + 12 + half * 2, angle as u16);
        }
    }
    m.sw(track + keys * 20, u32::MAX);
    let record = slot_record(m, slot);
    m.sw(record + 0x38, rng.next() % (keys * 8));
    [slot, 0, 0, 0]
}

/// 80048A80: the pending entries (context +0x748, count +0x3C2).
fn latest_event_case(m: &mut Machine, _: &Snapshot, rng: &mut oracle::Rng) -> [u32; 4] {
    let context = context(m);
    m.sb(context + 0x3c2, (rng.next() % 6) as u8);
    for index in 0..6 {
        let entry = context + 0x748 + index * 8;
        m.sb(entry + 1, (rng.next() & 1) as u8);
        m.sb(entry + 6, rng.next() as u8);
    }
    [0; 4]
}

/// 8004DC38: the phase (`[gp+0x334]`), its kind and two slots in the phase
/// table 800B1FC4, the mission and the difficulty.
fn phase_case(m: &mut Machine, _: &Snapshot, rng: &mut oracle::Rng) -> [u32; 4] {
    vary_mission(m, rng);
    let slots = live_slots(m);
    let phase = rng.next() % 8;
    m.sw(GP + 0x334, phase);
    let entry = 0x800b_1fc4 + phase * 4;
    m.sb(entry, (rng.next() % 9) as u8);
    for index in 0..2 {
        let slot = if coin(rng) || slots.is_empty() {
            0
        } else {
            pick(rng, &slots)
        };
        m.sb(entry + 2 + index, slot as u8);
    }
    [0; 4]
}

/// 8004BA7C: a group of 1..4 live slots written into its definition
/// (`[gp+0x378]`, 12 bytes: +2 count, +4 slots; the captured mission's
/// definitions are not all initialized), its break flag and frames, and the
/// members' slot fields.
fn group_case(m: &mut Machine, _: &Snapshot, rng: &mut oracle::Rng) -> [u32; 4] {
    vary_slot(m, rng);
    let slots = live_slots(m);
    let group = rng.next() % 4;
    let definition = gp_word(m, 0x378) + group * 12;
    let state = gp_word(m, 0x380) + group * 12;
    if in_ram(definition) && in_ram(state) && !slots.is_empty() {
        let count = 1 + rng.next() % 4;
        m.sh(definition + 2, count as u16);
        for index in 0..count {
            let slot = pick(rng, &slots);
            m.sb(definition + 4 + index, slot as u8);
        }
        m.sb(state + 7, (rng.next() & 1) as u8);
        m.sw(state + 8, rng.next() % 3);
    }
    [group, 0, 0, 0]
}

/// 80047A54: the fire requests (status bit 0x10000) of slots 2..31.
fn fire_control_case(m: &mut Machine, _: &Snapshot, rng: &mut oracle::Rng) -> [u32; 4] {
    vary_slot(m, rng);
    for slot in 2..32 {
        if coin(rng) {
            let status = SLOT_FLAGS + slot * 4;
            let value = m.lw(status) ^ 0x1_0000;
            m.sw(status, value);
        }
    }
    [0; 4]
}

/// The AI routines of the part (all their JP differences; 80047A54 and
/// 8004BA7C are not patched: their differences only change `v0`, which the
/// callers ignore, and the generated translations are checked here as they
/// are), from the 8004701C (AI actions) entry state of `flight-002`.
#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn oracle_ai_routines() {
    const AI: u32 = 0x8004_701c;
    const SLOT: &str = "slot, mission, difficulty, AI record, status, kinds, pilot, weapon";
    let family = |start, cases, setup, varied| Family {
        start,
        base: AI,
        cases,
        setup,
        varied,
    };
    run_families(&[
        family(0x8004_f0c8, 48, slot_case, SLOT),
        family(0x8004_a678, 48, slot_case, SLOT),
        family(0x8004_aa3c, 48, slot_case, SLOT),
        family(0x8004_b108, 48, slot_case, SLOT),
        family(0x8004_b4d8, 48, slot_case, SLOT),
        family(0x8005_0184, 48, slot_case, SLOT),
        family(0x8005_02d4, 48, slot_case, SLOT),
        family(
            0x8005_1010,
            48,
            keyframes_case,
            "keyframe track, step, slot fields",
        ),
        family(0x8005_1304, 48, slot_case, SLOT),
        family(0x8004_b308, 48, slot_case, SLOT),
        family(0x8005_0de8, 48, slot_case, SLOT),
        family(0x8005_13d8, 32, slot_case, SLOT),
        family(0x8004_986c, 48, slot_case, SLOT),
        family(0x8004_fc04, 48, slot_case, SLOT),
        family(0x8004_f570, 96, slot_case, SLOT),
        family(0x8004_7d68, 96, slot_case, SLOT),
        family(
            0x8004_8a80,
            32,
            latest_event_case,
            "pending entries and their count",
        ),
        family(
            0x8004_dc38,
            48,
            phase_case,
            "phase, phase kind and slots, mission, difficulty",
        ),
        family(
            0x8004_ba7c,
            48,
            group_case,
            "group, break state, member fields",
        ),
        family(
            0x8004_7a54,
            48,
            fire_control_case,
            "fire requests, slot fields",
        ),
    ]);
}

/// The OT and the packet cursor of the 8008FFAC capture (its `work` +0/+4).
fn overlay_work(m: &mut Machine, entry: &Snapshot) -> (u32, u32) {
    let work = entry.gpr(4);
    (m.lw(work), m.lw(work + 4))
}

/// 8008F950: the displayed half (context +4) and an optional offset.
fn screen_blend_case(m: &mut Machine, entry: &Snapshot, rng: &mut oracle::Rng) -> [u32; 4] {
    let (ot, cursor) = overlay_work(m, entry);
    let context = context(m);
    m.sw(context + 4, rng.next() & 1);
    let rect = if coin(rng) {
        0
    } else {
        let rect = cursor + 0x4000;
        let offset = rng.next() ^ (rng.next() << 16);
        m.sw(rect, offset);
        rect
    };
    [ot.wrapping_sub(4), cursor, rect, 0]
}

/// 8008488C: the level (0, additive, subtractive).
fn noise_case(m: &mut Machine, entry: &Snapshot, rng: &mut oracle::Rng) -> [u32; 4] {
    let (ot, cursor) = overlay_work(m, entry);
    let random = rng.next() & 0x1fff;
    let level = pick(
        rng,
        &[
            0,
            1,
            0x400,
            0x1000,
            0xffff_fc00,
            0xffff_f000,
            random,
            random.wrapping_neg(),
        ],
    );
    [ot, cursor, level, 0]
}

/// 80092168: two view-space points (some behind `near`, some outside the
/// side planes) in a free part of the packet buffer.
fn clip_case(m: &mut Machine, entry: &Snapshot, rng: &mut oracle::Rng) -> [u32; 4] {
    let (_, cursor) = overlay_work(m, entry);
    let a = (cursor + 0x4000) & !3;
    let (b, planes) = (a + 0x10, a + 0x20);
    for point in [a, b] {
        let x = signed(rng, 0x800);
        let y = signed(rng, 0x800);
        let z = signed(rng, 0x1800).wrapping_add(0x1000);
        m.sw(point, x);
        m.sw(point + 4, y);
        m.sw(point + 8, z);
    }
    let near = pick(rng, &[1, 0x40, 0x100]);
    [a, b, near, planes]
}

/// 8008ABF0: the 80089F44 capture's debris piece as an ember: age, life,
/// size and position varied.
fn ember_case(m: &mut Machine, entry: &Snapshot, rng: &mut oracle::Rng) -> [u32; 4] {
    let (piece, work) = (entry.gpr(4), entry.gpr(5));
    let life = pick(rng, &[0, 0xffff, 30, 90, 180]);
    m.sh(piece + 2, life as u16);
    m.sh(piece, (rng.next() % 200) as u16);
    let size = pick(rng, &[0x10, 0x100, 0x800, 0x4000]);
    m.sh(piece + 0x16, size as u16);
    for axis in 0..3 {
        if coin(rng) {
            let at = piece + 4 + axis * 4;
            let value = m.lw(at).wrapping_add(signed(rng, 0x40_0000));
            m.sw(at, value);
        }
    }
    [piece, work, 0, 0]
}

/// 8008E4F8: the fog level from the sun state height (`[gp+0x33C]` +4).
fn rain_case(m: &mut Machine, _: &Snapshot, rng: &mut oracle::Rng) -> [u32; 4] {
    let eye = m.lw(SCRATCH + 0xc);
    let sun = gp_word(m, 0x33c);
    if in_ram(sun) {
        let fog = (rng.next() % 0x1400).wrapping_sub(0x200);
        m.sw(sun + 4, (fog << 14).wrapping_sub(eye));
    }
    [0; 4]
}

/// 8008F1B8: the weather record's mote count, fade and ceiling.
fn motes_case(m: &mut Machine, _: &Snapshot, rng: &mut oracle::Rng) -> [u32; 4] {
    let gameplay = gameplay(m);
    let weather = m.lw(gameplay + 0x268);
    if in_ram(weather) {
        let count = pick(rng, &[0, 1, 8, 40]);
        m.sb(weather + 0x1d, count as u8);
        let fade = pick(rng, &[0, 0x800, 0xfff, 0x1000]);
        m.sh(weather + 0x1e, fade as u16);
        let eye = m.lw(SCRATCH + 0xc);
        let lift = pick(rng, &[0xffff_ff00, 0, 0x800, 0x1000]);
        m.sh(
            weather + 8,
            ((eye as i32) >> 12).wrapping_add(lift as i32) as u16,
        );
    }
    [0; 4]
}

/// 8008F580: the weather record's leaf count and the eye position.
fn leaves_case(m: &mut Machine, _: &Snapshot, rng: &mut oracle::Rng) -> [u32; 4] {
    let gameplay = gameplay(m);
    let weather = m.lw(gameplay + 0x268);
    if in_ram(weather) {
        let count = pick(rng, &[0, 1, 5, 20]);
        m.sb(weather + 0x1c, count as u8);
    }
    for axis in 0..3 {
        let at = SCRATCH + 8 + axis * 4;
        let value = m.lw(at).wrapping_add(signed(rng, 0x80_0000));
        m.sw(at, value);
    }
    [0; 4]
}

/// The render routines of the part that no capture reaches, from entry
/// states of the 3-D frame of `flight-002`: the overlays (8008FFAC), the
/// ambient particles (8008E038) and a debris piece (80089F44).
#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn oracle_render_routines() {
    run_families(&[
        Family {
            start: 0x8008_f950,
            base: 0x8008_ffac,
            cases: 16,
            setup: screen_blend_case,
            varied: "displayed half, offset rectangle",
        },
        Family {
            start: 0x8008_488c,
            base: 0x8008_ffac,
            cases: 16,
            setup: noise_case,
            varied: "level",
        },
        Family {
            start: 0x8009_2168,
            base: 0x8008_ffac,
            cases: 64,
            setup: clip_case,
            varied: "segment end points, near plane",
        },
        Family {
            start: 0x8008_abf0,
            base: 0x8008_9f44,
            cases: 48,
            setup: ember_case,
            varied: "age, life, size, position",
        },
        Family {
            start: 0x8008_e4f8,
            base: 0x8008_e038,
            cases: 16,
            setup: rain_case,
            varied: "fog level",
        },
        Family {
            start: 0x8008_f1b8,
            base: 0x8008_e038,
            cases: 32,
            setup: motes_case,
            varied: "mote count, fade, ceiling",
        },
        Family {
            start: 0x8008_f580,
            base: 0x8008_e038,
            cases: 16,
            setup: leaves_case,
            varied: "leaf count, eye position",
        },
    ]);
}

/// The loading screen's OT and primitive cursor (the 80036A84 capture's
/// arguments).
fn loading_args(entry: &Snapshot) -> [u32; 4] {
    [entry.gpr(4), entry.gpr(5), 0, 0]
}

/// 80036440: the displayed half (context +4).
fn copy_frame_case(m: &mut Machine, entry: &Snapshot, rng: &mut oracle::Rng) -> [u32; 4] {
    let context = context(m);
    m.sw(context + 4, rng.next() & 1);
    loading_args(entry)
}

/// 800364C4: the camera timer (context +0x20).
fn star_screen_case(m: &mut Machine, entry: &Snapshot, rng: &mut oracle::Rng) -> [u32; 4] {
    let context = context(m);
    m.sw(context + 0x20, rng.next());
    loading_args(entry)
}

/// 80083FE0: the view rotation (GTE R11..R33).
fn starfield_case(m: &mut Machine, entry: &Snapshot, rng: &mut oracle::Rng) -> [u32; 4] {
    for register in 0..5 {
        let low = signed(rng, 0x1000) & 0xffff;
        let high = signed(rng, 0x1000) << 16;
        m.gte.ctc2(ac3_gte::ctrl::R11R12 + register, low | high);
    }
    loading_args(entry)
}

/// 80036E84: the aperture state after 800369C8 (stage, amount, tick).
fn aperture_case(m: &mut Machine, entry: &Snapshot, rng: &mut oracle::Rng) -> [u32; 4] {
    dispatch::call(m, 0x8003_69c8, [0; 4]);
    let state = gp_word(m, 0x6c);
    m.sb(state + 6, (rng.next() % 3) as u8);
    m.sh(state + 2, (rng.next() % 0x1400) as u16);
    m.sw(state + 0x38, signed(rng, 20));
    loading_args(entry)
}

/// 8007961C: the flags and the view byte (context +0x39F).
fn start_camera_case(m: &mut Machine, _: &Snapshot, rng: &mut oracle::Rng) -> [u32; 4] {
    let context = context(m);
    m.sb(context + 0x39f, (rng.next() & 1) as u8);
    let flags = pick(rng, &[0, 1, 0x100, 0x200, 0x2000, 0x4000, 0x6300, 0x8000]);
    [flags, 0, 0, 0]
}

/// A script block `[operand][skip 8][else 4]`, then `01 11 00`, else
/// `01 22 00` (opcode 1 stores its byte as the mission phase), written over
/// the script of the 80054EBC capture.
fn script_block(m: &mut Machine, entry: &Snapshot, operand: u32) -> u32 {
    let at = entry.gpr(4);
    let bytes = [
        operand as u8,
        8,
        4,
        0x01,
        0x11,
        0x00,
        0x01,
        0x22,
        0x00,
        0x00,
    ];
    m.write_bytes(at, &bytes);
    at
}

/// 80055F3C: the flag index and context byte +0xD0C + index.
fn block_flag_case(m: &mut Machine, entry: &Snapshot, rng: &mut oracle::Rng) -> [u32; 4] {
    let index = rng.next() % 8;
    let context = context(m);
    m.sb(context + 0xd0c + index, (rng.next() & 1) as u8);
    [script_block(m, entry, index), 0, 0, 0]
}

/// 800560E4: the compared value and context byte +0x394.
fn block_difficulty_case(m: &mut Machine, entry: &Snapshot, rng: &mut oracle::Rng) -> [u32; 4] {
    let value = rng.next() % 4;
    let context = context(m);
    m.sb(context + 0x394, (rng.next() % 4) as u8);
    [script_block(m, entry, value), 0, 0, 0]
}

/// 80038054: the mission index and the result column.
fn set_result_case(_: &mut Machine, _: &Snapshot, rng: &mut oracle::Rng) -> [u32; 4] {
    [rng.next() % 0x40, rng.next() % 4, 0, 0]
}

/// The loading-screen, camera and script routines of the part that no
/// capture reaches: from the loading screen (80036A84), the camera update
/// (8007B438) and the script interpreter (80054EBC) entry states of
/// `flight-002`.
#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn oracle_loading_camera_and_script_routines() {
    const LOADING: u32 = 0x8003_6a84;
    const SCRIPT: u32 = 0x8005_4ebc;
    run_families(&[
        Family {
            start: 0x8003_6440,
            base: LOADING,
            cases: 8,
            setup: copy_frame_case,
            varied: "displayed half",
        },
        Family {
            start: 0x8003_64c4,
            base: LOADING,
            cases: 16,
            setup: star_screen_case,
            varied: "camera timer",
        },
        Family {
            start: 0x8008_3fe0,
            base: LOADING,
            cases: 16,
            setup: starfield_case,
            varied: "view rotation",
        },
        Family {
            start: 0x8003_6e84,
            base: LOADING,
            cases: 32,
            setup: aperture_case,
            varied: "aperture stage, amount, tick",
        },
        Family {
            start: 0x8007_961c,
            base: 0x8007_b438,
            cases: 16,
            setup: start_camera_case,
            varied: "flags, view byte",
        },
        Family {
            start: 0x8005_5f3c,
            base: SCRIPT,
            cases: 16,
            setup: block_flag_case,
            varied: "flag index, flag",
        },
        Family {
            start: 0x8005_60e4,
            base: SCRIPT,
            cases: 16,
            setup: block_difficulty_case,
            varied: "value, difficulty",
        },
        Family {
            start: 0x8003_8054,
            base: SCRIPT,
            cases: 16,
            setup: set_result_case,
            varied: "mission index, column",
        },
    ]);
}

/// 8008BA8C: the sun word (corona byte, flash byte), the eye height and the
/// projected direction (state +8).
fn sun_case(m: &mut Machine, entry: &Snapshot, rng: &mut oracle::Rng) -> [u32; 4] {
    let state = entry.gpr(4);
    let corona = pick(rng, &[0, 0x100, 0x8000]);
    let flash = pick(rng, &[0, 1, 2]);
    let sun = (entry.gpr(5) & !0xffff) | corona | flash;
    let height = entry.gpr(6).wrapping_add(signed(rng, 0x40_0000));
    if coin(rng) {
        let x = u32::from(m.lhu(state + 8));
        let y = u32::from(m.lhu(state + 10));
        let x = x.wrapping_add(signed(rng, 0x400)) & 0xffff;
        let y = y.wrapping_add(signed(rng, 0x400)) & 0xffff;
        m.sw(state + 8, x | (y << 16));
    }
    [state, sun, height, 0]
}

/// 8008B310: the eye height (scratch +0xC, at most 0x7800 units up, where
/// the moon is drawn), the moon's direction (state +8: near the view axis,
/// the GTE rotation's third row, scaled, or the captured one) and colour.
fn moon_case(m: &mut Machine, entry: &Snapshot, rng: &mut oracle::Rng) -> [u32; 4] {
    let state = entry.gpr(4);
    let eye = signed(rng, 0x380_0000).wrapping_sub(0x380_0000);
    m.sw(SCRATCH + 0xc, eye);
    if coin(rng) {
        let up = ((eye as i32) >> 12).wrapping_neg();
        let (row, r33) = (m.gte.control[3], m.gte.control[4]);
        let scale = pick(rng, &[1, 2, 4]) as i32;
        let x = i32::from(row as u16 as i16) * scale + signed(rng, 0x200) as i32;
        let y = i32::from((row >> 16) as u16 as i16) * scale + signed(rng, 0x200) as i32;
        let z = i32::from(r33 as u16 as i16) * scale + signed(rng, 0x200) as i32;
        // The routine uses `up - y` as the height.
        let y = up.wrapping_sub(y);
        m.sw(state + 8, (x as u32 & 0xffff) | ((y as u32) << 16));
        m.sw(state + 12, z as u32);
    }
    let colour = if coin(rng) { entry.gpr(5) } else { rng.next() };
    [state, colour, 0, 0]
}

/// 80037E50: the mission state's phase (+0x29, the translated phases),
/// pending flags (+0x2A) and values (+0x23..+0x26), the mission index (with
/// 0x36) and the next index (context +0x3BD).
fn step_case(m: &mut Machine, _: &Snapshot, rng: &mut oracle::Rng) -> [u32; 4] {
    vary_mission(m, rng);
    let context = context(m);
    if coin(rng) {
        m.sh(context + 0x38c, 0x36);
    }
    let next = pick(rng, &[0, 0x36, 0x12]);
    m.sb(context + 0x3bd, next as u8);
    let state = gp_word(m, 0x340);
    let phase = pick(rng, &[0, 1, 2, 3, 4, 5, 9]);
    m.sb(state + 0x29, phase as u8);
    let flags = pick(rng, &[0, 0, 0, 1, 2, 4, 6]);
    m.sb(state + 0x2a, flags as u8);
    for offset in 0x23..0x27 {
        m.sb(state + offset, (rng.next() & 3) as u8);
    }
    [0; 4]
}

/// Captured routines of the part whose single capture takes one path, with
/// the fields their JP differences depend on varied around it: the sun and
/// the moon (the corona and glow, the flash, the screen bounds) and the
/// mission state step (every translated phase).
#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn oracle_captured_routines_varied() {
    run_families(&[
        Family {
            start: 0x8008_ba8c,
            base: 0x8008_ba8c,
            cases: 48,
            setup: sun_case,
            varied: "corona and flash bytes, eye height, direction",
        },
        Family {
            start: 0x8008_b310,
            base: 0x8008_b310,
            cases: 24,
            setup: moon_case,
            varied: "eye height, direction, colour",
        },
        Family {
            start: 0x8003_7e50,
            base: 0x8003_7e50,
            cases: 48,
            setup: step_case,
            varied: "phase, pending flags, values, mission index, next index",
        },
    ]);
}

// ---- the model-ray carry of the projectile and particle passes ---------------

/// Run `f` with the development fallback forcing `spec` (a
/// `Fallback::force` spec; empty: nothing forced) on this thread.
#[cfg(feature = "dev-fallback")]
fn forced<T>(spec: &str, f: impl FnOnce() -> T) -> T {
    let mut fallback = ac3_fallback::Fallback::new(ac3_fallback::Version::Jp);
    if !spec.is_empty() {
        fallback.force(spec).expect("force spec");
    }
    ac3_fallback::install(fallback);
    let out = f();
    ac3_fallback::uninstall();
    out
}

/// The RAM (outside the stack below `sp` and the `skip` ranges), scratchpad
/// and GTE differences of two runs, or `None`.
#[cfg(feature = "dev-fallback")]
fn run_differences(
    a: &Machine,
    b: &Machine,
    sp: u32,
    skip: &[(u32, u32, &'static str)],
) -> Option<String> {
    let stack = snapshot::STACK_START..(sp & 0x1f_ffff) as usize;
    let ram: Vec<u32> = (0..0x20_0000usize)
        .filter(|&i| !stack.contains(&i) && a.ram()[i] != b.ram()[i])
        .map(|i| 0x8000_0000 + i as u32)
        .filter(|&a| excluded(a, skip).is_none())
        .collect();
    let scratch = (0..0x400)
        .filter(|&i| a.scratch()[i] != b.scratch()[i])
        .count();
    let data: Vec<usize> = (0..32)
        .filter(|&i| a.gte.data[i] != b.gte.data[i])
        .collect();
    let control: Vec<usize> = (0..32)
        .filter(|&i| a.gte.control[i] != b.gte.control[i])
        .collect();
    if ram.is_empty() && scratch == 0 && data.is_empty() && control.is_empty() {
        return None;
    }
    Some(format!(
        "ram {} {:08x?} scratch {scratch} gte data {data:?} control {control:?}",
        ram.len(),
        &ram[..ram.len().min(12)]
    ))
}

/// Install a model of one split face (flag 0x200) as model `kind` (res1's
/// construction): its two triangles' normals face away from and toward a
/// ray along +z, so the ray meets the second triangle, whose plane point is
/// the model ray's carried register. Model, vertices and face lists go past
/// the arena pointer (context +0x1C), the normals at the last offsets of the
/// normal table.
#[cfg(feature = "dev-fallback")]
fn split_face_model(m: &mut Machine, rng: &mut oracle::Rng, kind: u32) {
    let context = context(m);
    let block = (m.lw(context + 0x1c) + 0x4000) & !0xf;
    let (model, vertices, faces, empty) = (block, block + 0x40, block + 0x100, block + 0x180);
    m.sw(model + 4, vertices);
    m.sw(model + 0xc, empty);
    m.sw(model + 0x10, faces);
    m.sw(empty, 0);
    let size = 0x20 + rng.next() % 0x40;
    let corners = [(-1i32, -1i32), (1, -1), (-1, 1), (1, 1), (0, 0), (0, 0)];
    m.sw(vertices, 6);
    for (i, (x, y)) in corners.into_iter().enumerate() {
        let group = vertices + 4 + (i as u32 / 3) * 0x14;
        let k = i as u32 % 3;
        let x = x.wrapping_mul(size as i32) as u32;
        let y = y.wrapping_mul(size as i32) as u32;
        m.sw(group + 4 * k, (x & 0xffff) | (y << 16));
        m.sh(group + 0xc + 2 * k, 0);
    }
    let gameplay = gameplay(m);
    let normals = m.lw(gameplay + 0x25c);
    let (away, toward) = (0xffe0u32, 0xffe8u32);
    m.sw(normals + away, 0);
    m.sw(normals + away + 4, 0x1000);
    m.sw(normals + toward, 0);
    m.sw(normals + toward + 4, 0xf000);
    m.sw(faces, 1);
    m.sw(faces + 4, 0x200);
    m.sh(faces + 8, away as u16);
    m.sh(faces + 0xa, toward as u16);
    m.sw(faces + 0x14, 0x0302_0100);
    let models = m.lw(gameplay + 0x258);
    m.sw(models + kind * 4, model);
}

/// Make entity `slot` (32..63: the passes take the struck entity from the
/// word's bits 0..5, and a detailed model hit needs bit 0x20 too) a
/// collision target with a detailed model hit (word 0x4200 collected,
/// 0x1000 detailed, 0x20 model, team bit 0x40; byte +5 bits 1..2 clear) of
/// model `kind` and radius `radius`. Returns the entity.
#[cfg(feature = "dev-fallback")]
fn model_target(m: &mut Machine, slot: u32, kind: u32, radius: u32) -> u32 {
    let object = entity(m, slot);
    let word = m.lw(object);
    m.sw(object, (word & !0x3f) | slot | 0x5240);
    let byte = m.lbu(object + 5);
    m.sb(object + 5, byte & !6);
    m.sb(object + 4, kind as u8);
    m.sh(object + 0x4a, radius as u16);
    object
}

/// The number of entities the passes collect as targets (word & 0x4200).
#[cfg(feature = "dev-fallback")]
fn collected(m: &mut Machine) -> u32 {
    (0..64)
        .filter(|&slot| {
            let object = entity(m, slot);
            m.lw(object) & 0x4200 == 0x4200
        })
        .count() as u32
}

/// The sound voice records (24 of 0x14 bytes from 800C5830). After a model
/// hit, the particles' impact sound (8002F460) comes out with another pan:
/// it reads stack words below the pass's frame that the original ray
/// chain (8009B4EC, 8009A944 and the face scans) left as register saves and
/// the native one, whose frames hold no saves, does not. The hit itself
/// agrees: interpreted together from a stub that sets s4, the original
/// chain and `object_ray_jp` with that carry leave the same RAM outside the
/// stack, scratchpad and GTE, and every other byte of the pass agrees.
#[cfg(feature = "dev-fallback")]
const VOICE_RECORDS: &[(u32, u32, &str)] = &[(
    0x800c_5830,
    0x800c_5830 + 24 * 0x14,
    "the impact sound's pan reads the ray chain's stack leftovers",
)];

/// One case of a pass (`base` holds a record aimed at the model target
/// `object`). Runs the pass three ways: the patch (`object_ray_jp` with the
/// carry), the original instructions of the chain (pass -> 8009B4EC ->
/// 8009A944 interpreted together, so the registers carry over), and the
/// pass and 8009B4EC interpreted with the model ray patched at carry 0.
/// Every other routine runs as the crate stands in all three: the
/// particles' sound call (8002F460) passes on the `a3` 8009B4EC leaves,
/// which the translations do not model (they take no fourth argument), so
/// interpreting it would compare that rather than the carry. Returns (the patch and the chain agree
/// or the difference, the model ray decided the outcome: the patch differs
/// from a run with the target's model test off (byte +5 bit 1), carry 0
/// would differ).
#[cfg(feature = "dev-fallback")]
fn ray_case(
    base: &Machine,
    routine: u32,
    object: u32,
    skip: &[(u32, u32, &'static str)],
) -> (Result<(), String>, bool, bool) {
    let sp = base.sp;
    let run = |m: &Machine, spec: &str| {
        let mut m = m.clone();
        forced(spec, || dispatch::call(&mut m, routine, [0; 4]));
        let fault = m.take_fault();
        (m, fault)
    };
    let (native, nf) = run(base, "");
    let (original, of) = run(base, &format!("{routine:x},8009b4ec,8009a944"));
    let (zero, _) = run(base, &format!("{routine:x},8009b4ec"));
    let mut flat = base.clone();
    let byte = flat.lbu(object + 5);
    flat.sb(object + 5, byte | 2);
    let (flat, _) = run(&flat, "");
    let agree = if nf != of {
        Err(format!("faults: patch {nf:?}, original {of:?}"))
    } else {
        match run_differences(&native, &original, sp, skip) {
            Some(d) => Err(d),
            None => Ok(()),
        }
    };
    let byte = (object + 5 - 0x8000_0000) as usize;
    let stack = snapshot::STACK_START..(sp & 0x1f_ffff) as usize;
    let hit = (0..0x20_0000usize)
        .any(|i| i != byte && !stack.contains(&i) && native.ram()[i] != flat.ram()[i]);
    let matters = run_differences(&zero, &original, sp, skip).is_some();
    (agree, hit, matters)
}

/// 8006A11C (projectiles) and 8006B504 (particles) hand their s4 through
/// 8009B4EC to the JP model ray 8009A944 as its carried register (the
/// projectile: the last active record's exclusion mask; the particles: the
/// scratchpad target cursor, 0x1F800000 + 0x10 per target). Each case aims
/// one record of the pass at a model target and runs the pass as the patch,
/// as the original chain (registers carried over) and with a zero carry
/// ([`ray_case`]): the first two must agree. Random models from the model
/// table, and split-face models (res1's construction) whose ray meets the
/// face planed at the carried address, where the plane point is written
/// (the carry 0 run then differs).
#[cfg(feature = "dev-fallback")]
#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn passes_hand_the_model_ray_their_carry() {
    let Some((_, entry, _)) = captured(0x8003_809c, 1).into_iter().next() else {
        panic!("no capture of 8003809C");
    };
    let image = entry.machine();
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let mut rng = oracle::Rng(0x8006_a11c);
    let in_ram = |a: u32| (0x8001_0000..0x801f_0000).contains(&a) && a & 3 == 0;
    let mut failures = Vec::new();
    let mut report = Vec::new();
    for routine in [0x8006_a11cu32, 0x8006_b504] {
        let (mut cases, mut hits, mut matters) = (0, 0, 0);
        for case in 0..96u32 {
            let mut m = image.clone();
            let crafted = case % 2 == 1;
            let kind = if crafted {
                let kind = 1 + case % 8;
                split_face_model(&mut m, &mut rng, kind);
                kind
            } else {
                let gameplay = gameplay(&mut m);
                let models = m.lw(gameplay + 0x258);
                let kind = rng.next() % 0x30;
                let model = m.lw(models + kind * 4);
                if !in_ram(model)
                    || !in_ram(m.lw(model + 4))
                    || !in_ram(m.lw(model + 0xc))
                    || !in_ram(m.lw(model + 0x10))
                {
                    continue;
                }
                kind
            };
            let slots: Vec<u32> = (0x20..0x40)
                .filter(|&s| in_ram(entity(&mut m, s)))
                .collect();
            if slots.is_empty() {
                continue;
            }
            let slot = slots[rng.next() as usize % slots.len()];
            let object = model_target(&mut m, slot, kind, 0x200);
            let depth = 0x40 + rng.next() % 0x100;
            let spread = if crafted {
                [0, 0]
            } else {
                [
                    (rng.next() % 0x40).wrapping_sub(0x20),
                    (rng.next() % 0x40).wrapping_sub(0x20),
                ]
            };
            let block = gp_word(&mut m, 0x50c);
            // The record: team word without bit 0x40 (the target's team).
            let team = rng.next() & 0x180;
            let (position, velocity, team_at) = if routine == 0x8006_a11c {
                let index = rng.next() % 32;
                let active = m.lw(block + 0x1548);
                m.sw(block + 0x1548, active | (1 << index));
                let record = block + 0xc8 + index * 0xa4;
                (record + 0x1c, record + 0x38, record + 0x6c)
            } else {
                let index = rng.next() % 0x80;
                let mask_at = block + 0x33d0 + (index / 32) * 4;
                let mask = m.lw(mask_at);
                m.sw(mask_at, mask | (1 << (index % 32)));
                let record = block + 0x15d0 + index * 60;
                (record + 8, record + 0x28, record + 0x38)
            };
            m.sw(team_at, team);
            for i in 0..3u32 {
                let at = m.lw(object + 8 + 4 * i);
                let (offset, speed) = match i {
                    2 => (depth.wrapping_neg(), 2 * depth),
                    _ => (spread[i as usize], 0),
                };
                m.sw(position + 4 * i, at.wrapping_add(offset << 12));
                m.sw(velocity + 4 * i, speed << 12);
            }
            if crafted {
                // The plane point at the carried address.
                let carry = if routine == 0x8006_a11c {
                    0x8000_0000 | (!team & 0x1c0)
                } else {
                    SCRATCH + collected(&mut m) * 0x10
                };
                for i in 0..3u32 {
                    let value = if i == 2 {
                        rng.next() % 0x100
                    } else {
                        rng.next() % 0x40
                    };
                    m.sh(carry + 2 * i, value as u16);
                }
            }
            let skip = if routine == 0x8006_b504 {
                VOICE_RECORDS
            } else {
                &[][..]
            };
            let (agree, hit, differs) = ray_case(&m, routine, object, skip);
            if let Err(e) = agree {
                failures.push(format!("{routine:08x} case {case} model {kind:#x}: {e}"));
            }
            cases += 1;
            hits += usize::from(hit);
            matters += usize::from(differs);
        }
        report.push(format!(
            "{routine:08x}: {cases} cases, {hits} where the model ray decided the hit, {matters} \
             where a zero carry would differ"
        ));
        if hits == 0 || matters == 0 {
            failures.push(format!(
                "{routine:08x}: no hit or no case where the carry matters"
            ));
        }
    }
    std::panic::set_hook(hook);
    for line in &report {
        eprintln!("{line}");
    }
    for line in &failures {
        eprintln!("FAILED {line}");
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// The model ray with each pass's kind of carry, exactly: the original
/// 8009B4EC -> 8009A944 (with the face scans 80099428/80099750) interpreted
/// together by the oracle from a four-instruction stub at 80099418 that sets
/// s4 (`lui/ori s4; j 8009B4EC`), against `object_ray_jp` with that carry,
/// for a particle aimed at a split-face target: RAM outside the stack,
/// scratchpad, GTE and `v0` must agree. The carries: the particles'
/// scratchpad cursor (0x1F800000 + 0x10 per target) and a projectile's
/// exclusion mask as a low-RAM address with a plane point written there.
#[cfg(feature = "dev-fallback")]
#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn model_ray_chain_matches_with_the_carry() {
    let Some((_, entry, _)) = captured(0x8003_809c, 1).into_iter().next() else {
        panic!("no capture of 8003809C");
    };
    let image = entry.machine();
    let mut rng = oracle::Rng(0x8009_b4ec);
    let mut cases = 0;
    for case in 0..32u32 {
        let mut m = image.clone();
        let kind = 1 + case % 8;
        split_face_model(&mut m, &mut rng, kind);
        let slot = 0x20 + case % 0x20;
        if !in_ram(entity(&mut m, slot)) {
            continue;
        }
        let object = model_target(&mut m, slot, kind, 0x200);
        let block = gp_word(&mut m, 0x50c);
        let record = block + 0x15d0 + (case % 0x80) * 60;
        let depth = 0x40 + rng.next() % 0x100;
        for i in 0..3u32 {
            let at = m.lw(object + 8 + 4 * i);
            let (offset, speed) = if i == 2 {
                (depth.wrapping_neg(), 2 * depth)
            } else {
                (0, 0)
            };
            m.sw(record + 8 + 4 * i, at.wrapping_add(offset << 12));
            m.sw(record + 0x28 + 4 * i, speed << 12);
        }
        let carry = if case % 2 == 0 {
            SCRATCH + collected(&mut m) * 0x10
        } else {
            let carry = 0x8000_0000 | (0x40 + (rng.next() % 7) * 0x40);
            for i in 0..3u32 {
                let value = if i == 2 {
                    rng.next() % 0x100
                } else {
                    rng.next() % 0x40
                };
                m.sh(carry + 2 * i, value as u16);
            }
            carry
        };
        let sp = m.sp - 0x2d0;
        m.sp = sp;
        let out = sp + 0x260;
        m.sw(sp + 0x10, SCRATCH);
        let stub = 0x8009_9418u32;
        m.sw(stub, 0x3c14_0000 | (carry >> 16));
        m.sw(stub + 4, 0x3694_0000 | (carry & 0xffff));
        m.sw(stub + 8, 0x0800_0000 | ((0x8009_b4ec >> 2) & 0x03ff_ffff));
        m.sw(stub + 12, 0);
        let args = [record + 8, record + 0x28, object, out];
        let mut native = m.clone();
        let nv = ac3_flight_jp::jp::res_p1_player::object_ray_jp(
            &mut native,
            args[0],
            args[1],
            args[2],
            args[3],
            SCRATCH,
            carry,
        );
        let mut original = m.clone();
        let ov = oracle::run(&mut original, stub, 0x8009_b6ac, args);
        let label = format!("case {case} model {kind:#x} carry {carry:08x}");
        assert_eq!(nv, ov, "{label}: v0");
        assert_eq!(
            native.take_fault(),
            original.take_fault(),
            "{label}: faults"
        );
        if let Some(d) = run_differences(&native, &original, sp, &[]) {
            panic!("{label}: {d}");
        }
        cases += 1;
    }
    eprintln!("model ray chain: {cases} cases agree with the carry");
    assert!(cases > 0);
}
