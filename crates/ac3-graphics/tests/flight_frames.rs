//! Original first-flight frames replayed through the native GPU.
//!
//! Each capture holds consecutive VRAM snapshots and every GP0/GP1 word the
//! original libgpu handed to the GPU between them (see `docs/FLIGHT_GPU.md`).
//! Replays are renderer-only evidence: captured commands and VRAM are inputs.

use ac3_graphics::{Dithering, Gpu, Vram};
use std::{fs, path::Path};

const VRAM_BYTES: usize = 1024 * 512 * 2;
const CAPTURES: [&str; 4] = [
    "analysis/flight-runtime/gpu-001",
    "analysis/flight-runtime/gpu-002",
    "analysis/flight-runtime/gpu-003",
    "analysis/flight-runtime/gpu-004",
];

#[derive(Debug)]
struct Record {
    snap: usize,
    kind: String,
    words: Vec<u32>,
    gp1: u32,
}

fn load_vram(path: &Path) -> Vram {
    let bytes = fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    // Redux exposes a 1024-row allocation; the retail VRAM is the first MiB.
    Vram::from_le_bytes(&bytes[..VRAM_BYTES]).unwrap()
}

fn records(capture: &Path) -> Vec<Record> {
    let words: Vec<u32> = fs::read(capture.join("gpu-words.bin"))
        .unwrap()
        .chunks_exact(4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    let text = fs::read_to_string(capture.join("gpu.jsonl")).unwrap();
    let mut out = Vec::new();
    let mut seq = 0;
    for line in text.lines() {
        let row: serde_json::Value = serde_json::from_str(line).unwrap();
        seq += 1;
        assert_eq!(
            row["seq"].as_u64(),
            Some(seq),
            "records are complete and ordered"
        );
        let kind = row["kind"].as_str().unwrap().to_owned();
        let slice = |row: &serde_json::Value| {
            let offset = row["word_offset"].as_u64().unwrap() as usize;
            let count = row["word_count"].as_u64().unwrap() as usize;
            words[offset..offset + count].to_vec()
        };
        let (record_words, gp1) = match kind.as_str() {
            "chain" | "direct" | "load_image" => (slice(&row), 0),
            // StoreImage writes C0, position and size to GP0 directly.
            "store_image" => (
                vec![
                    0xc000_0000,
                    row["xy"].as_u64().unwrap() as u32,
                    row["wh"].as_u64().unwrap() as u32,
                ],
                0,
            ),
            "gp1" => (Vec::new(), row["value"].as_u64().unwrap() as u32),
            "put_draw_env" | "put_disp_env" => (Vec::new(), 0),
            other => panic!("capture uses an unreplayed libgpu path: {other}"),
        };
        out.push(Record {
            snap: row["snap"].as_u64().unwrap() as usize,
            kind,
            words: record_words,
            gp1,
        });
    }
    out
}

fn feed(gpu: &mut Gpu, record: &Record) {
    if record.kind == "gp1" {
        gpu.write_gp1(record.gp1).unwrap();
    } else {
        gpu.write_gp0_words(&record.words).unwrap();
    }
}

fn differences(actual: &[u16], expected: &[u16]) -> (usize, Vec<String>) {
    let mut samples = Vec::new();
    let mut count = 0;
    for (index, (a, e)) in actual.iter().zip(expected).enumerate() {
        if a != e {
            count += 1;
            if samples.len() < 12 {
                samples.push(format!(
                    "({},{}) {a:04x}!={e:04x}",
                    index % 1024,
                    index / 1024
                ));
            }
        }
    }
    (count, samples)
}

#[test]
#[ignore = "requires AC3_DATA_DIR and original flight GPU captures"]
fn original_flight_frames_replay_pixel_exact() {
    let root = std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR is required");
    let root = Path::new(&root);
    let mut failures = 0;
    // Exploratory captures can be named explicitly; the default list is the accepted evidence.
    let names: Vec<String> = match std::env::var("AC3_FLIGHT_GPU_CAPTURES") {
        Ok(list) => list.split(',').map(str::to_owned).collect(),
        Err(_) => CAPTURES.iter().map(|s| (*s).to_owned()).collect(),
    };
    for name in &names {
        let capture = root.join(name);
        let summary: serde_json::Value =
            serde_json::from_slice(&fs::read(capture.join("gpu-summary.json")).unwrap()).unwrap();
        // Snapshot 0 is the checkpoint; older summaries list consecutive fields only.
        let stems: Vec<String> = match summary["snapshots"].as_array() {
            Some(list) => list
                .iter()
                .map(|s| s.as_str().unwrap().to_owned())
                .collect(),
            None => std::iter::once("checkpoint".to_owned())
                .chain((1..=summary["frames"].as_u64().unwrap()).map(|f| format!("frame-{f:05}")))
                .collect(),
        };
        let records = records(&capture);
        let snapshots: Vec<Vram> = stems
            .iter()
            .map(|stem| load_vram(&capture.join(format!("{stem}.vram.bin"))))
            .collect();
        let mut chained = Gpu::new(snapshots[0].clone());
        let mut isolated = Gpu::new(snapshots[0].clone());
        let mut intervals = Vec::new();
        let mut totals = [0u64; 6];
        for k in 0..stems.len() - 1 {
            isolated.set_vram(snapshots[k].clone());
            let before = isolated.stats().clone();
            let mut gp0_words = 0;
            for record in records.iter().filter(|r| r.snap == k) {
                feed(&mut isolated, record);
                feed(&mut chained, record);
                gp0_words += record.words.len();
            }
            assert!(
                isolated.is_idle() && chained.is_idle(),
                "interval {k} ends mid-command"
            );
            let after = isolated.stats();
            let commands: u64 =
                after.commands.values().sum::<u64>() - before.commands.values().sum::<u64>();
            let drawn = after.drawn - before.drawn;
            let expected = snapshots[k + 1].words();
            let (changed, _) = differences(snapshots[k].words(), expected);
            let (isolated_diff, isolated_samples) = differences(isolated.vram().words(), expected);
            let (chained_diff, chained_samples) = differences(chained.vram().words(), expected);
            // The displayed field: the screenshot rows/columns from the GP1(05) start.
            let stem = &stems[k + 1];
            let meta: serde_json::Value =
                serde_json::from_slice(&fs::read(capture.join(format!("{stem}.json"))).unwrap())
                    .unwrap();
            let (width, height) = (
                meta["width"].as_u64().unwrap() as usize,
                meta["height"].as_u64().unwrap() as usize,
            );
            let screen: Vec<u16> = fs::read(capture.join(format!("{stem}.screen.raw")))
                .unwrap()
                .chunks_exact(2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
                .collect();
            assert_eq!(screen.len(), width * height, "15-bit screenshot");
            let shown = chained.display_pixels(width, height).unwrap();
            let (screen_diff, _) = differences(&shown, &screen);
            eprintln!(
                "{name} {}->{stem}: {gp0_words} GP0 words, {commands} commands, {drawn} drawn, \
                 {changed} pixels changed; VRAM differences isolated {isolated_diff} \
                 {isolated_samples:?}, chained {chained_diff} {chained_samples:?}; display \
                 {width}x{height}@({},{}) differences {screen_diff}",
                stems[k],
                chained.display().start_x,
                chained.display().start_y,
            );
            failures += isolated_diff + chained_diff + screen_diff;
            totals[0] += gp0_words as u64;
            totals[1] += commands;
            totals[2] += drawn;
            totals[3] += expected.len() as u64;
            totals[4] += (width * height) as u64;
            totals[5] += changed as u64;
            intervals.push(serde_json::json!({
                "from": stems[k], "to": stem, "gp0_words": gp0_words, "commands": commands,
                "drawn": drawn, "pixels_changed": changed, "vram_pixels": expected.len(),
                "isolated_differences": isolated_diff, "chained_differences": chained_diff,
                "display": [width, height, chained.display().start_x, chained.display().start_y],
                "display_differences": screen_diff,
            }));
        }
        let counts: serde_json::Map<String, serde_json::Value> = chained
            .stats()
            .commands
            .iter()
            .map(|(op, n)| (format!("{op:02x}"), serde_json::json!(n)))
            .collect();
        // Negative control: the same isolated replay without dithering must
        // differ wherever dithered primitives were drawn, showing that the
        // comparison is sensitive to rasterizer rules.
        let mut control = Gpu::new(snapshots[0].clone());
        control.set_dithering(Dithering::Off);
        let mut control_diff = 0;
        for k in 0..stems.len() - 1 {
            control.set_vram(snapshots[k].clone());
            for record in records.iter().filter(|r| r.snap == k) {
                feed(&mut control, record);
            }
            control_diff += differences(control.vram().words(), snapshots[k + 1].words()).0;
        }
        let dithered = chained.stats().paths.keys().any(|p| p.contains(".dither"));
        assert_eq!(control_diff > 0, dithered, "{name}: dithering-off control");
        eprintln!("{name}: dithering-off control differs at {control_diff} pixels");
        eprintln!("{name}: paths {:?}", chained.stats().paths);
        let report = serde_json::json!({
            "passed": failures == 0, "capture": name, "intervals": intervals,
            "gp0_words": totals[0], "commands": totals[1], "drawn": totals[2],
            "vram_pixels_compared_per_replay": totals[3], "display_pixels_compared": totals[4],
            "pixels_changed_by_original": totals[5], "command_counts": counts,
            "rasterizer_paths": chained.stats().paths,
            "control_dithering_off_differences": control_diff,
            "scope": "Renderer-only: original GP0/GP1 streams and VRAM snapshots are inputs. \
                      Isolated replays start each interval from the captured VRAM; the chained \
                      replay starts once from the checkpoint VRAM and never reseeds.",
        });
        fs::write(
            capture.join("native-gpu-replay.json"),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
        eprintln!(
            "{name}: {} GP0 words, {} commands, {} drawn, {} pixels changed, {} VRAM pixels per \
             replay, {} display pixels",
            totals[0], totals[1], totals[2], totals[5], totals[3], totals[4]
        );
    }
    assert_eq!(failures, 0, "native GPU differs from the original frames");
}
