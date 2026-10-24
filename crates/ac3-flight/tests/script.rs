//! Mission script interpreter 80053704 and mission step 80037B24 on
//! original entry snapshots must reproduce the original return snapshots.
//!
//! Captures: `script-002` (window 80032510: the init script run by
//! 80052574 with opcode 0x01 → 80037B24, and the 80031C20 script with
//! opcode 0x2D), `script-003` (flight callbacks 80033C58), `script-007`
//! (original-code oracle over all opcodes on the flight savestate).
use ac3_flight::{Machine, dispatch, script, shared, snapshot};
use std::path::PathBuf;

fn data(path: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR")).join(path)
}
fn initialization() -> PathBuf {
    data("analysis/flight-runtime/script-002/snapshots.jsonl")
}
fn flight() -> PathBuf {
    data("analysis/flight-runtime/script-003/snapshots.jsonl")
}

fn interpret(m: &mut Machine, entry: &snapshot::Snapshot) -> Option<u32> {
    Some(script::interpret(m, entry.gpr(4)))
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/script-002"]
fn init_script_80053704_with_step() {
    let changed = snapshot::check_call(&initialization(), 0x8005_3704, 0, interpret).unwrap();
    eprintln!("original changed {changed} bytes");
    assert!(changed > 0, "{changed}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/script-002"]
fn tail_script_80053704() {
    let changed = snapshot::check_call(&initialization(), 0x8005_3704, 1, interpret).unwrap();
    eprintln!("original changed {changed} bytes");
    assert!(changed > 0, "{changed}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/script-002"]
fn mission_step_80037b24() {
    let changed = snapshot::check_call(&initialization(), 0x8003_7b24, 0, |m, _| {
        script::step_80037b24(m);
        None
    })
    .unwrap();
    eprintln!("original changed {changed} bytes");
    assert!(changed > 0, "{changed}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/script-003"]
fn flight_script_80053704() {
    let changed = snapshot::check_call(&flight(), 0x8005_3704, 0, interpret).unwrap();
    eprintln!("original changed {changed} bytes");
    assert!(changed > 0, "{changed}");
}

// ---- original oracle cases (script-007) ------------------------------------------------

/// `script-007`: original 80053704/80037B24 (and directly called reached
/// routines) run by `PCSX.callGuest` on the
/// flight savestate `script-003/final.sav` (frame 640) with staged script
/// bytes and state fields (`cases.json`); `case-NNN.ram.bin` is all RAM
/// after the call. The guest entry `$sp` is the oracle's `sp` minus the
/// 16-byte argument area callGuest reserves. Cases marked `needs` reach a
/// routine owned by another module; while it is a stub they must stop with
/// exactly that `Fault::Untranslated`.
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/script-007"]
fn oracle_cases() {
    use serde_json::Value;
    const ORACLE_SP: u32 = 0x801f_f000;
    let directory = data("analysis/flight-runtime/script-007");
    let read = |name: &str| std::fs::read(directory.join(name)).unwrap();
    let ram = read("baseline.ram.bin");
    let scratch = read("baseline.scratch.bin");
    let registers: Value = serde_json::from_slice(&read("baseline.json")).unwrap();
    let words = |v: &Value| -> [u32; 32] { std::array::from_fn(|i| v[i].as_u64().unwrap() as u32) };
    let cases: Value = serde_json::from_slice(&read("cases.json")).unwrap();
    let results: Vec<Value> = String::from_utf8(read("results.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let cases = cases.as_array().unwrap();
    assert_eq!(cases.len(), results.len());
    let (mut exact, mut pending, mut changed_total) = (0, Vec::new(), 0usize);
    let mut failures = Vec::new();
    let mut unchanged = Vec::new();
    for (case, result) in cases.iter().zip(&results) {
        let name = case["name"].as_str().unwrap();
        assert_eq!(result["status"], "returned", "{name}");
        assert_eq!(result["stores_dropped"], false, "{name}");
        let expected = read(&format!("{}.ram.bin", result["stem"].as_str().unwrap()));
        let mut m = Machine::from_images(&ram, &scratch).unwrap();
        m.gte.control = words(&registers["cp2c"]);
        m.gte.data = words(&registers["cp2d"]);
        m.sp = ORACLE_SP - 16;
        for stage in case["stages"].as_array().unwrap() {
            let address = stage[0].as_u64().unwrap() as u32;
            let hex = stage[1].as_str().unwrap();
            let bytes: Vec<u8> = (0..hex.len() / 2)
                .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap())
                .collect();
            m.write_bytes(address, &bytes);
        }
        let staged = m.ram().to_vec();
        let pc = case["pc"].as_u64().unwrap() as u32;
        let args: Vec<u32> = case["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_u64().unwrap() as u32)
            .collect();
        let arg = |i: usize| args.get(i).copied().unwrap_or(0);
        let v0 = match pc {
            0x8005_3704 => Some(script::interpret(&mut m, arg(0))),
            0x8003_7b24 => {
                script::step_80037b24(&mut m);
                None
            }
            // Other reached routines by original address (script.rs and
            // init_38818.rs claim every routine they translate).
            _ => {
                let v0 = dispatch::call(&mut m, pc, [arg(0), arg(1), arg(2), arg(3)]);
                case["check_v0"].as_bool().unwrap_or(false).then_some(v0)
            }
        };
        let fault = m.take_fault();
        let changed = (0..snapshot::STACK_START)
            .filter(|&i| staged[i] != expected[i])
            .count();
        changed_total += changed;
        if let Some(needs) = case["needs"].as_str()
            && let Err(ac3_flight::Fault::Untranslated { address }) = fault
        {
            pending.push(format!("{name} (stops at {address:08x}; needs {needs})"));
            continue;
        }
        let differing: Vec<String> = (0..snapshot::STACK_START)
            .filter(|&i| m.ram()[i] != expected[i])
            .take(8)
            .map(|i| format!("{:08x}", 0x8000_0000 + i as u32))
            .collect();
        let v0_ok = v0.is_none_or(|v| u64::from(v) == result["v0"].as_u64().unwrap());
        if fault.is_err() || !differing.is_empty() || !v0_ok {
            failures.push(format!(
                "{name}: fault {fault:?}, v0 {v0:x?} vs {}, ram {differing:?}",
                result["v0"]
            ));
        } else {
            if changed == 0 {
                unchanged.push(name.to_owned());
            }
            exact += 1;
        }
    }
    eprintln!(
        "oracle: {exact} cases exact, {} pending, original changed {changed_total} bytes",
        pending.len()
    );
    for line in &pending {
        eprintln!("  pending: {line}");
    }
    assert!(failures.is_empty(), "{failures:#?}");
    // Cases whose original run leaves RAM unchanged: deliberate no-op and
    // refusal paths (the link and cell cases also check `v0`).
    assert_eq!(
        unchanged,
        [
            "op4e_bit10_set",
            "op15_unit",
            "op33_message_same",
            "link_full_8004577c",
            "link_player_locked_8004577c",
            "effect_cell_unchanged_8005cd38",
        ],
        "vacuous cases"
    );
}

// ---- synthetic edge paths --------------------------------------------------------------

const GP: u32 = ac3_flight::GP;
const STATE: u32 = 0x8010_0000;
const CONTEXT: u32 = 0x8010_1000;
const GAMEPLAY: u32 = 0x8010_3000;
const ENTITIES: u32 = 0x8010_4000;
const RESOURCES: u32 = 0x8010_5000;
const SCRIPT: u32 = 0x8011_0000;

fn machine() -> Machine {
    let mut m = Machine::default();
    m.sw(GP + 0x330, STATE);
    m.sw(GP + 0x2dc, CONTEXT);
    m.sw(GP + 0x2d0, GAMEPLAY);
    m.sw(GP + 0x2d8, ENTITIES);
    m.sw(GP + 0x2cc, RESOURCES);
    for i in 0..64 {
        m.sw(ENTITIES + i * 4, 0x8012_0000 + i * 0x100);
    }
    m
}

fn run(m: &mut Machine, bytes: &[u8]) -> u32 {
    m.write_bytes(SCRIPT, bytes);
    let end = script::interpret(m, SCRIPT);
    assert!(m.take_fault().is_ok());
    end
}

#[test]
fn returns_terminator_and_skips_high_opcodes() {
    let mut m = machine();
    assert_eq!(run(&mut m, &[0]), SCRIPT);
    assert_eq!(run(&mut m, &[0x50, 0xff, 0x26, 0x45, 0]), SCRIPT + 4);
    assert_eq!(m.sp, ac3_flight::machine::INITIAL_SP, "frames balanced");
}

#[test]
fn mission_byte_opcodes() {
    let mut m = machine();
    let end = run(
        &mut m,
        &[
            0x02, 7, 0x03, 8, 0x04, 9, 0x05, 10, 0x06, 3, 0x2d, 11, 0x2f, 12, 0x34, 13, 0x36, 14,
            0x39, 15, 0x31, 0x31, 0x4a, 1, 2, 3, 4, 0x48, 5, 0xfe, 0xff, 0x3a, 0x00, 0x80, 0,
        ],
    );
    assert_eq!(end, SCRIPT + 34);
    let bytes = m.read_bytes(STATE, 0x40);
    assert_eq!(&bytes[0x23..0x28], &[8, 7, 9, 10, 8]);
    assert_eq!((bytes[0x1c], bytes[0x1e], bytes[0x1f]), (15, 11, 12));
    assert_eq!((bytes[0x2b], bytes[0x2c], bytes[0x22]), (13, 14, 2));
    assert_eq!(&bytes[0x34..0x38], &[1, 2, 3, 4]);
    assert_eq!((bytes[0x31], m.lhu(STATE + 0x32)), (5, 0xfffe));
    assert_eq!(m.lw(STATE + 0x14), 0xffff_8000, "sign-extended");
}

#[test]
fn opcode_06_leaves_unknown_values() {
    let mut m = machine();
    m.sb(STATE + 0x27, 0x77);
    run(&mut m, &[0x06, 9, 0]);
    assert_eq!(m.lbu(STATE + 0x27), 0x77);
    run(&mut m, &[0x06, 7, 0]);
    assert_eq!(m.lbu(STATE + 0x27), 0x40);
}

#[test]
fn condition_blocks_choose_then_or_else() {
    // 0x0D [value][skip][else] then: 0x02 1 | else: 0x02 2 ; continue: 0x03 9.
    let block = |value| [0x0d, value, 8, 4, 0x02, 1, 0, 0x02, 2, 0, 0x03, 9, 0];
    let mut m = machine();
    m.sb(STATE + 0x22, 4);
    let end = run(&mut m, &block(4));
    assert_eq!((m.lbu(STATE + 0x24), m.lbu(STATE + 0x23)), (1, 9));
    assert_eq!(end, SCRIPT + 12);
    run(&mut m, &block(5));
    assert_eq!(m.lbu(STATE + 0x24), 2);
}

#[test]
fn block_word_comparison_is_signed() {
    let mut m = machine();
    m.sw(STATE + 0x10, 0xffff_fff0);
    // limit -1 >= value -16 → then-block.
    let script = [
        0x0b, 0xff, 0xff, 0xff, 0xff, 8, 4, 0x02, 1, 0, 0x02, 2, 0, 0,
    ];
    run(&mut m, &script);
    assert_eq!(m.lbu(STATE + 0x24), 1);
    m.sw(STATE + 0x10, 0);
    run(&mut m, &script);
    assert_eq!(m.lbu(STATE + 0x24), 2);
}

#[test]
fn rank_modes() {
    let mut m = machine();
    let set = |m: &mut Machine, mode: u8| m.sb(STATE + 0x34, mode);
    set(&mut m, 0xff);
    assert_eq!(script::rank_80053604(&mut m), 5);
    set(&mut m, 0);
    assert_eq!(script::rank_80053604(&mut m), 4);
    set(&mut m, 3);
    assert_eq!(script::rank_80053604(&mut m), 4);
    set(&mut m, 2);
    m.sh(STATE + 0x38, 100);
    m.sh(STATE + 0x3a, 200);
    m.sw(STATE + 0xc, 50);
    assert_eq!(script::rank_80053604(&mut m), 1);
    m.sw(STATE + 0xc, 150);
    assert_eq!(script::rank_80053604(&mut m), 2);
    m.sw(STATE + 0xc, 250);
    assert_eq!(script::rank_80053604(&mut m), 3);
    set(&mut m, 4);
    m.sw(STATE + 4, 100);
    assert_eq!(script::rank_80053604(&mut m), 1);
    m.sw(STATE + 4, 50);
    assert_eq!(script::rank_80053604(&mut m), 3);
    m.sw(STATE + 4, 0xffff_ffff);
    assert_eq!(script::rank_80053604(&mut m), 3, "signed word");
}

#[test]
fn step_flag_paths_request_transitions() {
    let mut m = machine();
    for (flags, expected) in [(1u8, 0x80u32), (2, 0x10_0000)] {
        m.sb(STATE + 0x2a, flags | 0x10);
        script::step_80037b24(&mut m);
        assert!(m.take_fault().is_ok());
        assert_eq!(m.lw(RESOURCES + 0x70), expected);
        assert_eq!(m.lw(RESOURCES + 0x74), 0);
        assert_eq!(m.lbu(STATE + 0x2a), 0x10);
        assert_eq!(m.lw(GAMEPLAY) & 0x2000_0000, 0x2000_0000);
    }
    assert_eq!(m.lbu(STATE + 0x3f), 2, "step counter");
    m.sb(STATE + 0x29, 3);
    m.sb(STATE + 0x25, 0x42);
    script::step_80037b24(&mut m);
    assert_eq!(
        (m.lw(RESOURCES + 0x70), m.lw(RESOURCES + 0x74)),
        (0x400, 0x42)
    );
    m.sb(STATE + 0x29, 7);
    script::step_80037b24(&mut m);
    assert!(
        m.take_fault().is_err(),
        "undefined phase needs caller registers"
    );
}

#[test]
fn links_attach_and_detach() {
    let mut m = machine();
    let links = shared::LOCK_LINKS;
    for i in 0..64 {
        m.write_bytes(links + i * 4, &[0, 0xff, 0xff, 0xff]);
    }
    m.sw(CONTEXT + 0x398, 1);
    assert_eq!(shared::set_lock(&mut m, 5, 0), 0);
    assert_eq!(shared::set_lock(&mut m, 6, 0), 0);
    assert_eq!(shared::set_lock(&mut m, 7, 0), 0xff, "slots full");
    assert_eq!(m.read_bytes(links, 4), [0, 0xff, 5, 6]);
    assert_eq!(m.lbu(links + 5 * 4 + 1), 0);
    shared::release_lock(&mut m, 6);
    assert_eq!(m.read_bytes(links, 4), [0, 0xff, 5, 0xff]);
    assert_eq!(m.lbu(links + 6 * 4 + 1), 0xff);
    m.sw(CONTEXT + 0x398, 0);
    assert_eq!(shared::set_lock(&mut m, 8, 0), 0xff, "player slot 3 locked");
    assert!(m.take_fault().is_ok());
}

#[test]
fn message_layout_with_accent() {
    let mut m = machine();
    let control = 0x8013_0000;
    m.sw(GP + 0x470, control);
    // Glyph 'E' (4) width 8; accent glyph 0x20 width 4, raise 3.
    m.write_bytes(0x800b_0070 + 4 * 4, &[0, 0, 8, 0]);
    m.write_bytes(0x800b_0070 + 0x20 * 4, &[0, 0, 4, 3]);
    m.write_bytes(0x8013_1000, &[0xc9, b' ', b'E', 0]);
    m.sh(0x8013_2000, 10);
    m.sh(0x8013_2002, 20);
    m.sw(0x8013_2004, 0x8013_1000);
    script::layout_text_8004b920(&mut m, 0x8013_2000);
    assert_eq!(m.lw(control + 0x110), 3);
    let glyph = |m: &mut Machine, i: u32| m.read_bytes(control + 8 + i * 8, 8);
    assert_eq!(glyph(&mut m, 0), [0, 0x20, 0, 0, 12, 0, 21, 0]);
    assert_eq!(glyph(&mut m, 1), [0, 4, 0, 0, 10, 0, 20, 0]);
    assert_eq!(glyph(&mut m, 2), [0, 4, 0, 0, 27, 0, 20, 0]);
    assert!(m.take_fault().is_ok());
}
