//! PadInitDirect 8009C2A0's RAM effects (`boot::pad_init_direct`) against
//! two cold boots of the original (`tools/capture-boot-writers-linux.py`):
//! `mainloop-padinit-001` stopped at its entry (`--stop 0x8009C2A0`) and
//! `mainloop-padinit-002` stopped at its return into 800260AC
//! (`--stop 0x80026130`), which also records the last writer of every byte.
//! The emulator is deterministic, so both boots share the timeline up to the
//! entry.
//!
//! Every RAM byte outside the stack must match the return image, except the
//! bytes that code other than PadInitDirect and its callees (interrupt
//! handlers) changed in between; those must keep their entry value.
use ac3_flight::{Machine, boot};
use std::{fs, path::PathBuf};

fn capture(name: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR"))
        .join("analysis/flight-runtime")
        .join(name)
}

/// PadInitDirect and the routines it calls: _padInitDirSeq 8009C900, bzero
/// 800AC018, _padSetVsyncParam 80099EF8.
const CODE: [std::ops::Range<u32>; 4] = [
    0x8009_c2a0..0x8009_c400,
    0x8009_c900..0x8009_c934,
    0x800a_c018..0x800a_c048,
    0x8009_9ef8..0x8009_9f24,
];

/// The stack (excluded from every machine comparison).
const STACK: usize = 0x1f_e000;

#[test]
#[ignore = "requires AC3_DATA_DIR and analysis/flight-runtime/mainloop-padinit-001/-002"]
fn pad_init_direct_8009c2a0() {
    let entry = fs::read(capture("mainloop-padinit-001").join("main-loop.ram.bin")).unwrap();
    let exit_directory = capture("mainloop-padinit-002");
    let exit = fs::read(exit_directory.join("main-loop.ram.bin")).unwrap();
    let writers: Vec<u32> = fs::read(exit_directory.join("boot-writers.bin"))
        .unwrap()
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    let mut m = Machine::from_images(&entry, &[0; 0x400]).unwrap();
    assert_eq!(m.lw(0x800b_2518), 0, "entry image before PadInitDirect");
    let first = m.lw(ac3_flight::GP + 0x10);
    let second = first + 0x22;
    assert_eq!(boot::pad_init_direct(&mut m, first, second), 1);
    assert!(m.take_fault().is_ok());
    let native = m.ram();
    let ours = |i: usize| CODE.iter().any(|r| r.contains(&writers[i]));
    let mut written = 0;
    let mut interrupts = 0;
    let mut wrong = Vec::new();
    for i in 0..STACK {
        if exit[i] != entry[i] && ours(i) {
            written += 1;
        }
        if native[i] == exit[i] {
            continue;
        }
        if !ours(i) && exit[i] != entry[i] && native[i] == entry[i] {
            interrupts += 1;
            continue;
        }
        wrong.push(format!(
            "{:08x}: native {:02x} original {:02x} (entry {:02x}, writer {:08x})",
            0x8000_0000 + i as u32,
            native[i],
            exit[i],
            entry[i],
            writers[i]
        ));
    }
    assert!(
        wrong.is_empty(),
        "{} bytes differ:\n{}",
        wrong.len(),
        wrong[..wrong.len().min(40)].join("\n")
    );
    for (address, routine) in [(0x800b_2518, 0x8009_c8dc), (0x800b_2520, 0x8009_cd88)] {
        assert_eq!(m.lw(address), routine, "{address:08x}");
    }
    eprintln!(
        "PadInitDirect: {written} bytes changed by it, exact; {interrupts} changed by other code left alone"
    );
    assert!(written > 40, "{written}");
}
