//! CompMatrix 800A4C18, RotMatrix_gte 800A5E78 and StoreImage2 800A8E9C in
//! `sdk.rs`.
//!
//! Captures (`analysis/flight-runtime`): `mainloop-gte-ov2brief-calltree-001`
//! (the briefing, `ov2brief-calltree-001`'s inputs) and
//! `mainloop-gte-ov2air-003` (the aircraft overlay, `ov2air-003`'s inputs),
//! each with 20 CompMatrix and 20 RotMatrix_gte calls, compared exactly
//! (`snapshot::check_call`, plus the output matrix, which callers keep on
//! the stack). The oracle (`support::oracle`) covers other angles and
//! matrices. StoreImage2 (device polls; the oracle cannot run it) is
//! compared on its one captured call (`mainloop-storeimage2-001`, the
//! menu with `ov2core-002`'s inputs, field 282), with the VRAM words the
//! original's DMA delivered supplied by a test device, and has a unit test.
mod support;

use ac3_flight::{
    Device, Machine,
    machine::HostCall,
    sdk,
    snapshot::{self, Snapshot},
};
use support::oracle;

const CAPTURES: [&str; 2] = [
    "mainloop-gte-ov2brief-calltree-001",
    "mainloop-gte-ov2air-003",
];

/// Every captured call of `callee`, exact, plus `length` bytes at the
/// output matrix (argument register `out`).
fn check_all(
    name: &str,
    callee: u32,
    out: usize,
    length: u32,
    run: impl Fn(&mut Machine, &Snapshot) -> u32,
) -> usize {
    let index_file = oracle::capture(name);
    let index = snapshot::read_index(&index_file).unwrap();
    let directory = index_file.parent().unwrap();
    let entries: Vec<_> = index
        .iter()
        .filter(|s| s.callee == callee && s.stage == "entry")
        .collect();
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
        run(&mut m, &entry);
        let address = entry.gpr(out);
        let native = m.read_bytes(address, length as usize);
        let original: Vec<u8> = (0..length)
            .map(|i| original_byte(&exit, address.wrapping_add(i)))
            .collect();
        assert_eq!(
            native, original,
            "{name} {callee:08x} #{occurrence}: matrix"
        );
    }
    println!("{name} {callee:08x}: {} calls exact", entries.len());
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

#[test]
#[ignore = "requires AC3_DATA_DIR and the mainloop-gte-* captures"]
fn captured_comp_matrix_and_rot_matrix_gte() {
    let mut calls = [0; 2];
    for name in CAPTURES {
        calls[0] += check_all(name, 0x800a_4c18, 6, 0x20, |m, e| {
            sdk::comp_matrix(m, e.gpr(4), e.gpr(5), e.gpr(6))
        });
        calls[1] += check_all(name, 0x800a_5e78, 5, 0x12, |m, e| {
            sdk::rot_matrix_gte(m, e.gpr(4), e.gpr(5))
        });
    }
    assert_eq!(calls, [40, 40]);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the mainloop-gte-* captures"]
fn oracle_rot_matrix_gte_and_comp_matrix() {
    let entry = oracle::entries(CAPTURES[0], 0x800a_5e78)
        .into_iter()
        .next()
        .unwrap();
    let base = entry.machine();
    let (angles, matrix, m0, m1, m2) = (
        0x801f_0000,
        0x801f_0010,
        0x801f_0040,
        0x801f_0080,
        0x801f_00c0,
    );
    let mut rng = oracle::Rng(0x1234_5678);
    let mut changed = 0;
    let special = [
        0i16, 1, -1, 0x400, -0x400, 0x800, -0x800, 0xfff, -0x1000, 0x7fff, -0x8000,
    ];
    for case in 0..300 {
        let mut m = base.clone();
        let pick = |rng: &mut oracle::Rng| {
            if case < 121 {
                special[rng.next() as usize % special.len()]
            } else {
                rng.next() as i16
            }
        };
        let [x, y, z] = [pick(&mut rng), pick(&mut rng), pick(&mut rng)];
        for (i, v) in [x, y, z].into_iter().enumerate() {
            m.sh(angles + i as u32 * 2, v as u16);
        }
        changed += oracle::diff(
            "RotMatrix_gte",
            &m,
            (0x800a_5e78, 0x800a_6088),
            [angles, matrix, 0, 0],
            |m| Some(sdk::rot_matrix_gte(m, angles, matrix)),
        );
    }
    for case in 0..200 {
        let mut m = base.clone();
        for address in [m0, m1] {
            for i in 0..8 {
                let word = if i < 5 {
                    // 4.12 rotation elements (some at the 16-bit limits).
                    let a = rng.next() as u16 as i16 >> (rng.next() % 4);
                    let b = rng.next() as u16 as i16 >> (rng.next() % 4);
                    (a as u16 as u32) | ((b as u16 as u32) << 16)
                } else {
                    rng.next() as i32 as u32 >> (rng.next() % 12)
                };
                m.sw(address + i * 4, word);
            }
        }
        // Aliasing as the callers use it: m2 = m0 or m1 now and then.
        let out = match case % 5 {
            0 => m0,
            1 => m1,
            _ => m2,
        };
        changed += oracle::diff(
            "CompMatrix",
            &m,
            (0x800a_4c18, 0x800a_4d78),
            [m0, m1, out, 0],
            |m| Some(sdk::comp_matrix(m, m0, m1, out)),
        );
    }
    println!("oracle: 500 cases, {changed} bytes changed");
    assert!(changed > 5000);
}

/// Answers StoreImage's transfer (host call 800A6970 `[rect, pixels]`)
/// with the pixel words the original's DMA wrote (from its return image).
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

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/mainloop-storeimage2-001"]
fn captured_store_image2() {
    let index_file = oracle::capture("mainloop-storeimage2-001");
    let index = snapshot::read_index(&index_file).unwrap();
    let exit_info = index
        .iter()
        .find(|s| s.callee == 0x800a_8e9c && s.stage == "return")
        .unwrap();
    let exit = Snapshot::load(index_file.parent().unwrap(), exit_info).unwrap();
    let changed = snapshot::check_call(&index_file, 0x800a_8e9c, 0, |m, e| {
        m.device = Some(Box::new(Vram(exit.ram.clone())));
        Some(sdk::store_image2(m, e.gpr(4), e.gpr(5)))
    })
    .unwrap();
    println!("StoreImage2: {changed} original bytes changed");
    assert!(changed > 0);
}

#[test]
fn store_image2_arms_the_timeout_and_transfers() {
    let mut m = Machine::default();
    // libgpu's table with the standard StoreImage transfer at +0x1C, VRAM
    // limits 1024x512, and a VBlank count.
    m.sw(0x800b_97dc, 0x8010_8800);
    m.sw(0x8010_8800 + 0x1c, 0x800a_8068);
    m.sh(sdk::DRAW_LIMIT_WIDTH, 1024);
    m.sh(sdk::DRAW_LIMIT_HEIGHT, 512);
    m.sw(sdk::VSYNC_COUNTER, 100);
    // DMACallback's setter through the interrupt table (800A1124).
    m.sw(0x800b_3fb4, 0x8010_9000);
    m.sw(0x8010_9004, 0x800a_1124);
    let (rect, pixels) = (0x8010_0000, 0x8010_1000);
    for (i, half) in [16u16, 32, 2000, 8].into_iter().enumerate() {
        m.sh(rect + i as u32 * 2, half);
    }
    assert_eq!(
        ac3_flight::dispatch::call(&mut m, 0x800a_8e9c, [rect, pixels, 0, 0]),
        0
    );
    assert_eq!(m.lw(sdk::GPU_TIMEOUT_DEADLINE), 100 + 0xf0);
    assert_eq!(m.lw(sdk::GPU_TIMEOUT_POLLS), 0);
    assert_eq!(m.lw(sdk::DMA_CALLBACKS + 2 * 4), 0x800a_91c8);
    assert_eq!(m.lhu(rect + 4), 1024, "width clamped to the VRAM");
    let last = *m.host_calls.last().unwrap();
    assert_eq!(
        (last.function, last.args),
        (0x800a_6970, [rect, pixels, 0, 0])
    );
    assert!(m.take_fault().is_ok());
}
