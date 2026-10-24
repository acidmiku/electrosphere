//! The attract-mode movie (state 5) end to end on the native machine:
//! translated movie state, libpress over the host MDEC
//! (`src/machine_mdec.rs`), the CD stream through `cdstream`'s drive with
//! its XA audio into the SPU (`src/machine_movie.rs`), from the original's
//! main-loop top.
//!
//! Start: capture `ovmisc-021` (title from `overlay-entry.sav`, CROSS at
//! 301, idle), the 80025720 entry at frame 1358: the first pass in state 5
//! (substate 0, 800563C8 runs in it). VRAM and the SPU (sound RAM and
//! registers: main and CD volume) come from the save state `frame-01357.sav`
//! of the same run; the CD drive starts idle (the title's music stream is
//! stopped by the movie's own requests).
//!
//! Checks: the run stays in the movie without a fault for the whole
//! window, the MDEC decodes the frames, XA sectors reach the SPU and the
//! mix is not silent; every movie frame rectangle in the original's VRAM
//! at the frames `ovmisc-020` saved (same inputs) is compared with the
//! frames the native run completed (rectangle contents by hash). Pictures
//! (every 25 VBlanks) and `audio.wav` go to `$AC3_MOVIE_OUT` (default
//! `target/movie-021`).
use ac3_flight::{
    Device, HostCall, Machine,
    snapshot::{self, Snapshot},
};
use ac3_graphics::Vram;
use ac3_port::machine_audio::AudioOutput;
use ac3_port::machine_movie::display_rgb;
use ac3_port::machine_player::{Host, Player};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};

const GP: u32 = 0x800b_a9f4;
/// `[gp+0x4DC]`: the movie decoder.
const DECODER: u32 = GP + 0x4dc;
const CONTEXT: u32 = GP + 0x2dc;
/// `[gp+0x4E8]`: the picture decoder (`ac3_flight::states::picture`).
const PICTURE_DECODER: u32 = GP + 0x4e8;
/// VBlanks to run.
const WINDOW: u64 = 1100;

fn varint(data: &[u8], at: &mut usize) -> u64 {
    let mut value = 0u64;
    let mut shift = 0;
    loop {
        let byte = data[*at];
        *at += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte < 0x80 {
            return value;
        }
        shift += 7;
    }
}
/// Length-delimited fields of a protobuf message, by number.
fn fields(data: &[u8]) -> BTreeMap<u64, &[u8]> {
    let mut out = BTreeMap::new();
    let mut at = 0;
    while at < data.len() {
        let key = varint(data, &mut at);
        match key & 7 {
            0 => {
                varint(data, &mut at);
            }
            1 => at += 8,
            5 => at += 4,
            2 => {
                let length = varint(data, &mut at) as usize;
                out.entry(key >> 3).or_insert(&data[at..at + length]);
                at += length;
            }
            other => panic!("protobuf wire type {other}"),
        }
    }
    out
}

fn word(m: &mut Machine, address: u32) -> u32 {
    m.lw(address)
}

/// The two frame rectangles {x, y, w, h} of the movie decoder (VRAM
/// halfwords).
fn rects(ram: &[u8]) -> [[usize; 4]; 2] {
    let read = |a: u32| {
        let at = (a & 0x1f_ffff) as usize;
        u32::from_le_bytes(ram[at..at + 4].try_into().unwrap())
    };
    let half = |a: u32| {
        let at = (a & 0x1f_ffff) as usize;
        usize::from(u16::from_le_bytes([ram[at], ram[at + 1]]))
    };
    let d = read(DECODER);
    std::array::from_fn(|r| {
        let base = d + 0x18 + 8 * r as u32;
        std::array::from_fn(|i| half(base + 2 * i as u32))
    })
}

fn rect_hash(vram: &[u16], [x, y, w, h]: [usize; 4]) -> u64 {
    let mut hasher = DefaultHasher::new();
    for row in y..y + h {
        vram[row * 1024 + x..row * 1024 + x + w].hash(&mut hasher);
    }
    hasher.finish()
}

fn save_png(path: &Path, rgb: &[u32], w: usize, h: usize) {
    let file = std::io::BufWriter::new(fs::File::create(path).unwrap());
    let mut encoder = png::Encoder::new(file, w as u32, h as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let bytes: Vec<u8> = rgb
        .iter()
        .flat_map(|&p| [(p >> 16) as u8, (p >> 8) as u8, p as u8])
        .collect();
    encoder
        .write_header()
        .unwrap()
        .write_image_data(&bytes)
        .unwrap();
}

/// The 80025720 entry (main-loop top) at `frame` in `capture`.
fn loop_top(capture: &Path, frame: u64) -> snapshot::SnapshotInfo {
    snapshot::read_index(&capture.join("snapshots.jsonl"))
        .unwrap()
        .into_iter()
        .find(|s| s.callee == 0x8002_5720 && s.stage == "entry" && s.frame == frame)
        .expect("loop top")
}

/// A player from a loop-top snapshot (which must be in `state`, substate
/// 0), with VRAM and the SPU from the save state `<saved>.sav` /
/// `<saved>.vram.bin` of the same capture and the disc.
fn player_at(
    root: &Path,
    capture: &Path,
    start: snapshot::SnapshotInfo,
    state: u32,
    saved: &str,
) -> Player {
    let snap = Snapshot::load(capture, &start).unwrap();
    let mut machine = snap.machine();
    let context = word(&mut machine, CONTEXT);
    assert_eq!(
        (machine.lw(context + 0x2c), machine.lw(context + 0x30)),
        (state, 0),
        "state {state} substate 0"
    );
    let save = fs::read(capture.join(format!("{saved}.sav"))).unwrap();
    let top = fields(&save);
    let spu = fields(top[&6]);
    let vram = fs::read(capture.join(format!("{saved}.vram.bin"))).unwrap();
    let vram = Vram::from_le_bytes(&vram[..1024 * 512 * 2]).unwrap();
    let disc =
        fs::File::open(root.join("Ace Combat 3 - Electrosphere (Europe) (En,Fr,De,Es,It).bin"))
            .unwrap();
    let mut host = Host::new(vram, Some(disc));
    host.spu.load_state(spu[&1], spu[&2]);
    // The display as the saved GP1 control words set it (display enable,
    // mode, ranges, start).
    let controls = fields(top[&5])[&2];
    let mut ram = vec![0u8; 0x20_0000];
    let mut scratch = [0u8; 0x400];
    for command in [8usize, 6, 7, 5, 3] {
        let word = u32::from_le_bytes(controls[4 * command..4 * command + 4].try_into().unwrap());
        let call = HostCall {
            function: ac3_port::flight_host::sdk::GP1,
            args: [word, 0, 0, 0],
        };
        host.gpu.call(&mut ram, &mut scratch, call);
    }
    Player::new(machine, host)
}

/// Save the displayed picture (15- or 24-bit) as `vblank-NNNNNN.png`.
fn save_picture(player: &Player, out: &Path) {
    let host = player.host.borrow();
    let display = host.gpu.gpu.display();
    let [_, _, w, h] = host.gpu.display;
    let (w, h) = (w.max(1) as usize, h.max(1) as usize);
    let rgb = display_rgb(
        host.gpu.gpu.vram().words(),
        (usize::from(display.start_x), usize::from(display.start_y)),
        w,
        h,
        display.mode & 0x10 != 0,
    );
    save_png(
        &out.join(format!("vblank-{:06}.png", player.vblanks())),
        &rgb,
        w,
        h,
    );
}

fn vram_words(bytes: &[u8]) -> Vec<u16> {
    bytes[..1024 * 512 * 2]
        .chunks_exact(2)
        .map(|p| u16::from_le_bytes([p[0], p[1]]))
        .collect()
}

#[test]
#[ignore = "needs AC3_DATA_DIR, the disc image and analysis/flight-runtime/ovmisc-020, -021"]
fn attract_movie_plays_natively() {
    let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR"));
    let runtime = root.join("analysis/flight-runtime");
    let capture = runtime.join("ovmisc-021");
    let out = std::env::var_os("AC3_MOVIE_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/movie-021"));
    fs::create_dir_all(&out).unwrap();

    let start = loop_top(&capture, 1358);
    let mut player = player_at(&root, &capture, start, 5, "frame-01357");
    let mut audio = AudioOutput::new(false, Some(&out.join("audio.wav"))).unwrap();

    let mut native_frames: HashSet<u64> = HashSet::new();
    let mut numbers = Vec::new();
    // Stream frame number (decoder +0x11034) after each pass, by VBlank.
    let mut timeline: BTreeMap<u64, u16> = BTreeMap::new();
    let mut fault = None;
    let mut pictures = 0;
    let mut loudest = 0i32;
    while player.vblanks() < WINDOW {
        if let Err(error) = player.iteration() {
            fault = Some(format!("VBlank {}: {error}", player.vblanks()));
            break;
        }
        if let Some(error) = player.host.borrow().error() {
            fault = Some(format!("VBlank {}: device: {error}", player.vblanks()));
            break;
        }
        let pcm = player.host.borrow_mut().take_audio();
        loudest = pcm
            .iter()
            .flat_map(|f| f.iter().map(|&s| i32::from(s).abs()))
            .fold(loudest, i32::max);
        audio.submit(&pcm).unwrap();
        let vblank = player.vblanks();
        let m = &mut player.machine;
        let context = m.lw(CONTEXT);
        if m.lw(context + 0x2c) != 5 {
            break;
        }
        let d = m.lw(DECODER);
        let number = m.lhu(d + 0x1_1034);
        if numbers.last() != Some(&number) {
            numbers.push(number);
        }
        timeline.insert(vblank, number);
        {
            let host = player.host.borrow();
            let words = host.gpu.gpu.vram().words();
            for rect in rects(m.ram()) {
                if rect[2] > 0 && rect[3] > 0 {
                    native_frames.insert(rect_hash(words, rect));
                }
            }
        }
        if player.vblanks() >= (pictures + 1) * 25 {
            pictures += 1;
            save_picture(&player, &out);
        }
    }
    let audio = audio.finish().unwrap();
    let host = player.host.borrow();
    println!(
        "{} VBlanks, fault {fault:?}; MDEC macroblocks {} (stalls {}); XA sectors {} (rejected {}); movie frames {}..{} ({} numbers); loudest sample {loudest}; audio {audio}",
        player.vblanks(),
        host.mdec.macroblocks,
        host.mdec.stalls,
        host.xa_player.sectors,
        host.xa_player.rejected,
        numbers.first().copied().unwrap_or(0),
        numbers.last().copied().unwrap_or(0),
        numbers.len(),
    );
    // The original's movie frames at the saved frames of `ovmisc-020`.
    let original_run = runtime.join("ovmisc-020");
    let mut compared = 0;
    let mut matched = 0;
    for frame in [
        1365u64, 1380, 1400, 1450, 1500, 1600, 1800, 2000, 2200, 2400,
    ] {
        let Ok(vram) = fs::read(original_run.join(format!("frame-{frame:05}.vram.bin"))) else {
            continue;
        };
        let ram = fs::read(original_run.join(format!("frame-{frame:05}.ram.bin"))).unwrap();
        let words = vram_words(&vram);

        let vblank = frame - 1358;
        if vblank > player.vblanks() {
            continue;
        }
        let d = {
            let at = (DECODER & 0x1f_ffff) as usize;
            u32::from_le_bytes(ram[at..at + 4].try_into().unwrap())
        };
        let at = ((d + 0x1_1034) & 0x1f_ffff) as usize;
        let original_number = u16::from_le_bytes([ram[at], ram[at + 1]]);
        let native_number = timeline.range(..=vblank).next_back().map(|(_, n)| *n);
        println!(
            "frame {frame}: stream frame {original_number} (native {native_number:?} at VBlank {vblank})"
        );
        for rect in rects(&ram) {
            if rect[2] == 0 || rect[3] == 0 {
                continue;
            }
            compared += 1;
            let found = native_frames.contains(&rect_hash(&words, rect));
            matched += usize::from(found);
            println!("  rect {rect:?}: reproduced {found}");
        }
    }
    println!("{matched} of {compared} original movie rectangles reproduced exactly");
    assert_eq!(fault, None);
    assert!(host.mdec.macroblocks > 1000);
    assert!(numbers.len() > 50, "movie frames advance");
    assert!(host.xa_player.sectors > 0 && loudest > 0, "movie audio");
    assert_eq!(matched, compared, "every original movie rectangle");
    assert!(compared > 0);
}

/// State 6 (the streamed picture sequence, reached by the documented probe)
/// from capture `ovmisc-022`: the loop top at frame 451 (state 6, substate
/// 0), VRAM, SPU and display from `frame-00450.sav`. The run must stay in
/// state 6 without a fault, with the picture decoder's progress (frame
/// count, frames shown, shown bank) equal to the original's at every frame
/// `ovmisc-022` saved. The native VRAM of the nearest pass (within twelve
/// VBlanks) is compared with the saved one: the displayed area must be
/// identical from frame 700 on. Earlier frames are in the sequence's noise
/// fade, which a native pass draws and then clears with the frame end's
/// drawing-area fill (the pass leaves the area unchanged), while the
/// original's save caught VRAM between the two; the probe's stream (file
/// 0x800, no picture) also uploads a CLUT row (768,281) on every native
/// pass but only once in the original.
#[test]
#[ignore = "needs AC3_DATA_DIR, the disc image and analysis/flight-runtime/ovmisc-022"]
fn picture_state_plays_natively() {
    let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR"));
    let capture = root.join("analysis/flight-runtime/ovmisc-022");
    let out = std::env::var_os("AC3_PICTURE_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/picture-022"));
    fs::create_dir_all(&out).unwrap();
    let start = loop_top(&capture, 451);
    let mut player = player_at(&root, &capture, start, 6, "frame-00450");
    let frames = [500u64, 560, 620, 700, 800, 900, 1000, 1200, 1400];
    let originals: Vec<(u64, Vec<u16>)> = frames
        .iter()
        .map(|&f| {
            let bytes = fs::read(capture.join(format!("frame-{f:05}.vram.bin"))).unwrap();
            (f, vram_words(&bytes))
        })
        .collect();
    // Best (fewest differing) comparison per saved frame: display, whole.
    let mut best: BTreeMap<u64, (usize, usize, u64)> = BTreeMap::new();
    // Picture decoder (frame count +4, frames shown +8, shown bank +0x9F)
    // after each pass, by original frame number.
    let mut timeline: BTreeMap<u64, (u32, u16, u8)> = BTreeMap::new();
    let mut fault = None;
    let mut pictures = 0;
    while player.vblanks() + 451 < 1403 {
        if let Err(error) = player.iteration() {
            fault = Some(format!("VBlank {}: {error}", player.vblanks()));
            break;
        }
        if let Some(error) = player.host.borrow().error() {
            fault = Some(format!("VBlank {}: device: {error}", player.vblanks()));
            break;
        }
        player.host.borrow_mut().take_audio();
        let context = player.machine.lw(CONTEXT);
        if player.machine.lw(context + 0x2c) != 6 {
            fault = Some(format!("left state 6 at VBlank {}", player.vblanks()));
            break;
        }
        let frame = player.vblanks() + 451;
        let d = player.machine.lw(PICTURE_DECODER);
        let progress = (
            player.machine.lw(d + 4),
            player.machine.lhu(d + 8),
            player.machine.lbu(d + 0x9f),
        );
        timeline.insert(frame, progress);

        {
            let host = player.host.borrow();
            let words = host.gpu.gpu.vram().words();
            let display = host.gpu.gpu.display();
            let [_, _, w, h] = host.gpu.display;
            for (f, original) in &originals {
                if frame.abs_diff(*f) > 12 {
                    continue;
                }
                let (x0, y0) = (usize::from(display.start_x), usize::from(display.start_y));
                let shown = (y0..y0 + h.max(0) as usize)
                    .flat_map(|y| {
                        (x0..x0 + w.max(0) as usize).map(move |x| (y & 511) * 1024 + (x & 1023))
                    })
                    .filter(|&i| words[i] != original[i])
                    .count();
                let whole = words.iter().zip(original).filter(|(a, b)| a != b).count();
                let entry = best.entry(*f).or_insert((usize::MAX, usize::MAX, 0));
                if (shown, whole) < (entry.0, entry.1) {
                    *entry = (shown, whole, frame);
                    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
                    fs::write(out.join(format!("native-{f:05}.vram.bin")), bytes).unwrap();
                }
            }
        }
        if player.vblanks() >= (pictures + 1) * 25 {
            pictures += 1;
            save_picture(&player, &out);
        }
    }
    let mut exact = 0;
    for (f, (shown, whole, at)) in &best {
        let ram = fs::read(capture.join(format!("frame-{f:05}.ram.bin"))).unwrap();
        let read = |a: u32, n: usize| {
            let at = (a & 0x1f_ffff) as usize;
            ram[at..at + n]
                .iter()
                .rev()
                .fold(0u32, |v, &b| (v << 8) | u32::from(b))
        };
        let d = read(PICTURE_DECODER, 4);
        let original = (
            read(d + 4, 4),
            read(d + 8, 2) as u16,
            read(d + 0x9f, 1) as u8,
        );
        let native = timeline.range(..=*f).next_back().map(|(_, p)| *p);
        println!(
            "frame {f}: displayed area {shown} halfwords differ, VRAM {whole} (native frame {at}); decoder (frame, shown, bank) original {original:?} native {native:?}"
        );
        assert_eq!(native, Some(original), "decoder progress at frame {f}");
        if *f >= 700 {
            assert_eq!(*shown, 0, "displayed area at frame {f}");
            exact += 1;
        }
    }
    println!("{} VBlanks, fault {fault:?}", player.vblanks());
    assert_eq!(fault, None);
    assert_eq!(best.len(), frames.len());
    assert!(exact > 0);
}
