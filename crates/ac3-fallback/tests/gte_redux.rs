//! Validation (a), GTE: the PCSX-Redux GTE oracle (`analysis/gte-oracle/
//! gte-001`, docs/GTE.md) replayed as real guest instructions through the
//! fallback interpreter.
//!
//! Each case is a template of single instructions (COP2 commands, transfers,
//! CPU loads and stores of the input and output tables, NOPs) that Redux ran
//! on a raw initial register file. Here every template op becomes the MIPS
//! instruction it stands for (`lw`/`sw`/`lwc2`/`swc2` address the tables
//! through `$zero`), the interpreter runs it on the machine, and all 64 COP2
//! registers and every stored word must equal Redux's. That covers every
//! command and MVMVA encoding, every transfer, and the write-to-use, read
//! and busy-GTE hazards of the pipeline.
mod support;

use ac3_fallback::{Fallback, Version};
use ac3_flight::Machine;
use std::path::{Path, PathBuf};
use support::{Pal, data_dir};

const IN: u32 = 0x1000;
const OUT: u32 = 0x1400;
const CODE: u32 = 0x8010_0000;
const OUT_SENTINEL: u32 = 0xa5a5_0000;

struct Template {
    kind: u32,
    ops: Vec<u32>,
    inputs: usize,
    outputs: usize,
}

fn words(path: &Path) -> Vec<u32> {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn templates(w: &[u32]) -> Vec<Template> {
    assert_eq!(w[0], u32::from_le_bytes(*b"GTET"));
    assert_eq!(w[1], 1);
    let mut out = Vec::new();
    let mut at = 3;
    for id in 1..=w[2] {
        assert_eq!(w[at], id);
        let n = w[at + 2] as usize;
        out.push(Template {
            kind: w[at + 1],
            ops: w[at + 5..at + 5 + n].to_vec(),
            inputs: w[at + 3] as usize,
            outputs: w[at + 4] as usize,
        });
        at += 5 + n;
    }
    assert_eq!(at, w.len());
    out
}

/// The instruction a template op stands for.
fn instruction(op: u32) -> u32 {
    let g = op >> 16 & 31;
    let reg = op >> 8 & 31;
    let k = op & 0xff;
    let cop2 = |rs: u32| 0x12 << 26 | rs << 21 | g << 16 | reg << 11;
    match op >> 26 {
        1 => 0x12 << 26 | 1 << 25 | (op & 0x1ff_ffff),
        2 => 0x23 << 26 | g << 16 | (IN + 4 * k),
        3 => 0x2b << 26 | g << 16 | (OUT + 4 * k),
        4 => cop2(4),
        5 => cop2(6),
        6 => cop2(0),
        7 => cop2(2),
        8 => 0x32 << 26 | reg << 16 | (IN + 4 * k),
        9 => 0x3a << 26 | reg << 16 | (OUT + 4 * k),
        10 => 0,
        other => panic!("template op {other}"),
    }
}

fn capture() -> PathBuf {
    std::env::var_os("AC3_GTE_ORACLE")
        .map(PathBuf::from)
        .unwrap_or_else(|| data_dir().join("analysis/gte-oracle/gte-001"))
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the Redux GTE oracle capture"]
fn interpreted_cop2_matches_redux() {
    let dir = capture();
    let templates = templates(&words(&dir.join("gte-templates.bin")));
    let data = words(&dir.join("gte-cases.bin"));
    assert_eq!(data[0], u32::from_le_bytes(*b"GTEC"));
    let fallback = Fallback::new(Version::Pal);
    let mut m = Machine::default();
    let (mut cases, mut failures, mut by_kind) = (0usize, Vec::new(), [0usize; 8]);
    let started = std::time::Instant::now();
    let mut at = 2;
    while at < data.len() {
        let t = &templates[data[at] as usize - 1];
        let initial = &data[at + 1..at + 65];
        let inputs = &data[at + 65..at + 65 + t.inputs];
        let outputs = &data[at + 65 + t.inputs..at + 65 + t.inputs + t.outputs];
        let last = &data[at + 65 + t.inputs + t.outputs..at + 129 + t.inputs + t.outputs];
        at += 129 + t.inputs + t.outputs;

        for (i, &op) in t.ops.iter().enumerate() {
            m.sw(CODE + 4 * i as u32, instruction(op));
        }
        for (k, &word) in inputs.iter().enumerate() {
            m.sw(0x8000_0000 | (IN + 4 * k as u32), word);
        }
        for k in 0..t.outputs as u32 {
            m.sw(0x8000_0000 | (OUT + 4 * k), OUT_SENTINEL + k);
        }
        m.gte.data.copy_from_slice(&initial[..32]);
        m.gte.control.copy_from_slice(&initial[32..]);
        let end = CODE + 4 * t.ops.len() as u32;
        // The run ends at the template's end (a "call" there returns at once).
        fallback.run_range(
            &mut Pal(&mut m),
            (CODE, end),
            [0; 4],
            &mut |_: &mut Pal<'_>, _, _| 0,
        );
        assert!(m.take_fault().is_ok());
        let got: Vec<u32> = m.gte.data.iter().chain(&m.gte.control).copied().collect();
        let stored: Vec<u32> = (0..t.outputs as u32)
            .map(|k| m.lw(0x8000_0000 | (OUT + 4 * k)))
            .collect();
        cases += 1;
        by_kind[t.kind as usize] += 1;
        if got != last || stored != outputs {
            let registers: Vec<usize> = (0..64).filter(|&r| got[r] != last[r]).collect();
            failures.push(format!(
                "case {cases} kind {} ops {:08x?}: registers {registers:?} stored {:08x?} want {:08x?}",
                t.kind, t.ops, stored, outputs
            ));
        }
    }
    let seconds = started.elapsed().as_secs_f64();
    let report = fallback.report();
    eprintln!(
        "GTE Redux oracle through the interpreter: {cases} cases in {seconds:.2} s \
         ({} instructions, {:.1} M instructions/s), by kind {by_kind:?}, {} pipeline hazards met, {} failures",
        report["instructions"],
        report["instructions"].as_f64().unwrap_or(0.0) / seconds / 1e6,
        report["gte_hazards"],
        failures.len()
    );
    for line in failures.iter().take(12) {
        eprintln!("  {line}");
    }
    assert!(cases > 100_000, "{cases} cases");
    assert!(failures.is_empty());
    assert!(
        report["gte_hazards"].as_u64().unwrap() > 1_000,
        "hazard cases are exercised"
    );
}
