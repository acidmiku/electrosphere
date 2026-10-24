//! Native replay of the PCSX-Redux GTE oracle (`tools/capture-gte-oracle.py`,
//! `docs/GTE.md`).
//!
//! Each case ran a template of single guest instructions (COP2 commands,
//! transfers, CPU loads/stores of the input and output tables, NOPs) on a raw
//! initial register file. Redux recorded the stored words and the raw register
//! file afterwards; the replay must reproduce both bit for bit. It models
//! Redux's write-to-use timing: a COP2 write lands two instructions after it
//! issues, reads see landed writes only, and commands run through
//! [`Gte::command_after_writes`].
use ac3_gte::Gte;
use ac3_gte::pipeline::{PendingWrite, still_busy, write_need};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const DEFAULT_CAPTURE: &str = "analysis/gte-oracle/gte-001";
const OUT_SENTINEL: u32 = 0xa5a5_0000;

mod op {
    pub const CMD: u32 = 1;
    pub const LW: u32 = 2;
    pub const SW: u32 = 3;
    pub const MTC2: u32 = 4;
    pub const CTC2: u32 = 5;
    pub const MFC2: u32 = 6;
    pub const CFC2: u32 = 7;
    pub const LWC2: u32 = 8;
    pub const SWC2: u32 = 9;
    pub const NOP: u32 = 10;
}
mod kind {
    pub const COMMAND: u32 = 1;
    pub const PIPELINE: u32 = 2;
    pub const TRANSFER: u32 = 3;
    pub const HAZARD_WRITE: u32 = 4;
    pub const HAZARD_DOUBLE: u32 = 5;
    pub const HAZARD_READ: u32 = 6;
    pub const HAZARD_BUSY: u32 = 7;
    pub const NAMES: [&str; 8] = [
        "?",
        "command",
        "pipeline",
        "transfer",
        "hazard_write",
        "hazard_double",
        "hazard_read",
        "hazard_busy",
    ];
}

const FUNCTS: [(u32, &str); 22] = [
    (0x01, "RTPS"),
    (0x06, "NCLIP"),
    (0x0c, "OP"),
    (0x10, "DPCS"),
    (0x11, "INTPL"),
    (0x12, "MVMVA"),
    (0x13, "NCDS"),
    (0x14, "CDP"),
    (0x16, "NCDT"),
    (0x1b, "NCCS"),
    (0x1c, "CC"),
    (0x1e, "NCS"),
    (0x20, "NCT"),
    (0x28, "SQR"),
    (0x29, "DCPL"),
    (0x2a, "DPCT"),
    (0x2d, "AVSZ3"),
    (0x2e, "AVSZ4"),
    (0x30, "RTPT"),
    (0x3d, "GPF"),
    (0x3e, "GPL"),
    (0x3f, "NCCT"),
];
fn name(code: u32) -> &'static str {
    FUNCTS
        .iter()
        .find(|(f, _)| *f == code & 0x3f)
        .map_or("?", |(_, n)| n)
}
const REGISTERS: [&str; 64] = [
    "VXY0", "VZ0", "VXY1", "VZ1", "VXY2", "VZ2", "RGBC", "OTZ", "IR0", "IR1", "IR2", "IR3", "SXY0",
    "SXY1", "SXY2", "SXYP", "SZ0", "SZ1", "SZ2", "SZ3", "RGB0", "RGB1", "RGB2", "RES1", "MAC0",
    "MAC1", "MAC2", "MAC3", "IRGB", "ORGB", "LZCS", "LZCR", "R11R12", "R13R21", "R22R23", "R31R32",
    "R33", "TRX", "TRY", "TRZ", "L11L12", "L13L21", "L22L23", "L31L32", "L33", "RBK", "GBK", "BBK",
    "LR1LR2", "LR3LG1", "LG2LG3", "LB1LB2", "LB3", "RFC", "GFC", "BFC", "OFX", "OFY", "H", "DQA",
    "DQB", "ZSF3", "ZSF4", "FLAG",
];

struct Template {
    kind: u32,
    ops: Vec<u32>,
    inputs: usize,
    outputs: usize,
}
impl Template {
    fn commands(&self) -> impl Iterator<Item = u32> + '_ {
        self.ops
            .iter()
            .filter(|&&o| o >> 26 == op::CMD)
            .map(|&o| o & 0x1ff_ffff)
    }
}
struct Case<'a> {
    template: usize,
    initial: &'a [u32],
    inputs: &'a [u32],
    outputs: &'a [u32],
    last: &'a [u32],
}

fn capture_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("AC3_GTE_ORACLE") {
        return dir.into();
    }
    PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("set AC3_DATA_DIR")).join(DEFAULT_CAPTURE)
}
fn words(path: &Path) -> Vec<u32> {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert_eq!(bytes.len() % 4, 0, "{}", path.display());
    bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}
fn load_templates(w: &[u32]) -> Vec<Template> {
    assert_eq!(w[0], u32::from_le_bytes(*b"GTET"));
    assert_eq!(w[1], 1, "template file version");
    let mut templates = Vec::new();
    let mut at = 3;
    for id in 1..=w[2] {
        let (kind, n_ops) = (w[at + 1], w[at + 2] as usize);
        assert_eq!(w[at], id, "templates are numbered in order");
        templates.push(Template {
            kind,
            ops: w[at + 5..at + 5 + n_ops].to_vec(),
            inputs: w[at + 3] as usize,
            outputs: w[at + 4] as usize,
        });
        at += 5 + n_ops;
    }
    assert_eq!(at, w.len(), "trailing template data");
    templates
}
fn split_cases<'a>(w: &'a [u32], templates: &[Template]) -> Vec<Case<'a>> {
    assert_eq!(w[0], u32::from_le_bytes(*b"GTEC"));
    assert_eq!(w[1], 1, "case file version");
    let mut cases = Vec::new();
    let mut at = 2;
    while at < w.len() {
        let template = w[at] as usize - 1;
        let t = &templates[template];
        let initial = at + 1;
        let inputs = initial + 64;
        let outputs = inputs + t.inputs;
        let last = outputs + t.outputs;
        cases.push(Case {
            template,
            initial: &w[initial..inputs],
            inputs: &w[inputs..outputs],
            outputs: &w[outputs..last],
            last: &w[last..last + 64],
        });
        at = last + 64;
    }
    assert_eq!(at, w.len(), "truncated case file");
    cases
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Timing {
    /// Redux: writes land two instructions after they issue.
    Redux,
    /// Naive sequential semantics: every write lands at once.
    Immediate,
}

fn replay(t: &Template, initial: &[u32], inputs: &[u32], timing: Timing) -> (Gte, Vec<u32>) {
    replay_with(t, initial, inputs, timing, false)
}
/// [`replay`], optionally with the sub-pixel window enabled.
fn replay_with(
    t: &Template,
    initial: &[u32],
    inputs: &[u32],
    timing: Timing,
    subpixel: bool,
) -> (Gte, Vec<u32>) {
    let mut gte = Gte::default();
    if subpixel {
        gte.enable_subpixel(ac3_gte::subpixel::DEFAULT_CAPACITY);
    }
    gte.data.copy_from_slice(&initial[..32]);
    gte.control.copy_from_slice(&initial[32..]);
    let mut gpr = [0_u32; 32];
    let mut outputs: Vec<u32> = (0..t.outputs as u32).map(|i| OUT_SENTINEL + i).collect();
    let mut pending: Vec<(usize, PendingWrite)> = Vec::new();
    // Every case starts with an idle GTE (the dispatcher waits for it).
    let mut last_command: Option<(usize, u32)> = None;
    for (position, &word) in t.ops.iter().enumerate() {
        // A write has landed once two more instructions issued after it.
        while pending
            .first()
            .is_some_and(|&(issued, _)| position >= issued + 3)
        {
            gte.apply_write(&pending.remove(0).1);
        }
        // A command or read that finds the GTE busy stalls; writes land meanwhile.
        if matches!(word >> 26, op::CMD | op::MFC2 | op::CFC2 | op::SWC2)
            && last_command.is_some_and(|(at, code)| still_busy(code, (position - at) as u32))
        {
            for (_, w) in pending.drain(..) {
                gte.apply_write(&w);
            }
        }
        let g = (word >> 16 & 31) as usize;
        let reg = (word >> 8 & 31) as usize;
        let k = (word & 0xff) as usize;
        let write = |control, value| PendingWrite {
            control,
            index: reg,
            value,
            distance: 0,
        };
        match word >> 26 {
            op::CMD => {
                let writes: Vec<PendingWrite> = pending
                    .drain(..)
                    .map(|(issued, w)| PendingWrite {
                        distance: (position - issued - 1) as u32,
                        ..w
                    })
                    .collect();
                gte.command_after_writes(word & 0x1ff_ffff, &writes)
                    .expect("oracle commands are valid");
                last_command = Some((position, word & 0x1ff_ffff));
            }
            op::LW => gpr[g] = inputs[k],
            op::SW => outputs[k] = gpr[g],
            op::MTC2 => pending.push((position, write(false, gpr[g]))),
            op::CTC2 => pending.push((position, write(true, gpr[g]))),
            op::LWC2 => pending.push((position, write(false, inputs[k]))),
            op::MFC2 => gpr[g] = gte.mfc2(reg),
            op::CFC2 => gpr[g] = gte.cfc2(reg),
            op::SWC2 => outputs[k] = gte.mfc2(reg),
            op::NOP => {}
            _ => panic!("unknown template op {word:#010x}"),
        }
        if timing == Timing::Immediate {
            for (_, w) in pending.drain(..) {
                gte.apply_write(&w);
            }
        }
    }
    for (_, w) in pending {
        gte.apply_write(&w);
    }
    (gte, outputs)
}
fn registers(gte: &Gte) -> Vec<u32> {
    gte.data.iter().chain(&gte.control).copied().collect()
}

fn describe(index: usize, case: &Case, t: &Template, native: &[u32], outputs: &[u32]) -> String {
    let mut s = format!(
        "case {index} (template {}, {}): ops {:08x?}\n  inputs {:08x?}\n",
        case.template + 1,
        kind::NAMES[t.kind as usize],
        t.ops,
        case.inputs
    );
    for r in 0..64 {
        if native[r] != case.last[r] {
            s += &format!(
                "  {:>6}: initial {:08x} redux {:08x} native {:08x}\n",
                REGISTERS[r], case.initial[r], case.last[r], native[r]
            );
        }
    }
    if outputs != case.outputs {
        s += &format!(
            "  stored redux {:08x?}\n  stored native {:08x?}\n",
            case.outputs, outputs
        );
    }
    s
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the Redux GTE oracle capture"]
fn native_gte_matches_redux_oracle() {
    let dir = capture_dir();
    let templates = load_templates(&words(&dir.join("gte-templates.bin")));
    let data = words(&dir.join("gte-cases.bin"));
    let cases = split_cases(&data, &templates);

    let mut failures = (0, String::new());
    // Sub-pixel replay: (mismatches, report, vertices projected, positions).
    let mut subpixel = (0, String::new(), 0_u64, 0_u64);
    let mut by_kind = [0_usize; 8];
    let mut executions = BTreeMap::<&str, usize>::new();
    let mut raw_by_command = BTreeMap::<&str, usize>::new();
    let mut mvmva = BTreeSet::new();
    let mut flags = BTreeMap::<&str, [usize; 32]>::new();
    let mut transfers = BTreeSet::new();
    // Hazard evidence: cases a naive replay (every write lands at once) gets
    // wrong. Single writes: (command, code, register) -> (need, wrong at
    // distance 0 / 1); reads: distance -> (cases, wrong).
    let (mut hazard_cases, mut naive_wrong, mut unexpected) = (0, 0, 0);
    let mut entries = BTreeMap::<(&str, u32, usize), (u32, [bool; 2])>::new();
    let mut reads = BTreeMap::<usize, (usize, usize)>::new();
    // Busy GTE: first command -> (fewest instructions after it at which a
    // late write was visible, most at which `still_busy` predicts a stall).
    let mut busy = BTreeMap::<&str, (u32, u32)>::new();
    for (index, case) in cases.iter().enumerate() {
        let t = &templates[case.template];
        let (gte, outputs) = replay(t, case.initial, case.inputs, Timing::Redux);
        let native = registers(&gte);
        if native != case.last || outputs != case.outputs {
            failures.0 += 1;
            if failures.0 <= 12 {
                failures.1 += &describe(index, case, t, &native, &outputs);
            }
        }
        // The same case with the sub-pixel window on: bit-identical, and
        // every position it kept floors to its screen word.
        let (precise, precise_outputs) =
            replay_with(t, case.initial, case.inputs, Timing::Redux, true);
        if registers(&precise) != native || precise_outputs != outputs {
            subpixel.0 += 1;
            if subpixel.0 <= 4 {
                subpixel.1 += &describe(index, case, t, &registers(&precise), &precise_outputs);
            }
        }
        let window = precise.subpixel().expect("enabled");
        subpixel.2 += window.projected();
        for (word, position) in window.entries() {
            let Some([x, y, _]) = position else { continue };
            subpixel.3 += 1;
            let floors = x.floor() as i16 == word as u16 as i16
                && y.floor() as i16 == (word >> 16) as u16 as i16;
            subpixel.0 += usize::from(!floors);
        }
        by_kind[t.kind as usize] += 1;
        for code in t.commands() {
            *executions.entry(name(code)).or_default() += 1;
        }
        if t.kind == kind::COMMAND {
            let code = t.commands().next().expect("command template");
            *raw_by_command.entry(name(code)).or_default() += 1;
            if code & 0x3f == 0x12 {
                mvmva.insert(code & 0xfe400);
            }
            let bits = flags.entry(name(code)).or_insert([0; 32]);
            for (bit, count) in bits.iter_mut().enumerate() {
                *count += (case.last[63] >> bit & 1) as usize;
            }
        }
        for &o in &t.ops {
            let r = (o >> 8 & 31) as usize;
            match o >> 26 {
                op::MTC2 | op::LWC2 => transfers.insert(("write", r)),
                op::CTC2 => transfers.insert(("write", 32 + r)),
                op::MFC2 | op::SWC2 => transfers.insert(("read", r)),
                op::CFC2 => transfers.insert(("read", 32 + r)),
                _ => false,
            };
        }
        if t.kind >= kind::HAZARD_WRITE {
            hazard_cases += 1;
            let (naive, naive_outputs) = replay(t, case.initial, case.inputs, Timing::Immediate);
            let wrong = registers(&naive) != case.last || naive_outputs != case.outputs;
            naive_wrong += usize::from(wrong);
            // The write ([lw, nop,] mtc2/ctc2/lwc2), NOPs, then a command or read.
            let at = t
                .ops
                .iter()
                .position(|&o| matches!(o >> 26, 4 | 5 | 8))
                .unwrap();
            let next = at
                + 1
                + t.ops[at + 1..]
                    .iter()
                    .take_while(|&&o| o >> 26 == op::NOP)
                    .count();
            let distance = next - at - 1;
            if t.kind == kind::HAZARD_WRITE {
                let code = t.ops[next] & 0x1ff_ffff;
                let o = t.ops[at];
                let register = (o >> 8 & 31) as usize + usize::from(o >> 26 == op::CTC2) * 32;
                let need = write_need(code, register);
                unexpected += usize::from(wrong && distance as u32 >= need);
                let entry = entries.entry((name(code), code, register)).or_default();
                entry.0 = need;
                if distance < 2 {
                    entry.1[distance] |= wrong;
                }
            }
            if t.kind == kind::HAZARD_READ {
                let entry = reads.entry(distance).or_default();
                entry.0 += 1;
                entry.1 += usize::from(wrong);
            }
            if t.kind == kind::HAZARD_BUSY {
                // ops: lw, nop, first command, nop x b, write, nop x d, command or read
                let first = t.ops[2] & 0x1ff_ffff;
                let after = (next - 2) as u32;
                let window = (1..64).take_while(|&n| still_busy(first, n)).count() as u32;
                let entry = busy.entry(name(first)).or_insert((u32::MAX, window));
                if wrong {
                    entry.0 = entry.0.min(after);
                }
            }
        }
    }

    eprintln!(
        "GTE oracle {}: {} cases, {} templates",
        dir.display(),
        cases.len(),
        templates.len()
    );
    for (k, n) in by_kind.iter().enumerate().skip(1) {
        eprintln!("  {:<14} {n}", kind::NAMES[k]);
    }
    eprintln!("  command executions: {executions:?}");
    eprintln!(
        "  MVMVA encodings (sf, lm, mx, v, cv) from raw states: {}",
        mvmva.len()
    );
    for (command, bits) in &flags {
        let set: Vec<String> = (12..32)
            .filter(|&b| bits[b] > 0)
            .map(|b| format!("{b}:{}", bits[b]))
            .collect();
        eprintln!("  FLAG bits {command:<5} {}", set.join(" "));
    }
    // Observed need: the first distance at which a naive replay is right.
    let mut table = BTreeMap::<&str, Vec<String>>::new();
    let (mut with_need, mut confirmed) = (0, 0);
    for (&(command, code, register), &(need, wrong)) in &entries {
        let observed = if wrong[1] { 2 } else { u32::from(wrong[0]) };
        with_need += usize::from(need > 0);
        confirmed += usize::from(need > 0 && observed == need);
        if need > 0 && observed != need {
            eprintln!(
                "  not observed: {command} {code:07x} {} need {need}, seen {observed}",
                REGISTERS[register]
            );
        }
        if observed > 0 {
            let label = if command == "MVMVA" {
                format!("{}@{code:07x}={observed}", REGISTERS[register])
            } else {
                format!("{}={observed}", REGISTERS[register])
            };
            table.entry(command).or_default().push(label);
        }
    }
    eprintln!(
        "  hazards: {hazard_cases} cases, {naive_wrong} differ from a naive replay; \
         {confirmed} of {with_need} write-to-use table entries observed exactly"
    );
    for (command, needs) in &table {
        eprintln!("  observed need {command:<5} {}", needs.join(" "));
    }
    for (distance, (count, wrong)) in &reads {
        eprintln!(
            "  read {distance} instructions after a write: {wrong} of {count} see the old value"
        );
    }
    let windows: Vec<String> = busy
        .iter()
        .map(|(command, (visible, window))| format!("{command}:{window}/{visible}"))
        .collect();
    eprintln!(
        "  busy window (instructions predicted busy / first visible hazard) {}",
        windows.join(" ")
    );

    eprintln!(
        "  sub-pixel replay: {} vertices projected, {} positions kept, {} mismatches",
        subpixel.2, subpixel.3, subpixel.0
    );

    assert_eq!(
        failures.0,
        0,
        "{} of {} oracle cases differ:\n{}",
        failures.0,
        cases.len(),
        failures.1
    );
    assert_eq!(
        subpixel.0, 0,
        "sub-pixel replay differs or does not floor:\n{}",
        subpixel.1
    );
    // Raw random states saturate or overflow most projections: 1,403 of
    // 11,789 keep a position.
    assert!(subpixel.3 >= 1_000, "{} sub-pixel positions", subpixel.3);
    // Coverage: the oracle is not vacuous.
    let command_cases = by_kind[kind::COMMAND as usize] + by_kind[kind::PIPELINE as usize];
    assert!(command_cases >= 50_000, "{command_cases} command cases");
    for (_, command) in FUNCTS {
        assert!(
            raw_by_command.get(command).copied().unwrap_or(0) >= 1_000,
            "{command}"
        );
    }
    assert_eq!(mvmva.len(), 256, "every sf/lm/mx/v/cv combination");
    assert!(by_kind[kind::PIPELINE as usize] >= 1_000);
    assert!(by_kind[kind::TRANSFER as usize] >= 1_000);
    assert_eq!(transfers.len(), 128, "every register written and read");
    let all_flags: [usize; 32] = std::array::from_fn(|b| flags.values().map(|f| f[b]).sum());
    for (bit, &count) in all_flags.iter().enumerate().skip(12) {
        assert!(count > 0, "FLAG bit {bit} never set");
    }
    for (command, bit) in [
        ("RTPS", 17), // division overflow
        ("RTPT", 17),
        ("RTPS", 30), // 44-bit MAC1 overflow
        ("MVMVA", 27),
        ("RTPS", 18),  // SZ3 saturation (negative or far depth)
        ("RTPS", 12),  // IR0 saturation
        ("NCDT", 21),  // color saturation
        ("AVSZ4", 16), // MAC0 overflow
        ("NCLIP", 15),
    ] {
        assert!(flags[command][bit] > 0, "{command} FLAG bit {bit}");
    }
    assert!(by_kind[kind::HAZARD_WRITE as usize] > 0 && by_kind[kind::HAZARD_DOUBLE as usize] > 0);
    assert!(by_kind[kind::HAZARD_READ as usize] > 0);
    assert!(naive_wrong >= 1_000, "hazard cases must discriminate");
    assert_eq!(unexpected, 0, "a landed write changed a command's result");
    assert!(
        confirmed * 10 >= with_need * 9,
        "observed {confirmed} of {with_need} table entries"
    );
    assert_eq!(
        reads.get(&2).map(|r| r.1),
        Some(0),
        "reads see writes after two"
    );
    // The busy window is exact for every command: a hazard is first visible
    // one instruction after the last stalling one.
    assert_eq!(busy.len(), FUNCTS.len(), "every command as the running one");
    for (command, &(visible, window)) in &busy {
        assert_eq!(visible, window + 1, "{command} busy window");
    }
}
