//! Briefing sub-mode of overlay module 2 (`ov2_briefing`,
//! `ov2_briefing_scene`) on original function snapshots. Every translated
//! routine runs on a captured entry state (overlay module 2 loaded) and must
//! reproduce the captured return state exactly: all of RAM except the dead
//! frames below the entry `$sp` (the caller's frames above it are compared),
//! the scratchpad, every GTE register, and `v0` where the routine returns
//! one.
//!
//! Captures (`.ov2brief/redux-ov2-snapshots.lua`, inputs of the
//! `analysis/menu-runtime/briefing-wrapper-*` captures from
//! `mission-002/final.sav`; tick frames are capture frames):
//! - `ov2brief-snap-fast-002`: the initialization (frame 29) and ticks of
//!   the START fast-forward run: panels, zoom, fly-in, script commands, the
//!   fast-forward (frame 201), finish, label map with LEFT/RIGHT, cancel;
//! - `ov2brief-snap-normal-001`: the script without input: text lines and
//!   pages, labels with sound, label slides, the script end;
//! - `ov2brief-snap-confirm-003`: CROSS presses: panel skip, line skips,
//!   CROSS fast-forward, finish skip, confirm and the forward exit;
//! - `ov2brief-snap-nav-001`: the fast run's LEFT presses on the label map
//!   (frames 351, 401);
//! - `ov2brief-snap-page-001`: the normal run with CROSS at frame 425: a
//!   full text page skipped (frames 425..427);
//! - `ov2brief-snap-cancel-001`: TRIANGLE+START+CROSS during the script
//!   (from `briefing-wrapper-fast-001/frame-00180.sav`): cancel, then 3,0.
//!
//! Two modes per occurrence:
//! - **replay**: calls into other modules that the capture recorded (the
//!   ov2core routines and the SDK routines TransMatrix, strlen and sprintf;
//!   `effects.jsonl`) are checked and replaced by the original callee's
//!   recorded effect: target, used register and stack arguments, `$sp`
//!   (frame placement), the GTE registers and a hash of RAM below the stack
//!   at the call must equal the original's; then the callee's RAM,
//!   scratchpad and GTE writes and its `v0` are applied. This validates this
//!   module's code exactly, independently of its callees.
//! - **native**: everything runs through `dispatch`. An occurrence that
//!   reaches a routine nobody translates yet is reported as BLOCKED; any
//!   other fault or difference fails.
use ac3_flight::{
    Fault, Machine,
    machine::{RAM_BYTES, SCRATCH_BYTES, STACK_LIMIT},
    ov2_briefing::{self, ExternalReplay},
    snapshot::{self, Snapshot},
};
use serde_json::Value;
use std::{
    cell::RefCell,
    collections::VecDeque,
    fs,
    path::{Path, PathBuf},
    rc::Rc,
};

const FAST: &str = "ov2brief-snap-fast-002";
const NORMAL: &str = "ov2brief-snap-normal-001";
const CONFIRM: &str = "ov2brief-snap-confirm-003";
const NAV: &str = "ov2brief-snap-nav-001";
const PAGE: &str = "ov2brief-snap-page-001";
const CANCEL: &str = "ov2brief-snap-cancel-001";

fn directory(capture: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR"))
        .join("analysis/flight-runtime")
        .join(capture)
}

/// One recorded call into another module.
#[derive(Clone, Debug)]
struct Effect {
    sequence: u64,
    function: u32,
    sp: u32,
    irq: u64,
    args: [u32; 4],
    stack: [u32; 4],
    hash: u32,
    entry_data: [u32; 32],
    entry_control: [u32; 32],
    v0: u32,
    ram: Vec<(u32, Vec<u8>)>,
    scratch: Vec<(u32, Vec<u8>)>,
    data: [u32; 32],
    control: [u32; 32],
}

fn number(v: &Value) -> u32 {
    match v {
        Value::String(s) => s.parse::<u64>().expect("number") as u32,
        other => other.as_u64().expect("number") as u32,
    }
}
fn words<const N: usize>(v: &Value) -> [u32; N] {
    let list = v.as_array().expect("list");
    assert_eq!(list.len(), N);
    std::array::from_fn(|i| number(&list[i]))
}
fn runs(v: &Value) -> Vec<(u32, Vec<u8>)> {
    v.as_array()
        .expect("runs")
        .iter()
        .map(|run| {
            let hex = run[1].as_str().expect("hex");
            let bytes = (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex"))
                .collect();
            (number(&run[0]), bytes)
        })
        .collect()
}

fn read_effects(capture: &str) -> Vec<Effect> {
    let text = fs::read_to_string(directory(capture).join("effects.jsonl")).expect("effects");
    text.lines()
        .map(|line| {
            let v: Value = serde_json::from_str(line).expect("effect");
            Effect {
                sequence: v["sequence"].as_u64().unwrap(),
                function: number(&v["f"]),
                sp: number(&v["sp"]),
                irq: v["irq"].as_u64().unwrap(),
                args: words(&v["a"]),
                stack: words(&v["stack"]),
                hash: u32::from_str_radix(v["hash"].as_str().unwrap(), 16).unwrap(),
                entry_data: words(&v["entry_cp2d"]),
                entry_control: words(&v["entry_cp2c"]),
                v0: number(&v["v0"]),
                ram: runs(&v["ram"]),
                scratch: runs(&v["scratch"]),
                data: words(&v["cp2d"]),
                control: words(&v["cp2c"]),
            }
        })
        .collect()
}

/// The recorded routines owned elsewhere and the register/stack arguments
/// each one reads (sprintf: format plus one per conversion).
fn used_arguments(m: &mut Machine, function: u32, args: [u32; 4]) -> (usize, usize) {
    match function {
        0x800f_18ec | 0x800f_1908 => (3, 0),
        0x800f_193c | 0x800f_1770 | 0x800d_0b28 | 0x800e_7a18 | 0x800a_53a8 => (2, 0),
        0x800f_196c | 0x800d_0c58 | 0x800e_f1f4 | 0x800a_c328 => (1, 0),
        0x800d_0c68 | 0x800d_0f20 | 0x800e_983c => (0, 0),
        0x800d_04c8 => (4, 3),
        0x800a_cd28 => {
            let format = m.read_bytes(args[1], 64);
            let conversions = format
                .iter()
                .take_while(|&&b| b != 0)
                .filter(|&&b| b == b'%')
                .count();
            ((2 + conversions).min(4), 0)
        }
        _ => panic!("{function:08x}: no argument table"),
    }
}

/// Rotate-xor hash of RAM words below the stack (the observer's `hash_ram`).
fn ram_hash(m: &Machine) -> u32 {
    m.ram()[..STACK_LIMIT as usize & (RAM_BYTES - 1)]
        .chunks_exact(4)
        .fold(0u32, |h, w| {
            h.rotate_left(5) ^ u32::from_le_bytes(w.try_into().unwrap())
        })
}

/// The replay hook over `effects` (in call order). Mismatches are collected
/// in `errors`; the hook then still applies the effect so that later
/// differences are reported against the original course.
fn replay(
    effects: Vec<Effect>,
    entry_irq: u64,
    errors: Rc<RefCell<Vec<String>>>,
    remaining: Rc<RefCell<usize>>,
) -> ExternalReplay {
    let recorded: Vec<u32> = RECORDED.to_vec();
    let mut queue: VecDeque<Effect> = effects.into();
    *remaining.borrow_mut() = queue.len();
    Box::new(move |m: &mut Machine, target: u32, args: [u32; 4]| {
        if !recorded.contains(&target) {
            return None;
        }
        let mut errors = errors.borrow_mut();
        let Some(effect) = queue.pop_front() else {
            errors.push(format!("{target:08x}: call the original did not make"));
            return Some(0);
        };
        *remaining.borrow_mut() = queue.len();
        let at = format!("#{} {:08x}", effect.sequence, effect.function);
        if effect.function != target {
            errors.push(format!("{at}: called {target:08x} instead"));
        }
        let (registers, stack) = used_arguments(m, target, args);
        if args[..registers] != effect.args[..registers] {
            errors.push(format!(
                "{at}: arguments {:08x?}, original {:08x?}",
                &args[..registers],
                &effect.args[..registers]
            ));
        }
        if m.sp != effect.sp {
            errors.push(format!("{at}: sp {:08x}, original {:08x}", m.sp, effect.sp));
        }
        let stack_words: Vec<u32> = (0..stack as u32)
            .map(|k| m.lw(m.sp + 0x10 + k * 4))
            .collect();
        if stack_words[..] != effect.stack[..stack] {
            errors.push(format!(
                "{at}: stack arguments {stack_words:08x?}, original {:08x?}",
                &effect.stack[..stack]
            ));
        }
        if m.gte.data != effect.entry_data || m.gte.control != effect.entry_control {
            let data: Vec<usize> = (0..32)
                .filter(|&i| m.gte.data[i] != effect.entry_data[i])
                .collect();
            let control: Vec<usize> = (0..32)
                .filter(|&i| m.gte.control[i] != effect.entry_control[i])
                .collect();
            errors.push(format!(
                "{at}: GTE at entry differs: data {data:?} control {control:?}"
            ));
        }
        // After an exception inside the routine the original's RAM also
        // holds the handler's writes, which the final comparison reports.
        let hash = ram_hash(m);
        if hash != effect.hash && effect.irq == entry_irq {
            errors.push(format!(
                "{at}: RAM below the stack differs at the call (hash {hash:08x}, original {:08x})",
                effect.hash
            ));
        }
        drop(errors);
        for (address, bytes) in &effect.ram {
            m.write_bytes(*address, bytes);
        }
        for (address, bytes) in &effect.scratch {
            m.write_bytes(*address, bytes);
        }
        m.gte.data = effect.data;
        m.gte.control = effect.control;
        Some(effect.v0)
    })
}

/// RAM that interrupt handling writes, excluded only when an exception ran
/// between a routine's entry and return (the observer counts entries of the
/// exception vector 0x80000080). Replay mode needs few of these, since a
/// handler that runs inside a recorded callee is part of its recorded
/// effect; native mode runs every callee natively and needs them all.
/// The ranges follow `tests/loading.rs` (`INTERRUPT_OWNED`) plus the CD
/// stream callback state seen here. Observed on the two fast-forward occurrences
/// that span a VBlank (`ov2brief-snap-fast-002` frame 201,
/// `ov2brief-snap-confirm-003` frame 231).
const INTERRUPT_OWNED: [(u32, u32, &str); 7] = [
    // BIOS exception dispatch, event control blocks, the interrupted
    // context's register save area (0x8000E200..) and kernel services.
    (0x8000_0000, 0x8001_0000, "BIOS kernel RAM"),
    // Set by the sound-timer interrupt handler 80099F8C.
    (0x800b_256c, 0x800b_2570, "sound interrupt flag"),
    // libetc interrupt manager context and its private interrupt stack
    // (handler frames; the bytes seen at 0x800B3EC4..0x800B3F14).
    (
        0x800b_2f2c,
        0x800b_3f44,
        "libetc interrupt context and stack",
    ),
    // VBlank counter incremented by the VBlank handler 800A0E90.
    (0x800b_3ff4, 0x800b_3ff8, "VSync counter"),
    // The game's CD stream callback state (800297B8 and its callees,
    // reached from the CD ready callback during the briefing's XA music).
    (0x800b_f178, 0x800b_f1c8, "CD stream callback state"),
    // Sound driver state updated on every sound-timer tick.
    (0x800c_5bcc, 0x800c_5bce, "sound driver tick state"),
    (0x800c_5c70, 0x800c_5c72, "sound driver tick state"),
];

/// Routines owned elsewhere whose effects the captures record.
const RECORDED: [u32; 16] = [
    0x800f_193c,
    0x800f_18ec,
    0x800f_1908,
    0x800f_1770,
    0x800f_196c,
    0x800d_0b28,
    0x800d_04c8,
    0x800d_0f20,
    0x800d_0c58,
    0x800d_0c68,
    0x800e_7a18,
    0x800e_983c,
    0x800e_f1f4,
    0x800a_53a8,
    0x800a_c328,
    0x800a_cd28,
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Replay,
    Native,
}

enum Outcome {
    Exact(usize),
    Blocked(Fault),
}

/// Run `callee` (through `ov2_briefing::dispatch`, original register
/// arguments) on its `occurrence`th entry snapshot in `capture` and compare
/// with the matching return.
fn run(capture: &str, callee: u32, occurrence: usize, returns: bool, mode: Mode) -> Outcome {
    let dir = directory(capture);
    let index = snapshot::read_index(&dir.join("snapshots.jsonl")).expect("index");
    let entry = index
        .iter()
        .filter(|s| s.callee == callee && s.stage == "entry")
        .nth(occurrence)
        .unwrap_or_else(|| panic!("{capture}: {callee:08x}: no entry #{occurrence}"));
    let exit = index
        .iter()
        .find(|s| {
            s.callee == callee
                && s.stage == "return"
                && s.sequence > entry.sequence
                && s.ra == entry.ra
                && s.registers.gpr[29] == entry.registers.gpr[29]
        })
        .unwrap_or_else(|| panic!("{capture}: {callee:08x}: no return #{occurrence}"));
    let irq = |sequence: u64| -> u64 {
        let text = fs::read_to_string(dir.join("snapshots.jsonl")).unwrap();
        text.lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .find(|v| v["sequence"].as_u64() == Some(sequence))
            .and_then(|v| v["irq"].as_u64())
            .unwrap_or(0)
    };
    let (entry_irq, exit_irq) = (irq(entry.sequence), irq(exit.sequence));
    let entry = Snapshot::load(&dir, entry).expect("entry");
    let exit = Snapshot::load(&dir, exit).expect("exit");
    let mut m = entry.machine();
    let errors = Rc::new(RefCell::new(Vec::new()));
    let remaining = Rc::new(RefCell::new(0));
    if mode == Mode::Replay {
        let effects: Vec<Effect> = read_effects(capture)
            .into_iter()
            .filter(|e| e.sequence > entry.info.sequence && e.sequence < exit.info.sequence)
            .collect();
        ov2_briefing::set_external_replay(Some(replay(
            effects,
            entry_irq,
            errors.clone(),
            remaining.clone(),
        )));
    }
    let args = [entry.gpr(4), entry.gpr(5), entry.gpr(6), entry.gpr(7)];
    let v0 = ov2_briefing::dispatch(&mut m, callee, args).expect("claimed by ov2_briefing");
    ov2_briefing::set_external_replay(None);
    let label = format!("{callee:08x} {capture} #{occurrence} ({mode:?})");
    if let Err(fault) = m.take_fault() {
        if mode == Mode::Native && matches!(fault, Fault::Untranslated { .. }) {
            return Outcome::Blocked(fault);
        }
        panic!("{label}: {fault}");
    }
    let errors = errors.borrow();
    assert!(errors.is_empty(), "{label}: {}", errors.join("\n"));
    assert_eq!(*remaining.borrow(), 0, "{label}: recorded calls not made");
    assert_eq!(m.sp, entry.gpr(29), "{label}: stack pointer not restored");
    let dead = STACK_LIMIT as usize & (RAM_BYTES - 1)..entry.gpr(29) as usize & (RAM_BYTES - 1);
    let interrupted = entry_irq != exit_irq;
    let handler = |i: usize| {
        let address = 0x8000_0000 + i as u32;
        interrupted
            && INTERRUPT_OWNED
                .iter()
                .any(|&(start, end, _)| (start..end).contains(&address))
    };
    let ram: Vec<u32> = (0..RAM_BYTES)
        .filter(|i| !dead.contains(i) && !handler(*i) && m.ram()[*i] != exit.ram[*i])
        .map(|i| 0x8000_0000 + i as u32)
        .collect();
    let scratch: Vec<u32> = (0..SCRATCH_BYTES)
        .filter(|&i| m.scratch()[i] != exit.scratch[i])
        .map(|i| 0x1f80_0000 + i as u32)
        .collect();
    let data: Vec<usize> = (0..32)
        .filter(|&i| m.gte.data[i] != exit.info.registers.cp2d[i])
        .collect();
    let control: Vec<usize> = (0..32)
        .filter(|&i| m.gte.control[i] != exit.info.registers.cp2c[i])
        .collect();
    let hex = |v: &[u32]| {
        v.iter()
            .take(if std::env::var_os("OV2BRIEF_ALL").is_some() {
                4096
            } else {
                16
            })
            .map(|a| format!("{a:08x}"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let note = if interrupted {
        " (an exception ran inside the routine)"
    } else {
        ""
    };
    assert!(
        ram.is_empty() && scratch.is_empty() && data.is_empty() && control.is_empty(),
        "{label}{note}: ram {} [{}] scratch {} [{}] gte data {data:?} control {control:?}",
        ram.len(),
        hex(&ram),
        scratch.len(),
        hex(&scratch)
    );
    if returns {
        assert_eq!(
            v0,
            exit.gpr(2),
            "{label}: v0 {v0:#010x}, original {:#010x}",
            exit.gpr(2)
        );
    }
    if interrupted {
        eprintln!("{label}: an exception ran inside; handler-owned bytes not compared");
    }
    let changed = (0..RAM_BYTES)
        .filter(|i| !dead.contains(i) && !handler(*i) && entry.ram[*i] != exit.ram[*i])
        .count()
        + (0..SCRATCH_BYTES)
            .filter(|&i| entry.scratch[i] != exit.scratch[i])
            .count()
        + (0..32)
            .filter(|&i| entry.info.registers.cp2d[i] != exit.info.registers.cp2d[i])
            .count()
        + (0..32)
            .filter(|&i| entry.info.registers.cp2c[i] != exit.info.registers.cp2c[i])
            .count();
    Outcome::Exact(changed)
}

/// Every listed occurrence must be exact in replay mode and exact or
/// BLOCKED in native mode; the comparisons must not be vacuous.
fn check(callee: u32, occurrences: &[(&str, usize)], returns: bool) {
    let mut changed = 0;
    for &(capture, occurrence) in occurrences {
        match run(capture, callee, occurrence, returns, Mode::Replay) {
            Outcome::Exact(n) => {
                changed += n;
                eprintln!("{callee:08x} {capture} #{occurrence}: replay exact, {n} changed");
            }
            Outcome::Blocked(_) => unreachable!(),
        }
        match run(capture, callee, occurrence, returns, Mode::Native) {
            Outcome::Exact(n) => {
                eprintln!("{callee:08x} {capture} #{occurrence}: native exact, {n} changed")
            }
            Outcome::Blocked(fault) => {
                eprintln!("{callee:08x} {capture} #{occurrence}: native BLOCKED: {fault}")
            }
        }
    }
    assert!(changed > 0 || returns, "{callee:08x}: nothing changed");
}

fn occurrences(capture: &str, count: usize) -> Vec<(&str, usize)> {
    (0..count).map(|k| (capture, k)).collect()
}

fn available() -> bool {
    std::env::var_os("AC3_DATA_DIR").is_some_and(|d| {
        Path::new(&d)
            .join("analysis/flight-runtime")
            .join(FAST)
            .join("effects.jsonl")
            .exists()
    })
}

macro_rules! snapshot_test {
    ($name:ident, $callee:expr, $returns:expr, [$(($capture:expr, $count:expr)),* $(,)?]) => {
        #[test]
        #[ignore = "needs the ov2brief snapshot captures (AC3_DATA_DIR)"]
        fn $name() {
            assert!(available(), "captures missing");
            let mut list = Vec::new();
            $(list.extend(occurrences($capture, $count));)*
            check($callee, &list, $returns);
        }
    };
}

// ---- leaves: commands, resets, helpers ----------------------------------------------

snapshot_test!(command_place_800eb0f0, 0x800e_b0f0, false, [(FAST, 2)]);
snapshot_test!(command_move_800eb1d8, 0x800e_b1d8, false, [(FAST, 1)]);
snapshot_test!(
    command_label_800eb304,
    0x800e_b304,
    false,
    [(FAST, 1), (NORMAL, 2)]
);
snapshot_test!(command_pan_800eb414, 0x800e_b414, false, [(FAST, 1)]);
snapshot_test!(command_camera_800eb4f0, 0x800e_b4f0, false, [(FAST, 1)]);
snapshot_test!(command_marker_800eb524, 0x800e_b524, false, [(FAST, 1)]);
snapshot_test!(reset_objects_800eb564, 0x800e_b564, false, [(FAST, 1)]);
snapshot_test!(reset_labels_800eb5ac, 0x800e_b5ac, false, [(FAST, 1)]);
/// 800EB630 runs once, at the initialization, on already-clear fields: the
/// snapshot comparison is exact but changes nothing; `reset_camera_clears`
/// covers the stores.
#[test]
#[ignore = "needs the ov2brief snapshot captures (AC3_DATA_DIR)"]
fn reset_camera_800eb630() {
    assert!(available(), "captures missing");
    for mode in [Mode::Replay, Mode::Native] {
        assert!(matches!(
            run(FAST, 0x800e_b630, 0, false, mode),
            Outcome::Exact(0)
        ));
    }
}
snapshot_test!(text_line_800d0b74, 0x800d_0b74, true, [(FAST, 1)]);
snapshot_test!(text_line_count_800d0bd8, 0x800d_0bd8, true, [(FAST, 1)]);
snapshot_test!(spaces_to_gaps_800d0f60, 0x800d_0f60, false, [(FAST, 1)]);
snapshot_test!(set_visual_color_800f1788, 0x800f_1788, true, [(FAST, 1)]);

// ---- movement -------------------------------------------------------------------------

snapshot_test!(tick_objects_800eb654, 0x800e_b654, true, [(FAST, 1)]);
snapshot_test!(tick_camera_800ebc14, 0x800e_bc14, true, [(FAST, 1)]);
snapshot_test!(
    tick_labels_800eb9e8,
    0x800e_b9e8,
    false,
    [(FAST, 3), (NORMAL, 4)]
);
snapshot_test!(navigate_800edd28, 0x800e_dd28, true, [(FAST, 2), (NAV, 2)]);
snapshot_test!(
    aim_camera_800ede98,
    0x800e_de98,
    false,
    [(FAST, 2), (NAV, 2)]
);

// ---- scene ------------------------------------------------------------------------------

snapshot_test!(
    label_connector_800eb754,
    0x800e_b754,
    false,
    [(FAST, 2), (NORMAL, 3)]
);
snapshot_test!(view_matrix_800ebcd8, 0x800e_bcd8, false, [(FAST, 2)]);
snapshot_test!(terrain_800ebf68, 0x800e_bf68, true, [(FAST, 2)]);
snapshot_test!(objects_800ece7c, 0x800e_ce7c, true, [(FAST, 2)]);
snapshot_test!(marker_800ed63c, 0x800e_d63c, true, [(FAST, 2)]);
snapshot_test!(scene_800ed834, 0x800e_d834, true, [(FAST, 3)]);
snapshot_test!(minimap_800ed8b0, 0x800e_d8b0, true, [(FAST, 1)]);

// ---- phase steps --------------------------------------------------------------------------

snapshot_test!(phase_reset_view_800edf78, 0x800e_df78, true, [(FAST, 1)]);
snapshot_test!(
    phase_open_panels_800edfa4,
    0x800e_dfa4,
    true,
    [(FAST, 3), (CONFIRM, 1)]
);
snapshot_test!(phase_zoom_in_800ee128, 0x800e_e128, true, [(FAST, 2)]);
snapshot_test!(phase_fly_in_800ee1e4, 0x800e_e1e4, true, [(FAST, 2)]);
snapshot_test!(
    phase_script_800ee3b8,
    0x800e_e3b8,
    true,
    [(FAST, 4), (NORMAL, 6), (CONFIRM, 3), (PAGE, 3), (CANCEL, 2)]
);
snapshot_test!(
    phase_finish_800ee860,
    0x800e_e860,
    true,
    [(FAST, 2), (NORMAL, 1), (CONFIRM, 1)]
);
snapshot_test!(phase_hide_labels_800ee8d0, 0x800e_e8d0, true, [(FAST, 1)]);
snapshot_test!(phase_labels_hidden_800ee95c, 0x800e_e95c, true, [(FAST, 2)]);
snapshot_test!(phase_select_label_800ee9a0, 0x800e_e9a0, true, [(FAST, 1)]);
snapshot_test!(
    phase_map_800eea20,
    0x800e_ea20,
    true,
    [(FAST, 4), (CONFIRM, 1), (NAV, 2)]
);
snapshot_test!(
    phase_cancel_800eed50,
    0x800e_ed50,
    true,
    [(FAST, 1), (CANCEL, 1)]
);
snapshot_test!(
    phase_close_800eedbc,
    0x800e_edbc,
    true,
    [(FAST, 2), (CONFIRM, 1)]
);
snapshot_test!(initialize_800eee70, 0x800e_ee70, true, [(FAST, 1)]);

// ---- unit tests for paths the captures do not reach ------------------------------------

/// A machine with overlay module 2 loaded and a briefing state block.
fn briefing_machine() -> (Machine, u32) {
    let mut m = Machine::default();
    let context = 0x800b_d158;
    m.sw(ac3_flight::GP + 0x2dc, context);
    m.sw(context + 0x38, 2);
    let state = 0x8010_0000;
    m.sw(ov2_briefing::BRIEFING, state);
    (m, state)
}

/// Record calls into other modules, answering each with `v0` 0.
/// Calls a recorder saw: target and register arguments.
type CallLog = Rc<RefCell<Vec<(u32, [u32; 4])>>>;

fn recorder() -> (ExternalReplay, CallLog) {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let log = calls.clone();
    let hook: ExternalReplay = Box::new(move |_m: &mut Machine, target: u32, args: [u32; 4]| {
        log.borrow_mut().push((target, args));
        Some(0)
    });
    (hook, calls)
}

#[test]
fn module_check_and_noop_command() {
    let (mut m, _) = briefing_machine();
    assert_eq!(ov2_briefing::dispatch(&mut m, 0x800e_b0e8, [0; 4]), Some(0));
    m.sw(0x800b_d158 + 0x38, 1);
    assert_eq!(ov2_briefing::dispatch(&mut m, 0x800e_b0e8, [0; 4]), None);
    assert!(m.take_fault().is_ok());
}

#[test]
fn zero_durations_reach_the_division_traps() {
    let (mut m, _) = briefing_machine();
    let (hook, _) = recorder();
    ov2_briefing::set_external_replay(Some(hook));
    let command = 0x8011_0000;
    m.sb(command + 2, 2);
    ov2_briefing::command_move(&mut m, command);
    assert_eq!(
        m.take_fault(),
        Err(Fault::Trap {
            address: 0x800e_b2c0,
            code: 7
        })
    );
    ov2_briefing::command_pan(&mut m, command);
    assert_eq!(
        m.take_fault(),
        Err(Fault::Trap {
            address: 0x800e_b49c,
            code: 7
        })
    );
    ov2_briefing::set_external_replay(None);
}

#[test]
fn closing_panels_2_2_and_3_1() {
    for (address, store_first) in [(0x800e_ec90u32, false), (0x800e_edbc, true)] {
        let (mut m, state) = briefing_machine();
        let (hook, calls) = recorder();
        ov2_briefing::set_external_replay(Some(hook));
        m.sw(state + ov2_briefing::PANEL_TIMER, 3);
        let result = ov2_briefing::dispatch(&mut m, address, [0; 4]).unwrap();
        assert_eq!(result, 0);
        let up = (-(22 * 22)) as u32;
        assert_eq!(
            *calls.borrow(),
            [
                (ov2_briefing::SET_VISUAL_OFFSET, [3, 0, up, 0]),
                (ov2_briefing::SET_VISUAL_OFFSET, [2, 0, 22 * 22, 0])
            ],
            "{address:08x} (offsets first: {store_first})"
        );
        assert_eq!(m.lhu(state + ov2_briefing::PANEL_OFFSETS), 22 * 22);
        assert_eq!(m.lhu(state + ov2_briefing::PANEL_OFFSETS + 2), up as u16);
        assert_eq!(m.lw(state + ov2_briefing::PANEL_TIMER), 2);
        m.sw(state + ov2_briefing::PANEL_TIMER, 0);
        assert_eq!(ov2_briefing::dispatch(&mut m, address, [0; 4]), Some(4));
        m.sw(state, 4);
        assert_eq!(ov2_briefing::dispatch(&mut m, address, [0; 4]), Some(8));
        ov2_briefing::set_external_replay(None);
        assert!(m.take_fault().is_ok());
    }
}

#[test]
fn navigation_wraps_and_cancels_a_pending_selection() {
    let (mut m, state) = briefing_machine();
    let (hook, calls) = recorder();
    ov2_briefing::set_external_replay(Some(hook));
    let label = |k: u32| state + ov2_briefing::LABELS + k * ov2_briefing::LABEL_SIZE;
    m.sw(state + ov2_briefing::LABEL_COUNT, 3);
    // LEFT from label 0: wraps to the last; label 0 was still waiting to be
    // highlighted (0x40): no sound, flags 0x42 cleared.
    m.sw(label(0), 0x43);
    m.sw(label(2), 0x23);
    let new = ov2_briefing::navigate(&mut m, 4);
    assert_eq!(new, label(2));
    assert_eq!(m.lw(state + ov2_briefing::SELECTED), 2);
    assert_eq!(m.lw(label(0)), 1);
    assert_eq!(m.lw(label(2)), 7, "a fading label is highlighted again");
    assert!(calls.borrow().is_empty());
    // RIGHT from the last label wraps to 0, fading the old one (sound 0x65).
    m.sh(label(0) + 6, 9);
    let new = ov2_briefing::navigate(&mut m, 8);
    assert_eq!(new, label(0));
    assert_eq!(m.lw(label(2)), 0x23);
    assert_eq!(m.lw(label(0)), 0x43);
    assert_eq!(m.lhu(label(0) + 6), 0);
    assert_eq!(
        *calls.borrow(),
        [(ov2_briefing::PLAY_SOUND, [0x65, 0, 0, 0])]
    );
    ov2_briefing::set_external_replay(None);
    assert!(m.take_fault().is_ok());
}

#[test]
fn reset_camera_clears() {
    let (mut m, state) = briefing_machine();
    for offset in 0x518..0x536 {
        m.sb(state + offset, 0x5a);
    }
    ov2_briefing::reset_camera(&mut m);
    assert_eq!(m.lw(state + 0x518), 0);
    assert_eq!(m.lw(state + 0x52c), 0);
    for offset in [0x530, 0x532, 0x534] {
        assert_eq!(m.lhu(state + offset), 0);
    }
    assert_eq!(
        m.lw(state + 0x51c),
        0x5a5a_5a5a,
        "the camera position is kept"
    );
    assert!(m.take_fault().is_ok());
}

#[test]
fn fast_forward_after_the_script_end_is_the_next_substate() {
    let (mut m, state) = briefing_machine();
    let (hook, calls) = recorder();
    ov2_briefing::set_external_replay(Some(hook));
    m.sw(0x800b_d158 + 0x12c, 0x40);
    m.sw(state, 0x21);
    m.sw(state + 8, 7);
    let script = 0x8011_0000;
    m.sh(script, 9);
    m.sw(state + 0xc, script);
    assert_eq!(ov2_briefing::phase_script(&mut m), 2);
    assert!(calls.borrow().is_empty());
    assert_eq!(m.lw(state), 0x21, "flags unchanged");
    assert_eq!(m.lw(state + 8), 7, "clock unchanged");
    // TRIANGLE cancels first: flags |= 4, result 0x10.
    m.sw(0x800b_d158 + 0x12c, 0x60);
    assert_eq!(ov2_briefing::phase_script(&mut m), 0x10);
    assert_eq!(m.lw(state), 0x25);
    ov2_briefing::set_external_replay(None);
    assert!(m.take_fault().is_ok());
}
