//! Overlay-2 core (`ov2_core*.rs`) on original function snapshots taken
//! while module 2 is loaded: every translated routine runs on the captured
//! entry state (through `ov2_core::dispatch`, original register ABI) and
//! must reproduce the captured return state exactly: all RAM outside the
//! stack, the interrupt-owned and device-value ranges (`ALLOWED`) and one
//! dead-stack copy (`dead_stack_copy`), the scratchpad, every GTE register,
//! and `v0` where the routine returns one (`snapshot::check_call` rules
//! otherwise). StoreImage's VRAM words come from a test device (`Vram`).
//!
//! A run that reaches a routine nobody has translated yet (resident SDK
//! routines such as SetDrawMode 800AAC38, sub-mode routines of the other
//! overlay-2 agents) is reported as BLOCKED with the address instead of
//! failing; it becomes a full comparison when that routine lands. Any other
//! fault and any difference fail.
//!
//! Captures (`analysis/flight-runtime/`):
//! - `ov2core-002`: from `menu-runtime/continuation-001` with DOWN 1/5 and
//!   CROSS 21/25: the state-3 initialization (frame 86), the mission loader
//!   (inner state 14, frames 87..294) and the first hub frame (295); whole
//!   substate-1 windows 0..4, 25, 26, 139, 140, 174, 186, 187.
//! - `ov2core-003`: from `mission-002` (hub) with CROSS 1/5: hub frames,
//!   the hub exit (frame 28) and the first 110 briefing frames; whole
//!   windows at frames 0, 3, 28..31, 82, 108, 134; text, map, visual and
//!   helper routines in hub frames (up to 10) and briefing frames (100 on).
//! - `ov2core-004`: from `mission-004` (CROSS held from frame 1): briefing
//!   end, inner-state change to aircraft selection (frame 30).
//! - `ov2core-005`: from `mission-005` with CROSS 1/5: aircraft confirm and
//!   weapon selection (frame 29 on).
//! - `ov2core-006`: from `mission-006` with CROSS 1/5 and 81/85: weapon
//!   confirm, launch (inner state 3 from frame 30) and the resident
//!   transition 0x60000002 at frame 81.
mod support;

use ac3_flight::{
    Device, Fault, Machine,
    machine::HostCall,
    ov2_core,
    snapshot::{self, Snapshot},
};
use std::path::PathBuf;
use support::oracle;

fn index(capture: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR"))
        .join("analysis/flight-runtime")
        .join(capture)
        .join("snapshots.jsonl")
}

const LOADER: &str = "ov2core-002";
const HUB: &str = "ov2core-003";
const BRIEFING_END: &str = "ov2core-004";
const WEAPONS: &str = "ov2core-005";
const LAUNCH: &str = "ov2core-006";

/// One routine's snapshot occurrences: address, number of occurrences,
/// whether `v0` is a result callers use (an occurrence that changed no RAM
/// still counts as EXACT then, instead of SAME). `v0` is compared for every
/// routine but the opcode handlers (`dead_v0`).
struct Case(u32, usize, bool);

enum Outcome {
    /// Bytes the original changed, differing bytes ignored as allowed.
    Exact(usize, usize),
    Blocked(u32),
}

/// RAM written by code other than the routine's own call tree (interrupt
/// handlers, the BIOS, libgpu's timing-dependent queue: the original
/// enqueued texture uploads issued while a DMA was still running and
/// drained them from the DMA interrupt; the host GPU finishes each transfer
/// before returning), or holding device values the host supplies (GPU
/// status words, the interrupt mask). The ranges and their writers are the
/// ones identified for the resident loader in `tests/loading.rs`
/// (`INTERRUPT_OWNED`), the boot (`tests/boot.rs`), the state replays
/// (`tests/states.rs`, `CLEAR_STATUS`) and the briefing (`tests/ov2_briefing.rs`,
/// `HANDLER_BYTES`), plus the libgpu DMA poll counter of `tests/frame.rs`.
/// Excluded from the comparison; each occurrence reports how many differing
/// bytes fell there. No overlay-2 routine writes them.
const ALLOWED: [(u32, u32, &str); 17] = [
    (0x8000_0000, 0x8001_0000, "BIOS kernel RAM"),
    (0x800b_256c, 0x800b_2570, "sound interrupt flag (80099FCC)"),
    (
        0x800b_2f2c,
        0x800b_3f44,
        "libetc interrupt context and stack",
    ),
    (0x800b_3ff4, 0x800b_3ff8, "VSync counter (800A0EBC)"),
    (
        0x800b_4010,
        0x800b_4014,
        "GPU DMA callback slot (libgpu enqueue)",
    ),
    (
        0x800b_40e8,
        0x800b_40ec,
        "libcd drive status byte (CD interrupt; 004 frame 3: 0x22 -> 0x02)",
    ),
    (
        0x800b_43b8,
        0x800b_43bc,
        "libcd read state (CD interrupt; 004 frame 3: 3 -> 2)",
    ),
    (0x800b_990c, 0x800b_9914, "libgpu queue head/tail"),
    (
        0x800b_9914,
        0x800b_9918,
        "libgpu saved interrupt mask (the SetIntrMask result, a host value: 0 natively, 0x2D in the original)",
    ),
    (
        0x800b_9920,
        0x800b_9928,
        "libgpu timeout and DMA poll counter",
    ),
    (
        0x800b_f178,
        0x800b_f1c8,
        "game CD stream callback state (800297B8 and its callees, run from the CD ready callback during the XA music)",
    ),
    (0x800c_5bcc, 0x800c_5bce, "sound driver tick state"),
    (0x800c_5c70, 0x800c_5c72, "sound driver tick state"),
    (0x800c_5c98, 0x800c_5ca0, "libcd last result bytes"),
    (
        0x800c_80f4,
        0x800c_80f8,
        "ClearImage draw mode (GPU status, a host value)",
    ),
    (
        0x800c_810c,
        0x800c_8118,
        "ClearImage drawing area (GPU status, a host value)",
    ),
    (0x800c_8230, 0x800c_9a30, "libgpu queue entries"),
];

/// 800ED834 state pointer: the briefing state block.
const BRIEFING_STATE: u32 = 0x800f_6860;

/// The 28 visual opcode handlers (table 0x800F695C): reached only from the
/// interpreter's `jalr` at 800F1724, which reloads v0 at 800F1730, so
/// their v0 is dead (the translations return 0).
/// Routines that return a resident callee's v0 which that callee's
/// translation does not reproduce yet: reported (`V0`), not failed; the
/// comparison becomes exact once the callee returns the original value.
const CALLEE_V0: [(u32, u32, &str); 1] = [(
    0x800d_0c88,
    0x8002_c280,
    "shared.rs returns 0; the original leaves [gp+0x48], the CD queue block",
)];

fn dead_v0(m: &mut Machine, routine: u32) -> bool {
    (0..28).any(|i| m.lw(ov2_core::visual::HANDLERS + i * 4) == routine)
}

fn allowed(address: u32) -> bool {
    ALLOWED
        .iter()
        .any(|&(start, end, _)| (start..end).contains(&address))
}

/// Dead stack the original copies into RAM: the MATRIX pad halfword of the
/// briefing view matrix kept at state +0x554 (800EBCD8 builds the matrix at
/// its `sp + 0x88` and copies all 32 bytes; nothing writes the pad at
/// `sp + 0x9A`). In a whole 800CBB88 window that stack word (0x801FFF08) last
/// held the `s0` (16, the OT length) that VSync 800A0568 saved in its
/// prologue under libgpu's DMA timeout arm 800A8B98, from ClearOTagR(OT, 16)
/// of 800F1B00. Native SDK routines keep saved registers in Rust, so the
/// native copy carries older stack contents (0x8015). The GTE never reads
/// the pad (`ctc2` R33 takes the low half of the word). Started from a
/// captured 800EF01C or 800ED834 entry (the original's stack) the bytes are
/// compared. Returns the pad's two addresses.
fn dead_stack_copy(m: &mut Machine) -> std::ops::Range<u32> {
    let pad = m.lw(BRIEFING_STATE).wrapping_add(0x566);
    pad..pad + 2
}

/// Answers StoreImage's transfer (host call 800A6970 `[rect, pixels]`) with
/// the pixel words the original's DMA delivered, taken from its return image
/// (the harness has no VRAM; the same device as `tests/sdk_matrix.rs`).
struct Vram(Vec<u8>);
impl Device for Vram {
    fn call(&mut self, ram: &mut [u8], _: &mut [u8], call: HostCall) -> Option<u32> {
        if call.function != 0x800a_6970 {
            return None;
        }
        let [rect, pixels, ..] = call.args;
        let half = |a: u32| {
            u16::from_le_bytes([
                ram[(a & 0x1f_ffff) as usize],
                ram[((a + 1) & 0x1f_ffff) as usize],
            ])
        };
        let words = (u32::from(half(rect + 4)) * u32::from(half(rect + 6))).div_ceil(2);
        for i in 0..words * 4 {
            let at = (pixels.wrapping_add(i) & 0x1f_ffff) as usize;
            ram[at] = self.0[at];
        }
        None
    }
}

fn run(capture: &str, case: &Case, occurrence: usize) -> Result<Outcome, String> {
    let Case(callee, ..) = *case;
    let path = index(capture);
    let directory = path.parent().expect("directory");
    let all = snapshot::read_index(&path).map_err(|e| e.to_string())?;
    let entry = all
        .iter()
        .filter(|s| s.callee == callee && s.stage == "entry")
        .nth(occurrence)
        .ok_or_else(|| format!("{callee:08x}: no entry snapshot #{occurrence}"))?;
    let exit = all
        .iter()
        .find(|s| {
            s.callee == callee
                && s.stage == "return"
                && s.sequence > entry.sequence
                && s.ra == entry.ra
                && s.registers.gpr[29] == entry.registers.gpr[29]
        })
        .ok_or_else(|| format!("{callee:08x}#{occurrence}: no return snapshot"))?;
    let entry = Snapshot::load(directory, entry).map_err(|e| e.to_string())?;
    let exit = Snapshot::load(directory, exit).map_err(|e| e.to_string())?;
    let mut m = entry.machine();
    // Whole windows only (see `dead_stack_copy`).
    let pad = if callee == 0x800c_bb88 {
        dead_stack_copy(&mut m)
    } else {
        0..0
    };
    m.device = Some(Box::new(Vram(exit.ram.clone())));
    let args = [entry.gpr(4), entry.gpr(5), entry.gpr(6), entry.gpr(7)];
    let v0 = ov2_core::dispatch(&mut m, callee, args).expect("claimed while module 2 is loaded");
    match m.take_fault() {
        // No module claims the address (a routine of this module would
        // have been dispatched natively).
        Err(Fault::Untranslated { address }) => return Ok(Outcome::Blocked(address)),
        Err(fault) => return Err(format!("{callee:08x}#{occurrence}: {fault}")),
        Ok(()) => {}
    }
    let mut differences = snapshot::compare(&m, &exit);
    let before = differences.ram.len();
    differences
        .ram
        .retain(|&address| !allowed(address) && !pad.contains(&address));
    let ignored = before - differences.ram.len();
    if !differences.is_empty() {
        if std::env::var_os("OV2_DEBUG").is_some() {
            for &address in &differences.ram {
                let at = (address & 0x1f_ffff) as usize;
                println!(
                    "  {address:08x}: entry {:02x} original {:02x} native {:02x}",
                    entry.ram[at],
                    exit.ram[at],
                    m.ram()[at]
                );
            }
        }
        return Err(format!(
            "{callee:08x}#{occurrence}: {}",
            differences.summary()
        ));
    }
    if !dead_v0(&mut m, callee) && v0 != exit.gpr(2) {
        if let Some((_, from, why)) = CALLEE_V0.iter().find(|c| c.0 == callee) {
            println!(
                "V0      {callee:08x}#{occurrence}: {v0:#010x}, original {:#010x} (the v0 of {from:08x}: {why})",
                exit.gpr(2)
            );
            let changed = snapshot::changed_bytes(&entry, &exit);
            return Ok(Outcome::Exact(changed, ignored));
        }
        return Err(format!(
            "{callee:08x}#{occurrence}: v0 {v0:#010x}, original {:#010x}",
            exit.gpr(2)
        ));
    }
    let changed = snapshot::changed_bytes(&entry, &exit);
    Ok(Outcome::Exact(changed, ignored))
}

/// A machine with overlay module 2 marked loaded (for ownership queries).
fn loaded() -> Machine {
    let mut m = Machine::default();
    m.sw(ac3_flight::GP + 0x2dc, 0x800b_d158);
    m.sw(0x800b_d158 + 0x38, 2);
    m
}

/// Run every occurrence of every case; panics on any failure and returns
/// (exact, blocked) counts.
fn check(capture: &str, cases: &[Case]) -> (usize, usize) {
    let mut failures = Vec::new();
    let (mut exact, mut blocked) = (0, 0);
    for case in cases {
        for occurrence in 0..case.1 {
            match run(capture, case, occurrence) {
                Ok(Outcome::Exact(bytes, ignored)) => {
                    if bytes == 0 && !case.2 {
                        // The original rewrote identical values: no proof
                        // here (unit tests cover these routines).
                        println!("SAME    {:08x}#{occurrence}: no byte changed", case.0);
                    } else {
                        exact += 1;
                        println!(
                            "EXACT   {:08x}#{occurrence}: {bytes} original bytes ({ignored} allowed)",
                            case.0
                        );
                    }
                }
                Ok(Outcome::Blocked(address)) => {
                    blocked += 1;
                    println!(
                        "BLOCKED {:08x}#{occurrence}: untranslated {address:08x}",
                        case.0
                    );
                }
                Err(error) => {
                    println!("FAIL    {error}");
                    failures.push(error);
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    (exact, blocked)
}

/// 800CB8B4 and the subsystem initializers it calls (frame 86).
#[test]
#[ignore = "needs the ov2core-002 capture"]
fn initialization() {
    let (exact, _) = check(
        LOADER,
        &[
            Case(0x800c_b8b4, 1, true),
            Case(0x800c_b3bc, 1, false),
            Case(0x800f_197c, 1, true),
            Case(0x800f_1a9c, 3, false),
            Case(0x800d_0c2c, 1, false),
            Case(0x800d_0e90, 1, false),
            Case(0x800d_169c, 1, false),
            Case(0x800e_af0c, 1, false),
            Case(0x800e_7e58, 1, false),
            Case(0x800e_986c, 1, false),
            Case(0x800e_a598, 1, false),
            Case(0x800e_a038, 3, false),
        ],
    );
    assert!(exact > 0);
}

/// The substate-1 pieces outside the sub-modes and the inner dispatcher
/// running the loader (frames 87/88).
#[test]
#[ignore = "needs the ov2core-002 capture"]
fn frame_pieces() {
    check(
        LOADER,
        &[
            Case(0x800c_bab8, 1, false),
            Case(0x800f_1b00, 1, false),
            Case(0x800f_1770, 2, true),
            Case(0x800c_b408, 2, true),
        ],
    );
}

/// Inner state 14: the loader callbacks and every phase leaf.
#[test]
#[ignore = "needs the ov2core-002 capture"]
fn loader() {
    let (exact, _) = check(
        LOADER,
        &[
            Case(0x800d_3254, 1, true),
            Case(0x800d_328c, 15, true),
            Case(0x800c_ebcc, 2, false),
            Case(0x800c_ed30, 1, true),
            Case(0x800c_ed5c, 1, true),
            Case(0x800c_eda8, 1, true),
            Case(0x800c_ee3c, 1, true),
            Case(0x800c_eef0, 2, true),
            Case(0x800c_ef34, 1, true),
            Case(0x800c_f014, 1, true),
            Case(0x800c_f064, 1, true),
            Case(0x800c_f100, 2, true),
            Case(0x800c_f144, 1, true),
            Case(0x800c_f25c, 1, true),
            Case(0x800c_f2f4, 3, true),
            Case(0x800c_f350, 1, true),
            Case(0x800c_eb80, 1, false),
            Case(0x800c_eb90, 1, false),
            Case(0x800c_e7e4, 2, false),
            Case(0x800c_e844, 2, true),
            Case(0x800c_c9ac, 1, false),
            Case(0x800d_0ecc, 1, false),
            Case(0x800d_1890, 2, true),
            Case(0x800d_c074, 1, false),
            Case(0x800e_7e20, 1, false),
            Case(0x800e_8008, 1, false),
        ],
    );
    assert!(exact > 0);
}

/// Whole substate-1 callbacks (800CBB88) of the loader frames and the
/// first hub frame.
#[test]
#[ignore = "needs the ov2core-002 capture"]
fn loader_windows() {
    check(LOADER, &[Case(0x800c_bb88, 12, true)]);
}

/// Without module 2 loaded nothing is claimed.
#[test]
fn claims_only_while_module_2_is_loaded() {
    let mut m = loaded();
    assert!(ov2_core::dispatch(&mut m, 0x800c_bc70, [0; 4]).is_some());
    m.sw(0x800b_d158 + 0x38, 1);
    assert!(ov2_core::dispatch(&mut m, 0x800c_bc70, [0; 4]).is_none());
    m.sw(0x800b_d158 + 0x38, 0xffff_ffff);
    assert!(ov2_core::dispatch(&mut m, 0x800c_bb88, [0; 4]).is_none());
}

/// 800CBC70 and 800CBC78 (substates 2 and 3, not reached on the path).
#[test]
fn substates_2_and_3() {
    let mut m = loaded();
    assert_eq!(ov2_core::dispatch(&mut m, 0x800c_bc70, [0; 4]), Some(1));
    let ctx = 0x800b_d158;
    for (pressed, result) in [
        (0x10, 0x100),
        (0x30, 0x100),
        (0x20, 0x200),
        (0, 0),
        (0x4f, 0),
    ] {
        m.sw(ctx + 0x12c, pressed);
        assert_eq!(
            ov2_core::dispatch(&mut m, 0x800c_bc78, [0; 4]),
            Some(result)
        );
    }
}

/// Hub and briefing frames: the shared drawing layers and helpers, the
/// briefing wrapper and the hub-to-briefing dispatch.
#[test]
#[ignore = "needs the ov2core-003 capture"]
fn hub_and_briefing() {
    let (exact, _) = check(
        HUB,
        &[
            Case(0x800c_b408, 2, true),
            Case(0x800c_f484, 4, true),
            Case(0x800d_04c8, 4, true),
            Case(0x800d_0b28, 1, true),
            Case(0x800d_0c58, 1, false),
            Case(0x800d_0c68, 1, false),
            Case(0x800d_0cc4, 1, false),
            Case(0x800d_0ee4, 2, false),
            Case(0x800d_0f20, 2, false),
            Case(0x800d_0fc0, 2, true),
            Case(0x800e_7a18, 1, false),
            Case(0x800e_7ab4, 1, false),
            Case(0x800e_80d8, 2, false),
            Case(0x800e_8574, 2, true),
            Case(0x800e_9060, 2, true),
            Case(0x800e_9748, 2, true),
            Case(0x800e_983c, 1, false),
            Case(0x800e_9a54, 2, true),
            Case(0x800e_b014, 2, true),
            Case(0x800e_f01c, 4, true),
            Case(0x800e_f21c, 3, true),
            Case(0x800e_fad0, 2, false),
            Case(0x800e_fcb8, 2, false),
            Case(0x800e_ff74, 2, false),
            Case(0x800f_04ec, 2, false),
            Case(0x800f_0c88, 1, false),
            Case(0x800f_0cd8, 1, false),
            Case(0x800f_0cec, 1, false),
            Case(0x800f_0d0c, 1, false),
            Case(0x800f_0d40, 1, false),
            Case(0x800f_0d70, 1, false),
            Case(0x800f_0dd4, 1, false),
            Case(0x800f_0e1c, 1, false),
            Case(0x800f_0e64, 1, false),
            Case(0x800f_0ed0, 1, false),
            Case(0x800f_0f44, 2, false),
            Case(0x800f_1674, 4, false),
            Case(0x800f_1770, 2, true),
            Case(0x800f_18ec, 2, false),
            Case(0x800f_1908, 2, false),
            Case(0x800f_193c, 2, false),
            Case(0x800f_196c, 1, false),
            Case(0x800f_1ba0, 1, true),
            Case(0x800f_1bb8, 2, true),
        ],
    );
    assert!(exact > 0);
}

/// Whole substate-1 callbacks of hub and briefing frames.
#[test]
#[ignore = "needs the ov2core-003 capture"]
fn hub_and_briefing_windows() {
    check(HUB, &[Case(0x800c_bb88, 9, true)]);
}

/// Briefing end, aircraft and weapon selection, launch: wrappers,
/// dispatcher routes and helpers, and whole windows.
#[test]
#[ignore = "needs the ov2core-004..006 captures"]
fn selection_and_launch() {
    check(
        BRIEFING_END,
        &[
            Case(0x800c_b408, 2, true),
            Case(0x800c_bb88, 8, true),
            Case(0x800c_f484, 2, true),
            Case(0x800d_04c8, 2, true),
            Case(0x800d_0c58, 1, false),
            Case(0x800d_0c68, 1, false),
            Case(0x800d_0c88, 1, false),
            Case(0x800d_0ee4, 2, false),
            Case(0x800d_0f20, 2, false),
            Case(0x800d_a65c, 3, true),
            Case(0x800e_7a18, 1, false),
            Case(0x800e_9824, 1, false),
            Case(0x800e_9a54, 1, true),
            Case(0x800e_b014, 1, true),
            Case(0x800e_f01c, 3, true),
            Case(0x800f_1674, 1, false),
            Case(0x800f_1770, 3, true),
            Case(0x800f_18ec, 2, false),
            Case(0x800f_196c, 1, false),
        ],
    );
    check(
        WEAPONS,
        &[
            Case(0x800c_b408, 2, true),
            Case(0x800c_bb88, 6, true),
            Case(0x800d_04c8, 2, true),
            Case(0x800d_0ee4, 2, false),
            Case(0x800d_0f20, 2, false),
            Case(0x800d_a65c, 3, true),
            Case(0x800f_1674, 1, false),
            Case(0x800f_1770, 2, true),
            Case(0x800f_18ec, 2, false),
        ],
    );
    check(
        LAUNCH,
        &[
            Case(0x800c_b408, 5, true),
            Case(0x800c_bb88, 7, true),
            Case(0x800d_0c88, 1, false),
            Case(0x800d_0f20, 2, false),
            Case(0x800d_a65c, 1, true),
            Case(0x800e_79d4, 3, false),
            Case(0x800e_9748, 1, true),
            Case(0x800e_983c, 1, false),
            Case(0x800e_9a54, 1, true),
            Case(0x800e_b014, 1, true),
            Case(0x800f_1770, 2, true),
            Case(0x800f_18ec, 2, false),
            Case(0x800f_196c, 1, false),
        ],
    );
}

// ---- differential oracle cases --------------------------------------------------------------
//
// Routines and branches the captures do not reach (or reach only with
// unchanged state) are compared with `support::oracle`: the original
// instruction bytes of the routine are interpreted from a captured
// module-2 machine state with varied inputs, and the native translation
// must produce the same RAM (outside the dead stack), scratchpad, GTE,
// `sp`, faults and `v0`. Calls leaving the routine run natively in both
// runs. A case whose native run reaches an untranslated routine is
// reported BLOCKED (none since the SDK routines landed in `sdk.rs`).

/// Free RAM above the arena limit and below the stack (outputs, compared).
const OUT: u32 = 0x801f_b800;
/// Dead stack below every captured entry `sp` (read-only inputs).
const INPUT: u32 = 0x801f_d000;

/// Totals of one oracle group: cases compared, cases that changed RAM, cases
/// blocked by an untranslated routine.
#[derive(Default, Debug)]
struct Tally {
    compared: usize,
    changed: usize,
    blocked: usize,
    /// Per label: cases compared and cases that changed RAM.
    labels: std::collections::BTreeMap<String, (usize, usize)>,
}

impl Tally {
    /// One oracle comparison of `address` over `range` from `base` (`v0`
    /// included unless dead, see `dead_v0`).
    fn case(
        &mut self,
        label: &str,
        base: &Machine,
        address: u32,
        range: (u32, u32),
        args: [u32; 4],
    ) {
        let mut probe = base.clone();
        ov2_core::dispatch(&mut probe, address, args).expect("claimed");
        if let Some(Fault::Untranslated { address: missing }) = probe.fault() {
            self.blocked += 1;
            println!("BLOCKED {label}: untranslated {missing:08x}");
            return;
        }
        let compare = !dead_v0(&mut probe, address);
        let bytes = oracle::diff(label, base, range, args, |m| {
            let v0 = ov2_core::dispatch(m, address, args).expect("claimed");
            compare.then_some(v0)
        });
        self.compared += 1;
        let entry = self.labels.entry(label.to_string()).or_default();
        entry.0 += 1;
        if bytes > 0 {
            self.changed += 1;
            entry.1 += 1;
        }
    }
    fn report(&self, group: &str) {
        println!(
            "ORACLE  {group}: {} compared, {} changed RAM, {} blocked",
            self.compared, self.changed, self.blocked
        );
        if self.labels.len() <= 40 {
            for (label, (compared, changed)) in &self.labels {
                println!("ORACLE    {label}: {compared} compared, {changed} changed RAM");
            }
        }
    }
}

/// First captured entry state of `callee` in `capture`, module 2 loaded.
fn base(capture: &str, callee: u32) -> Machine {
    let entry = oracle::entries(capture, callee)
        .into_iter()
        .next()
        .expect("entry snapshot");
    entry.machine()
}

fn context_of(m: &mut Machine) -> u32 {
    m.lw(ac3_flight::GP + 0x2dc)
}

/// Group 1: the routines whose captured calls rewrote identical values.
#[test]
#[ignore = "needs the ov2core-002..004 captures"]
fn oracle_unchanged_routines() {
    let mut rng = oracle::Rng(0x0c0f_fee1);
    let mut tally = Tally::default();
    // 800CB3BC: mission ids around 32..37 and options with and without 0x80.
    let start = base(LOADER, 0x800c_b3bc);
    for case in 0..72u32 {
        let mut m = start.clone();
        let ctx = context_of(&mut m);
        let mission = if case < 64 { case } else { rng.next() & 0xffff };
        m.sh(ctx + 0x390, mission as u16);
        let options = rng.next() | if case & 1 != 0 { 0x80 } else { 0 };
        m.sw(ctx + 0x970, options);
        tally.case(
            "800cb3bc",
            &m,
            0x800c_b3bc,
            (0x800c_b3bc, 0x800c_b408),
            [0; 4],
        );
    }
    // 800D0C2C / 800D0E90 / 800D0F20: blocks filled with noise first.
    for (callee, end, capture) in [
        (0x800d_0c2cu32, 0x800d_0c58u32, LOADER),
        (0x800d_0e90, 0x800d_0ecc, LOADER),
        (0x800d_0f20, 0x800d_0f50, BRIEFING_END),
    ] {
        let start = base(capture, callee);
        for _ in 0..8 {
            let mut m = start.clone();
            let (pointer, range) = if callee == 0x800d_0c2c {
                (ov2_core::STREAM, [0..4u32, 0x1004..0x1010])
            } else {
                (ov2_core::TEXT, [0..4u32, 0x10..0x110])
            };
            let block = m.lw(pointer);
            for part in range {
                for offset in part {
                    m.sb(block + offset, rng.next() as u8);
                }
            }
            tally.case(&format!("{callee:08x}"), &m, callee, (callee, end), [0; 4]);
        }
    }
    // 800CEBCC: totals/loaded around each other, signed, into compared RAM.
    let start = base(LOADER, 0x800c_ebcc);
    for case in 0..32u32 {
        let mut m = start.clone();
        let block = m.lw(0x800f_1e4c);
        let total = rng.next() as i32 - 0x40_0000;
        let loaded = match case % 4 {
            0 => total,
            1 => total.wrapping_add(1),
            2 => total.wrapping_sub(1),
            _ => rng.next() as i32 - 0x40_0000,
        };
        m.sw(block + 0x10, total as u32);
        m.sw(block + 0x14, loaded as u32);
        m.sw(OUT, 0xdead_beef);
        m.sw(OUT + 4, 0xdead_beef);
        tally.case(
            "800cebcc",
            &m,
            0x800c_ebcc,
            (0x800c_ebcc, 0x800c_ec18),
            [OUT, OUT + 4, 0, 0],
        );
    }
    // 800D0CC4: request flag clear/set over many resource ids (the start
    // path needs a stream file and an idle CD queue).
    let start = base(HUB, 0x800d_0cc4);
    for case in 0..64u32 {
        let mut m = start.clone();
        let stream = m.lw(ov2_core::STREAM);
        let flags = m.lw(stream);
        m.sw(stream, if case % 2 == 0 { flags & !1 } else { flags | 1 });
        m.sh(stream + 0x1004, (case * 5) as u16);
        tally.case(
            "800d0cc4",
            &m,
            0x800d_0cc4,
            (0x800d_0cc4, 0x800d_0d30),
            [0; 4],
        );
    }
    // 800F0E64 (opcode 13), 800F1908, 800F18EC, 800F193C: random records.
    let start = base(HUB, 0x800f_0e64);
    for _ in 0..16 {
        let mut m = start.clone();
        m.sw(INPUT, 13);
        m.sh(INPUT + 4, (rng.next() & 0xf) as u16);
        for offset in (6..0x12).step_by(2) {
            m.sh(INPUT + offset, rng.next() as u16);
        }
        tally.case(
            "800f0e64",
            &m,
            0x800f_0e64,
            (0x800f_0e64, 0x800f_0ed0),
            [INPUT, 0, 0, 0],
        );
    }
    let start = base(HUB, 0x800f_1908);
    for _ in 0..32 {
        let mut m = start.clone();
        let block = m.lw(ov2_core::VISUAL);
        let index = rng.next() & 0x1f;
        m.sw(block + 0x298 + index * 4, rng.next());
        m.sw(OUT, 0xdead_beef);
        tally.case(
            "800f1908",
            &m,
            0x800f_1908,
            (0x800f_1908, 0x800f_193c),
            [index, OUT, OUT + 2, 0],
        );
    }
    for (callee, end) in [(0x800f_18ecu32, 0x800f_1908u32), (0x800f_193c, 0x800f_1954)] {
        let start = base(HUB, callee);
        for _ in 0..32 {
            let m = start.clone();
            let index = rng.next() & 0xf;
            let args = [index, rng.next(), rng.next(), 0];
            tally.case(&format!("{callee:08x}"), &m, callee, (callee, end), args);
        }
    }
    tally.report("unchanged routines");
    assert_eq!(tally.blocked, 0);
    assert!(tally.changed * 2 > tally.compared, "{tally:?}");
}

/// Script instruction `op` with `words` operands at [`INPUT`].
fn instruction(m: &mut Machine, op: u32, words: &[u32]) {
    m.sw(INPUT, op);
    for (i, &word) in words.iter().enumerate() {
        m.sw(INPUT + 4 + i as u32 * 4, word);
    }
}

/// Randomize the interpreter scratch state and the selected animation
/// record so the handlers' gates go both ways.
fn vary_visual(m: &mut Machine, rng: &mut oracle::Rng) {
    let block = m.lw(ov2_core::VISUAL);
    let scratch = 0x1f80_0000;
    m.sh(scratch + 4, (rng.next() % 0x80) as u16);
    m.sh(scratch + 6, (rng.next() % 0x80) as u16);
    m.sw(scratch + 0x10, rng.next() % 6);
    m.sh(scratch + 0xe, (rng.next() % 6) as u16);
    m.sw(scratch + 0x14, rng.next() & 0x7f);
    m.sw(scratch + 0x18, rng.next() & 0x1f);
    m.sw(
        scratch + 0x1c,
        [0xffff_ffff, 0, 1, 2, 3][(rng.next() % 5) as usize],
    );
    let record = rng.next() & 0xf;
    m.sw(block + 0x498, record);
    let at = block + 0x318 + record * 16;
    m.sh(at + 2, (rng.next() % 3) as u16);
    m.sh(at + 0xc, (rng.next() % 20) as u16);
    m.sh(at + 0xe, (rng.next() % 20) as u16);
}

/// Random ribbon trails (15 positions and 15 angle triples each) in front
/// of the ribbon camera (identity rotation, zero translation): inside the
/// view box when `inside`, else spread up to twice the depth sideways.
fn vary_trails(m: &mut Machine, rng: &mut oracle::Rng, inside: bool) {
    let map = m.lw(ov2_core::MAP);
    for trail in 0..3u32 {
        let table = m.lw(map + 0xf4);
        let positions = m.lw(table + trail * 4);
        let table = m.lw(map + 0xf0);
        let angles = m.lw(table + trail * 4);
        for entry in 0..15u32 {
            let depth = 0x1800 + rng.next() % 0x5000;
            let spread = if inside { depth / 4 } else { depth * 2 };
            for (offset, value) in [
                (0, (rng.next() % (2 * spread + 1)).wrapping_sub(spread)),
                (4, (rng.next() % (2 * spread + 1)).wrapping_sub(spread)),
                (8, depth),
            ] {
                m.sw(positions + entry * 16 + offset, value);
            }
            for k in 0..3u32 {
                m.sh(angles + entry * 8 + k * 2, (rng.next() & 0xfff) as u16);
            }
        }
    }
}

/// Group 2: visual opcodes no capture reaches, the clip modes, 800EF85C,
/// 800EF1F4, and the ribbon layer.
#[test]
#[ignore = "needs the ov2core-003 capture"]
fn oracle_unreached_visual_and_ribbons() {
    let mut rng = oracle::Rng(0x5eed_0002);
    let start = base(HUB, 0x800e_fad0);
    let mut visual = Tally::default();
    let small: [(u32, u32, u32, usize); 6] = [
        (0, 0x800e_fac8, 0x800e_fad0, 1),
        (3, 0x800f_0c9c, 0x800f_0cd8, 2),
        (10, 0x800f_0e30, 0x800f_0e64, 2),
        (15, 0x800f_0ef4, 0x800f_0f30, 3),
        (16, 0x800f_0f30, 0x800f_0f44, 1),
        (24, 0x800f_0d84, 0x800f_0db4, 1),
    ];
    for (op, entry, end, words) in small {
        for _ in 0..16 {
            let mut m = start.clone();
            vary_visual(&mut m, &mut rng);
            let operands: Vec<u32> = (0..words).map(|_| rng.next()).collect();
            instruction(&mut m, op, &operands);
            if op == 15 {
                m.sh(INPUT + 4, (rng.next() & 0xf) as u16);
            }
            visual.case(
                &format!("op {op}"),
                &m,
                entry,
                (entry, end),
                [INPUT, 0, 0, 0],
            );
        }
    }
    {
        let mut m = start.clone();
        visual.case(
            "op 25",
            &m.clone(),
            0x800f_0db4,
            (0x800f_0db4, 0x800f_0dd4),
            [INPUT, 0, 0, 0],
        );
        let block = m.lw(ov2_core::VISUAL);
        let flags = m.lw(block);
        m.sw(block, flags | 8);
        visual.case(
            "op 25 set",
            &m,
            0x800f_0db4,
            (0x800f_0db4, 0x800f_0dd4),
            [INPUT, 0, 0, 0],
        );
    }
    // Lines (17/18/19): points around the screen.
    for (op, entry, end, points) in [
        (17, 0x800f_0748, 0x800f_08d8, 2usize),
        (18, 0x800f_08d8, 0x800f_0a9c, 3),
        (19, 0x800f_0a9c, 0x800f_0c88, 4),
    ] {
        for _ in 0..32 {
            let mut m = start.clone();
            vary_visual(&mut m, &mut rng);
            let words: Vec<u32> = (0..points).map(|_| rng.next()).collect();
            instruction(&mut m, op, &words);
            visual.case(
                &format!("op {op}"),
                &m,
                entry,
                (entry, end),
                [INPUT, 0, 0, 0],
            );
        }
    }
    // Draw area (23): x -1 (whole screen) and every clip edge.
    let mut areas = std::collections::BTreeMap::new();
    for case in 0..96u32 {
        let mut m = start.clone();
        vary_visual(&mut m, &mut rng);
        let x = if case % 8 == 0 {
            -1i32
        } else {
            rng.next() as i32 % 900 - 450
        };
        let y = rng.next() as i32 % 1200 - 600;
        let w = rng.next() % 500;
        let h = rng.next() % 700;
        instruction(
            &mut m,
            23,
            &[(x as u32 & 0xffff) | ((y as u32) << 16), w | (h << 16)],
        );
        let state = 0x1f80_0000;
        let block = m.lw(ov2_core::VISUAL);
        let pair = block + 0x298 + (m.lw(state + 0x18) << 2);
        let ox = i32::from(m.lh(pair)) + i32::from(m.lh(state + 4));
        let oy = i32::from(m.lh(pair + 2)) + i32::from(m.lh(state + 6));
        // The bottom test adds the width, as the original does.
        let edge = |start: i32, size: i32, extent: i32, limit: i32| {
            if start >= limit {
                "beyond"
            } else if start + size >= limit {
                "clipped end"
            } else if start >= 0 {
                "inside"
            } else if start + extent < 0 {
                "before"
            } else {
                "clipped start"
            }
        };
        let left = x + i32::from(ox as i16);
        let top = y + i32::from(oy as i16);
        let class = if x == -1 {
            ("screen", "screen")
        } else {
            (
                edge(left, w as i32, w as i32, 0x140),
                edge(top, w as i32, h as i32, 0x200),
            )
        };
        *areas.entry(class).or_insert(0usize) += 1;
        visual.case(
            "op 23",
            &m,
            0x800f_0560,
            (0x800f_0560, 0x800f_0748),
            [INPUT, 0, 0, 0],
        );
    }
    println!("ORACLE  op 23 branches (x, y): {} {areas:?}", areas.len());
    // Animated sprites (26) and tiles (27): frames before, inside and after.
    for (op, entry, end, stride) in [
        (26u32, 0x800f_1050u32, 0x800f_1430u32, 6usize),
        (27, 0x800f_1430, 0x800f_1674, 3),
    ] {
        for _ in 0..64 {
            let mut m = start.clone();
            vary_visual(&mut m, &mut rng);
            let count = 1 + rng.next() % 4;
            let first = rng.next() % 4;
            let mut words = vec![count | (first << 16)];
            for _ in 0..(count as usize * stride + 1) {
                words.push(rng.next());
            }
            instruction(&mut m, op, &words);
            if op == 26 {
                for frame in 0..count {
                    let at = INPUT + 0xc + frame * 24;
                    m.sh(at + 0x12, (rng.next() % 4) as u16);
                }
            }
            visual.case(
                &format!("op {op}"),
                &m,
                entry,
                (entry, end),
                [INPUT, 0, 0, 0],
            );
        }
    }
    visual.report("visual opcodes 0, 3, 10, 15..19, 23..27");
    assert_eq!(visual.blocked, 0);

    // Clip modes: distinct times, nonzero spans and offsets.
    let mut clip = Tally::default();
    let mut branches = std::collections::BTreeMap::new();
    let rect = OUT;
    let record = OUT + 0x10;
    for case in 0..160u32 {
        let mut m = start.clone();
        let mode = case % 6;
        let first = (rng.next() % 40) as i16 + 1;
        let second = first + 1 + (rng.next() % 30) as i16;
        let third = second + 1 + (rng.next() % 30) as i16;
        let now = first - 2 + (rng.next() % (third - first + 5) as u32) as i16;
        let ox = (rng.next() % 300) as i16;
        let oy = (rng.next() % 200) as i16;
        let x = ox + 1 + (rng.next() % 100) as i16;
        let y = if case % 5 == 0 {
            oy
        } else {
            oy + 1 + (rng.next() % 100) as i16
        };
        for (offset, value) in [
            (0, x),
            (2, y),
            (4, (rng.next() % 200) as i16),
            (6, (rng.next() % 200) as i16),
        ] {
            m.sh(rect + offset, value as u16);
        }
        for (offset, value) in [
            (0, 0i16),
            (2, mode as i16),
            (4, ox),
            (6, oy),
            (8, first),
            (0xa, second),
            (0xc, third),
            (0xe, now),
        ] {
            m.sh(record + offset, value as u16);
        }
        let phase = match mode {
            1..=3 if now <= first => "before",
            1 | 2 if now <= second => "first half",
            3 if now < second => "running",
            1 | 2 if now < third => "second half",
            1..=3 => "done",
            4 if now >= third => "shown",
            4 => "hidden",
            _ => "unchanged",
        };
        *branches
            .entry((mode, phase, mode == 1 && y == oy))
            .or_insert(0usize) += 1;
        clip.case(
            &format!("clip mode {mode}"),
            &m,
            0x800e_f21c,
            (0x800e_f21c, 0x800e_f85c),
            [rect, record, 0, 0],
        );
    }
    clip.report("800EF21C modes 0..5");
    println!(
        "ORACLE  800EF21C branches (mode, phase, y at origin): {} {branches:?}",
        branches.len()
    );
    assert_eq!(clip.blocked, 0);

    // 800EF85C: axis-aligned (mode 0, random halfwords) and rotated corners
    // (any other mode: on-screen rects, a center near them, any angle);
    // 800EF1F4.
    let mut corners = Tally::default();
    for case in 0..64u32 {
        let mut m = start.clone();
        let prim = OUT + 0x40;
        let mode = if case % 2 == 0 { 0 } else { 1 + rng.next() % 3 };
        if mode == 0 {
            for offset in (0..8).step_by(2) {
                m.sh(rect + offset, rng.next() as u16);
                m.sh(INPUT + offset, rng.next() as u16);
            }
        } else {
            let (x, y) = (rng.next() % 320, rng.next() % 240);
            let (w, h) = (1 + rng.next() % 200, 1 + rng.next() % 160);
            for (offset, value) in [(0, x), (2, y), (4, w), (6, h)] {
                m.sh(rect + offset, value as u16);
            }
            m.sh(INPUT, rng.next() as u16);
            m.sh(INPUT + 2, (x + rng.next() % (w + 1)) as u16);
            m.sh(INPUT + 4, (y + rng.next() % (h + 1)) as u16);
            m.sh(INPUT + 6, rng.next() as u16);
        }
        corners.case(
            "800ef85c",
            &m,
            0x800e_f85c,
            (0x800e_f85c, 0x800e_fac8),
            [prim, rect, INPUT, mode],
        );
    }
    for _ in 0..8 {
        let mut m = start.clone();
        for offset in (0..0x12).step_by(2) {
            m.sh(OUT + offset, rng.next() as u16);
        }
        corners.case(
            "800ef1f4",
            &m,
            0x800e_f1f4,
            (0x800e_f1f4, 0x800e_f21c),
            [OUT, 0, 0, 0],
        );
    }
    corners.report("800EF85C / 800EF1F4");

    // Ribbon layer.
    let map_state = base(HUB, 0x800e_7ab4);
    let mut ribbons = Tally::default();
    for case in 0..16u32 {
        let mut m = map_state.clone();
        let map = m.lw(ov2_core::MAP);
        let ot = m.lw(map + 8);
        let scene = m.lw(ov2_core::SCENE);
        let packet = m.lw(scene.wrapping_add(0x20094));
        let entry = ot + (rng.next() & 0xff) * 4;
        ribbons.case(
            "800ea53c",
            &m,
            0x800e_a53c,
            (0x800e_a53c, 0x800e_a598),
            [case, entry, packet, 0],
        );
    }
    for case in 0..16u32 {
        let mut m = map_state.clone();
        for offset in (0..0x20).step_by(4) {
            m.sw(OUT + 0x40 + offset, rng.next() ^ (rng.next() << 16));
        }
        m.sw(OUT + 0x54, rng.next() % 4000);
        m.sw(OUT + 0x58, rng.next() % 4000);
        m.sw(OUT + 0x5c, 2000 + rng.next() % 8000);
        for offset in (0..0x30).step_by(2) {
            m.sh(INPUT + offset, (rng.next() % 2000) as u16);
        }
        let count = 1 + case % 6;
        ribbons.case(
            "800ea7d0",
            &m,
            0x800e_a7d0,
            (0x800e_a7d0, 0x800e_a900),
            [INPUT, OUT, count, 0],
        );
    }
    for _ in 0..16 {
        let mut m = map_state.clone();
        let map = m.lw(ov2_core::MAP);
        let scene = m.lw(ov2_core::SCENE);
        let packet = m.lw(scene.wrapping_add(0x20094));
        for offset in (0..0x28).step_by(2) {
            m.sh(INPUT + offset, rng.next() as u16);
        }
        let segment = rng.next() % 15;
        ribbons.case(
            "800ea900",
            &m,
            0x800e_a900,
            (0x800e_a900, 0x800e_aa34),
            [map, INPUT, segment, packet],
        );
    }
    for case in 0..48u32 {
        let mut m = map_state.clone();
        for entry in 0..15u32 {
            let depth = if case % 3 == 0 {
                0x1000 + rng.next() % 0x7000
            } else {
                rng.next() % 0x10000
            };
            let width = depth * 2 / 3;
            m.sw(
                OUT + entry * 16,
                (rng.next() % (width + 1)).wrapping_sub(width / 2),
            );
            m.sw(
                OUT + entry * 16 + 4,
                (rng.next() % (width + 1)).wrapping_sub(width / 2),
            );
            m.sw(OUT + entry * 16 + 8, depth);
        }
        ribbons.case(
            "800eaa34",
            &m,
            0x800e_aa34,
            (0x800e_aa34, 0x800e_aafc),
            [OUT, 0, 0, 0],
        );
    }
    // 800EA6CC: the camera block and work translation filled with noise
    // (the routine rebuilds them from zero angles and position).
    let work = 0x1f80_0000;
    for _ in 0..16 {
        let mut m = map_state.clone();
        let map = m.lw(ov2_core::MAP);
        for offset in (0x2c..0xac).step_by(4) {
            m.sw(map + offset, rng.next());
        }
        for offset in (0xd8..0xe4).step_by(4) {
            m.sw(work + offset, rng.next());
        }
        ribbons.case(
            "800ea6cc",
            &m,
            0x800e_a6cc,
            (0x800e_a6cc, 0x800e_a7d0),
            [map, work, 0, 0],
        );
    }
    // 800EAAFC / 800EAE74: random trails under the ribbon camera, in view
    // (quads) or partly outside the view box (trails restart), random step
    // vectors; 800EAE74 also over its modes 0..3 and the counter around 60.
    let mut modes = std::collections::BTreeSet::new();
    let mut quads = Vec::new();
    for case in 0..48u32 {
        let mut m = map_state.clone();
        let map = m.lw(ov2_core::MAP);
        let scene = m.lw(ov2_core::SCENE);
        let packet = m.lw(scene.wrapping_add(0x20094));
        ov2_core::map::ribbon_camera(&mut m, map, work);
        vary_trails(&mut m, &mut rng, case % 4 != 0);
        m.sw(map + 0xfc, (rng.next() % 0x200) << 16);
        m.sw(map + 0x100, 0x100 + rng.next() % 0x200);
        if case < 32 {
            // Trails drawn: quad bytes beyond the two DR_MODEs (0x18).
            let drawn = ov2_core::map::draw_ribbons(&mut m.clone(), map, work, packet)
                .wrapping_sub(packet)
                .wrapping_sub(0x18);
            quads.push(drawn);
            ribbons.case(
                "800eaafc",
                &m,
                0x800e_aafc,
                (0x800e_aafc, 0x800e_ae74),
                [map, work, packet, 0],
            );
        } else {
            let mode = case % 4;
            let counter = 0x3a + (case / 4) % 5;
            m.sw(map + 0x1c, mode);
            m.sw(map + 0x28, counter);
            modes.insert((mode, counter >= 0x3c));
            ribbons.case(
                "800eae74",
                &m,
                0x800e_ae74,
                (0x800e_ae74, 0x800e_af0c),
                [map, work, packet, 0],
            );
        }
    }
    ribbons.report("ribbon layer");
    println!(
        "ORACLE  800EAE74 branches (mode, counter at 60): {} {modes:?}",
        modes.len()
    );
    quads.sort_unstable();
    println!(
        "ORACLE  800EAAFC quad bytes: none in {} cases, {}..{} otherwise",
        quads.iter().filter(|&&q| q == 0).count(),
        quads.iter().find(|&&q| q > 0).copied().unwrap_or(0),
        quads.last().copied().unwrap_or(0)
    );
    assert_eq!(ribbons.blocked, 0);
}

/// Group 3: every inner-dispatcher route (800CB408) for all 16 inner
/// states: results 1, 0x100 (with every choice and option combination the
/// routes test), 0x200, 0x10, 0x20, 0x80 and an unrouted value.
#[test]
#[ignore = "needs the ov2core-003 capture"]
fn oracle_dispatcher_routes() {
    let mut rng = oracle::Rng(0xd15b_a7c4);
    let start = base(HUB, 0x800c_b408);
    let mut tally = Tally::default();
    let mut branches = std::collections::BTreeSet::new();
    let row = INPUT + 0x100;
    for state in 0..16u32 {
        for result in [1u32, 0x100, 0x200, 0x10, 0x20, 0x80, 0x40] {
            let variants = if result == 0x100 { 40 } else { 6 };
            for variant in 0..variants {
                let mut m = start.clone();
                let ctx = context_of(&mut m);
                let scene = m.lw(ov2_core::SCENE);
                let callback = match result {
                    1 => 0x800c_bc70,
                    0x100 | 0x200 => {
                        m.sw(ctx + 0x12c, if result == 0x100 { 0x10 } else { 0x20 });
                        0x800c_bc78
                    }
                    other => {
                        // 800D1890 returns the first mission record's word
                        // when no record matches a0 (the scene pointer).
                        m.sw(0x800f_595c, other);
                        0x800d_1890
                    }
                };
                let substate = variant % 3;
                for slot in 0..3 {
                    m.sw(row + slot * 4, callback);
                }
                m.sw(ov2_core::STATE_TABLE + state * 4, row);
                m.sh(scene, state as u16);
                m.sh(scene + 2, substate as u16);
                m.sh(scene + 4, (rng.next() % 16) as u16);
                m.sh(scene + 6, (variant % 5) as u16);
                let mut options = (rng.next() & !0x50)
                    | [0, 8, 0x10, 0x80, 0x18, 0x90, 0x88][(variant % 7) as usize];
                if result != 0x100 && result != 0x200 && options & 0x50 == 0x50 {
                    options &= !0x40;
                }
                m.sw(ctx + 0x970, options);
                m.sw(ctx + 0x964, rng.next());
                let mission = [1u32, 31, 32, 37, 38][(variant / 7 % 5) as usize];
                m.sh(ctx + 0x390, mission as u16);
                // The route branch these inputs select (options 0x80 is
                // recomputed from the mission id for states 5 and 12).
                let bypass = (32..38).contains(&mission);
                let branch = match (result, state) {
                    (0x100, 0) if variant % 5 == 0 => {
                        format!("choice 0 options 0x80 {}", options & 0x80 != 0)
                    }
                    (0x100, 0 | 4) => format!("choice {}", variant % 5),
                    (0x100, 6) => format!("options 0x80 {}", options & 0x80 != 0),
                    (0x100, 12) => format!(
                        "options 8 {} 0x10 {} bypass {bypass}",
                        options & 8 != 0,
                        options & 0x10 != 0
                    ),
                    (0x100, 9) => format!(
                        "options 8 {} 0x10 {}",
                        options & 8 != 0,
                        options & 0x10 != 0
                    ),
                    (0x100, 13 | 14) => format!("options 0x10 {}", options & 0x10 != 0),
                    (0x200, 2) => format!("options 0x80 {}", options & 0x80 != 0),
                    _ => String::new(),
                };
                branches.insert((state, result, branch));
                tally.case(
                    &format!("state {state} result {result:#x} variant {variant}"),
                    &m,
                    0x800c_b408,
                    (0x800c_b408, 0x800c_b8b4),
                    [0x78, 0, 0, 0],
                );
            }
        }
    }
    tally.report("inner dispatcher routes");
    println!(
        "ORACLE  800CB408 route branches (state, result, condition): {}",
        branches.len()
    );
    assert_eq!(tally.blocked, 0);
    assert_eq!(tally.compared, 16 * (6 * 6 + 40));
}
