//! The Display page's CRT preview: a game-sized picture (the background art
//! cropped to 4:3, averaged down to 320x240 and to 15-bit colour, as the
//! PlayStation would show it) drawn through the chosen bundled look by the
//! game's own shader code (`crate::present::shader`, librashader), on the
//! launcher's graphics card, shown by egui as a texture.
//!
//! A look is loaded when it is first shown (a fraction of a second); the
//! preview is drawn again only when the look or its glass changes.
use crate::movie_pack::RgbaImage;
use librashader_common::shader_features::ShaderFeatures;
use librashader_common::{Size, Viewport};
use librashader_runtime_wgpu::options::FrameOptionsWgpu;
use librashader_runtime_wgpu::{FilterChainWgpu, WgpuOutputView};
use std::path::{Path, PathBuf};

/// The game-sized picture.
pub const INPUT: (u32, u32) = (320, 240);
/// The preview's size in pixels.
pub const OUTPUT: (u32, u32) = (640, 480);
/// Frames drawn per preview (the looks' afterglow and noise settle).
const FRAMES: usize = 4;

/// What the preview shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Want {
    /// The picture through this preset.
    Crt(PathBuf),
    /// The plain picture (the CRT off).
    Plain,
}

/// The preview's textures and the loaded look.
pub struct Preview {
    input: wgpu::Texture,
    output: wgpu::Texture,
    chain: Option<(PathBuf, FilterChainWgpu)>,
    /// The egui textures of the plain picture and the CRT output.
    plain_id: egui::TextureId,
    crt_id: egui::TextureId,
    /// What was drawn last, and what it said.
    pub shown: Option<(Want, Result<egui::TextureId, String>)>,
    frames: usize,
}

impl Preview {
    /// The preview of `art` on `device`, registered with egui's `renderer`.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut egui_wgpu::Renderer,
        art: &RgbaImage,
    ) -> Self {
        let texture = |label, (width, height): (u32, u32), usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage,
                view_formats: &[],
            })
        };
        let input = texture(
            "ac3 launcher preview input",
            INPUT,
            wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
        );
        let output = texture(
            "ac3 launcher preview",
            OUTPUT,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        let picture = game_picture(art);
        queue.write_texture(
            input.as_image_copy(),
            &picture,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(INPUT.0 * 4),
                rows_per_image: Some(INPUT.1),
            },
            input.size(),
        );
        let view =
            |texture: &wgpu::Texture| texture.create_view(&wgpu::TextureViewDescriptor::default());
        let plain_id =
            renderer.register_native_texture(device, &view(&input), wgpu::FilterMode::Nearest);
        let crt_id =
            renderer.register_native_texture(device, &view(&output), wgpu::FilterMode::Linear);
        Self {
            input,
            output,
            chain: None,
            plain_id,
            crt_id,
            shown: None,
            frames: 0,
        }
    }

    /// Show `want`: draw it unless it is shown already. Loading a look
    /// blocks for a fraction of a second.
    pub fn show(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, want: &Want) {
        if self.shown.as_ref().is_some_and(|(shown, _)| shown == want) {
            return;
        }
        let result = match want {
            Want::Plain => Ok(self.plain_id),
            Want::Crt(preset) => self.draw(device, queue, preset).map(|()| self.crt_id),
        };
        if let Err(error) = &result {
            eprintln!("ac3: launcher: preview: {error}");
        }
        self.shown = Some((want.clone(), result));
    }

    fn draw(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        preset: &Path,
    ) -> Result<(), String> {
        if self
            .chain
            .as_ref()
            .is_none_or(|(loaded, _)| loaded != preset)
        {
            let chain =
                FilterChainWgpu::load_from_path(preset, ShaderFeatures::NONE, device, queue, None)
                    .map_err(|e| format!("{}: {e}", preset.display()))?;
            self.chain = Some((preset.to_owned(), chain));
        }
        let (_, chain) = self.chain.as_mut().expect("loaded above");
        let view = self
            .output
            .create_view(&wgpu::TextureViewDescriptor::default());
        let size = Size::new(OUTPUT.0, OUTPUT.1);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        for _ in 0..FRAMES {
            let viewport = Viewport {
                x: 0.0,
                y: 0.0,
                mvp: None,
                output: WgpuOutputView::new_from_raw(&view, size, wgpu::TextureFormat::Rgba8Unorm),
                size,
            };
            let options = FrameOptionsWgpu {
                frame_direction: 1,
                total_subframes: 1,
                current_subframe: 1,
                ..Default::default()
            };
            chain
                .frame(
                    &self.input,
                    &viewport,
                    &mut encoder,
                    self.frames,
                    Some(&options),
                )
                .map_err(|e| e.to_string())?;
            self.frames += 1;
        }
        queue.submit([encoder.finish()]);
        Ok(())
    }
}

/// `art` as the game would show it: its middle 4:3 cropped, each of the
/// 320x240 pixels the mean of its area, at 15-bit colour (RGBA8 out).
pub fn game_picture(art: &RgbaImage) -> Vec<u8> {
    let (width, height) = (art.width as usize, art.height as usize);
    let crop_w = width.min(height * 4 / 3).max(1);
    let crop_h = (crop_w * 3 / 4).min(height).max(1);
    // The art's subject sits right of centre: crop from the right.
    let left = width - crop_w;
    let top = (height - crop_h) / 2;
    let (out_w, out_h) = (INPUT.0 as usize, INPUT.1 as usize);
    let mut out = Vec::with_capacity(out_w * out_h * 4);
    for y in 0..out_h {
        let (y0, y1) = (
            top + y * crop_h / out_h,
            top + ((y + 1) * crop_h / out_h).max(y * crop_h / out_h + 1),
        );
        for x in 0..out_w {
            let (x0, x1) = (
                left + x * crop_w / out_w,
                left + ((x + 1) * crop_w / out_w).max(x * crop_w / out_w + 1),
            );
            let mut sum = [0u32; 3];
            for sy in y0..y1.min(height) {
                for sx in x0..x1.min(width) {
                    let at = (sy * width + sx) * 4;
                    for (c, total) in sum.iter_mut().enumerate() {
                        *total += u32::from(art.pixels[at + c]);
                    }
                }
            }
            let count = ((y1.min(height) - y0) * (x1.min(width) - x0)).max(1) as u32;
            for channel in sum {
                // To 5 bits and back, as the PlayStation's 15-bit colour.
                let five = (channel / count) >> 3;
                out.push(((five << 3) | (five >> 2)) as u8);
            }
            out.push(255);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_art_becomes_a_game_sized_15_bit_picture() {
        // 640x480 of 2x2 blocks: each output pixel one block's mean.
        let art = RgbaImage {
            width: 640,
            height: 480,
            pixels: (0..480u32)
                .flat_map(|y| (0..640u32).map(move |x| (x, y)))
                .flat_map(|(x, y)| [(x / 2 % 256) as u8, (y / 2 % 256) as u8, 0xff, 255])
                .collect(),
        };
        let picture = game_picture(&art);
        assert_eq!(picture.len(), 320 * 240 * 4);
        let at = |x: usize, y: usize| &picture[(y * 320 + x) * 4..][..4];
        // Means 10 and 20 in 5-bit steps (8, 16); 255 stays 255.
        assert_eq!(at(10, 20), [8, 16, 0xff, 255]);
        // Means 63 and 239: steps 7 and 29, widened to 57 and 239.
        assert_eq!(at(319, 239), [57, 239, 0xff, 255]);
    }
}
