//! HUD and overlay routines (the 80039D84, 80033524, 80033544 and 8003357C
//! trees) on original entry snapshots must reproduce the matching return
//! snapshots (`snapshot::check_call`: RAM outside the stack, scratchpad, all
//! GTE registers, v0).
//!
//! Captures: `hud-003` brackets the first two calls of every routine in the
//! flight callbacks of frames 307/309/311; `hud-004` brackets chosen routines
//! in later frames (431, 559, 1029, 1371, 1795, 1801, 1855, 1923, 2487) of a
//! run with weapon, target and view inputs (`analysis/flight-runtime/hud-002`
//! holds the call tree of that run).
//!
//! Tests of routines whose subtree reaches a shared or SDK routine that is
//! still a stub (`Fault::Untranslated`) use [`check_blocked`]: they pass
//! while that stub is the only failure and become full comparisons once the
//! stub is translated.
use ac3_flight::SmallData;
use ac3_flight::{Machine, hud, snapshot};
use snapshot::Snapshot;
use std::path::{Path, PathBuf};

fn data(relative: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR")).join(relative)
}

fn early() -> PathBuf {
    data("analysis/flight-runtime/hud-003/snapshots.jsonl")
}

fn late() -> PathBuf {
    data("analysis/flight-runtime/hud-004/snapshots.jsonl")
}

/// Occurrence index (for `check_call`) of the `callee` entry captured in
/// `frame` of `index`.
fn at_frame(index: &Path, callee: u32, frame: u64) -> usize {
    snapshot::read_index(index)
        .unwrap()
        .iter()
        .filter(|s| s.callee == callee && s.stage == "entry")
        .position(|s| s.frame == frame)
        .unwrap_or_else(|| panic!("{callee:08x}: no entry in frame {frame}"))
}

/// Strict comparison; returns the bytes the original changed.
fn check(
    index: &Path,
    callee: u32,
    occurrence: usize,
    native: impl FnOnce(&mut Machine, &Snapshot) -> Option<u32>,
) -> usize {
    snapshot::check_call(index, callee, occurrence, native).unwrap()
}

/// Like [`check`], for a routine whose subtree reaches one of `stubs`
/// (shared/SDK routines still raising `Fault::Untranslated`). While the first
/// fault is one of those stubs the comparison cannot run and the test
/// reports it as blocked; any other failure panics.
fn check_blocked(
    index: &Path,
    callee: u32,
    occurrence: usize,
    stubs: &[u32],
    native: impl FnOnce(&mut Machine, &Snapshot) -> Option<u32>,
) {
    match snapshot::check_call(index, callee, occurrence, native) {
        Ok(changed) => eprintln!("{callee:08x}: unblocked, {changed} bytes reproduced"),
        Err(error)
            if stubs.iter().any(|stub| {
                error.ends_with(&format!("untranslated original routine {stub:#010x}"))
            }) =>
        {
            eprintln!("BLOCKED {error}");
        }
        Err(error) => panic!("{error}"),
    }
}

/// Word at the caller's `$sp + offset` in the entry snapshot (stack
/// arguments 5.. are at +0x10, +0x14, ...).
fn stack_arg(e: &Snapshot, offset: u32) -> u32 {
    let at = (e.gpr(29).wrapping_add(offset) & 0x1f_ffff) as usize;
    u32::from_le_bytes(e.ram[at..at + 4].try_into().unwrap())
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003"]
fn glyph_3b00c() {
    for occurrence in 0..2 {
        let changed = check(&early(), 0x8003b00c, occurrence, |m, e| {
            hud::glyph_3b00c(
                m,
                e.gpr(4),
                e.gpr(5),
                e.gpr(6),
                e.gpr(7),
                stack_arg(e, 0x10),
                stack_arg(e, 0x14),
            );
            None
        });
        assert!(changed > 0);
    }
}

/// Every captured call of `callee` to check: both hud-003 occurrences (or
/// the one there is) and the hud-004 entries of `late_frames`.
fn hud_cases(callee: u32, late_frames: &[u64]) -> Vec<(PathBuf, usize)> {
    let early_count = snapshot::read_index(&early())
        .unwrap()
        .iter()
        .filter(|s| s.callee == callee && s.stage == "entry")
        .count();
    let mut cases: Vec<(PathBuf, usize)> = (0..early_count).map(|i| (early(), i)).collect();
    for &frame in late_frames {
        cases.push((late(), at_frame(&late(), callee, frame)));
    }
    cases
}

/// Partial check of a routine whose only missing pieces are void stubs that
/// write a few known words (e.g. CatPrim): run it on the entry state,
/// require its first fault (if any) to be one of `stubs`, and require that
/// every difference from the original return state lies in the words
/// `allowed(entry)` (what those stubs write). Scratchpad and GTE must match
/// exactly. Returns the bytes the original changed.
fn check_partial(
    index: &Path,
    callee: u32,
    occurrence: usize,
    stubs: &[u32],
    allowed: impl Fn(&Snapshot) -> Vec<u32>,
    native: impl FnOnce(&mut Machine, &Snapshot),
) -> usize {
    let directory = index.parent().unwrap();
    let infos = snapshot::read_index(index).unwrap();
    let entry = infos
        .iter()
        .filter(|s| s.callee == callee && s.stage == "entry")
        .nth(occurrence)
        .unwrap();
    let exit = infos
        .iter()
        .find(|s| {
            s.callee == callee
                && s.stage == "return"
                && s.sequence > entry.sequence
                && s.ra == entry.ra
                && s.registers.gpr[29] == entry.registers.gpr[29]
        })
        .unwrap();
    let entry = Snapshot::load(directory, entry).unwrap();
    let exit = Snapshot::load(directory, exit).unwrap();
    let mut m = entry.machine();
    native(&mut m, &entry);
    if let Err(fault) = m.take_fault() {
        assert!(
            stubs
                .iter()
                .any(|&stub| fault == ac3_flight::Fault::Untranslated { address: stub }),
            "{callee:08x}: {fault}"
        );
    }
    let words = allowed(&entry);
    let differences = snapshot::compare(&m, &exit);
    let outside: Vec<u32> = differences
        .ram
        .iter()
        .copied()
        .filter(|&address| !words.contains(&(address & !3)))
        .collect();
    assert!(
        outside.is_empty()
            && differences.scratch.is_empty()
            && differences.gte_data.is_empty()
            && differences.gte_control.is_empty(),
        "{callee:08x}: {} outside the stub words: {outside:08x?}",
        differences.summary()
    );
    snapshot::changed_bytes(&entry, &exit)
}

/// Word at `address` in the entry snapshot's RAM.
fn ram_word(e: &Snapshot, address: u32) -> u32 {
    let at = (address & 0x1f_ffff) as usize;
    u32::from_le_bytes(e.ram[at..at + 4].try_into().unwrap())
}

/// Check every case of a routine without arguments or result; returns the
/// bytes changed per case.
fn hud_void(callee: u32, late_frames: &[u64], run: Routine) -> Vec<usize> {
    hud_cases(callee, late_frames)
        .into_iter()
        .map(|(index, occurrence)| {
            check(&index, callee, occurrence, |m, _| {
                run(m);
                None
            })
        })
        .collect()
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
fn popups_overlay_33524() {
    let changed = hud_void(0x80033524, &[559, 1801], hud::popups::overlay_33524);
    assert!(changed.iter().all(|&c| c > 0), "{changed:?}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
fn popups_popups_4b248() {
    let changed = hud_void(0x8004b248, &[559, 1801], hud::popups::popups_4b248);
    assert!(changed.iter().all(|&c| c > 0), "{changed:?}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
fn popups_begin_4b0d4() {
    let changed = hud_void(0x8004b0d4, &[559, 1801], hud::popups::begin_4b0d4);
    assert!(changed.iter().any(|&c| c > 0), "{changed:?}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
fn popups_link_4b11c() {
    let changed = hud_void(0x8004b11c, &[559, 1801], hud::popups::link_4b11c);
    assert!(changed.iter().all(|&c| c > 0), "{changed:?}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
fn popups_build_4b2d8() {
    // The popup list is empty in these frames (only the clear path runs);
    // the drawing paths are checked by `popups_active` below.
    hud_void(0x8004b2d8, &[559, 1801], hud::popups::build_4b2d8);
}

/// A routine without arguments or result.
type Routine = fn(&mut Machine);

/// hud-006: popup entries active in frames 431-557 (six entries) and
/// 2027-2403 (thirteen), bracketing fade-in, shown and shrinking states.
fn popup_frames() -> PathBuf {
    data("analysis/flight-runtime/hud-006/snapshots.jsonl")
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-006"]
fn popups_active() {
    let index = popup_frames();
    let frames = [
        431, 433, 449, 501, 541, 551, 557, 2027, 2101, 2375, 2403, 2405,
    ];
    let routines: [(u32, Routine); 4] = [
        (0x80033524, hud::popups::overlay_33524),
        (0x8004b248, hud::popups::popups_4b248),
        (0x8004b2d8, hud::popups::build_4b2d8),
        (0x8004b11c, hud::popups::link_4b11c),
    ];
    for (callee, run) in routines {
        for frame in frames {
            let changed = check(&index, callee, at_frame(&index, callee, frame), |m, _| {
                run(m);
                None
            });
            // In 2405 the list has just emptied: 8004B2D8 only finds it free.
            assert!(
                changed > 0 || (callee == 0x8004b2d8 && frame == 2405),
                "{callee:08x} frame {frame}"
            );
        }
    }
}

#[test]
fn popups_clear_4b74c() {
    let mut m = Machine::default();
    m.sw(ac3_flight::GP + 0x470, 0x8010_0000);
    m.sw(0x8010_0110, 5);
    m.sw(0x8010_011c, 2);
    hud::popups::clear_4b74c(&mut m);
    assert_eq!(m.lw(0x8010_0110), 0);
    assert_eq!(m.lw(0x8010_011c), 0xff);
    assert!(m.take_fault().is_ok());
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
fn stream_overlay_33544() {
    let changed = hud_void(0x80033544, &[559, 1801], hud::stream::overlay_33544);
    assert!(changed.iter().any(|&c| c > 0), "{changed:?}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
fn stream_overlay_2cfcc() {
    for (index, occurrence) in hud_cases(0x8002cfcc, &[559, 1801]) {
        check(&index, 0x8002cfcc, occurrence, |m, e| {
            Some(hud::stream::overlay_2cfcc(m, e.gpr(4), e.gpr(5)))
        });
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
fn stream_stream_word_2c558() {
    for (index, occurrence) in hud_cases(0x8002c558, &[559, 1801]) {
        check(&index, 0x8002c558, occurrence, |m, e| {
            Some(hud::stream::stream_word_2c558(m, e.gpr(4), e.gpr(5)))
        });
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-004"]
fn stream_entry_2b188() {
    for frame in [559, 1801] {
        let occurrence = at_frame(&late(), 0x8002b188, frame);
        check(&late(), 0x8002b188, occurrence, |m, _| {
            Some(hud::stream::entry_2b188(m))
        });
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
fn stream_digits_2c8fc() {
    let changed = hud_void(0x8002c8fc, &[559, 1801], hud::stream::digits_2c8fc);
    assert!(changed.iter().any(|&c| c > 0), "{changed:?}");
}

/// The drawers of 8005CA50 take (panel, ot, block) in a0..a2.
fn panel_drawer(callee: u32, run: fn(&mut Machine, u32, u32, u32)) -> Vec<usize> {
    hud_cases(callee, &[559, 1801])
        .into_iter()
        .map(|(index, occurrence)| {
            check(&index, callee, occurrence, |m, e| {
                run(m, e.gpr(4), e.gpr(5), e.gpr(6));
                None
            })
        })
        .collect()
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
fn tree_5cbf4_heading_5af88() {
    let changed = panel_drawer(0x8005af88, hud::tree_5cbf4::heading_5af88);
    assert!(changed.iter().all(|&c| c > 0), "{changed:?}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
fn tree_5cbf4_timer_5b908() {
    let changed = panel_drawer(0x8005b908, hud::tree_5cbf4::timer_5b908);
    assert!(changed.iter().all(|&c| c > 0), "{changed:?}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
fn tree_5cbf4_damage_5bd40() {
    let changed = panel_drawer(0x8005bd40, hud::tree_5cbf4::damage_5bd40);
    assert!(changed.iter().all(|&c| c > 0), "{changed:?}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
fn tree_5cbf4_missiles_5c270() {
    let changed = panel_drawer(0x8005c270, hud::tree_5cbf4::missiles_5c270);
    assert!(changed.iter().all(|&c| c > 0), "{changed:?}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
fn tree_5cbf4_frame_5c718() {
    let changed = panel_drawer(0x8005c718, hud::tree_5cbf4::frame_5c718);
    assert!(changed.iter().all(|&c| c > 0), "{changed:?}");
}

/// Blocked on SDK CatPrim (800AABD8): until it is translated, everything
/// except the three tags it links (block +0x14, +0x104, +0x78) must match.
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
fn tree_5cbf4_altitude_5b450() {
    for (index, occurrence) in hud_cases(0x8005b450, &[559, 1801]) {
        check_blocked(&index, 0x8005b450, occurrence, &[0x800aabd8], |m, e| {
            hud::tree_5cbf4::altitude_5b450(m, e.gpr(4), e.gpr(5), e.gpr(6));
            None
        });
        let changed = check_partial(
            &index,
            0x8005b450,
            occurrence,
            &[0x800aabd8],
            |e| [0x14, 0x104, 0x78].map(|offset| e.gpr(6) + offset).to_vec(),
            |m, e| hud::tree_5cbf4::altitude_5b450(m, e.gpr(4), e.gpr(5), e.gpr(6)),
        );
        assert!(changed > 0);
    }
}

/// Blocked on SDK CatPrim (800AABD8) through 8005B450 (partial check
/// except the three tags it links in the altitude block, chain + 0x9C).
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
fn tree_5cbf4_draw_5ca50() {
    for (index, occurrence) in hud_cases(0x8005ca50, &[559, 1801]) {
        check_blocked(&index, 0x8005ca50, occurrence, &[0x800aabd8], |m, e| {
            hud::tree_5cbf4::draw_5ca50(m, e.gpr(4), e.gpr(5), e.gpr(6));
            None
        });
        let changed = check_partial(
            &index,
            0x8005ca50,
            occurrence,
            &[0x800aabd8],
            |e| {
                [0x14, 0x104, 0x78]
                    .map(|offset| e.gpr(6) + 0x9c + offset)
                    .to_vec()
            },
            |m, e| hud::tree_5cbf4::draw_5ca50(m, e.gpr(4), e.gpr(5), e.gpr(6)),
        );
        assert!(changed > 0);
    }
}

/// Blocked on SDK CatPrim (800AABD8) through 8005B450 (partial check
/// except its three tags in this buffer's altitude block).
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
fn tree_5cbf4_panel_5cbf4() {
    for (index, occurrence) in hud_cases(0x8005cbf4, &[559, 1801]) {
        check_blocked(&index, 0x8005cbf4, occurrence, &[0x800aabd8], |m, e| {
            hud::tree_5cbf4::panel_5cbf4(m, e.gpr(4));
            None
        });
        let changed = check_partial(
            &index,
            0x8005cbf4,
            occurrence,
            &[0x800aabd8],
            |e| {
                let context = ram_word(e, ac3_flight::GP + 0x2dc);
                let buffer = ram_word(e, context + 4);
                let chains = ram_word(e, ac3_flight::GP + 0x4f4);
                let altitude = ram_word(e, chains + buffer * 4) + 0x9c;
                [0x14, 0x104, 0x78].map(|offset| altitude + offset).to_vec()
            },
            |m, e| hud::tree_5cbf4::panel_5cbf4(m, e.gpr(4)),
        );
        assert!(changed > 0);
    }
}
/// Synthetic panel at 0x80100000, ordering-table slot at 0x80120000,
/// packet block at 0x80110000, font 1 with u = code * 8.
fn panel_machine() -> Machine {
    let mut m = Machine::default();
    m.sw(0x800a_fa84 + 4, 8);
    m.sw(0x8012_0000, 0x00ff_ffff);
    m
}

/// 8005B908 in timer mode 1 below 1500 ticks (no capture): `ss:ff` in red,
/// separator red while tick bit 2 is clear, dark otherwise.
#[test]
fn tree_5cbf4_timer_mode_1() {
    let (panel, ot, block) = (0x8010_0000, 0x8012_0000, 0x8011_0000);
    // 1003 = 40 * 25 + 3 and 1004 = 40 * 25 + 4: seconds 40, frames 12/16.
    for (ticks, last, separator) in [(1003u32, 16, [0x83, 0, 0]), (1004, 48, [0, 0, 0])] {
        let mut m = panel_machine();
        m.sh(panel + 0x26, 1);
        m.sw(panel + 0x28, ticks);
        hud::tree_5cbf4::timer_5b908(&mut m, panel, ot, block);
        let us: Vec<u8> = [0x28, 0x3c, 0x64, 0x78]
            .iter()
            .map(|&offset| m.lbu(block + offset + 0xc))
            .collect();
        assert_eq!(us, [32, 0, 8, last]);
        for offset in [0x28, 0x3c, 0x50, 0x64, 0x78] {
            let rgb = [4, 5, 6].map(|i| m.lbu(block + offset + i));
            assert_eq!(rgb, [0x83, 0, 0], "glyph at +{offset:#x} is red");
        }
        assert_eq!([4, 5, 6].map(|i| m.lbu(block + i)), [0x83, 0, 0]);
        assert_eq!([4, 5, 6].map(|i| m.lbu(block + 0x14 + i)), separator);
        assert_eq!(m.lw(ot) & 0xff_ffff, block & 0xff_ffff);
        assert!(m.take_fault().is_ok());
    }
}

/// 8005B450 with a positive altitude word (shown negative, no capture):
/// the minus sign is drawn at +0xF0 and linked first.
#[test]
fn tree_5cbf4_altitude_negative() {
    let (panel, ot, block) = (0x8010_0000, 0x8012_0000, 0x8011_0000);
    let mut m = panel_machine();
    m.sp = 0x801f_fff0;
    // 0x1E000 >> 12 = 30, then -60 / 3 = -20.
    m.sw(panel + 4, 0x1_e000);
    hud::tree_5cbf4::altitude_5b450(&mut m, panel, ot, block);
    let sign = block + 0xf0;
    assert_eq!(
        (m.lbu(sign + 0xc), m.lbu(sign + 0xd)),
        (0x2c, 10),
        "code 11"
    );
    assert_eq!(m.lh(sign + 8), 0xf3);
    // Digits 0 then 2 from x = 0x11B leftwards.
    assert_eq!(m.lbu(block + 0x8c + 0xc), 0);
    assert_eq!(m.lbu(block + 0xa0 + 0xc), 16);
    assert_eq!(m.lh(block + 0xa0 + 8), 0x111);
    // The sign packet continues the slot's previous link.
    assert_eq!(m.lw(sign) & 0xff_ffff, 0xff_ffff);
    // Ladder: tens digit -2 becomes 8, ticks at y = 8 + 0x68 - 10 + 10 * i.
    let ticks: Vec<i16> = (0..5).map(|i| m.lh(block + 0x32 + 0x14 * i)).collect();
    assert_eq!(ticks, [102, 112, 122, 132, 142]);
    assert_eq!(m.sp, 0x801f_fff0);
    let fault = m.take_fault();
    assert!(
        fault.is_ok()
            || fault
                == Err(ac3_flight::Fault::Untranslated {
                    address: 0x800aabd8
                }),
        "{fault:?}"
    );
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
fn tree_3e828_markers_3e828() {
    let changed = hud_void(
        0x8003e828,
        &[559, 1029, 1801],
        hud::tree_3e828::markers_3e828,
    );
    assert!(changed.iter().all(|&c| c > 0), "{changed:?}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
fn tree_3e828_begin_3e860() {
    let changed = hud_void(0x8003e860, &[559, 1801], hud::tree_3e828::begin_3e860);
    assert!(changed.iter().any(|&c| c > 0), "{changed:?}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
fn tree_3e828_link_3e89c() {
    let changed = hud_void(0x8003e89c, &[559, 1801], hud::tree_3e828::link_3e89c);
    assert!(changed.iter().all(|&c| c > 0), "{changed:?}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
fn tree_3e828_targets_3ed64() {
    // Markers are drawn only in frame 1029 (the other frames have no
    // entity with flag 0x1000); the full comparison runs on all five.
    let changed = hud_void(
        0x8003ed64,
        &[559, 1029, 1801],
        hud::tree_3e828::targets_3ed64,
    );
    assert!(changed.iter().any(|&c| c > 0), "{changed:?}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
fn tree_3e828_reticle_4049c() {
    hud_void(0x8004049c, &[559, 1801], hud::tree_3e828::reticle_4049c);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-004"]
fn tree_3e828_box_3f28c() {
    let occurrence = at_frame(&late(), 0x8003f28c, 1029);
    let changed = check(&late(), 0x8003f28c, occurrence, |m, e| {
        hud::tree_3e828::box_3f28c(m, e.gpr(4), e.gpr(5));
        None
    });
    assert!(changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-004"]
fn tree_3e828_arrow_3fd24() {
    let occurrence = at_frame(&late(), 0x8003fd24, 1029);
    let changed = check(&late(), 0x8003fd24, occurrence, |m, e| {
        Some(hud::tree_3e828::arrow_3fd24(m, e.gpr(4)))
    });
    assert!(changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-004"]
fn tree_3e828_status_3ff34() {
    let occurrence = at_frame(&late(), 0x8003ff34, 1029);
    let changed = check(&late(), 0x8003ff34, occurrence, |m, e| {
        hud::tree_3e828::status_3ff34(m, e.gpr(4), e.gpr(5), e.gpr(6));
        None
    });
    assert!(changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-004"]
fn tree_3e828_label_40040() {
    let occurrence = at_frame(&late(), 0x80040040, 1029);
    let changed = check(&late(), 0x80040040, occurrence, |m, e| {
        hud::tree_3e828::label_40040(m, e.gpr(4), e.gpr(5), e.gpr(6));
        None
    });
    assert!(changed > 0);
}
/// Synthetic marker list for the unit tests: list at 0x80100000, block at
/// 0x80110000, all counts zero.
fn marker_machine() -> Machine {
    let mut m = Machine::default();
    m.sw(ac3_flight::GP + 0x434, 0x8010_0000);
    m.sw(ac3_flight::GP + 0x438, 0x8011_0000);
    m
}

/// 8004049C, both reticle shapes (no capture locks on): the diamond while
/// the timer byte is below 32, the crosshair afterwards.
#[test]
fn tree_3e828_reticle_4049c_shapes() {
    let gp = ac3_flight::GP;
    let mut m = marker_machine();
    m.sb(gp + 0x344, 1);
    m.sb(gp + 0x345, 3);
    m.sh(gp + 0x346, 16);
    m.sh(gp + 0x348, 100);
    m.sh(gp + 0x34a, 80);
    hud::tree_3e828::reticle_4049c(&mut m);
    // Diamond: reach = 16 + 2 = 18, two three-point lines at +0x408.
    assert_eq!(m.lw(0x8010_000c), 2);
    let line = 0x8011_0408;
    let points =
        |m: &mut Machine, at: u32| -> Vec<i16> { (0..6).map(|i| m.lh(at + 8 + 2 * i)).collect() };
    assert_eq!(m.lw(line + 4) & 0xff_ffff, 0xff);
    assert_eq!(points(&mut m, line), [100, 62, 118, 80, 100, 98]);
    assert_eq!(points(&mut m, line + 0x18), [100, 62, 82, 80, 100, 98]);
    // Crosshair: eight red lines at +8 (size 16, quarter 4).
    m.sb(gp + 0x345, 0x20);
    hud::tree_3e828::reticle_4049c(&mut m);
    assert_eq!(m.lw(0x8010_0008), 8);
    let expected: [[i16; 4]; 8] = [
        [100, 64, 100, 76],
        [100, 96, 100, 84],
        [84, 80, 96, 80],
        [116, 80, 104, 80],
        [96, 64, 104, 64],
        [96, 96, 104, 96],
        [84, 76, 84, 84],
        [116, 76, 116, 84],
    ];
    for (i, want) in expected.iter().enumerate() {
        let at = 0x8011_0008 + 0x10 * i as u32;
        let got: Vec<i16> = (0..4).map(|k| m.lh(at + 8 + 2 * k)).collect();
        assert_eq!(&got, want, "line {i}");
        assert_eq!(m.lw(at + 4) & 0xff_ffff, 0xff);
    }
    // Inactive: nothing is drawn.
    m.sb(gp + 0x344, 0);
    hud::tree_3e828::reticle_4049c(&mut m);
    assert_eq!(m.lw(0x8010_0008), 8);
    assert!(m.take_fault().is_ok());
}

/// 8003FF34 colors per status and the skipped status 0; 80040040 skips a
/// zero-width label.
#[test]
fn tree_3e828_status_and_label_paths() {
    let mut m = marker_machine();
    for status in 0..5u32 {
        hud::tree_3e828::status_3ff34(&mut m, status, 50, 60);
    }
    assert_eq!(m.lw(0x8010_0010), 4, "status 0 draws nothing");
    let colors: Vec<[u8; 3]> = (0..4)
        .map(|i| {
            let at = 0x8011_1008 + 0x14 * i;
            [m.lbu(at + 4), m.lbu(at + 5), m.lbu(at + 6)]
        })
        .collect();
    assert_eq!(colors[0], [0x83, 0, 0]);
    assert_eq!(colors[1], [0x83, 0x83, 0]);
    assert_eq!(colors[2], [0, 0x23, 0x83]);
    assert_eq!(colors[3], [0, 0, 0], "status 4 keeps the color bytes");
    // A label whose rectangle has width 0 is skipped.
    let label = (0..256u32)
        .find(|&l| m.lbu(0x800a_fae4 + l * 4 + 2) == 0)
        .unwrap();
    hud::tree_3e828::label_40040(&mut m, label, 50, 60);
    assert_eq!(m.lw(0x8010_0010), 4);
    assert!(m.take_fault().is_ok());
}

mod tree_3bdf8_a_tests {
    use super::*;

    /// The snapshot cases of a routine: hud-003 occurrences `0..early_count`
    /// (frames 307/309) and the listed hud-004 frames.
    fn t3a_cases(callee: u32, early_count: usize, late_frames: &[u64]) -> Vec<(PathBuf, usize)> {
        let mut cases: Vec<_> = (0..early_count).map(|n| (early(), n)).collect();
        for &frame in late_frames {
            cases.push((late(), at_frame(&late(), callee, frame)));
        }
        cases
    }

    /// Strict comparison on every case of [`t3a_cases`]; returns the bytes the
    /// original changed in each case.
    fn t3a_check(
        callee: u32,
        early_count: usize,
        late_frames: &[u64],
        native: impl Fn(&mut Machine, &Snapshot),
    ) -> Vec<usize> {
        t3a_cases(callee, early_count, late_frames)
            .into_iter()
            .map(|(index, occurrence)| {
                let changed = check(&index, callee, occurrence, |m, e| {
                    native(m, e);
                    None
                });
                eprintln!(
                    "{callee:08x} {} #{occurrence}: {changed} bytes",
                    index
                        .parent()
                        .and_then(Path::file_name)
                        .unwrap()
                        .to_string_lossy()
                );
                changed
            })
            .collect()
    }

    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003 and hud-004"]
    fn begin_frame_3be98() {
        let changed = t3a_check(0x8003be98, 2, &[559, 1801], |m, _| {
            hud::tree_3bdf8_a::begin_frame_3be98(m);
        });
        assert!(changed.iter().all(|&c| c > 0));
    }

    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003 and hud-004"]
    fn link_hud_3bed8() {
        let changed = t3a_check(0x8003bed8, 2, &[559, 1801], |m, _| {
            hud::tree_3bdf8_a::link_hud_3bed8(m);
        });
        assert!(changed.iter().all(|&c| c > 0));
    }

    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-004"]
    fn link_layer_3c3d8() {
        let changed = t3a_check(0x8003c3d8, 0, &[559], |m, _| {
            hud::tree_3bdf8_a::link_layer_3c3d8(m);
        });
        assert!(changed.iter().all(|&c| c > 0));
    }

    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003 and hud-004"]
    fn radar_basis_3c8d4() {
        let changed = t3a_check(0x8003c8d4, 2, &[559, 1801], |m, e| {
            hud::tree_3bdf8_a::radar_basis_3c8d4(m, e.gpr(4));
        });
        assert!(changed.iter().all(|&c| c > 0));
    }

    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003 and hud-004"]
    fn radar_distances_3c980() {
        // Every captured call reaches SquareRoot0 (entities 2..9 are on the radar).
        for (index, occurrence) in t3a_cases(0x8003c980, 2, &[559, 1801]) {
            check_blocked(&index, 0x8003c980, occurrence, &[0x800a47c8], |m, _| {
                hud::tree_3bdf8_a::radar_distances_3c980(m);
                None
            });
        }
    }

    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003 and hud-004"]
    fn radar_range_3cac8() {
        // 307/309: nearest-entity step (0 -> 0x8000, 0x8000 -> 0xC000); 559:
        // marked-entity scan (bit 0x8000, 0xC514 -> 0xC482); 1801: locked entity
        // step 0x8000, which keeps the range 0x8000 (the original stores the
        // same word, so that case changes no byte and checks the store value).
        let changed = t3a_check(0x8003cac8, 2, &[559, 1801], |m, _| {
            hud::tree_3bdf8_a::radar_range_3cac8(m);
        });
        assert!(changed[..3].iter().all(|&c| c > 0));
        assert_eq!(changed[3], 0);
    }

    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003 and hud-004"]
    fn frame_tiles_3cca0() {
        let changed = t3a_check(0x8003cca0, 2, &[559, 1801], |m, _| {
            hud::tree_3bdf8_a::frame_tiles_3cca0(m);
        });
        assert!(changed.iter().all(|&c| c > 0));
    }

    /// Synthetic HUD layout for the ordering-table tests: gameplay, ordering
    /// table slot, HUD block and primitive area.
    const T3A_GAMEPLAY: u32 = 0x8010_0000;
    const T3A_OT: u32 = 0x8010_1000;
    const T3A_HUD: u32 = 0x8010_2000;
    const T3A_AREA: u32 = 0x8011_0000;
    const T3A_END: u32 = 0x0012_3456;

    fn t3a_layout() -> Machine {
        let mut m = Machine::default();
        m.set_gw(0x2d0, T3A_GAMEPLAY);
        m.set_gw(0x424, T3A_HUD);
        m.set_gw(0x428, T3A_AREA);
        m.sw(T3A_GAMEPLAY + 0x10, T3A_OT);
        m.sw(T3A_OT, T3A_END);
        // Every packet carries length 3 so kept lengths are visible.
        for offset in (0..0x1080).step_by(4) {
            m.sw(T3A_AREA + offset, 0x0300_0000);
        }
        m
    }

    /// Walk the ordering-table chain from the slot: area offsets in drawing
    /// order, then the link found after the last packet of the area.
    fn t3a_walk(m: &mut Machine) -> (Vec<u32>, u32) {
        let mut order = Vec::new();
        let mut link = m.lw(T3A_OT) & hud::LINK;
        while (0x0011_0000..0x0011_1080).contains(&link) && order.len() < 0x400 {
            order.push(link - 0x0011_0000);
            link = m.lw(0x8000_0000 | link) & hud::LINK;
        }
        (order, link)
    }

    /// Expected drawing order from the insertion order (`(head, tail)` pairs;
    /// single packets have head == tail).
    fn t3a_expected(insertions: &[(u32, u32)]) -> Vec<u32> {
        let mut order = Vec::new();
        for &(head, tail) in insertions.iter().rev() {
            order.push(head);
            if tail != head {
                order.push(tail);
            }
        }
        order
    }

    fn t3a_runs(list: &mut Vec<(u32, u32)>, first: u32, stride: u32, count: u32) {
        list.extend((0..count).map(|i| (first + i * stride, first + i * stride)));
    }

    #[test]
    fn link_hud_3bed8_optional_groups() {
        for flags in [0, 1, 2, 3] {
            let mut m = t3a_layout();
            m.sw(T3A_HUD + 8, 1);
            m.sw(T3A_HUD + 0x10, 1);
            m.sw(T3A_HUD + 0x14, 2);
            m.sw(T3A_HUD + 0x18, flags);
            // The two pre-chained sets: head links to tail.
            m.sw(
                T3A_AREA + 0xb6c,
                0x0300_0000 | ((T3A_AREA + 0xbe4) & hud::LINK),
            );
            m.sw(
                T3A_AREA + 0xacc,
                0x0300_0000 | ((T3A_AREA + 0xb44) & hud::LINK),
            );
            hud::tree_3bdf8_a::link_hud_3bed8(&mut m);
            assert!(m.take_fault().is_ok());

            let mut list = Vec::new();
            if flags & 2 != 0 {
                t3a_runs(&mut list, 0xc74, 0x14, 3);
            }
            t3a_runs(&mut list, 0x918, 0x10, 1);
            t3a_runs(&mut list, 0xc0c, 0x14, 2);
            t3a_runs(&mut list, 0xc, 0, 1);
            t3a_runs(&mut list, 0xa88, 0, 1);
            if flags & 1 != 0 {
                t3a_runs(&mut list, 0xc54, 0x10, 2);
            }
            t3a_runs(&mut list, 0x998, 0x18, 2);
            t3a_runs(&mut list, 0x18, 0x14, 1);
            t3a_runs(&mut list, 0xa7c, 0, 1);
            list.push((0xb6c, 0xbe4));
            for offset in [0xc44, 0xa70, 0xa94, 0xaa4, 0xa64] {
                list.push((offset, offset));
            }
            list.push((0xacc, 0xb44));
            for offset in [0xc34, 0xa58, 0, 0xa9c] {
                list.push((offset, offset));
            }
            assert_eq!(
                t3a_walk(&mut m),
                (t3a_expected(&list), T3A_END),
                "flags {flags}"
            );
            // Mode packets take length 2 (the slot's length byte is 0), all
            // others keep theirs; the slot ends with length 0.
            for offset in [0xa88, 0xa7c, 0xa70, 0xa64, 0xa58] {
                assert_eq!(m.lw(T3A_AREA + offset) >> 24, 2, "{offset:#x}");
            }
            assert_eq!(m.lw(T3A_AREA + 0xa9c) >> 24, 3);
            assert_eq!(m.lw(T3A_OT) >> 24, 0);
        }
    }

    #[test]
    fn link_layer_3c3d8_optional_packet() {
        for flags in [0, 8] {
            let mut m = t3a_layout();
            m.sw(T3A_GAMEPLAY + 0xc, flags);
            m.sw(T3A_HUD + 0xc, 3);
            hud::tree_3bdf8_a::link_layer_3c3d8(&mut m);
            assert!(m.take_fault().is_ok());

            let mut list = Vec::new();
            if flags != 0 {
                t3a_runs(&mut list, 0xffc, 0, 1);
            }
            t3a_runs(&mut list, 0x1078, 0, 1);
            t3a_runs(&mut list, 0x518, 0x10, 3);
            t3a_runs(&mut list, 0x1070, 0, 1);
            t3a_runs(&mut list, 0xfcc, 0x18, 2);
            t3a_runs(&mut list, 0x1010, 0x14, 4);
            t3a_runs(&mut list, 0x1068, 0, 1);
            t3a_runs(&mut list, 0xf2c, 0x10, 10);
            t3a_runs(&mut list, 0xecc, 0x18, 4);
            t3a_runs(&mut list, 0xcd4, 0x28, 3);
            t3a_runs(&mut list, 0xcc8, 0, 1);
            t3a_runs(&mut list, 0xd4c, 0x28, 4);
            t3a_runs(&mut list, 0xcbc, 0, 1);
            t3a_runs(&mut list, 0xdec, 0x28, 4);
            t3a_runs(&mut list, 0xe8c, 0x10, 4);
            t3a_runs(&mut list, 0xcb0, 0, 1);
            assert_eq!(
                t3a_walk(&mut m),
                (t3a_expected(&list), T3A_END),
                "flags {flags}"
            );
            for offset in [0xcc8, 0xcbc, 0xcb0] {
                assert_eq!(m.lw(T3A_AREA + offset) >> 24, 2, "{offset:#x}");
            }
        }
    }

    /// Radar range test state: context bit 0x8000 (`marked`), locked entity,
    /// old range and (entity, flag word, distance) rows.
    fn t3a_radar_range(marked: bool, locked: u8, old: i32, rows: &[(u32, u32, u32)]) -> i32 {
        let mut m = Machine::default();
        m.set_gw(0x2dc, 0x8010_0000);
        m.sw(0x8010_0128, if marked { 0x8000 } else { 0 });
        m.sb(0x800c_4b36, locked);
        m.sw(0x800c_4b44, old as u32);
        for index in 0..0x40 {
            m.sw(0x800c_4828 + index * 0xc + 8, u32::MAX);
        }
        for &(index, flags, distance) in rows {
            m.sw(0x800c_4d98 + index * 4, flags);
            m.sw(0x800c_4828 + index * 0xc + 8, distance);
        }
        hud::tree_3bdf8_a::radar_range_3cac8(&mut m);
        assert!(m.take_fault().is_ok());
        m.lw(0x800c_4b44) as i32
    }

    #[test]
    fn radar_range_3cac8_branches() {
        // Marked mode, locked entity marked and on the radar: 1.25 x distance.
        assert_eq!(
            t3a_radar_range(true, 5, 0x2000, &[(5, 0x100, 0x3000)]),
            0x2e00
        );
        // Marked mode, locked entity not marked: nearest marked entity (5 is
        // nearer but unmarked, 4 is off the radar); odd mean rounds up.
        let rows = [(3, 0x100, 0x5000), (4, 0x100, u32::MAX), (5, 0, 0x1000)];
        assert_eq!(t3a_radar_range(true, 5, 0x63fe, &rows), 0x6400);
        // Marked mode, locked entity off the radar: scan as well.
        let rows = [(7, 0x100, u32::MAX), (9, 0x100, 0x4000)];
        assert_eq!(t3a_radar_range(true, 7, 0x5000, &rows), 0x5000);
        // Marked mode, nearest beyond 0x40000: clamp to 0x40000.
        assert_eq!(
            t3a_radar_range(true, 5, 0x40000, &[(5, 0x100, 0x80000)]),
            0x40000
        );
        // Step mode, locked entity off the radar: step 0x10000; odd mean below
        // the old range rounds down.
        assert_eq!(
            t3a_radar_range(false, 7, 0x10002, &[(2, 0, 0x100)]),
            0x10000
        );
        // Step mode, nearest of entities 2..31 (flags ignored, 32 not scanned)
        // below 0x6000: step 0x8000.
        let rows = [(2, 0, 0x5000), (0x20, 0, 0)];
        assert_eq!(t3a_radar_range(false, 0xff, 0x8000, &rows), 0x8000);
        // Step mode, nearest below 0x3000: step 0x4000.
        assert_eq!(
            t3a_radar_range(false, 0xff, 0x4000, &[(0x1f, 0, 0x2fff)]),
            0x4000
        );
        // Step mode, nothing on the radar: step 0x10000, mean clamped to 0x2000.
        assert_eq!(t3a_radar_range(false, 0xff, -0x20000, &[]), 0x2000);
        // Step mode, locked entity at 0x5fff (flags ignored): step 0x8000.
        assert_eq!(t3a_radar_range(false, 3, 0x8000, &[(3, 0, 0x5fff)]), 0x8000);
    }

    #[test]
    fn frame_tiles_3cca0_near_range() {
        let mut m = t3a_layout();
        m.sw(T3A_HUD + 0x10, 3);
        for (i, value) in [0x40u8, 0x82, 0xc4, 0x10, 0x20, 0x31]
            .into_iter()
            .enumerate()
        {
            m.sb(0x800c_4b30 + i as u32, value);
        }
        m.sw(0x800c_4b44, 0x4000);
        hud::tree_3bdf8_a::frame_tiles_3cca0(&mut m);
        assert!(m.take_fault().is_ok());
        assert_eq!(m.lw(T3A_HUD + 0x10), 5);
        let tile = T3A_AREA + 0x918 + 0x30;
        assert_eq!(m.read_bytes(tile + 4, 3), [0x20, 0x41, 0x62]);
        assert_eq!(
            [
                m.lh(tile + 8),
                m.lh(tile + 0xa),
                m.lh(tile + 0xc),
                m.lh(tile + 0xe)
            ],
            [0, 0x2b, 5, 0x2b]
        );
        let tile = tile + 0x10;
        assert_eq!(m.read_bytes(tile + 4, 3), [0x20, 0x41, 0x62]);
        assert_eq!(
            [
                m.lh(tile + 8),
                m.lh(tile + 0xa),
                m.lh(tile + 0xc),
                m.lh(tile + 0xe)
            ],
            [0x56, 0xc, 0x140, 0xc]
        );
        assert_eq!(m.read_bytes(T3A_AREA + 0xc0c + 4, 3), [0x10, 0x20, 0x31]);
        assert_eq!(m.read_bytes(T3A_AREA + 0xc20 + 4, 3), [0x08, 0x10, 0x18]);
        assert_eq!(m.lbu(T3A_AREA + 0xc20 + 0xc), 0x18);
    }

    #[test]
    fn radar_distances_3c980_off_radar() {
        // Entities without radar flag 0x2000 or with entity flag 0x40 get -1
        // without reaching SquareRoot0.
        let mut m = Machine::default();
        m.set_gw(0x2d8, 0x8010_0000);
        for index in 0..0x40 {
            let entity = 0x8010_1000 + index * 0x40;
            m.sw(0x8010_0000 + index * 4, entity);
            let even = index.is_multiple_of(2);
            m.sw(entity, if even { 0x40 } else { 0 });
            m.sw(0x800c_4d98 + index * 4, if even { 0x2000 } else { 0x1fff });
            m.sw(0x800c_4828 + index * 0xc + 8, 0x1234);
        }
        hud::tree_3bdf8_a::radar_distances_3c980(&mut m);
        assert!(m.take_fault().is_ok());
        for index in 0..0x40 {
            let expected = if index < 2 { 0x1234 } else { u32::MAX };
            assert_eq!(m.lw(0x800c_4828 + index * 0xc + 8), expected, "{index}");
        }
    }
}

use hud::tree_3bdf8_b as t3b;

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
fn tree_3bdf8_b_radar_map_3ce04() {
    let changed = hud_void(0x8003ce04, &[559, 1801], t3b::radar_map_3ce04);
    assert!(changed.iter().any(|&c| c > 0), "{changed:?}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
fn tree_3bdf8_b_radar_blips_3d20c() {
    let changed = hud_void(0x8003d20c, &[559, 1795, 1801], t3b::radar_blips_3d20c);
    assert!(changed.iter().any(|&c| c > 0), "{changed:?}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
fn tree_3bdf8_b_entity_blips_3d24c() {
    let changed = hud_void(0x8003d24c, &[559, 1801], t3b::entity_blips_3d24c);
    assert!(changed.iter().any(|&c| c > 0), "{changed:?}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
fn tree_3bdf8_b_ground_blips_3d534() {
    // The captured frames rewrite identical records, so the flag byte of
    // every ground record (always cleared first, then possibly set) is
    // poisoned before the run: matching the return state proves the stores.
    for (index, occurrence) in hud_cases(0x8003d534, &[1795, 1801]) {
        check(&index, 0x8003d534, occurrence, |m, _| {
            for record in 0..32 {
                m.sb(0x800c_46a8 + record * 12, 0xa5);
            }
            t3b::ground_blips_3d534(m);
            None
        });
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
fn tree_3bdf8_b_draw_blips_3d6a0() {
    let changed = hud_void(0x8003d6a0, &[559, 1801], t3b::draw_blips_3d6a0);
    assert!(changed.iter().any(|&c| c > 0), "{changed:?}");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
fn tree_3bdf8_b_blip_sprite_3d838() {
    for (index, occurrence) in hud_cases(0x8003d838, &[559, 1801]) {
        check(&index, 0x8003d838, occurrence, |m, e| {
            Some(t3b::blip_sprite_3d838(m, e.gpr(4)))
        });
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-004"]
fn tree_3bdf8_b_map_window_3dedc() {
    let index = late();
    let changed = check(
        &index,
        0x8003dedc,
        at_frame(&index, 0x8003dedc, 559),
        |m, _| {
            t3b::map_window_3dedc(m);
            None
        },
    );
    assert!(changed > 0);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-004"]
fn tree_3bdf8_b_map_position_3e44c() {
    let index = late();
    check(
        &index,
        0x8003e44c,
        at_frame(&index, 0x8003e44c, 559),
        |m, e| Some(t3b::map_position_3e44c(m, e.gpr(4), e.gpr(5), e.gpr(6))),
    );
}

mod tree_39d84_tests {
    use super::*;

    use ac3_flight::{Fault, GP};
    use hud::tree_39d84 as t39;

    /// Whether the script interpreter 80053704 (owned by the script module) is
    /// still a stub: it then faults; once translated it returns normally on an
    /// empty script.
    fn interpreter_is_stub() -> bool {
        let mut m = ac3_flight::Machine::default();
        ac3_flight::script::interpret(&mut m, 0x8010_0000);
        m.take_fault()
            == Err(Fault::Untranslated {
                address: 0x8005_3704,
            })
    }

    /// The interpreter was reached: its stub fault while it is a stub, a clean
    /// run once it is translated.
    fn script_called(fault: Result<(), Fault>) {
        if interpreter_is_stub() {
            assert_eq!(
                fault,
                Err(Fault::Untranslated {
                    address: 0x8005_3704
                })
            );
        } else {
            assert_eq!(fault, Ok(()));
        }
    }

    /// Snapshot occurrences of `callee`: the first `early_count` hud-003 entries
    /// (frames 307/309, or both calls of frame 307 for a routine called twice
    /// per frame) and the hud-004 entries of `late_frames`.
    fn occurrences_39d84(
        callee: u32,
        early_count: usize,
        late_frames: &[u64],
    ) -> Vec<(PathBuf, usize)> {
        let mut list: Vec<(PathBuf, usize)> = (0..early_count).map(|o| (early(), o)).collect();
        for &frame in late_frames {
            list.push((late(), at_frame(&late(), callee, frame)));
        }
        list
    }

    /// Byte of an entry snapshot's RAM at guest address `address`.
    fn ram_byte_39d84(e: &Snapshot, address: u32) -> u8 {
        e.ram[(address & 0x1f_ffff) as usize]
    }

    /// Word of an entry snapshot's RAM at guest address `address`.
    fn ram_word_39d84(e: &Snapshot, address: u32) -> u32 {
        let at = (address & 0x1f_ffff) as usize;
        u32::from_le_bytes(e.ram[at..at + 4].try_into().unwrap())
    }

    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
    fn screen_targets_3abac() {
        let mut total = 0;
        for (index, occurrence) in occurrences_39d84(0x8003abac, 2, &[559, 1801]) {
            // The original returns 0 (`clear v0`).
            total += check(&index, 0x8003abac, occurrence, |m, _| {
                t39::screen_targets_3abac(m);
                Some(0)
            });
        }
        assert!(total > 0);
    }

    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
    fn marker_size_3afb4() {
        for (index, occurrence) in occurrences_39d84(0x8003afb4, 2, &[559, 1801]) {
            let mut size = 0;
            check(&index, 0x8003afb4, occurrence, |_, e| {
                size = t39::marker_size_3afb4(e.gpr(4));
                Some(size)
            });
            assert!((2..=0x18).contains(&size));
        }
    }

    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
    fn lockon_3e780() {
        for (index, occurrence) in occurrences_39d84(0x8003e780, 2, &[559, 1801]) {
            check_blocked(
                &index,
                0x8003e780,
                occurrence,
                &[0x8003a0f8, 0x8003a08c],
                |m, _| {
                    t39::lockon_3e780(m);
                    None
                },
            );
        }
    }

    /// In every captured frame the lock-on is idle and the original rewrites
    /// bytes with the values they already hold. To make the comparison show
    /// that the translation performs those writes, the entry state is dirtied
    /// at bytes 8003EB14 stores before it reads them (it never reads them): the
    /// locked-slot byte `LOCK_BLOCK + 0x12` and byte 0 of every screen record.
    /// The original's return state must then be reproduced exactly.
    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
    fn lock_states_3eb14() {
        for (index, occurrence) in occurrences_39d84(0x8003eb14, 2, &[559, 1801]) {
            let mut dirtied = 0;
            check(&index, 0x8003eb14, occurrence, |m, _| {
                m.sb(t39::LOCK_BLOCK + 0x12, 0x5a);
                dirtied += 1;
                for slot in 2..64 {
                    let record = m.lw(t39::SCREEN_RECORDS + slot * 4);
                    if record != 0 {
                        m.sb(record, 0x5a);
                        dirtied += 1;
                    }
                }
                t39::lock_states_3eb14(m);
                None
            });
            assert!(dirtied > 1);
        }
    }

    /// The captured frames take the idle path (cursor state 0: no stores);
    /// the active paths are covered by `lock_cursor_4016c_synthetic`.
    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
    fn lock_cursor_4016c() {
        for (index, occurrence) in occurrences_39d84(0x8004016c, 2, &[559, 1801]) {
            check(&index, 0x8004016c, occurrence, |m, e| {
                assert_eq!(ram_byte_39d84(e, t39::CURSOR), 0, "idle cursor");
                t39::lock_cursor_4016c(m);
                None
            });
        }
    }

    /// The captured frames have the panel closed (state 0); the original
    /// returns 0. The animation is covered by `panel_40ce8_synthetic`.
    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
    fn panel_40ce8() {
        for (index, occurrence) in occurrences_39d84(0x80040ce8, 2, &[559, 1801]) {
            check(&index, 0x80040ce8, occurrence, |m, e| {
                assert_eq!(ram_byte_39d84(e, t39::PANEL), 0, "closed panel");
                t39::panel_40ce8(m);
                Some(0)
            });
        }
    }

    /// The captured frames have no message animation (state 0); the original
    /// returns 0. The animation is covered by `message_41010_synthetic`.
    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
    fn message_41010() {
        for (index, occurrence) in occurrences_39d84(0x80041010, 2, &[559, 1801]) {
            check(&index, 0x80041010, occurrence, |m, e| {
                assert_eq!(ram_byte_39d84(e, t39::MESSAGE_BLOCK + 2), 0, "idle message");
                t39::message_41010(m);
                Some(0)
            });
        }
    }

    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-004"]
    fn reset_41a48() {
        let occurrence = at_frame(&late(), 0x80041a48, 1923);
        let changed = check(&late(), 0x80041a48, occurrence, |m, _| {
            t39::reset_41a48(m);
            None
        });
        assert!(changed > 0);
    }

    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-004"]
    fn flag_55038() {
        let occurrence = at_frame(&late(), 0x80055038, 2487);
        check(&late(), 0x80055038, occurrence, |m, e| {
            let value = t39::flag_55038(m);
            assert_eq!(value, ram_word_39d84(e, GP + 0x4bc));
            Some(value)
        });
    }

    /// Frame 2487 fades at level 0 (tile only, no quads); the quad path is
    /// covered by `fade_8e42c_synthetic`.
    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-004"]
    fn fade_8e42c() {
        let occurrence = at_frame(&late(), 0x8008e42c, 2487);
        check_blocked(&late(), 0x8008e42c, occurrence, &[0x800a45c8], |m, e| {
            Some(t39::fade_8e42c(m, e.gpr(4), e.gpr(5), e.gpr(6)))
        });
    }

    #[test]
    fn lock_start_4011c() {
        let mut m = Machine::default();
        let cursor = t39::CURSOR;
        assert_eq!(t39::lock_start_4011c(&mut m, 7), 1);
        assert_eq!(m.lbu(cursor), 1);
        assert_eq!(m.lbu(cursor + 1), 7);
        assert_eq!(m.lhu(cursor + 2), 2);
        assert_eq!(m.lhu(cursor + 4), 0xa0);
        assert_eq!(m.lhu(cursor + 6), 0x80);
        // Already running: nothing changes.
        assert_eq!(t39::lock_start_4011c(&mut m, 9), 0);
        assert_eq!(m.lbu(cursor + 1), 7);
        assert!(m.take_fault().is_ok());
    }

    #[test]
    fn lock_clear_40160() {
        let mut m = Machine::default();
        m.sb(t39::CURSOR, 2);
        m.sb(t39::CURSOR + 1, 5);
        assert_eq!(t39::lock_clear_40160(&mut m), 0xff);
        assert_eq!(m.lbu(t39::CURSOR), 0);
        assert_eq!(m.lbu(t39::CURSOR + 1), 5);
        assert!(m.take_fault().is_ok());
    }

    #[test]
    fn marker_size_3afb4_synthetic() {
        assert_eq!(t39::marker_size_3afb4(0x1001), 2);
        assert_eq!(t39::marker_size_3afb4(0x7fff_ffff), 2);
        assert_eq!(t39::marker_size_3afb4(0x1000), 2, "clamped up");
        assert_eq!(t39::marker_size_3afb4(0), 0x18);
        // 3 * 0xF00^2 >> 21 = 21.
        assert_eq!(t39::marker_size_3afb4(0x100), 21);
        assert_eq!(t39::marker_size_3afb4(0xffff_ff00), 0x18, "clamped down");
    }

    /// Synthetic flight state for the lock-on and marker tests: entity table,
    /// gameplay, context and HUD mode blocks, and slot records.
    struct Slots39d84;
    impl Slots39d84 {
        const ENTITIES: u32 = 0x8010_0000;
        const ENTITY: u32 = 0x8011_0000;
        const SCREEN: u32 = 0x8012_0000;
        const TARGET: u32 = 0x8013_0000;
        const HUD_MODE: u32 = 0x8014_0000;
        const GAMEPLAY: u32 = 0x8015_0000;
        const CONTEXT: u32 = 0x8016_0000;
        const CAMERA: u32 = 0x8017_0000;

        fn machine() -> Machine {
            let mut m = Machine::default();
            m.sw(GP + 0x2d8, Self::ENTITIES);
            m.sw(GP + 0x2d0, Self::GAMEPLAY);
            m.sw(GP + 0x2dc, Self::CONTEXT);
            m.sw(GP + 0x330, Self::HUD_MODE);
            m.sw(Self::GAMEPLAY + 0x28, Self::CAMERA);
            m.sb(t39::LOCK_BLOCK + 0xe, 0xff);
            m
        }
        fn entity(slot: u32) -> u32 {
            Self::ENTITY + slot * 0x100
        }
        fn screen(slot: u32) -> u32 {
            Self::SCREEN + slot * 0x10
        }
        fn target(slot: u32) -> u32 {
            Self::TARGET + slot * 0x40
        }
        /// A live, lockable slot that is on screen.
        fn add(m: &mut Machine, slot: u32) {
            m.sw(Self::ENTITIES + slot * 4, Self::entity(slot));
            m.sw(Self::entity(slot), 0x4000);
            m.sw(t39::SCREEN_RECORDS + slot * 4, Self::screen(slot));
            m.sw(t39::TARGET_RECORDS + slot * 4, Self::target(slot));
            m.sw(t39::EVENT_FLAGS + slot * 4, 0x1000 | 0x4000 | 1);
            m.sb(Self::screen(slot) + 2, 1);
            m.sw(Self::target(slot), 1 | 8);
            m.sw(Self::target(slot) + 0x20, 0x2_0000);
        }
    }

    #[test]
    fn lock_states_3eb14_synthetic() {
        let mut m = Slots39d84::machine();
        for slot in [5, 6, 7] {
            Slots39d84::add(&mut m, slot);
        }
        // Slot 7 is not a live entity.
        m.sw(Slots39d84::entity(7), 0);
        m.sb(Slots39d84::screen(7), 0x33);
        m.sb(t39::LOCK_BLOCK + 0xe, 5);

        // Slot 5 is selected and starts the cursor; slot 6 is a visible target.
        t39::lock_states_3eb14(&mut m);
        assert!(m.take_fault().is_ok());
        let [s5, s6, s7] = [5, 6, 7].map(Slots39d84::screen);
        assert_eq!([m.lbu(s5), m.lbu(s5 + 1)], [2, 1]);
        assert_eq!([m.lbu(s6), m.lbu(s6 + 1)], [1, 0]);
        assert_eq!(m.lbu(s7), 0);
        assert_eq!([m.lbu(t39::CURSOR), m.lbu(t39::CURSOR + 1)], [1, 5]);
        assert_eq!(m.lbu(t39::LOCK_BLOCK + 0x12), 0xff);

        // Once the cursor has locked (state 2), slot 5 becomes the locked slot.
        m.sb(t39::CURSOR, 2);
        t39::lock_states_3eb14(&mut m);
        assert_eq!(m.lbu(s5), 3);
        assert_eq!(m.lbu(t39::LOCK_BLOCK + 0x12), 5);

        // Suppressed (weapon mode): selected but no lock.
        m.sb(t39::LOCK_BLOCK + 0x10, 1);
        t39::lock_states_3eb14(&mut m);
        assert_eq!(m.lbu(s5), 2);
        assert_eq!(m.lbu(t39::LOCK_BLOCK + 0x12), 0xff);
        m.sb(t39::LOCK_BLOCK + 0x10, 0);

        // The selection moves to slot 6: slot 5 stops the cursor (80040160
        // returns 0xFF into its owner byte) and slot 6 restarts it in the same
        // pass.
        m.sb(t39::LOCK_BLOCK + 0xe, 6);
        t39::lock_states_3eb14(&mut m);
        assert_eq!([m.lbu(s5), m.lbu(s5 + 1)], [1, 0xff]);
        assert_eq!([m.lbu(s6), m.lbu(s6 + 1)], [2, 1]);
        assert_eq!([m.lbu(t39::CURSOR), m.lbu(t39::CURSOR + 1)], [1, 6]);

        // A cursor owner whose cursor stopped (state 0) drops ownership.
        m.sb(t39::CURSOR, 0);
        t39::lock_states_3eb14(&mut m);
        assert_eq!([m.lbu(s6), m.lbu(s6 + 1)], [2, 0]);
        assert!(m.take_fault().is_ok());
    }

    #[test]
    fn lock_cursor_4016c_synthetic() {
        let mut m = Slots39d84::machine();
        Slots39d84::add(&mut m, 5);
        let record = Slots39d84::screen(5);
        m.sb(record, 2);
        m.sh(record + 6, 8);
        m.sh(record + 8, 0xb0);
        m.sh(record + 0xa, 0x70);
        m.sw(Slots39d84::GAMEPLAY + 0xc, 1);
        assert_eq!(t39::lock_start_4011c(&mut m, 5), 1);
        let cursor = t39::CURSOR;
        let position = |m: &mut Machine| [m.lhu(cursor + 4), m.lhu(cursor + 6), m.lhu(cursor + 2)];

        // Moving: x/y by at most the size (2, then 6), size by at most 4.
        t39::lock_cursor_4016c(&mut m);
        assert_eq!(position(&mut m), [0xa2, 0x7e, 6]);
        t39::lock_cursor_4016c(&mut m);
        assert_eq!(position(&mut m), [0xa8, 0x78, 8]);
        assert_eq!(m.lbu(cursor), 1);
        // Arrival in this call (steps of exactly the size): locked.
        t39::lock_cursor_4016c(&mut m);
        assert_eq!(position(&mut m), [0xb0, 0x70, 8]);
        assert_eq!(m.lbu(cursor), 2);
        // Locked: follows the marker exactly.
        m.sh(record + 8, 0x20);
        m.sh(record + 0xa, 0xf0);
        m.sh(record + 6, 0x14);
        t39::lock_cursor_4016c(&mut m);
        assert_eq!(position(&mut m), [0x20, 0xf0, 0x14]);
        assert!(m.take_fault().is_ok());

        // Shrinking: steps of -4, then the last -4 exactly.
        m.sb(cursor, 1);
        m.sh(record + 6, 8);
        for (size, state) in [(0x10, 1), (0xc, 1), (8, 2)] {
            t39::lock_cursor_4016c(&mut m);
            assert_eq!((m.lhu(cursor + 2), m.lbu(cursor)), (size, state));
        }

        // The target leaves the lockable state: the cursor stops.
        m.sb(record, 0);
        t39::lock_cursor_4016c(&mut m);
        assert_eq!(m.lbu(cursor), 0);
        m.sb(record, 1);
        m.sb(cursor, 1);
        m.sw(t39::EVENT_FLAGS + 5 * 4, 0x1000);
        t39::lock_cursor_4016c(&mut m);
        assert_eq!(m.lbu(cursor), 0);
        m.sw(t39::EVENT_FLAGS + 5 * 4, 1);
        m.sb(cursor, 1);
        m.sw(Slots39d84::target(5), 1);
        t39::lock_cursor_4016c(&mut m);
        assert_eq!(m.lbu(cursor), 0);
        assert!(m.take_fault().is_ok());

        // Every 8th frame a running cursor plays sound 0x2D (8002D7B8, pending).
        m.sw(Slots39d84::target(5), 8);
        m.sb(cursor, 1);
        m.sw(Slots39d84::GAMEPLAY + 0xc, 8);
        t39::lock_cursor_4016c(&mut m);
        // Now translated (claimed in dispatch), so the call completes.
        assert!(m.take_fault().is_ok());

        // Arrival in mission mode 4 at an entity with bit 0x100 calls 8004B760.
        m.sw(Slots39d84::GAMEPLAY + 0xc, 1);
        m.sb(t39::MISSION_MODE, 4);
        m.sw(Slots39d84::entity(5), 0x4100);
        m.sb(cursor, 1);
        for (offset, value) in [(2, 6), (4, 0x40), (6, 0x40)] {
            m.sh(cursor + offset, value);
            m.sh(record + 4 + offset, value);
        }
        t39::lock_cursor_4016c(&mut m);
        assert_eq!(m.lbu(cursor), 2);
        // Now translated (claimed in dispatch), so the call completes.
        assert!(m.take_fault().is_ok());
    }

    #[test]
    fn panel_40ce8_synthetic() {
        let mut m = Slots39d84::machine();
        let (state, extent) = (t39::PANEL, t39::PANEL + 2);
        m.sb(state, 1);
        m.sh(extent, 0x40);
        m.sw(GP + 0x44c, 0x1234);
        for expected in (0x50..0xc8).step_by(0x10) {
            t39::panel_40ce8(&mut m);
            assert_eq!((m.lbu(state), m.lhu(extent)), (1, expected));
        }
        t39::panel_40ce8(&mut m);
        assert_eq!(
            (m.lbu(state), m.lhu(extent), m.lw(GP + 0x44c)),
            (2, 0xc8, 0)
        );
        t39::panel_40ce8(&mut m);
        assert_eq!((m.lbu(state), m.lhu(extent)), (2, 0xc8));
        // Closing from 0xC0: 0xA0, ..., 0 (closed).
        m.sb(state, 3);
        m.sh(extent, 0xc0);
        for expected in [0xa0, 0x80, 0x60, 0x40, 0x20] {
            t39::panel_40ce8(&mut m);
            assert_eq!((m.lbu(state), m.lhu(extent)), (3, expected));
        }
        t39::panel_40ce8(&mut m);
        assert_eq!((m.lbu(state), m.lhu(extent)), (0, 0));
        // Paused context: nothing moves.
        m.sb(state, 1);
        m.sw(Slots39d84::CONTEXT, 0x2000_0000);
        t39::panel_40ce8(&mut m);
        assert_eq!((m.lbu(state), m.lhu(extent)), (1, 0));
        assert!(m.take_fault().is_ok());
        // Opening from 0 plays sound 0x3E, closing from 200 sound 0x36.
        m.sw(Slots39d84::CONTEXT, 0);
        t39::panel_40ce8(&mut m);
        assert_eq!(m.lhu(extent), 0x10);
        // Now translated (claimed in dispatch), so the call completes.
        assert!(m.take_fault().is_ok());
        m.sb(state, 3);
        m.sh(extent, 0xc8);
        t39::panel_40ce8(&mut m);
        assert_eq!(m.lhu(extent), 0xa8);
        // Now translated (claimed in dispatch), so the call completes.
        assert!(m.take_fault().is_ok());
    }

    #[test]
    fn message_41010_synthetic() {
        let mut m = Slots39d84::machine();
        let block = t39::MESSAGE_BLOCK;
        m.sb(block + 2, 1);
        for (counter, glyphs, state) in [
            (0, [0x2b, 6], 1),
            (1, [0xc, 0x29], 1),
            (2, [0x15, 0x14], 1),
            (3, [0x2b, 0x29], 2),
        ] {
            assert_eq!(m.lhu(block), counter);
            t39::message_41010(&mut m);
            assert_eq!([m.lbu(block + 6), m.lbu(block + 7)], glyphs);
            assert_eq!(m.lbu(block + 2), state);
            assert_eq!(m.lhu(block), counter + 1);
        }
        // State 1 past frame 3 only counts.
        m.sb(block + 2, 1);
        t39::message_41010(&mut m);
        assert_eq!(
            (m.lhu(block), m.lbu(block + 6), m.lbu(block + 7)),
            (5, 0x2b, 0x29)
        );
        // State 3 fades byte +7 by >> 2 and ends at 0.
        m.sb(block + 2, 3);
        m.sb(block + 6, 0);
        for (value, state) in [(0xa, 3), (2, 3), (0, 0)] {
            t39::message_41010(&mut m);
            assert_eq!(
                (m.lbu(block + 6), m.lbu(block + 7), m.lbu(block + 2)),
                (0x2b, value, state)
            );
        }
        // Paused context: nothing moves.
        m.sb(block + 2, 1);
        m.sw(Slots39d84::CONTEXT, 0x2000_0000);
        t39::message_41010(&mut m);
        assert_eq!(m.lhu(block), 5);
        assert!(m.take_fault().is_ok());
    }

    #[test]
    fn reset_41a48_synthetic() {
        let mut m = Machine::default();
        for offset in (0..0x10).step_by(4) {
            m.sw(t39::MESSAGE_SLOT + offset, 0x5a5a_5a5a);
        }
        t39::reset_41a48(&mut m);
        assert_eq!(m.lw(t39::MESSAGE_SLOT), 0xffff_ffff);
        assert_eq!(m.lw(t39::MESSAGE_SLOT + 4), 0);
        assert_eq!(m.lw(t39::MESSAGE_SLOT + 8), 0x5a5a_5a00);
        assert_eq!(m.lw(t39::MESSAGE_SLOT + 0xc), 0x5a5a_5a00);
    }

    /// Pass 1 projects slot 5 to the screen centre; pass 2 counts its 90th
    /// frame on screen and runs its first sighting script (80053704, a stub).
    #[test]
    fn screen_targets_3abac_synthetic() {
        let mut m = Slots39d84::machine();
        // Identity rotation at gameplay+0x4C, H = 0x100, offset (160, 128).
        for (offset, word) in [(0x4c, 0x1000), (0x54, 0x1000), (0x5c, 0x1000)] {
            m.sw(Slots39d84::GAMEPLAY + offset, word);
        }
        m.gte.ctc2(ac3_gte::ctrl::H, 0x100);
        m.gte.ctc2(ac3_gte::ctrl::OFX, 160 << 16);
        m.gte.ctc2(ac3_gte::ctrl::OFY, 128 << 16);
        m.sw(Slots39d84::GAMEPLAY + 4, 4);
        Slots39d84::add(&mut m, 5);
        let entity = Slots39d84::entity(5);
        m.sw(Slots39d84::CAMERA + 0xc, 0x10_0000);
        m.sw(entity + 0x10, 0x50_0000);
        let target = Slots39d84::target(5);
        m.sw(target + 0x24, 0x58);
        let scripts = 0x8018_0000;
        m.sw(GP + 0x374, scripts);
        let sp = m.sp;

        t39::screen_targets_3abac(&mut m);
        assert!(m.take_fault().is_ok());
        let screen = Slots39d84::screen(5);
        assert_eq!(m.lw(target + 0x20), 0x7_0000);
        assert_eq!((m.lhu(screen + 8), m.lhu(screen + 0xa)), (160, 128));
        assert_eq!(m.lhu(screen + 6), 21, "depth 0x400 / 4");
        assert_eq!(m.lw(target + 0x24), 0x59);
        assert_eq!(m.sp, sp);

        // The 90th frame: the event bit is set and the script is interpreted.
        m.sw(scripts + 5 * 0x40 + 0x38, 0x8019_0000);
        t39::screen_targets_3abac(&mut m);
        assert_eq!(m.lw(target + 0x24), 0x5a);
        assert_eq!(m.lw(t39::EVENT_FLAGS + 5 * 4) & 0x300_0000, 0x100_0000);
        script_called(m.take_fault());
        // Only once; the second script needs [gameplay+8] > 0.
        t39::screen_targets_3abac(&mut m);
        assert!(m.take_fault().is_ok());
        m.sw(Slots39d84::GAMEPLAY + 8, 1);
        m.sw(scripts + 5 * 0x40 + 0x3c, 0x8019_0000);
        t39::screen_targets_3abac(&mut m);
        assert_eq!(m.lw(t39::EVENT_FLAGS + 5 * 4) & 0x300_0000, 0x300_0000);
        script_called(m.take_fault());
        // Paused context: the counter holds.
        m.sw(Slots39d84::CONTEXT, 0x2000_0000);
        t39::screen_targets_3abac(&mut m);
        assert_eq!(m.lw(target + 0x24), 0x5c);
        // Off screen (x = 352): flags drop to 0x50000 and the counter resets.
        m.sw(entity + 8, 0x30_0000);
        t39::screen_targets_3abac(&mut m);
        assert_eq!(m.lw(target + 0x20), 0x5_0000);
        assert_eq!(m.lw(target + 0x24), 0);
        // Out of range (relative x beyond a halfword): only the flags are reset.
        m.sw(entity + 8, 0x800_0000);
        t39::screen_targets_3abac(&mut m);
        assert_eq!(m.lw(target + 0x20), 0);
        assert!(m.take_fault().is_ok());
    }

    /// Slots 32..63 take their scripts from the 0x28-byte table at
    /// `[gp+0x378]`: only the right word triggers the interpreter.
    #[test]
    fn screen_targets_3abac_high_slots() {
        for (script, faults) in [(0, false), (0x8019_0000, true)] {
            let mut m = Slots39d84::machine();
            for (offset, word) in [(0x4c, 0x1000), (0x54, 0x1000), (0x5c, 0x1000)] {
                m.sw(Slots39d84::GAMEPLAY + offset, word);
            }
            m.gte.ctc2(ac3_gte::ctrl::H, 0x100);
            m.gte.ctc2(ac3_gte::ctrl::OFX, 160 << 16);
            m.gte.ctc2(ac3_gte::ctrl::OFY, 128 << 16);
            m.sw(Slots39d84::GAMEPLAY + 4, 4);
            m.sb(t39::MISSION_MODE, 6);
            Slots39d84::add(&mut m, 40);
            m.sw(Slots39d84::entity(40) + 0x10, 0x40_0000);
            let scripts = 0x8018_0000;
            m.sw(GP + 0x378, scripts);
            m.sw(scripts + 8 * 0x28 + 0x20, script);
            t39::screen_targets_3abac(&mut m);
            assert_eq!(m.lw(t39::EVENT_FLAGS + 40 * 4) & 0x100_0000, 0x100_0000);
            let fault = m.take_fault();
            assert_eq!(fault.is_err(), faults && interpreter_is_stub(), "{fault:?}");
        }
    }

    /// Partial level: DR_MODE, faded TILE, four textured quads and the closing
    /// DR_MODE are linked between the slot and its previous first packet.
    /// `rsin` (800A45C8) is still pending, so the colors are not checked.
    #[test]
    fn fade_8e42c_synthetic() {
        let mut m = Slots39d84::machine();
        let (ot, prim) = (0x8017_8000, 0x8018_0000);
        m.sw(ot, 0x00ab_cdef);
        assert_eq!(t39::fade_8e42c(&mut m, ot, prim, 0xffff_ffff), prim);
        assert!(m.take_fault().is_ok());
        assert_eq!(m.lw(ot), 0x00ab_cdef);

        let next = t39::fade_8e42c(&mut m, ot, prim, 0x400);
        // Now translated (claimed in dispatch), so the call completes.
        assert!(m.take_fault().is_ok());
        assert_eq!(next, prim + 0xc8);
        assert_eq!(m.lw(ot), prim & 0xff_ffff);
        assert_eq!(m.lw(prim), 0x0200_0000 | ((prim + 0xc) & 0xff_ffff));
        assert_eq!(m.lw(prim + 4), 0xe600_0001);
        assert_eq!(m.lw(prim + 8), 0xe100_0340);
        assert_eq!(m.lw(prim + 0xc), 0x0300_0000 | ((prim + 0x1c) & 0xff_ffff));
        assert_eq!(m.lw(prim + 0x18), 0x0100_0140);
        for i in 0..4 {
            let quad = prim + 0x1c + i * 0x28;
            assert_eq!(m.lw(quad), 0x0900_0000 | ((quad + 0x28) & 0xff_ffff));
            let (left, top) = ((i >> 1) * 0xa0, (i & 1) * 0x80);
            let corners: Vec<u32> = [8, 0x10, 0x18, 0x20]
                .iter()
                .map(|&o| m.lw(quad + o))
                .collect();
            assert_eq!(
                corners,
                [
                    left | top << 16,
                    (left + 0xa0) | top << 16,
                    left | (top + 0x80) << 16,
                    (left + 0xa0) | (top + 0x80) << 16
                ]
            );
            // Context +4 is 0: texture page 0x130 (+2 for the right half).
            assert_eq!(u32::from(m.lhu(quad + 0x16)), 0x130 | (i & 2));
        }
        assert_eq!(m.lhu(prim + 0x1c + 0xc), 0x203);
        assert_eq!(m.lhu(prim + 0x1c + 0x9c), 0xfebd);
        let tail = prim + 0xbc;
        assert_eq!(m.lw(tail), 0x0200_0000 | 0x00ab_cdef);
        assert_eq!(m.lw(tail + 4), 0xe600_0000);
        assert_eq!(m.lw(tail + 8), 0xe100_0220);

        // Full level: opaque black tile, no quads.
        let next = t39::fade_8e42c(&mut m, ot, prim, 0x2000);
        // Now translated (claimed in dispatch), so the call completes.
        assert!(m.take_fault().is_ok());
        assert_eq!(next, prim + 0x28);
        assert_eq!(m.lw(prim + 0x10), 0x6000_0000);
        assert_eq!(m.lw(prim + 0x1c), 0x0200_0000 | (prim & 0xff_ffff));
    }
}

mod tree_40a24_tests {
    use super::*;

    use ac3_flight::{GP, hud::tree_40a24};

    /// [`check`] on each of `occurrences`; returns the bytes the original
    /// changed, summed.
    fn check_each(
        index: &Path,
        callee: u32,
        occurrences: &[usize],
        native: impl Fn(&mut Machine, &Snapshot) -> Option<u32>,
    ) -> usize {
        occurrences
            .iter()
            .map(|&occurrence| {
                let changed = check(index, callee, occurrence, &native);
                eprintln!("{callee:08x} #{occurrence}: {changed} bytes reproduced");
                changed
            })
            .sum()
    }

    /// Occurrences of `callee` in the hud-004 `frames`.
    fn late_frames(callee: u32, frames: &[u64]) -> Vec<usize> {
        frames
            .iter()
            .map(|&frame| at_frame(&late(), callee, frame))
            .collect()
    }

    /// Byte of the entry snapshot's RAM at `address`.
    fn entry_byte(e: &Snapshot, address: u32) -> u8 {
        e.ram[(address & 0x1f_ffff) as usize]
    }

    /// Overwrite `length` bytes at `address` with 0xA5 before a translation runs,
    /// so bytes the original rewrites with unchanged values are still compared.
    fn poison(m: &mut Machine, address: u32, length: u32) {
        for i in 0..length {
            m.sb(address + i, 0xa5);
        }
    }

    /// Poisons the bytes 80039BF8 writes on every path (+8..+0xD, +0xF, +0x11).
    fn run_hud_color(m: &mut Machine, e: &Snapshot) -> Option<u32> {
        poison(m, 0x800c_4b30, 6);
        poison(m, 0x800c_4b37, 1);
        poison(m, 0x800c_4b39, 1);
        tree_40a24::hud_color_39bf8(m, e.gpr(4));
        None
    }

    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003"]
    fn hud_color_39bf8() {
        assert!(check_each(&early(), 0x8003_9bf8, &[0, 1], run_hud_color) > 0);
    }

    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-004"]
    fn hud_color_39bf8_late() {
        let occurrences = late_frames(0x8003_9bf8, &[559, 1801]);
        assert!(check_each(&late(), 0x8003_9bf8, &occurrences, run_hud_color) > 0);
    }

    /// Every captured 8003A158 call is in mission mode 1 (800BAEB4 != 3): the
    /// routine returns without writing, which the comparison confirms. The draw
    /// path is covered by [`counter_3a158_draw_path`].
    fn run_counter(m: &mut Machine, e: &Snapshot) -> Option<u32> {
        assert_ne!(entry_byte(e, 0x800b_aeb4), 3, "draw path captured");
        tree_40a24::counter_3a158(m);
        None
    }

    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003"]
    fn counter_3a158() {
        check_each(&early(), 0x8003_a158, &[0, 1], run_counter);
    }

    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-004"]
    fn counter_3a158_late() {
        let occurrences = late_frames(0x8003_a158, &[559, 1801]);
        check_each(&late(), 0x8003_a158, &occurrences, run_counter);
    }

    /// Mission mode 3 draw path through the text routine 80035144: the
    /// label call, then the digits of 42 right to left (the last string is "4"),
    /// with the color word and string pointer in the original frame.
    #[test]
    fn counter_3a158_draw_path() {
        let mut m = Machine::default();
        let mission = 0x8001_0000;
        let game = 0x8001_1000;
        m.sb(0x800b_aeb4, 3);
        m.set_gw(0x330, mission);
        m.sw(mission + 4, 42);
        m.set_gw(0x2d0, game);
        m.sw(game + 0x10, 0x8001_2000);
        m.sw(game + 0x14, 0x8001_3000);
        for (offset, value) in [(0xb, 0x83), (0xc, 0x42), (0xd, 0x10)] {
            m.sb(0x800c_4b28 + offset, value);
        }
        let sp = m.sp;
        tree_40a24::counter_3a158(&mut m);
        // 80035144 is translated by `loading` (called by address).
        assert_eq!(m.take_fault(), Ok(()));
        let frame = sp - 0x38;
        assert_eq!(m.sp, sp);
        assert_eq!(m.lw(frame + 0x10), 0x0008_2141);
        assert_eq!(m.lw(frame + 0x14), frame + 0x18);
        assert_eq!(m.lbu(frame + 0x18), b'4');
        assert_eq!(m.lbu(frame + 0x19), 0);
    }

    fn run_overlay(m: &mut Machine, e: &Snapshot) -> Option<u32> {
        tree_40a24::overlay_40a24(m, e.gpr(4));
        None
    }

    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003"]
    fn overlay_40a24() {
        assert!(check_each(&early(), 0x8004_0a24, &[0, 1], run_overlay) > 0);
    }

    /// Frame 1795 reaches 8002D724 (a warning label's sound) through 80041A68.
    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-004"]
    fn overlay_40a24_late() {
        let occurrences = late_frames(0x8004_0a24, &[431, 559, 1801]);
        assert!(check_each(&late(), 0x8004_0a24, &occurrences, run_overlay) > 0);
        let blocked = at_frame(&late(), 0x8004_0a24, 1795);
        check_blocked(&late(), 0x8004_0a24, blocked, &[0x8002_d724], run_overlay);
    }

    fn run_select_buffer(m: &mut Machine, _: &Snapshot) -> Option<u32> {
        tree_40a24::select_buffer_40acc(m);
        None
    }

    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003"]
    fn select_buffer_40acc() {
        assert!(check_each(&early(), 0x8004_0acc, &[0, 1], run_select_buffer) > 0);
    }

    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-004"]
    fn select_buffer_40acc_late() {
        let occurrences = late_frames(0x8004_0acc, &[559, 1801]);
        assert!(check_each(&late(), 0x8004_0acc, &occurrences, run_select_buffer) > 0);
    }

    fn run_link_frame(m: &mut Machine, _: &Snapshot) -> Option<u32> {
        tree_40a24::link_frame_40b00(m);
        None
    }

    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003"]
    fn link_frame_40b00() {
        assert!(check_each(&early(), 0x8004_0b00, &[0, 1], run_link_frame) > 0);
    }

    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-004"]
    fn link_frame_40b00_late() {
        let occurrences = late_frames(0x8004_0b00, &[559, 1801]);
        assert!(check_each(&late(), 0x8004_0b00, &occurrences, run_link_frame) > 0);
    }

    /// gp+0x450 set (not captured): the six frame primitives follow the first
    /// one into the slot, in the original order.
    #[test]
    fn link_frame_40b00_linked() {
        let mut m = Machine::default();
        let (game, ot, buffer) = (0x8001_1000, 0x8001_2000, 0x8002_0000);
        m.set_gw(0x2d0, game);
        m.sw(game + 0x10, ot);
        m.sw(game + 0x14, 0x8001_3000);
        m.sw(ot, 0x0012_3456);
        m.set_gw(0x444, buffer);
        m.set_gw(0x49c, 0xffff_ffff);
        m.sb(GP + 0x450, 1);
        for offset in [0, 0x14, 0x28, 0x38, 0x48, 0x58, 0x60] {
            m.sw(buffer + offset, 0x0300_0000);
        }
        tree_40a24::link_frame_40b00(&mut m);
        assert!(m.take_fault().is_ok());
        assert_eq!(m.lw(game + 0x14), 0x8001_3000);
        let mut link = 0x0012_3456;
        for offset in [0, 0x14, 0x28, 0x48, 0x58, 0x38, 0x60] {
            assert_eq!(m.lw(buffer + offset), 0x0300_0000 | link, "+{offset:#x}");
            link = (buffer + offset) & 0x00ff_ffff;
        }
        assert_eq!(m.lw(ot), link);
    }

    /// Poisons the primitive fields 80040DE4 writes on every path.
    fn run_frame(m: &mut Machine, e: &Snapshot) -> Option<u32> {
        let buffer = m.gw(0x444);
        for (prim, end) in [
            (0, 0xd),
            (0x14, 0xc),
            (0x28, 0x10),
            (0x38, 0x10),
            (0x48, 0x10),
        ] {
            poison(m, buffer + prim + 4, 3);
            poison(m, buffer + prim + 8, end - 8);
        }
        tree_40a24::frame_40de4(m, e.gpr(4));
        None
    }

    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003"]
    fn frame_40de4() {
        assert!(check_each(&early(), 0x8004_0de4, &[0, 1], run_frame) > 0);
    }

    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-004"]
    fn frame_40de4_late() {
        let occurrences = late_frames(0x8004_0de4, &[559, 1801]);
        assert!(check_each(&late(), 0x8004_0de4, &occurrences, run_frame) > 0);
    }

    /// A shift of 0x20 (every captured call has 0): widths grow by shift - 14.
    #[test]
    fn frame_40de4_shifted() {
        let mut m = Machine::default();
        let buffer = 0x8002_0000;
        m.set_gw(0x444, buffer);
        for (offset, value) in [
            (8, 0x11),
            (9, 0x22),
            (0xa, 0x33),
            (0xb, 1),
            (0xc, 2),
            (0xd, 3),
        ] {
            m.sb(0x800c_4b28 + offset, value);
        }
        tree_40a24::frame_40de4(&mut m, 0x20);
        assert!(m.take_fault().is_ok());
        assert_eq!(m.lhu(buffer + 8), 0x10f - 0x20);
        assert_eq!(m.lbu(buffer + 0xc), 0x78);
        assert_eq!(m.lw(buffer + 4) & 0xff_ffff, 0x03_0201);
        assert_eq!(m.lhu(buffer + 0x14 + 8), 0x140 - 0x20);
        assert_eq!(m.lhu(buffer + 0x28 + 8), 0x14e - 0x20);
        assert_eq!(m.lhu(buffer + 0x28 + 0xc), 0x20 - 0xe);
        assert_eq!(m.lw(buffer + 0x28 + 4) & 0xff_ffff, 0x33_2211);
        assert_eq!(m.lhu(buffer + 0x38 + 8), 0x149 - 0x20);
        assert_eq!(m.lhu(buffer + 0x38 + 0xc), 0x20 - 0xe + 5);
        assert_eq!(m.lhu(buffer + 0x48 + 0xa), 0x31);
        assert_eq!(m.lhu(buffer + 0x48 + 0xe), 3);
    }

    /// Every captured 8004112C call has the target box disabled (800C4F3A is
    /// 0): only `v0 = 0` is compared. See [`target_box_4112c_draw_path`].
    fn run_target_box(m: &mut Machine, e: &Snapshot) -> Option<u32> {
        assert_eq!(entry_byte(e, 0x800c_4f3a), 0, "draw path captured");
        Some(tree_40a24::target_box_4112c(m))
    }

    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003"]
    fn target_box_4112c() {
        check_each(&early(), 0x8004_112c, &[0, 1], run_target_box);
    }

    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-004"]
    fn target_box_4112c_late() {
        let occurrences = late_frames(0x8004_112c, &[559, 1801]);
        check_each(&late(), 0x8004_112c, &occurrences, run_target_box);
    }

    /// Enabled box 32x24 at (100, 50), cell 11, frame counter 7: geometry,
    /// texture cell, scan-line scroll (28 % 16, 35 % 16) and link order worked
    /// out from the instructions.
    #[test]
    fn target_box_4112c_draw_path() {
        let mut m = Machine::default();
        let (game, ot, buffer, state) = (0x8001_1000, 0x8001_2000, 0x8002_0000, 0x800c_4f38);
        m.set_gw(0x2d0, game);
        m.sw(game + 0xc, 7);
        m.sw(game + 0x10, ot);
        m.sw(ot, 0x0012_3456);
        m.set_gw(0x444, buffer);
        for (offset, value) in [(0xb, 1), (0xc, 2), (0xd, 3)] {
            m.sb(0x800c_4b28 + offset, value);
        }
        m.sb(state + 2, 1);
        m.sb(state + 3, 11);
        m.sb(state + 6, 32);
        m.sb(state + 7, 24);
        m.sh(state + 8, 100);
        m.sh(state + 0xa, 50);
        assert_eq!(tree_40a24::target_box_4112c(&mut m), 0);
        assert!(m.take_fault().is_ok());
        let xy = |m: &mut Machine, prim: u32, offset: u32| {
            (m.lhu(prim + offset), m.lhu(prim + offset + 2))
        };
        let quad = buffer + 0x210;
        assert_eq!(m.lw(quad + 4) & 0xff_ffff, 0x03_0201);
        assert_eq!(xy(&mut m, quad, 8), (105, 58));
        assert_eq!(xy(&mut m, quad, 0x10), (137, 58));
        assert_eq!(xy(&mut m, quad, 0x18), (105, 82));
        assert_eq!(xy(&mut m, quad, 0x20), (137, 82));
        let quad = buffer + 0x1e8;
        assert_eq!(xy(&mut m, quad, 8), (111, 61));
        assert_eq!(xy(&mut m, quad, 0x10), (131, 61));
        assert_eq!(xy(&mut m, quad, 0x18), (111, 79));
        assert_eq!(xy(&mut m, quad, 0x20), (131, 79));
        assert_eq!(m.lhu(quad + 0xe), 0x34d6);
        let uv: Vec<(u8, u8)> = [0xc, 0x14, 0x1c, 0x24]
            .iter()
            .map(|&o| (m.lbu(quad + o), m.lbu(quad + o + 1)))
            .collect();
        assert_eq!(uv, [(96, 0xa4), (127, 0xa4), (96, 0xc7), (127, 0xc7)]);
        let tile = buffer + 0x238;
        assert_eq!(xy(&mut m, tile, 8), (112, 62));
        assert_eq!(xy(&mut m, tile, 0xc), (19, 16));
        assert_eq!(xy(&mut m, buffer + 0x248, 8), (112, 73));
        assert_eq!(xy(&mut m, buffer + 0x248, 0xc), (19, 1));
        assert_eq!(xy(&mut m, buffer + 0x258, 8), (112, 64));
        assert_eq!(xy(&mut m, buffer + 0x258, 0xc), (19, 1));
        let mut link = 0x0012_3456;
        for offset in [0x210, 0x1e8, 0x248, 0x258, 0x268, 0x238, 0x270] {
            assert_eq!(m.lw(buffer + offset), link, "+{offset:#x}");
            link = (buffer + offset) & 0x00ff_ffff;
        }
        assert_eq!(m.lw(ot), link);
    }

    /// No radio message in any capture (`[gp+0x49C]` = -1): v0 = the next free
    /// primitive passed in a1. See [`message_54d18_line`].
    fn run_message(m: &mut Machine, e: &Snapshot) -> Option<u32> {
        Some(tree_40a24::message_54d18(m, e.gpr(4), e.gpr(5)))
    }

    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003"]
    fn message_54d18() {
        check_each(&early(), 0x8005_4d18, &[0, 1], run_message);
    }

    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-004"]
    fn message_54d18_late() {
        let occurrences = late_frames(0x8005_4d18, &[559, 1801]);
        check_each(&late(), 0x8005_4d18, &occurrences, run_message);
    }

    /// Active message (not captured): line selection with a sign-extended table
    /// entry, text color/cursor, then the pending line routine 8005AC84.
    #[test]
    fn message_54d18_line() {
        let mut m = Machine::default();
        let (table, offsets, strings) = (0x8003_0000, 0x8003_1000, 0x8003_2000);
        m.set_gw(0x49c, 2);
        m.set_gw(0x4a0, table);
        m.sh(table + 8, 0xffff);
        m.set_gw(0x4ac, 10);
        m.set_gw(0x4b0, offsets);
        m.sh(offsets + 9 * 4, 0x20);
        m.set_gw(0x4b4, strings);
        tree_40a24::message_54d18(&mut m, 0x8001_2000, 0x8001_3000);
        // 8005AC84 (shared::draw_text) draws the empty line: no packets.
        assert!(m.take_fault().is_ok());
        assert_eq!(m.gw(0x4b8), 9);
        assert_eq!((m.lhu(0x800c_5790), m.lhu(0x800c_5792)), (0x84, 0x12));
        assert_eq!(m.read_bytes(0x800c_5798, 3), [0, 0x73, 0]);
    }

    #[test]
    fn text_position_5ac58() {
        let mut m = Machine::default();
        tree_40a24::text_position_5ac58(&mut m, 0x1_0123, 0xfffe);
        assert_eq!((m.lhu(0x800c_5790), m.lhu(0x800c_5792)), (0x0123, 0xfffe));
    }

    #[test]
    fn text_color_5ac6c() {
        let mut m = Machine::default();
        tree_40a24::text_color_5ac6c(&mut m, 0x101, 0x73, 0xff);
        assert_eq!(m.read_bytes(0x800c_5797, 5), [0, 1, 0x73, 0xff, 0]);
    }

    fn run_warnings(m: &mut Machine, e: &Snapshot) -> Option<u32> {
        tree_40a24::warnings_41a68(m, e.gpr(4));
        None
    }

    /// Frames 1795 and 1855 start a warning label's sound (8002D724).
    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-004"]
    fn warnings_41a68() {
        let occurrences = late_frames(0x8004_1a68, &[431, 559, 1801]);
        assert!(check_each(&late(), 0x8004_1a68, &occurrences, run_warnings) > 0);
        for occurrence in late_frames(0x8004_1a68, &[1795, 1855]) {
            check_blocked(
                &late(),
                0x8004_1a68,
                occurrence,
                &[0x8002_d724],
                run_warnings,
            );
        }
    }

    fn run_area(m: &mut Machine, e: &Snapshot) -> Option<u32> {
        Some(tree_40a24::area_status_46aa8(m, e.gpr(4)))
    }

    /// 1801 is the call from 80045F08 (the frame's first 80046AA8 call).
    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-004"]
    fn area_status_46aa8() {
        let occurrences = late_frames(0x8004_6aa8, &[431, 559, 1801]);
        check_each(&late(), 0x8004_6aa8, &occurrences, run_area);
    }

    /// The box tests, the 256-frame prediction and the mode 0x2A/0x3A altitude
    /// limits.
    #[test]
    fn area_status_46aa8_paths() {
        let mut m = Machine::default();
        let (table, area, entity) = (0x8001_0000, 0x8001_1000, 0x8001_2000);
        m.set_gw(0x2d8, table);
        m.sw(table + 4, entity);
        m.set_gw(0x32c, area);
        for (offset, value) in [(0x10, -1000i32), (0x14, 1000), (0x18, -1000), (0x1c, 1000)] {
            m.sw(area + offset, value as u32);
        }
        let status = |m: &mut Machine, x: i32, z: i32, vx: i32, y: i32, vy: i32, mode: u8| {
            m.sw(entity + 8, x as u32);
            m.sw(entity + 0xc, y as u32);
            m.sw(entity + 0x10, z as u32);
            m.sw(entity + 0x38, vx as u32);
            m.sw(entity + 0x3c, vy as u32);
            m.sb(0x800b_aeb4, mode);
            tree_40a24::area_status_46aa8(m, 1)
        };
        assert_eq!(status(&mut m, 1001, 0, 0, 0, 0, 0), 2);
        assert_eq!(status(&mut m, 0, -1001, 0, 0, 0, 0), 2);
        assert_eq!(status(&mut m, 900, 0, 1, 0, 0, 0), 1, "900 + 256 ahead");
        assert_eq!(status(&mut m, 0, 0, -3, 0, 0, 0), 0);
        assert_eq!(status(&mut m, 0, 0, 0, 0x03a9_8000, 1, 0x2a), 1);
        assert_eq!(status(&mut m, 0, 0, 0, 0x03a9_8000, 0, 0x2a), 0);
        assert_eq!(status(&mut m, 0, 0, 0, -0x0ea6_0001, 0, 0x2a), 1);
        assert_eq!(status(&mut m, 0, 0, 0, -0x0030_0000, 1, 0x3a), 1);
        assert_eq!(status(&mut m, 0, 0, 0, -0x0030_0000, 0, 0x3a), 0);
        assert_eq!(status(&mut m, 0, 0, 0, 0x7fff_0000, 0, 0x2b), 0);
        assert!(m.take_fault().is_ok());
    }

    fn run_label(m: &mut Machine, e: &Snapshot) -> Option<u32> {
        Some(tree_40a24::warning_label_415d8(m, e.gpr(4)))
    }

    /// Frame 1795 starts the label's sound (8002D724).
    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-004"]
    fn warning_label_415d8() {
        let occurrences = late_frames(0x8004_15d8, &[1801, 1855]);
        assert!(check_each(&late(), 0x8004_15d8, &occurrences, run_label) > 0);
        let blocked = at_frame(&late(), 0x8004_15d8, 1795);
        check_blocked(&late(), 0x8004_15d8, blocked, &[0x8002_d724], run_label);
    }

    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-004"]
    fn raise_warning_41d4c() {
        let occurrences = late_frames(0x8004_1d4c, &[1795]);
        let changed = check_each(&late(), 0x8004_1d4c, &occurrences, |m, e| {
            tree_40a24::raise_warning_41d4c(m, e.gpr(4));
            None
        });
        assert!(changed > 0);
    }

    /// Priority: a code at or above the active one is ignored, a lower one
    /// replaces it.
    #[test]
    fn raise_warning_41d4c_priority() {
        let mut m = Machine::default();
        let warning = 0x800c_4f48;
        m.sw(warning, 4);
        tree_40a24::raise_warning_41d4c(&mut m, 4);
        assert_eq!(m.read_bytes(warning + 4, 12), [0; 12]);
        m.sh(0x800a_fed0 + 3 * 4, 0xffc0);
        m.sb(0x800a_fed0 + 3 * 4 + 2, 7);
        m.sb(0x800a_fed0 + 3 * 4 + 3, 8);
        m.sw(warning + 8, 0xffff_ff00);
        tree_40a24::raise_warning_41d4c(&mut m, 3);
        assert_eq!(m.lw(warning), 3);
        assert_eq!(m.lw(warning + 4), 0xffff_ffc0);
        assert_eq!(m.read_bytes(warning + 8, 8), [1, 7, 0, 0, 1, 8, 0, 0]);
    }

    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-004"]
    fn voice_4b108() {
        let occurrences = late_frames(0x8004_b108, &[1795, 1855]);
        check_each(&late(), 0x8004_b108, &occurrences, |m, _| {
            Some(tree_40a24::voice_4b108(m))
        });
    }

    /// 80041DC8 has no capture: the gp+0x454 slot through 800415D8 with a
    /// one-glyph label (kind 0, color 1) at frame 3 (sound frame 7, so no sound).
    #[test]
    fn label_41dc8() {
        let mut m = Machine::default();
        let (game, ot, ctx, set, buffer) = (
            0x8001_1000,
            0x8001_2000,
            0x8001_4000,
            0x8004_0000,
            0x8002_0000,
        );
        let label = 0x800a_fc88 + 0x10;
        m.set_gw(0x2d0, game);
        m.sw(game + 0x10, ot);
        m.sw(ot, 0x0012_3456);
        m.set_gw(0x2dc, ctx);
        m.set_gw(0x440, set);
        m.sw(set + 8, 1);
        m.sw(set + 0xc, 2);
        m.set_gw(0x444, buffer);
        let slot = GP + 0x454;
        m.write_bytes(slot, &[1, 1, 3, 0]);
        m.write_bytes(
            label,
            &[0x30, 1, 10, 0, 0x40, 0, 0x30, 0, 2, 3, 0xff, 0, 5, 0, 7, 0],
        );
        m.write_bytes(0x800a_fe58 + 8, &[0x10, 0x20, 0x30, 8]);
        assert_eq!(tree_40a24::label_41dc8(&mut m), 1);
        assert!(m.take_fault().is_ok());
        assert_eq!(m.lhu(slot + 2), 4);
        let sprite = buffer + 0x68 + 20;
        let mode = buffer + 0x1a8 + 2 * 8;
        let marker = sprite + 0x14;
        assert_eq!(m.lw(sprite + 4) & 0xff_ffff, 0x50_5050);
        assert_eq!((m.lhu(sprite + 8), m.lhu(sprite + 0xa)), (0x43, 0x32));
        assert_eq!(m.read_bytes(sprite + 0xc, 2), [0x10, 0x20]);
        assert_eq!(m.lhu(sprite + 0xe), 0x7f9b);
        assert_eq!((m.lhu(sprite + 0x10), m.lhu(sprite + 0x12)), (0x30, 8));
        assert_eq!(m.lw(marker + 4) & 0xff_ffff, 0x00_4080);
        assert_eq!((m.lhu(marker + 8), m.lhu(marker + 0xa)), (0x40, 0x30));
        assert_eq!(m.read_bytes(marker + 0xc, 2), [0, 0xa]);
        assert_eq!((m.lhu(marker + 0x10), m.lhu(marker + 0x12)), (0x84, 9));
        assert_eq!(m.lw(mode + 4), 0xe100_0636);
        assert_eq!(m.lw(mode + 8 + 4), 0xe100_0616);
        assert_eq!((m.lw(set + 8), m.lw(set + 0xc)), (3, 4));
        let mut link = 0x0012_3456;
        for prim in [sprite, mode, marker, mode + 8] {
            assert_eq!(m.lw(prim) & 0xff_ffff, link, "{prim:#010x}");
            link = prim & 0x00ff_ffff;
        }
        assert_eq!(m.lw(mode) >> 24, 1, "draw-mode length");
        assert_eq!(m.lw(ot), link);
    }
}

mod tree_62680_a_tests {
    use super::*;

    /// (index, occurrence) cases of `callee`: both hud-003 calls when
    /// `with_early`, then the listed hud-004 frames.
    fn cases_62680_a(callee: u32, with_early: bool, frames: &[u64]) -> Vec<(PathBuf, usize)> {
        let mut cases = Vec::new();
        if with_early {
            cases.extend([(early(), 0), (early(), 1)]);
        }
        for &frame in frames {
            cases.push((late(), at_frame(&late(), callee, frame)));
        }
        cases
    }

    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
    fn reset_pool_627b4() {
        let changed: Vec<usize> = cases_62680_a(0x800627b4, true, &[559, 1801])
            .into_iter()
            .map(|(index, occurrence)| {
                check(&index, 0x800627b4, occurrence, |m, _| {
                    hud::tree_62680_a::reset_pool_627b4(m);
                    None
                })
            })
            .collect();
        eprintln!("800627b4: {changed:?} bytes");
        assert!(changed.iter().all(|&c| c > 0), "{changed:?}");
    }

    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
    fn link_pool_62800() {
        let changed: Vec<usize> = cases_62680_a(0x80062800, true, &[559, 1801])
            .into_iter()
            .map(|(index, occurrence)| {
                check(&index, 0x80062800, occurrence, |m, _| {
                    hud::tree_62680_a::link_pool_62800(m);
                    None
                })
            })
            .collect();
        eprintln!("80062800: {changed:?} bytes");
        assert!(changed.iter().all(|&c| c > 0), "{changed:?}");
    }

    /// No capture calls 80062EA0: the routine changes nothing and v0 is the
    /// last compare (1), which `check` compares with the original.
    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
    fn rotation_check_62c34() {
        for (index, occurrence) in cases_62680_a(0x80062c34, true, &[559]) {
            let mut v0 = 0;
            check(&index, 0x80062c34, occurrence, |m, e| {
                v0 = hud::tree_62680_a::rotation_check_62c34(m, e.gpr(4));
                Some(v0)
            });
            assert_eq!(v0, 1);
        }
    }

    /// Blocked on shared 800306AC (the rotation from the view angles, reached
    /// first) and SDK MulMatrix2 800A5238.
    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
    fn boresight_62cb0() {
        for (index, occurrence) in cases_62680_a(0x80062cb0, true, &[559]) {
            check_blocked(
                &index,
                0x80062cb0,
                occurrence,
                &[0x800306ac, 0x800a5238],
                |m, e| Some(hud::tree_62680_a::boresight_62cb0(m, e.gpr(4))),
            );
        }
    }

    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
    fn center_lines_634e8() {
        let changed: Vec<usize> = cases_62680_a(0x800634e8, true, &[559])
            .into_iter()
            .map(|(index, occurrence)| {
                check(&index, 0x800634e8, occurrence, |m, _| {
                    hud::tree_62680_a::center_lines_634e8(m);
                    None
                })
            })
            .collect();
        eprintln!("800634e8: {changed:?} bytes");
        assert!(changed.iter().all(|&c| c > 0), "{changed:?}");
    }

    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
    fn brackets_6492c() {
        let changed: Vec<usize> = cases_62680_a(0x8006492c, true, &[559])
            .into_iter()
            .map(|(index, occurrence)| {
                check(&index, 0x8006492c, occurrence, |m, e| {
                    hud::tree_62680_a::brackets_6492c(m, e.gpr(4));
                    None
                })
            })
            .collect();
        eprintln!("8006492c: {changed:?} bytes");
        assert!(changed.iter().all(|&c| c > 0), "{changed:?}");
    }

    /// Frames 431 and 559 have no target (slot 0xFF): nothing changes and v0
    /// (0) is compared. Frame 1371 (flagged target) is blocked on SDK
    /// SquareRoot0 800A47C8, frame 1801 on shared 800306AC (via 8004444C);
    /// ApplyMatrixLV 800A4D78 and shared 80030B10 follow on both.
    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-004"]
    fn target_arrow_645c0() {
        for frame in [431, 559] {
            let occurrence = at_frame(&late(), 0x800645c0, frame);
            let mut v0 = 1;
            check(&late(), 0x800645c0, occurrence, |m, _| {
                v0 = hud::tree_62680_a::target_arrow_645c0(m);
                Some(v0)
            });
            assert_eq!(v0, 0);
        }
        for frame in [1371, 1801] {
            let occurrence = at_frame(&late(), 0x800645c0, frame);
            check_blocked(
                &late(),
                0x800645c0,
                occurrence,
                &[0x800a47c8, 0x800306ac, 0x800a4d78, 0x80030b10],
                |m, _| Some(hud::tree_62680_a::target_arrow_645c0(m)),
            );
        }
    }

    /// Synthetic pool for [`link_pool_62800`]: descriptor, buffer, gameplay
    /// block and ordering-table slot.
    const POOL_62680_A: u32 = 0x8010_0000;
    const BUFFER_62680_A: u32 = 0x8011_0000;
    const GAMEPLAY_62680_A: u32 = 0x8012_0000;
    const SLOT_62680_A: u32 = 0x8013_0000;

    fn pool_machine_62680_a(counts: [u32; 7], flag: u32) -> Machine {
        let mut m = Machine::default();
        m.set_gw(0x508, POOL_62680_A);
        m.set_gw(0x50c, BUFFER_62680_A);
        m.set_gw(0x2d0, GAMEPLAY_62680_A);
        m.sw(GAMEPLAY_62680_A + 0x10, SLOT_62680_A);
        m.sw(SLOT_62680_A, 0x0012_3456);
        for (i, count) in counts.into_iter().enumerate() {
            m.sw(POOL_62680_A + 8 + 4 * i as u32, count);
        }
        m.sw(POOL_62680_A + 0x24, flag);
        // Every packet tag carries a length byte that linking must keep.
        for offset in (0..0x980).step_by(4) {
            m.sw(BUFFER_62680_A + offset, 0x0500_0000);
        }
        m
    }

    /// Walk the ordering-table chain from the slot: packet addresses until the
    /// original link 0x123456.
    fn chain_62680_a(m: &mut Machine) -> Vec<u32> {
        let mut chain = Vec::new();
        let mut link = m.lw(SLOT_62680_A) & 0xff_ffff;
        while link != 0x12_3456 && chain.len() < 100 {
            let packet = 0x8000_0000 | link;
            assert_eq!(m.lw(packet) >> 24, 5, "length byte of {packet:08x}");
            chain.push(packet);
            link = m.lw(packet) & 0xff_ffff;
        }
        chain
    }

    /// Expected chain: the first buffer packet, the +0x940 packet when
    /// flagged, then each class from the last to the first, packets descending.
    fn expected_chain_62680_a(counts: [u32; 7], flag: bool, skip_first: bool) -> Vec<u32> {
        const CLASSES: [(u32, u32); 7] = [
            (0x008, 0x18),
            (0x188, 0x1c),
            (0x1f8, 0x28),
            (0x3d8, 0x10),
            (0x7d8, 0x14),
            (0x878, 0x28),
            (0x8a0, 0x14),
        ];
        let mut chain = vec![BUFFER_62680_A];
        if flag {
            chain.push(BUFFER_62680_A + 0x940);
        }
        for (class, (base, stride)) in CLASSES.into_iter().enumerate().rev() {
            if class == 0 && skip_first {
                continue;
            }
            for index in (0..counts[class]).rev() {
                chain.push(BUFFER_62680_A + base + index * stride);
            }
        }
        chain
    }

    /// Every class and the flagged +0x940 packet (the captures only use three
    /// classes and never the flag).
    #[test]
    fn link_pool_62800_all_classes() {
        // Within each class's capacity (the quad class at +0x878 holds one).
        let counts = [2, 1, 1, 3, 1, 1, 2];
        let mut m = pool_machine_62680_a(counts, 1);
        hud::tree_62680_a::link_pool_62800(&mut m);
        assert!(m.take_fault().is_ok());
        assert_eq!(
            chain_62680_a(&mut m),
            expected_chain_62680_a(counts, true, false)
        );
    }

    /// 16 packets in the first class: the warning printf (800AC3E8, pending)
    /// replaces that class; the others are still linked.
    #[test]
    fn link_pool_62800_overflow() {
        let counts = [0x10, 0, 1, 0, 0, 1, 0];
        let mut m = pool_machine_62680_a(counts, 0);
        hud::tree_62680_a::link_pool_62800(&mut m);
        assert!(m.take_fault().is_ok());
        assert_eq!(
            m.host_calls,
            [ac3_flight::machine::HostCall {
                function: 0x800a_c3e8,
                args: [0x8002_407c, 0x10, 0, 0]
            }]
        );
        assert_eq!(
            chain_62680_a(&mut m),
            expected_chain_62680_a(counts, false, true)
        );
    }

    /// The 80062EA0 branch (no capture reaches it) and the -2..=2 bounds.
    #[test]
    fn rotation_check_62c34_bounds() {
        let view = 0x8010_0000;
        let angles = 0x8010_0100;
        for (values, called) in [
            ([0xfffe, 2, 0], false),
            ([0, 0, 0xfffd], true),
            ([0, 3, 0], true),
            ([0x8000, 0, 0], true),
        ] {
            let mut m = Machine::default();
            m.sw(view + 0x10, angles);
            for (i, value) in values.into_iter().enumerate() {
                m.sh(angles + 2 * i as u32, value);
            }
            let sp = m.sp;
            let v0 = hud::tree_62680_a::rotation_check_62c34(&mut m, view);
            if called {
                // 80062EA0 ran (attitude marker); its SDK callees SetMulMatrix
                // 800A4ED8 and NormalClip 800A58E8 may still be untranslated.
                let _ = v0;
                let fault = m.take_fault();
                assert!(
                    matches!(
                        fault,
                        Ok(())
                            | Err(ac3_flight::Fault::Untranslated {
                                address: 0x800a_4ed8 | 0x800a_58e8
                            })
                    ),
                    "{values:?}: {fault:?}"
                );
                assert_eq!(m.sp, sp);
            } else {
                assert_eq!(v0, 1, "{values:?}");
                assert!(m.take_fault().is_ok());
            }
        }
    }

    /// No brackets while the boresight depth (800C4B40) is not positive.
    #[test]
    fn brackets_6492c_behind() {
        let mut m = Machine::default();
        m.sw(0x800c_4b40, 0xffff_ffff);
        let before = m.ram().to_vec();
        hud::tree_62680_a::brackets_6492c(&mut m, 0x8010_0000);
        assert!(m.take_fault().is_ok());
        assert!(m.ram() == &before[..]);
    }

    /// The early exits of 800645C0 that no capture takes (null object, dead
    /// object, near unflagged object, flagged object without a screen record):
    /// v0 0, no callee, nothing written, frame released.
    #[test]
    fn target_arrow_645c0_exits() {
        let object = 0x8010_0000;
        for (pointer, word0, halfword18, word20) in [
            (0, 1, 0x400, 0),
            (object, 0x0100_0082, 0x400, 0),
            (object, 1, 0x3ff, 0x0005_0000),
            (object, 1, 0x3ff, 0x0007_0000),
        ] {
            let mut m = Machine::default();
            m.sb(0x800c_4b36, 3);
            m.sw(0x800c_4c98 + 12, pointer);
            m.sw(object, word0);
            m.sh(object + 0x18, halfword18);
            m.sw(object + 0x20, word20);
            let before = m.ram().to_vec();
            let sp = m.sp;
            assert_eq!(hud::tree_62680_a::target_arrow_645c0(&mut m), 0);
            assert!(m.take_fault().is_ok(), "{pointer:08x} {word0:08x}");
            assert!(m.ram() == &before[..]);
            assert_eq!(m.sp, sp);
        }
    }

    /// 8004444C has no capture: its offset VECTOR (`(point - object) >> 12`,
    /// arithmetic) is checked in its frame; the rest is blocked on shared
    /// 800306AC and SDK ApplyMatrixLV 800A4D78.
    #[test]
    fn relative_view_4444c() {
        let mut m = Machine::default();
        let (object, point, out) = (0x8010_0000, 0x8010_0100, 0x8010_0200);
        for (i, (from, to)) in [
            (0x1000_u32, 0x5fff_u32),
            (0x0000_1000, 0x0000_0000),
            (0x7fff_f000, 0x8000_0000),
        ]
        .into_iter()
        .enumerate()
        {
            m.sw(object + 4 * i as u32, from);
            m.sw(point + 4 * i as u32, to);
        }
        let sp = m.sp;
        assert_eq!(
            hud::tree_62680_a::relative_view_4444c(&mut m, object, point, out),
            out
        );
        assert_eq!(m.sp, sp);
        let fault = m.take_fault();
        assert!(
            matches!(
                fault,
                Ok(())
                    | Err(ac3_flight::Fault::Untranslated {
                        address: 0x8003_06ac | 0x800a_4d78
                    })
            ),
            "{fault:?}"
        );
        let offset = sp - 0x50 + 0x30;
        assert_eq!(
            [m.lw(offset), m.lw(offset + 4), m.lw(offset + 8)],
            [4, 0xffff_ffff, 1]
        );
    }
}

mod tree_62680_b_tests {
    use super::*;

    use ac3_flight::Fault;
    use ac3_flight::hud::tree_62680_b as g6b;

    /// (index, occurrence) of the first `early_count` hud-003 entries of
    /// `callee` and of its hud-004 entries in `late_frames`.
    fn g6b_cases(callee: u32, early_count: usize, late_frames: &[u64]) -> Vec<(PathBuf, usize)> {
        let mut cases: Vec<_> = (0..early_count).map(|n| (early(), n)).collect();
        for &frame in late_frames {
            cases.push((late(), at_frame(&late(), callee, frame)));
        }
        cases
    }

    /// The return snapshot that `check_call` pairs with entry `occurrence` of
    /// `callee` in `index`.
    fn g6b_exit(index: &Path, callee: u32, occurrence: usize) -> Snapshot {
        let list = snapshot::read_index(index).unwrap();
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
        Snapshot::load(index.parent().unwrap(), exit).unwrap()
    }

    /// Replays still-stubbed callees so the rest of a routine can be compared
    /// in full: copies the guest stack bytes the stubs produce (`frame +
    /// offset`, `length`; the routine's return snapshot still holds them in its
    /// stale frame, which no later callee overlays) into the entry machine,
    /// with the GTE state the stubs leave that the routine's own projections
    /// do not replace: the color FIFO RGB0..RGB2 pushed by the GPF sequences of
    /// 800306AC/80030534, and the high halfwords of IR1..IR3 (raw storage from
    /// the stubs' `mfc2` sign-extension write-back; RTPS/RTPT replace only the
    /// low halfwords, which stay compared). All other GTE registers the stubs
    /// write are overwritten by the routine's projections.
    fn g6b_replay(m: &mut Machine, exit: &Snapshot, frame: u32, ranges: &[(u32, usize)]) {
        for &(offset, length) in ranges {
            let at = ((frame + offset) & 0x1f_ffff) as usize;
            m.write_bytes(frame + offset, &exit.ram[at..at + length]);
        }
        let after = &exit.info.registers.cp2d;
        // RGB0..RGB2 (data registers 20..22).
        m.gte.data[20..23].copy_from_slice(&after[20..23]);
        // IR1..IR3 (data registers 9..11).
        for (stored, high) in m.gte.data[9..12].iter_mut().zip(&after[9..12]) {
            *stored = (high & 0xffff_0000) | (*stored & 0xffff);
        }
    }

    /// After a replayed run: the first fault may only be one of the replayed
    /// `stubs` (no-ops while untranslated). True when one was raised.
    fn g6b_stub_fault(m: &mut Machine, stubs: &[u32]) -> bool {
        match m.take_fault() {
            Ok(()) => false,
            Err(Fault::Untranslated { address }) if stubs.contains(&address) => true,
            Err(fault) => panic!("{fault}"),
        }
    }

    /// Synthetic state: packet counters at 80100000 (`[gp+0x508]`), packet
    /// pool at 80110000 (`[gp+0x50C]`).
    fn g6b_machine() -> Machine {
        let mut m = Machine::default();
        m.set_gw(0x508, 0x8010_0000);
        m.set_gw(0x50c, 0x8011_0000);
        m
    }

    const G6B_LADDER_STUBS: [u32; 2] = [0x8003_06ac, 0x800a_5238];
    const G6B_PIPPER_STUBS: [u32; 3] = [0x8003_0534, 0x800a_4fe8, 0x8003_06ac];

    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003 and hud-004"]
    fn pitch_ladder_635cc() {
        for (index, occurrence) in g6b_cases(0x8006_35cc, 2, &[559]) {
            check_blocked(
                &index,
                0x8006_35cc,
                occurrence,
                &G6B_LADDER_STUBS,
                |m, e| {
                    g6b::pitch_ladder_635cc(m, e.gpr(4));
                    Some(0)
                },
            );
        }
    }

    /// Full comparison with the outputs of 800306AC/MulMatrix2 replayed: the
    /// product matrix at frame +0x20 (nine halfwords) and RGB0..2.
    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003 and hud-004"]
    fn pitch_ladder_635cc_replayed() {
        for (index, occurrence) in g6b_cases(0x8006_35cc, 2, &[559]) {
            let exit = g6b_exit(&index, 0x8006_35cc, occurrence);
            let changed = check(&index, 0x8006_35cc, occurrence, |m, e| {
                g6b_replay(m, &exit, e.gpr(29) - 0xb8, &[(0x20, 0x12)]);
                g6b::pitch_ladder_635cc(m, e.gpr(4));
                g6b_stub_fault(m, &G6B_LADDER_STUBS);
                Some(0)
            });
            eprintln!(
                "replayed {occurrence} of {}: {changed} bytes reproduced",
                index.display()
            );
            assert!(changed > 0);
        }
    }

    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003 and hud-004"]
    fn horizon_ring_63dcc() {
        for (index, occurrence) in g6b_cases(0x8006_3dcc, 2, &[559]) {
            check_blocked(&index, 0x8006_3dcc, occurrence, &[0x8003_06ac], |m, e| {
                g6b::horizon_ring_63dcc(m, e.gpr(4));
                Some(0)
            });
        }
    }

    /// Full comparison with 800306AC replayed (matrix at frame +0x10, RGB0..2);
    /// covers 80063FE4's label paths on the steps that project.
    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003 and hud-004"]
    fn horizon_ring_63dcc_replayed() {
        for (index, occurrence) in g6b_cases(0x8006_3dcc, 2, &[559]) {
            let exit = g6b_exit(&index, 0x8006_3dcc, occurrence);
            let changed = check(&index, 0x8006_3dcc, occurrence, |m, e| {
                g6b_replay(m, &exit, e.gpr(29) - 0x58, &[(0x10, 0x12)]);
                g6b::horizon_ring_63dcc(m, e.gpr(4));
                g6b_stub_fault(m, &[0x8003_06ac]);
                Some(0)
            });
            eprintln!(
                "replayed {occurrence} of {}: {changed} bytes reproduced",
                index.display()
            );
            assert!(changed > 0);
        }
    }

    /// The captured calls are steps 0x200/0x400 (no label, v0 -1).
    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003 and hud-004"]
    fn ring_label_63fe4() {
        for (index, occurrence) in g6b_cases(0x8006_3fe4, 2, &[559]) {
            let mut result = 0;
            check(&index, 0x8006_3fe4, occurrence, |m, e| {
                result = g6b::ring_label_63fe4(m, e.gpr(4), e.gpr(5));
                Some(result)
            });
            assert_eq!(result, u32::MAX);
        }
    }

    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-004"]
    fn gun_pipper_64ae8() {
        for (index, occurrence) in g6b_cases(0x8006_4ae8, 0, &[1029, 1371, 1801]) {
            check_blocked(
                &index,
                0x8006_4ae8,
                occurrence,
                &G6B_PIPPER_STUBS,
                |m, _| Some(g6b::gun_pipper_64ae8(m)),
            );
        }
    }

    /// Full comparison with 80030534/ApplyRotMatrixLV/800306AC replayed: the
    /// camera matrix at frame +0x10, the forward offset at +0x40 and RGB0..2.
    #[test]
    #[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-004"]
    fn gun_pipper_64ae8_replayed() {
        for (index, occurrence) in g6b_cases(0x8006_4ae8, 0, &[1029, 1371, 1801]) {
            let exit = g6b_exit(&index, 0x8006_4ae8, occurrence);
            let changed = check(&index, 0x8006_4ae8, occurrence, |m, e| {
                g6b_replay(m, &exit, e.gpr(29) - 0x80, &[(0x10, 0x12), (0x40, 12)]);
                let v0 = g6b::gun_pipper_64ae8(m);
                g6b_stub_fault(m, &G6B_PIPPER_STUBS);
                Some(v0)
            });
            eprintln!(
                "replayed {occurrence} of {}: {changed} bytes reproduced",
                index.display()
            );
            assert!(changed > 0);
        }
    }

    /// The readout glyphs of a negative and a three-digit pitch (font 2 with
    /// u 0x10, v 0x20, height 7, width/stride 5; color byte 1 = red).
    #[test]
    fn pitch_readout_635cc_synthetic() {
        let (left, right) = (0x0032_0064u32, 0x0034_00dcu32);
        for (pitch, glyphs) in [(-1229i32, [u32::MAX, 2, 7]), (4779, [0, 0, 5])] {
            let mut m = g6b_machine();
            m.write_bytes(0x800a_fa8c, &[0x10, 0x20, 0, 7, 5, 0, 0, 0]);
            m.sb(0x800c_4b39, 1);
            m.sw(0x8010_0020, 3);
            let frame = m.push_frame(0xb8);
            g6b::pitch_readout_635cc(&mut m, frame, pitch, left, right);
            assert!(m.take_fault().is_ok());
            let codes: Vec<u32> = glyphs.into_iter().skip(usize::from(pitch >= 0)).collect();
            assert_eq!(m.lw(0x8010_0020), 3 + 2 * codes.len() as u32);
            let mut sprite = 0x8011_0000 + 0x8a0 + 3 * 20;
            for (code, dx) in codes
                .iter()
                .zip(if pitch < 0 { [-9, -4, 1] } else { [-4, 1, 0] })
            {
                for at in [left, right] {
                    let x = (at as u16).wrapping_add(dx as u16);
                    let y = ((at >> 16) as u16).wrapping_sub(3);
                    assert_eq!((m.lhu(sprite + 8), m.lhu(sprite + 0xa)), (x, y));
                    assert_eq!(m.read_bytes(sprite + 4, 3), [0x83, 0, 0]);
                    let (u, v, w, h) = match *code {
                        u32::MAX => (0x28, 6, 4, 6),
                        digit => (0x10 + 5 * digit as u8, 0x20, 5, 7),
                    };
                    assert_eq!((m.lbu(sprite + 0xc), m.lbu(sprite + 0xd)), (u, v));
                    assert_eq!((m.lhu(sprite + 0x10), m.lhu(sprite + 0x12)), (w, h));
                    sprite += 20;
                }
            }
            assert_eq!(m.lw(frame + 0x10), sprite - 20, "last sprite argument");
            assert_eq!(m.lw(frame + 0x14), 1, "color argument");
        }
    }

    /// The pipper sprite's three outcomes.
    #[test]
    fn pipper_sprite_64ae8_synthetic() {
        let mut m = g6b_machine();
        m.write_bytes(0x800c_4b33, &[0x11, 0x22, 0x33]);
        m.sw(0x8010_0024, 7);
        let record = 0x8012_0000;
        let screen = 0x005a_0082;
        m.sw(record, 0x20);
        assert_eq!(
            g6b::pipper_sprite_64ae8(&mut m, record, 0x41, screen, 0x8000_1000),
            0
        );
        m.sw(record, 0x1000_0083);
        assert_eq!(
            g6b::pipper_sprite_64ae8(&mut m, record, 0x41, screen, 0x1000),
            0
        );
        assert_eq!(m.lw(0x8010_0024), 7);
        assert!(m.read_bytes(0x8011_0940, 16).iter().all(|&b| b == 0));
        m.sw(record, 0x20);
        assert_eq!(
            g6b::pipper_sprite_64ae8(&mut m, record, 0x41, screen, 0x1000),
            8
        );
        let sprite = 0x8011_0940;
        assert_eq!(m.read_bytes(sprite + 4, 3), [0x11, 0x22, 0x33]);
        assert_eq!((m.lhu(sprite + 8), m.lhu(sprite + 0xa)), (0x78, 0x50));
        assert_eq!(m.lhu(sprite + 0xe), 0x7e59);
        assert_eq!(m.lw(0x8010_0024), 8);
        assert!(m.take_fault().is_ok());
    }

    /// No locked target record: the trail still advances (unless context bit
    /// 29 is set) and v0 is 1. The forward offset is read back from the guest
    /// frame, whatever 80030534/ApplyRotMatrixLV leave there.
    #[test]
    fn gun_pipper_64ae8_no_target() {
        for paused in [false, true] {
            let mut m = g6b_machine();
            m.set_gw(0x2d8, 0x8013_0000);
            m.sw(0x8013_0000, 0x8013_1000);
            for k in 0..3 {
                m.sw(0x8013_1008 + k * 4, 0x1000 * (k + 1));
                m.sw(0x8013_1038 + k * 4, k + 1);
            }
            m.set_gw(0x2dc, 0x8013_2000);
            m.sw(0x8013_2000, if paused { 0x2000_0000 } else { 0 });
            m.sb(0x800b_aef8, 0x1f);
            m.sb(0x800c_4b36, 5);
            let frame = m.sp - 0x80;
            assert_eq!(g6b::gun_pipper_64ae8(&mut m), 1);
            g6b_stub_fault(&mut m, &G6B_PIPPER_STUBS[..2]);
            assert_eq!(m.sp, frame + 0x80);
            let head: u32 = if paused { 0x1f } else { 0 };
            assert_eq!(u32::from(m.lbu(0x800b_aef8)), head);
            for k in 0..3 {
                let offset = m.lw(frame + 0x40 + k * 4);
                let expected = (0x1000 * (k + 1))
                    .wrapping_add(offset)
                    .wrapping_add((k + 1) << 5);
                assert_eq!(m.lw(0x800c_57a0 + head * 16 + k * 4), expected);
            }
            assert_eq!(m.lbu(0x800b_aef9), 0, "shown entry untouched");
        }
    }

    /// A target in gun range: delay (0x68 << 5) / 0x100 = 13 frames selects the
    /// trail entry 13 behind the new head (5 - 13 = 24 mod 32), which lies
    /// 0x100 ahead of the eye. Both rotations are pre-filled identities (what
    /// 80030534/800306AC give for zero angles with sin 0 = 0, cos 0 = 1.0) and
    /// the forward offset (0, 0, 0x1580000); with the stubs as no-ops the entry
    /// projects to the screen center (160, 120). Whatever the stubs leave, the
    /// sprite must follow the frame's own projection and flag.
    #[test]
    fn gun_pipper_64ae8_in_range() {
        let mut m = g6b_machine();
        m.sh(0x8001_0498 + 0x2000, 0x4000);
        m.set_gw(0x2d8, 0x8013_0000);
        m.sw(0x8013_0000, 0x8013_1000);
        m.set_gw(0x2dc, 0x8013_2000);
        m.set_gw(0x2d0, 0x8013_3000);
        m.sw(0x8013_3028, 0x8013_4000);
        let eye = [0x1_0000u32, 0x2_0000, 0x3_0000];
        for (k, &e) in eye.iter().enumerate() {
            m.sw(0x8013_4004 + k as u32 * 4, e);
            let ahead = if k == 2 { 0x100 << 12 } else { 0 };
            m.sw(0x800c_57a0 + 24 * 16 + k as u32 * 4, e + ahead);
        }
        m.sb(0x800b_aef8, 4);
        m.sb(0x800c_4b36, 9);
        m.sw(0x800c_4c98 + 9 * 4, 0x8012_0000);
        m.sw(0x8012_0000, 0x20);
        m.sw(0x8012_0004, 0x68);
        m.sw(0x800c_4c90, 0x100);
        m.write_bytes(0x800c_4b33, &[0x11, 0x22, 0x33]);
        m.gte.control[24] = 160 << 16;
        m.gte.control[25] = 120 << 16;
        m.gte.control[26] = 0x100;
        let frame = m.sp - 0x80;
        for (k, value) in [0x1000u16, 0, 0, 0, 0x1000, 0, 0, 0, 0x1000]
            .into_iter()
            .enumerate()
        {
            m.sh(frame + 0x10 + k as u32 * 2, value);
        }
        m.sw(frame + 0x48, 0x158_0000);
        let v0 = g6b::gun_pipper_64ae8(&mut m);
        let stubbed = g6b_stub_fault(&mut m, &G6B_PIPPER_STUBS);
        assert_eq!(m.lbu(0x800b_aef8), 5);
        assert_eq!(m.lbu(0x800b_aef9), 24);
        let offset: Vec<u32> = (0..3).map(|k| m.lw(frame + 0x40 + k * 4)).collect();
        for (k, &value) in offset.iter().enumerate() {
            assert_eq!(
                m.lw(0x800c_57a0 + 5 * 16 + k as u32 * 4),
                value,
                "head entry"
            );
        }
        let (flag, screen) = (m.lw(frame + 0x64), m.lw(frame + 0x58));
        if stubbed {
            assert_eq!((flag, screen), (0, 0x0078_00a0));
        }
        let sprite = 0x8011_0940;
        if (flag as i32) < 0 {
            assert_eq!(v0, 0);
        } else {
            assert_eq!(v0, 1);
            assert_eq!(m.read_bytes(sprite + 4, 3), [0x11, 0x22, 0x33]);
            let x = (screen as u16).wrapping_sub(10);
            let y = ((screen >> 16) as u16).wrapping_sub(10);
            assert_eq!((m.lhu(sprite + 8), m.lhu(sprite + 0xa)), (x, y));
            assert_eq!(
                m.lhu(sprite + 0xe),
                0x7e18 + 0x42,
                "delay 13: step (6 & 3) + (6 & 0xC) * 16"
            );
        }
    }

    /// Steps off the 0x800 grid return -1 without touching memory; the
    /// dispatch claims all four routines' addresses.
    #[test]
    fn ring_label_63fe4_off_grid() {
        let mut m = Machine::default();
        let before = m.ram().to_vec();
        assert_eq!(
            hud::dispatch(&mut m, 0x8006_3fe4, [0x8012_0000, 0x4200, 0, 0]),
            Some(u32::MAX)
        );
        assert_eq!(g6b::ring_label_63fe4(&mut m, 0x8012_0000, 0x7ff), u32::MAX);
        assert!(m.ram() == before.as_slice() && m.take_fault().is_ok());
        for target in [0x8006_35cc, 0x8006_3dcc, 0x8006_4ae8] {
            let mut m = g6b_machine();
            assert!(g6b::dispatch(&mut m, target, [0x8012_0000, 0, 0, 0]).is_some());
        }
    }
}

/// Blocked on shared 8003A0F8/8003A08C (through 8003E780) and, in frame
/// 2487 (fade), on rsin 800A45C8.
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
fn status_status_39d84() {
    for (index, occurrence) in hud_cases(0x80039d84, &[559, 1801, 1923, 2487]) {
        check_blocked(
            &index,
            0x80039d84,
            occurrence,
            &[0x8003a0f8, 0x8003a08c, 0x800a45c8],
            |m, _| {
                hud::status::status_39d84(m);
                None
            },
        );
    }
}

#[test]
fn mips_div_edges() {
    assert_eq!(hud::mips_div(7, 2), 3);
    assert_eq!(hud::mips_div(-7, 2), -3);
    assert_eq!(hud::mips_div(5, 0), -1);
    assert_eq!(hud::mips_div(-5, 0), 1);
    assert_eq!(hud::mips_div(i32::MIN, -1), i32::MIN);
}

/// Stubs the composition can reach first: SDK memset (800AC148) in
/// 8003357C, then the shared/SDK routines of its subtree.
const COMPOSE_STUBS: [u32; 9] = [
    0x800ac148, 0x800306ac, 0x80030534, 0x80030b10, 0x800a5238, 0x800a47c8, 0x800aabd8, 0x800a4fe8,
    0x800a4d78,
];

/// Like [`check_blocked`] for the composite routines, with one explicitly
/// allowed difference: the upper halfword of GTE register VZ0 (data 1).
/// `lwc2` stores the full word, and in a composite run the pad halfword of
/// the last SVECTOR a callee loads from its stack frame is uninitialized:
/// it holds stale stack contents, in the original e.g. the upper half of a
/// saved register (0x800C.... in frame 1801), which the native code does not
/// keep in guest memory. VZ0 is a 16-bit register: the low halfword, the
/// only part the GTE uses and `mfc2` returns, must still match.
fn check_composite(
    index: &Path,
    callee: u32,
    occurrence: usize,
    native: impl FnOnce(&mut Machine, &Snapshot),
) {
    let directory = index.parent().unwrap();
    let infos = snapshot::read_index(index).unwrap();
    let entry = infos
        .iter()
        .filter(|s| s.callee == callee && s.stage == "entry")
        .nth(occurrence)
        .unwrap();
    let exit = infos
        .iter()
        .find(|s| {
            s.callee == callee
                && s.stage == "return"
                && s.sequence > entry.sequence
                && s.ra == entry.ra
                && s.registers.gpr[29] == entry.registers.gpr[29]
        })
        .unwrap();
    let entry = Snapshot::load(directory, entry).unwrap();
    let exit = Snapshot::load(directory, exit).unwrap();
    let mut m = entry.machine();
    native(&mut m, &entry);
    if let Err(fault) = m.take_fault() {
        let blocked = COMPOSE_STUBS
            .iter()
            .any(|&stub| fault == ac3_flight::Fault::Untranslated { address: stub });
        assert!(blocked, "{callee:08x}: {fault}");
        eprintln!("BLOCKED {callee:08x}: {fault}");
        return;
    }
    let mut differences = snapshot::compare(&m, &exit);
    let ours = m.gte.data[1];
    let original = exit.info.registers.cp2d[1];
    if differences.gte_data == [1] && (ours ^ original) & 0xffff == 0 {
        eprintln!("{callee:08x}: VZ0 upper half {ours:#010x} / {original:#010x} (stale pad)");
        differences.gte_data.clear();
    }
    assert!(
        differences.is_empty(),
        "{callee:08x}: {}",
        differences.summary()
    );
    let changed = snapshot::changed_bytes(&entry, &exit);
    eprintln!("{callee:08x}: unblocked, {changed} bytes reproduced");
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
fn compose_composition_3357c() {
    for (index, occurrence) in hud_cases(0x8003357c, &[559, 1801]) {
        check_composite(&index, 0x8003357c, occurrence, |m, _| {
            hud::compose::composition_3357c(m);
        });
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
fn compose_compose_39b80() {
    for (index, occurrence) in hud_cases(0x80039b80, &[559, 1801]) {
        check_composite(&index, 0x80039b80, occurrence, |m, e| {
            hud::compose::compose_39b80(m, e.gpr(4));
        });
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
fn compose_gauges_3bdf8() {
    for (index, occurrence) in hud_cases(0x8003bdf8, &[559, 1801]) {
        check_composite(&index, 0x8003bdf8, occurrence, |m, e| {
            hud::compose::gauges_3bdf8(m, e.gpr(4));
        });
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-003, hud-004"]
fn compose_overlay_62680() {
    for (index, occurrence) in hud_cases(0x80062680, &[431, 559, 1029, 1371, 1801]) {
        check_composite(&index, 0x80062680, occurrence, |m, e| {
            hud::compose::overlay_62680(m, e.gpr(4));
        });
    }
}

mod stream_draw_tests {
    //! No capture reaches the 8002CFCC drawing modes yet: synthetic states,
    //! expected packets with hand-derived values (from the instructions),
    //! compared over the whole RAM.
    use super::*;
    use hud::stream_draw as draw;

    const BLOCK: u32 = 0x8010_0000;
    const OT: u32 = 0x8011_0000;
    const PRIMS: u32 = 0x8012_0000;
    const CONTEXT: u32 = 0x8013_0000;
    const GAMEPLAY: u32 = 0x8013_1000;
    const MODES: u32 = 0x800a_e7cc;
    const ICONS: u32 = 0x800a_e8a8;
    /// Initial slot word: its high byte must survive every addPrim.
    const OT_WORD: u32 = 0xab12_3456;

    /// A mode record (800AE7CC + 20 * mode).
    struct Record {
        width: u8,
        height: u8,
        per_page: u8,
        rgb: [u8; 3],
        semi: u8,
        x: u8,
        rows: [u16; 3],
        flags: u8,
    }

    const SAMPLE: Record = Record {
        width: 0x60,
        height: 0x0c,
        per_page: 5,
        rgb: [0x80, 0x70, 0x60],
        semi: 0,
        x: 0x20,
        rows: [0x30, 0x40, 0x50],
        flags: 0x10,
    };

    /// Block mode `mode` with line numbers `lines`, texture page base 0x108,
    /// the record of `mode`, and a primitive buffer full of 0xCD so that
    /// every untouched byte is checked too.
    fn machine(mode: u32, lines: [u8; 3], record: &Record) -> Machine {
        let mut m = Machine::default();
        m.sw(ac3_flight::GP + 0x304, BLOCK);
        m.sw(ac3_flight::GP + 0x2dc, CONTEXT);
        m.sw(ac3_flight::GP + 0x2d0, GAMEPLAY);
        m.sw(BLOCK, mode);
        for (i, &line) in lines.iter().enumerate() {
            m.sb(BLOCK + 4 + i as u32, line);
        }
        m.sh(BLOCK + 0x1c, 0x0108);
        m.sh(BLOCK + 0x1e, 0x1234);
        let at = MODES + 20 * mode;
        m.sb(at, record.width);
        m.sb(at + 1, record.height);
        m.sb(at + 2, record.per_page);
        for (i, &component) in record.rgb.iter().enumerate() {
            m.sb(at + 3 + i as u32, component);
        }
        m.sb(at + 6, record.semi);
        m.sb(at + 7, record.x);
        for (i, &y) in record.rows.iter().enumerate() {
            m.sh(at + 8 + 2 * i as u32, y);
        }
        m.sb(at + 0xf, record.flags);
        m.sw(OT, OT_WORD);
        for i in 0..0x30 {
            m.sw(PRIMS + 4 * i, 0xcdcd_cdcd);
        }
        m
    }

    /// addPrim on the expected image: the packet tag takes `length` and the
    /// slot's link; the slot keeps its high byte and links to the packet.
    fn link(e: &mut Machine, prim: u32, length: u8) {
        let slot = e.lw(OT);
        e.sw(prim, (u32::from(length) << 24) | (slot & 0x00ff_ffff));
        e.sw(OT, (slot & 0xff00_0000) | (prim & 0x00ff_ffff));
    }

    struct Sprite {
        code: u8,
        rgb: [u8; 3],
        x: u16,
        y: u16,
        u: u8,
        v: u8,
        clut: u16,
        w: u16,
        h: u16,
    }

    /// SPRT (length 4) at `at`.
    fn sprite(e: &mut Machine, at: u32, s: Sprite) {
        link(e, at, 4);
        for (i, &component) in s.rgb.iter().enumerate() {
            e.sb(at + 4 + i as u32, component);
        }
        e.sb(at + 7, s.code);
        e.sh(at + 8, s.x);
        e.sh(at + 0xa, s.y);
        e.sb(at + 0xc, s.u);
        e.sb(at + 0xd, s.v);
        e.sh(at + 0xe, s.clut);
        e.sh(at + 0x10, s.w);
        e.sh(at + 0x12, s.h);
    }

    /// DR_TPAGE (length 1) at `at`.
    fn tpage(e: &mut Machine, at: u32, word: u32) {
        link(e, at, 1);
        e.sw(at + 4, word);
    }

    /// POLY_FT4 (length 9) of the SAMPLE geometry for row `y`, texture row
    /// `v`: (x, y)..(x + 0x60, y + 2 * 0x0C - 1), uv (0, v)..(0x60, v + 0x0C).
    fn quad(e: &mut Machine, at: u32, code: u8, y: u16, v: u8, clut: u16, page: u16) {
        link(e, at, 9);
        for (i, &component) in SAMPLE.rgb.iter().enumerate() {
            e.sb(at + 4 + i as u32, component);
        }
        e.sb(at + 7, code);
        let corners = [
            (0x20, y, 0, v),
            (0x80, y, 0x60, v),
            (0x20, y + 0x17, 0, v + 0x0c),
            (0x80, y + 0x17, 0x60, v + 0x0c),
        ];
        for (k, (x, y, u, v)) in corners.into_iter().enumerate() {
            let vertex = at + 8 + 8 * k as u32;
            e.sh(vertex, x);
            e.sh(vertex + 2, y);
            e.sb(vertex + 4, u);
            e.sb(vertex + 5, v);
        }
        e.sh(at + 0xe, clut);
        e.sh(at + 0x16, page);
    }

    /// Whole-RAM comparison naming the first differing address.
    fn assert_ram(m: &Machine, e: &Machine, case: &str) {
        if let Some(at) = (0..m.ram().len()).find(|&i| m.ram()[i] != e.ram()[i]) {
            panic!(
                "{case}: {:#010x} is {:#04x}, expected {:#04x}",
                0x8000_0000 + at as u32,
                m.ram()[at],
                e.ram()[at]
            );
        }
        assert!(m.scratch() == e.scratch(), "{case}: scratchpad");
    }

    /// Run `draw` on `m`, require `result` and the expected image `e`.
    fn run(
        mut m: Machine,
        e: &Machine,
        draw: fn(&mut Machine, u32, u32) -> u32,
        result: u32,
        case: &str,
    ) {
        let sp = m.sp;
        assert_eq!(draw(&mut m, OT, PRIMS), result, "{case}");
        assert!(m.take_fault().is_ok(), "{case}");
        assert_eq!(m.sp, sp, "{case}");
        assert_ram(&m, e, case);
    }

    /// 8002CF08: no line at +4 draws nothing (lines +5/+6 are ignored);
    /// otherwise the copy packet takes source y 10 * (n - 1) and target y
    /// 0x1D6 only for buffer 1.
    #[test]
    fn copy_line_2cf08() {
        let m = machine(9, [0, 5, 5], &SAMPLE);
        let e = m.clone();
        run(m, &e, draw::copy_line_2cf08, PRIMS, "no line");
        for (line, source, buffer, target) in [
            (3, 0x0014_0240, 0, 0x00d6_0036),
            (3, 0x0014_0240, 1, 0x01d6_0036),
            (0xff, 0x09ec_0240, 2, 0x00d6_0036),
        ] {
            let mut m = machine(9, [line, 0, 0], &SAMPLE);
            m.sw(0x800b_abfc, buffer);
            let mut e = m.clone();
            for (offset, word) in [
                (4, 0x0100_0000),
                (8, 0x8000_0000),
                (0xc, source),
                (0x10, target),
                (0x14, 0x000a_0180),
            ] {
                e.sw(PRIMS + offset, word);
            }
            link(&mut e, PRIMS, 5);
            run(
                m,
                &e,
                draw::copy_line_2cf08,
                PRIMS + 0x18,
                &format!("line {line} buffer {buffer}"),
            );
        }
    }

    /// 8002CCE4: line 0 is skipped; line 7 = page 1 row 1 (v 12, CLUT
    /// 0x4A56, page 0x108); line 43 = page 8 row 2 (v 24, CLUT 0x4A16 as
    /// 8 & 7 = 0, page 0x108 + 4); line 12 = page 2 row 1 (CLUT 0x4A96, page
    /// 0x109); record +6 selects code 0x2E.
    #[test]
    fn quad_lines_2cce4() {
        for (lines, semi) in [([0, 7, 43], 0), ([12, 7, 43], 1)] {
            let record = Record { semi, ..SAMPLE };
            let m = machine(10, lines, &record);
            let mut e = m.clone();
            let code = if semi != 0 { 0x2e } else { 0x2c };
            let mut at = PRIMS;
            if lines[0] != 0 {
                quad(&mut e, at, code, 0x30, 12, 0x4a96, 0x109);
                at += 0x28;
            }
            quad(&mut e, at, code, 0x40, 12, 0x4a56, 0x108);
            quad(&mut e, at + 0x28, code, 0x50, 24, 0x4a16, 0x10c);
            let case = format!("{lines:?}");
            run(m, &e, draw::quad_lines_2cce4, at + 0x50, &case);
        }
        let m = machine(10, [0, 0, 0], &SAMPLE);
        let e = m.clone();
        run(m, &e, draw::quad_lines_2cce4, PRIMS, "no lines");
    }

    /// 8002CA48, mode 0: suppressed while `[context+0x2C]` is 2 and gameplay
    /// bit 0 is set; drawn otherwise. Lines 12 (page 2 row 1) and 43 (page 8
    /// row 2) with icon byte 3 (icon 2 at (0x84, 0x38)); line 1 without icon.
    #[test]
    fn sprite_lines_2ca48_mode0() {
        let mut m = machine(0, [12, 0, 43], &SAMPLE);
        m.sw(CONTEXT + 0x2c, 2);
        m.sw(GAMEPLAY, 1);
        let e = m.clone();
        run(m, &e, draw::sprite_lines_2ca48, PRIMS, "suppressed");

        let record = Record { semi: 1, ..SAMPLE };
        let mut m = machine(0, [12, 0, 43], &record);
        m.sw(CONTEXT + 0x2c, 2);
        m.sw(GAMEPLAY, 2);
        m.sb(ac3_flight::GP + 0x30a, 3);
        for (i, byte) in [0x40, 0x80, 0x20, 0x18].into_iter().enumerate() {
            m.sb(ICONS + 8 + i as u32, byte);
        }
        let mut e = m.clone();
        let line = |y, v, clut| Sprite {
            code: 0x66,
            rgb: SAMPLE.rgb,
            x: 0x20,
            y,
            u: 0,
            v,
            clut,
            w: 0x60,
            h: 0x0c,
        };
        sprite(&mut e, PRIMS, line(0x30, 12, 0x4a96));
        e.sh(BLOCK + 0x1e, 0x4a16);
        tpage(&mut e, PRIMS + 0x14, 0xe100_0309);
        sprite(&mut e, PRIMS + 0x1c, line(0x50, 24, 0x4a16));
        tpage(&mut e, PRIMS + 0x30, 0xe100_030c);
        let icon = Sprite {
            code: 0x66,
            rgb: SAMPLE.rgb,
            x: 0x84,
            y: 0x38,
            u: 0x40,
            v: 0x80,
            clut: 0x7fd8,
            w: 0x20,
            h: 0x18,
        };
        sprite(&mut e, PRIMS + 0x38, icon);
        tpage(&mut e, PRIMS + 0x4c, 0xe100_0236);
        let result = PRIMS + 0x54;
        run(m, &e, draw::sprite_lines_2ca48, result, "icon");

        let mut m = machine(0, [1, 0, 0], &SAMPLE);
        m.sw(CONTEXT + 0x2c, 1);
        m.sw(GAMEPLAY, 1);
        let mut e = m.clone();
        let first = Sprite {
            code: 0x64,
            rgb: SAMPLE.rgb,
            x: 0x20,
            y: 0x30,
            u: 0,
            v: 0,
            clut: 0x4a16,
            w: 0x60,
            h: 0x0c,
        };
        sprite(&mut e, PRIMS, first);
        e.sh(BLOCK + 0x1e, 0x4a16);
        tpage(&mut e, PRIMS + 0x14, 0xe100_0308);
        let result = PRIMS + 0x1c;
        run(m, &e, draw::sprite_lines_2ca48, result, "icon 0");
    }

    /// 8002CA48, other modes: mode 6 gives row 1 height 0x8A; record flag
    /// 0x10 clear means no icon; no line drawn means nothing at all (block
    /// +0x1E included); in mode 3 the icon sits at (x - 4, row 0 y - 6)
    /// (wrapping) and the page word is masked with 0x9FF (0xFFFE + 2).
    #[test]
    fn sprite_lines_2ca48() {
        let record = Record { flags: 0, ..SAMPLE };
        let mut m = machine(6, [5, 9, 0], &record);
        m.sb(ac3_flight::GP + 0x30a, 3);
        let mut e = m.clone();
        let line = |y, v, clut, h| Sprite {
            code: 0x64,
            rgb: SAMPLE.rgb,
            x: 0x20,
            y,
            u: 0,
            v,
            clut,
            w: 0x60,
            h,
        };
        sprite(&mut e, PRIMS, line(0x30, 0x30, 0x4a16, 0x0c));
        e.sh(BLOCK + 0x1e, 0x4a16);
        tpage(&mut e, PRIMS + 0x14, 0xe100_0308);
        sprite(&mut e, PRIMS + 0x1c, line(0x40, 0x24, 0x4a56, 0x8a));
        tpage(&mut e, PRIMS + 0x30, 0xe100_0308);
        let result = PRIMS + 0x38;
        run(m, &e, draw::sprite_lines_2ca48, result, "mode 6");

        let mut m = machine(3, [0, 0, 0], &SAMPLE);
        m.sb(ac3_flight::GP + 0x30a, 3);
        let e = m.clone();
        run(m, &e, draw::sprite_lines_2ca48, PRIMS, "no lines");

        let record = Record {
            x: 2,
            rows: [3, 0x40, 0x50],
            ..SAMPLE
        };
        let mut m = machine(3, [0, 0, 21], &record);
        m.sh(BLOCK + 0x1c, 0xfffe);
        m.sb(ac3_flight::GP + 0x30a, 1);
        for (i, byte) in [0x10, 0x20, 0x30, 0x40].into_iter().enumerate() {
            m.sb(ICONS + i as u32, byte);
        }
        let mut e = m.clone();
        let third = Sprite {
            code: 0x64,
            rgb: SAMPLE.rgb,
            x: 2,
            y: 0x50,
            u: 0,
            v: 0,
            clut: 0x4b16,
            w: 0x60,
            h: 0x0c,
        };
        sprite(&mut e, PRIMS, third);
        e.sh(BLOCK + 0x1e, 0x4a16);
        tpage(&mut e, PRIMS + 0x14, 0xe100_0200);
        let icon = Sprite {
            code: 0x64,
            rgb: SAMPLE.rgb,
            x: 0xfffe,
            y: 0xfffd,
            u: 0x10,
            v: 0x20,
            clut: 0x7fd8,
            w: 0x30,
            h: 0x40,
        };
        sprite(&mut e, PRIMS + 0x1c, icon);
        tpage(&mut e, PRIMS + 0x30, 0xe100_0236);
        let result = PRIMS + 0x38;
        run(m, &e, draw::sprite_lines_2ca48, result, "mode 3");
    }

    /// 8002D138 through the module dispatch: icon 5 (table +20) in mode 5 at
    /// (0x20 - 4, 0x30 - 6), semi-transparent code 0x66; the four drawing
    /// routines are claimed by address.
    #[test]
    fn icon_sprite_2d138() {
        let record = Record { semi: 1, ..SAMPLE };
        let mut m = machine(5, [0, 0, 0], &record);
        for (i, byte) in [1, 2, 3, 4].into_iter().enumerate() {
            m.sb(ICONS + 20 + i as u32, byte);
        }
        let mut e = m.clone();
        let icon = Sprite {
            code: 0x66,
            rgb: SAMPLE.rgb,
            x: 0x1c,
            y: 0x2a,
            u: 1,
            v: 2,
            clut: 0x7fd8,
            w: 3,
            h: 4,
        };
        sprite(&mut e, PRIMS, icon);
        tpage(&mut e, PRIMS + 0x14, 0xe100_0236);
        let sp = m.sp;
        assert_eq!(
            hud::dispatch(&mut m, 0x8002_d138, [OT, PRIMS, 5, 0]),
            Some(PRIMS + 0x1c)
        );
        assert!(m.take_fault().is_ok());
        assert_eq!(m.sp, sp);
        assert_ram(&m, &e, "icon 5");
        // Line-less blocks: each mode routine is claimed and returns `a1`.
        for target in [0x8002_cf08, 0x8002_cce4, 0x8002_ca48] {
            let mut m = machine(3, [0, 0, 0], &SAMPLE);
            assert_eq!(
                hud::dispatch(&mut m, target, [OT, PRIMS, 0, 0]),
                Some(PRIMS)
            );
            assert!(m.take_fault().is_ok());
        }
    }
}

mod marker_shapes_tests {
    //! 8003F54C and 8003F9A8 are reached by no capture yet. Each test runs a
    //! translation on a synthetic state and requires the whole RAM and the
    //! scratchpad to equal the same state plus the packets derived by hand
    //! from the instructions. The block is pre-filled with 0xA5, so a stray,
    //! missing or misplaced store shows.
    use super::*;
    use hud::marker_shapes;

    /// Marker list and block of [`marker_machine`].
    const LIST: u32 = 0x8010_0000;
    const BLOCK: u32 = 0x8011_0000;
    /// Synthetic screen and distance records of entity slot [`SLOT`].
    const RECORD: u32 = 0x8012_0000;
    const DISTANCE: u32 = 0x8012_0100;
    const SLOT: u32 = 37;

    type Shape = fn(&mut Machine, u32, u32);
    const SHAPES: [(u32, Shape); 2] = [
        (0x8003_f54c, marker_shapes::hexagon_3f54c),
        (0x8003_f9a8, marker_shapes::corner_ticks_3f9a8),
    ];

    /// [`marker_machine`] with the block filled with 0xA5, the two-point and
    /// three-point line counts `counts` (+8, +0xC) and slot [`SLOT`]'s screen
    /// record at (`x`, `y`) with half size `size` and distance flags `flags`.
    fn shape_machine(x: i16, y: i16, size: i16, flags: u32, counts: [u32; 2]) -> Machine {
        let mut m = marker_machine();
        m.write_bytes(BLOCK, &[0xa5; 0x1000]);
        m.sw(LIST + 8, counts[0]);
        m.sw(LIST + 0xc, counts[1]);
        m.sw(0x800c_4568 + SLOT * 4, RECORD);
        m.sw(0x800c_4c98 + SLOT * 4, DISTANCE);
        m.sh(RECORD + 6, size as u16);
        m.sh(RECORD + 8, x as u16);
        m.sh(RECORD + 0xa, y as u16);
        m.sw(DISTANCE, flags);
        m
    }

    /// Store `lines` (their points) as packets of `stride` bytes from
    /// `first`, each with code word `word`.
    fn expect_lines<const N: usize>(
        m: &mut Machine,
        first: u32,
        stride: u32,
        word: u32,
        lines: &[[[i16; 2]; N]],
    ) {
        for (i, points) in lines.iter().enumerate() {
            let at = first + stride * i as u32;
            m.sw(at + 4, word);
            for (k, &[x, y]) in points.iter().enumerate() {
                m.sh(at + 8 + 4 * k as u32, x as u16);
                m.sh(at + 0xa + 4 * k as u32, y as u16);
            }
        }
    }

    /// Require no fault and RAM and scratchpad equal to `expected`.
    fn assert_same(m: &mut Machine, expected: &Machine) {
        assert!(m.take_fault().is_ok());
        let first = m
            .ram()
            .iter()
            .zip(expected.ram())
            .position(|(a, b)| a != b)
            .map(|at| format!("{:#010x}", 0x8000_0000 + at));
        assert_eq!(first, None, "first differing RAM byte");
        assert!(m.scratch() == expected.scratch());
    }

    /// 8003F54C with distance flag 0x100 (and others): the hexagon outline
    /// after one existing three-point line. s = 32, s/2 = 16: x -42, -10,
    /// 22 (negative values truncate to halfwords), y 68, 84, 116, 132.
    #[test]
    fn hexagon_3f54c_outline() {
        let mut m = shape_machine(-10, 100, 32, 0x1100, [5, 1]);
        let mut expected = shape_machine(-10, 100, 32, 0x1100, [5, 1]);
        marker_shapes::hexagon_3f54c(&mut m, SLOT, 0x0040_ff40);
        let lines = [
            [[-42, 84], [-10, 68], [22, 84]],
            [[-42, 84], [-42, 116], [-10, 132]],
            [[22, 84], [22, 116], [-10, 132]],
        ];
        expect_lines(
            &mut expected,
            BLOCK + 0x408 + 0x18,
            0x18,
            0x4a40_ff40,
            &lines,
        );
        expected.sw(LIST + 0xc, 4);
        assert_same(&mut m, &expected);
    }

    /// 8003F54C without flag 0x100: six vertex brackets after two existing
    /// lines. s = 47: s/2 = 23, s/4 = 11, s/8 = 5, (9 * 5) >> 2 = 11; box
    /// x 113, 160, 207; y 53, 77, 123, 147.
    #[test]
    fn hexagon_3f54c_brackets() {
        let mut m = shape_machine(160, 100, 47, 0x20, [0, 2]);
        let mut expected = shape_machine(160, 100, 47, 0x20, [0, 2]);
        marker_shapes::hexagon_3f54c(&mut m, SLOT, 0xff);
        let lines = [
            [[149, 58], [160, 53], [171, 58]],
            [[113, 88], [113, 77], [124, 72]],
            [[113, 112], [113, 123], [124, 128]],
            [[149, 142], [160, 147], [171, 142]],
            [[207, 88], [207, 77], [196, 72]],
            [[207, 112], [207, 123], [196, 128]],
        ];
        expect_lines(
            &mut expected,
            BLOCK + 0x408 + 2 * 0x18,
            0x18,
            0x4a00_00ff,
            &lines,
        );
        expected.sw(LIST + 0xc, 8);
        assert_same(&mut m, &expected);
    }

    /// 8003F54C brackets for a negative half size: every shift is
    /// arithmetic. s = -20: s/2 = -10, s/4 = -5, s/8 = -3, -27 >> 2 = -7;
    /// box x 70, 50, 30; y 60, 50, 30, 20.
    #[test]
    fn hexagon_3f54c_negative_size() {
        let mut m = shape_machine(50, 40, -20, 0, [0, 0]);
        let mut expected = shape_machine(50, 40, -20, 0, [0, 0]);
        marker_shapes::hexagon_3f54c(&mut m, SLOT, 0xff00);
        let lines = [
            [[55, 57], [50, 60], [45, 57]],
            [[70, 43], [70, 50], [65, 53]],
            [[70, 37], [70, 30], [65, 27]],
            [[55, 23], [50, 20], [45, 23]],
            [[30, 43], [30, 50], [35, 53]],
            [[30, 37], [30, 30], [35, 27]],
        ];
        expect_lines(&mut expected, BLOCK + 0x408, 0x18, 0x4a00_ff00, &lines);
        expected.sw(LIST + 0xc, 6);
        assert_same(&mut m, &expected);
    }

    /// 8003F9A8 without distance flag 0x20: four corner ticks after three
    /// existing two-point lines. s = 32, s/4 = 8; box x -27..37, y -35..29.
    #[test]
    fn corner_ticks_3f9a8_plain() {
        let mut m = shape_machine(5, -3, 32, 0x1100, [3, 4]);
        let mut expected = shape_machine(5, -3, 32, 0x1100, [3, 4]);
        marker_shapes::corner_ticks_3f9a8(&mut m, SLOT, 0xff00);
        let lines = [
            [[-27, -35], [-19, -27]],
            [[37, -35], [29, -27]],
            [[-27, 29], [-19, 21]],
            [[37, 29], [29, 21]],
        ];
        expect_lines(
            &mut expected,
            BLOCK + 8 + 3 * 0x10,
            0x10,
            0x4200_ff00,
            &lines,
        );
        expected.sw(LIST + 8, 7);
        assert_same(&mut m, &expected);
    }

    /// 8003F9A8 with flag 0x20: corner ticks, cross ticks (s/8 = 4) and the
    /// centre dot (the record's x/y word in both points). s = 32, box
    /// x 128..192, y 68..132.
    #[test]
    fn corner_ticks_3f9a8_crosses() {
        let mut m = shape_machine(160, 100, 32, 0x20, [0, 0]);
        let mut expected = shape_machine(160, 100, 32, 0x20, [0, 0]);
        marker_shapes::corner_ticks_3f9a8(&mut m, SLOT, 0x0040_ff40);
        let lines = [
            [[128, 68], [136, 76]],
            [[192, 68], [184, 76]],
            [[128, 132], [136, 124]],
            [[192, 132], [184, 124]],
            [[124, 72], [132, 64]],
            [[188, 64], [196, 72]],
            [[124, 128], [132, 136]],
            [[188, 136], [196, 128]],
            [[160, 100], [160, 100]],
        ];
        expect_lines(&mut expected, BLOCK + 8, 0x10, 0x4240_ff40, &lines);
        expected.sw(LIST + 8, 9);
        assert_same(&mut m, &expected);
    }

    /// 8003F9A8 with flag 0x20 for a negative half size (arithmetic shifts:
    /// s = -20, s/4 = -5, s/8 = -3; box x 70..30, y 60..20) and color 0.
    #[test]
    fn corner_ticks_3f9a8_negative_size() {
        let mut m = shape_machine(50, 40, -20, 0x20, [0, 0]);
        let mut expected = shape_machine(50, 40, -20, 0x20, [0, 0]);
        marker_shapes::corner_ticks_3f9a8(&mut m, SLOT, 0);
        let lines = [
            [[70, 60], [65, 55]],
            [[30, 60], [35, 55]],
            [[70, 20], [65, 25]],
            [[30, 20], [35, 25]],
            [[73, 57], [67, 63]],
            [[33, 63], [27, 57]],
            [[73, 23], [67, 17]],
            [[33, 17], [27, 23]],
            [[50, 40], [50, 40]],
        ];
        expect_lines(&mut expected, BLOCK + 8, 0x10, 0x4200_0000, &lines);
        expected.sw(LIST + 8, 9);
        assert_same(&mut m, &expected);
    }

    /// Both shapes draw nothing for a slot without a screen record or
    /// without a distance record.
    #[test]
    fn marker_shapes_without_records() {
        for table in [0x800c_4568u32, 0x800c_4c98] {
            for (_, shape) in SHAPES {
                let mut m = shape_machine(160, 100, 32, 0x120, [1, 1]);
                m.sw(table + SLOT * 4, 0);
                let mut expected = shape_machine(160, 100, 32, 0x120, [1, 1]);
                expected.sw(table + SLOT * 4, 0);
                shape(&mut m, SLOT, 0xff);
                assert_same(&mut m, &expected);
            }
        }
    }

    /// Both addresses are claimed (a0 slot, a1 color, v0 0), through
    /// `hud::dispatch` and the crate-wide `dispatch::call`.
    #[test]
    fn marker_shapes_dispatch() {
        for (target, shape) in SHAPES {
            let mut expected = shape_machine(160, 100, 32, 0x120, [0, 0]);
            shape(&mut expected, SLOT, 0xff);
            assert!(expected.lw(LIST + 8) + expected.lw(LIST + 0xc) > 0);
            let mut m = shape_machine(160, 100, 32, 0x120, [0, 0]);
            assert_eq!(hud::dispatch(&mut m, target, [SLOT, 0xff, 0, 0]), Some(0));
            assert_same(&mut m, &expected);
            let mut m = shape_machine(160, 100, 32, 0x120, [0, 0]);
            let v0 = ac3_flight::dispatch::call(&mut m, target, [SLOT, 0xff, 0, 0]);
            assert_eq!(v0, 0);
            assert_same(&mut m, &expected);
        }
        let mut m = marker_machine();
        assert_eq!(marker_shapes::dispatch(&mut m, 0x8003_f28c, [0; 4]), None);
    }
}

mod tree_41ff0_tests {
    //! No capture reaches the 80041FF0 guidance tree yet: every path runs on
    //! synthetic states with expected packets derived from the instructions
    //! (the gate screen points from RotTransPers of the expected vertices on
    //! a separate GTE). rcos (800A4698), SetMulMatrix (800A4ED8) and 80041DEC
    //! are not translated yet: their `Untranslated` faults are the only ones
    //! accepted, and the states are chosen so that their results do not
    //! matter (rsin/rcos tables zero where read, identity matrices).
    use super::*;
    use ac3_flight::{Fault, GP, sdk, shared::SINE_TABLE};
    use ac3_gte::ctrl;
    use hud::tree_41ff0 as t;

    const SET: u32 = 0x8010_0000;
    const BUFFER0: u32 = 0x8010_1000;
    const BUFFER: u32 = 0x8010_4000;
    const GAMEPLAY: u32 = 0x8010_8000;
    const CONTEXT: u32 = 0x8010_9000;
    const ENTITIES: u32 = 0x8010_a000;
    const PLAYER: u32 = 0x8010_b000;
    const OWN: u32 = 0x8010_c000;
    const POINT: u32 = 0x8010_d000;
    const VIEW: u32 = 0x8010_e000;
    const PAD: u32 = 0x8010_f000;
    const SLOT: u32 = 0x8011_0000;
    const RCOS: u32 = 0x800a_4698;
    const SET_MUL_MATRIX: u32 = 0x800a_4ed8;
    const MESSAGE: u32 = 0x8004_1dec;
    /// 800B4444: the rsin/rcos quarter-wave table.
    const RSIN_TABLE: u32 = 0x800b_4444;

    /// The executable's gate tables 800AFEFC..800AFF2B (left x, right x,
    /// bracket y, bracket z, chevron x, chevron y; four halfwords each).
    const GATES: [i16; 24] = [
        -0x96, -0x8c, -0x96, 0, 0x96, 0x8c, 0x96, 0, -0x28, -0x32, -0x3c, 0, 0xa, 0, 0xa, 0, 0,
        -0x28, 0x28, 0, 0, -0xa, -0xa, -0xa,
    ];
    /// The executable's arrow shapes 800AFF2C..800AFF4B.
    const SHAPES: [[i8; 8]; 4] = [
        [10, -5, 10, 5, 10, -10, 10, 10],
        [-10, -5, -10, 5, -10, -10, -10, 10],
        [-5, 10, 5, 10, -10, 10, 10, 10],
        [-5, -10, 5, -10, -10, -10, 10, -10],
    ];

    fn line3(i: u32) -> u32 {
        BUFFER + i * 0x18
    }
    fn line2(i: u32) -> u32 {
        BUFFER + 0xc00 + i * 0x10
    }
    fn poly3(i: u32) -> u32 {
        BUFFER + 0x1400 + i * 0x14
    }
    /// (LINE_F3, LINE_F2, POLY_F3) counts of the set.
    fn counts(m: &mut Machine) -> [u32; 3] {
        [m.lw(SET + 8), m.lw(SET + 0xc), m.lw(SET + 0x10)]
    }
    /// LINE_F2 `i`: color bytes and (x0, y0, x1, y1).
    fn line(m: &mut Machine, i: u32) -> ([u8; 3], [u16; 4]) {
        let at = line2(i);
        let rgb = [m.lbu(at + 4), m.lbu(at + 5), m.lbu(at + 6)];
        (rgb, [8, 0xa, 0xc, 0xe].map(|offset| m.lhu(at + offset)))
    }

    /// Packet set (buffer 1 selected), gameplay (counter 1), context
    /// (buffer index 1), entity table, pad word, view block and the
    /// executable's gate and arrow tables.
    fn machine() -> Machine {
        let mut m = Machine::default();
        m.sw(GP + 0x45c, SET);
        m.sw(SET, BUFFER0);
        m.sw(SET + 4, BUFFER);
        m.sw(GP + 0x460, BUFFER);
        m.sw(GP + 0x2d0, GAMEPLAY);
        m.sw(GP + 0x2dc, CONTEXT);
        m.sw(CONTEXT + 4, 1);
        m.sw(GP + 0x2d8, ENTITIES);
        m.sw(ENTITIES, PLAYER);
        m.sw(GAMEPLAY + 0xc, 1);
        m.sw(GAMEPLAY + 0x10, SLOT);
        m.sw(GAMEPLAY + 0x28, OWN);
        m.sw(GP + 0x528, PAD);
        m.sw(VIEW + 0x38, POINT);
        for (i, value) in GATES.into_iter().enumerate() {
            m.sh(t::LEFT_X + 2 * i as u32, value as u16);
        }
        for (i, value) in SHAPES.into_iter().flatten().enumerate() {
            m.sb(t::ARROWS + i as u32, value as u8);
        }
        m
    }

    /// Arrow `shape` at (`x`, `y`) as POLY_F3 `poly` (head) and LINE_F2
    /// `bar`.
    fn expect_arrow(m: &mut Machine, poly: u32, bar: u32, shape: usize, x: u16, y: u16) {
        let d = SHAPES[shape].map(|value| value as u16);
        let at = poly3(poly);
        let head = [8, 0xa, 0xc, 0xe, 0x10, 0x12].map(|offset| m.lhu(at + offset));
        let tip = [x, y];
        let corners = [
            x.wrapping_add(d[0]),
            y.wrapping_add(d[1]),
            x.wrapping_add(d[2]),
            y.wrapping_add(d[3]),
        ];
        assert_eq!(head[..2], tip, "arrow {shape} tip");
        assert_eq!(head[2..], corners, "arrow {shape} head");
        let ends = [
            x.wrapping_add(d[4]),
            y.wrapping_add(d[5]),
            x.wrapping_add(d[6]),
            y.wrapping_add(d[7]),
        ];
        assert_eq!(line(m, bar), ([0, 0xff, 0], ends), "arrow {shape} bar");
    }

    /// Whether 80041DEC ran: `Some(true)` for its `Untranslated` fault while
    /// it is a stub, else its message bytes (gp+0x454 = 1, gp+0x455 = `id` +
    /// 0xC). `None` when rcos or SetMulMatrix, called before it, is the first
    /// fault. Any other fault fails.
    fn message(m: &mut Machine, id: u32) -> Option<bool> {
        match m.take_fault() {
            Err(Fault::Untranslated { address: MESSAGE }) => Some(true),
            Err(Fault::Untranslated {
                address: RCOS | SET_MUL_MATRIX,
            }) => None,
            Err(fault) => panic!("{fault}"),
            Ok(()) => {
                let sent = m.lbu(GP + 0x454) == 1;
                if sent {
                    assert_eq!(u32::from(m.lbu(GP + 0x455)), id + 0xc);
                }
                Some(sent)
            }
        }
    }

    #[test]
    fn begin_420b4() {
        let mut m = machine();
        for offset in [8, 0xc, 0x10] {
            m.sw(SET + offset, 7);
        }
        m.sw(GP + 0x460, 0);
        t::begin_420b4(&mut m);
        assert_eq!(counts(&mut m), [0; 3]);
        assert_eq!(m.lw(GP + 0x460), BUFFER);
        m.sw(CONTEXT + 4, 0);
        t::begin_420b4(&mut m);
        assert_eq!(m.lw(GP + 0x460), BUFFER0);
        assert!(m.take_fault().is_ok());
    }

    #[test]
    fn link_420ec() {
        let mut m = machine();
        m.sw(SLOT, 0x0012_3456);
        t::link_420ec(&mut m);
        assert_eq!(m.lw(SLOT), 0x0012_3456, "nothing counted");
        m.sw(SET + 8, 2);
        m.sw(SET + 0xc, 1);
        m.sw(SET + 0x10, 1);
        let packets = [(line3(0), 5), (line3(1), 5), (line2(0), 3), (poly3(0), 4)];
        for (at, length) in packets {
            m.sw(at, (length << 24) | 0x00ff_ffff);
        }
        t::link_420ec(&mut m);
        let mut link = 0x0012_3456;
        for (at, length) in packets {
            assert_eq!(m.lw(at), (length << 24) | link, "{at:#010x}");
            link = at & 0x00ff_ffff;
        }
        assert_eq!(m.lw(SLOT), link);
        assert!(m.take_fault().is_ok());
    }

    #[test]
    fn gate_432c4() {
        let mut m = machine();
        m.sw(SET + 8, 3);
        m.sw(GAMEPLAY + 0xc, 0x1d);
        let at = line3(3);
        m.sw(at + 4, 0x48ab_cdef);
        t::gate_432c4(&mut m, 0x0010_0020, 0x0030_0040, 0x0050_0060, 5);
        assert_eq!(m.lw(at + 4), 0x4800_2000, "green ((0x1D + 5) & 0xF) << 4");
        let points = [8, 0xc, 0x10].map(|offset| m.lw(at + offset));
        assert_eq!(points, [0x0010_0020, 0x0030_0040, 0x0050_0060]);
        assert_eq!(counts(&mut m), [4, 0, 0]);
        assert!(m.take_fault().is_ok());
    }

    #[test]
    fn chevron_4332c() {
        let mut m = machine();
        m.sw(SET + 8, 1);
        m.sw(line3(1) + 4, 0x48ab_cdef);
        t::chevron_4332c(&mut m, 0x11, 0x22, 0x33, 0x44);
        for (i, middle) in [(1, 0x22), (2, 0x33)] {
            let at = line3(i);
            assert_eq!(m.lw(at + 4) & 0x00ff_ffff, 0xff00, "green");
            let points = [8, 0xc, 0x10].map(|offset| m.lw(at + offset));
            assert_eq!(points, [0x11, middle, 0x44]);
        }
        assert_eq!(counts(&mut m), [3, 0, 0]);
        assert!(m.take_fault().is_ok());
    }

    #[test]
    fn arrow_433b4() {
        let mut m = machine();
        m.sw(SET + 0xc, 2);
        m.sw(SET + 0x10, 1);
        let places = [(0x50, 0x78), (0xf0, 0x78), (0xa0, 0x3c), (0, 0)];
        for (shape, (x, y)) in places.into_iter().enumerate() {
            let index = shape as u32;
            t::arrow_433b4(&mut m, index, x, y);
            expect_arrow(&mut m, 1 + index, 2 + index, shape, x as u16, y as u16);
        }
        assert_eq!(counts(&mut m), [0, 6, 5]);
        assert!(m.take_fault().is_ok());
    }

    /// One 800430B0 case: player roll and pitch halfwords, an optional sine
    /// table entry (index, value), the expected arrows and the bar.
    struct Attitude {
        roll: i16,
        pitch: u16,
        sine: Option<(u32, u16)>,
        arrows: &'static [(usize, u16, u16)],
        rgb: [u8; 3],
        y: u16,
    }

    #[test]
    fn attitude_430b0() {
        const GREEN: [u8; 3] = [0, 0x80, 0];
        const RED: [u8; 3] = [0x80, 0, 0];
        let cases = [
            // Level at the 0x71C reference pitch.
            Attitude {
                roll: 0,
                pitch: 0x71c,
                sine: None,
                arrows: &[],
                rgb: GREEN,
                y: 0x78,
            },
            // Roll 0x1C8, error 0x200: sin(-0x200) = 0xF080 (high byte -0x10).
            Attitude {
                roll: 0x1c8,
                pitch: 0x51c,
                sine: Some((0x3e00, 0xf080)),
                arrows: &[(2, 0x50, 0x78), (3, 0xf0, 0x78), (2, 0xa0, 0x3c)],
                rgb: RED,
                y: 0x68,
            },
            // Roll -0x1C8, error -0x200: sin(0x200) = 0x0FFF (high byte 0xF).
            Attitude {
                roll: -0x1c8,
                pitch: 0x91c,
                sine: Some((0x200, 0x0fff)),
                arrows: &[(3, 0x50, 0x78), (2, 0xf0, 0x78), (3, 0xa0, 0xb4)],
                rgb: RED,
                y: 0x87,
            },
            // Just inside: roll 0x1C7, error 0xE3.
            Attitude {
                roll: 0x1c7,
                pitch: 0x639,
                sine: None,
                arrows: &[],
                rgb: GREEN,
                y: 0x78,
            },
            // Roll -0x1C7 (inside), error -0xE4: down arrow, red.
            Attitude {
                roll: -0x1c7,
                pitch: 0x800,
                sine: None,
                arrows: &[(3, 0xa0, 0xb4)],
                rgb: RED,
                y: 0x78,
            },
        ];
        for case in cases {
            let mut m = machine();
            m.sh(PLAYER + 0x1c, case.roll as u16);
            m.sh(PLAYER + 0x18, case.pitch);
            if let Some((index, value)) = case.sine {
                m.sh(SINE_TABLE + index * 2, value);
            }
            t::attitude_430b0(&mut m);
            let n = case.arrows.len() as u32;
            assert_eq!(counts(&mut m), [0, n + 4, n], "roll {:#x}", case.roll);
            for (i, &(shape, x, y)) in case.arrows.iter().enumerate() {
                expect_arrow(&mut m, i as u32, i as u32, shape, x, y);
            }
            let y = case.y;
            assert_eq!(line(&mut m, n), (case.rgb, [0x8c, y, 0xb4, y]));
            assert_eq!(
                line(&mut m, n + 1),
                ([0, 0x40, 0], [0x8c, 0x78, 0xb4, 0x78])
            );
            assert_eq!(
                line(&mut m, n + 2),
                ([0, 0x20, 0], [0x8c, 0x73, 0xb4, 0x73])
            );
            assert_eq!(
                line(&mut m, n + 3),
                ([0, 0x20, 0], [0x8c, 0x7d, 0xb4, 0x7d])
            );
            assert!(m.take_fault().is_ok());
        }
    }

    /// The fixed reference diamond of 80042A3C (LINE_F2 6..9 of a run).
    const DIAMOND: [[u16; 4]; 4] = [
        [0xa0, 0x9c, 0xb8, 0xb4],
        [0xb8, 0xb4, 0xa0, 0xcc],
        [0xa0, 0xcc, 0x88, 0xb4],
        [0x88, 0xb4, 0xa0, 0x9c],
    ];

    /// The ten deviation lines of 80042A3C from LINE_F2 `first`: lateral
    /// line at `x`, vertical line at `y`, and the diamond grown by `dz`.
    fn expect_diamond(m: &mut Machine, first: u32, x: u16, y: u16, dz: i16, rgb: [[u8; 3]; 3]) {
        assert_eq!(line(m, first), (rgb[0], [x, 0x9c, x, 0xcc]));
        assert_eq!(line(m, first + 1), (rgb[1], [0x88, y, 0xb8, y]));
        let dz = dz as u16;
        let top = 0x9c_u16.wrapping_sub(dz);
        let right = 0xb8_u16.wrapping_add(dz);
        let bottom = 0xcc_u16.wrapping_add(dz);
        let left = 0x88_u16.wrapping_sub(dz);
        let grown = [
            [0xa0, top, right, 0xb4],
            [right, 0xb4, 0xa0, bottom],
            [0xa0, bottom, left, 0xb4],
            [left, 0xb4, 0xa0, top],
        ];
        for (i, xy) in grown.into_iter().chain(DIAMOND).enumerate() {
            assert_eq!(
                line(m, first + 2 + i as u32),
                (rgb[2], xy),
                "diamond line {i}"
            );
        }
    }

    const GREEN: [u8; 3] = [0, 0xff, 0];
    const BLUE: [u8; 3] = [0, 0, 0xff];

    #[test]
    fn alignment_42a3c_buttons() {
        let mut m = machine();
        m.sw(PAD, 0x80);
        m.sw(POINT + 0x18, 0x3000);
        m.sh(PLAYER + 0x1a, 0x500);
        assert_eq!(t::alignment_42a3c(&mut m, VIEW), 0);
        assert_eq!(counts(&mut m), [0; 3], "pad 0x80: nothing drawn");
        assert!(m.take_fault().is_ok());
    }

    /// Far, turn left (message 0xD at phase 0) and up (no message at this
    /// phase); offsets +0x1C 100, +0x20 5, +0x24 -300 (after `>> 12`).
    #[test]
    fn alignment_42a3c_left_up() {
        let mut m = machine();
        m.sw(POINT + 0x18, 0x3000);
        m.sw(POINT + 0x1c, 0x6_4000);
        m.sw(POINT + 0x20, 0x5000);
        m.sw(POINT + 0x24, 0xffed_4000);
        m.sh(PLAYER + 0x1a, 0x500);
        m.sh(PLAYER + 0x18, 0xf700);
        m.sw(GAMEPLAY + 0xc, 0x80);
        assert_eq!(t::alignment_42a3c(&mut m, VIEW), 0);
        assert_eq!(message(&mut m, 0xd), Some(true));
        assert_eq!(counts(&mut m), [0, 12, 2]);
        expect_arrow(&mut m, 0, 0, 0, 0x50, 0x78);
        expect_arrow(&mut m, 1, 1, 2, 0xa0, 0x3c);
        // 0xA0 - 100 / 50, 0xB4 - 5 / 80, diamond grown by 300 / 150.
        expect_diamond(&mut m, 2, 0x9e, 0xb4, 2, [GREEN, BLUE, GREEN]);
    }

    /// Far, turn right (no message at phase 0x40) and down (message 0xB);
    /// all offsets 0 (blue).
    #[test]
    fn alignment_42a3c_right_down() {
        let mut m = machine();
        m.sw(POINT + 0x18, 0x2001);
        m.sw(POINT + 0x20, 0xffff_b000);
        m.sh(PLAYER + 0x1a, 0x3a00);
        m.sh(PLAYER + 0x18, 0x900);
        m.sw(GAMEPLAY + 0xc, 0x1c0);
        assert_eq!(t::alignment_42a3c(&mut m, VIEW), 0);
        assert_eq!(message(&mut m, 0xb), Some(true));
        assert_eq!(counts(&mut m), [0, 12, 2]);
        expect_arrow(&mut m, 0, 0, 1, 0xf0, 0x78);
        expect_arrow(&mut m, 1, 1, 3, 0xa0, 0xb4);
        expect_diamond(&mut m, 2, 0xa0, 0xb4, 0, [BLUE, BLUE, BLUE]);
    }

    /// Range 0x2000 (no arrows) with saturated offsets: lateral -0x40,
    /// vertical +0x40, diamond -0x18.
    #[test]
    fn alignment_42a3c_near_clamped() {
        let mut m = machine();
        m.sw(POINT + 0x18, 0x2000);
        m.sw(POINT + 0x1c, 0x7fff_f000);
        m.sw(POINT + 0x20, 0x8000_0000);
        m.sw(POINT + 0x24, 0x7fff_f000);
        m.sh(PLAYER + 0x1a, 0x1000);
        m.sw(GAMEPLAY + 0xc, 0);
        assert_eq!(t::alignment_42a3c(&mut m, VIEW), 0);
        assert!(m.take_fault().is_ok());
        assert_eq!(m.lbu(GP + 0x454), 0);
        assert_eq!(counts(&mut m), [0, 10, 0]);
        expect_diamond(&mut m, 0, 0x60, 0xf4, -0x18, [GREEN; 3]);
    }

    /// Far but inside the dead bands: turn ±0x100 and pitch offsets 1 / -1
    /// draw no arrow and request nothing (counter phase 0).
    #[test]
    fn alignment_42a3c_dead_band() {
        for (yaw, vertical, pitch) in [(0x100, 1, 0), (0x3f00, 0xffff_ffff, 0x900)] {
            let mut m = machine();
            m.sw(POINT + 0x18, 0x2001);
            m.sw(POINT + 0x20, vertical);
            m.sh(PLAYER + 0x1a, yaw);
            m.sh(PLAYER + 0x18, pitch);
            m.sw(GAMEPLAY + 0xc, 0);
            assert_eq!(t::alignment_42a3c(&mut m, VIEW), 0);
            assert!(m.take_fault().is_ok());
            assert_eq!(m.lbu(GP + 0x454), 0);
            assert_eq!(counts(&mut m), [0, 10, 0], "yaw {yaw:#x}");
        }
    }

    /// Guide point at `own + (0x20, -0x10, 0x2000) << 12` with zero angles,
    /// identity `[gameplay+0x4C]` and cos(0) = 0x4000 in the 80030534 sine
    /// table (both rotations are identities, so SetMulMatrix keeps the
    /// identity whether or not it is translated; rsin/rcos(0) read zero
    /// entries), H 0x100, center (160, 120), and the player at the point +
    /// (-5000, 1000, 0) << 12.
    fn approach_machine() -> Machine {
        let mut m = machine();
        for offset in [0, 8, 0x10] {
            m.sh(GAMEPLAY + 0x4c + offset, 0x1000);
        }
        m.sh(SINE_TABLE + 0x2000, 0x4000);
        m.gte.ctc2(ctrl::H, 0x100);
        m.gte.ctc2(ctrl::OFX, 160 << 16);
        m.gte.ctc2(ctrl::OFY, 120 << 16);
        let own = [0x1000u32, 0x2000, 0x3000];
        let offset = [0x2_0000u32, 0xffff_0000, 0x200_0000];
        let player = [5000u32.wrapping_neg() << 12, 1000 << 12, 0];
        let words = own.into_iter().zip(offset).zip(player);
        for (at, ((own, offset), player)) in (0..).step_by(4).zip(words) {
            let point = own.wrapping_add(offset);
            m.sw(OWN + 4 + at, own);
            m.sw(POINT + at, point);
            m.sw(PLAYER + 8 + at, point.wrapping_add(player));
        }
        m
    }

    /// Faults allowed in an 80042258 run: the untranslated callees only.
    fn approach_faults(m: &mut Machine) {
        if let Err(fault) = m.take_fault() {
            let allowed = [RCOS, SET_MUL_MATRIX, MESSAGE]
                .into_iter()
                .any(|address| fault == Fault::Untranslated { address });
            assert!(allowed, "{fault}");
        }
    }

    /// The gate packets of 80042258: every gate whose points all project
    /// without a FLAG error, compared with RotTransPers of the expected
    /// vertices on a separate GTE (identity rotation, the camera offset).
    #[test]
    fn approach_42258_gates() {
        let mut m = approach_machine();
        m.sh(POINT + 0x2e, 0x1000);
        m.sh(PLAYER + 0x18, 0x300);
        m.sw(POINT + 0x18, 0x1000);
        t::approach_42258(&mut m, VIEW);
        approach_faults(&mut m);
        let camera = [0x20i32, -0x10, 0x2000];
        let translation = [ctrl::TRX, ctrl::TRX + 1, ctrl::TRX + 2].map(|r| m.gte.cfc2(r) as i32);
        assert_eq!(translation, camera);
        let rotation = [
            ctrl::R11R12,
            ctrl::R13R21,
            ctrl::R22R23,
            ctrl::R31R32,
            ctrl::R33,
        ];
        let rotation = rotation.map(|r| m.gte.cfc2(r));
        assert_eq!(
            rotation,
            [0x1000, 0, 0x1000, 0, 0x1000],
            "point frame identity"
        );

        let mut model = Machine::default();
        for (i, word) in [0x1000, 0, 0x1000, 0, 0x1000].into_iter().enumerate() {
            model.gte.ctc2(ctrl::R11R12 + i, word);
        }
        for (i, value) in camera.into_iter().enumerate() {
            model.gte.ctc2(ctrl::TRX + i, value as u32);
        }
        model.gte.ctc2(ctrl::H, 0x100);
        model.gte.ctc2(ctrl::OFX, 160 << 16);
        model.gte.ctc2(ctrl::OFY, 120 << 16);
        let mut project = |v: [i32; 3]| {
            let at = 0x8010_0000;
            for (i, value) in v.into_iter().enumerate() {
                model.sh(at + 2 * i as u32, value as u16);
            }
            sdk::rot_trans_pers(&mut model, at, at + 8, at + 0xc, at + 0x10);
            (model.lw(at + 8), (model.lw(at + 0x10) as i32) >= 0)
        };
        let g = |i: usize| i32::from(GATES[i]);
        let mut expected: Vec<([u32; 3], u8)> = Vec::new();
        for xs in [0, 4] {
            for gate in 0..16i32 {
                let points: Vec<(u32, bool)> = (0..3)
                    .map(|p| {
                        project([
                            g(xs + p),
                            g(8 + p) - 8 * gate * gate,
                            g(12 + p) - 0x400 * gate,
                        ])
                    })
                    .collect();
                if points.iter().all(|&(_, ok)| ok) {
                    let green = (((1 + gate) & 0xf) << 4) as u8;
                    expected.push(([points[0].0, points[1].0, points[2].0], green));
                }
            }
        }
        for gate in 0..16i32 {
            let points: Vec<(u32, bool)> = (0..4)
                .map(|p| project([g(16 + p), g(20 + p) - 8 * gate * gate, -0x400 * gate]))
                .collect();
            if points.iter().all(|&(_, ok)| ok) {
                let [a, b, c, d] = [0, 1, 2, 3].map(|p| points[p].0);
                expected.push(([a, b, d], 0xff));
                expected.push(([a, c, d], 0xff));
            }
        }
        eprintln!("80042258: {} of 64 gate LINE_F3 drawn", expected.len());
        assert!(
            !expected.is_empty() && expected.len() < 64,
            "{}",
            expected.len()
        );
        assert_eq!(counts(&mut m), [expected.len() as u32, 6, 2]);
        for (i, (points, green)) in expected.into_iter().enumerate() {
            let at = line3(i as u32);
            let stored = [8, 0xc, 0x10].map(|offset| m.lw(at + offset));
            assert_eq!(stored, points, "LINE_F3 {i}");
            let rgb = [m.lbu(at + 4), m.lbu(at + 5), m.lbu(at + 6)];
            assert_eq!(rgb, [0, green, 0], "LINE_F3 {i}");
        }
    }

    /// Angle +0x12 0x100 (rsin 0x800, rcos reads a zero entry) moves the
    /// camera x by rsin >> 8, or >> 3 in mission 25.
    #[test]
    fn approach_42258_camera_offset() {
        for (mission, offset) in [(0x18, 0x800 >> 8), (0x19, 0x800 >> 3)] {
            let mut m = approach_machine();
            m.sh(POINT + 0x12, 0x100);
            m.sh(RSIN_TABLE + 0x200, 0x800);
            m.sb(t::MISSION, mission);
            t::approach_42258(&mut m, VIEW);
            approach_faults(&mut m);
            let translation =
                [ctrl::TRX, ctrl::TRX + 1, ctrl::TRX + 2].map(|r| m.gte.cfc2(r) as i32);
            assert_eq!(
                translation,
                [0x20 + offset, -0x10, 0x2000],
                "mission {mission:#x}"
            );
        }
    }

    /// One 80042258 cue case on [`approach_machine`]: heading `+0x2E`,
    /// frame counter, state `+0x34`, player pitch halfword, range `+0x18`,
    /// offset `+0x24`, view `+0x30`; the expected arrows and the message
    /// (`None`: no message).
    #[derive(Clone, Copy)]
    struct Cue {
        heading: u16,
        counter: u32,
        state: u32,
        pitch: u16,
        range: u32,
        depth: u32,
        altitude: u32,
        arrows: &'static [(usize, u16, u16)],
        message: Option<u32>,
    }
    const CUE: Cue = Cue {
        heading: 0,
        counter: 1,
        state: 0,
        pitch: 0,
        range: 0,
        depth: 0,
        altitude: 0x10_0000,
        arrows: &[],
        message: None,
    };

    /// Turn and pitch cues of 80042258, and the deviation cross: the player
    /// at (-5000, 1000) in the point's frame gives (0xA0 + 50, 0x78 - 10).
    #[test]
    fn approach_42258_cues() {
        let cases = [
            // Left (no message below 0x801), down (range 0x1000).
            Cue {
                heading: 0x1000,
                pitch: 0x300,
                range: 0x1000,
                arrows: &[(0, 0x50, 0x78), (3, 0xa0, 0xb4)],
                ..CUE
            },
            // Left from 0x801 with message 0xD; up with view +0x30 low.
            Cue {
                heading: 0x801,
                counter: 0,
                altitude: 0,
                arrows: &[(0, 0x50, 0x78), (2, 0xa0, 0x3c)],
                message: Some(0xd),
                ..CUE
            },
            Cue {
                heading: 0x800,
                counter: 0,
                arrows: &[(0, 0x50, 0x78)],
                ..CUE
            },
            Cue {
                heading: 0x200,
                counter: 0,
                ..CUE
            },
            // Right from 0x2000 with message 0xC; none from 0x3800.
            Cue {
                heading: 0x2000,
                counter: 0,
                arrows: &[(1, 0xf0, 0x78)],
                message: Some(0xc),
                ..CUE
            },
            Cue {
                heading: 0x3800,
                counter: 0,
                arrows: &[(1, 0xf0, 0x78)],
                ..CUE
            },
            Cue {
                heading: 0x3e00,
                counter: 0,
                ..CUE
            },
            // State 1: message 3 instead of the pitch cue.
            Cue {
                state: 1,
                counter: 0x80,
                pitch: 0x900,
                range: 0x1000,
                message: Some(3),
                ..CUE
            },
            // Error -0x300 with view +0x30 high: up, message 0xA.
            Cue {
                pitch: 0xfd00,
                counter: 0x40,
                arrows: &[(2, 0xa0, 0x3c)],
                message: Some(0xa),
                ..CUE
            },
            // Error 0x300 out of range 0x801..0x3FFF, view +0x30 0xFFFFF: up.
            Cue {
                pitch: 0x300,
                range: 0x4000,
                altitude: 0xf_ffff,
                arrows: &[(2, 0xa0, 0x3c)],
                ..CUE
            },
            Cue {
                pitch: 0x300,
                range: 0x800,
                ..CUE
            },
            Cue {
                pitch: 0x200,
                range: 0x1000,
                ..CUE
            },
            // Error 0x900 with +0x24 negative: down, message 0xB.
            Cue {
                pitch: 0x900,
                range: 0x801,
                depth: 0xffff_ffff,
                counter: 0x40,
                arrows: &[(3, 0xa0, 0xb4)],
                message: Some(0xb),
                ..CUE
            },
            Cue {
                pitch: 0x900,
                range: 0x3fff,
                counter: 0x40,
                arrows: &[(3, 0xa0, 0xb4)],
                ..CUE
            },
        ];
        for (number, case) in cases.into_iter().enumerate() {
            let mut m = approach_machine();
            m.sh(POINT + 0x2e, case.heading);
            m.sw(GAMEPLAY + 0xc, case.counter);
            m.sw(POINT + 0x34, case.state);
            m.sh(PLAYER + 0x18, case.pitch);
            m.sw(POINT + 0x18, case.range);
            m.sw(POINT + 0x24, case.depth);
            m.sw(VIEW + 0x30, case.altitude);
            t::approach_42258(&mut m, VIEW);
            let sent = message(&mut m, case.message.unwrap_or(0));
            assert_ne!(sent, Some(case.message.is_none()), "case {number}: message");
            let n = case.arrows.len() as u32;
            assert_eq!(counts(&mut m)[1..], [n + 4, n], "case {number}");
            for (i, &(shape, x, y)) in case.arrows.iter().enumerate() {
                expect_arrow(&mut m, i as u32, i as u32, shape, x, y);
            }
            assert_eq!(line(&mut m, n), (GREEN, [0xd2, 0x64, 0xd2, 0x8c]));
            assert_eq!(line(&mut m, n + 1), (GREEN, [0x8c, 0x6e, 0xb4, 0x6e]));
            let dim = [0, 0x40, 0];
            assert_eq!(line(&mut m, n + 2), (dim, [0xa0, 0x64, 0xa0, 0x8c]));
            assert_eq!(line(&mut m, n + 3), (dim, [0x8c, 0x78, 0xb4, 0x78]));
        }
    }

    /// Course kinds of mode 3 (record 800B010C): 0/1 approach, 2/3
    /// alignment, 4 attitude, 5..7 nothing; the buffer and counts are reset
    /// first and everything drawn is linked into the slot.
    #[test]
    fn guidance_41ff0() {
        for kind in 0..8u8 {
            let mut m = machine();
            m.sw(GP + 0x324, 3);
            m.sb(t::COURSES + 12, kind);
            m.sh(PLAYER + 0x18, 0x71c);
            for offset in [8, 0xc, 0x10] {
                m.sw(SET + offset, 9);
            }
            m.sw(GP + 0x460, 0);
            m.sw(SLOT, 0x0012_3456);
            for i in 0..12 {
                m.sw(line2(i), 0x0300_0000);
            }
            t::guidance_41ff0(&mut m, VIEW);
            approach_faults(&mut m);
            assert_eq!(m.lw(GP + 0x460), BUFFER);
            let [lines3, lines, polys] = counts(&mut m);
            let expected = match kind {
                // Pitch error 0x71C over 0x200 out of range: up arrow, cross.
                0 | 1 => [5, 1],
                2 | 3 => [10, 0],
                4 => [4, 0],
                _ => [0, 0],
            };
            assert_eq!([lines, polys], expected, "kind {kind}");
            if kind >= 2 {
                assert_eq!(lines3, 0);
                // Only LINE_F2: they are linked in order behind the slot.
                let mut link = 0x0012_3456;
                for i in 0..lines {
                    assert_eq!(m.lw(line2(i)), 0x0300_0000 | link, "kind {kind} line {i}");
                    link = line2(i) & 0x00ff_ffff;
                }
                assert_eq!(m.lw(SLOT), link, "kind {kind}");
            }
        }
        // Pad buttons 0x240: nothing drawn, nothing linked.
        for buttons in [0x40, 0x200] {
            let mut m = machine();
            m.sb(t::COURSES, 4);
            m.sw(PAD, buttons);
            m.sw(SLOT, 0x0012_3456);
            t::guidance_41ff0(&mut m, VIEW);
            assert!(m.take_fault().is_ok());
            assert_eq!(counts(&mut m), [0; 3]);
            assert_eq!(m.lw(SLOT), 0x0012_3456);
        }
    }

    /// Every routine of the module is claimed by original address.
    #[test]
    fn tree_41ff0_dispatch() {
        let view = [VIEW, 0, 0, 0];
        let targets = [
            (0x8004_1ff0, view),
            (0x8004_20b4, [0; 4]),
            (0x8004_20ec, [0; 4]),
            (0x8004_2258, view),
            (0x8004_2a3c, view),
            (0x8004_30b0, view),
            (0x8004_32c4, [1, 2, 3, 4]),
            (0x8004_332c, [1, 2, 3, 4]),
            (0x8004_33b4, [0, 0x50, 0x78, 0]),
        ];
        for (target, args) in targets {
            let mut m = machine();
            assert_eq!(hud::dispatch(&mut m, target, args), Some(0), "{target:08x}");
            approach_faults(&mut m);
        }
        let mut m = machine();
        let v0 = ac3_flight::dispatch::call(&mut m, 0x8004_33b4, [1, 0xf0, 0x78, 0]);
        assert_eq!(v0, 0);
        expect_arrow(&mut m, 0, 0, 1, 0xf0, 0x78);
        assert!(m.take_fault().is_ok());
    }
}

mod extras_tests {
    //! 8003DAB4, 8005C8F0, 80064DC4 and 800652A4 are not reached by any
    //! capture: synthetic states with expected values derived by hand from
    //! the instructions (projections against an independent GTE run).
    use super::*;

    use ac3_flight::GP;
    use ac3_flight::hud::extras;
    use ac3_gte::{Gte, ctrl, reg};

    /// 80010498: 1.14 sine table.
    const SINE: u32 = 0x8001_0498;
    /// 800B4C54: SquareRoot0 mantissa table (also used by 800827B4).
    const SQRT: u32 = 0x800b_4c54;
    /// 800C4B44: radar range.
    const RANGE: u32 = 0x800c_4b44;
    /// Radar basis in the scratchpad, as 8003C8D4 stores it.
    const BASIS: u32 = 0x1f80_0100;
    const HUD: u32 = 0x8013_0000;
    const AREA: u32 = 0x8014_0000;
    const GAMEPLAY: u32 = 0x8015_0000;
    const CAMERA: u32 = 0x8016_0000;
    const MARKER: u32 = 0x8017_0000;

    /// Addresses (RAM outside the guest stack 801FE000..80200000, then
    /// scratchpad) where `a` and `b` differ.
    fn differences(a: &Machine, b: &Machine) -> Vec<u32> {
        let ram = a.ram()[..0x1f_e000]
            .iter()
            .zip(&b.ram()[..0x1f_e000])
            .enumerate()
            .filter(|(_, (x, y))| x != y)
            .map(|(i, _)| 0x8000_0000 + i as u32);
        let scratch = a
            .scratch()
            .iter()
            .zip(b.scratch())
            .enumerate()
            .filter(|(_, (x, y))| x != y)
            .map(|(i, _)| 0x1f80_0000 + i as u32);
        ram.chain(scratch).collect()
    }

    /// Require RAM outside the stack and the scratchpad of `m` to equal
    /// `expected`.
    #[track_caller]
    fn assert_same(m: &Machine, expected: &Machine, what: &str) {
        let differ = differences(m, expected);
        assert!(
            differ.is_empty(),
            "{what}: differs at {:#x?}",
            &differ[..differ.len().min(16)]
        );
    }

    /// Radar marker state: radar basis at (0x1000, 0x2000) with yaw 0x100
    /// and the sin/cos (/4) words `sin`, `cos`; radar `range`; the marker
    /// six words at radar offset (`dx`, `dz`) from the basis (position units
    /// `>> 12`), y 0, heading 0x300; the camera at the marker minus (0, 0,
    /// `depth << 12`); identity rotation at gameplay +0x4C and gameplay +0xC
    /// = 12 (color phase 3); line angle offsets 0 / 0x1000 with shifts 10 /
    /// 9; sine entries 0x3E00 (-0x1000), 0x4E00 (0x2000), 0xE00 (0xC00),
    /// 0x1E00 (-0x800); SquareRoot0 entries 0 (0x1003), 0x69 (0x1A00), 0xB0
    /// (0x1F00); H 0x20, screen offset (160, 120). The HUD flag word starts
    /// at 0x10 and the area around the marker packets holds 0xEE bytes.
    fn marker_machine(sin: u32, cos: u32, range: u32, dx: i32, dz: i32, depth: i32) -> Machine {
        let mut m = Machine::default();
        m.set_gw(0x42c, BASIS);
        for (offset, word) in [
            (0, 0x1000),
            (4, 0x2000),
            (8, sin),
            (0xc, cos),
            (0x10, 0x100),
        ] {
            m.sw(BASIS + offset, word);
        }
        m.sw(RANGE, range);
        m.set_gw(0x424, HUD);
        m.sw(HUD + 0x18, 0x10);
        m.set_gw(0x428, AREA);
        for offset in (0xc40..0xcc0).step_by(4) {
            m.sw(AREA + offset, 0xeeee_eeee);
        }
        m.set_gw(0x2d0, GAMEPLAY);
        m.sw(GAMEPLAY + 0xc, 12);
        m.sw(GAMEPLAY + 0x28, CAMERA);
        for offset in [0x4c, 0x54, 0x5c] {
            m.sw(GAMEPLAY + offset, 0x1000);
        }
        let x = ((0x1000 + dx) << 12) as u32;
        let z = ((0x2000 - dz) << 12) as u32;
        m.sw(MARKER, x);
        m.sw(MARKER + 8, z);
        m.sh(MARKER + 0x12, 0x300);
        m.sw(CAMERA + 4, x);
        m.sw(CAMERA + 0xc, z.wrapping_sub((depth << 12) as u32));
        m.sh(GP + 0x1de, 0x1000);
        m.sb(GP + 0x1e0, 10);
        m.sb(GP + 0x1e1, 9);
        for (index, value) in [
            (0x3e00u32, -0x1000i16),
            (0x4e00, 0x2000),
            (0xe00, 0xc00),
            (0x1e00, -0x800),
        ] {
            m.sh(SINE + index * 2, value as u16);
        }
        for (index, value) in [(0u32, 0x1003u16), (0x69, 0x1a00), (0xb0, 0x1f00)] {
            m.sh(SQRT + index * 2, value);
        }
        m.gte.ctc2(ctrl::H, 0x20);
        m.gte.ctc2(ctrl::OFX, 160 << 16);
        m.gte.ctc2(ctrl::OFY, 120 << 16);
        m
    }

    /// The two red radar lines from (`x`, `y`): the first ends at (x - 4,
    /// y + 8) (sine -0x1000 >> 10, cosine 0x2000 >> 10 at angle 0x3E00), the
    /// second at (x + 6, y - 4) (0xC00 >> 9, -0x800 >> 9 at angle 0xE00).
    fn expect_lines(e: &mut Machine, x: u16, y: u16) {
        for (i, (x1, y1)) in [(x - 4, y + 8), (x + 6, y - 4)].into_iter().enumerate() {
            let line = AREA + 0xc54 + 0x10 * i as u32;
            e.sb(line + 4, 0xff);
            e.sb(line + 5, 0);
            e.sb(line + 6, 0);
            for (offset, value) in [(8, x), (0xa, y), (0xc, x1), (0xe, y1)] {
                e.sh(line + offset, value);
            }
        }
    }

    /// The three triangles of size `size` above the screen centre (160,
    /// 120): triangle `i` has its tip `(2i + 1) * size` and its base `(2i +
    /// 2) * size` above the point; colors (phase 3 + i) << 6: red 0xC0 /
    /// 0x00 / 0x40, green 0x60 / 0 / 0x20, blue 0.
    fn expect_triangles(e: &mut Machine, size: u16) {
        for (i, (red, green)) in [(0xc0u8, 0x60u8), (0, 0), (0x40, 0x20)]
            .into_iter()
            .enumerate()
        {
            let triangle = AREA + 0xc74 + 0x14 * i as u32;
            let n = i as u16;
            e.sb(triangle + 4, red);
            e.sb(triangle + 5, green);
            e.sb(triangle + 6, 0);
            let base = 120 - (2 * n + 2) * size;
            for (offset, value) in [
                (8, 160),
                (0xa, 120 - (2 * n + 1) * size),
                (0xc, 160 + size),
                (0xe, base),
                (0x10, 160 - size),
                (0x12, base),
            ] {
                e.sh(triangle + offset, value);
            }
        }
    }

    /// Run 8003DAB4 on `m` and require v0 0, no fault and the frame popped.
    fn run_marker(m: &mut Machine) {
        let sp = m.sp;
        assert_eq!(extras::radar_marker_3dab4(m, MARKER), 0);
        assert!(m.take_fault().is_ok());
        assert_eq!(m.sp, sp);
    }

    /// Identity basis, offset (0x3E0, 0): length SquareRoot0(0xF0400) =
    /// 0x1F00 >> 3 = 0x3E0 <= range 0x400; radar point (0x3E0 << 5) / 0x400
    /// = 31 -> x 0x4B, y 0x2C. Depth 0x7C00: distance (800827B4) 0x1F00 << 2
    /// = 0x7C00, size clamp((0x8000 - 0x7C00) >> 10) = 2; the point
    /// projects to the screen centre. Flag word: 1, then | 2.
    #[test]
    fn radar_marker_3dab4() {
        let mut m = marker_machine(0, 0x1000, 0x400, 0x3e0, 0, 0x7c00);
        let mut expected = m.clone();
        expect_lines(&mut expected, 0x4b, 0x2c);
        expect_triangles(&mut expected, 2);
        expected.sw(HUD + 0x18, 3);
        run_marker(&mut m);
        assert_same(&m, &expected, "");
        // The stack frame (0x58 bytes) holds the SVECTOR and screen point.
        let frame = m.sp - 0x58;
        assert_eq!(m.lw(frame + 0x30), 0, "x, y");
        assert_eq!(m.lhu(frame + 0x34), 0x7c00, "z");
        assert_eq!(m.lw(frame + 0x38), (120 << 16) | 160);
        assert!((m.lw(frame + 0x44) as i32) >= 0, "GTE flag");

        // The same through the dispatch.
        let mut again = marker_machine(0, 0x1000, 0x400, 0x3e0, 0, 0x7c00);
        assert_eq!(
            hud::dispatch(&mut again, 0x8003_dab4, [MARKER, 0, 0, 0]),
            Some(0)
        );
        assert_same(&again, &expected, "");
    }

    /// Triangle sizes: distance 0x6800 (depth 0x6800, mantissa 0x1A00 << 2)
    /// gives 0x1800 >> 10 = 6; distance 0x40 (depth 0x40: 0x1003 << 6 >>
    /// 12) gives 31, clamped to 8.
    #[test]
    fn radar_marker_3dab4_sizes() {
        for (depth, size) in [(0x6800, 6), (0x40, 8)] {
            let mut m = marker_machine(0, 0x1000, 0x400, 0x3e0, 0, depth);
            let mut expected = m.clone();
            expect_lines(&mut expected, 0x4b, 0x2c);
            expect_triangles(&mut expected, size);
            expected.sw(HUD + 0x18, 3);
            run_marker(&mut m);
            assert_same(&m, &expected, &format!("depth {depth:#x}"));
        }
    }

    /// Offset (0x4000, 0) takes the scaled root: SquareRoot0(0x400²) =
    /// (0x1003 << 10) >> 12 = 0x400, << 4 = 0x4000 (the unscaled root would
    /// be 0x400C, beyond range 0x4004). The 45° basis (sin = cos = 0xB50)
    /// puts it at ((0x4000 * 0xB50) >> 12 << 5) / 0x4004 = 22 across and
    /// -22 along: radar point (0x42, 0x16). Depth 0x8000: distance (0x1003
    /// << 15) >> 12 = 0x8018 > 0x7FFF, so only the lines and flag 1. With
    /// range 0x3FFF the length is beyond the range: nothing is written.
    #[test]
    fn radar_marker_3dab4_scaled_radar() {
        let mut m = marker_machine(0xb50, 0xb50, 0x4004, 0x4000, 0, 0x8000);
        let mut expected = m.clone();
        expect_lines(&mut expected, 0x42, 0x16);
        expected.sw(HUD + 0x18, 1);
        run_marker(&mut m);
        assert_same(&m, &expected, "");

        let mut m = marker_machine(0xb50, 0xb50, 0x3fff, 0x4000, 0, 0x8000);
        let before = m.clone();
        run_marker(&mut m);
        assert_same(&m, &before, "");
    }

    /// Radar bounds (identity basis, range 0x400): 31 pixels either way is
    /// drawn, 32 is not (offset 0x400: length 0x400 <= range, but (0x400 <<
    /// 5) / 0x400 = 32). A point behind the camera (depth -0x40: distance
    /// 0x40, RotTransPers flag bit 31) keeps the lines and flag 1 only.
    #[test]
    fn radar_marker_3dab4_bounds() {
        for (dx, dz) in [(0x400, 0), (-0x400, 0), (0, 0x400), (0, -0x400)] {
            let mut m = marker_machine(0, 0x1000, 0x400, dx, dz, 0x7c00);
            let before = m.clone();
            run_marker(&mut m);
            assert_same(&m, &before, &format!("offset ({dx:#x}, {dz:#x})"));
        }
        for (dx, dz, x, y) in [
            (-0x3e0, 0, 0x2c - 31, 0x2c),
            (0, 0x3e0, 0x2c, 0x2c + 31),
            (0, -0x3e0, 0x2c, 0x2c - 31),
        ] {
            let mut m = marker_machine(0, 0x1000, 0x400, dx, dz, 0x7c00);
            let mut expected = m.clone();
            expect_lines(&mut expected, x, y);
            expect_triangles(&mut expected, 2);
            expected.sw(HUD + 0x18, 3);
            run_marker(&mut m);
            assert_same(&m, &expected, &format!("offset ({dx:#x}, {dz:#x})"));
        }
        let mut m = marker_machine(0, 0x1000, 0x400, 0x3e0, 0, -0x40);
        let mut expected = m.clone();
        expect_lines(&mut expected, 0x4b, 0x2c);
        expected.sw(HUD + 0x18, 1);
        run_marker(&mut m);
        assert_same(&m, &expected, "");
        assert!((m.lw(m.sp - 0x58 + 0x44) as i32) < 0, "GTE flag");
    }

    /// 8005C8F0: lines at +0x28/+0x38 take half of panel +0x1C.. (0x81,
    /// 0x40, 0xFF -> 0x40, 0x20, 0x7F), sprites at +0/+0x14 half of panel
    /// +0x20.. (0x11, 0x22, 0x33 -> 0x08, 0x11, 0x19); the tag at +0x14
    /// keeps length 4 and takes the slot's link, the slot keeps its length
    /// byte and points at +0x28.
    #[test]
    fn dim_frame_5c8f0() {
        let (panel, ot, block) = (0x8010_0000u32, 0x8011_0000u32, 0x8012_0000u32);
        let setup = || {
            let mut m = Machine::default();
            for (i, value) in [0x81u8, 0x40, 0xff, 0x5a, 0x11, 0x22, 0x33]
                .into_iter()
                .enumerate()
            {
                m.sb(panel + 0x1c + i as u32, value);
            }
            for offset in (0..0x48).step_by(4) {
                m.sw(block + offset, 0xeeee_eeee);
            }
            m.sw(block + 0x14, 0x0412_0028);
            m.sw(ot, 0x07ab_cdef);
            m
        };
        let mut m = setup();
        let mut expected = m.clone();
        for (to, rgb) in [
            (0x2c, [0x40, 0x20, 0x7f]),
            (0x3c, [0x40, 0x20, 0x7f]),
            (4, [0x08, 0x11, 0x19]),
            (0x18, [0x08, 0x11, 0x19]),
        ] {
            for (c, value) in rgb.into_iter().enumerate() {
                expected.sb(block + to + c as u32, value);
            }
        }
        expected.sw(block + 0x14, 0x04ab_cdef);
        expected.sw(ot, 0x0712_0028);
        extras::dim_frame_5c8f0(&mut m, panel, ot, block);
        assert!(m.take_fault().is_ok());
        assert_same(&m, &expected, "");

        let mut again = setup();
        assert_eq!(
            hud::dispatch(&mut again, 0x8005_c8f0, [panel, ot, block, 0]),
            Some(0)
        );
        assert_same(&again, &expected, "");
    }

    /// Smallest `n >= 1` with `n * speed + 0x29D * n * (n + 1) / 2 >=
    /// distance` (0 for `distance <= 0`), without wrapping.
    fn steps_reference(distance: i64, speed: i64) -> u32 {
        if distance <= 0 {
            return 0;
        }
        (1i64..)
            .find(|n| n * speed + 0x29d * n * (n + 1) / 2 >= distance)
            .unwrap() as u32
    }

    #[test]
    fn fall_steps_652a4() {
        let steps = extras::fall_steps_652a4;
        assert_eq!(steps(0, 0x100), 0);
        assert_eq!(steps(-5i32 as u32, 0x100), 0);
        assert_eq!(steps(1, 0), 1);
        assert_eq!(steps(0x7d7, 0), 2, "0x29D + 0x53A");
        assert_eq!(steps(0x7d8, 0), 3);
        assert_eq!(steps(1, -0x29di32 as u32), 2, "the first step adds 0");
        assert_eq!(steps(0x10_0000, 0x180), 55);
        assert_eq!(steps(0x18_0000, 0x180), 68);
        for distance in [1i32, 0x29d, 0x1000, 0x12345, 0x10_0000, 0x40_0000] {
            for speed in [0i32, 0x180, -0x1000, 0x2000, -0x29d] {
                assert_eq!(
                    steps(distance as u32, speed as u32),
                    steps_reference(i64::from(distance), i64::from(speed)),
                    "distance {distance:#x}, speed {speed:#x}"
                );
            }
        }
        let mut m = Machine::default();
        assert_eq!(
            hud::dispatch(&mut m, 0x8006_52a4, [0x10_0000, 0x180, 0, 0]),
            Some(55)
        );
    }

    const TABLE: u32 = 0x8010_0000;
    const PLAYER: u32 = 0x8011_0000;
    /// Entity of target slot 5.
    const TARGET: u32 = 0x8012_0000;
    const VIEW: u32 = 0x8013_0000;
    const CONTEXT: u32 = 0x8014_0000;
    const POOL: u32 = 0x8017_0000;
    const BUFFER: u32 = 0x8018_0000;
    /// 800C4B36 locked target slot, 800BAEB4 mission, 800C4D98 slot flags.
    const SLOT: u32 = 0x800c_4b36;
    const MISSION: u32 = 0x800b_aeb4;
    const SLOT_FLAGS: u32 = 0x800c_4d98;

    /// Impact marker state: player (entity 0) at (0x100000, -0x100000,
    /// 0x200000) with velocity (0x400, 0x100, -0x200); target slot 5 at y
    /// 0x80000 (slot byte 0xFF: no target); view ground level 0; weapon
    /// modes 5 (+0x3AE) and 9 (+0x3AF); mission 3; camera at (0x100000,
    /// -0x10000, 0x100000) with zero angles (sine entry 0x1000 = 0x4000, so
    /// 800306AC gives the identity); 3 pool lines used; line color (0x11,
    /// 0x22, 0x33); H 0x100, screen offset (160, 120).
    fn impact_machine() -> Machine {
        let mut m = Machine::default();
        m.set_gw(0x2d8, TABLE);
        m.sw(TABLE, PLAYER);
        m.sw(TABLE + 5 * 4, TARGET);
        m.sb(SLOT, 0xff);
        m.sb(MISSION, 3);
        for (offset, word) in [
            (8, 0x10_0000),
            (0xc, 0xfff0_0000),
            (0x10, 0x20_0000),
            (0x38, 0x400),
            (0x3c, 0x100),
            (0x40, 0xffff_fe00),
        ] {
            m.sw(PLAYER + offset, word);
        }
        m.sw(TARGET + 0xc, 0x8_0000);
        m.sw(VIEW + 0x2c, 0);
        m.set_gw(0x2dc, CONTEXT);
        m.sb(CONTEXT + 0x3ae, 5);
        m.sb(CONTEXT + 0x3af, 9);
        m.set_gw(0x2d0, GAMEPLAY);
        m.sw(GAMEPLAY + 0x28, CAMERA);
        m.sw(CAMERA + 4, 0x10_0000);
        m.sw(CAMERA + 8, 0xffff_0000);
        m.sw(CAMERA + 0xc, 0x10_0000);
        m.sh(SINE + 0x1000 * 2, 0x4000);
        m.set_gw(0x508, POOL);
        m.sw(POOL + 0x14, 3);
        m.set_gw(0x50c, BUFFER);
        for (i, value) in [0x11u8, 0x22, 0x33].into_iter().enumerate() {
            m.sb(0x800c_4b30 + i as u32, value);
        }
        m.gte.ctc2(ctrl::H, 0x100);
        m.gte.ctc2(ctrl::OFX, 160 << 16);
        m.gte.ctc2(ctrl::OFY, 120 << 16);
        m
    }

    /// RotTransPers of `v` on an independent GTE with the identity rotation,
    /// no translation and the [`impact_machine`] screen: (sx, sy).
    fn project(v: [i16; 3]) -> (u16, u16) {
        let mut gte = Gte::default();
        for index in [ctrl::R11R12, ctrl::R22R23, ctrl::R33] {
            gte.ctc2(index, 0x1000);
        }
        gte.ctc2(ctrl::H, 0x100);
        gte.ctc2(ctrl::OFX, 160 << 16);
        gte.ctc2(ctrl::OFY, 120 << 16);
        gte.mtc2(
            reg::VXY0,
            u32::from(v[0] as u16) | (u32::from(v[1] as u16) << 16),
        );
        gte.mtc2(reg::VZ0, u32::from(v[2] as u16));
        gte.rtps(true, false);
        let sxy = gte.mfc2(reg::SXY2);
        (sxy as u16, (sxy >> 16) as u16)
    }

    /// Run 80064DC4 and check the camera-relative impact point it projected
    /// (frame +0x10), the six lines from pool buffer +0x3D8 + 3 * 0x10 at
    /// the independently projected point (a cross of +-10 and a diamond of
    /// radius 6), the pool line count (3 + 6) and that nothing else outside
    /// the stack changed.
    fn check_impact(mut m: Machine, point: [i16; 3]) {
        let mut expected = m.clone();
        let (sx, sy) = project(point);
        let shape: [[i16; 4]; 6] = [
            [-10, -10, 10, 10],
            [-10, 10, 10, -10],
            [0, -6, 6, 0],
            [6, 0, 0, 6],
            [0, 6, -6, 0],
            [-6, 0, 0, -6],
        ];
        for (i, [x0, y0, x1, y1]) in shape.into_iter().enumerate() {
            let line = BUFFER + 0x3d8 + 0x10 * (3 + i as u32);
            for (c, value) in [0x11u8, 0x22, 0x33].into_iter().enumerate() {
                expected.sb(line + 4 + c as u32, value);
            }
            for (offset, base, delta) in [(8, sx, x0), (0xa, sy, y0), (0xc, sx, x1), (0xe, sy, y1)]
            {
                expected.sh(line + offset, base.wrapping_add(delta as u16));
            }
        }
        expected.sw(POOL + 0x14, 9);
        let sp = m.sp;
        extras::impact_marker_64dc4(&mut m, VIEW);
        assert!(m.take_fault().is_ok());
        assert_eq!(m.sp, sp);
        let frame = sp - 0x70;
        let stored: Vec<i16> = (0..3).map(|k| m.lh(frame + 0x10 + 2 * k)).collect();
        assert_eq!(stored, point, "impact point");
        assert_eq!(
            m.lw(frame + 0x18),
            (u32::from(sy) << 16) | u32::from(sx),
            "screen"
        );
        assert_same(&m, &expected, "");
    }

    /// No target: ground level 0 (view +0x2C), height 0x100000, start speed
    /// 0x100 * 1.5 = 0x180: 55 steps. Weapon mode 5 and 8003A08C 0 (no
    /// target): 1.5 times the speed, offset (0x600 * 55, -0x300 * 55) =
    /// (0x14A00, -0xA500); relative to the camera `>> 12`: (0x14, 0x10,
    /// 0xF5). Main mode 9: (0x400 * 55, -0x200 * 55) -> (0xD, 0x10, 0xF9).
    #[test]
    fn impact_marker_64dc4() {
        check_impact(impact_machine(), [0x14, 0x10, 0xf5]);
        let mut m = impact_machine();
        m.sb(CONTEXT + 0x3ae, 9);
        check_impact(m, [0xd, 0x10, 0xf9]);
        // Through the dispatch.
        let mut m = impact_machine();
        assert_eq!(hud::dispatch(&mut m, 0x8006_4dc4, [VIEW, 0, 0, 0]), Some(0));
        assert_eq!(m.lw(POOL + 0x14), 9);
        assert!(m.take_fault().is_ok());
    }

    /// The locked target (slot 5) sets the ground level and, available to
    /// 8003A08C with special mode 9, the unscaled speed: level 0x80000,
    /// height 0x180000, 68 steps, offset (0x400 * 68, -0x200 * 68) ->
    /// (0x11, 0x90, 0xF7). Above the player (y -0x200000) the view level is
    /// used: 55 steps -> (0xD, 0x10, 0xF9). With slot flag 0x400 the target
    /// is neither the level nor available: (0x14, 0x10, 0xF5); in mission
    /// 0xD the flag is ignored for the level only: 68 steps scaled,
    /// (0x600 * 68, -0x300 * 68) -> (0x19, 0x90, 0xF3). A player below the
    /// level falls 0 steps: the impact point is under the player (0, 0x10,
    /// 0x100).
    #[test]
    fn impact_marker_64dc4_target() {
        let locked = || {
            let mut m = impact_machine();
            m.sb(SLOT, 5);
            m
        };
        check_impact(locked(), [0x11, 0x90, 0xf7]);
        let mut m = locked();
        m.sw(TARGET + 0xc, 0xffe0_0000);
        check_impact(m, [0xd, 0x10, 0xf9]);
        let mut m = locked();
        m.sw(SLOT_FLAGS + 5 * 4, 0x400);
        check_impact(m, [0x14, 0x10, 0xf5]);
        let mut m = locked();
        m.sw(SLOT_FLAGS + 5 * 4, 0x400);
        m.sb(MISSION, 0xd);
        check_impact(m, [0x19, 0x90, 0xf3]);
        let mut m = impact_machine();
        m.sw(PLAYER + 0xc, 0x1_0000);
        check_impact(m, [0, 0x10, 0x100]);
    }
}

mod tree_62ea0_tests {
    //! No capture reaches 80062EA0/80062FB0 (80062C34 calls them only while
    //! the view is turned), so these are synthetic states with expectations
    //! read off the instructions. SetMulMatrix (800A4ED8) and NormalClip
    //! (800A58E8) have no translation yet: calls to them record
    //! `Fault::Untranslated` and return 0, so the face paths are covered on
    //! the packet half ([`tree_62ea0::dart_packets`]) with chosen NormalClip
    //! results.
    use super::*;
    use ac3_flight::{Fault, dispatch, shared};
    use ac3_gte::{ctrl, reg};
    use hud::tree_62ea0;

    const POOL: u32 = 0x8010_0000;
    const BUFFER: u32 = 0x8011_0000;
    /// First `POLY_G3` packet (0x1C bytes) and first `LINE_G2` packet (0x14
    /// bytes) of the two pool classes.
    const TRIANGLES: u32 = BUFFER + 0x188;
    const LINES: u32 = BUFFER + 0x7d8;
    /// Class counts at entry (pool +0x0C and +0x18).
    const TRIANGLES_USED: u32 = 2;
    const LINES_USED: u32 = 3;

    /// Faces as 80062FB0 stores them: green levels and vertex indices.
    const T0: ([u8; 3], [usize; 3]) = ([0x10, 0x10, 0x40], [0, 2, 1]);
    const T1: ([u8; 3], [usize; 3]) = ([0x10, 0x40, 0x10], [0, 1, 3]);
    const T2: ([u8; 3], [usize; 3]) = ([0x10, 0x10, 0x10], [0, 3, 2]);
    const T3: ([u8; 3], [usize; 3]) = ([0x40, 0x10, 0x10], [1, 2, 3]);
    /// Edges: green levels and vertex indices.
    const L01: ([u8; 2], [usize; 2]) = ([0x18, 0x40], [0, 1]);
    const L02: ([u8; 2], [usize; 2]) = ([0x18, 0x18], [0, 2]);
    const L03: ([u8; 2], [usize; 2]) = ([0x18, 0x18], [0, 3]);
    const L12: ([u8; 2], [usize; 2]) = ([0x40, 0x18], [1, 2]);
    const L13: ([u8; 2], [usize; 2]) = ([0x40, 0x18], [1, 3]);
    const L23: ([u8; 2], [usize; 2]) = ([0x18, 0x18], [2, 3]);
    const ALL_LINES: [([u8; 2], [usize; 2]); 6] = [L01, L02, L03, L12, L13, L23];

    /// The pool with counts 2 and 3 and a buffer of 0xAA bytes (so bytes the
    /// routine does not write stay visible).
    fn pool_machine() -> Machine {
        let mut m = Machine::default();
        m.set_gw(0x508, POOL);
        m.set_gw(0x50c, BUFFER);
        m.sw(POOL + 0xc, TRIANGLES_USED);
        m.sw(POOL + 0x18, LINES_USED);
        for offset in 0..0x900 {
            m.sb(BUFFER + offset, 0xaa);
        }
        m
    }

    /// Expected packet bytes: untouched 0xAA except, per vertex k, the color
    /// (0, green, 0) at 4 + 8k and the xy word at 8 + 8k.
    fn packet(size: usize, greens: &[u8], xys: &[u32]) -> Vec<u8> {
        let mut bytes = vec![0xaa; size];
        for (k, (&green, &xy)) in greens.iter().zip(xys).enumerate() {
            bytes[4 + 8 * k..7 + 8 * k].copy_from_slice(&[0, green, 0]);
            bytes[8 + 8 * k..12 + 8 * k].copy_from_slice(&xy.to_le_bytes());
        }
        bytes
    }

    /// The packets appended after the entry counts are exactly `triangles`
    /// and `lines` (screen words `screen`), the next slot of each class is
    /// untouched and both counts were bumped.
    fn assert_packets(
        m: &mut Machine,
        screen: [u32; 4],
        triangles: &[([u8; 3], [usize; 3])],
        lines: &[([u8; 2], [usize; 2])],
    ) {
        let mut slot = TRIANGLES + 0x1c * TRIANGLES_USED;
        for (i, (greens, vertices)) in triangles.iter().enumerate() {
            let xys = vertices.map(|v| screen[v]);
            assert_eq!(
                m.read_bytes(slot, 0x1c),
                packet(0x1c, greens, &xys),
                "triangle {i}"
            );
            slot += 0x1c;
        }
        assert_eq!(m.read_bytes(slot, 0x1c), vec![0xaa; 0x1c], "next triangle");
        let mut slot = LINES + 0x14 * LINES_USED;
        for (i, (greens, vertices)) in lines.iter().enumerate() {
            let xys = vertices.map(|v| screen[v]);
            assert_eq!(
                m.read_bytes(slot, 0x14),
                packet(0x14, greens, &xys),
                "line {i}"
            );
            slot += 0x14;
        }
        assert_eq!(m.read_bytes(slot, 0x14), vec![0xaa; 0x14], "next line");
        assert_eq!(m.lw(POOL + 0xc), TRIANGLES_USED + triangles.len() as u32);
        assert_eq!(m.lw(POOL + 0x18), LINES_USED + lines.len() as u32);
    }

    /// Faces with a positive NormalClip become triangles; an edge is drawn
    /// when its first face (or, that being negative, its second) is `>= 0`;
    /// v0 is the line count after the last edge, or face 3's NormalClip when
    /// faces 2 and 3 are both back faces.
    #[test]
    fn dart_packets_culling() {
        let screen = [0x0011_0022, 0x0033_0044, 0x0055_0066, 0x0077_0088];
        type Case = (
            [i32; 4],
            &'static [([u8; 3], [usize; 3])],
            &'static [([u8; 2], [usize; 2])],
            u32,
        );
        let cases: [Case; 6] = [
            ([5, 7, 9, 11], &[T0, T1, T2, T3], &ALL_LINES, LINES_USED + 6),
            ([0, 0, 0, 0], &[], &ALL_LINES, LINES_USED + 6),
            (
                [1, -1, 0, -1],
                &[T0],
                &[L01, L02, L03, L12, L23],
                LINES_USED + 5,
            ),
            (
                [-1, 2, -3, 4],
                &[T1, T3],
                &[L01, L03, L12, L13, L23],
                LINES_USED + 5,
            ),
            ([1, -2, -1, -4], &[T0], &[L01, L02, L12], -4i32 as u32),
            ([-1, -1, -5, -3], &[], &[], -3i32 as u32),
        ];
        let frame = 0x8012_0000;
        for (areas, triangles, lines, v0) in cases {
            let mut m = pool_machine();
            for (i, xy) in screen.into_iter().enumerate() {
                m.sw(frame + 0x10 + 4 * i as u32, xy);
            }
            for (i, area) in areas.into_iter().enumerate() {
                m.sw(frame + 0x28 + 4 * i as u32, area as u32);
            }
            assert_eq!(tree_62ea0::dart_packets(&mut m, frame), v0, "{areas:?}");
            assert!(m.take_fault().is_ok());
            assert_packets(&mut m, screen, triangles, lines);
        }
    }

    /// Rotation diag(1, 1, 0) (4.12), H = 0x200 and TR = (0, 0, 0x400): every
    /// vertex has depth 0x400, so screen = offset + (x, y) / 2.
    fn projection(m: &mut Machine) {
        m.gte.ctc2(ctrl::R11R12, 0x1000);
        m.gte.ctc2(ctrl::R22R23, 0x1000);
        m.gte.ctc2(ctrl::H, 0x200);
        // The 800B0988 vertices of the executable (top, nose, wing tips).
        for (i, [x, y, z]) in [[0, -20, 0], [0, 20, 180], [-90, 20, -30], [90, 20, -30]]
            .into_iter()
            .enumerate()
        {
            let vertex = 0x800b_0988 + 8 * i as u32;
            m.sh(vertex, x as i16 as u16);
            m.sh(vertex + 2, y as i16 as u16);
            m.sh(vertex + 4, z as i16 as u16);
        }
    }

    /// The vertices projected about (0xA0, 0xC8) by [`projection`].
    const SCREEN: [u32; 4] = [0x00be_00a0, 0x00d2_00a0, 0x00d2_0073, 0x00d2_00cd];

    type Triangles = &'static [([u8; 3], [usize; 3])];
    type Lines = &'static [([u8; 2], [usize; 2])];

    /// Expected packets of the dart at [`SCREEN`] for its stored face
    /// results: all 0 while NormalClip (800A58E8) is untranslated, else the
    /// NCLIP values worked out by hand: face 0 (s0, s2, s1) and face 1 (s0,
    /// s1, s3) -900, face 2 (s0, s3, s2) 1800, face 3 (s1, s2, s3, collinear)
    /// 0, which give triangle 2 and every edge but 0-1.
    fn screen_packets(areas: [i32; 4]) -> (Triangles, Lines) {
        match areas {
            [0, 0, 0, 0] => (&[], &ALL_LINES),
            [-900, -900, 1800, 0] => (&[T2], &[L02, L03, L12, L13, L23]),
            other => panic!("unexpected face results {other:?}"),
        }
    }

    /// 80062FB0 (0xA0, 0xC8), as 80062EA0 calls it: the four projections in
    /// the frame, the geometry offset restored to (0xA0, 0x80), the face
    /// results and the packets ([`screen_packets`]), v0 the final line
    /// count; run by original address as well. NormalClip is the only
    /// callee without a translation (the face results are then 0).
    #[test]
    fn dart_62fb0() {
        let mut m = pool_machine();
        projection(&mut m);
        m.gte.ctc2(ctrl::TRZ, 0x400);
        let mut by_address = m.clone();
        let sp = m.sp;
        let v0 = tree_62ea0::dart_62fb0(&mut m, 0xa0, 0xc8);
        let fault = m.take_fault();
        assert_eq!(m.sp, sp);
        let frame = sp - 0x50;
        let areas = [0, 1, 2, 3].map(|i| m.lw(frame + 0x28 + 4 * i) as i32);
        match fault {
            Err(Fault::Untranslated {
                address: 0x800a_58e8,
            }) => assert_eq!(areas, [0; 4]),
            Ok(()) => assert_eq!(areas, [-900, -900, 1800, 0]),
            other => panic!("{other:?}"),
        }
        for (i, xy) in SCREEN.into_iter().enumerate() {
            assert_eq!(m.lw(frame + 0x10 + 4 * i as u32), xy, "vertex {i}");
        }
        assert_eq!(m.lw(frame + 0x38), 0, "depth cue (DQA = DQB = 0)");
        assert_eq!(m.lw(frame + 0x3c), 0, "flag");
        assert_eq!(m.gte.cfc2(ctrl::OFX), 0xa0 << 16);
        assert_eq!(m.gte.cfc2(ctrl::OFY), 0x80 << 16);
        // The last three projections, or the arguments of the last
        // NormalClip (s1, s2, s3): the same words.
        assert_eq!(m.gte.data[reg::SXY0..=reg::SXY2], SCREEN[1..]);
        assert_eq!(m.gte.data[reg::SZ3], 0x400);
        let (triangles, lines) = screen_packets(areas);
        assert_packets(&mut m, SCREEN, triangles, lines);
        assert_eq!(v0, LINES_USED + lines.len() as u32);

        let v0_by_address = dispatch::call(&mut by_address, 0x8006_2fb0, [0xa0, 0xc8, 0, 0]);
        assert_eq!(by_address.take_fault(), fault);
        assert_eq!(v0_by_address, v0);
        assert!(by_address.ram() == m.ram());
        assert_eq!(by_address.gte.data, m.gte.data);
        assert_eq!(by_address.gte.control, m.gte.control);
    }

    /// The 3x3 halfwords of a MATRIX (row-major), sign-extended.
    fn elements(bytes: &[u8]) -> [[i32; 3]; 3] {
        std::array::from_fn(|i| {
            std::array::from_fn(|j| {
                let at = 2 * (3 * i + j);
                i32::from(i16::from_le_bytes([bytes[at], bytes[at + 1]]))
            })
        })
    }

    /// 80062EA0: the transposed rotation of the view object's angles
    /// (`[gameplay+0x28]` +0x14..) at frame +0x50, entity 0's rotation
    /// (+0x18..) at +0x30 and the same with y = 0 at +0x10 (each compared
    /// with the shared routine on the same angles), translations (0, 0,
    /// 0x400), TR from +0x30, then the dart. While SetMulMatrix (800A4ED8)
    /// is untranslated (the first fault), the rotation registers keep the
    /// test's [`projection`] and the dart lands as in [`dart_62fb0`]; once it
    /// is translated, the rotation must be view x attitude (4.12, RTV0 per
    /// column; no saturation for these angles).
    #[test]
    fn attitude_marker_62ea0() {
        let mut m = pool_machine();
        for i in 0..0x5000u32 {
            let angle = f64::from(i) * std::f64::consts::TAU / 16384.0;
            m.sh(
                0x8001_0498 + 2 * i,
                (16384.0 * angle.sin()).round() as i16 as u16,
            );
        }
        let (gameplay, object, table, player) =
            (0x8012_0000, 0x8012_1000, 0x8012_2000, 0x8012_3000);
        m.set_gw(0x2d0, gameplay);
        m.sw(gameplay + 0x28, object);
        m.set_gw(0x2d8, table);
        m.sw(table, player);
        let view_angles = [0x0123, 0x0456, 0x3789];
        let player_angles = [0x0abc, 0x0def, 0x1312];
        for i in 0..3 {
            m.sh(object + 0x14 + 2 * i, view_angles[i as usize]);
            m.sh(player + 0x18 + 2 * i, player_angles[i as usize]);
        }
        projection(&mut m);
        for tr in [ctrl::TRX, ctrl::TRY, ctrl::TRZ] {
            m.gte.ctc2(tr, 0x1111);
        }

        // The three rotations by the shared routines on the same angles.
        let mut oracle = m.clone();
        let (angles, out) = (0x8013_0000, 0x8013_0100);
        let mut rotation = |angles_xyz: [u16; 3], transposed: bool| {
            for (i, angle) in angles_xyz.into_iter().enumerate() {
                oracle.sh(angles + 2 * i as u32, angle);
            }
            if transposed {
                shared::rotation_yxz_transposed(&mut oracle, angles, out);
            } else {
                shared::rotation_yxz(&mut oracle, angles, out);
            }
            oracle.read_bytes(out, 0x12)
        };
        let view = rotation(view_angles, true);
        let attitude = rotation(player_angles, false);
        let level = rotation([player_angles[0], 0, player_angles[2]], false);
        assert!(view != attitude && attitude != level && view.iter().any(|&b| b != 0));

        let mut by_address = m.clone();
        let sp = m.sp;
        let v0 = tree_62ea0::attitude_marker_62ea0(&mut m);
        let fault = m.take_fault();
        let multiplied = match fault {
            Err(Fault::Untranslated {
                address: 0x800a_4ed8,
            }) => false,
            Ok(())
            | Err(Fault::Untranslated {
                address: 0x800a_58e8,
            }) => true,
            other => panic!("{other:?}"),
        };
        assert_eq!(m.sp, sp);
        let frame = sp - 0x88;
        assert_eq!(m.read_bytes(frame + 0x50, 0x12), view, "view matrix");
        assert_eq!(
            m.read_bytes(frame + 0x30, 0x12),
            attitude,
            "attitude matrix"
        );
        assert_eq!(m.read_bytes(frame + 0x10, 0x12), level, "zero-y matrix");
        for matrix in [0x10, 0x30, 0x50] {
            let translation = [0x14, 0x18, 0x1c].map(|t| m.lw(frame + matrix + t));
            assert_eq!(translation, [0, 0, 0x400], "translation of +{matrix:#x}");
        }
        let angles = [0, 2, 4].map(|o| m.lhu(frame + 0x70 + o));
        assert_eq!(angles, [player_angles[0], 0, player_angles[2]]);
        let tr = [ctrl::TRX, ctrl::TRY, ctrl::TRZ].map(|r| m.gte.cfc2(r));
        assert_eq!(tr, [0, 0, 0x400]);
        assert_eq!(m.gte.cfc2(ctrl::OFX), 0xa0 << 16);
        assert_eq!(m.gte.cfc2(ctrl::OFY), 0x80 << 16);
        let dart = frame - 0x50;
        if multiplied {
            let c = m.gte.control;
            let half = |word: u32, high: bool| {
                i32::from((if high { word >> 16 } else { word }) as u16 as i16)
            };
            let rotation = [
                [half(c[0], false), half(c[0], true), half(c[1], false)],
                [half(c[1], true), half(c[2], false), half(c[2], true)],
                [half(c[3], false), half(c[3], true), half(c[4], false)],
            ];
            let (a, b) = (elements(&view), elements(&attitude));
            let product: [[i32; 3]; 3] = std::array::from_fn(|i| {
                std::array::from_fn(|j| (0..3).map(|k| a[i][k] * b[k][j]).sum::<i32>() >> 12)
            });
            assert_eq!(rotation, product, "SetMulMatrix(view, attitude)");
            // The dart's packets for this rotation are covered by dart_62fb0.
            let lines = m.lw(POOL + 0x18);
            assert!(v0 == lines || v0 == m.lw(dart + 0x34), "{v0:#x}");
            eprintln!("80062EA0: SetMulMatrix translated, dart packets not compared here");
        } else {
            for (i, xy) in SCREEN.into_iter().enumerate() {
                assert_eq!(m.lw(dart + 0x10 + 4 * i as u32), xy, "vertex {i}");
            }
            let areas = [0, 1, 2, 3].map(|i| m.lw(dart + 0x28 + 4 * i) as i32);
            let (triangles, lines) = screen_packets(areas);
            assert_packets(&mut m, SCREEN, triangles, lines);
            assert_eq!(v0, LINES_USED + lines.len() as u32, "80062FB0's v0");
        }

        let v0_by_address = dispatch::call(&mut by_address, 0x8006_2ea0, [0x8014_0000, 0, 0, 0]);
        assert_eq!(by_address.take_fault(), fault);
        assert_eq!(v0_by_address, v0);
        assert!(by_address.ram() == m.ram());
        assert_eq!(by_address.gte.data, m.gte.data);
        assert_eq!(by_address.gte.control, m.gte.control);
    }
}

/// hud-015: a lock-on run. From the mission-006 checkpoint (inputs
/// `inputs-009`), an autopilot "player" (`.hud/lockon-snap.lua`, reading
/// guest memory only and issuing pad presses) keeps the selected target
/// (slot 3) centred. The lock cursor starts in frames 3053 and 4501 (8004011C),
/// travels to the marker (4501..4509), locks (3055/3057, 4509/4511), and is
/// cleared when the target passes (4833, 80040160); the reticle 8004049C is
/// drawn in both shapes. Snapshots: the lock-on routines in frames 3053, 3055,
/// 3393, 4501..4509 and 4833, the composite routines in 3057 and 4511.
fn lockon_index() -> PathBuf {
    data("analysis/flight-runtime/hud-015/snapshots.jsonl")
}

/// Every hud-015 entry of `callee` (frame, occurrence).
fn lockon_cases(callee: u32) -> Vec<(u64, usize)> {
    snapshot::read_index(&lockon_index())
        .unwrap()
        .iter()
        .filter(|s| s.callee == callee && s.stage == "entry")
        .enumerate()
        .map(|(occurrence, s)| (s.frame, occurrence))
        .collect()
}

/// Strict comparison of every hud-015 entry of `callee`; returns the bytes
/// changed per frame.
fn lockon_check(
    callee: u32,
    native: impl Fn(&mut Machine, &Snapshot) -> Option<u32>,
) -> Vec<(u64, usize)> {
    let cases = lockon_cases(callee);
    assert!(!cases.is_empty(), "{callee:08x}: no hud-015 entries");
    cases
        .into_iter()
        .map(|(frame, occurrence)| {
            let changed = check(&lockon_index(), callee, occurrence, |m, e| native(m, e));
            (frame, changed)
        })
        .collect()
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-015"]
fn lockon_state_routines() {
    use hud::tree_39d84 as t39;
    let changed = lockon_check(0x8003abac, |m, _| {
        t39::screen_targets_3abac(m);
        Some(0)
    });
    assert!(changed.iter().all(|&(_, c)| c > 0), "{changed:?}");
    lockon_check(0x8003e780, |m, _| {
        t39::lockon_3e780(m);
        None
    });
    let changed = lockon_check(0x8003eb14, |m, _| {
        t39::lock_states_3eb14(m);
        None
    });
    assert!(changed.iter().any(|&(_, c)| c > 0), "{changed:?}");
    // The cursor starts in 3053 and 4501.
    let changed = lockon_check(0x8004011c, |m, e| Some(t39::lock_start_4011c(m, e.gpr(4))));
    assert_eq!(
        changed.iter().map(|&(f, _)| f).collect::<Vec<_>>(),
        [3053, 4501]
    );
    assert!(changed.iter().all(|&(_, c)| c > 0), "{changed:?}");
    // It travels, locks and follows the marker.
    let changed = lockon_check(0x8004016c, |m, _| {
        t39::lock_cursor_4016c(m);
        None
    });
    assert!(
        changed.iter().filter(|&&(_, c)| c > 0).count() >= 6,
        "{changed:?}"
    );
    // It is cleared when the target passes (4833).
    let changed = lockon_check(0x80040160, |m, _| Some(t39::lock_clear_40160(m)));
    assert_eq!(changed.iter().map(|&(f, _)| f).collect::<Vec<_>>(), [4833]);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-015"]
fn lockon_marker_routines() {
    use hud::tree_3e828 as t;
    let changed = lockon_check(0x8003e828, |m, _| {
        t::markers_3e828(m);
        None
    });
    assert!(changed.iter().all(|&(_, c)| c > 0), "{changed:?}");
    lockon_check(0x8003e860, |m, _| {
        t::begin_3e860(m);
        None
    });
    lockon_check(0x8003ed64, |m, _| {
        t::targets_3ed64(m);
        None
    });
    lockon_check(0x8003fd24, |m, e| Some(t::arrow_3fd24(m, e.gpr(4))));
    lockon_check(0x80040040, |m, e| {
        t::label_40040(m, e.gpr(4), e.gpr(5), e.gpr(6));
        None
    });
    lockon_check(0x8003ff34, |m, e| {
        t::status_3ff34(m, e.gpr(4), e.gpr(5), e.gpr(6));
        None
    });
    lockon_check(0x8003f28c, |m, e| {
        t::box_3f28c(m, e.gpr(4), e.gpr(5));
        None
    });
    // The reticle: diamond while the lock timer is below 32, then crosshair;
    // nothing once the cursor is cleared (3393, 4833).
    let changed = lockon_check(0x8004049c, |m, _| {
        t::reticle_4049c(m);
        None
    });
    let drawn: Vec<u64> = changed
        .iter()
        .filter(|&&(_, c)| c > 0)
        .map(|&(f, _)| f)
        .collect();
    assert_eq!(
        drawn,
        [3053, 3055, 3057, 4501, 4503, 4505, 4507, 4509, 4511],
        "{changed:?}"
    );
    lockon_check(0x8003e89c, |m, _| {
        t::link_3e89c(m);
        None
    });
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-015"]
fn lockon_instrument_routines() {
    lockon_check(0x800645c0, |m, _| {
        Some(hud::tree_62680_a::target_arrow_645c0(m))
    });
    lockon_check(0x80064ae8, |m, _| {
        Some(hud::tree_62680_b::gun_pipper_64ae8(m))
    });
    for (frame, occurrence) in lockon_cases(0x80062680) {
        let _ = frame;
        check_composite(&lockon_index(), 0x80062680, occurrence, |m, e| {
            hud::compose::overlay_62680(m, e.gpr(4));
        });
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-015"]
fn lockon_composite_routines() {
    let index = lockon_index();
    for (frame, occurrence) in lockon_cases(0x80039d84) {
        let _ = frame;
        check_composite(&index, 0x80039d84, occurrence, |m, _| {
            hud::status::status_39d84(m);
        });
    }
    for (frame, occurrence) in lockon_cases(0x8003357c) {
        let _ = frame;
        check_composite(&index, 0x8003357c, occurrence, |m, _| {
            hud::compose::composition_3357c(m);
        });
    }
    for (frame, occurrence) in lockon_cases(0x80039b80) {
        let _ = frame;
        check_composite(&index, 0x80039b80, occurrence, |m, e| {
            hud::compose::compose_39b80(m, e.gpr(4));
        });
    }
    for (frame, occurrence) in lockon_cases(0x8003bdf8) {
        let _ = frame;
        check_composite(&index, 0x8003bdf8, occurrence, |m, e| {
            hud::compose::gauges_3bdf8(m, e.gpr(4));
        });
    }
}

/// hud-017: the hud-015 autopilot run, now firing missiles (CIRCLE) every
/// 40 frames while locked: targets are destroyed and the selection moves on
/// (slots 3, 2, 6, 8), and the mission-end fade (8008E42C with partial
/// levels, quads) runs from frame 6659. Every HUD routine is bracketed in
/// frames 3083 (missile in flight) and 3285 (first target destroyed, next
/// one selected), 80039D84/8008E42C/80055038 in 6661 (all routines),
/// 6681, 6701 and 6715.
fn firing_index() -> PathBuf {
    data("analysis/flight-runtime/hud-017/snapshots.jsonl")
}

/// The composite routines, compared with [`check_composite`] (VZ0 pad).
const COMPOSITES: [u32; 5] = [0x8003357c, 0x80039b80, 0x80039d84, 0x80062680, 0x8003bdf8];

/// Every bracketed call of `index` through [`hud::dispatch`] with its
/// original a0..a3 (stack arguments come from the captured frame), compared
/// strictly (state only; v0 is checked by the per-routine tests). Returns
/// (calls checked, bytes the original changed).
fn sweep(index: &Path) -> (usize, usize) {
    let infos = snapshot::read_index(index).unwrap();
    let mut seen: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
    let mut failures = Vec::new();
    let (mut checked, mut changed) = (0, 0);
    for info in infos.iter().filter(|s| s.stage == "entry") {
        let callee = info.callee;
        let occurrence = *seen.entry(callee).and_modify(|n| *n += 1).or_insert(0);
        let args: [u32; 4] = std::array::from_fn(|i| info.registers.gpr[4 + i]);
        checked += 1;
        if COMPOSITES.contains(&callee) {
            check_composite(index, callee, occurrence, |m, _| {
                assert!(hud::dispatch(m, callee, args).is_some());
            });
            continue;
        }
        match snapshot::check_call(index, callee, occurrence, |m, _| {
            assert!(
                hud::dispatch(m, callee, args).is_some(),
                "{callee:08x} unclaimed"
            );
            None
        }) {
            Ok(bytes) => changed += bytes,
            Err(error) => failures.push(format!("frame {}: {error}", info.frame)),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    (checked, changed)
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-017"]
fn firing_run_all_routines() {
    let (checked, changed) = sweep(&firing_index());
    assert!(checked > 150, "{checked}");
    assert!(changed > 0);
}

/// The fade in 6661..6715 draws quads (partial levels); v0 is the next free
/// primitive.
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-017"]
fn firing_run_fade_8e42c() {
    let index = firing_index();
    let frames: Vec<u64> = snapshot::read_index(&index)
        .unwrap()
        .iter()
        .filter(|s| s.callee == 0x8008e42c && s.stage == "entry")
        .map(|s| s.frame)
        .collect();
    assert_eq!(frames, [6661, 6681, 6701, 6715]);
    for occurrence in 0..frames.len() {
        let changed = check(&index, 0x8008e42c, occurrence, |m, e| {
            Some(hud::tree_39d84::fade_8e42c(m, e.gpr(4), e.gpr(5), e.gpr(6)))
        });
        assert!(changed > 0);
    }
}

/// hud-015 (lock-on run) swept the same way.
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/hud-015"]
fn lockon_run_all_routines() {
    let (checked, changed) = sweep(&lockon_index());
    assert!(checked > 150, "{checked}");
    assert!(changed > 0);
}
