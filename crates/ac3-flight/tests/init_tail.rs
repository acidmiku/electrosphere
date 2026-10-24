//! Tail of 80032510 (80032A6C..80032C78) on original snapshots: its direct
//! callees (`init-callees-001`), their subtree (`tail-001`) and the RAM stage
//! snapshots of `initialization-002`, plus unit tests for untraced paths.
//! Tests whose path reaches the mission script interpreter 80053704 end in
//! `_needs_script`; they fail until `script::interpret` is translated.
use ac3_flight::{
    Fault, GP, Machine,
    init_tail::{self as tail},
    shared,
    snapshot::{self, STACK_START, Snapshot},
};
use std::path::{Path, PathBuf};

mod support;
use support::oracle;

fn data(relative: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR")).join(relative)
}
fn callees() -> PathBuf {
    data("analysis/flight-runtime/init-callees-001/callees.jsonl")
}
fn subtree() -> PathBuf {
    data("analysis/flight-runtime/tail-001/snapshots.jsonl")
}
fn stages() -> PathBuf {
    data("analysis/menu-runtime/initialization-002")
}

const GAMEPLAY: u32 = GP + 0x2d0;
const ENTITIES: u32 = GP + 0x2d8;
const CONTEXT: u32 = GP + 0x2dc;
const RESOURCES: u32 = GP + 0x2cc;
const LOW: u32 = 0x8000_0001;
const HIGH: u32 = 0x7fff_ffff;

/// The `occurrence`th `stage` snapshot of `callee` in `index`.
fn snap(index: &Path, callee: u32, stage: &str, occurrence: usize) -> Snapshot {
    let records = snapshot::read_index(index).unwrap();
    let info = records
        .iter()
        .filter(|r| r.callee == callee && r.stage == stage)
        .nth(occurrence)
        .unwrap_or_else(|| panic!("{callee:08x} {stage} #{occurrence}"));
    Snapshot::load(index.parent().unwrap(), info).unwrap()
}

/// Run `native` on `entry` and require `exit` exactly (RAM outside the
/// stack, scratchpad, GTE, and `v0` when returned). Returns the bytes the
/// original changed.
fn check_span(
    entry: &Snapshot,
    exit: &Snapshot,
    native: impl FnOnce(&mut Machine) -> Option<u32>,
) -> usize {
    let mut m = entry.machine();
    let v0 = native(&mut m);
    if let Err(fault) = m.take_fault() {
        panic!("{fault}");
    }
    let differences = snapshot::compare(&m, exit);
    assert!(differences.is_empty(), "{}", differences.summary());
    if let Some(v0) = v0 {
        assert_eq!(v0, exit.gpr(2), "v0 {v0:#010x}");
    }
    snapshot::changed_bytes(entry, exit)
}

fn stage_ram(name: &str) -> Vec<u8> {
    std::fs::read(stages().join(format!("initialization-{name}.ram.bin"))).unwrap()
}

/// `result` (v0) recorded with stage snapshot `name`.
fn stage_result(name: &str) -> u32 {
    let file = format!("initialization-{name}.ram.bin");
    let text = std::fs::read_to_string(stages().join("initialization.jsonl")).unwrap();
    let record = text
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|v| v["ram_file"] == file.as_str())
        .unwrap();
    record["result"].as_u64().unwrap() as u32
}

/// RAM-only stage check (these snapshots have no scratchpad or GTE state):
/// run `native` on stage `from` and require stage `to` outside the stack.
fn check_stages(from: &str, to: &str, native: impl FnOnce(&mut Machine) -> Option<u32>) -> usize {
    let entry = stage_ram(from);
    let exit = stage_ram(to);
    let mut m = Machine::from_images(&entry, &[0; 0x400]).unwrap();
    let v0 = native(&mut m);
    if let Err(fault) = m.take_fault() {
        panic!("{fault}");
    }
    let differing: Vec<String> = (0..STACK_START)
        .filter(|&i| m.ram()[i] != exit[i])
        .map(|i| format!("{:08x}", 0x8000_0000 + i))
        .collect();
    assert!(
        differing.is_empty(),
        "ram {} [{}]",
        differing.len(),
        differing[..differing.len().min(16)].join(" ")
    );
    if let Some(v0) = v0 {
        assert_eq!(v0, stage_result(to), "v0");
    }
    (0..STACK_START).filter(|&i| entry[i] != exit[i]).count()
}

/// Machine holding the original resident executable at its load address.
fn resident() -> Machine {
    let exe = std::fs::read(data("extracted/programs/ACE.unpacked.exe")).unwrap();
    let word = |at: usize| u32::from_le_bytes(exe[at..at + 4].try_into().unwrap());
    let (address, size) = (word(0x18), word(0x1c) as usize);
    let mut m = Machine::default();
    m.write_bytes(address, &exe[0x800..0x800 + size]);
    m
}

// ---- 80032A6C..80032BD4: inline entity parameter adjustment ----

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/init-callees-001"]
fn entity_adjustment_80032a6c() {
    // 8004C1A0 returns to 80032A6C; 80035248 is entered from 80032BD4.
    let entry = snap(&callees(), 0x8004_c1a0, "return", 0);
    let exit = snap(&callees(), 0x8003_5248, "entry", 0);
    let changed = check_span(&entry, &exit, |m| {
        tail::adjust_entity_parameters(m);
        None
    });
    assert!(changed > 20, "{changed}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/menu-runtime/initialization-002"]
fn entity_adjustment_stages_07_08() {
    let changed = check_stages("07-record_adjustment", "08-record_adjustment_done", |m| {
        tail::adjust_entity_parameters(m);
        None
    });
    assert!(changed > 20, "{changed}");
}

#[test]
fn entity_adjustment_paths() {
    let mut m = Machine::default();
    let (context, gameplay, table) = (0x8010_0000, 0x8010_1000, 0x8010_4000);
    let (by_16, by_4) = (0x8010_2000, 0x8010_3000);
    m.sw(CONTEXT, context);
    m.sw(GAMEPLAY, gameplay);
    m.sw(ENTITIES, table);
    m.sw(context + 0xf80, 0x40);
    m.sw(context + 0xf88, 4);
    m.sw(gameplay + 0x250, by_16);
    m.sw(gameplay + 0x258, by_4);
    let entity = |i: u32| 0x8010_5000 + i * 0x100;
    for i in 0..10 {
        m.sw(table + i * 4, entity(i));
    }
    // (index, flags, kind, byte5, +0x4A, +0x4C, record half +2)
    let cases: [(u32, u32, u8, u8, u16, u16, u16); 8] = [
        (2, 0x4000, 3, 0, 10, 0xfff0, 0x0100),
        (3, 0x4000, 4, 0, 1, 2, 0x0020),
        (4, 0x4000, 5, 0, 7, 7, 0x8000),
        (5, 0x4020, 2, 0, 0, 5, 100),
        (6, 0x4020, 4, 0, 0xfffb, 0, 0),
        (7, 0x4020, 1, 2, 0xfffe, 0, 0x0200),
        (8, 0x4000, 16, 0, 3, 3, 0x0100),
        (9, 0, 0, 0, 3, 3, 0),
    ];
    for (n, &(i, flags, kind, byte5, first, second, half)) in cases.iter().enumerate() {
        let e = entity(i);
        m.sw(e, flags);
        m.sb(e + 4, kind);
        m.sb(e + 5, byte5);
        m.sh(e + 0x4a, first);
        m.sh(e + 0x4c, second);
        let record = 0x8010_6000 + n as u32 * 0x10;
        m.sh(record + 2, half);
        if flags & 0x20 != 0 && byte5 & 2 == 0 {
            m.sw(by_4 + u32::from(kind) * 4, record);
        } else {
            m.sw(by_16 + u32::from(kind) * 16, record);
        }
    }
    tail::adjust_entity_parameters(&mut m);
    assert!(m.take_fault().is_ok());
    let halves = |m: &mut Machine, i: u32| (m.lhu(entity(i) + 0x4a), m.lhu(entity(i) + 0x4c));
    assert_eq!(
        halves(&mut m, 2),
        (74, 0x0030),
        "adds (0x100 >> 2), wrapping"
    );
    assert_eq!(halves(&mut m, 3), (16, 17), "amount at least 15");
    assert_eq!(
        halves(&mut m, 4),
        (22, 22),
        "half +2 is signed: 0x8000 >> 2 < 15"
    );
    assert_eq!(
        halves(&mut m, 5),
        (100, 5),
        "0x20 path: non-positive halves negated"
    );
    assert_eq!(halves(&mut m, 6), (0xfffb, 0), "kind beyond context +0xF88");
    assert_eq!(
        halves(&mut m, 7),
        (130, 128),
        "byte5 bit 1: 0x10 table, 0x20 negation"
    );
    assert_eq!(halves(&mut m, 8), (3, 3), "kind*4 beyond context +0xF80");
    assert_eq!(halves(&mut m, 9), (3, 3), "inactive");
    assert_eq!(halves(&mut m, 0), (1, 1));
    assert_eq!(halves(&mut m, 1), (1, 1));
}

// ---- Direct callees of 80032510 (init-callees-001) ----

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/init-callees-001"]
fn gameplay_bit0_80035248() {
    let changed = snapshot::check_call(&callees(), 0x8003_5248, 0, |m, entry| {
        // The original leaves bit 0 as it was; flip it so the native
        // translation must reproduce the decision.
        let gameplay = m.lw(GAMEPLAY);
        let flags = m.lw(gameplay);
        m.sw(gameplay, flags ^ 1);
        tail::update_gameplay_bit0(m, entry.gpr(4));
        None
    })
    .unwrap();
    assert_eq!(changed, 0);
}

#[test]
fn gameplay_bit0_paths() {
    let mut m = Machine::default();
    let (context, gameplay, record) = (0x8010_0000, 0x8010_1000, 0x8010_2000);
    m.sw(CONTEXT, context);
    m.sw(GAMEPLAY, gameplay);
    m.sw(gameplay + 0x28, record);
    let mut run = |request: u32, byte_3a2: u8, word_50: u32, flags: u32| {
        m.sb(context + 0x3a2, byte_3a2);
        m.sw(record + 0x50, word_50);
        m.sw(gameplay, flags);
        tail::update_gameplay_bit0(&mut m, request);
        m.lw(gameplay)
    };
    assert_eq!(run(1, 0, 0, 0xffff_ffff), 0xffff_fffe);
    assert_eq!(run(0, 0, 0, 0x10), 0x11);
    assert_eq!(run(1, 1, 0, 0x10), 0x11);
    assert_eq!(run(1, 0, 0x800, 0x10), 0x11);
    assert_eq!(run(1, 0, 0xffff_f7ff, 0x11), 0x10);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/init-callees-001"]
fn flight_bounds_800322d4() {
    let changed = snapshot::check_call(&callees(), 0x8003_22d4, 0, |m, entry| {
        tail::set_flight_bounds(m, entry.gpr(4) as i32);
        None
    })
    .unwrap();
    assert!(changed > 0);
}

/// Every case of 800322D4 through the original jump table (resident RAM).
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/init-callees-001"]
fn flight_bounds_cases() {
    let base = snap(&callees(), 0x8003_22d4, "entry", 0).machine();
    let default = (
        0,
        0,
        [LOW, HIGH, LOW, HIGH, LOW, HIGH],
        [LOW, LOW, LOW, HIGH, 0, HIGH],
    );
    let wide = [LOW, HIGH, 0xff48_a000, 0x0177_0000, LOW, HIGH];
    let expected = |mission: i32| match mission {
        10 => (
            0x18,
            0,
            [LOW, HIGH, LOW, 0, LOW, HIGH],
            [LOW, 0xff6e_0000, LOW, HIGH, 0xffef_8000, HIGH],
        ),
        17 | 37 => (
            0x300,
            0x1000,
            [LOW, HIGH, LOW, HIGH, LOW, HIGH],
            [LOW, LOW, LOW, HIGH, HIGH, HIGH],
        ),
        32 => (
            0x10,
            0,
            wide,
            [LOW, 0xff5d_4000, LOW, HIGH, 0xffef_8000, HIGH],
        ),
        33 | 34 => (
            0x10,
            0,
            wide,
            [
                0xde96_e000,
                0xff5d_4000,
                0xea90_6000,
                0x056f_0000,
                0xffef_8000,
                0x1176_4000,
            ],
        ),
        _ => default,
    };
    for mission in [
        -32768, -1, 0, 1, 9, 10, 11, 16, 17, 18, 31, 32, 33, 34, 35, 36, 37, 38, 99,
    ] {
        let mut m = base.clone();
        let gameplay = m.lw(GAMEPLAY);
        let bounds = m.lw(gameplay + 0x28);
        let (flags, bound_flags) = (m.lw(gameplay), m.lw(bounds));
        tail::set_flight_bounds(&mut m, mission);
        assert!(m.take_fault().is_ok(), "{mission}");
        let (set, bound_set, pairs, limits) = expected(mission);
        assert_eq!(m.lw(gameplay), flags | set, "{mission}");
        assert_eq!(m.lw(bounds), bound_flags | bound_set, "{mission}");
        for (i, value) in pairs.into_iter().enumerate() {
            assert_eq!(
                m.lw(gameplay + 0x2a0 + 4 * i as u32),
                value,
                "{mission} pair {i}"
            );
        }
        for (offset, value) in [0x7c, 0x80, 0x84, 0x8c, 0x90, 0x94].into_iter().zip(limits) {
            assert_eq!(m.lw(bounds + offset), value, "{mission} +{offset:x}");
        }
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/init-callees-001"]
fn arena_peek_80026fd8() {
    snapshot::check_call(&callees(), 0x8002_6fd8, 0, |m, entry| {
        Some(ac3_flight::arena::peek(m, entry.gpr(4)))
    })
    .unwrap();
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/init-callees-001"]
fn frame_buffers_800321b4() {
    let changed = snapshot::check_call(&callees(), 0x8003_21b4, 0, |m, entry| {
        tail::prepare_frame_buffers(m, entry.gpr(4));
        None
    })
    .unwrap();
    assert!(changed > 300, "{changed}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/init-callees-001; blocked on 80053704"]
fn mission_phase_80031e3c_needs_script() {
    let changed = snapshot::check_call(&callees(), 0x8003_1e3c, 0, |m, entry| {
        Some(tail::enter_mission_phase(m, entry.gpr(4), entry.gpr(5)))
    })
    .unwrap();
    assert!(changed > 0);
}

/// Everything in the tail before 80031E3C, composed: from 80032A6C to the
/// 80031E3C entry. With `phase_entities_80038930` this leaves only the
/// script interpreter unchecked in the traced tail (the 80031E3C entry and
/// return states equal tail-001's 80031C20 entry and return outside the
/// stack, so the two captures are consistent).
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/init-callees-001"]
fn tail_before_phase_entry_80032a6c() {
    let entry = snap(&callees(), 0x8004_c1a0, "return", 0);
    let exit = snap(&callees(), 0x8003_1e3c, "entry", 0);
    let changed = check_span(&entry, &exit, |m| {
        tail::prepare_phase_entry(m);
        None
    });
    assert!(changed > 300, "{changed}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/init-callees-001; blocked on 80053704"]
fn whole_tail_needs_script() {
    // 80032A6C (8004C1A0 return) to 80032C60 (80031E3C return).
    let entry = snap(&callees(), 0x8004_c1a0, "return", 0);
    let exit = snap(&callees(), 0x8003_1e3c, "return", 0);
    let changed = check_span(&entry, &exit, |m| {
        Some(tail::finish_gameplay_initialization(m))
    });
    assert!(changed > 400, "{changed}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/menu-runtime/initialization-002; blocked on 80053704"]
fn whole_tail_stages_07_09_needs_script() {
    let changed = check_stages("07-record_adjustment", "09-return", |m| {
        Some(tail::finish_gameplay_initialization(m))
    });
    assert!(changed > 400, "{changed}");
}

/// The untraced 80032268 branch (context byte +0x3A9 set): phase 0x4000000
/// needs no script, so the whole tail runs.
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/init-callees-001"]
fn alternate_buffers_80032268_tail() {
    let entry = snap(&callees(), 0x8004_c1a0, "return", 0);
    let mut m = entry.machine();
    let context = m.lw(CONTEXT);
    m.sb(context + 0x3a9, 1);
    let base = m.lw(context + 0x1c);
    let heap = m.lw(GP + 0x54c);
    let entities = m.lw(ENTITIES);
    let marks: Vec<u32> = (0..64)
        .map(|i| {
            let entity = m.lw(entities + i * 4);
            m.lw(entity) & 0x2000
        })
        .collect();
    let v0 = tail::finish_gameplay_initialization(&mut m);
    assert!(m.take_fault().is_ok());
    assert_eq!(v0, m.lw(0x800a_ee3c));
    assert_eq!(v0, 0x1000_000f);
    let (gameplay, resources) = (m.lw(GAMEPLAY), m.lw(RESOURCES));
    assert_eq!(m.lw(resources + 0x70), 0x0400_0000);
    assert_eq!(m.lw(resources + 0x74), 0);
    assert_eq!(m.lw(gameplay + 4), 0x0400_0000);
    assert_eq!(m.lw(gameplay + 8), 0);
    assert_eq!(m.lw(gameplay + 0x2b8), base);
    let pair = 0x4000 + 0x1_b800;
    assert_eq!(m.lw(gameplay + 0x18), base);
    assert_eq!(m.lw(gameplay + 0x20), base + 0x4000);
    assert_eq!(m.lw(gameplay + 0x1c), base + pair);
    assert_eq!(m.lw(gameplay + 0x24), base + pair + 0x4000);
    assert_eq!(m.lw(context + 0x1c), base + 2 * pair);
    assert_eq!(m.lw(GP + 0x54c), heap, "no work heap");
    for (i, mark) in marks.into_iter().enumerate() {
        let entity = m.lw(entities + 4 * i as u32);
        assert_eq!(m.lw(entity) & 0x2000, mark, "entity {i} unmarked");
    }
    // The shared part matches the original traced tail.
    let exit = snap(&callees(), 0x8003_1e3c, "return", 0);
    for offset in (0x2a0..0x2b8).step_by(4) {
        assert_eq!(
            m.lw(gameplay + offset).to_le_bytes(),
            exit.ram[(gameplay + offset - 0x8000_0000) as usize..][..4]
        );
    }
}

/// 80031E3C phases that need no script, on the original entry state. The
/// stopping phases key every voice off (SpuSetKey, a host call) and clear
/// the DCT-out DMA callback.
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/init-callees-001"]
fn mission_phase_paths() {
    let base = snap(&callees(), 0x8003_1e3c, "entry", 0).machine();
    let table = |m: &mut Machine, slot: u32| m.lw(0x800a_ee04 + slot * 4);
    for (phase, slot) in [
        (1, Some(0)),
        (0x80, Some(3)),
        (0x200, None),
        (0x2_0000, Some(6)),
        (0x10_0000, Some(7)),
        (0x20_0000, Some(8)),
        (0x40_0000, Some(9)),
        (0x80_0000, Some(10)),
        (0x1000_0000, Some(11)),
        (0x100_0000, Some(12)),
        (0x200_0000, Some(13)),
        (0x400_0000, Some(14)),
        (0, None),
        (3, None),
    ] {
        let mut m = base.clone();
        let gameplay = m.lw(GAMEPLAY);
        let context = m.lw(CONTEXT);
        let queue = m.lw(GP + 0x48);
        let count = m.lhu(context + 0x3b0);
        m.sw(gameplay + 4, phase);
        let v0 = tail::enter_mission_phase(&mut m, 0, 0);
        let stops = matches!(
            phase,
            0x40_0000 | 0x80_0000 | 0x100_0000 | 0x200_0000 | 0x1000_0000
        );
        assert!(m.take_fault().is_ok(), "{phase:#x}");
        let expected = slot.map_or(0, |slot| table(&mut m, slot));
        assert_eq!(v0, expected, "{phase:#x}");
        let keyed_off = m
            .host_calls
            .iter()
            .any(|call| call.function == 0x8009_f3c8 && call.args[..2] == [0, 0xff_ffff]);
        assert_eq!(keyed_off, stops, "{phase:#x}: SpuSetKey(0, all)");
        if !stops {
            assert!(m.host_calls.is_empty(), "{phase:#x}");
        }
        let bump = u16::from(phase == 0x1000_0000);
        assert_eq!(
            m.lhu(context + 0x3b0),
            count.wrapping_add(bump),
            "{phase:#x}"
        );
        if stops || phase == 1 {
            assert_eq!(m.lw(queue + 0x148), 1);
            assert_eq!(m.lbu(queue + 0x14c), 1);
            assert_eq!((m.lw(queue + 0x150), m.lw(queue + 0x15c)), (0, 0));
        }
        if phase == 0x80 {
            assert_eq!(m.lbu(queue + 0x14c), 5);
        }
        if stops {
            assert_eq!(m.lw(0x800c_43d8), 0, "channels released");
            assert_eq!(m.lbu(0x800c_35f8), 0xff, "voices reset");
        }
    }
    // Exit hook of phase 0x40 (80031C98 -> 80078238 -> 80076210), with no
    // entry hook: the camera record is reinitialized.
    let mut m = base.clone();
    let gameplay = m.lw(GAMEPLAY);
    m.sw(gameplay + 4, 0);
    let camera = m.lw(gameplay + 0x28);
    m.sh(camera + 0x24, 0);
    m.sw(camera + 0xbc, 0);
    assert_eq!(tail::enter_mission_phase(&mut m, 0x40, 0), 0);
    assert!(m.take_fault().is_ok());
    assert_eq!(m.lhu(camera + 0x24), 0x128);
    assert_eq!(m.lw(camera + 0xbc), 0xffff_ffff);
}

// ---- Subtree of 800321B4 and 80031E3C (tail-001) ----

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/tail-001"]
fn work_heap_8007c4b4() {
    let changed = snapshot::check_call(&subtree(), 0x8007_c4b4, 0, |m, _| {
        tail::create_work_heap(m);
        None
    })
    .unwrap();
    assert!(changed > 50, "{changed}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/tail-001; blocked on 80053704"]
fn phase_2_80031c20_needs_script() {
    let changed = snapshot::check_call(&subtree(), 0x8003_1c20, 0, |m, entry| {
        tail::enter_phase_2(m, entry.gpr(4));
        None
    })
    .unwrap();
    assert!(changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/tail-001"]
fn phase_entities_80038930() {
    let changed = snapshot::check_call(&subtree(), 0x8003_8930, 0, |m, _| {
        tail::prepare_phase_entities(m);
        None
    })
    .unwrap();
    assert!(changed > 50, "{changed}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/tail-001"]
fn phase_record_80052760() {
    let changed = snapshot::check_call(&subtree(), 0x8005_2760, 0, |m, _| {
        // The original rewrites values already present; clobber them so the
        // translation must reproduce them.
        let state = m.lw(GP + 0x330);
        let gameplay = m.lw(GAMEPLAY);
        let phase = m.lw(gameplay + 8);
        let table = m.lw(GP + 0x32c);
        let kind = m.lbu(state + 0x1c);
        m.sw(state + 0x10, 0xdead_beef);
        m.sb(state + 0x1c, !kind);
        if m.lbu(table + phase + 0x34) == 1 {
            let value = m.lw(state + 0x14);
            m.sw(state + 0x14, !value);
        }
        tail::select_phase_record(m);
        None
    })
    .unwrap();
    assert_eq!(changed, 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/tail-001"]
fn group_entities_8004c54c() {
    let changed = snapshot::check_call(&subtree(), 0x8004_c54c, 0, |m, _| {
        tail::activate_group_entities(m);
        None
    })
    .unwrap();
    assert!(changed > 50, "{changed}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/tail-001"]
fn entity_bit16_8004cb04() {
    for occurrence in 0..2 {
        let changed = snapshot::check_call(&subtree(), 0x8004_cb04, occurrence, |m, entry| {
            shared::mark_unit(m, entry.gpr(4));
            None
        })
        .unwrap();
        assert!(changed > 0);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/tail-001"]
fn configure_entity_8004cb2c() {
    for occurrence in 0..2 {
        let changed = snapshot::check_call(&subtree(), 0x8004_cb2c, occurrence, |m, entry| {
            shared::activate_unit(m, entry.gpr(4));
            None
        })
        .unwrap();
        assert!(changed > 0);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/tail-001"]
fn entity_marker_8004d59c() {
    for occurrence in 0..2 {
        let changed = snapshot::check_call(&subtree(), 0x8004_d59c, occurrence, |m, entry| {
            shared::update_marker(m, entry.gpr(4), entry.gpr(5));
            None
        })
        .unwrap();
        assert!(changed > 0);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/tail-001"]
fn entity_status_8004d6bc() {
    for occurrence in 0..2 {
        let changed = snapshot::check_call(&subtree(), 0x8004_d6bc, occurrence, |m, entry| {
            shared::update_status(m, entry.gpr(4), entry.gpr(5));
            None
        })
        .unwrap();
        assert!(changed > 0);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/tail-001"]
fn group_entities_high_8004d028() {
    let changed = snapshot::check_call(&subtree(), 0x8004_d028, 0, |m, _| {
        // No high entity matches on this mission and 800501F8 rewrites
        // values already present; clobber them so the translation must
        // reproduce them.
        m.sb(GP + 0x474, 0);
        m.sb(GP + 0x475, 0);
        m.sw(GP + 0x478, 0);
        tail::activate_group_entities_high(m);
        None
    })
    .unwrap();
    assert_eq!(changed, 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/tail-001"]
fn sound_channels_80030220() {
    let changed = snapshot::check_call(&subtree(), 0x8003_0220, 0, |m, _| {
        Some(tail::reset_sound_channels(m))
    })
    .unwrap();
    assert!(changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/tail-001"]
fn init_sound_channels_8002f0e4() {
    let changed = snapshot::check_call(&subtree(), 0x8002_f0e4, 0, |m, entry| {
        tail::init_sound_channels(m, entry.gpr(4));
        None
    })
    .unwrap();
    assert!(changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/tail-001"]
fn channel_state_8002e418() {
    for occurrence in 0..2 {
        let changed = snapshot::check_call(&subtree(), 0x8002_e418, occurrence, |m, entry| {
            shared::reset_envelope(m, entry.gpr(4));
            None
        })
        .unwrap();
        assert!(changed > 0);
    }
}

// ---- Untraced paths ----

/// 8004D028's entities 32..63 (8004D2F8, 8004D344) on synthetic records.
#[test]
fn high_entity_setup_8004d344() {
    let mut m = Machine::default();
    let (gameplay, table, entity, setup) = (0x8010_0000, 0x8010_1000, 0x8010_2000, 0x8010_3000);
    let (kind, parameters, marker) = (0x8010_4000, 0x8010_5000, 0x8010_6000);
    m.sw(GAMEPLAY, gameplay);
    m.sw(ENTITIES, table);
    m.sw(GP + 0x378, setup);
    m.sb(0x800b_0160, 0x23);
    m.sw(table + 0x80, entity);
    m.sw(entity, 0x4040);
    m.sh(entity + 0x4e, 5);
    m.sb(entity + 0x48, 7);
    m.sw(setup + 8, kind);
    m.sw(setup + 0xc, parameters);
    m.sb(kind + 2, 0);
    m.sb(kind + 0x1c, 3);
    m.sb(kind + 0x1d, 0x20);
    m.sb(kind + 0x1e, 0x23);
    for (offset, value) in [
        (3, 9),
        (4, 1),
        (5, 1),
        (6, 3),
        (7, 1),
        (8, 0),
        (9, 2),
        (0xa, 0),
    ] {
        m.sb(parameters + offset, value);
    }
    m.sh(parameters + 0xc, 0x1234);
    m.sh(parameters + 0xe, 0xffff);
    m.sw(0x800c_4568 + 32 * 4, marker);
    tail::activate_group_entities_high(&mut m);
    assert!(m.take_fault().is_ok());
    assert_eq!(m.lw(entity), 0x4040 | 0x1_0000 | 0x1000 | 0x200);
    assert_eq!((m.lhu(entity + 0x4a), m.lhu(entity + 0x4c)), (0x1234, 0));
    assert_eq!(m.lw(0x800c_4e18), 0x0080_db11);
    assert_eq!(m.read_bytes(marker + 2, 4), [0x82, 7, 9, 1]);
    assert_eq!(m.read_bytes(0x800c_4828 + 32 * 12, 3), [0x10, 1, 1]);
    assert_eq!(m.lw(GP + 0x478), 0x10_0000, "800501F8");
}

/// Resident machine with synthetic gameplay/entity/high setup pointers for
/// 8004D028's mission-code specials; high setup target words are 0x100+i.
fn specials_machine() -> Machine {
    let mut m = resident();
    m.sw(GAMEPLAY, 0x8010_0000);
    m.sw(ENTITIES, 0x8010_1000);
    m.sw(GP + 0x378, 0x8010_3000);
    for i in 0..64 {
        m.sw(0x8010_3000 + i * 0x28 + 0x18, 0x100 + i);
    }
    m
}

/// 8004D028's mission-code specials 80052524 (code 0x0D) and 80052568
/// (0x2B). 80052540 (0x15, phase 1) only runs script `[[gp+0x328]+0x148]`.
#[test]
#[ignore = "requires AC3_DATA_DIR (resident executable)"]
fn mission_specials_8004d028() {
    let mut m = specials_machine();
    m.sb(0x800b_aeb4, 0x0d);
    m.sh(GP + 0x48c, 7);
    tail::activate_group_entities_high(&mut m);
    assert!(m.take_fault().is_ok());
    let halves: Vec<u16> = [0x480, 0x482, 0x484, 0x48c]
        .into_iter()
        .map(|o| m.lhu(GP + o))
        .collect();
    assert_eq!(halves, [0x4000, 0x4000, 0x4000, 0]);

    let mut m = specials_machine();
    m.sb(0x800b_aeb4, 0x2b);
    m.sw(GP + 0x488, 9);
    tail::activate_group_entities_high(&mut m);
    assert!(m.take_fault().is_ok());
    assert_eq!(m.lw(GP + 0x488), 0);
    assert_eq!(m.lw(GP + 0x478), 0x10_0000, "800501F8");
}

/// 80052398 (mission code 0x37): ten SDK `rand` calls permute the high
/// setup target words through the original jump table at 80023C98.
#[test]
#[ignore = "requires AC3_DATA_DIR (resident executable); blocked on sdk::rand (800AC178)"]
fn shuffle_80052398_needs_sdk_rand() {
    let mut m = specials_machine();
    m.sb(0x800b_aeb4, 0x37);
    m.sw(0x800c_9d30, 12345);
    tail::activate_group_entities_high(&mut m);
    assert!(m.take_fault().is_ok());
    let mut state = 12345u32;
    for _ in 0..10 {
        state = state.wrapping_mul(0x41c6_4e6d).wrapping_add(0x3039);
    }
    assert_eq!(m.lw(0x800c_9d30), state, "ten rand calls");
    let mut targets: Vec<u32> = (0..64)
        .map(|i| m.lw(0x8010_3000 + i * 0x28 + 0x18))
        .collect();
    assert!(
        (0..64).any(|i| targets[i as usize] != 0x100 + i),
        "shuffled"
    );
    targets.sort_unstable();
    assert_eq!(targets, (0x100..0x140).collect::<Vec<_>>(), "a permutation");
}

#[test]
fn event_queue_8002c280_8002c2f4() {
    let mut m = Machine::default();
    let queue = 0x8010_0000;
    m.sw(GP + 0x48, queue);
    for i in 0..16 {
        m.sb(queue + i * 0x14 + 8, 0x55);
    }
    m.sb(queue + 0x14d, 9);
    shared::reset_cd_queue(&mut m, 0x11, 0x22);
    assert!((0..16).all(|i| m.lbu(queue + i * 0x14 + 8) == 0));
    assert_eq!((m.lw(queue + 0x148), m.lbu(queue + 0x14c)), (1, 1));
    assert_eq!((m.lw(queue + 0x150), m.lw(queue + 0x15c)), (0x11, 0x22));
    assert_eq!(m.lbu(queue + 0x14d), 0);

    m.sw(queue, 14);
    m.sw(queue + 4, 17);
    for (entry, state) in [(14, 3), (15, 4), (0, 2), (1, 3), (2, 3), (13, 4)] {
        m.sb(queue + entry * 0x14 + 8, state);
    }
    shared::cancel_stream_requests(&mut m);
    let states: Vec<u8> = [14, 15, 0, 1, 2, 13]
        .into_iter()
        .map(|e| m.lbu(queue + e * 0x14 + 8))
        .collect();
    assert_eq!(states, [0, 0, 2, 0, 3, 4], "head..=tail, states 3/4 only");
    assert_eq!((m.lbu(queue + 0x14c), m.lw(queue + 0x15c)), (5, 0));
    assert!(m.take_fault().is_ok());
}

#[test]
fn stop_all_sound_80030248() {
    let mut m = Machine::default();
    for i in 0..24 * 20 {
        m.sb(0x800c_35f8 + i, 0x55);
    }
    m.sw(0x800c_43d8, 2);
    for i in 0..4 {
        for (j, offset) in [4, 8, 0xc].into_iter().enumerate() {
            m.sb(0x800c_43d8 + i + offset, (i * 3 + j as u32) as u8);
        }
    }
    for (offset, voice) in [(0x31d, 4), (0x31e, 30), (0x31f, 5)] {
        m.sb(GP + offset, voice);
    }
    assert_eq!(shared::stop_all_sound(&mut m), 0);
    assert!(m.take_fault().is_ok());
    assert_eq!(m.host_calls.len(), 1, "SpuSetKey(0, 0xFFFFFF)");
    assert_eq!(
        (m.host_calls[0].function, m.host_calls[0].args[..2].to_vec()),
        (0x8009_f3c8, vec![0, 0xff_ffff])
    );
    for voice in 0..24 {
        let at = 0x800c_35f8 + voice * 20;
        assert_eq!(m.read_bytes(at, 4), [0xff, 0, 0, 0], "voice {voice}");
        assert_eq!(m.read_bytes(at + 4, 12), [0x55; 12]);
        assert_eq!(m.read_bytes(at + 0x10, 4), [0x80, 0x80, 0, 0]);
    }
    assert_eq!(m.lw(0x800c_43d8), 0);
    assert!((0..12).all(|i| m.lbu(0x800c_43dc + i) == 0xff));
    assert_eq!(m.read_bytes(GP + 0x31d, 3), [0xff; 3]);
    assert!((0..8).all(|i| m.lw(0x800c_4318 + i * 4) == 0xffff_00ff));
    assert!((0..32).all(|i| m.lw(0x800c_4338 + i * 4) == 0xffff_ffff));
}

#[test]
fn copy_words_800270fc() {
    let mut m = Machine::default();
    for i in 0..4 {
        m.sw(0x8010_0000 + i * 4, 0x1111_0000 + i);
    }
    assert_eq!(
        shared::copy_words(&mut m, 0x8010_1000, 0x8010_0000, 0),
        0x8010_1000
    );
    assert_eq!(
        shared::copy_words(&mut m, 0x8010_1000, 0x8010_0000, 0xffff_ffff),
        0x8010_1000
    );
    assert_eq!(m.lw(0x8010_1000), 0);
    assert_eq!(
        shared::copy_words(&mut m, 0x8010_1000, 0x8010_0000, 3),
        0x8010_100c
    );
    assert_eq!(m.read_bytes(0x8010_1000, 12), m.read_bytes(0x8010_0000, 12));
    assert_eq!(m.lw(0x8010_100c), 0);
}

/// Phase 0x10 state copy (80038980, 8006AD60) on original mid-init state.
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/tail-001"]
fn phase_state_80038980() {
    let mut m = snap(&subtree(), 0x8003_8930, "entry", 0).machine();
    let context = m.lw(CONTEXT);
    let mark = m.lw(context + 0x1c);
    tail::save_phase_state(&mut m);
    assert!(m.take_fault().is_ok());
    assert_ne!(m.lw(context) & 0x1000_0000, 0);
    assert_eq!((m.lw(GP + 0x3d8), m.lw(GP + 0x3d4)), (mark, mark));
    assert_eq!(m.lw(context + 0x1c), mark + 0x98c);
    let gameplay = m.lw(GAMEPLAY);
    let record = m.lw(gameplay + 0x28);
    assert_eq!(m.read_bytes(mark, 0x18c), m.read_bytes(record, 0x18c));
    let entities = m.lw(ENTITIES);
    for i in 0..64 {
        let entity = m.lw(entities + i * 4);
        let copy = mark + 0x18c + i * 0x20;
        assert_eq!(m.read_bytes(copy, 0x20), m.read_bytes(entity, 0x20), "{i}");
    }
    let slots = m.lw(GP + 0x514);
    assert!((0..64).all(|i| m.lbu(slots + 8 + i) == 0xff));
    assert!((0..64).all(|i| m.lbu(slots + 0x48 + i) == 0 && m.lbu(slots + 0x88 + i) == 0));
    for offset in [0, 4, 0x1548, 0x154c, 0x33d0, 0x33d4, 0x33d8, 0x33dc, 0x33e0] {
        assert_eq!(m.lw(slots + offset), 0, "+{offset:x}");
    }
}

#[test]
fn dispatch_claims_translations() {
    for &address in &tail::TRANSLATED {
        let mut m = Machine::default();
        assert!(
            tail::dispatch(&mut m, address, [0; 4]).is_some(),
            "{address:08x}"
        );
    }
    let mut m = Machine::default();
    assert_eq!(shared::dispatch(&mut m, 0x8002_d980, [5, 0, 0, 0]), Some(5));
    assert_eq!(
        shared::dispatch(&mut m, 0x8002_d980, [0x18, 0, 0, 0]),
        Some(0xff)
    );
    assert_eq!(tail::dispatch(&mut m, 0x8002_d980, [5, 0, 0, 0]), None);
    assert_eq!(tail::dispatch(&mut m, 0x8005_3704, [0; 4]), None);
    assert_eq!(tail::dispatch(&mut m, 0x8003_2510, [0; 4]), None);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/tail-001"]
fn dispatch_routes_80038930() {
    let changed = snapshot::check_call(&subtree(), 0x8003_8930, 0, |m, _| {
        ac3_flight::dispatch::call(m, 0x8003_8930, [0; 4]);
        None
    })
    .unwrap();
    assert!(changed > 50, "{changed}");
}

/// 80059C4C (phase 0x400 setup) on synthetic state over the resident
/// executable: stage mode selection, phase record, context bit and sound
/// channels. Some callees have no owner yet, so the run faults on the first
/// of them that is still untranslated (80041DBC is now owned by init_39a68).
#[test]
#[ignore = "requires AC3_DATA_DIR (resident executable)"]
fn phase_400_setup_80059c4c() {
    let base = {
        let mut m = resident();
        m.sw(GAMEPLAY, 0x8010_0000);
        m.sw(ENTITIES, 0x8010_1000);
        m.sw(CONTEXT, 0x8010_2000);
        m.sw(GP + 0x330, 0x8010_3000);
        m.sb(0x8010_3025, 0x5a);
        m
    };
    // Mission code 0x22: flags 0x2003, mode 0x10, fixed camera, stage
    // record start (not passed on: flags 0x6000).
    let mut m = base.clone();
    m.sb(0x800b_aeb4, 0x22);
    m.sw(0x8010_0008, 7);
    let sp = m.sp;
    tail::setup_phase_400(&mut m);
    // 80059C4C callees without an owner when this test was written.
    const UNOWNED: [u32; 10] = [
        0x8004_b74c,
        0x8007_22a4,
        0x8007_7fac,
        0x8007_574c,
        0x8007_4c60,
        0x8007_4c40,
        0x8002_c81c,
        0x8004_c644,
        0x8004_d204,
        0x8003_9af8,
    ];
    match m.take_fault() {
        Ok(()) => {}
        Err(Fault::Untranslated { address }) if UNOWNED.contains(&address) => {}
        other => panic!("unexpected 80059C4C outcome: {other:?}"),
    }
    assert_eq!(m.sp, sp);
    assert_eq!((m.lw(GP + 0x324), m.lw(GP + 0x4ec)), (0x10, 0x2003));
    assert_eq!(m.lbu(0x8010_3028), 0x5a);
    assert_eq!((m.lbu(0x8010_301c), m.lw(0x8010_3014)), (0xff, 0));
    assert_ne!(m.lw(0x8010_2000) & 0x1000_0000, 0);
    assert_eq!(m.lw(0x800c_43d8), 2, "80030220");

    // Mission code 4 with phase index 3: no stage mode.
    let mut m = base.clone();
    m.sb(0x800b_aeb4, 0x04);
    m.sw(0x8010_0008, 3);
    m.sw(GP + 0x324, 0x77);
    tail::setup_phase_400(&mut m);
    assert_eq!((m.lw(GP + 0x324), m.lw(GP + 0x4ec)), (0x77, 0));
    assert_eq!((m.lbu(0x8010_301c), m.lw(0x8010_3014)), (1, 0x1518));
    assert_eq!(m.lbu(GP + 0x4f0), 1, "first pass of the saved start");
}

/// 80039AF8 `reset_lock` (no owner until `jp-sdk` found it through its JP
/// twin): the original instructions and the translation on the resident
/// image with random lock-block bytes; the block and `v0` must match.
#[test]
#[ignore = "requires AC3_DATA_DIR"]
fn reset_lock_80039af8() {
    const LOCK: u32 = 0x800c_4b28;
    let mut seed = 0x1234_5678u32;
    for _ in 0..16 {
        let mut original = resident();
        for offset in 0..0x18 {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            original.sb(LOCK + offset, (seed >> 16) as u8);
        }
        let mut native = original.clone();
        let v0 = oracle::run(&mut original, 0x8003_9af8, 0x8003_9b30, [0; 4]);
        assert_eq!(tail::reset_lock(&mut native), v0);
        native.take_fault().unwrap();
        for offset in 0..0x18 {
            assert_eq!(
                native.lbu(LOCK + offset),
                original.lbu(LOCK + offset),
                "+{offset:#x}"
            );
        }
    }
}
