//! `ac3 export-movies`: every movie on the Japanese discs, decoded frame by
//! frame as the game shows it ([`crate::movie_disc`]), as lossless PNGs of
//! the active rectangle (the MDEC frame, without the letterbox) and a
//! manifest. See `docs/MOVIES.md`.
//!
//! Layout of the output folder:
//! - `manifest.json`: every movie (key, source, frames, sizes, rate,
//!   audio, the game's players and subtitles) and the export settings;
//! - `<key>/000000.png ...`: frame *index* 0.. (from the start; the game's
//!   frame number is `first_number - index`), RGB, 8 bits a channel.
use crate::movie_disc::{Depth, Frame, FrameDecoder, MovieDisc, MovieInfo};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Instant,
};

/// 800B2AE8: the movie and picture table `[gp+0x220]` in the Japanese
/// resident's data, 8 bytes an entry (by context +0x3A8): +0 stream file
/// (an SPH resource; bit 12 the stream-control flag), +2 the subtitle flag
/// (2: the state-5 movie's subtitle lines, 1: in-mission and picture
/// screens), +4 the subtitle image bank (a BPB file).
pub const MOVIE_TABLE: u32 = 0x800b_2ae8;
/// Entries of the table (0..0x1E).
pub const MOVIE_TABLE_ENTRIES: u32 = 0x1f;
/// Movie table entries that go to other states instead of the movie
/// player (`res_p1_states::movie_setup`): pictures (state 6), the purge
/// terminal (8) and the credits (7).
const NOT_MOVIES: [u32; 5] = [7, 0xc, 0x10, 0x1d, 0x1e];
/// The state-5 player's screen: 320x240, the frame at (0, 32).
pub const SCREEN: (u32, u32) = (320, 240);
pub const FRAME_ORIGIN: (u32, u32) = (0, 32);

/// One row of the movie table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TableEntry {
    pub index: u32,
    pub resource: u16,
    pub subtitles: u16,
    pub bank: u16,
}

/// The movie table from the resident `payload` (loaded at 0x80010000).
pub fn movie_table(payload: &[u8]) -> Vec<TableEntry> {
    let base = (MOVIE_TABLE - 0x8001_0000) as usize;
    (0..MOVIE_TABLE_ENTRIES)
        .filter_map(|index| {
            let at = base + 8 * index as usize;
            let half = |o: usize| u16::from_le_bytes([payload[at + o], payload[at + o + 1]]);
            (at + 8 <= payload.len()).then(|| TableEntry {
                index,
                resource: half(0),
                subtitles: half(2),
                bank: half(4),
            })
        })
        .collect()
}

/// Options of [`run`].
pub struct ExportOptions<'a> {
    pub discs: Vec<PathBuf>,
    pub output: &'a Path,
    /// Keys (`movie_09e6`) or resources (`9e6`) to export; empty: all.
    pub only: Vec<String>,
    /// `None`: the depth the game's player uses (24-bit for the movie
    /// table's movies, 15-bit for the hub's clips).
    pub depth: Option<Depth>,
    /// Write the manifest only (no frames).
    pub manifest_only: bool,
    pub threads: usize,
}

struct Job {
    info: MovieInfo,
    frames: Vec<Frame>,
    depth: Depth,
    discs: Vec<Value>,
    table: Vec<TableEntry>,
}

/// The player that shows a movie, and its MDEC depth, from the movie
/// table's entries that name it: none, the hub's clip player (15-bit, a
/// texture in the hub's pages); subtitle flag 1, the in-mission event
/// player (`res_p1_replay::movie_start`: 15-bit, the subtitle sprites of
/// overlay mode 7 over it); otherwise movie state 5 (24-bit; subtitle lines
/// in the letterbox, overlay mode 9).
pub fn player(uses: &[TableEntry]) -> (&'static str, Depth) {
    let movies: Vec<&TableEntry> = uses
        .iter()
        .filter(|e| !NOT_MOVIES.contains(&e.index))
        .collect();
    if movies.is_empty() {
        (
            "hub clip player (overlay 2): 15-bit, a texture on the hub's pages",
            Depth::Rgb15,
        )
    } else if movies.iter().all(|e| e.subtitles == 1) {
        (
            "in-mission event player: 15-bit full screen, subtitle sprites over it",
            Depth::Rgb15,
        )
    } else {
        ("movie state 5: 24-bit full screen", Depth::Rgb24)
    }
}

fn wanted(only: &[String], info: &MovieInfo) -> bool {
    only.is_empty()
        || only.iter().any(|item| {
            let item = item.trim().to_ascii_lowercase();
            item == info.key()
                || u16::from_str_radix(item.trim_start_matches("0x"), 16)
                    .is_ok_and(|r| r & 0xfff == info.resource & 0xfff)
        })
}

/// Export the movies; returns the manifest.
pub fn run(options: &ExportOptions<'_>) -> crate::Result<Value> {
    let started = Instant::now();
    let first = options.discs.first().ok_or("export-movies needs a disc")?;
    let payload = crate::assets::GameData::load_jp_disc(first)?
        .executable
        .payload;
    let table = movie_table(&payload);
    // Every movie of every disc; one job per key (the same resource on both
    // discs is one movie when its content is the same).
    let mut jobs: BTreeMap<String, Job> = BTreeMap::new();
    let mut conflicts = Vec::new();
    for path in &options.discs {
        let mut disc = MovieDisc::open(path)?;
        eprintln!(
            "ac3: export-movies: disc {} ({}): ACE.SPB at sector {}",
            disc.disc,
            path.display(),
            disc.spb_lba
        );
        for (info, frames) in disc.movies()? {
            if !wanted(&options.only, &info) {
                continue;
            }
            let source = json!({
                "disc": info.disc,
                "image": path.display().to_string(),
                "lba": info.lba,
                "sectors": info.sectors,
            });
            match jobs.get_mut(&info.key()) {
                Some(job) if job.info.content == info.content => job.discs.push(source),
                Some(_) => conflicts.push(json!({
                    "key": info.key(),
                    "disc": info.disc,
                    "note": "a different movie under the same resource; the first disc's is exported",
                })),
                None => {
                    let uses: Vec<TableEntry> = table
                        .iter()
                        .filter(|e| e.resource & 0xfff == info.resource & 0xfff)
                        .copied()
                        .collect();
                    let depth = options.depth.unwrap_or_else(|| player(&uses).1);
                    jobs.insert(
                        info.key(),
                        Job {
                            info,
                            frames,
                            depth,
                            discs: vec![source],
                            table: uses,
                        },
                    );
                }
            }
        }
    }
    std::fs::create_dir_all(options.output)?;
    let jobs: Vec<Job> = jobs.into_values().collect();
    let next = AtomicUsize::new(0);
    let results = Mutex::new(BTreeMap::new());
    let threads = options.threads.clamp(1, 64).min(jobs.len().max(1));
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                loop {
                    let at = next.fetch_add(1, Ordering::Relaxed);
                    let Some(job) = jobs.get(at) else {
                        break;
                    };
                    let result = export_movie(job, &payload, options);
                    results.lock().unwrap().insert(job.info.key(), result);
                }
            });
        }
    });
    let mut results = results.into_inner().unwrap();
    let mut movies = Vec::new();
    let (mut frames, mut bytes) = (0u64, 0u64);
    for job in &jobs {
        let key = job.info.key();
        let written = match results.remove(&key) {
            Some(Ok(written)) => written,
            Some(Err(error)) => return Err(format!("{key}: {error}").into()),
            None => return Err(format!("{key}: not exported").into()),
        };
        frames += written.frames;
        bytes += written.bytes;
        movies.push(movie_json(job, &written));
    }
    let manifest = json!({
        "format": "ac3-movies",
        "version": 1,
        "game": "Ace Combat 3: Electrosphere (Japan), English patch",
        "index": "frame files are numbered from the movie's start (000000.png = its first frame); the game's frame number counts down: number = first_number - index",
        "movies": movies,
        "conflicts": conflicts,
        "frames_written": frames,
        "bytes_written": bytes,
        "seconds": started.elapsed().as_secs_f64(),
    });
    std::fs::write(
        options.output.join("manifest.json"),
        serde_json::to_string_pretty(&manifest)? + "\n",
    )?;
    Ok(manifest)
}

#[derive(Default)]
struct Written {
    frames: u64,
    bytes: u64,
    /// Frames the decoder could not decode (not MDEC video).
    skipped: Vec<u16>,
    /// Frame size changes: (first index, width, height).
    sizes: Vec<(u32, u16, u16)>,
    stalls: u64,
}

fn export_movie(job: &Job, payload: &[u8], options: &ExportOptions<'_>) -> crate::Result<Written> {
    let folder = options.output.join(job.info.key());
    std::fs::create_dir_all(&folder)?;
    let mut decoder = FrameDecoder::new(payload, job.depth);
    let mut written = Written::default();
    for frame in &job.frames {
        let index = u32::from(job.info.first_number.saturating_sub(frame.number));
        if written
            .sizes
            .last()
            .is_none_or(|&(_, w, h)| (w, h) != (frame.width, frame.height))
        {
            written.sizes.push((index, frame.width, frame.height));
        }
        if options.manifest_only {
            continue;
        }
        let Some(rgb) = decoder.decode(frame) else {
            written.skipped.push(frame.number);
            continue;
        };
        let path = folder.join(format!("{index:06}.png"));
        let png = encode_png(&rgb, frame.width.into(), frame.height.into())?;
        written.bytes += png.len() as u64;
        std::fs::write(&path, png)?;
        written.frames += 1;
    }
    written.stalls = decoder.stalls();
    Ok(written)
}

/// A lossless 8-bit RGB PNG of `rgb` (0RGB words).
pub fn encode_png(rgb: &[u32], width: u32, height: u32) -> crate::Result<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        let bytes: Vec<u8> = rgb
            .iter()
            .flat_map(|&p| [(p >> 16) as u8, (p >> 8) as u8, p as u8])
            .collect();
        encoder.write_header()?.write_image_data(&bytes)?;
    }
    Ok(out)
}

fn movie_json(job: &Job, written: &Written) -> Value {
    let info = &job.info;
    let (player, game_depth) = player(&job.table);
    let uses: Vec<Value> = job
        .table
        .iter()
        .filter(|e| !NOT_MOVIES.contains(&e.index))
        .map(|e| {
            json!({
                "entry": e.index,
                "subtitles": match e.subtitles {
                    2 => "subtitle lines in the letterbox (overlay mode 9)",
                    1 => "in-mission subtitle sprites (overlay mode 7)",
                    _ => "none",
                },
                "subtitle_bank": (e.bank != 0).then_some(e.bank),
            })
        })
        .collect();
    let screen_rect = if game_depth == Depth::Rgb15 && job.table.is_empty() {
        Value::Null
    } else {
        json!([FRAME_ORIGIN.0, FRAME_ORIGIN.1, info.width, info.height])
    };
    json!({
        "key": info.key(),
        "resource": format!("0x{:03x}", info.resource),
        "channel": info.channel,
        "sources": job.discs,
        "frames": info.frames,
        "shown_frames": info.frames.saturating_sub(1),
        "first_number": info.first_number,
        "frame_size": [info.width, info.height],
        "sizes": written.sizes.iter().map(|&(i, w, h)| json!({"from": i, "size": [w, h]})).collect::<Vec<_>>(),
        "screen": [SCREEN.0, SCREEN.1],
        "active_rect": screen_rect,
        "depth": job.depth.bits(),
        "game_depth": game_depth.bits(),
        "player": player,
        "table_entries": uses,
        "fps": info.fps,
        "vblanks_per_frame": info.vblanks_per_frame,
        "seconds": f64::from(info.frames) / info.fps,
        "bitstream_versions": info.versions,
        "form2": info.form2,
        "audio": info.audio,
        "content_sha256": info.content,
        "files": format!("{}/{{index:06}}.png", info.key()),
        "frames_written": written.frames,
        "bytes_written": written.bytes,
        "skipped_frame_numbers": written.skipped,
        "mdec_stalls": written.stalls,
    })
}
