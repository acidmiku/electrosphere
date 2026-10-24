//! The development interpreter fallback in the JP crate. From the JP Redux
//! probe RAM at the dispatch of overlay 1's first callback (800CCE98, state
//! 4, frame 1334), the state dispatcher (`mainloop::dispatch_state`, the
//! re-addressed 80026350) runs frame after frame: translations natively,
//! whatever is still untranslated interpreted, every call between them
//! through `dispatch::call`.
//!
//! What it proves does not depend on how much is still untranslated: two
//! translated routines are forced through the interpreter (the JP
//! original's instructions), overlay 1's initialisation 800CCE98 (reached
//! through the state dispatcher) and the resident arena allocator 80027410
//! (reached from the interpreted initialisation, through `dispatch::call`
//! again). Both must run interpreted without a fault, the fallback's report
//! must be well formed, and the run must leave the RAM (but the stack, where
//! the original saves its registers and a translation need not) and the
//! scratchpad of the run with the translations. Drive the dispatcher, not a
//! callback: a state's substate-0 callback initialises once and returns
//! NEXT_SUBSTATE (called again, it allocates twice and the arena overflows,
//! in the JP original too). Feature `dev-fallback`:
//! `cargo test --release -p ac3-flight-jp --features dev-fallback --test fallback_smoke -- --ignored`.
#![cfg(feature = "dev-fallback")]

use ac3_fallback::{Fallback, Version};
use ac3_flight_jp::{Machine, SmallData, common, mainloop};

/// Overlay 1's initialisation (state 4, substate 0; the patch layer
/// translates it).
const INIT: u32 = 0x800c_ce98;
/// The resident arena allocator (exact; `arena::alloc` translates it).
const ALLOCATOR: u32 = 0x8002_7410;
/// The routines forced through the interpreter.
const FORCED: &str = "ov1:800cce98,80027410";
/// The stack: above the arena's limit (801FB700) up to the end of RAM.
const STACK: (u32, u32) = (0x801f_b700, 0x8020_0000);

/// A run's machine, the fallback's report, its summary and the interpreted
/// calls and instructions of [`INIT`] and [`ALLOCATOR`].
type Run = (Machine, serde_json::Value, String, [Option<(u64, u64)>; 2]);

/// Run `frames` frames of the state dispatcher from the probe RAM with the
/// fallback installed (`force`: routines forced through the interpreter).
fn run(frames: u32, force: &str) -> Run {
    let home = std::env::var("HOME").unwrap();
    let ram = std::fs::read(format!("{home}/ac3-jp/analysis/redux/run1/probe-1/ram.bin")).unwrap();
    let mut m = Machine::from_images(&ram, &[0; 0x400]).unwrap();
    m.sp = 0x801f_ffb0;
    assert_eq!(common::loaded_module(&mut m), 1);
    let mut fallback = Fallback::new(Version::Jp);
    if !force.is_empty() {
        assert_eq!(fallback.force(force), Ok(2), "force {force}");
    }
    let fallback = ac3_fallback::install(fallback);
    if let Some(lines) = std::env::var("TRACE").ok().and_then(|t| t.parse().ok()) {
        fallback.set_trace(lines);
    }
    for frame in 0..frames {
        m.host_calls.clear();
        let v0 = mainloop::dispatch_state(&mut m);
        if let Err(fault) = m.take_fault() {
            panic!("frame {frame} (forced: {force:?}): v0 {v0:#x}, fault {fault}");
        }
    }
    let reached = [fallback.reached(INIT), fallback.reached(ALLOCATOR)];
    let (report, summary) = (fallback.report(), fallback.summary());
    drop(fallback);
    ac3_fallback::uninstall();
    (m, report, summary, reached)
}

#[test]
#[ignore = "requires ~/ac3-jp/analysis/redux/run1/probe-1/ram.bin"]
fn jp_overlay_1_runs_with_the_fallback() {
    let frames = std::env::var("FRAMES")
        .ok()
        .and_then(|f| f.parse().ok())
        .unwrap_or(120);
    let (native, _, native_summary, _) = run(frames, "");
    let (mut forced, report, summary, reached) = run(frames, FORCED);
    let context = forced.gw(common::CONTEXT);
    eprintln!(
        "{frames} frames: state {} substate {}, arena pointer {:#010x}",
        forced.lw(context + 0x2c),
        forced.lw(context + 0x30),
        forced.lw(context + 0x1c)
    );
    eprintln!("translations only: {native_summary}");
    eprintln!("forced {FORCED}: {summary}");
    for r in report["queue"].as_array().unwrap().iter().take(25) {
        eprintln!(
            "  {} {} {} {:>6} B {:>8} calls {:>10} instr  {}",
            r["module"],
            r["address"],
            r["class"],
            r["bytes"],
            r["calls"],
            r["instructions"],
            r["name"]
        );
    }
    // the report is well formed, without a fault
    for key in ["queue", "blocked", "unknown", "stubs", "faults"] {
        assert!(report[key].is_array(), "report[{key}]: {}", report[key]);
    }
    assert!(
        report["routines"].is_u64(),
        "report[routines]: {}",
        report["routines"]
    );
    assert!(
        report["faults"].as_array().unwrap().is_empty(),
        "faults: {}",
        report["faults"]
    );
    // both forced routines ran interpreted: the initialisation once
    let [init, allocator] = reached;
    let (calls, instructions) = init.expect("the forced initialisation was not interpreted");
    assert!(
        calls == 1 && instructions > 0,
        "initialisation: {calls} calls"
    );
    let (calls, instructions) = allocator.expect("the forced allocator was not interpreted");
    assert!(calls > 0 && instructions > 0);
    assert!(report["routines"].as_u64().unwrap() >= 2);
    // and the interpreted originals left what the translations leave
    let differ: Vec<u32> = (0..0x20_0000u32)
        .map(|i| 0x8000_0000 + i)
        .filter(|a| !(STACK.0..STACK.1).contains(a))
        .filter(|&a| {
            native.ram()[(a & 0x1f_ffff) as usize] != forced.ram()[(a & 0x1f_ffff) as usize]
        })
        .take(8)
        .collect();
    assert!(differ.is_empty(), "RAM differs at {differ:08x?}");
    assert_eq!(native.scratch(), forced.scratch());
    eprintln!(
        "initialisation interpreted once, allocator {calls} times ({instructions} instructions); \
         RAM outside the stack identical"
    );
}
