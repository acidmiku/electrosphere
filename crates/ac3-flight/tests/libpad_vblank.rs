//! libpad's VBlank handler (`mainloop::libpad`, 80099F8C) against cold
//! boots of the original with each kind of controller on port 1 and
//! nothing on port 2 (`analysis/flight-runtime`, captured with
//! `tools/capture-pad-boot-linux.py`):
//!
//! - `mainloop-pad-005`: a digital pad;
//! - `mainloop-pad-006`: a DualShock in analog mode;
//! - `mainloop-pad-007`: a DualShock in digital mode, which the game
//!   switches to analog (PadSetMainMode).
//!
//! Each records libpad's RAM (driver words 0x800B2500.., BSS 0x800C59D0..),
//! the game's pad block 0x800BE1E0.. (receive buffers, controller records
//! with the actuator alignment) and pad states 0x800BD1A8.. (actuator
//! data) at the entry and return of
//! every handler call from PadStartCom on (742 calls, frames 156..899), with
//! DOWN held on frames 300..306 and CROSS on 400..406, 500..506, 600..606.
//! Every call runs natively from the original's entry RAM and must give its
//! return RAM, except root counter 2's wait start/length (cycle timing).
use ac3_flight::mainloop::libpad::{self, Controller, Sio};
use std::{fs, path::PathBuf};

const RANGES: [(u32, usize); 4] = [
    (0x800b_2500, 0x90),
    (0x800c_59d0, 0x2f0),
    (0x800b_e1e0, 0xd0),
    (0x800b_d1a8, 0xd0),
];

fn timing(address: u32) -> bool {
    (libpad::WAIT_START..libpad::WAIT_LENGTH + 4).contains(&address)
}

fn capture(name: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR"))
        .join("analysis/flight-runtime")
        .join(name)
}

/// (frame, 0 entry / 1 return, the ranges' bytes) per handler call.
fn snapshots(name: &str) -> Vec<(u32, u32, Vec<u8>)> {
    let bytes = fs::read(capture(name).join("handler.bin")).unwrap();
    let size = 8 + RANGES.iter().map(|r| r.1).sum::<usize>();
    bytes
        .chunks_exact(size)
        .map(|chunk| {
            let word = |i: usize| u32::from_le_bytes(chunk[i..i + 4].try_into().unwrap());
            (word(0), word(4), chunk[8..].to_vec())
        })
        .collect()
}

/// The capture's held buttons at `frame` (libpad active-high bits).
fn buttons(frame: u32) -> u16 {
    let mut held = 0;
    if (300..307).contains(&frame) {
        held |= 1 << 6;
    }
    if [400u32, 500, 600]
        .iter()
        .any(|&start| (start..start + 7).contains(&frame))
    {
        held |= 1 << 14;
    }
    held
}

fn check(name: &str, controller: Controller) -> usize {
    let mut ram = fs::read(capture(name).join("entry.ram.bin")).unwrap();
    let mut sio = Sio::new([Some(controller), None]);
    let calls = snapshots(name);
    let mut changed = 0;
    for (call, pair) in calls.chunks_exact(2).enumerate() {
        let ((frame, 0, before), (_, 1, after)) = (&pair[0], &pair[1]) else {
            panic!("{name}: call {call}: unpaired snapshots");
        };
        let mut offset = 0;
        for (base, length) in RANGES {
            let at = (base & 0x1f_ffff) as usize;
            ram[at..at + length].copy_from_slice(&before[offset..offset + length]);
            offset += length;
        }
        if let Some(pad) = &mut sio.ports[0] {
            pad.buttons = buttons(*frame);
        }
        libpad::vblank(&mut ram, &mut sio);
        assert_eq!(sio.error, None, "{name}: call {call} (frame {frame})");
        let mut differences = Vec::new();
        let mut offset = 0;
        for (base, length) in RANGES {
            let at = (base & 0x1f_ffff) as usize;
            for i in 0..length {
                let address = base + i as u32;
                let (native, original) = (ram[at + i], after[offset + i]);
                if before[offset + i] != original {
                    changed += 1;
                }
                if native != original && !timing(address) {
                    differences.push(format!(
                        "{address:08x}: native {native:02x} original {original:02x} (entry {:02x})",
                        before[offset + i]
                    ));
                }
            }
            offset += length;
        }
        assert!(
            differences.is_empty(),
            "{name}: call {call} (frame {frame}): {} bytes differ\n{}",
            differences.len(),
            differences[..differences.len().min(40)].join("\n")
        );
    }
    eprintln!(
        "{name}: {} handler calls exact, {changed} bytes changed",
        calls.len() / 2
    );
    calls.len() / 2
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/mainloop-pad-005"]
fn digital_pad() {
    assert_eq!(check("mainloop-pad-005", Controller::digital()), 742);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/mainloop-pad-006"]
fn dualshock_analog() {
    assert_eq!(check("mainloop-pad-006", Controller::dualshock(true)), 742);
}

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/mainloop-pad-007"]
fn dualshock_digital_mode() {
    assert_eq!(check("mainloop-pad-007", Controller::dualshock(false)), 742);
}
