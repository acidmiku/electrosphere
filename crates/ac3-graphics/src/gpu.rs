//! VRAM-targeted GP0/GP1 command processor.
//!
//! [`Gpu`] consumes the GP0 word stream exactly as the console GPU receives it
//! (linked-list DMA payloads, direct writes and CPU-to-VRAM transfers) and
//! draws straight into the 1024x512 [`Vram`]. Offscreen composition therefore
//! works naturally: copies, render-to-texture and feedback sampling all read
//! the live VRAM. The rasterization, blending, dithering, clipping and quirks
//! follow the PCSX-Redux software GPU at revision
//! `e1693b7f756ebdc4ccae1230ed09992079b51f90`, which produced the evidence.
//!
//! Supported: every polygon (flat/Gouraud, textured/untextured, raw or
//! modulated, opaque or semi-transparent, triangles and quads), single lines
//! and polylines, every rectangle size, E1-E6 state, GP0 02 fill, 80 copy,
//! A0 upload and C0 readback commands (readback data is not produced). Texture
//! flip bits are accepted and ignored, as in the reference. Inputs that the
//! reference resolves through memory outside the 1024x512 VRAM return
//! [`Error::Unsupported`] instead of guessing: the 2 MB VRAM gate, fills and
//! CPU transfers crossing the VRAM edge, copy coordinates outside VRAM,
//! texture/palette reads past the last VRAM word, and polygon or sprite pixels
//! below row 511 (only reachable with an E4 bottom above 511).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::soft_line::draw_line;
use crate::soft_pixel::{Sampler, TexDepth, WriteState, command_color};
use crate::soft_poly::{Area, Shading, Target, Vertex, draw_triangle};
use crate::soft_rect::{copy_rect, draw_rect, draw_sprite, fill_area};
use crate::{BlendMode, Error, Result, VRAM_HEIGHT, VRAM_WIDTH, Vram};

/// Dithering policy (the reference renderer's "Dithering" setting).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Dithering {
    Off,
    /// Dither Gouraud primitives when E1 bit 9 requests it (reference default).
    #[default]
    GameDependent,
    Always,
}

/// Display configuration written through GP1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Display {
    /// GP1(05) start in VRAM.
    pub start_x: u16,
    pub start_y: u16,
    /// GP1(06) raw horizontal range (x1 << 12 | x0).
    pub horizontal_range: u32,
    /// GP1(07) raw vertical range (y1 << 10 | y0).
    pub vertical_range: u32,
    /// GP1(08) raw display mode.
    pub mode: u32,
    pub enabled: bool,
}

impl Display {
    /// Horizontal resolution selected by GP1(08).
    pub fn width(&self) -> u16 {
        if self.mode & 0x40 != 0 {
            368
        } else {
            [256, 320, 512, 640][(self.mode & 3) as usize]
        }
    }
}

/// Counters of processed commands.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuStats {
    /// Complete GP0 commands, keyed by command byte.
    pub commands: BTreeMap<u8, u64>,
    /// GP0 words consumed, including transfer payloads.
    pub words: u64,
    /// Polygons, line segments and rectangles that reached the rasterizer.
    pub drawn: u64,
    /// Polygons and lines dropped by the coordinate-range limits.
    pub culled: u64,
    /// Drawn primitives per rasterizer path, e.g. `poly.gt.semi1.dither`:
    /// kind (`poly`, `line`, `tile`, `sprite`, `fill`, `copy`, `upload`),
    /// shading/texture (`f`/`g`, `t`, `raw`), size for rectangles, then the
    /// blend function, dithering and mask modifiers that applied.
    pub paths: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DrawState {
    area: Area,
    offset: [i32; 2],
    // Renderer texture page (E1, or the page word of a drawn textured polygon).
    tex_base: [i32; 2],
    depth: TexDepth,
    abr: BlendMode,
    dither: bool,
    status_dither: bool,
    window_mask: [i32; 2],
    window_offset: [i32; 2],
    set_mask: u16,
    check_mask: bool,
    cheat: u16,
}

impl Default for DrawState {
    fn default() -> Self {
        Self {
            area: Area {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            offset: [0; 2],
            tex_base: [0; 2],
            depth: TexDepth::Clut4,
            abr: BlendMode::Average,
            dither: false,
            status_dither: false,
            window_mask: [0; 2],
            window_offset: [0; 2],
            set_mask: 0,
            check_mask: false,
            cheat: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Upload {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    pixels: Vec<u16>,
}

/// Software GPU with its own VRAM. Serialization covers the whole state
/// (VRAM, drawing state, partial commands and transfers, display, counters)
/// for save states.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gpu {
    vram: Vram,
    state: DrawState,
    dithering: Dithering,
    fifo: Vec<u32>,
    upload: Option<Upload>,
    display: Display,
    stats: GpuStats,
    /// Last E1..E6 words (index 1..6), as GPUSTAT and GP1(10h) report them.
    environment: [u32; 8],
}

fn sign_extend_11(value: u32) -> i32 {
    ((value << 21) as i32) >> 21
}

fn over_limit(a: (i32, i32), b: (i32, i32)) -> bool {
    (a.0 - b.0).abs() >= 1024 || (a.1 - b.1).abs() >= 512
}

fn terminator(word: u32) -> bool {
    word & 0xf000_f000 == 0x5000_5000
}

impl Gpu {
    pub fn new(vram: Vram) -> Self {
        Self {
            vram,
            state: DrawState::default(),
            dithering: Dithering::default(),
            fifo: Vec::new(),
            upload: None,
            display: Display::default(),
            stats: GpuStats::default(),
            environment: [0; 8],
        }
    }
    pub fn vram(&self) -> &Vram {
        &self.vram
    }
    /// Replace VRAM while keeping drawing state, e.g. to start from a snapshot.
    pub fn set_vram(&mut self, vram: Vram) {
        self.vram = vram;
    }
    pub fn into_vram(self) -> Vram {
        self.vram
    }
    pub fn display(&self) -> Display {
        self.display
    }
    pub fn stats(&self) -> &GpuStats {
        &self.stats
    }
    pub fn set_dithering(&mut self, dithering: Dithering) {
        self.dithering = dithering;
    }
    /// True when no command or transfer is partially received.
    pub fn is_idle(&self) -> bool {
        self.fifo.is_empty() && self.upload.is_none()
    }
    /// Displayed RGB555/STP pixels of `width` x `height` from the GP1(05) start.
    pub fn display_pixels(&self, width: usize, height: usize) -> Result<Vec<u16>> {
        let (x, y) = (
            usize::from(self.display.start_x),
            usize::from(self.display.start_y),
        );
        if x + width > VRAM_WIDTH || y + height > VRAM_HEIGHT {
            return Err(Error::Unsupported("display area crossing the VRAM edge"));
        }
        let words = self.vram.words();
        Ok((y..y + height)
            .flat_map(|row| {
                words[row * VRAM_WIDTH + x..row * VRAM_WIDTH + x + width]
                    .iter()
                    .copied()
            })
            .collect())
    }

    pub fn write_gp0_words(&mut self, words: &[u32]) -> Result<()> {
        words.iter().try_for_each(|&word| self.write_gp0(word))
    }

    /// Feed one GP0 word; complete commands execute immediately.
    pub fn write_gp0(&mut self, word: u32) -> Result<()> {
        self.stats.words += 1;
        if let Some(upload) = &mut self.upload {
            upload.pixels.push(word as u16);
            upload.pixels.push((word >> 16) as u16);
            if upload.pixels.len() >= upload.width * upload.height {
                let upload = self.upload.take().expect("upload in progress");
                self.finish_upload(upload);
            }
            return Ok(());
        }
        self.fifo.push(word);
        if self.command_complete()? {
            let words = std::mem::take(&mut self.fifo);
            let result = self.execute(&words);
            self.fifo = words;
            self.fifo.clear();
            result?;
        }
        Ok(())
    }

    /// GPUSTAT bits 0..10 (texture page, semi-transparency, depth, dither,
    /// drawing to the display area: the last E1 parameter) and bit 15 (E1
    /// bit 11), with the ready bits 26..28 set: the GPU here is always idle.
    pub fn status(&self) -> u32 {
        let e1 = self.environment[1];
        (e1 & 0x7ff) | ((e1 >> 11) & 1) << 15 | 0x1c00_0000
    }

    /// GP1(10h) information `index`: 2..5 give the last E2..E5 parameters
    /// (texture window, drawing area corners, drawing offset); others 0.
    pub fn info(&self, index: u32) -> u32 {
        match index & 0xf {
            2..=5 => self.environment[(index & 7) as usize],
            _ => 0,
        }
    }

    /// GP1 control word. Only state that affects drawing or display is kept.
    pub fn write_gp1(&mut self, word: u32) -> Result<()> {
        match word >> 24 {
            0x00 => {
                self.fifo.clear();
                self.upload = None;
                self.state = DrawState::default();
                self.display = Display::default();
                self.environment = [0; 8];
            }
            // FIFO reset leaves a partially received command untouched.
            0x01 | 0x02 | 0x04 | 0x10..=0x1f => {}
            0x03 => self.display.enabled = word & 1 == 0,
            0x05 => {
                self.display.start_x = (word & 0x3ff) as u16;
                self.display.start_y = ((word >> 10) & 0x1ff) as u16;
            }
            0x06 => self.display.horizontal_range = word & 0xff_ffff,
            0x07 => self.display.vertical_range = word & 0xf_ffff,
            0x08 => self.display.mode = word & 0xff_ffff,
            0x09 if word & 1 != 0 => return Err(Error::Unsupported("2 MB VRAM gate")),
            _ => {}
        }
        Ok(())
    }

    fn command_complete(&self) -> Result<bool> {
        let words = &self.fifo;
        let op = (words[0] >> 24) as u8;
        let needed = match op {
            0x02 => 3,
            0x20..=0x3f => {
                let vertices = if op & 8 != 0 { 4 } else { 3 };
                let textured = usize::from(op & 4 != 0);
                let gouraud = op & 0x10 != 0;
                1 + vertices * (1 + textured) + if gouraud { vertices - 1 } else { 0 }
            }
            0x40..=0x5f if op & 8 == 0 => {
                if op & 0x10 != 0 {
                    4
                } else {
                    3
                }
            }
            0x40..=0x5f => {
                // Polyline: a terminator is recognised on any word once two
                // vertices are complete.
                let first_two = 1 + if op & 0x10 != 0 { 3 } else { 2 };
                if words.len() > first_two && terminator(*words.last().expect("nonempty")) {
                    return Ok(true);
                }
                if words.len() > 0x10000 {
                    return Err(Error::Unsupported("unterminated polyline"));
                }
                usize::MAX
            }
            0x60..=0x7f => {
                let textured = usize::from(op & 4 != 0);
                2 + textured + usize::from(op & 0x18 == 0)
            }
            0x80..=0x9f => 4,
            0xa0..=0xdf => 3,
            _ => 1,
        };
        Ok(words.len() >= needed)
    }

    fn execute(&mut self, words: &[u32]) -> Result<()> {
        let command = words[0];
        let op = (command >> 24) as u8;
        *self.stats.commands.entry(op).or_default() += 1;
        if (0xe1..=0xe6).contains(&op) {
            self.environment[usize::from(op & 7)] = command & 0xff_ffff;
        }
        match op {
            0x02 => self.fill(words),
            0x20..=0x3f => self.polygon(words),
            0x40..=0x5f => self.line(words),
            0x60..=0x7f => self.rectangle(words),
            0x80..=0x9f => self.copy(words),
            0xa0..=0xbf => self.begin_upload(words),
            // VRAM-to-CPU readback changes nothing in VRAM.
            0xc0..=0xdf => Ok(()),
            0xe1 => {
                self.status_texture_page(command & 0xff_ffff);
                self.texture_page(command & 0xff_ffff);
                Ok(())
            }
            0xe2 => {
                let field = |shift: u32| ((command >> shift) & 0x1f) as i32;
                self.state.window_mask = [field(0) << 3, field(5) << 3];
                self.state.window_offset = [
                    (field(10) << 3) & self.state.window_mask[0],
                    (field(15) << 3) & self.state.window_mask[1],
                ];
                Ok(())
            }
            0xe3 => {
                self.state.area.left = (command & 0x3ff) as i32;
                self.state.area.top = ((command >> 10) & 0x3ff) as i32;
                Ok(())
            }
            0xe4 => {
                self.state.area.right = (command & 0x3ff) as i32;
                self.state.area.bottom = ((command >> 10) & 0x3ff) as i32;
                Ok(())
            }
            0xe5 => {
                self.state.offset = [sign_extend_11(command), sign_extend_11(command >> 11)];
                Ok(())
            }
            0xe6 => {
                self.state.set_mask = if command & 1 != 0 { 0x8000 } else { 0 };
                self.state.check_mask = command & 2 != 0;
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn dither_enabled(&self, requested: bool) -> bool {
        match self.dithering {
            Dithering::Off => false,
            Dithering::GameDependent => requested,
            Dithering::Always => true,
        }
    }

    /// GPUSTAT bookkeeping that textured polygons consult for their dither bit.
    fn status_texture_page(&mut self, raw: u32) {
        self.state.status_dither = self.dither_enabled(raw & 0x200 != 0);
    }

    /// Renderer texture page: base, depth, blend function and dithering.
    fn texture_page(&mut self, raw: u32) {
        self.state.tex_base = [((raw & 0xf) << 6) as i32, (((raw >> 4) & 1) << 8) as i32];
        self.state.abr = BlendMode::from_tpage(raw as u16);
        self.state.depth = match (raw >> 7) & 3 {
            0 => TexDepth::Clut4,
            1 => TexDepth::Clut8,
            _ => TexDepth::Direct15,
        };
        self.state.dither = self.dither_enabled(raw & 0x200 != 0);
    }

    /// Count one drawn primitive under `base` plus the modifiers that apply.
    fn count_path(&mut self, mut path: String, semi: bool, dither: bool, masks: bool) {
        if semi {
            let abr = match self.state.abr {
                BlendMode::Average => 0,
                BlendMode::Add => 1,
                BlendMode::Subtract => 2,
                BlendMode::AddQuarter => 3,
            };
            path.push_str(&format!(".semi{abr}"));
        }
        if dither {
            path.push_str(".dither");
        }
        if masks && self.state.check_mask {
            path.push_str(".check");
        }
        if masks && self.state.set_mask != 0 {
            path.push_str(".set");
        }
        self.stats.drawn += 1;
        *self.stats.paths.entry(path).or_default() += 1;
    }

    fn write_state(&self, semi: bool) -> WriteState {
        WriteState {
            abr: self.state.abr,
            semi,
            check_mask: self.state.check_mask,
            set_mask: self.state.set_mask,
        }
    }

    fn sampler(&self, clut: u32) -> Sampler {
        let clut_x = ((clut & 0x3f) * 16) as i32;
        let clut_y = ((clut >> 6) & 0x3ff) as i32;
        Sampler {
            depth: self.state.depth,
            base_x: self.state.tex_base[0],
            base_y: self.state.tex_base[1],
            mask_x: i32::from(!(self.state.window_mask[0] as u8)),
            mask_y: i32::from(!(self.state.window_mask[1] as u8)),
            window_x: self.state.window_offset[0],
            window_y: self.state.window_offset[1],
            clut: if self.state.depth == TexDepth::Direct15 {
                0
            } else {
                (clut_y << 10) + clut_x
            },
        }
    }

    fn offset_point(&self, xy: (i32, i32)) -> (i32, i32) {
        (
            sign_extend_11((xy.0 + self.state.offset[0]) as u32),
            sign_extend_11((xy.1 + self.state.offset[1]) as u32),
        )
    }

    fn polygon(&mut self, words: &[u32]) -> Result<()> {
        let op = (words[0] >> 24) as u8;
        let gouraud = op & 0x10 != 0;
        let quad = op & 8 != 0;
        let textured = op & 4 != 0;
        let semi = op & 2 != 0;
        let raw = op & 1 != 0;
        let count = if quad { 4 } else { 3 };
        let mut points = [(0, 0); 4];
        let mut uv = [(0, 0); 4];
        let mut colors = [words[0] & 0xff_ffff; 4];
        let (mut clut, mut page) = (0, 0);
        let mut at = 1;
        for i in 0..count {
            if gouraud && i > 0 {
                colors[i] = words[at] & 0xff_ffff;
                at += 1;
            }
            if gouraud && textured && raw {
                colors[i] = 0x80_8080;
            }
            let xy = words[at];
            at += 1;
            points[i] = (sign_extend_11(xy), sign_extend_11(xy >> 16));
            if textured {
                let word = words[at];
                at += 1;
                uv[i] = ((word & 0xff) as i32, ((word >> 8) & 0xff) as i32);
                match i {
                    0 => clut = word >> 16,
                    1 => page = (word >> 16) & 0x9ff,
                    _ => {}
                }
            }
        }
        let points = points.map(|p| self.offset_point(p));
        let culled = if quad {
            [(0, 1), (1, 3), (3, 2), (2, 0), (1, 2)]
                .iter()
                .any(|&(a, b)| over_limit(points[a], points[b]))
        } else {
            [(0, 1), (1, 2), (2, 0)]
                .iter()
                .any(|&(a, b)| over_limit(points[a], points[b]))
        };
        if culled {
            self.stats.culled += 1;
            return Ok(());
        }
        let m = if textured && !raw {
            [0, 8, 16].map(|shift| ((words[0] >> shift) & 0xff) as i32)
        } else {
            [128; 3]
        };
        if textured {
            // A drawn polygon's page word carries the current dither status.
            let page = if self.state.status_dither {
                page | 0x200
            } else {
                page
            };
            self.status_texture_page(page);
            self.texture_page(page);
        }
        let vertex = |i: usize| Vertex {
            x: points[i].0,
            y: points[i].1,
            u: uv[i].0,
            v: uv[i].1,
            rgb: colors[i],
        };
        let shading = match (gouraud, textured) {
            (false, false) => Shading::Flat(colors[0]),
            (false, true) => Shading::FlatTextured(self.sampler(clut), m),
            (true, false) => Shading::Gouraud,
            (true, true) => Shading::GouraudTextured(self.sampler(clut)),
        };
        // Flat paths never dither; Gouraud follows the renderer dither mode.
        let dither = gouraud && self.state.dither;
        let path = format!(
            "poly.{}{}{}{}",
            if gouraud { 'g' } else { 'f' },
            count,
            if textured { "t" } else { "" },
            if textured && raw { "raw" } else { "" }
        );
        self.count_path(path, semi, dither, true);
        let write = self.write_state(semi);
        let area = self.state.area;
        let mut target = Target {
            vram: &mut self.vram.words,
            area,
            write,
            dither,
        };
        if quad {
            draw_triangle(&mut target, [vertex(1), vertex(3), vertex(2)], shading)?;
        }
        draw_triangle(&mut target, [vertex(0), vertex(1), vertex(2)], shading)
    }

    fn line(&mut self, words: &[u32]) -> Result<()> {
        let op = (words[0] >> 24) as u8;
        let gouraud = op & 0x10 != 0;
        let poly = op & 8 != 0;
        let semi = op & 2 != 0;
        let point = |xy: u32| (sign_extend_11(xy), sign_extend_11(xy >> 16));
        let mut colors = vec![words[0] & 0xff_ffff];
        let mut points = vec![point(words[1])];
        // Polylines end with their terminator word, which carries no vertex.
        let end = if poly { words.len() - 1 } else { words.len() };
        let mut at = 2;
        while at < end {
            if gouraud {
                colors.push(words[at] & 0xff_ffff);
                at += 1;
                if at >= end {
                    break;
                }
            } else {
                colors.push(colors[0]);
            }
            points.push(point(words[at]));
            at += 1;
        }
        if colors.len() < 2 || colors.len() != points.len() {
            // The reference rejects a polyline ending between color and vertex.
            return Ok(());
        }
        let write = self.write_state(semi);
        for i in 1..points.len() {
            let a = self.offset_point(points[i - 1]);
            let b = self.offset_point(points[i]);
            if over_limit(a, b) {
                self.stats.culled += 1;
                continue;
            }
            let path = format!(
                "line.{}{}",
                if gouraud { 'g' } else { 'f' },
                if poly { "poly" } else { "" }
            );
            self.count_path(path, semi, false, true);
            draw_line(
                &mut self.vram.words,
                self.state.area,
                &write,
                [a.0, a.1, b.0, b.1],
                colors[i - 1],
                colors[i],
                gouraud,
            );
        }
        Ok(())
    }

    fn rectangle(&mut self, words: &[u32]) -> Result<()> {
        let op = (words[0] >> 24) as u8;
        let textured = op & 4 != 0;
        let semi = op & 2 != 0;
        let raw = op & 1 != 0;
        let x = sign_extend_11(words[1]);
        let y = sign_extend_11(words[1] >> 16);
        let (w, h) = match (op >> 3) & 3 {
            0 => {
                let size = words[2 + usize::from(textured)];
                ((size & 0x3ff) as i32, ((size >> 16) & 0x1ff) as i32)
            }
            1 => (1, 1),
            2 => (8, 8),
            _ => (16, 16),
        };
        if w == 0 || h == 0 {
            return Ok(());
        }
        let size = ["", "1", "8", "16"][usize::from((op >> 3) & 3)];
        let path = if textured {
            format!("sprite{size}{}", if raw { ".raw" } else { "" })
        } else {
            format!("tile{size}")
        };
        self.count_path(path, semi, false, true);
        let color = words[0] & 0xff_ffff;
        let m = if textured && !raw {
            [0, 8, 16].map(|shift| ((color >> shift) & 0xff) as i32)
        } else {
            [128; 3]
        };
        let (x0, y0) = (x + self.state.offset[0], y + self.state.offset[1]);
        let write = self.write_state(semi);
        let area = self.state.area;
        if textured {
            let word = words[2];
            let sampler = self.sampler(word >> 16);
            draw_sprite(
                &mut self.vram.words,
                area,
                &write,
                &sampler,
                m,
                [x0, y0, w, h],
                (word & 0xff) as i32,
                ((word >> 8) & 0xff) as i32,
            )
        } else {
            draw_rect(
                &mut self.vram.words,
                area,
                &write,
                [x0, y0, x0 + w, y0 + h],
                command_color(color),
                &mut self.state.cheat,
            );
            Ok(())
        }
    }

    fn fill(&mut self, words: &[u32]) -> Result<()> {
        let x = (words[1] & 0x3f0) as i32;
        let y = (words[1] >> 16) as i32;
        let w = ((((words[2] & 0x3ff) + 0xf) & !0xf) as i32).min(1024);
        let h = ((words[2] >> 16) & 0x1ff) as i32;
        if w == 0 || h == 0 {
            return Ok(());
        }
        if y >= VRAM_HEIGHT as i32 || x + w > VRAM_WIDTH as i32 || y + h > VRAM_HEIGHT as i32 {
            return Err(Error::Unsupported("fill crossing the VRAM edge"));
        }
        self.count_path("fill".to_owned(), false, false, false);
        fill_area(
            &mut self.vram.words,
            [x, y, x + w, y + h],
            command_color(words[0] & 0xff_ffff),
        );
        Ok(())
    }

    fn copy(&mut self, words: &[u32]) -> Result<()> {
        let coordinate = |word: u32| (sign_extend_11(word), sign_extend_11(word >> 16));
        let (sx, sy) = coordinate(words[1]);
        let (dx, dy) = coordinate(words[2]);
        let w = (((words[3] & 0xffff).wrapping_sub(1) & 0x3ff) + 1) as i32;
        let h = (((words[3] >> 16).wrapping_sub(1) & 0x1ff) + 1) as i32;
        let inside = |x: i32, y: i32| (0..1024).contains(&x) && (0..512).contains(&y);
        if !inside(sx, sy) || !inside(dx, dy) {
            return Err(Error::Unsupported("VRAM copy coordinates outside VRAM"));
        }
        self.count_path("copy".to_owned(), false, false, true);
        copy_rect(
            &mut self.vram.words,
            [sx, sy],
            [dx, dy],
            [w, h],
            self.state.set_mask,
            self.state.check_mask,
        );
        Ok(())
    }

    fn begin_upload(&mut self, words: &[u32]) -> Result<()> {
        let x = (words[1] & 0xffff) as usize % VRAM_WIDTH;
        let y = (words[1] >> 16) as usize % VRAM_HEIGHT;
        let width = ((words[2] & 0xffff).wrapping_sub(1) & 0x3ff) as usize + 1;
        let height = ((words[2] >> 16).wrapping_sub(1) & 0x1ff) as usize + 1;
        if x + width > VRAM_WIDTH || y + height > VRAM_HEIGHT {
            return Err(Error::Unsupported(
                "CPU-to-VRAM transfer crossing the VRAM edge",
            ));
        }
        self.count_path("upload".to_owned(), false, false, false);
        self.upload = Some(Upload {
            x,
            y,
            width,
            height,
            pixels: Vec::with_capacity(width * height + 1),
        });
        Ok(())
    }

    /// Uploads ignore mask set/check in the reference renderer.
    fn finish_upload(&mut self, upload: Upload) {
        for row in 0..upload.height {
            let to = (upload.y + row) * VRAM_WIDTH + upload.x;
            self.vram.words[to..to + upload.width]
                .copy_from_slice(&upload.pixels[row * upload.width..(row + 1) * upload.width]);
        }
    }
}
