//! Resident asset preparation at 80035730 (mission) and 80035C90 (aircraft).
//!
//! Addresses stay section-relative. Aliased record slots and high-bit encoded
//! fields are preserved; no guest pointers, allocator, gameplay state or CPU are
//! synthesized. The validated inputs are PAL BPB7 and BPB48. Other resources with
//! the same checked layout may parse, but their gameplay meaning is not inferred.

use ac3_audio::Bank;
use ac3_formats::{dot1, ulz};
use ac3_graphics::{VRAM_HEIGHT, VRAM_WIDTH, Vram, scalar::ScalarMath, tim};
use std::{collections::BTreeSet, fmt, ops::Range};

/// A resource and the aggregate of its decompressed children must fit PSX RAM.
pub const MAX_ASSET_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug)]
pub enum Error {
    Invalid(&'static str),
    Format(ac3_formats::Error),
    Graphics(ac3_graphics::Error),
    Audio(ac3_audio::Error),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(s) => write!(f, "invalid resident flight assets: {s}"),
            Self::Format(e) => e.fmt(f),
            Self::Graphics(e) => e.fmt(f),
            Self::Audio(e) => e.fmt(f),
        }
    }
}
impl std::error::Error for Error {}
impl From<ac3_formats::Error> for Error {
    fn from(value: ac3_formats::Error) -> Self {
        Self::Format(value)
    }
}
impl From<ac3_graphics::Error> for Error {
    fn from(value: ac3_graphics::Error) -> Self {
        Self::Graphics(value)
    }
}
impl From<ac3_audio::Error> for Error {
    fn from(value: ac3_audio::Error) -> Self {
        Self::Audio(value)
    }
}
pub type Result<T> = std::result::Result<T, Error>;

/// Names identify recovered loader uses, not speculative model/terrain formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionRole {
    MissionPaletteParameters,
    MissionContext1c,
    MissionRecords4Context20,
    MissionRecords6Context24,
    MissionRecords6Context28,
    MissionRecords4Context2c,
    MissionContext30,
    MissionContext34,
    MissionContext38,
    MissionDirectoryContext3c,
    MissionDirectoryContext40,
    AircraftRecords6Context08,
    AircraftContext0c,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordPointer {
    /// Offset within this decoded section. Zero-valued relative fields point to
    /// the record itself; the original relocation routine does not treat them as null.
    SectionOffset(usize),
    /// 80026EE0 leaves high-bit words untouched. BPB48 uses FFFFFFFF sentinels.
    /// This variant must not be dereferenced as a host address.
    Encoded(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relocation {
    pub field_offset: usize,
    pub target: RecordPointer,
}

#[derive(Debug, Clone)]
pub struct RecordDirectory {
    /// One offset per logical slot; duplicates intentionally alias one record.
    pub slots: Vec<usize>,
    /// Each distinct record's relocation fields occur once, including aliased records.
    pub relocations: Vec<Relocation>,
    pub fields_per_record: usize,
}

impl RecordDirectory {
    fn parse(bytes: &[u8], fields: usize) -> Result<Self> {
        let header = word(bytes, 0)?;
        if fields != 0 && header & 0xffff0000 != 0 {
            return Err(Error::Invalid(
                "record loop count exceeds relocated table count",
            ));
        }
        let count = (header & 0xffff) as usize;
        let table_end = 4 + count * 4;
        if table_end > bytes.len() {
            return Err(Error::Invalid("truncated record table"));
        }
        let mut slots = Vec::with_capacity(count);
        let mut previous = table_end;
        for index in 0..count {
            let offset = word(bytes, 4 + index * 4)? as usize;
            if offset < previous || offset >= bytes.len() || offset & 3 != 0 {
                return Err(Error::Invalid(
                    "record offsets must be aligned, bounded and nondecreasing",
                ));
            }
            slots.push(offset);
            previous = offset;
        }
        let unique: Vec<usize> = slots
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut relocations = Vec::new();
        for (index, &start) in unique.iter().enumerate() {
            let end = unique.get(index + 1).copied().unwrap_or(bytes.len());
            if fields != 0 && start + 4 + fields * 4 > end {
                return Err(Error::Invalid("truncated record relocation fields"));
            }
            for field in 0..fields {
                let field_offset = start + 4 + field * 4;
                let value = word(bytes, field_offset)?;
                let target = if value & 0x8000_0000 != 0 {
                    RecordPointer::Encoded(value)
                } else {
                    let target = start + value as usize;
                    if target >= end {
                        return Err(Error::Invalid("relative pointer leaves its record"));
                    }
                    RecordPointer::SectionOffset(target)
                };
                relocations.push(Relocation {
                    field_offset,
                    target,
                });
            }
        }
        Ok(Self {
            slots,
            relocations,
            fields_per_record: fields,
        })
    }
}

#[derive(Debug, Clone)]
pub struct ResidentSection {
    pub index: usize,
    pub role: SectionRole,
    /// Offset from the original allocation start, including its child table.
    pub allocation_offset: usize,
    bytes: Vec<u8>,
    pub directory: Option<RecordDirectory>,
}
impl ResidentSection {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordRef {
    pub section: usize,
    pub offset: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadSource {
    TimPixels { section: usize, image: usize },
    TimClut { section: usize, image: usize },
    DerivedPalette { sine_index: u8 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upload {
    pub source: UploadSource,
    /// VRAM word coordinates and dimensions (not texture pixel dimensions).
    pub rectangle: [u16; 4],
    pub bytes: Vec<u8>,
}
impl Upload {
    pub fn apply(&self, vram: &mut Vram) -> Result<()> {
        let [x, y, width, height] = self.rectangle;
        vram.load_block(&tim::Block {
            x,
            y,
            width,
            height,
            data: &self.bytes,
        })?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct TextureGroup {
    pub section: usize,
    pub image_count: usize,
    /// Decoded DOT1/TIM bytes are retained for later resource interpretation.
    pub bytes: Vec<u8>,
    /// Pixel and then CLUT upload for each TIM, in original execution order.
    pub uploads: Vec<Upload>,
}

#[derive(Debug)]
pub struct FlightGraphics {
    /// Native input VRAM after all ordered uploads; no display drawing is performed.
    pub vram: Vram,
    pub uploads: Vec<Upload>,
}

#[derive(Debug)]
pub struct FlightMissionAssets {
    sections: Vec<ResidentSection>,
    textures: Vec<TextureGroup>,
    sound_bank: Vec<u8>,
    object_slots: Vec<RecordRef>,
    resident_bytes: usize,
}

impl FlightMissionAssets {
    pub fn parse(resource: &[u8]) -> Result<Self> {
        let table = top_level(resource, 17)?;
        let mut budget = 72;
        let mut sections = Vec::with_capacity(11);
        let mut allocation_offset = 72;
        let roles = [
            SectionRole::MissionPaletteParameters,
            SectionRole::MissionContext1c,
            SectionRole::MissionRecords4Context20,
            SectionRole::MissionRecords6Context24,
            SectionRole::MissionRecords6Context28,
            SectionRole::MissionRecords4Context2c,
            SectionRole::MissionContext30,
            SectionRole::MissionContext34,
            SectionRole::MissionContext38,
            SectionRole::MissionDirectoryContext3c,
            SectionRole::MissionDirectoryContext40,
        ];
        for (index, role) in roles.into_iter().enumerate() {
            let bytes = decode(table.child(index)?, &mut budget)?;
            let fields = match index {
                2 | 5 => Some(4),
                3 | 4 => Some(6),
                9 | 10 => Some(0),
                _ => None,
            };
            let directory = fields
                .map(|count| RecordDirectory::parse(&bytes, count))
                .transpose()?;
            let len = bytes.len();
            sections.push(ResidentSection {
                index,
                role,
                allocation_offset,
                bytes,
                directory,
            });
            allocation_offset += len;
        }
        validate_palette_records(&sections[0].bytes)?;
        let mut object_slots: Vec<_> = sections[3]
            .directory
            .as_ref()
            .ok_or(Error::Invalid("missing object directory"))?
            .slots
            .iter()
            .map(|&offset| RecordRef { section: 3, offset })
            .collect();
        let extra = &sections[4]
            .directory
            .as_ref()
            .ok_or(Error::Invalid("missing extra object directory"))?
            .slots;
        let destination = object_slots
            .get_mut(96..96 + extra.len())
            .ok_or(Error::Invalid("extra records exceed object slot table"))?;
        for (slot, &offset) in destination.iter_mut().zip(extra) {
            *slot = RecordRef { section: 4, offset };
        }
        let mut textures = Vec::with_capacity(5);
        // 80035730 expands texture children in reverse order into temporary space.
        for index in (11..=15).rev() {
            textures.push(texture_group(index, table.child(index)?, &mut budget)?);
        }
        let sound_bank = checked_bank(table.child(16)?)?;
        Ok(Self {
            sections,
            textures,
            sound_bank,
            object_slots,
            resident_bytes: allocation_offset,
        })
    }

    pub fn sections(&self) -> &[ResidentSection] {
        &self.sections
    }
    pub fn texture_groups(&self) -> &[TextureGroup] {
        &self.textures
    }
    pub fn sound_bank(&self) -> Result<Bank<'_>> {
        Ok(Bank::parse(&self.sound_bank)?)
    }
    pub fn sound_bank_bytes(&self) -> &[u8] {
        &self.sound_bank
    }
    pub fn object_slots(&self) -> &[RecordRef] {
        &self.object_slots
    }
    pub fn resident_bytes(&self) -> usize {
        self.resident_bytes
    }

    /// Native 80026D8C + 8005D490. Start from the caller's live native VRAM:
    /// the palette source can include cells not overwritten by this resource.
    /// On success this also applies the section0 palette-reference patches;
    /// failure leaves both the asset and the caller's VRAM unchanged.
    pub fn prepare_graphics(&mut self, resident: &[u8], initial: &Vram) -> Result<FlightGraphics> {
        let math = ScalarMath::from_resident(resident, 0x80010000)?;
        let rectangle_at = 0x800bac38 - 0x80010000;
        let source_rect = rectangle(resident, rectangle_at)?;
        if source_rect[2..] != [16, 64] {
            return Err(Error::Invalid("palette source must contain 1024 colors"));
        }
        validate_rectangle(source_rect)?;
        let mut graphics = prepare_textures(&self.textures, initial)?;
        let [x, y, width, height] = source_rect.map(usize::from);
        let source: Vec<u16> = (0..height)
            .flat_map(|row| {
                graphics.vram.words()
                    [(y + row) * VRAM_WIDTH + x..(y + row) * VRAM_WIDTH + x + width]
                    .iter()
                    .copied()
            })
            .collect();
        let mut patched = self.sections[0].bytes.clone();
        let far = [patched[8], patched[9], patched[10]];
        for index in 0..8_u8 {
            let sine_index = index * 2;
            let ir0 = math.sin(i32::from(sine_index) << 6);
            if !(0..=4096).contains(&ir0) {
                return Err(Error::Invalid(
                    "palette sine coefficient outside interpolation domain",
                ));
            }
            let bytes = source
                .iter()
                .flat_map(|&color| palette_color(color, far, ir0).to_le_bytes())
                .collect();
            let upload = Upload {
                source: UploadSource::DerivedPalette { sine_index },
                rectangle: [
                    320 + u16::from(index % 4) * 16,
                    384 + u16::from(index / 4) * 64,
                    16,
                    64,
                ],
                bytes,
            };
            upload.apply(&mut graphics.vram)?;
            graphics.uploads.push(upload);
        }
        for at in palette_record_offsets(&patched)? {
            let index = usize::from(halfword(&patched, at)?);
            patched[at + 6..at + 8].copy_from_slice(&source[index].to_le_bytes());
        }
        self.sections[0].bytes = patched;
        Ok(graphics)
    }
}

#[derive(Debug)]
pub struct FlightAircraftAssets {
    sections: Vec<ResidentSection>,
    textures: Vec<TextureGroup>,
    sound_bank: Vec<u8>,
    resident_bytes: usize,
}
impl FlightAircraftAssets {
    pub fn parse(resource: &[u8]) -> Result<Self> {
        let table = top_level(resource, 4)?;
        let mut budget = 20;
        let mut sections = Vec::with_capacity(2);
        let mut allocation_offset = 20;
        for (index, role) in [
            SectionRole::AircraftRecords6Context08,
            SectionRole::AircraftContext0c,
        ]
        .into_iter()
        .enumerate()
        {
            let bytes = decode(table.child(index)?, &mut budget)?;
            let directory = if index == 0 {
                Some(RecordDirectory::parse(&bytes, 6)?)
            } else {
                None
            };
            let len = bytes.len();
            sections.push(ResidentSection {
                index,
                role,
                allocation_offset,
                bytes,
                directory,
            });
            allocation_offset += len;
        }
        let textures = vec![texture_group(2, table.child(2)?, &mut budget)?];
        let sound_bank = checked_bank(table.child(3)?)?;
        Ok(Self {
            sections,
            textures,
            sound_bank,
            resident_bytes: allocation_offset,
        })
    }
    pub fn sections(&self) -> &[ResidentSection] {
        &self.sections
    }
    pub fn texture_groups(&self) -> &[TextureGroup] {
        &self.textures
    }
    pub fn sound_bank(&self) -> Result<Bank<'_>> {
        Ok(Bank::parse(&self.sound_bank)?)
    }
    pub fn sound_bank_bytes(&self) -> &[u8] {
        &self.sound_bank
    }
    pub fn resident_bytes(&self) -> usize {
        self.resident_bytes
    }
    pub fn prepare_graphics(&self, initial: &Vram) -> Result<FlightGraphics> {
        prepare_textures(&self.textures, initial)
    }
}

fn top_level(resource: &[u8], count: usize) -> Result<dot1::Container<'_>> {
    if resource.len() > MAX_ASSET_BYTES {
        return Err(Error::Invalid("resource exceeds 2MiB limit"));
    }
    let table = dot1::parse(resource)?;
    if table.len() != count {
        return Err(Error::Invalid("unexpected top-level child count"));
    }
    Ok(table)
}

fn decode(bytes: &[u8], budget: &mut usize) -> Result<Vec<u8>> {
    let size = (word(bytes, 4)? & 0x00ff_ffff) as usize;
    if size == 0 || size & 3 != 0 || size > MAX_ASSET_BYTES.saturating_sub(*budget) {
        return Err(Error::Invalid("decoded section length or aggregate limit"));
    }
    let result = ulz::decompress(bytes)?.payload;
    *budget += result.len();
    Ok(result)
}

fn texture_group(section: usize, compressed: &[u8], budget: &mut usize) -> Result<TextureGroup> {
    let bytes = decode(compressed, budget)?;
    let table = dot1::parse(&bytes)?;
    let image_count = table.len();
    let mut uploads = Vec::new();
    for image in 0..image_count {
        let decoded = tim::parse(table.child(image)?)?;
        for (block, source) in
            std::iter::once((&decoded.pixels, UploadSource::TimPixels { section, image })).chain(
                decoded
                    .clut
                    .as_ref()
                    .map(|block| (block, UploadSource::TimClut { section, image })),
            )
        {
            let rectangle = [block.x, block.y, block.width, block.height];
            validate_rectangle(rectangle)?;
            uploads.push(Upload {
                source,
                rectangle,
                bytes: block.data.to_vec(),
            });
        }
    }
    Ok(TextureGroup {
        section,
        image_count,
        bytes,
        uploads,
    })
}

fn checked_bank(bytes: &[u8]) -> Result<Vec<u8>> {
    let bank = Bank::parse(bytes)?;
    Ok(bytes[..bank.bytes_consumed()].to_vec())
}

fn prepare_textures(groups: &[TextureGroup], initial: &Vram) -> Result<FlightGraphics> {
    let mut vram = initial.clone();
    let mut uploads = Vec::new();
    for group in groups {
        for upload in &group.uploads {
            upload.apply(&mut vram)?;
            uploads.push(upload.clone());
        }
    }
    Ok(FlightGraphics { vram, uploads })
}

fn validate_rectangle(rect: [u16; 4]) -> Result<()> {
    let [x, y, width, height] = rect.map(usize::from);
    if width == 0 || height == 0 || x + width > VRAM_WIDTH || y + height > VRAM_HEIGHT {
        return Err(Error::Invalid("upload rectangle leaves retail VRAM"));
    }
    Ok(())
}

fn palette_record_offsets(bytes: &[u8]) -> Result<impl Iterator<Item = usize> + use<>> {
    let count = word(bytes, 0x74)? as usize;
    if count > bytes.len().saturating_sub(0x78) / 12 {
        return Err(Error::Invalid("palette reference records outside section0"));
    }
    Ok((0..count).map(|index| 0x78 + index * 12))
}
fn validate_palette_records(bytes: &[u8]) -> Result<()> {
    for at in palette_record_offsets(bytes)? {
        if halfword(bytes, at)? >= 1024 {
            return Err(Error::Invalid("palette reference index outside source"));
        }
    }
    Ok(())
}

// IRGB -> INTPL -> ORGB, preserving STP and transparent zero (8005D3F8).
fn palette_color(color: u16, far: [u8; 3], ir0: i32) -> u16 {
    if color == 0 {
        return 0;
    }
    let mut result = color & 0x8000;
    for (axis, far) in far.into_iter().enumerate() {
        let source = i32::from((color >> (axis * 5)) & 31) * 128;
        let value = ((source << 12) + ir0 * (i32::from(far) * 16 - source)) >> 19;
        result |= (value.clamp(0, 31) as u16) << (axis * 5);
    }
    result
}

fn slice(bytes: &[u8], range: Range<usize>) -> Result<&[u8]> {
    bytes.get(range).ok_or(Error::Invalid("truncated field"))
}
fn word(bytes: &[u8], at: usize) -> Result<u32> {
    let b = slice(bytes, at..at + 4)?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}
fn halfword(bytes: &[u8], at: usize) -> Result<u16> {
    let b = slice(bytes, at..at + 2)?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}
fn rectangle(bytes: &[u8], at: usize) -> Result<[u16; 4]> {
    Ok([
        halfword(bytes, at)?,
        halfword(bytes, at + 2)?,
        halfword(bytes, at + 4)?,
        halfword(bytes, at + 6)?,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    fn words(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    #[test]
    fn aliased_records_relocate_once_and_zero_is_not_null() {
        let bytes = words(&[3, 16, 16, 28, 9, 0, 0xffff_ffff, 8, 0, 4]);
        let directory = RecordDirectory::parse(&bytes, 2).unwrap();
        assert_eq!(directory.slots, [16, 16, 28]);
        assert_eq!(directory.relocations.len(), 4);
        assert_eq!(
            directory.relocations[0].target,
            RecordPointer::SectionOffset(16)
        );
        assert_eq!(
            directory.relocations[1].target,
            RecordPointer::Encoded(0xffff_ffff)
        );
        assert_eq!(
            directory.relocations[3].target,
            RecordPointer::SectionOffset(32)
        );
    }

    #[test]
    fn malformed_record_offsets_and_cross_record_pointers_fail() {
        for values in [
            vec![2, 12, 8, 0],
            vec![1, 9, 0],
            vec![1, 0x80000008, 0],
            vec![1, 8, 0, 8],
            vec![2, 12, 20, 0, 8, 0, 0],
        ] {
            assert!(RecordDirectory::parse(&words(&values), 1).is_err());
        }
        assert!(RecordDirectory::parse(&words(&[2, 12, 12, 0]), 2).is_err());
        assert!(RecordDirectory::parse(&words(&[0x10001, 8, 0, 0]), 1).is_err());
        assert!(RecordDirectory::parse(&[0; 4], 6).unwrap().slots.is_empty());
    }

    #[test]
    fn oversized_decompression_and_bad_palette_references_fail_before_work() {
        let mut budget = MAX_ASSET_BYTES - 4;
        assert!(decode(&words(&[0x1a7a6c55, 8]), &mut budget).is_err());
        assert_eq!(budget, MAX_ASSET_BYTES - 4);
        assert!(FlightMissionAssets::parse(&words(&[0])).is_err());
        assert!(FlightAircraftAssets::parse(&[]).is_err());
        let mut palette = vec![0; 0x84];
        palette[0x74] = 1;
        palette[0x79] = 4;
        assert!(validate_palette_records(&palette).is_err());
        palette[0x79] = 3;
        assert!(validate_palette_records(&palette).is_ok());
        palette[0x74] = 2;
        assert!(validate_palette_records(&palette).is_err());
    }

    #[test]
    fn palette_interpolation_keeps_transparency_and_stp() {
        assert_eq!(palette_color(0, [255; 3], 4096), 0);
        assert_eq!(palette_color(0x8000, [255; 3], 4096), 0xffff);
        assert_eq!(palette_color(0x7fff, [0; 3], 0), 0x7fff);
        assert_eq!(palette_color(0xffff, [0; 3], 4096), 0x8000);
        assert_eq!(palette_color(1, [0; 3], 2048), 0);
    }

    #[test]
    fn palette_preparation_reads_inherited_cells_and_patches_records_atomically() {
        let mut bytes = vec![0; 0x84];
        bytes[0x74] = 1;
        bytes[0x78] = 2;
        let mut assets = FlightMissionAssets {
            sections: vec![ResidentSection {
                index: 0,
                role: SectionRole::MissionPaletteParameters,
                allocation_offset: 72,
                bytes: bytes.clone(),
                directory: None,
            }],
            textures: Vec::new(),
            sound_bank: Vec::new(),
            object_slots: Vec::new(),
            resident_bytes: 72 + bytes.len(),
        };
        let mut initial = Vram::new();
        initial
            .load_block(&tim::Block {
                x: 322,
                y: 256,
                width: 1,
                height: 1,
                data: &0xa345_u16.to_le_bytes(),
            })
            .unwrap();
        assert!(assets.prepare_graphics(&[], &initial).is_err());
        assert_eq!(assets.sections[0].bytes, bytes);
        // Synthetic tables isolate the upload/copy transaction: all sine values
        // are zero, so each derived palette is the unchanged input palette.
        let mut resident = vec![0; 0xb0000];
        let at = 0x800bac38 - 0x80010000;
        for (index, value) in [320_u16, 256, 16, 64].into_iter().enumerate() {
            resident[at + index * 2..at + index * 2 + 2].copy_from_slice(&value.to_le_bytes());
        }
        let graphics = assets.prepare_graphics(&resident, &initial).unwrap();
        assert_eq!(halfword(assets.sections[0].bytes(), 0x7e).unwrap(), 0xa345);
        assert_eq!(initial.words()[384 * VRAM_WIDTH + 322], 0);
        assert_eq!(graphics.vram.words()[384 * VRAM_WIDTH + 322], 0xa345);
        assert_eq!(graphics.uploads.len(), 8);
        assert!(
            graphics
                .uploads
                .iter()
                .all(|u| halfword(&u.bytes, 4).unwrap() == 0xa345)
        );
    }
}
