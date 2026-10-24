//! `ov2_misc` (overlay-2 inner states 5, 6, 7/8, 10 and 15) against the
//! original game on ov2air's call traces (see `tests/ov2air_trace/mod.rs`)
//! and, on paths no capture reaches, its original-code oracle; plus unit
//! tests.
mod ov2air_trace;

use ac3_flight::{
    Machine,
    ov2_aircraft::{self, Calls},
    ov2_misc,
};
use ov2air_trace::{Stats, Trace, check_capture, oracle};
use std::collections::{BTreeMap, BTreeSet};

/// Routines whose `v0` no caller uses (not compared).
const VOID: [u32; 9] = [
    0x800d_a9b8,
    0x800e_49dc,
    0x800e_5288,
    0x800e_5c98,
    0x800e_62c0,
    0x800e_6378,
    0x800e_6d44,
    0x800e_6dfc,
    0x800e_6ec0,
];

fn routines() -> Vec<(u32, bool)> {
    ov2_misc::ROUTINES
        .iter()
        .map(|&r| (r, !VOID.contains(&r)))
        .collect()
}

/// The traces (`.ov2air/captures.py`; perturbed ones are written once, see
/// there).
const CAPTURES: [(&str, &str); 9] = [
    (
        "ov2air-012",
        "natural: hub choice 1, mission records (state 7): entry, navigation, exit to the hub",
    ),
    (
        "ov2air-013",
        "natural: the records screen entered and left five times",
    ),
    (
        "ov2air-022",
        "state 15 (progress low byte 0x21 written at the state-3 entry): reload, then state 10",
    ),
    (
        "ov2air-023",
        "natural: hub choice 3, OPTIONS (state 10): GAME CONFIG, the sound item, back",
    ),
    (
        "ov2air-026",
        "OPTIONS with context +0x974 bit 2 set at the state-10 entry: the music player (ov2_options), then state 15",
    ),
    (
        "ov2air-029",
        "natural: GAME CONFIG items 1..4 by DOWN, each adjusted and confirmed",
    ),
    (
        "ov2air-030",
        "natural: GAME CONFIG items 4..1 by UP, the language changed, the others cancelled",
    ),
    (
        "ov2air-015",
        "free mission as 021 without the cleared missions: the empty mission list (state 5); the original then uploads a garbage image and the game stops (trace truncated)",
    ),
    (
        "ov2air-021",
        "free mission (hub routed as state 4, option 0x10, missions 1..6 cleared): mission list (state 5); the original crashes entering state 6, trace truncated",
    ),
];

/// Every traced occurrence reproduces the original.
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/ov2air-012..030 (those listed)"]
fn traced_captures_reproduce_the_original() {
    let routines = routines();
    let mut total: BTreeMap<u32, Stats> = BTreeMap::new();
    for (capture, description) in CAPTURES {
        eprintln!("== {capture}: {description}");
        for (routine, s) in check_capture(&Trace::load(capture), &routines) {
            let t = total.entry(routine).or_default();
            t.passed += s.passed;
            t.effective += s.effective;
            t.changed += s.changed;
            t.spliced += s.spliced;
            t.interrupts += s.interrupts;
        }
    }
    eprintln!("== all captures");
    let mut unreached = Vec::new();
    for (routine, _) in &routines {
        match total.get(routine) {
            Some(s) => eprintln!(
                "{routine:08x}: {} exact ({} changing state), {} bytes changed, {} calls spliced, {} interrupts",
                s.passed, s.effective, s.changed, s.spliced, s.interrupts
            ),
            None => unreached.push(format!("{routine:08x}")),
        }
    }
    eprintln!("not reached by these captures: {}", unreached.join(" "));
}

/// Records every outside call and returns 0.
#[derive(Default)]
struct Recorder {
    calls: Vec<(u32, [u32; 4])>,
}

impl Calls for Recorder {
    fn call(&mut self, _: &mut Machine, target: u32, args: [u32; 4]) -> u32 {
        self.calls.push((target, args));
        0
    }
}

/// Every listed routine is claimed (run once on an empty machine).
#[test]
fn every_routine_is_claimed() {
    for &routine in &ov2_misc::ROUTINES {
        let mut m = Machine::default();
        m.sp = 0x801f_ff00;
        let mut calls = Recorder::default();
        assert!(
            ov2_misc::routines::dispatch(&mut m, &mut calls, routine, [0; 4]).is_some(),
            "{routine:08x}"
        );
    }
}

// ---- original-code differential on paths no capture reaches -------------

const CONTEXT: u32 = 0x800b_d158;

/// Every combination must agree with the interpreted original (one call
/// each, accumulated into `s`).
fn differential_cases(s: &mut Sessions, cases: Vec<(String, Machine, u32, [u32; 4])>) {
    for (name, m, routine, args) in cases {
        s.once(&name, &m, routine, args, &oracle::zero);
    }
}

/// Records screen (state 7/8) on free-mission flags, difficulties, input
/// bits and mission bounds the natural capture does not visit, started
/// from the natural records state (ov2air-012).
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/ov2air-012"]
fn records_paths_match_the_original_code() {
    let trace = Trace::load("ov2air-012");
    let (base, args) = ov2air_trace::machine_at(&trace, 0x800d_b6cc, 5);
    let mut probe = base.clone();
    let block = probe.lw(0x800f_6034);
    let mut cases = Vec::new();
    for flags in [0u32, 1, 2, 3] {
        for difficulty in [0u16, 1, 2, 3] {
            for (mission, last) in [(1u16, 1u32), (1, 0x24), (0x24, 0x24), (5, 9)] {
                for (pressed, repeat) in [(0, 0), (0x20, 0), (0, 8), (0, 4), (2, 0), (1, 0), (0, 2)]
                {
                    let mut m = base.clone();
                    m.sw(block, flags);
                    m.sh(block + 4, difficulty);
                    m.sh(block + 6, mission);
                    m.sw(block + 8, last);
                    m.sw(CONTEXT + 0x12c, pressed);
                    m.sw(CONTEXT + 0x130, repeat);
                    let name = format!(
                        "800db6cc flags {flags} difficulty {difficulty} mission {mission}/{last} pressed {pressed:x} repeat {repeat:x}"
                    );
                    cases.push((name, m, 0x800d_b6cc, args));
                }
            }
            for t in [0u32, 7, 25] {
                let mut m = base.clone();
                m.sw(block, flags);
                m.sh(block + 4, difficulty);
                cases.push((
                    format!("800da9b8 flags {flags} difficulty {difficulty} t {t}"),
                    m,
                    0x800d_a9b8,
                    [t, 0, 0, 0],
                ));
            }
        }
    }
    for options in [0u32, 0x10] {
        let mut m = base.clone();
        m.sw(CONTEXT + 0x970, options);
        cases.push((
            format!("800db454 options {options:x}"),
            m,
            0x800d_b454,
            args,
        ));
    }
    for t in [0u32, 1, 24, 25, 26] {
        let mut m = base.clone();
        m.sw(block + 0x10, t);
        cases.push((format!("800db618 t {t}"), m.clone(), 0x800d_b618, args));
        cases.push((format!("800db8c8 t {t}"), m, 0x800d_b8c8, args));
    }
    // Grades and best times drawn at random for every mission (context
    // progress +0x300/+0x33C/+0x378 grade bytes, +0x3F0 flag bytes,
    // +0xA8/+0x120/+0x198/+0x288 time
    // halfwords), so each record's grade and time formats are drawn.
    let mut random = Lcg(0x0a3c_0007);
    for _ in 0..300 {
        let mut m = base.clone();
        for mission in 0..0x3cu32 {
            for grades in [0x300u32, 0x33c, 0x378] {
                m.sb(
                    CONTEXT + 0x960 + grades + mission,
                    random.pick(&[0u8, 1, 2, 3, 4, 5, 6]),
                );
            }
            m.sb(CONTEXT + 0x960 + 0x3f0 + mission, random.pick(&[0u8, 1]));
            for times in [0xa8u32, 0x120, 0x198, 0x288] {
                let time = random.pick(&[0u16, 59, 60, 3599, 3600, 35999, 36000, 0xffff]);
                m.sh(CONTEXT + 0x960 + times + 2 * mission, time);
            }
        }
        m.sw(block, random.pick(&[0u32, 1, 2, 3]));
        m.sh(block + 4, random.pick(&[0u16, 1, 2]));
        m.sh(block + 6, random.pick(&[1u16, 2, 5, 0x10, 0x24]));
        let t = random.pick(&[0u32, 7, 25]);
        cases.push((
            format!("800da9b8 random records t {t}"),
            m.clone(),
            0x800d_a9b8,
            [t, 0, 0, 0],
        ));
        cases.push(("800db6cc random records".to_owned(), m, 0x800d_b6cc, args));
    }
    let mut s = Sessions::default();
    differential_cases(&mut s, cases);
    eprintln!("{} cases agree, {} changed state", s.updates, s.effective);
    report(&base, &RECORDS, &s.covered);
    assert!(s.effective > 0);
}

/// The oracle itself: interpreting the original records drawing 800DA9B8
/// (text, sprintf, labels; no device calls) from its traced entries
/// reproduces every traced return exactly.
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/ov2air-012"]
fn oracle_reproduces_traced_records_drawing() {
    let trace = Trace::load("ov2air-012");
    for occurrence in 0..20 {
        let (mut m, args) = ov2air_trace::machine_at(&trace, 0x800d_a9b8, occurrence * 8);
        let mut log = Vec::new();
        ov2air_trace::oracle::interpret(&mut m, 0x800d_a9b8, args, &mut log);
        assert!(m.take_fault().is_ok());
        assert!(log.is_empty(), "no device calls expected");
        let (exit, _) = ov2air_trace::return_state(&trace, 0x800d_a9b8, occurrence * 8);
        assert_eq!(
            ov2air_trace::machine_differences(&m, &exit),
            0,
            "occurrence {}",
            occurrence * 8
        );
    }
}

/// Scripted input: `(update, bits)` taps set as pressed, repeat and held
/// bits of their update (0 otherwise).
fn taps(inputs: &[(usize, u32)]) -> impl FnMut(&mut Machine, usize) + '_ {
    move |m, update| {
        let bits = inputs
            .iter()
            .filter(|e| e.0 == update)
            .fold(0, |bits, e| bits | e.1);
        m.sw(CONTEXT + 0x128, bits);
        m.sw(CONTEXT + 0x12c, bits);
        m.sw(CONTEXT + 0x130, bits);
    }
}

/// Print the instruction coverage of `routines` in the original code the
/// oracle interpreted; returns the instructions never interpreted.
fn report(m: &Machine, routines: &[u32], covered: &BTreeSet<u32>) -> usize {
    let mut missed = 0;
    for (start, done, all, never) in oracle::coverage(m, routines, covered) {
        missed += never.len();
        eprintln!(
            "{start:08x}: {done}/{all} instructions interpreted{}{}",
            if never.is_empty() { "" } else { "; never: " },
            oracle::ranges(&never)
        );
    }
    missed
}

/// Accumulates scripted sessions.
#[derive(Default)]
struct Sessions {
    updates: usize,
    effective: usize,
    covered: BTreeSet<u32>,
}

impl Sessions {
    /// One scripted session of `update` from `m` (it must agree throughout).
    fn run(
        &mut self,
        name: &str,
        m: &Machine,
        (update, args): (u32, [u32; 4]),
        count: usize,
        host: oracle::Host,
        prepare: &mut dyn FnMut(&mut Machine, usize),
    ) -> oracle::Session {
        // Each update starts a frame: the scene's primitive pointer (+0x20094)
        // back at its first value, as the main loop's buffer swap would
        // leave it (otherwise the primitives grow over the scene block).
        let mut probe = m.clone();
        let scene = probe.lw(ov2_aircraft::SCENE);
        let start = probe.lw(scene + 0x2_0094);
        let mut frame = |m: &mut Machine, update: usize| {
            let scene = m.lw(ov2_aircraft::SCENE);
            m.sw(scene + 0x2_0094, start);
            prepare(m, update);
        };
        let session = oracle::updates(m, update, args, count, host, &mut frame)
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        eprintln!(
            "{name}: {} updates agree ({} changing state){}; results {}",
            session.updates,
            session.effective,
            session
                .fault
                .map_or(String::new(), |f| format!(", both ending on {f:?}")),
            compact(&session.results)
        );
        self.updates += session.updates;
        self.effective += session.effective;
        self.covered.extend(session.coverage.iter().copied());
        session
    }

    /// One call both ways.
    fn once(
        &mut self,
        name: &str,
        m: &Machine,
        routine: u32,
        args: [u32; 4],
        host: oracle::Host,
    ) -> oracle::Outcome {
        let outcome = oracle::compare(m, routine, args, !VOID.contains(&routine), host)
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        self.updates += 1;
        self.effective += usize::from(outcome.changed > 0);
        self.covered.extend(outcome.coverage.iter().copied());
        outcome
    }
}

/// Results as `value xcount` runs.
fn compact(results: &[u32]) -> String {
    let mut out: Vec<(u32, usize)> = Vec::new();
    for &r in results {
        match out.last_mut() {
            Some((v, n)) if *v == r => *n += 1,
            _ => out.push((r, 1)),
        }
    }
    out.iter()
        .map(|(v, n)| {
            if *n > 1 {
                format!("{v:#x}*{n}")
            } else {
                format!("{v:#x}")
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

const STATE_6: [u32; 8] = [
    0x800d_d754,
    0x800d_d918,
    0x800d_d2a0,
    0x800d_d300,
    0x800d_d38c,
    0x800d_d3b4,
    0x800d_d540,
    0x800d_d64c,
];

/// Inner state 6 (difficulty selection), which no capture reaches whole
/// (the original crashes in its entry's image upload after the perturbed
/// free-mission list of ov2air-021): from that entry, with the uploads
/// stubbed, for every starting difficulty, scripted sessions of the
/// update (the panel reveal flag scene +0x200E4 bit 0 set or not, the
/// difficulty stepped past both ends, confirm and cancel), each update
/// run both ways from the original's previous state.
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/ov2air-021"]
fn difficulty_screen_matches_the_original_code() {
    let trace = Trace::load("ov2air-021");
    let (entry, args) = ov2air_trace::machine_at(&trace, 0x800d_d754, 0);
    let (_, update_args) = ov2air_trace::machine_at(&trace, 0x800d_e3ec, 0);
    let mut s = Sessions::default();
    // (name, reveal flag set at, loader jumped at update 100 to its step 8,
    // 800CF2F4, which reports done (2) after ten updates; taps)
    type Script<'a> = (&'a str, Option<usize>, bool, &'a [(usize, u32)]);
    let scripts: [Script; 5] = [
        (
            "confirm",
            None,
            false,
            &[(20, 2), (30, 2), (40, 2), (50, 1), (60, 0x10)],
        ),
        ("cancel", None, false, &[(20, 1), (30, 1), (40, 0x20)]),
        ("revealed, confirm", Some(0), false, &[(40, 2), (60, 0x10)]),
        (
            "revealed late, up and down",
            Some(9),
            false,
            &[(30, 1), (31, 2), (32, 1), (50, 0x10)],
        ),
        (
            "confirm, loader finishing",
            None,
            true,
            &[(20, 2), (60, 0x10)],
        ),
    ];
    for difficulty in [0u32, 1, 2] {
        let mut m = entry.clone();
        m.sw(CONTEXT + 0x398, difficulty);
        let init = s.once("800dd754", &m, 0x800d_d754, args, &oracle::zero);
        for (name, reveal, finish, inputs) in scripts {
            let mut input = taps(inputs);
            let mut prepare = |m: &mut Machine, update: usize| {
                input(m, update);
                if Some(update) == reveal || (reveal.is_some() && update == 61) {
                    let scene = m.lw(ov2_aircraft::SCENE) + 0x2_00e4;
                    let flags = m.lw(scene);
                    m.sw(scene, flags | 1);
                }
                if finish && update == 100 {
                    let block = m.lw(ov2_misc::DIFFICULTY);
                    m.sw(block + 4, 8);
                }
            };
            s.run(
                &format!("difficulty {difficulty}, {name}"),
                &init.reference,
                (0x800d_d918, update_args),
                200,
                &oracle::zero,
                &mut prepare,
            );
        }
    }
    eprintln!("{} updates agree, {} changed state", s.updates, s.effective);
    report(&entry, &STATE_6, &s.covered);
    assert!(s.effective > 0);
}

const STATE_5: [u32; 8] = [
    0x800d_e354,
    0x800d_e3ec,
    0x800d_dbd0,
    0x800d_dc10,
    0x800d_dd2c,
    0x800d_de5c,
    0x800d_e2f4,
    0x800d_da70,
];

/// Inner state 5 (free-mission list) beyond ov2air-021's six cleared
/// missions: from its entry, for no, one, scattered and all 0x24 cleared
/// categories (at each difficulty table) and context missions inside and
/// outside the list, scripted sessions of the update: steps and pages
/// past both ends, confirm and cancel.
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/ov2air-021"]
fn free_mission_list_matches_the_original_code() {
    let trace = Trace::load("ov2air-021");
    let (entry, args) = ov2air_trace::machine_at(&trace, 0x800d_e354, 0);
    let (_, update_args) = ov2air_trace::machine_at(&trace, 0x800d_e3ec, 0);
    let mut s = Sessions::default();
    let cleared: [(&str, &[u32]); 4] = [
        ("none", &[]),
        ("one", &[0x10]),
        ("scattered", &[1, 2, 7, 0x10, 0x11, 0x23, 0x24]),
        ("all", &[]),
    ];
    let scripts: [(&str, &[(usize, u32)]); 4] = [
        (
            "steps, confirm",
            &[
                (30, 2),
                (31, 2),
                (32, 2),
                (33, 2),
                (34, 2),
                (40, 1),
                (41, 1),
                (60, 0x10),
            ],
        ),
        (
            "pages, cancel",
            &[
                (30, 0x200),
                (40, 0x200),
                (50, 0x200),
                (60, 0x100),
                (61, 0x100),
                (90, 0x20),
            ],
        ),
        (
            "past both ends",
            &[
                (30, 1),
                (31, 1),
                (40, 0x100),
                (50, 0x200),
                (51, 0x200),
                (52, 0x200),
                (53, 0x200),
                (54, 0x200),
                (55, 0x200),
                (56, 0x200),
                (57, 0x200),
                (58, 0x200),
                (59, 0x200),
                (70, 2),
                (71, 2),
                (80, 0x10),
            ],
        ),
        ("idle", &[]),
    ];
    for (table, difficulty) in [(0x300u32, 0), (0x33c, 1), (0x378, 2)] {
        for (set, missions) in cleared {
            for mission in [1u16, 0x10, 0x24, 0x30] {
                let mut m = entry.clone();
                for category in 1..=0x24u32 {
                    let on = set == "all" || missions.contains(&category);
                    for t in [0x300u32, 0x33c, 0x378] {
                        m.sb(CONTEXT + 0x960 + t + category, u8::from(on && t == table));
                    }
                }
                m.sh(CONTEXT + 0x390, mission);
                let init = s.once("800de354", &m, 0x800d_e354, args, &oracle::zero);
                for (name, inputs) in scripts {
                    s.run(
                        &format!(
                            "difficulty {difficulty}, {set} cleared, mission {mission:#x}: {name}"
                        ),
                        &init.reference,
                        (0x800d_e3ec, update_args),
                        120,
                        &oracle::zero,
                        &mut taps(inputs),
                    );
                }
            }
        }
    }
    eprintln!("{} updates agree, {} changed state", s.updates, s.effective);
    report(&entry, &STATE_5, &s.covered);
    assert!(s.effective > 0);
}

/// Scripted held buttons: `(first update, updates held, bits)`: pressed on
/// the first update, held throughout, repeating on the first and every
/// sixth update after it.
fn holds(inputs: &[(usize, usize, u32)]) -> impl FnMut(&mut Machine, usize) + '_ {
    move |m, update| {
        let (mut held, mut pressed, mut repeat) = (0, 0, 0);
        for &(first, length, bits) in inputs {
            if (first..first + length).contains(&update) {
                held |= bits;
                if update == first {
                    pressed |= bits;
                }
                if (update - first) % 6 == 0 {
                    repeat |= bits;
                }
            }
        }
        m.sw(CONTEXT + 0x128, held);
        m.sw(CONTEXT + 0x12c, pressed);
        m.sw(CONTEXT + 0x130, repeat);
    }
}

/// Host results of the settings' device calls: GetODE (the display field),
/// SpuReadDecodedData (the half `half`, its four 0x400-byte sample buffers
/// filled with a sawtooth of amplitude `amplitude`) and the SPU transfer.
fn settings_host(
    ode: u32,
    half: u32,
    amplitude: i32,
) -> impl Fn(&mut Machine, u32, [u32; 4]) -> u32 {
    move |m, key, a| match key {
        0x800a_73c0 => ode,
        0x8009_f358 => {
            for i in 0..0x800u32 {
                let sample = ((i * 37 % 200) as i32 - 100) * amplitude / 100;
                m.sh(a[0] + 2 * i, sample as u16);
            }
            half
        }
        0x8009_f7d8 => 1,
        _ => 0,
    }
}

/// Inner state 10's GAME CONFIG (mode 2) beyond the captures: scripted
/// sessions of the state's update 800DD214 (`ov2_options`, interpreted in both
/// runs) from the natural captures' states at the menu and at each item's
/// entry (colour bars, screen position, option rows and language from
/// ov2air-029, sound from ov2air-023): every direction tapped and held,
/// confirm and cancel at several moments, with GetODE answering both ways;
/// random walks over the whole screen with the display, option and audio
/// inputs varied; and [`settings_sweep`] from the same states.
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/ov2air-023 and -029"]
fn settings_match_the_original_code() {
    const UPDATE: u32 = 0x800d_d214;
    let traces: BTreeMap<&str, Trace> = ["ov2air-023", "ov2air-029"]
        .into_iter()
        .map(|name| (name, Trace::load(name)))
        .collect();
    let at = |capture: &str, frame: u64| {
        let trace = &traces[capture];
        let occurrence = ov2air_trace::occurrence_at(trace, UPDATE, frame);
        ov2air_trace::machine_at(trace, UPDATE, occurrence)
    };
    type Script<'a> = (&'a str, &'a [(usize, usize, u32)]);
    let scripts: [Script; 9] = [
        ("idle", &[]),
        (
            "taps",
            &[
                (20, 1, 1),
                (30, 1, 2),
                (40, 1, 4),
                (50, 1, 8),
                (60, 1, 2),
                (70, 1, 1),
            ],
        ),
        ("held right, left", &[(20, 40, 8), (70, 40, 4)]),
        ("held down, up", &[(20, 40, 2), (70, 40, 1)]),
        ("held diagonals", &[(20, 30, 0xa), (60, 30, 5)]),
        ("confirm", &[(30, 1, 8), (40, 1, 0x10), (90, 1, 0x10)]),
        ("cancel", &[(30, 1, 4), (40, 1, 0x20), (90, 1, 0x20)]),
        (
            "down, confirm, cancel",
            &[(25, 1, 2), (35, 1, 2), (45, 1, 0x10), (100, 1, 0x20)],
        ),
        ("both", &[(40, 1, 0x30), (80, 1, 0x30)]),
    ];
    let starts: [(&str, &str, u64); 7] = [
        ("menu", "ov2air-029", 187),
        ("colour bars", "ov2air-029", 266),
        ("screen position", "ov2air-029", 496),
        ("option rows", "ov2air-029", 746),
        ("language", "ov2air-029", 1006),
        ("sound", "ov2air-023", 326),
        ("menu entry", "ov2air-029", 150),
    ];
    let mut s = Sessions::default();
    for (start, capture, frame) in starts {
        let (base, args) = at(capture, frame);
        for (name, inputs) in scripts {
            for ode in [0u32, 1] {
                let host = settings_host(ode, 0, 50);
                s.run(
                    &format!("{start} ({capture} frame {frame}), {name}, GetODE {ode}"),
                    &base,
                    (UPDATE, args),
                    160,
                    &host,
                    &mut holds(inputs),
                );
            }
        }
    }
    // Random walks through the whole screen from the menu and each item:
    // a button every 5 updates (mostly directions, short holds), confirm
    // and cancel now and then, so items are left, others entered and
    // their values changed and committed.
    let mut random = Lcg(0x0a3c_0010);
    for (start, capture, frame) in &starts[..6] {
        let (base, args) = at(capture, *frame);
        for walk in 0..12u32 {
            let inputs: Vec<(usize, usize, u32)> = (15..400)
                .step_by(5)
                .map(|t| {
                    let bits = random.pick(&[0u32, 1, 2, 4, 8, 1, 2, 4, 8, 0, 0x10, 0x20, 0xa, 5]);
                    (t, random.pick(&[1usize, 1, 1, 3, 12]), bits)
                })
                .collect();
            // Display and option flags (context +0x124 bit 0x10, +0x120 bits
            // 0/1) and the decoded audio vary with the walk.
            let mut m = base.clone();
            let flags = m.lw(CONTEXT + 0x124);
            m.sw(CONTEXT + 0x124, (flags & !0x10) | (walk & 4) << 2);
            let flags = m.lw(CONTEXT + 0x120);
            m.sw(CONTEXT + 0x120, (flags & !3) | [0, 1, 3][walk as usize % 3]);
            let amplitude = [0, 50, 32767][(walk as usize / 3) % 3];
            let host = settings_host(walk & 1, (walk >> 1) & 1, amplitude);
            s.run(
                &format!("{start} ({capture} frame {frame}), random walk {walk}"),
                &m,
                (UPDATE, args),
                400,
                &host,
                &mut holds(&inputs),
            );
        }
    }
    let bases: Vec<Machine> = starts
        .iter()
        .map(|&(_, capture, frame)| at(capture, frame).0)
        .collect();
    let items = [at("ov2air-023", 340).0, at("ov2air-029", 760).0];
    let calls = settings_sweep(&mut s, &bases, &items);
    eprintln!("sweep: {calls} calls agree");
    eprintln!("{} updates agree, {} changed state", s.updates, s.effective);
    let (base, _) = at("ov2air-029", 187);
    let state_10: Vec<u32> = ov2_misc::ROUTINES
        .iter()
        .copied()
        .filter(|&r| (0x800e_4978..0x800e_7500).contains(&r))
        .chain([0x800d_0d30, 0x800d_0db8])
        .collect();
    report(&base, &state_10, &s.covered);
    assert!(s.effective > 0);
}

/// A small deterministic generator for the random walks.
struct Lcg(u64);

impl Lcg {
    fn pick<T: Copy>(&mut self, from: &[T]) -> T {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        from[((self.0 >> 33) as usize) % from.len()]
    }
}

/// The settings' routines called directly from `bases` (natural states of
/// the settings) with their globals drawn at random, out-of-range phases
/// and negative timers included (the menu, item and sound globals from
/// 0x800F97F8, the state block's timer, the context's language, option,
/// display and pad fields, the screen offset), random input and device
/// answers. Each call must agree with the original code; returns the
/// calls made. Then every value step of the sound and option rows from
/// `items` (the sound and option-row updates running) at both ends.
fn settings_sweep(s: &mut Sessions, bases: &[Machine], items: &[Machine; 2]) -> usize {
    // (routine, argument choices)
    let routines: [(u32, &[u32]); 23] = [
        (0x800e_4978, &[0]),
        (0x800e_49dc, &[0]),
        (0x800e_4b10, &[0]),
        (0x800e_4b44, &[0]),
        (0x800e_4cf0, &[0]),
        (0x800e_4d6c, &[0]),
        (0x800e_5288, &[0]),
        (0x800e_5328, &[0]),
        (0x800e_53c8, &[0]),
        (0x800e_594c, &[0]),
        (0x800e_59e8, &[0]),
        (0x800e_5c98, &[0]),
        (0x800e_5d50, &[0]),
        (0x800e_5ecc, &[0]),
        (0x800e_62c0, &[0]),
        (0x800e_6378, &[0, 1]),
        (0x800e_6504, &[0]),
        (0x800e_66a0, &[0]),
        (0x800e_6d44, &[0]),
        (0x800e_6dfc, &[0]),
        (0x800e_6ec0, &[0, 1, 2]),
        (0x800e_72c4, &[0]),
        (0x800e_73dc, &[0]),
    ];
    const SMALL: &[u32] = &[0, 1, 2, 3, 4, 5, 6, 7, 8, u32::MAX];
    const TIMER: &[u32] = &[
        u32::MAX - 1,
        u32::MAX,
        0,
        1,
        0x10,
        0x11,
        0x12,
        0x16,
        24,
        25,
        26,
    ];
    let words: [(u32, &[u32]); 21] = [
        (0x800f_97f8, SMALL),
        (0x800f_97fc, TIMER),
        // Indices (item step and cursor, language cursor and entry, option
        // and sound rows) only within their ranges: out of range they read
        // past their tables or stack copies (uninitialized stack words).
        (0x800f_9800, &[0, 1]),
        (0x800f_9804, &[0, 1]),
        (0x800f_9808, &[0, 1, 2, 3, 4]),
        (0x800f_9810, SMALL),
        (0x800f_9820, SMALL),
        (0x800f_9828, &[0, 5, 6, 12, 13]),
        (0x800f_982c, &[0, 1, 2, 3, 4]),
        (0x800f_9830, &[0, 1, 2, 3, 4]),
        (0x800f_9834, SMALL),
        (0x800f_9838, &[0, 5, 6, 12, 13]),
        (0x800f_985c, &[0, 1, 2]),
        (0x800f_9864, SMALL),
        (0x800f_9870, &[0, 0x30, 0x61, 0x62, 0x63, 0x64]),
        (0x800f_987c, SMALL),
        (0x800f_9884, &[0, 1]),
        (0x800f_9888, &[0, 1, 2]),
        (CONTEXT + 0x38c, &[0, 1, 2, 3, 4]),
        (CONTEXT + 0x124, &[0, 0x10]),
        (CONTEXT + 0x120, &[0, 1, 2, 3]),
    ];
    let bytes: [(u32, &[u8]); 11] = [
        (0x800f_983c, &[0, 1, 2]),
        (0x800f_983d, &[0, 1, 2]),
        (0x800f_9858, &[0, 1, 2]),
        (0x800f_9860, &[0, 1, 2, 3]),
        (0x800f_9861, &[0, 1, 2]),
        (0x800f_9868, &[0, 1, 2, 3]),
        (0x800f_988c, &[0, 1, 0x7f, 0x80]),
        (0x800f_988d, &[0, 1, 0x7f, 0x80]),
        (0x800f_988e, &[0, 1, 2]),
        (CONTEXT + 0x3a0, &[0, 1, 2]),
        (CONTEXT + 0x95e, &[0, 1, 2]),
    ];
    let mut random = Lcg(0x0a3c_0011);
    let mut calls = 0;
    for base in bases {
        for (routine, choices) in routines {
            // The two item updates with the most branches get more draws.
            let draws = if [0x800e_66a0, 0x800e_73dc].contains(&routine) {
                400
            } else {
                40
            };
            for _ in 0..draws {
                let mut m = base.clone();
                let mut set = Vec::new();
                for (at, values) in words {
                    if random.pick(&[false, true]) {
                        let value = random.pick(values);
                        m.sw(at, value);
                        set.push(format!("{at:08x}={value:x}"));
                    }
                }
                for (at, values) in bytes {
                    if random.pick(&[false, true]) {
                        let value = random.pick(values);
                        m.sb(at, value);
                        set.push(format!("{at:08x}={value:x}"));
                    }
                }
                for at in [CONTEXT + 0x39c, CONTEXT + 0x39d, CONTEXT + 0x39e] {
                    m.sb(
                        at,
                        random.pick(&[0u8, 1, 8, 0x10, 0x11, 0x7f, 0x80, 0xef, 0xf0, 0xf5, 0xff]),
                    );
                }
                let block = m.lw(0x800f_6060);
                m.sh(block + 6, random.pick(&[0u16, 0x11, 0x12, 0x13]));
                let display = m.lw(0x800b_acb8);
                m.sw(
                    display + 8,
                    random.pick(&[u32::MAX - 2, u32::MAX - 1, 0, 0x10, 0x11]),
                );
                m.sw(display + 0xc, random.pick(&[u32::MAX - 1, 0, 0x10, 0x11]));
                m.sh(CONTEXT + 0x60, random.pick(&[0u16, 0x2000]));
                m.sh(CONTEXT + 0x68, random.pick(&[0x70u16, 0x80, 0x81, 0x90]));
                let held = random.pick(&[0u32, 1, 2, 4, 8, 0xa, 5]);
                m.sw(CONTEXT + 0x128, held);
                m.sw(
                    CONTEXT + 0x12c,
                    random.pick(&[0u32, held, 0x10, 0x20, 0x30, 0x40]),
                );
                m.sw(CONTEXT + 0x130, random.pick(&[0u32, held]));
                let host = settings_host(
                    random.pick(&[0, 1]),
                    random.pick(&[0, 1]),
                    random.pick(&[0, 50, 32767]),
                );
                let arg = random.pick(choices);
                let name = format!("{routine:08x}({arg}) with {}", set.join(" "));
                s.once(&name, &m, routine, [arg, 0, 0, 0], &host);
                calls += 1;
            }
        }
    }
    let [sound, options] = items;
    for row in 0..3u32 {
        for bits in [4u32, 8] {
            for value in [0u8, 1, 8, 0x10, 0x11, 0xef, 0xf0, 0xf5, 0xff] {
                let mut m = sound.clone();
                m.sw(0x800f_987c, 3);
                m.sw(0x800f_9888, row);
                for at in [CONTEXT + 0x39c, CONTEXT + 0x39d, CONTEXT + 0x39e] {
                    m.sb(at, value);
                }
                for at in [0x128u32, 0x12c, 0x130] {
                    m.sw(CONTEXT + at, bits);
                }
                let name = format!("800e73dc row {row} value {value:#x} bits {bits:x}");
                s.once(&name, &m, 0x800e_73dc, [0; 4], &oracle::zero);
                calls += 1;
            }
            for value in 0..3u8 {
                let mut m = options.clone();
                m.sw(0x800f_9864, 3);
                m.sw(0x800f_985c, row);
                for at in [0x800f_9858u32, 0x800f_9861, 0x800f_9868] {
                    m.sb(at, value);
                }
                for at in [0x128u32, 0x12c, 0x130] {
                    m.sw(CONTEXT + at, bits);
                }
                let name = format!("800e66a0 row {row} value {value} bits {bits:x}");
                s.once(&name, &m, 0x800e_66a0, [0; 4], &oracle::zero);
                calls += 1;
            }
        }
    }
    calls
}

const RECORDS: [u32; 5] = [
    0x800d_a9b8,
    0x800d_b454,
    0x800d_b618,
    0x800d_b6cc,
    0x800d_b8c8,
];

const STATE_15: [u32; 2] = [0x800d_3534, 0x800d_3584];

/// Inner state 15 (the reload) beyond ov2air-022: from its entry, for
/// every language, with and without the free-mission option (context
/// +0x970 bit 0x10), sessions of the update through the slide-in, with the
/// loader phases left running and jumped to their last row (800CF350,
/// which reports done, 4).
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/ov2air-022"]
fn reload_matches_the_original_code() {
    let trace = Trace::load("ov2air-022");
    let (entry, args) = ov2air_trace::machine_at(&trace, 0x800d_3534, 0);
    let (_, update_args) = ov2air_trace::machine_at(&trace, 0x800d_3584, 0);
    let mut s = Sessions::default();
    for language in 0..5u32 {
        for option in [0u32, 0x10] {
            for finish in [None, Some(5usize), Some(40)] {
                let mut m = entry.clone();
                m.sw(CONTEXT + 0x38c, language);
                let options = m.lw(CONTEXT + 0x970);
                m.sw(CONTEXT + 0x970, (options & !0x10) | option);
                let init = s.once("800d3534", &m, 0x800d_3534, args, &oracle::zero);
                let mut prepare = |m: &mut Machine, update: usize| {
                    if Some(update) == finish {
                        let block = m.lw(ov2_misc::RELOAD);
                        m.sh(block, 4);
                        m.sh(block + 2, 0);
                    }
                };
                s.run(
                    &format!("language {language}, option {option:#x}, finish at {finish:?}"),
                    &init.reference,
                    (0x800d_3584, update_args),
                    60,
                    &oracle::zero,
                    &mut prepare,
                );
            }
        }
    }
    eprintln!("{} updates agree, {} changed state", s.updates, s.effective);
    report(&entry, &STATE_15, &s.covered);
    assert!(s.effective > 0);
}
