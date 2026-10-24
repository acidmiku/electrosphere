//! sprintf 800ACD28 and its libc helpers (strlen 800AC328, memchr 800AC0B8,
//! memmove 800AD5A8), strcat 800AC1B8 and TransMatrix 800A53A8 in `sdk.rs`.
//!
//! Captures (`analysis/flight-runtime`):
//! * `mainloop-sprintf-001`: the aircraft overlay (mission-004 checkpoint,
//!   `ov2air-003`'s inputs): 30 sprintf calls (`%2d.%2d`, `%5d`, `: %s`,
//!   `(%4d,%4d)`), 15 strlen, 10 memmove.
//! * `mainloop-sprintf-002`: the briefing (mission-002 checkpoint,
//!   `ov2brief-calltree-001`'s inputs): 30 sprintf (`%2d:%2d`), 20 strlen,
//!   20 TransMatrix, 10 memmove.
//!
//! Every captured call is compared exactly (`snapshot::check_call`). The
//! conversions and flags no capture uses, memchr and strcat (no captured
//! calls) are compared with the test oracle (`support::oracle`), which
//! interprets the original instructions from a captured machine.
mod support;

use ac3_flight::{
    Machine, sdk,
    snapshot::{self, Snapshot},
};
use support::oracle;

/// Every captured call of `callee` in capture `name`: `snapshot::check_call`
/// (RAM outside the stack, scratchpad, GTE, v0), plus the bytes `output`
/// names (address, length) in the original's return image, since the
/// callers' buffers are often on the stack.
fn check_all(
    name: &str,
    callee: u32,
    output: impl Fn(&Snapshot, u32) -> (u32, u32),
    run: impl Fn(&mut Machine, &Snapshot) -> u32,
) -> usize {
    let index_file = oracle::capture(name);
    let index = snapshot::read_index(&index_file).unwrap();
    let directory = index_file.parent().unwrap();
    let entries: Vec<_> = index
        .iter()
        .filter(|s| s.callee == callee && s.stage == "entry")
        .collect();
    let mut compared = 0;
    for (occurrence, entry_info) in entries.iter().enumerate() {
        snapshot::check_call(&index_file, callee, occurrence, |m, e| Some(run(m, e)))
            .unwrap_or_else(|e| panic!("{name} {callee:08x} #{occurrence}: {e}"));
        let exit_info = index
            .iter()
            .find(|s| {
                s.callee == callee
                    && s.stage == "return"
                    && s.sequence > entry_info.sequence
                    && s.ra == entry_info.ra
                    && s.registers.gpr[29] == entry_info.registers.gpr[29]
            })
            .unwrap();
        let entry = Snapshot::load(directory, entry_info).unwrap();
        let exit = Snapshot::load(directory, exit_info).unwrap();
        let mut m = entry.machine();
        let v0 = run(&mut m, &entry);
        let (address, length) = output(&entry, v0);
        let native = m.read_bytes(address, length as usize);
        let original: Vec<u8> = (0..length)
            .map(|i| original_byte(&exit, address.wrapping_add(i)))
            .collect();
        assert_eq!(
            native, original,
            "{name} {callee:08x} #{occurrence}: output"
        );
        compared += length as usize;
    }
    println!(
        "{name} {callee:08x}: {} calls exact, {compared} output bytes compared",
        entries.len()
    );
    entries.len()
}

/// A byte of a snapshot: scratchpad (0x1F800000..) or main RAM.
fn original_byte(snapshot: &Snapshot, address: u32) -> u8 {
    if address & 0x1fff_fc00 == 0x1f80_0000 {
        snapshot.scratch[(address & 0x3ff) as usize]
    } else {
        snapshot.ram[(address & 0x1f_ffff) as usize]
    }
}

fn args(e: &Snapshot) -> [u32; 4] {
    [4, 5, 6, 7].map(|r| e.gpr(r))
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/mainloop-sprintf-{001,002}"]
fn captured_sprintf_800acd28() {
    let mut calls = 0;
    for name in ["mainloop-sprintf-001", "mainloop-sprintf-002"] {
        calls += check_all(
            name,
            0x800a_cd28,
            |e, length| (e.gpr(4), length + 1),
            |m, e| {
                let [a0, a1, a2, a3] = args(e);
                sdk::sprintf(m, a0, a1, a2, a3)
            },
        );
    }
    assert_eq!(calls, 60);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/mainloop-sprintf-{001,002}"]
fn captured_strlen_memmove_trans_matrix() {
    let mut calls = [0; 3];
    for name in ["mainloop-sprintf-001", "mainloop-sprintf-002"] {
        calls[0] += check_all(
            name,
            0x800a_c328,
            |e, _| (e.gpr(4), 0),
            |m, e| sdk::strlen(m, e.gpr(4)),
        );
        calls[1] += check_all(
            name,
            0x800a_d5a8,
            |e, _| (e.gpr(4), e.gpr(6).min(0x1000)),
            |m, e| {
                let [a0, a1, a2, _] = args(e);
                sdk::memmove(m, a0, a1, a2)
            },
        );
    }
    calls[2] += check_all(
        "mainloop-sprintf-002",
        0x800a_53a8,
        |e, _| (e.gpr(4) + 0x14, 12),
        |m, e| sdk::trans_matrix(m, e.gpr(4), e.gpr(5)),
    );
    assert_eq!(calls, [35, 20, 20]);
}

// ---- oracle --------------------------------------------------------------------------

/// Free RAM in the captured machine for the cases (strings, buffers).
const FORMAT: u32 = 0x801f_0000;
const OUTPUT: u32 = 0x801f_0100;
const STRINGS: u32 = 0x801f_0400;
const COUNTED: u32 = 0x801f_0480;

fn base() -> Machine {
    let entry = oracle::entries("mainloop-sprintf-001", 0x800a_cd28)
        .into_iter()
        .next()
        .expect("a captured sprintf entry");
    let mut m = entry.machine();
    m.write_bytes(STRINGS, b"hello world\0");
    m.write_bytes(COUNTED, b"\x05Pascal string");
    m.write_bytes(OUTPUT, &[0xaa; 0x100]);
    m
}

/// One sprintf case: `words` are the variadic words (a2, a3, then the
/// caller's argument area at sp+0x10).
fn case(base: &Machine, format: &str, words: &[u32]) -> usize {
    let mut m = base.clone();
    let mut bytes = format.as_bytes().to_vec();
    bytes.push(0);
    m.write_bytes(FORMAT, &bytes);
    let word = |i: usize| words.get(i).copied().unwrap_or(0);
    for (i, &value) in words.iter().enumerate().skip(2) {
        let address = m.sp + 0x10 + (i as u32 - 2) * 4;
        m.sw(address, value);
    }
    let (a2, a3) = (word(0), word(1));
    oracle::diff(
        format,
        &m,
        (0x800a_cd28, 0x800a_d5a8),
        [OUTPUT, FORMAT, a2, a3],
        |m| Some(sdk::sprintf(m, OUTPUT, FORMAT, a2, a3)),
    )
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/mainloop-sprintf-001"]
fn oracle_sprintf_conversions() {
    let m = base();
    let numbers = [
        0u32,
        1,
        7,
        42,
        -1i32 as u32,
        -42i32 as u32,
        12345,
        0x7fff_ffff,
        0x8000_0000,
        0xffff_8000,
        0x1_2345,
        0xdead_beef,
    ];
    let integer_formats = [
        "%d", "%i", "%5d|", "%-5d|", "%05d", "%+d", "% d", "% 05d", "%+05d", "%-05d|", "%.3d",
        "%5.3d|", "%08.3d", "%*d", "%-*d|", "%.*d", "%hd", "%hu", "%ld", "%Ld", "%u", "%5u",
        "%05u", "%o", "%#o", "%08o", "%#08o", "%x", "%X", "%#x", "%#X", "%08x", "%#08x", "%.6x",
        "%hx", "%p", "%#p", "%c", "%3c|", "%-3c|", "%lhx",
    ];
    let mut cases = 0;
    let mut changed = 0;
    for format in integer_formats {
        for &n in &numbers {
            // For `*`, the first word is the width or precision.
            let words = if format.contains('*') {
                vec![-7i32 as u32, n]
            } else {
                vec![n, 0x1234]
            };
            changed += case(&m, format, &words);
            cases += 1;
        }
    }
    for (format, words) in [
        ("%s", vec![STRINGS]),
        ("%15s|", vec![STRINGS]),
        ("%-15s|", vec![STRINGS]),
        ("%.5s", vec![STRINGS]),
        ("%.20s", vec![STRINGS]),
        ("%3.1s|", vec![STRINGS]),
        ("%#s", vec![COUNTED]),
        ("%#.3s", vec![COUNTED]),
        ("%#9s|", vec![COUNTED]),
        ("%.*s", vec![4, STRINGS]),
        ("ab%ncd", vec![OUTPUT + 0x80]),
        ("abc%hn", vec![OUTPUT + 0x80]),
        ("100%%", vec![]),
        ("%5%|", vec![]),
        ("x%qy", vec![1]),
        ("%", vec![]),
        ("plain", vec![]),
        ("", vec![]),
        (
            "%d %s %c %x %5.2d",
            vec![-3i32 as u32, STRINGS, 0x41, 0xbeef, 9],
        ),
        ("%2d:%2d %-4d|%04X", vec![3, 7, 12, 0xab]),
    ] {
        changed += case(&m, format, &words);
        cases += 1;
    }
    println!("oracle sprintf: {cases} cases, {changed} bytes changed");
    assert!(cases > 500 && changed > 2000);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/mainloop-sprintf-001"]
fn oracle_string_helpers_and_trans_matrix() {
    let m = base();
    let mut changed = 0;
    for (s, c, n) in [
        (STRINGS, 0x6f, 11),
        (STRINGS, 0x77, 3),
        (STRINGS, 0, 20),
        (STRINGS, 0x7a, 11),
        (STRINGS, 0x16f, 11),
        (0, 0, 5),
        (STRINGS, 0x68, 0),
        (STRINGS, 0x68, -1i32 as u32),
    ] {
        changed += oracle::diff(
            "memchr",
            &m,
            (0x800a_c0b8, 0x800a_c108),
            [s, c, n, 0],
            |m| Some(sdk::memchr(m, s, c, n)),
        );
    }
    for s in [STRINGS, COUNTED, OUTPUT, 0] {
        oracle::diff(
            "strlen",
            &m,
            (0x800a_c328, 0x800a_c35c),
            [s, 0, 0, 0],
            |m| Some(sdk::strlen(m, s)),
        );
    }
    for (dst, src, n) in [
        (OUTPUT, STRINGS, 11),
        (STRINGS + 2, STRINGS, 8),
        (STRINGS, STRINGS + 2, 8),
        (OUTPUT, STRINGS, 0),
        (OUTPUT, STRINGS, -3i32 as u32),
        (STRINGS, STRINGS, 4),
    ] {
        changed += oracle::diff(
            "memmove",
            &m,
            (0x800a_d5a8, 0x800a_d614),
            [dst, src, n, 0],
            |m| Some(sdk::memmove(m, dst, src, n)),
        );
    }
    changed += oracle::diff(
        "TransMatrix",
        &m,
        (0x800a_53a8, 0x800a_53cc),
        [OUTPUT, STRINGS, 0, 0],
        |m| Some(sdk::trans_matrix(m, OUTPUT, STRINGS)),
    );
    assert!(changed > 30, "{changed}");
}

/// strcat 800AC1B8 (its strlen calls leave the range: `sdk::strlen`).
#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/mainloop-sprintf-001"]
fn oracle_strcat() {
    let base = base();
    let mut changed = 0;
    // (dst text, dst, src): dst text written at OUTPUT first.
    let cases: [(&[u8], u32, u32); 9] = [
        (b"AC3 \0", OUTPUT, STRINGS),
        (b"\0", OUTPUT, STRINGS),
        (b"menu\0", OUTPUT, OUTPUT + 0x80),
        (b"x\0", OUTPUT, 0),
        (b"x\0", 0, STRINGS),
        // The ends coincide (src is a suffix of dst): nothing is copied.
        (b"abcdef\0", OUTPUT, OUTPUT + 3),
        (b"abcdef\0", OUTPUT, OUTPUT + 6),
        // src starting before dst in the same string: the ends coincide too.
        (b"abcdef\0", OUTPUT + 2, OUTPUT),
        (b"long destination string, \0", OUTPUT, STRINGS + 6),
    ];
    for (text, dst, src) in cases {
        let mut m = base.clone();
        m.write_bytes(OUTPUT, text);
        m.write_bytes(OUTPUT + 0x80, b"\0");
        changed += oracle::diff(
            "strcat",
            &m,
            (0x800a_c1b8, 0x800a_c25c),
            [dst, src, 0, 0],
            |m| Some(sdk::strcat(m, dst, src)),
        );
    }
    println!("strcat: {} cases, {changed} bytes changed", cases.len());
    assert!(changed >= 25, "{changed}");
}

#[test]
fn sprintf_formats_without_data() {
    let mut m = Machine::default();
    let (buf, format, text) = (0x8010_0000, 0x8010_0100, 0x8010_0200);
    m.write_bytes(text, b"AC3\0");
    // The executable's hexadecimal digit tables (80025124, 80025138).
    m.write_bytes(0x8002_5124, b"0123456789ABCDEF\0\0\0\x000123456789abcdef");
    let mut run = |format_text: &str, a2: u32, a3: u32| {
        let mut bytes = format_text.as_bytes().to_vec();
        bytes.push(0);
        m.write_bytes(format, &bytes);
        let length = sdk::sprintf(&mut m, buf, format, a2, a3);
        let out = m.read_bytes(buf, length as usize + 1);
        assert_eq!(out[length as usize], 0, "{format_text}: terminated");
        String::from_utf8(out[..length as usize].to_vec()).unwrap()
    };
    assert_eq!(run("%2d:%2d", 5, 30), " 5:30");
    assert_eq!(run("%05d|%-4d|", -42i32 as u32, 7), "-0042|7   |");
    assert_eq!(run("%#x %X", 0xbeef, 0xabc), "0xbeef ABC");
    assert_eq!(run("%s!", text, 0), "AC3!");
    assert_eq!(run("%c%c", 0x41, 0x42), "AB");
    assert_eq!(run("50%% %q", 0, 0), "50% ");
    assert_eq!(sdk::strlen(&mut m, text), 3);
    assert_eq!(sdk::memchr(&mut m, text, b'3' as u32, 3), text + 2);
    m.write_bytes(buf, b"AB\0");
    assert_eq!(sdk::strcat(&mut m, buf, text), buf);
    assert_eq!(m.read_bytes(buf, 6), b"ABAC3\0");
    assert_eq!(sdk::strcat(&mut m, buf, 0), 0);
    assert!(m.take_fault().is_ok());
}
