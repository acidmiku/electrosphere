//! The wgpu side of presentation: the scaling pipeline, the picture and
//! message textures, and drawing into a target view (the window's surface,
//! or an offscreen texture in the tests). A shader preset
//! ([`super::shader`]), when one is set and on, draws the picture instead
//! of the scaling pipeline.
use super::osd::Image;
use super::scale::{self, Rect};
use super::shader::Shader;
use crate::settings::{Filter, Scaling};
use std::cell::RefCell;

const SHADER: &str = include_str!("scale.wgsl");

/// A texture with its bind group.
struct Layer {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    width: u32,
    height: u32,
    /// The size the scaling lays out: the texture's, or the 1x frame of a
    /// GPU renderer picture drawn at N x.
    frame: (u32, u32),
    /// The GPU renderer picture it shows (its generation), if any.
    source: Option<u64>,
}

/// Upload `rgba` (4 bytes a pixel, the layer's whole size) into `layer`.
fn write(queue: &wgpu::Queue, layer: &Layer, rgba: &[u8]) {
    queue.write_texture(
        layer.texture.as_image_copy(),
        rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(layer.width * 4),
            rows_per_image: Some(layer.height),
        },
        wgpu::Extent3d {
            width: layer.width,
            height: layer.height,
            depth_or_array_layers: 1,
        },
    );
}

/// Draws the game picture, scaled into the target, and the messages.
pub struct Renderer {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    /// Format of the textures: sRGB exactly when the target is, so the
    /// picture's values reach the target unchanged.
    texel_format: wgpu::TextureFormat,
    layout: wgpu::BindGroupLayout,
    picture_pipeline: wgpu::RenderPipeline,
    overlay_pipeline: wgpu::RenderPipeline,
    picture_sampler: wgpu::Sampler,
    overlay_sampler: wgpu::Sampler,
    pub scaling: Scaling,
    /// The shape the picture is shown at: [`scale::FOUR_BY_THREE`], or a
    /// widescreen aspect while flight is drawn wide.
    pub aspect: (u32, u32),
    picture: Option<Layer>,
    overlay: Option<Layer>,
    staging: Vec<u8>,
    /// The format of the views drawn into (for the movie layer).
    target: wgpu::TextureFormat,
    /// A movie pack's image over the movie frame (`super::movie`), made at
    /// its first use.
    movie: Option<super::movie::MovieDraw>,
    /// The shader preset (`[display] shader_preset`), if one is set; its
    /// chain draws from [`draw`](Self::draw), which takes `&self`.
    shader: RefCell<Option<Shader>>,
    /// Why the last draw left the shader preset (for the screen), once.
    shader_note: RefCell<Option<String>>,
    /// The shader preset's output (the picture rectangle's size) and its
    /// bind group.
    shaded: RefCell<Option<(wgpu::Texture, wgpu::BindGroup)>>,
    /// Pictures shown so far (the shader's FrameCount): a software frame
    /// or a new GPU renderer picture, not a redraw.
    frames: usize,
}

impl Renderer {
    /// `target`: the format of the views [`draw`](Self::draw) renders into.
    pub fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        target: wgpu::TextureFormat,
        scaling: Scaling,
        filter: Filter,
    ) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ac3 scale"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ac3 picture"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ac3 scale"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = |entry: &str, blend: Option<wgpu::BlendState>| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(entry),
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
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: target,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let picture_pipeline = pipeline("fs_picture", None);
        let overlay_pipeline = pipeline("fs_overlay", Some(wgpu::BlendState::ALPHA_BLENDING));
        let sampler = |mode| {
            device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("ac3 picture"),
                mag_filter: mode,
                min_filter: mode,
                ..Default::default()
            })
        };
        let picture_sampler = sampler(match filter {
            Filter::Nearest => wgpu::FilterMode::Nearest,
            Filter::Linear => wgpu::FilterMode::Linear,
        });
        let overlay_sampler = sampler(wgpu::FilterMode::Nearest);
        let texel_format = if target.is_srgb() {
            wgpu::TextureFormat::Rgba8UnormSrgb
        } else {
            wgpu::TextureFormat::Rgba8Unorm
        };
        Self {
            device,
            queue,
            texel_format,
            layout,
            picture_pipeline,
            overlay_pipeline,
            picture_sampler,
            overlay_sampler,
            scaling,
            aspect: scale::FOUR_BY_THREE,
            picture: None,
            overlay: None,
            staging: Vec::new(),
            target,
            movie: None,
            shader: RefCell::new(None),
            shader_note: RefCell::new(None),
            shaded: RefCell::new(None),
            frames: 0,
        }
    }

    /// Draw the picture through the shader preset at `path` from the next
    /// draw on (`None`: the scaling pipeline). Returns what happened.
    pub fn set_shader(&mut self, path: Option<&std::path::Path>) -> Option<String> {
        let shader = path.map(|path| Shader::load(&self.device, &self.queue, path));
        let note = shader.as_ref().map(|(_, note)| note.clone());
        *self.shader.get_mut() = shader.map(|(shader, _)| shader);
        note
    }

    /// F2: the shader preset off or on; `None` without one.
    pub fn toggle_shader(&mut self) -> Option<String> {
        let shader = self.shader.get_mut().as_mut()?;
        shader.enabled = !shader.enabled;
        Some(match (shader.enabled, shader.active()) {
            (false, _) => format!("Shader {} off", shader.name()),
            (true, true) => format!("Shader {} on", shader.name()),
            (true, false) => format!(
                "Shader {} is not loaded (Shift+F2 loads it again)",
                shader.name()
            ),
        })
    }

    /// Shift+F2: load the shader preset again (and turn it on); `None`
    /// without one.
    pub fn reload_shader(&mut self) -> Option<String> {
        let shader = self.shader.get_mut().as_mut()?;
        shader.enabled = true;
        Some(shader.reload(&self.device, &self.queue))
    }

    /// Whether a shader preset is set and turned on (loaded or not).
    pub fn shader_enabled(&self) -> bool {
        self.shader
            .borrow()
            .as_ref()
            .is_some_and(|shader| shader.enabled)
    }

    /// Whether a shader preset is loaded and on.
    pub fn shader_loaded(&self) -> bool {
        self.shader.borrow().as_ref().is_some_and(Shader::active)
    }

    /// Why the last draw left the shader preset, once.
    pub fn take_shader_note(&mut self) -> Option<String> {
        self.shader_note.get_mut().take()
    }

    /// Draw `layer` (a movie pack's image) over the movie frame of the
    /// picture from the next draw on; `None`: none.
    pub fn set_movie(&mut self, layer: Option<&crate::movie_pack::MovieLayer>) {
        if layer.is_none() && self.movie.is_none() {
            return;
        }
        let movie = self.movie.get_or_insert_with(|| {
            super::movie::MovieDraw::new(&self.device, self.target, self.texel_format)
        });
        movie.set(&self.device, &self.queue, layer);
    }

    /// Size of the current picture's texture.
    pub fn picture_size(&self) -> Option<(u32, u32)> {
        self.picture.as_ref().map(|p| (p.width, p.height))
    }

    /// Upload the game picture: `width * height` 0RGB words, row by row.
    pub fn set_picture(&mut self, rgb: &[u32], width: usize, height: usize) {
        if width == 0 || height == 0 || rgb.len() < width * height {
            return;
        }
        self.staging.clear();
        self.staging.extend(
            rgb[..width * height]
                .iter()
                .flat_map(|&p| [(p >> 16) as u8, (p >> 8) as u8, p as u8, 0xff]),
        );
        if self
            .picture
            .as_ref()
            .is_some_and(|layer| layer.source.is_some())
        {
            self.picture = None;
        }
        self.prepare(false, width as u32, height as u32);
        if let Some(layer) = &self.picture {
            write(&self.queue, layer, &self.staging);
        }
        self.frames = self.frames.wrapping_add(1);
    }

    /// Show `texture` (the GPU renderer's picture, of this renderer's texel
    /// format and `generation`) as the game picture, laid out as a `frame`
    /// picture (its 1x size).
    pub fn set_picture_texture(
        &mut self,
        texture: &wgpu::Texture,
        frame: (u32, u32),
        generation: u64,
    ) {
        if let Some(layer) = &mut self.picture
            && layer.source == Some(generation)
        {
            layer.frame = frame;
            return;
        }
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.bind(&view, false);
        self.picture = Some(Layer {
            texture: texture.clone(),
            bind_group,
            width: texture.width(),
            height: texture.height(),
            frame,
            source: Some(generation),
        });
        self.frames = self.frames.wrapping_add(1);
    }

    /// The texel format pictures must have.
    pub fn texel_format(&self) -> wgpu::TextureFormat {
        self.texel_format
    }

    fn bind(&self, view: &wgpu::TextureView, overlay: bool) -> wgpu::BindGroup {
        let sampler = if overlay {
            &self.overlay_sampler
        } else {
            &self.picture_sampler
        };
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ac3 picture"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        })
    }

    /// Upload the messages image, or clear it.
    pub fn set_overlay(&mut self, image: Option<&Image>) {
        match image.filter(|i| i.width > 0 && i.height > 0) {
            Some(image) => {
                self.prepare(true, image.width, image.height);
                if let Some(layer) = &self.overlay {
                    write(&self.queue, layer, &image.rgba);
                }
            }
            None => self.overlay = None,
        }
    }

    /// (Re)create the picture (`overlay` false) or messages layer when its
    /// size changes.
    fn prepare(&mut self, overlay: bool, width: u32, height: u32) {
        let current = if overlay {
            &self.overlay
        } else {
            &self.picture
        };
        if current
            .as_ref()
            .is_none_or(|l| (l.width, l.height) != (width, height))
        {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(if overlay {
                    "ac3 messages"
                } else {
                    "ac3 picture"
                }),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.texel_format,
                // COPY_SRC: a shader preset's history copies the picture.
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind_group = self.bind(&view, overlay);
            let layer = Some(Layer {
                texture,
                bind_group,
                width,
                height,
                frame: (width, height),
                source: None,
            });
            if overlay {
                self.overlay = layer;
            } else {
                self.picture = layer;
            }
        }
    }

    /// Where the picture and the messages go on a `size` target.
    pub fn layout(&self, size: (u32, u32)) -> (Option<Rect>, Option<Rect>) {
        let picture = self
            .picture
            .as_ref()
            .map(|p| scale::picture_rect(size, p.frame, self.scaling, self.aspect))
            .filter(|r| !r.is_empty());
        let overlay = self.overlay.as_ref().and_then(|o| {
            // Top left, a whole number of target pixels per font pixel,
            // smaller if the target is too small for it.
            (1..=scale::osd_scale(size.1)).rev().find_map(|s| {
                let rect = Rect {
                    x: 4 * s,
                    y: 4 * s,
                    width: o.width * s,
                    height: o.height * s,
                };
                (rect.x + rect.width <= size.0 && rect.y + rect.height <= size.1).then_some(rect)
            })
        });
        (picture, overlay)
    }

    /// Clear `target` (of `size` pixels) to black, then draw the picture (by
    /// the shader preset when one is on) and the messages, and submit.
    pub fn draw(&self, target: &wgpu::TextureView, size: (u32, u32)) {
        let (picture_rect, overlay_rect) = self.layout(size);
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("ac3 present"),
            });
        // The shader preset's picture (the movie layer included), at the
        // picture's size on the target.
        let shaded = picture_rect.and_then(|rect| self.draw_shader(&mut encoder, rect));
        // Else the movie layer's viewport, over the picture (`None`: none).
        // One layer draw a frame: its parameters are written for it.
        let movie = self
            .movie
            .as_ref()
            .zip(picture_rect)
            .filter(|_| shaded.is_none())
            .and_then(|(movie, rect)| {
                let frame = self.picture.as_ref()?.frame;
                Some((movie, movie.prepare(&self.queue, rect, frame)?))
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ac3 present"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            let picture = self
                .picture
                .as_ref()
                .map(|layer| shaded.as_ref().unwrap_or(&layer.bind_group));
            let overlay = self.overlay.as_ref().map(|layer| &layer.bind_group);
            let layers = [
                (picture, picture_rect, &self.picture_pipeline),
                (overlay, overlay_rect, &self.overlay_pipeline),
            ];
            for (index, (bind_group, rect, pipeline)) in layers.into_iter().enumerate() {
                // The movie over the picture, under the messages.
                if index == 1
                    && let Some((movie, viewport)) = movie
                {
                    movie.draw(&mut pass, viewport);
                }
                if let (Some(bind_group), Some(rect)) = (bind_group, rect) {
                    pass.set_viewport(
                        rect.x as f32,
                        rect.y as f32,
                        rect.width as f32,
                        rect.height as f32,
                        0.0,
                        1.0,
                    );
                    pass.set_pipeline(pipeline);
                    pass.set_bind_group(0, bind_group, &[]);
                    pass.draw(0..4, 0..1);
                }
            }
        }
        self.queue.submit([encoder.finish()]);
    }

    /// With a shader preset on: record the chain drawing the picture into a
    /// texture of `rect`'s size (the chain clears all of its output), and
    /// return that texture's bind group for the picture's place. `None`: no
    /// preset on, or a failure (which leaves a note and the scaling
    /// pipeline).
    fn draw_shader(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        rect: Rect,
    ) -> Option<wgpu::BindGroup> {
        let mut shader = self.shader.borrow_mut();
        let shader = shader.as_mut().filter(|shader| shader.active())?;
        let layer = self.picture.as_ref()?;
        let mut output = self.shaded.borrow_mut();
        if output.as_ref().is_none_or(|(texture, _)| {
            (texture.width(), texture.height()) != (rect.width, rect.height)
        }) {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("ac3 shader output"),
                size: wgpu::Extent3d {
                    width: rect.width,
                    height: rect.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.texel_format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind_group = self.bind(&view, false);
            *output = Some((texture, bind_group));
        }
        let (texture, bind_group) = output.as_ref().expect("made above");
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        // The movie layer goes into the chain's 1x input.
        let (width, height) = layer.frame;
        let whole = Rect {
            x: 0,
            y: 0,
            width,
            height,
        };
        let movie = self
            .movie
            .as_ref()
            .and_then(|movie| Some((movie, movie.prepare(&self.queue, whole, layer.frame)?)));
        let drawn = shader.draw(
            &self.device,
            &self.queue,
            encoder,
            (&layer.texture, layer.frame, movie),
            (&view, self.texel_format, (rect.width, rect.height)),
            Rect {
                x: 0,
                y: 0,
                width: rect.width,
                height: rect.height,
            },
            self.frames,
        );
        match drawn {
            Ok(()) => Some(bind_group.clone()),
            Err(note) => {
                *self.shader_note.borrow_mut() = Some(note);
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! Offscreen: draw known pictures through the scaler and read the
    //! pixels back. Uses one shared device on any Vulkan/Metal/DX12 adapter
    //! (whatever adapter wgpu picks), one test at a time; skips when there is
    //! none unless `AC3_REQUIRE_GPU` is set.
    use super::*;
    use crate::present::osd;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    /// The device, created once: several instances set up in parallel
    /// threads can deadlock in the drivers.
    fn device() -> Option<&'static (wgpu::Device, wgpu::Queue)> {
        static DEVICE: OnceLock<Option<(wgpu::Device, wgpu::Queue)>> = OnceLock::new();
        DEVICE
            .get_or_init(|| {
                let descriptor = wgpu::InstanceDescriptor {
                    backends: wgpu::Backends::PRIMARY.with_env(),
                    ..wgpu::InstanceDescriptor::new_without_display_handle_from_env()
                };
                let instance = wgpu::Instance::new(descriptor);
                let adapter = pollster::block_on(instance.request_adapter(&Default::default()));
                let adapter = match adapter {
                    Ok(adapter) => adapter,
                    Err(error) => {
                        eprintln!("skipped: no wgpu adapter ({error})");
                        return None;
                    }
                };
                let info = adapter.get_info();
                eprintln!("offscreen adapter: {} ({})", info.name, info.backend);
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

    /// One GPU test at a time.
    fn exclusive() -> MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn renderer(scaling: Scaling, filter: Filter) -> Option<Renderer> {
        let (device, queue) = device()?;
        Some(Renderer::new(
            device.clone(),
            queue.clone(),
            wgpu::TextureFormat::Rgba8Unorm,
            scaling,
            filter,
        ))
    }

    /// Draw into a `width` x `height` RGBA8 texture and read it back.
    fn draw(renderer: &Renderer, width: u32, height: u32) -> Vec<[u8; 4]> {
        let device = &renderer.device;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        renderer.draw(&view, (width, height));
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
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        renderer.queue.submit([encoder.finish()]);
        buffer.map_async(wgpu::MapMode::Read, .., |result| result.unwrap());
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let bytes = buffer.get_mapped_range(..).unwrap();
        (0..height as usize)
            .flat_map(|y| (0..width as usize).map(move |x| (x, y)))
            .map(|(x, y)| {
                let at = y * row as usize + x * 4;
                [bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]
            })
            .collect()
    }

    /// A 4x3 picture of distinct colors.
    fn pattern() -> Vec<u32> {
        (0..12u32)
            .map(|i| ((i * 20) << 16) | ((255 - i * 20) << 8) | (i * 7 + 3))
            .collect()
    }
    fn rgba(p: u32) -> [u8; 4] {
        [(p >> 16) as u8, (p >> 8) as u8, p as u8, 255]
    }

    #[test]
    fn integer_nearest_repeats_each_pixel_exactly() {
        let _gpu = exclusive();
        let Some(mut renderer) = renderer(Scaling::Integer, Filter::Nearest) else {
            return;
        };
        let picture = pattern();
        renderer.set_picture(&picture, 4, 3);
        // 64x48 is exactly 16x.
        let pixels = draw(&renderer, 64, 48);
        for y in 0..48 {
            for x in 0..64 {
                let expected = rgba(picture[(y / 16) * 4 + x / 16]);
                assert_eq!(pixels[y * 64 + x], expected, "pixel {x},{y}");
            }
        }
        // 72x52: 17x both ways (68x51), centered at (2, 0).
        let pixels = draw(&renderer, 72, 52);
        let rect = renderer.layout((72, 52)).0.unwrap();
        assert_eq!(
            rect,
            Rect {
                x: 2,
                y: 0,
                width: 68,
                height: 51
            }
        );
        for y in 0..52u32 {
            for x in 0..72u32 {
                let expected = if rect.contains(x, y) {
                    let (px, py) = ((x - rect.x) / 17, (y - rect.y) / 17);
                    rgba(picture[(py * 4 + px) as usize])
                } else {
                    [0, 0, 0, 255]
                };
                assert_eq!(pixels[(y * 72 + x) as usize], expected, "pixel {x},{y}");
            }
        }
    }

    #[test]
    fn aspect_letterboxes_and_stretch_fills() {
        let _gpu = exclusive();
        let Some(mut renderer) = renderer(Scaling::Aspect, Filter::Nearest) else {
            return;
        };
        let picture = pattern();
        renderer.set_picture(&picture, 4, 3);
        // 128x48: a 64x48 picture with 32-pixel black bars on each side.
        let pixels = draw(&renderer, 128, 48);
        for y in 0..48 {
            for x in 0..128 {
                let expected = if (32..96).contains(&x) {
                    rgba(picture[(y / 16) * 4 + (x - 32) / 16])
                } else {
                    [0, 0, 0, 255]
                };
                assert_eq!(pixels[y * 128 + x], expected, "pixel {x},{y}");
            }
        }
        renderer.scaling = Scaling::Stretch;
        let pixels = draw(&renderer, 128, 48);
        for y in 0..48 {
            for x in 0..128 {
                assert_eq!(
                    pixels[y * 128 + x],
                    rgba(picture[(y / 16) * 4 + x / 32]),
                    "pixel {x},{y}"
                );
            }
        }
        // A new size replaces the texture.
        renderer.set_picture(&[0x00ff_0000, 0x0000_00ff], 2, 1);
        assert_eq!(renderer.picture_size(), Some((2, 1)));
        let pixels = draw(&renderer, 64, 4);
        assert_eq!(pixels[0], [255, 0, 0, 255]);
        assert_eq!(pixels[63], [0, 0, 255, 255]);
    }

    #[test]
    fn widescreen_pictures_fill_their_shape() {
        let _gpu = exclusive();
        let Some(mut renderer) = renderer(Scaling::Aspect, Filter::Nearest) else {
            return;
        };
        let picture = pattern();
        renderer.set_picture(&picture, 4, 3);
        // 4:3 in a 64x36 (16:9) target: 48 wide with 8-pixel side bars.
        let rect = renderer.layout((64, 36)).0.unwrap();
        assert_eq!((rect.x, rect.width), (8, 48));
        // At 16:9 the same picture fills it, each pixel 16x12.
        renderer.aspect = (16, 9);
        assert_eq!(
            renderer.layout((64, 36)).0.unwrap(),
            Rect {
                x: 0,
                y: 0,
                width: 64,
                height: 36
            }
        );
        let pixels = draw(&renderer, 64, 36);
        for y in 0..36 {
            for x in 0..64 {
                let expected = rgba(picture[(y / 12) * 4 + x / 16]);
                assert_eq!(pixels[y * 64 + x], expected, "pixel {x},{y}");
            }
        }
    }

    #[test]
    fn linear_filter_blends_neighbours() {
        let _gpu = exclusive();
        let Some(mut renderer) = renderer(Scaling::Stretch, Filter::Linear) else {
            return;
        };
        renderer.set_picture(&[0x0000_0000, 0x00ff_ffff], 2, 1);
        let pixels = draw(&renderer, 64, 4);
        let red: Vec<u8> = pixels[..64].iter().map(|p| p[0]).collect();
        assert_eq!(red[0], 0);
        assert_eq!(red[63], 255);
        assert!(red.windows(2).all(|w| w[0] <= w[1]), "{red:?}");
        assert!(red[24..40].iter().all(|&v| v > 0 && v < 255), "{red:?}");
    }

    #[test]
    fn messages_draw_over_the_picture() {
        let _gpu = exclusive();
        let Some(mut renderer) = renderer(Scaling::Stretch, Filter::Nearest) else {
            return;
        };
        renderer.set_picture(&[0x0000_8000], 1, 1);
        let image = osd::rasterize(&[("Quick save".into(), 255)]);
        renderer.set_overlay(Some(&image));
        // 640 lines: two target pixels per font pixel, 8 pixels in.
        let (width, height) = (256, 640);
        let rect = renderer.layout((width, height)).1.unwrap();
        assert_eq!((rect.x, rect.y), (8, 8));
        assert_eq!(
            (rect.width, rect.height),
            (image.width * 2, image.height * 2)
        );
        let pixels = draw(&renderer, width, height);
        let at = |x: u32, y: u32| pixels[(y * width + x) as usize];
        for y in (rect.y..rect.y + rect.height).step_by(2) {
            for x in (rect.x..rect.x + rect.width).step_by(2) {
                let source = image.pixel((x - rect.x) / 2, (y - rect.y) / 2);
                let got = at(x, y);
                // Alpha blending over the green picture.
                let alpha = u32::from(source[3]);
                let blend = |s: u8, d: u32| (u32::from(s) * alpha + d * (255 - alpha)) / 255;
                let expected = [
                    blend(source[0], 0),
                    blend(source[1], 0x80),
                    blend(source[2], 0),
                ];
                for channel in 0..3 {
                    let difference = (i64::from(got[channel]) - i64::from(expected[channel])).abs();
                    assert!(difference <= 1, "pixel {x},{y}: {got:?} vs {expected:?}");
                }
            }
        }
        // White text is there; outside the box the picture is untouched.
        assert!((0..height).any(|y| (0..width).any(|x| at(x, y) == [255, 255, 255, 255])));
        assert_eq!(at(width - 1, height - 1), [0, 0x80, 0, 255]);
        renderer.set_overlay(None);
        assert_eq!(
            draw(&renderer, width, height)[(10 * width + 10) as usize],
            [0, 0x80, 0, 255]
        );
    }

    #[test]
    fn gpu_renderer_pictures_scale_by_their_1x_frame() {
        let _gpu = exclusive();
        let Some(mut renderer) = renderer(Scaling::Integer, Filter::Nearest) else {
            return;
        };
        // An 8x6 texture standing for a 4x3 frame drawn at 2x: integer
        // scaling lays out the 4x3 frame (16x in 64x48), so each texel
        // covers 8x8 target pixels.
        let picture: Vec<u32> = (0..48u32).map(|i| (i * 5) << 16 | (255 - i * 5)).collect();
        let device = renderer.device.clone();
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: 8,
                height: 6,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: renderer.texel_format(),
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let bytes: Vec<u8> = picture.iter().flat_map(|&p| rgba(p)).collect();
        renderer.queue.write_texture(
            texture.as_image_copy(),
            &bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(8 * 4),
                rows_per_image: Some(6),
            },
            wgpu::Extent3d {
                width: 8,
                height: 6,
                depth_or_array_layers: 1,
            },
        );
        renderer.set_picture_texture(&texture, (4, 3), 1);
        assert_eq!(renderer.picture_size(), Some((8, 6)));
        let pixels = draw(&renderer, 64, 48);
        for y in 0..48 {
            for x in 0..64 {
                assert_eq!(
                    pixels[y * 64 + x],
                    rgba(picture[(y / 8) * 8 + x / 8]),
                    "pixel {x},{y}"
                );
            }
        }
        // A software frame replaces it.
        renderer.set_picture(&[0x00ff_0000], 1, 1);
        assert_eq!(renderer.picture_size(), Some((1, 1)));
    }

    /// `got` against `wanted` (`width` pixels a row): how many pixels and
    /// which first differ.
    fn same(got: &[[u8; 4]], wanted: &[[u8; 4]], width: usize, what: &str) {
        assert_eq!(got.len(), wanted.len(), "{what}");
        let differing: Vec<_> = (0..got.len()).filter(|&i| got[i] != wanted[i]).collect();
        let first: Vec<String> = differing
            .iter()
            .take(6)
            .map(|&i| {
                format!(
                    "({},{}) {:?} for {:?}",
                    i % width,
                    i / width,
                    got[i],
                    wanted[i]
                )
            })
            .collect();
        assert!(
            differing.is_empty(),
            "{what}: {} of {} pixels differ: {}",
            differing.len(),
            got.len(),
            first.join(", ")
        );
    }

    /// `tests/shaders/passthrough.slangp`: one nearest pass, the picture as
    /// it is.
    fn passthrough() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/shaders/passthrough.slangp")
    }

    #[test]
    fn a_passthrough_preset_draws_as_the_scaler_does() {
        let _gpu = exclusive();
        let Some(mut renderer) = renderer(Scaling::Integer, Filter::Nearest) else {
            return;
        };
        let picture = pattern();
        renderer.set_picture(&picture, 4, 3);
        let plain = draw(&renderer, 72, 52);
        let note = renderer.set_shader(Some(&passthrough())).unwrap();
        assert!(note.contains("loaded"), "{note}");
        let shaded = draw(&renderer, 72, 52);
        same(&shaded, &plain, 72, "integer nearest through the chain");
        assert_eq!(renderer.take_shader_note(), None);
        // F2 off: the scaler; on again: the chain.
        assert!(renderer.toggle_shader().unwrap().ends_with("off"));
        same(&draw(&renderer, 72, 52), &plain, 72, "off");
        assert!(renderer.toggle_shader().unwrap().ends_with("on"));
        same(&draw(&renderer, 72, 52), &plain, 72, "on again");
        // Messages stay on top of the chain's picture.
        let image = osd::rasterize(&[("Quick save".into(), 255)]);
        renderer.set_overlay(Some(&image));
        let with_messages = draw(&renderer, 256, 192);
        renderer.set_shader(None);
        same(&draw(&renderer, 256, 192), &with_messages, 256, "messages");
    }

    #[test]
    fn the_preset_gets_the_gpu_picture_at_1x() {
        let _gpu = exclusive();
        let Some(mut renderer) = renderer(Scaling::Integer, Filter::Nearest) else {
            return;
        };
        // An 8x6 picture of a 4x3 frame at 2x: each 2x2 block averages to
        // a known 1x pixel (sums divisible by 4).
        let block = |i: u32| [i * 8, i * 8 + 4, i * 8 + 8, i * 8 + 12].map(|v| v.min(255));
        let mut bytes = vec![0u8; 8 * 6 * 4];
        let mut expected = Vec::new();
        for py in 0..3u32 {
            for px in 0..4u32 {
                let i = py * 4 + px;
                let values = block(i);
                for (k, (dx, dy)) in [(0, 0), (1, 0), (0, 1), (1, 1)].into_iter().enumerate() {
                    let at = (((py * 2 + dy) * 8 + px * 2 + dx) * 4) as usize;
                    let v = values[k] as u8;
                    bytes[at..at + 4].copy_from_slice(&[v, 255 - v, (i * 20) as u8, 255]);
                }
                let mean = values.iter().sum::<u32>() / 4;
                expected.push([mean as u8, (255 - mean) as u8, (i * 20) as u8, 255]);
            }
        }
        let device = renderer.device.clone();
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: 8,
                height: 6,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: renderer.texel_format(),
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        renderer.queue.write_texture(
            texture.as_image_copy(),
            &bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(8 * 4),
                rows_per_image: Some(6),
            },
            texture.size(),
        );
        renderer.set_picture_texture(&texture, (4, 3), 1);
        renderer.set_shader(Some(&passthrough())).unwrap();
        // 64x48: each 1x pixel 16x16.
        let pixels = draw(&renderer, 64, 48);
        for y in 0..48 {
            for x in 0..64 {
                assert_eq!(
                    pixels[y * 64 + x],
                    expected[(y / 16) * 4 + x / 16],
                    "pixel {x},{y}"
                );
            }
        }
    }

    /// A preview of a shader preset (`--ignored`): `$AC3_SHADER_PICTURE` (a
    /// PNG of the game's 1x picture, as headless runs save them) through
    /// `$AC3_SHADER_PRESET` at `$AC3_SHADER_SIZE` (default 1280x960),
    /// written to `$AC3_SHADER_OUT` (default `target/shader-preview.png`).
    #[test]
    #[ignore = "a preview: needs a picture and a preset"]
    fn shader_preview() {
        let _gpu = exclusive();
        let Some(mut renderer) = renderer(Scaling::Aspect, Filter::Nearest) else {
            return;
        };
        let var = |name: &str| std::env::var(name).ok();
        let picture = var("AC3_SHADER_PICTURE").expect("AC3_SHADER_PICTURE");
        let preset = var("AC3_SHADER_PRESET").expect("AC3_SHADER_PRESET");
        let (width, height) = var("AC3_SHADER_SIZE")
            .and_then(|size| {
                let (w, h) = size.split_once('x')?;
                Some((w.parse().ok()?, h.parse().ok()?))
            })
            .unwrap_or((1280u32, 960u32));
        let out = var("AC3_SHADER_OUT").unwrap_or_else(|| "target/shader-preview.png".into());
        let mut decoder = png::Decoder::new(std::io::BufReader::new(
            std::fs::File::open(&picture).unwrap(),
        ));
        decoder.set_transformations(png::Transformations::normalize_to_color8());
        let mut reader = decoder.read_info().unwrap();
        let mut buffer = vec![0; reader.output_buffer_size().unwrap()];
        let frame = reader.next_frame(&mut buffer).unwrap();
        let channels = frame.line_size / frame.width as usize;
        let rgb: Vec<u32> = buffer[..frame.line_size * frame.height as usize]
            .chunks(channels)
            .map(|p| u32::from(p[0]) << 16 | u32::from(p[1]) << 8 | u32::from(p[2]))
            .collect();
        renderer.set_picture(&rgb, frame.width as usize, frame.height as usize);
        let note = renderer.set_shader(Some(std::path::Path::new(&preset)));
        eprintln!("{}", note.unwrap_or_default());
        let started = std::time::Instant::now();
        let pixels = draw(&renderer, width, height);
        eprintln!("drawn in {:?}", started.elapsed());
        assert_eq!(renderer.take_shader_note(), None);
        let file = std::fs::File::create(&out).unwrap();
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(pixels.as_flattened()).unwrap();
        writer.finish().unwrap();
        eprintln!("{out}");
    }

    #[test]
    fn every_bundled_crt_look_loads_and_draws() {
        let _gpu = exclusive();
        let Some(mut renderer) = renderer(Scaling::Aspect, Filter::Nearest) else {
            return;
        };
        let picture = pattern();
        renderer.set_picture(&picture, 4, 3);
        let plain = draw(&renderer, 64, 48);
        let folder =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(super::super::crt::FOLDER);
        for index in 0..super::super::crt::LOOKS.len() {
            for curved in [true, false] {
                let preset = super::super::crt::path(&folder, index, curved);
                let note = renderer.set_shader(Some(&preset)).unwrap();
                assert!(renderer.shader_loaded(), "{note}");
                let shaded = draw(&renderer, 64, 48);
                assert_eq!(renderer.take_shader_note(), None, "{}", preset.display());
                assert_ne!(
                    shaded,
                    plain,
                    "{}: drew nothing of its own",
                    preset.display()
                );
            }
        }
    }

    #[test]
    fn a_preset_that_does_not_load_leaves_the_scaler() {
        let _gpu = exclusive();
        let Some(mut renderer) = renderer(Scaling::Integer, Filter::Nearest) else {
            return;
        };
        let picture = pattern();
        renderer.set_picture(&picture, 4, 3);
        let plain = draw(&renderer, 64, 48);
        let missing = passthrough().with_file_name("missing.slangp");
        let note = renderer.set_shader(Some(&missing)).unwrap();
        assert!(note.contains("not loaded"), "{note}");
        assert_eq!(draw(&renderer, 64, 48), plain);
        assert!(renderer.toggle_shader().unwrap().ends_with("off"));
        assert!(renderer.toggle_shader().unwrap().contains("not loaded"));
        assert!(renderer.reload_shader().unwrap().contains("not loaded"));
        assert_eq!(draw(&renderer, 64, 48), plain);
        // Without a preset, F2 has nothing to say.
        renderer.set_shader(None);
        assert_eq!(renderer.toggle_shader(), None);
    }
}
