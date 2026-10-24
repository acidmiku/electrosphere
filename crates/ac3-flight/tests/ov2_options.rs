//! OPTIONS (mission state 10) and DEBRIEFING (state 11) of overlay
//! module 2 (`ov2_options`, with `ov2_options_keys` and
//! `ov2_options_results`).
//!
//! **Snapshots.** Every captured routine runs on a captured entry state
//! (module 2 loaded) and must reproduce the captured return state exactly:
//! all of RAM except the dead frames below the entry `$sp`, the scratchpad,
//! every GTE register, and `v0` where the routine returns one. Captures
//! (`.ov2brief/redux-ov2-snapshots.lua`, from
//! `analysis/menu-runtime/mission-002/final.sav`, the hub menu's choice 3 at
//! capture frame 45; the options menu opens at frame 73):
//! - `ov2brief-snap-keys-001`: the menu, KEY CONFIG twice (the MAIN page
//!   rows and values, the button page with the grid, the action list, the
//!   type-2 preset; the digital pad of the capture is controller type 2, so
//!   the analog page is skipped), EXIT with the settings stored, the menu's
//!   return;
//! - `ov2brief-snap-title-001`: the menu's grid moves (the MUSIC PLAYER cell
//!   locked), CROSS ignored on BACK TO TITLE, START on it: the slide-out
//!   and the 50-frame fade to the title (result 0x40);
//! - `ov2brief-snap-game-sdk-001`: GAME CONFIG's SOUND and SCREEN pages
//!   with the SDK routines GetODE, SpuReadDecodedData and `_spu_Fr_`
//!   snapshotted (their tests are at the end, with the oracle runs of
//!   MoveImage, GetODE and `__udivdi3`).
//!
//! Two modes per occurrence, as in `tests/ov2_briefing.rs`: **replay**
//! checks every call into other modules against the recorded original
//! (`effects.jsonl`: target, used register and stack arguments, `$sp`, GTE,
//! a hash of RAM below the stack) and applies its recorded effect; **native**
//! runs everything through `dispatch` (BLOCKED when it reaches a routine
//! nobody translates yet).
//!
//! **Oracle.** The paths no capture reaches (the analog page and the
//! calibrations of controller types 0, 1 and 3, the special-action
//! confirmation, the MUSIC PLAYER, the mode machine's other results, and
//! all of state 11, which needs a finished flight) are compared with the
//! original instructions through the shared oracle
//! (`tests/support/oracle.rs`) from captured base states with varied
//! fields.
mod support;

use ac3_flight::{
    Fault, Machine,
    machine::{RAM_BYTES, SCRATCH_BYTES, STACK_LIMIT},
    ov2_briefing::{self, ExternalReplay},
    ov2_options, sdk,
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

const KEYS: &str = "ov2brief-snap-keys-001";
const TITLE: &str = "ov2brief-snap-title-001";
/// GetODE, SpuReadDecodedData and `_spu_Fr_` inside GAME CONFIG's SOUND
/// and SCREEN pages (OPTIONS choice 1; no recorded externals).
const GAME_SDK: &str = "ov2brief-snap-game-sdk-001";

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
/// each one reads (sprintf: format plus one per conversion, the conversions
/// beyond two from the stack).
fn used_arguments(m: &mut Machine, function: u32, args: [u32; 4]) -> (usize, usize) {
    match function {
        0x800f_18ec | 0x800f_1908 | 0x800e_79d4 | 0x800d_170c | 0x800c_e7e4 | 0x800a_69d0 => (3, 0),
        0x800f_193c | 0x800f_1770 | 0x800d_0b28 | 0x800d_0ee4 | 0x800e_7a18 | 0x800a_73f0
        | 0x800a_c2d8 => (2, 0),
        0x800f_196c | 0x800e_7b50 | 0x800c_eb80 | 0x800d_17d0 | 0x8002_d724 | 0x800a_c328
        | 0x800a_6664 => (1, 0),
        0x800f_1ba0 | 0x800d_0f20 | 0x800e_983c | 0x800e_9824 | 0x800c_e844 | 0x800a_c178 => (0, 0),
        0x800d_04c8 => (4, 3),
        0x800c_e978 | 0x800a_ac38 => (4, 1),
        0x800a_cd28 => {
            let format = m.read_bytes(args[1], 64);
            let conversions = format
                .iter()
                .take_while(|&&b| b != 0)
                .filter(|&&b| b == b'%')
                .count();
            ((2 + conversions).min(4), conversions.saturating_sub(2))
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
/// stream callback state (the same table as `tests/ov2_briefing.rs`).
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
const RECORDED: [u32; 30] = [
    0x800f_193c,
    0x800f_18ec,
    0x800f_1908,
    0x800f_1770,
    0x800f_196c,
    0x800f_1ba0,
    0x800d_0b28,
    0x800d_04c8,
    0x800d_0f20,
    0x800d_0ee4,
    0x800e_79d4,
    0x800e_7b50,
    0x800e_983c,
    0x800e_9824,
    0x800d_170c,
    0x800c_e7e4,
    0x800c_e844,
    0x800c_e978,
    0x800c_eb80,
    0x800d_17d0,
    0x8002_d724,
    0x800a_ac38,
    0x800a_73f0,
    0x800a_c328,
    0x800a_cd28,
    0x800a_c2d8,
    0x800a_c178,
    0x800a_69d0,
    0x800a_6664,
    0x800e_7a18,
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

/// Run `callee` (through `ov2_options::dispatch`, original register
/// arguments) on its `occurrence`th entry snapshot in `capture` and compare
/// with the matching return.
fn run(
    capture: &str,
    callee: u32,
    occurrence: usize,
    returns: bool,
    mode: Mode,
    device: &[(u32, u32, &str)],
) -> Outcome {
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
    let v0 = ov2_options::dispatch(&mut m, callee, args).expect("claimed by ov2_options");
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
        (interrupted
            && INTERRUPT_OWNED
                .iter()
                .any(|&(start, end, _)| (start..end).contains(&address)))
            || (mode == Mode::Native
                && device
                    .iter()
                    .any(|&(start, end, _)| (start..end).contains(&address)))
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
/// BLOCKED in native mode (`device`: RAM another module's device model
/// leaves differently in native mode, documented at the use); the
/// comparisons must not be vacuous unless `quiet`.
fn check(
    callee: u32,
    occurrences: &[(&str, usize)],
    returns: bool,
    quiet: bool,
    device: &[(u32, u32, &str)],
) {
    let mut changed = 0;
    for &(capture, occurrence) in occurrences {
        match run(capture, callee, occurrence, returns, Mode::Replay, &[]) {
            Outcome::Exact(n) => {
                changed += n;
                eprintln!("{callee:08x} {capture} #{occurrence}: replay exact, {n} changed");
            }
            Outcome::Blocked(_) => unreachable!(),
        }
        match run(capture, callee, occurrence, returns, Mode::Native, device) {
            Outcome::Exact(n) => {
                eprintln!("{callee:08x} {capture} #{occurrence}: native exact, {n} changed")
            }
            Outcome::Blocked(fault) => {
                eprintln!("{callee:08x} {capture} #{occurrence}: native BLOCKED: {fault}")
            }
        }
    }
    assert!(
        changed > 0 || returns || quiet,
        "{callee:08x}: nothing changed"
    );
}

fn occurrences(capture: &str, count: usize) -> Vec<(&str, usize)> {
    (0..count).map(|k| (capture, k)).collect()
}

fn available() -> bool {
    std::env::var_os("AC3_DATA_DIR").is_some_and(|d| {
        Path::new(&d)
            .join("analysis/flight-runtime")
            .join(KEYS)
            .join("effects.jsonl")
            .exists()
    })
}

macro_rules! snapshot_test {
    ($name:ident, $callee:expr, $returns:expr, [$(($capture:expr, $count:expr)),* $(,)?]) => {
        snapshot_test!($name, $callee, $returns, false, &[], [$(($capture, $count)),*]);
    };
    ($name:ident, $callee:expr, $returns:expr, $quiet:expr, $device:expr,
     [$(($capture:expr, $count:expr)),* $(,)?]) => {
        #[test]
        #[ignore = "needs the ov2brief options captures (AC3_DATA_DIR)"]
        fn $name() {
            assert!(available(), "captures missing");
            let mut list = Vec::new();
            $(list.extend(occurrences($capture, $count));)*
            check($callee, &list, $returns, $quiet, $device);
        }
    };
}

/// libgpu's command queue (read/write indexes 0x800B990C/0x800B9910, the
/// pending count 0x800B9924, and the first entry at 0x800C8FB0): the
/// original's LoadImage of 800CE978 queues the upload and the GPU DMA
/// interrupt drains it, while the native SDK model uploads at once and
/// leaves the queue as it was. Excluded from 800E19C4's native comparison
/// only.
const GPU_QUEUE: [(u32, u32, &str); 3] = [
    (0x800b_990c, 0x800b_9914, "libgpu queue indexes"),
    (0x800b_9924, 0x800b_9928, "libgpu queue count"),
    (0x800c_8fb0, 0x800c_8fc4, "libgpu queue entry"),
];

// ---- state 10: the OPTIONS menu, BACK TO TITLE and the mode machine -------------------

snapshot_test!(initialize_800dd1a4, 0x800d_d1a4, true, [(KEYS, 1)]);
snapshot_test!(tick_800dd214, 0x800d_d214, true, [(KEYS, 1), (TITLE, 1)]);
snapshot_test!(
    run_modes_800e50b8,
    0x800e_50b8,
    true,
    [(KEYS, 2), (TITLE, 4)]
);
snapshot_test!(menu_open_800dcaac, 0x800d_caac, true, [(KEYS, 2)]);
snapshot_test!(menu_slide_in_800dcc48, 0x800d_cc48, true, [(KEYS, 1)]);
snapshot_test!(
    menu_select_800dcd34,
    0x800d_cd34,
    true,
    [(KEYS, 4), (TITLE, 9)]
);
snapshot_test!(
    menu_slide_out_800dccc4,
    0x800d_ccc4,
    true,
    [(KEYS, 1), (TITLE, 2)]
);
snapshot_test!(
    place_panels_800dc9b0,
    0x800d_c9b0,
    false,
    [(KEYS, 1), (TITLE, 1)]
);
// The slides return 800F18EC's `v0` (its slot entry); ov2core's native
// 800F18EC returns 0, so their `v0` is not compared.
snapshot_test!(
    slide_left_800dc8f4,
    0x800d_c8f4,
    false,
    [(KEYS, 1), (TITLE, 1)]
);
snapshot_test!(slide_right_800dc938, 0x800d_c938, false, [(KEYS, 1)]);
snapshot_test!(slide_down_800dc974, 0x800d_c974, false, [(TITLE, 1)]);
snapshot_test!(title_slide_out_800dcf70, 0x800d_cf70, true, [(TITLE, 1)]);
snapshot_test!(title_fade_start_800dcfa4, 0x800d_cfa4, true, [(TITLE, 1)]);
snapshot_test!(title_fade_800dcfec, 0x800d_cfec, true, [(TITLE, 4)]);

// ---- KEY CONFIG ----------------------------------------------------------------------------

snapshot_test!(keys_open_800e470c, 0x800e_470c, true, [(KEYS, 1)]);
snapshot_test!(keys_load_800e47ac, 0x800e_47ac, true, [(KEYS, 1)]);
snapshot_test!(keys_tick_800e47e0, 0x800e_47e0, true, [(KEYS, 3)]);
snapshot_test!(pad_check_800e1764, 0x800e_1764, true, [(KEYS, 1)]);
snapshot_test!(init_type_800e1850, 0x800e_1850, true, [(KEYS, 1)]);
snapshot_test!(load_settings_800deab0, 0x800d_eab0, false, [(KEYS, 1)]);
snapshot_test!(type_index_800dea48, 0x800d_ea48, true, [(KEYS, 1)]);
snapshot_test!(
    show_type_800e19c4,
    0x800e_19c4,
    true,
    false,
    &GPU_QUEUE,
    [(KEYS, 1)]
);
snapshot_test!(settings_open_800e1ea0, 0x800e_1ea0, true, [(KEYS, 3)]);
snapshot_test!(settings_rows_800e1a2c, 0x800e_1a2c, false, [(KEYS, 1)]);
snapshot_test!(settings_input_800e1ed0, 0x800e_1ed0, true, [(KEYS, 8)]);
snapshot_test!(analog_open_800e2298, 0x800e_2298, true, [(KEYS, 1)]);
snapshot_test!(buttons_open_800e2970, 0x800e_2970, true, [(KEYS, 1)]);
snapshot_test!(button_page_800e2ab4, 0x800e_2ab4, true, [(KEYS, 10)]);
snapshot_test!(main_rows_800df424, 0x800d_f424, false, [(KEYS, 1)]);
snapshot_test!(glyph_800de4f8, 0x800d_e4f8, true, [(KEYS, 1)]);
// The captured help line and tab arrows rewrite identical values (no
// bounce runs); the oracle covers their stores.
snapshot_test!(
    tab_arrows_800e05d0,
    0x800e_05d0,
    false,
    true,
    &[],
    [(KEYS, 1)]
);
snapshot_test!(button_grid_800e0810, 0x800e_0810, true, [(KEYS, 6)]);
snapshot_test!(button_list_open_800e09d4, 0x800e_09d4, true, [(KEYS, 1)]);
snapshot_test!(button_list_grow_800e0a9c, 0x800e_0a9c, true, [(KEYS, 2)]);
snapshot_test!(
    item_text_800e0538,
    0x800e_0538,
    false,
    true,
    &[],
    [(KEYS, 1)]
);
snapshot_test!(button_list_800e0b94, 0x800e_0b94, true, [(KEYS, 4)]);
snapshot_test!(button_list_shrink_800e0f8c, 0x800e_0f8c, true, [(KEYS, 2)]);
snapshot_test!(preset_c_800de8e0, 0x800d_e8e0, false, [(KEYS, 1)]);
snapshot_test!(calibration_open_800e4578, 0x800e_4578, true, [(KEYS, 2)]);
snapshot_test!(store_and_leave_800e46ec, 0x800e_46ec, true, [(KEYS, 2)]);
snapshot_test!(store_for_type_800df39c, 0x800d_f39c, false, [(KEYS, 1)]);
snapshot_test!(store_settings_800dee34, 0x800d_ee34, false, [(KEYS, 1)]);

// ---- SDK routines: GetODE, SpuReadDecodedData, _spu_Fr_ -------------------------------------

fn snapshot_index(capture: &str) -> PathBuf {
    directory(capture).join("snapshots.jsonl")
}

/// Entry and matching return snapshot of call `occurrence` of `callee`.
fn snapshot_pair(capture: &str, callee: u32, occurrence: usize) -> (Snapshot, Snapshot) {
    let dir = directory(capture);
    let index = snapshot::read_index(&dir.join("snapshots.jsonl")).expect("index");
    let entry = index
        .iter()
        .filter(|s| s.callee == callee && s.stage == "entry")
        .nth(occurrence)
        .expect("entry");
    let exit = index
        .iter()
        .find(|s| {
            s.callee == callee
                && s.stage == "return"
                && s.sequence > entry.sequence
                && s.ra == entry.ra
                && s.registers.gpr[29] == entry.registers.gpr[29]
        })
        .expect("return");
    (
        Snapshot::load(&dir, entry).expect("entry"),
        Snapshot::load(&dir, exit).expect("exit"),
    )
}

fn sdk_available() -> bool {
    std::env::var_os("AC3_DATA_DIR").is_some_and(|d| {
        Path::new(&d)
            .join("analysis/flight-runtime")
            .join(GAME_SDK)
            .join("snapshots.jsonl")
            .exists()
    })
}

/// 800A73C0 GetODE on the SOUND and SCREEN pages: the GPUSTAT read (host
/// result) is answered with the original's field bit; RAM, GTE and `v0`
/// must match (both fields occur).
#[test]
#[ignore = "needs the ov2brief options captures (AC3_DATA_DIR)"]
fn get_ode_800a73c0() {
    assert!(sdk_available(), "captures missing");
    let mut fields = Vec::new();
    for occurrence in 0..4 {
        let (_, exit) = snapshot_pair(GAME_SDK, 0x800a_73c0, occurrence);
        let field = exit.gpr(2);
        fields.push(field);
        snapshot::check_call(
            &snapshot_index(GAME_SDK),
            0x800a_73c0,
            occurrence,
            |m, _| {
                m.host_results.push_back(field << 31);
                Some(ac3_flight::dispatch::call(m, 0x800a_73c0, [0; 4]))
            },
        )
        .unwrap();
    }
    assert!(fields.contains(&0) && fields.contains(&1), "{fields:?}");
}

/// Device stand-in for the SPU-to-RAM DMA of `_spu_Fr_`: during its host
/// call it writes the original's bytes of the transfer (`blocks` * 64 from
/// `buf`), and it answers SpuReadDecodedData's SPUSTAT read with the
/// original's result bit. Everything else the routines write is checked.
struct SpuReplay {
    exit: Vec<u8>,
    status: u32,
    transfers: Rc<RefCell<Vec<[u32; 4]>>>,
}
impl ac3_flight::Device for SpuReplay {
    fn call(&mut self, ram: &mut [u8], _: &mut [u8], call: ac3_flight::HostCall) -> Option<u32> {
        match call.function {
            0x8009_dba4 => {
                let [buf, _, blocks, _] = call.args;
                let start = (buf & 0x1f_ffff) as usize;
                let end = start + (blocks * 64) as usize;
                ram[start..end].copy_from_slice(&self.exit[start..end]);
                self.transfers.borrow_mut().push(call.args);
                None
            }
            0x8009_f358 => Some(self.status),
            _ => None,
        }
    }
}

fn spu_replay_check(callee: u32, occurrence: usize) -> Vec<[u32; 4]> {
    let (_, exit) = snapshot_pair(GAME_SDK, callee, occurrence);
    let transfers = Rc::new(RefCell::new(Vec::new()));
    let device = SpuReplay {
        exit: exit.ram.to_vec(),
        status: if exit.gpr(2) != 0 { 0x800 } else { 0 },
        transfers: transfers.clone(),
    };
    snapshot::check_call(&snapshot_index(GAME_SDK), callee, occurrence, |m, e| {
        m.device = Some(Box::new(device));
        let args = [e.gpr(4), e.gpr(5), e.gpr(6), e.gpr(7)];
        Some(ac3_flight::dispatch::call(m, callee, args))
    })
    .unwrap();
    transfers.borrow().clone()
}

/// 8009F358 SpuReadDecodedData (from 800D0D30, part 5): the transfer it
/// starts (the CD capture buffers, 0x20 blocks from SPU 0), its RAM (the
/// DMA-read flag 800B2694) and `v0`.
#[test]
#[ignore = "needs the ov2brief options captures (AC3_DATA_DIR)"]
fn spu_read_decoded_data_8009f358() {
    assert!(sdk_available(), "captures missing");
    for occurrence in 0..2 {
        let (entry, _) = snapshot_pair(GAME_SDK, 0x8009_f358, occurrence);
        let transfers = spu_replay_check(0x8009_f358, occurrence);
        assert_eq!(transfers, vec![[entry.gpr(4), 0, 0x20, 0]]);
    }
}

/// 8009DBA4 `_spu_Fr_`: the flag 800B2694 and `v0` (1).
#[test]
#[ignore = "needs the ov2brief options captures (AC3_DATA_DIR)"]
fn spu_fr_8009dba4() {
    assert!(sdk_available(), "captures missing");
    let (entry, _) = snapshot_pair(GAME_SDK, 0x8009_dba4, 0);
    let transfers = spu_replay_check(0x8009_dba4, 0);
    assert_eq!(
        transfers,
        vec![[entry.gpr(4), entry.gpr(5), entry.gpr(6), 0]]
    );
}

// ---- oracle: the paths no capture reaches --------------------------------------------------

use support::oracle;

/// Original routine ranges (start, end) of the oracle cases.
const RANGES: [(u32, u32); 80] = [
    (0x800d_c8f4, 0x800d_c938),
    (0x800d_c938, 0x800d_c974),
    (0x800d_c974, 0x800d_c9b0),
    (0x800d_c9b0, 0x800d_caac),
    (0x800d_caac, 0x800d_cc48),
    (0x800d_cc48, 0x800d_ccc4),
    (0x800d_cd34, 0x800d_cf70),
    (0x800d_ccc4, 0x800d_cd34),
    (0x800d_cf70, 0x800d_cfa4),
    (0x800d_cfa4, 0x800d_cfec),
    (0x800d_cfec, 0x800d_d0cc),
    (0x800d_d0cc, 0x800d_d170),
    (0x800d_d170, 0x800d_d1a4),
    (0x800d_d1a4, 0x800d_d214),
    (0x800d_d214, 0x800d_d2a0),
    (0x800e_50b8, 0x800e_5288),
    (0x800e_9854, 0x800e_986c),
    (0x800d_e4f8, 0x800d_e73c),
    (0x800d_e73c, 0x800d_e818),
    (0x800d_e818, 0x800d_e8e0),
    (0x800d_e8e0, 0x800d_e978),
    (0x800d_e978, 0x800d_ea48),
    (0x800d_ea48, 0x800d_eab0),
    (0x800d_eab0, 0x800d_ee34),
    (0x800d_ee34, 0x800d_f39c),
    (0x800d_f39c, 0x800d_f424),
    (0x800d_f424, 0x800d_f650),
    (0x800d_f650, 0x800d_f93c),
    (0x800d_f93c, 0x800d_f9d0),
    (0x800d_f9d0, 0x800d_fabc),
    (0x800d_fabc, 0x800d_ff58),
    (0x800d_ff58, 0x800e_0054),
    (0x800e_0054, 0x800e_0108),
    (0x800e_0108, 0x800e_0140),
    (0x800e_0140, 0x800e_0440),
    (0x800e_0440, 0x800e_0538),
    (0x800e_0538, 0x800e_05d0),
    (0x800e_05d0, 0x800e_0810),
    (0x800e_0810, 0x800e_09d4),
    (0x800e_09d4, 0x800e_0a9c),
    (0x800e_0a9c, 0x800e_0b94),
    (0x800e_0b94, 0x800e_0f8c),
    (0x800e_0f8c, 0x800e_1040),
    (0x800e_1040, 0x800e_1128),
    (0x800e_1128, 0x800e_11b8),
    (0x800e_11b8, 0x800e_1518),
    (0x800e_1518, 0x800e_1764),
    (0x800e_1764, 0x800e_182c),
    (0x800e_182c, 0x800e_1850),
    (0x800e_1850, 0x800e_19c4),
    (0x800e_19c4, 0x800e_1a2c),
    (0x800e_1a2c, 0x800e_1ea0),
    (0x800e_1ea0, 0x800e_1ed0),
    (0x800e_1ed0, 0x800e_2298),
    (0x800e_2298, 0x800e_2390),
    (0x800e_2390, 0x800e_2970),
    (0x800e_2970, 0x800e_2ab4),
    (0x800e_2ab4, 0x800e_339c),
    (0x800e_339c, 0x800e_3adc),
    (0x800e_3adc, 0x800e_4578),
    (0x800e_4578, 0x800e_4688),
    (0x800e_4688, 0x800e_46ec),
    (0x800e_46ec, 0x800e_470c),
    (0x800e_470c, 0x800e_47ac),
    (0x800e_47ac, 0x800e_47e0),
    (0x800e_47e0, 0x800e_4978),
    (0x800c_f358, 0x800c_f404),
    (0x800d_1828, 0x800d_186c),
    (0x800d_191c, 0x800d_1ba4),
    (0x800d_1ba4, 0x800d_1f28),
    (0x800d_1f28, 0x800d_2174),
    (0x800d_2174, 0x800d_233c),
    (0x800d_233c, 0x800d_2410),
    (0x800d_2410, 0x800d_27d0),
    (0x800d_27d0, 0x800d_29d8),
    (0x800d_29d8, 0x800d_2bc8),
    (0x800d_2bc8, 0x800d_2c98),
    (0x800d_2c98, 0x800d_2e18),
    (0x800d_2e18, 0x800d_31fc),
    (0x800d_31fc, 0x800d_3254),
];

fn range(address: u32) -> (u32, u32) {
    *RANGES
        .iter()
        .find(|r| r.0 == address)
        .unwrap_or_else(|| panic!("{address:08x}: no range"))
}

/// Oracle comparisons per routine: cases compared, cases that changed RAM,
/// cases whose run reached a fault (an untranslated callee: the oracle and
/// the translation then meet the same fault at the same call and are still
/// compared).
#[derive(Default)]
struct Tally {
    labels: std::collections::BTreeMap<u32, (usize, usize, usize)>,
}

impl Tally {
    fn case(&mut self, base: &Machine, address: u32, args: [u32; 4], returns: bool) {
        let entry = self.labels.entry(address).or_default();
        let label = format!("{address:08x} oracle case {}", entry.0);
        let mut probe = base.clone();
        ac3_flight::dispatch::call(&mut probe, address, args);
        let faulted = probe.fault().is_some();
        let bytes = oracle::diff(&label, base, range(address), args, |m| {
            // By address: 800D1828 is owned by ov2_hub.
            let v0 = ac3_flight::dispatch::call(m, address, args);
            returns.then_some(v0)
        });
        entry.0 += 1;
        if bytes > 0 {
            entry.1 += 1;
        }
        if faulted {
            entry.2 += 1;
        }
    }
    /// Print the counts; every routine except the pure ones (compared on
    /// `v0`) must have changed RAM in some case.
    fn report(&self, group: &str) {
        let total: usize = self.labels.values().map(|v| v.0).sum();
        println!("ORACLE {group}: {total} cases");
        for (address, (compared, changed, faulted)) in &self.labels {
            println!(
                "ORACLE   {address:08x}: {compared} compared, {changed} changed RAM, {faulted} reached a fault"
            );
            assert!(
                *changed > 0 || PURE.contains(address),
                "{address:08x}: no case changed RAM"
            );
        }
    }
}

/// Routines that only compute `v0`.
const PURE: [u32; 3] = [0x800d_1828, 0x800d_ea48, 0x800e_182c];

/// First captured entry state of `callee` in `capture` (module 2 loaded).
fn base(capture: &str, callee: u32) -> Machine {
    oracle::entries(capture, callee)
        .into_iter()
        .next()
        .expect("entry snapshot")
        .machine()
}

fn context(m: &mut Machine) -> u32 {
    m.lw(ac3_flight::GP + 0x2dc)
}

const KEYS_BLOCK: u32 = 0x800f_6668;
const CONTROL: u32 = 0x800f_6060;
const ITEM_COUNTS: u32 = 0x800f_6540;

fn pick(rng: &mut oracle::Rng, values: &[u32]) -> u32 {
    values[(rng.next() as usize) % values.len()]
}

/// Pressed (+0x12C) and held (+0x130) buttons: none, one, or two of the
/// menu buttons.
fn press(m: &mut Machine, rng: &mut oracle::Rng) {
    const BUTTONS: [u32; 9] = [0, 1, 2, 4, 8, 0x10, 0x20, 0x40, 0];
    let ctx = context(m);
    let mut pressed = pick(rng, &BUTTONS);
    if rng.next().is_multiple_of(6) {
        pressed |= pick(rng, &BUTTONS);
    }
    let held = if rng.next() & 1 != 0 {
        pressed
    } else {
        pick(rng, &BUTTONS)
    };
    m.sw(ctx + 0x12c, pressed);
    m.sw(ctx + 0x130, held);
}

/// Vary the KEY CONFIG block and its context inputs. `groups` bounds the
/// sub-mode list (+4) and its step (+6) when given.
fn vary_keys(m: &mut Machine, rng: &mut oracle::Rng, groups: &[u32]) {
    let block = m.lw(KEYS_BLOCK);
    let kind = rng.next() % 4;
    m.sb(block + 0x30, kind as u8);
    m.sh(block + 0x2e, ((rng.next() % 7) as i32 - 1) as u16);
    m.sb(block + 0x38, (rng.next() % 4) as u8);
    let layout = m.lw(block + 0x48 + kind * 4);
    let count = u32::from(m.lbu(ITEM_COUNTS + kind));
    let item = if count > 0 { rng.next() % count } else { 0 };
    let button = m.lw(layout + item * 0x14);
    m.sb(block + 0x39, button as u8);
    m.sb(block + 0x3a, (rng.next() % 8) as u8);
    m.sb(block + 0x3b, (rng.next() % 0x15) as u8);
    m.sb(block + 0x3c, (rng.next() % 0xd) as u8);
    m.sb(block + 0x40, (rng.next() & 1) as u8);
    m.sw(block + 8, rng.next() & 3);
    m.sw(block + 0xc, rng.next() & 0x1ff7);
    m.sw(block + 0x10, rng.next() & 7);
    for k in 0..0xf {
        m.sb(block + 0x14 + k, (rng.next() % 0x15) as u8);
    }
    for k in 0..4 {
        m.sb(block + 0x24 + k, (rng.next() % 0xd) as u8);
        m.sb(block + 0x28 + k, (rng.next() & 1) as u8);
    }
    m.sb(block + 0x2c, (rng.next() % 3) as u8);
    m.sb(block + 0x2d, (rng.next() % 3) as u8);
    m.sw(block + 0x44, rng.next() % 3);
    m.sw(block + 0x6c, rng.next() % 13);
    m.sw(block + 0x74, rng.next() % 13);
    m.sh(block + 0x70, ((rng.next() % 21) as i32 - 10) as u16);
    m.sh(block + 0x72, ((rng.next() % 21) as i32 - 10) as u16);
    for k in 0..0x10 {
        m.sb(block + 0x58 + k, rng.next() as u8);
    }
    m.sb(block + 0x68, (rng.next() % 6) as u8);
    if !groups.is_empty() {
        let group = rng.next() % groups.len() as u32;
        m.sh(block + 4, group as u16);
        m.sh(block + 6, (rng.next() % groups[group as usize]) as u16);
    }
    let ctx = context(m);
    for k in 0..4 {
        m.sh(ctx + 0x62 + 2 * k, (rng.next() & 0xff) as u16);
    }
    let types = pick(rng, &[1, 2, 4, 8, 0x10, 0x20, 0x12, 0x18, 0x22]);
    m.sw(ctx + 0x124, types);
    let pad = pick(rng, &[0, 0, 0, 1, 2, 3, 8, 4]);
    m.sh(ctx + 0x156, pad as u16);
    press(m, rng);
}

/// Analog sub-mode lists (table 0x800F669C) and button ones (0x800F66CC).
const ANALOG_GROUPS: [u32; 3] = [1, 4, 3];
const BUTTON_GROUPS: [u32; 3] = [1, 4, 4];
/// KEY CONFIG task lists (table 0x800F670C).
const TASK_GROUPS: [u32; 7] = [2, 2, 2, 2, 2, 2, 1];

#[test]
#[ignore = "needs the ov2brief options captures (AC3_DATA_DIR)"]
fn oracle_key_config_pages() {
    assert!(available(), "captures missing");
    let start = base(KEYS, 0x800e_2ab4);
    let mut rng = oracle::Rng(0x00b7_c0f1);
    let mut tally = Tally::default();
    for case in 0..96u32 {
        let mut m = start.clone();
        vary_keys(&mut m, &mut rng, &ANALOG_GROUPS);
        for address in [
            0x800e_2298,
            0x800e_2390,
            0x800d_f650,
            0x800d_f93c,
            0x800d_f9d0,
            0x800d_fabc,
            0x800d_ff58,
            0x800e_0108,
            0x800e_0140,
            0x800e_0440,
        ] {
            tally.case(&m, address, [0; 4], true);
        }
        tally.case(&m, 0x800e_0054, [0; 4], false);
        tally.case(&m, 0x800d_f424, [0; 4], false);
        let mut m = start.clone();
        vary_keys(&mut m, &mut rng, &BUTTON_GROUPS);
        for address in [
            0x800e_2970,
            0x800e_2ab4,
            0x800e_0810,
            0x800e_09d4,
            0x800e_0a9c,
            0x800e_0b94,
            0x800e_0f8c,
            0x800e_1040,
            0x800e_1128,
            0x800e_11b8,
            0x800e_1518,
        ] {
            tally.case(&m, address, [0; 4], true);
        }
        let item = rng.next() % 0x18;
        tally.case(&m, 0x800e_0538, [item, 0, 0, 0], false);
        let bits = pick(&mut rng, &[0x15, 0x115, 0x215, 0x415, 0x815, 0x16, 0x917]);
        tally.case(&m, 0x800e_05d0, [bits, 0, 0, 0], false);
        let block = m.lw(KEYS_BLOCK);
        let kind = u32::from(m.lbu(block + 0x30));
        let count = u32::from(m.lbu(ITEM_COUNTS + kind)).max(1);
        let work = m.lw(0x800f_69d0);
        let cursor = m.lw(work + 0x2_0094);
        let glyph = [cursor, rng.next() % count, rng.next() % 0x18, case & 1];
        tally.case(&m, 0x800d_e4f8, glyph, true);
    }
    tally.report("KEY CONFIG pages");
}

/// 800DEAB0 (load settings) is not run under the oracle: its instruction at
/// 0x800DEAD0 is the oracle's end-of-run sentinel (`RETURN`), so the oracle
/// would stop there; the snapshot test and the oracle runs of its callers
/// (800E1850 calls it natively) cover it.
#[test]
#[ignore = "needs the ov2brief options captures (AC3_DATA_DIR)"]
fn oracle_key_config_tasks() {
    assert!(available(), "captures missing");
    let start = base(KEYS, 0x800e_1ed0);
    let mut rng = oracle::Rng(0x7a5c_0001);
    let mut tally = Tally::default();
    for case in 0..96u32 {
        let mut m = start.clone();
        vary_keys(&mut m, &mut rng, &[]);
        let block = m.lw(KEYS_BLOCK);
        let group = rng.next() % TASK_GROUPS.len() as u32;
        m.sh(block, group as u16);
        m.sh(block + 2, (rng.next() % TASK_GROUPS[group as usize]) as u16);
        if rng.next() & 1 != 0 {
            let ctx = context(&mut m);
            let types = m.lw(ctx + 0x124);
            m.sw(block + 0x34, types);
        }
        for address in [
            0x800e_1764,
            0x800e_182c,
            0x800e_1850,
            0x800e_1ea0,
            0x800e_1ed0,
            0x800e_4578,
            0x800e_4688,
            0x800e_339c,
            0x800e_3adc,
            0x800e_46ec,
            0x800e_47e0,
        ] {
            tally.case(&m, address, [0; 4], true);
        }
        for address in [
            0x800e_1a2c,
            0x800d_e73c,
            0x800d_e818,
            0x800d_e8e0,
            0x800d_e978,
            0x800d_f39c,
        ] {
            tally.case(&m, address, [0; 4], false);
        }
        tally.case(&m, 0x800d_ee34, [case & 1, 0, 0, 0], false);
        let bits = pick(&mut rng, &[0, 1, 2, 4, 8, 0x10, 0x20, 0x40, 0x3e, 0x30]);
        tally.case(&m, 0x800d_ea48, [bits, 0, 0, 0], true);
        if case.is_multiple_of(8) {
            tally.case(&m, 0x800e_19c4, [0; 4], true);
            tally.case(&m, 0x800e_470c, [0; 4], true);
            tally.case(&m, 0x800e_47ac, [0; 4], true);
        }
    }
    tally.report("KEY CONFIG tasks");
}

/// Vary the OPTIONS control block (mode, step, choice, timer, flags) and
/// the context's unlock and return bits.
fn vary_menu(m: &mut Machine, rng: &mut oracle::Rng) {
    const STEPS: [u32; 5] = [4, 3, 3, 3, 2];
    let block = m.lw(CONTROL);
    let mode = rng.next() % 5;
    m.sh(block, mode as u16);
    m.sh(block + 2, (rng.next() % STEPS[mode as usize]) as u16);
    m.sh(block + 4, (rng.next() % 4) as u16);
    m.sh(block + 6, ((rng.next() % 66) as i32 - 10) as u16);
    m.sh(block + 8, (rng.next() & 7) as u16);
    let ctx = context(m);
    let bits = (m.lw(ctx + 0x964) & !0x44) | (rng.next() & 0x40);
    m.sw(ctx + 0x964, bits);
    let unlocked = (m.lw(ctx + 0x974) & !2) | (rng.next() & 2);
    m.sw(ctx + 0x974, unlocked);
    let options = m.lw(ctx + 0x970) ^ (rng.next() & 0x50);
    m.sw(ctx + 0x970, options);
    press(m, rng);
}

#[test]
#[ignore = "needs the ov2brief options captures (AC3_DATA_DIR)"]
fn oracle_options_menu() {
    assert!(available(), "captures missing");
    let start = base(KEYS, 0x800d_cd34);
    let mut rng = oracle::Rng(0x0e71_0e55);
    let mut tally = Tally::default();
    for _ in 0..128u32 {
        let mut m = start.clone();
        vary_menu(&mut m, &mut rng);
        let slot = rng.next() % 10;
        let t = ((rng.next() % 40) as i32 - 12) as u32;
        for address in [0x800d_c8f4, 0x800d_c938, 0x800d_c974] {
            tally.case(&m, address, [slot, t, 0, 0], true);
        }
        tally.case(&m, 0x800d_c9b0, [t, 0, 0, 0], false);
        for address in [
            0x800d_caac,
            0x800d_cc48,
            0x800d_cd34,
            0x800d_ccc4,
            0x800d_cf70,
            0x800d_cfa4,
            0x800d_cfec,
            0x800d_d0cc,
            0x800d_d170,
            0x800d_d1a4,
            0x800d_d214,
        ] {
            tally.case(&m, address, [0; 4], true);
        }
        let control = m.lw(CONTROL);
        tally.case(&m, 0x800e_50b8, [control, 0, 0, 0], true);
        tally.case(&m, 0x800e_9854, [rng.next() % 4, 0, 0, 0], true);
    }
    tally.report("OPTIONS menu");
}

const RESULTS: u32 = 0x800f_5d50;
const RECORD: u32 = 0x800f_942c;

/// A flight result for state 11: the record at context +0x3BC (time, track,
/// events of every class), the results block (clock, event index, listed
/// kinds with names, markers, text paging) and the context fields the
/// screen reads.
fn vary_results(m: &mut Machine, rng: &mut oracle::Rng) {
    let ctx = context(m);
    let record = ctx + 0x3bc;
    m.sw(RECORD, record);
    m.sh(record + 2, (rng.next() % 0x6000) as u16);
    let length = 1 + rng.next() % 0x60;
    m.sh(record + 8, length as u16);
    let events = rng.next() % 14;
    m.sb(record + 0xa, events as u8);
    m.sb(record + 0xb, (rng.next() % 6) as u8);
    for i in 0..=length {
        m.sb(record + 0xc + 2 * i, rng.next() as u8);
        m.sb(record + 0xd + 2 * i, rng.next() as u8);
    }
    for k in 0..events {
        let event = record + 0x390 + 8 * k;
        let kind = match rng.next() % 4 {
            0 => rng.next() % 0x40,
            1 => 0x70 + rng.next() % 5,
            2 => rng.next() & 0xff,
            _ => rng.next() % 4,
        };
        m.sb(event, kind as u8);
        m.sb(event + 1, (rng.next() % 6) as u8);
        m.sh(event + 2, (rng.next() % (4 * length + 8)) as u16);
        m.sb(event + 4, rng.next() as u8);
        m.sb(event + 5, rng.next() as u8);
    }
    let block = m.lw(RESULTS);
    m.sw(block, rng.next() & 3);
    m.sw(block + 4, pick(rng, &[0, 5, 9, 10, 0x5e, 0x5f, 0x60]));
    m.sw(block + 8, pick(rng, &[0xffff_ffff, 0, 1, 2, 3, 5]));
    m.sw(block + 0xc, rng.next() % 6);
    m.sh(block + 0x10, 4);
    m.sh(block + 0x12, 0);
    m.sw(block + 0x14, rng.next() % (4 * length + 16));
    m.sw(block + 0x18, rng.next() % 12);
    m.sw(block + 0x1c, rng.next() % (events + 1));
    for (count, base, air) in [(0x20u32, 0u32, false), (0x22, 0x160, true)] {
        let kinds = rng.next() % 4;
        m.sh(block + count, kinds as u16);
        for k in 0..kinds {
            let entry = block + base + 0x2c * k;
            for c in 0..6 {
                m.sb(entry + 0x124 + c, b'A' + (rng.next() % 26) as u8);
            }
            m.sb(entry + 0x124 + 6, 0);
            let kind = if air {
                0x70 + rng.next() % 5
            } else {
                rng.next() % 0x40
            };
            m.sb(entry + 0x14c, kind as u8);
            m.sb(entry + 0x14d, (rng.next() % 12) as u8);
        }
    }
    let shown = rng.next() % 6;
    for k in 0..0x40 {
        let marker = block + 0x24 + 4 * k;
        m.sb(marker, rng.next() as u8);
        m.sb(marker + 1, rng.next() as u8);
        m.sb(marker + 2, (rng.next() % 6) as u8);
        m.sb(marker + 3, u8::from(k < shown));
    }
    m.sw(ctx + 0x398, rng.next() % 5);
    m.sb(ctx + 0x3c0, (rng.next() % 0x30) as u8);
    m.sb(ctx + 0x3c1, (rng.next() % 0x30) as u8);
    m.sh(ctx + 0x95c, (rng.next() % 2000) as u16);
    m.sw(ctx + 0x38c, rng.next() % 5);
    let options = m.lw(ctx + 0x970) ^ (rng.next() & 0x10);
    m.sw(ctx + 0x970, options);
    m.sh(ctx + 0x390, (rng.next() % 0x30) as u16);
    press(m, rng);
}

#[test]
#[ignore = "needs the ov2brief options captures (AC3_DATA_DIR)"]
fn oracle_debriefing() {
    assert!(available(), "captures missing");
    let start = base(KEYS, 0x800d_cd34);
    let mut rng = oracle::Rng(0x0011_7e57);
    let mut tally = Tally::default();
    for case in 0..96u32 {
        let mut m = start.clone();
        vary_results(&mut m, &mut rng);
        let work = m.lw(0x800f_69d0);
        let cursor = m.lw(work + 0x2_0094);
        let ot = m.lw(work + 0x90);
        for address in [
            0x800d_1f28,
            0x800d_2174,
            0x800d_2410,
            0x800d_27d0,
            0x800d_29d8,
            0x800d_2bc8,
        ] {
            tally.case(&m, address, [cursor, ot, 0, 0], true);
        }
        tally.case(&m, 0x800d_233c, [cursor, case % 3, 0, 0], true);
        tally.case(&m, 0x800d_1ba4, [0; 4], true);
        tally.case(&m, 0x800d_191c, [0; 4], false);
        tally.case(&m, 0x800d_1828, [rng.next() % 0x20, 0, 0, 0], true);
        for address in [0x800d_2c98, 0x800d_2e18, 0x800d_31fc] {
            tally.case(&m, address, [0; 4], true);
        }
        // 800CF358 with its stack arguments in the caller's frame.
        let sp = m.sp;
        m.sw(sp + 0x10, rng.next() & 0x1ff);
        m.sw(sp + 0x14, rng.next() & 0x1ff);
        m.sw(sp + 0x18, case & 1);
        m.sw(sp + 0x1c, sp + 0x20);
        m.sw(sp + 0x20, rng.next());
        let ends = [cursor, ot, rng.next() & 0x1ff, rng.next() & 0x1ff];
        tally.case(&m, 0x800c_f358, ends, true);
    }
    tally.report("DEBRIEFING");
}

// ---- oracle: SDK additions -----------------------------------------------------------------

/// MoveImage on rectangles of every kind (empty width or height, both
/// halves of VRAM, odd sizes) and every libgpu debug level (the traces are
/// host calls).
#[test]
#[ignore = "needs the ov2brief options captures (AC3_DATA_DIR)"]
fn oracle_move_image_800a69d0() {
    assert!(available(), "captures missing");
    let start = base(KEYS, 0x800d_cd34);
    let mut rng = oracle::Rng(0x0a69_d0d0);
    let rect = 0x801f_0000;
    let (mut compared, mut empty) = (0, 0);
    for case in 0..64u32 {
        let mut m = start.clone();
        let w = if case % 9 == 0 { 0 } else { rng.next() % 0x400 };
        let h = if case % 11 == 0 {
            0
        } else {
            rng.next() % 0x200
        };
        m.sh(rect, (rng.next() % 0x400) as u16);
        m.sh(rect + 2, (rng.next() % 0x200) as u16);
        m.sh(rect + 4, w as u16);
        m.sh(rect + 6, h as u16);
        m.sb(0x800b_97e6, (case % 3) as u8);
        let args = [rect, rng.next() % 0x400, rng.next() % 0x200, 0];
        let label = format!("800a69d0 case {case}");
        oracle::diff(&label, &m, (0x800a_69d0, 0x800a_6a88), args, |m| {
            Some(ac3_flight::dispatch::call(m, 0x800a_69d0, args))
        });
        compared += 1;
        if w == 0 || h == 0 {
            empty += 1;
        }
    }
    println!("ORACLE MoveImage: {compared} compared, {empty} empty rectangles");
    assert!(empty > 0 && empty < compared);
}

/// GetODE with both fields (the GPUSTAT host result).
#[test]
#[ignore = "needs the ov2brief options captures (AC3_DATA_DIR)"]
fn oracle_get_ode_800a73c0() {
    assert!(available(), "captures missing");
    let start = base(KEYS, 0x800d_cd34);
    let mut rng = oracle::Rng(0x0a73_c0c0);
    for case in 0..32u32 {
        let mut m = start.clone();
        m.host_results.push_back(rng.next() << (case & 8));
        let label = format!("800a73c0 case {case}");
        oracle::diff(&label, &m, (0x800a_73c0, 0x800a_73f0), [0; 4], |m| {
            Some(ac3_flight::dispatch::call(m, 0x800a_73c0, [0; 4]))
        });
    }
}

/// `__udivdi3` against the original instructions (8009CF18 with
/// `__udivmoddi4` and its blocks, 8009CF18..8009D548): the low word as the
/// call's `v0`, the high word through a six-instruction stub at 8009CF00
/// (`move s7, ra; jal 8009CF18; nop; move v0, v1; jr s7; nop`, over the
/// end of `__divdi3`, which the run does not use). A zero divisor reaches
/// libgcc's `break 7`, which the oracle and the translation record
/// differently, so it is checked on its own.
#[test]
#[ignore = "needs the ov2brief options captures (AC3_DATA_DIR)"]
fn oracle_udivdi3_8009cf18() {
    assert!(available(), "captures missing");
    let start = base(KEYS, 0x800d_cd34);
    let mut rng = oracle::Rng(0x0009_cf18);
    let stub = [0x03e0_b821u32, 0x0c02_73c6, 0, 0x0060_1021, 0x02e0_0008, 0];
    for case in 0..400u32 {
        let wide = |rng: &mut oracle::Rng| {
            (u64::from(rng.next()) << 40) ^ (u64::from(rng.next()) << 16) ^ u64::from(rng.next())
        };
        let numerator = match case % 4 {
            0 => wide(&mut rng),
            1 => u64::from(rng.next()),
            2 => u64::MAX - u64::from(rng.next()),
            _ => wide(&mut rng) >> (rng.next() % 64),
        };
        let mut denominator = match case % 5 {
            0 => u64::from(rng.next() % 0x10000),
            1 => u64::from(rng.next()),
            2 => wide(&mut rng) >> (rng.next() % 64),
            3 => numerator >> (rng.next() % 8),
            _ => wide(&mut rng),
        };
        if denominator == 0 {
            denominator = 1 + u64::from(rng.next() % 7);
        }
        let args = [
            numerator as u32,
            (numerator >> 32) as u32,
            denominator as u32,
            (denominator >> 32) as u32,
        ];
        let mut low = start.clone();
        let low_word = oracle::run(&mut low, 0x8009_cf18, 0x8009_d548, args);
        let mut high = start.clone();
        for (k, word) in stub.iter().enumerate() {
            high.sw(0x8009_cf00 + 4 * k as u32, *word);
        }
        let high_word = oracle::run(&mut high, 0x8009_cf00, 0x8009_d548, args);
        assert_eq!(
            low.take_fault(),
            Ok(()),
            "{numerator:#x} / {denominator:#x}"
        );
        assert_eq!(
            high.take_fault(),
            Ok(()),
            "{numerator:#x} / {denominator:#x}"
        );
        let mut native = start.clone();
        let quotient = sdk::udivdi3(&mut native, numerator, denominator);
        assert_eq!(
            (quotient as u32, (quotient >> 32) as u32),
            (low_word, high_word),
            "{numerator:#x} / {denominator:#x}"
        );
        let mut by_address = start.clone();
        assert_eq!(
            ac3_flight::dispatch::call(&mut by_address, 0x8009_cf18, args),
            low_word
        );
    }
    let mut m = start.clone();
    assert_eq!(sdk::udivdi3(&mut m, 5, 0), 0);
    assert!(m.take_fault().is_err(), "the zero divisor traps");
}
