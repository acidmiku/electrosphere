//! Movie packs: replacement images for the movies' frames, shown in place
//! of the game's MDEC frames on the picture only (`docs/MOVIES.md`).
//!
//! - **Pack layout:** one folder per movie key (`movie_09e6`, as
//!   `ac3 export-movies` names them) of frame images named by number
//!   (`000000.png`, `.jpg`/`.jpeg` or `.webp`). Optional `movie.toml` in a
//!   movie's folder (or `pack.toml` for every movie): `subframes` (images
//!   per game frame, for frame-interpolated packs; default 1) and
//!   `first_file` (the number of the first image; default 0, or 1 when the
//!   folder starts at 1).
//! - **Which image:** the game's frame `index` (from the movie's start,
//!   `first_number - number`, [`crate::movie_disc`]) shows images
//!   `first_file + index * subframes + k`, `k` in `0..subframes`, spread
//!   over the game frame's VBlanks.
//! - **Identity:** the frame comes from the decoder's own frame number at
//!   each MDEC decode ([`MovieTracker`]), so a frame the game drops or
//!   repeats drops or repeats its image; the screen shows the image exactly
//!   while the game's field holds that frame.
//! - **Decoding:** worker threads decode ahead of the shown frame into a
//!   cache bounded by bytes ([`Loader`]); a headless run waits for each
//!   image, a window run until its next pass is due, and shows the game's
//!   frame if the image is still not ready.
//! - **Overlays:** the pixels of the frame the game drew over after the
//!   MDEC wrote them (subtitles, fades) stay the game's ([`MovieLayer::mask`]).
//! - **Missing images** show the game's frame; each movie is logged once.
//! - **The hub's clips** (128x96, drawn by the hub as a texture) are not
//!   shown frames: each one complete in VRAM goes to the GPU renderer
//!   ([`MovieReplacer::on_load_image`] gives the host a [`MovieFrame`]),
//!   which draws its texels from the pack's image of that frame
//!   ([`HubImages`]; `gpu_renderer::replace`, "Movie frames"). Only above
//!   1x: the software renderer and 1x draw the game's frames.
use crate::gpu_renderer::replace::{MovieFrame, MovieImages};
use crate::gpu_renderer::tiles::Rect;
use crate::machine_movie::{MovieTracker, ShownFrame};
use crate::movie_disc::MovieDisc;
use crate::movie_pack_mkv::FrameSpan;
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};

/// A decoded replacement image: RGBA, 8 bits a channel, row by row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// The largest image side accepted.
pub const MAX_SIDE: u32 = 16384;

/// Decode the PNG, JPEG or WebP image `bytes` (by `extension`).
pub fn decode_image(bytes: &[u8], extension: &str) -> Result<RgbaImage, String> {
    let image = match extension.to_ascii_lowercase().as_str() {
        "png" => decode_png(bytes),
        "jpg" | "jpeg" => decode_jpeg(bytes),
        "webp" => decode_webp(bytes),
        other => Err(format!("unsupported image type .{other}")),
    }?;
    if image.width == 0
        || image.height == 0
        || image.width > MAX_SIDE
        || image.height > MAX_SIDE
        || image.pixels.len() != image.width as usize * image.height as usize * 4
    {
        return Err(format!("bad image size {}x{}", image.width, image.height));
    }
    Ok(image)
}

/// Expand `channels`-per-pixel 8-bit data (1 grey, 2 grey+alpha, 3 RGB, 4
/// RGBA) to RGBA.
fn to_rgba(data: &[u8], channels: usize, width: u32, height: u32) -> Result<RgbaImage, String> {
    let count = width as usize * height as usize;
    if data.len() < count * channels {
        return Err("truncated image data".to_owned());
    }
    let mut pixels = Vec::with_capacity(count * 4);
    match channels {
        4 => pixels.extend_from_slice(&data[..count * 4]),
        3 => {
            for p in data[..count * 3].chunks_exact(3) {
                pixels.extend_from_slice(&[p[0], p[1], p[2], 255]);
            }
        }
        2 => {
            for p in data[..count * 2].chunks_exact(2) {
                pixels.extend_from_slice(&[p[0], p[0], p[0], p[1]]);
            }
        }
        1 => {
            for &g in &data[..count] {
                pixels.extend_from_slice(&[g, g, g, 255]);
            }
        }
        _ => return Err(format!("{channels} channels")),
    }
    Ok(RgbaImage {
        width,
        height,
        pixels,
    })
}

fn decode_png(bytes: &[u8]) -> Result<RgbaImage, String> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let size = reader
        .output_buffer_size()
        .ok_or("the image is too large")?;
    let mut buffer = vec![0; size];
    let frame = reader.next_frame(&mut buffer).map_err(|e| e.to_string())?;
    let channels = frame.color_type.samples();
    let (width, height) = (frame.width, frame.height);
    let row = width as usize * channels;
    if frame.line_size != row {
        let mut packed = Vec::with_capacity(row * height as usize);
        for line in buffer.chunks(frame.line_size).take(height as usize) {
            packed.extend_from_slice(&line[..row]);
        }
        buffer = packed;
    }
    to_rgba(&buffer, channels, width, height)
}

fn decode_jpeg(bytes: &[u8]) -> Result<RgbaImage, String> {
    use zune_jpeg::zune_core::{
        bytestream::ZCursor, colorspace::ColorSpace, options::DecoderOptions,
    };
    let options = DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::RGBA)
        .set_max_width(MAX_SIDE as usize)
        .set_max_height(MAX_SIDE as usize);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(bytes), options);
    let pixels = decoder.decode().map_err(|e| format!("{e:?}"))?;
    let (width, height) = decoder.dimensions().ok_or("no JPEG dimensions")?;
    to_rgba(&pixels, 4, width as u32, height as u32)
}

fn decode_webp(bytes: &[u8]) -> Result<RgbaImage, String> {
    let mut decoder =
        image_webp::WebPDecoder::new(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let (width, height) = decoder.dimensions();
    let channels = if decoder.has_alpha() { 4 } else { 3 };
    let size = decoder
        .output_buffer_size()
        .ok_or("the image is too large")?;
    let mut buffer = vec![0; size];
    decoder.read_image(&mut buffer).map_err(|e| e.to_string())?;
    to_rgba(&buffer, channels, width, height)
}

/// The image types a pack may hold, by extension.
pub const EXTENSIONS: [&str; 4] = ["png", "jpg", "jpeg", "webp"];

/// One movie's folder in a pack.
#[derive(Clone, Debug, Default)]
pub struct PackMovie {
    /// Images by number: files, or the frames of a Motion JPEG `.mkv`.
    pub files: BTreeMap<u32, Source>,
    /// Images per game frame.
    pub subframes: u32,
    /// Number of the image of frame 0.
    pub first_file: u32,
}

/// Where a pack image comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// An image file (PNG, JPEG or WebP by its extension).
    Image(PathBuf),
    /// A frame of a Motion JPEG Matroska file ([`crate::movie_pack_mkv`]).
    Mkv(Arc<PathBuf>, FrameSpan),
}

impl Source {
    /// Its compressed size in bytes.
    pub fn bytes(&self) -> u64 {
        match self {
            Self::Image(path) => std::fs::metadata(path).map_or(0, |m| m.len()),
            Self::Mkv(_, span) => u64::from(span.length),
        }
    }
    /// Read and decode it.
    pub fn load(&self) -> Result<RgbaImage, String> {
        match self {
            Self::Image(path) => {
                let extension = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or_default();
                let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
                decode_image(&bytes, extension).map_err(|e| format!("{}: {e}", path.display()))
            }
            Self::Mkv(path, span) => {
                let bytes = crate::movie_pack_mkv::read_span(path, *span)?;
                decode_jpeg(&bytes)
                    .map_err(|e| format!("{} (frame at byte {}): {e}", path.display(), span.offset))
            }
        }
    }
}

/// `movie.toml` / `pack.toml`.
#[derive(Clone, Copy, Debug, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
struct MovieConfig {
    subframes: Option<u32>,
    first_file: Option<u32>,
}

fn read_config(path: &Path) -> Result<MovieConfig, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(MovieConfig::default()),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

/// A movie pack folder, scanned.
#[derive(Clone, Debug, Default)]
pub struct MoviePack {
    pub root: PathBuf,
    pub movies: BTreeMap<String, PackMovie>,
    /// What the scan found odd (unknown files, bad configs).
    pub notes: Vec<String>,
}

impl MoviePack {
    /// Scan `root`: its `movie_xxxx` folders and their numbered images.
    pub fn open(root: &Path) -> Result<Self, String> {
        let defaults = read_config(&root.join("pack.toml"))?;
        let entries = std::fs::read_dir(root).map_err(|e| format!("{}: {e}", root.display()))?;
        let mut pack = Self {
            root: root.to_owned(),
            ..Self::default()
        };
        let mut folders: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_dir())
            .collect();
        folders.sort();
        for folder in folders {
            let Some(key) = folder
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_ascii_lowercase)
                .filter(|n| is_key(n))
            else {
                continue;
            };
            let config = match read_config(&folder.join("movie.toml")) {
                Ok(config) => config,
                Err(error) => {
                    pack.notes.push(error);
                    MovieConfig::default()
                }
            };
            let mut files = BTreeMap::new();
            let mut videos = Vec::new();
            if let Ok(entries) = std::fs::read_dir(&folder) {
                for path in entries.filter_map(|e| e.ok().map(|e| e.path())) {
                    let (Some(stem), Some(extension)) = (
                        path.file_stem().and_then(|s| s.to_str()),
                        path.extension().and_then(|s| s.to_str()),
                    ) else {
                        continue;
                    };
                    if extension.eq_ignore_ascii_case("mkv") {
                        videos.push(path.clone());
                        continue;
                    }
                    if !EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str()) {
                        continue;
                    }
                    // A number, possibly after a prefix (frame_000123).
                    let digits: String = stem
                        .chars()
                        .rev()
                        .take_while(char::is_ascii_digit)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect();
                    if let Ok(number) = digits.parse::<u32>()
                        && files.insert(number, Source::Image(path.clone())).is_some()
                    {
                        pack.notes.push(format!(
                            "{key}: two images numbered {number}; {} is used",
                            path.display()
                        ));
                    }
                }
            }
            // No images: one Motion JPEG file, its frames numbered from 0.
            if files.is_empty() {
                videos.sort();
                match videos.first() {
                    Some(video) => match crate::movie_pack_mkv::mjpeg_frames(video) {
                        Ok(frames) => {
                            let video = Arc::new(video.clone());
                            files.extend(
                                frames
                                    .into_iter()
                                    .enumerate()
                                    .map(|(n, span)| (n as u32, Source::Mkv(video.clone(), span))),
                            );
                        }
                        Err(error) => pack.notes.push(format!("{key}: {error}")),
                    },
                    None => continue,
                }
                if files.is_empty() {
                    continue;
                }
            }
            let lowest = *files.keys().next().unwrap();
            let first_file = config
                .first_file
                .or(defaults.first_file)
                .unwrap_or(if lowest == 1 { 1 } else { 0 });
            let subframes = config
                .subframes
                .or(defaults.subframes)
                .unwrap_or(1)
                .clamp(1, 16);
            pack.movies.insert(
                key,
                PackMovie {
                    files,
                    subframes,
                    first_file,
                },
            );
        }
        Ok(pack)
    }

    pub fn len(&self) -> usize {
        self.movies.len()
    }
    pub fn is_empty(&self) -> bool {
        self.movies.is_empty()
    }
    /// Images in all.
    pub fn images(&self) -> usize {
        self.movies.values().map(|m| m.files.len()).sum()
    }

    /// One line for the log.
    pub fn summary(&self) -> String {
        let interpolated = self.movies.values().filter(|m| m.subframes > 1).count();
        format!(
            "{} ({} movies, {} images{})",
            self.root.display(),
            self.len(),
            self.images(),
            if interpolated > 0 {
                format!(", {interpolated} with several images a frame")
            } else {
                String::new()
            }
        )
    }
}

/// `movie_` and four hex digits.
fn is_key(name: &str) -> bool {
    name.len() == 10
        && name.starts_with("movie_")
        && name[6..].chars().all(|c| c.is_ascii_hexdigit())
}

// ---- the decoding threads ----------------------------------------------------------------

type ImageKey = (Arc<str>, u32);

#[derive(Clone)]
enum Slot {
    Pending,
    Ready(Arc<RgbaImage>),
    Failed(String),
}

#[derive(Default)]
struct LoaderState {
    queue: VecDeque<(ImageKey, Source)>,
    slots: HashMap<ImageKey, Slot>,
    bytes: usize,
    stop: bool,
    decoded: u64,
    decode_seconds: f64,
    failed: u64,
}

/// Worker threads decoding pack images into a cache bounded by bytes.
pub struct Loader {
    shared: Arc<(Mutex<LoaderState>, Condvar)>,
    workers: Vec<std::thread::JoinHandle<()>>,
    /// Cache budget in bytes.
    pub budget: usize,
}

/// Decoding statistics for the report.
#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct LoaderStats {
    pub decoded: u64,
    pub failed: u64,
    pub mean_decode_ms: f64,
    pub cached_bytes: usize,
}

impl Loader {
    /// `threads` workers, at most `budget` bytes of decoded images.
    pub fn new(threads: usize, budget: usize) -> Self {
        let shared = Arc::new((Mutex::new(LoaderState::default()), Condvar::new()));
        let workers = (0..threads.max(1))
            .map(|n| {
                let shared = shared.clone();
                std::thread::Builder::new()
                    .name(format!("ac3-movie-{n}"))
                    .spawn(move || worker(&shared))
                    .expect("movie decoding thread")
            })
            .collect();
        Self {
            shared,
            workers,
            budget,
        }
    }

    /// Queue `source` as `key` unless it is cached or queued already.
    fn request(&self, key: ImageKey, source: &Source) {
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().unwrap();
        if state.slots.contains_key(&key) {
            return;
        }
        state.slots.insert(key.clone(), Slot::Pending);
        state.queue.push_back((key, source.clone()));
        wake.notify_one();
    }

    /// The image of `key` once decoded, waiting until `deadline` (`None`:
    /// for as long as it takes) while it is being decoded.
    fn get(
        &self,
        key: &ImageKey,
        deadline: Option<Instant>,
    ) -> Option<Result<Arc<RgbaImage>, String>> {
        cached(&self.shared, key, deadline)
    }

    /// Drop cached and queued images outside `keep` (by key) and, while
    /// over the budget, the cached ones farthest from it.
    fn retain(&self, keep: &dyn Fn(&ImageKey) -> Option<u32>) {
        let (lock, _) = &*self.shared;
        let mut state = lock.lock().unwrap();
        state.queue.retain(|(key, _)| keep(key).is_some());
        let LoaderState { slots, bytes, .. } = &mut *state;
        slots.retain(|key, slot| {
            let kept = keep(key).is_some() || matches!(slot, Slot::Pending);
            if !kept && let Slot::Ready(image) = slot {
                *bytes -= image.pixels.len();
            }
            kept
        });
        if state.bytes > self.budget {
            let mut ready: Vec<(u32, ImageKey, usize)> = state
                .slots
                .iter()
                .filter_map(|(key, slot)| match slot {
                    Slot::Ready(image) => Some((
                        keep(key).unwrap_or(u32::MAX),
                        key.clone(),
                        image.pixels.len(),
                    )),
                    _ => None,
                })
                .collect();
            ready.sort_by_key(|(distance, ..)| std::cmp::Reverse(*distance));
            for (_, key, size) in ready {
                if state.bytes <= self.budget {
                    break;
                }
                state.slots.remove(&key);
                state.bytes -= size;
            }
        }
    }

    /// Queue image `file` of movie `key` from `source` (benchmarks).
    pub fn request_source(&self, key: Arc<str>, file: u32, source: &Source) {
        self.request((key, file), source);
    }

    /// Wait for image `file` of movie `key` (`None`: never queued).
    pub fn wait_for(&self, key: &Arc<str>, file: u32) -> Option<Result<Arc<RgbaImage>, String>> {
        self.get(&(key.clone(), file), None)
    }

    /// Forget everything (a reload).
    fn clear(&self) {
        let (lock, _) = &*self.shared;
        let mut state = lock.lock().unwrap();
        state.queue.clear();
        state.slots.retain(|_, slot| matches!(slot, Slot::Pending));
        state.bytes = 0;
    }

    pub fn stats(&self) -> LoaderStats {
        let state = self.shared.0.lock().unwrap();
        LoaderStats {
            decoded: state.decoded,
            failed: state.failed,
            mean_decode_ms: if state.decoded > 0 {
                state.decode_seconds * 1000.0 / state.decoded as f64
            } else {
                0.0
            },
            cached_bytes: state.bytes,
        }
    }
}

impl Drop for Loader {
    fn drop(&mut self) {
        {
            let (lock, wake) = &*self.shared;
            lock.lock().unwrap().stop = true;
            wake.notify_all();
        }
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

/// The image of `key` in the loader's cache once decoded, waiting until
/// `deadline` (`None`: for as long as it takes) while it is being decoded;
/// `None` when it was never queued (or was dropped).
fn cached(
    shared: &(Mutex<LoaderState>, Condvar),
    key: &ImageKey,
    deadline: Option<Instant>,
) -> Option<Result<Arc<RgbaImage>, String>> {
    let (lock, wake) = shared;
    let mut state = lock.lock().unwrap();
    loop {
        match state.slots.get(key) {
            None => return None,
            Some(Slot::Ready(image)) => return Some(Ok(image.clone())),
            Some(Slot::Failed(error)) => return Some(Err(error.clone())),
            Some(Slot::Pending) => {}
        }
        state = match deadline {
            None => wake.wait(state).unwrap(),
            Some(deadline) => {
                let now = Instant::now();
                if now >= deadline {
                    return None;
                }
                wake.wait_timeout(state, deadline - now).unwrap().0
            }
        };
    }
}

/// The pack's decoded images for the hub's clips in the GPU renderer
/// ([`MovieImages`]): the images [`MovieReplacer::on_load_image`] queued,
/// waited for in headless runs and never in a window (a frame whose image
/// is not decoded yet draws the game's texels until it is). A broken
/// image is logged once per movie.
pub struct HubImages {
    shared: Arc<(Mutex<LoaderState>, Condvar)>,
    wait: bool,
    failed: RefCell<HashSet<Arc<str>>>,
}

impl MovieImages for HubImages {
    fn image(&self, key: &Arc<str>, file: u32) -> Option<Arc<RgbaImage>> {
        let deadline = (!self.wait).then(Instant::now);
        match cached(&self.shared, &(key.clone(), file), deadline)? {
            Ok(image) => Some(image),
            Err(error) => {
                if self.failed.borrow_mut().insert(key.clone()) {
                    eprintln!("ac3: movie pack: {error}; the game's frames are drawn");
                }
                None
            }
        }
    }
}

fn worker(shared: &(Mutex<LoaderState>, Condvar)) {
    let (lock, wake) = shared;
    loop {
        let (key, source) = {
            let mut state = lock.lock().unwrap();
            loop {
                if state.stop {
                    return;
                }
                if let Some(job) = state.queue.pop_front() {
                    break job;
                }
                state = wake.wait(state).unwrap();
            }
        };
        let started = Instant::now();
        let result = source.load();
        let seconds = started.elapsed().as_secs_f64();
        let mut state = lock.lock().unwrap();
        match result {
            Ok(image) => {
                state.decoded += 1;
                state.decode_seconds += seconds;
                // A slot dropped meanwhile (a reload) is not filled again.
                if state.slots.contains_key(&key) {
                    state.bytes += image.pixels.len();
                    state.slots.insert(key, Slot::Ready(Arc::new(image)));
                }
            }
            Err(error) => {
                state.failed += 1;
                if state.slots.contains_key(&key) {
                    state.slots.insert(key, Slot::Failed(error));
                }
            }
        }
        wake.notify_all();
    }
}

// ---- the replacer --------------------------------------------------------------------------

/// What the presenter draws over the movie frame: the replacement image in
/// the frame's rectangle of the 1x picture, under the game's overlay
/// pixels.
#[derive(Clone, Debug)]
pub struct MovieLayer {
    pub image: Arc<RgbaImage>,
    /// Changes exactly when `image` does.
    pub generation: u64,
    /// The 1x picture's size and the frame's rectangle in it.
    pub picture: (u32, u32),
    pub rect: [u32; 4],
    /// Per pixel of `rect` (row by row), 255 where the game's own pixel
    /// stays on top; `None`: nowhere.
    pub mask: Option<Arc<Vec<u8>>>,
    /// Changes exactly when `mask` does.
    pub mask_generation: u64,
}

/// Counters for the run report.
#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct ReplacerStats {
    /// Passes that showed a movie frame, and those that showed its image.
    pub frames_shown: u64,
    pub frames_replaced: u64,
    /// Shown frames without an image (missing in the pack), and those whose
    /// image was not decoded in time.
    pub missing: u64,
    pub late: u64,
    /// Shown frames with overlay pixels kept.
    pub with_overlay: u64,
    /// Hub clip frames complete in VRAM with an image in the pack (handed
    /// to the GPU renderer), and those without one.
    pub hub_frames: u64,
    pub hub_missing: u64,
}

/// The frame shown now and its images.
#[derive(Clone, Debug)]
struct Current {
    key: Arc<str>,
    /// VBlanks per frame at the movie's rate.
    frame_vblanks: f64,
    index: u32,
    serial: u64,
    /// The VBlank the frame was first seen in the shown field.
    first_vblank: u64,
    rect: [u32; 4],
    picture: (u32, u32),
    mask: Option<Arc<Vec<u8>>>,
    mask_generation: u64,
}

/// The movie pack in a run: the [`MovieTracker`] the host feeds, the pack
/// and its decoding threads, and what the screen shows now.
pub struct MovieReplacer {
    pub tracker: MovieTracker,
    pack: MoviePack,
    root: PathBuf,
    /// The disc images, for each movie's first frame number.
    discs: Vec<PathBuf>,
    facts: HashMap<u16, Option<(u16, f64)>>,
    loader: Loader,
    /// Wait for every image (headless runs and tests).
    pub wait: bool,
    /// Images decoded ahead of the shown one.
    pub ahead: u32,
    logged: HashSet<String>,
    current: Option<Current>,
    generation: u64,
    /// Generation of each image handed out, by (key, file).
    generations: HashMap<ImageKey, u64>,
    /// Bytes of the last decoded image (bounds the decoding ahead).
    image_bytes: usize,
    pub stats: ReplacerStats,
    /// Hub clip frames complete in VRAM, by movie (in the pack or not).
    pub hub_seen: BTreeMap<String, u64>,
}

/// Decoded images kept: 768 MiB.
pub const DEFAULT_BUDGET: usize = 768 << 20;

impl MovieReplacer {
    /// The pack at `root` (logged; an unreadable or empty folder replaces
    /// nothing), with the run's disc images.
    pub fn open(root: &Path, discs: Vec<PathBuf>, wait: bool) -> Self {
        let pack = Self::scan(root);
        let threads = std::thread::available_parallelism()
            .map_or(4, |n| n.get())
            .saturating_sub(2)
            .clamp(2, 8);
        Self {
            tracker: MovieTracker::default(),
            pack,
            root: root.to_owned(),
            discs,
            facts: HashMap::new(),
            loader: Loader::new(threads, DEFAULT_BUDGET),
            wait,
            ahead: 12,
            logged: HashSet::new(),
            current: None,
            generation: 0,
            generations: HashMap::new(),
            image_bytes: 0,
            stats: ReplacerStats::default(),
            hub_seen: BTreeMap::new(),
        }
    }

    fn scan(root: &Path) -> MoviePack {
        match MoviePack::open(root) {
            Ok(pack) => {
                eprintln!("ac3: movie pack: {}", pack.summary());
                for note in &pack.notes {
                    eprintln!("ac3: movie pack: {note}");
                }
                pack
            }
            Err(error) => {
                eprintln!("ac3: movie pack: {error} (the game's own movie frames are shown)");
                MoviePack {
                    root: root.to_owned(),
                    ..MoviePack::default()
                }
            }
        }
    }

    pub fn pack(&self) -> &MoviePack {
        &self.pack
    }
    pub fn loader_stats(&self) -> LoaderStats {
        self.loader.stats()
    }

    /// After the MDEC device answered `function` (`args`): the tracker
    /// follows it, and a movie frame the game starts decoding has its image
    /// queued at once (a pass before it shows).
    pub fn on_mdec(&mut self, ram: &[u8], function: u32, args: [u32; 4]) {
        let Some(id) = self.tracker.on_mdec(ram, function, args) else {
            return;
        };
        let key: Arc<str> = crate::movie_disc::key_of(id.resource).into();
        let Some((subframes, first_file)) = self
            .pack
            .movies
            .get(&*key)
            .map(|m| (m.subframes, m.first_file))
        else {
            return;
        };
        if let Some((first, _)) = self.facts(id.resource) {
            let index = u32::from(first.saturating_sub(id.number));
            self.prefetch(&key, index, subframes, first_file);
        }
    }

    /// Before LoadImage(`rect`, `pixels`) writes VRAM (the tracker). When
    /// it completes a hub clip's frame whose image the pack has: the frame
    /// for the GPU renderer, to hand over once the upload is in (its image
    /// queued for decoding).
    pub fn on_load_image(&mut self, ram: &[u8], rect: [u16; 4], pixels: u32) -> Option<MovieFrame> {
        let done = self
            .tracker
            .on_load_image(ram, rect, pixels)
            .filter(|done| done.id.hub)?;
        let key: Arc<str> = crate::movie_disc::key_of(done.id.resource).into();
        *self.hub_seen.entry(key.to_string()).or_default() += 1;
        let movie = self.pack.movies.get(&*key)?;
        let (subframes, first_file) = (movie.subframes, movie.first_file);
        let Some((first, _)) = self.facts(done.id.resource) else {
            self.log_once(&key, || {
                format!("{key}: not found on the discs; its frames are the game's")
            });
            return None;
        };
        let index = u32::from(first.saturating_sub(done.id.number));
        let file = first_file + index * subframes;
        let Some(source) = self.pack.movies[&*key].files.get(&file).cloned() else {
            self.stats.hub_missing += 1;
            self.log_once(&key, || {
                format!(
                    "{key}: no image for frame {index} (file {file}); the game's frames are drawn \
                     where images are missing"
                )
            });
            return None;
        };
        self.loader.request((key.clone(), file), &source);
        self.stats.hub_frames += 1;
        let [x, y, w, h] = done.rect.map(i32::from);
        Some(MovieFrame {
            rect: Rect::new(x, y, w, h),
            key,
            file,
            stp: done.stp,
        })
    }

    /// The pack's images for the GPU renderer's hub clips (headless runs
    /// wait for them, as for shown frames).
    pub fn hub_images(&self) -> HubImages {
        HubImages {
            shared: self.loader.shared.clone(),
            wait: self.wait,
            failed: RefCell::default(),
        }
    }

    /// F9: scan the folder again; returns the note to show.
    pub fn reload(&mut self) -> String {
        self.loader.clear();
        self.generations.clear();
        self.logged.clear();
        self.pack = Self::scan(&self.root);
        self.current = None;
        format!("Movie pack loaded: {} movies", self.pack.len())
    }

    /// The first frame number of `resource`'s movie and its frame rate,
    /// from the discs.
    fn facts(&mut self, resource: u16) -> Option<(u16, f64)> {
        let discs = &self.discs;
        *self.facts.entry(resource).or_insert_with(|| {
            discs.iter().find_map(|path| {
                MovieDisc::open(path)
                    .ok()?
                    .movie_facts(resource)
                    .ok()
                    .flatten()
            })
        })
    }

    fn log_once(&mut self, key: &str, text: impl FnOnce() -> String) {
        if self.logged.insert(key.to_owned()) {
            eprintln!("ac3: movie pack: {}", text());
        }
    }

    /// The pass's shown movie frame (`shown`, from [`MovieTracker::shown`]
    /// on the shown field) at VBlank `vblank`: the layer of its first image,
    /// waiting for it until `deadline` (never past it; `None` with
    /// [`Self::wait`] off: no wait). `None`: the game's frame is shown.
    pub fn update(
        &mut self,
        shown: Option<ShownFrame>,
        vblank: u64,
        deadline: Option<Instant>,
    ) -> Option<MovieLayer> {
        let Some(shown) = shown else {
            self.current = None;
            return None;
        };
        self.stats.frames_shown += 1;
        let key: Arc<str> = crate::movie_disc::key_of(shown.id.resource).into();
        let Some(movie) = self.pack.movies.get(&*key) else {
            self.current = None;
            return None;
        };
        let (subframes, first_file) = (movie.subframes, movie.first_file);
        let Some((first, fps)) = self.facts(shown.id.resource) else {
            self.log_once(&key, || {
                format!("{key}: not found on the discs; its frames are the game's")
            });
            self.current = None;
            return None;
        };
        let index = u32::from(first.saturating_sub(shown.id.number));
        let same = self
            .current
            .as_ref()
            .is_some_and(|c| c.serial == shown.serial && *c.key == *key);
        if !same {
            let mask = (shown.overlay_pixels > 0).then(|| {
                Arc::new(
                    shown
                        .overlay
                        .iter()
                        .map(|&o| if o { 255 } else { 0 })
                        .collect::<Vec<u8>>(),
                )
            });
            self.generation += 1;
            self.current = Some(Current {
                key: key.clone(),
                frame_vblanks: crate::movie_disc::FIELDS_PER_SECOND / fps.max(1.0),
                index,
                serial: shown.serial,
                first_vblank: vblank,
                rect: shown.rect,
                picture: shown.picture,
                mask,
                mask_generation: self.generation,
            });
        } else if let Some(current) = self.current.as_mut() {
            // Overlay pixels can change while the frame stays (a subtitle
            // line appears).
            let changed = match &current.mask {
                Some(mask) => {
                    mask.len() != shown.overlay.len()
                        || mask
                            .iter()
                            .zip(&shown.overlay)
                            .any(|(&m, &o)| (m != 0) != o)
                }
                None => shown.overlay_pixels > 0,
            };
            if changed {
                self.generation += 1;
                current.mask_generation = self.generation;
                current.mask = (shown.overlay_pixels > 0).then(|| {
                    Arc::new(
                        shown
                            .overlay
                            .iter()
                            .map(|&o| if o { 255 } else { 0 })
                            .collect(),
                    )
                });
            }
        }
        if shown.overlay_pixels > 0 {
            self.stats.with_overlay += 1;
        }
        self.prefetch(&key, index, subframes, first_file);
        let file = first_file + index * subframes;
        let wait = if self.wait {
            None
        } else {
            Some(deadline.unwrap_or_else(Instant::now))
        };
        let layer = self.layer_for(&key, file, wait);
        match &layer {
            Some(_) => self.stats.frames_replaced += 1,
            None if self.has_file(&key, file) => self.stats.late += 1,
            None => self.stats.missing += 1,
        }
        layer
    }

    fn has_file(&self, key: &str, file: u32) -> bool {
        self.pack
            .movies
            .get(key)
            .is_some_and(|m| m.files.contains_key(&file))
    }

    /// Queue the frames from `index` on and drop the rest of the cache.
    fn prefetch(&mut self, key: &Arc<str>, index: u32, subframes: u32, first_file: u32) {
        let Some(movie) = self.pack.movies.get(&**key) else {
            return;
        };
        let start = first_file + index * subframes;
        // Ahead as far as the cache holds (two thirds of it, so the images
        // behind and those decoding fit too).
        let fits = match self.image_bytes {
            0 => u32::MAX,
            bytes => (self.loader.budget * 2 / 3 / bytes).max(2) as u32,
        };
        let end = start + ((self.ahead + 1) * subframes).min(fits);
        for file in start..end {
            if let Some(source) = movie.files.get(&file) {
                self.loader.request((key.clone(), file), source);
            }
        }
        let back = start.saturating_sub(subframes * 2);
        let keep = |(k, file): &ImageKey| {
            (**k == **key && (back..end).contains(file)).then(|| file.abs_diff(start))
        };
        self.loader.retain(&keep);
    }

    /// The layer of image `file` of `key` (logging a missing or broken one
    /// once per movie).
    fn layer_for(
        &mut self,
        key: &Arc<str>,
        file: u32,
        wait: Option<Instant>,
    ) -> Option<MovieLayer> {
        let current = self.current.clone()?;
        if !self.has_file(key, file) {
            self.log_once(key, || {
                format!(
                    "{key}: no image for frame {} (file {file}); the game's frames are shown where \
                     images are missing",
                    current.index
                )
            });
            return None;
        }
        let image_key = (key.clone(), file);
        let image = match self.loader.get(&image_key, wait) {
            Some(Ok(image)) => image,
            Some(Err(error)) => {
                self.log_once(&format!("{key}/error"), || error);
                return None;
            }
            None => return None,
        };
        self.image_bytes = image.pixels.len();
        let generation = match self.generations.get(&image_key) {
            Some(&generation) => generation,
            None => {
                self.generation += 1;
                if self.generations.len() > 4096 {
                    self.generations.clear();
                }
                self.generations.insert(image_key, self.generation);
                self.generation
            }
        };
        Some(MovieLayer {
            image,
            generation,
            picture: current.picture,
            rect: current.rect,
            mask: current.mask.clone(),
            mask_generation: current.mask_generation,
        })
    }

    /// Images per game frame of the movie shown now (1 when none).
    pub fn subframes(&self) -> u32 {
        self.current
            .as_ref()
            .and_then(|c| self.pack.movies.get(&*c.key))
            .map_or(1, |m| m.subframes)
    }

    /// The shown frame's VBlanks per frame at its movie's rate (the drive's
    /// pacing: 4 at 15 fps, 2 at 30 fps).
    pub fn frame_vblanks(&self) -> f64 {
        self.current.as_ref().map_or(4.0, |c| c.frame_vblanks)
    }

    /// The layer of image `k` (0..[`Self::subframes`]) of the shown frame,
    /// without waiting; `None` while it is not decoded.
    pub fn subframe(&mut self, k: u32) -> Option<MovieLayer> {
        let current = self.current.clone()?;
        let movie = self.pack.movies.get(&*current.key)?;
        let file = movie.first_file + current.index * movie.subframes + k.min(movie.subframes - 1);
        self.layer_for(&current.key, file, Some(Instant::now()))
    }

    /// The image the shown frame has at `vblank` (headless pictures: image
    /// `k` from the VBlanks since the frame was first seen).
    pub fn at_vblank(&mut self, vblank: u64) -> Option<MovieLayer> {
        let current = self.current.clone()?;
        let subframes = self.subframes();
        let elapsed = vblank.saturating_sub(current.first_vblank) as f64;
        let k = ((elapsed * f64::from(subframes) / self.frame_vblanks()) as u32).min(subframes - 1);
        let movie = self.pack.movies.get(&*current.key)?;
        let file = movie.first_file + current.index * movie.subframes + k;
        let wait = if self.wait {
            None
        } else {
            Some(Instant::now())
        };
        self.layer_for(&current.key, file, wait)
    }

    /// Forget the frames (after a quick load).
    pub fn reset(&mut self) {
        self.tracker.clear();
        self.current = None;
    }

    /// The pack's part of the run report.
    pub fn report(&self) -> serde_json::Value {
        serde_json::json!({
            "folder": self.pack.root.display().to_string(),
            "movies": self.pack.len(),
            "images": self.pack.images(),
            "tracker": self.tracker.stats,
            "replacer": self.stats,
            "hub_clip_frames": self.hub_seen,
            "decoding": self.loader.stats(),
        })
    }
}

/// How long a window run waits for an image at most: until its next pass
/// is due, capped here.
pub const MAX_WAIT: Duration = Duration::from_millis(50);

// ---- headless pictures -------------------------------------------------------------------

/// The picture `base` (`bw` x `bh` 0RGB words: the 1x picture or an N x one)
/// with `layer`'s image in the movie's rectangle, at `scale` times the 1x
/// picture. The image is fitted into the rectangle keeping its shape
/// (bilinear; an image of exactly the rectangle's size is copied), black
/// beside it; the mask's pixels keep the base picture's.
pub fn composite(
    base: &[u32],
    bw: usize,
    bh: usize,
    layer: &MovieLayer,
    scale: u32,
) -> (Vec<u32>, usize, usize) {
    let (pw, ph) = (layer.picture.0 as usize, layer.picture.1 as usize);
    let s = scale.max(1) as usize;
    let (w, h) = (pw * s, ph * s);
    let mut out = vec![0u32; w * h];
    for y in 0..h {
        let by = (y * bh / h).min(bh - 1);
        for x in 0..w {
            let bx = (x * bw / w).min(bw - 1);
            out[y * w + x] = base[by * bw + bx];
        }
    }
    let [rx, ry, rw, rh] = layer.rect.map(|v| v as usize);
    let (tx, ty, tw, th) = (rx * s, ry * s, rw * s, rh * s);
    let image = &layer.image;
    let (iw, ih) = (image.width as usize, image.height as usize);
    // Fit: the largest image size inside the rectangle with its shape.
    let (fw, fh) = if iw * th >= ih * tw {
        (tw, (ih * tw).div_ceil(iw).min(th))
    } else {
        ((iw * th).div_ceil(ih).min(tw), th)
    };
    let (ox, oy) = ((tw - fw) / 2, (th - fh) / 2);
    let texel = |x: usize, y: usize| -> [f32; 3] {
        let at = (y * iw + x) * 4;
        let p = &image.pixels[at..at + 3];
        [f32::from(p[0]), f32::from(p[1]), f32::from(p[2])]
    };
    for y in 0..th {
        for x in 0..tw {
            let mask_at = (y / s) * rw + x / s;
            if layer
                .mask
                .as_ref()
                .is_some_and(|m| m.get(mask_at).is_some_and(|&v| v != 0))
            {
                continue;
            }
            let rgb = if x < ox || y < oy || x >= ox + fw || y >= oy + fh {
                0
            } else if (fw, fh) == (iw, ih) {
                let at = ((y - oy) * iw + (x - ox)) * 4;
                let p = &image.pixels[at..at + 3];
                (u32::from(p[0]) << 16) | (u32::from(p[1]) << 8) | u32::from(p[2])
            } else {
                // Bilinear at the pixel centre.
                let u = ((x - ox) as f32 + 0.5) * iw as f32 / fw as f32 - 0.5;
                let v = ((y - oy) as f32 + 0.5) * ih as f32 / fh as f32 - 0.5;
                let (x0, y0) = (u.floor().max(0.0) as usize, v.floor().max(0.0) as usize);
                let (x1, y1) = ((x0 + 1).min(iw - 1), (y0 + 1).min(ih - 1));
                let (fx, fy) = (
                    (u - x0 as f32).clamp(0.0, 1.0),
                    (v - y0 as f32).clamp(0.0, 1.0),
                );
                let mix =
                    |a: [f32; 3], b: [f32; 3], t: f32| [0, 1, 2].map(|c| a[c] + (b[c] - a[c]) * t);
                let top = mix(texel(x0, y0), texel(x1, y0), fx);
                let bottom = mix(texel(x0, y1), texel(x1, y1), fx);
                let c = mix(top, bottom, fy).map(|v| v.round().clamp(0.0, 255.0) as u32);
                (c[0] << 16) | (c[1] << 8) | c[2]
            };
            out[(ty + y) * w + tx + x] = rgb;
        }
    }
    (out, w, h)
}

/// The scale a headless picture with `layer` is saved at: the base's, or
/// the image's over the rectangle when larger.
pub fn picture_scale(base_width: usize, layer: &MovieLayer) -> u32 {
    let base = (base_width as u32 / layer.picture.0.max(1)).max(1);
    let image = (layer.image.width + layer.rect[2] / 2) / layer.rect[2].max(1);
    base.max(image).clamp(1, 16)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(width: u32, height: u32, rgb: impl Fn(u32, u32) -> [u8; 3]) -> RgbaImage {
        let mut pixels = Vec::new();
        for y in 0..height {
            for x in 0..width {
                let [r, g, b] = rgb(x, y);
                pixels.extend_from_slice(&[r, g, b, 255]);
            }
        }
        RgbaImage {
            width,
            height,
            pixels,
        }
    }

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ac3-movie-pack-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn png(image: &RgbaImage) -> Vec<u8> {
        let rgb: Vec<u32> = image
            .pixels
            .chunks_exact(4)
            .map(|p| (u32::from(p[0]) << 16) | (u32::from(p[1]) << 8) | u32::from(p[2]))
            .collect();
        crate::movie_export::encode_png(&rgb, image.width, image.height).unwrap()
    }

    #[test]
    fn images_decode_from_every_format() {
        let original = image(5, 3, |x, y| [x as u8 * 40, y as u8 * 80, 7]);
        assert_eq!(decode_image(&png(&original), "png").unwrap(), original);
        // WebP lossless and a JPEG, both written by their encoders here.
        let mut webp = Vec::new();
        let rgb: Vec<u8> = original
            .pixels
            .chunks_exact(4)
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect();
        image_webp::WebPEncoder::new(&mut webp)
            .encode(&rgb, 5, 3, image_webp::ColorType::Rgb8)
            .unwrap();
        assert_eq!(decode_image(&webp, "webp").unwrap(), original);
        assert!(decode_image(b"not an image", "jpg").is_err());
        assert!(decode_image(&webp, "gif").is_err());
    }

    #[test]
    fn packs_scan_movies_numbers_and_configs() {
        let root = temp("scan");
        let one = root.join("movie_09e6");
        std::fs::create_dir_all(&one).unwrap();
        let picture = png(&image(2, 2, |_, _| [1, 2, 3]));
        for name in ["000000.png", "000001.png", "frame_000002.png", "notes.txt"] {
            std::fs::write(one.join(name), &picture).unwrap();
        }
        // Counting from 1, with four images a frame.
        let two = root.join("MOVIE_0800");
        std::fs::create_dir_all(&two).unwrap();
        for n in 1..=8 {
            std::fs::write(two.join(format!("{n:05}.png")), &picture).unwrap();
        }
        std::fs::write(two.join("movie.toml"), "subframes = 4\n").unwrap();
        std::fs::create_dir_all(root.join("unrelated")).unwrap();
        let pack = MoviePack::open(&root).unwrap();
        assert_eq!(pack.len(), 2);
        let first = &pack.movies["movie_09e6"];
        assert_eq!(first.files.keys().copied().collect::<Vec<_>>(), [0, 1, 2]);
        assert_eq!((first.subframes, first.first_file), (1, 0));
        let second = &pack.movies["movie_0800"];
        assert_eq!((second.subframes, second.first_file), (4, 1));
        assert!(pack.summary().contains("2 movies, 11 images"));
        std::fs::write(two.join("movie.toml"), "frames = 4\n").unwrap();
        assert!(
            !MoviePack::open(&root).unwrap().notes.is_empty(),
            "unknown key"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_loader_decodes_ahead_and_keeps_its_budget() {
        let root = temp("loader");
        let mut paths = Vec::new();
        for n in 0..6u8 {
            let path = root.join(format!("{n}.png"));
            std::fs::write(&path, png(&image(4, 4, |_, _| [n, n, n]))).unwrap();
            paths.push(path);
        }
        // Each image is 64 bytes: a budget of three.
        let loader = Loader::new(2, 3 * 64);
        let key: Arc<str> = "movie_0001".into();
        for (n, path) in paths.iter().enumerate() {
            loader.request((key.clone(), n as u32), &Source::Image(path.clone()));
        }
        for n in 0..6u32 {
            let image = loader.get(&(key.clone(), n), None).unwrap().unwrap();
            assert_eq!(image.pixels[0], n as u8);
        }
        loader.retain(&|(_, file)| (2..6).contains(file).then(|| file.abs_diff(2)));
        let stats = loader.stats();
        assert!(stats.cached_bytes <= 3 * 64, "{stats:?}");
        assert!(loader.get(&(key.clone(), 0), None).is_none(), "dropped");
        assert!(
            loader.get(&(key.clone(), 2), None).is_some(),
            "the nearest kept"
        );
        assert_eq!(stats.decoded, 6);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn hub_images_come_from_the_loaders_cache() {
        let root = temp("hub");
        let good = root.join("0.png");
        std::fs::write(&good, png(&image(4, 2, |x, _| [x as u8, 0, 0]))).unwrap();
        let bad = root.join("1.png");
        std::fs::write(&bad, b"not a png").unwrap();
        let loader = Loader::new(1, 1 << 20);
        let key: Arc<str> = "movie_0810".into();
        let images = |wait| HubImages {
            shared: loader.shared.clone(),
            wait,
            failed: RefCell::default(),
        };
        // Never queued: none (the game's texels).
        assert!(images(true).image(&key, 0).is_none());
        loader.request((key.clone(), 0), &Source::Image(good));
        loader.request((key.clone(), 1), &Source::Image(bad));
        // Headless runs wait for each image; a broken one is none, noted.
        let waiting = images(true);
        assert_eq!(waiting.image(&key, 0).unwrap().width, 4);
        assert!(waiting.image(&key, 1).is_none());
        assert!(waiting.failed.borrow().contains(&key));
        // A window run never waits, but takes what is decoded.
        assert_eq!(images(false).image(&key, 0).unwrap().height, 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn composites_place_the_image_under_the_mask() {
        // A 4x3 picture, the movie at (0, 1) 4x1.
        let base: Vec<u32> = (0..12).collect();
        let layer = |image: RgbaImage, mask: Option<Vec<u8>>| MovieLayer {
            image: Arc::new(image),
            generation: 1,
            picture: (4, 3),
            rect: [0, 1, 4, 1],
            mask: mask.map(Arc::new),
            mask_generation: 1,
        };
        // Its own pixels at 1x: the same picture.
        let same = image(4, 1, |x, _| [0, 0, 4 + x as u8]);
        assert_eq!(composite(&base, 4, 3, &layer(same, None), 1).0, base);
        // An 8x2 image at 2x, pixel 1 masked.
        let big = layer(image(8, 2, |_, _| [9, 9, 9]), Some(vec![0, 255, 0, 0]));
        assert_eq!(picture_scale(4, &big), 2);
        let (out, w, h) = composite(&base, 4, 3, &big, 2);
        assert_eq!((w, h), (8, 6));
        assert_eq!(out[2 * 8], 0x0009_0909);
        assert_eq!(out[2 * 8 + 2], 5, "the mask keeps the game's pixel");
        assert_eq!(out[0], 0, "outside: the base picture");
        // A wider image is letterboxed inside the rectangle (black).
        let wide = layer(image(16, 2, |_, _| [9, 9, 9]), None);
        let (out, ..) = composite(&base, 4, 3, &wide, 4);
        assert_eq!(out[4 * 16], 0, "the bar above the image");
        assert_eq!(out[6 * 16], 0x0009_0909);
    }
}
