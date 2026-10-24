//! 80038818 and its subtree on original entry snapshots must reproduce the
//! original return snapshots (`snapshot::check_call`).
//!
//! Captures: `init-callees-001` (80038818 itself) and `script-002`
//! (window 80032510: 8004BBB8, 800527C8, 80052574, 80054B54, 800477C8,
//! 8004C258, 8004CDB0, 80053704, 80037B24).
use ac3_flight::SmallData;
use ac3_flight::{Fault, Machine, init_38818, snapshot};
use std::path::{Path, PathBuf};

fn data(path: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR")).join(path)
}
fn callees() -> PathBuf {
    data("analysis/flight-runtime/init-callees-001/callees.jsonl")
}
fn subtree() -> PathBuf {
    data("analysis/flight-runtime/script-002/snapshots.jsonl")
}

/// Run `native` on an entry snapshot and compare with its return. While
/// the shared 800827B4 (distance) is still a stub, the only permitted
/// differences are the distance words it would have produced (motion
/// record +4 of the entities in `entities`) and the GTE registers it uses;
/// once it lands this is the strict comparison. Returns the bytes the
/// original changed.
fn check_with_distance_stub(
    index: &Path,
    callee: u32,
    entities: impl Fn(&mut Machine) -> Vec<u32>,
    native: impl FnOnce(&mut Machine),
) -> usize {
    let directory = index.parent().unwrap();
    let infos = snapshot::read_index(index).unwrap();
    let entry = infos
        .iter()
        .find(|s| s.callee == callee && s.stage == "entry")
        .expect("entry snapshot");
    let exit = infos
        .iter()
        .find(|s| {
            s.callee == callee
                && s.stage == "return"
                && s.sequence > entry.sequence
                && s.ra == entry.ra
                && s.registers.gpr[29] == entry.registers.gpr[29]
        })
        .expect("return snapshot");
    let entry = snapshot::Snapshot::load(directory, entry).unwrap();
    let exit = snapshot::Snapshot::load(directory, exit).unwrap();
    let mut m = entry.machine();
    native(&mut m);
    let stubbed = match m.take_fault() {
        Ok(()) => false,
        Err(Fault::Untranslated {
            address: 0x8008_27b4,
        }) => true,
        Err(fault) => panic!("{callee:08x}: {fault}"),
    };
    let differences = snapshot::compare(&m, &exit);
    if stubbed {
        let mut expected = exit.machine();
        let allowed: Vec<u32> = entities(&mut expected)
            .into_iter()
            .flat_map(|index| {
                let motion = expected.lw(0x800c_4c98 + index * 4);
                (4..8).map(move |i| (motion + i) | 0x8000_0000)
            })
            .collect();
        let unexpected: Vec<u32> = differences
            .ram
            .iter()
            .copied()
            .filter(|a| !allowed.contains(a))
            .collect();
        assert!(
            unexpected.is_empty() && differences.scratch.is_empty(),
            "{callee:08x}: {}",
            differences.summary()
        );
        eprintln!(
            "{callee:08x}: 800827B4 still a stub; {} distance bytes differ as expected",
            differences.ram.len()
        );
    } else {
        assert!(
            differences.is_empty(),
            "{callee:08x}: {}",
            differences.summary()
        );
    }
    snapshot::changed_bytes(&entry, &exit)
}

fn units(m: &mut Machine) -> Vec<u32> {
    let count = u32::from(m.lbu(ac3_flight::GP + 0x364));
    (2..count).collect()
}
fn objects(m: &mut Machine) -> Vec<u32> {
    let count = u32::from(m.lbu(ac3_flight::GP + 0x36c));
    (0x20..0x20 + count).collect()
}

#[test]
#[ignore = "requires AC3_DATA_DIR, init-callees-001 and the shared 800827B4"]
fn initialize_80038818_exact() {
    let changed = snapshot::check_call(&callees(), 0x8003_8818, 0, |m, _| {
        init_38818::initialize_38818(m);
        None
    })
    .unwrap();
    eprintln!("original changed {changed} bytes");
    assert!(changed > 3000, "{changed}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/init-callees-001"]
fn initialize_80038818_apart_from_distances() {
    let changed = check_with_distance_stub(
        &callees(),
        0x8003_8818,
        |m| {
            let mut all = units(m);
            all.extend(objects(m));
            all
        },
        init_38818::initialize_38818,
    );
    eprintln!("original changed {changed} bytes");
    assert!(changed > 3000, "{changed}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/script-002"]
fn mission_tables_8004bbb8() {
    let changed = snapshot::check_call(&subtree(), 0x8004_bbb8, 0, |m, _| {
        init_38818::load_mission_tables_8004bbb8(m);
        None
    })
    .unwrap();
    eprintln!("original changed {changed} bytes");
    assert!(changed > 500, "{changed}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/script-002"]
fn script_tables_800527c8() {
    let changed = snapshot::check_call(&subtree(), 0x8005_27c8, 0, |m, _| {
        init_38818::load_script_tables_800527c8(m);
        None
    })
    .unwrap();
    eprintln!("original changed {changed} bytes");
    assert!(changed > 300, "{changed}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/script-002"]
fn mission_state_80052574() {
    let changed = snapshot::check_call(&subtree(), 0x8005_2574, 0, |m, _| {
        init_38818::start_mission_state_80052574(m);
        None
    })
    .unwrap();
    eprintln!("original changed {changed} bytes");
    assert!(changed > 20, "{changed}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/script-002"]
fn message_queue_80054b54() {
    let changed = snapshot::check_call(&subtree(), 0x8005_4b54, 0, |m, _| {
        init_38818::reset_message_queue_80054b54(m);
        None
    })
    .unwrap();
    eprintln!("original changed {changed} bytes");
    assert!(changed > 10, "{changed}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/script-002"]
fn result_block_800477c8() {
    let changed = snapshot::check_call(&subtree(), 0x8004_77c8, 0, |m, _| {
        init_38818::reset_result_800477c8(m);
        None
    })
    .unwrap();
    eprintln!("original changed {changed} bytes");
    assert!(changed > 0, "{changed}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/script-002"]
fn objects_8004cdb0() {
    let changed = snapshot::check_call(&subtree(), 0x8004_cdb0, 0, |m, _| {
        init_38818::initialize_objects_8004cdb0(m);
        None
    })
    .unwrap();
    eprintln!("original changed {changed} bytes");
    assert!(changed > 0, "{changed}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR, script-002 and the shared 800827B4"]
fn units_8004c258_exact() {
    let changed = snapshot::check_call(&subtree(), 0x8004_c258, 0, |m, _| {
        init_38818::initialize_units_8004c258(m);
        None
    })
    .unwrap();
    eprintln!("original changed {changed} bytes");
    assert!(changed > 1000, "{changed}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/script-002"]
fn units_8004c258_apart_from_distances() {
    let changed = check_with_distance_stub(
        &subtree(),
        0x8004_c258,
        units,
        init_38818::initialize_units_8004c258,
    );
    eprintln!("original changed {changed} bytes");
    assert!(changed > 1000, "{changed}");
}

/// Effect sequences whose steps queue ring commands (8005CD38 with gameplay
/// flag 2 → 8007CEB4 → 8007C8AC), which the captured mission does not use.
#[test]
fn effect_steps_queue_ring_commands() {
    let mut m = Machine::default();
    let gp = ac3_flight::GP;
    let gameplay = 0x8010_0000;
    let cells = 0x8011_0000;
    let ring = 0x8012_0000;
    m.sw(gp + 0x2d0, gameplay);
    m.sw(gameplay, 2);
    m.sw(gameplay + 0x248, cells);
    m.sw(gp + 0x54c, ring);
    m.sw(ring + 4, 0x8012_1000);
    m.sw(ring + 8, 0x8012_2000);
    m.sw(ring + 0xc, 0x8012_1000);
    m.sw(ring + 0x10, 0x8012_1000);
    // Sequence list: entry 0 → steps at +0; entry 1..31 → empty step at +8*5.
    let list = 0x8013_0000;
    let steps = 0x8013_0100;
    m.set_gw(0x398, list);
    m.set_gw(0x35c, steps);
    for i in 1..0x20 {
        m.sb(list + i, 5);
    }
    m.sh(steps, 1);
    m.write_bytes(steps + 2, &[3, 2, 0x34, 0x01]);
    m.sh(steps + 8, 0);
    m.sh(cells + (2 << 9) + 3 * 2, 0xfe05);
    assert_eq!(init_38818::start_effects_80047e40(&mut m), 0);
    assert!(m.take_fault().is_ok());
    assert_eq!(m.lhu(cells + (2 << 9) + 6), 0xfe00 | 0x134);
    let command = 0x8012_1000;
    assert_eq!(m.lw(ring + 0xc), command + 8);
    assert_eq!(m.lbu(command + 4), 3);
    assert_eq!(m.lbu(command + 5), 2);
    assert_eq!(m.lw(command), 0x42u32 | 5 << 14 | 0x134 << 23);
}
