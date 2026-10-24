//! Validation of JP overlay 2, part 4 (800EB944..800F5834; the `jp/ov2_p4*`
//! patch modules).
//!
//! * `captured_calls_reproduce`: every JP Redux entry/return pair of a
//!   claimed routine of the part (`analysis/redux/*/snapshots-module2.jsonl`)
//!   runs natively from its entry and must reproduce the return snapshot
//!   (RAM outside the stack, scratchpad, GTE, `v0`), as `tests/captures.rs`.
//! * `oracle_matches_the_jp_original`: every claimed routine against the JP
//!   original (the test-only oracle, `support::oracle`) on JP overlay-2
//!   machine states: the captured entry snapshots of the part's routines and
//!   the module-2 RAM dumps of the reach runs, each with randomised
//!   arguments and randomised fields of the block the routine works on.
//!   RAM outside the stack window, scratchpad, GTE, `sp`, the first fault,
//!   the host-call log and `v0` must be equal. A case whose original stops
//!   at a compiler trap (`break`) compares the fault only: the oracle stops
//!   there and the translation continues with the MIPS result. Per-routine
//!   cases, instruction coverage of the original and the paths not reached
//!   are printed.
//!
//! `cargo test --release -p ac3-flight-jp --features dev-fallback --test
//! ov2_p4 -- --include-ignored --nocapture` (untranslated callees run
//! interpreted). Data: `AC3_JP_DIR` (default `~/ac3-jp`).
mod support;

use ac3_flight_jp::snapshot::{self, Snapshot, SnapshotInfo};
use ac3_flight_jp::{Fault, Machine, dispatch};
use std::collections::{BTreeMap, BTreeSet};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use support::{oracle, scan};

/// The part: 800EB944 .. the end of 800F56D8.
const LOW: u32 = 0x800e_b944;
const HIGH: u32 = 0x800f_5834;
/// Guest `sp` for RAM-dump states (the differential harness's).
const SP: u32 = 0x801f_ff00;
/// The stack window (not compared), as the snapshot comparisons.
const STACK: std::ops::Range<usize> = 0x1f_c000..0x20_0000;
/// The capture runs with overlay-2 pairs.
const RUNS: [&str; 4] = ["boot-002", "story-002", "flight-002", "save-002"];
/// Module-2 RAM dumps of the reach runs (state 3): the broadcast, the hub,
/// the archive replay, the briefing, aircraft select, the hub after the
/// mission, the save screen, after the save.
const DUMPS: [&str; 9] = [
    "story-001/ram-017800",
    "story-001/ram-030300",
    "story-001/ram-041610",
    "story-001/ram-046300",
    "story-001/ram-049570",
    "movie-001/ram-020500",
    "flight-001/ram-053900",
    "save-001/ram-054990",
    "save-001/ram-056390",
];

fn redux() -> PathBuf {
    scan::jp_dir().join("analysis/redux")
}

/// The part's routines from the map: start -> end.
fn routines() -> BTreeMap<u32, u32> {
    scan::jp_map(&scan::jp_dir())
        .iter()
        .filter(|r| r.region == "ov2" && (LOW..HIGH).contains(&r.jp))
        .map(|r| (r.jp, r.jp + r.size))
        .collect()
}

/// The addresses the part's modules claim.
fn claimed() -> BTreeSet<u32> {
    scan::jp_overlay_claims()
        .into_iter()
        .filter(|((module, address), sites)| {
            *module == 2
                && (LOW..HIGH).contains(address)
                && sites.iter().all(|(f, _)| f.starts_with("jp/ov2_p4"))
        })
        .map(|((_, address), _)| address)
        .collect()
}

/// The development fallback for untranslated callees (thread-local).
fn install() {
    #[cfg(feature = "dev-fallback")]
    if ac3_fallback::active().is_none() {
        ac3_fallback::install(ac3_fallback::Fallback::new(ac3_fallback::Version::Jp));
    }
}

fn panic_text(e: &(dyn std::any::Any + Send)) -> String {
    e.downcast_ref::<String>()
        .cloned()
        .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default()
}

/// Every overlay-2 pair of the part: (directory, entry, return).
fn pairs() -> Vec<(PathBuf, SnapshotInfo, SnapshotInfo)> {
    let mut out = Vec::new();
    for run in RUNS {
        let path = redux().join(run).join("snapshots-module2.jsonl");
        let Ok(index) = snapshot::read_index(&path) else {
            continue;
        };
        for entry in index
            .iter()
            .filter(|s| s.stage == "entry" && (LOW..HIGH).contains(&s.callee))
        {
            let exit = index.iter().find(|s| {
                s.callee == entry.callee
                    && s.stage == "return"
                    && s.sequence > entry.sequence
                    && s.ra == entry.ra
                    && s.registers.gpr[29] == entry.registers.gpr[29]
            });
            if let Some(exit) = exit {
                out.push((
                    path.parent().unwrap().to_path_buf(),
                    entry.clone(),
                    exit.clone(),
                ));
            }
        }
    }
    out
}

/// RAM that interrupt handlers and host-supplied device values write while
/// a routine waits on the GPU (DrawSync, LoadImage, StoreImage, ClearImage)
/// or runs the disc-check callbacks: the JP addresses of the ranges the PAL
/// tests identified (`crates/ac3-flight/tests/loading.rs` `INTERRUPT_OWNED`,
/// `tests/ov2_core.rs` `ALLOWED`), re-addressed through the data run +0x2180
/// (JP_PORT_PLAN 5), plus one ClearImage word seen here. Excluded only for
/// the routines in [`WAITS`]; each pair reports how many bytes fell there.
const HANDLER_OWNED: &[(u32, u32, &str)] = &[
    (
        0x8000_0000,
        0x8001_0000,
        "BIOS kernel RAM (exception dispatch, event blocks, saved context)",
    ),
    (
        0x800b_46ec,
        0x800b_46f0,
        "sound interrupt flag (PAL 800B256C, set by the sound-timer handler)",
    ),
    (
        0x800b_50ac,
        0x800b_60c4,
        "libetc interrupt context and its interrupt stack (PAL 800B2F2C..800B3F44)",
    ),
    (0x800b_6174, 0x800b_6178, "VSync counter (PAL 800B3FF4)"),
    (
        0x800b_6190,
        0x800b_6194,
        "GPU DMA callback slot (PAL 800B4010)",
    ),
    (
        0x800b_ba8c,
        0x800b_ba94,
        "libgpu queue head/tail (PAL 800B990C)",
    ),
    (
        0x800b_ba94,
        0x800b_ba98,
        "libgpu saved interrupt mask (PAL 800B9914: the SetIntrMask result, a host value)",
    ),
    (
        0x800b_baa0,
        0x800b_baa8,
        "libgpu timeout and DMA poll counter (PAL 800B9920)",
    ),
    (
        0x800c_7df4,
        0x800c_7df6,
        "sound driver tick state (PAL 800C5BCC)",
    ),
    (
        0x800c_7e98,
        0x800c_7e9a,
        "sound driver tick state (PAL 800C5C70)",
    ),
    (
        0x800c_a310,
        0x800c_a314,
        "ClearImage: a word computed from the GPU status (a host value)",
    ),
    (
        0x800c_a31c,
        0x800c_a320,
        "ClearImage draw mode (PAL 800C80F4: the GPU status, a host value)",
    ),
    (
        0x800c_a334,
        0x800c_a340,
        "ClearImage drawing area (PAL 800C810C: the GPU status, a host value)",
    ),
    (
        0x800c_a458,
        0x800c_bc58,
        "libgpu queue entries (PAL 800C8230: transfers queued while a DMA ran)",
    ),
];

/// The routines whose call tree waits on the GPU or the disc.
const WAITS: &[u32] = &[
    0x800e_b9ec,
    0x800e_bd48,
    0x800e_c4ec,
    0x800e_c6b4,
    0x800e_cda8,
    0x800e_ce38,
    0x800e_cec8,
    0x800e_cf88,
    0x800e_d000,
    0x800e_d0d4,
    0x800e_dad4,
    0x800e_dcc0,
    0x800f_1430,
    0x800f_1a7c,
    0x800f_1d04,
];

/// Device values a callee stores in RAM, which the test device does not
/// supply: 800EB9EC's 800E3FCC stores the eight memory-card event
/// descriptors (OpenEvent results) at `[0x80112DF0] + 4..0x24`; 800ED0D4
/// reads the palette rectangle `[HUB] + 0xD8` back from VRAM (StoreImage)
/// into `[[HUB] + 0xD0]`. Located on the return snapshot.
fn host_values(callee: u32, exit: &Snapshot) -> Vec<(u32, u32, &'static str)> {
    let word = |a: u32| {
        let i = (a & 0x1f_ffff) as usize;
        u32::from_le_bytes(exit.ram[i..i + 4].try_into().unwrap())
    };
    let half = |a: u32| {
        let i = (a & 0x1f_ffff) as usize;
        u32::from(u16::from_le_bytes(exit.ram[i..i + 2].try_into().unwrap()))
    };
    match callee {
        0x800e_b9ec => {
            let card = word(0x8011_2df0);
            vec![(
                card.wrapping_add(4),
                card.wrapping_add(0x24),
                "card event descriptors (OpenEvent results)",
            )]
        }
        0x800f_1430 => {
            let b = word(0x8011_2db0);
            let pixels = word(b.wrapping_add(0x1694));
            vec![(
                pixels,
                pixels.wrapping_add(0x8000),
                "StoreImage destination (VRAM contents of the saved screen)",
            )]
        }
        0x800e_d0d4 => {
            let hub = word(HUB);
            let pixels = word(hub.wrapping_add(0xd0));
            let rect = hub.wrapping_add(0xd8);
            let bytes = half(rect + 4) * half(rect + 6) * 2;
            vec![(
                pixels,
                pixels.wrapping_add(bytes),
                "StoreImage destination (VRAM contents)",
            )]
        }
        _ => Vec::new(),
    }
}

/// Replay one pair: Ok((bytes the original changed, differing bytes in
/// [`HANDLER_OWNED`])) or Err(cause).
fn replay(
    directory: &Path,
    entry: &SnapshotInfo,
    exit: &SnapshotInfo,
) -> Result<(usize, usize), String> {
    let entry = Snapshot::load(directory, entry).map_err(|e| format!("load: {e}"))?;
    let exit = Snapshot::load(directory, exit).map_err(|e| format!("load: {e}"))?;
    let mut m = entry.machine();
    let args = [entry.gpr(4), entry.gpr(5), entry.gpr(6), entry.gpr(7)];
    let callee = entry.info.callee;
    let v0 = panic::catch_unwind(AssertUnwindSafe(|| dispatch::call(&mut m, callee, args)))
        .map_err(|e| format!("panic: {}", panic_text(&*e)))?;
    if let Err(fault) = m.take_fault() {
        return Err(format!("fault: {fault}"));
    }
    let mut d = snapshot::compare(&m, &exit);
    let before = d.ram.len();
    if WAITS.contains(&callee) {
        let host = host_values(callee, &exit);
        d.ram.retain(|a| {
            !HANDLER_OWNED
                .iter()
                .chain(&host)
                .any(|(s, e, _)| (*s..*e).contains(a))
        });
    }
    let allowed = before - d.ram.len();
    if !d.is_empty() {
        let values: Vec<String> = d
            .ram
            .iter()
            .take(8)
            .map(|&a| {
                let i = (a & 0x1f_ffff) as usize;
                format!(
                    "{a:08x}: {:02x} -> {:02x}, native {:02x}",
                    entry.ram[i],
                    exit.ram[i],
                    m.ram()[i]
                )
            })
            .collect();
        return Err(format!("state differs: {} {values:?}", d.summary()));
    }
    if v0 != exit.gpr(2) && !spec(callee).void {
        return Err(format!("v0 {v0:#x}, original {:#x}", exit.gpr(2)));
    }
    Ok((snapshot::changed_bytes(&entry, &exit), allowed))
}

#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn captured_calls_reproduce() {
    install();
    let claimed = claimed();
    assert!(!claimed.is_empty(), "the scan found the part's claims");
    let hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let (mut pass, mut failures, mut skipped) = (0, Vec::new(), BTreeSet::new());
    let mut seen = BTreeSet::new();
    for (directory, entry, exit) in pairs() {
        if !claimed.contains(&entry.callee) {
            skipped.insert(entry.callee);
            continue;
        }
        seen.insert(entry.callee);
        let run = directory
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        match replay(&directory, &entry, &exit) {
            Ok((n, allowed)) => {
                pass += 1;
                println!(
                    "pass {:08x} {run} #{}: {n} bytes changed, {allowed} differing bytes handler-owned",
                    entry.callee, entry.sequence
                );
            }
            Err(e) => failures.push(format!(
                "{:08x} {run} #{}: {e}",
                entry.callee, entry.sequence
            )),
        }
    }
    panic::set_hook(hook);
    println!(
        "captures: {pass} pairs of {} claimed routines reproduced; {} failures; captured but not claimed: {:08x?}",
        seen.len(),
        failures.len(),
        skipped
    );
    for f in &failures {
        println!("  FAIL {f}");
    }
    assert!(pass > 0);
    assert!(failures.is_empty(), "{failures:#?}");
}

// ---- the differential oracle ------------------------------------------------------

/// Deterministic generator (the PAL oracle's LCG).
struct Rng(u32);
impl Rng {
    /// 24 bits: the LCG state through a mixing hash (an LCG's low bits
    /// repeat with the low bits of the seed, which the variants share).
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(1_103_515_245).wrapping_add(12345);
        let mut z = self.0 ^ (self.0 >> 16);
        z = z.wrapping_mul(0x7feb_352d);
        z ^= z >> 15;
        z = z.wrapping_mul(0x846c_a68b);
        (z ^ (z >> 16)) >> 8
    }
    fn below(&mut self, n: u32) -> u32 {
        self.next() % n.max(1)
    }
}

/// A field the cases randomise: `width` bytes at `[pointer] + offset`
/// (`pointer` 0: `offset` is absolute), values below `limit` (0: any).
#[derive(Clone, Copy)]
struct Knob {
    pointer: u32,
    offset: u32,
    width: u8,
    limit: u32,
}

const fn knob(pointer: u32, offset: u32, width: u8, limit: u32) -> Knob {
    Knob {
        pointer,
        offset,
        width,
        limit,
    }
}

/// The text block `[0x80112E00]` + 0x8000.
const TEXT: u32 = 0x8011_2e00;
const TEXT_KNOBS: &[Knob] = &[
    knob(TEXT, 0x8000 + 0x28d0, 4, 12),
    knob(TEXT, 0x8000 + 0x28e4, 2, 0x200),
    knob(TEXT, 0x8000 + 0x28e6, 2, 0x200),
    knob(TEXT, 0x8000 + 0x28f0, 2, 0x20),
    knob(TEXT, 0x8000 + 0x28f2, 2, 4),
    knob(TEXT, 0x8000 + 0x28f4, 2, 0x400),
    knob(TEXT, 0x8000 + 0x28f6, 2, 0x200),
    knob(TEXT, 0x8000 + 0x2900, 1, 0x20),
    knob(TEXT, 0x8000 + 0x2901, 1, 0x20),
    knob(TEXT, 0x8000 + 0x2902, 1, 0x40),
    knob(TEXT, 0x8000 + 0x2903, 1, 2),
    knob(TEXT, 0x8000 + 0x2908, 1, 2),
    knob(TEXT, 0x8000 + 0x2909, 1, 4),
    knob(TEXT, 0x8000 + 0x290a, 1, 8),
    knob(TEXT, 0x8000 + 0x290b, 1, 3),
    knob(TEXT, 0x8000 + 0x290c, 2, 0x100),
    knob(TEXT, 0x8000 + 0x290e, 2, 0x100),
    knob(TEXT, 0x8000 + 0x2910, 2, 0x100),
];

/// The disc-check block `[0x80112DF8]`, the hub `[0x80112DC4]`, the story
/// state `[0x80112DA8]`, the tint block gp+0x2D4, the context.
const DISC: u32 = 0x8011_2df8;
const HUB: u32 = 0x8011_2dc4;
const STORY: u32 = 0x8011_2da8;
const TINT: u32 = ac3_flight_jp::GP + 0x2d4;
const CONTEXT: u32 = ac3_flight_jp::GP + 0x2ec;
const DISC_KNOBS: &[Knob] = &[
    knob(DISC, 0, 1, 2),
    knob(DISC, 1, 1, 8),
    knob(DISC, 2, 1, 3),
    knob(DISC, 4, 1, 2),
    knob(DISC, 7, 1, 0x30),
    knob(DISC, 8, 1, 2),
    knob(DISC, 9, 1, 3),
    knob(HUB, 0xe0, 4, 6),
    knob(STORY, 0x14, 4, 2),
    knob(TINT, 0x10, 1, 0x100),
    knob(CONTEXT, 0x124, 4, 0x20),
    knob(CONTEXT, 0x38c, 2, 0x30),
    // the frame counter (any word: the blend phase rounds negatives)
    knob(CONTEXT, 0x20, 4, 0),
    // the CD state block gp+0x34 (+0 state, +4 disc) of 8002B528
    knob(ac3_flight_jp::GP + 0x34, 0, 4, 6),
    knob(ac3_flight_jp::GP + 0x34, 4, 4, 3),
];
/// The broadcast stream block `[0x80112DFC]`.
const STREAM: u32 = 0x8011_2dfc;
const STREAM_KNOBS: &[Knob] = &[
    knob(STREAM, 0, 4, 2),
    knob(STREAM, 8, 2, 6),
    knob(STREAM, 0xa, 2, 24),
    knob(STREAM, 0x1a0, 4, 5),
    knob(STREAM, 0x1b0, 2, 9),
    knob(STREAM, 0x1b2, 2, 9),
    knob(STREAM, 0x1b8, 4, 200),
    knob(STREAM, 0x150, 2, 0x400),
    knob(STREAM, 0x152, 2, 0x200),
    knob(STREAM, 0x158, 2, 0x400),
    knob(STREAM, 0x15a, 2, 0x200),
];

/// The story-event block `[0x80112DB0]`.
const STORY_EVENTS: u32 = 0x8011_2db0;
const STORY_KNOBS: &[Knob] = &[
    knob(STORY_EVENTS, 0x10, 4, 40),
    knob(STORY_EVENTS, 0x28, 4, 3),
    knob(STORY_EVENTS, 0xf38, 4, 2),
    knob(STORY_EVENTS, 0xf3c, 4, 200),
    knob(STORY_EVENTS, 0xf40, 4, 200),
    knob(STORY_EVENTS, 0x16b0, 1, 6),
    knob(STORY_EVENTS, 0x16b1, 1, 3),
    knob(STORY_EVENTS, 0x16b2, 2, 12),
    knob(HUB, 0x86, 2, 17),
    knob(HUB, 0x128, 4, 2),
    knob(CONTEXT, 0x12c, 4, 0x20),
];

/// The event phases: the story-event block, the context's story position
/// and mission, the hub's mode, voice and result words.
const SCENE_KNOBS: &[Knob] = &[
    knob(STORY_EVENTS, 0xc, 4, 4),
    knob(STORY_EVENTS, 0x10, 4, 40),
    knob(STORY_EVENTS, 0x28, 4, 3),
    knob(STORY_EVENTS, 0xf38, 4, 3),
    knob(STORY_EVENTS, 0x1698, 4, 2),
    knob(HUB, 0x86, 2, 17),
    knob(HUB, 0xe0, 4, 6),
    knob(HUB, 0x12c, 4, 3),
    knob(HUB, 0x128, 4, 2),
    knob(CONTEXT, 0x12c, 4, 0x20),
    knob(CONTEXT, 0x124, 4, 0x20),
    knob(CONTEXT, 0x38c, 2, 0x40),
    knob(CONTEXT, 0x956, 1, 4),
    knob(CONTEXT, 0x95e, 1, 4),
    knob(CONTEXT, 0x954, 2, 4),
    knob(CONTEXT, 0x44, 4, 2),
    knob(STORY, 0x14, 4, 3),
    knob(STORY_EVENTS, 0x24, 4, 12),
];

/// The captured entries with the broadcast stream's resource loaded.
const BROADCAST: &[u32] = &[0x800e_dcc0, 0x800e_c6b4, 0x800e_c4ec, 0x800e_c340];

/// How the routine at `start` takes its arguments.
#[derive(Clone, Copy)]
enum Arg {
    /// Pointers from the gp area, 0, -1, small and medium numbers.
    Any,
    /// A number below the limit.
    Below(u32),
    /// A string of the overlay's data.
    Text,
    /// A pointer from the gp area.
    Pointer,
    /// As captured (a stack argument left alone).
    Keep,
}

/// How the cases of the routine at `start` are made.
struct Spec {
    args: [Arg; 4],
    /// Stack arguments (the caller's frame at sp + 0x10, 0x14, ...).
    stack: &'static [Arg],
    knobs: &'static [Knob],
    /// Only the routine's own captured entries, with the captured
    /// arguments (they are resources of that state).
    own_only: bool,
    /// `v0` is not compared (the dispatch arm models none).
    void: bool,
    /// When not empty: the states are the captured entries of these
    /// routines (states whose resources the routine needs).
    from: &'static [u32],
    /// A special setup of the mutated variants (fields behind pointers,
    /// arguments that must match the state).
    prepare: Option<fn(&mut Machine, &mut Rng, &mut [u32; 4])>,
}

fn spec(start: u32) -> Spec {
    use Arg::*;
    let any = Spec {
        args: [Any; 4],
        stack: &[],
        knobs: &[],
        own_only: false,
        void: false,
        from: &[],
        prepare: None,
    };
    let text = |args| Spec {
        args,
        knobs: TEXT_KNOBS,
        ..any
    };
    match start {
        0x800f_35b8 => text([Below(12), Any, Any, Any]),
        0x800f_33f8 | 0x800f_34f4 => text([Below(5), Any, Any, Any]),
        0x800f_3f2c | 0x800f_4124 => text([Below(0x100), Any, Any, Any]),
        0x800f_46e4 | 0x800f_4928 => Spec {
            prepare: Some(prepare_text::<2>),
            ..text([Below(0x200), Below(0x200), Text, Any])
        },
        0x800f_4a78 | 0x800f_4ae0 | 0x800f_4b40 => Spec {
            prepare: Some(prepare_text::<0>),
            ..text([Text, Below(100), Below(100), Below(100)])
        },
        0x800f_33a8..0x800f_4c04 => text([Any; 4]),
        0x800e_c11c => Spec {
            args: [Below(8), Any, Any, Any],
            knobs: STREAM_KNOBS,
            ..any
        },
        0x800e_bd48 => Spec {
            knobs: DISC_KNOBS,
            prepare: Some(prepare_leave),
            ..any
        },
        0x800e_b944..0x800e_c11c => Spec {
            knobs: DISC_KNOBS,
            ..any
        },
        0x800e_c4ec => Spec {
            args: [Pointer; 4],
            stack: &[Below(5)],
            knobs: STREAM_KNOBS,
            ..any
        },
        0x800e_c6b4 => Spec {
            stack: &[Keep, Below(5)],
            knobs: STREAM_KNOBS,
            own_only: true,
            ..any
        },
        0x800e_c340..0x800e_cd20 => Spec {
            args: [Pointer; 4],
            knobs: STREAM_KNOBS,
            prepare: Some(prepare_stream),
            ..any
        },
        0x800e_cd4c => Spec {
            own_only: true,
            prepare: Some(prepare_raw::<HOLDER>),
            ..any
        },
        0x800e_cda8 | 0x800e_ce38 => Spec {
            own_only: true,
            prepare: Some(prepare_raw::<DIRECTORY>),
            ..any
        },
        0x800e_cf88 | 0x800e_d000 => Spec {
            own_only: true,
            prepare: Some(prepare_raw::<TIM>),
            ..any
        },
        0x800e_cd50..0x800e_d15c => Spec {
            own_only: true,
            ..any
        },
        // the broadcast stream: only the states where its resource is loaded
        0x800e_dcc0 => Spec {
            args: [Below(12), Any, Any, Any],
            knobs: STORY_KNOBS,
            from: BROADCAST,
            ..any
        },
        0x800e_dad4 => Spec {
            knobs: STORY_KNOBS,
            from: BROADCAST,
            prepare: Some(prepare_stream_command),
            ..any
        },
        0x800e_dc80 => Spec {
            args: [Pointer, Below(0x40), Below(8), Any],
            prepare: Some(prepare_find),
            ..any
        },
        0x800e_d15c..0x800e_dbcc => Spec {
            knobs: STORY_KNOBS,
            prepare: Some(prepare_command),
            ..any
        },
        0x800e_e0d8 | 0x800e_e570 => Spec {
            knobs: STORY_KNOBS,
            prepare: Some(prepare_list),
            ..any
        },
        0x800e_e9e4 => Spec {
            knobs: STORY_KNOBS,
            prepare: Some(prepare_wireframe),
            ..any
        },
        0x800e_f4f4 => Spec {
            knobs: STORY_KNOBS,
            void: true,
            prepare: Some(prepare_slots),
            ..any
        },
        0x800f_00a8 | 0x800f_0144 | 0x800f_0174 | 0x800f_0244 => Spec {
            args: [Below(14), Any, Any, Any],
            knobs: SCENE_KNOBS,
            ..any
        },
        0x800f_1d04 => Spec {
            knobs: SCENE_KNOBS,
            prepare: Some(prepare_play),
            ..any
        },
        0x800f_1430 => Spec {
            knobs: SCENE_KNOBS,
            prepare: Some(prepare_bind),
            ..any
        },
        0x800f_00ac..0x800f_33a8 => Spec {
            knobs: SCENE_KNOBS,
            prepare: Some(prepare_scene),
            ..any
        },
        // the replay graph: synthetic blocks in every variant
        0x800f_4c04..0x800f_5834 => Spec {
            prepare: Some(prepare_graph),
            ..any
        },
        0x800e_dbcc..0x800f_00a8 => Spec {
            knobs: STORY_KNOBS,
            ..any
        },
        _ => any,
    }
}

/// A small random halfword now and then -1 (script operands).
fn operand(rng: &mut Rng) -> u32 {
    match rng.below(8) {
        0 => u32::MAX,
        1 => rng.below(4),
        _ => rng.below(0x140),
    }
}

/// Script opcodes: a random command at the script pointer (slot 0..15, the
/// operands small or -1); for 800EDAD4 also the never-stored count word of
/// its frame (at entry sp - 0x40 + 0x14).
fn prepare_command(m: &mut Machine, rng: &mut Rng, _: &mut [u32; 4]) {
    let b = m.lw(STORY_EVENTS);
    let cmd = m.lw(b.wrapping_add(0x14));
    m.sh(cmd, rng.below(16) as u16);
    for offset in (2..0x18).step_by(2) {
        let v = operand(rng);
        m.sh(cmd.wrapping_add(offset), v as u16);
    }
    m.sb(cmd.wrapping_add(0x17), rng.below(5) as u8);
    let stale = m.sp.wrapping_sub(0x40 - 0x14);
    // a count of 0 or 1 (with more, 800EC4EC copies the frame's saved ra,
    // which only the original stores)
    m.sw(stale, rng.below(2));
    let _ = m.take_fault();
}

/// 800EDC80: one of the six event ids of 0x8010B37C, or another value.
fn prepare_find(m: &mut Machine, rng: &mut Rng, args: &mut [u32; 4]) {
    args[0] = 0x8010_b37c;
    args[1] = if rng.below(4) == 0 {
        rng.below(0x40)
    } else {
        u32::from(m.lhu(0x8010_b37c + 2 * rng.below(6)))
    };
    args[2] = rng.below(8);
}

/// 800EE9E4: the hub step, the script clock and the box flag.
fn prepare_wireframe(m: &mut Machine, rng: &mut Rng, _: &mut [u32; 4]) {
    let b = m.lw(STORY_EVENTS);
    m.sw(b.wrapping_add(0x10), rng.below(40));
    m.sw(b.wrapping_add(0x28), rng.below(3));
    let hub = m.lw(HUB);
    let step = if rng.below(8) == 0 {
        rng.below(4).wrapping_neg()
    } else {
        rng.below(17)
    };
    m.sh(hub.wrapping_add(0x86), step as u16);
    let _ = m.take_fault();
}

/// 800EF4F4: every slot in a random mode with random times and frames, a
/// random clock and stream flag, an event list (the event's kind decides
/// the progress bar).
fn prepare_slots(m: &mut Machine, rng: &mut Rng, _: &mut [u32; 4]) {
    event_list(m, rng);
    let b = m.lw(STORY_EVENTS);
    m.sw(b.wrapping_add(0x10), rng.below(60));
    m.sw(b.wrapping_add(0xf38), rng.below(3));
    for i in 0..16u32 {
        let s = b.wrapping_add(0x52c + 0xa0 * i);
        m.sh(s, rng.below(7) as u16);
        let start = rng.below(20);
        let mid = start + rng.below(20);
        let end = mid + rng.below(20);
        m.sh(s.wrapping_add(2), start as u16);
        m.sh(s.wrapping_add(4), mid as u16);
        m.sh(s.wrapping_add(6), end as u16);
        for offset in (8..0x18).step_by(2) {
            let v = rng.below(0x140);
            m.sh(s.wrapping_add(offset), v as u16);
        }
        m.sh(s.wrapping_add(0x18), (1 + rng.below(4)) as u16);
        m.sh(s.wrapping_add(0x1a), rng.below(4) as u16);
        m.sh(s.wrapping_add(0x1c), rng.below(5) as u16);
        m.sh(s.wrapping_add(0x1e), rng.below(5) as u16);
    }
    let _ = m.take_fault();
}

/// The list ticks: the hub's style entries 3..5 (`[[0x80112DC0] + 0x58]`
/// halfwords), the cursor and the list, the pad bit.
fn prepare_list(m: &mut Machine, rng: &mut Rng, _: &mut [u32; 4]) {
    let styles = m.lw(0x8011_2dc0);
    let halves = m.lw(styles.wrapping_add(0x58));
    for entry in 3..6u32 {
        m.sh(halves.wrapping_add(2 * entry), rng.below(3) as u16);
    }
    let b = m.lw(STORY_EVENTS);
    m.sh(b.wrapping_add(0x16b2), rng.below(12).wrapping_sub(1) as u16);
    m.sb(b.wrapping_add(0x16b0), rng.below(6) as u8);
    let context = m.lw(CONTEXT);
    m.sw(context.wrapping_add(0x12c), rng.below(2) << 4);
    let hub = m.lw(HUB);
    m.sw(hub.wrapping_add(0x128), rng.below(2));
    let _ = m.take_fault();
}

/// 800F1D04: the stream flag +0xF38 kept off 1 and 2, so 800EF4F4 (a void
/// dispatch arm: its `v0` for other flags is the caller's `s1`) returns
/// the same through the oracle's dispatch as natively.
fn prepare_play(m: &mut Machine, rng: &mut Rng, args: &mut [u32; 4]) {
    prepare_scene(m, rng, args);
    let b = m.lw(STORY_EVENTS);
    m.sw(b.wrapping_add(0xf38), if rng.below(2) == 0 { 0 } else { 3 });
    // the stream quads blended (+0x1654 rectangles, +0x1674 textures: at
    // most four); a stale count elsewhere loops for billions of calls
    m.sw(b.wrapping_add(0x1650), rng.below(5));
    // the picture effect (hub +0x114), the 0x153 event's context flag 0x20
    let hub = m.lw(HUB);
    if in_ram(hub) {
        m.sw(hub + 0x114, rng.below(2));
    }
    let context = m.lw(CONTEXT);
    if in_ram(context) {
        let flags = m.lw(context + 0x96c) & !0x20;
        m.sw(context + 0x96c, flags | (rng.below(2) << 5));
    }
    // a quarter of the time the script done (+2 = 0) on the 0x153 event
    let event = m.lw(b.wrapping_add(0x189c));
    if in_ram(b) && in_ram(event) && rng.below(4) == 0 {
        if let Some(id) = (0..EVENT_COUNT).find(|&i| m.lhu(EVENTS + 8 * i) == 0x153) {
            m.sh(event, id as u16);
        }
        m.sh(b + 2, 0);
    }
    let _ = m.take_fault();
}

/// The text routines: a third of the time, argument `N` is a synthetic
/// string at 0x801E0000 (`0`..`z`, two-byte codes, newlines, tabs and
/// spaces, which the overlay's strings lack; no `%` for the printf forms).
fn prepare_text<const N: usize>(m: &mut Machine, rng: &mut Rng, args: &mut [u32; 4]) {
    const AT: u32 = 0x801e_0000;
    if rng.below(3) != 0 {
        return;
    }
    let mut at = AT;
    for _ in 0..rng.below(24) {
        let bytes: &[u8] = match rng.below(8) {
            0 => b"\t",
            1 => b" ",
            2 => b"\n",
            3 => &[0x82, 0xa0],
            4 => &[0x81, 0x40],
            _ => &[b'0' + rng.below(0x4b) as u8],
        };
        for &b in bytes {
            m.sb(at, b);
            at += 1;
        }
    }
    m.sb(at, 0);
    args[N] = AT;
}

/// The broadcast stream's routines: in a state without the stream (a
/// stale `[0x80112DFC] + 0x1A4`, 0 among them), a frame count or a frame's tile count
/// can be any word, and 800EC880 then links packets for minutes (in the
/// original as natively, through the oracle's native callee): such counts
/// are made small.
fn prepare_stream(m: &mut Machine, rng: &mut Rng, _: &mut [u32; 4]) {
    // RAM through any segment (a stale pointer of 0 reads the kernel area)
    let ram = |p: u32| p & 0x1fff_ffff < 0x1f_c000;
    let b = m.lw(STREAM);
    let frames = m.lw(b.wrapping_add(0x1a4));
    if !ram(b) || !ram(frames) {
        return;
    }
    if m.lw(frames) > 0x1000 {
        m.sw(frames, rng.below(8));
    }
    // a negative current frame (+0xA) reads entries before the table
    if m.lh(b + 0xa) < 0 {
        let count = m.lw(frames);
        m.sh(b + 0xa, rng.below(count + 1) as u16);
    }
    for i in 0..m.lw(frames) {
        let entry = frames.wrapping_add(4 + 12 * i);
        if !ram(entry) {
            break;
        }
        if m.lw(entry) > 0x400 {
            m.sw(entry, rng.below(16));
        }
    }
    let _ = m.take_fault();
}

/// 800EBD48: leaving the disc check (block +2 = 1) from the hub's mode 0
/// in one of the missions 0x14, 0x23, 0x28, 0x2D, which the knobs reach
/// together only rarely.
fn prepare_leave(m: &mut Machine, rng: &mut Rng, _: &mut [u32; 4]) {
    if rng.below(2) != 0 {
        return;
    }
    let (b, hub, context) = (m.lw(DISC), m.lw(HUB), m.lw(CONTEXT));
    if !(in_ram(b) && in_ram(hub) && in_ram(context)) {
        return;
    }
    m.sb(b + 2, 1);
    m.sw(hub + 0xe0, 0);
    let mission = [0x14u16, 0x23, 0x28, 0x2d, 0x15][rng.below(5) as usize];
    m.sh(context + 0x38c, mission);
    let _ = m.take_fault();
}

/// What [`prepare_raw`] passes as the first argument.
const HOLDER: u8 = 0;
const DIRECTORY: u8 = 1;
const TIM: u8 = 2;

/// The upload helpers, captured with one form of resource each: half the
/// time an uncompressed directory at 0x801E0000 instead (a count, then
/// offsets from its start: one to three 2x2 16-bit TIMs; for 800ECD4C now
/// and then negative offsets), passed as `MODE`: a word holding its
/// address (800ECD4C), the directory (800ECDA8, 800ECE38) or its first TIM
/// (800ECF88, 800ED000); for the single images a quarter of the time a
/// compressed resource found in the state.
fn prepare_raw<const MODE: u8>(m: &mut Machine, rng: &mut Rng, args: &mut [u32; 4]) {
    const AT: u32 = 0x801e_0000;
    const ULZ: u32 = 0x1a7a_6c55;
    match rng.below(4) {
        0 | 1 => return,
        // a single image: now and then a compressed resource of the state
        // (the first ULZ header from a random place on) instead
        2 if MODE == TIM => {
            let from = 0x8001_0000 + (rng.below(0x1e0) << 12);
            if let Some(at) = (from..0x801e_0000).step_by(4).find(|&a| m.lw(a) == ULZ) {
                args[0] = at;
            }
            return;
        }
        _ => {}
    }
    let count = 1 + rng.below(3);
    m.sw(AT, count);
    let first = AT + 4 + 4 * count;
    for i in 0..count {
        let tim = first + 28 * i;
        let offset = if MODE == HOLDER && rng.below(2) == 0 {
            (1 + rng.below(64)).wrapping_neg()
        } else {
            tim - AT
        };
        m.sw(AT + 4 + 4 * i, offset);
        m.sw(tim, 0x10);
        m.sw(tim + 4, 2);
        m.sw(tim + 8, 12 + 8);
        m.sh(tim + 12, rng.below(0x400) as u16);
        m.sh(tim + 14, rng.below(0x200) as u16);
        m.sh(tim + 16, 2);
        m.sh(tim + 18, 2);
        m.sw(tim + 20, rng.next());
        m.sw(tim + 24, rng.next());
    }
    args[0] = match MODE {
        HOLDER => {
            m.sw(AT + 0x800, AT);
            AT + 0x800
        }
        DIRECTORY => AT,
        _ => first,
    };
}

/// Guest RAM below the stack window.
fn in_ram(address: u32) -> bool {
    (0x8000_0000..0x801f_c000).contains(&address)
}

/// The 8-byte event records (0x8010B400..0x8010B980): +0 the resource,
/// +2 the voice, +6 the kind.
const EVENTS: u32 = 0x8010_b400;
const EVENT_COUNT: u32 = 0xb0;

/// An event id: a third of the time one whose resource a phase tests (the
/// voices of 0x10E, 0x14B, 0x15D, 0x135, 0x16A, 0x174, 0x178; 0x153), a
/// third one of the kinds 8..11 (the list, the effect, the kind-10 chain).
fn event_id(m: &mut Machine, rng: &mut Rng) -> u32 {
    const RESOURCES: [u16; 8] = [0x10e, 0x14b, 0x15d, 0x135, 0x16a, 0x174, 0x178, 0x153];
    let found = match rng.below(3) {
        0 => {
            let want = RESOURCES[rng.below(8) as usize];
            (0..EVENT_COUNT).find(|&i| m.lhu(EVENTS + 8 * i) == want)
        }
        1 => {
            let want = 8 + rng.below(4) as u8;
            let ids: Vec<u32> = (0..EVENT_COUNT)
                .filter(|&i| m.lbu(EVENTS + 8 * i + 6) == want)
                .collect();
            (!ids.is_empty()).then(|| ids[rng.below(ids.len() as u32) as usize])
        }
        _ => None,
    };
    found.unwrap_or_else(|| rng.below(EVENT_COUNT))
}

/// The story-event phases (800F00A8..800F33A7): style entries 0..7
/// (`[[0x80112DC0] + 0x58]`, mostly idle), the CD monitor (`[gp + 0x34]`:
/// state +0, +4, the stream state +0xC, status +0x28), the voice channel
/// playing, the simulator event (hub +0x124) and its voice's stream entry
/// kind (the entry's top
/// three bits), story +0x14 = 2, the context's result (+0x954), pad bits
/// 0x10 / 0x20 and the events left (+0xC), a mission with result kind 2
/// (0x8010B9B4) or 0x10 / 0x25 now and then, and an event list at
/// 0x801E0000 instead of a stale (or, a third of the time, the state's)
/// `+0x189C`.
fn prepare_scene(m: &mut Machine, rng: &mut Rng, _: &mut [u32; 4]) {
    let styles = m.lw(0x8011_2dc0);
    let halves = m.lw(styles.wrapping_add(0x58));
    if in_ram(styles) && in_ram(halves) {
        for n in 0..8u32 {
            let state = if rng.below(3) == 0 { rng.below(30) } else { 0 };
            m.sh(halves + 2 * n, state as u16);
        }
    }
    let monitor = m.lw(ac3_flight_jp::GP + 0x34);
    if in_ram(monitor) {
        m.sw(monitor, rng.below(5));
        m.sw(monitor + 4, rng.below(3));
        m.sw(monitor + 0xc, rng.below(7));
        m.sb(monitor + 0x28, rng.below(0x100) as u8);
    }
    let hub = m.lw(HUB);
    // a third of the time the voice channel (hub +0x12C) playing for
    // 8002CAE4: the sound block `[gp + 0x48]` (+0x174 the channel, +0 below
    // +4, +0x148 clear) and the CD status `[[gp + 0x38]]` bits 0x81000000
    let sound = m.lw(ac3_flight_jp::GP + 0x48);
    let cd = m.lw(ac3_flight_jp::GP + 0x38);
    if in_ram(hub) && in_ram(sound) && in_ram(cd) && rng.below(3) == 0 {
        let channel = m.lw(hub + 0x12c);
        m.sw(sound + 0x174, channel);
        m.sw(sound, 0);
        m.sw(sound + 4, 1 + rng.below(4));
        m.sw(sound + 0x148, 0);
        let status = m.lw(cd);
        m.sw(cd, status | 0x8100_0000);
    }
    if in_ram(hub) {
        if rng.below(2) == 0 {
            let id = event_id(m, rng);
            m.sh(hub + 0x124, id as u16);
        }
        let id = u32::from(m.lhu(hub + 0x124));
        if id < EVENT_COUNT && in_ram(monitor) {
            let voice = u32::from(m.lhu(EVENTS + 8 * id + 2));
            let table = m.lw(monitor + if voice & 0x800 != 0 { 0x1c } else { 0x18 });
            let entry = table.wrapping_add((voice & 0x7ff) << 3);
            if in_ram(table) && in_ram(entry) {
                let word = m.lw(entry);
                m.sw(entry, (word & 0x1fff_ffff) | (rng.below(8) << 29));
            }
        }
    }
    // after a download (story +0x14 = 2), the context's result and the
    // events left, which the download and voice phases test together
    let story = m.lw(STORY);
    if in_ram(story) && rng.below(2) == 0 {
        m.sw(story + 0x14, 2);
    }
    let context = m.lw(CONTEXT);
    if in_ram(context) && rng.below(2) == 0 {
        m.sh(context + 0x954, rng.below(4) as u16);
        let pad = m.lw(context + 0x12c) & !0x30;
        m.sw(context + 0x12c, pad | (rng.below(4) << 4));
    }
    let b = m.lw(STORY_EVENTS);
    if in_ram(b) && rng.below(2) == 0 {
        m.sw(b + 0xc, rng.below(3));
        // the previous event's kind
        m.sw(b + 0x24, rng.below(12));
    }
    if in_ram(context) && rng.below(4) == 0 {
        let second: Vec<u32> = (0..0x40).filter(|&i| m.lbu(0x8010_b9b4 + i) == 2).collect();
        let mission = match rng.below(3) {
            0 => 0x10,
            1 => 0x25,
            _ if !second.is_empty() => second[rng.below(second.len() as u32) as usize],
            _ => rng.below(0x40),
        };
        m.sh(context + 0x38c, mission as u16);
    }
    event_list(m, rng);
    let _ = m.take_fault();
}

/// An event list at 0x801E0000 (event ids at a 4-byte stride) for a stale
/// (or, a third of the time, the state's) `[0x80112DB0] + 0x189C`.
fn event_list(m: &mut Machine, rng: &mut Rng) {
    const LIST: u32 = 0x801e_0000;
    let b = m.lw(STORY_EVENTS);
    if !in_ram(b) {
        return;
    }
    let list = m.lw(b + 0x189c);
    if !in_ram(list.wrapping_sub(4)) || list & 1 != 0 || rng.below(3) == 0 {
        for i in 0..8u32 {
            let id = event_id(m, rng);
            m.sw(LIST + 4 * i, id);
        }
        m.sw(b + 0x189c, LIST + 4 * (1 + rng.below(4)));
    }
}

/// 800F1430: as the other phases, and the event resource's TIM count
/// (`[child 1]`, 0..5) and now and then the TIM directory's count
/// (`[child 2]`, 0..2) of the resource `[[gp + 0x2DC] + 0x60]` (a
/// directory: a halfword count, then offsets from its start or pointers).
fn prepare_bind(m: &mut Machine, rng: &mut Rng, args: &mut [u32; 4]) {
    prepare_scene(m, rng, args);
    let resources = m.lw(ac3_flight_jp::GP + 0x2dc);
    if !in_ram(resources) {
        return;
    }
    let resource = m.lw(resources + 0x60);
    if !in_ram(resource) || m.lhu(resource) < 3 {
        return;
    }
    let mut child = |i: u32| {
        let v = m.lw(resource + 4 + 4 * i);
        if v & 0x8000_0000 != 0 {
            v
        } else {
            resource.wrapping_add(v)
        }
    };
    let (list, directory) = (child(1), child(2));
    if in_ram(list) {
        m.sw(list, rng.below(6));
    }
    if in_ram(directory) && rng.below(2) == 0 {
        m.sw(directory, rng.below(3));
    }
    let _ = m.take_fault();
}

/// 800EDAD4 runs in the broadcast states, where the script pointer
/// (+0x14) is stale: a random command at 0x801E0000.
fn prepare_stream_command(m: &mut Machine, rng: &mut Rng, args: &mut [u32; 4]) {
    let b = m.lw(STORY_EVENTS);
    m.sw(b.wrapping_add(0x14), 0x801e_0000);
    prepare_command(m, rng, args);
}

/// The replay graph's blocks, which no state holds (the debriefing is not
/// captured): a random replay state at 0x801E0000, a random record at
/// 0x801E8000 (path, events of kill kinds, kinds 0x70..0x74 and others),
/// and a packet buffer at 0x801EC000 as the first argument.
fn prepare_graph(m: &mut Machine, rng: &mut Rng, args: &mut [u32; 4]) {
    const STATE_AT: u32 = 0x801e_0000;
    const RECORD_AT: u32 = 0x801e_8000;
    m.sw(0x8011_2e04, STATE_AT);
    m.sw(0x8011_2e08, RECORD_AT);
    for i in (0..0x400u32).step_by(4) {
        m.sw(STATE_AT + i, 0);
    }
    let events = rng.below(12);
    m.sb(STATE_AT + 1, rng.below(events + 2) as u8);
    // the replay clock, now and then negative (the compiler's rounding)
    let clock = if rng.below(8) == 0 {
        (rng.below(16) + 1).wrapping_neg()
    } else {
        rng.below(240)
    };
    m.sh(STATE_AT + 2, clock as u16);
    let shown = rng.below(66);
    for n in 0..64u32 {
        let marker = STATE_AT + 0x10 + 4 * n;
        m.sb(marker, rng.below(0x100) as u8);
        m.sb(marker + 1, rng.below(0x100) as u8);
        m.sb(marker + 2, rng.below(6) as u8);
        m.sb(marker + 3, u8::from(n < shown));
    }
    for (list, count, base) in [(0x138u32, 0x3d0u32, 0u32), (0x298, 0x3d1, 0x70)] {
        let n = rng.below(5);
        m.sb(STATE_AT + count, n as u8);
        for k in 0..n {
            m.sb(STATE_AT + list + 0x2c * k, (base + rng.below(5)) as u8);
            m.sb(STATE_AT + list + 1 + 0x2c * k, rng.below(9) as u8);
        }
    }
    m.sh(RECORD_AT + 8, rng.below(64) as u16);
    m.sb(RECORD_AT + 0xa, events as u8);
    for i in 0..128u32 {
        m.sb(RECORD_AT + 0xc + i, rng.below(0x100) as u8);
    }
    for n in 0..16u32 {
        let event = RECORD_AT + 0x390 + 8 * n;
        let kind = match rng.below(3) {
            0 => rng.below(0x30),
            1 => 0x70 + rng.below(5),
            _ => 0x30 + rng.below(0x40),
        };
        m.sb(event, kind as u8);
        m.sb(event + 1, rng.below(6) as u8);
        m.sh(event + 2, rng.below(240) as u16);
        m.sb(event + 4, rng.below(0x100) as u8);
        m.sb(event + 5, rng.below(0x100) as u8);
    }
    args[0] = 0x801e_c000;
    let _ = m.take_fault();
}

/// A starting state: name, RAM, scratchpad, GTE, `sp`, and the captured
/// arguments when it is an entry of the routine under test (raw images, so
/// worker threads build their own machines).
#[derive(Clone)]
struct State {
    name: String,
    ram: Vec<u8>,
    scratch: Vec<u8>,
    gte: Option<([u32; 32], [u32; 32])>,
    sp: u32,
    callee: Option<(u32, [u32; 4])>,
}

impl State {
    fn machine(&self) -> Machine {
        let mut m = Machine::from_images(&self.ram, &self.scratch).expect("images");
        if let Some((data, control)) = self.gte {
            m.gte.data = data;
            m.gte.control = control;
        }
        m.sp = self.sp;
        m
    }
}

fn dump(name: &str) -> Option<State> {
    let ram = std::fs::read(redux().join(format!("{name}.ram.bin"))).ok()?;
    let scratch = std::fs::read(redux().join(format!("{name}.scratch.bin")))
        .unwrap_or_else(|_| vec![0; 0x400]);
    (ram.len() == 0x20_0000 && scratch.len() == 0x400).then(|| State {
        name: name.to_string(),
        ram,
        scratch,
        gte: None,
        sp: SP,
        callee: None,
    })
}

fn entry_state(directory: &Path, entry: &SnapshotInfo) -> Option<State> {
    let snap = Snapshot::load(directory, entry).ok()?;
    let args = [snap.gpr(4), snap.gpr(5), snap.gpr(6), snap.gpr(7)];
    Some(State {
        name: format!(
            "{} #{} ({:08x})",
            directory.file_name().unwrap().to_string_lossy(),
            entry.sequence,
            entry.callee
        ),
        sp: snap.gpr(29),
        gte: Some((entry.registers.cp2d, entry.registers.cp2c)),
        ram: snap.ram,
        scratch: snap.scratch,
        callee: Some((entry.callee, args)),
    })
}

/// Word-aligned pointers held in the gp area, and NUL-terminated printable
/// strings of the overlay's data.
fn pools(ram: &[u8]) -> (Vec<u32>, Vec<u32>) {
    let word = |a: u32| {
        let at = (a & 0x1f_ffff) as usize;
        u32::from_le_bytes(ram[at..at + 4].try_into().unwrap())
    };
    let gp = ac3_flight_jp::GP;
    let mut pointers: Vec<u32> = (gp - 0x8000..gp + 0x8000)
        .step_by(4)
        .map(word)
        .filter(|&v| (0x8001_0000..0x8020_0000).contains(&v) && v % 4 == 0)
        .collect();
    pointers.sort_unstable();
    pointers.dedup();
    let mut strings = Vec::new();
    let (mut start, mut run) = (0x800c_c000usize, 0);
    for a in 0x800c_c000usize..0x8010_9000 {
        let b = ram[a & 0x1f_ffff];
        if (0x20..0x7f).contains(&b) || b == 0x0a || b == 0x09 {
            if run == 0 {
                start = a;
            }
            run += 1;
        } else {
            if b == 0 && run >= 3 && strings.len() < 4096 {
                strings.push(start as u32);
            }
            run = 0;
        }
    }
    (pointers, strings)
}

fn argument(rng: &mut Rng, kind: Arg, pointers: &[u32], strings: &[u32]) -> u32 {
    match kind {
        Arg::Below(n) => rng.below(n),
        Arg::Text if !strings.is_empty() => strings[rng.below(strings.len() as u32) as usize],
        Arg::Pointer if !pointers.is_empty() => pointers[rng.below(pointers.len() as u32) as usize],
        _ => match rng.below(12) {
            0..=4 if !pointers.is_empty() => pointers[rng.below(pointers.len() as u32) as usize],
            5 | 6 => 0,
            7 | 8 => rng.below(16),
            9 => u32::MAX,
            10 => rng.below(16).wrapping_neg(),
            _ => rng.below(0x200),
        },
    }
}

fn mutate(m: &mut Machine, rng: &mut Rng, knobs: &[Knob]) {
    if knobs.is_empty() {
        return;
    }
    for _ in 0..1 + rng.below(4) {
        let k = knobs[rng.below(knobs.len() as u32) as usize];
        let base = if k.pointer == 0 { 0 } else { m.lw(k.pointer) };
        let address = base.wrapping_add(k.offset);
        let value = match (k.limit, rng.below(4)) {
            (0, _) => rng.next() ^ (rng.next() << 16),
            // a negative halfword or word now and then
            (limit, 0) if k.width > 1 => rng.below(limit).wrapping_neg(),
            (limit, _) => rng.below(limit),
        };
        match k.width {
            1 => m.sb(address, value as u8),
            2 => m.sh(address, value as u16),
            _ => m.sw(address, value),
        }
    }
    let _ = m.take_fault();
}

/// The result of one case.
enum Outcome {
    /// Equal; the bytes the original changed.
    Equal(usize),
    /// Both stopped at the same compiler trap.
    Trap,
    /// The original runs away on this random state (step or host-call
    /// limit): not a usable case.
    Runaway,
}

/// A device that answers nothing and stops a run after `HOST_CALLS` calls.
struct Limit(usize);
impl ac3_flight_jp::Device for Limit {
    fn call(&mut self, _: &mut [u8], _: &mut [u8], _: ac3_flight_jp::HostCall) -> Option<u32> {
        self.0 += 1;
        assert!(self.0 <= HOST_CALLS, "runaway: {HOST_CALLS} host calls");
        None
    }
}
const HOST_CALLS: usize = 20_000;

/// One case: the oracle and the translation from clones of `base`.
fn compare(
    base: &Machine,
    (start, end): (u32, u32),
    args: [u32; 4],
    void: bool,
) -> Result<Outcome, String> {
    let mut original = base.clone();
    original.device = Some(Box::new(Limit(0)));
    let v0 = match panic::catch_unwind(AssertUnwindSafe(|| {
        oracle::run(&mut original, start, end, args)
    })) {
        Ok(v0) => v0,
        Err(e) if panic_text(&*e).contains("runaway") => return Ok(Outcome::Runaway),
        Err(e) => return Err(format!("oracle stopped: {}", panic_text(&*e))),
    };
    let mut native = base.clone();
    native.device = Some(Box::new(Limit(0)));
    let nv0 = panic::catch_unwind(AssertUnwindSafe(|| {
        dispatch::call(&mut native, start, args)
    }))
    .map_err(|e| format!("native panic: {}", panic_text(&*e)))?;
    // a random state can make the original write over its own code, which
    // the interpreter then runs: not a usable case
    if (start..end).any(|pc| {
        let i = (pc & 0x1f_ffff) as usize;
        original.ram()[i] != base.ram()[i]
    }) {
        return Ok(Outcome::Runaway);
    }
    let (fo, fnat) = (original.take_fault(), native.take_fault());
    if fo != fnat {
        return Err(format!("faults: original {fo:?}, native {fnat:?}"));
    }
    if let Err(Fault::Trap { .. }) = fo {
        return Ok(Outcome::Trap);
    }
    let ram: Vec<u32> = (0..0x20_0000usize)
        .filter(|i| !STACK.contains(i) && original.ram()[*i] != native.ram()[*i])
        .map(|i| 0x8000_0000 + i as u32)
        .collect();
    let scratch = (0..0x400)
        .filter(|&i| original.scratch()[i] != native.scratch()[i])
        .count();
    if !ram.is_empty() || scratch != 0 || original.gte != native.gte || original.sp != native.sp {
        let values: Vec<String> = ram
            .iter()
            .take(8)
            .map(|&a| {
                let i = (a & 0x1f_ffff) as usize;
                format!("{a:08x}: {:02x}/{:02x}", original.ram()[i], native.ram()[i])
            })
            .collect();
        return Err(format!(
            "state differs: ram {values:?} ({} bytes) scratch {scratch} gte equal {} sp {:x}/{:x}; host calls {}/{}",
            ram.len(),
            original.gte == native.gte,
            original.sp,
            native.sp,
            original.host_calls.len(),
            native.host_calls.len()
        ));
    }
    if original.host_calls != native.host_calls {
        return Err(format!(
            "host calls differ: original {:x?}, native {:x?}",
            &original.host_calls[..original.host_calls.len().min(4)],
            &native.host_calls[..native.host_calls.len().min(4)]
        ));
    }
    if v0 != nv0 && !void {
        return Err(format!("v0: original {v0:#x}, native {nv0:#x}"));
    }
    Ok(Outcome::Equal(
        (0..0x20_0000usize)
            .filter(|i| !STACK.contains(i) && base.ram()[*i] != original.ram()[*i])
            .count(),
    ))
}

/// The instructions of `start..end` the original executes on `base` (a
/// copy of the oracle's decoder, recording program counters only: calls out
/// of the range run natively, as there).
fn coverage(base: &Machine, (start, end): (u32, u32), args: [u32; 4], seen: &mut BTreeSet<u32>) {
    let mut m = base.clone();
    m.device = Some(Box::new(Limit(0)));
    let mut r = [0u32; 32];
    r[4..8].copy_from_slice(&args);
    r[28] = ac3_flight_jp::GP;
    r[29] = m.sp;
    const RETURN: u32 = 0xffff_fff0;
    r[31] = RETURN;
    let (mut hi, mut lo) = (0u32, 0u32);
    let (mut pc, mut next) = (start, start + 4);
    for _ in 0..2_000_000 {
        if pc == RETURN || m.fault().is_some() {
            return;
        }
        if pc < start || pc >= end {
            // the callee's frame and stack arguments are at the guest `sp`
            m.sp = r[29];
            r[2] = dispatch::call(&mut m, pc, [r[4], r[5], r[6], r[7]]);
            pc = r[31];
            next = pc + 4;
            continue;
        }
        seen.insert(pc);
        let w = m.lw(pc);
        let cur = pc;
        pc = next;
        next = next.wrapping_add(4);
        let op = w >> 26;
        let (rs, rt, rd) = (
            ((w >> 21) & 31) as usize,
            ((w >> 16) & 31) as usize,
            ((w >> 11) & 31) as usize,
        );
        let sa = (w >> 6) & 31;
        let simm = (w & 0xffff) as u16 as i16 as i32 as u32;
        let zimm = w & 0xffff;
        let bt = cur.wrapping_add(4).wrapping_add(simm << 2);
        let jt = (cur.wrapping_add(4) & 0xf000_0000) | ((w & 0x03ff_ffff) << 2);
        let (a, b) = (r[rs], r[rt]);
        let mut out: Option<(usize, u32)> = None;
        match op {
            0 => match w & 63 {
                0x00 => out = Some((rd, b << sa)),
                0x02 => out = Some((rd, b >> sa)),
                0x03 => out = Some((rd, ((b as i32) >> sa) as u32)),
                0x04 => out = Some((rd, b << (a & 31))),
                0x06 => out = Some((rd, b >> (a & 31))),
                0x07 => out = Some((rd, ((b as i32) >> (a & 31)) as u32)),
                0x08 => next = a,
                0x09 => {
                    out = Some((rd, cur + 8));
                    next = a;
                }
                0x0d => return,
                0x10 => out = Some((rd, hi)),
                0x11 => hi = a,
                0x12 => out = Some((rd, lo)),
                0x13 => lo = a,
                0x18 => {
                    let p = i64::from(a as i32) * i64::from(b as i32);
                    (lo, hi) = (p as u32, (p >> 32) as u32);
                }
                0x19 => {
                    let p = u64::from(a) * u64::from(b);
                    (lo, hi) = (p as u32, (p >> 32) as u32);
                }
                0x1a => {
                    let (n, d) = (a as i32, b as i32);
                    if d == 0 {
                        (lo, hi) = (if n < 0 { 1 } else { u32::MAX }, a);
                    } else {
                        (lo, hi) = (n.wrapping_div(d) as u32, n.wrapping_rem(d) as u32);
                    }
                }
                0x1b => {
                    lo = a.checked_div(b).unwrap_or(u32::MAX);
                    hi = a.checked_rem(b).unwrap_or(a);
                }
                0x20 | 0x21 => out = Some((rd, a.wrapping_add(b))),
                0x22 | 0x23 => out = Some((rd, a.wrapping_sub(b))),
                0x24 => out = Some((rd, a & b)),
                0x25 => out = Some((rd, a | b)),
                0x26 => out = Some((rd, a ^ b)),
                0x27 => out = Some((rd, !(a | b))),
                0x2a => out = Some((rd, u32::from((a as i32) < (b as i32)))),
                0x2b => out = Some((rd, u32::from(a < b))),
                _ => return,
            },
            1 => {
                let take = if rt & 1 == 0 {
                    (a as i32) < 0
                } else {
                    (a as i32) >= 0
                };
                if rt & 0x10 != 0 {
                    r[31] = cur + 8;
                }
                if take {
                    next = bt;
                }
            }
            2 => next = jt,
            3 => {
                r[31] = cur + 8;
                next = jt;
            }
            4 if a == b => next = bt,
            5 if a != b => next = bt,
            6 if (a as i32) <= 0 => next = bt,
            7 if (a as i32) > 0 => next = bt,
            4..=7 => {}
            8 | 9 => out = Some((rt, a.wrapping_add(simm))),
            0xa => out = Some((rt, u32::from((a as i32) < (simm as i32)))),
            0xb => out = Some((rt, u32::from(a < simm))),
            0xc => out = Some((rt, a & zimm)),
            0xd => out = Some((rt, a | zimm)),
            0xe => out = Some((rt, a ^ zimm)),
            0xf => out = Some((rt, zimm << 16)),
            0x12 => {
                if w & (1 << 25) != 0 {
                    let _ = m.gte.command(w & 0x1ff_ffff);
                } else {
                    match rs {
                        0 => out = Some((rt, m.gte.mfc2(rd))),
                        2 => out = Some((rt, m.gte.cfc2(rd))),
                        4 => m.gte.mtc2(rd, b),
                        6 => m.gte.ctc2(rd, b),
                        _ => return,
                    }
                }
            }
            0x20 => out = Some((rt, m.lb(a.wrapping_add(simm)) as i32 as u32)),
            0x21 => out = Some((rt, m.lh(a.wrapping_add(simm)) as i32 as u32)),
            0x23 => out = Some((rt, m.lw(a.wrapping_add(simm)))),
            0x24 => out = Some((rt, u32::from(m.lbu(a.wrapping_add(simm))))),
            0x25 => out = Some((rt, u32::from(m.lhu(a.wrapping_add(simm))))),
            0x28 => m.sb(a.wrapping_add(simm), b as u8),
            0x29 => m.sh(a.wrapping_add(simm), b as u16),
            0x2b => m.sw(a.wrapping_add(simm), b),
            0x32 => {
                let v = m.lw(a.wrapping_add(simm));
                m.gte.mtc2(rt, v);
            }
            0x3a => {
                let v = m.gte.mfc2(rt);
                m.sw(a.wrapping_add(simm), v);
            }
            0x22 | 0x26 | 0x2a | 0x2e => {
                let addr = a.wrapping_add(simm);
                let (aligned, shift) = (addr & !3, (addr & 3) as usize);
                let mem = m.lw(aligned);
                match op {
                    0x22 => {
                        const MASK: [u32; 4] = [0x00ff_ffff, 0x0000_ffff, 0x0000_00ff, 0];
                        const SH: [u32; 4] = [24, 16, 8, 0];
                        out = Some((rt, (b & MASK[shift]) | (mem << SH[shift])));
                    }
                    0x26 => {
                        const MASK: [u32; 4] = [0, 0xff00_0000, 0xffff_0000, 0xffff_ff00];
                        const SH: [u32; 4] = [0, 8, 16, 24];
                        out = Some((rt, (b & MASK[shift]) | (mem >> SH[shift])));
                    }
                    0x2a => {
                        const MASK: [u32; 4] = [0xffff_ff00, 0xffff_0000, 0xff00_0000, 0];
                        const SH: [u32; 4] = [24, 16, 8, 0];
                        m.sw(aligned, (mem & MASK[shift]) | (b >> SH[shift]));
                    }
                    _ => {
                        const MASK: [u32; 4] = [0, 0x0000_00ff, 0x0000_ffff, 0x00ff_ffff];
                        const SH: [u32; 4] = [0, 8, 16, 24];
                        m.sw(aligned, (mem & MASK[shift]) | (b << SH[shift]));
                    }
                }
            }
            _ => return,
        }
        if let Some((i, v)) = out
            && i != 0
        {
            r[i] = v;
        }
    }
}

/// Variants per state (the first runs unmutated).
const VARIANTS: u32 = 20;

/// Routines tested at once, and the time one may take.
const WORKERS: usize = 6;
const BUDGET: std::time::Duration = std::time::Duration::from_secs(240);

#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures and RAM dumps)"]
fn oracle_matches_the_jp_original() {
    let routines = routines();
    let claimed = claimed();
    // ONLY: a comma-separated list of routines to test
    let only: Option<Vec<u32>> = std::env::var("ONLY").ok().map(|s| {
        s.split(',')
            .filter_map(|a| u32::from_str_radix(a.trim().trim_start_matches("0x"), 16).ok())
            .collect()
    });
    // states: the dumps with module 2 loaded and every captured entry
    let mut dumps: Vec<State> = DUMPS.iter().filter_map(|d| dump(d)).collect();
    assert!(
        !dumps.is_empty(),
        "no RAM dumps under {}",
        redux().display()
    );
    dumps.retain(|s| {
        let mut m = s.machine();
        ac3_flight_jp::common::loaded_module(&mut m) == 2
    });
    let entries: Vec<State> = pairs()
        .iter()
        .filter_map(|(d, e, _)| entry_state(d, e))
        .collect();
    let (dumps, entries) = (std::sync::Arc::new(dumps), std::sync::Arc::new(entries));
    let hook = std::sync::Arc::new(panic::take_hook());
    {
        let hook = hook.clone();
        panic::set_hook(Box::new(move |info| {
            if std::thread::current().name() != Some("ov2-p4-worker") {
                hook(info);
            }
        }));
    }
    let list: Vec<(u32, u32)> = routines
        .iter()
        .filter(|(start, _)| {
            claimed.contains(start) && only.as_ref().is_none_or(|o| o.contains(start))
        })
        .map(|(&s, &e)| (s, e))
        .collect();
    let (tx, rx) = std::sync::mpsc::channel::<(u32, String, Option<String>, usize)>();
    let mut active: BTreeMap<u32, std::time::Instant> = BTreeMap::new();
    let mut results: BTreeMap<u32, (String, Option<String>)> = BTreeMap::new();
    let (mut next, mut total) = (0, 0);
    while results.len() < list.len() {
        while active.len() < WORKERS && next < list.len() {
            let (start, end) = list[next];
            next += 1;
            let (tx, dumps, entries) = (tx.clone(), dumps.clone(), entries.clone());
            std::thread::Builder::new()
                .name("ov2-p4-worker".into())
                .stack_size(64 << 20)
                .spawn(move || {
                    install();
                    let (line, failure, cases) = test_routine(start, end, &dumps, &entries);
                    let _ = tx.send((start, line, failure, cases));
                })
                .expect("worker");
            active.insert(start, std::time::Instant::now());
        }
        match rx.recv_timeout(std::time::Duration::from_secs(1)) {
            Ok((start, line, failure, cases)) => {
                if active.remove(&start).is_some() {
                    println!("{line}");
                    if let Some(f) = &failure {
                        println!("  FAIL {f}");
                    }
                    total += cases;
                    results.insert(start, (line, failure));
                }
            }
            Err(_) => {
                let late: Vec<u32> = active
                    .iter()
                    .filter(|(_, t)| t.elapsed() > BUDGET)
                    .map(|(s, _)| *s)
                    .collect();
                for start in late {
                    active.remove(&start);
                    let line = format!("{start:08x}: did not finish in {BUDGET:?}");
                    println!("{line}");
                    results.insert(start, (line.clone(), Some(line)));
                }
            }
        }
    }
    let _ = panic::take_hook();
    let report: String = results.values().map(|(l, _)| l.clone() + "\n").collect();
    let failures: Vec<String> = results.values().filter_map(|(_, f)| f.clone()).collect();
    let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join("jp-ov2-p4-oracle.txt");
    std::fs::write(&out, &report).expect("report");
    println!(
        "oracle: {} routines, {total} cases; {} failures; report {}",
        results.len(),
        failures.len(),
        out.display()
    );
    assert!(total > 0);
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Every case of one routine: (report line, first failure, cases run).
fn test_routine(
    start: u32,
    end: u32,
    dumps: &[State],
    entries: &[State],
) -> (String, Option<String>, usize) {
    // the routine's own entries first, then the other entries of its
    // file's range, then the dumps
    let spec = spec(start);
    let own: Vec<&State> = entries
        .iter()
        .filter(|s| s.callee.is_some_and(|(c, _)| c == start))
        .collect();
    let near: Vec<&State> = entries
        .iter()
        .filter(|s| {
            s.callee
                .is_some_and(|(c, _)| c != start && c.abs_diff(start) < 0x4000)
        })
        .take(6)
        .collect();
    let states: Vec<&State> = if spec.own_only {
        own.clone()
    } else if !spec.from.is_empty() {
        entries
            .iter()
            .filter(|s| {
                s.callee
                    .is_some_and(|(c, _)| c == start || spec.from.contains(&c))
            })
            .collect()
    } else {
        own.iter()
            .chain(&near)
            .copied()
            .chain(dumps.iter())
            .collect()
    };
    // CASE_TIMES: each case's time on stderr (finding slow cases)
    let times = std::env::var_os("CASE_TIMES").is_some();
    let mut seen = BTreeSet::new();
    let (mut equal, mut traps, mut changed, mut runaway) = (0, 0, 0, 0);
    let mut failed = None;
    'states: for state in &states {
        let (pointers, strings) = pools(&state.ram);
        let machine = state.machine();
        for variant in 0..VARIANTS {
            let mut rng = Rng(start ^ (variant << 20) ^ name_hash(&state.name));
            let mut base = machine.clone();
            let captured = state.callee.filter(|(c, _)| *c == start).map(|(_, a)| a);
            let args = match captured {
                Some(args) if variant == 0 || spec.own_only => args,
                _ => std::array::from_fn(|i| argument(&mut rng, spec.args[i], &pointers, &strings)),
            };
            if captured.is_none() || variant > 0 {
                for (i, kind) in spec.stack.iter().enumerate() {
                    if matches!(kind, Arg::Keep) {
                        continue;
                    }
                    let value = argument(&mut rng, *kind, &pointers, &strings);
                    base.sw(base.sp.wrapping_add(0x10 + 4 * i as u32), value);
                }
            }
            let mut args = args;
            if variant > 0 {
                mutate(&mut base, &mut rng, spec.knobs);
            }
            if (variant > 0 || captured.is_none())
                && let Some(prepare) = spec.prepare
            {
                prepare(&mut base, &mut rng, &mut args);
            }
            let clock = std::time::Instant::now();
            let _ = panic::catch_unwind(AssertUnwindSafe(|| {
                coverage(&base, (start, end), args, &mut seen)
            }));
            let traced = clock.elapsed();
            let result = compare(&base, (start, end), args, spec.void);
            if times {
                eprintln!(
                    "{start:08x} {} v{variant}: trace {traced:?}, compare {:?}",
                    state.name,
                    clock.elapsed() - traced
                );
            }
            match result {
                Ok(Outcome::Equal(n)) => {
                    equal += 1;
                    changed += n;
                }
                Ok(Outcome::Trap) => traps += 1,
                Ok(Outcome::Runaway) => runaway += 1,
                Err(e) => {
                    failed = Some(format!(
                        "{start:08x} {} v{variant} args {args:08x?}: {e}",
                        state.name
                    ));
                    break 'states;
                }
            }
        }
    }
    let instructions = (end - start) / 4;
    let missed: Vec<u32> = (start..end)
        .step_by(4)
        .filter(|pc| !seen.contains(pc))
        .collect();
    let line = format!(
        "{start:08x}: {equal} equal + {traps} trap cases ({changed} RAM bytes changed; {runaway} runaway skipped), {}/{instructions} instructions executed{}",
        seen.len(),
        if missed.is_empty() {
            String::new()
        } else {
            format!(", not reached: {}", ranges(&missed))
        }
    );
    (line, failed, equal + traps)
}

/// FNV-1a of a state name (the variants' seed).
fn name_hash(name: &str) -> u32 {
    name.bytes().fold(0x811c_9dc5u32, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
    })
}

/// `a..b` runs of consecutive instruction addresses.
fn ranges(pcs: &[u32]) -> String {
    let mut out = Vec::new();
    let mut i = 0;
    while i < pcs.len() {
        let mut j = i;
        while j + 1 < pcs.len() && pcs[j + 1] == pcs[j] + 4 {
            j += 1;
        }
        out.push(if i == j {
            format!("{:08x}", pcs[i])
        } else {
            format!("{:08x}-{:08x}", pcs[i], pcs[j])
        });
        i = j + 1;
    }
    out.join(" ")
}
