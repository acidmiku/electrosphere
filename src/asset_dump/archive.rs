//! Static dump: every image in the disc's resources, as PNG plus manifest.
//!
//! The walk follows `tools/unpack_archives.py`, so node names match the
//! files in `extracted/archives/`: BPH entry `bpb_NNNN`, DOT1 child
//! `_NNNN` (the same strict table test), Ulz stream `_dec`. A node is a TIM
//! when it starts with the TIM id and flags 0..3/8/9 (read as the game reads
//! it, `tim::parse_loaded`); any other leaf is scanned for strictly valid
//! embedded TIMs (`tim::scan`), as are the resident program and the disc's
//! two executables. Overlay 3's credits images, raw CLUT + pixel blocks
//! with their rectangles in overlay data (800D653C), are the one non-TIM
//! format the game uploads from these files.
use super::key::{self, Key, Mode, PaletteKey};
use super::png_io::{self, Pixels};
use crate::{Result, assets::GameData, sha256, version::Version};
use ac3_formats::{dot1, ulz};
use ac3_graphics::tim::{self, Depth};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

/// Recursion limit of the container walk (as the Python inventory).
const MAX_DEPTH: usize = 12;

/// One step from a top-level file to a node.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    Bph(usize),
    Dot1(usize),
    Ulz,
}

impl Step {
    fn text(&self) -> String {
        match self {
            Self::Bph(index) => format!("bph:{index}"),
            Self::Dot1(index) => format!("dot1:{index}"),
            Self::Ulz => "ulz".to_owned(),
        }
    }
}

/// Where one image was found.
#[derive(Debug, Clone)]
struct Source {
    /// Top-level file: `ACE.BPB`, `ACE.BIN` (resident payload), `ACE.EXE`...
    file: &'static str,
    /// Node id (`bpb_0072_0003_0007`), as in `extracted/archives/`.
    node: String,
    chain: Vec<Step>,
    /// Image offset inside the node.
    offset: usize,
    /// Absolute offset in the top-level file when no decompression is on
    /// the way (for `ACE.BIN`: in the decompressed resident payload).
    absolute: Option<usize>,
    /// `container` (the node is the image), `scan` or `credits-table`.
    found_by: &'static str,
}

impl Source {
    fn group(&self) -> String {
        if self.file == "ACE.BPB" {
            self.node.chars().take(8).collect()
        } else if matches!(self.file, "ACE.BIN" | "ACE3.BIN") {
            "resident".to_owned()
        } else {
            self.file.to_owned()
        }
    }
    fn json(&self) -> Value {
        let entry = self.chain.iter().find_map(|step| match step {
            Step::Bph(index) => Some(*index),
            _ => None,
        });
        let extracted =
            (self.file == "ACE.BPB").then(|| format!("extracted/archives/{}.bin", self.node));
        json!({
            "file": self.file,
            "node": self.node,
            "entry": entry,
            "chain": self.chain.iter().map(Step::text).collect::<Vec<_>>(),
            "offset_in_node": self.offset,
            "file_offset": self.absolute,
            "extracted": extracted,
            "found_by": self.found_by,
        })
    }
}

/// One rectangle of an image: VRAM position and its little-endian bytes.
#[derive(Debug, Clone)]
struct Rect {
    x: u16,
    y: u16,
    width: u16,
    height: u16,
    data: Vec<u8>,
}

impl Rect {
    fn from_block(block: &tim::Block<'_>) -> Self {
        Self {
            x: block.x,
            y: block.y,
            width: block.width,
            height: block.height,
            data: block.data.to_vec(),
        }
    }
    fn json(&self) -> Value {
        json!({"x": self.x, "y": self.y, "w": self.width, "h": self.height})
    }
    fn word(&self, index: usize) -> u16 {
        u16::from_le_bytes([self.data[index * 2], self.data[index * 2 + 1]])
    }
}

/// A found image before deduplication.
struct Found {
    depth: Depth,
    tim_flags: Option<u32>,
    clut: Option<Rect>,
    pixels: Rect,
    /// The bytes the game reads (the TIM, or CLUT then pixels).
    original: Vec<u8>,
    lengths_match: bool,
}

impl Found {
    fn from_tim(image: &tim::TimImage<'_>, bytes: &[u8], lengths_match: bool) -> Self {
        Self {
            depth: image.depth,
            tim_flags: Some(u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]])),
            clut: image.clut.as_ref().map(Rect::from_block),
            pixels: Rect::from_block(&image.pixels),
            original: bytes[..image.bytes_consumed].to_vec(),
            lengths_match,
        }
    }
}

/// A unique image (by the SHA-256 of its original bytes) and where it occurs.
struct Image {
    found: Found,
    sha256: String,
    sources: Vec<Source>,
}

struct Walker {
    images: Vec<Image>,
    by_hash: HashMap<String, usize>,
    nodes: usize,
    ulz_streams: usize,
    /// Streams only the size-bounded decoder reads (the Japanese patch's).
    ulz_bounded: usize,
    errors: Vec<Value>,
}

/// The strict DOT1 test of `tools/unpack_archives.py` (`dot1_entries`):
/// a full-word count of 1..4096, the first child at the table end or its
/// 16/2048-byte alignment with zero padding, aligned, strictly increasing
/// offsets inside the data.
fn dot1_children(data: &[u8]) -> Option<dot1::Container<'_>> {
    let word = |offset: usize| -> Option<usize> {
        data.get(offset..offset + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
    };
    if data.len() < 8 {
        return None;
    }
    let count = word(0)?;
    if !(1..=4096).contains(&count) || 4 + 4 * count > data.len() {
        return None;
    }
    let table_end = 4 + 4 * count;
    let offsets: Vec<usize> = (0..count).map(|i| word(4 + 4 * i)).collect::<Option<_>>()?;
    let first = offsets[0];
    let allowed = [
        table_end,
        (table_end + 15) & !15,
        (table_end + 2047) & !2047,
    ];
    if !allowed.contains(&first) || first >= data.len() {
        return None;
    }
    if data[table_end..first].iter().any(|&b| b != 0) {
        return None;
    }
    if offsets
        .iter()
        .any(|&a| !a.is_multiple_of(4) || a >= data.len())
        || offsets.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return None;
    }
    dot1::parse(data).ok()
}

impl Walker {
    fn new() -> Self {
        Self {
            images: Vec::new(),
            by_hash: HashMap::new(),
            nodes: 0,
            ulz_streams: 0,
            ulz_bounded: 0,
            errors: Vec::new(),
        }
    }

    fn add(&mut self, found: Found, source: Source) {
        let hash = sha256(&found.original);
        match self.by_hash.get(&hash) {
            Some(&index) => self.images[index].sources.push(source),
            None => {
                self.by_hash.insert(hash.clone(), self.images.len());
                self.images.push(Image {
                    found,
                    sha256: hash,
                    sources: vec![source],
                });
            }
        }
    }

    fn error(&mut self, node: &str, error: impl std::fmt::Display) {
        self.errors
            .push(json!({"node": node, "error": error.to_string()}));
    }

    fn visit(
        &mut self,
        file: &'static str,
        data: &[u8],
        node: String,
        chain: Vec<Step>,
        absolute: Option<usize>,
        depth: usize,
    ) {
        self.nodes += 1;
        if depth > MAX_DEPTH {
            self.error(&node, "recursion limit");
            return;
        }
        if data.starts_with(b"Ulz\x1a") {
            match ulz::decompress_bounded(data) {
                Ok(ulz::BoundedDecoded {
                    decoded, bounded, ..
                }) => {
                    self.ulz_streams += 1;
                    self.ulz_bounded += usize::from(bounded);
                    let mut chain = chain;
                    chain.push(Step::Ulz);
                    self.visit(
                        file,
                        &decoded.payload,
                        format!("{node}_dec"),
                        chain,
                        None,
                        depth + 1,
                    );
                }
                Err(error) => self.error(&node, error),
            }
            return;
        }
        if let Some(table) = dot1_children(data) {
            for index in 0..table.len() {
                let Ok(range) = table.range(index) else {
                    continue;
                };
                let mut child_chain = chain.clone();
                child_chain.push(Step::Dot1(index));
                self.visit(
                    file,
                    &data[range.clone()],
                    format!("{node}_{index:04}"),
                    child_chain,
                    absolute.map(|a| a + range.start),
                    depth + 1,
                );
            }
            return;
        }
        let head = |offset: usize| {
            data.get(offset..offset + 4)
                .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        };
        if head(0) == Some(0x10) && matches!(head(4), Some(0..=3 | 8 | 9)) {
            match tim::parse_loaded(data) {
                Ok(loaded) => {
                    let found = Found::from_tim(&loaded.image, data, loaded.lengths_match);
                    self.add(
                        found,
                        Source {
                            file,
                            node,
                            chain,
                            offset: 0,
                            absolute,
                            found_by: "container",
                        },
                    );
                }
                Err(error) => self.error(&node, error),
            }
            return;
        }
        self.scan(file, data, &node, &chain, absolute);
    }

    fn scan(
        &mut self,
        file: &'static str,
        data: &[u8],
        node: &str,
        chain: &[Step],
        absolute: Option<usize>,
    ) {
        for (offset, image) in tim::scan(data) {
            let found = Found::from_tim(&image, &data[offset..], true);
            self.add(
                found,
                Source {
                    file,
                    node: node.to_owned(),
                    chain: chain.to_vec(),
                    offset,
                    absolute: absolute.map(|a| a + offset),
                    found_by: "scan",
                },
            );
        }
    }

    /// Overlay 3's credits images (800D653C): each a 16-colour CLUT and
    /// its 4-bit pixels, CLUT first, with rectangle fields (w, h, x, y, CLUT
    /// x, CLUT y; words) in a 0x18-byte overlay record.
    fn credits(&mut self, overlay: &[u8]) {
        const BASE: u32 = 0x800c_9dc0;
        // (pixels, record) as in ov3::upload_images.
        const IMAGES: [(u32, u32); 5] = [
            (0x800c_dc94, 0x800d_9b14),
            (0x800c_9de4, 0x800d_9aac),
            (0x800c_c114, 0x800d_9ac4),
            (0x800c_cb54, 0x800d_9adc),
            (0x800c_d474, 0x800d_9af4),
        ];
        let half = |address: u32| -> Option<u16> {
            let at = (address - BASE) as usize;
            overlay
                .get(at..at + 2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
        };
        for (pixels, record) in IMAGES {
            let fields: Option<Vec<u16>> = (0..6).map(|i| half(record + i * 4)).collect();
            let Some(fields) = fields else {
                self.error("bpb_0003", "credits record outside overlay 3");
                continue;
            };
            let [w, h, x, y, clut_x, clut_y] = fields[..] else {
                continue;
            };
            let start = (pixels - 0x20 - BASE) as usize;
            let end = start + 0x20 + usize::from(w) * usize::from(h) * 2;
            let Some(bytes) = overlay.get(start..end) else {
                self.error("bpb_0003", "credits image outside overlay 3");
                continue;
            };
            if usize::from(x) + usize::from(w) > 1024 || usize::from(y) + usize::from(h) > 512 {
                self.error("bpb_0003", "credits image outside VRAM");
                continue;
            }
            let found = Found {
                depth: Depth::Indexed4,
                tim_flags: None,
                clut: Some(Rect {
                    x: clut_x,
                    y: clut_y,
                    width: 16,
                    height: 1,
                    data: bytes[..0x20].to_vec(),
                }),
                pixels: Rect {
                    x,
                    y,
                    width: w,
                    height: h,
                    data: bytes[0x20..].to_vec(),
                },
                original: bytes.to_vec(),
                lengths_match: true,
            };
            self.add(
                found,
                Source {
                    file: "ACE.BPB",
                    node: "bpb_0003".to_owned(),
                    chain: vec![Step::Bph(3)],
                    offset: start,
                    absolute: None,
                    found_by: "credits-table",
                },
            );
        }
    }
}

/// One PNG to write for an image: a palette choice (or none).
struct Output {
    key: Key,
    palette_index: Option<usize>,
    clut_position: Option<(u16, u16)>,
    index_range: Option<(u8, u8)>,
    png: Vec<u8>,
}

/// Every PNG of one image: one per palette of its CLUT block (16 or 256
/// entries each, row by row, left to right), or one for direct colour.
fn outputs(found: &Found) -> Result<Vec<Output>> {
    let mode = Mode::from_depth(found.depth);
    let pixels = &found.pixels;
    let width = found.depth.texel_width(pixels.width);
    let height = usize::from(pixels.height);
    let texture = key::texture_hash_bytes(pixels.width, pixels.height, &pixels.data);
    let block = tim::Block {
        x: pixels.x,
        y: pixels.y,
        width: pixels.width,
        height: pixels.height,
        data: &pixels.data,
    };
    let make_key = |palette| Key {
        mode,
        texture: texture.clone(),
        palette,
        width,
        height,
    };
    match found.depth {
        Depth::Direct15 => {
            let colors: Vec<u16> = (0..width * height).map(|i| pixels.word(i)).collect();
            Ok(vec![Output {
                key: make_key(PaletteKey::None),
                palette_index: None,
                clut_position: None,
                index_range: None,
                png: png_io::encode(width, height, Pixels::Color15(&colors))?,
            }])
        }
        Depth::Direct24 => {
            let stride = usize::from(pixels.width) * 2;
            let bytes: Vec<u8> = pixels
                .data
                .chunks(stride)
                .flat_map(|row| row[..width * 3].to_vec())
                .collect();
            Ok(vec![Output {
                key: make_key(PaletteKey::None),
                palette_index: None,
                clut_position: None,
                index_range: None,
                png: png_io::encode(width, height, Pixels::Rgb24(&bytes))?,
            }])
        }
        Depth::Indexed4 | Depth::Indexed8 => {
            let indices = tim::indices(&block, found.depth).ok_or("indexed image")?;
            let bits = found.depth.bits();
            let entries = found.depth.palette_entries();
            let range = key::index_range(&pixels.data, mode);
            let Some(clut) = &found.clut else {
                // No CLUT of its own: a grey ramp, keyed `nopal`.
                let ramp: Vec<u16> = (0..entries)
                    .map(|i| {
                        let level = (i * 31 / (entries - 1)) as u16;
                        (level | (level << 5) | (level << 10)).max(1)
                    })
                    .collect();
                return Ok(vec![Output {
                    key: make_key(PaletteKey::Missing),
                    palette_index: None,
                    clut_position: None,
                    index_range: range,
                    png: png_io::encode(
                        width,
                        height,
                        Pixels::Indexed {
                            bits,
                            indices: &indices,
                            palette: &ramp,
                        },
                    )?,
                }]);
            };
            let per_row = (usize::from(clut.width) / entries).max(1);
            let mut list = Vec::new();
            for row in 0..usize::from(clut.height) {
                for column in 0..per_row {
                    let first = row * usize::from(clut.width) + column * entries;
                    let available = (usize::from(clut.width) - column * entries).min(entries);
                    let palette: Vec<u16> = (0..available).map(|i| clut.word(first + i)).collect();
                    let (lo, hi) = range.unwrap_or((0, 0));
                    list.push(Output {
                        key: make_key(PaletteKey::Hash(key::palette_hash(lo, hi, &palette))),
                        palette_index: Some(row * per_row + column),
                        clut_position: Some((
                            clut.x + (column * entries) as u16,
                            clut.y + row as u16,
                        )),
                        index_range: range,
                        png: png_io::encode(
                            width,
                            height,
                            Pixels::Indexed {
                                bits,
                                indices: &indices,
                                palette: &palette,
                            },
                        )?,
                    });
                }
            }
            Ok(list)
        }
    }
}

/// Read an optional disc file next to the verified ones.
fn disc_file(directory: &Path, name: &str) -> Option<Vec<u8>> {
    fs::read(directory.join(name)).ok()
}

/// Walk every resource and write each image as PNG under `out/static/`,
/// with `out/manifest.json`. Returns the manifest's summary.
///
/// Sources: every BPH entry of ACE.BPB (DOT1 tables and Ulz streams
/// recursively), the resident program (ACE.BIN, decompressed), overlay 3's
/// credits table, and ACE.EXE / SCES_020.66 from the disc directory when
/// present. Identical images (same original bytes) are written once and
/// list every source; PNGs are named `<source>-<key>.png` (see
/// `docs/ASSET_DUMP.md`) in the folder of their first source.
pub fn dump_archive_images(data: &GameData, out: &Path) -> Result<Value> {
    let mut walker = Walker::new();
    for entry in &data.directory.entries {
        let node = format!("bpb_{:04}", entry.index);
        let Some(extent) = entry.extent else {
            walker.error(&node, "special directory entry without an extent");
            continue;
        };
        match data.resource(entry.index) {
            Ok(bytes) => walker.visit(
                "ACE.BPB",
                bytes,
                node,
                vec![Step::Bph(entry.index)],
                Some(extent.offset),
                0,
            ),
            Err(error) => walker.error(&node, error),
        }
    }
    // Overlay 3's credits table is PAL's (the Japanese overlay 3 differs).
    let (resident, executables): (&'static str, &[&'static str]) = match data.version {
        Version::Pal => {
            walker.credits(data.overlay(3)?);
            ("ACE.BIN", &["ACE.EXE", "SCES_020.66"])
        }
        Version::Jp { .. } => ("ACE3.BIN", &["SLPS_020.20", "SLPS_020.21"]),
    };
    walker.scan(resident, &data.executable.payload, "resident", &[], Some(0));
    for &name in executables {
        let file: &'static str = name;
        if let Some(bytes) = disc_file(data.disc_directory(), name) {
            walker.scan(file, &bytes, name, &[], Some(0));
        }
    }

    let root = out.join("static");
    fs::create_dir_all(&root)?;
    let mut written: HashMap<String, (String, String)> = HashMap::new();
    let mut images_json = Vec::with_capacity(walker.images.len());
    let mut by_format: std::collections::BTreeMap<&str, [usize; 2]> = Default::default();
    let mut by_found: std::collections::BTreeMap<&str, usize> = Default::default();
    let mut png_bytes = 0usize;
    let mut occurrences = 0usize;
    let mut mismatched_lengths = 0usize;
    let mut unwritable = Vec::new();
    for image in &walker.images {
        let found = &image.found;
        let mode = Mode::from_depth(found.depth);
        let counts = by_format.entry(mode.name()).or_default();
        counts[0] += 1;
        counts[1] += image.sources.len();
        occurrences += image.sources.len();
        for source in &image.sources {
            *by_found.entry(source.found_by).or_default() += 1;
        }
        mismatched_lengths += usize::from(!found.lengths_match);
        let first = &image.sources[0];
        let group = first.group();
        let mut prefix = first
            .node
            .strip_prefix(&group)
            .unwrap_or(&first.node)
            .trim_start_matches('_')
            .to_owned();
        if first.found_by != "container" {
            if !prefix.is_empty() {
                prefix.push('_');
            }
            prefix.push_str(&format!("at{:06x}", first.offset));
        }
        let mut pngs = Vec::new();
        let list = match outputs(found) {
            Ok(list) => list,
            Err(error) => {
                // An image no PNG can hold (a zero-sized rectangle): listed
                // as an error, the dump goes on.
                unwritable.push(json!({
                    "node": first.node,
                    "error": format!("{error} ({}x{} at {}, {})", found.pixels.width, found.pixels.height, found.pixels.x, found.pixels.y),
                }));
                continue;
            }
        };
        for output in list {
            let stem = output.key.to_string();
            let (file, png_sha) = match written.get(&stem) {
                Some(done) => done.clone(),
                None => {
                    let name = if prefix.is_empty() {
                        format!("{stem}.png")
                    } else {
                        format!("{prefix}-{stem}.png")
                    };
                    let relative = format!("static/{group}/{name}");
                    let path: PathBuf = root.join(&group).join(&name);
                    fs::create_dir_all(path.parent().ok_or("png path")?)?;
                    fs::write(&path, &output.png)?;
                    png_bytes += output.png.len();
                    let done = (relative, sha256(&output.png));
                    written.insert(stem.clone(), done.clone());
                    done
                }
            };
            pngs.push(json!({
                "palette": output.palette_index,
                "clut_x": output.clut_position.map(|p| p.0),
                "clut_y": output.clut_position.map(|p| p.1),
                "index_range": output.index_range.map(|(lo, hi)| [lo, hi]),
                "key": stem,
                "file": file,
                "sha256": png_sha,
            }));
        }
        images_json.push(json!({
            "sha256": image.sha256,
            "bytes": found.original.len(),
            "format": mode.name(),
            "tim_flags": found.tim_flags,
            "width": found.depth.texel_width(found.pixels.width),
            "height": found.pixels.height,
            "vram": found.pixels.json(),
            "clut": found.clut.as_ref().map(Rect::json),
            "texture_hash": key::texture_hash_bytes(
                found.pixels.width,
                found.pixels.height,
                &found.pixels.data,
            ),
            "stated_lengths_match": found.lengths_match,
            "occurrences": image.sources.len(),
            "sources": image.sources.iter().map(Source::json).collect::<Vec<_>>(),
            "pngs": pngs,
        }));
    }
    let formats: serde_json::Map<String, Value> = by_format
        .iter()
        .map(|(name, [unique, total])| {
            (
                (*name).to_owned(),
                json!({"unique": unique, "occurrences": total}),
            )
        })
        .collect();
    let summary = json!({
        "unique_images": walker.images.len(),
        "occurrences": occurrences,
        "pngs": written.len(),
        "png_bytes": png_bytes,
        "by_format": formats,
        "occurrences_by_finder": by_found,
        "stated_length_mismatches": mismatched_lengths,
        "nodes_visited": walker.nodes,
        "ulz_streams": walker.ulz_streams,
        "ulz_bounded_streams": walker.ulz_bounded,
        "errors": walker.errors.len() + unwritable.len(),
    });
    let manifest = json!({
        "schema": "ac3-asset-dump/static/1",
        "alpha": super::ALPHA_NOTE,
        "key": super::KEY_NOTE,
        "summary": summary,
        "errors": walker.errors.iter().chain(&unwritable).collect::<Vec<_>>(),
        "images": images_json,
    });
    fs::write(
        out.join("manifest.json"),
        serde_json::to_string_pretty(&manifest)? + "\n",
    )?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dot1_test_matches_the_python_inventory_rules() {
        let table = |words: &[u32], tail: usize| -> Vec<u8> {
            let mut bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
            bytes.resize(bytes.len() + tail, 0xaa);
            bytes
        };
        // Two children right after the table.
        let data = table(&[2, 12, 16], 8);
        let container = dot1_children(&data).unwrap();
        assert_eq!(container.range(0).unwrap(), 12..16);
        assert_eq!(container.range(1).unwrap(), 16..20);
        // First child at the 16-byte alignment with zero padding.
        let mut padded = table(&[1, 16, 0, 0], 4);
        assert!(dot1_children(&padded).is_some());
        padded[8] = 1;
        assert!(dot1_children(&padded).is_none(), "nonzero padding");
        // A TIM header never qualifies: count 16, first "offset" = flags.
        assert!(dot1_children(&table(&[0x10, 8, 44, 0], 64)).is_none());
        assert!(
            dot1_children(&table(&[2, 16, 12], 8)).is_none(),
            "decreasing"
        );
        assert!(dot1_children(&table(&[0x10000, 8], 8)).is_none(), "count");
    }

    #[test]
    fn multi_palette_tims_give_one_png_per_palette() {
        // 4-bit 2x1 words (8x1 texels), CLUT 32x2: four 16-colour palettes.
        let clut: Vec<u8> = (0..64u16)
            .flat_map(|i| ((i << 5) | 1).to_le_bytes())
            .collect();
        let found = Found {
            depth: Depth::Indexed4,
            tim_flags: Some(8),
            clut: Some(Rect {
                x: 0,
                y: 480,
                width: 32,
                height: 2,
                data: clut,
            }),
            pixels: Rect {
                x: 640,
                y: 0,
                width: 2,
                height: 1,
                data: vec![0x10, 0x32, 0x54, 0x76],
            },
            original: Vec::new(),
            lengths_match: true,
        };
        let list = outputs(&found).unwrap();
        assert_eq!(list.len(), 4);
        assert_eq!(list[1].clut_position, Some((16, 480)));
        assert_eq!(list[2].clut_position, Some((0, 481)));
        assert_eq!(list[3].palette_index, Some(3));
        assert_eq!(list[0].index_range, Some((0, 7)));
        assert_eq!(list[0].key.width, 8);
        let keys: std::collections::HashSet<_> = list.iter().map(|o| o.key.clone()).collect();
        assert_eq!(keys.len(), 4, "distinct palettes, distinct keys");
        let png_io::Decoded::Indexed {
            indices, palette, ..
        } = png_io::decode(&list[3].png).unwrap()
        else {
            panic!("indexed");
        };
        assert_eq!(indices, [0, 1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(palette[0], (48 << 5) | 1);
    }
}
