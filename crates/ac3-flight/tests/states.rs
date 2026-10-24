//! Resident states 0/1 and 5 and the overlay module loader
//! (`ac3_flight::states`) against original entry/return snapshots.
//!
//! Captures (`analysis/flight-runtime`, `.ovmisc/snapshots.lua` observer):
//! - `ovmisc-006`/`ovmisc-009`: natural title attract mode from
//!   `overlay-entry.sav` (CROSS at frame 301, idle): state 5 at frame 1358
//!   plays the opening movie; START at 1420 skips it; state 4 (module 1)
//!   is loaded again from frame 1425.
//! - `ovmisc-007`/`ovmisc-008`: new game from `continuation-001/final.sav`
//!   (DOWN, CROSS): module 2 is loaded from frame 29 to 84.
//!
//! Routines translated elsewhere but not yet in this workspace (the
//! loading fade 80092350, ClearImage 800A67E8, ResetCallback 800A0778) are
//! replayed from their own original snapshots in the same capture: the
//! test invoker applies the bytes their entry/return pair changed and
//! returns their `v0` (see [`Recorded`]). Everything else runs natively.
//!
//! Interrupts: the original's VBlank, CD, SPU-timer and DMA handlers run
//! inside long callbacks; their RAM is listed in [`INTERRUPT_OWNED`] and
//! counted, not compared. Every other byte outside the stack, the
//! scratchpad, the GTE registers and `v0` must match.
mod support;

use ac3_flight::{
    Machine, dispatch,
    machine::{Device, HostCall},
    snapshot::{self, Snapshot},
    states,
};
use std::path::{Path, PathBuf};
use support::oracle;

fn root() -> PathBuf {
    PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR"))
}
fn capture(name: &str) -> PathBuf {
    root()
        .join("analysis/flight-runtime")
        .join(name)
        .join("snapshots.jsonl")
}

/// RAM written by interrupt handlers (and the BIOS) during the long
/// callbacks, never by the routine's own call tree.
const INTERRUPT_OWNED: &[(u32, u32, &str)] = &[
    (0x8000_0000, 0x8001_0000, "BIOS kernel RAM"),
    (0x800b_256c, 0x800b_2570, "sound interrupt flag"),
    (
        0x800b_2f2c,
        0x800b_3f44,
        "libetc interrupt context and stack",
    ),
    // VSync's reading of root counter 1 (a hardware counter; the host's
    // VSync supplies it).
    (0x800b_2ec4, 0x800b_2ec8, "VSync root-counter reading"),
    (0x800b_3ff4, 0x800b_3ff8, "VSync counter"),
    (0x800b_4010, 0x800b_4014, "GPU DMA callback slot"),
    (0x800b_990c, 0x800b_9914, "libgpu queue head/tail"),
    // libgpu's copy of I_MASK (SetIntrMask's result in 800A83FC): the
    // interrupt-mask register is device state the host supplies.
    (0x800b_9914, 0x800b_9918, "libgpu saved interrupt mask"),
    (0x800b_9920, 0x800b_9928, "libgpu timeout"),
    (0x800c_5bcc, 0x800c_5bce, "sound driver tick state"),
    (0x800c_5c70, 0x800c_5c72, "sound driver tick state"),
    (0x800c_8230, 0x800c_9a30, "libgpu queue entries"),
];
/// CD stream state the CD interrupt (data-ready callbacks of
/// `cdstream.rs`, libcd) updates while a movie streams: the CD monitor and
/// driver blocks (`[gp+0x34]`, `[gp+0x38]`), the ring state and sector
/// header buffer (`[gp+0x40]`, `[gp+0x3C]`); the ring itself is added per
/// test from the decoder ([`movie_ring`]).
const CD_STREAM: &[(u32, u32, &str)] = &[
    (0x800b_f150, 0x800b_f1d0, "CD monitor and driver state"),
    (0x800c_31d0, 0x800c_3420, "CD ring state and sector headers"),
];
/// The movie's stream ring (decoder +0x11040, +0x1103C bytes): sectors
/// arrive by CD DMA in the interrupt.
fn movie_ring(entry: &Snapshot) -> (u32, u32, &'static str) {
    let mut m = entry.machine();
    let decoder = m.lw(0x800b_a9f4 + 0x4dc);
    let ring = m.lw(decoder + 0x1_1040);
    let size = m.lw(decoder + 0x1_103c);
    (ring, ring + size, "movie stream ring (CD DMA)")
}
fn owner(address: u32, extra: &[(u32, u32, &'static str)]) -> Option<&'static str> {
    INTERRUPT_OWNED
        .iter()
        .chain(extra)
        .find(|(start, end, _)| (*start..*end).contains(&address))
        .map(|(_, _, name)| *name)
}

/// `start..end` runs of consecutive addresses (at most 32).
fn ranges(addresses: &[u32]) -> String {
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for &a in addresses {
        match runs.last_mut() {
            Some((_, end)) if *end == a => *end = a + 1,
            _ => runs.push((a, a + 1)),
        }
    }
    runs.iter()
        .take(32)
        .map(|(s, e)| format!("{s:08x}..{e:08x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

struct Pair {
    entry: Snapshot,
    exit: Snapshot,
}
fn pair(index: &Path, callee: u32, occurrence: usize) -> Pair {
    let directory = index.parent().unwrap();
    let all = snapshot::read_index(index).unwrap();
    let entry = all
        .iter()
        .filter(|s| s.callee == callee && s.stage == "entry")
        .nth(occurrence)
        .unwrap_or_else(|| panic!("{callee:08x}: no entry #{occurrence}"));
    let exit = all
        .iter()
        .find(|s| {
            s.callee == callee
                && s.stage == "return"
                && s.sequence > entry.sequence
                && s.ra == entry.ra
                && s.registers.gpr[29] == entry.registers.gpr[29]
        })
        .unwrap_or_else(|| panic!("{callee:08x}: no return"));
    Pair {
        entry: Snapshot::load(directory, entry).unwrap(),
        exit: Snapshot::load(directory, exit).unwrap(),
    }
}
fn arguments(entry: &Snapshot) -> [u32; 4] {
    std::array::from_fn(|i| entry.gpr(4 + i))
}

/// One recorded original call: its arguments, the bytes it changed and
/// its `v0`.
struct Call {
    callee: u32,
    args: [u32; 4],
    ram: Vec<(usize, u8)>,
    scratch: Vec<(usize, u8)>,
    gte: ([u32; 32], [u32; 32]),
    v0: u32,
    used: bool,
}
/// The recorded calls of `callees` made inside one call of a routine (the
/// `occurrence`th entry of `parent` up to its return), in call order.
struct Recorded(Vec<Call>, Vec<u32>);
impl Recorded {
    fn load(index: &Path, parent: u32, occurrence: usize, callees: &[u32]) -> Self {
        let directory = index.parent().unwrap();
        let all = snapshot::read_index(index).unwrap();
        let p = pair(index, parent, occurrence);
        let window = p.entry.info.sequence..p.exit.info.sequence;
        let mut calls = Vec::new();
        for entry in all.iter().filter(|s| {
            s.stage == "entry" && callees.contains(&s.callee) && window.contains(&s.sequence)
        }) {
            let Some(exit) = all.iter().find(|s| {
                s.callee == entry.callee
                    && s.stage == "return"
                    && s.sequence > entry.sequence
                    && s.ra == entry.ra
                    && s.registers.gpr[29] == entry.registers.gpr[29]
            }) else {
                continue;
            };
            let a = Snapshot::load(directory, entry).unwrap();
            let b = Snapshot::load(directory, exit).unwrap();
            let stack = snapshot::STACK_START;
            calls.push(Call {
                callee: entry.callee,
                args: arguments(&a),
                ram: (0..stack)
                    .filter(|&i| a.ram[i] != b.ram[i])
                    .map(|i| (i, b.ram[i]))
                    .collect(),
                scratch: (0..a.scratch.len())
                    .filter(|&i| a.scratch[i] != b.scratch[i])
                    .map(|i| (i, b.scratch[i]))
                    .collect(),
                gte: (b.info.registers.cp2d, b.info.registers.cp2c),
                v0: b.gpr(2),
                used: false,
            });
        }
        Recorded(calls, callees.to_vec())
    }
    /// The invoker: a recorded callee replays the first unused recording
    /// with the same arguments (it must exist); other targets run natively.
    fn call(&mut self, m: &mut Machine, target: u32, args: [u32; 4]) -> u32 {
        if !self.1.contains(&target) {
            return dispatch::call(m, target, args);
        }
        let call = self
            .0
            .iter_mut()
            .find(|c| {
                let n = arity(target);
                c.callee == target && c.args[..n] == args[..n] && !c.used
            })
            .unwrap_or_else(|| panic!("{target:08x}{args:08x?}: no recording"));
        call.used = true;
        // Interrupt-owned bytes (libgpu queue, timeouts, ...) are not
        // replayed: the original queued transfers behind a busy GPU that
        // the host completes at once.
        for &(i, b) in &call.ram {
            let address = 0x8000_0000 + i as u32;
            if owner(address, &[]).is_none() {
                m.sb(address, b);
            }
        }
        for &(i, b) in &call.scratch {
            m.sb(0x1f80_0000 + i as u32, b);
        }
        (m.gte.data, m.gte.control) = call.gte;
        call.v0
    }
}

/// Result of a comparison: bytes the original changed and the differing
/// interrupt-owned bytes per owner.
struct Report {
    changed: usize,
    excluded: Vec<(&'static str, usize)>,
}
fn report(name: &str, r: &Report) {
    println!(
        "{name}: original changed {} bytes; interrupt-owned differences {:?}",
        r.changed, r.excluded
    );
}

/// Run `native` on the entry snapshot and compare with the return
/// snapshot: everything outside [`INTERRUPT_OWNED`] and the stack exactly,
/// the scratchpad, the GTE and (when given) `v0`.
fn check(
    index: &Path,
    callee: u32,
    occurrence: usize,
    native: impl FnOnce(&mut Machine, &Snapshot) -> Option<u32>,
) -> Report {
    check_owned(index, callee, occurrence, &[], native)
}

/// [`check`] with more interrupt-owned ranges.
fn check_owned(
    index: &Path,
    callee: u32,
    occurrence: usize,
    extra: &[(u32, u32, &'static str)],
    native: impl FnOnce(&mut Machine, &Snapshot) -> Option<u32>,
) -> Report {
    let p = pair(index, callee, occurrence);
    let mut m = p.entry.machine();
    let v0 = native(&mut m, &p.entry);
    m.take_fault()
        .unwrap_or_else(|f| panic!("{callee:08x}#{occurrence}: {f}"));
    let d = snapshot::compare(&m, &p.exit);
    assert!(
        d.scratch.is_empty() && d.gte_data.is_empty() && d.gte_control.is_empty(),
        "{callee:08x}#{occurrence}: {}",
        d.summary()
    );
    let foreign: Vec<u32> = d
        .ram
        .iter()
        .copied()
        .filter(|&a| owner(a, extra).is_none())
        .collect();
    assert!(
        foreign.is_empty(),
        "{callee:08x}#{occurrence}: {} bytes differ outside interrupt-owned RAM: {}",
        foreign.len(),
        ranges(&foreign)
    );
    if let Some(v0) = v0 {
        assert_eq!(v0, p.exit.gpr(2), "{callee:08x}#{occurrence}: v0");
    }
    let mut excluded: Vec<(&'static str, usize)> = vec![];
    for name in d.ram.iter().filter_map(|&a| owner(a, extra)) {
        match excluded.iter_mut().find(|(n, _)| *n == name) {
            Some((_, count)) => *count += 1,
            None => excluded.push((name, 1)),
        }
    }
    let changed = snapshot::changed_bytes(&p.entry, &p.exit);
    assert!(changed > 0 || v0.is_some(), "{callee:08x}: vacuous");
    Report { changed, excluded }
}

/// Test device: StoreImage (host call 800A6970) delivers the VRAM words
/// the original stored, taken from the return snapshot.
struct StoreFromExit(Vec<u8>);
impl Device for StoreFromExit {
    fn call(&mut self, ram: &mut [u8], _scratch: &mut [u8], call: HostCall) -> Option<u32> {
        if call.function == 0x800a_6970 {
            let [rect, pixels, ..] = call.args;
            let half = |a: u32| {
                let i = (a & 0x1f_ffff) as usize;
                u32::from(u16::from_le_bytes([ram[i], ram[i + 1]]))
            };
            let bytes = (half(rect + 4) * half(rect + 6)).div_ceil(2) * 4;
            let start = (pixels & 0x1f_ffff) as usize;
            let end = start + bytes as usize;
            ram[start..end].copy_from_slice(&self.0[start..end]);
        }
        None
    }
}

/// Test device: the MDEC output DMA (host call 800AB118) is still running
/// when DecDCTout returns, as in the original's interrupt-driven flow.
struct MdecRunning;
impl Device for MdecRunning {
    fn call(&mut self, _ram: &mut [u8], _scratch: &mut [u8], call: HostCall) -> Option<u32> {
        (call.function == 0x800a_b118).then_some(0)
    }
}

/// The movie's two slice buffers (decoder +0x10/+0x14, slice size words
/// each): the MDEC writes them by DMA while the slice callback runs.
fn movie_slices(entry: &Snapshot) -> [(u32, u32, &'static str); 2] {
    let mut m = entry.machine();
    let decoder = m.lw(0x800b_a9f4 + 0x4dc);
    let bytes = (m.lh(decoder + 0x1_1036) as i32 as u32) * 4;
    let a = m.lw(decoder + 0x10);
    let b = m.lw(decoder + 0x14);
    [
        (a, a + bytes, "MDEC slice DMA"),
        (b, b + bytes, "MDEC slice DMA"),
    ]
}

/// ClearImage's packet words read from the GPU (`sdk.rs` clear transfer:
/// the draw-mode status and the drawing-area words of 800A83A8), device
/// values the host supplies.
const CLEAR_STATUS: &[(u32, u32, &str)] = &[
    (
        0x800c_80f4,
        0x800c_80f8,
        "ClearImage draw mode (GPU status)",
    ),
    (
        0x800c_810c,
        0x800c_8118,
        "ClearImage drawing area (GPU status)",
    ),
];

/// Arguments a replayed routine takes (the rest are stale registers).
fn arity(target: u32) -> usize {
    match target {
        RESET_CALLBACK | STREAM_SECTOR => 0,
        _ => 4,
    }
}

/// Pending externals replayed from recordings.
const RESET_CALLBACK: u32 = 0x800a_0778;
/// 800297B8 (`cdstream.rs`): a deferred stream sector.
const STREAM_SECTOR: u32 = 0x8002_97b8;

// ---- states 0/1 -------------------------------------------------------------------------

/// 80025BA4 is `mainloop.rs`'s; the loader expects state 4 / module 1.
#[test]
fn initial_state_selects_the_menus() {
    let mut m = Machine::default();
    assert_eq!(dispatch::call(&mut m, 0x8002_5ba4, [0; 4]), 0x6000_0004);
    assert!(m.take_fault().is_ok());
}

// ---- module loader ------------------------------------------------------------------------

#[test]
#[ignore = "needs analysis/flight-runtime/ovmisc-007"]
fn module_load_begin_matches_original() {
    let index = capture("ovmisc-007");
    let r = check(&index, 0x8009_9bc8, 0, |m, e| {
        let [a0, a1, ..] = arguments(e);
        let mut c = dispatch::call;
        Some(states::module_load_begin(m, &mut c, a0, a1))
    });
    report("80099BC8 (module 2, display phase 1)", &r);
    let r = check(&index, 0x8002_c490, 0, |m, e| {
        Some(dispatch::call(m, 0x8002_c490, arguments(e)))
    });
    report("8002C490", &r);
    let r = check(&index, 0x8009_9bbc, 0, |m, e| {
        dispatch::call(m, 0x8009_9bbc, arguments(e));
        None
    });
    report("80099BBC", &r);
}

/// Every captured 80099CD8 call of `name`; returns how many completed.
fn module_load_polls(name: &str) -> usize {
    let index = capture(name);
    let all = snapshot::read_index(&index).unwrap();
    let polls = all
        .iter()
        .filter(|s| s.callee == 0x8009_9cd8 && s.stage == "entry")
        .count();
    let mut complete = 0;
    for occurrence in 0..polls {
        let mut recorded = Recorded::load(&index, 0x8009_9cd8, occurrence, &[]);
        let mut result = 0;
        let r = check(&index, 0x8009_9cd8, occurrence, |m, _| {
            let mut c = |m: &mut Machine, t: u32, a: [u32; 4]| recorded.call(m, t, a);
            result = states::module_load_poll(m, &mut c);
            Some(result)
        });
        complete += usize::from(result != 0);
        report(&format!("{name} 80099CD8 #{occurrence} -> {result:#x}"), &r);
    }
    complete
}

#[test]
#[ignore = "needs analysis/flight-runtime/ovmisc-008"]
fn module_load_poll_matches_original() {
    assert!(
        module_load_polls("ovmisc-008") >= 1,
        "the completing poll was captured"
    );
}

#[test]
#[ignore = "needs analysis/flight-runtime/ovmisc-009"]
fn module_load_from_display_phase_0_matches_original() {
    let index = capture("ovmisc-009");
    let r = check(&index, 0x8009_9bc8, 0, |m, e| {
        Some(dispatch::call(m, 0x8009_9bc8, arguments(e)))
    });
    report("80099BC8 (module 1, display phase 0)", &r);
    let r = check(&index, 0x8003_606c, 0, |m, e| {
        Some(dispatch::call(m, 0x8003_606c, arguments(e)))
    });
    report("8003606C", &r);
    module_load_polls("ovmisc-009");
}

// ---- state 5 ----------------------------------------------------------------------------

#[test]
#[ignore = "needs analysis/flight-runtime/ovmisc-006"]
fn movie_state_callbacks_match_original() {
    let index = capture("ovmisc-006");
    let mut c = dispatch::call;
    let r = check(&index, 0x8005_63c8, 0, |m, _| {
        Some(states::movie_prepare(m, &mut c))
    });
    report("800563C8 (attract: movie)", &r);
    let r = check(&index, 0x8005_6538, 0, |m, _| {
        Some(states::movie_load_music(m, &mut c))
    });
    report("80056538", &r);
    for occurrence in 0..2 {
        let r = check(&index, 0x8005_65c0, occurrence, |m, _| {
            Some(states::movie_setup(m, &mut c))
        });
        report(&format!("800565C0 #{occurrence}"), &r);
    }
}

/// Recorded pending routines inside a call, as an invoker for `native`.
fn with_recorded(
    index: &Path,
    callee: u32,
    occurrence: usize,
    pending: &[u32],
    native: impl FnOnce(&mut Machine, &mut states::Invoker) -> u32,
) -> Report {
    let mut recorded = Recorded::load(index, callee, occurrence, pending);
    check(index, callee, occurrence, |m, _| {
        let mut c = |m: &mut Machine, t: u32, a: [u32; 4]| recorded.call(m, t, a);
        Some(native(m, &mut c))
    })
}

#[test]
#[ignore = "needs analysis/flight-runtime/ovmisc-010"]
fn movie_start_and_end_match_original() {
    let index = capture("ovmisc-010");
    let exit = pair(&index, 0x8005_65c0, 2).exit.ram;
    let r = with_recorded_owned(&index, 0x8005_65c0, 2, &[], CLEAR_STATUS, |m, c| {
        m.device = Some(Box::new(StoreFromExit(exit)));
        states::movie_setup(m, c)
    });
    report("800565C0 #2 (movie block, VRAM save and clear)", &r);
    let r = with_recorded(&index, 0x8005_5238, 0, &[], |m, _| {
        states::movie_decoder_reset(m)
    });
    report("80055238", &r);
    let r = with_recorded(&index, 0x8005_53b4, 0, &[], |m, c| {
        let p = pair(&index, 0x8005_53b4, 0);
        let [a0, a1, a2, a3] = arguments(&p.entry);
        let sp = p.entry.gpr(29);
        let stack = std::array::from_fn(|i| m.lw(sp + 0x10 + 4 * i as u32));
        states::movie_decoder_init(m, c, a0, a1, a2, a3, stack)
    });
    report("800553B4", &r);
    // Phase 0 (frame 1363), phase 1 (1366), the skip (1421: decoder stop,
    // screen clear), phase 3 (1422) and the end (1423: 800567EC, state 4).
    for occurrence in 0..5 {
        let r = with_recorded_owned(
            &index,
            0x8005_687c,
            occurrence,
            &[],
            CLEAR_STATUS,
            states::movie_frame,
        );
        report(&format!("8005687C #{occurrence}"), &r);
    }
    let r = with_recorded(&index, 0x8005_67ec, 0, &[], states::movie_finish);
    report("800567EC", &r);
}

#[test]
#[ignore = "needs analysis/flight-runtime/ovmisc-006"]
fn movie_decoder_matches_original() {
    let index = capture("ovmisc-006");
    let r = check(&index, 0x8005_5290, 0, |m, e| {
        states::movie_decoder_setup(m, e.gpr(4));
        None
    });
    report("80055290", &r);
    let r = check(&index, 0x8005_5484, 0, |m, _| {
        let mut c = dispatch::call;
        states::movie_stream_start(m, &mut c);
        None
    });
    report("80055484", &r);
    // #1 runs while the MDEC's slice interrupts of the previous frame
    // write the image buffers: the CPU decode alone is checked on #0 here
    // and on the two decodes of `ovmisc-009`.
    for occurrence in 0..1 {
        let p = pair(&index, 0x800a_b398, occurrence);
        let owned = [CD_STREAM[0], CD_STREAM[1], movie_ring(&p.entry)];
        let r = check_owned(&index, 0x800a_b398, occurrence, &owned, |m, e| {
            let [a0, a1, a2, _] = arguments(e);
            Some(states::pending_sdk::dec_dct_vlc2(m, a0, a1, a2))
        });
        report(&format!("800AB398 DecDCTvlc2 #{occurrence}"), &r);
    }
    for occurrence in 0..3 {
        let p = pair(&index, 0x8005_59d0, occurrence);
        let owned = [CD_STREAM[0], CD_STREAM[1], movie_ring(&p.entry)];
        let r = check_owned(&index, 0x8005_59d0, occurrence, &owned, |m, e| {
            let mut c = dispatch::call;
            Some(states::movie_next_frame(m, &mut c, e.gpr(4)))
        });
        report(&format!("800559D0 #{occurrence}"), &r);
    }
    // The third callback services a deferred CD sector (800297B8,
    // `cdstream.rs`, not in this workspace yet): checked in
    // `movie_slice_callback_with_deferred_sector_matches_original`.
    for occurrence in 0..2 {
        let p = pair(&index, 0x8005_5878, occurrence);
        let owned = movie_slices(&p.entry);
        let r = check_owned(&index, 0x8005_5878, occurrence, &owned, |m, _| {
            // The original's next slice completes in a later interrupt. The
            // callback ran on libetc's interrupt stack (800B2F2C..800B3F44,
            // interrupt-owned); the native frames go to the main stack.
            m.device = Some(Box::new(MdecRunning));
            m.sp = ac3_flight::machine::INITIAL_SP;
            let mut c = dispatch::call;
            states::movie_slice_done(m, &mut c);
            None
        });
        report(&format!("80055878 #{occurrence}"), &r);
    }
    let r = check(&index, 0x8005_578c, 0, |m, e| {
        Some(states::movie_sync(m, e.gpr(4), e.gpr(5)))
    });
    report("8005578C", &r);
    let r = check(&index, 0x8005_5594, 0, |m, _| {
        let mut c = dispatch::call;
        Some(states::movie_stop(m, &mut c))
    });
    report("80055594", &r);
    let r = check(&index, 0x800a_b6e8, 0, |m, e| {
        states::pending_sdk::dec_dct_vlc_build(m, e.gpr(4));
        None
    });
    report("800AB6E8 DecDCTvlcBuild", &r);
}

#[test]
#[ignore = "needs analysis/flight-runtime/ovmisc-009"]
fn movie_vlc_decode_matches_original() {
    let index = capture("ovmisc-009");
    for occurrence in 0..2 {
        let p = pair(&index, 0x800a_b398, occurrence);
        let owned = [CD_STREAM[0], CD_STREAM[1], movie_ring(&p.entry)];
        let r = check_owned(&index, 0x800a_b398, occurrence, &owned, |m, e| {
            let [a0, a1, a2, _] = arguments(e);
            Some(states::pending_sdk::dec_dct_vlc2(m, a0, a1, a2))
        });
        report(&format!("800AB398 DecDCTvlc2 #{occurrence}"), &r);
    }
}

// ---- state 6: the picture player ---------------------------------------------------------
//
// `ovmisc-012`: from the title (`overlay-entry.sav`, CROSS at 301), the
// documented one-register state selection (`.ovmisc/snapshots.lua`: the
// first state-callback result 0 on or after frame 450 becomes 0x60000006)
// enters state 6. Context +0x3AC is 0 there, so the player streams table
// entry 0 (file 0x800) through the picture parser. Everything after the
// selection is original execution.

/// The picture stream ring (decoder +0xD4, +0xD0 bytes), written by CD DMA.
fn picture_ring(entry: &Snapshot) -> (u32, u32, &'static str) {
    let mut m = entry.machine();
    let decoder = m.lw(0x800b_a9f4 + 0x4e8);
    let ring = m.lw(decoder + 0xd4);
    let size = m.lw(decoder + 0xd0);
    (
        ring,
        ring.wrapping_add(size),
        "picture stream ring (CD DMA)",
    )
}

fn picture_owned(entry: &Snapshot) -> [(u32, u32, &'static str); 3] {
    [CD_STREAM[0], CD_STREAM[1], picture_ring(entry)]
}

/// Like [`with_recorded`], with more interrupt-owned ranges.
fn with_recorded_owned(
    index: &Path,
    callee: u32,
    occurrence: usize,
    pending: &[u32],
    extra: &[(u32, u32, &'static str)],
    native: impl FnOnce(&mut Machine, &mut states::Invoker) -> u32,
) -> Report {
    let mut recorded = Recorded::load(index, callee, occurrence, pending);
    check_owned(index, callee, occurrence, extra, |m, _| {
        let mut c = |m: &mut Machine, t: u32, a: [u32; 4]| recorded.call(m, t, a);
        Some(native(m, &mut c))
    })
}

fn count(index: &Path, callee: u32) -> usize {
    snapshot::read_index(index)
        .unwrap()
        .iter()
        .filter(|s| s.callee == callee && s.stage == "entry")
        .count()
}

#[test]
#[ignore = "needs analysis/flight-runtime/ovmisc-012"]
fn picture_state_matches_original() {
    let index = capture("ovmisc-012");
    // The second call's ClearImage was queued behind the StoreImage DMA:
    // libgpu's fill packet (800C80E0, built by the transfer 800A7BFC) is
    // written when the DMA interrupt drains the queue, after the replayed
    // ClearImage returned.
    let fill = [(0x800c_80e0, 0x800c_8130, "libgpu fill packet (queue drain)")];
    for occurrence in 0..2 {
        let exit = pair(&index, 0x8005_9900, occurrence).exit.ram;
        let r = with_recorded_owned(&index, 0x8005_9900, occurrence, &[], &fill, |m, c| {
            m.device = Some(Box::new(StoreFromExit(exit)));
            states::picture_state_setup(m, c)
        });
        report(&format!("80059900 #{occurrence}"), &r);
    }
    for occurrence in 0..count(&index, 0x8005_9adc) {
        let p = pair(&index, 0x8005_9adc, occurrence);
        let owned = picture_owned(&p.entry);
        let r = with_recorded_owned(
            &index,
            0x8005_9adc,
            occurrence,
            &[],
            &owned,
            states::picture_state_frame,
        );
        report(&format!("80059ADC #{occurrence}"), &r);
    }
}

#[test]
#[ignore = "needs analysis/flight-runtime/ovmisc-012"]
fn picture_player_matches_original() {
    let index = capture("ovmisc-012");
    for occurrence in 0..count(&index, 0x8005_6950) {
        let p = pair(&index, 0x8005_6950, occurrence);
        let owned = picture_owned(&p.entry);
        let r = with_recorded_owned(
            &index,
            0x8005_6950,
            occurrence,
            &[],
            &owned,
            states::picture_player,
        );
        report(&format!("80056950 #{occurrence}"), &r);
    }
    for occurrence in 0..count(&index, 0x8005_6f90) {
        let p = pair(&index, 0x8005_6f90, occurrence);
        let owned = picture_owned(&p.entry);
        let r = check_owned(&index, 0x8005_6f90, occurrence, &owned, |m, e| {
            let mut c = dispatch::call;
            Some(states::picture_next_chunk(m, &mut c, e.gpr(4)))
        });
        report(&format!("80056F90 #{occurrence}"), &r);
    }
    for occurrence in 0..count(&index, 0x8005_7774) {
        let p = pair(&index, 0x8005_7774, occurrence);
        let owned = picture_owned(&p.entry);
        let r = check_owned(&index, 0x8005_7774, occurrence, &owned, |m, _| {
            let mut c = dispatch::call;
            Some(states::picture_frame(m, &mut c))
        });
        report(&format!("80057774 #{occurrence}"), &r);
    }
    for occurrence in 0..count(&index, 0x8005_891c) {
        let r = check(&index, 0x8005_891c, occurrence, |m, e| {
            Some(states::picture_border(m, e.gpr(4), e.gpr(5)))
        });
        report(&format!("8005891C #{occurrence}"), &r);
    }
    for occurrence in 0..count(&index, 0x8008_3084) {
        let r = check(&index, 0x8008_3084, occurrence, |m, e| {
            let [a0, a1, a2, a3] = arguments(e);
            let mut c = dispatch::call;
            Some(states::picture_noise(m, &mut c, a0, a1, a2, a3))
        });
        report(&format!("80083084 #{occurrence}"), &r);
    }
}

/// `ovmisc-011`: slice callbacks from frame 1385 on, with the deferred CD
/// sector routine 800297B8 replayed from its recordings.
#[test]
#[ignore = "needs analysis/flight-runtime/ovmisc-011"]
fn movie_slice_callback_with_deferred_sector_matches_original() {
    let index = capture("ovmisc-011");
    let mut deferred = 0;
    for occurrence in 0..count(&index, 0x8005_5878) {
        let p = pair(&index, 0x8005_5878, occurrence);
        let mut owned = movie_slices(&p.entry).to_vec();
        owned.extend_from_slice(CD_STREAM);
        owned.push(movie_ring(&p.entry));
        let mut recorded = Recorded::load(&index, 0x8005_5878, occurrence, &[]);
        deferred += recorded.0.len();
        let r = check_owned(&index, 0x8005_5878, occurrence, &owned, |m, _| {
            m.device = Some(Box::new(MdecRunning));
            m.sp = ac3_flight::machine::INITIAL_SP;
            let mut c = |m: &mut Machine, t: u32, a: [u32; 4]| recorded.call(m, t, a);
            states::movie_slice_done(m, &mut c);
            None
        });
        report(&format!("80055878 #{occurrence}"), &r);
    }
    println!("deferred sectors serviced: {deferred}");
}

// ---- differential oracle: paths no capture reaches -----------------------------------------

/// `[gp+0x4E4]` / `[gp+0x4E8]`: the picture block and decoder; `[gp+0x4DC]`
/// the movie decoder.
const PICTURE_BLOCK: u32 = ac3_flight::GP + 0x4e4;
const PICTURE_DECODER: u32 = ac3_flight::GP + 0x4e8;
const MOVIE_DECODER: u32 = ac3_flight::GP + 0x4dc;

/// Random effect slots in both banks of picture decoder `d` (start, end >=
/// start + 2, parameters with nonzero divisors: the original traps on a
/// zero span or zoom, where the oracle stops), tile page flags, and the
/// decoder's effect, time, frame, bank and slot (bank 2, whose records
/// overlap other fields, only with `odd_bank`). Returns the effect.
fn picture_effect_state(m: &mut Machine, rng: &mut oracle::Rng, d: u32, odd_bank: bool) -> u32 {
    use states::picture as pic;
    for bank in 0..2 {
        for slot in 0..5 {
            let r = d + bank * pic::BANK_SIZE + slot * pic::SLOT_SIZE + pic::SLOTS;
            let start = rng.next() % 300;
            let length = 2 + rng.next() % 120;
            m.sb(r, (rng.next() % 11) as u8);
            m.sh(r + 2, start as u16);
            m.sh(r + 4, (start + length) as u16);
            let p0 = if rng.next().is_multiple_of(3) {
                0
            } else {
                rng.next() % (length - 1)
            };
            let p1 = if rng.next().is_multiple_of(2) {
                0
            } else {
                start + p0 + 1 + rng.next() % 60
            };
            let zoom = [1i16, 2, 3, 4, -1, -2, 8][(rng.next() % 7) as usize];
            m.sh(r + 6, p0 as u16);
            m.sh(r + 8, p1 as u16);
            m.sh(r + 0xa, zoom as u16);
            m.sh(r + 0xc, (rng.next() % 30) as u16);
        }
    }
    for tile in 0..40 {
        m.sb(d + pic::TILE_PAGES + tile, (rng.next() % 2) as u8);
    }
    let bank = if odd_bank && rng.next().is_multiple_of(8) {
        2
    } else {
        rng.next() % 2
    };
    let slot = rng.next() % 5;
    m.sb(d + pic::SHOWN_BANK, bank as u8);
    m.sb(d + pic::SLOT, slot as u8);
    let effect = rng.next() % 11;
    m.sb(d + pic::EFFECT, effect as u8);
    m.sw(d + pic::EFFECT_TIME, rng.next() % 90);
    let r = d + bank * pic::BANK_SIZE + slot * pic::SLOT_SIZE + pic::SLOTS;
    let frame = match rng.next() % 3 {
        0 => u32::from(m.lhu(r + 2)),
        1 => u32::from(m.lhu(r + 4)).wrapping_add(u32::from(m.lhu(r + 0xc))),
        _ => rng.next() % 400,
    };
    m.sw(d + pic::FRAME, frame);
    effect
}

/// The picture's layered effects, which the captured sequence never
/// reaches (it does not restart): 80057938 and the effects it selects
/// (80057AE0 tiles, 80057F14 blend, 80058230 tint, 80058474 zoom, 8005868C
/// wipe), on the 80057774 entries of `ovmisc-012` with random slots (the
/// support oracle; callees run natively in both runs).
#[test]
#[ignore = "needs analysis/flight-runtime/ovmisc-012"]
fn picture_effects_match_original() {
    let bases = oracle::entries("ovmisc-012", 0x8005_7774);
    let mut rng = oracle::Rng(0x0005_7938);
    let routines: [(u32, u32, &str); 6] = [
        (0x8005_7938, 0x8005_7ae0, "80057938"),
        (0x8005_7ae0, 0x8005_7f14, "80057AE0"),
        (0x8005_7f14, 0x8005_8230, "80057F14"),
        (0x8005_8230, 0x8005_8474, "80058230"),
        (0x8005_8474, 0x8005_868c, "80058474"),
        (0x8005_868c, 0x8005_891c, "8005868C"),
    ];
    let mut effects = [[0usize; 11]; 6];
    let mut changed = [0usize; 6];
    for case in 0..900usize {
        let mut m = bases[case % bases.len()].machine();
        let d = m.lw(PICTURE_DECODER);
        let block = m.lw(PICTURE_BLOCK);
        let packets = m.lw(block + 0x34);
        let ot = m.lw(block + 0x38);
        let which = case % 6;
        let effect = picture_effect_state(&mut m, &mut rng, d, which == 0 || which == 5);
        let (start, end, label) = routines[which];
        changed[which] += oracle::diff(label, &m, (start, end), [d, packets, ot, 0], |m| {
            Some(match which {
                0 => states::picture_layers(m, d, packets, ot),
                1 => states::picture_tiles(m, d, packets, ot),
                2 => states::picture_blend(m, d, packets, ot),
                3 => states::picture_tint(m, d, packets, ot),
                4 => states::picture_zoom(m, d, packets, ot),
                _ => states::picture_wipe(m, d, packets, ot),
            })
        });
        effects[which][effect as usize] += 1;
    }
    for (i, (_, _, label)) in routines.iter().enumerate() {
        println!(
            "{label}: 150 cases, effects {:?}, {} bytes",
            effects[i], changed[i]
        );
        assert!(changed[i] > 0);
    }
}

/// 800556E0 (no caller) on the 800555C0 entries of `ovmisc-006`: at END,
/// without a new frame, and decoding with both wait modes.
#[test]
#[ignore = "needs analysis/flight-runtime/ovmisc-006"]
fn movie_decode_ready_matches_original() {
    let bases = oracle::entries("ovmisc-006", 0x8005_55c0);
    let mut cases = 0;
    let mut changed = 0;
    let mut results = [0usize; 2];
    for base in &bases {
        for mode in [0u32, 1] {
            for end in [0u8, 1] {
                let mut m = base.machine();
                let d = m.lw(MOVIE_DECODER);
                m.sb(d + states::decoder::END, end);
                let mut v0 = 0;
                changed += oracle::diff(
                    "800556E0",
                    &m,
                    (0x8005_56e0, 0x8005_578c),
                    [mode, 0, 0, 0],
                    |m| {
                        v0 = states::movie_decode_ready(m, &mut dispatch::call, mode);
                        Some(v0)
                    },
                );
                results[v0 as usize] += 1;
                cases += 1;
            }
        }
    }
    println!("800556E0: {cases} cases, results {results:?}, {changed} bytes");
    assert!(changed > 0 && results[0] > 0 && results[1] > 0);
}
