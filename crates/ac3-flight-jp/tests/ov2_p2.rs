//! Validation of JP overlay 2, part 2 (800D7DB0..800E14D0, `src/jp/ov2_p2*.rs`)
//! against the JP originals.
//!
//! * [`captured_calls_reproduce_the_original`]: every captured call of a
//!   routine of the part (`~/ac3-jp/analysis/redux/{story,flight,save}-002/
//!   snapshots-module2.jsonl`) runs natively from its entry snapshot and must
//!   reproduce the return snapshot (`snapshot::check_call`: RAM outside the
//!   stack, scratchpad, GTE, and `v0` where the translation models it).
//! * [`routines_match_the_oracle`]: every routine of the part runs on JP
//!   overlay-2 RAM images (Redux RAM dumps of the hub: `story-001`,
//!   `flight-001`, `save-001`) with randomised arguments and fields, against
//!   the original instructions (the test-only oracle, `support::oracle`'s
//!   interpreter, here with coverage and `jr ra; nop` stubs, see [`trace`]):
//!   equal RAM outside the stack window, scratchpad, GTE, `sp`, first fault
//!   and `v0`. A case in which the original stops at a compiler trap
//!   (`break`, a zero divisor) must raise the same `Fault::Trap`.
//!
//! Untranslated callees (other parts of overlay 2, resident routines nobody
//! translated) run in the development interpreter fallback in both runs, so
//! the file needs `--features dev-fallback`:
//! `cargo test --release -p ac3-flight-jp --features dev-fallback --test ov2_p2 -- --include-ignored --nocapture`.
#![cfg(feature = "dev-fallback")]
mod support;

use ac3_fallback::{Fallback, Version};
use ac3_flight_jp::jp::{ov2_p2, ov2_p2_draw, ov2_p2_hub, ov2_p2_records};
use ac3_flight_jp::{Fault, Machine, common, dispatch, snapshot};
use std::collections::BTreeMap;
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;

use support::scan;

/// Deterministic case generator (xorshift32, 24-bit outputs). The
/// oracle's LCG gives low bits with short periods, which made some of the
/// generators' choices degenerate.
struct Rng(u32);
impl Rng {
    fn next(&mut self) -> u32 {
        let mut x = self.0.max(1);
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x >> 8
    }
}

/// The fallback for this thread while the guard lives.
struct Fallbacks;
impl Fallbacks {
    fn install() -> Self {
        ac3_fallback::install(Fallback::new(Version::Jp));
        Self
    }
}
impl Drop for Fallbacks {
    fn drop(&mut self) {
        ac3_fallback::uninstall();
    }
}

/// Routines of the part whose translation leaves `v0` unmodelled (dispatch
/// arms `{ ...; 0 }`, as `scan` reads them; the original's `v0` is whatever
/// its last computation left and no caller reads it): `v0` is not
/// compared.
fn void_routines() -> std::collections::BTreeSet<u32> {
    scan::claims_and_void(&scan::crate_dir(), &|name| !name.starts_with("jp/ov2_p2")).1
}
const WORK: u32 = ov2_p2::WORK;
const VISUAL: u32 = ov2_p2::VISUAL;

fn redux() -> PathBuf {
    scan::jp_dir().join("analysis/redux")
}

/// The module-2 snapshot indexes.
fn indexes() -> Vec<PathBuf> {
    ["story-002", "flight-002", "save-002"]
        .iter()
        .map(|run| redux().join(run).join("snapshots-module2.jsonl"))
        .filter(|p| p.exists())
        .collect()
}

/// The part's claims (the patch-layer files `jp/ov2_p2*.rs`).
fn claimed() -> Vec<u32> {
    scan::jp_overlay_claims()
        .into_iter()
        .filter(|((module, _), sites)| {
            *module == 2 && sites.iter().any(|(f, _)| f.starts_with("jp/ov2_p2"))
        })
        .map(|((_, address), _)| address)
        .collect()
}

/// How a captured call was reproduced.
enum Replay {
    /// The return snapshot exactly: bytes the original changed.
    Exact(usize),
    /// The call reached devices (host calls) and the return snapshot differs
    /// in state the devices and interrupt handlers own (kernel RAM, libetc's
    /// VBlank counter and handler state, libpad's VBlank flag, libgpu's
    /// queue, data a device read returns), which the test runs without:
    /// the original's instructions (the oracle) from the same captured
    /// entry state then match the translation exactly. Bytes that differ
    /// from the capture.
    Devices(usize),
}

/// Replay captured call `occurrence` of `address` (as `snapshot::check_call`).
fn replay(
    index: &std::path::Path,
    address: u32,
    occurrence: usize,
    void: bool,
    end: u32,
) -> Result<Replay, String> {
    let directory = index.parent().unwrap();
    let entries = snapshot::read_index(index).map_err(|e| e.to_string())?;
    let entry = entries
        .iter()
        .filter(|s| s.callee == address && s.stage == "entry")
        .nth(occurrence)
        .ok_or("no entry")?;
    let exit = entries
        .iter()
        .find(|s| {
            s.callee == address
                && s.stage == "return"
                && s.sequence > entry.sequence
                && s.ra == entry.ra
                && s.registers.gpr[29] == entry.registers.gpr[29]
        })
        .ok_or("no return snapshot")?;
    let entry = snapshot::Snapshot::load(directory, entry).map_err(|e| e.to_string())?;
    let exit = snapshot::Snapshot::load(directory, exit).map_err(|e| e.to_string())?;
    let mut m = entry.machine();
    let args = [entry.gpr(4), entry.gpr(5), entry.gpr(6), entry.gpr(7)];
    let v0 = dispatch::call(&mut m, address, args);
    m.take_fault().map_err(|f| f.to_string())?;
    let d = snapshot::compare(&m, &exit);
    if d.is_empty() && (void || v0 == exit.gpr(2)) {
        return Ok(Replay::Exact(snapshot::changed_bytes(&entry, &exit)));
    }
    if m.host_calls.is_empty() {
        return Err(if d.is_empty() {
            format!("v0 {v0:#010x}, original {:#010x}", exit.gpr(2))
        } else {
            d.summary()
        });
    }
    let base = entry.machine();
    let native = |m: &mut Machine| dispatch::call(m, address, args);
    let mut seen = std::collections::BTreeSet::new();
    match compare(&base, (address, end), args, void, &native, &mut seen) {
        Ok(Outcome::Match(_)) => Ok(Replay::Devices(d.ram.len())),
        Ok(Outcome::Trap) => Err("the original stops at a trap".into()),
        Ok(Outcome::Wait) => Err("the original waits for a device".into()),
        Err(e) => Err(format!(
            "differs from the capture ({}) and from the oracle: {e}",
            d.summary()
        )),
    }
}

#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn captured_calls_reproduce_the_original() {
    let _fallback = Fallbacks::install();
    let indexes = indexes();
    assert!(!indexes.is_empty(), "no module-2 captures");
    let claimed = claimed();
    let extents = extents();
    let voids = void_routines();
    let mut failures = Vec::new();
    let (mut exact, mut devices, mut bytes) = (0, 0, 0);
    let mut seen: BTreeMap<u32, usize> = BTreeMap::new();
    for index in &indexes {
        let entries = snapshot::read_index(index).unwrap();
        for &address in &claimed {
            let count = entries
                .iter()
                .filter(|s| s.callee == address && s.stage == "entry")
                .count();
            for occurrence in 0..count {
                let void = voids.contains(&address);
                let run = index
                    .parent()
                    .unwrap()
                    .file_name()
                    .unwrap()
                    .to_string_lossy();
                match replay(index, address, occurrence, void, extents[&address]) {
                    Ok(Replay::Exact(n)) => {
                        exact += 1;
                        bytes += n;
                        *seen.entry(address).or_default() += 1;
                        let note = match (n, void) {
                            (0, true) => " (vacuous: nothing changed, v0 not modelled)",
                            (0, false) => " (v0 compared)",
                            _ => "",
                        };
                        println!(
                            "{address:08x} {run} #{occurrence}: exact, {n} bytes changed{note}"
                        );
                    }
                    Ok(Replay::Devices(n)) => {
                        devices += 1;
                        *seen.entry(address).or_default() += 1;
                        println!(
                            "{address:08x} {run} #{occurrence}: reaches devices; {n} device/interrupt-owned \
                             bytes differ from the capture; the oracle from the captured entry matches"
                        );
                    }
                    Err(e) => failures.push(format!("{run}: {address:08x}: {e}")),
                }
            }
        }
    }
    println!(
        "captures: {} calls of {} routines: {exact} exact ({bytes} bytes changed by the originals), \
         {devices} through the oracle from the captured entry; {} claimed routines",
        exact + devices,
        seen.len(),
        claimed.len()
    );
    assert!(failures.is_empty(), "{failures:#?}");
    assert!(exact > 0);
}
/// JP RAM images with module 2 loaded: Redux field dumps of the hub.
fn states() -> Vec<(String, Machine)> {
    let mut out = Vec::new();
    for (run, fields) in [
        (
            "story-001",
            &[17800, 30300, 38710, 40610, 41610, 46300, 49510][..],
        ),
        ("flight-001", &[53900][..]),
        ("save-001", &[54010, 54490, 55190][..]),
    ] {
        for field in fields {
            let path = redux().join(run).join(format!("ram-{field:06}.ram.bin"));
            let Ok(ram) = std::fs::read(&path) else {
                continue;
            };
            let scratch = std::fs::read(path.with_extension("").with_extension("scratch.bin"))
                .unwrap_or_else(|_| vec![0; 0x400]);
            let Some(mut m) = Machine::from_images(&ram, &scratch) else {
                continue;
            };
            m.sp = 0x801f_ff00;
            if common::loaded_module(&mut m) == 2 {
                out.push((format!("{run}/{field}"), m));
            }
        }
    }
    out
}

/// The extent of every overlay-2 routine (JP map).
fn extents() -> BTreeMap<u32, u32> {
    scan::jp_map(&scan::jp_dir())
        .into_iter()
        .filter(|r| r.region == "ov2")
        .map(|r| (r.jp, r.jp + r.size))
        .collect()
}

// ---- case generators ---------------------------------------------------------------------------

/// Words of a pointer table, while they point into RAM.
fn in_ram(value: u32) -> bool {
    (0x8000_0000..0x801f_0000).contains(&value) && value.is_multiple_of(4)
}

/// Display-list command bodies of `opcode` (the hub's lists: visual block
/// +0x160 + 4 * list, pages, commands until -1), as the interpreter
/// 800E22E4 walks them.
fn commands(m: &mut Machine, opcode: u32) -> Vec<u32> {
    const SIZES: [u32; 7] = [0x14, 0x1c, 0xc, 0x20, 0x10, 0x10, 0x14];
    let visual = m.lw(VISUAL);
    let mut out = Vec::new();
    for list in 0..4 {
        let table = m.lw(visual + 0x160 + 4 * list);
        if !in_ram(table) {
            continue;
        }
        for page in 0..32 {
            let mut command = m.lw(table + 4 * page);
            if !in_ram(command) {
                continue;
            }
            for _ in 0..500 {
                let op = m.lw(command);
                if op as usize >= SIZES.len() {
                    break;
                }
                if op == opcode {
                    out.push(command + 4);
                }
                command += 4 + SIZES[op as usize];
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

fn pick(rng: &mut Rng, list: &[u32], otherwise: u32) -> u32 {
    if list.is_empty() {
        otherwise
    } else {
        list[rng.next() as usize % list.len()]
    }
}

/// A random cell record (visual block +0x16C lists, 12-byte cells).
fn cell(m: &mut Machine, rng: &mut Rng) -> u32 {
    let visual = m.lw(VISUAL);
    let records = m.lw(visual + 0x16c + 4 * (rng.next() % 4));
    if in_ram(records) {
        records + 12 * (rng.next() % 0x12)
    } else {
        0x801e_0000
    }
}

/// Random fade slots: mode 0..4, level and target 0..`range`.
fn random_fades(m: &mut Machine, rng: &mut Rng, range: u32) {
    let block = m.lw(WORK);
    for (field, limit) in [(0x58, 4), (0x50, range), (0x54, range)] {
        let array = m.lw(block.wrapping_add(field));
        for slot in 0..0x10 {
            m.sh(array.wrapping_add(2 * slot), (rng.next() % limit) as u16);
        }
    }
}

/// Random flag bytes (0..3).
fn random_flags(m: &mut Machine, rng: &mut Rng) {
    let block = m.lw(WORK);
    let flags = m.lw(block.wrapping_add(0x5c));
    for index in 0..0x40 {
        m.sb(flags.wrapping_add(index), (rng.next() % 3) as u8);
    }
}

/// A current animation record of the lists (opcode 5) or the work block's,
/// with random fades, its slot's mode mostly 1 and its time around its
/// start..end window, and a random current rectangle.
fn random_step(m: &mut Machine, rng: &mut Rng) {
    let records = commands(m, 5);
    let block = m.lw(WORK);
    if !rng.next().is_multiple_of(4) {
        let record = pick(rng, &records, block + 0x40);
        m.sw(block + 0x3c, record);
    }
    random_fades(m, rng, 0x40);
    let record = m.lw(block + 0x3c);
    let slot = (m.lh(record + 0xc) as u32) & 0xf;
    let start = m.lh(record + 4) as i32;
    let end = m.lh(record + 8) as i32;
    let span = (end - start).clamp(0, 0x100) as u32 + 8;
    let time = start - 4 + (rng.next() % span) as i32;
    let modes = m.lw(block + 0x58);
    let levels = m.lw(block + 0x50);
    m.sh(
        modes + 2 * slot,
        [0, 1, 1, 1, 1, 2, 3][rng.next() as usize % 7],
    );
    m.sh(levels + 2 * slot, time as u16);
    m.sh(record + 0xc, slot as u16);
    let cur = m.lw(ov2_p2_draw::CURRENT);
    for i in 0..4 {
        m.sh(cur + 2 * i, (rng.next() % 0x200) as u16);
    }
}

/// A handler case: a command of `opcode`, the visual ordering table, a
/// cell, and the handler table entry as the interpreter passes them.
fn handler(m: &mut Machine, rng: &mut Rng, opcode: u32) -> [u32; 4] {
    random_step(m, rng);
    if rng.next().is_multiple_of(2) {
        random_flags(m, rng);
    }
    let list = commands(m, opcode);
    let mut command = pick(rng, &list, 0x801e_0000);
    if (opcode == 1 || opcode == 3) && rng.next().is_multiple_of(2) {
        // a synthesized command: random texture position, signed sizes,
        // texture depth 0..3
        let at = 0x801e_0100;
        for n in 0..0x20 {
            m.sb(at + n, rng.next() as u8);
        }
        for half in [0xc, 0xe, 0x10, 0x12, 0x14, 0x16] {
            m.sh(at + half, (rng.next() % 0x200).wrapping_sub(0x100) as u16);
        }
        m.sh(at + 0x18, 0);
        if opcode == 1 {
            m.sh(at + 0x14, 0);
        }
        m.sb(at + 0x1e, (rng.next() % 4) as u8);
        command = at;
    }
    let visual = m.lw(VISUAL);
    let ot = m.lw(visual + 8);
    let cell = cell(m, rng);
    [command, ot, cell, ov2_p2_draw::HANDLERS + 4 * opcode]
}

/// For a routine without a map start: the oracle's range (start, end)
/// and the translation to compare.
type Unclaimed = (u32, u32, fn(&mut Machine, [u32; 4]) -> u32);

/// How a routine's cases are drawn: the state's fields, then the arguments.
struct Plan {
    address: u32,
    prepare: fn(&mut Machine, &mut Rng) -> [u32; 4],
    /// For a routine without a map start: the oracle's range and the
    /// native translation (not claimed, so not reached by dispatch).
    unclaimed: Option<Unclaimed>,
    /// Cases per state: VARIANTS times this.
    scale: u32,
}

fn small(_: &mut Machine, rng: &mut Rng) -> [u32; 4] {
    std::array::from_fn(|_| rng.next() % 0x20)
}
/// List (0..4) and cell (0..0x14) indexes, small signed values, and random
/// fifth and sixth arguments in the caller's frame.
fn cells(m: &mut Machine, rng: &mut Rng) -> [u32; 4] {
    let sp = m.sp;
    m.sw(sp + 0x10, rng.next());
    m.sw(sp + 0x14, rng.next());
    [
        rng.next() % 4,
        rng.next() % 0x14,
        (rng.next() % 0x400).wrapping_sub(0x200),
        (rng.next() % 0x400).wrapping_sub(0x200),
    ]
}
/// Fade slot, mode and signed level/target (negative: unchanged).
fn fades(_: &mut Machine, rng: &mut Rng) -> [u32; 4] {
    [
        rng.next() % 0x10,
        rng.next() % 4,
        (rng.next() % 0x40).wrapping_sub(0x10),
        (rng.next() % 0x40).wrapping_sub(0x10),
    ]
}
fn slot(m: &mut Machine, rng: &mut Rng) -> [u32; 4] {
    random_fades(m, rng, 8);
    [rng.next() % 0x10, 0, 0, 0]
}
fn step(m: &mut Machine, rng: &mut Rng) -> [u32; 4] {
    random_step(m, rng);
    small(m, rng)
}

fn plans() -> Vec<Plan> {
    let p = |address, prepare: fn(&mut Machine, &mut Rng) -> [u32; 4]| Plan {
        address,
        prepare,
        unclaimed: None,
        scale: 1,
    };
    // the routines with many paths get more cases
    let p6 = |address, prepare: fn(&mut Machine, &mut Rng) -> [u32; 4]| Plan {
        address,
        prepare,
        unclaimed: None,
        scale: 6,
    };
    vec![
        // ov2_p2.rs
        p(0x800d_e79c, small),
        p(0x800d_e8f0, small),
        p(0x800d_e908, |_, r| [r.next(), r.next(), r.next(), 0]),
        p(0x800d_e924, small),
        p(0x800d_eb5c, cells),
        p(0x800d_ebb8, |_, r| [r.next() % 4, r.next() % 0x16, 0, 0]),
        p(0x800d_eca8, cells),
        p(0x800d_ecd4, cells),
        p(0x800d_ed38, cells),
        p(0x800d_ed68, cells),
        p(0x800d_eda0, |_, r| {
            [r.next() % 4, r.next() % 0x14, 0x801e_0000, 0]
        }),
        p(0x800d_edf0, |_, r| [r.next() % 0x10, r.next(), 0, 0]),
        p(0x800d_ee08, fades),
        p(0x800d_ee60, |_, r| [r.next() % 0x40, 0, 0, 0]),
        p(0x800d_ee84, slot),
        p(0x800d_eea8, slot),
        p(0x800d_eecc, |m, r| {
            random_fades(m, r, 8);
            [0; 4]
        }),
        p(0x800d_ef94, |_, r| [r.next() % 0x40, r.next(), 0, 0]),
        p(0x800d_efb4, small),
        // ov2_p2_draw.rs
        Plan {
            address: 0x800d_efec,
            prepare: step,
            unclaimed: Some((0x800d_efec, 0x800d_eff4, |_, _| 1)),
            scale: 1,
        },
        Plan {
            address: 0x800d_eff4,
            prepare: step,
            unclaimed: Some((0x800d_eff4, 0x800d_f2b0, |m, _| ov2_p2_draw::open_window(m))),
            scale: 2,
        },
        p(0x800d_f2b0, step),
        p(0x800d_f2e8, step),
        p(0x800d_f45c, step),
        p(0x800d_f5d8, step),
        p(0x800d_f760, |m, r| handler(m, r, 0)),
        p6(0x800d_f940, |m, r| handler(m, r, 1)),
        p(0x800d_fba8, |m, r| handler(m, r, 2)),
        p6(0x800d_fcc8, |m, r| handler(m, r, 3)),
        p(0x800e_019c, |m, r| handler(m, r, 4)),
        p(0x800e_01ac, |m, r| handler(m, r, 5)),
        p(0x800e_01c4, |m, r| handler(m, r, 6)),
        // ov2_p2_cells.rs
        p(0x800e_03cc, small),
        p6(0x800e_03d4, |m, r| animation(m, r, 1)),
        p6(0x800e_056c, |m, r| animation(m, r, 5)),
        p6(0x800e_0798, |m, r| animation(m, r, 7)),
        // the inline cases 11..15 have rare branches (cell 11 reaching
        // velocity 0x1A, cell 12's pad bit 8, cell 15's edges)
        Plan {
            address: 0x800e_08d4,
            prepare: |m, r| {
                let cell = [0, 1, 3, 5, 7, 11, 12, 13, 14, 15, 0x11][r.next() as usize % 11];
                animation(m, r, cell)
            },
            unclaimed: None,
            scale: 24,
        },
        // ov2_p2_grid.rs
        p(0x800e_0f88, coordinates),
        p(0x800e_1040, |m, r| {
            let sp = m.sp;
            m.sw(sp + 0x10, r.next() % 3);
            coordinates(m, r)
        }),
        p(0x800e_1118, |_, r| [r.next() % 8, 0, 0, 0]),
        p(0x800e_11f4, |m, r| {
            random_grid(m, r);
            [r.next() % 4, r.next() % 0x80, 0, 0]
        }),
        p6(0x800e_1258, |m, r| {
            random_grid(m, r);
            [0; 4]
        }),
        p(0x800e_1334, |m, r| {
            random_grid(m, r);
            [0; 4]
        }),
        p(0x800e_14d0, |m, r| {
            random_grid(m, r);
            [0; 4]
        }),
        // ov2_p2_hub.rs
        p(0x800d_e188, |m, r| {
            let visual = m.lw(VISUAL);
            m.sw(visual + 0x270, r.next() % 3);
            small(m, r)
        }),
        p(0x800d_e3cc, |m, r| {
            let visual = m.lw(VISUAL);
            m.sh(visual + 0x1c6, (r.next() % 13) as u16);
            let state = m.lw(ov2_p2_hub::STATE);
            m.sb(state + 2, (r.next() % 2) as u8);
            small(m, r)
        }),
        p(0x800d_e4bc, small),
        p(0x800d_e540, |m, r| {
            let visual = m.lw(VISUAL);
            m.sh(visual + 0x1c6, [3, 3, 0, 1, 2][r.next() as usize % 5]);
            small(m, r)
        }),
        p(0x800d_e718, small),
        p(0x800d_e73c, |m, r| {
            pads(m, r);
            small(m, r)
        }),
        // ov2_p2_records.rs
        p6(0x800d_c144, |m, r| {
            missions(m, r);
            small(m, r)
        }),
        p6(0x800d_c43c, |m, r| {
            missions(m, r);
            records(m, r);
            pads(m, r);
            small(m, r)
        }),
        p(0x800d_cb5c, |m, r| {
            let context = m.lw(0x800b_ce60);
            m.sw(context + 0x96c, r.next() % 0x40);
            small(m, r)
        }),
        // ov2_p2_options.rs
        Plan {
            address: 0x800d_7db0,
            prepare: |m, r| {
                options(m, r);
                [0; 4]
            },
            unclaimed: None,
            scale: 48,
        },
        p6(0x800d_8ca8, |m, r| {
            options(m, r);
            [0; 4]
        }),
        p(0x800d_911c, small),
        // ov2_p2_display.rs
        p(0x800d_b3f8, |m, r| {
            pads(m, r);
            [0; 4]
        }),
        p6(0x800d_b714, |m, r| {
            display(m, r);
            if r.next() % 2 == 0 {
                m.host_results.push_back(0x8000_0000);
            }
            [0; 4]
        }),
        p(0x800d_baec, config_record),
        p6(0x800d_bc94, config_record),
        p(0x800d_bfc4, config_record),
        p(0x800d_c0a0, |m, r| {
            config_record(m, r);
            let state = m.lw(ov2_p2_hub::STATE);
            m.sw(state + 0x140, r.next() % 4);
            [0; 4]
        }),
        // ov2_p2_music.rs
        p(0x800d_cc80, |m, r| {
            random_fades(m, r, 4);
            [0; 4]
        }),
        p(0x800d_cdc4, |m, r| {
            random_fades(m, r, 4);
            [0; 4]
        }),
        p(0x800d_ce0c, small),
        p6(0x800d_d1d0, |m, r| {
            music(m, r);
            [0; 4]
        }),
        p(0x800d_dfa4, |m, r| {
            let context = m.lw(0x800b_ce60);
            m.sw(context + 0x44, r.next() % 2);
            [0; 4]
        }),
        p(0x800d_e078, |m, r| {
            pads(m, r);
            [0; 4]
        }),
        // ov2_p2_screen.rs, ov2_p2_config.rs
        p6(0x800d_91fc, |m, r| {
            sticks(m, r);
            [0; 4]
        }),
        p(0x800d_9994, |m, r| {
            sticks(m, r);
            [0; 4]
        }),
        p(0x800d_a444, |m, r| {
            sticks(m, r);
            options(m, r);
            let state = m.lw(ov2_p2_hub::STATE);
            m.sh(state + 0xc, [1, 3, 0][r.next() as usize % 3]);
            [0; 4]
        }),
        p(0x800d_a4f4, |m, r| {
            sound_page(m, r);
            [0; 4]
        }),
        p(0x800d_a5ec, |m, r| {
            sound_page(m, r);
            [0; 4]
        }),
        p(0x800d_a66c, |m, r| {
            sound_page(m, r);
            [0; 4]
        }),
        p6(0x800d_a870, |m, r| {
            sound_page(m, r);
            [0; 4]
        }),
        p6(0x800d_aa6c, |m, r| {
            sound_page(m, r);
            [0; 4]
        }),
        p(0x800d_cbe0, |m, r| {
            let visual = m.lw(VISUAL);
            m.sh(visual + 0x86, (r.next() % 4) as u16);
            m.sh(visual + 0x84, [7, 7, 0, 3, 6][r.next() as usize % 5]);
            small(m, r)
        }),
    ]
}

/// A random options-menu state: selection, level, toggles, targets,
/// sub-pages, the confirm-blocking word, pads and fades.
fn options(m: &mut Machine, rng: &mut Rng) {
    let state = m.lw(ov2_p2_hub::STATE);
    m.sh(state + 0xc, (rng.next() % 4) as u16);
    m.sh(state + 0xe, (rng.next() % 4) as u16);
    m.sb(state + 0x1c, (rng.next() % 2) as u8);
    m.sb(state + 0x1d, (rng.next() % 2) as u8);
    m.sb(state + 0x1f, (1 + rng.next() % 3) as u8);
    // word +0x20 (its upper half blocks a confirm) overlaps the sub-page
    // bytes +0x21..+0x23: either the word or the bytes are random
    m.sw(state + 0x20, [0, 0, 0x1_0000][rng.next() as usize % 3]);
    if rng.next().is_multiple_of(2) {
        for offset in [0x21, 0x22, 0x23] {
            m.sb(state + offset, (rng.next() % 4) as u8);
        }
    }
    // the entries' help text bytes (+0x24 + 16 * selection + index),
    // sometimes the ones with alternate texts (0xE, 0xF, 0x13)
    if rng.next().is_multiple_of(2) {
        for offset in 0x24..0x64 {
            m.sb(state + offset, [0xe, 0xf, 0x13, 1][rng.next() as usize % 4]);
        }
    }
    m.sw(state + 0x10, rng.next() % 3);
    m.sw(state + 0xe0, rng.next() % 2);
    m.sw(state + 0xe4, rng.next() % 2);
    m.sw(state + 0xe8, rng.next() % 2);
    for offset in [0xb4, 0xb5] {
        m.sb(state + offset, (rng.next() % 2) as u8);
    }
    let context = m.lw(0x800b_ce60);
    m.sh(context + 0x156, [0, 0, 1, 8, 2][rng.next() as usize % 5]);
    m.sw(
        context + 0x124,
        [0, 4, 2, 0x10, 1, 8, 0x20, 0x14][rng.next() as usize % 8],
    );
    pads(m, rng);
    random_fades(m, rng, 4);
}

/// Random stick axes (context +0x62..+0x68), ranges (state +0xEC..+0xF3),
/// phase (+0xFC 0..5) and pads.
fn sticks(m: &mut Machine, rng: &mut Rng) {
    let context = m.lw(0x800b_ce60);
    for axis in 0..4 {
        let value = [0, 0x10, 0x60, 0x80, 0xa0, 0xf0, 0xff][rng.next() as usize % 7] as u16;
        m.sh(context + 0x62 + 2 * axis, value);
    }
    let state = m.lw(ov2_p2_hub::STATE);
    for offset in 0xec..0xf4 {
        m.sb(
            state + offset,
            [0x20, 0x80, 0xd0, 0x3f, 0xc1][rng.next() as usize % 5],
        );
    }
    m.sw(
        state + 0xfc,
        [0, 1, 2, 3, 4, 5, u32::MAX][rng.next() as usize % 7],
    );
    pads(m, rng);
    let context = m.lw(0x800b_ce60);
    m.sw(context + 0x12c, [0, 0x40, 0x40][rng.next() as usize % 3]);
}

/// A random sound page: cursor (state +0x100), choice (+6), made (+3),
/// the settings (context +0x398..+0x39A), pads and fades.
fn sound_page(m: &mut Machine, rng: &mut Rng) {
    let state = m.lw(ov2_p2_hub::STATE);
    m.sb(state + 0x100, (rng.next() % 4) as u8);
    m.sb(state + 6, (rng.next() % 5) as u8);
    m.sb(state + 3, (rng.next() % 3) as u8);
    m.sb(state + 0x101, (rng.next() % 3) as u8);
    for offset in [0x102, 0x103] {
        m.sb(state + offset, rng.next() as u8);
    }
    let context = m.lw(0x800b_ce60);
    for offset in [0x398, 0x399] {
        m.sb(
            context + offset,
            [0, 0x10, 0x80, 0xef, 0xf0, 0xff][rng.next() as usize % 6],
        );
    }
    m.sb(context + 0x39a, (rng.next() % 4) as u8);
    pads(m, rng);
    random_fades(m, rng, 4);
}

/// A random display offset ([gp+0x2D4] +8/+0xC), saved offset (state
/// +0x106/+0x108), option pad and stick (context +0x124, +0x68, +0x60).
fn display(m: &mut Machine, rng: &mut Rng) {
    let display = m.lw(ac3_flight_jp::GP + 0x2d4);
    m.sw(display + 8, (rng.next() % 0x1a).wrapping_sub(0xc));
    m.sw(display + 0xc, (rng.next() % 0xb).wrapping_sub(1));
    let state = m.lw(ov2_p2_hub::STATE);
    m.sh(state + 0x106, (rng.next() % 0x10) as u16);
    m.sh(state + 0x108, (rng.next() % 0x10) as u16);
    let context = m.lw(0x800b_ce60);
    m.sw(context + 0x124, [0, 0x10, 0x10, 4][rng.next() as usize % 4]);
    m.sh(
        context + 0x68,
        [0x40, 0x80, 0x81, 0xff][rng.next() as usize % 4],
    );
    m.sh(context + 0x60, [0, 0x1000][rng.next() as usize % 2]);
    pads(m, rng);
}

/// A random game configuration record at state +0x13A (bytes 0..3) and
/// context settings; returns it as the argument.
fn config_record(m: &mut Machine, rng: &mut Rng) -> [u32; 4] {
    let state = m.lw(ov2_p2_hub::STATE);
    let record = state + 0x13a;
    for n in 0..4 {
        m.sb(record + n, (rng.next() % 4) as u8);
    }
    let context = m.lw(0x800b_ce60);
    m.sb(context + 0x39c, (rng.next() % 3) as u8);
    m.sw(context + 0x120, rng.next() % 4);
    m.sb(context + 0x95a, (rng.next() % 3) as u8);
    pads(m, rng);
    [record, 0, 0, 0]
}

/// A music page opened natively (800DCE0C), with a random cursor, play
/// mode, phase, track and pads.
fn music(m: &mut Machine, rng: &mut Rng) {
    ac3_flight_jp::jp::ov2_p2_music::open_music(m);
    let _ = m.take_fault();
    let page = m.lw(ac3_flight_jp::jp::ov2_p2_music::PAGE);
    m.sb(page + 0x23b8, (rng.next() % 9) as u8);
    m.sb(page + 0x23b9, (rng.next() % 3) as u8);
    m.sh(page + 0x23b6, rng.next() as u16);
    let player = m.lw(ac3_flight_jp::jp::ov2_p2_music::PLAYER);
    if (0x8000_0000..0x801f_0000).contains(&player) {
        m.sw(player + 8, (rng.next() % 0x24).wrapping_sub(2));
        m.sw(player, rng.next() % 4);
    }
    let context = m.lw(0x800b_ce60);
    m.sw(context + 0x20, word(rng));
    let driver = m.lw(ac3_flight_jp::GP + 0x38);
    if (0x8000_0000..0x801f_0000).contains(&driver) {
        let flags = [0, 0x4000_0000, 0x0200_0000, 0x4200_0000][rng.next() as usize % 4];
        m.sw(driver, flags);
    }
    pads(m, rng);
}

/// Random mission flags (context +0xD0C..), completion flags and times
/// of the records (context +0x95C + 0x18.., +0x58.., +0x6D0).
fn missions(m: &mut Machine, rng: &mut Rng) {
    let context = m.lw(0x800b_ce60);
    for n in 0..0x3b {
        m.sb(context + 0xd0c + n, rng.next().is_multiple_of(3) as u8);
    }
    let records = context + 0x95c;
    for n in 0..0x50 {
        m.sb(records + 0x18 + n, (rng.next() % 3) as u8);
    }
    let times = [0, 0x100, 0x066f_f2ff, 0x0670_0000, u32::MAX, 0x8000_0000];
    m.sw(records + 0x6d0, times[rng.next() as usize % times.len()]);
}

/// Random pad words: held (context +0x128), pressed (+0x12C), repeated
/// (+0x130).
fn pads(m: &mut Machine, rng: &mut Rng) {
    let context = m.lw(0x800b_ce60);
    for offset in [0x128, 0x12c, 0x130] {
        let bits = [0, 0x20, 4, 8, 0x24, 0x2c, 1, 2, 0x10, 0x40, 0x12][rng.next() as usize % 11];
        m.sw(context + offset, bits);
    }
}

/// A records list (as 800DC144 builds it, at a scratch address) with a
/// random count and selection, and a random difficulty (context +0x394).
fn records(m: &mut Machine, rng: &mut Rng) {
    let list = 0x801e_8000;
    m.sw(ov2_p2_records::LIST, list);
    let count = 1 + rng.next() % 0x30;
    for i in 0..count {
        m.sb(list + i, (rng.next() % 0x3b) as u8);
    }
    m.sb(list + 0x3c, count as u8);
    m.sb(list + 0x3d, (rng.next() % 0x40) as u8);
    m.sb(list + 0x3e, (rng.next() % 0x10) as u8);
    m.sb(list + 0x3f, (rng.next() % count) as u8);
    let context = m.lw(0x800b_ce60);
    m.sw(context + 0x394, rng.next() % 3);
}

/// A cell animation case: random modes, velocities and positions of the
/// list-0 cells, a random counter (+0x1A4) around the timed steps, random
/// pad words (context +0x128/+0x12C). Arguments: `cell`.
fn animation(m: &mut Machine, rng: &mut Rng, cell: u32) -> [u32; 4] {
    let visual = m.lw(VISUAL);
    let records = m.lw(visual + 0x16c);
    for n in 0..0x12 {
        let record = records + 12 * n;
        let near = |rng: &mut Rng, edges: &[i32]| {
            let edge = edges[rng.next() as usize % edges.len()];
            (edge + (rng.next() % 9) as i32 - 4) as u16
        };
        let x = near(rng, &[-0x13f, 0, 0x80, -0x180]);
        m.sh(record + 4, x);
        let y = near(rng, &[-0x1df, 0, 0x1e0, -0x10, 0x100]);
        m.sh(record + 6, y);
        let v = near(rng, &[0, 0x1a, 0x1c, -0x18, 2, -2, -6]);
        m.sh(record + 8, v);
        m.sh(
            record + 0xa,
            [0, 0, 1, 2, 2, 3, 0xffff][rng.next() as usize % 7],
        );
    }
    let times = [
        0,
        1,
        0x1a,
        0x1b,
        0x1c,
        0x1d,
        0x1e,
        0x1f,
        0x20,
        0x21,
        0x22,
        0x23,
        0x24,
        0x25,
        0x26,
        0x2b,
        0x190,
        0x18f,
        u32::MAX,
    ];
    m.sw(visual + 0x1a4, times[rng.next() as usize % times.len()]);
    let context = m.lw(0x800b_ce60);
    m.sw(context + 0x128, rng.next() % 16);
    m.sw(context + 0x12c, rng.next() % 16);
    [cell, 0, 0, 0]
}

/// A random 32-bit word.
fn word(rng: &mut Rng) -> u32 {
    rng.next() << 8 ^ rng.next()
}

/// Random screen coordinates.
fn coordinates(_: &mut Machine, rng: &mut Rng) -> [u32; 4] {
    std::array::from_fn(|_| rng.next() % 0x200)
}

/// A random reveal grid: mode 0..4, timer 0..0x80, cursor, visible and
/// pending rows, a random context selector +0x38C (0..2).
fn random_grid(m: &mut Machine, rng: &mut Rng) {
    let block = m.lw(WORK);
    m.sh(block + 0x172, (rng.next() % 4) as u16);
    let timers = [0, 1, 0x1e, 0x1f, 0x20, 0x77, 0x78, 0x79, 0x40];
    m.sh(block + 0x170, timers[rng.next() as usize % timers.len()]);
    m.sh(block + 0x174, (rng.next() % 0x20) as u16);
    m.sh(block + 0x176, (rng.next() % 0x20) as u16);
    for row in 0..0x20 {
        let visible = word(rng) & word(rng);
        m.sw(block + 0xf0 + 4 * row, visible);
        let pending = word(rng) & word(rng) & word(rng);
        m.sw(block + 0x70 + 4 * row, pending);
    }
    let context = m.lw(0x800b_ce60);
    m.sh(context + 0x38c, (rng.next() % 3) as u16);
    m.sw(context + 0x20, word(rng));
}

/// Cases per routine and state.
const VARIANTS: u32 = 6;

/// The oracle: the interpreter of `support::oracle::run` (the same
/// instruction semantics; calls out of the range run through `dispatch`),
/// recording every executed address of the routine for the coverage
/// report, with one difference: a call out of the range to a `jr ra; nop`
/// stub returns with `v0` unchanged, as the stub does. The shared oracle
/// stores the dispatched value there, and the claimed stub 800E03CC (case 0
/// of 800E08D4) returns 0 through `dispatch` (its `v0` is the caller's).
mod trace {
    use ac3_flight_jp::{Fault, Machine, dispatch};
    use std::collections::BTreeSet;

    const RETURN: u32 = 0xffff_fff0;
    /// `jr ra; nop`.
    const STUB: [u32; 2] = [0x03e0_0008, 0];

    pub fn run(
        m: &mut Machine,
        start: u32,
        end: u32,
        args: [u32; 4],
        seen: &mut BTreeSet<u32>,
    ) -> u32 {
        let mut r = [0u32; 32];
        let (mut hi, mut lo) = (0u32, 0u32);
        r[4..8].copy_from_slice(&args);
        r[28] = ac3_flight_jp::GP;
        r[29] = m.sp;
        r[31] = RETURN;
        let (mut pc, mut next) = (start, start + 4);
        let mut steps = 0u32;
        loop {
            if pc == RETURN {
                m.sp = r[29];
                return r[2];
            }
            if pc < start || pc >= end {
                m.sp = r[29];
                let wide = |hi: u32, lo: u32| (u64::from(hi) << 32) | u64::from(lo);
                match pc {
                    0x8009_ea48 => {
                        let q = ac3_flight_jp::sdk::udivdi3(m, wide(r[5], r[4]), wide(r[7], r[6]));
                        (r[2], r[3]) = (q as u32, (q >> 32) as u32);
                    }
                    0x8009_e9b0 => {
                        let q = ac3_flight_jp::sdk::divdi3(
                            m,
                            wide(r[5], r[4]) as i64,
                            wide(r[7], r[6]) as i64,
                        ) as u64;
                        (r[2], r[3]) = (q as u32, (q >> 32) as u32);
                    }
                    _ if [m.lw(pc), m.lw(pc.wrapping_add(4))] == STUB => {}
                    _ => r[2] = dispatch::call(m, pc, [r[4], r[5], r[6], r[7]]),
                }
                pc = r[31];
                next = pc + 4;
                continue;
            }
            steps += 1;
            assert!(steps < 10_000_000, "runaway at {pc:08x}");
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
                    0x0d => {
                        // The compiler's traps put the 10-bit code in the
                        // upper half of the break code (0x1C00 -> 7).
                        let code = ((w >> 6) & 0xf_ffff) >> 10;
                        m.raise(Fault::Trap { address: cur, code });
                        m.sp = r[29];
                        return r[2];
                    }
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
                    f => panic!("special {f:#x} at {cur:08x}"),
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
                        m.gte.command(w & 0x1ff_ffff).unwrap();
                    } else {
                        match rs {
                            0 => out = Some((rt, m.gte.mfc2(rd))),
                            2 => out = Some((rt, m.gte.cfc2(rd))),
                            4 => m.gte.mtc2(rd, b),
                            6 => m.gte.ctc2(rd, b),
                            x => panic!("cop2 {x} at {cur:08x}"),
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
                // lwl/lwr/swl/swr: no routine of this part uses them.
                o => panic!("op {o:#x} at {cur:08x}"),
            }
            if let Some((i, v)) = out
                && i != 0
            {
                r[i] = v;
            }
        }
    }
}

/// A device that answers nothing and stops a case after 100,000 host calls
/// (a wait for a device the test does not run).
struct Limit(usize);
impl ac3_flight_jp::Device for Limit {
    fn call(&mut self, _: &mut [u8], _: &mut [u8], _: ac3_flight_jp::HostCall) -> Option<u32> {
        self.0 += 1;
        assert!(
            self.0 <= 100_000,
            "runaway: waits for a device (100000 host calls)"
        );
        None
    }
}

/// The outcome of one oracle case.
enum Outcome {
    /// Equal results; RAM and scratchpad bytes the original changed.
    Match(usize),
    /// Both stopped at the same compiler trap (`break`).
    Trap,
    /// The original waits for a device this test does not run (a CD or
    /// card transfer): not compared.
    Wait,
}

fn panic_text(e: &(dyn std::any::Any + Send)) -> String {
    e.downcast_ref::<String>()
        .cloned()
        .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default()
}

/// One case: the oracle and the translation from clones of `base`. Ok(RAM
/// and scratchpad bytes the original changed, or None when both stopped at the same
/// compiler trap).
fn compare(
    base: &Machine,
    range: (u32, u32),
    args: [u32; 4],
    void: bool,
    native: &dyn Fn(&mut Machine) -> u32,
    seen: &mut std::collections::BTreeSet<u32>,
) -> Result<Outcome, String> {
    let mut original = base.clone();
    original.device = Some(Box::new(Limit(0)));
    let v0 = match panic::catch_unwind(AssertUnwindSafe(|| {
        trace::run(&mut original, range.0, range.1, args, seen)
    })) {
        Ok(v0) => v0,
        Err(e) => {
            let text = panic_text(&*e);
            if text.contains("waits for a device") {
                return Ok(Outcome::Wait);
            }
            return Err(format!("oracle: {text}"));
        }
    };
    let mut translated = base.clone();
    translated.device = Some(Box::new(Limit(0)));
    let nv0 = native(&mut translated);
    let (fo, fnat) = (original.take_fault(), translated.take_fault());
    if fo != fnat {
        return Err(format!("faults: original {fo:?}, native {fnat:?}"));
    }
    if let Err(Fault::Trap { .. }) = fo {
        return Ok(Outcome::Trap);
    }
    let low = snapshot::STACK_START;
    let high = (base.sp & 0x1f_ffff) as usize;
    let dead = |i: usize| (low..high).contains(&i);
    let ram: Vec<u32> = (0..0x20_0000usize)
        .filter(|&i| !dead(i) && original.ram()[i] != translated.ram()[i])
        .map(|i| 0x8000_0000 + i as u32)
        .collect();
    let scratch = (0..0x400)
        .filter(|&i| original.scratch()[i] != translated.scratch()[i])
        .count();
    if !ram.is_empty()
        || scratch != 0
        || original.gte != translated.gte
        || original.sp != translated.sp
    {
        return Err(format!(
            "ram {:08x?} ({} bytes) scratch {scratch} gte equal {} sp {:x}/{:x}",
            &ram[..ram.len().min(8)],
            ram.len(),
            original.gte == translated.gte,
            original.sp,
            translated.sp
        ));
    }
    if !void && v0 != nv0 {
        return Err(format!("v0: original {v0:#x}, native {nv0:#x}"));
    }
    Ok(Outcome::Match(
        (0..0x20_0000usize)
            .filter(|&i| !dead(i) && base.ram()[i] != original.ram()[i])
            .count()
            + (0..0x400)
                .filter(|&i| base.scratch()[i] != original.scratch()[i])
                .count(),
    ))
}

#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux RAM dumps)"]
fn routines_match_the_oracle() {
    let _fallback = Fallbacks::install();
    let states = states();
    assert!(
        states.len() >= 4,
        "too few overlay-2 states: {}",
        states.len()
    );
    let extents = extents();
    let claimed = claimed();
    let voids = void_routines();
    let only = std::env::var("ONLY")
        .ok()
        .and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok());
    let hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let mut failures = Vec::new();
    let (mut total, mut covered, mut all) = (0, 0, 0);
    let mut planned = Vec::new();
    for plan in plans() {
        if only.is_some_and(|a| a != plan.address) {
            continue;
        }
        planned.push(plan.address);
        let (start, end) = match plan.unclaimed {
            Some((start, end, _)) => (start, end),
            None => (plan.address, extents[&plan.address]),
        };
        let void = voids.contains(&plan.address);
        let (mut cases, mut traps, mut waits, mut changed) = (0, 0, 0, 0);
        let mut seen = std::collections::BTreeSet::new();
        for (index, (name, base)) in states.iter().enumerate() {
            for variant in 0..VARIANTS * plan.scale {
                let mut rng = Rng((plan.address ^ (variant << 20) ^ (index as u32) << 8)
                    .wrapping_mul(0x9e37_79b9)
                    | 1);
                let mut m = base.clone();
                let args = (plan.prepare)(&mut m, &mut rng);
                let label = format!("{:08x} {name} v{variant} {args:08x?}", plan.address);
                let address = plan.address;
                let unclaimed = plan.unclaimed;
                let native = move |m: &mut Machine| match unclaimed {
                    Some((_, _, f)) => f(m, args),
                    None => dispatch::call(m, address, args),
                };
                let result = panic::catch_unwind(AssertUnwindSafe(|| {
                    compare(&m, (start, end), args, void, &native, &mut seen)
                }));
                match result {
                    Ok(Ok(Outcome::Match(n))) => {
                        cases += 1;
                        changed += n;
                    }
                    Ok(Ok(Outcome::Trap)) => traps += 1,
                    Ok(Ok(Outcome::Wait)) => waits += 1,
                    Ok(Err(e)) => failures.push(format!("{label}: {e}")),
                    Err(e) => failures.push(format!("{label}: native panic {}", panic_text(&*e))),
                }
            }
        }
        total += cases + traps;
        let instructions = (end - start) / 4;
        if std::env::var_os("COVERAGE").is_some() {
            let missing: Vec<String> = (start..end)
                .step_by(4)
                .filter(|pc| !seen.contains(pc))
                .map(|pc| format!("{pc:08x}"))
                .collect();
            if !missing.is_empty() {
                println!("  not executed: {}", missing.join(" "));
            }
        }
        covered += seen.len();
        all += instructions as usize;
        println!(
            "{:08x}: {cases} cases match ({changed} RAM/scratchpad bytes changed by the original), \
             {traps} equal traps, {waits} wait for a device; {}/{instructions} instructions executed",
            plan.address,
            seen.len()
        );
    }
    panic::set_hook(hook);
    println!(
        "oracle: {total} cases on {} states; {covered}/{all} instructions executed",
        states.len()
    );
    if only.is_none() {
        let missing: Vec<String> = claimed
            .iter()
            .filter(|a| !planned.contains(a))
            .map(|a| format!("{a:08x}"))
            .collect();
        assert!(
            missing.is_empty(),
            "claimed routines without oracle cases: {missing:?}"
        );
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
