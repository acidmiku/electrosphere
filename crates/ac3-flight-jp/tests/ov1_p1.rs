//! JP overlay 1, part 1 (`src/jp/ov1_p1*.rs`, the routines
//! 800CC5D8..800D4808) against the JP originals:
//!
//! * [`captured_calls`]: every captured call (`~/ac3-jp/analysis/redux/
//!   */snapshots-module1.jsonl`, boot-002 and save-002) of every routine
//!   the part claims runs natively from its entry snapshot and must
//!   reproduce the return snapshot: RAM outside the stack, scratchpad,
//!   GTE, and `v0` unless the translation models none; or pass as device
//!   work with the proof of `support::proof` (the translation equals the
//!   original instructions from the same entry and reached a device).
//!   The replays (`proof::replay_machine`) answer the BIOS event calls from
//!   the kernel's event table and the CD drive and VRAM reads from the
//!   capture pair (`proof::Replay`). Untranslated callees run interpreted
//!   through the development fallback, as in `ac3-dev`.
//! * [`oracle_cases`]: the routines no capture reaches, and the paths the
//!   captures miss, against the JP instructions (the test oracle,
//!   `support::oracle`, through `proof::compare`) on the JP RAM dumps of
//!   module 1 (boot-001, save-001), with fields and arguments varied.
//!
//! Feature `dev-fallback`, data under `AC3_JP_DIR` (default `~/ac3-jp`):
//! `cargo test --release -p ac3-flight-jp --features dev-fallback --test
//! ov1_p1 -- --include-ignored --nocapture`.
#![cfg(feature = "dev-fallback")]
mod support;

use ac3_fallback::{Fallback, Guest, Version};
use ac3_flight_jp::{
    Device, HostCall, Machine, dispatch,
    fallback::MachineGuest,
    snapshot::{self, Snapshot, SnapshotInfo},
};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use support::proof::{self, Bios};
use support::{oracle, scan};

/// The capture root (`AC3_JP_CAPTURES`, else `AC3_JP_DIR/analysis/redux`).
fn root() -> PathBuf {
    std::env::var_os("AC3_JP_CAPTURES")
        .map(PathBuf::from)
        .unwrap_or_else(|| scan::jp_dir().join("analysis/redux"))
}

/// The part's claims in module 1 (files `jp/ov1_p1*`), and those whose
/// translation models no `v0`.
fn claims() -> (BTreeSet<u32>, BTreeSet<u32>) {
    let (all, void) =
        scan::claims_and_void(&scan::crate_dir(), &|name| !name.starts_with("jp/ov1_p1"));
    (all.keys().copied().collect(), void)
}

/// The function extents of overlay 1 (`jp_pal_map.tsv`): start -> end.
fn extents() -> BTreeMap<u32, u32> {
    scan::jp_map(&scan::jp_dir())
        .into_iter()
        .filter(|r| r.region == "ov1")
        .map(|r| (r.jp, r.jp + r.size))
        .collect()
}

/// The oracle states' card file answers (a test fixture of the oracle
/// cases only, the [`Bios`] device's extra answers; the captures never
/// use it): the word [`CARD_FILE`] says whether `open` (canonical
/// 0x800AD808) gives a descriptor (bit 0; else its default -1), whether
/// `read` (0x800AD818) of a 0x80-byte header fills it with a valid one
/// (bit 1: word 1 0x00990426, word 0 the XOR of words 1..31 with
/// 0x41434533) or an empty one (bit 2), returning the size; `lseek` (JP
/// 0x800AF3E8) returns the offset; the GPU status (0x800A7B04) has the
/// interlace field bit 31 with bit 3.
fn card_file(ram: &mut [u8], call: HostCall) -> Option<u32> {
    let at = |a: u32| (a & 0x1f_ffff) as usize;
    let flags = u32::from_le_bytes(ram[at(CARD_FILE)..at(CARD_FILE) + 4].try_into().unwrap());
    if flags & 0x8000_0000 == 0 {
        return None;
    }
    let [_, a1, a2, _] = call.args;
    match call.function {
        0x800a_d808 if flags & 1 != 0 => Some(3),
        0x800a_f3e8 => Some(a1),
        0x800a_d818 if a2 == 0x80 && flags & 6 != 0 => {
            let mut words = [0u32; 32];
            if flags & 2 != 0 {
                words[1] = 0x0099_0426;
                for (k, w) in words.iter_mut().enumerate().skip(2) {
                    *w = (k as u32).wrapping_mul(0x0123_4567);
                }
                words[0] = words[1..].iter().fold(0x4143_4533, |v, w| v ^ w);
            }
            for (k, w) in words.iter().enumerate() {
                let a = at(a1) + 4 * k;
                ram[a..a + 4].copy_from_slice(&w.to_le_bytes());
            }
            Some(a2)
        }
        0x800a_d818 if flags & 1 != 0 => Some(a2),
        0x800a_7b04 if flags & 8 != 0 => Some(0x8000_0000),
        _ => None,
    }
}

/// The word the card file answers of the oracle states read (bit 31 on).
const CARD_FILE: u32 = 0x801f_bff0;

fn exit_of<'a>(index: &'a [SnapshotInfo], entry: &SnapshotInfo) -> Option<&'a SnapshotInfo> {
    index.iter().find(|s| {
        s.callee == entry.callee
            && s.stage == "return"
            && s.sequence > entry.sequence
            && s.ra == entry.ra
            && s.registers.gpr[29] == entry.registers.gpr[29]
    })
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
    let mut m = proof::replay_machine(&entry, &exit, 1);
    let args = [entry.gpr(4), entry.gpr(5), entry.gpr(6), entry.gpr(7)];
    let v0 = panic::catch_unwind(AssertUnwindSafe(|| {
        dispatch::call(&mut m, entry.info.callee, args)
    }))
    .map_err(|e| format!("panic: {}", panic_text(&*e)))?;
    if let Err(fault) = m.take_fault() {
        return Err(format!("fault: {fault}"));
    }
    let d = snapshot::compare(&m, &exit);
    if !d.is_empty() {
        let mut ranges: Vec<(u32, u32)> = Vec::new();
        for &a in &d.ram {
            match ranges.last_mut() {
                Some((_, end)) if a <= *end + 16 => *end = a,
                _ => ranges.push((a, a)),
            }
        }
        let text: Vec<String> = ranges
            .iter()
            .take(24)
            .map(|(a, b)| format!("{a:08x}-{b:08x}"))
            .collect();
        return Err(format!(
            "state differs: {} ranges {}",
            d.summary(),
            text.join(" ")
        ));
    }
    if !void && v0 != exit.gpr(2) {
        return Err(format!(
            "v0 differs: {v0:#010x}, original {:#010x}",
            exit.gpr(2)
        ));
    }
    Ok(snapshot::changed_bytes(&entry, &exit))
}

/// The device-work proof ([`proof::device_work`]) of a call its replay
/// does not reproduce.
fn device_work(
    directory: &Path,
    entry: &SnapshotInfo,
    exit: &SnapshotInfo,
    end: u32,
    void: bool,
) -> Result<String, String> {
    let entry = Snapshot::load(directory, entry).map_err(|e| format!("load: {e}"))?;
    let exit = Snapshot::load(directory, exit).map_err(|e| format!("load: {e}"))?;
    let base = proof::replay_machine(&entry, &exit, 1);
    let call = proof::Call {
        entry: &entry,
        exit: &exit,
        base: &base,
        module: 1,
        range: (entry.info.callee, end),
        void,
    };
    proof::device_work(&call, &|| Box::new(proof::Replay::new(&entry, &exit, 1)))
}

#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn captured_calls() {
    let (claimed, void) = claims();
    assert!(!claimed.is_empty(), "no claims in jp/ov1_p1*");
    let extents = extents();
    let _fallback = ac3_fallback::install(Fallback::new(Version::Jp));
    let mut runs: Vec<PathBuf> = std::fs::read_dir(root())
        .expect("capture root")
        .flatten()
        .map(|e| e.path().join("snapshots-module1.jsonl"))
        .filter(|p| p.is_file())
        .collect();
    runs.sort();
    let only: Option<u32> = std::env::var("ONLY")
        .ok()
        .and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok());
    let hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let (mut passed, mut failed, mut bytes) = (BTreeSet::new(), Vec::new(), 0usize);
    let mut device = 0;
    for path in &runs {
        let directory = path.parent().unwrap();
        let run = directory
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let index = snapshot::read_index(path).unwrap();
        for entry in index.iter().filter(|s| s.stage == "entry") {
            let callee = entry.callee;
            if !claimed.contains(&callee) || only.is_some_and(|a| a != callee) {
                continue;
            }
            let Some(exit) = exit_of(&index, entry) else {
                continue;
            };
            let is_void = void.contains(&callee);
            let e = match replay(directory, entry, exit, is_void) {
                Ok(n) => {
                    bytes += n;
                    passed.insert(callee);
                    println!(
                        "pass {run} #{} {callee:08x}: {n} bytes changed",
                        entry.sequence
                    );
                    continue;
                }
                Err(e) => e,
            };
            let proof = if e.starts_with("state differs") || e.starts_with("v0 differs") {
                device_work(directory, entry, exit, extents[&callee], is_void)
            } else {
                Err("not a state or v0 difference".into())
            };
            match proof {
                Ok(why) => {
                    device += 1;
                    passed.insert(callee);
                    println!(
                        "pass (device work) {run} #{} {callee:08x}: {e}; {why}",
                        entry.sequence
                    );
                }
                Err(not) => {
                    let text = format!(
                        "{run} #{} {callee:08x}: {e}; not device work: {not}",
                        entry.sequence
                    );
                    println!("FAIL {text}");
                    failed.push(text);
                }
            }
        }
    }
    panic::set_hook(hook);
    let uncaptured: Vec<String> = claimed
        .iter()
        .filter(|a| !passed.contains(a) && !failed.iter().any(|f| f.contains(&format!("{a:08x}"))))
        .map(|a| format!("{a:08x}"))
        .collect();
    println!(
        "captured: {} routines pass ({bytes} original bytes changed; {device} calls as device \
         work), {} failures; {} claimed routines have no capture: {}",
        passed.len(),
        failed.len(),
        uncaptured.len(),
        uncaptured.join(" ")
    );
    assert!(failed.is_empty(), "{failed:#?}");
    assert!(!passed.is_empty());
}

// ---- the oracle ------------------------------------------------------------------

/// A JP RAM dump of module 1 (`redux/<run>/ram-NNNNNN`), as a machine with
/// `sp` below the dump's stack pointer.
fn dump(run: &str, field: u32) -> Machine {
    let base = root().join(run).join(format!("ram-{field:06}"));
    let ram = std::fs::read(base.with_extension("ram.bin")).expect("ram");
    let scratch = std::fs::read(base.with_extension("scratch.bin")).expect("scratch");
    let regs: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(base.with_extension("regs.json")).unwrap())
            .unwrap();
    assert_eq!(regs["loaded"], 1, "{run} {field}: module 1 not loaded");
    let mut m = Machine::from_images(&ram, &scratch).expect("images");
    m.sp = (regs["gpr"][29].as_u64().unwrap() as u32).wrapping_sub(0x100) & !7;
    m
}

/// The module-1 dumps: title (1490), name entry (2710), difficulty
/// (4210), sound and screen settings (5640, 7140), saving (8110, 9210),
/// the log-out title and the download path (save-001).
const DUMPS: [(&str, u32); 12] = [
    ("boot-001", 1490),
    ("boot-001", 2110),
    ("boot-001", 2710),
    ("boot-001", 3310),
    ("boot-001", 4210),
    ("boot-001", 5640),
    ("boot-001", 7140),
    ("boot-001", 8110),
    ("boot-001", 9210),
    ("save-001", 56790),
    ("save-001", 57290),
    ("save-001", 57790),
];

const CONTEXT: u32 = 0x800b_ce60;
const MENU: u32 = 0x800d_d9d0;
const CARD: u32 = 0x800d_da48;
const POOLS: u32 = 0x800d_da18;
const DISPLAY: u32 = 0x800b_ce48;
/// `[gp+0x34]` (0x800BCBA8): the CD block (+0, +4 states, +0xC the disc).
const CD_BLOCK: u32 = 0x800b_cba8;
/// Free RAM between the arena limit (0x801FB700) and the stack window,
/// for test data.
const FREE: u32 = 0x801f_b800;

/// The original instructions and the translation from `base` must agree
/// ([`proof::compare`], with the oracle states' card file answers).
/// Ok(bytes the original changed outside the stack below the entry `sp`).
fn compare(base: &Machine, range: (u32, u32), args: [u32; 4], void: bool) -> Result<usize, String> {
    let device = || -> Box<dyn Device> { Box::new(Bios::with(card_file)) };
    let original = proof::compare(base, range, args, void, &device)?;
    let dead = snapshot::STACK_START..(base.sp & 0x1f_ffff) as usize;
    Ok((0..0x20_0000usize)
        .filter(|&i| !dead.contains(&i) && base.ram()[i] != original.machine.ram()[i])
        .count())
}

fn pick(rng: &mut oracle::Rng, values: &[u32]) -> u32 {
    values[rng.next() as usize % values.len()]
}
fn below(rng: &mut oracle::Rng, n: u32) -> u32 {
    rng.next() % n
}
/// Pad words: each button bit with probability 1/4.
fn pad(rng: &mut oracle::Rng) -> u32 {
    [1, 2, 4, 8, 0x10, 0x20, 0x40, 0x100, 0x200]
        .into_iter()
        .filter(|_| below(rng, 4) == 0)
        .fold(0, |v, b| v | b)
}
fn flip(m: &mut Machine, rng: &mut oracle::Rng, address: u32, bit: u32) {
    let v = m.lw(address);
    m.sw(
        address,
        if below(rng, 2) == 0 {
            v | bit
        } else {
            v & !bit
        },
    );
}

type Setup = fn(&mut Machine, &mut oracle::Rng) -> [u32; 4];

/// The dump as it is; a0 the menu block (the screen steps' argument).
fn none(m: &mut Machine, _: &mut oracle::Rng) -> [u32; 4] {
    [m.lw(MENU), 0, 0, 0]
}

/// The dialogs' inputs: the pad words (+0x128 held, +0x12C pressed,
/// +0x130 repeat), context flags (+0x964 bit 0, +0x96C bits 1/4, +0x124
/// bit 4), the account entry +0x38C; the menu block's slot +0x5A, file
/// +0x5D, entry +0x5F, +0x60, the counter +0x88, +0xD0..+0xD4; per slot
/// the card layer's probe flag +0x348, probe and file steps +0x2C/+0x30,
/// format flag +0x34A and pending flag +0x34C; the CD block (disc number
/// and states); the Download level and the sound-settings row.
fn dialog(m: &mut Machine, rng: &mut oracle::Rng) -> [u32; 4] {
    let ctx = m.lw(CONTEXT);
    for offset in [0x128, 0x12c, 0x130] {
        let v = pad(rng);
        m.sw(ctx + offset, v);
    }
    flip(m, rng, ctx + 0x964, 1);
    flip(m, rng, ctx + 0x96c, 0x10);
    flip(m, rng, ctx + 0x96c, 2);
    flip(m, rng, ctx + 0x124, 0x10);
    let entry = below(rng, 60);
    m.sh(ctx + 0x38c, entry as u16);
    let block = m.lw(MENU);
    for (offset, n) in [
        (0x5a, 2),
        (0x5d, 3),
        (0x5f, 9),
        (0x60, 3),
        (0xd0, 2),
        (0xd1, 2),
        (0xd2, 2),
        (0xd3, 3),
    ] {
        let v = below(rng, n);
        m.sb(block + offset, v as u8);
    }
    let v = pick(rng, &[u32::MAX, 0, 1, 0x40]);
    m.sw(block + 0x88, v);
    let v = below(rng, 2);
    m.sw(block + 0xd4, v);
    let card = m.lw(CARD);
    for slot in 0..2 {
        let v = below(rng, 2);
        m.sb(card + 0x348 + slot, v as u8);
        let v = pick(rng, &[0, 0, 1, 2, 2, 2, 3, 3, 5, 0xffff]);
        m.sh(card + 0x2c + 2 * slot, v as u16);
        let v = pick(rng, &[0, 0, 0, 1, 2, 3, 4, 0xffff]);
        m.sh(card + 0x30 + 2 * slot, v as u16);
        let v = below(rng, 3);
        m.sb(card + 0x34a + slot, v as u8);
        let v = pick(rng, &[0, 0, 1]);
        m.sb(card + 0x34c + slot, v as u8);
    }
    card_layer(m, rng, card);
    let cd = m.lw(CD_BLOCK);
    let v = pick(rng, &[0, 1, 2, 3, 4, 5]);
    m.sw(cd + 0xc, v);
    for offset in [0, 4] {
        let v = below(rng, 4);
        m.sw(cd + offset, v);
    }
    let v = pick(rng, &[0xffff, 0, 1, 2, 3]);
    m.sh(0x800d_d5f0, v as u16);
    let v = pick(rng, &[u32::MAX, 0, 1, 2, 3]);
    m.sw(0x800d_d4c0, v);
    let v = pick(rng, &[0, 7, 0x80, 0xffff_ff00, 0x8000_0003]) ^ below(rng, 0x40);
    m.sw(ctx + 0x20, v);
    for bit in [1, 2, 4] {
        flip(m, rng, ctx + 0x120, bit);
    }
    let v = below(rng, 2);
    m.sb(ctx + 0x95a, v as u8);
    let progress = ctx + 0x95c;
    for first in [0x2e2, 0x2e7, 0x2d2, 0x2f5, 0x2f0] {
        for k in 0..4 {
            let v = pick(rng, &[0, 0, 0, 1]);
            m.sb(progress + first + 0x3c * k, v as u8);
        }
    }
    // The cursor layer's state 1 (800DA898(1)) and the account dialog's
    // state (800DA4DC).
    let directory = m.lw(0x800d_d9dc);
    let cursors = m.lw(directory + 0x48);
    if in_ram(cursors) {
        let v = below(rng, 4);
        m.sh(cursors + 2, v as u16);
    }
    let account = m.lw(0x800d_d910);
    if in_ram(account) {
        let v = below(rng, 8);
        m.sw(account, v);
    }
    [block, 0, 0, 0]
}

fn in_ram(a: u32) -> bool {
    (0x8000_0000..0x8020_0000).contains(&a) && a & 3 == 0
}

/// The card layer's file state: the open-pending flag +0x336, the retry
/// counters +0x358/+0x35A, the save-file mask (header +8), the three save
/// buffers' and the record's entry masks (+0x16), the record's name (+8),
/// the kernel's statuses of its eight events, and the card file answers
/// ([`CARD_FILE`]).
fn card_layer(m: &mut Machine, rng: &mut oracle::Rng, card: u32) {
    let v = below(rng, 2);
    m.sb(card + 0x336, v as u8);
    let v = below(rng, 5);
    m.sh(card + 0x358, v as u16);
    let v = below(rng, 2);
    m.sh(card + 0x35a, v as u16);
    let header = m.lw(card + 0x2e8);
    if in_ram(header) {
        let v = below(rng, 8);
        m.sb(header + 8, v as u8);
    }
    for k in 0..4 {
        let buffer = m.lw(card + 0x2ec + 4 * k);
        if in_ram(buffer) {
            let v = rng.next();
            m.sh(buffer + 0x16, v as u16);
        }
    }
    let record = m.lw(card + 0x2f8);
    if in_ram(record) {
        let length = below(rng, 10);
        for k in 0..length {
            let c = pick(rng, &[0x20, 0x41, 0x4e, 0x65, 0x6f]);
            m.sb(record + 8 + k, c as u8);
        }
        m.sb(record + 8 + length, 0);
    }
    let table = m.lw(0x8000_0120);
    for k in 0..8 {
        let event = m.lw(card + 4 + 4 * k);
        let status = table.wrapping_add((event & 0xffff) * 0x1c).wrapping_add(4);
        if status & 0x1f_ffff < 0x1_0000 {
            let v = pick(rng, &[0x2000, 0x4000]);
            m.sw(status, v);
        }
    }
    let v = 0x8000_0000 | below(rng, 16);
    m.sw(CARD_FILE, v);
}

/// [`dialog`] with the screen's dialog state +0x4C/+0x4E set to a state
/// of the table with `states` entries, step 0.
fn screen_state(m: &mut Machine, rng: &mut oracle::Rng, states: u32) -> [u32; 4] {
    let args = dialog(m, rng);
    let block = m.lw(MENU);
    let state = below(rng, states);
    m.sh(block + 0x4c, state as u16);
    m.sh(block + 0x4e, 0);
    args
}
fn account_screen(m: &mut Machine, rng: &mut oracle::Rng) -> [u32; 4] {
    screen_state(m, rng, 20)
}
fn load_screen(m: &mut Machine, rng: &mut oracle::Rng) -> [u32; 4] {
    screen_state(m, rng, 12)
}

/// [`dialog`] with the link-mode menu's fields: the opening counter +0x5E,
/// the choice +0x59 and the card layer flag +0x18.
fn link(m: &mut Machine, rng: &mut oracle::Rng) -> [u32; 4] {
    let args = dialog(m, rng);
    let block = m.lw(MENU);
    let v = pick(rng, &[0, 5, 0x1d, 0x1e, 0x40]);
    m.sb(block + 0x5e, v as u8);
    let v = below(rng, 3);
    m.sb(block + 0x59, v as u8);
    let v = below(rng, 2);
    m.sw(block + 0x18, v);
    args
}

/// [`dialog`] with the keyboard cursor +0x5B (0..0x35) and page +0x5C.
fn keyboard(m: &mut Machine, rng: &mut oracle::Rng) -> [u32; 4] {
    let args = dialog(m, rng);
    let block = m.lw(MENU);
    let v = if below(rng, 2) == 0 {
        pick(rng, &[0xd, 0x1a, 0x26, 0x27, 0x31, 0x32, 0x33])
    } else {
        below(rng, 0x36)
    };
    m.sb(block + 0x5b, v as u8);
    let v = below(rng, 2);
    m.sb(block + 0x5c, v as u8);
    args
}

/// [`dialog`] with the settings: difficulty +0x394, levels +0x398/+0x399,
/// mono +0x39A, the screen offset (display +8/+0xC), the reset inputs
/// (+0x68, +0x60 bit 0x1000).
fn settings(m: &mut Machine, rng: &mut oracle::Rng) -> [u32; 4] {
    let args = dialog(m, rng);
    let ctx = m.lw(CONTEXT);
    let v = pick(rng, &[u32::MAX, 0, 1, 2, 3, 7]);
    m.sw(ctx + 0x394, v);
    for offset in [0x398, 0x399] {
        let v = pick(rng, &[0, 8, 0x10, 0x80, 0xf0, 0xff]);
        m.sb(ctx + offset, v as u8);
    }
    let v = below(rng, 4);
    m.sb(ctx + 0x39a, v as u8);
    let request = m.lw(DISPLAY);
    let v = pick(
        rng,
        &[0xffff_fff0, 0xffff_fff5, 0xffff_fff6, 0, 0xb, 0xc, 0xd],
    );
    m.sw(request + 8, v);
    let v = pick(rng, &[u32::MAX, 0, 1, 7, 8, 9]);
    m.sw(request + 0xc, v);
    let v = pick(rng, &[0, 0x80, 0x81, 0xff]);
    m.sh(ctx + 0x68, v as u16);
    flip(m, rng, ctx + 0x60, 0x1000);
    args
}

/// [`dialog`] with the frame's counters: idle +0x78 around 601, the exit
/// flag +0x58, the card layer +0x18.
fn frame(m: &mut Machine, rng: &mut oracle::Rng) -> [u32; 4] {
    let args = dialog(m, rng);
    let block = m.lw(MENU);
    let v = pick(rng, &[0, 0x258, 0x259, 0x300]);
    m.sw(block + 0x78, v);
    let v = below(rng, 2);
    m.sb(block + 0x58, v as u8);
    let v = below(rng, 2);
    m.sw(block + 0x18, v);
    args
}

/// The loading start with and without the log-out flag (+0x96C bit 1).
fn begin(m: &mut Machine, rng: &mut oracle::Rng) -> [u32; 4] {
    let ctx = m.lw(CONTEXT);
    flip(m, rng, ctx + 0x96c, 2);
    [m.lw(MENU), 0, 0, 0]
}

/// The loading screen's inputs, its drawing path only (the context's
/// loading flag +0x44 set, or log-out mode with time left): the end of
/// the load (800CCFA4) installs the sound bank, which on these states
/// does not finish (its capture validates it).
fn load_wait(m: &mut Machine, rng: &mut oracle::Rng) -> [u32; 4] {
    let ctx = m.lw(CONTEXT);
    let block = m.lw(MENU);
    let v = below(rng, 0x80);
    m.sw(block + 0x48, v + 1);
    let loading = below(rng, 2);
    m.sw(ctx + 0x44, loading);
    if loading == 0 {
        let v = m.lw(ctx + 0x96c);
        m.sw(ctx + 0x96c, v | 2);
        let v = m.lw(ctx + 0x12c);
        m.sw(ctx + 0x12c, v & !0x10);
    } else {
        flip(m, rng, ctx + 0x96c, 2);
    }
    [block, 0, 0, 0]
}

/// The rank bytes of the 55 records (context +0xC1C/+0xC58/+0xC94), some
/// above 5 (counted into the caller's frame, as the original does).
fn ranks(m: &mut Machine, rng: &mut oracle::Rng) -> [u32; 4] {
    let ctx = m.lw(CONTEXT);
    for record in 1..0x38 {
        for offset in [0xc1c, 0xc58, 0xc94] {
            let v = pick(rng, &[0, 0, 1, 2, 3, 4, 5, 6, 7]);
            m.sb(ctx + record + offset, v as u8);
        }
    }
    let block = m.lw(MENU);
    [block + 0x94, 0, 0, 0]
}

/// The pulsing tile: a frame counter and an ordering-table entry.
fn pulse(m: &mut Machine, rng: &mut oracle::Rng) -> [u32; 4] {
    let ctx = m.lw(CONTEXT);
    let v = rng.next() ^ (rng.next() << 8);
    m.sw(ctx + 0x20, v);
    let ot = m.lw(ctx + 8) + 4 * below(rng, 16);
    let block = m.lw(MENU);
    let packet = m.lw(block + 8);
    [ot, packet, 0, 0]
}

/// An offset table at [`FREE`] (count, then byte offsets, some negative
/// or unaligned) and the output after it.
fn offsets(m: &mut Machine, rng: &mut oracle::Rng) -> [u32; 4] {
    let count = pick(rng, &[0, 0xffff_ffff, 1, 3, 8]);
    let table = FREE + 0x10;
    m.sw(table, count);
    for k in 0..8 {
        let v = pick(rng, &[0, 4, 7, 0x40, 0xffff_fffc, 0xffff_fff9, 0x123]);
        m.sw(table + 4 + 4 * k, v);
    }
    m.sw(FREE, table);
    [FREE, FREE + 0x100, 0, 0]
}

/// The sprite pool record's selection inputs (buffer index, counter).
fn pool(m: &mut Machine, rng: &mut oracle::Rng) -> [u32; 4] {
    let ctx = m.lw(CONTEXT);
    let v = below(rng, 2);
    m.sw(ctx + 4, v);
    let pools = m.lw(POOLS);
    let v = rng.next();
    m.sw(pools + 0x20, v);
    [0; 4]
}

/// A text at [`FREE`] (letters, digits, spaces, newlines, characters out
/// of range), the colour and flags stack arguments, an ordering-table
/// entry; the pool's sprite and draw-mode pointers selected.
fn text(m: &mut Machine, rng: &mut oracle::Rng) -> [u32; 4] {
    let length = below(rng, 24);
    for k in 0..length {
        let c = pick(
            rng,
            &[
                0x41, 0x5a, 0x61, 0x7a, 0x30, 0x21, 0x20, 0xa, 0x7f, 0x80, 0x10, 0x4d,
            ],
        );
        m.sb(FREE + k, c as u8);
    }
    m.sb(FREE + length, 0);
    let pools = m.lw(POOLS);
    let index = below(rng, 2) * 4;
    let sprites = m.lw(pools + index);
    m.sw(pools + 8, sprites);
    let modes = m.lw(pools + 0xc + index);
    m.sw(pools + 0x14, modes);
    let v = rng.next() ^ pick(rng, &[0, 0x8000_0000]);
    m.sw(pools + 0x20, v);
    let sp = m.sp;
    for k in 0..3 {
        let v = rng.next();
        m.sb(sp + 0x10 + k, v as u8);
    }
    let flags = below(rng, 16);
    m.sw(sp + 0x14, flags);
    let ctx = m.lw(CONTEXT);
    let ot = m.lw(ctx + 8) + 4 * below(rng, 16);
    [below(rng, 0x140), below(rng, 0xf0), FREE, ot]
}

/// The small allocator's words and a size.
fn heap(m: &mut Machine, rng: &mut oracle::Rng) -> [u32; 4] {
    for k in 0..4 {
        let v = 0x801f_0000 + (rng.next() & 0xfffc);
        m.sw(0x800d_da20 + 4 * k, v);
    }
    [pick(rng, &[0, 1, 3, 4, 0x81, 0xffff_fffd]), 0, 0, 0]
}

/// [`dialog`] with the kernel's event statuses of the card layer's eight
/// events (ready or not) and (slot, value) arguments.
fn events(m: &mut Machine, rng: &mut oracle::Rng) -> [u32; 4] {
    dialog(m, rng);
    let table = m.lw(0x8000_0120);
    let card = m.lw(CARD);
    for k in 0..8 {
        let event = m.lw(card + 4 + 4 * k);
        let status = table.wrapping_add((event & 0xffff) * 0x1c).wrapping_add(4);
        if status & 0x1f_ffff < 0x1_0000 {
            let v = pick(rng, &[0x2000, 0x4000]);
            m.sw(status, v);
        }
    }
    [below(rng, 2), below(rng, 0x100), 0, 0]
}

/// [`dialog`] with a slot argument.
fn slot(m: &mut Machine, rng: &mut oracle::Rng) -> [u32; 4] {
    dialog(m, rng);
    [below(rng, 2), 0, 0, 0]
}
/// [`dialog`] with a save-buffer index (0..2).
fn buffer(m: &mut Machine, rng: &mut oracle::Rng) -> [u32; 4] {
    dialog(m, rng);
    [below(rng, 3), 0, 0, 0]
}
/// [`dialog`] with the record's flags (+0x464, +0x468) varied; a0 the
/// menu block's +0x5F.
fn record(m: &mut Machine, rng: &mut oracle::Rng) -> [u32; 4] {
    dialog(m, rng);
    let card = m.lw(CARD);
    let record = m.lw(card + 0x2f8);
    let v = below(rng, 2);
    m.sb(record + 0x464, v as u8);
    let v = below(rng, 16);
    m.sw(record + 0x468, v);
    let v = below(rng, 4);
    m.sb(record + 0x45c, v as u8);
    let block = m.lw(MENU);
    [block + 0x5f, 0, 0, 0]
}
/// [`dialog`] with the progress bytes 800D2BA0 reads.
fn progress(m: &mut Machine, rng: &mut oracle::Rng) -> [u32; 4] {
    let args = dialog(m, rng);
    let progress = m.lw(CONTEXT) + 0x95c;
    for first in [0x2e2, 0x2e7, 0x2d2, 0x2f5, 0x2f0] {
        for k in 0..4 {
            let v = pick(rng, &[0, 0, 0, 1]);
            m.sb(progress + first + 0x3c * k, v as u8);
        }
    }
    args
}
/// [`dialog`] with (file, entry) arguments.
fn info(m: &mut Machine, rng: &mut oracle::Rng) -> [u32; 4] {
    dialog(m, rng);
    [below(rng, 3), below(rng, 9), 0, 0]
}
/// [`dialog`] for the account entry choice: the cursor layer free
/// (800DA898(1) = 2) half the time, the entries around the new file (6)
/// and the simulation (7).
fn entries(m: &mut Machine, rng: &mut oracle::Rng) -> [u32; 4] {
    let args = dialog(m, rng);
    let directory = m.lw(0x800d_d9dc);
    let cursors = m.lw(directory + 0x48);
    if in_ram(cursors) && below(rng, 2) == 0 {
        m.sh(cursors + 2, 2);
    }
    let block = m.lw(MENU);
    let v = pick(rng, &[0, 1, 4, 5, 6, 7, 8]);
    m.sb(block + 0x5f, v as u8);
    args
}

/// [`dialog`] with the account dialog routine index +0xE0.
fn account(m: &mut Machine, rng: &mut oracle::Rng) -> [u32; 4] {
    let args = dialog(m, rng);
    let block = m.lw(MENU);
    let v = below(rng, 15);
    m.sw(block + 0xe0, v);
    args
}

/// The cases: (routine, states per dump, setup).
fn cases() -> Vec<(u32, usize, Setup)> {
    let mut out: Vec<(u32, usize, Setup)> = vec![
        // ov1_p1.rs
        (0x800c_c5d8, 1, none),
        (0x800c_c764, 8, frame),
        (0x800c_c938, 8, ranks),
        (0x800c_cb38, 8, pulse),
        (0x800c_cc90, 1, none),
        (0x800c_cd48, 8, dialog),
        (0x800c_ce38, 8, offsets),
        (0x800c_ce98, 2, begin),
        (0x800c_d07c, 8, load_wait),
        (0x800c_d1a0, 1, none),
        (0x800c_d2f0, 16, text),
        (0x800c_d8c8, 4, pool),
        (0x800c_d914, 4, heap),
        (0x800c_d930, 4, heap),
        (0x800c_d968, 4, heap),
        (0x800c_d978, 4, heap),
        // ov1_p1_card.rs
        (0x800d_3ce8, 1, none),
        (0x800d_3d60, 6, dialog),
        (0x800d_3d90, 12, slot),
        (0x800d_3ea8, 8, events),
        (0x800d_3f88, 4, events),
        (0x800d_3ff4, 4, events),
        (0x800d_4060, 8, events),
        (0x800d_4140, 1, none),
        (0x800d_4158, 1, none),
        (0x800d_4190, 1, none),
        (0x800d_41c8, 6, dialog),
        (0x800d_4658, 1, none),
        (0x800d_47b8, 4, buffer),
        (0x800d_4808, 6, record),
        // ov1_p1_screens.rs
        (0x800c_d9a8, 4, link),
        (0x800c_db64, 48, link),
        (0x800c_e24c, 4, dialog),
        (0x800c_e290, 12, account),
        (0x800c_e3f4, 1, none),
        (0x800c_e518, 1, none),
        (0x800c_e5ac, 64, keyboard),
        (0x800c_eb20, 1, none),
        (0x800c_eb90, 8, dialog),
        (0x800c_ec70, 1, none),
        (0x800c_ecf8, 12, settings),
        (0x800c_eecc, 1, none),
        (0x800c_ef44, 8, dialog),
        (0x800c_f004, 1, none),
        // ov1_p1_settings.rs
        (0x800c_f06c, 16, settings),
        (0x800c_f750, 1, none),
        (0x800c_f7b4, 16, settings),
        (0x800c_fa34, 1, none),
        (0x800c_fa94, 8, settings),
        (0x800c_fb2c, 1, none),
        (0x800c_fbd0, 8, dialog),
        // ov1_p1_load.rs
        (0x800d_1798, 1, none),
        (0x800d_181c, 12, account_screen),
        (0x800d_2ba0, 12, progress),
        (0x800d_2c84, 12, info),
        (0x800d_3b3c, 12, load_screen),
        (0x800d_2fb8, 48, entries),
    ];
    // The dialog steps of ov1_p1_save.rs and ov1_p1_load.rs, on the
    // dialog inputs.
    for address in [
        0x800c_fc38,
        0x800c_fc94,
        0x800c_fd28,
        0x800c_fd60,
        0x800c_fdcc,
        0x800c_fea4,
        0x800c_ff90,
        0x800d_0074,
        0x800d_02dc,
        0x800d_0384,
        0x800d_0460,
        0x800d_0548,
        0x800d_061c,
        0x800d_0668,
        0x800d_06d4,
        0x800d_076c,
        0x800d_0848,
        0x800d_08dc,
        0x800d_0978,
        0x800d_0a10,
        0x800d_0aec,
        0x800d_0b0c,
        0x800d_0bfc,
        0x800d_0cec,
        0x800d_0dc8,
        0x800d_0e54,
        0x800d_1080,
        0x800d_1108,
        0x800d_12a4,
        0x800d_1300,
        0x800d_13b8,
        0x800d_1488,
        0x800d_1554,
        0x800d_15c4,
        0x800d_1618,
        0x800d_16d4,
        0x800d_1738,
        0x800d_1a5c,
        0x800d_1adc,
        0x800d_1b2c,
        0x800d_1c18,
        0x800d_1cf0,
        0x800d_1dcc,
        0x800d_202c,
        0x800d_20bc,
        0x800d_2154,
        0x800d_21d0,
        0x800d_2268,
        0x800d_22e4,
        0x800d_237c,
        0x800d_23cc,
        0x800d_248c,
        0x800d_255c,
        0x800d_2600,
        0x800d_26f0,
        0x800d_2954,
        0x800d_2994,
        0x800d_29fc,
        0x800d_2ae8,
        0x800d_2e74,
        0x800d_34fc,
        0x800d_3584,
        0x800d_37fc,
        0x800d_3840,
        0x800d_3a24,
        0x800d_3a8c,
        0x800d_3ae8,
    ] {
        out.push((address, 16, dialog));
    }
    out
}

/// The machine under the fallback's interpreter, recording the addresses
/// it executes inside one routine (instruction coverage of the original
/// on the oracle states).
struct Traced<'a> {
    g: MachineGuest<'a>,
    range: (u32, u32),
    seen: &'a RefCell<BTreeSet<u32>>,
}
impl Guest for Traced<'_> {
    fn fetch(&self, pc: u32) -> u32 {
        if (self.range.0..self.range.1).contains(&pc) {
            self.seen.borrow_mut().insert(pc);
        }
        self.g.fetch(pc)
    }
    fn lb(&mut self, address: u32) -> i8 {
        Guest::lb(&mut self.g, address)
    }
    fn lbu(&mut self, address: u32) -> u8 {
        Guest::lbu(&mut self.g, address)
    }
    fn lh(&mut self, address: u32) -> i16 {
        Guest::lh(&mut self.g, address)
    }
    fn lhu(&mut self, address: u32) -> u16 {
        Guest::lhu(&mut self.g, address)
    }
    fn lw(&mut self, address: u32) -> u32 {
        Guest::lw(&mut self.g, address)
    }
    fn sb(&mut self, address: u32, value: u8) {
        Guest::sb(&mut self.g, address, value);
    }
    fn sh(&mut self, address: u32, value: u16) {
        Guest::sh(&mut self.g, address, value);
    }
    fn sw(&mut self, address: u32, value: u32) {
        Guest::sw(&mut self.g, address, value);
    }
    fn gte(&mut self) -> &mut ac3_gte::Gte {
        Guest::gte(&mut self.g)
    }
    fn sp(&self) -> u32 {
        Guest::sp(&self.g)
    }
    fn set_sp(&mut self, sp: u32) {
        Guest::set_sp(&mut self.g, sp);
    }
    fn loaded_module(&mut self) -> u32 {
        Guest::loaded_module(&mut self.g)
    }
    fn faulted(&self) -> bool {
        Guest::faulted(&self.g)
    }
    fn raise_trap(&mut self, address: u32, code: u32) {
        Guest::raise_trap(&mut self.g, address, code);
    }
    fn raise_untranslated(&mut self, address: u32) {
        Guest::raise_untranslated(&mut self.g, address);
    }
    fn raise_unsupported(&mut self, address: u32, word: u32) {
        Guest::raise_unsupported(&mut self.g, address, word);
    }
    fn raise_runaway(&mut self, address: u32) {
        Guest::raise_runaway(&mut self.g, address);
    }
    fn host_stub(&mut self, target: u32, id: u32, args: [u32; 4]) -> Option<u32> {
        Guest::host_stub(&mut self.g, target, id, args)
    }
    fn busy_wait(&mut self, site: u32, cycles: u32) -> bool {
        Guest::busy_wait(&mut self.g, site, cycles)
    }
}

/// Interprets the original routine once more on `base`, adding the
/// addresses it executes to `seen`.
fn trace(
    base: &Machine,
    range: (u32, u32),
    args: [u32; 4],
    fallback: &Fallback,
    seen: &RefCell<BTreeSet<u32>>,
) {
    let mut m = base.clone();
    m.device = Some(Box::new(Bios::with(card_file)));
    let mut traced = Traced {
        g: MachineGuest(&mut m),
        range,
        seen,
    };
    let mut call =
        |g: &mut Traced<'_>, target: u32, args: [u32; 4]| dispatch::call(g.g.0, target, args);
    let _ = panic::catch_unwind(AssertUnwindSafe(|| {
        fallback.run_range(&mut traced, range, args, &mut call)
    }));
}

/// The instructions of `range` not in `seen`, as address ranges.
fn gaps(range: (u32, u32), seen: &BTreeSet<u32>) -> Vec<String> {
    let mut out: Vec<(u32, u32)> = Vec::new();
    for pc in (range.0..range.1)
        .step_by(4)
        .filter(|pc| !seen.contains(pc))
    {
        match out.last_mut() {
            Some((_, end)) if *end + 4 == pc => *end = pc,
            _ => out.push((pc, pc)),
        }
    }
    out.iter()
        .map(|(a, b)| {
            if a == b {
                format!("{a:08x}")
            } else {
                format!("{a:08x}-{b:08x}")
            }
        })
        .collect()
}

#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux RAM dumps)"]
fn oracle_cases() {
    let fallback = ac3_fallback::install(Fallback::new(Version::Jp));
    let (claimed, void) = claims();
    let extents = extents();
    let states: Vec<(String, Machine)> = DUMPS
        .iter()
        .map(|(run, field)| (format!("{run}/{field}"), dump(run, *field)))
        .collect();
    let only: Option<u32> = std::env::var("ONLY")
        .ok()
        .and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok());
    let cases = cases();
    let listed: BTreeSet<u32> = cases.iter().map(|c| c.0).collect();
    let missing: Vec<String> = claimed
        .iter()
        .filter(|a| !listed.contains(a))
        .map(|a| format!("{a:08x}"))
        .collect();
    let hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let (mut total, mut failures) = (0, Vec::new());
    let (mut covered, mut instructions) = (0, 0);
    for (address, variants, setup) in cases {
        assert!(claimed.contains(&address), "{address:08x} is not claimed");
        if only.is_some_and(|a| a != address) {
            continue;
        }
        let range = (address, extents[&address]);
        let seen = RefCell::new(BTreeSet::new());
        let mut rng = oracle::Rng(address);
        let (mut n, mut changed, mut failed) = (0, 0, 0);
        for (name, state) in &states {
            for v in 0..variants {
                let mut base = state.clone();
                let args = setup(&mut base, &mut rng);
                match compare(&base, range, args, void.contains(&address)) {
                    Ok(bytes) => {
                        changed += bytes;
                        trace(&base, range, args, &fallback, &seen);
                    }
                    Err(e) => {
                        failed += 1;
                        if failed <= 3 {
                            failures.push(format!("{address:08x} {name} #{v} {args:x?}: {e}"));
                        }
                    }
                }
                n += 1;
            }
        }
        let seen = seen.into_inner();
        let size = (range.1 - range.0) / 4;
        covered += seen.len();
        instructions += size as usize;
        let open = gaps(range, &seen);
        println!(
            "oracle {address:08x}: {n} states, {failed} failed, {changed} bytes changed by the \
             original; {}/{size} instructions executed{}{}",
            seen.len(),
            if open.is_empty() {
                ""
            } else {
                "; not reached: "
            },
            open.join(" ")
        );
        total += n;
    }
    panic::set_hook(hook);
    println!(
        "oracle: {total} states compared, {covered}/{instructions} instructions executed; \
         claimed routines without a case: {}",
        missing.join(" ")
    );
    for f in &failures {
        println!("FAIL {f}");
    }
    assert!(failures.is_empty(), "{} failing cases", failures.len());
}
