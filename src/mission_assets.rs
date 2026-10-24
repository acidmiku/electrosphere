//! Mission-front-end resource load path: 0x800CEF34 / 0x800CF144.
use crate::{Result, assets::GameData, menu_scene::LanguageScene};
use ac3_formats::{
    dot1::{self, Container},
    ulz,
};
use ac3_graphics::{Vram, tim};

pub struct TextureUpload {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    pub data: Vec<u8>,
}

/// Native 800CE7E4/800CE844 queue. A poll processes at most `batch_size` TIMs,
/// preserving pixel/palette upload order; compressed TIMs are decoded on demand.
pub struct MissionTextureQueue<'a> {
    textures: Container<'a>,
    next: usize,
    batch_size: usize,
}
pub struct TexturePoll {
    pub complete: bool,
    pub uploads: Vec<TextureUpload>,
}
impl<'a> MissionTextureQueue<'a> {
    pub fn new(bytes: &'a [u8], batch_size: usize) -> Result<Self> {
        if batch_size == 0 {
            return Err("mission texture batch size must be positive".into());
        }
        Ok(Self {
            textures: dot1::parse(bytes)?,
            next: 0,
            batch_size,
        })
    }

    pub fn poll(&mut self, vram: &mut Vram) -> Result<TexturePoll> {
        let end = self
            .next
            .saturating_add(self.batch_size)
            .min(self.textures.len());
        let mut uploads = Vec::new();
        while self.next < end {
            let input = self.textures.child(self.next)?;
            let decoded;
            let bytes = if input.starts_with(b"Ulz\x1a") {
                decoded = ulz::decompress(input)?.payload;
                &decoded[..]
            } else {
                input
            };
            let image = tim::parse(bytes)?;
            vram.load_tim(&image)?;
            for block in [Some(image.pixels), image.clut].into_iter().flatten() {
                uploads.push(TextureUpload {
                    x: block.x,
                    y: block.y,
                    width: block.width,
                    height: block.height,
                    data: block.data.to_vec(),
                });
            }
            self.next += 1;
        }
        Ok(TexturePoll {
            complete: self.next == self.textures.len(),
            uploads,
        })
    }
}

pub struct MissionAssets<'a> {
    pub vram: Vram,
    pub uploads: Vec<TextureUpload>,
    pub scripts: Container<'a>,
    pub strings: Container<'a>,
    pub layouts: &'a [u8],
    pub map_textures: Container<'a>,
    pub mission_data: Container<'a>,
}
impl<'a> MissionAssets<'a> {
    /// The first mission's resolved resource is 74; callers supply the mapping
    /// result rather than deriving identity from filenames or screenshots.
    pub fn load(data: &'a GameData, mission_resource: usize) -> Result<Self> {
        let mission_data = dot1::parse(data.resource(mission_resource)?)?;
        let common = dot1::parse(data.resource(72)?)?;
        if mission_data.len() != 5 || common.len() != 13 {
            return Err("unexpected PAL mission resource layout".into());
        }
        let mut vram = LanguageScene::load(data)?.vram;
        let mut uploads = Vec::new();
        // 0x800CE7E4/0x800CE844: each TIM uploads pixels, then its palette.
        for bytes in [mission_data.child(4)?, common.child(10)?] {
            let mut queue = MissionTextureQueue::new(bytes, 10)?;
            loop {
                let result = queue.poll(&mut vram)?;
                uploads.extend(result.uploads);
                if result.complete {
                    break;
                }
            }
        }
        Ok(Self {
            vram,
            uploads,
            scripts: dot1::parse(common.child(3)?)?,
            strings: dot1::parse(common.child(7)?)?,
            layouts: common.child(8)?,
            map_textures: dot1::parse(common.child(0)?)?,
            mission_data,
        })
    }
}
