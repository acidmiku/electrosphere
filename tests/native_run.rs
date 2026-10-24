//! End-to-end native run from the original's first resident state-2 main-loop
//! top (`analysis/flight-runtime/native-run-001`, `loop-0001` at field 83,
//! captured with `tools/redux-loop-snapshot.lua`): the translated main loop
//! runs state-2 loading, gameplay initialization and flight with the host
//! devices of `machine_player`, under the capture's pad input (UP held on
//! fields 330..370, RIGHT on 400..440). At each captured field the native
//! VRAM is compared with the original's. Native screens (PNG) and a JSON
//! report go to `AC3_NATIVE_RUN_OUT` (default `target/native-run-001`).
//!
//! This is a progress gauge while translation continues: it reports the
//! first fault (an untranslated routine) and the comparisons reached, and
//! asserts only that the run starts. Its frames cannot match the original's
//! exactly: native loading is 91 fields shorter (gameplay initialization
//! 80032510 runs at native field 214 against the original's 305), so flight
//! starts from another `rand` state (800C9D30), draw-buffer parity
//! (800BD15C) and loading-screen state.
//!
//! `native_flight_from_original_flight_start` is the strict check: it starts
//! from the original's first flight main-loop top and requires the original's
//! RAM (outside host and timing state) and displayed frame after every
//! iteration.
use ac3_flight::Machine;
use ac3_graphics::{Vram, rgba8};
use ac3_port::machine_player::{Host, Player};
use serde_json::{Value, json};
use std::{fs, io::BufWriter, path::PathBuf};

const START_FIELD: u64 = 83;
const CAPTURED: &[u64] = &[310, 320, 340, 360, 380, 400, 420, 450];
const UP: u16 = 1 << 4;
const RIGHT: u16 = 1 << 5;

fn words(value: &Value) -> [u32; 32] {
    std::array::from_fn(|i| value[i].as_u64().unwrap() as u32)
}

/// Redux dumps 1024x1024 VRAM; the console's is the first 512 lines.
const VRAM_BYTES: usize = 1024 * 512 * 2;

fn vram_words(bytes: &[u8]) -> Vec<u16> {
    bytes[..VRAM_BYTES]
        .chunks_exact(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect()
}

fn save_png(path: &PathBuf, width: usize, height: usize, pixels: &[u16]) {
    let writer = BufWriter::new(fs::File::create(path).unwrap());
    let mut encoder = png::Encoder::new(writer, width as u32, height as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let rgba: Vec<u8> = pixels.iter().flat_map(|&p| rgba8(p)).collect();
    encoder
        .write_header()
        .unwrap()
        .write_image_data(&rgba)
        .unwrap();
}

#[test]
#[ignore = "requires AC3_DATA_DIR, the disc image and analysis/flight-runtime/native-run-001"]
fn native_run_from_state_two_entry() {
    let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR"));
    let capture = root.join("analysis/flight-runtime/native-run-001");
    let out = std::env::var_os("AC3_NATIVE_RUN_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/native-run-001"));
    fs::create_dir_all(&out).unwrap();
    let index = fs::read_to_string(capture.join("loop-snapshots.jsonl")).unwrap();
    let first: Value = serde_json::from_str(index.lines().next().unwrap()).unwrap();
    assert_eq!(first["frame"].as_u64(), Some(START_FIELD));
    let ram = fs::read(capture.join("loop-0001.ram.bin")).unwrap();
    let scratch = fs::read(capture.join("loop-0001.scratch.bin")).unwrap();
    let vram = fs::read(capture.join("loop-0001.vram.bin")).unwrap();
    let vram = Vram::from_le_bytes(&vram[..VRAM_BYTES]).unwrap();
    let mut machine = Machine::from_images(&ram, &scratch).unwrap();
    machine.gte.data = words(&first["cp2d"]);
    machine.gte.control = words(&first["cp2c"]);
    machine.sp = words(&first["gpr"])[29];
    let disc =
        fs::File::open(root.join("Ace Combat 3 - Electrosphere (Europe) (En,Fr,De,Es,It).bin"))
            .unwrap();
    let mut host = Host::new(vram, Some(disc));
    host.pad_input = Some(Box::new(|vblank| {
        let field = START_FIELD + vblank + 1;
        let mut buttons = 0;
        if (330..370).contains(&field) {
            buttons |= UP;
        }
        if (400..440).contains(&field) {
            buttons |= RIGHT;
        }
        buttons
    }));
    let mut player = Player::new(machine, host);

    let mut iterations = Vec::new();
    let mut comparisons = Vec::new();
    let mut pending = CAPTURED.iter().copied().peekable();
    let mut previous: Option<(u64, Vec<u16>)> = None;
    let mut fault = None;
    while let Some(&target) = pending.peek() {
        let result = player.iteration();
        let field = START_FIELD + player.vblanks();
        let calls = player.machine.host_calls.len();
        iterations.push(json!({"field": field, "host_calls": calls}));
        if let Err(error) = result {
            fault = Some(format!("field {field}: {error}"));
            break;
        }
        if let Some(error) = player.host.borrow().error() {
            fault = Some(format!("field {field}: device: {error}"));
            break;
        }
        let host = player.host.borrow();
        let current = host.gpu.gpu.vram().words().to_vec();
        if field >= target {
            pending.next();
            let original =
                vram_words(&fs::read(capture.join(format!("frame-{target:05}.vram.bin"))).unwrap());
            let differing =
                |vram: &[u16]| vram.iter().zip(&original).filter(|(a, b)| a != b).count();
            let [_, _, w, h] = host.gpu.display;
            let shown = host.gpu.displayed().unwrap();
            save_png(
                &out.join(format!("native-{target:05}.png")),
                w as usize,
                h as usize,
                &shown,
            );
            comparisons.push(json!({
                "field": target,
                "native_field_after": field,
                "differing_words_after": differing(&current),
                "native_field_before": previous.as_ref().map(|(f, _)| *f),
                "differing_words_before": previous.as_ref().map(|(_, v)| differing(v)),
                "display": host.gpu.display,
            }));
        }
        drop(host);
        previous = Some((field, current));
    }
    let calls: serde_json::Map<String, Value> = player
        .host
        .borrow()
        .calls
        .iter()
        .map(|(f, n)| (format!("{f:08x}"), json!(n)))
        .collect();
    let report = json!({
        "start_field": START_FIELD,
        "fault": fault,
        "iterations": iterations.len(),
        "comparisons": comparisons,
        "host_calls": calls,
        "iteration_log": iterations,
    });
    fs::write(
        out.join("report.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    eprintln!(
        "native run: {} iterations, fault {:?}, comparisons {}",
        report["iterations"],
        report["fault"],
        serde_json::to_string(&report["comparisons"]).unwrap()
    );
    assert!(!iterations.is_empty());
}

/// The original's first flight main-loop top: `mainloop-align-002`
/// (native-run-001's checkpoint and inputs, `tools/redux-loop-snapshot.lua`
/// armed at gameplay initialization 80032510), `loop-0001` at field 307. Its
/// frames 304..332 are captured every field; native-run-001's frames
/// 340..450 are the same deterministic run.
const FLIGHT_CAPTURE: &str = "mainloop-align-002";
const FLIGHT_START: u64 = 307;
const FLIGHT_END: u64 = 451;

/// RAM that host devices and capture timing own, not game state. The
/// original's frame T is captured at VBlank T, before the work the game does
/// on that VBlank; a native iteration ends after it (the swap's DRAWENV and
/// DISPENV copies and packet, the loop-top clocks). The native devices keep
/// no hardware timing (VSync records, libgpu's queue and timeout clock, the
/// CPU-time meter) and answer the pad without libpad's SIO path.
const HOST_STATE: &[(u32, u32)] = &[
    (0x8000_0000, 0x8001_0000), // kernel and BIOS
    (0x800b_2540, 0x800b_2570), // libpad state (PADMAIN, VSync flag)
    (0x800b_2ec4, 0x800b_2ecc), // VSync record
    (0x800b_3ff4, 0x800b_3ff8), // VSync counter
    (0x800b_97f0, 0x800b_9860), // libgpu DRAWENV/DISPENV copies
    (0x800b_9910, 0x800b_9924), // libgpu queue counter, timeout clock
    (0x800b_af74, 0x800b_af78), // DRAWENV packet tag (rebuilt at the swap)
    (0x800b_d178, 0x800b_d184), // main-loop context +0x20..+0x2B: VSync clocks
    (0x800b_d1a0, 0x800b_d1a8), // main-loop context +0x48: CPU-time meter
    (0x800b_e1e0, 0x800b_e224), // libpad receive buffers, ports 1 and 2
    (0x800c_5a00, 0x800c_5c78), // libpad SIO state (PadInitDirect, _padSioRW)
    (0x801f_c000, 0x8020_0000), // stack
];

fn host_state(address: u32) -> bool {
    HOST_STATE
        .iter()
        .any(|&(start, end)| (start..end).contains(&address))
}

#[test]
#[ignore = "requires AC3_DATA_DIR, the disc image and analysis/flight-runtime/mainloop-align-002"]
fn native_flight_from_original_flight_start() {
    let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR"));
    let flight = root.join("analysis/flight-runtime").join(FLIGHT_CAPTURE);
    let later = root.join("analysis/flight-runtime/native-run-001");
    let index = fs::read_to_string(flight.join("loop-snapshots.jsonl")).unwrap();
    let first: Value = serde_json::from_str(index.lines().next().unwrap()).unwrap();
    assert_eq!(first["frame"].as_u64(), Some(FLIGHT_START));
    let ram = fs::read(flight.join("loop-0001.ram.bin")).unwrap();
    let scratch = fs::read(flight.join("loop-0001.scratch.bin")).unwrap();
    let vram = fs::read(flight.join("loop-0001.vram.bin")).unwrap();
    let vram = Vram::from_le_bytes(&vram[..VRAM_BYTES]).unwrap();
    let mut machine = Machine::from_images(&ram, &scratch).unwrap();
    machine.gte.data = words(&first["cp2d"]);
    machine.gte.control = words(&first["cp2c"]);
    machine.sp = words(&first["gpr"])[29];
    let disc =
        fs::File::open(root.join("Ace Combat 3 - Electrosphere (Europe) (En,Fr,De,Es,It).bin"))
            .unwrap();
    let mut host = Host::new(vram, Some(disc));
    host.pad_input = Some(Box::new(|vblank| {
        let field = FLIGHT_START + vblank + 1;
        let mut buttons = 0;
        if (330..370).contains(&field) {
            buttons |= UP;
        }
        if (400..440).contains(&field) {
            buttons |= RIGHT;
        }
        buttons
    }));
    let mut player = Player::new(machine, host);
    let mut compared = Vec::new();
    loop {
        if let Err(error) = player.iteration() {
            panic!("field {}: {error}", FLIGHT_START + player.vblanks());
        }
        let field = FLIGHT_START + player.vblanks();
        // The original's capture of the field before the iteration's end.
        let target = field - 1;
        let name = format!("frame-{target:05}");
        let path = |directory: &PathBuf, suffix: &str| directory.join(format!("{name}.{suffix}"));
        let directory = [&flight, &later]
            .into_iter()
            .find(|d| path(d, "ram.bin").exists());
        if let Some(directory) = directory {
            let original = fs::read(path(directory, "ram.bin")).unwrap();
            let native = player.machine.ram();
            let differing: Vec<u32> = (0..0x20_0000u32)
                .map(|i| 0x8000_0000 + i)
                .filter(|&a| !host_state(a))
                .filter(|&a| native[(a & 0x1f_ffff) as usize] != original[(a & 0x1f_ffff) as usize])
                .collect();
            assert!(
                differing.is_empty(),
                "field {target}: {} game-state bytes differ, first at {:08x?}",
                differing.len(),
                &differing[..differing.len().min(16)]
            );
            // The displayed frame buffer, pixel for pixel.
            let original = vram_words(&fs::read(path(directory, "vram.bin")).unwrap());
            let host = player.host.borrow();
            let [x, y, w, h] = host.gpu.display;
            let native = host.gpu.gpu.vram().words();
            let mut pixels = 0;
            for row in y as usize..(y + h) as usize {
                for column in x as usize..(x + w) as usize {
                    let i = (row & 511) * 1024 + column;
                    if native[i] != original[i] {
                        pixels += 1;
                    }
                }
            }
            assert_eq!(pixels, 0, "field {target}: displayed pixels differ");
            compared.push(target);
        }
        if field >= FLIGHT_END {
            break;
        }
    }
    eprintln!(
        "native flight from field {FLIGHT_START}: RAM and displayed frame exact at {} fields {:?}",
        compared.len(),
        compared
    );
    assert!(compared.len() >= 19);
}
