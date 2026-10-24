//! JP overlay 2, part 1 (`src/jp/ov2_p1*.rs`, 800CD7C0..800D7DB0) against
//! the JP original:
//! - **captures**: every captured call of a routine of the part
//!   (`~/ac3-jp/analysis/redux/*/snapshots-module2.jsonl`) runs natively
//!   from its entry snapshot and must reproduce the return snapshot (RAM
//!   outside the stack, scratchpad, GTE, `v0`), as `tests/captures.rs`;
//! - **oracle**: every routine runs as the original instructions (the
//!   test-only oracle, `support::oracle`) and natively from the same JP
//!   machine states, which must end equal (RAM outside the stack below the
//!   entry `sp`, scratchpad, GTE, `sp`, fault, `v0`). The states are the
//!   module-2 entry snapshots of the captures with randomised fields that
//!   select the routine's branches (`oracle::Rng`).
//!
//! The routines of other parts that these call are not all translated yet,
//! so the tests need the interpreter fallback (feature `dev-fallback`):
//! `cargo test --release -p ac3-flight-jp --features dev-fallback --test
//! ov2_p1 -- --include-ignored`.
#![cfg(feature = "dev-fallback")]
mod support;

use ac3_flight_jp::snapshot::{self, Snapshot, SnapshotInfo};
use ac3_flight_jp::{Machine, dispatch, sdk};
use std::path::{Path, PathBuf};
use support::oracle::{self, Rng};
use support::scan;

/// The part: (JP address, size in bytes).
const PART: &[(u32, u32)] = &[
    (0x800c_d7c0, 48),
    (0x800c_d7f0, 772),
    (0x800c_daf4, 252),
    (0x800c_dbf0, 120),
    (0x800c_dc68, 108),
    (0x800c_dcd4, 176),
    (0x800c_dd84, 444),
    (0x800c_df40, 1424),
    (0x800c_e4d0, 132),
    (0x800c_e554, 348),
    (0x800c_e6b0, 60),
    (0x800c_e6ec, 104),
    (0x800c_e75c, 144),
    (0x800c_e7ec, 168),
    (0x800c_e894, 144),
    (0x800c_e924, 192),
    (0x800c_e9e4, 136),
    (0x800c_ea6c, 144),
    (0x800c_eafc, 152),
    (0x800c_eb94, 220),
    (0x800c_ec70, 152),
    (0x800c_ed08, 48),
    (0x800c_ed38, 112),
    (0x800c_eda8, 1076),
    (0x800c_f1dc, 392),
    (0x800c_f364, 304),
    (0x800c_f494, 112),
    (0x800c_f504, 96),
    (0x800c_f564, 220),
    (0x800c_f640, 112),
    (0x800c_f6b0, 16),
    (0x800c_f6c0, 404),
    (0x800c_f854, 340),
    (0x800c_f9a8, 1460),
    (0x800c_ff5c, 216),
    (0x800d_0034, 264),
    (0x800d_013c, 952),
    (0x800d_04f4, 148),
    (0x800d_0588, 568),
    (0x800d_07c0, 104),
    (0x800d_0830, 320),
    (0x800d_0970, 120),
    (0x800d_09e8, 588),
    (0x800d_0c34, 1504),
    (0x800d_1214, 68),
    (0x800d_1258, 68),
    (0x800d_129c, 68),
    (0x800d_12e0, 304),
    (0x800d_1418, 136),
    (0x800d_14a0, 48),
    (0x800d_14d0, 136),
    (0x800d_1558, 48),
    (0x800d_180c, 216),
    (0x800d_18e4, 196),
    (0x800d_19a8, 148),
    (0x800d_1a3c, 200),
    (0x800d_1b04, 104),
    (0x800d_240c, 220),
    (0x800d_24e8, 128),
    (0x800d_2568, 1636),
    (0x800d_2bcc, 424),
    (0x800d_1588, 644),
    (0x800d_1b6c, 1000),
    (0x800d_1f54, 1208),
    (0x800d_2d74, 124),
    (0x800d_2df0, 440),
    (0x800d_2fa8, 224),
    (0x800d_3088, 288),
    (0x800d_31a8, 8),
    (0x800d_31b0, 48),
    (0x800d_31e0, 48),
    (0x800d_3210, 48),
    (0x800d_3240, 156),
    (0x800d_32dc, 32),
    (0x800d_32fc, 140),
    (0x800d_3388, 1380),
    (0x800d_38ec, 756),
    (0x800d_3be0, 960),
    (0x800d_3fa0, 1012),
    (0x800d_4394, 156),
    (0x800d_4430, 940),
    (0x800d_47dc, 284),
    (0x800d_48f8, 940),
    (0x800d_4ca4, 124),
    (0x800d_4d20, 600),
    (0x800d_4f78, 3164),
    (0x800d_5bd4, 400),
    (0x800d_5d64, 920),
    (0x800d_60fc, 1216),
    (0x800d_65bc, 1408),
    (0x800d_6b3c, 1624),
    (0x800d_7194, 1468),
    (0x800d_7750, 1632),
];

/// The capture root (`AC3_JP_CAPTURES`, else `AC3_JP_DIR/analysis/redux`).
fn root() -> PathBuf {
    std::env::var_os("AC3_JP_CAPTURES")
        .map(PathBuf::from)
        .unwrap_or_else(|| scan::jp_dir().join("analysis/redux"))
}

/// Every module-2 snapshot index under the root.
fn indexes() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(root())
        .into_iter()
        .flatten()
        .flatten()
        .map(|run| run.path().join("snapshots-module2.jsonl"))
        .filter(|p| p.is_file())
        .collect();
    out.sort();
    out
}

/// The interpreter fallback for the callees nobody translated yet.
/// A step limit stops a runaway interpretation (a random state's loop)
/// with a fault, in both runs alike.
fn fallback() -> std::rc::Rc<ac3_fallback::Fallback> {
    let mut fallback = ac3_fallback::Fallback::new(ac3_fallback::Version::Jp);
    fallback.set_max_steps(5_000_000);
    ac3_fallback::install(fallback)
}

fn size_of(address: u32) -> Option<u32> {
    PART.iter().find(|p| p.0 == address).map(|p| p.1)
}

/// The return snapshot of an entry (same callee, `ra` and `sp`).
fn exit_of<'a>(index: &'a [SnapshotInfo], entry: &SnapshotInfo) -> Option<&'a SnapshotInfo> {
    index.iter().find(|s| {
        s.callee == entry.callee
            && s.stage == "return"
            && s.sequence > entry.sequence
            && s.ra == entry.ra
            && s.registers.gpr[29] == entry.registers.gpr[29]
    })
}

#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn captured_calls_reproduce() {
    let _fallback = fallback();
    let (mut passed, mut failed) = (Vec::new(), Vec::new());
    for path in indexes() {
        let directory = path.parent().unwrap();
        let run = directory.file_name().unwrap().to_string_lossy().to_string();
        let index = snapshot::read_index(&path).unwrap();
        for entry in index.iter().filter(|s| s.stage == "entry") {
            if size_of(entry.callee).is_none() {
                continue;
            }
            let Some(exit) = exit_of(&index, entry) else {
                continue;
            };
            let label = format!("{run} #{} {:08x}", entry.sequence, entry.callee);
            match replay(directory, entry, exit) {
                Ok(n) => passed.push(format!("{label}: {n} bytes")),
                Err(e) => failed.push(format!("{label}: {e}")),
            }
        }
    }
    for line in &passed {
        println!("pass {line}");
    }
    for line in &failed {
        println!("FAIL {line}");
    }
    println!("captures: {} pass, {} fail", passed.len(), failed.len());
    assert!(failed.is_empty(), "{failed:#?}");
    assert!(!passed.is_empty());
}

/// RAM the original's interrupt handlers and devices own, which a capture
/// pair may change while the routine runs (the PAL tests' allowances,
/// `ac3-flight` `tests/frame.rs` and `tests/ov3.rs`, at the JP addresses):
/// the native host calls complete at once and run no handler. libgpu's
/// LoadImage of a queued upload (800ECDA8 installing a bank) is drained by
/// the GPU DMA interrupt; ClearOTagR / DrawSync poll the DMA (the poll
/// counter counts hardware time); the saved interrupt mask is a device
/// value.
const OWNED: &[(u32, u32, &str)] = &[
    (0x8000_0000, 0x8001_0000, "BIOS kernel RAM"),
    (sdk::PAD_VSYNC_FLAG, sdk::PAD_VSYNC_FLAG + 4, "VBlank flag"),
    (
        sdk::VSYNC_LAST_HCOUNT,
        sdk::VSYNC_LAST_COUNT + 4,
        "VSync root-counter reading",
    ),
    (
        sdk::INTERRUPT_MANAGER,
        0x800b_60c4,
        "libetc interrupt context and stack",
    ),
    (sdk::VSYNC_COUNTER, sdk::VSYNC_COUNTER + 4, "VSync counter"),
    (
        sdk::DMA_CALLBACKS + 8,
        sdk::DMA_CALLBACKS + 0xc,
        "GPU DMA callback slot",
    ),
    (
        sdk::GPU_QUEUE_WRITE,
        sdk::GPU_SAVED_MASK + 4,
        "libgpu queue indexes, saved interrupt mask",
    ),
    (
        sdk::GPU_TIMEOUT_DEADLINE,
        sdk::GPU_TIMEOUT_POLLS + 4,
        "libgpu timeout and DMA poll counter",
    ),
    (0x800c_a458, 0x800c_bc58, "libgpu queue entries"),
];

fn owner(address: u32) -> Option<&'static str> {
    OWNED
        .iter()
        .find(|(start, end, _)| (*start..*end).contains(&address))
        .map(|o| o.2)
}

/// Replay one captured call: Ok(bytes the original changed) or Err(cause).
fn replay(directory: &Path, entry: &SnapshotInfo, exit: &SnapshotInfo) -> Result<usize, String> {
    let entry = Snapshot::load(directory, entry).map_err(|e| e.to_string())?;
    let exit = Snapshot::load(directory, exit).map_err(|e| e.to_string())?;
    let mut m = entry.machine();
    let args = [entry.gpr(4), entry.gpr(5), entry.gpr(6), entry.gpr(7)];
    let v0 = dispatch::call(&mut m, entry.info.callee, args);
    if let Err(fault) = m.take_fault() {
        return Err(format!("fault {fault}"));
    }
    let mut d = snapshot::compare(&m, &exit);
    d.ram.retain(|&a| owner(a | 0x8000_0000).is_none());
    if !d.is_empty() {
        let mut detail = String::new();
        for &a in d.ram.iter().take(8) {
            let at = (a & 0x1f_fffc) as usize;
            let word = |ram: &[u8]| u32::from_le_bytes(ram[at..at + 4].try_into().unwrap());
            detail += &format!(
                " [{:08x}: entry {:08x} original {:08x} native {:08x}]",
                a & !3,
                word(entry.machine().ram()),
                word(exit.machine().ram()),
                word(m.ram())
            );
        }
        return Err(format!("state differs: {}{detail}", d.summary()));
    }
    if v0 != exit.gpr(2) {
        return Err(format!("v0 {v0:#x}, original {:#x}", exit.gpr(2)));
    }
    Ok(snapshot::changed_bytes(&entry, &exit))
}

/// Module-2 machine states: the entry snapshots of the part's captured
/// routines (one per routine, the hub screens they ran in).
fn states() -> Vec<(String, Machine)> {
    let mut out = Vec::new();
    for path in indexes() {
        let directory = path.parent().unwrap();
        let run = directory.file_name().unwrap().to_string_lossy().to_string();
        let index = snapshot::read_index(&path).unwrap();
        for entry in index.iter().filter(|s| s.stage == "entry") {
            if size_of(entry.callee).is_none() {
                continue;
            }
            let snap = Snapshot::load(directory, entry).unwrap();
            out.push((format!("{run}#{}", entry.sequence), snap.machine()));
        }
    }
    assert!(
        !out.is_empty(),
        "no module-2 states under {}",
        root().display()
    );
    out
}

/// Guest `sp` of the oracle cases.
const SP: u32 = 0x801f_ff00;

/// Run `cases` oracle comparisons of `address` over the states: each case
/// starts from a state (round robin), applies `setup` (fields and
/// arguments) and compares the original with the translation. `void`:
/// the translation models no `v0`. Returns the RAM bytes the original
/// changed over all cases.
fn oracle_cases(
    states: &[(String, Machine)],
    address: u32,
    cases: u32,
    void: bool,
    setup: impl Fn(&mut Machine, &mut Rng, u32) -> [u32; 4],
) -> usize {
    let size = size_of(address).expect("a routine of the part");
    let mut changed = 0;
    for case in 0..cases {
        let (name, state) = &states[case as usize % states.len()];
        let mut base = state.clone();
        base.sp = SP;
        let mut rng = Rng(address ^ case.wrapping_mul(0x9e37_79b9));
        let args = setup(&mut base, &mut rng, case);
        let label = format!("{address:08x} case {case} ({name}) args {args:08x?}");
        changed += oracle::diff(&label, &base, (address, address + size), args, |m| {
            let v0 = dispatch::call(m, address, args);
            (!void).then_some(v0)
        });
        let mut copy = base.clone();
        COVERAGE.with_borrow_mut(|c| {
            let seen = c.entry(address).or_default();
            coverage::run(&mut copy, (address, address + size), args, seen);
        });
    }
    changed
}

type Seen = std::collections::BTreeMap<u32, std::collections::BTreeSet<u32>>;

thread_local! {
    /// The original instructions the oracle cases executed, by routine.
    static COVERAGE: std::cell::RefCell<Seen> = const { std::cell::RefCell::new(Seen::new()) };
}

/// Print (and clear) the instruction coverage of the routines this test
/// ran: the original's instructions the cases executed, and the first
/// ones none did.
fn coverage_summary(test: &str) {
    COVERAGE.with_borrow_mut(|c| {
        for (address, seen) in c.iter() {
            let total = size_of(*address).unwrap() / 4;
            let missing: Vec<String> = (0..total)
                .map(|i| address + 4 * i)
                .filter(|a| !seen.contains(a))
                .take(6)
                .map(|a| format!("{a:08x}"))
                .collect();
            let note = if missing.is_empty() {
                String::new()
            } else {
                format!(" (not run: {} ...)", missing.join(" "))
            };
            println!(
                "coverage {test} {address:08x}: {} of {total} instructions{note}",
                seen.len()
            );
        }
        c.clear();
    });
}

/// The oracle's interpreter (`support::oracle::run`) recording the
/// addresses it executes inside the routine: coverage only, never
/// compared.
mod coverage {
    use ac3_flight_jp::{Fault, Machine, dispatch};
    use std::collections::BTreeSet;

    const RETURN: u32 = 0xffff_fff0;

    pub fn run(
        m: &mut Machine,
        (start, end): (u32, u32),
        args: [u32; 4],
        seen: &mut BTreeSet<u32>,
    ) {
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
                return;
            }
            if pc < start || pc >= end {
                m.sp = r[29];
                r[2] = dispatch::call(m, pc, [r[4], r[5], r[6], r[7]]);
                pc = r[31];
                next = pc.wrapping_add(4);
                continue;
            }
            steps += 1;
            if steps > 2_000_000 || matches!(m.fault(), Some(Fault::Trap { .. })) {
                return;
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
                        (lo, hi) = if d == 0 {
                            (if n < 0 { 1 } else { u32::MAX }, a)
                        } else {
                            (n.wrapping_div(d) as u32, n.wrapping_rem(d) as u32)
                        };
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
                // break, lwl/lwr/swl/swr: not in this part's routines
                _ => return,
            }
            if let Some((i, v)) = out
                && i != 0
            {
                r[i] = v;
            }
        }
    }
}

/// A random choice of `values`.
fn pick(rng: &mut Rng, values: &[u32]) -> u32 {
    values[rng.next() as usize % values.len()]
}

const WORK: u32 = 0x8011_2dc4;

fn work(m: &mut Machine) -> u32 {
    m.lw(WORK)
}

fn context(m: &mut Machine) -> u32 {
    m.lw(0x800b_ce60)
}

#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn framework_matches_the_original() {
    let _fallback = fallback();
    let states = states();
    let mut total = 0;
    // 800CD7C0, 800CDAF4: the double-buffer index and the fade timer
    total += oracle_cases(&states, 0x800c_d7c0, 8, false, |m, rng, _| {
        let c = context(m);
        m.sw(c + 4, rng.next() & 1);
        [0; 4]
    });
    total += oracle_cases(&states, 0x800c_daf4, 24, false, |m, rng, case| {
        let w = work(m);
        let timer = match case % 4 {
            0 => 0x20,
            1 => 0x21,
            2 => rng.next() % 0x40,
            _ => rng.next() | 0x8000_0000,
        };
        m.sw(w + 0x50, timer);
        [0; 4]
    });
    // 800CDBF0, 800CDC68: the pages of context +0x954 and option bit 0x20
    for address in [0x800c_dbf0, 0x800c_dc68] {
        total += oracle_cases(&states, address, 32, false, |m, rng, case| {
            let c = context(m);
            m.sh(c + 0x954, (case % 5) as u16);
            let options = m.lw(c + 0x96c);
            m.sw(c + 0x96c, options ^ (rng.next() & 0x20));
            let scene = m.lw(0x8011_2da8);
            m.sw(scene + 0x14, rng.next() % 3);
            [rng.next(), rng.next(), 0, 0]
        });
    }
    // 800CDCD4: every phase with its results; the phase table's five
    // routines return 0, 1 and 2 only, so indices 8 and 9 (past it, the
    // load phases 800CE554 and 800CE6B0 of the next table) give the
    // results above 2 and the negative one (0x80110000) its code handles
    total += oracle_cases(&states, 0x800c_dcd4, 36, false, |m, rng, case| {
        let w = work(m);
        let c = context(m);
        m.sh(w + 0x8a, [0, 2, 3, 4, 8, 9][case as usize % 6]);
        m.sh(c + 0x954, (case / 6 % 4) as u16);
        m.sw(c + 0x44, rng.next() & 1);
        let scene = m.lw(0x8011_2da8);
        m.sw(scene + 0x14, (case / 3) & 1);
        [0; 4]
    });
    // 800CDD84: the debug meter
    total += oracle_cases(&states, 0x800c_dd84, 8, false, |m, rng, _| {
        let c = context(m);
        m.sw(c + 0x48, rng.next() % 0x200);
        m.sw(c + 0x4c, rng.next() % 0x200);
        [0; 4]
    });
    println!("framework: {total} RAM bytes changed by the originals");
    coverage_summary("framework");
    assert!(total > 0);
}

/// Panel 0 entry 1's state halfword (800DED38: `[W+0x16C]` + 12 + 0xA).
fn set_panel_state(m: &mut Machine, value: u16) {
    let w = work(m);
    let panel = m.lw(w + 0x16c);
    m.sh(panel + 12 + 0xa, value);
}

/// Hub cursor halfword `index` (800DEE84: `[[0x80112DC0]+0x58]+2*index`).
fn set_cursor(m: &mut Machine, index: u32, value: u16) {
    let flags = m.lw(0x8011_2dc0);
    let cursors = m.lw(flags + 0x58);
    m.sh(cursors + 2 * index, value);
}

#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn hub_frame_matches_the_original() {
    let _fallback = fallback();
    let states = states();
    let mut total = 0;
    // 800CDF40: every screen group and screen the hub screen table holds,
    // the lamp and exit branches
    // (the state's own screen, or the hub menu's two: other screens' blocks
    // are stale in a state)
    total += oracle_cases(&states, 0x800c_df40, 72, false, |m, rng, case| {
        let w = work(m);
        let c = context(m);
        if case % 3 != 0 {
            m.sb(w + 0x7c, 0);
            m.sb(w + 0x7d, (case % 3 - 1) as u8);
        }
        let options = m.lw(c + 0x96c);
        m.sw(c + 0x96c, options ^ (rng.next() & 0x10));
        let lit = m.lw(c + 0x124);
        m.sw(c + 0x124, lit ^ (rng.next() & 0x10));
        m.sh(w + 0x84, pick(rng, &[0, 2, 6]) as u16);
        match case / 6 % 4 {
            0 => m.sw(w + 0x94, 0),
            1 => m.sw(w + 0x94, pick(rng, &[4, 0x6000_0003])),
            2 => {
                m.sw(w + 0x94, 0);
                m.sb(w + 0x7f, 1);
            }
            _ => {
                m.sw(w + 0x94, 0);
                m.sb(w + 0x7f, 0);
                m.sw(w + 0x11c, 1);
            }
        }
        [0; 4]
    });
    total += oracle_cases(&states, 0x800c_e4d0, 4, false, |_, _, _| [0; 4]);
    println!("hub frame: {total} RAM bytes changed by the originals");
    coverage_summary("hub frame");
    assert!(total > 0);
}

#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn page_loaders_match_the_original() {
    let _fallback = fallback();
    let states = states();
    let mut total = 0;
    // the load phases: loading or loaded, option bit 0x10, page, +0x95F
    let loaders: [u32; 13] = [
        0x800c_e554,
        0x800c_e6b0,
        0x800c_e6ec,
        0x800c_e75c,
        0x800c_e7ec,
        0x800c_e894,
        0x800c_e924,
        0x800c_e9e4,
        0x800c_ea6c,
        0x800c_eafc,
        0x800c_eb94,
        0x800c_ec70,
        0x800c_ed08,
    ];
    for address in loaders {
        total += oracle_cases(&states, address, 12, false, |m, rng, case| {
            let c = context(m);
            let w = work(m);
            m.sw(c + 0x44, case & 1);
            let options = m.lw(c + 0x96c);
            m.sw(c + 0x96c, options ^ (rng.next() & 0x10));
            m.sw(w + 0xc4, rng.next() % 5);
            m.sb(c + 0x95f, (case >> 1 & 1) as u8);
            [case >> 2 & 1, 0, 0, 0]
        });
    }
    // the menu phases and the tick
    total += oracle_cases(&states, 0x800c_ed38, 16, false, |m, rng, _| {
        set_cursor(m, 1, pick(rng, &[0, 1, 2]) as u16);
        set_panel_state(m, pick(rng, &[0, 1, 2]) as u16);
        [0; 4]
    });
    total += oracle_cases(&states, 0x800c_eda8, 240, false, |m, rng, case| {
        let c = context(m);
        let w = work(m);
        m.sw(c + 0x12c, pick(rng, &[0, 0x10, 0x10, 0x20]));
        m.sw(c + 0x128, pick(rng, &[0, 0, 0, 1]));
        m.sw(c + 0x130, pick(rng, &[0, 4, 8, 0xc]));
        let options = m.lw(c + 0x96c);
        m.sw(c + 0x96c, options ^ (rng.next() & 0x10));
        m.sh(w + 0x1c6, pick(rng, &[3, 3, 3, 1]) as u16);
        m.sb(w + 0x7e, (case % 5) as u8);
        m.sh(c + 0x38c, pick(rng, &[0, 0x3b, 5]) as u16);
        m.sh(c + 0x954, pick(rng, &[0, 3]) as u16);
        [0; 4]
    });
    total += oracle_cases(&states, 0x800c_f1dc, 40, false, |m, rng, case| {
        let c = context(m);
        let w = work(m);
        set_cursor(m, 1, pick(rng, &[0, 0, 0, 2]) as u16);
        m.sh(w + 0x1c6, pick(rng, &[3, 3, 3, 0]) as u16);
        m.sb(w + 0x7e, (case % 5) as u8);
        let options = m.lw(c + 0x96c);
        m.sw(c + 0x96c, options ^ (rng.next() & 0x10));
        m.sh(c + 0x38c, (rng.next() % 0x40) as u16);
        [0; 4]
    });
    total += oracle_cases(&states, 0x800c_f364, 8, false, |m, rng, _| {
        let w = work(m);
        m.sb(w + 0x7e, pick(rng, &[0, 2, 4]) as u8);
        [0; 4]
    });
    let tick = |idle: bool| {
        move |m: &mut Machine, rng: &mut Rng, case: u32| {
            let w = work(m);
            let c = context(m);
            m.sh(w + 0x1c4, (case % 3) as u16);
            let phase = [0, 1, 2, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13][case as usize / 3 % 13];
            m.sh(w + 0x1c6, if idle { 3 } else { phase });
            m.sw(c + 0x44, rng.next() & 1);
            [w, 0, 0, 0]
        }
    };
    total += oracle_cases(&states, 0x800c_f494, 39, false, tick(false));
    // load phase 3 is the stub 800CE754 (`jr ra; nop`, outside the function
    // map): the oracle runs it as a call through dispatch, which cannot
    // carry `v0` in, so `v0` (the stub's own address) is asserted apart
    total += oracle_cases(&states, 0x800c_f494, 6, true, tick(true));
    for (_, state) in states.iter().take(3) {
        let mut m = state.clone();
        m.sp = SP;
        let w = work(&mut m);
        m.sh(w + 0x1c6, 3);
        assert_eq!(
            dispatch::call(&mut m, 0x800c_f494, [w, 0, 0, 0]),
            0x800c_e754
        );
    }
    println!("page loaders: {total} RAM bytes changed by the originals");
    coverage_summary("page loaders");
    assert!(total > 0);
}

const SEARCH: u32 = 0x8011_2dd0;

/// The states whose search block pointer is a RAM block with a plausible
/// list (the Search Engine screens).
fn search_states(states: &[(String, Machine)]) -> Vec<(String, Machine)> {
    let out: Vec<(String, Machine)> = states
        .iter()
        .filter(|(_, m)| {
            let mut m = m.clone();
            let s = m.lw(SEARCH);
            (0x8010_0000..0x801f_0000).contains(&s) && m.lw(s + 0x254) < 0x50
        })
        .cloned()
        .collect();
    assert!(!out.is_empty(), "no Search Engine state");
    out
}

/// Randomise the list: count, page, cursor, entry page, entry numbers.
fn shuffle_list(m: &mut Machine, rng: &mut Rng) {
    let s = m.lw(SEARCH);
    let count = pick(rng, &[0, 1, 3, 4, 5, 8, 9, 0x13, 0xffff_fffe]);
    m.sw(s + 0x254, count);
    m.sb(s + 2, (rng.next() % 4) as u8);
    m.sb(s + 3, (rng.next() % 4) as u8);
    m.sb(s + 4, (rng.next() % 3) as u8);
    for i in 0..0x14 {
        m.sb(s + 0x204 + i, (rng.next() % 0x4f) as u8);
    }
}

#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn search_engine_matches_the_original() {
    let _fallback = fallback();
    let states = search_states(&states());
    let mut total = 0;
    for address in [0x800c_f504, 0x800c_f564, 0x800c_f640] {
        total += oracle_cases(&states, address, 24, false, |m, rng, _| {
            shuffle_list(m, rng);
            [rng.next() % 4, rng.next() % 4, 0, 0]
        });
    }
    for address in [0x800c_f6b0, 0x800c_f6c0, 0x800c_f854, 0x800d_04f4] {
        total += oracle_cases(&states, address, 12, false, |m, rng, _| {
            shuffle_list(m, rng);
            [0; 4]
        });
    }
    total += oracle_cases(&states, 0x800c_f9a8, 96, false, |m, rng, case| {
        shuffle_list(m, rng);
        let c = context(m);
        m.sw(0x8011_2dd4, pick(rng, &[0, 0, 1, 5]));
        let buttons = [1, 2, 0x100, 0x200, 3, 0x300, 0];
        m.sw(c + 0x130, buttons[case as usize % buttons.len()]);
        m.sw(c + 0x12c, pick(rng, &[0, 0, 0x10, 0x20]));
        m.sw(c + 0x128, pick(rng, &[0, 0, 0, 1, 0x40]));
        [0; 4]
    });
    total += oracle_cases(&states, 0x800c_ff5c, 12, false, |m, rng, case| {
        shuffle_list(m, rng);
        let s = m.lw(SEARCH);
        m.sb(s + 1, (case % 3) as u8);
        [0; 4]
    });
    total += oracle_cases(&states, 0x800d_0034, 8, false, |m, rng, case| {
        shuffle_list(m, rng);
        let c = context(m);
        m.sw(c + 0x44, case & 1);
        [0; 4]
    });
    total += oracle_cases(&states, 0x800d_013c, 64, false, |m, rng, case| {
        shuffle_list(m, rng);
        let c = context(m);
        m.sw(c + 0x12c, [0, 1, 2, 0x20, 3][case as usize % 5]);
        m.sw(c + 0x128, pick(rng, &[0, 0, 0x10]));
        for cursor in [3, 4, 5] {
            set_cursor(m, cursor, pick(rng, &[0, 0, 1]) as u16);
        }
        [0; 4]
    });
    total += oracle_cases(&states, 0x800d_0588, 12, false, |m, rng, case| {
        let w = work(m);
        let c = context(m);
        m.sw(w + 0x270, case % 3);
        let few = case % 2 == 0;
        for i in 0..0x4f {
            let owned = if few {
                u32::from(i < 2)
            } else {
                pick(rng, &[0, 0, 1, 2])
            };
            m.sb(c + 0xd48 + i, owned as u8);
        }
        [0; 4]
    });
    total += oracle_cases(&states, 0x800d_07c0, 36, false, |m, rng, case| {
        shuffle_list(m, rng);
        let s = m.lw(SEARCH);
        let c = context(m);
        m.sb(s, (case % 6) as u8);
        m.sb(s + 1, (rng.next() % 2) as u8);
        m.sw(c + 0x44, rng.next() & 1);
        [0; 4]
    });
    println!("search engine: {total} RAM bytes changed by the originals");
    coverage_summary("search engine");
    assert!(total > 0);
}

const ARCHIVE: u32 = 0x8011_2db8;

/// The states whose archive block pointer is a RAM block with a plausible
/// list (the Archive screens).
fn archive_states(states: &[(String, Machine)]) -> Vec<(String, Machine)> {
    let out: Vec<(String, Machine)> = states
        .iter()
        .filter(|(_, m)| {
            let mut m = m.clone();
            let a = m.lw(ARCHIVE);
            (0x8010_0000..0x801f_0000).contains(&a) && m.lw(a + 0xb8) < 0x96
        })
        .cloned()
        .collect();
    assert!(!out.is_empty(), "no Archive state");
    out
}

/// Randomise the message list: count, page, cursor, message numbers.
fn shuffle_messages(m: &mut Machine, rng: &mut Rng) {
    let a = m.lw(ARCHIVE);
    let count = pick(rng, &[0, 1, 3, 4, 5, 8, 9, 0x13, 0xffff_fffe]);
    m.sw(a + 0xb8, count);
    m.sb(a + 2, (rng.next() % 4) as u8);
    m.sb(a + 3, (rng.next() % 4) as u8);
    for i in 0..0x14 {
        m.sb(a + 4 + i, (0x1a + rng.next() % 0x95) as u8);
    }
}

#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn archive_matches_the_original() {
    let _fallback = fallback();
    let all = states();
    let states = archive_states(&all);
    let mut total = 0;
    for address in [0x800d_0830, 0x800d_0970] {
        total += oracle_cases(&states, address, 24, false, |m, rng, _| {
            shuffle_messages(m, rng);
            [rng.next() % 4, 0, 0, 0]
        });
    }
    total += oracle_cases(&states, 0x800d_09e8, 12, false, |m, rng, case| {
        let w = work(m);
        let c = context(m);
        m.sw(w + 0x270, case % 3);
        let few = case % 2 == 0;
        for i in 0x1a..0xaf {
            let received = if few {
                u32::from(i < 0x1c)
            } else {
                pick(rng, &[0, 0, 0, 1, 2])
            };
            m.sb(c + 0xd98 + i, received as u8);
        }
        [0; 4]
    });
    total += oracle_cases(&states, 0x800d_0c34, 84, false, |m, rng, case| {
        shuffle_messages(m, rng);
        let c = context(m);
        m.sw(0x8011_2dd4, pick(rng, &[0, 0, 1, 5]));
        let buttons = [1, 2, 0x100, 0x200, 3, 0x300, 0];
        m.sw(c + 0x130, buttons[case as usize % buttons.len()]);
        m.sw(c + 0x12c, pick(rng, &[0, 0, 0x10, 0x20]));
        m.sw(c + 0x128, pick(rng, &[0, 0, 0, 1, 0x40]));
        [0; 4]
    });
    for (address, phases) in [
        (0x800d_1214u32, &[0u16, 3, 4][..]),
        (0x800d_1258, &[0, 1, 2, 3, 4, 5][..]),
        (0x800d_129c, &[0, 1, 2, 3][..]),
    ] {
        total += oracle_cases(&states, address, 12, false, |m, rng, case| {
            shuffle_messages(m, rng);
            let w = work(m);
            m.sh(w + 0x86, phases[case as usize % phases.len()]);
            [w, 0, 0, 0]
        });
    }
    total += oracle_cases(&states, 0x800d_12e0, 24, false, |m, rng, case| {
        shuffle_messages(m, rng);
        let w = work(m);
        let c = context(m);
        m.sh(w + 0x1c6, [0, 1, 2, 4, 5, 6][case as usize % 6]);
        m.sw(c + 0x44, rng.next() & 1);
        [0; 4]
    });
    // load phase 3 (the idle stub) as in the hub menu's tick
    total += oracle_cases(&states, 0x800d_12e0, 6, true, |m, rng, _| {
        shuffle_messages(m, rng);
        let w = work(m);
        m.sh(w + 0x1c6, 3);
        [0; 4]
    });
    for address in [0x800d_1418, 0x800d_14a0, 0x800d_14d0, 0x800d_1558] {
        total += oracle_cases(&all, address, 8, false, |m, _, case| {
            let c = context(m);
            m.sw(c + 0x44, case & 1);
            [0; 4]
        });
    }
    println!("archive: {total} RAM bytes changed by the originals");
    coverage_summary("archive");
    assert!(total > 0);
}

const OPTIONS: u32 = 0x8011_2ddc;

/// The states whose options block pointer is a RAM block (the OPTIONS
/// screens).
fn options_states(states: &[(String, Machine)]) -> Vec<(String, Machine)> {
    let out: Vec<(String, Machine)> = states
        .iter()
        .filter(|(_, m)| {
            let mut m = m.clone();
            let k = m.lw(OPTIONS);
            (0x8010_0000..0x801f_0000).contains(&k) && m.lbu(k + 4) < 6
        })
        .cloned()
        .collect();
    assert!(!out.is_empty(), "no OPTIONS state");
    out
}

#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn options_match_the_original() {
    let _fallback = fallback();
    let states = options_states(&states());
    let mut total = 0;
    for address in [0x800d_180c, 0x800d_18e4, 0x800d_19a8, 0x800d_1a3c] {
        total += oracle_cases(&states, address, 4, false, |_, _, _| [0; 4]);
    }
    total += oracle_cases(&states, 0x800d_1b04, 24, false, |_, rng, case| {
        [
            if case < 8 {
                1 << case
            } else {
                rng.next() % 0x80
            },
            0,
            0,
            0,
        ]
    });
    total += oracle_cases(&states, 0x800d_240c, 8, false, |m, rng, case| {
        let k = m.lw(OPTIONS);
        m.sb(k + 5, (case & 1) as u8);
        m.sb(k + 4, (rng.next() % 6) as u8);
        [0; 4]
    });
    total += oracle_cases(&states, 0x800d_24e8, 12, false, |m, rng, _| {
        set_cursor(m, 1, pick(rng, &[0, 1, 2]) as u16);
        set_panel_state(m, pick(rng, &[0, 1, 2]) as u16);
        [0; 4]
    });
    total += oracle_cases(&states, 0x800d_2568, 360, false, |m, rng, case| {
        let k = m.lw(OPTIONS);
        let c = context(m);
        let w = work(m);
        m.sb(k + 4, (case % 7) as u8);
        m.sw(c + 0x130, [1, 2, 4, 8, 0xc, 3, 0][case as usize / 7 % 7]);
        m.sw(c + 0x12c, pick(rng, &[0, 0, 0x10, 0x20, 0x40]));
        m.sw(c + 0x128, pick(rng, &[0, 0, 0, 1, 0x40]));
        m.sw(w + 0x90, pick(rng, &[0, 0, 5]));
        let options = m.lw(c + 0x96c);
        m.sw(c + 0x96c, options ^ (rng.next() & 0x10));
        [0; 4]
    });
    total += oracle_cases(&states, 0x800d_2bcc, 60, false, |m, rng, case| {
        let k = m.lw(OPTIONS);
        let w = work(m);
        set_cursor(m, 1, pick(rng, &[0, 0, 0, 1]) as u16);
        m.sh(w + 0x1c6, pick(rng, &[0, 0, 0, 3]) as u16);
        m.sb(k + 5, pick(rng, &[0, 0, 1]) as u8);
        m.sb(k + 3, pick(rng, &[0, 0, 1, 2]) as u8);
        m.sb(k + 4, (case % 6) as u8);
        [0; 4]
    });
    println!("options: {total} RAM bytes changed by the originals");
    coverage_summary("options");
    assert!(total > 0);
}

/// Randomise the controller records of the context (buttons, axes,
/// flags, options) and the settings bytes of `K`.
fn shuffle_settings(m: &mut Machine, rng: &mut Rng) {
    let c = context(m);
    let k = m.lw(OPTIONS);
    m.sw(c + 0x124, pick(rng, &[1, 2, 4, 8, 0x10, 0x20, 0x40, 0]));
    m.sw(c + 0x120, rng.next() & 4);
    for record in 0..5 {
        let base = c + 0x15c + record * 0x5c;
        for h in 0..0x15 {
            let mask = [0, 0, 1, 0x10, 0x80, 0x4000, 0x200][rng.next() as usize % 7];
            m.sh(base + 0x16 + 2 * h, mask);
        }
        m.sh(base + 0x50, (rng.next() & 0x1f) as u16);
        for axis in 0..6 {
            m.sb(
                base + 0x52 + axis,
                pick(rng, &[0, 1, 2, 4, 0x40, 0xff]) as u8,
            );
        }
        m.sb(base + 0x43, (rng.next() % 0x80) as u8);
        m.sw(base + 0x58, rng.next() & 0x18);
    }
    for slot in 0..4 {
        for b in 0..0xf {
            m.sb(k + 0x24 + slot * 16 + b, (rng.next() % 0x15) as u8);
        }
        for b in 0..4 {
            m.sb(
                k + 0x84 + slot * 4 + b,
                pick(rng, &[0, 2, 6, 8, 0xa, 0xc, 0x10]) as u8,
            );
            m.sb(k + 0x9c + slot * 4 + b, (rng.next() & 1) as u8);
        }
        m.sb(k + 0x15 + slot, (rng.next() % 4) as u8);
    }
    m.sb(k + 0x14, (rng.next() & 1) as u8);
    m.sb(k + 0x1b, (rng.next() % 4) as u8);
    m.sb(k + 0xb4, (rng.next() & 1) as u8);
    m.sb(k + 0xb5, (rng.next() & 1) as u8);
}

#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn key_settings_match_the_original() {
    let _fallback = fallback();
    let states = options_states(&states());
    let mut total = 0;
    for address in [0x800d_1b6c, 0x800d_1f54] {
        total += oracle_cases(&states, address, 160, false, |m, rng, _| {
            shuffle_settings(m, rng);
            [0; 4]
        });
    }
    total += oracle_cases(&states, 0x800d_1588, 24, false, |m, rng, _| {
        // the RECT and the primitive buffer (stack argument +0x10)
        let rect = 0x801e_0000;
        let prim = 0x801e_0100;
        m.sh(
            rect,
            (rng.next() % 0x400) as u16 ^ pick(rng, &[0, 0x8000]) as u16,
        );
        m.sh(
            rect + 2,
            (rng.next() % 0x200) as u16 ^ pick(rng, &[0, 0x8000]) as u16,
        );
        m.sh(rect + 4, (1 + rng.next() % 0x40) as u16);
        m.sh(rect + 6, (1 + rng.next() % 0x40) as u16);
        m.sw(SP + 0x10, prim);
        [rng.next() % 0x140, rng.next() % 0xf0, rect, rng.next() % 8]
    });
    total += oracle_cases(&states, 0x800d_2d74, 12, false, |m, rng, _| {
        set_cursor(m, 1, pick(rng, &[0, 1, 2]) as u16);
        set_panel_state(m, pick(rng, &[0, 1, 2]) as u16);
        [0; 4]
    });
    total += oracle_cases(&states, 0x800d_2df0, 60, false, |m, rng, case| {
        let k = m.lw(OPTIONS);
        let c = context(m);
        m.sb(k + 5, (case % 4) as u8);
        m.sw(c + 0x130, pick(rng, &[0, 4, 8, 0xc]));
        m.sw(c + 0x12c, pick(rng, &[0, 0, 0x10, 0x20]));
        m.sw(c + 0x128, pick(rng, &[0, 0, 0, 1, 0x40]));
        [0; 4]
    });
    total += oracle_cases(&states, 0x800d_2fa8, 32, false, |m, rng, case| {
        let k = m.lw(OPTIONS);
        let w = work(m);
        set_cursor(m, 1, pick(rng, &[0, 0, 0, 1]) as u16);
        m.sh(w + 0x1c6, pick(rng, &[0, 0, 0, 3]) as u16);
        m.sb(k + 5, (case % 4) as u8);
        [0; 4]
    });
    total += oracle_cases(&states, 0x800d_3088, 8, false, |m, rng, _| {
        shuffle_settings(m, rng);
        [0; 4]
    });
    for address in [0x800d_31b0, 0x800d_31e0, 0x800d_3210, 0x800d_32dc] {
        total += oracle_cases(&states, address, 2, false, |_, _, _| [0; 4]);
    }
    total += oracle_cases(&states, 0x800d_31a8, 2, true, |_, _, _| [0; 4]);
    total += oracle_cases(&states, 0x800d_3240, 16, false, |m, _, case| {
        let k = m.lw(OPTIONS);
        m.sh(k + 0xc, (case % 5) as u16);
        [0; 4]
    });
    total += oracle_cases(&states, 0x800d_32fc, 24, false, |m, rng, case| {
        let k = m.lw(OPTIONS);
        m.sb(k + 0xb4, (rng.next() & 1) as u8);
        m.sb(k + 0xb5, (rng.next() & 1) as u8);
        [[0x13, 0xe, 0xf, 3][case as usize % 4], 0, 0, 0]
    });
    println!("key settings: {total} RAM bytes changed by the originals");
    coverage_summary("key settings");
    assert!(total > 0);
}

/// The KEY CONFIG screen's state on `K`: a primitive buffer (`K+8`), the
/// four types' button orders (`K+0xC8`, identity tables), the type, the
/// cursors and popups, random assignments; and random pad bits.
fn key_screen(m: &mut Machine, rng: &mut Rng, kind: u32) {
    shuffle_settings(m, rng);
    let k = m.lw(OPTIONS);
    let c = context(m);
    m.sw(k + 8, 0x801e_1000);
    for t in 0..4 {
        let order = 0x801e_0200 + t * 0x20;
        m.sw(k + 0xc8 + 4 * t, order);
        for b in 0..0x10 {
            m.sb(order + b, (1 + (b + t) % 0xf) as u8);
        }
    }
    m.sh(k + 0xc, kind as u16);
    // axis codes of every kind on the analog pages
    for b in 0..0x10 {
        m.sb(k + 0x84 + b, pick(rng, &[0, 1, 2, 4, 6, 8, 0xa, 0xc]) as u8);
    }
    m.sh(k + 0xe, (rng.next() % 5) as u16);
    m.sb(k + 0x1c, (rng.next() & 1) as u8);
    m.sb(k + 0x1e, (rng.next() % 5) as u8);
    m.sb(
        k + 0x1f,
        pick(rng, &[0, 1, 2, 3, 4, 5, 6, 9, 0xb, 0xc, 0xd, 0xf]) as u8,
    );
    m.sb(k + 0x20, (rng.next() % 7) as u8);
    m.sb(
        k + 0x21,
        pick(rng, &[0, 5, 0xb, 0xe, 0xf, 0x13, 0x14]) as u8,
    );
    m.sb(k + 0x22, (rng.next() % 3) as u8);
    m.sb(k + 0x23, (rng.next() % 3) as u8);
    m.sw(c + 0x130, pick(rng, &[0, 1, 2, 4, 8, 3, 0xc]));
    m.sw(c + 0x12c, pick(rng, &[0, 0, 0x10, 0x20, 0x40]));
    m.sw(c + 0x128, pick(rng, &[0, 0, 0, 1, 0x40]));
    set_cursor(m, 6, pick(rng, &[0, 2]) as u16);
}

#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn key_screen_matches_the_original() {
    let _fallback = fallback();
    let states = options_states(&states());
    let mut total = 0;
    let addresses: [(u32, u32); 18] = [
        (0x800d_3388, 12),
        (0x800d_38ec, 8),
        (0x800d_3be0, 8),
        (0x800d_3fa0, 16),
        (0x800d_4394, 20),
        (0x800d_4430, 24),
        (0x800d_47dc, 40),
        (0x800d_48f8, 40),
        (0x800d_4ca4, 20),
        (0x800d_4d20, 80),
        (0x800d_4f78, 600),
        (0x800d_5bd4, 40),
        (0x800d_5d64, 200),
        (0x800d_60fc, 80),
        (0x800d_65bc, 400),
        (0x800d_6b3c, 100),
        (0x800d_7194, 400),
        (0x800d_7750, 600),
    ];
    for (address, cases) in addresses {
        let changed = oracle_cases(&states, address, cases, false, |m, rng, case| {
            key_screen(
                m,
                rng,
                [0, 1, 2, 3, 0, 1, 3, 4, 5, 0xffff][case as usize % 10],
            );
            [0; 4]
        });
        println!("  {address:08x}: {cases} cases, {changed} RAM bytes changed");
        assert!(changed > 0, "{address:08x}: no case changed RAM");
        total += changed;
    }
    println!("key screen: {total} RAM bytes changed by the originals");
    coverage_summary("key screen");
    assert!(total > 0);
}
