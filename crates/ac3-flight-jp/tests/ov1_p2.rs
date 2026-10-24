//! Validation of JP overlay 1, part 2 (`800D4D54`..`800DCAA4`, the patch
//! files `src/jp/ov1_p2*.rs`) against the JP original.
//!
//! * [`captured_calls_reproduce`]: every captured call of a claimed routine
//!   (`~/ac3-jp/analysis/redux/{boot,save}-002/snapshots-module1.jsonl`) runs
//!   natively from its entry snapshot and must reproduce the return
//!   snapshot (RAM outside the stack, scratchpad, GTE, and `v0` unless the
//!   arm is void), as `snapshot::check_call` does.
//! * [`oracle_matches_original`]: every claimed routine runs on the
//!   original instructions (the test-only oracle) and natively from the same
//!   JP state (module 1 entry snapshots), with randomised arguments and
//!   block fields per routine, and must leave the same RAM, scratchpad, GTE,
//!   `sp`, fault and `v0`. Host calls get the same seeded answers in both
//!   runs.
//!
//! The first half of the module (another part) is not translated yet, so
//! the development interpreter fallback runs it in both kinds of test
//! (feature `dev-fallback`): `cargo test --release -p ac3-flight-jp
//! --features dev-fallback --test ov1_p2 -- --ignored --nocapture`.
#![cfg(feature = "dev-fallback")]
mod support;

use ac3_fallback::{Fallback, Version};
use ac3_flight_jp::{
    Device, HostCall, Machine,
    snapshot::{self, Snapshot, SnapshotInfo},
};
use std::collections::{BTreeMap, BTreeSet};
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use support::{oracle, scan};

/// The part's address range.
const PART: (u32, u32) = (0x800d_4d54, 0x800d_cb00);

/// The capture root (`AC3_JP_CAPTURES`, else `AC3_JP_DIR/analysis/redux`).
fn root() -> PathBuf {
    std::env::var_os("AC3_JP_CAPTURES")
        .map(PathBuf::from)
        .unwrap_or_else(|| scan::jp_dir().join("analysis/redux"))
}

/// The part's claims and the claims whose arm is void (`v0` not modelled).
fn claims() -> (BTreeSet<u32>, BTreeSet<u32>) {
    let (claims, void) =
        scan::claims_and_void(&scan::crate_dir(), &|name| !name.starts_with("jp/ov1_p2"));
    (claims.keys().copied().collect(), void)
}

fn panic_text(e: &(dyn std::any::Any + Send)) -> String {
    e.downcast_ref::<String>()
        .cloned()
        .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default()
}

/// Run `f` with the panic hook silenced; Err(the panic's text).
fn quiet<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    panic::catch_unwind(AssertUnwindSafe(f)).map_err(|e| panic_text(&*e))
}

/// Install the development fallback on this thread (the first half of the
/// module and untranslated resident routines run interpreted).
fn install_fallback() {
    ac3_fallback::install(Fallback::new(Version::Jp));
}

/// The return snapshot matching an entry (`snapshot::check_call`'s rule).
fn exit_of<'a>(index: &'a [SnapshotInfo], entry: &SnapshotInfo) -> Option<&'a SnapshotInfo> {
    index.iter().find(|s| {
        s.callee == entry.callee
            && s.stage == "return"
            && s.sequence > entry.sequence
            && s.ra == entry.ra
            && s.registers.gpr[29] == entry.registers.gpr[29]
    })
}

/// The BIOS kernel's RAM (`0x80000000..0x80010000`: event and file control
/// blocks, the card driver's buffers): host state in the port (`bios.rs`),
/// which no game code reads. The original's kernel work during a captured
/// call (events opened and closed, card and DMA interrupts) is left out of
/// the comparisons and counted apart.
const KERNEL: std::ops::Range<usize> = 0..0x1_0000;

/// SDK driver state that the port's device-facing SDK translations leave to
/// the host (they do not model libgpu's command queue, nor what the
/// interrupt handlers write while a long call runs): libetc's interrupt
/// dispatcher state below its table (0x800B6134) and its VBlank counter
/// (`sdk::VSYNC_COUNTER`), libgpu's queue indices and DMA timeout
/// (`sdk::GPU_QUEUE_WRITE`..`GPU_TIMEOUT_POLLS`), the libspu/libcd driver
/// words (`sdk::SPU_SHADOW`..`CD_TIMEOUT_NAME`) and libgpu's queued
/// commands (BSS 0x800CA400..0x800CB300). Allowed only for the calls of
/// [`DEVICE_HEAVY`], which the oracle test also checks with the same SDK
/// translations on both sides.
const SDK_DRIVER: [std::ops::Range<u32>; 5] = [
    0x800b_5f00..0x800b_6134,
    0x800b_6174..0x800b_6178,
    0x800b_ba8c..0x800b_baa8,
    0x800c_7d00..0x800c_7f00,
    0x800c_a400..0x800c_b300,
];
/// 800D8BA8 / 800D8CEC (TIM uploads: LoadImage queued, DrawSync) and
/// 800DA340 (two SPU reads during XA playback).
const DEVICE_HEAVY: [u32; 3] = [0x800d_8ba8, 0x800d_8cec, 0x800d_a340];

/// libspu's SPU-to-RAM transfer (`[buf, sound RAM address, 64-byte
/// blocks]`) and SpuReadDecodedData's status read (canonical IDs).
const SPU_READ: u32 = 0x8009_dba4;
const SPU_STATUS: u32 = 0x8009_f358;

/// A device that answers selected host calls from a script (per function,
/// in call order), performs the SPU reads with the original's bytes (from
/// `spu_data`, the return snapshot's RAM) and panics after `limit` calls.
struct Scripted {
    answers: BTreeMap<u32, Vec<u32>>,
    spu_data: Option<Vec<u8>>,
    calls: usize,
    limit: usize,
}
impl Device for Scripted {
    fn call(&mut self, ram: &mut [u8], _: &mut [u8], call: HostCall) -> Option<u32> {
        self.calls += 1;
        assert!(
            self.calls <= self.limit,
            "runaway: {} host calls",
            self.limit
        );
        if call.function == SPU_READ
            && let Some(data) = &self.spu_data
        {
            let at = (call.args[0] & 0x1f_ffff) as usize;
            let bytes = (call.args[2] as usize) * 0x40;
            ram[at..at + bytes].copy_from_slice(&data[at..at + bytes]);
        }
        let list = self.answers.get_mut(&call.function)?;
        if list.is_empty() {
            None
        } else {
            Some(list.remove(0))
        }
    }
}

/// One captured call's comparison: (bytes the original changed outside the
/// kernel, kernel bytes that differ) or the cause.
type Replay = Result<(usize, usize), String>;

/// Replay one captured call with the device answers `answers`.
fn replay(
    entry: &Snapshot,
    exit: &Snapshot,
    void: bool,
    answers: BTreeMap<u32, Vec<u32>>,
) -> Replay {
    let mut m = entry.machine();
    m.device = Some(Box::new(Scripted {
        answers,
        spu_data: Some(exit.ram.clone()),
        calls: 0,
        limit: 100_000,
    }));
    let args = [entry.gpr(4), entry.gpr(5), entry.gpr(6), entry.gpr(7)];
    let callee = entry.info.callee;
    let v0 = quiet(|| ac3_flight_jp::dispatch::call(&mut m, callee, args))
        .map_err(|e| format!("panic: {e}"))?;
    if let Err(fault) = m.take_fault() {
        return Err(format!("fault: {fault}"));
    }
    let mut d = snapshot::compare(&m, exit);
    let before = d.ram.len();
    d.ram
        .retain(|&a| !KERNEL.contains(&((a & 0x1f_ffff) as usize)));
    if DEVICE_HEAVY.contains(&callee) {
        d.ram.retain(|a| !SDK_DRIVER.iter().any(|r| r.contains(a)));
    }
    let kernel = before - d.ram.len();
    if !d.is_empty() {
        if std::env::var("OV1_P2_DEBUG")
            .ok()
            .and_then(|v| u32::from_str_radix(&v, 16).ok())
            == Some(callee)
        {
            for &a in d.ram.iter().take(200) {
                let i = (a & 0x1f_ffff) as usize;
                eprintln!(
                    "{a:08x}: entry {:02x} native {:02x} original {:02x}",
                    entry.ram[i],
                    m.ram()[i],
                    exit.ram[i]
                );
            }
            for call in &m.host_calls {
                eprintln!("host {:08x} {:08x?}", call.function, call.args);
            }
        }
        return Err(format!("state differs: {}", d.summary()));
    }
    if !void && v0 != exit.gpr(2) {
        return Err(format!("v0 {v0:#010x}, original {:#010x}", exit.gpr(2)));
    }
    let changed = (KERNEL.end..snapshot::STACK_START)
        .filter(|&i| entry.ram[i] != exit.ram[i])
        .count();
    Ok((changed, kernel))
}

/// The original's host-call results a captured call needs, read from its
/// return snapshot: the file descriptor `open` returned is stored at save
/// block +0x3C; SpuReadDecodedData's status bit 11 selects the half that
/// 800DA2A8 returns (+0x18 set, +0xE0 clear).
fn answers_for(entry: &Snapshot, exit: &Snapshot) -> Vec<BTreeMap<u32, Vec<u32>>> {
    let callee = entry.info.callee;
    let word = |address: u32| {
        let at = (address & 0x1f_ffff) as usize;
        u32::from_le_bytes(exit.ram[at..at + 4].try_into().unwrap())
    };
    let mut answers = BTreeMap::new();
    let block = word(0x800d_da48);
    if (0x800d_5ec4..0x800d_75b4).contains(&callee) && block >> 24 == 0x80 {
        // open (canonical PAL ID 0x800AD808)
        answers.insert(0x800a_d808, vec![word(block.wrapping_add(0x3c))]);
    }
    match callee {
        // OpenEvent (canonical 0x800AD758): the seven descriptors it stored
        0x800d_56e0 => {
            let handles = (0..7)
                .map(|i| word(block.wrapping_add(4 + 4 * i)))
                .collect();
            answers.insert(0x800a_d758, handles);
            vec![answers]
        }
        0x800d_a2a8 => {
            let audio = word(0x800d_da60);
            let set =
                exit.gpr(2) == audio.wrapping_add(0x18) || exit.gpr(2) == audio.wrapping_add(0x418);
            answers.insert(SPU_STATUS, vec![if set { 0x800 } else { 0 }]);
            vec![answers]
        }
        // the level reads twice; the original's two status words are the
        // ones that reproduce its result
        0x800d_a340 => [(0, 0), (0x800, 0x800), (0, 0x800), (0x800, 0)]
            .into_iter()
            .map(|(a, b)| BTreeMap::from([(SPU_STATUS, vec![a, b])]))
            .collect(),
        _ => vec![answers],
    }
}

#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn captured_calls_reproduce() {
    install_fallback();
    let (claimed, void) = claims();
    let hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let (mut passed, mut failed) = (Vec::new(), Vec::new());
    let mut seen = BTreeSet::new();
    for run in ["boot-002", "save-002"] {
        let path = root().join(run).join("snapshots-module1.jsonl");
        let directory = path.parent().unwrap().to_path_buf();
        let index =
            snapshot::read_index(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        for entry in index.iter().filter(|s| s.stage == "entry") {
            let callee = entry.callee;
            if !(PART.0..PART.1).contains(&callee) {
                continue;
            }
            seen.insert(callee);
            if callee == 0x800d_a9d8 {
                // The map's 800DA9D8 is the delay slot of the gate stub at
                // 800DA9D4 (`jr ra; li v0,1`): its captured "entry" is a call
                // of that stub. The stub's translation must reproduce it.
                let Some(exit) = exit_of(&index, entry) else {
                    continue;
                };
                let entry_snapshot = Snapshot::load(&directory, entry).unwrap();
                let exit_snapshot = Snapshot::load(&directory, exit).unwrap();
                let m = entry_snapshot.machine();
                let v0 = ac3_flight_jp::jp::ov1_p2_visual::gate_always();
                let d = snapshot::compare(&m, &exit_snapshot);
                if d.is_empty() && v0 == exit_snapshot.gpr(2) {
                    passed.push(format!(
                        "{run} #{} {callee:08x} (the 800DA9D4 stub): v0 {v0}",
                        entry.sequence
                    ));
                } else {
                    failed.push(format!("{run} #{} 800da9d4 stub differs", entry.sequence));
                }
                continue;
            }
            if !claimed.contains(&callee) {
                failed.push(format!(
                    "{run} #{} {callee:08x}: not claimed",
                    entry.sequence
                ));
                continue;
            }
            let Some(exit) = exit_of(&index, entry) else {
                continue;
            };
            let entry_snapshot = Snapshot::load(&directory, entry).unwrap();
            let exit_snapshot = Snapshot::load(&directory, exit).unwrap();
            let mut result = Err("no device answers".to_string());
            for answers in answers_for(&entry_snapshot, &exit_snapshot) {
                result = replay(
                    &entry_snapshot,
                    &exit_snapshot,
                    void.contains(&callee),
                    answers,
                );
                if result.is_ok() {
                    break;
                }
            }
            match result {
                Ok((n, kernel)) => passed.push(format!(
                    "{run} #{} {callee:08x}: {n} bytes changed, {kernel} kernel or driver bytes apart",
                    entry.sequence
                )),
                Err(e) => failed.push(format!("{run} #{} {callee:08x}: {e}", entry.sequence)),
            }
        }
    }
    panic::set_hook(hook);
    ac3_fallback::uninstall();
    println!("captured routines of the part: {}", seen.len());
    for line in &passed {
        println!("  pass {line}");
    }
    for line in &failed {
        println!("  FAIL {line}");
    }
    println!("{} pass, {} fail", passed.len(), failed.len());
    assert!(failed.is_empty(), "{} captured calls differ", failed.len());
    assert!(passed.len() > 50);
}

// ---- the oracle differential --------------------------------------------------------------

/// The part's blocks (`ov1_p2.rs`) and globals.
const SAVE: u32 = 0x800d_da48;
const TEXT: u32 = 0x800d_da58;
const VISUAL: u32 = 0x800d_d9dc;
const AUDIO: u32 = 0x800d_da60;
const MENU_STATE: u32 = 0x800d_d9d0;
const GLYPHS: u32 = 0x800d_da50;
const RECT: u32 = 0x800d_d9d8;
const DISC: u32 = 0x800d_d910;
/// A RAM area the cases use for their records, packets and strings (alike
/// in both runs).
const TEST: u32 = 0x801e_0000;
/// The gate at 800DA9DC (no map function start: called directly).
const GATE_CLIP: (u32, u32) = (0x800d_a9dc, 0x800d_ac98);
/// The routines that return ExitCriticalSection's `v0`, which the kernel
/// leaves as the caller had it (`bios::exit_critical_section`): the
/// oracle's calls go through `dispatch`, which cannot pass `v0` in, so its
/// side reads 0 there. Their `v0` is checked by the captures instead.
const KEEPS_V0: [u32; 2] = [0x800d_56e0, 0x800d_58b0];
/// Random variants per base state, and the number of base states.
const VARIANTS: u32 = 8;
const STATES: usize = 10;

/// A deterministic generator whose small residues are independent (the
/// oracle's LCG output mixed through a 32-bit hash: its own low bits
/// repeat with a short period, which `% 2` choices would follow).
struct Mix(u32);
impl Mix {
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(1_103_515_245).wrapping_add(12345);
        let mut x = self.0;
        x ^= x >> 16;
        x = x.wrapping_mul(0x7feb_352d);
        x ^= x >> 15;
        x = x.wrapping_mul(0x846c_a68b);
        x ^ (x >> 16)
    }
}

/// Seeded host answers, the same sequence in both runs of a case: `open`
/// a small descriptor or -1, TestEvent and `_card_format` 0 or 1,
/// firstfile / nextfile the entry or 0, SpuReadDecodedData's status 0 or
/// bit 11; other calls their defaults. A runaway (20000 calls) panics.
struct Answers {
    rng: Mix,
    calls: usize,
}
impl Device for Answers {
    fn call(&mut self, _: &mut [u8], _: &mut [u8], call: HostCall) -> Option<u32> {
        self.calls += 1;
        assert!(self.calls <= 20_000, "runaway: 20000 host calls");
        let r = self.rng.next();
        Some(match call.function {
            0x800a_d808 => {
                if r % 10 < 7 {
                    r % 4
                } else {
                    u32::MAX
                }
            }
            0x800a_d788 | 0x800a_bc38 => r % 2,
            0x800a_da08 => {
                if r % 10 < 6 {
                    call.args[1]
                } else {
                    0
                }
            }
            0x800a_d848 => {
                if r.is_multiple_of(2) {
                    call.args[0]
                } else {
                    0
                }
            }
            SPU_STATUS => (r % 2) * 0x800,
            _ => return None,
        })
    }
}

fn answers(seed: u32) -> Box<Answers> {
    Box::new(Answers {
        rng: Mix(seed),
        calls: 0,
    })
}

/// The native side of a case: the claim through `dispatch`, or a routine
/// that has no map function start, called directly.
#[derive(Clone, Copy)]
enum Native {
    Dispatch(u32),
    Direct(fn(&mut Machine) -> u32),
}

/// One case: Ok(RAM bytes the original changed) or Err(cause).
fn case(
    base: &Machine,
    extent: (u32, u32),
    native: Native,
    args: [u32; 4],
    void: bool,
    seed: u32,
) -> Result<usize, String> {
    let sp = base.sp;
    let mut original = base.clone();
    original.device = Some(answers(seed));
    let v0 = quiet(|| oracle::run(&mut original, extent.0, extent.1, args))
        .map_err(|e| format!("oracle stopped: {e}"))?;
    let mut nat = base.clone();
    nat.device = Some(answers(seed));
    let nv0 = quiet(|| match native {
        Native::Dispatch(target) => ac3_flight_jp::dispatch::call(&mut nat, target, args),
        Native::Direct(f) => f(&mut nat),
    })
    .map_err(|e| format!("native panic: {e}"))?;
    let (fo, fnat) = (original.take_fault(), nat.take_fault());
    if fo != fnat {
        return Err(format!("faults: original {fo:?}, native {fnat:?}"));
    }
    let dead = snapshot::STACK_START..(sp & 0x1f_ffff) as usize;
    let ram: Vec<u32> = (0..0x20_0000usize)
        .filter(|i| !dead.contains(i) && original.ram()[*i] != nat.ram()[*i])
        .map(|i| 0x8000_0000 + i as u32)
        .collect();
    let scratch = (0..0x400)
        .filter(|&i| original.scratch()[i] != nat.scratch()[i])
        .count();
    let gte = original.gte == nat.gte;
    if !ram.is_empty() || scratch != 0 || !gte || original.sp != nat.sp {
        return Err(format!(
            "state differs: ram {:08x?} ({} bytes) scratch {scratch} gte equal {gte} sp {:x}/{:x}",
            &ram[..ram.len().min(8)],
            ram.len(),
            original.sp,
            nat.sp
        ));
    }
    if !void && v0 != nv0 {
        return Err(format!("v0: original {v0:#x}, native {nv0:#x}"));
    }
    Ok((0..0x20_0000usize)
        .filter(|i| !dead.contains(i) && base.ram()[*i] != original.ram()[*i])
        .count())
}

/// The oracle's interpreter (`support::oracle::run`) with the routine's
/// executed instruction addresses recorded: the cases' coverage.
fn traced(m: &mut Machine, (start, end): (u32, u32), args: [u32; 4], hits: &mut BTreeSet<u32>) {
    const RETURN: u32 = 0xffff_fff0;
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
            return;
        }
        if pc < start || pc >= end {
            m.sp = r[29];
            r[2] = ac3_flight_jp::dispatch::call(m, pc, [r[4], r[5], r[6], r[7]]);
            pc = r[31];
            next = pc + 4;
            continue;
        }
        hits.insert(pc);
        steps += 1;
        if steps >= 10_000_000 {
            return;
        }
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
                    lo = p as u32;
                    hi = (p >> 32) as u32;
                }
                0x19 => {
                    let p = u64::from(a) * u64::from(b);
                    lo = p as u32;
                    hi = (p >> 32) as u32;
                }
                0x1a => {
                    let (n, d) = (a as i32, b as i32);
                    if d == 0 {
                        lo = if n < 0 { 1 } else { u32::MAX };
                        hi = a;
                    } else {
                        lo = n.wrapping_div(d) as u32;
                        hi = n.wrapping_rem(d) as u32;
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
                _ => {
                    // break (and anything else): the run stops here
                    m.sp = r[29];
                    return;
                }
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
            0x20 => out = Some((rt, m.lb(a.wrapping_add(simm)) as i32 as u32)),
            0x21 => out = Some((rt, m.lh(a.wrapping_add(simm)) as i32 as u32)),
            0x23 => out = Some((rt, m.lw(a.wrapping_add(simm)))),
            0x24 => out = Some((rt, u32::from(m.lbu(a.wrapping_add(simm))))),
            0x25 => out = Some((rt, u32::from(m.lhu(a.wrapping_add(simm))))),
            0x28 => m.sb(a.wrapping_add(simm), b as u8),
            0x29 => m.sh(a.wrapping_add(simm), b as u16),
            0x2b => m.sw(a.wrapping_add(simm), b),
            0x22 | 0x26 | 0x2a | 0x2e => {
                let addr = a.wrapping_add(simm);
                let aligned = addr & !3;
                let shift = (addr & 3) as usize;
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
            _ => {
                m.sp = r[29];
                return;
            }
        }
        if let Some((i, v)) = out
            && i != 0
        {
            r[i] = v;
        }
    }
}

fn lbu_of(m: &mut Machine, address: u32) -> u32 {
    u32::from(m.lbu(address))
}
fn pick(rng: &mut Mix, values: &[u32]) -> u32 {
    values[rng.next() as usize % values.len()]
}
fn fill(m: &mut Machine, address: u32, bytes: u32, rng: &mut Mix) {
    for i in 0..bytes {
        m.sb(address + i, rng.next() as u8);
    }
}
fn write_string(m: &mut Machine, address: u32, text: &[u8]) {
    m.write_bytes(address, text);
    m.sb(address + text.len() as u32, 0);
}
/// XOR of the record checksum seed and the `words` words after the first.
fn checksum(m: &mut Machine, base: u32, words: u32) -> u32 {
    (1..=words).fold(0x4143_4533, |x, i| x ^ m.lw(base + 4 * i))
}

/// The save block's card and transfer fields at random (the probe's steps,
/// events and retries, the transfer flags, the account and record bits;
/// valid checksums half the time); `ready`: the probe often idle (the
/// first half's 800D3D60 then reports ready).
fn random_card_state(m: &mut Machine, rng: &mut Mix, ready: bool) {
    let save = m.lw(SAVE);
    m.sw(save + 0x354, rng.next() % 2);
    for s in 0..2 {
        let step = if ready && rng.next().is_multiple_of(2) {
            0
        } else {
            rng.next() % 10
        };
        m.sb(save + 0x34c + s, step as u8);
        m.sb(save + 0x34e + s, pick(rng, &[0, 1, 2, 3, 4, 0xff]) as u8);
        m.sb(save + 0x348 + s, (rng.next() % 2) as u8);
        m.sb(save + 0x34a + s, (rng.next() % 2) as u8);
        m.sb(save + 0x350 + s, pick(rng, &[0, 0x10, 0x11, 0x12]) as u8);
        m.sh(save + 0x2c + 2 * s, (rng.next() % 5) as u16);
        m.sh(
            save + 0x30 + 2 * s,
            pick(rng, &[0, 1, 2, 3, 4, 0xffff]) as u16,
        );
    }
    m.sw(save + 0x28, rng.next() % 3);
    m.sw(save + 0x318, pick(rng, &[u32::MAX, 0, 3]));
    m.sb(save + 0x336, (rng.next() % 2) as u8);
    m.sh(save + 0x35a, (rng.next() % 2) as u16);
    m.sh(save + 0x358, (rng.next() % 5) as u16);
    m.sh(save + 0x50, (rng.next() % 8) as u16);
    let flags = m.lw(save + 0x2e8);
    m.sb(flags + 8, (rng.next() % 8) as u8);
    if rng.next().is_multiple_of(2) {
        m.sw(flags + 4, 0x0099_0426);
        let sum = checksum(m, flags, 0x1f);
        m.sw(flags, sum);
    }
    for i in 0..4 {
        let record = m.lw(save + 0x2ec + 4 * i);
        m.sh(record + 0x16, (rng.next() % 0x80) as u16);
        if rng.next().is_multiple_of(2) {
            let sum = checksum(m, record, 0x11f);
            m.sw(record, sum);
        }
    }
    for i in 0..7 {
        let record = m.lw(save + 0x2fc + 4 * i);
        if rng.next().is_multiple_of(2) {
            let sum = checksum(m, record, 0x5f);
            m.sw(record, sum);
        }
    }
}

/// An animation (times, gate, timer, bin) at `TEST` for the visual
/// commands and gates, the timers at random, the clip rectangle on the
/// scratchpad at random and the packet cursor at `TEST + 0x400`.
fn random_animation(m: &mut Machine, rng: &mut Mix) {
    let visual = m.lw(VISUAL);
    let start = rng.next() % 20;
    let middle = start + rng.next() % 20;
    let end = middle + rng.next() % 20;
    for (offset, value) in [
        (0, rng.next() % 300),
        (2, rng.next() % 240),
        (4, start),
        (6, middle),
        (8, end),
        (0xa, rng.next() % 4),
        (0xc, rng.next() % 16),
        (0xe, rng.next() % 16),
    ] {
        m.sh(TEST + offset, value as u16);
    }
    m.sw(visual + 0x2c, TEST);
    for i in 0..16 {
        let values = m.lw(visual + 0x40);
        m.sh(values + 2 * i, (rng.next() % 64) as u16);
        let limits = m.lw(visual + 0x44);
        m.sh(limits + 2 * i, (rng.next() % 64) as u16);
        let states = m.lw(visual + 0x48);
        m.sh(states + 2 * i, (rng.next() % 4) as u16);
    }
    m.sw(RECT, 0x1f80_0000);
    for k in 0..4 {
        m.sh(0x1f80_0000 + 2 * k, (rng.next() % 320) as u16);
    }
    m.sw(visual + 4, TEST + 0x400);
}

/// A random case of routine `target` on `m`: its arguments (the state it
/// reads set at random); `None`: the routine runs on its captured
/// arguments only.
fn setup(target: u32, m: &mut Machine, rng: &mut Mix) -> Option<[u32; 4]> {
    let save = m.lw(SAVE);
    let text = m.lw(TEXT);
    let visual = m.lw(VISUAL);
    let small = rng.next() % 8;
    let r = [rng.next(), rng.next(), rng.next(), rng.next()];
    fill(m, TEST, 0x600, rng);
    let args = match target {
        // the save block
        0x800d_4d6c => [pick(rng, &[u32::MAX, 0, 1, 2, 3, 7]), 0, 0, 0],
        0x800d_4df4 | 0x800d_5a70 | 0x800d_5ac8 | 0x800d_5bf4 | 0x800d_5c2c => {
            let index = rng.next() % 7;
            let record = m.lw(save + 0x2fc + 4 * index);
            m.sb(record + 0x112, (rng.next() % 5) as u8);
            m.sb(record + 0x113, (rng.next() % 52) as u8);
            let time = pick(
                rng,
                &[
                    0,
                    59,
                    1799,
                    107_999,
                    0x066f_f2ff,
                    0x0670_0000,
                    0x8000_0000,
                    r[0],
                ],
            );
            m.sw(record + 0x114, time);
            let high = if rng.next().is_multiple_of(4) {
                0x100
            } else {
                0
            };
            [index | high, 0, 0, 0]
        }
        0x800d_4ff0 | 0x800d_8ba8 | 0x800d_8cec | 0x800d_c5ec | 0x800d_8c30 | 0x800d_8d64
        | 0x800d_8e38 => return None,
        0x800d_5970 => [pick(rng, &[u32::MAX, 0, 1, 2, 3, 4, 5]), 0, 0, 0],
        0x800d_59c0 => [pick(rng, &[u32::MAX, 0, 1, 2]), rng.next() % 17, 0, 0],
        0x800d_5a38 => [pick(rng, &[u32::MAX, 0, 1, 2, 3, 4]), 0, 0, 0],
        0x800d_5cc0 => {
            let index = rng.next() % 3;
            let record = m.lw(save + 0x2ec + 4 * index);
            m.sb(record + 0x464, (rng.next() % 2) as u8);
            [index, 0, 0, 0]
        }
        0x800d_5d54 | 0x800d_5d6c | 0x800d_5d84 | 0x800d_5d9c => [r[0], 0, 0, 0],
        0x800d_5db4 => {
            let flags = m.lw(save + 0x2e8);
            m.sb(flags + 8, small as u8);
            let ctx = m.lw(ac3_flight_jp::GP + 0x2ec);
            for i in 0..3 {
                if rng.next().is_multiple_of(2) {
                    let record = m.lw(save + 0x2ec + 4 * i);
                    let name = m.read_bytes(ctx + 0x948, 12);
                    m.write_bytes(record + 8, &name);
                    let id = if rng.next().is_multiple_of(2) {
                        m.lw(ctx + 0x968)
                    } else {
                        r[1]
                    };
                    m.sw(record + 4, id);
                }
            }
            [0; 4]
        }
        0x800d_5e74 | 0x800d_5ec4 | 0x800d_64c4 => {
            random_card_state(m, rng, true);
            [rng.next() % 2, 0, 0, 0]
        }
        0x800d_5f6c => {
            random_card_state(m, rng, false);
            [0; 4]
        }
        0x800d_6604 | 0x800d_6814 | 0x800d_6d78 => {
            random_card_state(m, rng, true);
            [rng.next() % 2, rng.next() % 3, 0, 0]
        }
        0x800d_6ab4 => {
            random_card_state(m, rng, true);
            [rng.next() % 2, rng.next() % 3, rng.next() % 3, 0]
        }
        0x800d_6f04 => {
            random_card_state(m, rng, true);
            [
                rng.next() % 2,
                pick(rng, &[0, 1, 2, u32::MAX]),
                rng.next() % 3,
                0,
            ]
        }
        0x800d_713c | 0x800d_73b8 => {
            random_card_state(m, rng, true);
            [
                rng.next() % 2,
                rng.next() % 3,
                rng.next() % 8,
                rng.next() % 3,
            ]
        }
        0x800d_75b4 => {
            write_string(m, TEST, b"Pixy");
            [TEST, 0, 0, 0]
        }
        // the text and glyphs
        0x800d_7644 => [rng.next() % 0x400, rng.next() % 0x200, 0, 0],
        0x800d_783c => [TEST, 0, 0, 0],
        0x800d_7868 => {
            let n = rng.next() % 3;
            m.sb(TEST + 2 * n, 0);
            [TEST, 0, 0, 0]
        }
        0x800d_7a58 | 0x800d_7c18 => {
            m.sw(GLYPHS, TEST + 0x100);
            m.sw(TEST + 0x100, TEST + 0x200);
            m.sw(TEST + 0x104, TEST + 0x380);
            m.sw(TEST + 0x108, TEST + 0x200);
            let sp = m.sp;
            m.sw(sp + 0x10, TEST + 0x500);
            [
                r[0] % 320,
                r[1] % 240,
                (rng.next() % 40).wrapping_sub(8),
                rng.next() % 5,
            ]
        }
        0x800d_7c40 | 0x800d_7ce4 | 0x800d_7c60 => [r[0] % 400, r[1] % 300, r[2] % 300, 0],
        0x800d_7c88 => [pick(rng, &[0xffff, u32::MAX, 0, 1, 2, 3]), 0, 0, 0],
        0x800d_7cd4 => [r[0] % 64, 0, 0, 0],
        0x800d_7d08 => [rng.next() % 9, 0, 0, 0],
        0x800d_8094 => [TEST, TEST + 2, 0, 0],
        0x800d_81f0 | 0x800d_c9ec => [TEST + 0x500, 0, 0, 0],
        0x800d_82a4 => {
            m.sb(text + 0x43, (rng.next() % 2) as u8);
            for offset in [0x44, 0x46, 0x48] {
                m.sh(text + offset, (rng.next() % 10) as u16);
            }
            m.sb(text + 0x3b, (rng.next() % 2) as u8);
            m.sb(text + 0x40, (rng.next() % 2) as u8);
            if rng.next().is_multiple_of(4) {
                // a texture position left of / above the page origin
                m.sh(text + 0x30, pick(rng, &[0xffc0, 0xff00, 0xfffe]) as u16);
                m.sh(text + 0x32, pick(rng, &[0xff00, 0xfff0, 0x80]) as u16);
            }
            [0x20 + rng.next() % 0x60, 0, 0, 0]
        }
        0x800d_8980 => {
            for i in 0..24 {
                let c = pick(rng, &[0xa, 9, 0x20, 0x41, 0x52, 0x7a]);
                m.sb(TEST + i, c as u8);
            }
            m.sw(text + 0x10, rng.next() % 24);
            [TEST, 0, 0, 0]
        }
        0x800d_8aa0 => {
            write_string(m, TEST, b"LV%d\t%x\nok");
            [TEST, r[0] % 1000, r[1], 0]
        }
        // loaders and primitives
        0x800d_8af0 => {
            write_string(m, TEST, b"bu00:");
            write_string(m, TEST + 0x40, b"BISLPSP02020");
            [TEST, TEST + 0x40, rng.next() % 0x100, TEST + 0x80]
        }
        0x800d_8f3c | 0x800d_9024 | 0x800d_90e8 | 0x800d_9140 | 0x800d_9230 | 0x800d_9294
        | 0x800d_939c | 0x800d_9400 | 0x800d_94c4 | 0x800d_9624 | 0x800d_9748 => {
            // the blend flags of the records (+0xB, +0xF, +0x13) clear half
            // the time, and texture positions below zero for POLY_FT4
            for offset in [0xb, 0xf, 0x13] {
                if rng.next().is_multiple_of(2) {
                    m.sb(TEST + offset, 0);
                }
            }
            if rng.next().is_multiple_of(3) {
                m.sh(TEST + 0x20, pick(rng, &[0xffc0, 0xff01, 0x40]) as u16);
                m.sh(TEST + 0x22, pick(rng, &[0xff00, 0xfff1, 0x100]) as u16);
            }
            [TEST, TEST + 0x100, TEST + 0x500, 0]
        }
        0x800d_5290 | 0x800d_5528 | 0x800d_5550 | 0x800d_5880 => {
            let ctx = m.lw(ac3_flight_jp::GP + 0x2ec);
            m.sw(ctx + 0x120, rng.next() % 8);
            m.sb(ctx + 0x39c, (rng.next() % 2) as u8);
            m.sb(ctx + 0x95a, (rng.next() % 3) as u8);
            [0; 4]
        }
        0x800d_7600 => {
            random_card_state(m, rng, true);
            [0; 4]
        }
        0x800d_9a08 => [r[0] % 320, r[1] % 240, r[2] % 0x2000, TEST + 0x100],
        0x800d_a0e0 => [r[0] % 10_000, 0, 0, 0],
        0x800d_a2a8 => [rng.next() % 4, 0, 0, 0],
        0x800d_a418 => {
            m.sw(TEST, rng.next() % 4);
            [TEST, 0, 0, 0]
        }
        0x800d_a49c => [r[0] & 0xff, 0, 0, 0],
        0x800d_a4dc | 0x800d_a4f4 | 0x800d_a504 | 0x800d_a520 | 0x800d_a694 | 0x800d_a6b8 => {
            let block = m.lw(ac3_flight_jp::GP + 0x34);
            m.sw(block + 0xc, rng.next() % 7);
            m.sw(block, rng.next() % 5);
            m.sw(block + 4, rng.next() % 4);
            let disc = m.lw(DISC);
            m.sw(disc + 8, rng.next() % 12);
            m.sw(disc + 4, rng.next() % 2);
            [rng.next() % 12, 0, 0, 0]
        }
        // the visual block
        0x800d_a714 => {
            let sp = m.sp;
            m.sw(sp + 0x10, r[3]);
            [rng.next() % 0x24, r[0], r[1], r[2]]
        }
        0x800d_a77c | 0x800d_a800 => [rng.next() % 0x24, 0, 0, 0],
        0x800d_a7a8 => [rng.next() % 0x20, 0, 0, 0],
        0x800d_a828 => [r[0], 0, 0, 0],
        0x800d_a838 => [
            rng.next() % 0x12,
            rng.next() % 4,
            pick(rng, &[u32::MAX, 0, 5, r[0] % 100]),
            pick(rng, &[u32::MAX, 0, 9, r[1] % 100]),
        ],
        0x800d_a898 | 0x800d_a8bc => [rng.next() % 16, 0, 0, 0],
        0x800d_a9a8 => [rng.next() % 0x44, r[0], 0, 0],
        0x800d_a8e0 | 0x800d_ac98 | 0x800d_acd0 | 0x800d_a9dc => {
            random_animation(m, rng);
            [0; 4]
        }
        0x800d_ae44 | 0x800d_b024 | 0x800d_b254 | 0x800d_b374 | 0x800d_b834 | 0x800d_b910
        | 0x800d_b928 => {
            random_animation(m, rng);
            let args = TEST + 0x40;
            for k in 0..0x10 {
                m.sh(args + 2 * k, (rng.next() % 0x140) as u16);
            }
            // the condition halfwords (commands 0, 1, 3 and 6) small
            for offset in [0xc, 0x14, 0x18] {
                let index = if rng.next().is_multiple_of(2) {
                    0
                } else {
                    rng.next() % 64
                };
                m.sh(args + offset, index as u16);
                m.sh(args + offset + 2, (rng.next() % 2) as u16);
            }
            m.sw(args + 0x10, rng.next() % 16);
            m.sb(args + 7, pick(rng, &[0, 1, 2, 0xff]) as u8);
            m.sb(args + 0xb, (rng.next() % 2) as u8);
            m.sb(args + 0x1e, (rng.next() % 2) as u8);
            // command 3's sizes: negative (mirrored) or equal to the
            // texture size in pixels
            let scale = if lbu_of(m, args + 0x1e) != 0 { 2 } else { 4 };
            let tw = rng.next() % 0x40;
            m.sh(args + 0x10, tw as u16);
            let w = pick(rng, &[0x10, 0xfff0, 0x20, tw * scale]);
            m.sh(args + 0x14, w as u16);
            let th = rng.next() % 0x40;
            m.sh(args + 0x12, th as u16);
            let h = pick(rng, &[0x10, 0xffe0, th]);
            m.sh(args + 0x16, h as u16);
            m.sh(args + 0xc, pick(rng, &[0xffc0, 0x3f, 0x40, 0x7f]) as u16);
            m.sh(args + 0xe, pick(rng, &[0xff00, 0xff, 0x100, 0x80]) as u16);
            [args, TEST + 0x500, TEST + 0x5c0, 0]
        }
        0x800d_bd20 | 0x800d_bf6c => {
            m.sw(visual + 0x54, rng.next() % 16);
            [r[0] % 320, r[1] % 240, rng.next() % 5, rng.next() % 3]
        }
        0x800d_bfe4 | 0x800d_bb30 => {
            m.sw(visual + 0x54, rng.next() % 16);
            [0; 4]
        }
        0x800d_c028 => [rng.next() % 4, 0, 0, 0],
        0x800d_c098 => {
            m.sw(visual + 0x54, rng.next() % 16);
            let flags = m.lw(save + 0x2e8);
            m.sb(flags + 8, small as u8);
            let state = m.lw(MENU_STATE);
            m.sb(state + 0x5d, (rng.next() % 4) as u8);
            [rng.next() % 3, rng.next() % 3, 0, 0]
        }
        0x800d_c6c4 => {
            let bits = m.lw(visual + 0x1c);
            let set: Vec<u32> = (0..32).filter(|i| bits >> i & 1 != 0).collect();
            if set.is_empty() {
                return None;
            }
            [set[rng.next() as usize % set.len()], 0, 0, 0]
        }
        _ => [r[0] % 8, r[1] % 8, r[2] % 8, r[3] % 8],
    };
    Some(args)
}

/// Captured cases built for the loaders no capture reaches, from the
/// captured call of 800D8CEC (a TIM in RAM): 800D8C30 a one-entry
/// directory of it at `TEST`, 800D8D64 the TIM at a position, 800D8E38 the
/// TIM with CLUT and pixel buffers at `TEST`.
fn loader_cases(target: u32, m: &mut Machine, tim: u32, rng: &mut Mix) -> [u32; 4] {
    match target {
        0x800d_8c30 => {
            m.sw(TEST, 1);
            m.sw(TEST + 4, tim.wrapping_sub(TEST));
            [TEST, 0, 0, 0]
        }
        0x800d_8d64 => {
            let sp = m.sp;
            m.sw(sp + 0x10, rng.next() % 0x100);
            [
                tim,
                rng.next() % 0x400,
                rng.next() % 0x200,
                rng.next() % 0x100,
            ]
        }
        _ => [tim, TEST + 0x100, TEST + 0x200, 0],
    }
}

/// One routine's verdict over its cases: (cases, RAM bytes changed,
/// instructions executed) or the first failing case.
type Verdict = Result<(usize, usize, usize), String>;

#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn oracle_matches_original() {
    let (claimed, void) = claims();
    let rows: Vec<scan::Row> = scan::jp_map(&scan::jp_dir())
        .into_iter()
        .filter(|r| r.region == "ov1" && (PART.0..PART.1).contains(&r.jp))
        .collect();
    // base states: module 1 entry snapshots with every block allocated
    let mut captured: BTreeMap<u32, Vec<Snapshot>> = BTreeMap::new();
    let mut states = Vec::new();
    for run in ["boot-002", "save-002"] {
        let path = root().join(run).join("snapshots-module1.jsonl");
        let index = snapshot::read_index(&path).unwrap();
        let directory = path.parent().unwrap();
        for entry in index.iter().filter(|s| s.stage == "entry") {
            let snapshot = Snapshot::load(directory, entry).unwrap();
            let mut m = snapshot.machine();
            let ready = [SAVE, TEXT, VISUAL, AUDIO, MENU_STATE, 0x800d_da68]
                .iter()
                .all(|&a| m.lw(a) >> 24 == 0x80);
            if ready {
                states.push(snapshot.clone());
            }
            if (PART.0..PART.1).contains(&entry.callee) {
                captured.entry(entry.callee).or_default().push(snapshot);
            }
        }
    }
    let step = (states.len() / STATES).max(1);
    let states: Vec<Snapshot> = states.into_iter().step_by(step).take(STATES).collect();
    let tim = captured[&0x800d_8cec][0].gpr(4);
    let tim_state = captured[&0x800d_8cec][0].clone();
    // every claim, and the gate at 800DA9DC called directly
    let mut routines: Vec<(u32, (u32, u32), Native)> = rows
        .iter()
        .filter(|r| claimed.contains(&r.jp))
        .map(|r| (r.jp, (r.jp, r.jp + r.size), Native::Dispatch(r.jp)))
        .collect();
    routines.push((
        GATE_CLIP.0,
        GATE_CLIP,
        Native::Direct(ac3_flight_jp::jp::ov1_p2_visual::gate_clip),
    ));
    let hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let results: Vec<(u32, u32, Verdict, BTreeSet<u32>)> = std::thread::scope(|scope| {
        let chunks: Vec<_> = routines.chunks(routines.len().div_ceil(4)).collect();
        let handles: Vec<_> = chunks
            .into_iter()
            .map(|chunk| {
                let (states, captured, void, tim_state) = (&states, &captured, &void, &tim_state);
                scope.spawn(move || {
                    install_fallback();
                    let mut out = Vec::new();
                    for &(address, extent, native) in chunk {
                        let void = void.contains(&address) || KEEPS_V0.contains(&address);
                        let mut hits = BTreeSet::new();
                        let mut verdict: Verdict = Ok((0, 0, 0));
                        let mut run = |base: &Machine, args: [u32; 4], seed: u32, label: &str| {
                            let Ok((n, changed, _)) = verdict else {
                                return;
                            };
                            match case(base, extent, native, args, void, seed) {
                                Ok(bytes) => {
                                    verdict = Ok((n + 1, changed + bytes, 0));
                                    let mut m = base.clone();
                                    m.device = Some(answers(seed));
                                    let _ = quiet(|| traced(&mut m, extent, args, &mut hits));
                                }
                                Err(e) => verdict = Err(format!("{label} args {args:08x?}: {e}")),
                            }
                        };
                        for (k, entry) in captured.get(&address).into_iter().flatten().enumerate() {
                            let args = [entry.gpr(4), entry.gpr(5), entry.gpr(6), entry.gpr(7)];
                            run(&entry.machine(), args, k as u32, "captured");
                        }
                        // the card routines' many branches get more variants
                        let variants = if (0x800d_5e74..0x800d_75b4).contains(&address) {
                            4 * VARIANTS
                        } else {
                            VARIANTS
                        };
                        for (s, state) in states.iter().enumerate() {
                            for variant in 0..variants {
                                let seed = address ^ (variant << 20) ^ ((s as u32) << 26);
                                let mut rng = Mix(seed);
                                let mut base = state.machine();
                                if let Some(args) = setup(address, &mut base, &mut rng) {
                                    run(&base, args, seed, &format!("state {s} variant {variant}"));
                                }
                            }
                        }
                        if matches!(address, 0x800d_8c30 | 0x800d_8d64 | 0x800d_8e38) {
                            for variant in 0..VARIANTS {
                                let mut rng = Mix(address ^ variant);
                                let mut base = tim_state.machine();
                                let args = loader_cases(address, &mut base, tim, &mut rng);
                                run(&base, args, variant, &format!("TIM variant {variant}"));
                            }
                        }
                        let size = (extent.1 - extent.0) / 4;
                        out.push((address, size, verdict, hits));
                    }
                    ac3_fallback::uninstall();
                    out
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect()
    });
    panic::set_hook(hook);
    let (mut pass, mut fail, mut total_cases) = (0, Vec::new(), 0);
    let (mut covered, mut instructions) = (0usize, 0usize);
    for (address, size, verdict, hits) in &results {
        covered += hits.len();
        instructions += *size as usize;
        match verdict {
            Ok((cases, bytes, _)) => {
                pass += 1;
                total_cases += cases;
                let missing: Vec<String> = (0..*size)
                    .map(|i| address + 4 * i)
                    .filter(|a| !hits.contains(a))
                    .map(|a| format!("{a:08x}"))
                    .collect();
                println!(
                    "  {address:08x}: {cases:4} cases, {bytes:8} bytes changed, {}/{size} instructions{}",
                    hits.len(),
                    if missing.is_empty() {
                        String::new()
                    } else {
                        format!(
                            " (not run: {}{})",
                            missing[..missing.len().min(6)].join(" "),
                            if missing.len() > 6 { " ..." } else { "" }
                        )
                    }
                );
            }
            Err(e) => fail.push(format!("{address:08x}: {e}")),
        }
    }
    for line in &fail {
        println!("  FAIL {line}");
    }
    println!(
        "oracle: {} routines, {pass} pass over {total_cases} cases, {} fail; instructions run {covered}/{instructions}",
        results.len(),
        fail.len()
    );
    assert!(
        fail.is_empty(),
        "{} routines differ from the original",
        fail.len()
    );
}
