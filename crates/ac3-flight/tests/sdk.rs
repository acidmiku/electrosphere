//! PsyQ SDK translations (`ac3_flight::sdk`) on original function snapshots
//! (see `snapshot::check_call`), plus edge cases the captures do not reach.
//!
//! Captures: `shared-001` (window 80033C58, the first two flight callbacks,
//! up to three calls per routine), `shared-002` (window 80032510, gameplay
//! initialization), `shared-003` (after the first flight callback: windows
//! 80033C58 and the main-loop routines 80025C20, 80025BB0, 8002DDCC,
//! 8002C0B4, 800269E4, GetRCnt, ResetRCnt and DrawSync; device-facing SDK
//! calls), `world-002` (libgpu packet helpers) and `objects-001`. Every
//! captured
//! call of a routine is checked. Besides `check_call`'s comparison (all RAM
//! outside the stack, scratchpad, GTE, `v0`), each call is re-run to compare
//! the stack at and above the entry `$sp` (the caller's frames, where most
//! output locals live) and to count GTE register changes, so routines whose
//! only effect is a stack local or a GTE register are not vacuous.
use ac3_flight::{
    Fault, Machine, sdk,
    snapshot::{self, Snapshot},
};
use std::path::PathBuf;

const FLIGHT: &str = "shared-001";
const INIT: &str = "shared-002";
const WORLD: &str = "world-002";
/// Window 80033C58 and the main-loop windows after the first flight
/// callback (multi-window capture; see the report).
const MAIN: &str = "shared-003";
/// The `objects` agent's capture (window 80032510).
const OBJECTS: &str = "objects-001";

fn index(capture: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR"))
        .join("analysis/flight-runtime")
        .join(capture)
        .join("snapshots.jsonl")
}

/// Entry and matching return snapshot of call `occurrence` (the rule of
/// `check_call`).
fn pair(capture: &str, callee: u32, occurrence: usize) -> (Snapshot, Snapshot) {
    let path = index(capture);
    let list = snapshot::read_index(&path).unwrap();
    let entry = list
        .iter()
        .filter(|s| s.callee == callee && s.stage == "entry")
        .nth(occurrence)
        .unwrap();
    let exit = list
        .iter()
        .find(|s| {
            s.callee == callee
                && s.stage == "return"
                && s.sequence > entry.sequence
                && s.ra == entry.ra
                && s.registers.gpr[29] == entry.registers.gpr[29]
        })
        .unwrap();
    let directory = path.parent().unwrap();
    (
        Snapshot::load(directory, entry).unwrap(),
        Snapshot::load(directory, exit).unwrap(),
    )
}

#[derive(Debug, Default)]
struct Outcome {
    calls: usize,
    /// Bytes the original changed outside the stack (from `check_call`).
    changed: usize,
    /// Bytes the original changed at or above the entry `$sp`.
    stack_changed: usize,
    /// GTE data/control registers the original changed.
    gte_changed: usize,
    /// Calls whose asserted `v0` was nonzero.
    nonzero_v0: usize,
}

/// Check every captured call of `callee` in `capture`.
fn check_all(
    capture: &str,
    callee: u32,
    native: impl Fn(&mut Machine, &Snapshot) -> Option<u32>,
) -> Outcome {
    let calls = snapshot::read_index(&index(capture))
        .unwrap()
        .iter()
        .filter(|s| s.callee == callee && s.stage == "entry")
        .count();
    assert!(calls > 0, "{capture}: no {callee:08x} snapshots");
    let mut outcome = Outcome {
        calls,
        ..Outcome::default()
    };
    for occurrence in 0..calls {
        let changed =
            snapshot::check_call(&index(capture), callee, occurrence, |m, e| native(m, e))
                .unwrap_or_else(|error| panic!("{capture} #{occurrence}: {error}"));
        outcome.changed += changed;
        let (entry, exit) = pair(capture, callee, occurrence);
        let mut m = entry.machine();
        if let Some(v0) = native(&mut m, &entry) {
            outcome.nonzero_v0 += usize::from(v0 != 0);
        }
        let sp = (entry.gpr(29) & 0x1f_ffff) as usize;
        let differing: Vec<usize> = (sp..0x20_0000)
            .filter(|&i| m.ram()[i] != exit.ram[i])
            .collect();
        assert!(
            differing.is_empty(),
            "{capture} #{occurrence}: stack differs at {:x?}",
            &differing[..differing.len().min(16)]
        );
        outcome.stack_changed += (sp..0x20_0000)
            .filter(|&i| entry.ram[i] != exit.ram[i])
            .count();
        let registers = (&entry.info.registers, &exit.info.registers);
        outcome.gte_changed += (0..32)
            .filter(|&i| registers.0.cp2d[i] != registers.1.cp2d[i])
            .count()
            + (0..32)
                .filter(|&i| registers.0.cp2c[i] != registers.1.cp2c[i])
                .count();
    }
    eprintln!("{callee:08x} {capture}: {outcome:?}");
    outcome
}

fn args(e: &Snapshot) -> [u32; 4] {
    [e.gpr(4), e.gpr(5), e.gpr(6), e.gpr(7)]
}

/// Stack word `offset` of the caller's outgoing argument area.
fn stack_arg(m: &mut Machine, e: &Snapshot, offset: u32) -> u32 {
    m.lw(e.gpr(29) + offset)
}

// ---- geometry registers ------------------------------------------------------

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-001"]
fn set_rot_matrix_800a5518() {
    let o = check_all(FLIGHT, 0x800a5518, |m, e| {
        sdk::set_rot_matrix(m, e.gpr(4));
        None
    });
    assert!(o.gte_changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-001"]
fn set_color_matrix_800a5548() {
    let o = check_all(FLIGHT, 0x800a5548, |m, e| {
        sdk::set_color_matrix(m, e.gpr(4));
        None
    });
    assert!(o.gte_changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-001"]
fn set_trans_matrix_800a5578() {
    // The captured translations repeat the loaded ones: poison TRX..TRZ so a
    // match proves the loads.
    let o = check_all(FLIGHT, 0x800a5578, |m, e| {
        for i in 5..8 {
            m.gte.control[i] = !m.gte.control[i];
        }
        sdk::set_trans_matrix(m, e.gpr(4));
        None
    });
    assert_eq!(o.calls, 3);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-001"]
fn read_geom_offset_800a5598() {
    let o = check_all(FLIGHT, 0x800a5598, |m, e| {
        // Poison both outputs so a match proves the stores.
        for out in [e.gpr(4), e.gpr(5)] {
            let value = m.lw(out);
            m.sw(out, !value);
        }
        sdk::read_geom_offset(m, e.gpr(4), e.gpr(5));
        None
    });
    assert_eq!(o.calls, 3);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-001"]
fn read_geom_screen_800a55b8() {
    let o = check_all(FLIGHT, 0x800a55b8, |m, _| Some(sdk::read_geom_screen(m)));
    assert_eq!(o.nonzero_v0, o.calls);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-001"]
fn set_back_color_800a55c8() {
    let o = check_all(FLIGHT, 0x800a55c8, |m, e| {
        sdk::set_back_color(m, e.gpr(4), e.gpr(5), e.gpr(6));
        None
    });
    assert!(o.calls > 0);
    // The flight value repeats; re-check with poisoned registers.
    snapshot::check_call(&index(FLIGHT), 0x800a55c8, 0, |m, e| {
        for i in 13..16 {
            m.gte.control[i] = !m.gte.control[i];
        }
        sdk::set_back_color(m, e.gpr(4), e.gpr(5), e.gpr(6));
        None
    })
    .unwrap();
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-001,-002"]
fn set_geom_offset_800a55e8() {
    for capture in [FLIGHT, INIT] {
        check_all(capture, 0x800a55e8, |m, e| {
            m.gte.control[24] = !m.gte.control[24];
            m.gte.control[25] = !m.gte.control[25];
            sdk::set_geom_offset(m, e.gpr(4), e.gpr(5));
            None
        });
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-001,-002"]
fn set_geom_screen_800a5608() {
    for capture in [FLIGHT, INIT] {
        check_all(capture, 0x800a5608, |m, e| {
            m.gte.control[26] = !m.gte.control[26];
            sdk::set_geom_screen(m, e.gpr(4));
            None
        });
    }
}

// ---- transforms ----------------------------------------------------------------

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-001"]
fn rot_trans_pers_800a5828() {
    let o = check_all(FLIGHT, 0x800a5828, |m, e| {
        Some(sdk::rot_trans_pers(
            m,
            e.gpr(4),
            e.gpr(5),
            e.gpr(6),
            e.gpr(7),
        ))
    });
    assert!(o.gte_changed > 0 && o.nonzero_v0 > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-001"]
fn rot_trans_pers3_800a5858() {
    let o = check_all(FLIGHT, 0x800a5858, |m, e| {
        let sxy1 = stack_arg(m, e, 0x10);
        let sxy2 = stack_arg(m, e, 0x14);
        let p = stack_arg(m, e, 0x18);
        let flag = stack_arg(m, e, 0x1c);
        Some(sdk::rot_trans_pers3(
            m,
            [e.gpr(4), e.gpr(5), e.gpr(6)],
            [e.gpr(7), sxy1, sxy2],
            p,
            flag,
        ))
    });
    assert!(o.gte_changed > 0 && o.nonzero_v0 > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-001"]
fn rot_trans_800a58b8() {
    let o = check_all(FLIGHT, 0x800a58b8, |m, e| {
        Some(sdk::rot_trans(m, e.gpr(4), e.gpr(5), e.gpr(6)))
    });
    assert!(o.gte_changed > 0 && o.changed + o.stack_changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-002"]
fn apply_matrix_sv_800a5348() {
    let o = check_all(INIT, 0x800a5348, |m, e| {
        Some(sdk::apply_matrix_sv(m, e.gpr(4), e.gpr(5), e.gpr(6)))
    });
    assert!(o.gte_changed > 0 && o.nonzero_v0 == o.calls);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-002,objects-001"]
fn apply_matrix_lv_800a4d78() {
    for capture in [INIT, OBJECTS] {
        let o = check_all(capture, 0x800a4d78, |m, e| {
            Some(sdk::apply_matrix_lv(m, e.gpr(4), e.gpr(5), e.gpr(6)))
        });
        assert!(o.gte_changed > 0 && o.changed + o.stack_changed > 0);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-001"]
fn apply_rot_matrix_lv_800a4fe8() {
    let o = check_all(FLIGHT, 0x800a4fe8, |m, e| {
        Some(sdk::apply_rot_matrix_lv(m, e.gpr(4), e.gpr(5)))
    });
    assert!(o.gte_changed > 0 && o.changed + o.stack_changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-001,-002"]
fn mul_matrix_800a5128() {
    for capture in [FLIGHT, INIT, OBJECTS] {
        let o = check_all(capture, 0x800a5128, |m, e| {
            Some(sdk::mul_matrix(m, e.gpr(4), e.gpr(5)))
        });
        assert!(o.gte_changed > 0 && o.changed + o.stack_changed > 0);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-001"]
fn mul_matrix2_800a5238() {
    let o = check_all(FLIGHT, 0x800a5238, |m, e| {
        Some(sdk::mul_matrix2(m, e.gpr(4), e.gpr(5)))
    });
    assert!(o.gte_changed > 0 && o.changed + o.stack_changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-001"]
fn transpose_matrix_800a5918() {
    let o = check_all(FLIGHT, 0x800a5918, |m, e| {
        Some(sdk::transpose_matrix(m, e.gpr(4), e.gpr(5)))
    });
    assert!(o.changed + o.stack_changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-001,-002"]
fn rot_matrix_yxz_800a5be8() {
    for capture in [FLIGHT, INIT, OBJECTS] {
        let o = check_all(capture, 0x800a5be8, |m, e| {
            Some(sdk::rot_matrix_yxz(m, e.gpr(4), e.gpr(5)))
        });
        assert!(o.changed + o.stack_changed > 0);
    }
}

// ---- scalar math ---------------------------------------------------------------

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-001"]
fn square_root0_800a47c8() {
    let o = check_all(FLIGHT, 0x800a47c8, |m, e| {
        Some(sdk::square_root0(m, e.gpr(4)))
    });
    assert!(o.nonzero_v0 > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-001"]
fn vector_normal_ss_800a4928() {
    let o = check_all(FLIGHT, 0x800a4928, |m, e| {
        Some(sdk::vector_normal_ss_v0(m, e.gpr(4), e.gpr(5)))
    });
    assert!(o.gte_changed > 0 && o.nonzero_v0 > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-001"]
fn ratan2_800a6088() {
    let o = check_all(FLIGHT, 0x800a6088, |m, e| {
        Some(sdk::ratan2(m, e.gpr(4), e.gpr(5)))
    });
    assert!(o.nonzero_v0 > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-002"]
fn divdi3_8009ce80() {
    let o = check_all(INIT, 0x8009ce80, |m, e| {
        let [a0, a1, a2, a3] = args(e);
        let n = (u64::from(a1) << 32 | u64::from(a0)) as i64;
        let d = (u64::from(a3) << 32 | u64::from(a2)) as i64;
        Some(sdk::divdi3(m, n, d) as u32)
    });
    assert!(o.nonzero_v0 > 0);
    // The high word (v1) as well.
    for occurrence in 0..o.calls {
        let (entry, exit) = pair(INIT, 0x8009ce80, occurrence);
        let mut m = entry.machine();
        let [a0, a1, a2, a3] = args(&entry);
        let n = (u64::from(a1) << 32 | u64::from(a0)) as i64;
        let d = (u64::from(a3) << 32 | u64::from(a2)) as i64;
        let q = sdk::divdi3(&mut m, n, d) as u64;
        assert_eq!(
            (q as u32, (q >> 32) as u32),
            (exit.gpr(2), exit.gpr(3)),
            "{n} / {d}"
        );
    }
}

// ---- libc ------------------------------------------------------------------------

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-001,-002"]
fn rand_800ac178() {
    for capture in [FLIGHT, INIT, OBJECTS] {
        let o = check_all(capture, 0x800ac178, |m, _| Some(sdk::rand(m)));
        assert!(o.changed > 0 && o.nonzero_v0 > 0);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-001"]
fn memset_800ac148() {
    let o = check_all(FLIGHT, 0x800ac148, |m, e| {
        // Poison the destination so a match proves every store.
        let n = e.gpr(6) as i32;
        for i in 0..n.max(0) as u32 {
            let value = m.lbu(e.gpr(4) + i);
            m.sb(e.gpr(4) + i, !value);
        }
        Some(sdk::memset(m, e.gpr(4), e.gpr(5), e.gpr(6)))
    });
    assert!(o.nonzero_v0 > 0);
}

// ---- libgpu ------------------------------------------------------------------------

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-001,-002,world-002"]
fn cat_prim_800aabd8() {
    for capture in [FLIGHT, INIT, WORLD] {
        let o = check_all(capture, 0x800aabd8, |m, e| {
            let value = m.lw(e.gpr(4));
            m.sw(e.gpr(4), value ^ 0x00ff_ffff);
            sdk::cat_prim(m, e.gpr(4), e.gpr(5));
            None
        });
        assert!(o.changed + o.stack_changed > 0);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-002,world-002"]
fn set_draw_tpage_800aac08() {
    for capture in [INIT, WORLD] {
        let o = check_all(capture, 0x800aac08, |m, e| {
            let p = e.gpr(4);
            let (tag, command) = (m.lw(p), m.lw(p + 4));
            m.sw(p, !tag & 0xff00_0000 | tag & 0x00ff_ffff);
            m.sw(p + 4, !command);
            sdk::set_draw_tpage(m, p, e.gpr(5), e.gpr(6), e.gpr(7));
            None
        });
        assert!(o.changed > 0);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-001,-002,world-002"]
fn set_draw_area_800a73f0() {
    for capture in [FLIGHT, INIT, WORLD] {
        let o = check_all(capture, 0x800a73f0, |m, e| {
            let p = e.gpr(4);
            for offset in [4, 8] {
                let value = m.lw(p + offset);
                m.sw(p + offset, !value);
            }
            Some(sdk::set_draw_area(m, p, e.gpr(5)))
        });
        assert!(o.nonzero_v0 == o.calls);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/world-002"]
fn draw_area_helpers_800a7938_800a79d0() {
    let o = check_all(WORLD, 0x800a7938, |m, e| {
        Some(sdk::draw_area_start(m, e.gpr(4) as i16, e.gpr(5) as i16))
    });
    assert!(o.nonzero_v0 == o.calls);
    let o = check_all(WORLD, 0x800a79d0, |m, e| {
        Some(sdk::draw_area_end(m, e.gpr(4) as i16, e.gpr(5) as i16))
    });
    assert!(o.nonzero_v0 == o.calls);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-001,-003"]
fn clear_otag_r_800a6b50() {
    for capture in [FLIGHT, MAIN] {
        let o = check_all(capture, 0x800a6b50, |m, e| {
            let (_, exit) = pair_for(capture, e);
            // The original's DMA-completion poll count (800A8BCC calls).
            m.host_results
                .push_back(exit_word(&exit, sdk::GPU_TIMEOUT_POLLS));
            Some(sdk::clear_otag_r(m, e.gpr(4), e.gpr(5)))
        });
        assert!(o.changed > 4 * 3);
    }
}

/// The entry/return pair of an entry snapshot (by sequence).
fn pair_for(capture: &str, e: &Snapshot) -> (Snapshot, Snapshot) {
    let list = snapshot::read_index(&index(capture)).unwrap();
    let occurrence = list
        .iter()
        .filter(|s| s.callee == e.info.callee && s.stage == "entry")
        .position(|s| s.sequence == e.info.sequence)
        .unwrap();
    pair(capture, e.info.callee, occurrence)
}

fn exit_word(exit: &Snapshot, address: u32) -> u32 {
    let at = (address & 0x1f_ffff) as usize;
    u32::from_le_bytes(exit.ram[at..at + 4].try_into().unwrap())
}

// ---- device-facing routines ---------------------------------------------------------

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-003"]
fn vsync_800a0568() {
    let o = check_all(MAIN, 0x800a0568, |m, e| Some(sdk::vsync(m, e.gpr(4))));
    assert!(o.nonzero_v0 == o.calls);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-003"]
fn memcpy_800ac108() {
    let o = check_all(MAIN, 0x800ac108, |m, e| {
        for i in 0..e.gpr(6) {
            let value = m.lbu(e.gpr(4) + i);
            m.sb(e.gpr(4) + i, !value);
        }
        Some(sdk::memcpy(m, e.gpr(4), e.gpr(5), e.gpr(6)))
    });
    assert!(o.nonzero_v0 == o.calls);
}

/// Invert the words every libgpu submission rewrites (timeout deadline and
/// polls, saved mask, submitted flag), so a match proves the stores.
fn poison_submission_words(m: &mut Machine) {
    for address in [0x800b9920, 0x800b9924, 0x800b9914, 0x800b97ec] {
        let value = m.lw(address);
        m.sw(address, !value);
    }
}

/// Push the original's SetIntrMask results for one libgpu submission: the
/// mask it saved at 800B9914, then the SetIntrMask(saved) result.
fn push_submission_masks(m: &mut Machine, exit: &Snapshot) {
    let saved = exit_word(exit, sdk::GPU_SAVED_MASK);
    m.host_results.push_back(saved);
    m.host_results.push_back(0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-003"]
fn draw_otag_800a6c58() {
    let o = check_all(MAIN, 0x800a6c58, |m, e| {
        let (_, exit) = pair_for(MAIN, e);
        push_submission_masks(m, &exit);
        poison_submission_words(m);
        let v0 = sdk::dispatch(m, 0x800a6c58, args(e));
        assert_eq!(m.host_calls.len(), 3, "mask, DMA2 start, mask");
        assert_eq!(m.host_calls[1].function, 0x800a8360);
        v0
    });
    assert_eq!(o.calls, 2);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-003"]
fn put_draw_env_800a6cc8() {
    let o = check_all(MAIN, 0x800a6cc8, |m, e| {
        let (_, exit) = pair_for(MAIN, e);
        push_submission_masks(m, &exit);
        // Poison the packet words SetDrawEnv writes (7, or 10 with a clear)
        // and the static copy, so a match proves the stores.
        poison_submission_words(m);
        let words = if m.lbu(e.gpr(4) + 0x18) != 0 { 10 } else { 7 };
        for address in (e.gpr(4) + 0x1c..e.gpr(4) + 0x1c + words * 4).chain(0x800b97f4..0x800b9850)
        {
            let value = m.lbu(address);
            m.sb(address, !value);
        }
        Some(sdk::put_draw_env(m, e.gpr(4)))
    });
    assert!(o.changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-003"]
fn put_disp_env_800a6e94() {
    let o = check_all(MAIN, 0x800a6e94, |m, e| {
        for address in 0x800b9850..0x800b9864 {
            let value = m.lbu(address);
            m.sb(address, !value);
        }
        Some(sdk::put_disp_env(m, e.gpr(4)))
    });
    assert!(o.changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-002,-003"]
fn draw_sync_800a6664_and_800a8a5c() {
    for capture in [INIT, MAIN] {
        for callee in [0x800a6664, 0x800a8a5c] {
            if capture == INIT && callee == 0x800a8a5c {
                continue;
            }
            let o = check_all(capture, callee, |m, e| {
                let (_, exit) = pair_for(capture, e);
                // Poison the timeout words (plain stores in mode 0).
                for address in [0x800b9920, 0x800b9924] {
                    let value = m.lw(address);
                    m.sw(address, !value);
                }
                m.host_results
                    .push_back(exit_word(&exit, sdk::GPU_TIMEOUT_POLLS));
                sdk::dispatch(m, callee, args(e))
            });
            assert!(o.calls > 0);
        }
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-003"]
fn set_intr_mask_check_callback_video_mode() {
    let o = check_all(MAIN, 0x800a08f4, |m, e| {
        let (_, exit) = pair_for(MAIN, e);
        m.host_results.push_back(exit.gpr(2));
        Some(sdk::set_intr_mask(m, e.gpr(4)))
    });
    assert!(o.nonzero_v0 > 0);
    check_all(MAIN, 0x800a08cc, |m, _| Some(sdk::check_callback(m)));
    let o = check_all(MAIN, 0x800a120c, |m, _| Some(sdk::get_video_mode(m)));
    assert!(o.nonzero_v0 == o.calls, "PAL");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-003"]
fn pad_chk_vsync_8009b1e0() {
    let o = check_all(MAIN, 0x8009b1e0, |m, _| Some(sdk::pad_chk_vsync(m)));
    assert!(o.nonzero_v0 > 0 && o.changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-003"]
fn root_counters_800ad934_800ad9d0() {
    check_all(MAIN, 0x800ad934, |m, e| {
        let (_, exit) = pair_for(MAIN, e);
        m.host_results.push_back(exit.gpr(2));
        Some(sdk::get_rcnt(m, e.gpr(4)))
    });
    let o = check_all(MAIN, 0x800ad9d0, |m, e| {
        let v0 = sdk::reset_rcnt(m, e.gpr(4));
        assert_eq!(m.host_calls.len(), 1);
        Some(v0)
    });
    assert!(o.nonzero_v0 == o.calls);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-003"]
fn cd_sync_800a14bc_800a1eec() {
    for callee in [0x800a14bc, 0x800a1eec] {
        let o = check_all(MAIN, callee, |m, e| {
            Some(sdk::cd_sync(m, e.gpr(4), e.gpr(5)))
        });
        assert!(o.changed > 0);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-003"]
fn spu_set_key_8009f3c8() {
    let o = check_all(MAIN, 0x8009f3c8, |m, e| {
        Some(sdk::spu_set_key(m, e.gpr(4), e.gpr(5)))
    });
    assert!(o.calls == 3);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-003"]
fn spu_voice_and_common_attributes() {
    let o = check_all(MAIN, 0x8009fd68, |m, e| {
        // Poison the notes of the selected voices when the mask selects the
        // sample note (0 or bit 0x40), so a match proves those stores.
        let (voices, mask) = (m.lw(e.gpr(4)), m.lw(e.gpr(4) + 4));
        for voice in (0..24).filter(|v| voices & (1 << v) != 0 && (mask == 0 || mask & 0x40 != 0)) {
            let value = m.lhu(sdk::SPU_VOICE_NOTE + voice * 2);
            m.sh(sdk::SPU_VOICE_NOTE + voice * 2, !value);
        }
        Some(sdk::spu_set_voice_attr(m, e.gpr(4)))
    });
    assert!(o.calls == 3);
    check_all(MAIN, 0x8009f888, |m, e| {
        Some(sdk::spu_set_common_attr(m, e.gpr(4)))
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-003"]
fn spu_get_all_keys_status_8009fcdc() {
    let o = check_all(MAIN, 0x8009fcdc, |m, e| {
        let (_, exit) = pair_for(MAIN, e);
        let at = (e.gpr(4) & 0x1f_ffff) as usize;
        for &byte in &exit.ram[at..at + 24] {
            m.host_results.push_back(u32::from(byte));
        }
        for i in 0..24 {
            m.sb(e.gpr(4) + i, 0xee);
        }
        Some(sdk::spu_get_all_keys_status(m, e.gpr(4)))
    });
    assert!(o.calls == 2);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-003"]
fn spu_note2pitch_and_fset_rxxa() {
    let o = check_all(MAIN, 0x800a0368, |m, e| {
        let [a0, a1, a2, a3] = args(e);
        Some(sdk::spu_note2pitch(m, a0, a1, a2, a3))
    });
    assert!(o.nonzero_v0 == o.calls);
    let o = check_all(MAIN, 0x8009dff8, |m, e| {
        let v0 = sdk::spu_fset_rxxa(m, e.gpr(4), e.gpr(5));
        assert_eq!(m.host_calls.len(), 1);
        Some(v0)
    });
    assert!(o.nonzero_v0 == o.calls);
}

// ---- edge cases without captures ------------------------------------------------

#[test]
fn divdi3_signs_overflow_and_zero_divisor() {
    let mut m = Machine::default();
    for (n, d) in [
        (7i64, 2i64),
        (-7, 2),
        (7, -2),
        (-7, -2),
        (1 << 62, 3),
        (-(1 << 40) - 5, 1 << 20),
        (i64::MAX, i64::MIN),
        (i64::MIN, i64::MIN),
        (i64::MIN, 7),
    ] {
        assert_eq!(sdk::divdi3(&mut m, n, d), n / d, "{n} / {d}");
    }
    assert_eq!(sdk::divdi3(&mut m, i64::MIN, -1), i64::MIN);
    assert!(m.take_fault().is_ok());
    assert_eq!(sdk::divdi3(&mut m, 5, 0), 0);
    assert_eq!(
        m.take_fault(),
        Err(Fault::Untranslated {
            address: 0x8009_d0c0
        })
    );
}

#[test]
fn memset_and_memcpy_lengths() {
    let mut m = Machine::default();
    let base = 0x8010_0001; // unaligned
    m.write_bytes(base, &[1, 2, 3, 4, 5, 6, 7]);
    assert_eq!(sdk::memset(&mut m, base, 0x1ab, 0), 0, "n = 0 returns 0");
    assert_eq!(sdk::memset(&mut m, base, 0x1ab, -3i32 as u32), 0);
    assert_eq!(sdk::memset(&mut m, 0, 0x1ab, 4), 0, "null destination");
    assert_eq!(m.read_bytes(base, 7), [1, 2, 3, 4, 5, 6, 7]);
    assert_eq!(sdk::memset(&mut m, base, 0x1ab, 3), base);
    assert_eq!(
        m.read_bytes(base - 1, 9),
        [0, 0xab, 0xab, 0xab, 4, 5, 6, 7, 0]
    );

    m.write_bytes(base, &[1, 2, 3, 4, 5, 6, 7]);
    assert_eq!(sdk::memcpy(&mut m, base + 3, base, 0), base + 3);
    assert_eq!(sdk::memcpy(&mut m, 0, base, 4), 0, "null destination");
    assert_eq!(sdk::memcpy(&mut m, base + 3, base, -1i32 as u32), base + 3);
    assert_eq!(m.read_bytes(base, 7), [1, 2, 3, 4, 5, 6, 7]);
    // Forward byte copy into an overlapping later range repeats the prefix.
    assert_eq!(sdk::memcpy(&mut m, base + 2, base, 5), base + 2);
    assert_eq!(m.read_bytes(base, 7), [1, 2, 1, 2, 1, 2, 1]);
    assert!(m.take_fault().is_ok());
}

#[test]
fn rand_sequence() {
    let mut m = Machine::default();
    sdk::srand(&mut m, 1);
    let first: Vec<u32> = (0..3).map(|_| sdk::rand(&mut m)).collect();
    // state = state * 0x41C64E6D + 0x3039 from 1.
    let mut state = 1u32;
    let expected: Vec<u32> = (0..3)
        .map(|_| {
            state = state.wrapping_mul(0x41c6_4e6d).wrapping_add(0x3039);
            (state >> 16) & 0x7fff
        })
        .collect();
    assert_eq!(first, expected);
    assert_eq!(m.lw(sdk::RAND_STATE), state);
}

#[test]
fn clear_otag_r_single_entry() {
    let mut m = Machine::default();
    m.sw(0x800b_97dc, 0x8010_0000);
    m.sw(0x8010_002c, 0x800a_7b1c);
    m.sw(sdk::VSYNC_COUNTER, 1000);
    let ot = 0x8012_0000;
    m.sw(ot, 0x1234_5678);
    m.sw(ot + 4, 0x1111_1111);
    assert_eq!(sdk::clear_otag_r(&mut m, ot, 1), ot);
    assert_eq!(m.lw(ot), 0x000b_98a4);
    assert_eq!(m.lw(ot + 4), 0x1111_1111, "only n words");
    assert_eq!(m.lw(0x800b_98a4), 0x040b_9890);
    assert_eq!(m.lw(sdk::GPU_TIMEOUT_DEADLINE), 1000 + 0xf0);
    assert_eq!(m.lw(sdk::GPU_TIMEOUT_POLLS), 0);
    assert_eq!(m.host_calls.len(), 1);
    // Three words, two polls.
    m.host_results.push_back(2);
    sdk::clear_otag_r(&mut m, ot, 3);
    assert_eq!(m.lw(ot + 8), 0x0012_0004);
    assert_eq!(m.lw(ot + 4), 0x0012_0000);
    assert_eq!(m.lw(ot), 0x000b_98a4);
    assert_eq!(m.lw(sdk::GPU_TIMEOUT_POLLS), 2);
    assert!(m.take_fault().is_ok());
    // More than 0xF0000 polls takes the (untranslated) timeout path.
    m.host_results.push_back(0xf_0002);
    sdk::clear_otag_r(&mut m, ot, 2);
    assert_eq!(m.lw(sdk::GPU_TIMEOUT_POLLS), 0xf_0002);
    assert_eq!(
        m.take_fault(),
        Err(Fault::Untranslated {
            address: 0x800a_8c20
        })
    );
}

#[test]
fn ratan2_and_square_root0_without_tables() {
    let mut m = Machine::default();
    assert_eq!(sdk::ratan2(&mut m, 0, 0), 0);
    // Zero tables: only the quadrant arithmetic remains.
    assert_eq!(sdk::ratan2(&mut m, 0, 5), 0);
    assert_eq!(sdk::ratan2(&mut m, 5, 0), 0x400);
    assert_eq!(sdk::ratan2(&mut m, 0, -5i32 as u32), 0x800);
    assert_eq!(sdk::ratan2(&mut m, -5i32 as u32, 0), -0x400i32 as u32);
    assert_eq!(
        sdk::ratan2(&mut m, -5i32 as u32, -5i32 as u32),
        -0x400i32 as u32
    );
    // 0x80000000 inputs wrap without trapping in these paths.
    sdk::ratan2(&mut m, 0x8000_0000, 3);
    sdk::ratan2(&mut m, 3, 0x8000_0000);
    assert!(m.take_fault().is_ok());
    // (0x80000000, 0) divides by zero after the wrap: the break is reported.
    sdk::ratan2(&mut m, 0, 0x8000_0000);
    assert_eq!(
        m.take_fault(),
        Err(Fault::Untranslated {
            address: 0x800a_61a0
        })
    );
    assert_eq!(sdk::square_root0(&mut m, 0), 0);
    assert_eq!(sdk::square_root0(&mut m, -1i32 as u32), 0);
}

/// Real tables from a flight snapshot: square roots, arctangent symmetry
/// and RotMatrixYXZ with negative angles.
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/shared-001"]
fn tables_square_root_atan_and_negative_angles() {
    let (entry, _) = pair(FLIGHT, 0x800a47c8, 0);
    let mut m = entry.machine();
    for k in [16u32, 100, 1000, 4096, 40000] {
        let root = sdk::square_root0(&mut m, k * k);
        assert!(root.abs_diff(k) <= k / 32 + 1, "sqrt({}) = {root}", k * k);
    }
    for (y, x) in [(1, 1), (100, 3), (3, 100), (-70, 20), (20, -70), (-9, -9)] {
        let a = sdk::ratan2(&mut m, y as u32, x as u32) as i32;
        let b = sdk::ratan2(&mut m, (-y) as u32, x as u32) as i32;
        assert_eq!(a, -b, "atan2 is odd in y");
        let expected = (f64::from(y).atan2(f64::from(x)) * 2048.0 / std::f64::consts::PI) as i32;
        assert!(
            (a - expected).abs() <= 2,
            "ratan2({y}, {x}) = {a}, {expected}"
        );
    }
    // RotMatrixYXZ against the table with sin(-a) = -sin(|a| & 0xFFF).
    let trig = |m: &mut Machine, a: i16| -> (i32, i32) {
        let index = i32::from(a).unsigned_abs() & 0xfff;
        let word = m.lw(sdk::SIN_COS_TABLE + index * 4);
        let sin = i32::from(word as u16 as i16);
        (if a < 0 { -sin } else { sin }, (word as i32) >> 16)
    };
    let mul = |a: i32, b: i32| a.wrapping_mul(b) >> 12;
    let angles = 0x8010_0000;
    let matrix = 0x8010_0010;
    for (x, y, z) in [
        (300i16, -700i16, 1200i16),
        (-300, 700, -1200),
        (-4095, -1, -2048),
        (i16::MIN, 0, 0),
    ] {
        for (i, a) in [x, y, z].into_iter().enumerate() {
            m.sh(angles + i as u32 * 2, a as u16);
        }
        assert_eq!(sdk::rot_matrix_yxz(&mut m, angles, matrix), matrix);
        let actual: Vec<i16> = (0..9).map(|i| m.lh(matrix + i * 2)).collect();
        let ((sx, cx), (sy, cy), (sz, cz)) = (trig(&mut m, x), trig(&mut m, y), trig(&mut m, z));
        let (sysx, cysx) = (mul(sy, sx), mul(cy, sx));
        let expected: Vec<i16> = [
            mul(cy, cz) + mul(sysx, sz),
            mul(sysx, cz) - mul(cy, sz),
            mul(sy, cx),
            mul(sz, cx),
            mul(cz, cx),
            -sx,
            mul(cysx, sz) - mul(sy, cz),
            mul(sy, sz) + mul(cysx, cz),
            mul(cy, cx),
        ]
        .map(|v| v as i16)
        .to_vec();
        assert_eq!(actual, expected, "angles {x} {y} {z}");
        if x == i16::MIN {
            assert_eq!(actual, [4096, 0, 0, 0, 4096, 0, 0, 0, 4096]);
        }
    }
    assert!(m.take_fault().is_ok());
}

#[test]
fn dispatch_claims_by_original_address() {
    let mut m = Machine::default();
    m.sw(0x8010_0000, 0x0900_0000);
    assert_eq!(
        sdk::dispatch(&mut m, 0x800a_abd8, [0x8010_0000, 0x8012_3456, 0, 0]),
        Some(0x0912_3456)
    );
    assert_eq!(
        sdk::dispatch(&mut m, 0x8009_ce80, [(-9i32) as u32, u32::MAX, 2, 0]),
        Some(-4i32 as u32)
    );
    assert_eq!(sdk::dispatch(&mut m, 0x8001_0000, [0; 4]), None);
}

#[test]
fn vsync_modes_and_host_results() {
    let mut m = Machine::default();
    m.sw(sdk::VSYNC_COUNTER, 500);
    m.sw(sdk::VSYNC_LAST_COUNT, 498);
    m.sw(sdk::VSYNC_LAST_HCOUNT, 0x1_0010);
    assert_eq!(sdk::vsync(&mut m, -1i32 as u32), 500);
    assert!(m.host_calls.is_empty(), "VSync(-1) only reads RAM");
    // Mode 1: hblanks since the last VSync (16 bits).
    m.host_results.push_back(0x1_0050);
    assert_eq!(sdk::vsync(&mut m, 1), 0x40);
    assert_eq!(m.lw(sdk::VSYNC_LAST_COUNT), 498, "mode 1 stores nothing");
    // Mode 0 with defaults: 0 hblanks, counter past the next VBlank.
    assert_eq!(sdk::vsync(&mut m, 0), 0);
    assert_eq!(m.lw(sdk::VSYNC_LAST_COUNT), 501);
    assert_eq!(m.lw(sdk::VSYNC_LAST_HCOUNT), 0);
    // Mode 4 waits for last + 3; host results override the counters.
    m.host_results.extend([7, 520, 9]);
    assert_eq!(sdk::vsync(&mut m, 4), 7);
    assert_eq!(m.lw(sdk::VSYNC_LAST_COUNT), 520);
    assert_eq!(m.lw(sdk::VSYNC_LAST_HCOUNT), 9);
    sdk::vsync(&mut m, 4);
    assert_eq!(m.lw(sdk::VSYNC_LAST_COUNT), 524, "520 + 4 - 1, plus one");
    assert_eq!(m.host_calls.len(), 4);
}

#[test]
fn spu_set_key_direct_and_buffered() {
    let mut m = Machine::default();
    // 8002D9B8's SpuSetKey(0, 0xFFFFFF): all voices off.
    m.sw(sdk::SPU_KEYED, 0x0012_3456);
    assert_eq!(sdk::spu_set_key(&mut m, 0, 0xff_ffff), 0);
    assert_eq!(m.lw(sdk::SPU_KEYED), 0);
    assert_eq!(m.host_calls[0].args, [0, 0xff_ffff, 0, 0]);
    assert_eq!(sdk::spu_set_key(&mut m, 1, 0x0300_0005), 0x0000_0005);
    assert_eq!(m.lw(sdk::SPU_KEYED), 5, "voices masked to 24 bits");
    assert_eq!(sdk::spu_set_key(&mut m, 2, 1), 1);
    assert_eq!(m.host_calls.len(), 2);
    // Buffered mode: RAM only.
    m.sw(0x800b_2630, 1);
    let buffer = sdk::SPU_KEY_BUFFER;
    m.sh(buffer + 4, 0x00ff);
    m.sh(buffer + 6, 0x0001);
    assert_eq!(sdk::spu_set_key(&mut m, 1, 0x01_000f), 0);
    assert_eq!(m.lhu(buffer), 0x000f);
    assert_eq!(m.lhu(buffer + 2), 0x0001);
    assert_eq!(m.lhu(buffer + 4), 0x00f0, "key-off bits cleared");
    assert_eq!(m.lhu(buffer + 6), 0);
    assert_eq!(m.lw(0x800b_25fc) & 1, 1);
    assert_eq!(m.lw(0x800b_25f8), 0x01_000f);
    assert_eq!(sdk::spu_set_key(&mut m, 0, 0x3), 0);
    assert_eq!(m.lhu(buffer + 4), 0x0003);
    assert_eq!(m.lhu(buffer), 0x000c);
    assert_eq!(m.lw(0x800b_25f8), 0x01_000c);
    assert_eq!(m.host_calls.len(), 2, "no device access when buffered");
}

#[test]
fn dma_callback_table() {
    let mut m = Machine::default();
    m.sw(0x800b_3fb4, 0x8010_0000);
    m.sw(0x8010_0004, 0x800a_1124);
    assert_eq!(sdk::dec_dct_out_callback(&mut m, 0x8005_0000), 0);
    assert_eq!(m.lw(sdk::DMA_CALLBACKS + 4), 0x8005_0000);
    assert_eq!(m.host_calls.len(), 1);
    assert_eq!(sdk::dec_dct_out_callback(&mut m, 0x8005_0000), 0x8005_0000);
    assert_eq!(m.host_calls.len(), 1, "unchanged: no device access");
    assert_eq!(sdk::dma_callback(&mut m, 1, 0), 0x8005_0000);
    assert_eq!(m.lw(sdk::DMA_CALLBACKS + 4), 0);
    // A replaced setter is called by address.
    m.sw(0x8010_0004, 0x8001_2340);
    sdk::dma_callback(&mut m, 2, 5);
    assert_eq!(
        m.take_fault(),
        Err(Fault::Untranslated {
            address: 0x8001_2340
        })
    );
}

#[test]
fn root_counters_pad_and_callback_flags() {
    let mut m = Machine::default();
    assert_eq!(sdk::get_rcnt(&mut m, 0xf200_0003), 0);
    assert_eq!(sdk::reset_rcnt(&mut m, 0xf200_0003), 0);
    assert!(m.host_calls.is_empty(), "invalid counters touch no device");
    m.host_results.push_back(0x1_2345);
    assert_eq!(sdk::get_rcnt(&mut m, 0xf200_0002), 0x2345);
    assert_eq!(sdk::reset_rcnt(&mut m, 2), 1);
    m.sw(sdk::PAD_VSYNC_FLAG, 3);
    assert_eq!(sdk::pad_chk_vsync(&mut m), 3);
    assert_eq!(sdk::pad_chk_vsync(&mut m), 0);
    m.sh(0x800b_2f2e, 1);
    assert_eq!(sdk::check_callback(&mut m), 1);
    assert_eq!(sdk::set_intr_mask(&mut m, 0x2d), 0, "default previous mask");
}

#[test]
fn cd_sync_modes() {
    let mut m = Machine::default();
    m.sw(sdk::VSYNC_COUNTER, 100);
    let result = 0x8010_0000;
    // Running command: mode 1 returns 0 after one poll.
    m.sb(sdk::CD_SYNC_STATUS, 1);
    assert_eq!(sdk::cd_sync(&mut m, 1, result), 0);
    assert_eq!(m.lw(sdk::CD_TIMEOUT_DEADLINE), 100 + 0x3c0);
    assert_eq!(m.lw(sdk::CD_TIMEOUT_POLLS), 1);
    // Complete: status reset to 2, result copied.
    m.sb(sdk::CD_SYNC_STATUS, 5);
    m.write_bytes(sdk::CD_SYNC_RESULT, &[1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(sdk::cd_sync(&mut m, 0, result), 5);
    assert_eq!(m.lbu(sdk::CD_SYNC_STATUS), 2);
    assert_eq!(m.read_bytes(result, 9), [1, 2, 3, 4, 5, 6, 7, 8, 0]);
    assert!(m.take_fault().is_ok());
    // Blocking wait on a running command: extra polls, then untranslated.
    m.sb(sdk::CD_SYNC_STATUS, 1);
    m.host_results.push_back(3);
    assert_eq!(sdk::cd_sync(&mut m, 0, 0), 0);
    assert_eq!(m.lw(sdk::CD_TIMEOUT_POLLS), 4);
    assert_eq!(
        m.take_fault(),
        Err(Fault::Untranslated {
            address: 0x800a_2134
        })
    );
}

#[test]
fn spu_note2pitch_octaves() {
    let mut m = Machine::default();
    // Semitone table entry 0 = 0x1000 and fine entry 0 = 0x8000 give a base
    // pitch of 0x800 at the center note.
    m.sh(0x800b_2ab4, 0x1000);
    m.sh(0x800b_2acc, 0x8000);
    assert_eq!(
        sdk::spu_note2pitch(&mut m, 60, 0, 24 + 60, 0),
        0x3fff,
        "+2 octaves"
    );
    assert_eq!(sdk::spu_note2pitch(&mut m, 60, 0, 12 + 60, 0), 0x400);
    assert_eq!(sdk::spu_note2pitch(&mut m, 60, 0, 60, 0), 0x200);
    assert_eq!(sdk::spu_note2pitch(&mut m, 60, 0, 48, 0), 0x100);
    // A negative remainder borrows an octave (entry 0 for -12 too).
    assert_eq!(sdk::spu_note2pitch(&mut m, 60, 0, 36, 0), 0x80);
}

#[test]
fn set_draw_env_clear_variants() {
    let mut m = Machine::default();
    m.sh(sdk::DRAW_LIMIT_WIDTH, 1024);
    m.sh(sdk::DRAW_LIMIT_HEIGHT, 512);
    let env = 0x8010_0000;
    let dr = env + 0x1c;
    // clip (64, 16, 320, 2000), offset (64, 16), tpage 0x1f, no clear.
    for (offset, value) in [(0, 64u16), (2, 16), (4, 320), (6, 2000), (8, 64), (0xa, 16)] {
        m.sh(env + offset, value);
    }
    m.sh(env + 0x14, 0x1f);
    m.sb(env + 0x16, 1);
    assert_eq!(sdk::set_draw_env(&mut m, dr, env), 6);
    assert_eq!(m.lbu(dr + 3), 6);
    assert_eq!(m.lw(dr + 4), 0xe300_0000 | (16 << 10) | 64);
    assert_eq!(m.lw(dr + 8), 0xe400_0000 | (511 << 10) | 383);
    assert_eq!(m.lw(dr + 0xc), 0xe500_0000 | (16 << 11) | 64);
    assert_eq!(m.lw(dr + 0x10), 0xe100_021f);
    assert_eq!(m.lw(dr + 0x14), 0xe200_0000);
    assert_eq!(m.lw(dr + 0x18), 0xe600_0000);
    // Clear with a 64-aligned x and width: FILL at the absolute position,
    // height clamped to 511.
    m.sb(env + 0x18, 1);
    m.write_bytes(env + 0x19, &[0x11, 0x22, 0x33]);
    assert_eq!(sdk::set_draw_env(&mut m, dr, env), 9);
    assert_eq!(m.lw(dr + 0x1c), 0x0233_2211);
    assert_eq!(m.lw(dr + 0x20), (16 << 16) | 64);
    assert_eq!(m.lw(dr + 0x24), (511 << 16) | 320);
    // Unaligned width: TILE relative to the offset.
    m.sh(env + 4, 100);
    sdk::set_draw_env(&mut m, dr, env);
    assert_eq!(m.lw(dr + 0x1c), 0x6033_2211);
    assert_eq!(m.lw(dr + 0x20), 0);
    assert_eq!(m.lw(dr + 0x24), (511 << 16) | 100);
}

#[test]
fn gpu_submission_paths() {
    let mut m = Machine::default();
    let table = 0x8010_0000;
    m.sw(0x800b_97dc, table);
    m.sw(table + 8, 0x800a_83fc);
    m.sw(table + 0x18, 0x800a_8360);
    m.sw(table + 0x3c, 0x800a_8a5c);
    m.sw(sdk::VSYNC_COUNTER, 50);
    m.host_results.extend([0x2d, 0]);
    sdk::draw_otag(&mut m, 0x8012_0000);
    assert_eq!(m.lw(sdk::GPU_SAVED_MASK), 0x2d);
    assert_eq!(m.lw(0x800b_97ec), 1);
    assert_eq!(m.lw(sdk::GPU_TIMEOUT_DEADLINE), 50 + 0xf0);
    let functions: Vec<u32> = m.host_calls.iter().map(|c| c.function).collect();
    assert_eq!(functions, [0x800a_08f4, 0x800a_8360, 0x800a_08f4]);
    assert_eq!(m.host_calls[2].args[0], 0x2d, "mask restored");
    assert_eq!(sdk::draw_sync(&mut m, 1), 0);
    assert!(m.take_fault().is_ok());
    // Queued mode with entries pending is not translated.
    m.sb(0x800b_97e5, 1);
    m.sw(sdk::GPU_QUEUE_WRITE, 3);
    sdk::draw_otag(&mut m, 0x8012_0000);
    assert_eq!(
        m.take_fault(),
        Err(Fault::Untranslated {
            address: 0x800a_8520
        })
    );
    assert_eq!(sdk::draw_sync(&mut m, 1), 3);
    assert_eq!(
        m.take_fault(),
        Err(Fault::Untranslated {
            address: 0x800a_8b34
        })
    );
}

#[test]
fn put_disp_env_unchanged_writes_display_start_only() {
    let mut m = Machine::default();
    let table = 0x8010_0000;
    m.sw(0x800b_97dc, table);
    m.sw(table + 0x10, 0x800a_82e8);
    let env = 0x8011_0000;
    m.sh(env, 0x140);
    m.sh(env + 2, 0x10);
    // Static copy equal to the environment, pad0 not 8.
    for i in 0..0x14 {
        let value = m.lbu(env + i);
        m.sb(sdk::GPU_DISP_ENV + i, value);
    }
    assert_eq!(sdk::put_disp_env(&mut m, env), env);
    assert_eq!(m.host_calls.len(), 1);
    assert_eq!(m.host_calls[0].args[0], 0x0500_4140);
    assert_eq!(m.lbu(sdk::GPU_GP1_SHADOW + 5), 0x40);
    // A changed width selects mode bits and recomputes the ranges.
    m.sh(env + 4, 320);
    m.sh(env + 6, 256);
    m.sw(sdk::VIDEO_MODE, 1);
    sdk::put_disp_env(&mut m, env);
    let commands: Vec<u32> = m.host_calls[1..].iter().map(|c| c.args[0] >> 24).collect();
    assert_eq!(commands, [5, 8, 6, 7]);
    assert_eq!(m.host_calls[2].args[0], 0x0800_0009, "PAL, 320 wide");
    assert_eq!(m.lbu(env + 0x12), 1, "pad0 = video mode after the ranges");
    assert!(m.take_fault().is_ok());
}

/// SpuSetKey (8009F3C8) direct and buffered paths (formerly a copy in
/// `script.rs`).
#[test]
fn spu_key_paths() {
    let mut m = Machine::default();
    m.sw(0x800b_25d0, 0xff_ffff);
    sdk::spu_set_key(&mut m, 0, 0x00f0_000f);
    assert_eq!(m.lw(0x800b_25d0), 0x0f_fff0);
    assert_eq!(m.host_calls.len(), 1);
    m.sw(0x800b_2630, 1);
    m.sh(0x800c_5c78, 0xffff);
    m.sh(0x800c_5c7a, 0xff);
    sdk::spu_set_key(&mut m, 0, 0x0001_0003);
    assert_eq!(m.lhu(0x800c_5c7c), 3);
    assert_eq!(m.lhu(0x800c_5c7e), 1);
    assert_eq!(m.lhu(0x800c_5c78), 0xfffc);
    assert_eq!(m.lhu(0x800c_5c7a), 0xfe);
    assert_eq!(m.lw(0x800b_25fc), 1);
    assert_eq!(m.host_calls.len(), 1);
    assert!(m.take_fault().is_ok());
}
