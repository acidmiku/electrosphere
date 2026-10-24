//! RetroArch slang shader presets over the picture (`[display]
//! shader_preset`), drawn by [librashader](https://github.com/SnowflakePowered/librashader):
//! CRT looks, anti-aliasing, sharpening.
//!
//! - **Input: native.** The chain gets the picture at its 1x size, as
//!   presets expect (a scanline per game line): the software renderer's
//!   frame as it is, the GPU renderer's N x picture averaged back to its 1x
//!   frame ([`Native`], `native.wgsl`: N x N supersampling).
//! - **Movies:** a movie pack's image (`super::movie`) is drawn into that
//!   1x input, filtered down over each pixel, the game's own pixels over
//!   it (subtitles, fades) kept: the shader draws the movies too.
//! - **Output:** the picture's rectangle of the target (`scale::picture_rect`:
//!   4:3, stretch or integer, a widescreen aspect). The bars stay black; the
//!   messages are drawn over it, unfiltered.
//! - **Frames:** the chain's FrameCount counts the pictures the game showed
//!   (not redraws); a new input size (240 and 480 lines) clears its history.
//! - **Failures:** a preset that does not load, or a frame that fails, leaves
//!   the plain scaler; the reason goes on screen. F2 toggles the chain and
//!   Shift+F2 loads the preset again ([`super::Presenter::window_event`]).
use super::scale::Rect;
use librashader_common::shader_features::ShaderFeatures;
use librashader_common::{Size, Viewport};
use librashader_runtime_wgpu::options::FrameOptionsWgpu;
use librashader_runtime_wgpu::{FilterChainWgpu, WgpuOutputView};
use std::path::{Path, PathBuf};
use std::time::Instant;

const SHADER: &str = include_str!("native.wgsl");

/// A shader preset and its filter chain.
pub struct Shader {
    path: PathBuf,
    /// `None` when the preset did not load or a frame failed.
    chain: Option<FilterChainWgpu>,
    /// F2: drawn through the chain (else the plain scaler).
    pub enabled: bool,
    native: Native,
    /// The input size of the previous frame (a new one clears the history).
    input: Option<(u32, u32)>,
}

impl Shader {
    /// Load the preset at `path` (`.slangp`, or a single `.slang`). The
    /// result says what happened, for the log and the screen.
    pub fn load(device: &wgpu::Device, queue: &wgpu::Queue, path: &Path) -> (Self, String) {
        let mut shader = Self {
            path: path.to_owned(),
            chain: None,
            enabled: true,
            native: Native::new(device),
            input: None,
        };
        let note = shader.reload(device, queue);
        (shader, note)
    }

    /// Load the preset again (Shift+F2, after editing it).
    pub fn reload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> String {
        let started = Instant::now();
        let name = self.name();
        match FilterChainWgpu::load_from_path(&self.path, ShaderFeatures::NONE, device, queue, None)
        {
            Ok(chain) => {
                self.chain = Some(chain);
                self.input = None;
                format!(
                    "Shader {name} loaded in {} ms",
                    started.elapsed().as_millis()
                )
            }
            Err(error) => {
                self.chain = None;
                format!("Shader {name} not loaded: {error}")
            }
        }
    }

    /// The preset's file name.
    pub fn name(&self) -> String {
        self.path.file_name().map_or_else(
            || self.path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        )
    }

    /// Whether the next draw goes through the chain.
    pub fn active(&self) -> bool {
        self.enabled && self.chain.is_some()
    }

    /// Record the chain drawing `picture` (laid out as its 1x `frame`, with
    /// the movie layer at its viewport in that frame, if any) into `rect` of
    /// `target` (`format`, `size`), as picture number `frames`. A failing
    /// frame drops the chain and says why.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        (picture, frame, movie): (&wgpu::Texture, (u32, u32), Option<Movie<'_>>),
        target: (&wgpu::TextureView, wgpu::TextureFormat, (u32, u32)),
        rect: Rect,
        frames: usize,
    ) -> Result<(), String> {
        let name = self.name();
        let Some(chain) = self.chain.as_mut() else {
            return Err("no shader loaded".into());
        };
        let input = self
            .native
            .input(device, queue, encoder, picture, frame, movie);
        let size = (input.width(), input.height());
        let clear_history = self.input.replace(size) != Some(size);
        let (view, format, (width, height)) = target;
        let viewport = Viewport {
            x: rect.x as f32,
            y: rect.y as f32,
            mvp: None,
            output: WgpuOutputView::new_from_raw(view, Size::new(width, height), format),
            size: Size::new(rect.width, rect.height),
        };
        let options = FrameOptionsWgpu {
            clear_history,
            frame_direction: 1,
            total_subframes: 1,
            current_subframe: 1,
            ..Default::default()
        };
        let drawn = chain.frame(input, &viewport, encoder, frames, Some(&options));
        drawn.map_err(|error| {
            self.chain = None;
            format!("Shader {name} failed: {error}")
        })
    }
}

/// The movie layer and its viewport in the 1x input.
pub type Movie<'a> = (&'a super::movie::MovieDraw, [f32; 4]);

/// The 1x input from an N x picture (`native.wgsl`), with the movie layer.
struct Native {
    layout: wgpu::BindGroupLayout,
    pipeline: Option<(wgpu::TextureFormat, wgpu::RenderPipeline)>,
    module: wgpu::ShaderModule,
    params: wgpu::Buffer,
    texture: Option<wgpu::Texture>,
}

impl Native {
    fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ac3 native"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ac3 native"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
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
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ac3 native"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            layout,
            pipeline: None,
            module,
            params,
            texture: None,
        }
    }

    /// `picture` at its 1x `frame`, recorded into `encoder`: itself when it
    /// is that size and no movie is shown, else this input's own texture
    /// (a copy, or the N x N block means), with the movie layer drawn in.
    fn input<'a>(
        &'a mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        picture: &'a wgpu::Texture,
        frame: (u32, u32),
        movie: Option<Movie<'_>>,
    ) -> &'a wgpu::Texture {
        let scale = picture.width() / frame.0.max(1);
        if scale == 0 || frame.0 * scale != picture.width() || frame.1 * scale != picture.height() {
            return picture;
        }
        if scale == 1 && movie.is_none() {
            return picture;
        }
        let format = picture.format();
        if self
            .texture
            .as_ref()
            .is_none_or(|t| (t.width(), t.height(), t.format()) != (frame.0, frame.1, format))
        {
            self.texture = Some(device.create_texture(&wgpu::TextureDescriptor {
                label: Some("ac3 native picture"),
                size: wgpu::Extent3d {
                    width: frame.0,
                    height: frame.1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            }));
        }
        if self.pipeline.as_ref().is_none_or(|(f, _)| *f != format) {
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("ac3 native"),
                bind_group_layouts: &[Some(&self.layout)],
                immediate_size: 0,
            });
            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("ac3 native"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &self.module,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &self.module,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            });
            self.pipeline = Some((format, pipeline));
        }
        let texture = self.texture.as_ref().expect("made above");
        if scale == 1 {
            encoder.copy_texture_to_texture(
                picture.as_image_copy(),
                texture.as_image_copy(),
                picture.size(),
            );
            Self::draw_movie(encoder, texture, movie);
            return texture;
        }
        queue.write_buffer(&self.params, 0, &scale.to_le_bytes());
        let source = picture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ac3 native"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&source),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.params.as_entire_binding(),
                },
            ],
        });
        let texture = self.texture.as_ref().expect("made above");
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ac3 native"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.pipeline.as_ref().expect("made above").1);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        Self::draw_movie(encoder, texture, movie);
        texture
    }

    /// The movie layer into the 1x input `texture`, if one is shown.
    fn draw_movie(
        encoder: &mut wgpu::CommandEncoder,
        texture: &wgpu::Texture,
        movie: Option<Movie<'_>>,
    ) {
        let Some((movie, viewport)) = movie else {
            return;
        };
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("ac3 native movie"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        movie.draw_input(&mut pass, viewport);
    }
}
