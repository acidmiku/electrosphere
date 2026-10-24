//! Overlay module 4 (state 8, the font and message checker,
//! `ac3_flight::ov4`) against the original.
//!
//! Capture `ovmisc-019`: from the title (CROSS at 301) the documented
//! one-register state selection (`.ovmisc/snapshots.lua`: the first
//! state-callback result 0 on or after frame 450 becomes 0x60000008)
//! enters state 8; module 4 loads by frame 469, where 800CD1B4 runs.
//! Snapshots: 800CD1B4 entry, 800CCC94 entry/return and the first 80026D00
//! entry/return (module 4 only). The original then stops for good: the
//! `sim:` files do not exist on the retail system and PCSX-Redux pauses on
//! an invalid read (0xFFFF8960) during the second font sheet's upload.
//!
//! Everything the capture cannot reach is compared with the differential
//! oracle (`tests/support/oracle.rs`, the original instructions interpreted
//! from the snapshot's RAM with module 4 loaded) on synthetic states built
//! on the 800CD1B4 entry snapshot: font and message data, pad bits, phases.
//! Calls leaving the routine run natively in both runs (`dispatch::call`),
//! so each routine is compared on its own instructions.
mod support;

use ac3_flight::{
    Fault, Machine, dispatch,
    machine::STACK_LIMIT,
    ov4,
    snapshot::{self, Snapshot},
};
use support::oracle::{self, Rng};

const CAPTURE: &str = "ovmisc-019";
const CONTEXT: u32 = 0x800b_acd0;

/// The 800CD1B4 entry (frame 469): module 4 loaded, its data in RAM.
fn base() -> Machine {
    oracle::entries(CAPTURE, 0x800c_d1b4)
        .first()
        .expect("800CD1B4 entry")
        .machine()
}

fn context(m: &mut Machine) -> u32 {
    m.lw(CONTEXT)
}

/// Bytes written by interrupt handlers, the BIOS and the routines' own
/// callees, excluded from the partial setup comparison.
const OWNED: &[(u32, u32, &str)] = &[
    (
        0x8000_0000,
        0x8001_0000,
        "BIOS kernel RAM (file system, TTY)",
    ),
    (0x800b_256c, 0x800b_2570, "sound interrupt flag"),
    (0x800b_2ec4, 0x800b_2ec8, "VSync root-counter reading"),
    (
        0x800b_2f2c,
        0x800b_3f44,
        "libetc interrupt context and stack",
    ),
    (0x800b_3ff4, 0x800b_3ff8, "VSync counter"),
    (0x800b_4010, 0x800b_4014, "GPU DMA callback slot"),
    (0x800b_990c, 0x800b_9928, "libgpu queue state"),
    (0x800b_f150, 0x800b_f1d0, "CD monitor and driver state"),
    (0x800c_31d0, 0x800c_3420, "CD ring state and sector headers"),
    (0x800c_5bcc, 0x800c_5bce, "sound driver tick state"),
    (0x800c_5c70, 0x800c_5c72, "sound driver tick state"),
    (0x800c_8230, 0x800c_9a30, "libgpu queue entries"),
    // printf (800AC3E8) buffers TTY output here (800ACBA8 / 800ACCDC);
    // `sdk::printf` is a host call.
    (0x800c_9d40, 0x800c_9d60, "libc printf TTY line buffer"),
];

#[test]
fn claims_only_while_loaded() {
    let mut m = Machine::default();
    m.sw(0x800b_a9f4 + 0x2dc, 0x800b_d158);
    m.sw(0x800b_d158 + 0x38, 1);
    assert_eq!(
        ov4::dispatch(&mut m, 0x800c_db24, [5, u32::from(b'1'), 0, 0]),
        None
    );
    m.sw(0x800b_d158 + 0x38, 4);
    assert_eq!(
        ov4::dispatch(&mut m, 0x800c_db24, [5, u32::from(b'1'), 0, 0]),
        Some(0xffff_ffff)
    );
    assert_eq!(ov4::dispatch(&mut m, 0x800c_c4d4, [0; 4]), None);
    assert!(m.take_fault().is_ok());
}

#[test]
fn kerning_examples() {
    let k = |font, left: u8, right: u8| ov4::kerning(font, left.into(), right.into()) as i32;
    assert_eq!(k(0, b'r', b'a'), -1);
    assert_eq!(k(1, b'e', b't'), -2);
    assert_eq!(k(2, b'i', b'd'), 1);
    assert_eq!(k(5, b'1', b'x'), -1);
    assert_eq!(k(6, b'1', b'x'), 0);
    assert_eq!(k(8, b'1', b'x'), 0);
    assert_eq!(k(5, b'C', b'h'), 0);
    assert_eq!(k(5, b'C', b'a'), -1);
    assert_eq!(k(16, b'W', b'i'), 2);
    assert_eq!(k(16, b'a', 0xdf), 0);
    assert_eq!(k(16, b'a', b'c'), -1);
    assert_eq!(k(22, b'V', 0), -1);
    assert_eq!(k(23, b'r', b'a'), 0);
    assert_eq!(ov4::kerning(16, u32::from(b'a'), 0x161), 0xffff_ffff);
    assert_eq!(ov4::kerning(0, 0x172, u32::from(b'a')), 0);
}

/// The captured call clears memory that is already zero (fresh arena), so
/// it shows `v0` only; the oracle cases clear random contents.
#[test]
#[ignore = "needs analysis/flight-runtime/ovmisc-019"]
fn reset_messages_matches_original() {
    let index = oracle::capture(CAPTURE);
    let changed = snapshot::check_call(&index, 0x800c_cc94, 0, |m, _| {
        ov4::reset_messages(m);
        Some(0)
    })
    .unwrap();
    println!("800CCC94 (captured): {changed} bytes, v0 0");
    let base = base();
    let mut rng = Rng(0x000c_cc94);
    let mut changed = 0;
    for _ in 0..20 {
        let mut m = base.clone();
        let block = 0x8015_0000 + 4 * (rng.next() % 0x100);
        m.sw(ov4::BLOCK, block);
        for i in (0..0x470).step_by(4) {
            m.sw(block + i, rng.next() | 1);
        }
        changed += oracle::diff("800CCC94", &m, (0x800c_cc94, 0x800c_ccd0), [0; 4], |m| {
            ov4::reset_messages(m);
            Some(0)
        });
    }
    println!("800CCC94: 20 oracle cases, {changed} bytes");
    assert!(changed > 0);
}

/// 800CD1B4 from its entry to the first 80026D00 entry (the original
/// never returns): allocations, display and context bits, the block, the
/// font file names and the first path. The callees in between are
/// replaced by their retail effects: sprintf writes the path, open fails
/// (-1), printf, read and close write nothing. Their own stack frames and
/// the argument home slots below the 0x8180-byte frame are excluded.
#[test]
#[ignore = "needs analysis/flight-runtime/ovmisc-019"]
fn setup_until_first_upload_matches_original() {
    let index = oracle::capture(CAPTURE);
    let directory = index.parent().unwrap();
    let all = snapshot::read_index(&index).unwrap();
    let load = |callee: u32, stage: &str| {
        let info = all
            .iter()
            .find(|s| s.callee == callee && s.stage == stage)
            .unwrap();
        Snapshot::load(directory, info).unwrap()
    };
    let entry = load(0x800c_d1b4, "entry");
    let upload = load(0x8002_6d00, "entry");
    let mut m = entry.machine();
    let mut checked = None;
    {
        let mut c = |m: &mut Machine, target: u32, a: [u32; 4]| -> u32 {
            match target {
                0x800a_cd28 => {
                    // sprintf(path, "sim:pm\font\%s", name)
                    let mut name = Vec::new();
                    let mut p = a[2];
                    while m.lbu(p) != 0 {
                        name.push(m.lbu(p));
                        p += 1;
                    }
                    let mut text = b"sim:pm\\font\\".to_vec();
                    text.extend(name);
                    text.push(0);
                    for (i, &b) in text.iter().enumerate() {
                        m.sb(a[0] + i as u32, b);
                    }
                    text.len() as u32 - 1
                }
                0x800a_d808 | 0x800a_d818 | 0x800a_d838 => 0xffff_ffff,
                0x800a_c3e8 => 0,
                0x8002_6d00 if checked.is_none() => {
                    assert_eq!(m.sp, upload.gpr(29), "sp at the upload");
                    assert_eq!(a[0], upload.gpr(4), "TIM argument");
                    let low = (m.sp & 0x1f_ffff) as usize - 0x1000;
                    let high = (m.sp & 0x1f_ffff) as usize + 0x10;
                    let differ: Vec<u32> = (0..snapshot::STACK_START)
                        .filter(|&i| m.ram()[i] != upload.ram[i])
                        .filter(|&i| !(low..high).contains(&i))
                        .map(|i| 0x8000_0000 + i as u32)
                        .filter(|&a| !OWNED.iter().any(|(s, e, _)| (*s..*e).contains(&a)))
                        .collect();
                    let changed = (0..snapshot::STACK_START)
                        .filter(|&i| entry.ram[i] != upload.ram[i])
                        .count();
                    checked = Some((differ, changed));
                    0
                }
                _ => 0,
            }
        };
        ov4::setup(&mut m, &mut c);
    }
    let (differ, changed) = checked.expect("80026D00 reached");
    assert!(
        differ.is_empty(),
        "{} bytes differ: {:08x?}",
        differ.len(),
        &differ[..differ.len().min(24)]
    );
    println!("800CD1B4 to the first upload: {changed} original bytes changed");
    assert!(changed > 0);
    // The 0x8180-byte frame reaches below the port's stack region.
    let frame = entry.gpr(29) - 0x8180;
    match m.take_fault() {
        Ok(()) => assert!(frame >= STACK_LIMIT),
        Err(f) => assert_eq!(
            f,
            Fault::Address {
                address: frame,
                width: 4
            }
        ),
    }
}

/// 800CDB24 for every font 0..=24 (and others), every left and right
/// character 0..=256 and random words.
#[test]
#[ignore = "needs analysis/flight-runtime/ovmisc-019"]
fn kerning_matches_original_everywhere() {
    let mut m = base();
    let mut rng = Rng(0x00dd_4b24);
    let mut extra: Vec<u32> = (0..64).map(|_| rng.next() | (rng.next() << 24)).collect();
    extra.extend([0xffff_ffff, 0x8000_0000, 0x7fff_ffff, 0x1_0061, 0xffff_ff61]);
    let fonts: Vec<u32> = (0..=26).chain([0x100, 0xffff_ffff]).collect();
    let values: Vec<u32> = (0..=256).chain(extra.iter().copied()).collect();
    let mut cases = 0usize;
    let mut nonzero = 0usize;
    for &font in &fonts {
        for &left in &values {
            for &right in &values {
                let v0 = oracle::run(&mut m, 0x800c_db24, 0x800c_eb68, [font, left, right, 0]);
                assert_eq!(
                    ov4::kerning(font, left, right),
                    v0,
                    "font {font} left {left:#x} right {right:#x}"
                );
                cases += 1;
                nonzero += usize::from(v0 != 0);
            }
        }
    }
    assert!(m.take_fault().is_ok());
    println!("800CDB24: {cases} cases, {nonzero} non-zero");
    assert!(nonzero > 1000);
}

/// GetTPage (800AAB98, `states::pending_sdk::get_tpage`, called by
/// 800CD1B4) over random arguments.
#[test]
#[ignore = "needs analysis/flight-runtime/ovmisc-019"]
fn get_tpage_matches_original() {
    let mut m = base();
    let mut rng = Rng(0x000a_ab98);
    for case in 0..5000u32 {
        let args = if case < 16 {
            [case & 3, case >> 2, 0x2c0 + 0x40 * case, case << 6]
        } else {
            [rng.next(), rng.next(), rng.next(), rng.next() << 2]
        };
        let v0 = oracle::run(&mut m, 0x800a_ab98, 0x800a_abd8, args);
        let [tp, abr, x, y] = args;
        assert_eq!(
            ac3_flight::states::pending_sdk::get_tpage(tp, abr, x, y),
            v0,
            "{args:x?}"
        );
    }
    assert!(m.take_fault().is_ok());
    println!("800AAB98: 5000 cases");
}

/// A synthetic message file at `file`: five language tables of `count`
/// texts each, offsets with random low bits (the lookups mask them).
fn message_file(m: &mut Machine, rng: &mut Rng, file: u32, size: u32) {
    for i in (0..size).step_by(4) {
        m.sw(file + i, (rng.next() & 0x7f7f_7f7f) | 0x2020_2020);
    }
    for i in (0..size).step_by(37) {
        m.sb(file + i, 0);
    }
    for language in 0..5 {
        let table = (0x40 + language * (size / 6)) & !3;
        m.sw(file + 4 + language * 4, table | (rng.next() & 3));
        let count = 1 + rng.next() % 12;
        m.sw(file + table, count);
        for k in 0..16 {
            let offset = 0x40 + 4 * (rng.next() % (size / 32));
            m.sw(file + table + 4 + k * 4, offset | (rng.next() & 3));
        }
    }
}

#[test]
#[ignore = "needs analysis/flight-runtime/ovmisc-019"]
fn message_lookups_match_original() {
    let base = base();
    let mut rng = Rng(0x000c_f1c8);
    let mut cases = 0;
    for case in 0..300u32 {
        let mut m = base.clone();
        let file = 0x8014_0000;
        message_file(&mut m, &mut rng, file, 0x4000);
        let ctx = context(&mut m);
        m.sw(ctx + 0x38c, rng.next() % 6);
        let a = rng.next() % 18;
        let b = rng.next() % 18;
        let changed = match case % 3 {
            0 => oracle::diff(
                "800CF1C8",
                &m,
                (0x800c_f1c8, 0x800c_f214),
                [a, file, 0, 0],
                |m| Some(ov4::message_text(m, a, file)),
            ),
            1 => oracle::diff(
                "800CF214",
                &m,
                (0x800c_f214, 0x800c_f278),
                [a, b, file, 0],
                |m| Some(ov4::message_text2(m, a, b, file)),
            ),
            _ => oracle::diff(
                "800CF278",
                &m,
                (0x800c_f278, 0x800c_f2cc),
                [a, file, 0, 0],
                |m| Some(ov4::message_word(m, a, file)),
            ),
        };
        assert_eq!(changed, 0);
        cases += 1;
    }
    println!("800CF1C8/800CF214/800CF278: {cases} cases (v0 compared)");
}

/// Characters the font rules test, Latin-1 letters, controls and noise.
fn text(m: &mut Machine, rng: &mut Rng, at: u32, length: u32) {
    const SET: &[u8] = b"lttt.bdefijuBLQWZagrwxyz!cinpshoAEHMRSTVNkDCxGm/1234567890 \n\n\r\x01\x03\x06\x07\xe4\xe9\xc9\xf1\xdf\xf6\xfc\xd3\xc1\xed\xf3\xfa\xe7\xe8\xf9\xe1-;()";
    for i in 0..length {
        let byte = if rng.next().is_multiple_of(8) {
            1 + (rng.next() % 255) as u8
        } else {
            SET[(rng.next() as usize) % SET.len()]
        };
        m.sb(at + i, byte);
    }
    m.sb(at + length, 0);
}

/// 800CEB68 over random fonts, texts, positions and colours.
#[test]
#[ignore = "needs analysis/flight-runtime/ovmisc-019"]
fn draw_text_matches_original() {
    let base = base();
    let mut rng = Rng(0x000c_eb68);
    let mut fonts = [0usize; 32];
    let mut changed = 0;
    for case in 0..600u32 {
        let mut m = base.clone();
        let font = if case % 20 == 19 {
            25 + rng.next() % 7
        } else {
            case % 25
        };
        let string = 0x8018_0000;
        let length = rng.next() % 48;
        text(&mut m, &mut rng, string, length);
        let ot = 0x801a_0000;
        m.sw(ot, rng.next());
        let packet = 0x8019_0000;
        let sp = m.sp;
        m.sw(sp + 0x10, rng.next() % 400);
        m.sw(sp + 0x14, rng.next() % 300);
        m.sw(sp + 0x18, rng.next() & 0x00ff_ffff);
        changed += oracle::diff(
            "800CEB68",
            &m,
            (0x800c_eb68, 0x800c_f1c8),
            [font, string, ot, packet],
            |m| {
                Some(ov4::draw_text(
                    m,
                    &mut dispatch::call,
                    font,
                    string,
                    ot,
                    packet,
                ))
            },
        );
        fonts[font as usize] += 1;
    }
    println!("800CEB68: 600 cases, {changed} bytes, fonts {fonts:?}");
    assert!(changed > 0);
}

/// The browser's block and packet buffers in free RAM.
fn browser_state(m: &mut Machine, rng: &mut Rng) {
    let block = 0x8015_0000;
    m.sw(ov4::BLOCK, block);
    for i in (0..0x464).step_by(4) {
        m.sw(block + i, 0);
    }
    m.sw(block + 0x454, 0x8016_0000);
    m.sw(block + 0x458, 0x8017_0000);
    m.sw(block + 0x45c, 0x8016_0000);
    let ot = 0x801a_0000;
    m.sw(ot, rng.next());
    let ctx = context(m);
    m.sw(ctx + 8, ot);
    m.sw(ctx + 4, rng.next() & 1);
    message_file(m, rng, ov4::MESSAGE_FILE, 0x8000);
    text(m, rng, ov4::TEST_TEXT, 60);
    m.sw(ctx + 0x38c, rng.next() % 5);
    m.sw(ov4::FONT, rng.next() % 25);
    m.sw(ov4::GROUP, rng.next() % 0x24);
    m.sw(ov4::MESSAGE, (rng.next() % 16).wrapping_sub(1));
}

const HELD: [u32; 6] = [0, 0x10, 0x20, 0x30, 0x0f, 0xff];
const PRESSED: [u32; 8] = [0, 4, 8, 0xc, 0x40, 1, 2, 0x4f];

/// 800CCDE0 over held/pressed bits, languages, files and messages.
#[test]
#[ignore = "needs analysis/flight-runtime/ovmisc-019"]
fn message_browser_matches_original() {
    let base = base();
    let mut rng = Rng(0x000c_cde0);
    let mut changed = 0;
    for case in 0..240u32 {
        let mut m = base.clone();
        browser_state(&mut m, &mut rng);
        let ctx = context(&mut m);
        m.sw(ctx + 0x128, HELD[(case % 6) as usize]);
        m.sw(ctx + 0x12c, PRESSED[(rng.next() % 8) as usize]);
        let font = rng.next() % 25;
        let packet = 0x8016_0000;
        changed += oracle::diff(
            "800CCDE0",
            &m,
            (0x800c_cde0, 0x800c_d1b4),
            [font, packet, 0, 0],
            |m| Some(ov4::message_browser(m, &mut dispatch::call, font, packet)),
        );
    }
    println!("800CCDE0: 240 cases, {changed} bytes");
    assert!(changed > 0);
}

/// 800CD440 over every phase (0..=6), pad bits, fonts and backgrounds
/// (background tile loads run natively in both runs).
#[test]
#[ignore = "needs analysis/flight-runtime/ovmisc-019"]
fn frame_matches_original() {
    let base = base();
    let mut rng = Rng(0x000c_d440);
    let mut changed = 0;
    let mut phases = [0usize; 7];
    for case in 0..350u32 {
        let mut m = base.clone();
        browser_state(&mut m, &mut rng);
        let phase = case % 7;
        let block = m.lw(ov4::BLOCK);
        m.sw(block, phase);
        let ctx = context(&mut m);
        m.sw(ctx + 0x128, HELD[(rng.next() % 6) as usize]);
        m.sw(ctx + 0x12c, PRESSED[(rng.next() % 8) as usize]);
        m.sw(ctx + 0x3a4, rng.next() % 10);
        m.sw(ov4::BACKGROUND, rng.next() % 26);
        m.sw(ov4::TEST_X, rng.next() % 300);
        m.sw(ov4::TEST_Y, rng.next() % 500);
        changed += oracle::diff(
            "800CD440",
            &m,
            (0x800c_d440, 0x800c_db24),
            [0, 0, 0, 0],
            |m| Some(ov4::frame(m, &mut dispatch::call)),
        );
        phases[phase as usize] += 1;
    }
    println!("800CD440: 350 cases {phases:?}, {changed} bytes");
    assert!(changed > 0);
}

/// Whether `target` has a native owner (no Untranslated fault).
fn owned(base: &Machine, target: u32, args: [u32; 4]) -> bool {
    let mut m = base.clone();
    dispatch::call(&mut m, target, args);
    !matches!(m.take_fault(), Err(Fault::Untranslated { .. }))
}

/// 800CCCD0 whole, over background sets and TIM CLUT block lengths in the
/// (stale) frame buffer. Its 0x4E50-byte frame reaches below
/// `machine::STACK_LIMIT` (0x801FC000): the native `push_frame` records
/// `Fault::Address` at the frame, which the original does not have (an
/// open request), so the first faults are compared separately: the
/// native one must be exactly that while `STACK_LIMIT` stays, the
/// oracle's is the first callee without an owner. RAM (outside the guest
/// stack above the frame), scratchpad, GTE, `sp` and `v0` must agree.
#[test]
#[ignore = "needs analysis/flight-runtime/ovmisc-019"]
fn load_backgrounds_matches_original() {
    let base = base();
    let mut rng = Rng(0x000c_ccd0);
    let mut changed = 0;
    for _ in 0..40 {
        let mut m = base.clone();
        let set = rng.next() % 30;
        let frame = m.sp - 0x4e50;
        let tim = frame + 0x110;
        for i in (0..0x4d40).step_by(4) {
            m.sw(tim + i, rng.next());
        }
        m.sw(tim, 0x10);
        m.sw(tim + 4, 8);
        m.sw(tim + 8, 0xc + 4 * (rng.next() % 0x100) + (rng.next() & 3));
        let mut original = m.clone();
        let v0 = oracle::run(&mut original, 0x800c_ccd0, 0x800c_cde0, [set, 0, 0, 0]);
        let mut native = m.clone();
        let nv0 = ov4::load_backgrounds(&mut native, &mut dispatch::call, set);
        let fault = native.take_fault();
        if frame < STACK_LIMIT {
            assert_eq!(
                fault,
                Err(Fault::Address {
                    address: frame,
                    width: 4
                })
            );
        } else {
            assert_eq!(fault, original.take_fault());
        }
        let high = (m.sp & 0x1f_ffff) as usize;
        let differ: Vec<u32> = (0..0x20_0000usize)
            .filter(|&i| !(snapshot::STACK_START..high).contains(&i))
            .filter(|&i| original.ram()[i] != native.ram()[i])
            .map(|i| 0x8000_0000 + i as u32)
            .collect();
        assert!(differ.is_empty(), "800CCCD0 set {set}: {differ:08x?}");
        assert_eq!(original.scratch(), native.scratch());
        assert!(original.gte == native.gte);
        assert_eq!((original.sp, v0), (native.sp, nv0));
        changed += (0..0x20_0000usize)
            .filter(|&i| m.ram()[i] != original.ram()[i])
            .count();
    }
    println!("800CCCD0: 40 cases, {changed} bytes");
    assert!(changed > 0);
}

/// 800CD1B4 whole. Its 0x8180-byte frame reaches below
/// `machine::STACK_LIMIT`, and the oracle reaches the arena allocator
/// 80026F70 through `dispatch::call`, which has no owner yet: both are open
/// requests. Until they are met this test only reports them
/// ([`setup_until_first_upload_matches_original`] covers the captured
/// part).
#[test]
#[ignore = "needs analysis/flight-runtime/ovmisc-019"]
fn setup_matches_original() {
    let base = base();
    let deep = base.sp - 0x8180 - 0x400;
    let arena = owned(&base, 0x8002_6f70, [4, 0, 0, 0]);
    if deep < STACK_LIMIT || !arena {
        println!(
            "skipped: 800CD1B4 needs the stack down to {deep:#x} (limit {STACK_LIMIT:#x}) and a dispatch owner for 80026F70 ({arena})"
        );
        return;
    }
    let mut rng = Rng(0x000c_d1b4);
    let mut changed = 0;
    for _ in 0..20 {
        let mut m = base.clone();
        browser_state(&mut m, &mut rng);
        changed += oracle::diff("800CD1B4", &m, (0x800c_d1b4, 0x800c_d440), [0; 4], |m| {
            Some(ov4::setup(m, &mut dispatch::call))
        });
    }
    println!("800CD1B4: 20 cases, {changed} bytes");
    assert!(changed > 0);
}
