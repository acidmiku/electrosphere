//! Differential validation of the re-addressed exact routines against the
//! JP originals. For every resident JP function the map calls exact (the
//! same instructions as its PAL counterpart once addresses are masked) and
//! the generated crate claims, the original JP MIPS (the test-only oracle,
//! `support::oracle`, re-addressed from the PAL crate's) and the generated
//! Rust (`dispatch::call`) run from the same JP machine state and must leave
//! the same RAM (outside the stack window), scratchpad, GTE, `sp`, first
//! fault and `v0` (`support::harness`).
//!
//! States: the four JP Redux RAM captures (`analysis/redux/run1/probe-*`),
//! with the resident code checked against `ACE3.BIN`, each with randomised
//! arguments (pointers found in the gp area, zero, small numbers) and, for
//! half the variants, randomised gp-area words: the PAL tests' method.
//! Calls out of the routine run natively in both runs, so only the routine
//! under test differs.
//!
//! Control: the same harness runs the PAL counterparts in the PAL crate
//! against the PAL originals on four PAL RAM captures (`AC3_DATA_DIR`). A
//! function that fails in both fails for a reason of the harness (random
//! states, routines it cannot isolate), not of the re-addressing; the
//! JP-only failures are the ones to read. Per-function results:
//! `$CARGO_TARGET_TMPDIR/jp-differential.tsv`.
mod support;

#[path = "../../ac3-flight/tests/support/oracle.rs"]
mod pal_oracle;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use support::harness::{self, Fault, State, Target, Verdict};
use support::{oracle, scan};

/// Resident code compared with ACE3.BIN (`start` .. the end of the SDK).
const TEXT: (u32, u32) = (0x8002_5724, 0x800b_0130);
/// PAL RAM captures of the control (under `AC3_DATA_DIR`).
const PAL_STATES: [&str; 4] = [
    "analysis/menu-runtime/baseline-001/frame-00060.ram.bin",
    "analysis/menu-runtime/hub-render-001/checkpoint.ram.bin",
    "analysis/flight-runtime/whole-flight-001/frame-00316.ram.bin",
    "analysis/flight-runtime/boot-001/main-loop.ram.bin",
];

macro_rules! target {
    ($name:ident, $krate:ident, $oracle:ident) => {
        struct $name($krate::Machine);
        impl Target for $name {
            const GP: u32 = $krate::GP;
            fn from_ram(ram: &[u8], sp: u32) -> Self {
                let mut m = $krate::Machine::from_images(ram, &[0; 0x400]).expect("images");
                m.sp = sp;
                Self(m)
            }
            fn duplicate(&self) -> Self {
                Self(self.0.clone())
            }
            fn sw(&mut self, address: u32, value: u32) {
                self.0.sw(address, value);
            }
            fn limit_host_calls(&mut self, limit: usize) {
                struct Limit(usize, usize);
                impl $krate::Device for Limit {
                    fn call(
                        &mut self,
                        _: &mut [u8],
                        _: &mut [u8],
                        _: $krate::HostCall,
                    ) -> Option<u32> {
                        self.0 += 1;
                        assert!(self.0 <= self.1, "runaway: {} host calls", self.1);
                        None
                    }
                }
                self.0.device = Some(Box::new(Limit(0, limit)));
            }
            fn oracle(&mut self, start: u32, end: u32, args: [u32; 4]) -> u32 {
                $oracle::run(&mut self.0, start, end, args)
            }
            fn native(&mut self, start: u32, args: [u32; 4]) -> u32 {
                $krate::dispatch::call(&mut self.0, start, args)
            }
            fn take_fault(&mut self) -> Option<Fault> {
                use $krate::Fault as F;
                self.0.take_fault().err().map(|f| match f {
                    F::Untranslated { address } => Fault::Untranslated(address),
                    F::Address { address, .. } => Fault::Address(address),
                    F::Misaligned { address, .. } => Fault::Misaligned(address),
                    other => Fault::Other(format!("{other:?}")),
                })
            }
            fn ram(&self) -> &[u8] {
                self.0.ram()
            }
            fn scratch(&self) -> &[u8] {
                self.0.scratch()
            }
            fn same_gte(&self, other: &Self) -> bool {
                self.0.gte == other.0.gte
            }
            fn sp(&self) -> u32 {
                self.0.sp
            }
        }
    };
}
target!(Jp, ac3_flight_jp, oracle);
target!(Pal, ac3_flight, pal_oracle);

/// The JP RAM captures, with the resident code compared with (and, should a
/// word differ, restored from) ACE3.BIN; and the number of words restored.
fn jp_states(dir: &Path) -> (Vec<State>, Vec<usize>) {
    let data = std::fs::read(dir.join("extracted/disc1/disc/ACE3.BIN")).expect("ACE3.BIN");
    assert_eq!(&data[..8], b"PS-X EXE");
    let payload = &data[0x800..];
    let (mut states, mut restored) = (Vec::new(), Vec::new());
    for n in 1..=4 {
        let path = dir.join(format!("analysis/redux/run1/probe-{n}/ram.bin"));
        let mut ram = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(ram.len(), 0x20_0000);
        let mut words = 0;
        for a in (TEXT.0..TEXT.1).step_by(4) {
            let (at, src) = ((a & 0x1f_ffff) as usize, (a - 0x8001_0000) as usize);
            if ram[at..at + 4] != payload[src..src + 4] {
                ram[at..at + 4].copy_from_slice(&payload[src..src + 4]);
                words += 1;
            }
        }
        states.push((format!("probe-{n}"), ram));
        restored.push(words);
    }
    (states, restored)
}

fn pal_states() -> Option<Vec<State>> {
    let dir = PathBuf::from(std::env::var_os("AC3_DATA_DIR")?);
    PAL_STATES
        .iter()
        .map(|p| {
            let ram = std::fs::read(dir.join(p)).ok()?;
            (ram.len() == 0x20_0000).then(|| (p.rsplit('/').nth(1).unwrap_or(p).to_string(), ram))
        })
        .collect()
}

fn cause(v: &Option<Verdict>) -> Option<&str> {
    match v {
        Some(Err((cause, _, _))) => Some(cause),
        _ => None,
    }
}

#[test]
#[ignore = "requires AC3_JP_DIR"]
fn exact_routines_match_the_jp_original() {
    let dir = scan::jp_dir();
    let rows = scan::jp_map(&dir);
    let (claims, void) = scan::jp_resident_claims();
    let exact: Vec<&scan::Row> = rows
        .iter()
        .filter(|r| r.region.starts_with("resident") && r.class == "exact" && r.pal_space == "pal")
        .collect();
    // and every resident routine of the patch layer, whatever its class
    let patched: Vec<u32> = claims
        .iter()
        .filter(|(_, sites)| sites.iter().any(|(f, _)| f.starts_with("jp/")))
        .map(|(a, _)| *a)
        .chain(scan::patch_table("REPLACES"))
        .collect();
    let tested: Vec<&scan::Row> = rows
        .iter()
        .filter(|r| r.region.starts_with("resident") && r.size > 0)
        .filter(|r| {
            patched.contains(&r.jp)
                || (r.class == "exact"
                    && r.pal_space == "pal"
                    && claims.get(&r.jp).map_or(0, Vec::len) == 1)
        })
        .collect();
    let (states, restored) = jp_states(&dir);
    let functions: Vec<(u32, u32, bool)> = tested
        .iter()
        .map(|r| (r.jp, r.jp + r.size, void.contains(&r.jp)))
        .collect();
    let (jp, jp_cases) = harness::run_all::<Jp>(&functions, states);

    // the PAL control: the same harness on the PAL counterparts
    let (pal_claims, pal_void) =
        scan::claims_and_void(&scan::pal_crate_dir(), &|n| n.starts_with("ov"));
    let pal_sizes: BTreeMap<u32, u32> = scan::pal_sizes(&dir);
    let control: Vec<(usize, (u32, u32, bool))> = tested
        .iter()
        .enumerate()
        .filter_map(|(i, r)| {
            let p = r.pal?;
            let size = pal_sizes.get(&p).copied()?;
            (pal_claims.get(&p).map_or(0, Vec::len) == 1)
                .then_some((i, (p, p + size, pal_void.contains(&p))))
        })
        .collect();
    let mut pal: Vec<Option<Verdict>> = vec![None; tested.len()];
    let mut pal_cases = 0;
    if let Some(states) = pal_states() {
        let list: Vec<(u32, u32, bool)> = control.iter().map(|c| c.1).collect();
        let (verdicts, n) = harness::run_all::<Pal>(&list, states);
        pal_cases = n;
        for ((i, _), v) in control.iter().zip(verdicts) {
            pal[*i] = v;
        }
    }

    let mut report = String::from(
        "jp_address\tpal_address\tsize\tjp_result\tjp_cases\tjp_ram_bytes\tpal_result\tdetail\n",
    );
    let mut table: BTreeMap<(String, String), usize> = BTreeMap::new();
    let (mut pass, mut vacuous, mut jp_only) = (0, 0, Vec::new());
    for (i, r) in tested.iter().enumerate() {
        let pal_text = match &pal[i] {
            None => "-".to_string(),
            Some(Ok(_)) => "pass".to_string(),
            Some(Err((c, _, _))) => c.clone(),
        };
        let (jp_text, cases, bytes, detail) = match &jp[i] {
            None => ("not run".to_string(), 0, 0, String::new()),
            Some(Ok((n, b))) => {
                pass += 1;
                vacuous += usize::from(*b == 0);
                ("pass".to_string(), *n, *b, String::new())
            }
            Some(Err((c, case, d))) => (
                c.clone(),
                0,
                0,
                format!("{case}: {d}").replace(['\t', '\n'], " "),
            ),
        };
        if jp_text != "pass" {
            let control = match &pal[i] {
                None => "no PAL control",
                Some(Ok(_)) => "PAL control passes",
                Some(Err(_)) => "fails in the PAL control too",
            };
            *table
                .entry((jp_text.clone(), control.to_string()))
                .or_default() += 1;
            if matches!(pal[i], Some(Ok(_))) {
                jp_only.push(format!(
                    "{:08x} (PAL {:08x}) {jp_text}: {detail}",
                    r.jp,
                    r.pal.unwrap_or(0)
                ));
            }
        }
        let _ = writeln!(
            report,
            "{:08x}\t{:08x}\t{}\t{jp_text}\t{cases}\t{bytes}\t{pal_text}\t{detail}",
            r.jp,
            r.pal.unwrap_or(0),
            r.size
        );
    }
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join("jp-differential.tsv");
    std::fs::write(&path, &report).expect("report");
    println!(
        "differential: {} exact resident JP functions, {} claimed once and tested over {jp_cases} cases \
         (resident words restored from ACE3.BIN: {restored:?})",
        exact.len(),
        tested.len(),
    );
    println!(
        "differential: {pass} pass ({vacuous} without a RAM change: v0, GTE and faults compared), {} fail, \
         {} not run",
        jp.iter().filter(|v| cause(v).is_some()).count(),
        jp.iter().filter(|v| v.is_none()).count()
    );
    println!(
        "PAL control: {} counterparts over {pal_cases} cases, {} pass",
        control.len(),
        pal.iter().filter(|v| matches!(v, Some(Ok(_)))).count()
    );
    for ((c, control), n) in &table {
        println!("  {n:5}  {c} / {control}");
    }
    for (i, r) in tested.iter().enumerate() {
        if patched.contains(&r.jp) {
            let result = match &jp[i] {
                Some(Ok((n, _))) => format!("pass ({n} cases)"),
                Some(Err((c, case, d))) => format!("{c}: {case}: {d}"),
                None => "not run".into(),
            };
            println!("patch layer {:08x} ({}): {result}", r.jp, r.class);
        }
    }
    println!(
        "JP-only failures (the PAL control passes): {}",
        jp_only.len()
    );
    for line in &jp_only {
        println!("  {line}");
    }
    println!("differential: per-function results in {}", path.display());
    assert!(pass > 800, "too few passes: {pass}");
}
