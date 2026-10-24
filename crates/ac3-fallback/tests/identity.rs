//! Validation (a): the fallback interprets PAL routines that are translated,
//! and must leave exactly the state their Rust translation leaves.
//!
//! States: every entry snapshot of the PAL captures
//! (`analysis/{flight,menu}-runtime/*/snapshots.jsonl`), up to
//! `PER_ROUTINE` per routine, on the guest stack or a scratchpad stack.
//! For each, from the same state:
//! * native: `dispatch::call`;
//! * shallow: the fallback interprets the routine; its calls go through
//!   `dispatch::call` with the fallback behind it (the integration);
//! * deep: as shallow, but every routine above the device boundary that the
//!   routine reaches, directly or not, is interpreted too (forced), with
//!   only device routines native;
//! * oracle: the test oracle (`ac3-flight/tests/support/oracle.rs`) on the
//!   same extent, against the fallback with plain native calls.
//!
//! Interpreted runs must equal native: RAM outside the stack, scratchpad,
//! all GTE registers, `$sp`, the first fault and the host-call log; `v0`
//! when the translation returns one (a translation without a meaningful
//! `v0` returns 0) unless it is the original's `v0` from the return
//! snapshot. The oracle comparison includes `v0`. Deep differences are
//! traced to the routines whose interpretation alone causes them.
#[allow(unused_imports, unexpected_cfgs)]
#[path = "../../ac3-flight/tests/support/oracle.rs"]
mod oracle;
mod support;

use ac3_fallback::{Fallback, Version};
use ac3_flight::{Fault, HostCall, Machine, snapshot};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use support::{Pal, call, data_dir, differences};

const PER_ROUTINE: usize = 8;
/// Deep failures traced to their routines.
const TRACED: usize = 120;
/// Deep differences explained by what the original does with dead stack
/// memory (label prefix, reason). The test fails on any other.
const KNOWN_DEEP: &[(&str, &str)] = &[(
    "ovmisc-019 800cd1b4 #1 deep",
    "with no card file the read fails and 80026D00 parses the uninitialised stack \
     buffer as a TIM: an interpreted sprintf leaves the original's stack words \
     there, its translation other ones",
)];

struct Case {
    index: PathBuf,
    entry: snapshot::SnapshotInfo,
    /// The original's `v0` at the matching return, when captured.
    v0: Option<u32>,
}

#[derive(Default)]
struct Stats {
    cases: usize,
    routines: BTreeSet<u32>,
    owners: BTreeMap<String, usize>,
    gte_cases: usize,
    host_call_cases: usize,
    host_calls: usize,
    value_cases: usize,
    /// Routines whose interpreted v0 is right only with their callees
    /// interpreted too (a translated callee returns 0 for the value).
    callee_v0: BTreeSet<u32>,
    off_stack: usize,
    scratch_stack: usize,
    shallow_instructions: u64,
    deep_instructions: u64,
    deep_routines: BTreeSet<u32>,
    deepest: u64,
    crossed: usize,
    oracle_cases: usize,
    oracle_panics: usize,
    failures: Vec<String>,
    deep_failures: Vec<String>,
    oracle_failures: Vec<String>,
    /// Routine (module, start, name) -> deep cases it alone breaks.
    culprits: BTreeMap<(usize, u32, &'static str), usize>,
    untraced: usize,
    host_coupled: BTreeSet<u32>,
}

impl Stats {
    fn merge(&mut self, o: Stats) {
        self.cases += o.cases;
        self.routines.extend(o.routines);
        for (k, v) in o.owners {
            *self.owners.entry(k).or_default() += v;
        }
        self.gte_cases += o.gte_cases;
        self.host_call_cases += o.host_call_cases;
        self.host_calls += o.host_calls;
        self.value_cases += o.value_cases;
        self.callee_v0.extend(o.callee_v0);
        self.off_stack += o.off_stack;
        self.scratch_stack += o.scratch_stack;
        self.shallow_instructions += o.shallow_instructions;
        self.deep_instructions += o.deep_instructions;
        self.deep_routines.extend(o.deep_routines);
        self.deepest = self.deepest.max(o.deepest);
        self.crossed += o.crossed;
        self.oracle_cases += o.oracle_cases;
        self.oracle_panics += o.oracle_panics;
        self.failures.extend(o.failures);
        self.deep_failures.extend(o.deep_failures);
        self.oracle_failures.extend(o.oracle_failures);
        for (k, v) in o.culprits {
            *self.culprits.entry(k).or_default() += v;
        }
        self.untraced += o.untraced;
        self.host_coupled.extend(o.host_coupled);
    }
}

/// printf: a host call whose unused argument registers hold whatever the
/// caller left (an interpreted caller's real registers, a translation's 0).
const PRINTF: u32 = 0x800a_c3e8;
/// ExitCriticalSection, which leaves `v0` alone.
const EXIT_CRITICAL_SECTION: u32 = 0x800a_d7f8;

/// The number of conversions in the format string at `format`.
fn conversions(m: &Machine, format: u32) -> usize {
    let byte = |at: u32| (m.peek(at & !3) >> ((at & 3) * 8)) as u8;
    let (mut count, mut at) = (0, format);
    while byte(at) != 0 && at - format < 512 {
        if byte(at) == b'%' {
            if byte(at + 1) == b'%' {
                at += 1;
            } else {
                count += 1;
            }
        }
        at += 1;
    }
    count
}

/// Two host calls agree: the same routine and the arguments it reads, where
/// a pointer into the stack may differ (frames sit elsewhere when a
/// translation keeps its locals in Rust; the stack is not compared).
fn same_call(m: &Machine, a: &HostCall, b: &HostCall) -> bool {
    let stack = |v: u32| (ac3_flight::machine::STACK_LIMIT..0x8020_0000).contains(&v);
    let used = if a.function == PRINTF {
        1 + conversions(m, a.args[0]).min(3)
    } else {
        4
    };
    a.function == b.function
        && (0..used).all(|i| a.args[i] == b.args[i] || (stack(a.args[i]) && stack(b.args[i])))
}

/// GTE data register `i` as compared: VZ0..VZ2 by their low halfword. The
/// upper halfword is a stale stack word `lwc2` loads with an SVECTOR's pad,
/// which the GTE never uses; it differs where a translation keeps in Rust a
/// value the original spilled (FLIGHT_MACHINE.md, rule 5).
fn gte_word(m: &Machine, i: usize) -> u32 {
    if matches!(i, 1 | 3 | 5) {
        m.gte.data[i] & 0xffff
    } else {
        m.gte.data[i]
    }
}

/// State differences of `got` from `want` (faults taken), as text.
fn state(want: &mut Machine, got: &mut Machine, sp: u32) -> Option<String> {
    let (fw, fg) = (want.take_fault(), got.take_fault());
    let (ram, scratch, sp_ok) = differences(want, got, sp);
    let gte: Vec<usize> = (0..32)
        .filter(|&i| gte_word(want, i) != gte_word(got, i))
        .chain(
            (0..32)
                .filter(|&i| want.gte.control[i] != got.gte.control[i])
                .map(|i| 32 + i),
        )
        .collect();
    let differs = |i: usize| match (want.host_calls.get(i), got.host_calls.get(i)) {
        (Some(a), Some(b)) => !same_call(want, a, b),
        _ => true,
    };
    let calls_len = want.host_calls.len().max(got.host_calls.len());
    let calls = !(0..calls_len).any(differs);
    if fw == fg && ram.is_empty() && scratch.is_empty() && gte.is_empty() && sp_ok && calls {
        return None;
    }
    let call = (0..calls_len)
        .find(|&i| differs(i))
        .map(|i| {
            format!(
                " first differing call #{i}: {:x?} / {:x?}",
                want.host_calls.get(i),
                got.host_calls.get(i)
            )
        })
        .unwrap_or_default();
    Some(format!(
        "fault {fw:?}/{fg:?} ram {} {:08x?} scratch {:08x?} gte {gte:?} sp {:x}/{:x} host calls {}/{}{call}",
        ram.len(),
        &ram[..ram.len().min(8)],
        &scratch[..scratch.len().min(8)],
        want.sp,
        got.sp,
        want.host_calls.len(),
        got.host_calls.len(),
    ))
}

/// A deep run of `callee` from `base` with `fallback`.
fn deep_run(fallback: &Fallback, base: &Machine, callee: u32, args: [u32; 4]) -> (Machine, u32) {
    let mut m = base.clone();
    let v0 = call(fallback, true, &mut Pal(&mut m), callee, args);
    (m, v0)
}

fn spec(module: usize, start: u32) -> String {
    if module == 0 {
        format!("{start:x}")
    } else {
        format!("ov{module}:{start:x}")
    }
}

struct Worker {
    shallow: Fallback,
    deep: Fallback,
    stats: Stats,
}

impl Worker {
    fn run(&mut self, label: &str, case: &Case, entry: &snapshot::Snapshot) {
        let stats = &mut self.stats;
        let base = entry.machine();
        let callee = entry.info.callee;
        let args = [entry.gpr(4), entry.gpr(5), entry.gpr(6), entry.gpr(7)];
        let mut probe = base.clone();
        let Some(owner) = self.shallow.owner(&mut Pal(&mut probe), callee) else {
            return;
        };
        if !owner.extent.interpretable() || owner.extent.start != callee {
            return;
        }
        if owner.extent.flags & ac3_fallback::flags::HOST != 0 {
            // Its translation talks to the host at another level than
            // the original instructions (see the generator): not compared.
            stats.host_coupled.insert(callee);
            return;
        }
        let sp = base.sp;
        if (0x1f80_0000..0x1f80_0400).contains(&sp) {
            stats.scratch_stack += 1;
        } else if !(ac3_flight::machine::STACK_LIMIT..0x8020_0000).contains(&sp) {
            // An interrupt handler's stack in BSS (libcd's callbacks): the
            // translations' frames fault there by design.
            stats.off_stack += 1;
            return;
        }
        let mut nat = base.clone();
        let nv0 = ac3_flight::dispatch::call(&mut nat, callee, args);
        if nat.fault() == Some(Fault::Untranslated { address: callee }) {
            return;
        }
        stats.cases += 1;
        stats.routines.insert(callee);
        let owner_name = if owner.extent.owner.is_empty() {
            "(none)".to_owned()
        } else {
            owner.extent.owner.to_owned()
        };
        *stats.owners.entry(owner_name).or_default() += 1;
        stats.gte_cases += usize::from(nat.gte != base.gte);
        stats.host_call_cases += usize::from(!nat.host_calls.is_empty());
        stats.host_calls += nat.host_calls.len();
        stats.value_cases += usize::from(nv0 != 0);
        // A v0 is right when it is the translation's or the original's;
        // a translation returning 0 has no meaningful v0.
        let right = |v: u32| v == nv0 || case.v0 == Some(v) || nv0 == 0;

        // Shallow.
        let mut sh = base.clone();
        let (before, crossings) = (self.shallow.instructions(), self.shallow.crossings());
        let fallback = &self.shallow;
        let sv0 = fallback.run(
            &mut Pal(&mut sh),
            callee,
            args,
            &mut |g: &mut Pal<'_>, t, a| call(fallback, false, g, t, a),
        );
        let steps = self.shallow.instructions() - before;
        let crossed = self.shallow.crossings() != crossings;
        stats.crossed += usize::from(crossed);
        stats.shallow_instructions += steps;
        let shallow_problem = state(&mut nat.clone(), &mut sh, sp);

        // Deep.
        let before = self.deep.instructions();
        let (mut dp, dv0) = deep_run(&self.deep, &base, callee, args);
        stats.deep_instructions += self.deep.instructions() - before;
        let deep_problem = state(&mut nat.clone(), &mut dp, sp);

        if shallow_problem.is_some() || !right(sv0) {
            if shallow_problem.is_none() && deep_problem.is_none() && right(dv0) {
                // Only v0, and interpreting the callees too gives the right
                // one: a translated callee returns 0 where the original
                // leaves the value this routine passes on.
                stats.callee_v0.insert(callee);
            } else {
                stats.failures.push(format!(
                    "{label} shallow: {} v0 {nv0:#x}/{sv0:#x} (original {:x?})",
                    shallow_problem.unwrap_or_default(),
                    case.v0
                ));
            }
        }
        if deep_problem.is_some() || !right(dv0) {
            let mut text = format!(
                "{label} deep: {} v0 {nv0:#x}/{dv0:#x} (original {:x?})",
                deep_problem.unwrap_or_default(),
                case.v0
            );
            let alone = {
                let mut one = Fallback::new(Version::Pal);
                one.force(&spec(owner.module, callee)).unwrap();
                let (mut m, v0) = deep_run(&one, &base, callee, args);
                state(&mut nat.clone(), &mut m, sp).is_some() || !right(v0)
            };
            if alone {
                text += " <- the routine alone";
            } else if stats.deep_failures.len() < TRACED {
                // Which routine's interpretation alone breaks it?
                let mut all = Fallback::new(Version::Pal);
                all.force("all").unwrap();
                deep_run(&all, &base, callee, args);
                let mut found = Vec::new();
                for (other, _, _) in all.reached_routines() {
                    if other.extent.start == callee {
                        continue;
                    }
                    let mut one = Fallback::new(Version::Pal);
                    one.force(&format!(
                        "{},{}",
                        spec(owner.module, callee),
                        spec(other.module, other.extent.start)
                    ))
                    .unwrap();
                    let (mut m, v0) = deep_run(&one, &base, callee, args);
                    if state(&mut nat.clone(), &mut m, sp).is_some() || !right(v0) {
                        found.push(format!("{:08x} {}", other.extent.start, other.extent.name));
                        *stats
                            .culprits
                            .entry((other.module, other.extent.start, other.extent.name))
                            .or_default() += 1;
                    }
                }
                if found.is_empty() {
                    stats.untraced += 1;
                }
                text += &format!(" <- {found:?}");
            }
            stats.deep_failures.push(text);
        }

        // The oracle, against the fallback with plain native calls. Its
        // semantics differ where the table splits a routine (it continues
        // at `$ra` after any exit); it asserts below 10M steps.
        let unsupported =
            matches!(sh.fault(), Some(Fault::Trap { code, .. }) if code >= ac3_fallback::RUNAWAY);
        if steps < 9_000_000 && !unsupported && !crossed {
            let mut plain = base.clone();
            let pv0 = self.shallow.run(
                &mut Pal(&mut plain),
                callee,
                args,
                &mut |g: &mut Pal<'_>, t, a| ac3_flight::dispatch::call(g.0, t, a),
            );
            let mut or = base.clone();
            let (start, end) = (owner.extent.start, owner.extent.end());
            let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                oracle::run(&mut or, start, end, args)
            }));
            match run {
                Err(_) => stats.oracle_panics += 1,
                Ok(ov0) => {
                    stats.oracle_cases += 1;
                    let problem = state(&mut or, &mut plain, sp);
                    // The oracle sets v0 from every call; after ExitCriticalSection
                    // (which leaves v0 alone) the fallback keeps it, and then
                    // agrees with the translation where the oracle does not.
                    let keeps = plain
                        .host_calls
                        .iter()
                        .any(|c| c.function == EXIT_CRITICAL_SECTION);
                    if problem.is_some() || (ov0 != pv0 && !keeps) {
                        stats.oracle_failures.push(format!(
                            "{label} oracle: {} v0 {ov0:#x}/{pv0:#x}",
                            problem.unwrap_or_default()
                        ));
                    }
                }
            }
        }
    }
}

fn indexes() -> Vec<PathBuf> {
    let dir = data_dir().join("analysis");
    let mut out = Vec::new();
    for group in ["flight-runtime", "menu-runtime"] {
        let Ok(entries) = std::fs::read_dir(dir.join(group)) else {
            continue;
        };
        for entry in entries.flatten() {
            let index = entry.path().join("snapshots.jsonl");
            if index.is_file() {
                out.push(index);
            }
        }
    }
    out.sort();
    out
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the PAL snapshot captures"]
fn interpreted_routines_match_their_translations() {
    // Up to PER_ROUTINE entries per routine, in capture order, each with the
    // matching return's v0 (the first later return of the callee with the
    // same `$ra` and `$sp`, as `snapshot::check_call` pairs them).
    let mut taken: BTreeMap<u32, usize> = BTreeMap::new();
    let mut cases = Vec::new();
    for index in indexes() {
        let Ok(infos) = snapshot::read_index(&index) else {
            continue;
        };
        for (at, info) in infos.iter().enumerate().filter(|(_, s)| s.stage == "entry") {
            let n = taken.entry(info.callee).or_default();
            if *n >= PER_ROUTINE {
                continue;
            }
            *n += 1;
            let v0 = infos[at + 1..]
                .iter()
                .find(|s| {
                    s.callee == info.callee
                        && s.stage == "return"
                        && s.ra == info.ra
                        && s.registers.gpr[29] == info.registers.gpr[29]
                })
                .map(|s| s.registers.gpr[2]);
            cases.push(Case {
                index: index.clone(),
                entry: info.clone(),
                v0,
            });
        }
    }
    let threads = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .min(16);
    let started = std::time::Instant::now();
    let results: Vec<Stats> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|t| {
                let cases = &cases;
                std::thread::Builder::new()
                    .stack_size(256 << 20)
                    .spawn_scoped(scope, move || {
                        let mut worker = Worker {
                            shallow: Fallback::new(Version::Pal),
                            deep: Fallback::new(Version::Pal),
                            stats: Stats::default(),
                        };
                        worker.deep.force("all").unwrap();
                        for case in cases.iter().skip(t).step_by(threads) {
                            let directory = case.index.parent().unwrap();
                            let Ok(entry) = snapshot::Snapshot::load(directory, &case.entry) else {
                                continue;
                            };
                            let capture = directory.file_name().unwrap().to_string_lossy();
                            let label = format!(
                                "{capture} {:08x} #{}",
                                case.entry.callee, case.entry.sequence
                            );
                            worker.run(&label, case, &entry);
                        }
                        let mut stats = worker.stats;
                        stats.deepest = worker.deep.report()["deepest"].as_u64().unwrap_or(0);
                        for (owner, _, _) in worker.deep.reached_routines() {
                            stats.deep_routines.insert(owner.extent.start);
                        }
                        stats
                    })
                    .unwrap()
            })
            .collect();
        workers.into_iter().map(|w| w.join().unwrap()).collect()
    });
    let mut total = Stats::default();
    for stats in results {
        total.merge(stats);
    }
    let seconds = started.elapsed().as_secs_f64();
    eprintln!(
        "identity: {} cases of {} routines from {} entry snapshots, {threads} threads, {seconds:.1} s",
        total.cases,
        total.routines.len(),
        cases.len(),
    );
    eprintln!(
        "  skipped {} on interrupt stacks and {} host-coupled routines {:08x?}; {} cases on a scratchpad stack",
        total.off_stack,
        total.host_coupled.len(),
        total.host_coupled,
        total.scratch_stack
    );
    eprintln!(
        "  {} cases change the GTE, {} make host calls ({} calls), {} return a value natively; \
         {} routines pass on a v0 that a translated callee returns as 0: {:08x?}",
        total.gte_cases,
        total.host_call_cases,
        total.host_calls,
        total.value_cases,
        total.callee_v0.len(),
        total.callee_v0
    );
    eprintln!(
        "  interpreted: shallow {} instructions ({} runs crossed into another extent); deep {} \
         instructions over {} routines, nesting {}; oracle cases {} ({} oracle runaways)",
        total.shallow_instructions,
        total.crossed,
        total.deep_instructions,
        total.deep_routines.len(),
        total.deepest,
        total.oracle_cases,
        total.oracle_panics
    );
    eprintln!("  cases by PAL Rust owner: {:?}", total.owners);
    for (kind, list) in [
        ("shallow", &total.failures),
        ("deep", &total.deep_failures),
        ("oracle", &total.oracle_failures),
    ] {
        eprintln!("  {kind} failures: {}", list.len());
        for line in list.iter().take(60) {
            eprintln!("    {line}");
        }
    }
    eprintln!(
        "  deep failures traced to routines (first {TRACED} per worker; {} untraced):",
        total.untraced
    );
    for ((module, start, name), n) in &total.culprits {
        eprintln!("    module {module} {start:08x} {name}: {n} cases");
    }
    assert!(total.cases >= 2000, "{} cases", total.cases);
    assert!(total.failures.is_empty(), "shallow failures");
    assert!(total.oracle_failures.is_empty(), "oracle failures");
    let unknown: Vec<&String> = total
        .deep_failures
        .iter()
        .filter(|f| !KNOWN_DEEP.iter().any(|(label, _)| f.starts_with(label)))
        .collect();
    for (label, reason) in KNOWN_DEEP {
        if total.deep_failures.iter().any(|f| f.starts_with(label)) {
            eprintln!("  known deep difference {label}: {reason}");
        }
    }
    assert!(unknown.is_empty(), "deep failures");
}

/// One case with a call trace: `AC3_FB_CASE=capture:callee-hex:sequence`
/// (and `AC3_FB_DEEP=1` for the deep run).
#[test]
#[ignore = "debugging aid: set AC3_FB_CASE"]
fn trace_one_case() {
    let Ok(spec) = std::env::var("AC3_FB_CASE") else {
        return;
    };
    let parts: Vec<&str> = spec.split(':').collect();
    let index = data_dir()
        .join("analysis/flight-runtime")
        .join(parts[0])
        .join("snapshots.jsonl");
    let index = if index.is_file() {
        index
    } else {
        data_dir()
            .join("analysis/menu-runtime")
            .join(parts[0])
            .join("snapshots.jsonl")
    };
    let callee = u32::from_str_radix(parts[1], 16).unwrap();
    let sequence: u64 = parts[2].parse().unwrap();
    let infos = snapshot::read_index(&index).unwrap();
    let info = infos
        .iter()
        .find(|s| s.callee == callee && s.sequence == sequence && s.stage == "entry")
        .expect("entry");
    let entry = snapshot::Snapshot::load(index.parent().unwrap(), info).unwrap();
    let base = entry.machine();
    let args = [entry.gpr(4), entry.gpr(5), entry.gpr(6), entry.gpr(7)];
    let mut nat = base.clone();
    let nv0 = ac3_flight::dispatch::call(&mut nat, callee, args);
    let deep = std::env::var_os("AC3_FB_DEEP").is_some();
    let mut fallback = Fallback::new(Version::Pal);
    if deep {
        let spec = std::env::var("AC3_FB_FORCE").unwrap_or_else(|_| "all".to_owned());
        fallback.force(&spec).unwrap();
    }
    fallback.set_trace(600);
    let mut m = base.clone();
    let v0 = if deep {
        call(&fallback, true, &mut Pal(&mut m), callee, args)
    } else {
        let f = &fallback;
        f.run(
            &mut Pal(&mut m),
            callee,
            args,
            &mut |g: &mut Pal<'_>, t, a| call(f, false, g, t, a),
        )
    };
    eprintln!(
        "native v0 {nv0:#x} host calls {:x?}",
        &nat.host_calls[..nat.host_calls.len().min(40)]
    );
    eprintln!(
        "interp v0 {v0:#x} host calls {:x?}",
        &m.host_calls[..m.host_calls.len().min(40)]
    );
    eprintln!("{:?}", state(&mut nat, &mut m, base.sp));
    eprintln!(
        "{}",
        serde_json::to_string_pretty(&fallback.report()["faults"]).unwrap()
    );
}
