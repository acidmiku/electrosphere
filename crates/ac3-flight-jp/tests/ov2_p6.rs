//! JP overlay 2, part 6 (800FF95C..8010980C; `src/jp/ov2_p6*.rs`):
//! validation against the JP originals.
//!
//! * [`captured_calls_reproduce`]: every captured call of a part-6 routine
//!   (`~/ac3-jp/analysis/redux/*/snapshots-module2.jsonl`) runs from its
//!   entry snapshot and must reproduce the return snapshot (RAM outside the
//!   stack, scratchpad, GTE, `v0`), as `tests/captures.rs` does. With the
//!   feature `dev-fallback` the routines of the other overlay-2 parts that
//!   are not translated yet run interpreted, so the routines that call them
//!   are validated too (`tests/captures.rs` installs no fallback); without
//!   it, the replays that stop at such a routine are listed, not failed.
//! * [`oracle_cases_match`]: the routines no capture reaches, and the
//!   branches the captures miss, run against the JP original through the
//!   differential oracle (`support::oracle`) from the captured entry states
//!   with varied fields and arguments.
//!
//! Both need the JP captures (`AC3_JP_DIR`, default `~/ac3-jp`, or
//! `AC3_JP_CAPTURES`).
mod support;

use ac3_flight_jp::Machine;
use ac3_flight_jp::snapshot::{self, Snapshot, SnapshotInfo};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use support::{oracle, scan};

/// The part's address range (its last routine ends at 0x80109840).
const PART: (u32, u32) = (0x800f_f95c, 0x8010_9840);
/// Part routines whose translation models no `v0`.
const VOID: [u32; 2] = [0x8010_09ac, 0x8010_0fdc];

/// The capture root (`AC3_JP_CAPTURES`, else `AC3_JP_DIR/analysis/redux`).
fn root() -> PathBuf {
    std::env::var_os("AC3_JP_CAPTURES")
        .map(PathBuf::from)
        .unwrap_or_else(|| scan::jp_dir().join("analysis/redux"))
}

/// Every overlay-2 snapshot index under the root.
fn module2_indexes() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(runs) = std::fs::read_dir(root()) else {
        return out;
    };
    for run in runs.flatten() {
        let path = run.path().join("snapshots-module2.jsonl");
        if path.is_file() {
            out.push(path);
        }
    }
    out.sort();
    out
}

/// The part's routines (start, end) from the JP function map.
fn routines() -> BTreeMap<u32, u32> {
    scan::jp_map(&scan::jp_dir())
        .into_iter()
        .filter(|r| r.region == "ov2" && (PART.0..PART.1).contains(&r.jp))
        .map(|r| (r.jp, r.jp + r.size))
        .collect()
}

/// The part's routines the patch layer claims (overlay 2).
fn claimed() -> BTreeMap<u32, u32> {
    let claims = scan::jp_overlay_claims();
    routines()
        .into_iter()
        .filter(|(a, _)| claims.contains_key(&(2, *a)))
        .collect()
}

/// The claimed routines' true extents: 80106A34 runs on through the map's
/// 80107154 (its tail, not a routine) to 801071B8.
fn extents() -> BTreeMap<u32, u32> {
    let mut out = claimed();
    out.insert(0x8010_6a34, 0x8010_71b8);
    out
}

/// The return snapshot matching an entry.
fn exit_of<'a>(index: &'a [SnapshotInfo], entry: &SnapshotInfo) -> Option<&'a SnapshotInfo> {
    index.iter().find(|s| {
        s.callee == entry.callee
            && s.stage == "return"
            && s.sequence > entry.sequence
            && s.ra == entry.ra
            && s.registers.gpr[29] == entry.registers.gpr[29]
    })
}

/// Install the development fallback (the other parts' untranslated
/// routines run interpreted) for this thread, when the feature is on.
fn with_fallback<R>(body: impl FnOnce() -> R) -> R {
    #[cfg(feature = "dev-fallback")]
    {
        let _ = ac3_fallback::install(ac3_fallback::Fallback::new(ac3_fallback::Version::Jp));
        let result = body();
        let _ = ac3_fallback::uninstall();
        result
    }
    #[cfg(not(feature = "dev-fallback"))]
    body()
}

/// RAM a device or an interrupt writes during a call, which the test
/// machine (no VRAM, no GPU queue, no BIOS) cannot reproduce:
/// * 80101EE4 reaches 800FF390, whose StoreImage (800A84A0) reads the map
///   palette from VRAM into the map block +0xBC..+0xDB, leaving libgpu's
///   queue byte 0x800BBA94 to the GPU's completion;
/// * 801067B0 runs long enough (it loads the scene's resources) for the
///   interrupt handlers to run: the BIOS saves their context in the
///   kernel's RAM below 0x80010000 and libetc's handlers use their own
///   stack below 0x800B60C4 (the manager 0x800B50AC's area after its setjmp
///   buffer); the GPU completes a queued transfer (libgpu's queue indices
///   0x800BBA8C/0x800BBA90) where the host GPU is idle at once (so the
///   native never counts a timeout poll, 0x800BBAA4), and the CD interrupt
///   advances the resource loader's transfer state 0x800CAAC0..0x800CAACB.
///   The oracle case of 801067B0, with the same callees on both sides, is
///   exact (`oracle_cases_match`).
fn device_bytes(m: &mut Machine, callee: u32) -> Vec<std::ops::Range<u32>> {
    match callee {
        0x8010_1ee4 => {
            let map = m.lw(0x8011_2da8);
            vec![map + 0xbc..map + 0xdc, 0x800b_ba94..0x800b_ba95]
        }
        0x8010_67b0 => vec![
            0x8000_0000..0x8001_0000,
            0x800b_5114..0x800b_60c4,
            0x800b_ba8c..0x800b_ba94,
            0x800b_baa4..0x800b_baa5,
            0x800c_aac0..0x800c_aacc,
        ],
        _ => Vec::new(),
    }
}

/// Replay one captured call: Ok(bytes the original changed) or Err(cause).
fn replay(directory: &Path, entry: &SnapshotInfo, exit: &SnapshotInfo) -> Result<usize, String> {
    let entry = Snapshot::load(directory, entry).map_err(|e| format!("load: {e}"))?;
    let exit = Snapshot::load(directory, exit).map_err(|e| format!("load: {e}"))?;
    let mut m = entry.machine();
    let args = [entry.gpr(4), entry.gpr(5), entry.gpr(6), entry.gpr(7)];
    let callee = entry.info.callee;
    let v0 = ac3_flight_jp::dispatch::call(&mut m, callee, args);
    if let Err(fault) = m.take_fault() {
        return Err(format!("fault: {fault}"));
    }
    let mut d = snapshot::compare(&m, &exit);
    let allowed = device_bytes(&mut m, callee);
    d.ram.retain(|a| !allowed.iter().any(|r| r.contains(a)));
    if !d.is_empty() {
        if std::env::var_os("OV2_P6_DEBUG").is_some() {
            for &a in d.ram.iter().take(40) {
                let i = (a & 0x1f_ffff) as usize;
                println!(
                    "  {a:08x}: entry {:02x} native {:02x} original {:02x}",
                    entry.ram[i],
                    m.ram()[i],
                    exit.ram[i]
                );
            }
        }
        return Err(format!("state differs: {}", d.summary()));
    }
    if !VOID.contains(&callee) && v0 != exit.gpr(2) {
        return Err(format!("v0 {v0:#010x}, original {:#010x}", exit.gpr(2)));
    }
    Ok(snapshot::changed_bytes(&entry, &exit))
}

#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn captured_calls_reproduce() {
    // 82 matcher functions and the two script opcode stubs that
    // tools/jp/fix_map.py adds (`jr ra; nop`, outside the matcher's map):
    // 80102650 (opcode table entry 0x80112B9C) and 80105E18 (0x80112BBC),
    // which ov2_p6_story handles natively (NO_OP_STUBS)
    assert_eq!(routines().len(), 84, "the part's routines");
    let routines = claimed();
    let indexes = module2_indexes();
    assert!(
        !indexes.is_empty(),
        "no captures under {}",
        root().display()
    );
    let mut passed: BTreeMap<u32, (usize, usize)> = BTreeMap::new();
    let mut failures = Vec::new();
    with_fallback(|| {
        for path in &indexes {
            let directory = path.parent().unwrap();
            let run = directory
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let index = snapshot::read_index(path).unwrap();
            for entry in index.iter().filter(|s| s.stage == "entry") {
                if !routines.contains_key(&entry.callee) {
                    continue;
                }
                let Some(exit) = exit_of(&index, entry) else {
                    continue;
                };
                match replay(directory, entry, exit) {
                    Ok(bytes) => {
                        let slot = passed.entry(entry.callee).or_default();
                        slot.0 += 1;
                        slot.1 += bytes;
                    }
                    Err(e) => failures.push(format!(
                        "{run} #{} {:08x}: {e}",
                        entry.sequence, entry.callee
                    )),
                }
            }
        }
    });
    for (address, (calls, bytes)) in &passed {
        println!("pass {address:08x}: {calls} call(s), {bytes} bytes changed");
    }
    // Without `dev-fallback` a call into another part's untranslated routine
    // stops the replay: such calls are listed, not failed (they pass with the
    // feature, and without it once the neighbour lands).
    #[cfg(not(feature = "dev-fallback"))]
    let failures: Vec<String> = {
        let (waiting, failures): (Vec<String>, Vec<String>) =
            failures.into_iter().partition(|e| neighbour_missing(e));
        for e in &waiting {
            println!("waiting for another part: {e}");
        }
        failures
    };
    println!(
        "captured part-6 routines: {} pass, {} failures",
        passed.len(),
        failures.len()
    );
    assert!(failures.is_empty(), "{failures:#?}");
    #[cfg(feature = "dev-fallback")]
    assert!(
        passed.len() >= 56,
        "too few captured routines: {}",
        passed.len()
    );
}

/// Whether a replay stopped at an untranslated routine outside this part.
#[cfg(not(feature = "dev-fallback"))]
fn neighbour_missing(error: &str) -> bool {
    error
        .split("untranslated original routine 0x")
        .nth(1)
        .and_then(|rest| u32::from_str_radix(rest.get(..8)?, 16).ok())
        .is_some_and(|address| !(PART.0..PART.1).contains(&address))
}

// ---- the oracle cases ----------------------------------------------------------------------

/// The entry snapshots of `callee` in the overlay-2 captures (machine,
/// arguments).
fn entries(callee: u32) -> Vec<(Machine, [u32; 4])> {
    let mut out = Vec::new();
    for path in module2_indexes() {
        let directory = path.parent().unwrap().to_path_buf();
        let index = snapshot::read_index(&path).unwrap();
        for entry in index
            .iter()
            .filter(|s| s.stage == "entry" && s.callee == callee)
        {
            let snapshot = Snapshot::load(&directory, entry).unwrap();
            let args = [
                snapshot.gpr(4),
                snapshot.gpr(5),
                snapshot.gpr(6),
                snapshot.gpr(7),
            ];
            out.push((snapshot.machine(), args));
        }
    }
    out
}

/// One differential case: the original (oracle) and the translation
/// (dispatch) from the same state. Returns the RAM bytes the original
/// changed.
fn case(label: &str, base: &Machine, start: u32, end: u32, args: [u32; 4], void: bool) -> usize {
    oracle::diff(label, base, (start, end), args, |m| {
        let v0 = ac3_flight_jp::dispatch::call(m, start, args);
        (!void).then_some(v0)
    })
}

// ---- the varied oracle cases ---------------------------------------------------------------

/// The part's globals the cases vary.
const MAP: u32 = 0x8011_2da8;
const SCENE: u32 = 0x8011_2dac;
const HUB: u32 = 0x8011_2dc4;
const MODE: u32 = 0x8011_2ddc;
const VIEWER: u32 = 0x8011_2e30;
const FADE: u32 = 0x8011_2bf4;
const RAND_STATE: u32 = 0x800c_bf58;
/// The context (`[gp+0x2EC]`), the display request block (`[gp+0x2D4]`).
const CONTEXT: u32 = 0x800b_ce60;
const DISPLAY: u32 = 0x800b_ce48;
/// Free RAM the cases use for made-up records (above the overlay arena,
/// below the stack).
const SPARE: u32 = 0x801e_0000;

/// A variant: changes `m` (and the arguments) for case `i`.
type Vary = fn(&mut Machine, &mut oracle::Rng, usize, &mut [u32; 4]);

/// A group of cases: (routine, the routine whose first captured entry is the
/// base state, cases, variant).
type Group = (u32, u32, usize, Vary);

fn pick(rng: &mut oracle::Rng, values: &[u32]) -> u32 {
    values[rng.next() as usize % values.len()]
}
fn small(rng: &mut oracle::Rng, range: u32) -> u32 {
    (rng.next() % (2 * range)).wrapping_sub(range)
}
fn map(m: &mut Machine) -> u32 {
    m.lw(MAP)
}
fn scene(m: &mut Machine) -> u32 {
    m.lw(SCENE)
}
fn hub(m: &mut Machine) -> u32 {
    m.lw(HUB)
}
fn context(m: &mut Machine) -> u32 {
    m.lw(CONTEXT)
}
/// The current script command of the scene block.
fn command(m: &mut Machine) -> u32 {
    let scene = scene(m);
    m.lw(scene + 0x9c8)
}
/// Random command arguments: frames arg0 nonzero, positions around 0x80.
fn command_args(m: &mut Machine, rng: &mut oracle::Rng, slot: bool) {
    let cmd = command(m);
    m.sh(cmd + 4, (1 + rng.next() % 90) as u16);
    for offset in [6u32, 8, 0xa, 0xc] {
        m.sh(cmd + offset, (0x40 + rng.next() % 0x80) as u16);
    }
    if slot {
        m.sb(cmd + 3, (rng.next() % 0x20) as u8);
    }
}
/// A made-up sub-mode state and viewer block in spare RAM.
fn viewer_block(m: &mut Machine, rng: &mut oracle::Rng, i: usize) {
    m.sw(MODE, SPARE);
    m.sw(SPARE, 0);
    let block = SPARE + 0x100;
    m.sw(VIEWER, block);
    m.sb(block + 0x4000, (i % 4) as u8);
    m.sb(block + 0x4001, (i & 1) as u8);
    m.sb(block + 0x4003, (rng.next() % 3) as u8);
    m.sw(block + 0x4004, RESOURCE);
}
/// Label records for counts up to 4 (`+0xA5C`; each a style byte naming one
/// of the 32 object slots at `+0x9DC`).
fn labels(m: &mut Machine) {
    let scene = scene(m);
    for k in 0..4 {
        let label = SPARE + 0x7800 + k;
        m.sb(label, (k * 7 % 32) as u8);
        m.sw(scene + 0xa5c + 4 * k, label);
    }
}

/// `[0x80112DC0]`: the visual slots; 800DEE84 reads their modes
/// (halfwords at `+0x58`), 800DEEA8 their values (`+0x50`).
const VISUALS: u32 = 0x8011_2dc0;
/// `[gp+0x2DC]` (resources), `[gp+0x2E0]` (gameplay), `[gp+0x2E8]`
/// (entity table).
const RESOURCES: u32 = 0x800b_ce50;
const GAMEPLAY: u32 = 0x800b_ce54;
const ENTITIES: u32 = 0x800b_ce5c;
/// The scene table (12-byte records, +0 the resource) and the scene
/// stream table (4-byte records, +2 the stream kind).
const SCENES: u32 = 0x8011_27ac;
const SCENE_STREAMS: u32 = 0x8011_2aac;

/// Visual slot `slot`'s mode and value (800DEE84 / 800DEEA8).
fn set_visual(m: &mut Machine, slot: u32, mode: u16, value: u16) {
    let visuals = m.lw(VISUALS);
    let modes = m.lw(visuals + 0x58);
    m.sh(modes + 2 * slot, mode);
    let values = m.lw(visuals + 0x50);
    m.sh(values + 2 * slot, value);
}
/// A literal-only byte stream of `data` at `at` for 800AFD08 (kind 0,
/// 800AFD40: flag words of up to 31 literal bits and the end bit, a zero
/// word ending the stream).
fn byte_stream(m: &mut Machine, at: u32, data: &[u8]) {
    let words = data.len().div_ceil(31) as u32;
    let flags = at + 0x10;
    let literals = flags + 4 * (words + 1);
    m.sw(at, 0x1a7a_6c55);
    m.sw(at + 4, data.len() as u32);
    m.sw(at + 8, (8 << 24) | (literals - at));
    m.sw(at + 0xc, literals - at + data.len() as u32);
    for (w, chunk) in data.chunks(31).enumerate() {
        let n = chunk.len() as u32;
        m.sw(
            flags + 4 * w as u32,
            (((1u64 << (n + 1)) - 1) << (31 - n)) as u32,
        );
    }
    m.sw(flags + 4 * words, 0);
    for (i, &byte) in data.iter().enumerate() {
        m.sb(literals + i as u32, byte);
    }
}
/// Random relocatable words (bit 31, the "absolute" mark, on one in four).
fn words(rng: &mut oracle::Rng, count: usize) -> Vec<u8> {
    (0..count)
        .flat_map(|_| {
            let word = rng.next();
            let word = if word.is_multiple_of(4) {
                word | 0x8000_0000
            } else {
                word & 0x3fff
            };
            word.to_le_bytes()
        })
        .collect()
}
/// Where the made-up resources go (the loader's buffer, `[resources+0x60]`).
const RESOURCE: u32 = SPARE + 0x8000;
/// A made-up model resource (80108334): a directory whose entry is a
/// two-file directory, +0 an empty texture group (800ED0D4), +4 the
/// packed model (seven words, six of them relocated).
fn model_resource(m: &mut Machine, rng: &mut oracle::Rng) {
    let at = RESOURCE;
    m.sw(at, 1);
    m.sw(at + 4, 0x10);
    let first = at + 0x10;
    m.sw(first, 2);
    m.sw(first + 4, 0x10);
    m.sw(first + 8, 0x20);
    m.sw(first + 0x10, 0);
    let model = words(rng, 7);
    byte_stream(m, first + 0x20, &model);
    let resources = m.lw(RESOURCES);
    m.sw(resources + 0x60, at);
}
/// A made-up scene resource (801093E8): a four-entry directory, +0 the
/// packed model table (two seven-word models), +4 the header, +8 an empty
/// texture group (the backdrop), +0xC the arena mark.
fn scene_resource(m: &mut Machine, rng: &mut oracle::Rng) {
    let at = RESOURCE;
    for (i, value) in [4u32, 0x20, 0x18, 0x400, 0x400].into_iter().enumerate() {
        m.sw(at + 4 * i as u32, value);
    }
    m.sw(at + 0x18, rng.next());
    let mut table = Vec::new();
    for word in [2u32, 0xc, 0x28] {
        table.extend_from_slice(&word.to_le_bytes());
    }
    table.extend(words(rng, 14));
    byte_stream(m, at + 0x20, &table);
    m.sw(at + 0x400, 0);
    let resources = m.lw(RESOURCES);
    m.sw(resources + 0x60, at);
}

const GROUPS: &[Group] = &[
    // the map layer: reveal and hide around the 96-step end
    (0x8010_084c, 0x8010_084c, 12, |m, rng, i, _| {
        let map = map(m);
        m.sw(map + 0x14, [1, 3, 2, 0][i % 4]);
        m.sw(
            map + 0x20,
            pick(rng, &[0, 0x5f, 0x60, 0x61, 0x7f, (-8i32) as u32]),
        );
    }),
    // rings and tunnel: every reveal row, both modes
    (0x800f_f95c, 0x800f_f95c, 16, |m, rng, i, _| {
        let map = map(m);
        m.sw(map + 0x14, [1, 3, 2, 0][i % 4]);
        m.sw(
            map + 0x20,
            pick(
                rng,
                &[0, 7, 8, 9, 15, 16, 24, 31, 40, 47, 55, (-1i32) as u32],
            ),
        );
        m.sw(map + 0x7c, rng.next() % 8);
    }),
    (0x8010_0304, 0x8010_0304, 12, |m, rng, i, _| {
        let map = map(m);
        m.sw(map + 0x14, [1, 3, 2, 0][i % 4]);
        m.sw(
            map + 0x20,
            pick(rng, &[0x30, 0x31, 0x38, 0x3f, 0x40, 0x2f, 0]),
        );
        m.sw(map + 0x7c, rng.next() % 8);
    }),
    // the stars: modes, counters around 64, steps around the 62 wrap
    (0x8010_0b94, 0x8010_0b94, 16, |m, rng, i, _| {
        let map = map(m);
        m.sw(map + 0x18, [1, 3, 2, 0][i % 4]);
        m.sw(map + 0x24, pick(rng, &[0, 10, 61, 62, 63, 64, 70]));
        m.sh(map + 0x74, pick(rng, &[0, 1, 3, 70, 0xfffe]) as u16);
    }),
    // the map's layers
    (0x8010_1fec, 0x8010_1fec, 8, |m, _, i, _| {
        let map = map(m);
        m.sw(map + 0x14, (i & 1) as u32);
        m.sw(map + 0x18, ((i >> 1) & 1) as u32);
        m.sw(map + 0x1c, ((i >> 2) & 1) as u32);
    }),
    // a trail restart: every start kind
    (0x8010_0fdc, 0x8010_0fdc, 12, |m, rng, i, args| {
        m.sw(RAND_STATE, rng.next().wrapping_mul(0x9e37_79b9));
        args[0] = (i % 3) as u32;
        let map = args[1];
        for trail in 1..3 {
            let positions = m.lw(map + 0xf4);
            m.sw(positions + 4 * trail, SPARE + 0x6000 + 0x100 * trail);
            let angles = m.lw(map + 0xf0);
            m.sw(angles + 4 * trail, SPARE + 0x6800 + 0x80 * trail);
        }
    }),
    // the ribbon layer's modes and its 60-step count
    (0x8010_1e4c, 0x8010_1e4c, 8, |m, rng, i, _| {
        let map = map(m);
        m.sw(map + 0x1c, [1, 2, 3, 3][i % 4]);
        m.sw(map + 0x28, pick(rng, &[0, 59, 60, 61]));
    }),
    // the ribbons: random turns, a trail that left the view restarts
    (0x8010_1ad4, 0x8010_1ad4, 6, |m, rng, i, _| {
        m.sw(RAND_STATE, rng.next());
        if i % 2 == 1 {
            let map = map(m);
            let table = m.lw(map + 0xf4);
            let positions = m.lw(table);
            for entry in 0..15 {
                m.sw(positions + entry * 0x10 + 8, 0);
            }
        }
    }),
    (0x8010_1a0c, 0x8010_1a0c, 10, |m, rng, _, args| {
        for entry in 0..15 {
            let at = args[0] + entry * 0x10;
            m.sw(at, small(rng, 0x3000));
            m.sw(at + 4, small(rng, 0x3000));
            m.sw(at + 8, pick(rng, &[0x800, 0x1000, 0x4000, 0x8000, 0x9000]));
        }
    }),
    (0x8010_0fc4, 0x8010_0fac, 2, |_, _, _, _| {}),
    (0x8010_17a8, 0x8010_17a8, 6, |_, _, i, args| {
        args[2] = 1 + i as u32
    }),
    (0x8010_18d8, 0x8010_18d8, 5, |_, _, i, args| {
        args[2] = (i * 3) as u32
    }),
    (0x8010_1514, 0x8010_1514, 4, |_, _, i, args| {
        args[0] = i as u32
    }),
    // the scene camera: before, during and after the swing, overridden
    (0x8010_2184, 0x8010_2184, 14, |m, rng, i, _| {
        let hub = hub(m);
        m.sw(hub + 0x50, [0, 0x51, 0x52, 0x60, 0x71, 0x72, 0x100][i % 7]);
        let scene = scene(m);
        m.sb(scene + 0x9d4, (i / 7) as u8);
        m.sw(scene + 0x20, small(rng, 0x4000));
        m.sw(scene + 0x28, small(rng, 0x4000));
    }),
    // the script commands with random arguments
    (0x8010_2658, 0x8010_2658, 10, |m, rng, _, args| {
        command_args(m, rng, true);
        args[0] = rng.next() % 0x100;
    }),
    (0x8010_2aa8, 0x8010_2aa8, 10, |m, rng, _, _| {
        command_args(m, rng, false)
    }),
    (0x8010_2bfc, 0x8010_2bfc, 8, |m, rng, i, _| {
        command_args(m, rng, true);
        let scene = scene(m);
        m.sb(scene + 0x9ce, (i % 4) as u8);
    }),
    (0x8010_2d1c, 0x8010_2f9c, 8, |m, rng, _, _| {
        command_args(m, rng, false);
        let scene = scene(m);
        m.sh(scene + 0x84, small(rng, 8) as u16);
        m.sh(scene + 0x86, small(rng, 8) as u16);
    }),
    (0x8010_2e78, 0x8010_2e78, 10, |m, rng, _, _| {
        command_args(m, rng, false)
    }),
    (0x8010_2f5c, 0x8010_2f5c, 4, |m, rng, _, _| {
        command_args(m, rng, false)
    }),
    (0x8010_2f9c, 0x8010_2f9c, 4, |m, rng, _, _| {
        command_args(m, rng, false)
    }),
    // moving and appearing objects
    (0x8010_2ff0, 0x8010_2ff0, 6, |m, rng, _, _| {
        let scene = scene(m);
        for slot in 0..32 {
            let object = m.lw(scene + 0x9dc + slot * 4);
            m.sh(object + 2, pick(rng, &[0, 1, 2, 2]) as u16);
            m.sh(object + 4, (rng.next() % 4) as u16);
            m.sh(object + 8, (rng.next() % 0x200) as u16);
            m.sh(object + 0x16, rng.next() as u16);
        }
    }),
    (0x8010_3100, 0x8010_3100, 8, |m, rng, _, _| {
        let scene = scene(m);
        m.sw(scene + 0x9d0, rng.next() | (rng.next() << 24));
        let (lx, lz) = (m.lw(scene + 0x20), m.lw(scene + 0x28));
        for slot in 0..32 {
            let object = m.lw(scene + 0x9dc + slot * 4);
            m.sh(object + 2, pick(rng, &[0, 1, 2]) as u16);
            m.sh(object + 4, (rng.next() % 30) as u16);
            m.sh(object + 6, (1 + rng.next() % 30) as u16);
            m.sh(object + 0xc, lx.wrapping_add(small(rng, 0x500)) as u16);
            m.sh(object + 0x10, lz.wrapping_add(small(rng, 0x500)) as u16);
        }
    }),
    (0x8010_3854, 0x8010_3854, 10, |m, _, i, _| {
        labels(m);
        let scene = scene(m);
        m.sb(scene + 0x9ce, (i % 5) as u8);
        m.sb(scene + 0x9d6, (i / 5) as u8);
    }),
    // the terrain: heights, quads and the grid around the looked-at point
    (0x8010_417c, 0x8010_48f0, 16, |_, rng, i, args| {
        *args = [small(rng, 0x40), small(rng, 0x40), (i % 5) as u32, 0];
    }),
    (0x8010_4218, 0x8010_48f0, 16, |m, rng, i, args| {
        let record = 0x1f80_0000;
        for offset in (0..0x40).step_by(2) {
            m.sh(record + offset, rng.next() as u16);
        }
        m.sh(record + 4, (rng.next() % 0x80) as u16);
        m.sh(record + 6, (rng.next() % 0x80) as u16);
        for corner in 0..4 {
            m.sh(record + 8 + corner * 2, (rng.next() % 0x800) as u16);
        }
        let edge = [-8i32, 0, 8, 3];
        *args = [record, SPARE, edge[i % 4] as u32, edge[(i / 4) % 4] as u32];
    }),
    (0x8010_48f0, 0x8010_48f0, 12, |m, rng, i, _| {
        let hub = hub(m);
        m.sw(
            hub + 0x50,
            [
                0x3b, 0x3c, 0x41, 0x42, 0x51, 0x52, 0x53, 0x60, 0x72, 0x80, 0x53, 0x10,
            ][i],
        );
        let scene = scene(m);
        m.sb(scene + 0x9d4, u8::from(i >= 10));
        m.sw(scene + 0x20, small(rng, 0x3000));
        m.sw(scene + 0x28, small(rng, 0x3000));
    }),
    // the story: the marker, the pan, the script, the phases
    (0x8010_5c20, 0x8010_5c20, 6, |m, _, i, _| {
        let scene = scene(m);
        m.sw(scene + 0x9c4, [u32::MAX, 0, 59, 60, 3600, 12345][i]);
    }),
    (0x8010_5d54, 0x8010_5d54, 6, |m, rng, i, _| {
        let scene = scene(m);
        m.sw(scene + 0x78, [0, 1, 5][i % 3]);
        m.sw(scene + 0x74, rng.next() % 0x400);
        m.sh(scene + 0x7e, rng.next() as u16);
    }),
    (0x8010_5e20, 0x8010_5e20, 4, |m, _, i, _| {
        if i % 2 == 1 {
            let cmd = command(m);
            let time = m.lh(cmd) as i32 as u32;
            let hub = hub(m);
            m.sw(hub + 0x50, time.wrapping_add(1));
        }
    }),
    (0x8010_606c, 0x8010_606c, 12, |m, rng, i, _| {
        let hub = hub(m);
        m.sw(
            hub + 0x50,
            [
                0x10, 0x1e, 0x23, 0x2d, 0x32, 0x37, 0x40, 0x78, 0x79, 0x10e, 0x28, 0x5,
            ][i],
        );
        let context = context(m);
        m.sw(context + 0x12c, pick(rng, &[0, 0x10]));
        m.sw(context + 0x128, pick(rng, &[0, 0, 0x20]));
    }),
    (0x8010_65c8, 0x8010_65c8, 6, |m, rng, i, _| {
        let hub = hub(m);
        m.sw(hub + 0x14c, [u32::MAX, 0, 5][i % 3]);
        let context = context(m);
        m.sw(context + 0x12c, pick(rng, &[0, 0x10]));
        m.sw(context + 0x128, pick(rng, &[0, 0x20]));
    }),
    (0x8010_6a34, 0x8010_6a34, 10, |m, rng, i, _| {
        let scene = scene(m);
        let (phase, step) = [(0, 1), (1, 1), (1, 2), (2, 0), (2, 1)][i % 5];
        m.sb(scene + 0x9d4, phase);
        m.sb(scene + 0x9d5, step);
        m.sb(scene + 0x9d6, (rng.next() % 3) as u8);
        let hub = hub(m);
        let current = m.lw(hub + 0x16c);
        m.sh(current + 0x46, (rng.next() % 4) as u16);
        let context = context(m);
        m.sw(context + 0x12c, pick(rng, &[0, 0x20]));
        m.sw(context + 0x128, 0);
    }),
    // the fade steps (not reached)
    (0x8010_71b8, 0x8010_606c, 2, |m, rng, i, _| {
        viewer_block(m, rng, i)
    }),
    (0x8010_7200, 0x8010_606c, 8, |m, rng, i, _| {
        viewer_block(m, rng, i);
        let map = map(m);
        m.sw(map + 0x14, (i & 1) as u32);
        let display = m.lw(DISPLAY);
        m.sb(display + 0x10, [0x10, 0x8e, 0x8f, 0xff][i / 2]);
    }),
    (0x8010_7270, 0x8010_606c, 6, |m, rng, i, _| {
        viewer_block(m, rng, i);
        m.sw(FADE, [1, 2, 0x20, 0, 7, 0x1f][i]);
    }),
    (0x8010_7358, 0x8010_606c, 2, |m, rng, i, _| {
        viewer_block(m, rng, i)
    }),
    // the model viewer (not reached) on the aircraft-select state
    (0x8010_8478, 0x8010_8874, 3, |m, rng, i, args| {
        viewer_block(m, rng, i);
        args[0] = i as u32;
    }),
    (0x8010_84d0, 0x8010_8874, 8, |m, rng, i, _| {
        viewer_block(m, rng, i);
        let context = context(m);
        m.sw(context + 0x128, rng.next() & 0x30f);
        let hub = hub(m);
        m.sw(
            hub + 0xfc,
            pick(
                rng,
                &[
                    0xffe0_0000,
                    0xffe0_1000,
                    0xfff8_0000,
                    0xfff7_f000,
                    0xfff0_0000,
                ],
            ),
        );
    }),
    (0x8010_85f8, 0x8010_85f8, 8, |m, rng, i, args| {
        viewer_block(m, rng, i);
        args[0] = (i % 4) as u32;
    }),
    (0x8010_8334, 0x8010_8874, 4, |m, rng, i, _| {
        viewer_block(m, rng, i);
        let context = context(m);
        m.sw(context + 0x44, 1);
    }),
    (0x8010_92bc, 0x8010_8874, 3, |m, rng, i, _| {
        viewer_block(m, rng, i);
        m.sb(SPARE + 0x100 + 0x4003, i as u8);
    }),
    (0x8010_93e8, 0x8010_8874, 4, |m, rng, i, _| {
        viewer_block(m, rng, i);
        let context = context(m);
        m.sw(context + 0x44, 1);
    }),
    (0x8010_9588, 0x8010_8874, 4, |m, rng, i, _| {
        viewer_block(m, rng, i);
        let context = context(m);
        m.sw(context + 0x12c, [0, 0x20][i % 2]);
        m.sw(context + 0x128, [0, 0x40][i / 2]);
    }),
    (0x8010_9660, 0x8010_8874, 4, |m, rng, i, _| {
        viewer_block(m, rng, i)
    }),
    (0x8010_8e88, 0x8010_8874, 8, |m, rng, i, _| {
        viewer_block(m, rng, i);
        let context = context(m);
        m.sw(context + 0x12c, [0, 8, 4, 0x20][i % 4]);
        m.sw(context + 0x128, [0, 0xc][i / 4]);
        m.sw(context + 0x44, 1);
    }),
    (0x8010_8298, 0x8010_8874, 2, |m, rng, i, _| {
        viewer_block(m, rng, i)
    }),
    (0x8010_8b7c, 0x8010_8874, 1, |m, rng, i, _| {
        viewer_block(m, rng, i)
    }),
    (0x8010_8d90, 0x8010_8874, 2, |m, rng, i, _| {
        viewer_block(m, rng, i);
        let context = context(m);
        m.sw(context + 0x44, 1);
    }),
    // ---- the branches the cases above miss (by measured instruction coverage)
    // rings and tunnel partly off-screen: the camera moved
    (0x800f_f95c, 0x800f_f95c, 24, |m, rng, i, _| {
        let map = map(m);
        m.sw(map + 0x14, [1, 2][i % 2]);
        m.sw(map + 0x20, 0x40);
        for offset in [0x40u32, 0x44] {
            let t = m.lw(map + offset);
            let range = pick(rng, &[0x100, 0x400, 0x1000, 0x4000, 0x1_0000]);
            m.sw(map + offset, t.wrapping_add(small(rng, range)));
        }
    }),
    (0x8010_0304, 0x8010_0304, 12, |m, rng, i, _| {
        let map = map(m);
        m.sw(map + 0x14, [1, 2][i % 2]);
        m.sw(map + 0x20, 0x40);
        for offset in [0x40u32, 0x44] {
            let t = m.lw(map + offset);
            let range = pick(rng, &[0x100, 0x400, 0x1000, 0x4000, 0x1_0000]);
            m.sw(map + offset, t.wrapping_add(small(rng, range)));
        }
    }),
    // the labels with sliding panels (visual slots 2..4)
    (0x8010_3854, 0x8010_3854, 20, |m, rng, i, _| {
        labels(m);
        let scene = scene(m);
        m.sb(scene + 0x9ce, (1 + i % 4) as u8);
        m.sb(scene + 0x9d6, u8::from(i >= 16));
        for slot in 2..5 {
            let mode = pick(rng, &[0, 1, 2, 3]) as u16;
            set_visual(m, slot, mode, (rng.next() % 31) as u16);
        }
    }),
    // the scene load: no scene (-1), each stream kind
    (0x8010_67b0, 0x8010_67b0, 4, |m, _, i, _| {
        let context = context(m);
        let number = m.lh(context + 0x38c) as i32 as u32;
        if i == 0 {
            m.sh(number.wrapping_mul(12).wrapping_add(SCENES), 0xffff);
        }
        m.sh(
            (number << 2).wrapping_add(SCENE_STREAMS) + 2,
            [0, 1, 0, 2][i],
        );
    }),
    // the story frame: the skip in every state, the minimap's colors
    (0x8010_6a34, 0x8010_6a34, 15, |m, rng, i, _| {
        let scene = scene(m);
        m.sb(scene + 0x9d4, 0);
        m.sb(scene + 0x9d5, 1);
        m.sb(scene + 0x9d6, (i / 5) as u8);
        let hub = hub(m);
        let current = m.lw(hub + 0x16c);
        m.sh(current + 0x46, [0, 1, 1, 2, 3][i % 5]);
        m.sw(
            hub + 0x1a4,
            pick(rng, &[0, 0x102, 0x120, 0x145, 0x200, 0x1000]),
        );
        let context = context(m);
        m.sw(context + 0x12c, 0x20);
        m.sw(context + 0x128, 0);
    }),
    (0x8010_6a34, 0x8010_6a34, 4, |m, _, i, _| {
        let scene = scene(m);
        m.sb(scene + 0x9d4, 0);
        m.sb(scene + 0x9d5, 1);
        m.sb(scene + 0x9d6, [1, 1, 2, 0][i]);
        let hub = hub(m);
        let current = m.lw(hub + 0x16c);
        m.sh(current + 0x46, [3, 2, 3, 3][i]);
        let context = context(m);
        m.sw(context + 0x12c, 0);
    }),
    (0x8010_6a34, 0x8010_6a34, 4, |m, _, i, _| {
        let scene = scene(m);
        m.sb(scene + 0x9d4, 1);
        m.sb(scene + 0x9d5, 1);
        m.sb(scene + 0x9d6, u8::from(i == 3));
        for slot in 0..32u32 {
            let object = m.lw(scene + 0x9dc + 4 * slot);
            m.sh(object, ((slot as usize + i * 3) % 12) as u16);
        }
    }),
    // the entity list: flags, sides, shapes, the locked target
    (0x8010_73b0, 0x8010_73b0, 12, |m, rng, _, _| {
        let table = m.lw(ENTITIES);
        for index in 0..32 {
            let entity = m.lw(table + 4 * index);
            m.sw(
                entity,
                pick(rng, &[0, 0x1_4000, 0x1_c000, 0x1_0000, 0x4000]),
            );
            m.sb(entity + 5, (rng.next() % 4) as u8);
            m.sh(
                entity + 0x48,
                pick(rng, &[0x2b, 0x23, 0x24, 0, 0x2c]) as u16,
            );
        }
        let gameplay = m.lw(GAMEPLAY);
        let camera = m.lw(gameplay + 0x28);
        m.sw(camera + 0xc4, rng.next() % 32);
        m.sw(camera + 0xc8, rng.next() % 32);
        m.sw(camera + 0x50, pick(rng, &[0, 1, 2, 4, 6]));
    }),
    // the model lists: every kind, the locked target, distances across the
    // levels of detail (positions around the eye in spare RAM)
    (0x8010_7518, 0x8010_7518, 24, |m, rng, i, args| {
        let list = args[1];
        let gameplay = m.lw(GAMEPLAY);
        let camera = m.lw(gameplay + 0x28);
        let eye = [m.lw(camera + 4), m.lw(camera + 8), m.lw(camera + 0xc)];
        let model = m.lbu(list);
        let id = m.lhu(list + 2);
        let count = 1 + i % 6;
        for j in 0..count {
            let entry = list + 0x10 * j as u32;
            m.sb(entry, model);
            m.sb(entry + 1, (rng.next() % 2) as u8);
            m.sh(entry + 2, id);
            m.sb(entry + 4, [1, 2, 4, 8, 6, 0x10, 0][(i + j) % 7]);
            m.sb(entry + 5, (rng.next() % 2) as u8);
            let position = SPARE + 0x7000 + 0x10 * j as u32;
            let range = pick(
                rng,
                &[
                    0x8_0000, 0x18_0000, 0x30_0000, 0x60_0000, 0xc0_0000, 0x200_0000,
                ],
            );
            for (k, base) in eye.into_iter().enumerate() {
                m.sw(
                    position + 4 * k as u32,
                    base.wrapping_add(small(rng, range)),
                );
            }
            m.sw(entry + 8, position);
            m.sw(entry + 0xc, pick(rng, &[0, 0x1_c000]));
        }
        m.sb(list + 0x10 * count as u32, 0xff);
        m.sh(camera + 0xb4, small(rng, 0x400) as u16);
        m.sh(camera + 0xb6, small(rng, 0x400) as u16);
    }),
    // the model viewer with the loads finished (made-up resources)
    (0x8010_8334, 0x8010_8874, 4, |m, rng, i, _| {
        viewer_block(m, rng, i);
        m.sb(SPARE + 0x100 + 0x4001, 1);
        let context = context(m);
        m.sw(context + 0x44, [0, 2, 0, 0][i]);
        set_visual(m, 2, [0, 0, 1, 0][i], 0);
        model_resource(m, rng);
    }),
    (0x8010_8d90, 0x8010_8874, 2, |m, rng, i, _| {
        viewer_block(m, rng, i);
        m.sb(SPARE + 0x100 + 0x4001, 1);
        let context = context(m);
        m.sw(context + 0x44, 0);
        set_visual(m, 2, 0, 0);
        model_resource(m, rng);
    }),
    (0x8010_8e88, 0x8010_8874, 8, |m, rng, i, _| {
        viewer_block(m, rng, i);
        let context = context(m);
        m.sw(context + 0x12c, 0x10);
        m.sw(context + 0x128, [0, 0x20][i / 4]);
        m.sw(context + 0x44, 1);
        set_visual(m, 2, (i % 2) as u16, 0);
        let hub = hub(m);
        let current = m.lw(hub + 0x16c);
        m.sh(current + 0x6a, [0, 2][i % 2]);
    }),
    (0x8010_92bc, 0x8010_8874, 6, |m, rng, i, _| {
        viewer_block(m, rng, i);
        m.sb(SPARE + 0x100 + 0x4003, (i % 3) as u8);
        set_visual(m, 2, [0, 1][i / 3], 0);
    }),
    (0x8010_93e8, 0x8010_8874, 4, |m, rng, i, _| {
        viewer_block(m, rng, i);
        m.sb(SPARE + 0x100 + 0x4000, [2, 3, 2, 0][i]);
        let context = context(m);
        m.sw(context + 0x44, 0);
        scene_resource(m, rng);
    }),
];

#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn oracle_cases_match() {
    let routines = extents();
    let mut passed: BTreeMap<u32, usize> = BTreeMap::new();
    let mut failures = Vec::new();
    let mut bases: BTreeMap<u32, Option<(Machine, [u32; 4])>> = BTreeMap::new();
    let run = |label: &str, base: &Machine, start: u32, args: [u32; 4]| {
        let end = routines[&start];
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            case(label, base, start, end, args, VOID.contains(&start))
        }))
        .map_err(|e| {
            format!(
                "{label}: {}",
                e.downcast_ref::<String>()
                    .cloned()
                    .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_default()
            )
        })
    };
    with_fallback(|| {
        // every claimed routine once from its captured entry state
        for &start in routines.keys() {
            if let Some((base, args)) = entries(start).into_iter().next() {
                match run(&format!("{start:08x} capture"), &base, start, args) {
                    Ok(_) => *passed.entry(start).or_default() += 1,
                    Err(e) => failures.push(e),
                }
            }
        }
        // the varied cases
        for &(start, from, count, vary) in GROUPS {
            let base = bases
                .entry(from)
                .or_insert_with(|| entries(from).into_iter().next())
                .clone();
            let Some((base, args)) = base else {
                failures.push(format!("{start:08x}: no captured state of {from:08x}"));
                continue;
            };
            let mut rng = oracle::Rng(start ^ 0x5eed);
            for i in 0..count {
                let mut m = base.clone();
                let mut args = args;
                vary(&mut m, &mut rng, i, &mut args);
                match run(&format!("{start:08x} case {i}"), &m, start, args) {
                    Ok(_) => *passed.entry(start).or_default() += 1,
                    Err(e) => failures.push(e),
                }
            }
        }
    });
    let total: usize = passed.values().sum();
    for (start, n) in &passed {
        println!("oracle {start:08x}: {n} case(s)");
    }
    println!(
        "oracle: {total} cases pass over {} routines, {} failures",
        passed.len(),
        failures.len()
    );
    assert!(failures.is_empty(), "{failures:#?}");
    let unchecked: Vec<String> = routines
        .keys()
        .filter(|a| !passed.contains_key(a))
        .map(|a| format!("{a:08x}"))
        .collect();
    assert!(
        unchecked.is_empty(),
        "routines without a case: {unchecked:?}"
    );
}
