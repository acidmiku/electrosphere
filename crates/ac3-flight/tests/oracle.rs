//! The test-only differential oracle (`support::oracle`) checked against
//! the original: interpreting the original instructions from captured entry
//! snapshots must reproduce each captured return snapshot exactly.
mod support;

use ac3_flight::snapshot::{self, Snapshot};
use support::oracle;

/// Routines with captured calls: (capture, entry, end = entry + size).
const CASES: [(&str, u32, u32); 7] = [
    ("player-015", 0x8008_094c, 0x8008_0994),
    ("player-015", 0x8008_1124, 0x8008_1198),
    ("player-015", 0x8008_1564, 0x8008_15d4),
    ("player-004", 0x8007_02cc, 0x8007_037c),
    ("player-004", 0x8006_fe5c, 0x8006_fec0),
    ("player-004", 0x8006_7030, 0x8006_7218),
    ("player-001", 0x8007_1358, 0x8007_22a4),
];

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/player-{001,004,015}"]
fn oracle_reproduces_captured_calls() {
    let mut total = 0;
    for (name, callee, end) in CASES {
        let index_file = oracle::capture(name);
        let index = snapshot::read_index(&index_file).unwrap();
        for e in index
            .iter()
            .filter(|s| s.callee == callee && s.stage == "entry")
        {
            let exit = index
                .iter()
                .find(|s| {
                    s.callee == callee
                        && s.stage == "return"
                        && s.sequence > e.sequence
                        && s.ra == e.ra
                        && s.registers.gpr[29] == e.registers.gpr[29]
                })
                .unwrap();
            let entry = Snapshot::load(index_file.parent().unwrap(), e).unwrap();
            let exit = Snapshot::load(index_file.parent().unwrap(), exit).unwrap();
            let mut m = entry.machine();
            let args = [entry.gpr(4), entry.gpr(5), entry.gpr(6), entry.gpr(7)];
            oracle::run(&mut m, callee, end, args);
            let fault = m.take_fault();
            let d = snapshot::compare(&m, &exit);
            assert!(
                d.is_empty() && fault.is_ok(),
                "{name} {callee:08x} frame {}: {} {fault:?}",
                e.frame,
                d.summary()
            );
            total += 1;
        }
    }
    assert!(total >= 7, "{total}");
    println!("oracle: {total} captured calls reproduced");
}

/// The trap convention: `break 0x1C00` (divide by zero) raises
/// `Fault::Trap { code: 7 }` at its address and stops the run.
#[test]
fn oracle_break_is_a_trap() {
    let mut m = ac3_flight::Machine::default();
    let code = 0x8010_0000;
    m.sw(code, 0x0007_000d); // break 0x1c00
    m.sw(code + 4, 0);
    oracle::run(&mut m, code, code + 8, [0; 4]);
    assert_eq!(
        m.take_fault(),
        Err(ac3_flight::Fault::Trap {
            address: code,
            code: 7
        })
    );
}
