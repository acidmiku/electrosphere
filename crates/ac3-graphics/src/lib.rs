//! Native menu graphics primitives: TIM upload, texture lookup and 2D drawing.
//!
//! This implements sprites, tiles, flat textured quads, Gouraud polygons,
//! flat lines and draw-area state observed in menus and briefing. There is no
//! CPU interpreter, GPU timing or general 3D renderer. Native fixed point
//! rotation, translation, perspective and scalar math cover the recovered
//! mission-camera calls, including observable saturation and overflow flags.
//! Drawing order is supplied by the translated menu, not inferred from placement.
//!
//! [`Gpu`] is the separate VRAM-targeted command processor: it consumes the
//! complete GP0/GP1 stream and draws into the whole 1024x512 VRAM, including
//! VRAM copies, transfers and offscreen composition. The original-frame replay
//! tests validate it against PCSX-Redux's software GPU. The [`Frame`]
//! renderer below keeps its established behaviour for existing callers.

pub mod gpu;
#[cfg(test)]
mod gpu_tests;
mod line;
pub mod projection;
mod quad;
pub mod scalar;
mod soft_line;
mod soft_pixel;
mod soft_poly;
mod soft_rect;
pub mod tim;
pub mod transform;

pub use gpu::{Display, Dithering, Gpu, GpuStats};

use std::fmt;

pub const VRAM_WIDTH: usize = 1024;
pub const VRAM_HEIGHT: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Truncated(&'static str),
    Invalid(&'static str),
    Unsupported(&'static str),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated(s) => write!(f, "truncated {s}"),
            Self::Invalid(s) => write!(f, "invalid graphics input: {s}"),
            Self::Unsupported(s) => write!(f, "unsupported graphics input: {s}"),
        }
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;

/// 1024x512 halfwords. Serialized for save states; a deserialized image is
/// not length-checked here (callers check `words().len()`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Vram {
    words: Vec<u16>,
}

impl Default for Vram {
    fn default() -> Self {
        Self::new()
    }
}

impl Vram {
    pub fn new() -> Self {
        Self {
            words: vec![0; VRAM_WIDTH * VRAM_HEIGHT],
        }
    }
    pub fn words(&self) -> &[u16] {
        &self.words
    }
    pub fn from_le_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != VRAM_WIDTH * VRAM_HEIGHT * 2 {
            return Err(Error::Invalid("VRAM snapshot length"));
        }
        Ok(Self {
            words: bytes
                .chunks_exact(2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
                .collect(),
        })
    }
    /// Match 0x80026D00: pixel rectangle first, then palette rectangle.
    pub fn load_tim(&mut self, image: &tim::TimImage<'_>) -> Result<()> {
        self.load_block(&image.pixels)?;
        if let Some(clut) = &image.clut {
            self.load_block(clut)?;
        }
        Ok(())
    }
    pub fn load_block(&mut self, block: &tim::Block<'_>) -> Result<()> {
        let (x, y, width, height) = (
            usize::from(block.x),
            usize::from(block.y),
            usize::from(block.width),
            usize::from(block.height),
        );
        if x + width > VRAM_WIDTH
            || y + height > VRAM_HEIGHT
            || block.data.len() != width * height * 2
        {
            return Err(Error::Invalid("VRAM upload bounds or length"));
        }
        for row in 0..height {
            for col in 0..width {
                let offset = (row * width + col) * 2;
                self.words[(y + row) * VRAM_WIDTH + x + col] =
                    u16::from_le_bytes([block.data[offset], block.data[offset + 1]]);
            }
        }
        Ok(())
    }
    /// Resolve a GP0 texture-page/CLUT pair. UVs have the GPU's 8-bit wrapping.
    pub fn sample(&self, tpage: u16, clut: u16, u: u8, v: u8) -> Result<u16> {
        let x = usize::from(tpage & 15) * 64;
        let y = usize::from((tpage >> 4) & 1) * 256;
        let depth = (tpage >> 7) & 3;
        let (word_x, shift, mask) = match depth {
            0 => (usize::from(u) / 4, usize::from(u & 3) * 4, 15),
            1 => (usize::from(u) / 2, usize::from(u & 1) * 8, 255),
            2 => (usize::from(u), 0, 0xffff),
            _ => return Err(Error::Unsupported("reserved texture depth")),
        };
        let packed = self.words[((y + usize::from(v)) & 511) * VRAM_WIDTH + ((x + word_x) & 1023)];
        if depth == 2 {
            return Ok(packed);
        }
        let index = usize::from((packed >> shift) & mask);
        let palette_x = (usize::from(clut & 63) * 16 + index) & 1023;
        let palette_y = usize::from((clut >> 6) & 511);
        Ok(self.words[palette_y * VRAM_WIDTH + palette_x])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BlendMode {
    Average,
    Add,
    Subtract,
    AddQuarter,
}
impl BlendMode {
    pub fn from_tpage(tpage: u16) -> Self {
        match (tpage >> 5) & 3 {
            0 => Self::Average,
            1 => Self::Add,
            2 => Self::Subtract,
            _ => Self::AddQuarter,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sprite {
    pub x: i16,
    pub y: i16,
    pub width: u16,
    pub height: u16,
    pub u: u8,
    pub v: u8,
    pub tpage: u16,
    pub clut: u16,
    pub color: [u8; 3],
    pub raw: bool,
    pub semi_transparent: bool,
}

/// RGB555/STP pixels are kept until presentation, so blend arithmetic stays 5-bit.
#[derive(Debug, Clone)]
pub struct Frame {
    width: u16,
    height: u16,
    words: Vec<u16>,
    // Inclusive GP0 E3/E4 draw bounds, intersected with the host framebuffer.
    draw_area: [i32; 4],
    draw_offset: [i16; 2],
    set_mask: u16,
    check_mask: bool,
}
impl Frame {
    pub fn new(width: u16, height: u16) -> Result<Self> {
        if width == 0
            || height == 0
            || usize::from(width) > VRAM_WIDTH
            || usize::from(height) > VRAM_HEIGHT
        {
            return Err(Error::Invalid("frame dimensions must fit 1024 by 512"));
        }
        Ok(Self {
            width,
            height,
            words: vec![0; usize::from(width) * usize::from(height)],
            draw_area: [0, 0, i32::from(width) - 1, i32::from(height) - 1],
            draw_offset: [0; 2],
            set_mask: 0,
            check_mask: false,
        })
    }
    pub fn width(&self) -> u16 {
        self.width
    }
    pub fn height(&self) -> u16 {
        self.height
    }
    pub fn words(&self) -> &[u16] {
        &self.words
    }
    pub fn clear(&mut self, rgb555: u16) {
        self.words.fill(rgb555);
    }
    pub fn rgba8(&self) -> Vec<u8> {
        self.words.iter().flat_map(|&word| rgba8(word)).collect()
    }
    fn position(&self, word: u32) -> [i16; 2] {
        std::array::from_fn(|axis| {
            let coordinate = ((word >> (axis * 16)) & 0x7ff) as u16;
            let translated = coordinate.wrapping_add(self.draw_offset[axis] as u16);
            ((translated << 5) as i16) >> 5
        })
    }
    fn write_pixel(&mut self, index: usize, front: u16, mode: Option<BlendMode>) {
        if self.check_mask && self.words[index] & 0x8000 != 0 {
            return;
        }
        let color = if let Some(mode) = mode {
            blend(self.words[index], front, mode)
        } else {
            front
        };
        self.words[index] = color | self.set_mask;
    }
    pub fn draw_sprite(&mut self, vram: &Vram, sprite: Sprite) -> Result<()> {
        self.draw_sprite_with_window(vram, sprite, TextureWindow::default())
    }
    fn draw_sprite_with_window(
        &mut self,
        vram: &Vram,
        sprite: Sprite,
        window: TextureWindow,
    ) -> Result<()> {
        if (sprite.tpage >> 7) & 3 == 3 {
            return Err(Error::Unsupported("reserved texture depth"));
        }
        let (x0, y0, x1, y1) = self.clipped(sprite.x, sprite.y, sprite.width, sprite.height);
        for y in y0..y1 {
            for x in x0..x1 {
                let u = sprite.u.wrapping_add((x - i32::from(sprite.x)) as u8);
                let v = sprite.v.wrapping_add((y - i32::from(sprite.y)) as u8);
                let (u, v) = window.apply(u, v);
                let texel = vram.sample(sprite.tpage, sprite.clut, u, v)?;
                if texel == 0 {
                    continue;
                }
                let front = if sprite.raw {
                    texel
                } else {
                    modulate(texel, sprite.color)
                };
                let index = y as usize * usize::from(self.width) + x as usize;
                let mode = (sprite.semi_transparent && texel & 0x8000 != 0)
                    .then(|| BlendMode::from_tpage(sprite.tpage));
                self.write_pixel(index, front, mode);
            }
        }
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    pub fn draw_tile(
        &mut self,
        x: i16,
        y: i16,
        width: u16,
        height: u16,
        color: [u8; 3],
        mode: Option<BlendMode>,
    ) {
        let front = u16::from(color[0] >> 3)
            | (u16::from(color[1] >> 3) << 5)
            | (u16::from(color[2] >> 3) << 10);
        let (x0, y0, x1, y1) = self.clipped(x, y, width, height);
        for y in y0..y1 {
            for x in x0..x1 {
                let index = y as usize * usize::from(self.width) + x as usize;
                self.write_pixel(index, front, mode);
            }
        }
    }
    fn clipped(&self, x: i16, y: i16, width: u16, height: u16) -> (i32, i32, i32, i32) {
        (
            i32::from(x).max(0).max(self.draw_area[0]),
            i32::from(y).max(0).max(self.draw_area[1]),
            (i32::from(x) + i32::from(width))
                .min(i32::from(self.width))
                .min(self.draw_area[2] + 1),
            (i32::from(y) + i32::from(height))
                .min(i32::from(self.height))
                .min(self.draw_area[3] + 1),
        )
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct TextureWindow {
    mask_x: u8,
    mask_y: u8,
    offset_x: u8,
    offset_y: u8,
}
impl TextureWindow {
    fn from_word(word: u32) -> Self {
        Self {
            mask_x: (word as u8 & 31) * 8,
            mask_y: ((word >> 5) as u8 & 31) * 8,
            offset_x: ((word >> 10) as u8 & 31) * 8,
            offset_y: ((word >> 15) as u8 & 31) * 8,
        }
    }
    fn apply(self, u: u8, v: u8) -> (u8, u8) {
        (
            (u & !self.mask_x) | (self.offset_x & self.mask_x),
            (v & !self.mask_y) | (self.offset_y & self.mask_y),
        )
    }
}

/// Render ordered menu GP0 payloads, excluding DMA headers.
///
/// Supports variable-size sprites/tiles, fixed 8x8 sprites, flat textured quads, Gouraud triangles
/// and quads, flat lines, E1..E6 drawing state, NOPs,
/// and identity VRAM copies. Other commands return errors instead of being
/// silently omitted. The frame intersects the E3/E4 draw area; E5 offsets wrap
/// primitive coordinates to signed 11-bit values before clipping. Returns primitive count.
pub fn render_packets(frame: &mut Frame, vram: &Vram, packets: &[Vec<u32>]) -> Result<usize> {
    render_packets_inner(frame, VramAccess::Read(vram), packets)
}

/// The same drawing path with ordered CPU-to-VRAM uploads enabled.
pub fn render_packets_with_transfers(
    frame: &mut Frame,
    vram: &mut Vram,
    packets: &[Vec<u32>],
) -> Result<usize> {
    render_packets_inner(frame, VramAccess::Write(vram), packets)
}

enum VramAccess<'a> {
    Read(&'a Vram),
    Write(&'a mut Vram),
}
impl VramAccess<'_> {
    fn read(&self) -> &Vram {
        match self {
            Self::Read(vram) => vram,
            Self::Write(vram) => vram,
        }
    }
    fn upload(&mut self, block: tim::Block<'_>) -> Result<()> {
        match self {
            Self::Read(_) => Err(Error::Unsupported("VRAM upload requires writable renderer")),
            Self::Write(vram) => vram.load_block(&block),
        }
    }
}

fn render_packets_inner(
    frame: &mut Frame,
    mut vram: VramAccess<'_>,
    packets: &[Vec<u32>],
) -> Result<usize> {
    let mut tpage = 0;
    let mut window = TextureWindow::default();
    let mut drawn = 0;
    for packet in packets {
        let mut offset = 0;
        while offset < packet.len() {
            let command = packet[offset];
            let op = command >> 24;
            let count = match op {
                0 | 1 | 0xe1..=0xe6 => 1,
                0x60..=0x63 | 0x74..=0x77 => 3,
                0x64..=0x67 | 0x80 => 4,
                0x2c..=0x2f => 9,
                0x28..=0x2b => 5,
                0x20..=0x23 => 4,
                0x30..=0x33 => 6,
                0x38..=0x3b => 8,
                0x40..=0x43 => 3,
                0xa0 => {
                    let size = *packet
                        .get(offset + 2)
                        .ok_or(Error::Truncated("GP0 upload header"))?;
                    let width = ((size.wrapping_sub(1) & 0x3ff) + 1) as usize;
                    let height = (((size >> 16).wrapping_sub(1) & 0x1ff) + 1) as usize;
                    3 + (width * height).div_ceil(2)
                }
                _ => {
                    return Err(Error::Unsupported(
                        "GP0 command outside the supported menu subset",
                    ));
                }
            };
            let words = packet
                .get(offset..offset + count)
                .ok_or(Error::Truncated("GP0 packet"))?;
            match op {
                0 | 1 => {}
                0xe1 => {
                    if command & 0x3800 != 0 {
                        return Err(Error::Unsupported("texture disable or sprite flipping"));
                    }
                    tpage = command as u16;
                }
                0xe2 => window = TextureWindow::from_word(command),
                0xe5 => {
                    frame.draw_offset = std::array::from_fn(|axis| {
                        ((((command >> (axis * 11)) as u16) << 5) as i16) >> 5
                    });
                }
                0xe6 => {
                    frame.set_mask = if command & 1 != 0 { 0x8000 } else { 0 };
                    frame.check_mask = command & 2 != 0;
                }
                0xe3 | 0xe4 => {
                    let at = if op == 0xe3 { 0 } else { 2 };
                    frame.draw_area[at] = (command & 0x3ff) as i32;
                    frame.draw_area[at + 1] = ((command >> 10) & 0x1ff) as i32;
                }
                0x80 => {
                    if words[1] != words[2] {
                        return Err(Error::Unsupported("nonidentity VRAM copy"));
                    }
                }
                0xa0 => {
                    if frame.set_mask != 0 || frame.check_mask {
                        return Err(Error::Unsupported("masked VRAM upload"));
                    }
                    let width = ((words[2].wrapping_sub(1) & 0x3ff) + 1) as u16;
                    let height = (((words[2] >> 16).wrapping_sub(1) & 0x1ff) + 1) as u16;
                    let payload: Vec<u8> = words[3..]
                        .iter()
                        .flat_map(|w| w.to_le_bytes())
                        .take(usize::from(width) * usize::from(height) * 2)
                        .collect();
                    vram.upload(tim::Block {
                        x: (words[1] & 0x3ff) as u16,
                        y: ((words[1] >> 16) & 0x1ff) as u16,
                        width,
                        height,
                        data: &payload,
                    })?;
                }
                0x28..=0x2b => {
                    frame.draw_quad(vram.read(), words, tpage, window)?;
                    drawn += 1;
                }
                0x20..=0x23 => {
                    frame.draw_flat_triangle(vram.read(), words, tpage, window)?;
                    drawn += 1;
                }
                0x2c..=0x2f => {
                    tpage = (tpage & !0x1ff) | ((words[4] >> 16) as u16 & 0x1ff);
                    frame.draw_quad(vram.read(), words, tpage, window)?;
                    drawn += 1;
                }
                0x30..=0x33 | 0x38..=0x3b => {
                    frame.draw_gouraud(vram.read(), words, tpage, window)?;
                    drawn += 1;
                }
                0x40..=0x43 => {
                    frame.draw_line(words, tpage);
                    drawn += 1;
                }
                _ => {
                    let color = [command as u8, (command >> 8) as u8, (command >> 16) as u8];
                    let [x, y] = frame.position(words[1]);
                    let (width, height) = if matches!(op, 0x74..=0x77) {
                        // The resident loading font uses SPRT_8 (0x76).
                        // Fixed-size sprite packets end with UV/CLUT, no WH word.
                        (8, 8)
                    } else {
                        let size = words[count - 1];
                        ((size & 0x3ff) as u16, ((size >> 16) & 0x1ff) as u16)
                    };
                    if op & 4 != 0 {
                        frame.draw_sprite_with_window(
                            vram.read(),
                            Sprite {
                                x,
                                y,
                                width,
                                height,
                                u: words[2] as u8,
                                v: (words[2] >> 8) as u8,
                                clut: (words[2] >> 16) as u16,
                                tpage,
                                color,
                                raw: op & 1 != 0,
                                semi_transparent: op & 2 != 0,
                            },
                            window,
                        )?;
                    } else {
                        frame.draw_tile(
                            x,
                            y,
                            width,
                            height,
                            color,
                            (op & 2 != 0).then(|| BlendMode::from_tpage(tpage)),
                        );
                    }
                    drawn += 1;
                }
            }
            offset += count;
        }
    }
    Ok(drawn)
}

pub fn rgba8(word: u16) -> [u8; 4] {
    let channel = |shift: u32| {
        let value = ((word >> shift) & 31u16) as u8;
        (value << 3) | (value >> 2)
    };
    [channel(0), channel(5), channel(10), 255]
}

fn modulate(texel: u16, color: [u8; 3]) -> u16 {
    let mut output = texel & 0x8000;
    for (i, color) in color.into_iter().enumerate() {
        let value = ((((texel >> (i * 5)) & 31) * u16::from(color)) >> 7).min(31);
        output |= value << (i * 5);
    }
    output
}

fn blend(back: u16, front: u16, mode: BlendMode) -> u16 {
    let mut output = front & 0x8000;
    for shift in [0, 5, 10] {
        let b = (back >> shift) & 31;
        let f = (front >> shift) & 31;
        let value = match mode {
            BlendMode::Average => (b + f) >> 1,
            BlendMode::Add => (b + f).min(31),
            BlendMode::Subtract => b.saturating_sub(f),
            BlendMode::AddQuarter => (b + (f >> 2)).min(31),
        };
        output |= value << shift;
    }
    output
}
