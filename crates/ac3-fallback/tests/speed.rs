//! Validation (c): interpreter throughput (instructions per second) on the
//! PAL machine, release build.
//!
//! * a synthetic checksum loop over 1 MiB of RAM (loads, ALU, a branch per
//!   word);
//! * PAL's own routines from the resident image, run as interpreted call
//!   trees with the fallback forcing them: `memcpy` (800AC108) copying
//!   256 KiB, `rsin`/`rcos` (800A45C8/800A4698, table lookups), `ratan2`
//!   (800A6088), and libgte's `RotTransPers` (800A5828, GTE commands),
//!   each from an interpreted driver loop.
mod support;

use ac3_fallback::{Fallback, Version};
use ac3_flight::Machine;
use std::time::Instant;
use support::{Pal, call, data_dir};

fn machine() -> Machine {
    let mut ram = vec![0u8; 0x20_0000];
    let resident = std::fs::read(data_dir().join("extracted/programs/ACE.unpacked.bin"))
        .expect("PAL resident image");
    ram[0x1_0000..0x1_0000 + resident.len()].copy_from_slice(&resident);
    Machine::from_images(&ram, &[0; 0x400]).unwrap()
}

fn report(label: &str, fallback: &Fallback, seconds: f64) -> f64 {
    let instructions = fallback.instructions();
    let rate = instructions as f64 / seconds / 1e6;
    eprintln!("{label:<44} {instructions:>11} instructions in {seconds:6.3} s: {rate:6.1} M/s");
    rate
}

const DRIVER: u32 = 0x8018_0000;

fn jal(target: u32) -> u32 {
    3 << 26 | (target >> 2 & 0x03ff_ffff)
}

/// Run the code at DRIVER (a loop that calls `target` `count` times with
/// a0 = the counter) interpreted, with `target`'s call tree forced too.
fn driver(m: &mut Machine, target: u32, spec: &str, count: u32, a1: u32) -> (Fallback, f64) {
    let words = [
        0x03e0_8821,                    // addu s1, ra, zero (the driver calls)
        0x3410_0000 | (count & 0xffff), // ori s0, zero, count
        0x0010_2021,                    // loop: addu a0, zero, s0
        jal(target),                    // jal target
        0x2405_0000 | (a1 & 0xffff),    // addiu a1, zero, a1 (delay slot)
        0x2610_ffff,                    // addiu s0, s0, -1
        0x1600_fffb,                    // bne s0, zero, loop
        0,
        0x0220_0008, // jr s1
        0,
    ];
    for (i, w) in words.iter().enumerate() {
        m.sw(DRIVER + 4 * i as u32, *w);
    }
    let mut fallback = Fallback::new(Version::Pal);
    fallback.force(spec).unwrap();
    let started = Instant::now();
    let f = &fallback;
    f.run_range(
        &mut Pal(m),
        (DRIVER, DRIVER + 40),
        [0; 4],
        &mut |g: &mut Pal<'_>, t, a| call(f, true, g, t, a),
    );
    let seconds = started.elapsed().as_secs_f64();
    assert!(m.take_fault().is_ok());
    (fallback, seconds)
}

#[test]
#[ignore = "requires AC3_DATA_DIR (the PAL resident image); a measurement"]
fn interpreter_throughput() {
    // Synthetic: sum and xor of 1 MiB, 16 times.
    let mut m = machine();
    let program = [
        0x2412_0010, // addiu s2, zero, 16
        0x3c08_8010, // outer: lui t0, 0x8010
        0x3c09_8020, // lui t1, 0x8020
        0x8d0a_0000, // inner: lw t2, 0(t0)
        0x0142_1021, // addu v0, t2, v0
        0x0043_1826, // xor v1, v0, v1
        0x2508_0004, // addiu t0, t0, 4
        0x1509_fffb, // bne t0, t1, inner
        0x0003_1840, // sll v1, v1, 1 (delay slot)
        0x2652_ffff, // addiu s2, s2, -1
        0x1640_fff6, // bne s2, zero, outer
        0,
        0x03e0_0008, // jr ra
        0,
    ];
    for (i, w) in program.iter().enumerate() {
        m.sw(DRIVER + 4 * i as u32, *w);
    }
    let fallback = Fallback::new(Version::Pal);
    let started = Instant::now();
    fallback.run_range(
        &mut Pal(&mut m),
        (DRIVER, DRIVER + 56),
        [0; 4],
        &mut |_: &mut Pal<'_>, _, _| 0,
    );
    let synthetic = report(
        "synthetic checksum loop (1 MiB x 16)",
        &fallback,
        started.elapsed().as_secs_f64(),
    );

    let mut m = machine();
    // memcpy(0x80180400.., 0x80100000.., 256 KiB) ten times.
    let words = [
        0x03e0_8821,      // addu s1, ra, zero
        0x2410_000a,      // addiu s0, zero, 10
        0x3c04_8018,      // loop: lui a0, 0x8018
        0x3484_0400,      // ori a0, a0, 0x400
        0x3c05_8010,      // lui a1, 0x8010
        jal(0x800a_c108), // memcpy
        0x3c06_0004,      // lui a2, 4 (delay slot: 256 KiB)
        0x2610_ffff,      // addiu s0, s0, -1
        0x1600_fff9,      // bne s0, zero, loop
        0,
        0x0220_0008, // jr s1
        0,
    ];
    for (i, w) in words.iter().enumerate() {
        m.sw(DRIVER + 4 * i as u32, *w);
    }
    let mut fallback = Fallback::new(Version::Pal);
    fallback.force("800ac108").unwrap();
    let started = Instant::now();
    let f = &fallback;
    f.run_range(
        &mut Pal(&mut m),
        (DRIVER, DRIVER + 48),
        [0; 4],
        &mut |g: &mut Pal<'_>, t, a| call(f, true, g, t, a),
    );
    let memcpy = report(
        "memcpy 256 KiB x 10",
        &fallback,
        started.elapsed().as_secs_f64(),
    );
    assert_eq!(m.lw(0x8018_0400), m.lw(0x8010_0000));

    let mut rates = vec![synthetic, memcpy];
    for (label, target, spec, count, a1) in [
        (
            "rsin x 50000",
            0x800a_45c8,
            "rsin,GEO_00_OBJ_2C,sin_1,GEO_00_OBJ_C4",
            50_000,
            0,
        ),
        (
            "rcos x 50000",
            0x800a_4698,
            "rcos,GEO_01_OBJ_98,sin_1,GEO_00_OBJ_C4",
            50_000,
            0,
        ),
        ("ratan2(n, 77) x 50000", 0x800a_6088, "sdk", 50_000, 77),
    ] {
        let mut m = machine();
        let spec: String = spec
            .split(',')
            .map(|name| {
                Fallback::new(Version::Pal)
                    .routines()
                    .find(|(_, module, e)| *module == 0 && e.name == name)
                    .map_or(name.to_owned(), |(_, _, e)| format!("{:x}", e.start))
            })
            .collect::<Vec<_>>()
            .join(",");
        let (fallback, seconds) = driver(&mut m, target, &spec, count, a1);
        rates.push(report(label, &fallback, seconds));
    }
    // RotTransPers(SVECTOR, &sxy, &p, &flag) on a rotation set up once.
    let mut m = machine();
    for (i, v) in [0x1000u32, 0, 0x1000, 0, 0x1000].iter().enumerate() {
        m.gte.ctc2(i, *v);
    }
    m.gte.ctc2(7, 1000); // TRZ
    m.gte.ctc2(26, 256); // H
    m.sw(0x8017_0000, 0x0020_0010); // SVECTOR (16, 32, 48)
    m.sw(0x8017_0004, 0x30);
    let words = [
        0x03e0_8821,      // addu s1, ra, zero
        0x3410_c350,      // ori s0, zero, 50000
        0x3c04_8017,      // loop: lui a0, 0x8017
        0x3c05_8017,      // lui a1, 0x8017
        0x34a5_0010,      // ori a1, a1, 0x10
        0x3c06_8017,      // lui a2, 0x8017
        0x34c6_0020,      // ori a2, a2, 0x20
        0x3c07_8017,      // lui a3, 0x8017
        jal(0x800a_5828), // RotTransPers
        0x34e7_0030,      // ori a3, a3, 0x30 (delay slot)
        0x2610_ffff,      // addiu s0, s0, -1
        0x1600_fff6,      // bne s0, zero, loop
        0,
        0x0220_0008, // jr s1
        0,
    ];
    for (i, w) in words.iter().enumerate() {
        m.sw(DRIVER + 4 * i as u32, *w);
    }
    let mut fallback = Fallback::new(Version::Pal);
    fallback.force("800a5828").unwrap();
    let started = Instant::now();
    let f = &fallback;
    f.run_range(
        &mut Pal(&mut m),
        (DRIVER, DRIVER + 60),
        [0; 4],
        &mut |g: &mut Pal<'_>, t, a| call(f, true, g, t, a),
    );
    rates.push(report(
        "RotTransPers x 50000 (GTE)",
        &fallback,
        started.elapsed().as_secs_f64(),
    ));
    assert!(m.take_fault().is_ok());
    assert!(rates.iter().all(|&r| r > 5.0), "{rates:?}");
}
