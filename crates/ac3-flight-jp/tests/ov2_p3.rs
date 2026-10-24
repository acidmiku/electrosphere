//! JP overlay 2, part 3 (800E18B8..800EB73C, `src/jp/ov2_p3*.rs`): every
//! routine of the part against the JP original.
//!
//! * [`captured_calls_reproduce`]: every captured call of a routine of the
//!   part (`~/ac3-jp/analysis/redux/*/snapshots-module2.jsonl`) runs
//!   natively from its entry snapshot and must reproduce the return
//!   snapshot (all RAM outside the stack, scratchpad, GTE, `v0` unless the
//!   arm is void). Device calls are answered as the original's were, from
//!   the return snapshot where it records them (see `script`).
//! * [`routines_match_the_oracle`]: every routine of the part runs as the
//!   original instructions (the test-only oracle) and natively from the
//!   same JP overlay-2 RAM images with randomised arguments and fields
//!   (card block, scene block, context), and must leave the same machine.
//!
//! Calls out of a routine go through `dispatch::call` in both runs; the
//! routines of the other overlay-2 parts that nobody has translated yet
//! run interpreted (`dev-fallback`), so this file needs the feature:
//! `cargo test --release -p ac3-flight-jp --features dev-fallback --test
//! ov2_p3 -- --include-ignored`.
#![cfg(feature = "dev-fallback")]
mod support;

use ac3_fallback::{Fallback, Version};
use ac3_flight_jp::snapshot::{self, Snapshot, SnapshotInfo};
use ac3_flight_jp::{Device, HostCall, Machine, dispatch};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use support::scan;

/// The part's address range.
const PART: (u32, u32) = (0x800e_18b8, 0x800e_b944);
/// The card block pointer.
const CARD: u32 = 0x8011_2df0;

/// The routines the part's files claim, and the void ones.
fn claims() -> (BTreeSet<u32>, BTreeSet<u32>) {
    let (claims, void) =
        scan::claims_and_void(&scan::crate_dir(), &|name| !name.starts_with("jp/ov2_p3"));
    let mine: BTreeSet<u32> = claims
        .keys()
        .copied()
        .filter(|a| (PART.0..PART.1).contains(a))
        .collect();
    assert_eq!(mine.len(), claims.len(), "a claim outside the part");
    (mine, void)
}

/// The capture root (`AC3_JP_CAPTURES`, else `AC3_JP_DIR/analysis/redux`).
fn root() -> PathBuf {
    std::env::var_os("AC3_JP_CAPTURES")
        .map(PathBuf::from)
        .unwrap_or_else(|| scan::jp_dir().join("analysis/redux"))
}

/// A device answering scripted results per host-call ID, in order; other
/// calls get the default.
struct Script(BTreeMap<u32, VecDeque<u32>>);
impl Device for Script {
    fn call(&mut self, _: &mut [u8], _: &mut [u8], call: HostCall) -> Option<u32> {
        self.0.get_mut(&call.function)?.pop_front()
    }
}

const TEST_EVENT: u32 = 0x800a_d788;
const OPEN_EVENT: u32 = 0x800a_d758;
const ENABLE_EVENT: u32 = 0x800a_d798;
const CLOSE_EVENT: u32 = 0x800a_d768;
const OPEN: u32 = 0x800a_d808;

fn word(s: &Snapshot, address: u32) -> u32 {
    let at = (address & 0x1f_ffff) as usize;
    u32::from_le_bytes(s.ram[at..at + 4].try_into().unwrap())
}

/// The device results of the original's call, from its return snapshot:
/// the event descriptors it opened, the file it opened, and which polled
/// event had occurred (its result).
fn script(callee: u32, entry: &Snapshot, exit: &Snapshot) -> BTreeMap<u32, VecDeque<u32>> {
    let card = word(entry, CARD);
    let v0 = exit.gpr(2);
    let mut out: BTreeMap<u32, VecDeque<u32>> = BTreeMap::new();
    let events = |offsets: &[u32]| -> VecDeque<u32> {
        // TestEvent of the polled events in order: the one whose index is
        // the result occurred (a result of -1: none)
        offsets
            .iter()
            .enumerate()
            .map(|(i, _)| u32::from(i as u32 == v0))
            .collect()
    };
    match callee {
        0x800e_2794 => {
            out.insert(TEST_EVENT, events(&[4, 8, 0xc, 0x10]));
        }
        0x800e_294c => {
            out.insert(TEST_EVENT, events(&[0x14, 0x18, 0x1c, 0x20]));
        }
        0x800e_2874 | 0x800e_28e0 => {
            out.insert(TEST_EVENT, [v0; 4].into());
        }
        0x800e_3fcc => {
            let opened = [4, 8, 0xc, 0x10, 0x14, 0x18, 0x1c].map(|o| word(exit, card + o));
            out.insert(OPEN_EVENT, opened.into());
            out.insert(ENABLE_EVENT, [v0; 7].into());
        }
        0x800e_419c => {
            out.insert(CLOSE_EVENT, [v0; 7].into());
        }
        // the record transfers: the file they open, and their completion
        // poll (still running when the transfer flag +0x336 stays set,
        // else done)
        // and the screen steps that run them
        0x800e_4ef0 | 0x800e_5100 | 0x800e_53a0 | 0x800e_5664 | 0x800e_57f0 | 0x800e_5a28
        | 0x800e_5ca4 | 0x800e_8b98 | 0x800e_8c50 | 0x800e_8e28 | 0x800e_955c | 0x800e_95dc
        | 0x800e_9f64 | 0x800e_a004 | 0x800e_a0c8 | 0x800e_b064 | 0x800e_b174 => {
            let running = exit.ram[((card + 0x336) & 0x1f_ffff) as usize] == 1;
            out.insert(TEST_EVENT, if running { [0; 4].into() } else { [1].into() });
        }
        _ => {}
    }
    out.insert(OPEN, [word(exit, card + 0x3c)].into());
    out
}

/// RAM the comparisons leave out, written by kernel or SDK code the port
/// answers with host calls: the kernel area (event and file control
/// blocks, the card driver: host state, `bios.rs`) and libc printf's TTY
/// line buffer (JP 800CBF68, PAL 800C9D40: `sdk::printf` is a host call).
const HOST_OWNED: [(u32, u32, &str); 2] = [
    (0x8000_0000, 0x8001_0000, "kernel area"),
    (0x800c_bf68, 0x800c_bf88, "libc printf TTY line buffer"),
];

fn host_owned(address: u32) -> bool {
    HOST_OWNED
        .iter()
        .any(|(start, end, _)| (*start..*end).contains(&address))
}

/// `snapshot::compare` with [`HOST_OWNED`] left out: all other RAM outside
/// the stack window, the scratchpad and the GTE.
fn compare(m: &Machine, entry: &Snapshot, exit: &Snapshot) -> Result<(), String> {
    let ram: Vec<u32> = (0..0x20_0000usize)
        .filter(|&i| !(snapshot::STACK_START..0x20_0000).contains(&i))
        .map(|i| (i, 0x8000_0000 + i as u32))
        .filter(|&(i, a)| !host_owned(a) && m.ram()[i] != exit.ram[i])
        .map(|(_, a)| a)
        .collect();
    let scratch = (0..0x400)
        .filter(|&i| m.scratch()[i] != exit.scratch[i])
        .count();
    let gte = m.gte.data == exit.info.registers.cp2d && m.gte.control == exit.info.registers.cp2c;
    if ram.is_empty() && scratch == 0 && gte {
        return Ok(());
    }
    let shown: Vec<String> = ram
        .iter()
        .take(8)
        .map(|&a| {
            let i = (a & 0x1f_ffff) as usize;
            format!(
                "{a:08x}: entry {:02x} original {:02x} native {:02x}",
                entry.ram[i],
                exit.ram[i],
                m.ram()[i]
            )
        })
        .collect();
    Err(format!(
        "state differs: {} RAM bytes [{}], scratch {scratch}, GTE equal {gte}",
        ram.len(),
        shown.join(", ")
    ))
}

fn panic_text(e: &(dyn std::any::Any + Send)) -> String {
    e.downcast_ref::<String>()
        .cloned()
        .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default()
}

/// Replay one captured call: Ok(bytes the original changed) or Err(cause).
fn replay(
    directory: &Path,
    entry: &SnapshotInfo,
    exit: &SnapshotInfo,
    void: bool,
) -> Result<usize, String> {
    let entry = Snapshot::load(directory, entry).map_err(|e| format!("load: {e}"))?;
    let exit = Snapshot::load(directory, exit).map_err(|e| format!("load: {e}"))?;
    let mut m = entry.machine();
    let callee = entry.info.callee;
    m.device = Some(Box::new(Script(script(callee, &entry, &exit))));
    let args = [entry.gpr(4), entry.gpr(5), entry.gpr(6), entry.gpr(7)];
    let v0 = panic::catch_unwind(AssertUnwindSafe(|| dispatch::call(&mut m, callee, args)))
        .map_err(|e| format!("panic: {}", panic_text(&*e)))?;
    m.take_fault().map_err(|f| format!("fault: {f}"))?;
    compare(&m, &entry, &exit)?;
    if !void && v0 != exit.gpr(2) {
        return Err(format!("v0 {v0:#010x}, original {:#010x}", exit.gpr(2)));
    }
    Ok(snapshot::changed_bytes(&entry, &exit))
}

#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn captured_calls_reproduce() {
    let (mine, void) = claims();
    let fallback = ac3_fallback::install(Fallback::new(Version::Jp));
    let mut results: Vec<(String, u32, Result<usize, String>)> = Vec::new();
    let mut runs: Vec<PathBuf> = std::fs::read_dir(root())
        .expect("captures")
        .flatten()
        .map(|r| r.path().join("snapshots-module2.jsonl"))
        .filter(|p| p.exists())
        .collect();
    runs.sort();
    for index_file in &runs {
        let directory = index_file.parent().unwrap();
        let run = directory
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let index = snapshot::read_index(index_file).expect("index");
        for entry in index.iter().filter(|s| s.stage == "entry") {
            if !mine.contains(&entry.callee) {
                continue;
            }
            let Some(exit) = index.iter().find(|s| {
                s.callee == entry.callee
                    && s.stage == "return"
                    && s.sequence > entry.sequence
                    && s.ra == entry.ra
                    && s.registers.gpr[29] == entry.registers.gpr[29]
            }) else {
                continue;
            };
            let result = replay(directory, entry, exit, void.contains(&entry.callee));
            results.push((format!("{run} #{}", entry.sequence), entry.callee, result));
        }
    }
    let captured: BTreeSet<u32> = results.iter().map(|r| r.1).collect();
    let mut failures = Vec::new();
    for (at, callee, result) in &results {
        match result {
            Ok(n) => println!("pass {callee:08x} {at}: {n} bytes changed"),
            Err(e) => {
                println!("FAIL {callee:08x} {at}: {e}");
                failures.push(format!("{callee:08x} {at}: {e}"));
            }
        }
    }
    println!(
        "captured: {} calls of {} routines ({} claimed by the part); {} fail",
        results.len(),
        captured.len(),
        mine.len(),
        failures.len()
    );
    println!("{}", fallback.summary());
    ac3_fallback::uninstall();
    assert!(!results.is_empty(), "no captured call of the part");
    assert!(failures.is_empty(), "{failures:#?}");
}

/// JP overlay-2 RAM images (module 2 loaded): the story screens, the save
/// screens and the hub after a mission.
const STATES: [&str; 11] = [
    "story-001/ram-017800",
    "story-001/ram-030300",
    "story-001/ram-041610",
    "story-001/ram-046300",
    "story-001/ram-049510",
    "save-001/ram-054010",
    "save-001/ram-054490",
    "save-001/ram-055190",
    "save-001/ram-055590",
    "save-001/ram-056390",
    "flight-001/ram-053900",
];
/// Randomised variants per image and routine.
const VARIANTS: u32 = 10;
/// Guest `sp` at entry, and the window the comparison leaves out.
const SP: u32 = 0x801f_ff00;
const STACK: std::ops::Range<usize> = 0x1f_c000..0x20_0000;
/// The save screen's state block, the scene block.
const SAVE: u32 = 0x8011_2df8;
const SCENE: u32 = 0x8011_2dc4;
const DRAW: u32 = 0x8011_2dc0;

/// A device that answers nothing and stops a run after `limit` host calls
/// (a wait on a device the test does not model).
struct Limit(usize, usize);
impl Device for Limit {
    fn call(&mut self, _: &mut [u8], _: &mut [u8], _: HostCall) -> Option<u32> {
        self.0 += 1;
        assert!(self.0 <= self.1, "runaway: {} host calls", self.1);
        None
    }
}

/// The variant generator (the PAL oracle's LCG).
struct Rng(u32);
impl Rng {
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(1_103_515_245).wrapping_add(12345);
        self.0 >> 8
    }
    fn chance(&mut self, n: u32) -> bool {
        self.next().is_multiple_of(n)
    }
    fn pick(&mut self, values: &[u32]) -> u32 {
        values[self.next() as usize % values.len()]
    }
}

/// Arguments as the callers pass them: the small indices most routines
/// take (ports, accounts, records, choices, flags, -1 for "the current
/// one"), and valid pointers for the routines that take pointers (a
/// garbage index or pointer makes the originals read garbage records and
/// print strings that never end, paths no caller takes).
fn arguments(start: u32, rng: &mut Rng, m: &mut Machine) -> ([u32; 4], u32) {
    let small = |rng: &mut Rng| rng.pick(&[0, 0, 1, 1, 2, 2, 3, 4, 5, 6, u32::MAX]);
    let mut args: [u32; 4] = std::array::from_fn(|_| small(rng));
    let mut fifth = 0;
    let draw = m.lw(DRAW);
    let cursor = m.lw(draw.wrapping_add(4));
    let scene = m.lw(SCENE);
    let card = m.lw(CARD);
    let ctx = m.lw(0x800b_ce60);
    match start {
        // the frame's primitive buffer
        0x800e_2494 => args[0] = cursor,
        // the slot byte's destination
        0x800e_30f4 => args[0] = card.wrapping_add(0x320),
        // a mission and the palette words
        0x800e_38dc => args[1] = card.wrapping_add(0x5c),
        // a name
        0x800e_5ea0 => args[0] = ctx.wrapping_add(0x948),
        // an account and one of its six records (and the "none" flag)
        0x800e_1f94 => {
            args[0] = rng.pick(&[0, 1, 2]);
            args[1] = rng.pick(&[0, 1, 2, 3, 4, 5]);
        }
        // the callers pass the loaded account (save block +5)
        0x800e_6c4c => {
            let save = m.lw(SAVE);
            args[0] = rng.pick(&[0, 1, 2, 3, 4, 5]);
            args[1] = u32::from(m.lbu(save.wrapping_add(5)));
            args[2] = rng.pick(&[0, 1]);
        }
        // a screen position
        0x800e_5f88 => {
            args[0] = rng.next() % 0x140;
            args[1] = rng.next() % 0x1e0;
        }
        // position, ordering-table entry, angle, primitive buffer
        0x800e_6dfc => {
            args[0] = rng.next() % 0x140;
            args[1] = rng.next() % 0x1e0;
            args[2] = m
                .lw(scene.wrapping_add(8))
                .wrapping_add(4 * (rng.next() % 16));
            args[3] = rng.next() % 0x1000;
            fifth = cursor;
        }
        _ => {}
    }
    (args, fifth)
}

/// Randomise the fields that select the routines' paths: the save
/// screen's state block, the card block's monitor and transfer fields,
/// the pad words of the context, the scene block's handler and selection.
fn randomise(m: &mut Machine, rng: &mut Rng, marks: bool) {
    let save = m.lw(SAVE);
    if (0x8001_0000..0x8020_0000).contains(&save) {
        for offset in [0u32, 1, 2, 3, 4, 5, 6, 7, 8, 9, 0xa] {
            // the account the screen loaded (the others' records hold
            // what the card held for them, their texts possibly garbage)
            if offset == 5 && !marks {
                continue;
            }
            if rng.chance(2) {
                let v = match offset {
                    // one of the account's six records
                    6 if !marks => rng.pick(&[0, 1, 2, 3, 4, 5]),
                    _ => rng.pick(&[0, 1, 2, 3, 5, 0x78, 0x80]),
                };
                m.sb(save + offset, v as u8);
            }
        }
        if rng.chance(2) {
            m.sh(save + 0x1a, rng.pick(&[0, 1, 2]) as u16);
        }
    }
    let card = m.lw(CARD);
    if (0x8001_0000..0x8020_0000).contains(&card) {
        for offset in 0x348u32..0x352 {
            if rng.chance(3) {
                let v = rng.pick(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 0x10, 0x11, 0xff]);
                m.sb(card + offset, v as u8);
            }
        }
        for (offset, values) in [
            (0x336u32, &[0u32, 1][..]),
            (0x358, &[0, 1, 2, 3, 4]),
            (0x35a, &[0, 1]),
            (0x50, &[0, 1, 2, 5, 6]),
            (0x2c, &[0, 1, 2, 3, 4]),
            (0x2e, &[0, 1, 2, 3, 4]),
            (0x30, &[0, 1, 2, 3, 4]),
            (0x32, &[0, 1, 2, 3, 4]),
        ] {
            if rng.chance(3) {
                m.sh(card + offset, rng.pick(values) as u16);
            }
        }
        for (offset, values) in [
            (0x28u32, &[0u32, 1, 2][..]),
            (0x354, &[0, 1]),
            (0x318, &[0, 1, 0x12c, u32::MAX]),
        ] {
            if rng.chance(3) {
                m.sw(card + offset, rng.pick(values));
            }
        }
        let info = m.lw(card + 0x2e8);
        if marks && (0x8001_0000..0x8020_0000).contains(&info) && rng.chance(2) {
            m.sb(info + 8, (rng.next() % 8) as u8);
        }
        for i in 0..4 {
            let slot = m.lw(card + 0x2ec + 4 * i);
            if marks && (0x8001_0000..0x8020_0000).contains(&slot) && rng.chance(2) {
                m.sh(slot + 0x16, (rng.next() % 0x80) as u16);
            }
        }
    }
    let ctx = m.lw(0x800b_ce60);
    if (0x8001_0000..0x8020_0000).contains(&ctx) {
        for offset in [0x124u32, 0x128, 0x12c, 0x130] {
            if rng.chance(2) {
                let v = rng.pick(&[0, 1, 2, 3, 4, 8, 0x10, 0x20, 0x40, 0x80, 0x12]);
                m.sw(ctx + offset, v);
            }
        }
    }
    // (the text routines read the context's mission and story fields)
    if marks && (0x8001_0000..0x8020_0000).contains(&ctx) {
        if rng.chance(3) {
            m.sh(ctx + 0x954, rng.pick(&[0, 1, 2, 3, 4]) as u16);
        }
        if rng.chance(3) {
            m.sw(ctx + 0x96c, rng.pick(&[0, 8, 0x10, 0x20, 0x40, 0x80]));
        }
        if rng.chance(3) {
            m.sh(
                ctx + 0x38c,
                rng.pick(&[0, 0x10, 0x25, 0x2a, 0x3b, 5]) as u16,
            );
        }
    }
    let scene = m.lw(SCENE);
    if marks && (0x8001_0000..0x8020_0000).contains(&scene) {
        if rng.chance(3) {
            m.sw(scene + 0xe0, rng.pick(&[0, 1, 2, 3, 4, 5]));
        }
        if rng.chance(3) {
            m.sb(scene + 0x7e, rng.pick(&[0, 1, 2, 3, 4]) as u8);
        }
    }
}

/// One case: Ok(Some(RAM bytes the original changed)), Ok(None) for an
/// invalid state (skipped), or Err(why).
fn oracle_case(
    base: &Machine,
    (start, end): (u32, u32),
    args: [u32; 4],
    void: bool,
) -> Result<Option<usize>, String> {
    let mut native = base.clone();
    native.device = Some(Box::new(Limit(0, 20_000)));
    let nv0 = panic::catch_unwind(AssertUnwindSafe(|| {
        dispatch::call(&mut native, start, args)
    }))
    .map_err(|e| format!("native panic: {}", panic_text(&*e)))?;
    // A state whose callees write over the stack at and above the entry
    // `sp` (the routine's saved return address and its caller's frame:
    // garbage records printed past the end of the primitive buffer) would
    // send the original to a garbage return address. Translations keep
    // their frames in Rust and never write there, so such a case is an
    // invalid state, not a comparison: it is skipped.
    let guarded = (SP - 8) as usize & 0x1f_ffff..0x20_0000;
    if guarded.clone().any(|i| native.ram()[i] != base.ram()[i]) {
        return Ok(None);
    }
    let mut original = base.clone();
    original.device = Some(Box::new(Limit(0, 20_000)));
    let v0 = panic::catch_unwind(AssertUnwindSafe(|| {
        support::oracle::run(&mut original, start, end, args)
    }))
    .map_err(|e| format!("oracle stopped: {}", panic_text(&*e)))?;
    let (fo, fnat) = (original.take_fault(), native.take_fault());
    if fo != fnat {
        return Err(format!("faults differ: original {fo:?}, native {fnat:?}"));
    }
    let ram: Vec<u32> = (0..0x20_0000usize)
        .filter(|&i| !STACK.contains(&i) && original.ram()[i] != native.ram()[i])
        .map(|i| 0x8000_0000 + i as u32)
        .collect();
    let scratch = (0..0x400)
        .filter(|&i| original.scratch()[i] != native.scratch()[i])
        .count();
    if !ram.is_empty() || scratch != 0 || original.gte != native.gte || original.sp != native.sp {
        let shown: Vec<String> = ram
            .iter()
            .take(6)
            .map(|&a| {
                let i = (a & 0x1f_ffff) as usize;
                format!(
                    "{a:08x}: base {:02x} original {:02x} native {:02x}",
                    base.ram()[i],
                    original.ram()[i],
                    native.ram()[i]
                )
            })
            .collect();
        return Err(format!(
            "state differs: {} RAM bytes [{}], scratch {scratch}, GTE equal {}, sp {:x}/{:x}",
            ram.len(),
            shown.join(", "),
            original.gte == native.gte,
            original.sp,
            native.sp
        ));
    }
    if !void && v0 != nv0 {
        return Err(format!("v0: original {v0:#x}, native {nv0:#x}"));
    }
    Ok(Some(
        (0..0x20_0000usize)
            .filter(|&i| !STACK.contains(&i) && base.ram()[i] != original.ram()[i])
            .count(),
    ))
}

/// One routine's result: Ok((cases, RAM bytes changed, skipped cases)) or
/// the first failing case.
type Outcome = Result<(usize, usize, usize), String>;

/// The cases of one routine.
fn oracle_routine((start, end): (u32, u32), void: bool, states: &[(String, Vec<u8>)]) -> Outcome {
    let (mut cases, mut changed, mut skipped) = (0usize, 0usize, 0usize);
    for (name, ram) in states {
        // the record texts are the save screens' (records read from the
        // card); elsewhere the arena's record buffers hold leftovers
        if PRINTS_RECORDS.contains(&start) && !name.starts_with("save-") {
            continue;
        }
        for variant in 0..VARIANTS {
            let mut rng = Rng(start ^ (variant << 24) ^ (name.len() as u32 * 0x9e37));
            let mut base = Machine::from_images(ram, &[0; 0x400]).expect("image");
            base.sp = SP;
            if variant > 0 {
                randomise(&mut base, &mut rng, !PRINTS_RECORDS.contains(&start));
            }
            let scene = base.lw(SCENE);
            let (mut args, fifth) = arguments(start, &mut rng, &mut base);
            if start == 0x800e_22e4 {
                // a display list the scene enables (the only lists the
                // callers run: 800E23FC by the enable masks); a disabled
                // list holds no terminator and runs away in the original
                let layer = rng.next() % 3;
                let mask = base.lw(scene.wrapping_add(0x178 + 4 * layer));
                let enabled: Vec<u32> = (0..32).filter(|i| mask & (1 << i) != 0).collect();
                if enabled.is_empty() {
                    continue;
                }
                args[0] = enabled[rng.next() as usize % enabled.len()];
                args[1] = layer;
            }
            // a fifth argument in the caller's frame
            base.sw(SP + 0x10, fifth);
            cases += 1;
            let compare_v0 = !void && !EXIT_CRITICAL_V0.contains(&start);
            match oracle_case(&base, (start, end), args, !compare_v0) {
                Ok(Some(n)) => changed += n,
                Ok(None) => {
                    cases -= 1;
                    skipped += 1;
                }
                Err(e) => return Err(format!("{name} v{variant} {args:08x?}: {e}")),
            }
        }
    }
    Ok((cases, changed, skipped))
}

/// The routines that print the texts of the present mission records
/// (unit, level, time): a record marked present at random holds whatever
/// the arena held, and its text indices then select garbage strings that
/// the interpreted text routines print for minutes. They run on the save
/// screens' images only (records read from the card), with the card's own
/// present marks.
const PRINTS_RECORDS: [u32; 9] = [
    0x800e_1f94,
    0x800e_6c4c,
    0x800e_a3f4,
    0x800e_a744,
    0x800e_a8ac,
    0x800e_ac04,
    0x800e_af44,
    0x800e_b064,
    0x800e_b174,
];

/// The step limit of an interpreted callee in the oracle cases.
const FALLBACK_STEPS: u64 = 4_000_000;

/// Worker threads, and the time one routine's cases may take (the text
/// and display-list routines run long interpreted callees).
const WORKERS: usize = 6;
const ROUTINE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1500);

/// Routines returning ExitCriticalSection's `v0`: the kernel leaves `v0`
/// alone, so it is the last EnableEvent / CloseEvent result, which the
/// translations pass on (`bios::exit_critical_section`); the oracle calls
/// the stub by address, whose dispatch arm cannot know it and returns 0.
/// Their RAM is compared; their `v0` is checked by the captures instead.
const EXIT_CRITICAL_V0: [u32; 2] = [0x800e_3fcc, 0x800e_419c];

#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux RAM images)"]
fn routines_match_the_oracle() {
    let (mine, void) = claims();
    let dir = scan::jp_dir();
    let rows = scan::jp_map(&dir);
    let extents: Vec<(u32, u32, bool)> = rows
        .iter()
        .filter(|r| r.region == "ov2" && mine.contains(&r.jp))
        .map(|r| (r.jp, r.jp + r.size, void.contains(&r.jp)))
        .collect();
    assert_eq!(extents.len(), mine.len(), "a claim without its extent");
    let unclaimed: Vec<u32> = rows
        .iter()
        .filter(|r| r.region == "ov2" && (PART.0..PART.1).contains(&r.jp) && !mine.contains(&r.jp))
        .map(|r| r.jp)
        .collect();
    assert!(
        unclaimed.is_empty(),
        "routines of the part nobody claims: {unclaimed:08x?}"
    );
    // `ORACLE_ONLY=800e2494,800e6c4c`: a subset, for development
    let only: Vec<u32> = std::env::var("ORACLE_ONLY")
        .map(|v| {
            v.split(',')
                .filter_map(|a| u32::from_str_radix(a.trim(), 16).ok())
                .collect()
        })
        .unwrap_or_default();
    let extents: Vec<(u32, u32, bool)> = extents
        .into_iter()
        .filter(|e| only.is_empty() || only.contains(&e.0))
        .collect();
    let states: Vec<(String, Vec<u8>)> = STATES
        .iter()
        .map(|name| {
            let path = dir.join(format!("analysis/redux/{name}.ram.bin"));
            let ram = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let mut m = Machine::from_images(&ram, &[0; 0x400]).expect("image");
            assert_eq!(ac3_flight_jp::common::loaded_module(&mut m), 2, "{name}");
            (name.to_string(), ram)
        })
        .collect();
    let states = std::sync::Arc::new(states);
    let hook = panic::take_hook();
    panic::set_hook(Box::new(|info| {
        if std::thread::current().name() != Some("oracle-worker") {
            eprintln!("{info}");
        }
    }));
    let (tx, rx) = std::sync::mpsc::channel::<(usize, Outcome)>();
    let mut results: Vec<Option<Outcome>> = vec![None; extents.len()];
    let mut active: BTreeMap<usize, std::time::Instant> = BTreeMap::new();
    let (mut next, mut done) = (0usize, 0usize);
    while done < extents.len() {
        while active.len() < WORKERS && next < extents.len() {
            let (start, end, void) = extents[next];
            let (states, tx, index) = (states.clone(), tx.clone(), next);
            std::thread::Builder::new()
                .name("oracle-worker".into())
                .stack_size(64 << 20)
                .spawn(move || {
                    // an interpreted callee that runs away on a random
                    // state (a garbage string it prints) stops in both
                    // runs alike, as a fault
                    let mut fallback = Fallback::new(Version::Jp);
                    fallback.set_max_steps(FALLBACK_STEPS);
                    let _fallback = ac3_fallback::install(fallback);
                    let result = oracle_routine((start, end), void, &states);
                    ac3_fallback::uninstall();
                    let _ = tx.send((index, result));
                })
                .expect("worker");
            active.insert(next, std::time::Instant::now());
            next += 1;
        }
        match rx.recv_timeout(std::time::Duration::from_secs(1)) {
            Ok((index, result)) => {
                active.remove(&index);
                eprintln!("oracle {:08x}: {result:?}", extents[index].0);
                results[index] = Some(result);
                done += 1;
            }
            Err(_) => {
                let late: Vec<usize> = active
                    .iter()
                    .filter(|(_, t)| t.elapsed() > ROUTINE_TIMEOUT)
                    .map(|(i, _)| *i)
                    .collect();
                for index in late {
                    active.remove(&index);
                    results[index] = Some(Err("does not finish in time".into()));
                    done += 1;
                }
            }
        }
    }
    panic::set_hook(hook);
    let (mut cases, mut changed, mut failures, mut unchanged) = (0, 0, Vec::new(), Vec::new());
    let mut skipped: Vec<(u32, usize)> = Vec::new();
    for ((start, _, _), result) in extents.iter().zip(&results) {
        match result {
            Some(Ok((n, c, s))) => {
                cases += n;
                changed += c;
                if *s != 0 {
                    skipped.push((*start, *s));
                }
                if *c == 0 {
                    unchanged.push(*start);
                }
            }
            Some(Err(e)) => failures.push(format!("{start:08x} {e}")),
            None => failures.push(format!("{start:08x} not run")),
        }
    }
    println!(
        "oracle: {} routines, {cases} passing cases over {} JP overlay-2 images, {} failures; {changed} RAM bytes changed in all",
        extents.len(),
        states.len(),
        failures.len()
    );
    println!(
        "oracle: {} routines change no RAM in any case (v0, GTE and faults compared): {:08x?}",
        unchanged.len(),
        unchanged
    );
    println!(
        "oracle: cases skipped as invalid states (callees writing over the entry frame): {:08x?}",
        skipped
    );
    for f in &failures {
        println!("FAIL {f}");
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
