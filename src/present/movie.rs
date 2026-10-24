//! The presenter's movie layer: a movie pack's replacement image
//! ([`crate::movie_pack::MovieLayer`]) drawn over the movie frame's
//! rectangle of the picture, at the window's resolution, after the picture
//! and before the on-screen messages. Pixels the game drew over its movie
//! frame (the layer's mask) are left to the picture below. Presentation
//! only: the picture texture, the GPU renderer and the game are untouched.
use super::scale::Rect;
use crate::movie_pack::MovieLayer;

const SHADER: &str = include_str!("movie.wgsl");

/// Most bilinear taps per axis over a target pixel's footprint.
const MAX_TAPS: f32 = 4.0;

/// A texture and its size.
struct Texture {
    texture: wgpu::Texture,
    size: (u32, u32),
    generation: u64,
}

/// The movie layer's pipeline and textures.
pub struct MovieDraw {
    pipeline: wgpu::RenderPipeline,
    /// The same for a shader preset's 1x input (of `texel_format`).
    input_pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    params: wgpu::Buffer,
    texel_format: wgpu::TextureFormat,
    image: Option<Texture>,
    mask: Option<Texture>,
    /// A 1x1 mask texture for layers without a mask.
    no_mask: wgpu::Texture,
    bind_group: Option<wgpu::BindGroup>,
    /// The layer shown (its rectangle in the 1x picture and its mask).
    layer: Option<(MovieLayer, bool)>,
}

impl MovieDraw {
    /// For views of `target` format, with textures of `texel_format` (the
    /// picture's: sRGB exactly when the target is).
    pub fn new(
        device: &wgpu::Device,
        target: wgpu::TextureFormat,
        texel_format: wgpu::TextureFormat,
    ) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ac3 movie"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let texture = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float {
                    filterable: binding == 0,
                },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ac3 movie"),
            entries: &[
                texture(0),
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                texture(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ac3 movie"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = |format| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("ac3 movie"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleStrip,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some("fs_movie"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let input_pipeline = pipeline(texel_format);
        let pipeline = pipeline(target);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("ac3 movie"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ac3 movie"),
            size: 48,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let no_mask = mask_texture(device, (1, 1));
        Self {
            pipeline,
            input_pipeline,
            layout,
            sampler,
            params,
            texel_format,
            image: None,
            mask: None,
            no_mask,
            bind_group: None,
            layer: None,
        }
    }

    /// Show `layer` (`None`: nothing) from the next draw on, uploading its
    /// image and mask when they changed.
    pub fn set(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, layer: Option<&MovieLayer>) {
        let Some(layer) = layer else {
            self.layer = None;
            return;
        };
        let mut rebind = self.bind_group.is_none();
        let image = &layer.image;
        let size = (image.width, image.height);
        if self
            .image
            .as_ref()
            .is_none_or(|t| t.generation != layer.generation)
        {
            if self.image.as_ref().is_none_or(|t| t.size != size) {
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("ac3 movie image"),
                    size: extent(size),
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: self.texel_format,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                self.image = Some(Texture {
                    texture,
                    size,
                    generation: 0,
                });
                rebind = true;
            }
            let texture = self.image.as_mut().expect("created");
            write(queue, &texture.texture, &image.pixels, size, 4);
            texture.generation = layer.generation;
        }
        let rect_size = (layer.rect[2], layer.rect[3]);
        let has_mask = match &layer.mask {
            Some(mask) if mask.len() == (rect_size.0 * rect_size.1) as usize => {
                if self
                    .mask
                    .as_ref()
                    .is_none_or(|t| t.generation != layer.mask_generation)
                {
                    if self.mask.as_ref().is_none_or(|t| t.size != rect_size) {
                        self.mask = Some(Texture {
                            texture: mask_texture(device, rect_size),
                            size: rect_size,
                            generation: 0,
                        });
                        rebind = true;
                    }
                    let texture = self.mask.as_mut().expect("created");
                    write(queue, &texture.texture, mask, rect_size, 1);
                    texture.generation = layer.mask_generation;
                }
                true
            }
            _ => false,
        };
        if rebind {
            let image = self.image.as_ref().expect("uploaded");
            let mask = self.mask.as_ref().map_or(&self.no_mask, |t| &t.texture);
            let image_view = image.texture.create_view(&Default::default());
            let mask_view = mask.create_view(&Default::default());
            self.bind_group = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("ac3 movie"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&image_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&mask_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: self.params.as_entire_binding(),
                    },
                ],
            }));
        }
        self.layer = Some((layer.clone(), has_mask));
    }

    /// Whether a layer is shown.
    pub fn active(&self) -> bool {
        self.layer.is_some()
    }

    /// Where the movie goes for a picture drawn at `picture` (target
    /// pixels) from a `frame`-sized 1x picture, and its shader parameters.
    /// `None`: no layer, or the layer was made for another picture size.
    pub fn placement(&self, picture: Rect, frame: (u32, u32)) -> Option<([f32; 4], [f32; 12])> {
        let (layer, has_mask) = self.layer.as_ref()?;
        if layer.picture != frame || frame.0 == 0 || frame.1 == 0 {
            return None;
        }
        let sx = picture.width as f32 / frame.0 as f32;
        let sy = picture.height as f32 / frame.1 as f32;
        let [x, y, w, h] = layer.rect.map(|v| v as f32);
        let target = [
            picture.x as f32 + x * sx,
            picture.y as f32 + y * sy,
            w * sx,
            h * sy,
        ];
        if target[2] < 1.0 || target[3] < 1.0 {
            return None;
        }
        // The image's share of the rectangle, keeping its shape.
        let (iw, ih) = (layer.image.width as f32, layer.image.height as f32);
        let (fx, fy) = if iw * target[3] >= ih * target[2] {
            (1.0, (ih * target[2]) / (iw * target[3]))
        } else {
            ((iw * target[3]) / (ih * target[2]), 1.0)
        };
        // Image texels per target pixel, and the taps that cover them.
        let ratio = [iw / (target[2] * fx), ih / (target[3] * fy)];
        let taps = ratio.map(|r| r.ceil().clamp(1.0, MAX_TAPS));
        let step = [
            1.0 / (target[2] * fx) / taps[0],
            1.0 / (target[3] * fy) / taps[1],
        ];
        let mask = if *has_mask {
            [layer.rect[2] as f32, layer.rect[3] as f32]
        } else {
            [0.0, 0.0]
        };
        let params = [
            (1.0 - fx) / 2.0,
            (1.0 - fy) / 2.0,
            1.0 / fx,
            1.0 / fy,
            taps[0],
            taps[1],
            step[0],
            step[1],
            mask[0],
            mask[1],
            0.0,
            0.0,
        ];
        Some((target, params))
    }

    /// Write the parameters for a draw at `picture` from a `frame` picture;
    /// returns the viewport to draw into. Call before the render pass.
    pub fn prepare(
        &self,
        queue: &wgpu::Queue,
        picture: Rect,
        frame: (u32, u32),
    ) -> Option<[f32; 4]> {
        let (target, params) = self.placement(picture, frame)?;
        let bytes: Vec<u8> = params.iter().flat_map(|v| v.to_le_bytes()).collect();
        queue.write_buffer(&self.params, 0, &bytes);
        Some(target)
    }

    /// Draw into `viewport` (from [`Self::prepare`]) of a target view.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, viewport: [f32; 4]) {
        self.draw_with(pass, viewport, &self.pipeline);
    }

    /// Draw into `viewport` of a shader preset's 1x input (a texture of the
    /// texel format; `super::shader`).
    pub fn draw_input(&self, pass: &mut wgpu::RenderPass<'_>, viewport: [f32; 4]) {
        self.draw_with(pass, viewport, &self.input_pipeline);
    }

    fn draw_with(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        viewport: [f32; 4],
        pipeline: &wgpu::RenderPipeline,
    ) {
        let Some(bind_group) = &self.bind_group else {
            return;
        };
        pass.set_viewport(viewport[0], viewport[1], viewport[2], viewport[3], 0.0, 1.0);
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        pass.draw(0..4, 0..1);
    }
}

fn extent((width, height): (u32, u32)) -> wgpu::Extent3d {
    wgpu::Extent3d {
        width: width.max(1),
        height: height.max(1),
        depth_or_array_layers: 1,
    }
}

fn mask_texture(device: &wgpu::Device, size: (u32, u32)) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("ac3 movie mask"),
        size: extent(size),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

/// Upload `bytes` (`texel` bytes a pixel) into the whole `texture`.
fn write(queue: &wgpu::Queue, texture: &wgpu::Texture, bytes: &[u8], size: (u32, u32), texel: u32) {
    queue.write_texture(
        texture.as_image_copy(),
        bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(size.0 * texel),
            rows_per_image: Some(size.1),
        },
        extent(size),
    );
}

#[cfg(test)]
mod tests {
    //! Offscreen: the movie layer over a known picture, read back. Skips
    //! without an adapter unless `AC3_REQUIRE_GPU` is set.
    use super::*;
    use crate::movie_pack::RgbaImage;
    use crate::present::gpu::Renderer;
    use crate::settings::{Filter, Scaling};
    use std::sync::{Arc, Mutex, OnceLock};

    fn device() -> Option<&'static (wgpu::Device, wgpu::Queue)> {
        static DEVICE: OnceLock<Option<(wgpu::Device, wgpu::Queue)>> = OnceLock::new();
        DEVICE
            .get_or_init(|| {
                let descriptor = wgpu::InstanceDescriptor {
                    backends: wgpu::Backends::PRIMARY.with_env(),
                    ..wgpu::InstanceDescriptor::new_without_display_handle_from_env()
                };
                let instance = wgpu::Instance::new(descriptor);
                let adapter =
                    pollster::block_on(instance.request_adapter(&Default::default())).ok()?;
                pollster::block_on(adapter.request_device(&Default::default())).ok()
            })
            .as_ref()
            .or_else(|| {
                assert!(
                    std::env::var_os("AC3_REQUIRE_GPU").is_none(),
                    "AC3_REQUIRE_GPU: no wgpu device"
                );
                None
            })
    }

    /// Draw `renderer` into a `width` x `height` RGBA8 texture, read back as
    /// 0RGB words.
    fn draw(renderer: &Renderer, width: u32, height: u32) -> Vec<u32> {
        let device = &renderer.device;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: extent((width, height)),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        renderer.draw(&texture.create_view(&Default::default()), (width, height));
        let row = (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: u64::from(row * height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(height),
                },
            },
            extent((width, height)),
        );
        renderer.queue.submit([encoder.finish()]);
        buffer.map_async(wgpu::MapMode::Read, .., |result| result.unwrap());
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let bytes = buffer.get_mapped_range(..).unwrap();
        (0..height as usize)
            .flat_map(|y| (0..width as usize).map(move |x| (x, y)))
            .map(|(x, y)| {
                let at = y * row as usize + x * 4;
                (u32::from(bytes[at]) << 16)
                    | (u32::from(bytes[at + 1]) << 8)
                    | u32::from(bytes[at + 2])
            })
            .collect()
    }

    fn image(width: u32, height: u32, rgb: impl Fn(u32, u32) -> u32) -> RgbaImage {
        let pixels = (0..height)
            .flat_map(|y| (0..width).map(move |x| (x, y)))
            .flat_map(|(x, y)| {
                let p = rgb(x, y);
                [(p >> 16) as u8, (p >> 8) as u8, p as u8, 255]
            })
            .collect();
        RgbaImage {
            width,
            height,
            pixels,
        }
    }

    fn layer(image: RgbaImage, mask: Option<Vec<u8>>, generation: u64) -> MovieLayer {
        MovieLayer {
            image: Arc::new(image),
            generation,
            picture: (8, 6),
            rect: [0, 2, 8, 2],
            mask: mask.map(Arc::new),
            mask_generation: generation,
        }
    }

    /// One GPU test at a time.
    static ONE: Mutex<()> = Mutex::new(());

    #[test]
    fn the_movie_layer_replaces_the_frame_under_the_mask() {
        let _gpu = ONE.lock().unwrap_or_else(|e| e.into_inner());
        let Some((device, queue)) = device() else {
            return;
        };
        let mut renderer = Renderer::new(
            device.clone(),
            queue.clone(),
            wgpu::TextureFormat::Rgba8Unorm,
            Scaling::Stretch,
            Filter::Nearest,
        );
        // An 8x6 picture of distinct pixels; the movie frame is rows 2..4.
        let picture: Vec<u32> = (0..48u32)
            .map(|i| (i * 5) << 16 | (200 - i * 3) << 8 | i)
            .collect();
        renderer.set_picture(&picture, 8, 6);
        // The frame's own pixels at 1x: the same picture.
        let own = image(8, 2, |x, y| picture[((y + 2) * 8 + x) as usize]);
        renderer.set_movie(Some(&layer(own, None, 1)));
        assert_eq!(draw(&renderer, 8, 6), picture, "identity at 1x");
        // A 4x image drawn at 4x (32x24): its pixels, the mask's pixel
        // (1, 0) keeps the picture's.
        let big = image(32, 8, |x, y| 0x00ff_0000 | (x << 8) | y);
        let mut mask = vec![0u8; 16];
        mask[1] = 255;
        renderer.set_movie(Some(&layer(big, Some(mask), 2)));
        let out = draw(&renderer, 32, 24);
        let at = |x: usize, y: usize| out[y * 32 + x];
        assert_eq!(at(0, 8), 0x00ff_0000, "image (0, 0)");
        assert_eq!(at(10, 13), 0x00ff_0000 | (10 << 8) | 5);
        assert_eq!(at(5, 9), picture[2 * 8 + 1], "masked: the picture");
        assert_eq!(at(3, 3), picture[0], "above the frame: the picture");
        // A wider image is fitted with black above and below it.
        let wide = image(64, 8, |_, _| 0x0000_ff00);
        renderer.set_movie(Some(&layer(wide, None, 3)));
        let out = draw(&renderer, 32, 24);
        assert_eq!(out[8 * 32 + 16], 0, "bar above the fitted image");
        assert_eq!(out[12 * 32 + 16], 0x0000_ff00);
        // None: the picture alone.
        renderer.set_movie(None);
        assert_eq!(draw(&renderer, 8, 6), picture);
    }
    #[test]
    fn a_shader_preset_draws_the_movie_too() {
        let _gpu = ONE.lock().unwrap_or_else(|e| e.into_inner());
        let Some((device, queue)) = device() else {
            return;
        };
        let mut renderer = Renderer::new(
            device.clone(),
            queue.clone(),
            wgpu::TextureFormat::Rgba8Unorm,
            Scaling::Stretch,
            Filter::Nearest,
        );
        let picture: Vec<u32> = (0..48u32)
            .map(|i| (i * 5) << 16 | (200 - i * 3) << 8 | i)
            .collect();
        renderer.set_picture(&picture, 8, 6);
        // A 4x image over rows 2..4, the mask's pixel (1, 0) the game's.
        let big = image(32, 8, |x, y| 0x00ff_0000 | (x << 8) | (y * 30));
        let mut mask = vec![0u8; 16];
        mask[1] = 255;
        renderer.set_movie(Some(&layer(big, Some(mask), 1)));
        // The movie filtered down into the 1x frame, as the preset's input.
        let one_x = draw(&renderer, 8, 6);
        assert_eq!(one_x[2 * 8 + 1], picture[2 * 8 + 1], "masked: the game's");
        assert_ne!(one_x[3 * 8 + 4], picture[3 * 8 + 4], "the movie's");
        // Through a passthrough preset at 4x: that input, each pixel 4x4.
        let preset = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/shaders/passthrough.slangp");
        assert!(
            renderer
                .set_shader(Some(&preset))
                .unwrap()
                .contains("loaded")
        );
        let out = draw(&renderer, 32, 24);
        for y in 0..24 {
            for x in 0..32 {
                assert_eq!(out[y * 32 + x], one_x[(y / 4) * 8 + x / 4], "pixel {x},{y}");
            }
        }
        assert_eq!(renderer.take_shader_note(), None);
        // F2 off: the movie at the window's size again.
        assert!(renderer.toggle_shader().unwrap().ends_with("off"));
        let plain = draw(&renderer, 32, 24);
        assert_eq!(plain[13 * 32 + 10], 0x00ff_0000 | (10 << 8) | (5 * 30));
    }
}
