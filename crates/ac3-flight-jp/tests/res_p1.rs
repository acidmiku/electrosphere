//! Resident part 1 of the JP translation wave (agent `jp-res1`): the patch
//! layer's routines whose PAL counterparts live in `frame`, `init_tail`,
//! `player`, `replay`, `session`, `shared` and `states`
//! (`src/jp/res_p1_<module>.rs`, `src/jp/mod.rs` REPLACES), and the render
//! routine 80033324 the flight frame calls (`src/jp/res_p1_render.rs`).
//!
//! * Captured calls: every JP Redux entry/return pair of a patched routine
//!   (`~/ac3-jp/analysis/redux/<run>/snapshots-module0.jsonl`) runs through
//!   `dispatch::call` (so the REPLACES wiring is exercised) and must
//!   reproduce the return snapshot: RAM outside the stack, scratchpad, GTE
//!   and `v0`. With `dev-fallback`, callees owned by other parts can be
//!   forced to run interpreted (the original instructions), so a patch is
//!   judged alone while those parts are still open.
//! * Oracle cases: the original JP instructions (`support::oracle`) and the
//!   patch run from the same JP machine state (a captured entry, with
//!   varied fields and arguments) and must agree.
//!
//! Every case judges the part's patches alone, with every other game
//! routine interpreted ([`isolation`]); that needs the development fallback,
//! so the file is built with `--features dev-fallback` only (without it the
//! other parts' open routines would decide the results).
#![cfg(feature = "dev-fallback")]
mod support;

use ac3_flight_jp::{
    Machine, dispatch,
    snapshot::{self, Snapshot, SnapshotInfo},
};
use std::path::PathBuf;
use support::{oracle, scan};

// ---- captures -------------------------------------------------------------------------

fn run_dir(run: &str) -> PathBuf {
    scan::jp_dir().join("analysis/redux").join(run)
}

/// The `occurrence`th captured call of `callee` in `run` (module index
/// `module`): its entry and return snapshots, paired like `check_call`.
fn pair(run: &str, module: u32, callee: u32, occurrence: usize) -> (Snapshot, Snapshot) {
    let directory = run_dir(run);
    let index_file = directory.join(format!("snapshots-module{module}.jsonl"));
    let index = snapshot::read_index(&index_file)
        .unwrap_or_else(|e| panic!("{}: {e}", index_file.display()));
    let entry: &SnapshotInfo = index
        .iter()
        .filter(|s| s.callee == callee && s.stage == "entry")
        .nth(occurrence)
        .unwrap_or_else(|| panic!("{run}: {callee:08x}: no entry #{occurrence}"));
    let exit = index
        .iter()
        .find(|s| {
            s.callee == callee
                && s.stage == "return"
                && s.sequence > entry.sequence
                && s.ra == entry.ra
                && s.registers.gpr[29] == entry.registers.gpr[29]
        })
        .unwrap_or_else(|| panic!("{run}: {callee:08x}: no return"));
    (
        Snapshot::load(&directory, entry).expect("entry image"),
        Snapshot::load(&directory, exit).expect("return image"),
    )
}

/// Differing addresses as ranges, for reports.
fn ranges(addresses: &[u32]) -> String {
    let mut out = Vec::new();
    let mut i = 0;
    while i < addresses.len() {
        let start = addresses[i];
        let mut end = start;
        while i + 1 < addresses.len() && addresses[i + 1] <= end + 4 {
            i += 1;
            end = addresses[i];
        }
        out.push(if start == end {
            format!("{start:08x}")
        } else {
            format!("{start:08x}..={end:08x}")
        });
        i += 1;
    }
    out.join(" ")
}

/// Run `callee` through `dispatch::call` from `entry`: the machine after
/// it and `v0`.
fn run_call(entry: &Snapshot) -> (Machine, u32) {
    let mut m = entry.machine();
    let args = [entry.gpr(4), entry.gpr(5), entry.gpr(6), entry.gpr(7)];
    let v0 = dispatch::call(&mut m, entry.info.callee, args);
    (m, v0)
}

/// Run `callee` through `dispatch::call` from `entry` and compare with
/// `exit`. `void`: the translation returns no `v0` the original defines.
fn replay(entry: &Snapshot, exit: &Snapshot, void: bool) -> Result<usize, String> {
    let (mut m, v0) = run_call(entry);
    if let Err(fault) = m.take_fault() {
        return Err(format!("fault: {fault}"));
    }
    let d = snapshot::compare(&m, exit);
    if !d.is_empty() {
        return Err(format!(
            "ram {} [{}] scratch [{}] gte data {:?} control {:?}",
            d.ram.len(),
            ranges(&d.ram),
            ranges(&d.scratch),
            d.gte_data,
            d.gte_control
        ));
    }
    if !void && v0 != exit.gpr(2) {
        return Err(format!("v0 {v0:#010x}, original {:#010x}", exit.gpr(2)));
    }
    Ok(snapshot::changed_bytes(entry, exit))
}

/// Install a fallback forcing `spec` (empty: none) for the duration of `f`.
fn forced<T>(spec: &str, f: impl FnOnce() -> T) -> T {
    let mut fallback = ac3_fallback::Fallback::new(ac3_fallback::Version::Jp);
    if !spec.is_empty() {
        fallback.force(spec).expect("force spec");
    }
    ac3_fallback::install(fallback);
    let out = f();
    ac3_fallback::uninstall();
    out
}

/// [`forced`], also returning the interpreted instructions and, for each
/// address of `count`, its interpreted calls.
fn forced_counting<T>(spec: &str, count: &[u32], f: impl FnOnce() -> T) -> (T, u64, Vec<u64>) {
    let mut fallback = ac3_fallback::Fallback::new(ac3_fallback::Version::Jp);
    if !spec.is_empty() {
        fallback.force(spec).expect("force spec");
    }
    let fallback = ac3_fallback::install(fallback);
    let out = f();
    ac3_fallback::uninstall();
    let calls = count
        .iter()
        .map(|&a| fallback.reached(a).map_or(0, |(c, _)| c))
        .collect();
    (out, fallback.instructions(), calls)
}

/// The modules of resident part 1 (their patches are
/// `src/jp/res_p1_<module>.rs`).
const MODULES: [&str; 7] = [
    "frame",
    "init_tail",
    "player",
    "replay",
    "session",
    "shared",
    "states",
];

/// The part's patches in other modules: `render::draw_lit_systems`
/// (80033324, `res_p1_render.rs`), which the flight frame calls.
const ALSO_PATCHED: [u32; 1] = [0x8003_3324];

/// The part's dispatcher claims (TAKES_OVER): the two ray routines.
const TAKEN_OVER: [u32; 2] = [0x8009_9894, 0x8009_a944];

/// The part's patched routines, from the patch layer's own REPLACES table,
/// and its dispatcher claims.
fn patched() -> Vec<u32> {
    let mut list: Vec<u32> = ac3_flight_jp::jp::REPLACES
        .iter()
        .filter(|(_, path)| MODULES.iter().any(|m| path.split("::").next() == Some(*m)))
        .map(|&(address, _)| address)
        .collect();
    for address in TAKEN_OVER {
        if ac3_flight_jp::jp::TAKES_OVER.contains(&address) && !list.contains(&address) {
            list.push(address);
        }
    }
    for address in ALSO_PATCHED {
        if !list.contains(&address) {
            list.push(address);
        }
    }
    list
}

/// A force spec that runs every game routine interpreted (the original
/// instructions) except the part's patched routines, minus `except` too:
/// a patch is then judged alone, whatever the other parts' state.
fn isolation(except: Option<u32>) -> String {
    let mut spec = String::from("game");
    for address in patched() {
        if Some(address) != except {
            spec.push_str(&format!(",-{address:x}"));
        }
    }
    spec
}

/// Run `f`, turning a panic (an unmapped site, a PAL-only body) into Err.
fn guarded<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    std::panic::set_hook(hook);
    out.unwrap_or_else(|e| {
        let text = e
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_default();
        Err(format!("panic: {text}"))
    })
}

/// The patch against the original instructions from the same captured
/// entry, both with the other game routines interpreted ([`isolation`]):
/// Ok when RAM (outside the stack), scratchpad, GTE, fault and `v0` agree.
/// This decides a capture the harness cannot reproduce byte for byte
/// because interrupt handlers ran inside the captured call (BIOS RAM,
/// libcd/libpress driver state): the patch must then leave exactly the
/// state the original instructions leave.
fn same_as_original(entry: &Snapshot, void: bool) -> Result<(), String> {
    let callee = entry.info.callee;
    let (mut native, nv0) = guarded(|| Ok(forced(&isolation(None), || run_call(entry))))?;
    let (mut original, ov0) = guarded(|| Ok(forced(&isolation(Some(callee)), || run_call(entry))))?;
    let (nf, of) = (native.take_fault(), original.take_fault());
    if nf != of {
        return Err(format!("faults: native {nf:?}, original {of:?}"));
    }
    let ram: Vec<u32> = (0..snapshot::STACK_START)
        .filter(|&i| native.ram()[i] != original.ram()[i])
        .map(|i| 0x8000_0000 + i as u32)
        .collect();
    let scratch: Vec<u32> = (0..0x400)
        .filter(|&i| native.scratch()[i] != original.scratch()[i])
        .map(|i| 0x1f80_0000 + i as u32)
        .collect();
    if !ram.is_empty() || !scratch.is_empty() || native.gte != original.gte {
        return Err(format!(
            "differs from the original: ram [{}] scratch [{}] gte equal {}",
            ranges(&ram),
            ranges(&scratch),
            native.gte == original.gte
        ));
    }
    if !void && nv0 != ov0 {
        return Err(format!("v0 {nv0:#x}, original {ov0:#x}"));
    }
    Ok(())
}

/// One captured call of a patched routine: (run, callee, occurrence, void).
struct Captured {
    run: &'static str,
    callee: u32,
    occurrence: usize,
    void: bool,
}

const fn cap(run: &'static str, callee: u32, void: bool) -> Captured {
    Captured {
        run,
        callee,
        occurrence: 0,
        void,
    }
}

/// Every captured call of the part's patched routines.
const CAPTURED: &[Captured] = &[
    // shared
    cap("flight-002", 0x8002_f70c, false),
    cap("flight-002", 0x8003_a5c0, false),
    cap("flight-002", 0x8003_a62c, false),
    cap("flight-002", 0x8004_63b8, false),
    // states (story-002: the intro movie)
    cap("story-002", 0x8005_7cd0, false),
    cap("story-002", 0x8005_7ebc, false),
    cap("story-002", 0x8005_81c4, false),
    cap("story-002", 0x8005_7720, false),
    // the module loader (boot-002: the first overlay load)
    cap("boot-002", 0x8009_b6b8, false),
    cap("boot-002", 0x8009_b7cc, false),
    // init_tail
    cap("flight-002", 0x8003_5740, true),
    cap("flight-002", 0x8003_2518, true),
    cap("flight-002", 0x8003_2080, false),
    cap("flight-002", 0x8004_e620, true),
    // frame
    cap("flight-002", 0x8003_39e0, true),
    cap("flight-002", 0x8003_3dc0, false),
    cap("flight-002", 0x8003_4040, false),
    // player
    cap("flight-002", 0x8007_29dc, true),
    cap("flight-002", 0x8006_ef30, true),
    cap("flight-002", 0x8007_2178, true),
    cap("flight-002", 0x8004_6124, false),
    cap("flight-002", 0x8005_1414, true),
    cap("flight-002", 0x8005_1840, true),
    cap("flight-002", 0x8005_157c, true),
    cap("flight-002", 0x8004_658c, true),
    cap("flight-002", 0x8006_d0dc, true),
    cap("flight-002", 0x8004_8658, true),
    cap("flight-002", 0x8004_5a40, true),
    cap("flight-002", 0x8009_9894, false),
    // replay and session
    cap("flight-002", 0x8007_f8e4, true),
    cap("flight-002", 0x8007_f448, true),
    cap("flight-002", 0x8007_f5b4, false),
    cap("flight-002", 0x8007_f808, true),
    cap("flight-002", 0x8008_4560, false),
    cap("flight-002", 0x8007_953c, true),
    cap("flight-002", 0x8003_4718, true),
    cap("flight-002", 0x8003_43c8, false),
    cap("flight-002", 0x8003_71c4, false),
    cap("flight-002", 0x8005_40e0, true),
    cap("flight-002", 0x8005_628c, true),
    cap("flight-002", 0x8005_6518, false),
];

/// Diagnosis (`RES_P1_DIAG=run:callee[:occurrence]`): the patch alone
/// ([`isolation`]), the original instructions alone, and the plain native
/// run (the other parts as they are) replay the capture, and with
/// `RES_P1_DIAG_FORCE=<spec>` a run forcing that fallback spec; all reports
/// are printed in full.
#[test]
#[ignore = "requires AC3_JP_DIR and RES_P1_DIAG"]
fn diagnose() {
    let Ok(spec) = std::env::var("RES_P1_DIAG") else {
        return;
    };
    let parts: Vec<&str> = spec.split(':').collect();
    let callee = u32::from_str_radix(parts[1], 16).unwrap();
    let occurrence = parts.get(2).map_or(0, |o| o.parse().unwrap());
    let (entry, exit) = pair(parts[0], 0, callee, occurrence);
    println!(
        "{spec}: entry seq {} frame {}, a0..a3 {:08x?}, {} bytes changed",
        entry.info.sequence,
        entry.info.frame,
        [entry.gpr(4), entry.gpr(5), entry.gpr(6), entry.gpr(7)],
        snapshot::changed_bytes(&entry, &exit)
    );
    let native = guarded(|| forced(&isolation(None), || replay(&entry, &exit, false)));
    println!("patch alone: {native:?}");
    let original = guarded(|| forced(&isolation(Some(callee)), || replay(&entry, &exit, false)));
    println!("original instructions: {original:?}");
    let plain = guarded(|| forced("", || replay(&entry, &exit, false)));
    println!("plain native: {plain:?}");
    if let Ok(force) = std::env::var("RES_P1_DIAG_FORCE") {
        let custom = guarded(|| forced(&force, || replay(&entry, &exit, false)));
        println!("forcing {force}: {custom:?}");
    }
}

#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn captured_calls_reproduce() {
    let mut failures = Vec::new();
    for c in CAPTURED {
        let (entry, exit) = pair(c.run, 0, c.callee, c.occurrence);
        let label = format!("{} {:08x} #{}", c.run, c.callee, c.occurrence);
        let plain = guarded(|| forced("", || replay(&entry, &exit, c.void)));
        let alone = guarded(|| forced(&isolation(None), || replay(&entry, &exit, c.void)));
        let plain = match plain {
            Ok(_) => "plain native run passes too".to_string(),
            Err(e) => format!("plain native run: {e}"),
        };
        match alone {
            Ok(n) => println!("{label}: pass, {n} bytes changed ({plain})"),
            Err(e) => match same_as_original(&entry, c.void) {
                Ok(()) => println!(
                    "{label}: pass against the original instructions ({} bytes changed); \
                     the original instructions leave the same capture differences: {e} \
                     ({plain})",
                    snapshot::changed_bytes(&entry, &exit)
                ),
                Err(o) => {
                    println!("{label}: {e}; {o}");
                    failures.push(format!("{label}: {e}; {o}"));
                }
            },
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

// ---- whole flight frames ------------------------------------------------------------------

/// The flight run's main-loop tops (`flight-001/loop-snapshots.jsonl`, the
/// loop top 80025BC8 of 53 flight frames): (index, field, machine).
fn loop_tops() -> Vec<(u64, u64, Machine)> {
    let directory = run_dir("flight-001");
    let index =
        std::fs::read_to_string(directory.join("loop-snapshots.jsonl")).expect("loop index");
    let words = |value: &serde_json::Value| -> [u32; 32] {
        std::array::from_fn(|i| value[i].as_u64().expect("register") as u32)
    };
    index
        .lines()
        .map(|line| {
            let top: serde_json::Value = serde_json::from_str(line).expect("loop top");
            let stem = top["stem"].as_str().expect("stem");
            let ram = std::fs::read(directory.join(format!("{stem}.ram.bin"))).expect("ram");
            let scratch =
                std::fs::read(directory.join(format!("{stem}.scratch.bin"))).expect("scratch");
            let mut m = Machine::from_images(&ram, &scratch).expect("images");
            m.gte.data = words(&top["cp2d"]);
            m.gte.control = words(&top["cp2c"]);
            m.sp = words(&top["gpr"])[29];
            (
                top["index"].as_u64().unwrap(),
                top["frame"].as_u64().unwrap(),
                m,
            )
        })
        .collect()
}

/// One main-loop pass from its top up to and including the state callback
/// (the flight frame), every routine called through `dispatch::call` so a
/// force spec decides how it runs: 80025BC8 display requests, 80026050
/// buffer swap, 80026E84 pads, 80028C24 pad mapping, 8002E1F4 sound,
/// 80026350 the state dispatcher (flight callback 80033EF0).
fn flight_frame(m: &mut Machine) {
    for routine in [0x8002_5bc8, 0x8002_6050, 0x8002_6e84] {
        dispatch::call(m, routine, [0; 4]);
    }
    let context = m.lw(CONTEXT);
    dispatch::call(
        m,
        0x8002_8c24,
        [context + 0x50, context + 0x120, context + 0x15c, 0],
    );
    dispatch::call(m, 0x8002_e1f4, [0; 4]);
    dispatch::call(m, 0x8002_6350, [0; 4]);
}

/// Differences between two machines after a frame from the same state.
fn frame_differences(a: &Machine, b: &Machine, sp: u32) -> Option<String> {
    let high = (sp & 0x1f_ffff) as usize;
    let ram: Vec<u32> = (0..0x20_0000usize)
        .filter(|&i| !(snapshot::STACK_START..high).contains(&i) && a.ram()[i] != b.ram()[i])
        .map(|i| 0x8000_0000 + i as u32)
        .collect();
    let scratch: Vec<u32> = (0..0x400)
        .filter(|&i| a.scratch()[i] != b.scratch()[i])
        .map(|i| 0x1f80_0000 + i as u32)
        .collect();
    let data: Vec<usize> = (0..32)
        .filter(|&i| a.gte.data[i] != b.gte.data[i])
        .collect();
    let control: Vec<usize> = (0..32)
        .filter(|&i| a.gte.control[i] != b.gte.control[i])
        .collect();
    if ram.is_empty() && scratch.is_empty() && data.is_empty() && control.is_empty() {
        return None;
    }
    Some(format!(
        "ram {} [{}] scratch [{}] gte data {data:?} control {control:?}",
        ram.len(),
        ranges(&ram[..ram.len().min(64)]),
        ranges(&scratch)
    ))
}

/// The 53 flight frames of `flight-001`, each from its captured loop top:
/// the original instructions (every game routine interpreted) against the
/// part's patches alone ([`isolation`]: every other game routine
/// interpreted), so each patched routine the frame reaches runs on real
/// flight state, as often as the frame calls it. Also reported, not
/// asserted: the plain native crate (all translations native, routines
/// nobody translated interpreted) against the original.
#[test]
#[ignore = "requires AC3_JP_DIR (the JP flight-001 loop tops)"]
fn flight_frames_match_the_original() {
    let tops = loop_tops();
    let patched = patched();
    let mut reached = vec![0u64; patched.len()];
    let (mut alone_equal, mut plain_equal) = (0, 0);
    let mut failures = Vec::new();
    for (index, field, base) in &tops {
        let sp = base.sp;
        let run = |spec: &str| {
            let mut m = base.clone();
            let mut counts = (0, Vec::new());
            let outcome = guarded(|| {
                let ((), instructions, calls) =
                    forced_counting(spec, &patched, || flight_frame(&mut m));
                counts = (instructions, calls);
                Ok(())
            });
            let fault = m.take_fault();
            (m, outcome, fault, counts)
        };
        let (original, o_outcome, o_fault, (o_instructions, o_calls)) = run("game");
        assert!(o_outcome.is_ok(), "loop {index}: original: {o_outcome:?}");
        assert!(
            o_instructions > 10_000,
            "loop {index}: the original ran natively"
        );
        for (sum, calls) in reached.iter_mut().zip(&o_calls) {
            *sum += calls;
        }
        let (alone, a_outcome, a_fault, (a_instructions, _)) = run(&isolation(None));
        let alone_result = match (&a_outcome, a_fault == o_fault) {
            (Err(e), _) => Some(e.clone()),
            (Ok(()), false) => Some(format!("fault {a_fault:?}, original {o_fault:?}")),
            _ => frame_differences(&original, &alone, sp),
        };
        let (plain, p_outcome, p_fault, _) = run("");
        let plain_result = match (&p_outcome, p_fault == o_fault) {
            (Err(e), _) => Some(e.clone()),
            (Ok(()), false) => Some(format!("fault {p_fault:?}, original {o_fault:?}")),
            _ => frame_differences(&original, &plain, sp),
        };
        let changed = (0..0x20_0000usize)
            .filter(|&i| original.ram()[i] != base.ram()[i])
            .count();
        if alone_result.is_none() {
            alone_equal += 1;
        }
        if plain_result.is_none() {
            plain_equal += 1;
        }
        println!(
            "loop {index} (field {field}, {changed} bytes changed, {o_instructions} / \
             {a_instructions} instructions interpreted): patches alone {}; plain native {}",
            alone_result.as_deref().unwrap_or("equal"),
            plain_result.as_deref().unwrap_or("equal")
        );
        if let Some(e) = alone_result {
            failures.push(format!("loop {index}: {e}"));
        }
    }
    println!(
        "flight frames: patches alone equal the original in {alone_equal} of {}; \
         the plain native crate in {plain_equal}",
        tops.len()
    );
    for (address, calls) in patched.iter().zip(&reached) {
        println!("  {address:08x}: {calls} calls in the 53 frames");
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// The rest of a main-loop pass after the state callback (the loop body of
/// 800261A8 as `mainloop::main_loop_iteration_with` translates it), with
/// every routine that has an address called through `dispatch::call`:
/// 8002C4DC CD queue, 800278A8 cursor sprites, the root counter / DrawSync
/// / VSync device calls (host calls: no interrupt runs during the waits),
/// 800260C0 frame end, then the context's DrawSync and pause bits.
fn loop_tail(m: &mut Machine) {
    use ac3_flight_jp::mainloop::flags;
    const ROOT_COUNTER_1: u32 = 0xf200_0001;
    dispatch::call(m, 0x8002_c4dc, [0; 4]);
    let context = m.lw(CONTEXT);
    let ot = m.lw(context + 0xc);
    let packet = m.lw(context + 0x14);
    let packet = dispatch::call(m, 0x8002_78a8, [ot, packet, 0, 0]);
    m.sw(context + 0x14, packet);
    let start = ac3_flight_jp::sdk::get_rcnt(m, ROOT_COUNTER_1);
    m.sw(context + 0x48, start);
    ac3_flight_jp::sdk::draw_sync(m, 0);
    let end = ac3_flight_jp::sdk::get_rcnt(m, ROOT_COUNTER_1);
    let display = m.lw(ac3_flight_jp::GP + 0x2d4);
    m.sw(context + 0x4c, end);
    let interval = m.lw(display + 0x14);
    ac3_flight_jp::sdk::vsync(m, interval);
    let vblanks = ac3_flight_jp::sdk::vsync(m, u32::MAX);
    m.sw(context + 0x20, vblanks);
    ac3_flight_jp::sdk::reset_rcnt(m, ROOT_COUNTER_1);
    dispatch::call(m, 0x8002_60c0, [0; 4]);
    let word = m.lw(context);
    if word & flags::DRAW_SYNC != 0 {
        m.sw(context, word & !flags::DRAW_SYNC);
        ac3_flight_jp::sdk::draw_sync(m, 0);
    }
    let word = m.lw(context);
    if word & flags::PAUSE_LOCK != 0 {
        m.sw(
            context,
            word & !(flags::PAUSED | flags::RESUME_REQUEST | flags::PAUSE_REQUEST),
        );
        return;
    }
    if word & flags::PAUSE_REQUEST != 0 {
        m.sw(context, (word & !flags::PAUSE_REQUEST) | flags::PAUSED);
    }
    let word = m.lw(context);
    if word & flags::RESUME_REQUEST != 0 {
        m.sw(context, word & !flags::RESUME_REQUEST & !flags::PAUSED);
    }
}

/// RAM addresses (outside the stack below `sp`) where `m` differs from the
/// captured image `next`.
fn ram_differences(m: &Machine, next: &[u8], sp: u32) -> Vec<u32> {
    let high = (sp & 0x1f_ffff) as usize;
    (0..0x20_0000usize)
        .filter(|&i| !(snapshot::STACK_START..high).contains(&i) && m.ram()[i] != next[i])
        .map(|i| 0x8000_0000 + i as u32)
        .collect()
}

/// The 29 consecutive loop-top pairs of `flight-001` (fields 50086..50144,
/// two fields apart): one whole main-loop pass from the first top ([`flight_frame`]
/// and [`loop_tail`]) against the second captured top. Both the original
/// instructions (every game routine interpreted) and the part's patches
/// alone ([`isolation`]) run; the patches must leave exactly the RAM,
/// scratchpad and GTE of the original instructions, and the bytes in which
/// both differ from the captured next top are reported: the state the
/// interrupt handlers and devices own (VBlank counters, libpad receive
/// buffers, root counters, CD/SPU driver state), which this harness does not
/// run during the VSync waits.
#[test]
#[ignore = "requires AC3_JP_DIR (the JP flight-001 loop tops)"]
fn flight_loop_passes_reach_the_next_loop_top() {
    let directory = run_dir("flight-001");
    let tops = loop_tops();
    let mut reproduced = 0;
    let mut pairs = 0;
    let mut host_bytes = std::collections::BTreeSet::new();
    let mut failures = Vec::new();
    for pair in tops.windows(2) {
        let ((index, field, base), (next_index, next_field, _)) = (&pair[0], &pair[1]);
        if *next_field != field + 2 {
            continue;
        }
        pairs += 1;
        let next = std::fs::read(directory.join(format!("loop-{next_index:04}.ram.bin"))).unwrap();
        let sp = base.sp;
        let run = |spec: &str| {
            let mut m = base.clone();
            let outcome = guarded(|| {
                forced(spec, || {
                    flight_frame(&mut m);
                    loop_tail(&mut m);
                });
                Ok(())
            });
            let _ = m.take_fault();
            (m, outcome)
        };
        let (original, o) = run("game");
        let (alone, a) = run(&isolation(None));
        assert!(o.is_ok() && a.is_ok(), "loop {index}: {o:?} {a:?}");
        let original_off = ram_differences(&original, &next, sp);
        let alone_off = ram_differences(&alone, &next, sp);
        let same = frame_differences(&original, &alone, sp);
        println!(
            "loop {index} -> {next_index} (fields {field} -> {next_field}): original instructions \
             {} bytes off the next top, patches {} bytes off, patches vs original: {}",
            original_off.len(),
            alone_off.len(),
            same.as_deref().unwrap_or("equal")
        );
        if same.is_none() && original_off == alone_off {
            reproduced += 1;
            host_bytes.extend(original_off.iter().copied());
        } else {
            failures.push(format!("loop {index}: {}", same.unwrap_or_default()));
        }
    }
    let host: Vec<u32> = host_bytes.into_iter().collect();
    println!(
        "flight loop passes: {reproduced} of {pairs} consecutive pairs reach the next captured \
         loop top exactly as the original instructions do; the bytes both leave different \
         (device and interrupt state): {}",
        ranges(&host)
    );
    assert!(pairs > 0 && failures.is_empty(), "{failures:#?}");
}

// ---- oracle cases ---------------------------------------------------------------------------

/// The JP extent of routine `address` from `jp_pal_map.tsv`.
fn extent(address: u32) -> (u32, u32) {
    let rows = scan::jp_map(&scan::jp_dir());
    let row = rows
        .iter()
        .find(|r| r.jp == address && r.region.starts_with("resident"))
        .unwrap_or_else(|| panic!("{address:08x}: not a resident JP routine"));
    (row.jp, row.jp + row.size)
}

/// Run the original and the patch (through `dispatch::call`) from `base`
/// with `args`; returns the bytes the original changed. Calls out of the
/// routine run the same way in both runs: every game routine but the part's
/// patches interpreted ([`isolation`]).
fn agree(label: &str, base: &Machine, routine: u32, args: [u32; 4], void: bool) -> usize {
    agree_forced(label, base, routine, args, void, &isolation(None))
}

/// [`agree`] with the routines of `force` interpreted in both runs (other
/// parts' callees whose translation is still open).
fn agree_forced(
    label: &str,
    base: &Machine,
    routine: u32,
    args: [u32; 4],
    void: bool,
    force: &str,
) -> usize {
    let range = extent(routine);
    forced(force, || {
        oracle::diff(label, base, range, args, |m| {
            let v0 = dispatch::call(m, routine, args);
            (!void).then_some(v0)
        })
    })
}

/// The captured entry state of `callee` in `run` as a machine.
fn entry_state(run: &str, callee: u32) -> (Machine, [u32; 4]) {
    let (entry, _) = pair(run, 0, callee, 0);
    let args = [entry.gpr(4), entry.gpr(5), entry.gpr(6), entry.gpr(7)];
    (entry.machine(), args)
}

const CONTEXT: u32 = ac3_flight_jp::GP + 0x2ec;
const ENTITIES: u32 = ac3_flight_jp::GP + 0x2e8;

fn entity(m: &mut Machine, index: u32) -> u32 {
    let table = m.lw(ENTITIES);
    m.lw(table + (index << 2))
}

#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn shared_routines_match_the_original() {
    let mut cases = 0;
    let mut changed = 0;
    // 8002F70C: entity kinds around the 0x30 bound, channels 0..3.
    let (base, _) = entry_state("flight-002", 0x8002_f70c);
    for index in [0u32, 1, 2, 5] {
        for kind in [0u16, 0x2f, 0x30, 0x31, 0x3f, 0x40, 0x7f] {
            for channel in [0u32, 3] {
                let mut m = base.clone();
                let e = entity(&mut m, index);
                m.sh(e + 0x48, kind);
                changed += agree(
                    &format!("8002f70c {index} {kind:#x} {channel}"),
                    &m,
                    0x8002_f70c,
                    [channel, index, 0, 0],
                    false,
                );
                cases += 1;
            }
        }
    }
    // 8003A5C0 / 8003A62C: target index, entity state, flags, mission.
    let (base, _) = entry_state("flight-002", 0x8003_a5c0);
    for target in [0u8, 1, 3, 0xff] {
        for state in [0x50i16, 0x51, 100] {
            for flags in [0u32, 0x100, 0x400, 0x500] {
                for mission in [0x15i16, 0x16, 0x116] {
                    let mut m = base.clone();
                    m.sb(0x800c_6d6e, target);
                    m.sb(0x800c_6d72, target ^ 2);
                    let index = u32::from(target & 0x7);
                    let e = entity(&mut m, index);
                    m.sh(e + 0x48, state as u16);
                    m.sw(0x800c_6fd0 + (index << 2), flags);
                    m.sw(0x800c_6fd0 + ((index ^ 2) << 2), flags ^ 0x400);
                    let context = m.lw(CONTEXT);
                    m.sh(context + 0x38c, mission as u16);
                    agree("8003a5c0", &m, 0x8003_a5c0, [0; 4], false);
                    agree("8003a62c", &m, 0x8003_a62c, [0; 4], false);
                    cases += 2;
                }
            }
        }
    }
    // 800463B8: lock links in every tracker state, context +0x394 on/off.
    let (base, _) = entry_state("flight-002", 0x8004_63b8);
    for slot in [0u32, 1, 2] {
        for target in [0u32, 1, 4, 0xff] {
            for (t2, t3) in [(0xffu8, 0xffu8), (1, 0xff), (1, 2), (0, 3)] {
                for flag in [0u32, 1] {
                    let mut m = base.clone();
                    let record = 0x800c_6d80 + ((target & 0xf) << 2);
                    m.sb(record + 2, t2);
                    m.sb(record + 3, t3);
                    let context = m.lw(CONTEXT);
                    m.sw(context + 0x394, flag);
                    changed += agree(
                        &format!("800463b8 {slot} {target:#x} {t2:#x}/{t3:#x} {flag}"),
                        &m,
                        0x8004_63b8,
                        [slot, target, 0, 0],
                        false,
                    );
                    cases += 1;
                }
            }
        }
    }
    println!("shared: {cases} oracle cases, {changed} bytes changed");
    assert!(changed > 0);
}

/// `[gp+0x4D0]`, `gp+0x4CC`, `[gp+0x218]` (`states.rs`).
const MOVIE: u32 = ac3_flight_jp::GP + 0x4d0;
const MOVIE_DELAY: u32 = ac3_flight_jp::GP + 0x4cc;
const MOVIE_MUSIC: u32 = ac3_flight_jp::GP + 0x218;

#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn movie_and_picture_states_match_the_original() {
    let (mut cases, mut changed) = (0, 0);
    // 80057CD0: attract mode or a numbered movie, visit counts, pad kinds,
    // music flags.
    let (base, args) = entry_state("story-002", 0x8005_7cd0);
    for index in [0u8, 1, 7] {
        for visits in [0u8, 1, 4, 0xff] {
            for pad in [0u32, 0x100, 0x300] {
                for music in [0u8, 1] {
                    let mut m = base.clone();
                    let context = m.lw(CONTEXT);
                    m.sb(context + 0x3a8, index);
                    m.sb(context + 0x3a4, visits);
                    m.sw(context + 0x128, pad);
                    m.sb(context + 0x39c, music);
                    m.sb(context + 0x3a7, 1);
                    let label = format!("80057cd0 {index} {visits} {pad:#x} {music}");
                    changed += agree(&label, &m, 0x8005_7cd0, args, false);
                    cases += 1;
                }
            }
        }
    }
    // 80057EBC: every movie index the JP switch tests, the delay counter,
    // the buffer, music.
    let (base, args) = entry_state("story-002", 0x8005_7ebc);
    for index in [0u8, 2, 7, 0xc, 0x10, 0x1d, 0x1e, 0x1f] {
        for delay in [0u32, 1, 2, 5] {
            for buffer in [0u32, 1] {
                let mut m = base.clone();
                let context = m.lw(CONTEXT);
                m.sb(context + 0x3a8, index);
                m.sw(context + 4, buffer);
                m.sw(MOVIE_DELAY, delay);
                let music = m.lw(MOVIE_MUSIC);
                m.sw(music + 4, delay & 1);
                let label = format!("80057ebc {index:#x} {delay} {buffer}");
                changed += agree(&label, &m, 0x8005_7ebc, args, false);
                cases += 1;
            }
        }
    }
    // 800581C4 and 80057720: every player phase; at the end the JP credits
    // switch (movies 9, 0x15, 0x17, 0x19, 0x1C unless context +0x96C bit
    // 0x10) or the context's next state.
    let (base, args) = entry_state("story-002", 0x8005_81c4);
    for phase in [0u32, 1, 2, 3, 4, 5] {
        for index in [0u8, 9, 0x15, 0x16, 0x17, 0x19, 0x1c, 0x1d] {
            for flag in [0u32, 0x10] {
                if phase != 4 && (index > 9 || flag != 0) {
                    continue;
                }
                let mut m = base.clone();
                let block = m.lw(MOVIE);
                m.sw(block, phase);
                let context = m.lw(CONTEXT);
                m.sb(context + 0x3a8, index);
                m.sw(context + 0x96c, flag);
                let label = format!("800581c4 phase {phase} {index:#x} {flag:#x}");
                changed += agree(&label, &m, 0x8005_81c4, args, false);
                let label = format!("80057720 phase {phase} {index:#x}");
                changed += agree(&label, &m, 0x8005_7720, [0; 4], false);
                cases += 2;
            }
        }
    }
    // 8005B308 then 8005B4E4 (state 6, which no capture reaches): set up from
    // the movie state, on either buffer, music on or off.
    let (base, _) = entry_state("story-002", 0x8005_7ebc);
    for buffer in [0u32, 1] {
        for music in [0u8, 1] {
            for index in [7u8, 0xc] {
                let mut m = base.clone();
                let context = m.lw(CONTEXT);
                m.sw(context + 4, buffer);
                m.sb(context + 0x39c, music);
                m.sb(context + 0x3a7, 1);
                m.sb(context + 0x3a8, index);
                let label = format!("8005b308 {buffer} {music} {index}");
                changed += agree(&label, &m, 0x8005_b308, [0; 4], false);
                cases += 1;
                if buffer == 1 {
                    dispatch::call(&mut m, 0x8005_b308, [0; 4]);
                    m.take_fault().unwrap();
                    for phase in [0u32, 3, 4] {
                        let mut p = m.clone();
                        let block = p.lw(ac3_flight_jp::GP + 0x4dc);
                        p.sw(block, phase);
                        let label = format!("8005b4e4 {music} {index} phase {phase}");
                        changed += agree(&label, &p, 0x8005_b4e4, [0; 4], false);
                        cases += 1;
                    }
                }
            }
        }
    }
    println!("states: {cases} oracle cases, {changed} bytes changed");
    assert!(changed > 0);
}

const GAMEPLAY: u32 = ac3_flight_jp::GP + 0x2e0;

#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn init_tail_routines_match_the_original() {
    let (mut cases, mut changed) = (0, 0);
    // 80035740: request, context +0x39E, record +0x50 bit 0x800.
    let (base, _) = entry_state("flight-002", 0x8003_5740);
    for request in [0u32, 1] {
        for flag in [0u8, 1] {
            for bit in [0u32, 0x800] {
                let mut m = base.clone();
                let context = m.lw(CONTEXT);
                m.sb(context + 0x39e, flag);
                let gameplay = m.lw(GAMEPLAY);
                let record = m.lw(gameplay + 0x28);
                m.sw(record + 0x50, bit);
                let label = format!("80035740 {request} {flag} {bit:#x}");
                changed += agree(&label, &m, 0x8003_5740, [request, 0, 0, 0], true);
                cases += 1;
            }
        }
    }
    // 80032518: every mission number of the table and beyond.
    let (base, _) = entry_state("flight-002", 0x8003_2518);
    for mission in -2i32..64 {
        let label = format!("80032518 mission {mission}");
        changed += agree(&label, &base, 0x8003_2518, [mission as u32, 0, 0, 0], true);
        cases += 1;
    }
    // 80032080: the phase codes with a result slot (and none); phase
    // 0x10000000 counts at context +0x3AC. Not here: leaving 0x40 (its hook
    // calls `init::initialize_camera`, 80077880, natively: resident part 2)
    // and the entry hooks that run mission scripts (`script`, part 2).
    let (base, args) = entry_state("flight-002", 0x8003_2080);
    for phase in [1u32, 0x80, 0x2_0000, 0x40_0000, 0x1000_0000, 0x400_0000, 3] {
        for leaving in [0u32, 8] {
            let mut m = base.clone();
            let gameplay = m.lw(GAMEPLAY);
            m.sw(gameplay + 4, phase);
            let label = format!("80032080 phase {phase:#x} leaving {leaving:#x}");
            changed += agree(&label, &m, 0x8003_2080, [leaving, args[1], 0, 0], false);
            cases += 1;
        }
    }
    // 8004E620: the mission specials in phases 0..2.
    let (base, _) = entry_state("flight-002", 0x8004_e620);
    for mission in [0i16, 0x0d, 0x15, 0x2b, 0x37, 0x10d, -0xd] {
        for phase in [0u32, 1, 2] {
            let mut m = base.clone();
            let context = m.lw(CONTEXT);
            m.sh(context + 0x38c, mission as u16);
            let gameplay = m.lw(GAMEPLAY);
            m.sw(gameplay + 8, phase);
            let label = format!("8004e620 mission {mission:#x} phase {phase}");
            changed += agree(&label, &m, 0x8004_e620, [0; 4], true);
            cases += 1;
        }
    }
    println!("init_tail: {cases} oracle cases, {changed} bytes changed");
    assert!(changed > 0);
}

/// `[gp+0x2D8]` ground-unit entity pointers, `[gp+0x388]` their 0x28-byte
/// records (+0x10: attack parameters), 800C6FD0 the slot state words,
/// `[800BD080]` the incoming-projectile mask (`player.rs`).
const UNITS: u32 = ac3_flight_jp::GP + 0x2d8;
const UNIT_RECORDS: u32 = ac3_flight_jp::GP + 0x388;
const SLOT_STATES: u32 = 0x800c_6fd0;
const INCOMING: u32 = 0x800b_d080;

/// A ground-unit attack case from `base` (flight-002, where no unit is in
/// play): unit `unit` with attack parameters of a random kind (guns 1..3,
/// missiles 0x11..0x19) in the record of unit 31, a random cone, range,
/// burst timing, side, entity kind, x position (inside or outside the
/// mission-0xC bands), target (0xFF: none), pause and counter; the target
/// slots in play or not, the incoming mask, the level (context +0x394) and
/// the mission (context +0x38C, 0xC or 1). Returns the machine, the unit and
/// a distance for 80053214.
fn unit_case(base: &Machine, rng: &mut oracle::Rng) -> (Machine, u32, u32) {
    const KINDS: [u8; 11] = [0, 1, 2, 3, 0x11, 0x12, 0x13, 0x14, 0x15, 0x18, 0x19];
    const XS: [u32; 6] = [
        0,
        0xe500_0100,
        0x0090_0000,
        0xf900_0010,
        0xec00_0100,
        0x1000_0000,
    ];
    let mut m = base.clone();
    let pick = |rng: &mut oracle::Rng, n: usize| rng.next() as usize % n;
    let unit = rng.next() % 8;
    let units = m.lw(UNITS);
    let records = m.lw(UNIT_RECORDS);
    let entity = m.lw(units + unit * 4);
    let attack = records + 31 * 0x28;
    m.sw(records + unit * 0x28 + 0x10, attack);
    m.sb(attack, KINDS[pick(rng, KINDS.len())]);
    m.sb(attack + 1, (rng.next() % 3) as u8);
    let range = if rng.next().is_multiple_of(4) {
        rng.next() % 0x400
    } else {
        0x7fff_ffff
    };
    m.sw(attack + 4, range);
    if rng.next().is_multiple_of(3) {
        for offset in [8, 0xa, 0xc, 0xe] {
            m.sh(
                attack + offset,
                (rng.next() as u16 & 0x3fff).wrapping_sub(0x1000),
            );
        }
    } else {
        for (offset, value) in [(8, 0xf000u16), (0xa, 0x1000), (0xc, 0), (0xe, 0x7fff)] {
            m.sh(attack + offset, value);
        }
    }
    m.sh(attack + 0x10, (1 + rng.next() % 4) as u16);
    m.sh(attack + 0x12, (rng.next() % 8) as u16);
    m.sh(attack + 0x14, (rng.next() % 8) as u16);
    let side = if rng.next() & 1 != 0 { 0x80 } else { 0 };
    m.sw(entity, (0x20 + unit) | side);
    m.sh(entity + 0x48, [0x36u16, 0x46, 0x20][pick(rng, 3)]);
    m.sw(entity + 8, XS[pick(rng, XS.len())]);
    m.sw(entity + 0xc, rng.next() << 4);
    m.sw(entity + 0x10, rng.next() << 4);
    m.sh(entity + 0x18, rng.next() as u16);
    m.sh(entity + 0x1a, rng.next() as u16);
    m.sb(entity + 0x5b, [0xffu8, 0, 1, 2, 3][pick(rng, 5)]);
    m.sb(entity + 0x5a, (rng.next() & 1) as u8);
    m.sw(entity + 0x64, rng.next() % 12);
    for slot in 1..4 {
        let word = SLOT_STATES + slot * 4;
        let state = m.lw(word);
        let state = if rng.next() & 3 != 0 {
            state | 1
        } else {
            state & !1
        };
        m.sw(word, state);
    }
    let incoming = m.lw(INCOMING);
    let mask = if rng.next().is_multiple_of(4) {
        0xffff_ffff
    } else {
        0
    };
    m.sw(incoming, mask);
    let context = m.lw(CONTEXT);
    m.sw(context + 0x394, rng.next() % 3);
    let mission = if rng.next() & 1 != 0 { 0xc } else { 1 };
    m.sh(context + 0x38c, mission);
    let distance = rng.next() % 0x2000;
    (m, unit, distance)
}

/// `[gp+0x520]` player state, `[gp+0x524]` motion block, `[gp+0x51C]`
/// controls, `[gp+0x518]` control outputs, `[0x800BCE1C]` the target
/// selection, `[0x800BCE18]` the view record, `[0x800BCE14]` the control
/// setting (`player.rs`).
const PLAYER_STATE: u32 = ac3_flight_jp::GP + 0x520;
const MOTION_BLOCK: u32 = ac3_flight_jp::GP + 0x524;
const CONTROLS: u32 = ac3_flight_jp::GP + 0x51c;
const CONTROL_OUT: u32 = ac3_flight_jp::GP + 0x518;
const TARGET_SELECTION: u32 = 0x800b_ce1c;
const VIEW_RECORD: u32 = 0x800b_ce18;
const SETTING: u32 = 0x800b_ce14;

/// The near player routines no capture reaches (`player.rs`, batch 3): the
/// unit attack step and its gun and missile steps, the player's missile
/// launcher, the kill camera and the formation, follow and take-off modes
/// of 800729DC.
#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn player_attack_and_flight_modes_match_the_original() {
    let (mut cases, mut changed) = (0, 0);
    let mut rng = oracle::Rng(0x5eed_0003);
    // 80052FA8 / 80053214 / 800535D4 from random unit states.
    let (base, _) = entry_state("flight-002", 0x8005_1414);
    for case in 0..400 {
        let (m, unit, distance) = unit_case(&base, &mut rng);
        let label = format!("80052fa8 case {case} unit {unit}");
        changed += agree(&label, &m, 0x8005_2fa8, [unit, 0, 0, 0], false);
        let label = format!("80053214 case {case} unit {unit} distance {distance:#x}");
        changed += agree(&label, &m, 0x8005_3214, [unit, distance, 0, 0], false);
        let label = format!("800535d4 case {case} unit {unit}");
        changed += agree(&label, &m, 0x8005_35d4, [unit, 0, 0, 0], false);
        cases += 3;
    }
    // 800535D4 launches: every missile kind and variant on every level for
    // both sides, the counter at the warning point (half the interval) or
    // past it, in a wide cone with nothing incoming.
    for kind in [0x11u8, 0x12, 0x13, 0x14, 0x18, 0x19] {
        for variant in [0u8, 1, 2] {
            for level in [0u32, 1, 2] {
                for side in [0u32, 0x80] {
                    for timer in [2u32, 4, 0x10] {
                        let (mut m, unit, _) = unit_case(&base, &mut rng);
                        let units = m.lw(UNITS);
                        let records = m.lw(UNIT_RECORDS);
                        let entity = m.lw(units + unit * 4);
                        let attack = m.lw(records + unit * 0x28 + 0x10);
                        m.sb(attack, kind);
                        m.sb(attack + 1, variant);
                        for (offset, value) in
                            [(8, 0xf000u16), (0xa, 0x1000), (0xc, 0), (0xe, 0x7fff)]
                        {
                            m.sh(attack + offset, value);
                        }
                        m.sh(attack + 0x12, 8);
                        m.sw(entity, (0x20 + unit) | side);
                        m.sb(entity + 0x5b, 1);
                        m.sb(entity + 0x5a, 0);
                        m.sw(entity + 0x64, timer);
                        let incoming = m.lw(INCOMING);
                        m.sw(incoming, 0);
                        let context = m.lw(CONTEXT);
                        m.sw(context + 0x394, level);
                        let label = format!(
                            "800535d4 kind {kind:#x} variant {variant} level {level} side \
                             {side:#x} timer {timer}"
                        );
                        changed += agree(&label, &m, 0x8005_35d4, [unit, 0, 0, 0], false);
                        cases += 1;
                    }
                }
            }
        }
    }
    let (base, _) = entry_state("flight-002", 0x8007_29dc);
    // 800797FC: the effects bit, context +0x39D, the entity flag 0x4000,
    // ids with and without bit 31.
    for id in [0u32, 1, 2, 5, 0x22, 0x8000_0002] {
        for effects in [0u32, 1] {
            for allowed in [0u8, 1] {
                for flag in [0u32, 0x4000] {
                    let mut m = base.clone();
                    let state = m.lw(PLAYER_STATE);
                    let word = m.lw(state);
                    m.sw(state, (word & !1) | effects);
                    let context = m.lw(CONTEXT);
                    m.sb(context + 0x39d, allowed);
                    let e = entity(&mut m, id & 0x3f);
                    let word = m.lw(e);
                    m.sw(e, (word & !0x4000) | flag);
                    let label = format!("800797fc {id:#x} {effects} {allowed} {flag:#x}");
                    changed += agree(&label, &m, 0x8007_97fc, [id, 0, 0, 0], true);
                    cases += 1;
                }
            }
        }
    }
    // 80072004: missiles left, launcher count and free launcher slots.
    for missiles in [0u32, 1, 5] {
        for count in [0u32, 1, 2, 4] {
            for slots in [0u32, 0x0101_0101, 0x0100_0001, 0x0001_0100] {
                let mut m = base.clone();
                let motion = m.lw(MOTION_BLOCK);
                m.sw(motion + 0x1c, missiles);
                m.sw(motion + 4, count);
                let state = m.lw(PLAYER_STATE);
                m.sw(state + 0x58, slots);
                let label = format!("80072004 {missiles} {count} {slots:#x}");
                changed += agree(&label, &m, 0x8007_2004, [state + 0x58, 0, 0, 0], true);
                cases += 1;
            }
        }
    }
    // 80075084: phases 0..2, the counter around the sound (0x96) and the
    // end (0x12D), the setting (sound 0x5E / 0x5F), random offsets small
    // enough to count or not, target velocities.
    for case in 0..96u32 {
        let mut m = base.clone();
        let player = entity(&mut m, 0);
        let state = m.lw(PLAYER_STATE);
        let offset = entity(&mut m, 3) + 0x5c;
        let target = entity(&mut m, 4) + 0x38;
        let scale = [0x1_0000u32, 0x10_0000, 0x400][(case % 3) as usize];
        for i in 0..3 {
            m.sw(offset + i * 4, (rng.next() % scale).wrapping_sub(scale / 2));
            m.sw(target + i * 4, (rng.next() % 0x4000).wrapping_sub(0x2000));
            let v = m.lw(target + i * 4);
            m.sw(
                player + 0x38 + i * 4,
                v.wrapping_add(rng.next() % 0x400).wrapping_sub(0x200),
            );
        }
        for i in 0..3 {
            m.sh(offset + 0x10 + i * 2, rng.next() as u16);
        }
        m.sw(state + 8, case % 3);
        m.sw(
            state + 0xc,
            [0u32, 0x95, 0x96, 0x12b, 0x12c, 0x12d][(case / 3 % 6) as usize],
        );
        m.sw(SETTING, 1 + (case / 18) % 2);
        let label = format!("80075084 case {case}");
        changed += agree(
            &label,
            &m,
            0x8007_5084,
            [player, state, offset, target],
            false,
        );
        cases += 1;
    }
    // 80074E64: the leader index and offsets (state +0x4C stays).
    for leader in [1u32, 2, 3, 4] {
        for _ in 0..6 {
            let mut m = base.clone();
            let player = entity(&mut m, 0);
            let state = m.lw(PLAYER_STATE);
            let controls = m.lw(CONTROLS);
            let target = m.lw(TARGET_SELECTION);
            m.sw(target, leader);
            for i in 1..4 {
                m.sw(target + i * 4, (rng.next() << 8) >> 4);
            }
            m.sh(state + 0x4c, 0x1234);
            let label = format!("80074e64 leader {leader}");
            changed += agree(
                &label,
                &m,
                0x8007_4e64,
                [player, state, controls, target],
                true,
            );
            cases += 1;
        }
    }
    // 80075B04: phases 0..3 (and 4), the counter around 0x5B, replay or not,
    // speeds around 0xF000; `view` and `replay` in the caller's stack words.
    for phase in 0u32..5 {
        for counter in [0u32, 0x2d, 0x5a, 0x5b] {
            for replay in [0u32, 1] {
                for speed in [0u32, 0xef00, 0xf000] {
                    if phase != 2 && speed != 0 {
                        continue;
                    }
                    let mut m = base.clone();
                    let player = entity(&mut m, 0);
                    let state = m.lw(PLAYER_STATE);
                    let out = m.lw(CONTROL_OUT);
                    let controls = m.lw(CONTROLS);
                    let view = m.lw(VIEW_RECORD);
                    m.sw(state + 8, phase);
                    m.sw(state + 0xc, counter);
                    m.sw(state + 0x48, speed);
                    m.sh(state + 0x52, [0xffffu16, 3][(counter & 1) as usize]);
                    let sp = m.sp;
                    m.sw(sp + 0x10, view);
                    m.sw(sp + 0x14, replay);
                    let label = format!("80075b04 phase {phase} {counter:#x} {replay} {speed:#x}");
                    changed += agree(
                        &label,
                        &m,
                        0x8007_5b04,
                        [player, state, out, controls],
                        false,
                    );
                    cases += 1;
                }
            }
        }
    }
    println!("player batch 3: {cases} oracle cases, {changed} bytes changed");
    assert!(changed > 0);
}

/// libc `rand` state word (`sdk::RAND_STATE`).
const RAND_STATE: u32 = 0x800c_bf58;

/// The replay module's routines no capture reaches: the sequence player's
/// effect packets (fade, red tint, sparks), the movie and picture phases and
/// the sequence start and end.
#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn replay_sequence_and_movie_phases_match_the_original() {
    let (mut cases, mut changed) = (0, 0);
    let mut rng = oracle::Rng(0x5eed_0004);
    let (base, _) = entry_state("flight-002", 0x8003_39e0);
    let mut m = base.clone();
    let context = m.lw(CONTEXT);
    let ot = m.lw(context + 0xc);
    let packets = m.lw(context + 0x14);
    // 80093BC4: levels around 0x1000, additive or subtractive.
    for level in [0u32, 1, 0x800, 0xfff, 0x1000, 0x2000, 0xffff_f000] {
        for additive in [0u32, 1] {
            let label = format!("80093bc4 level {level:#x} additive {additive}");
            changed += agree(
                &label,
                &base,
                0x8009_3bc4,
                [ot, packets, additive, level],
                false,
            );
            cases += 1;
        }
    }
    // 80093C90: levels 0 (nothing) to 0x1000.
    for level in [0u32, 1, 0x400, 0x800, 0x1000] {
        let label = format!("80093c90 level {level:#x}");
        changed += agree(&label, &base, 0x8009_3c90, [ot, packets, level, 0], false);
        cases += 1;
    }
    // 80093D1C: both phases, random positions and random seeds.
    for case in 0..64u32 {
        let mut m = base.clone();
        m.sw(RAND_STATE, (rng.next() << 8) | (rng.next() & 0xff));
        let (x, y, z) = (rng.next() << 8, rng.next() << 8, rng.next() << 8);
        let label = format!("80093d1c case {case}");
        changed += agree(&label, &m, 0x8009_3d1c, [x, y, z, case & 3], true);
        cases += 1;
    }
    // 80039430 / 800395D4: the sequence modes of the JP jump table and its
    // gaps, the modes whose end restores only the flag words, with and
    // without the context halfword +0x3AC.
    for mode in [
        0u32, 2, 4, 5, 6, 0xf, 0x13, 0x14, 0x15, 0x16, 0x1e, 0x25, 0x44, 0x55, 0x5c, 0x5d, 0x5e,
    ] {
        for pending in [0u16, 1] {
            let mut m = base.clone();
            let context = m.lw(CONTEXT);
            m.sw(context + 0x1038, mode);
            m.sh(context + 0x3ac, pending);
            let label = format!("80039430 mode {mode:#x} pending {pending}");
            changed += agree(&label, &m, 0x8003_9430, [0; 4], false);
            let label = format!("800395d4 mode {mode:#x} pending {pending}");
            changed += agree(&label, &m, 0x8003_95d4, [0; 4], false);
            cases += 2;
        }
    }
    // 800573E8 / 80057640 / 8005AE8C from the intro movie: the movie
    // entries the JP switches test, the context halfword +0x3AC, the music
    // option. The arena mark gameplay +0x2B8 (not set during the intro) is
    // the current arena pointer (context +0x1C).
    let (base, _) = entry_state("story-002", 0x8005_7ebc);
    for entry in [0u8, 2, 7, 0xb, 0xc, 0x10, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f] {
        for pending in [0u16, 1] {
            for music in [0u8, 1] {
                let mut m = base.clone();
                let context = m.lw(CONTEXT);
                let gameplay = m.lw(GAMEPLAY);
                let mark = m.lw(context + 0x1c);
                m.sw(gameplay + 0x2b8, mark);
                m.sb(context + 0x3a8, entry);
                m.sh(context + 0x3ac, pending);
                m.sb(context + 0x39c, music);
                m.sb(context + 0x3a7, 1);
                let label = format!("800573e8 entry {entry:#x} pending {pending} music {music}");
                changed += agree(&label, &m, 0x8005_73e8, [0; 4], true);
                let label = format!("80057640 entry {entry:#x} pending {pending} music {music}");
                changed += agree(&label, &m, 0x8005_7640, [0; 4], true);
                let label = format!("8005ae8c entry {entry:#x} pending {pending} music {music}");
                changed += agree(&label, &m, 0x8005_ae8c, [0; 4], true);
                cases += 3;
            }
        }
    }
    println!("replay: {cases} oracle cases, {changed} bytes changed");
    assert!(changed > 0);
}

/// 800C6610: the sound source count of the sequence frames (80030528).
const SOUND_SOURCE_COUNT: u32 = 0x800c_6610;

/// The sequence modes 800399D4 and 800396E8 test (their jump tables and
/// comparisons) and a few others.
const SEQUENCE_MODES: [u32; 20] = [
    0, 9, 0xc, 0x16, 0x1a, 0x25, 0x26, 0x27, 0x28, 0x2a, 0x30, 0x34, 0x35, 0x3a, 0x48, 0x4b, 0x59,
    0x5a, 0x5b, 0x5c,
];

/// The sequence player (800396E8 and its fade 800399D4), the course control
/// 8005C270 and the phase-0x400 setup 8005B654 (with 80032080 entering
/// phase 0x400), none of which a capture reaches.
#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn sequence_course_and_phase_400_match_the_original() {
    const GP: u32 = ac3_flight_jp::GP;
    let (mut cases, mut changed) = (0, 0);
    let mut rng = oracle::Rng(0x5eed_0005);
    let (base, _) = entry_state("flight-002", 0x8003_39e0);
    // The overlay ordering table and packet pointers (gameplay +0x10/+0x14,
    // set while a frame is drawn): the frame's ordering table (context +0xC)
    // and free memory past the arena pointer.
    let base = {
        let mut m = base;
        let context = m.lw(CONTEXT);
        let gameplay = m.lw(GAMEPLAY);
        let ot = m.lw(context + 0xc);
        let packets = (m.lw(context + 0x1c) + 0x2000) & !3;
        m.sw(gameplay + 0x10, ot);
        m.sw(gameplay + 0x14, packets);
        m
    };
    // 800399D4: random fade states, levels, frame windows, pad skips and
    // modes, then every fade state for the additive modes and the frames
    // around the mode-0x2A windows.
    let fade_cases = (0..600u32)
        .map(|_| {
            let mode = SEQUENCE_MODES[rng.next() as usize % SEQUENCE_MODES.len()];
            let frames = rng.next() % 0x300;
            let left = if frames == 0 { 0 } else { rng.next() % frames };
            (mode, frames, left, rng.next() % 7)
        })
        .chain(
            [9u32, 0xc, 0xd, 0x2a, 0x2b]
                .into_iter()
                .flat_map(|mode| (0..7u32).map(move |state| (mode, 0x200, 0x1f0, state))),
        )
        .chain((0x60..0x90u32).map(|elapsed| (0x2a, 0x200, 0x202 - elapsed, 2)))
        .collect::<Vec<_>>();
    for (case, (mode, frames, left, state)) in fade_cases.into_iter().enumerate() {
        let mut m = base.clone();
        let context = m.lw(CONTEXT);
        m.sw(context + 0x1038, mode);
        m.sw(GP + 0x3f4, frames);
        m.sh(GP + 0x400, left as u16);
        m.sw(GP + 0x3f0, state);
        m.sw(GP + 0x3fc, rng.next() & 1);
        m.sw(GP + 0x9c, rng.next() % 0x2000);
        m.sw(GP + 0xa0, rng.next() % 0x2000);
        m.sw(GP + 0xa4, rng.next() % 0x100);
        let pad = m.lw(context + 0x12c);
        let skip = if rng.next().is_multiple_of(4) {
            0x40
        } else {
            0
        };
        m.sw(context + 0x12c, (pad & !0x40) | skip);
        let label =
            format!("800399d4 case {case} mode {mode:#x} frames {frames:#x} left {left:#x}");
        changed += agree(&label, &m, 0x8003_99d4, [0; 4], true);
        cases += 1;
    }
    // 800396E8: phases 0x10 / 0x40 / playing, the stream wait flag, the end
    // flag, the step, frames left around 0x17 and the timeout counter. A
    // step plays a well-formed record past the arena pointer (size 8, no
    // commands, a sound frame with no sources and no one-shot sound)
    // as step 1 of a 3-step chunk; step 0 asks the stream (8002B9EC) first.
    for case in 0..200u32 {
        let mut m = base.clone();
        let gameplay = m.lw(GAMEPLAY);
        let phase = [0x10u32, 0x40, 0x20, 0x20, 0x20][case as usize % 5];
        m.sw(gameplay + 4, phase);
        let context = m.lw(CONTEXT);
        let mode = SEQUENCE_MODES[rng.next() as usize % SEQUENCE_MODES.len()];
        m.sw(context + 0x1038, mode);
        let record = (m.lw(context + 0x1c) + 0x40) & !3;
        for i in 0..0x50 {
            m.sw(record + 4 * i, 0);
        }
        m.sw(record, 8);
        m.sh(record + 0x80, 0xffff);
        m.sw(SOUND_SOURCE_COUNT, 0);
        m.sw(GP + 0x40c, record.wrapping_sub(0x23c));
        let header = record + 0x100;
        m.sh(header + 6, 0x40);
        m.sb(header + 8, 3);
        m.sw(GP + 0x410, header);
        m.sw(GP + 0x3f8, rng.next() & 1);
        m.sw(GP + 0x404, rng.next() & 1);
        m.sh(GP + 0x416, u16::from(!rng.next().is_multiple_of(4)));
        m.sh(GP + 0x414, 3);
        m.sh(GP + 0x400, (0x10 + rng.next() % 0x10) as u16);
        m.sw(GP + 0x3f4, 0x40 + rng.next() % 0x100);
        m.sw(GP + 0x418, [0u32, 0x95, 0x96][rng.next() as usize % 3]);
        m.sw(GP + 0x3f0, rng.next() % 6);
        let label = format!("800396e8 case {case} phase {phase:#x} mode {mode:#x}");
        changed += agree(&label, &m, 0x8003_96e8, [0; 4], true);
        cases += 1;
    }
    // 8005C270: every course entry and phase, pad shortcuts, the frame
    // counter around its thresholds, the course-7 progress.
    const BUTTONS: [u32; 8] = [0, 0x40, 0x80, 0x100, 0x200, 0x1000, 0x240, 0x4040];
    const FRAMES: [u32; 8] = [0, 0x10, 0x11c, 0x12c, 0x384, 0xe10, 0x1518, 0x1519];
    for case in 0..400u32 {
        let mut m = base.clone();
        let gameplay = m.lw(GAMEPLAY);
        let phase = [0x800u32, 0x800, 0x800, 0x400, 0x2000, 0x1000][case as usize % 6];
        m.sw(gameplay + 4, phase);
        let index = rng.next() % 0x18;
        m.sw(GP + 0x334, index);
        let state = m.lw(GP + 0x520);
        let buttons = BUTTONS[rng.next() as usize % 8] | (rng.next() & 0x10);
        m.sw(state, buttons);
        let context = m.lw(CONTEXT);
        let pad = m.lw(context + 0x12c);
        m.sw(context + 0x12c, (pad & !0x40) | (rng.next() & 0x40));
        let frame = FRAMES[rng.next() as usize % 8];
        m.sw(gameplay + 0xc, frame);
        let progress = m.lw(GP + 0x340);
        m.sh(
            progress + 8,
            [0xffffu16, 0, 0x185, 0x186][rng.next() as usize % 4],
        );
        m.sb(progress + 0xa, (rng.next() & 1) as u8);
        let label = format!(
            "8005c270 case {case} phase {phase:#x} course {index} buttons {buttons:#x} frame \
             {frame:#x}"
        );
        changed += agree(&label, &m, 0x8005_c270, [0; 4], true);
        cases += 1;
    }
    // 8005B654: every mission of its table (and one without) for phase
    // indices 0..2, every level (a negative one leaves the caller's s3: 0
    // here, as for a call by address), first and later passes.
    for mission in [
        1i16, 4, 7, 9, 0xe, 0x18, 0x19, 0x1d, 0x1f, 0x21, 0x22, 0x29, 0x2a, 0x2c, 0x32,
    ] {
        for index in [0u32, 1, 2] {
            for level in [-1i32, 0, 1, 2] {
                for pass in [0u8, 1] {
                    let mut m = base.clone();
                    let context = m.lw(CONTEXT);
                    m.sh(context + 0x38c, mission as u16);
                    m.sw(context + 0x394, level as u32);
                    let gameplay = m.lw(GAMEPLAY);
                    m.sw(gameplay + 8, index);
                    m.sb(GP + 0x4e8, pass);
                    m.sw(RAND_STATE, rng.next());
                    let label = format!(
                        "8005b654 mission {mission:#x} index {index} level {level} pass {pass}"
                    );
                    changed += agree(&label, &m, 0x8005_b654, [0; 4], true);
                    cases += 1;
                }
            }
        }
    }
    println!("sequence/course/phase 0x400: {cases} oracle cases, {changed} bytes changed");
    assert!(changed > 0);
}

/// The kill log 80048758 and the ground-unit destruction 800520C8 (no
/// capture destroys a unit): random kinds, slot state bits, entity flags,
/// attackers, missions, kill counts and log positions.
#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn unit_kill_log_and_destruction_match_the_original() {
    const KINDS: [u16; 7] = [0x2d, 0x30, 0x31, 0x32, 0x1b, 0x20, 0x46];
    const MISSIONS: [u16; 6] = [1, 0x11, 0x16, 0x1f, 0x21, 0x34];
    let (mut cases, mut changed) = (0, 0);
    let mut rng = oracle::Rng(0x5eed_0006);
    let (base, _) = entry_state("flight-002", 0x8005_1414);
    for case in 0..600u32 {
        let mut m = base.clone();
        let pick = |rng: &mut oracle::Rng, n: usize| rng.next() as usize % n;
        let unit = rng.next() % 8;
        let slot = if case % 2 == 0 {
            unit + 0x20
        } else {
            1 + rng.next() % 7
        };
        let e = entity(&mut m, slot);
        m.sh(e + 0x48, KINDS[pick(&mut rng, KINDS.len())]);
        let flags = (m.lw(e) & !0x41c0) | (rng.next() & 0x41c0);
        m.sw(e, flags);
        m.sw(
            e + 0x50,
            if rng.next() & 1 != 0 {
                0
            } else {
                rng.next() & 0x3f
            },
        );
        m.sh(e + 0x4e, (rng.next() % 3) as u16);
        let state = SLOT_STATES + slot * 4;
        m.sw(state, (rng.next() & 0xf00) | (rng.next() & 1) | 0x0300_0000);
        let context = m.lw(CONTEXT);
        m.sh(context + 0x38c, MISSIONS[pick(&mut rng, MISSIONS.len())]);
        m.sb(context + 0x3c2, (rng.next() % 6) as u8);
        m.sb(context + 0x39d, (rng.next() & 1) as u8);
        let stats = m.lw(ac3_flight_jp::GP + 0x340);
        m.sb(stats + 0x3c, (rng.next() % 6) as u8);
        let player_state = m.lw(ac3_flight_jp::GP + 0x520);
        let word = m.lw(player_state);
        m.sw(player_state, (word & !1) | (rng.next() & 1));
        let records = m.lw(UNIT_RECORDS);
        m.sw(records + unit * 0x28 + 0x1c, 0);
        let label = format!("80048758 case {case} slot {slot:#x}");
        changed += agree(&label, &m, 0x8004_8758, [slot, 0, 0, 0], true);
        if slot >= 0x20 {
            let label = format!("800520c8 case {case} unit {unit}");
            changed += agree(&label, &m, 0x8005_20c8, [unit, 0, 0, 0], true);
            cases += 1;
        }
        cases += 1;
    }
    // 800520C8 messages: a destroyed unit in play with state bit 0x800 and no
    // attacker, unflagged (0x40), kind 0x46 on mission 0x16 or flag 0x80.
    for (mission, kind, side) in [
        (0x16u16, 0x46u16, 0u32),
        (0x16, 0x45, 0x80),
        (1, 0x46, 0x80),
        (1, 0x46, 0),
    ] {
        for camera in [0u8, 1] {
            let mut m = base.clone();
            let e = entity(&mut m, 0x23);
            m.sh(e + 0x48, kind);
            let flags = m.lw(e) & !0x41c0;
            m.sw(e, flags | side | 0x4000);
            m.sw(e + 0x50, 0);
            m.sh(e + 0x4e, 0);
            m.sw(SLOT_STATES + 0x23 * 4, 0x801);
            let context = m.lw(CONTEXT);
            m.sh(context + 0x38c, mission);
            m.sb(context + 0x39d, camera);
            let records = m.lw(UNIT_RECORDS);
            m.sw(records + 3 * 0x28 + 0x1c, 0);
            let label = format!(
                "800520c8 message mission {mission:#x} kind {kind:#x} side {side:#x} camera {camera}"
            );
            changed += agree(&label, &m, 0x8005_20c8, [3, 0, 0, 0], true);
            cases += 1;
        }
    }
    println!("kill log / destruction: {cases} oracle cases, {changed} bytes changed");
    assert!(changed > 0);
}

/// The ground-unit motion 800522B0 (no capture has a unit in play): every
/// motion kind from a random motion record (in the records of units 30/31),
/// phase, counters, flags and positions near the player, missions 0x2B and
/// 1, gameplay modes and frames for the kind-9 overlay.
#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn unit_motion_matches_the_original() {
    const GP: u32 = ac3_flight_jp::GP;
    let (mut cases, mut changed) = (0, 0);
    let mut rng = oracle::Rng(0x5eed_0007);
    let (base, _) = entry_state("flight-002", 0x8005_1414);
    for case in 0..500u32 {
        let mut m = base.clone();
        let unit = rng.next() % 8;
        let records = m.lw(UNIT_RECORDS);
        let path = records + 30 * 0x28;
        m.sw(records + unit * 0x28 + 0x14, path);
        for i in 0..0x14 {
            m.sw(path + 4 * i, (rng.next() << 12) >> 8);
        }
        m.sb(path, (case % 11) as u8);
        m.sw(path + 8, rng.next() % 0x4000);
        let player = entity(&mut m, 0);
        let e = entity(&mut m, unit + 0x20);
        for offset in [8u32, 0xc, 0x10] {
            let p = m.lw(player + offset);
            m.sw(e + offset, p.wrapping_add((rng.next() << 8) >> 4));
        }
        m.sb(e + 0x58, (rng.next() % 5) as u8);
        m.sb(e + 5, (rng.next() & 0xff) as u8);
        m.sw(e + 0x5c, rng.next() % 0x80);
        m.sw(e + 0x60, rng.next() % 0x80);
        let flags = (m.lw(e) & !0xc0) | (rng.next() & 0xc0);
        m.sw(e, flags);
        let state = SLOT_STATES + (unit + 0x20) * 4;
        let bits = if rng.next().is_multiple_of(8) {
            0x40_0001
        } else {
            1
        };
        m.sw(state, bits);
        let context = m.lw(CONTEXT);
        m.sh(context + 0x38c, if rng.next() & 1 != 0 { 0x2b } else { 1 });
        m.sw(GP + 0x490, rng.next() & 0xc0);
        m.sw(GP + 0x484, rng.next() % 0x500);
        for i in 0..3 {
            m.sh(GP + 0x488 + 2 * i, (rng.next() % 0x4000) as u16);
        }
        let gameplay = m.lw(GAMEPLAY);
        m.sw(gameplay + 4, if rng.next() & 1 != 0 { 4 } else { 2 });
        m.sw(
            gameplay + 0xc,
            [0u32, 1, 0x20, 0x21][rng.next() as usize % 4],
        );
        let label = format!("800522b0 case {case} unit {unit} kind {}", case % 11);
        changed += agree(&label, &m, 0x8005_22b0, [unit, 0, 0, 0], true);
        cases += 1;
    }
    // Kind 9 for unit 0 with the player on an anchor or far away, in
    // gameplay mode 4 or not, at frame 0 (the PAL overlay reset) or later.
    for near in [false, true] {
        for mode in [4u32, 2] {
            for frame in [0u32, 1, 0x20] {
                for level in [0u32, 0x100] {
                    let mut m = base.clone();
                    let records = m.lw(UNIT_RECORDS);
                    let path = records + 30 * 0x28;
                    m.sw(records + 0x14, path);
                    m.sb(path, 9);
                    let player = entity(&mut m, 0);
                    if near {
                        for i in 0..3 {
                            let p = m.lw(0x800b_2078 + 4 * i);
                            m.sw(player + 8 + 4 * i, p);
                        }
                    }
                    let state = SLOT_STATES + 0x20 * 4;
                    m.sw(state, 1);
                    m.sw(GP + 0x484, level);
                    let gameplay = m.lw(GAMEPLAY);
                    m.sw(gameplay + 4, mode);
                    m.sw(gameplay + 0xc, frame);
                    let label = format!("800522b0 kind 9 near {near} mode {mode} frame {frame}");
                    changed += agree(&label, &m, 0x8005_22b0, [0, 0, 0, 0], true);
                    cases += 1;
                }
            }
        }
    }
    // Kind 4 arriving in phase 3 (distance 0) on mission 0x2B or not, with
    // the event flags 0x40 / 0x80 set or clear.
    for mission in [0x2bu16, 1] {
        for flags in [0x40u32, 0x80] {
            for events in [0u32, 0xc0] {
                let mut m = base.clone();
                let records = m.lw(UNIT_RECORDS);
                let path = records + 30 * 0x28;
                m.sw(records + 0x14, path);
                m.sb(path, 4);
                m.sw(path + 8, 0x100);
                let e = entity(&mut m, 0x20);
                for i in 0..3 {
                    let p = m.lw(e + 8 + 4 * i);
                    m.sw(path + 0x18 + 4 * i, p);
                    m.sw(path + 0x24 + 4 * i, p);
                }
                m.sb(e + 0x58, 3);
                let word = m.lw(e) & !0xc0;
                m.sw(e, word | flags);
                m.sw(SLOT_STATES + 0x20 * 4, 1);
                let context = m.lw(CONTEXT);
                m.sh(context + 0x38c, mission);
                m.sw(GP + 0x490, events);
                let label = format!(
                    "800522b0 kind 4 arrival mission {mission:#x} flags {flags:#x} events {events:#x}"
                );
                changed += agree(&label, &m, 0x8005_22b0, [0, 0, 0, 0], true);
                cases += 1;
            }
        }
    }
    println!("unit motion: {cases} oracle cases, {changed} bytes changed");
    assert!(changed > 0);
}

/// The JP model ray 8009A944 (no capture reaches it; the dispatch arm runs
/// [`object_hit_jp`](ac3_flight_jp::jp::res_p1_player::object_hit_jp) with the
/// carried register 0, as the oracle's registers start): the models of the
/// flight-002 model table (`[[gp+0x2E0]+0x258]`) at random offsets ahead of
/// rays aimed at them or not, the vertex work area past the arena pointer.
#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn object_ray_matches_the_original() {
    let (mut cases, mut changed, mut hits) = (0, 0, 0);
    let mut rng = oracle::Rng(0x5eed_0008);
    let (base, _) = entry_state("flight-002", 0x8005_1414);
    let in_ram = |a: u32| (0x8001_0000..0x801f_0000).contains(&a) && a & 3 == 0;
    for kind in 0..0x40u32 {
        let mut m = base.clone();
        let gameplay = m.lw(GAMEPLAY);
        let models = m.lw(gameplay + 0x258);
        if !in_ram(models) {
            break;
        }
        let model = m.lw(models + kind * 4);
        if !in_ram(model) {
            continue;
        }
        let vertices = m.lw(model + 4);
        let (high, low) = (m.lw(model + 0x10), m.lw(model + 0xc));
        if !in_ram(vertices) || !in_ram(high) || !in_ram(low) {
            continue;
        }
        for case in 0..12u32 {
            let mut m = base.clone();
            let context = m.lw(CONTEXT);
            let buffer = (m.lw(context + 0x1c) + 0x100) & !0xf;
            let (delta, out, offset, work) = (buffer, buffer + 0x10, buffer + 0x20, buffer + 0x40);
            let depth = 0x80 + rng.next() % 0x800;
            let spread =
                |rng: &mut oracle::Rng, range: u32| (rng.next() % range).wrapping_sub(range / 2);
            m.sh(offset, spread(&mut rng, 0x80) as u16);
            m.sh(offset + 2, spread(&mut rng, 0x80) as u16);
            m.sh(offset + 4, depth as u16);
            let aim = if case % 3 == 2 { 0x1000 } else { 0x40 };
            m.sw(delta, spread(&mut rng, aim));
            m.sw(delta + 4, spread(&mut rng, aim));
            m.sw(delta + 8, depth * 2);
            let sp = m.sp;
            m.sw(sp + 0x10, vertices);
            m.sw(sp + 0x14, offset);
            let label = format!("8009a944 model {kind:#x} case {case}");
            let range = extent(0x8009_a944);
            let args = [delta, out, work, model];
            changed += forced(&isolation(None), || {
                oracle::diff(&label, &m, range, args, |m| {
                    let v0 = dispatch::call(m, 0x8009_a944, args);
                    if (v0 as i32) < 0x7fff {
                        hits += 1;
                    }
                    Some(v0)
                })
            });
            cases += 1;
        }
    }
    println!("object ray: {cases} oracle cases ({hits} hits), {changed} bytes changed");
    assert!(cases > 0 && hits > 0);
}

/// The JP analog-pad control step 8006DD5C (no capture uses an analog
/// pad): random axis masks, axes, buttons, attitudes (pitch, heading, bank
/// around the 0x800 / 0x1000 thresholds), motion rates, the state factor
/// +0x4C, and the rates and controls it updates; the stack words +0x10
/// entity, +0x14 motion, +0x18 state.
#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn analog_control_matches_the_original() {
    const GP: u32 = ac3_flight_jp::GP;
    const ANGLES: [i32; 12] = [
        0, 0x3ff, 0x400, 0x7ff, 0x800, 0x801, 0xa00, 0xfff, 0x1000, 0x1001, 0x1800, 0x2000,
    ];
    let (mut cases, mut changed) = (0, 0);
    let mut rng = oracle::Rng(0x5eed_0009);
    let (base, _) = entry_state("flight-002", 0x8007_29dc);
    for case in 0..800u32 {
        let mut m = base.clone();
        let context = m.lw(CONTEXT);
        let input = context + 0x120;
        let pick = |rng: &mut oracle::Rng| {
            let angle = ANGLES[rng.next() as usize % ANGLES.len()];
            if rng.next() & 1 != 0 {
                angle.wrapping_neg()
            } else {
                angle
            }
        };
        m.sw(input + 8, (rng.next() << 9) & 0x7f80_0000);
        m.sw(input + 0x18, rng.next() & 0xf);
        for offset in [0x1cu32, 0x20, 0x24, 0x28] {
            m.sw(input + offset, (rng.next() % 0x2400).wrapping_sub(0x1200));
        }
        let entity = entity(&mut m, 0);
        for offset in [0x18u32, 0x1c] {
            m.sh(entity + offset, pick(&mut rng) as u16);
        }
        m.sh(entity + 0x1a, rng.next() as u16);
        let motion = m.lw(MOTION_BLOCK);
        m.sw(motion, rng.next() % 0x200);
        m.sw(motion + 4, rng.next() % 0x200);
        let state = m.lw(PLAYER_STATE);
        m.sh(state + 0x4c, (rng.next() % 0x2000) as u16);
        let rates = m.lw(GP + 0x514);
        let out = m.lw(CONTROL_OUT);
        let controls = m.lw(CONTROLS);
        for i in 0..4 {
            m.sh(
                rates + 2 * i,
                (rng.next() % 0x2400).wrapping_sub(0x1200) as u16,
            );
        }
        for i in 0..5 {
            m.sh(
                controls + 2 * i,
                (rng.next() % 0x2400).wrapping_sub(0x1200) as u16,
            );
        }
        let sp = m.sp;
        m.sw(sp + 0x10, entity);
        m.sw(sp + 0x14, motion);
        m.sw(sp + 0x18, state);
        let label = format!("8006dd5c case {case}");
        changed += agree(&label, &m, 0x8006_dd5c, [input, rates, out, controls], true);
        cases += 1;
    }
    println!("analog control: {cases} oracle cases, {changed} bytes changed");
    assert!(changed > 0);
}

/// The four near unit routines left unpatched (80051A54, 80051BA0,
/// 80051E34, 80052000): their JP instructions differ from PAL only in the
/// `v0` they leave (`addu v0, zero, zero` moved into or out of delay slots)
/// and in scheduling, and the translations return nothing. Their generated
/// translations (each excluded from the forcing, so it runs natively)
/// against the JP instructions, from random unit states: motion and wreck
/// descriptors (in the records of units 29..31; nonzero step durations, as
/// a zero one traps in the original, `break 7`, which the translations do
/// not model), state bits, entity steps, times, rates and flags, gameplay
/// modes.
#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn unpatched_unit_routines_match_the_original() {
    const ROUTINES: [u32; 4] = [0x8005_1a54, 0x8005_1ba0, 0x8005_1e34, 0x8005_2000];
    let (mut cases, mut changed) = (0, 0);
    let mut rng = oracle::Rng(0x5eed_000a);
    let (base, _) = entry_state("flight-002", 0x8005_1414);
    for case in 0..600u32 {
        let (mut m, unit, _) = unit_case(&base, &mut rng);
        let records = m.lw(UNIT_RECORDS);
        let record = records + unit * 0x28;
        let descriptor = records + 29 * 0x28;
        for i in 0..8 {
            m.sw(descriptor + 4 * i, (rng.next() & 0x00ff_00ff) | 0x0001_0001);
        }
        m.sb(descriptor + 2, (rng.next() % 4) as u8);
        m.sb(descriptor + 3, (1 + rng.next() % 4) as u8);
        m.sb(
            descriptor + 0xe,
            [0u8, 0xfe, 0xff, 3][rng.next() as usize % 4],
        );
        m.sw(
            record + 0x18,
            if rng.next().is_multiple_of(4) {
                0
            } else {
                descriptor
            },
        );
        m.sw(record + 0x10, 0);
        m.sw(record + 0x1c, 0);
        let e = entity(&mut m, unit + 0x20);
        m.sb(e + 0x59, (rng.next() % 5) as u8);
        m.sw(e + 0x60, rng.next() % 0x40);
        m.sw(e + 0x68, (rng.next() % 0x20).wrapping_sub(0x10));
        m.sh(e + 0x4e, (rng.next() % 3) as u16);
        let flags = m.lw(e);
        m.sw(e, (flags & !0xc000) | (rng.next() & 0xc000));
        let bits = (rng.next() & 0x2_0f01) | (rng.next() & 0x0030_0000);
        m.sw(SLOT_STATES + (unit + 0x20) * 4, bits);
        let gameplay = m.lw(GAMEPLAY);
        m.sw(gameplay + 4, if rng.next() & 1 != 0 { 4 } else { 2 });
        let routine = ROUTINES[case as usize % 4];
        let spec = format!("{},-{routine:x}", isolation(None));
        let label = format!("{routine:08x} case {case} unit {unit}");
        changed += agree_forced(&label, &m, routine, [unit, 0, 0, 0], true, &spec);
        cases += 1;
    }
    println!("unpatched unit routines: {cases} oracle cases, {changed} bytes changed");
    assert!(changed > 0);
}

/// The module loader of the loading state (8009B6B8 begin, 8009B7CC poll;
/// no capture loads a module): the display phases, both buffers, every
/// fade mode, the read active or not, pending or completed.
#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn module_loader_matches_the_original() {
    const GP: u32 = ac3_flight_jp::GP;
    let (mut cases, mut changed) = (0, 0);
    let (base, _) = entry_state("flight-002", 0x8007_29dc);
    for phase in [0u32, 1, 2] {
        for buffer in [0u32, 1] {
            for module in [3u32, 0x20] {
                let mut m = base.clone();
                let display = m.lw(GP + 0x2d4);
                m.sw(display + 4, phase);
                let context = m.lw(CONTEXT);
                m.sw(context + 4, buffer);
                let address = (m.lw(context + 0x1c) + 0x1000) & !3;
                let label = format!("8009b6b8 phase {phase} buffer {buffer} module {module:#x}");
                changed += agree(&label, &m, 0x8009_b6b8, [module, address, 0, 0], false);
                cases += 1;
            }
        }
    }
    for phase in [0u32, 1] {
        for mode in [0u32, 1, 2, 3] {
            for pending in [0u32, 1] {
                for active in [false, true] {
                    let mut m = base.clone();
                    let display = m.lw(GP + 0x2d4);
                    m.sw(display + 4, phase);
                    m.sw(GP + 0x2bc, mode);
                    m.sw(GP + 0x554, pending);
                    // The active CD request's file (8002CAD0) is the module
                    // being loaded, or another one.
                    let file = dispatch::call(&mut m.clone(), 0x8002_cad0, [0; 4]);
                    m.sw(GP + 0x2c0, if active { file } else { file ^ 0x55 });
                    let label = format!(
                        "8009b7cc phase {phase} mode {mode} pending {pending} active {active}"
                    );
                    changed += agree(&label, &m, 0x8009_b7cc, [0; 4], false);
                    cases += 1;
                }
            }
        }
    }
    println!("module loader: {cases} oracle cases, {changed} bytes changed");
    assert!(changed > 0);
}

/// 80070A80 hands its speed (s4) through 8009B4EC to the JP model ray
/// 8009A944 as the carried vertex register. Each case puts one model object
/// (random model, offset and approach velocity) in contact with the player
/// and runs 80070A80 three ways: the patches (`object_ray_jp` with the
/// speed), the original instructions of the whole chain 80070A80 -> 8009B4EC
/// -> 8009A944 (interpreted together, so the registers carry over), and
/// for contrast 80070A80 and 8009B4EC interpreted with the model ray
/// patched at carry 0 (the dispatch arm). The first two must agree; the
/// count of cases where the carry-0 run differs shows the carry matters.
#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn entity_contacts_pass_the_model_ray_carry() {
    let (mut cases, mut hits, mut carry_matters) = (0, 0, 0);
    let mut rng = oracle::Rng(0x5eed_000b);
    let (base, _) = entry_state("flight-002", 0x8007_29dc);
    let in_ram = |a: u32| (0x8001_0000..0x801f_0000).contains(&a) && a & 3 == 0;
    let chain = {
        let mut spec = String::from("game");
        for address in patched() {
            if address != 0x8007_0a80 && address != 0x8009_a944 {
                spec.push_str(&format!(",-{address:x}"));
            }
        }
        spec
    };
    let run = |m: &Machine, spec: &str, player: u32, state: u32| {
        let mut m = m.clone();
        forced(spec, || {
            dispatch::call(&mut m, 0x8007_0a80, [player, state, 0, 0])
        });
        let fault = m.take_fault();
        (m, fault)
    };
    for case in 0..240u32 {
        let mut m = base.clone();
        let gameplay = m.lw(GAMEPLAY);
        let models = m.lw(gameplay + 0x258);
        let kind = rng.next() % 0x30;
        let model = m.lw(models + kind * 4);
        if !in_ram(model)
            || !in_ram(m.lw(model + 4))
            || !in_ram(m.lw(model + 0xc))
            || !in_ram(m.lw(model + 0x10))
        {
            continue;
        }
        let player = entity(&mut m, 0);
        let slot = 1 + rng.next() % 7;
        let object = entity(&mut m, slot);
        let flags = m.lw(object);
        m.sw(object, flags | 0x5420);
        let byte = m.lbu(object + 5);
        m.sb(object + 5, byte & !6);
        m.sb(object + 4, kind as u8);
        m.sh(object + 0x4c, 0x7fff);
        let depth = 0x80 + rng.next() % 0x600;
        let spread =
            |rng: &mut oracle::Rng, range: u32| (rng.next() % range).wrapping_sub(range / 2);
        let offset = [spread(&mut rng, 0x80), spread(&mut rng, 0x80), depth];
        let aim = if case % 3 == 2 { 0x400 } else { 0x40 };
        for (i, d) in offset.into_iter().enumerate() {
            let i = i as u32;
            let position = m.lw(player + 8 + 4 * i);
            m.sw(object + 8 + 4 * i, position.wrapping_add(d << 12));
            let velocity = (d.wrapping_mul(2)).wrapping_add(spread(&mut rng, aim)) << 12;
            m.sw(player + 0x38 + 4 * i, velocity);
        }
        let state = m.lw(PLAYER_STATE);
        let sp = m.sp;
        let (native, nf) = run(&m, &isolation(None), player, state);
        let (original, of) = run(&m, &chain, player, state);
        let (zero, _) = run(&m, &isolation(Some(0x8007_0a80)), player, state);
        let label = format!("80070a80 case {case} model {kind:#x}");
        assert_eq!(nf, of, "{label}: faults");
        if let Some(d) = frame_differences(&native, &original, sp) {
            panic!("{label}: the patches differ from the original chain: {d}");
        }
        if m.lw(state + 0x10) & 0x20 == 0 && native.clone().lw(state + 0x10) & 0x20 != 0 {
            hits += 1;
        }
        if frame_differences(&zero, &original, sp).is_some() {
            carry_matters += 1;
        }
        cases += 1;
    }
    // A model of one split face (flag 0x200) whose first normal faces away
    // and second faces the ray: the second triangle's plane point is the
    // carried register, here the speed (an address in low RAM) or 0, as
    // the first face the ray meets. The model, vertices and face lists are
    // written past the arena pointer, the normals at the last offsets of
    // the normal table.
    for case in 0..48u32 {
        let mut m = base.clone();
        let context = m.lw(CONTEXT);
        let block = (m.lw(context + 0x1c) + 0x4000) & !0xf;
        let (model, vertices, faces, empty) = (block, block + 0x40, block + 0x100, block + 0x180);
        m.sw(model + 4, vertices);
        m.sw(model + 0xc, empty);
        m.sw(model + 0x10, faces);
        m.sw(empty, 0);
        let size = 0x20 + rng.next() % 0x40;
        let corners = [(-1i32, -1i32), (1, -1), (-1, 1), (1, 1), (0, 0), (0, 0)];
        m.sw(vertices, 6);
        for (i, (x, y)) in corners.into_iter().enumerate() {
            let group = vertices + 4 + (i as u32 / 3) * 0x14;
            let k = i as u32 % 3;
            let x = x.wrapping_mul(size as i32) as u32;
            let y = y.wrapping_mul(size as i32) as u32;
            m.sw(group + 4 * k, (x & 0xffff) | (y << 16));
            m.sh(group + 0xc + 2 * k, 0);
        }
        let gameplay = m.lw(GAMEPLAY);
        let normals = m.lw(gameplay + 0x25c);
        let (away, toward) = (0xffe0u32, 0xffe8u32);
        m.sw(normals + away, 0);
        m.sw(normals + away + 4, 0x1000);
        m.sw(normals + toward, 0);
        m.sw(normals + toward + 4, 0xf000);
        m.sw(faces, 1);
        m.sw(faces + 4, 0x200);
        m.sh(faces + 8, away as u16);
        m.sh(faces + 0xa, toward as u16);
        m.sw(faces + 0x14, 0x0302_0100);
        let models = m.lw(gameplay + 0x258);
        let kind = 1 + case % 8;
        m.sw(models + kind * 4, model);
        let player = entity(&mut m, 0);
        let object = entity(&mut m, 1 + case % 7);
        let flags = m.lw(object);
        m.sw(object, flags | 0x5420);
        let byte = m.lbu(object + 5);
        m.sb(object + 5, byte & !6);
        m.sb(object + 4, kind as u8);
        m.sh(object + 0x4c, 0x7fff);
        let depth = 0x40 + rng.next() % 0x100;
        // The speed (the carry) as an address: a plane point there.
        let speed = 2 * depth;
        for i in 0..3u32 {
            let value = if i == 2 {
                rng.next() % 0x100
            } else {
                rng.next() % 0x40
            };
            m.sh(0x8000_0000 + speed + 2 * i, value as u16);
        }
        for i in 0..3u32 {
            let position = m.lw(player + 8 + 4 * i);
            let d = if i == 2 { depth } else { 0 };
            m.sw(object + 8 + 4 * i, position.wrapping_add(d << 12));
            m.sw(player + 0x38 + 4 * i, (2 * d) << 12);
        }
        let state = m.lw(PLAYER_STATE);
        let sp = m.sp;
        let (native, nf) = run(&m, &isolation(None), player, state);
        let (original, of) = run(&m, &chain, player, state);
        let (zero, _) = run(&m, &isolation(Some(0x8007_0a80)), player, state);
        let label = format!("80070a80 split face case {case} depth {depth:#x}");
        assert_eq!(nf, of, "{label}: faults");
        if let Some(d) = frame_differences(&native, &original, sp) {
            panic!("{label}: the patches differ from the original chain: {d}");
        }
        if frame_differences(&zero, &original, sp).is_some() {
            carry_matters += 1;
        }
        cases += 1;
    }
    println!(
        "entity contacts: {cases} cases, {hits} with a contact, {carry_matters} where a zero carry \
         would differ"
    );
    assert!(cases > 0 && carry_matters > 0);
}
