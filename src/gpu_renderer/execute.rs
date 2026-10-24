//! The executor: runs recorded [`Frame`]s on the graphics card (wgpu) and
//! turns the N x display area into an RGBA picture, which the presenter
//! scales into the window (or the headless player reads back). A frame's
//! texture replacements go into the [`Atlas`](super::atlas::Atlas) first.
//! When a frame copied the shown field ([`Op::Show`]), the picture is drawn
//! from that copy until a frame says otherwise ([`Frame::shown`]).
//!
//! An effects frame ([`super::fx`]) also draws into the z target: its
//! [`Op::FxDraw`]s of [`Pipe::World`] and [`Pipe::ZClear`] run in render
//! passes with two colour attachments (VRAM and the z target), the
//! deferred ones ([`Pipe::Deferred`]) with the depth snapshot bound; the
//! effects' steps ([`Op::FxBegin`], [`Op::FxPost`], [`Op::FxLate`],
//! [`Op::FxSnapshot`]) run between passes. The resources are made at the
//! first effects frame ([`FxCard`]); frames without effects run exactly as
//! before.
use super::atlas::{Atlas, AtlasStats};
use super::fx::card::{CardStats, FxCard};
use super::fx::{CardEffect, Context, Pipe};
use super::record::{Frame, Op, Shown, VERTEX_WORDS};
use super::tiles::{HEIGHT, Rect, WIDTH};
use super::{MAX_SCALE, Options};
use std::collections::VecDeque;
use std::sync::Arc;

pub(crate) const DRAW_SHADER: &str = include_str!("draw.wgsl");
const DISPLAY_SHADER: &str = include_str!("display.wgsl");
/// The card's VRAM: PlayStation pixels.
pub const VRAM_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R16Uint;
/// Frames queued on the card before [`Executor::execute`] waits for the
/// oldest, so the game never runs ahead of the card by more.
const FRAMES_IN_FLIGHT: usize = 2;

/// Which effects step [`Executor::fx_step`] runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FxStep {
    /// World begin ([`CardEffect::begin`]).
    Begin,
    /// World end ([`CardEffect::post`]), after the depth snapshot.
    Post,
    /// After the deferred primitives ([`CardEffect::late`]).
    Late,
}

/// The displayed rectangle, in 1 x VRAM pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplayArea {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl DisplayArea {
    /// Inside VRAM (the software path shows nothing otherwise).
    pub fn fits(&self) -> bool {
        self.width > 0
            && self.height > 0
            && self.x + self.width <= WIDTH as u32
            && self.y + self.height <= HEIGHT as u32
    }
}

/// `rect` (1 x VRAM pixels) in N x pixels: x, y, width, height.
pub fn scaled(rect: Rect, scale: u32) -> [u32; 4] {
    let n = scale as i32;
    [rect.x * n, rect.y * n, rect.w * n, rect.h * n].map(|value| value.max(0) as u32)
}

/// What the executor has done.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct ExecutorStats {
    pub frames: u64,
    pub render_passes: u64,
    pub draw_calls: u64,
    pub texture_copies: u64,
    pub uploaded_bytes: u64,
    /// CPU time recording the card's commands, and waiting for the card
    /// (at most [`FRAMES_IN_FLIGHT`] frames ahead), in microseconds.
    pub encode_micros: u64,
    pub wait_micros: u64,
    /// Texture replacements on the card.
    pub replacements: AtlasStats,
    /// Effects on the card.
    pub effects: CardStats,
}

struct Picture {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
}

/// The copy of the shown field ([`Op::Show`]): N x VRAM pixels of the
/// display area, and the display group that reads it.
struct Field {
    texture: wgpu::Texture,
    group: wgpu::BindGroup,
}

/// The card side of the GPU renderer; see the module documentation.
pub struct Executor {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    scale: u32,
    vram: wgpu::Texture,
    vram_view: wgpu::TextureView,
    shadow: wgpu::Texture,
    shadow_view: wgpu::TextureView,
    mirror: wgpu::Texture,
    /// Effects: their card resources (made at the first effects frame;
    /// `Err` when they could not be set up) and the registered effects.
    fx: Option<Result<FxCard, String>>,
    effects: Vec<Box<dyn CardEffect>>,
    draw_pipeline: wgpu::RenderPipeline,
    draw_layout: wgpu::BindGroupLayout,
    globals: wgpu::Buffer,
    draw_group: wgpu::BindGroup,
    /// Texture replacements.
    atlas: Atlas,
    display_pipeline: wgpu::RenderPipeline,
    display_layout: wgpu::BindGroupLayout,
    display_group: wgpu::BindGroup,
    display_uniform: wgpu::Buffer,
    /// The copy of the shown field, and the display area (1 x) it holds
    /// while the screen shows it (`None`: the live display area).
    field: Option<Field>,
    showing: Option<Rect>,
    picture_format: wgpu::TextureFormat,
    picture: Option<Picture>,
    /// Counts replacements of the picture texture (a new size).
    pub picture_generation: u64,
    vertex_buffer: wgpu::Buffer,
    data_buffer: wgpu::Buffer,
    bytes: Vec<u8>,
    in_flight: VecDeque<wgpu::SubmissionIndex>,
    pub stats: ExecutorStats,
    /// Adapter, backend and internal resolution, for the log.
    pub description: String,
}

pub(crate) fn texture(
    device: &wgpu::Device,
    label: &str,
    (width, height): (u32, u32),
    format: wgpu::TextureFormat,
    usage: wgpu::TextureUsages,
) -> wgpu::Texture {
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
        format,
        usage,
        view_formats: &[],
    })
}

pub(crate) fn buffer(
    device: &wgpu::Device,
    label: &str,
    size: u64,
    usage: wgpu::BufferUsages,
) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage: usage | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

pub(crate) fn uint_texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    uint_entry(binding, wgpu::TextureViewDimension::D2)
}

fn uint_entry(
    binding: u32,
    view_dimension: wgpu::TextureViewDimension,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Uint,
            view_dimension,
            multisampled: false,
        },
        count: None,
    }
}

pub(crate) fn uniform_entry(
    binding: u32,
    visibility: wgpu::ShaderStages,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn words_to_bytes(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|word| word.to_le_bytes()).collect()
}

impl Executor {
    /// Set up on `device` (the presenter's, or an offscreen one) at
    /// `options.scale`; the display picture has `picture_format` (an 8-bit
    /// RGBA format, sRGB or not).
    pub fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        options: Options,
        picture_format: wgpu::TextureFormat,
    ) -> crate::Result<Self> {
        let scale = options.scale.clamp(1, MAX_SCALE);
        let limit = device.limits().max_texture_dimension_2d;
        if WIDTH as u32 * scale > limit {
            return Err(format!(
                "{scale}x needs {}-pixel textures; this graphics card allows {limit}",
                WIDTH as u32 * scale
            )
            .into());
        }
        // Any error while setting up (a shader the driver's compiler
        // refuses, too little memory) makes the renderer unavailable, and
        // the software renderer draws instead.
        let scopes = [
            wgpu::ErrorFilter::Validation,
            wgpu::ErrorFilter::OutOfMemory,
            wgpu::ErrorFilter::Internal,
        ]
        .map(|filter| device.push_error_scope(filter));
        let size = (WIDTH as u32 * scale, HEIGHT as u32 * scale);
        use wgpu::TextureUsages as Usage;
        let vram = texture(
            &device,
            "ac3 vram",
            size,
            VRAM_FORMAT,
            Usage::RENDER_ATTACHMENT | Usage::TEXTURE_BINDING | Usage::COPY_SRC | Usage::COPY_DST,
        );
        let shadow = texture(
            &device,
            "ac3 vram shadow",
            size,
            VRAM_FORMAT,
            Usage::TEXTURE_BINDING | Usage::COPY_SRC | Usage::COPY_DST,
        );
        let mirror = texture(
            &device,
            "ac3 vram mirror",
            (WIDTH as u32, HEIGHT as u32),
            VRAM_FORMAT,
            Usage::TEXTURE_BINDING | Usage::COPY_DST,
        );
        let vram_view = vram.create_view(&Default::default());
        let shadow_view = shadow.create_view(&Default::default());
        let globals = buffer(
            &device,
            "ac3 gpu renderer globals",
            16,
            wgpu::BufferUsages::UNIFORM,
        );
        queue.write_buffer(&globals, 0, &words_to_bytes(&[scale, 0, 0, 0]));

        let draw_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ac3 gpu renderer draw"),
            source: wgpu::ShaderSource::Wgsl(DRAW_SHADER.into()),
        });
        let draw_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ac3 gpu renderer draw"),
            entries: &[
                uniform_entry(0, wgpu::ShaderStages::VERTEX_FRAGMENT),
                uint_texture_entry(1),
                uint_texture_entry(2),
                uint_entry(3, wgpu::TextureViewDimension::D2Array),
                uint_texture_entry(4),
            ],
        });
        let draw_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ac3 gpu renderer draw"),
            bind_group_layouts: &[Some(&draw_layout)],
            immediate_size: 0,
        });
        let attributes = wgpu::vertex_attr_array![
            0 => Float32x2,
            1 => Float32x2,
            2 => Unorm8x4,
            3 => Uint32,
            4 => Uint32x4,
            5 => Uint32x4,
            6 => Uint32x4,
            7 => Uint32x4,
            8 => Float32
        ];
        let draw_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ac3 gpu renderer draw"),
            layout: Some(&draw_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &draw_module,
                entry_point: Some("vs_draw"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: (VERTEX_WORDS * 4) as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &attributes,
                })],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &draw_module,
                entry_point: Some("fs_draw"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: VRAM_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let atlas = Atlas::new(&device);
        let draw_group =
            Self::draw_group(&device, &draw_layout, &globals, &mirror, &shadow, &atlas);

        let display_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ac3 gpu renderer display"),
            source: wgpu::ShaderSource::Wgsl(DISPLAY_SHADER.into()),
        });
        let display_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ac3 gpu renderer display"),
            entries: &[
                uniform_entry(0, wgpu::ShaderStages::FRAGMENT),
                uint_texture_entry(1),
            ],
        });
        let display_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("ac3 gpu renderer display"),
                bind_group_layouts: &[Some(&display_layout)],
                immediate_size: 0,
            });
        let display_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ac3 gpu renderer display"),
            layout: Some(&display_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &display_module,
                entry_point: Some("vs_display"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &display_module,
                entry_point: Some("fs_display"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: picture_format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let display_uniform = buffer(
            &device,
            "ac3 gpu renderer display",
            16,
            wgpu::BufferUsages::UNIFORM,
        );
        let display_group =
            Self::display_group(&device, &display_layout, &display_uniform, &vram_view);
        let vertex_buffer = buffer(
            &device,
            "ac3 gpu renderer vertices",
            1 << 20,
            wgpu::BufferUsages::VERTEX,
        );
        let data_buffer = buffer(
            &device,
            "ac3 gpu renderer data",
            1 << 21,
            wgpu::BufferUsages::COPY_SRC,
        );
        // Scopes nest: pop them all, the innermost first.
        let mut failed = None;
        for scope in scopes.into_iter().rev() {
            if let Some(error) = pollster::block_on(scope.pop()) {
                failed.get_or_insert(error);
            }
        }
        if let Some(error) = failed {
            return Err(format!("the GPU renderer could not be set up: {error}").into());
        }
        let info = device.adapter_info();
        let description = format!(
            "{} ({}, {:?}), {scale}x internal resolution ({}x{})",
            info.name, info.backend, info.device_type, size.0, size.1
        );
        Ok(Self {
            device,
            queue,
            scale,
            vram,
            vram_view,
            shadow,
            shadow_view,
            mirror,
            fx: None,
            effects: Vec::new(),
            draw_pipeline,
            draw_layout,
            globals,
            draw_group,
            atlas,
            display_pipeline,
            display_layout,
            display_group,
            display_uniform,
            field: None,
            showing: None,
            picture_format,
            picture: None,
            picture_generation: 0,
            vertex_buffer,
            data_buffer,
            bytes: Vec::new(),
            in_flight: VecDeque::new(),
            stats: ExecutorStats::default(),
            description,
        })
    }

    /// The display pass's group: its uniform and the N x VRAM pixels `view`.
    fn display_group(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        uniform: &wgpu::Buffer,
        view: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ac3 gpu renderer display"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(view),
                },
            ],
        })
    }

    /// The texture for a shown field of `width` x `height` N x pixels,
    /// made again when the size changes.
    fn field_texture(&mut self, width: u32, height: u32) -> wgpu::Texture {
        let fits = self.field.as_ref().is_some_and(|field| {
            (field.texture.width(), field.texture.height()) == (width, height)
        });
        if !fits {
            let texture = texture(
                &self.device,
                "ac3 gpu renderer shown field",
                (width, height),
                VRAM_FORMAT,
                wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            );
            let view = texture.create_view(&Default::default());
            let group = Self::display_group(
                &self.device,
                &self.display_layout,
                &self.display_uniform,
                &view,
            );
            self.field = Some(Field { texture, group });
        }
        self.field.as_ref().expect("made above").texture.clone()
    }

    fn draw_group(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        globals: &wgpu::Buffer,
        mirror: &wgpu::Texture,
        shadow: &wgpu::Texture,
        atlas: &Atlas,
    ) -> wgpu::BindGroup {
        let mirror_view = mirror.create_view(&Default::default());
        let shadow_view = shadow.create_view(&Default::default());
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ac3 gpu renderer draw"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: globals.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&mirror_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&shadow_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&atlas.view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&atlas.slot_view),
                },
            ],
        })
    }

    /// Put a frame's new texture replacements into the atlas.
    fn add_replacements(&mut self, frame: &Frame) {
        if frame.replacements.is_empty() && !frame.replacements_reset {
            return;
        }
        let grown = self.atlas.add(
            &self.device,
            &self.queue,
            &frame.replacements,
            frame.replacements_reset,
        );
        if grown {
            self.draw_group = Self::draw_group(
                &self.device,
                &self.draw_layout,
                &self.globals,
                &self.mirror,
                &self.shadow,
                &self.atlas,
            );
        }
        self.stats.replacements = self.atlas.stats.clone();
    }

    /// An executor on its own device without a window (`machine
    /// --headless`, tests): Vulkan, DX12 or Metal; `WGPU_BACKEND`,
    /// `WGPU_ADAPTER_NAME` and `WGPU_POWER_PREF` choose, as for the window.
    pub fn offscreen(options: Options) -> crate::Result<Self> {
        let (device, queue) = offscreen_device()?;
        Self::new(device, queue, options, wgpu::TextureFormat::Rgba8Unorm)
    }

    pub fn scale(&self) -> u32 {
        self.scale
    }

    /// Wait until fewer than `frames` submissions are queued on the card.
    fn throttle(&mut self, frames: usize) {
        while self.in_flight.len() > frames.saturating_sub(1) {
            let index = self.in_flight.pop_front().expect("nonempty");
            if let Err(error) = self.device.poll(wgpu::PollType::Wait {
                submission_index: Some(index),
                timeout: None,
            }) {
                eprintln!("ac3: gpu renderer: waiting for the card: {error}");
            }
        }
    }

    /// Wait for everything submitted.
    pub fn finish(&mut self) {
        self.throttle(0);
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
    }

    /// Grow `buffer` to hold `size` bytes.
    fn reserve(device: &wgpu::Device, buffer: &mut wgpu::Buffer, size: usize, label: &str) {
        if (buffer.size() as usize) < size {
            let usage = buffer.usage();
            let grown = (size as u64).next_power_of_two();
            *buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: grown,
                usage,
                mapped_at_creation: false,
            });
        }
    }

    /// A render pass into VRAM (and the z target with `zorder`), the draw
    /// group and the vertices bound; the caller sets the pipeline.
    fn begin_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        zorder: Option<&wgpu::TextureView>,
    ) -> wgpu::RenderPass<'static> {
        let attachment = |view| {
            Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })
        };
        let attachments = [attachment(&self.vram_view), zorder.and_then(attachment)];
        let count = if zorder.is_some() { 2 } else { 1 };
        let mut pass = encoder
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ac3 gpu renderer"),
                color_attachments: &attachments[..count],
                ..Default::default()
            })
            .forget_lifetime();
        pass.set_bind_group(0, &self.draw_group, &[]);
        pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
        pass
    }

    /// Register the card side of an effect.
    pub fn add_effect(&mut self, effect: Box<dyn CardEffect>) {
        self.effects.push(effect);
    }

    /// Why the effects' card resources could not be set up, if they could
    /// not (the host then turns effects off).
    pub fn effects_failed(&self) -> Option<&str> {
        match &self.fx {
            Some(Err(error)) => Some(error),
            _ => None,
        }
    }

    /// The effects' card resources, made at the first call.
    fn fx_card(&mut self) -> Option<&mut FxCard> {
        if self.fx.is_none() {
            let scopes = [
                wgpu::ErrorFilter::Validation,
                wgpu::ErrorFilter::OutOfMemory,
                wgpu::ErrorFilter::Internal,
            ]
            .map(|filter| self.device.push_error_scope(filter));
            let card = FxCard::new(&self.device, self.scale, &self.draw_layout);
            let mut failed = None;
            for scope in scopes.into_iter().rev() {
                if let Some(error) = pollster::block_on(scope.pop()) {
                    failed.get_or_insert(error);
                }
            }
            self.fx = Some(match failed {
                None => {
                    eprintln!("ac3: effects: the z target and pipelines are set up");
                    Ok(card)
                }
                Some(error) => {
                    let error = format!("effects could not be set up: {error}");
                    eprintln!("ac3: {error}; effects off");
                    Err(error)
                }
            });
        }
        self.fx.as_mut()?.as_mut().ok()
    }

    /// Run an effects step (`begin`: world begin, else the post) of
    /// `frame` over `area`: the registered effects, then (post) the debug
    /// view.
    fn fx_step(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        frame: &Frame,
        area: Rect,
        step: FxStep,
    ) {
        let Some(fx_frame) = frame.fx.as_deref() else {
            return;
        };
        if self.fx_card().is_none() {
            return;
        }
        let scale = self.scale;
        let Some(Ok(card)) = self.fx.as_mut() else {
            return;
        };
        let post = step == FxStep::Post;
        match step {
            FxStep::Begin => card.stats.frames += 1,
            FxStep::Post => {
                card.snapshot(
                    &self.device,
                    &self.queue,
                    encoder,
                    &self.shadow_view,
                    area,
                    fx_frame.debug,
                );
                card.stats.posts += 1;
            }
            FxStep::Late => {}
        }
        if !self.effects.is_empty() {
            card.write_scene(&self.queue, fx_frame);
        }
        for effect in &mut self.effects {
            let data = fx_frame
                .data
                .iter()
                .find(|(name, _)| *name == effect.name())
                .map(|(_, data)| &**data);
            let mut ctx = Context {
                device: &self.device,
                queue: &self.queue,
                encoder,
                scale,
                area,
                pixels: scaled(area, scale),
                vram: &self.vram_view,
                vram_texture: &self.vram,
                shadow: &self.shadow_view,
                shadow_texture: &self.shadow,
                zorder: &card.zorder_view,
                snapshot: match step {
                    FxStep::Begin => None,
                    FxStep::Post | FxStep::Late => card.snapshot_view(),
                },
                scene: &fx_frame.scene,
                frame: fx_frame,
                scene_buffer: &card.scene_buffer,
                debug: fx_frame.debug,
                data,
            };
            match step {
                FxStep::Begin => effect.begin(&mut ctx),
                FxStep::Post => effect.post(&mut ctx),
                FxStep::Late => effect.late(&mut ctx),
            }
            card.stats.effect_calls += 1;
        }
        if post && fx_frame.debug != super::fx::DebugView::Off {
            card.debug_view(encoder, &self.vram_view);
        }
    }

    /// The last depth snapshot (r view z, g draw order; the world's area at
    /// N x) and where it is (x, y, width, height at N x), read back (tests).
    pub fn read_fx_snapshot(&mut self) -> Option<(Vec<[f32; 2]>, [u32; 4])> {
        let (texture, rect) = {
            let card = self.fx.as_ref()?.as_ref().ok()?;
            let (texture, rect) = card.snapshot_texture()?;
            (texture.clone(), rect)
        };
        let bytes = self.read_texture(&texture, rect[2], rect[3], 8);
        let values = bytes
            .chunks_exact(8)
            .map(|p| {
                [
                    f32::from_le_bytes([p[0], p[1], p[2], p[3]]),
                    f32::from_le_bytes([p[4], p[5], p[6], p[7]]),
                ]
            })
            .collect();
        Some((values, rect))
    }

    pub fn effects_stats(&self) -> Option<CardStats> {
        self.fx
            .as_ref()?
            .as_ref()
            .ok()
            .map(|card| card.stats.clone())
    }

    fn region(texture: &wgpu::Texture, x: u32, y: u32) -> wgpu::TexelCopyTextureInfo<'_> {
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d { x, y, z: 0 },
            aspect: wgpu::TextureAspect::All,
        }
    }

    /// Run a recorded frame on the card (submitted, not waited for).
    pub fn execute(&mut self, frame: &Frame) {
        let waiting = std::time::Instant::now();
        self.throttle(FRAMES_IN_FLIGHT);
        let started = std::time::Instant::now();
        self.stats.wait_micros += started.duration_since(waiting).as_micros() as u64;
        self.stats.frames += 1;
        if frame.shown == Shown::Live {
            self.showing = None;
        }
        if frame.is_empty() {
            return;
        }
        self.add_replacements(frame);
        let mut bytes = std::mem::take(&mut self.bytes);
        bytes.clear();
        bytes.extend(frame.vertices.iter().flat_map(|word| word.to_le_bytes()));
        if !bytes.is_empty() {
            Self::reserve(
                &self.device,
                &mut self.vertex_buffer,
                bytes.len(),
                "ac3 gpu renderer vertices",
            );
            self.queue.write_buffer(&self.vertex_buffer, 0, &bytes);
            self.stats.uploaded_bytes += bytes.len() as u64;
        }
        bytes.clear();
        bytes.extend(frame.data.iter().flat_map(|half| half.to_le_bytes()));
        bytes.resize(bytes.len().next_multiple_of(4), 0);
        if !bytes.is_empty() {
            Self::reserve(
                &self.device,
                &mut self.data_buffer,
                bytes.len(),
                "ac3 gpu renderer data",
            );
            self.queue.write_buffer(&self.data_buffer, 0, &bytes);
            self.stats.uploaded_bytes += bytes.len() as u64;
        }
        self.bytes = bytes;
        // An effects frame's vertex words.
        let fx_ready = !frame.fx_vertices.is_empty() && self.fx_card().is_some();
        if fx_ready {
            let bytes: Vec<u8> = frame
                .fx_vertices
                .iter()
                .flat_map(|word| word.to_le_bytes())
                .collect();
            if let Some(Ok(card)) = self.fx.as_mut() {
                Self::reserve(
                    &self.device,
                    &mut card.vertex_buffer,
                    bytes.len(),
                    "ac3 fx vertices",
                );
                self.queue.write_buffer(&card.vertex_buffer, 0, &bytes);
                self.stats.uploaded_bytes += bytes.len() as u64;
            }
        }
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("ac3 gpu renderer"),
            });
        let n = self.scale;
        // The open pass, whether it draws into the z target too, and the
        // pipeline bound (`None`: the renderer's own; `Some(None)`: none).
        let mut pass: Option<(wgpu::RenderPass<'static>, bool, Option<Option<Pipe>>)> = None;
        for op in &frame.ops {
            match op {
                Op::Draw {
                    first,
                    count,
                    scissor,
                } => {
                    let [x, y, w, h] = scaled(*scissor, n);
                    if w == 0 || h == 0 || *count == 0 {
                        continue;
                    }
                    if !matches!(pass, Some((_, false, _))) {
                        // The open pass (into the z target too) ends first.
                        drop(pass.take());
                        pass = Some((self.begin_pass(&mut encoder, None), false, None));
                        self.stats.render_passes += 1;
                    }
                    let (open, _, bound) = pass.as_mut().expect("opened above");
                    if *bound != Some(None) {
                        open.set_pipeline(&self.draw_pipeline);
                        *bound = Some(None);
                    }
                    open.set_scissor_rect(x, y, w, h);
                    open.draw(*first..first + count, 0..1);
                    self.stats.draw_calls += 1;
                }
                Op::FxDraw {
                    first,
                    count,
                    scissor,
                    pipe,
                } => {
                    let [x, y, w, h] = scaled(*scissor, n);
                    if w == 0 || h == 0 || *count == 0 {
                        continue;
                    }
                    let Some(Ok(card)) = self.fx.as_ref().filter(|_| fx_ready) else {
                        // Without the effects' resources: the colour alone,
                        // with the renderer's own pipeline.
                        if *pipe == Pipe::ZClear {
                            continue;
                        }
                        if !matches!(pass, Some((_, false, _))) {
                            drop(pass.take());
                            pass = Some((self.begin_pass(&mut encoder, None), false, None));
                            self.stats.render_passes += 1;
                        }
                        let (open, _, bound) = pass.as_mut().expect("opened above");
                        if *bound != Some(None) {
                            open.set_pipeline(&self.draw_pipeline);
                            *bound = Some(None);
                        }
                        open.set_scissor_rect(x, y, w, h);
                        open.draw(*first..first + count, 0..1);
                        self.stats.draw_calls += 1;
                        continue;
                    };
                    let (pipeline, depth) = card.pipeline(*pipe);
                    let group = match pipe {
                        Pipe::Deferred => match card.snapshot_group() {
                            Some(group) => Some(group),
                            None => continue,
                        },
                        _ => None,
                    };
                    if !matches!(pass, Some((_, open_depth, _)) if open_depth == depth) {
                        drop(pass.take());
                        let zorder = depth.then_some(&card.zorder_view);
                        pass = Some((self.begin_pass(&mut encoder, zorder), depth, None));
                        self.stats.render_passes += 1;
                    }
                    let (open, _, bound) = pass.as_mut().expect("opened above");
                    if *bound != Some(Some(*pipe)) {
                        open.set_pipeline(pipeline);
                        open.set_vertex_buffer(1, card.vertex_buffer.slice(..));
                        if let Some(group) = group {
                            open.set_bind_group(1, group, &[]);
                        }
                        *bound = Some(Some(*pipe));
                    }
                    open.set_scissor_rect(x, y, w, h);
                    open.draw(*first..first + count, 0..1);
                    self.stats.draw_calls += 1;
                }
                Op::FxBegin(rect) | Op::FxPost(rect) | Op::FxLate(rect) => {
                    pass = None;
                    if fx_ready {
                        let step = match op {
                            Op::FxBegin(_) => FxStep::Begin,
                            Op::FxPost(_) => FxStep::Post,
                            _ => FxStep::Late,
                        };
                        self.fx_step(&mut encoder, frame, *rect, step);
                    }
                }
                Op::FxSnapshot(rect) => {
                    pass = None;
                    if !fx_ready {
                        continue;
                    }
                    let debug = frame.fx.as_ref().map(|fx| fx.debug).unwrap_or_default();
                    if let Some(Ok(card)) = self.fx.as_mut() {
                        card.snapshot(
                            &self.device,
                            &self.queue,
                            &mut encoder,
                            &self.shadow_view,
                            *rect,
                            debug,
                        );
                    }
                }
                Op::Sync(rects) => {
                    pass = None;
                    for rect in rects {
                        let [x, y, w, h] = scaled(*rect, n);
                        if w == 0 || h == 0 {
                            continue;
                        }
                        encoder.copy_texture_to_texture(
                            Self::region(&self.vram, x, y),
                            Self::region(&self.shadow, x, y),
                            wgpu::Extent3d {
                                width: w,
                                height: h,
                                depth_or_array_layers: 1,
                            },
                        );
                        self.stats.texture_copies += 1;
                    }
                }
                Op::Mirror {
                    rect,
                    offset,
                    pitch,
                } => {
                    pass = None;
                    if rect.is_empty() {
                        continue;
                    }
                    encoder.copy_buffer_to_texture(
                        wgpu::TexelCopyBufferInfo {
                            buffer: &self.data_buffer,
                            layout: wgpu::TexelCopyBufferLayout {
                                offset: (*offset * 2) as u64,
                                bytes_per_row: Some((*pitch * 2) as u32),
                                rows_per_image: Some(rect.h as u32),
                            },
                        },
                        Self::region(&self.mirror, rect.x as u32, rect.y as u32),
                        wgpu::Extent3d {
                            width: rect.w as u32,
                            height: rect.h as u32,
                            depth_or_array_layers: 1,
                        },
                    );
                    self.stats.texture_copies += 1;
                }
                Op::Show(rect) => {
                    pass = None;
                    let [x, y, w, h] = scaled(*rect, n);
                    if w == 0 || h == 0 {
                        continue;
                    }
                    let field = self.field_texture(w, h);
                    encoder.copy_texture_to_texture(
                        Self::region(&self.vram, x, y),
                        Self::region(&field, 0, 0),
                        wgpu::Extent3d {
                            width: w,
                            height: h,
                            depth_or_array_layers: 1,
                        },
                    );
                    self.showing = Some(*rect);
                    self.stats.texture_copies += 1;
                }
            }
        }
        drop(pass);
        let index = self.queue.submit([encoder.finish()]);
        self.in_flight.push_back(index);
        self.stats.encode_micros += started.elapsed().as_micros() as u64;
    }

    /// Draw `area` of the card's VRAM into the display picture (N x its
    /// size) and return the picture; `None` when the area leaves VRAM. The
    /// copy of the shown field is drawn instead while it holds `area`.
    pub fn render_display(&mut self, area: DisplayArea) -> Option<&wgpu::Texture> {
        if !area.fits() {
            return None;
        }
        let n = self.scale;
        let rect = Rect::new(
            area.x as i32,
            area.y as i32,
            area.width as i32,
            area.height as i32,
        );
        let shown = self.field.is_some() && self.showing == Some(rect);
        let origin = if shown {
            [0, 0]
        } else {
            [area.x * n, area.y * n]
        };
        let (width, height) = (area.width * n, area.height * n);
        if self
            .picture
            .as_ref()
            .is_none_or(|p| (p.width, p.height) != (width, height))
        {
            let texture = texture(
                &self.device,
                "ac3 gpu renderer picture",
                (width, height),
                self.picture_format,
                wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
            );
            let view = texture.create_view(&Default::default());
            self.picture = Some(Picture {
                texture,
                view,
                width,
                height,
            });
            self.picture_generation += 1;
        }
        let srgb = u32::from(self.picture_format.is_srgb());
        self.queue.write_buffer(
            &self.display_uniform,
            0,
            &words_to_bytes(&[origin[0], origin[1], srgb, 0]),
        );
        let picture = self.picture.as_ref().expect("made above");
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("ac3 gpu renderer display"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ac3 gpu renderer display"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &picture.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.display_pipeline);
            let group = match &self.field {
                Some(field) if shown => &field.group,
                _ => &self.display_group,
            };
            pass.set_bind_group(0, group, &[]);
            pass.draw(0..3, 0..1);
        }
        let index = self.queue.submit([encoder.finish()]);
        self.in_flight.push_back(index);
        self.picture.as_ref().map(|p| &p.texture)
    }

    /// Read back a texture region (`bytes_per_pixel` bytes a pixel), row by row.
    fn read_texture(
        &mut self,
        texture: &wgpu::Texture,
        width: u32,
        height: u32,
        bytes_per_pixel: u32,
    ) -> Vec<u8> {
        let row = (width * bytes_per_pixel).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ac3 gpu renderer readback"),
            size: u64::from(row * height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
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
        self.queue.submit([encoder.finish()]);
        readback.map_async(wgpu::MapMode::Read, .., |result| {
            if let Err(error) = result {
                eprintln!("ac3: gpu renderer: readback: {error}");
            }
        });
        self.finish();
        let mut out = Vec::with_capacity((width * height * bytes_per_pixel) as usize);
        if let Ok(mapped) = readback.get_mapped_range(..) {
            for y in 0..height as usize {
                let at = y * row as usize;
                out.extend_from_slice(&mapped[at..at + (width * bytes_per_pixel) as usize]);
            }
        }
        out
    }

    /// The display area as 0RGB words at N x, read back (headless runs).
    pub fn read_display(&mut self, area: DisplayArea) -> Option<(Vec<u32>, usize, usize)> {
        let texture = self.render_display(area)?.clone();
        let (width, height) = (texture.width(), texture.height());
        let bytes = self.read_texture(&texture, width, height, 4);
        let srgb = self.picture_format.is_srgb();
        debug_assert!(!srgb, "offscreen pictures are not sRGB");
        let rgb = bytes
            .chunks_exact(4)
            .map(|p| (u32::from(p[0]) << 16) | (u32::from(p[1]) << 8) | u32::from(p[2]))
            .collect();
        Some((rgb, width as usize, height as usize))
    }

    /// The whole N x VRAM, read back (tests).
    pub fn read_vram(&mut self) -> Vec<u16> {
        let texture = self.vram.clone();
        let (width, height) = (texture.width(), texture.height());
        self.read_texture(&texture, width, height, 2)
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect()
    }
}

/// A device and queue without a window: the high-performance adapter of the
/// primary backends (`WGPU_ADAPTER_NAME` picks by name).
pub fn offscreen_device() -> crate::Result<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY.with_env(),
        ..wgpu::InstanceDescriptor::new_without_display_handle_from_env()
    });
    let adapter =
        match std::env::var("WGPU_ADAPTER_NAME") {
            Ok(name) => {
                let wanted = name.to_lowercase();
                pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()))
                    .into_iter()
                    .find(|adapter| adapter.get_info().name.to_lowercase().contains(&wanted))
                    .ok_or_else(|| format!("WGPU_ADAPTER_NAME={name}: no such adapter"))?
            }
            Err(_) => pollster::block_on(
                instance.request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::from_env()
                        .unwrap_or(wgpu::PowerPreference::HighPerformance),
                    ..Default::default()
                }),
            )
            .map_err(|e| format!("no graphics adapter for the GPU renderer: {e}"))?,
        };
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("ac3 gpu renderer"),
        required_limits:
            wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits()),
        ..Default::default()
    }))?;
    device.on_uncaptured_error(Arc::new(|error| {
        eprintln!("ac3: gpu renderer: GPU error: {error}");
    }));
    Ok((device, queue))
}

#[cfg(test)]
mod tests {
    //! The executor against the software GPU, offscreen. One shared device
    //! (whatever adapter wgpu picks), one test at a time; skipped when there is
    //! no adapter unless `AC3_REQUIRE_GPU` is set.
    use super::super::Recorder;
    use super::*;
    use ac3_graphics::{Gpu, Vram};
    use std::sync::{Mutex, MutexGuard, OnceLock};

    fn device() -> Option<&'static (wgpu::Device, wgpu::Queue)> {
        static DEVICE: OnceLock<Option<(wgpu::Device, wgpu::Queue)>> = OnceLock::new();
        DEVICE
            .get_or_init(|| match offscreen_device() {
                Ok(pair) => {
                    let info = pair.0.adapter_info();
                    eprintln!("offscreen adapter: {} ({})", info.name, info.backend);
                    Some(pair)
                }
                Err(error) => {
                    eprintln!("skipped: {error}");
                    None
                }
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

    fn exclusive() -> MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn executor(scale: u32) -> Option<Executor> {
        let (device, queue) = device()?;
        Some(
            Executor::new(
                device.clone(),
                queue.clone(),
                Options {
                    scale,
                    dither: false,
                },
                wgpu::TextureFormat::Rgba8Unorm,
            )
            .expect("executor"),
        )
    }

    /// Run `words` through the software GPU and the GPU renderer at `scale`;
    /// returns the software VRAM and the card's (N x).
    fn run(scale: u32, words: &[u32]) -> Option<(Vec<u16>, Vec<u16>)> {
        let mut executor = executor(scale)?;
        let mut gpu = Gpu::new(Vram::new());
        let mut recorder = Recorder::new(executor_options(scale), &gpu);
        for (i, &word) in words.iter().enumerate() {
            recorder.gp0(word, 0x8010_0004 + 4 * i as u32, gpu.vram());
            gpu.write_gp0(word).expect("software GPU");
        }
        executor.execute(&recorder.take_frame(gpu.vram()));
        Some((gpu.vram().words().to_vec(), executor.read_vram()))
    }

    fn executor_options(scale: u32) -> Options {
        Options {
            scale,
            dither: false,
        }
    }

    /// Pixels where the card's 1 x VRAM differs from the software's.
    fn differences(software: &[u16], card: &[u16]) -> Vec<(usize, usize, u16, u16)> {
        software
            .iter()
            .zip(card)
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .map(|(i, (&a, &b))| (i % 1024, i / 1024, a, b))
            .collect()
    }

    const AREA: [u32; 3] = [0xe300_0000, 0xe407_ffff, 0xe500_0000];

    /// A scene of every primitive kind the game uses, with semi-transparency,
    /// textures (4-bit, 8-bit and 15-bit), masks and dithering.
    fn scene() -> Vec<u32> {
        let mut words = AREA.to_vec();
        // Background: a fill, Gouraud quads.
        words.extend([0x0210_2030, 0x0000_0000, 0x0100_0140]);
        words.extend([
            0x3800_0000,
            0x0000_0000,
            0x0000_ff00,
            0x0000_013f,
            0x00ff_0000,
            0x00ff_0000,
            0x0080_8080,
            0x00ff_013f,
        ]);
        // A 4-bit texture at (640, 0) with a palette at (0, 480), uploaded.
        words.extend([0xa000_0000, 0x0000_0280, 0x0040_0010]);
        words.extend((0..16 * 64 / 2).map(|i| 0x0123_4567u32.rotate_left(i % 32) ^ i));
        words.extend([0xa000_0000, 0x01e0_0000, 0x0001_0010]);
        words.extend((0..8).map(|i| (0x8000_7c1fu32).wrapping_add(i * 0x0421_0842)));
        // E1: page 10 (x 640), 4-bit, average semi, dithering on.
        words.extend([0xe100_020a]);
        // Flat-textured quad, opaque and modulated.
        words.extend([
            0x2c60_a0c0,
            0x0010_0010,
            0x7800_0000,
            0x0012_0090,
            0x000a_3f00,
            0x0090_0018,
            0x0000_003f,
            0x0098_0098,
            0x0000_3f3f,
        ]);
        // Gouraud-textured, semi-transparent (each blend function) and dithered.
        for abr in 0u32..4 {
            let page = 0x0a | (abr << 5) | 0x200;
            let x = 0xa0 + abr * 0x28;
            let y = 0x20 << 16;
            words.extend([
                0x3640_80ff,
                y | x,
                0x7800_0000,
                0x00ff_4020,
                y | (x + 0x30),
                (page << 16) | 0x003f,
                0x0020_ff40,
                (y + (0x40 << 16)) | x,
                0x3f00,
            ]);
        }
        // Semi-transparent sprites of each function, over the quads.
        for abr in 0u32..4 {
            words.push(0xe100_000a | (abr << 5));
            words.extend([
                0x6670_9030,
                0x0060_0008 + abr * 0x18,
                0x7800_0c04,
                0x0018_0030,
            ]);
        }
        // Mask: set on a tile, then a checked sprite over it.
        words.extend([0xe600_0001, 0x6000_ff00, 0x00a0_0010, 0x0010_0020]);
        words.extend([
            0xe600_0002,
            0x6480_8080,
            0x00a4_0014,
            0x7800_0000,
            0x0010_0020,
        ]);
        words.extend([0xe600_0000]);
        // Lines: flat, Gouraud (both directions), semi, a polyline.
        words.extend([0x4000_ffff, 0x00c0_0010, 0x00f0_0080]);
        words.extend([0x5000_00ff, 0x00e0_0120, 0x0000_ff00, 0x00c0_0090]);
        words.extend([0x5000_00ff, 0x00c8_0040, 0x00ff_0000, 0x00e4_0041]);
        words.extend([0x4280_8080, 0x00d0_0010, 0x00d0_0100]);
        words.extend([
            0x4a00_ff80,
            0x00f0_0010,
            0x00f8_0030,
            0x00e0_0050,
            0x00f0_0070,
            0x5555_5555,
        ]);
        // A 15-bit texture drawn by the GPU itself and sampled back: a
        // Gouraud triangle at (512, 256), then a sprite from page 8 y 256.
        words.extend([
            0x3000_00ff,
            0x0100_0200,
            0x0000_ff00,
            0x0100_023f,
            0x00ff_0000,
            0x013f_0200,
        ]);
        words.extend([
            0xe100_0118,
            0x6480_8080,
            0x00b8_0100,
            0x0000_0000,
            0x0040_0040,
        ]);
        // VRAM copies: a plain one and one with set mask.
        words.extend([0x8000_0000, 0x0000_0000, 0x0140_0000, 0x0040_0040]);
        words.extend([
            0xe600_0001,
            0x8000_0000,
            0x0000_0010,
            0x0160_0000,
            0x0010_0020,
            0xe600_0000,
        ]);
        // Gouraud triangle with dithering off (E1 bit 9 clear), then flat.
        words.extend([0xe100_000a]);
        words.extend([
            0x3000_0000,
            0x00c0_0100,
            0x00ff_ffff,
            0x00d0_0130,
            0x0080_4020,
            0x00f0_0108,
        ]);
        words.extend([0x2200_80c0, 0x0010_0100, 0x0030_0118, 0x0004_0128]);
        words
    }

    #[test]
    fn one_x_matches_the_software_gpu_pixel_for_pixel() {
        let _gpu = exclusive();
        let Some((software, card)) = run(1, &scene()) else {
            return;
        };
        let wrong = differences(&software, &card);
        assert!(
            wrong.is_empty(),
            "{} pixels differ, first {:?}",
            wrong.len(),
            &wrong[..wrong.len().min(12)]
        );
        // The scene drew something everywhere it should.
        assert!(software.iter().filter(|&&p| p != 0).count() > 50_000);
    }

    /// A small deterministic generator for the random scene.
    struct Random(u64);
    impl Random {
        fn next(&mut self) -> u32 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 33) as u32
        }
        fn below(&mut self, n: u32) -> u32 {
            self.next() % n
        }
        fn range(&mut self, lo: i32, hi: i32) -> i32 {
            lo + self.below((hi - lo + 1) as u32) as i32
        }
        fn chance(&mut self, percent: u32) -> bool {
            self.below(100) < percent
        }
    }

    fn xy(x: i32, y: i32) -> u32 {
        (x as u32 & 0xffff) | ((y as u32 & 0xffff) << 16)
    }

    /// A random texture page word: any function, depth and dither bit, in
    /// the lower half of VRAM (where nothing is drawn: a primitive sampling
    /// its own output is feedback the card does not reproduce), not on the
    /// right edge (whose texels run past VRAM).
    fn random_page(r: &mut Random) -> u32 {
        r.below(12) | (1 << 4) | (r.below(4) << 5) | (r.below(3) << 7) | (r.below(2) << 9)
    }

    /// A random palette word on rows 480..510 (8-bit palettes must not run
    /// past the last row).
    fn random_clut(r: &mut Random) -> u32 {
        r.below(64) | ((r.range(480, 510) as u32) << 6)
    }

    /// Random commands of every kind, in random drawing states, that the
    /// software GPU accepts.
    fn random_scene(seed: u64, count: usize) -> Vec<u32> {
        random_commands(seed, count).concat()
    }

    /// [`random_scene`], one entry per command (the first: the set-up).
    fn random_commands(seed: u64, count: usize) -> Vec<Vec<u32>> {
        let mut commands = Vec::new();
        let mut r = Random(seed);
        let mut words = vec![0xe300_0000, 0xe403_fbff, 0xe500_0000, 0xe100_0010];
        // Random VRAM content to sample: eight 128x64 uploads.
        for i in 0..8 {
            words.extend([0xa000_0000, xy(i * 128, 256), xy(128, 64)]);
            for _ in 0..128 * 64 / 2 {
                words.push(r.next() ^ (r.next() << 16));
            }
        }
        words.extend([0xa000_0000, xy(0, 480), xy(256, 32)]);
        for _ in 0..256 * 32 / 2 {
            words.push((r.next() & 0xfbff_7fff) | (r.next() & 0x8000_0400));
        }
        let color = |r: &mut Random| r.next() & 0xff_ffff;
        for _ in 0..count {
            commands.push(std::mem::take(&mut words));
            match r.below(100) {
                0..=4 => {
                    // Draw area (upper half) and offset.
                    let (left, top) = (r.range(0, 200), r.range(0, 150));
                    let (right, bottom) = (r.range(left, 1023), r.range(top, 255));
                    words.push(0xe300_0000 | left as u32 | ((top as u32) << 10));
                    words.push(0xe400_0000 | right as u32 | ((bottom as u32) << 10));
                    let (ox, oy) = (r.range(-64, 300), r.range(-64, 200));
                    words.push(0xe500_0000 | (ox as u32 & 0x7ff) | ((oy as u32 & 0x7ff) << 11));
                }
                5..=9 => {
                    // Texture page and window, mask bits.
                    words.push(0xe100_0000 | random_page(&mut r));
                    if r.chance(30) {
                        words.push(0xe200_0000 | (r.next() & 0xf_ffff));
                    } else {
                        words.push(0xe200_0000);
                    }
                    words.push(0xe600_0000 | r.below(4));
                }
                10..=44 => {
                    // Polygons of every kind.
                    let op = 0x20 | r.below(32);
                    let vertices = if op & 8 != 0 { 4 } else { 3 };
                    let (cx, cy) = (r.range(-50, 600), r.range(-50, 450));
                    let size = if r.chance(20) { 300 } else { 60 };
                    words.push((op << 24) | color(&mut r));
                    for i in 0..vertices {
                        if op & 0x10 != 0 && i > 0 {
                            words.push(color(&mut r));
                        }
                        words.push(xy(cx + r.range(-size, size), cy + r.range(-size, size)));
                        if op & 4 != 0 {
                            let uv = r.below(0x1_0000);
                            let high = match i {
                                0 => random_clut(&mut r),
                                1 => random_page(&mut r),
                                _ => 0,
                            };
                            words.push(uv | (high << 16));
                        }
                    }
                }
                45..=74 => {
                    // Rectangles of every size, textured or not.
                    let op = 0x60 | r.below(32);
                    words.push((op << 24) | color(&mut r));
                    words.push(xy(r.range(-40, 700), r.range(-40, 500)));
                    if op & 4 != 0 {
                        let clut = random_clut(&mut r);
                        words.push(r.below(0x1_0000) | (clut << 16));
                    }
                    if op & 0x18 == 0 {
                        words.push(xy(r.range(0, 120), r.range(0, 90)));
                    }
                }
                75..=86 => {
                    // Lines and polylines.
                    let op = 0x40 | r.below(32);
                    let segments = if op & 8 != 0 { r.range(1, 5) } else { 1 };
                    let (mut x, mut y) = (r.range(-20, 650), r.range(-20, 480));
                    words.push((op << 24) | color(&mut r));
                    words.push(xy(x, y));
                    for _ in 0..segments {
                        if op & 0x10 != 0 {
                            words.push(color(&mut r));
                        }
                        match r.below(4) {
                            0 => x += r.range(-80, 80),
                            1 => y += r.range(-80, 80),
                            _ => {
                                x += r.range(-120, 120);
                                y += r.range(-120, 120);
                            }
                        }
                        words.push(xy(x, y));
                    }
                    if op & 8 != 0 {
                        words.push(0x5555_5555);
                    }
                }
                87..=91 => {
                    // Fills.
                    let (w, h) = (r.range(1, 20) * 16, r.range(1, 100));
                    let (x, y) = (r.range(0, (1024 - w) / 16) * 16, r.range(0, 511 - h));
                    words.extend([0x0200_0000 | color(&mut r), xy(x, y), xy(w, h)]);
                }
                92..=96 => {
                    // VRAM copies, overlapping ones too.
                    let (w, h) = (r.range(1, 100), r.range(1, 60));
                    let (sx, sy) = (r.range(0, 1023), r.range(0, 511));
                    let (dx, dy) = if r.chance(30) {
                        (
                            (sx + r.range(-8, 8)).clamp(0, 1023),
                            (sy + r.range(-4, 4)).clamp(0, 511),
                        )
                    } else {
                        (r.range(0, 1023), r.range(0, 511))
                    };
                    words.extend([0x8000_0000, xy(sx, sy), xy(dx, dy), xy(w, h)]);
                }
                _ => {
                    // A small upload.
                    let (w, h) = (r.range(1, 16), r.range(1, 16));
                    let (x, y) = (r.range(0, 1024 - w), r.range(0, 512 - h));
                    words.extend([0xa000_0000, xy(x, y), xy(w, h)]);
                    for _ in 0..(w * h + 1) / 2 {
                        words.push(r.next());
                    }
                }
            }
        }
        commands.push(words);
        commands
    }

    #[test]
    fn one_x_matches_the_software_gpu_on_random_commands() {
        let _gpu = exclusive();
        for seed in 1..=6 {
            let Some((software, card)) = run(1, &random_scene(seed, 2500)) else {
                return;
            };
            let wrong = differences(&software, &card);
            if wrong.is_empty() {
                continue;
            }
            // The first command after which the two differ.
            let commands = random_commands(seed, 2500);
            let differs = |count: usize| {
                let (software, card) = run(1, &commands[..count].concat()).expect("device");
                !differences(&software, &card).is_empty()
            };
            let (mut good, mut bad) = (0, commands.len());
            while bad - good > 1 {
                let middle = (good + bad) / 2;
                if differs(middle) {
                    bad = middle;
                } else {
                    good = middle;
                }
            }
            let (software, card) = run(1, &commands[..bad].concat()).expect("device");
            let first = differences(&software, &card);
            panic!(
                "seed {seed}: {} pixels differ; command {bad} starts it: {:08x?}, {} pixels, first {:?}",
                wrong.len(),
                commands[bad - 1],
                first.len(),
                &first[..first.len().min(8)]
            );
        }
    }

    /// The game animates palettes by uploading a new one between the
    /// sprites that use each version: each sprite must decode with the
    /// palette current at its turn, however many sprites read it.
    #[test]
    fn palette_animation_keeps_each_version_for_its_sprites() {
        let _gpu = exclusive();
        let mut words = AREA.to_vec();
        // A 4-bit texture at (640, 0) of all sixteen indices; page 10.
        words.extend([0xa000_0000, 0x0000_0280, 0x0010_0004]);
        words.extend((0..32).map(|_| 0x3210_7654u32));
        words.push(0xe100_000a);
        for version in 0..20u32 {
            // Palette at (432, 63), as the game's.
            words.extend([0xa000_0000, 0x003f_01b0, 0x0001_0010]);
            words.extend((0..8).map(|i| (version * 8 + i).wrapping_mul(0x0842_1084) | 0x0001_0001));
            // 15 to 34 sprites with it: around the number of accesses a
            // tile keeps apart.
            for i in 0..15 + version {
                let (x, y) = ((i % 30) * 10, version * 20 + i / 30 * 8);
                words.extend([0x6480_8080, (y << 16) | x, 0x0fdb_0000, 0x0008_0008]);
            }
        }
        let Some((software, card)) = run(1, &words) else {
            return;
        };
        let wrong = differences(&software, &card);
        assert!(
            wrong.is_empty(),
            "{} differ: {:?}",
            wrong.len(),
            &wrong[..wrong.len().min(8)]
        );
    }

    #[test]
    fn one_x_matches_the_software_gpu_across_frames() {
        let _gpu = exclusive();
        let Some(mut executor) = executor(1) else {
            return;
        };
        // Frames of random length: what one frame leaves out of `shadow`
        // must reach the next one's reads.
        let mut gpu = Gpu::new(Vram::new());
        let mut recorder = Recorder::new(executor_options(1), &gpu);
        let mut lengths = Random(99);
        let mut since = 0;
        for command in random_commands(7, 3000) {
            for word in command {
                recorder.gp0(word, 0, gpu.vram());
                gpu.write_gp0(word).expect("software GPU");
            }
            since += 1;
            if since > lengths.below(60) {
                executor.execute(&recorder.take_frame(gpu.vram()));
                since = 0;
            }
        }
        executor.execute(&recorder.take_frame(gpu.vram()));
        let wrong = differences(gpu.vram().words(), &executor.read_vram());
        assert!(
            wrong.is_empty(),
            "{} pixels differ, first {:?}",
            wrong.len(),
            &wrong[..wrong.len().min(12)]
        );
        assert!(recorder.stats().frames > 50);
    }

    #[test]
    fn scaled_vram_holds_each_pixel_n_times_for_rectangles_and_copies() {
        let _gpu = exclusive();
        // Tiles, fills, uploads and copies are whole pixels at any scale.
        let mut words = AREA.to_vec();
        words.extend([0x0200_ff00, 0x0000_0010, 0x0020_0020]);
        words.extend([0x6000_00ff, 0x0004_0003, 0x0005_0007]);
        words.extend([
            0xa000_0000,
            0x0008_0040,
            0x0002_0002,
            0x7fff_1234,
            0x0001_4321,
        ]);
        words.extend([0x8000_0000, 0x0000_0000, 0x0080_0100, 0x0020_0040]);
        for scale in [2u32, 3] {
            let Some((software, card)) = run(scale, &words) else {
                return;
            };
            let n = scale as usize;
            let width = 1024 * n;
            for y in 0..256 {
                for x in 0..512 {
                    let expected = software[y * 1024 + x];
                    for sy in 0..n {
                        for sx in 0..n {
                            let got = card[(y * n + sy) * width + x * n + sx];
                            assert_eq!(got, expected, "{scale}x: pixel {x},{y} sample {sx},{sy}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn scaled_polygons_cover_their_area_at_n_times() {
        let _gpu = exclusive();
        // A flat triangle: at 4x its edges are finer than 1x pixels, and its
        // area grows 16 times (within its edge pixels).
        let mut words = AREA.to_vec();
        words.extend([0x2000_00ff, 0x0000_0000, 0x0000_0040, 0x0040_0000]);
        let Some((software, card)) = run(4, &words) else {
            return;
        };
        let one = software.iter().filter(|&&p| p == 0x1f).count() as f64;
        let four = card.iter().filter(|&&p| p == 0x1f).count() as f64 / 16.0;
        // 64x64 / 2 = 2048 for the ideal triangle.
        assert!((one - 2048.0).abs() < 70.0, "{one}");
        assert!((four - 2048.0).abs() < 20.0, "{four}");
    }

    /// The widescreen squeeze redraws HUD sprites as textured quads
    /// (`ac3_machine::widescreen`). Over the sprite's own texels, with its
    /// far edge (u + width, v + height) as the far texture coordinates, such
    /// a quad draws exactly the sprite's texels at 1 x and above: each 1 x
    /// pixel its own texel, none lost or repeated (the HUD's glyphs keep
    /// their last row).
    #[test]
    fn a_quad_over_a_sprites_texels_draws_the_sprite() {
        let _gpu = exclusive();
        // A 16x16 15-bit texture at (512, 0), every texel its own colour.
        let texels: Vec<u16> = (0..256u16)
            .map(|i| 0x8000 | (i * 97 % 0x7ffe + 1))
            .collect();
        let mut words = AREA.to_vec();
        words.extend(upload(512, 0, 16, 16, &texels));
        let tpage = 8 | (2 << 7);
        words.push(0xe100_0000 | tpage);
        let (u, v, w, h) = (2u32, 3u32, 12u32, 10u32);
        // The sprite at (10, 20); the quad over the same texels at (40, 20).
        words.extend([0x6500_0000, xy(10, 20), (v << 8) | u, (h << 16) | w]);
        let (left, right, top, bottom) = (40, 40 + w as i32, 20, 20 + h as i32);
        words.extend([
            0x2d00_0000,
            xy(left, top),
            (v << 8) | u,
            xy(right, top),
            (tpage << 16) | (v << 8) | (u + w),
            xy(left, bottom),
            ((v + h) << 8) | u,
            xy(right, bottom),
            ((v + h) << 8) | (u + w),
        ]);
        for scale in [1u32, 2, 4] {
            let Some((software, card)) = run(scale, &words) else {
                return;
            };
            let n = scale as usize;
            let width = 1024 * n;
            for y in 0..h as usize {
                for x in 0..w as usize {
                    let texel = texels[(v as usize + y) * 16 + u as usize + x];
                    assert_eq!(software[(20 + y) * 1024 + 10 + x], texel, "sprite {x},{y}");
                    assert_eq!(software[(20 + y) * 1024 + 40 + x], texel, "1x quad {x},{y}");
                    for sy in 0..n {
                        for sx in 0..n {
                            let row = ((20 + y) * n + sy) * width;
                            assert_eq!(
                                card[row + (10 + x) * n + sx],
                                texel,
                                "{scale}x sprite: texel {x},{y} sample {sx},{sy}"
                            );
                            assert_eq!(
                                card[row + (40 + x) * n + sx],
                                texel,
                                "{scale}x quad: texel {x},{y} sample {sx},{sy}"
                            );
                        }
                    }
                }
            }
        }
    }

    // ---- texture replacement ----

    use super::super::replace::{self, TexturePack};
    use crate::asset_dump::key;
    use crate::asset_dump::png_io::{self, Pixels};

    /// Texel (x, y) of the test texture: a 4-bit index, 0 (transparent)
    /// along the top-left diagonal.
    fn test_index(x: usize, y: usize) -> u8 {
        ((x * 3 + y * 5) % 16) as u8
    }

    /// The test palette: entry 0 transparent, odd entries with STP set.
    fn test_palette(version: u16) -> Vec<u16> {
        (0..16u16)
            .map(|i| {
                if i == 0 {
                    0
                } else {
                    ((i * 2 + version) & 31)
                        | ((31 - i) << 5)
                        | ((i * 7 % 32) << 10)
                        | ((i & 1) << 15)
                }
            })
            .collect()
    }

    /// The test texture: 16x16 texels (4x16 words) at (324, 0), page 5.
    fn test_texture() -> Vec<u16> {
        (0..16)
            .flat_map(|y| {
                (0..4).map(move |w| {
                    (0..4).fold(0u16, |word, k| {
                        word | (u16::from(test_index(w * 4 + k, y)) << (4 * k))
                    })
                })
            })
            .collect()
    }

    fn upload(x: u32, y: u32, w: u32, h: u32, pixels: &[u16]) -> Vec<u32> {
        let mut words = vec![0xa000_0000, xy(x as i32, y as i32), xy(w as i32, h as i32)];
        words.extend(
            pixels
                .chunks(2)
                .map(|pair| u32::from(pair[0]) | (u32::from(*pair.get(1).unwrap_or(&0)) << 16)),
        );
        words
    }

    /// Sprites, polygons, semi-transparency and a palette change, all
    /// sampling the test texture.
    fn replacement_scene() -> Vec<u32> {
        let clut = 480u32 << 22;
        let mut words = AREA.to_vec();
        words.extend([0x0210_4080, xy(0, 256), xy(128, 64)]);
        words.extend(upload(324, 0, 4, 16, &test_texture()));
        words.extend(upload(0, 480, 16, 1, &test_palette(0)));
        words.push(0xe100_0005);
        // Raw and modulated sprites; a semi-transparent one (average).
        words.extend([0x7d00_0000, xy(8, 264), clut | 0x0010]);
        words.extend([0x7c40_80c0, xy(28, 264), clut | 0x0010]);
        words.extend([0x7f00_0000, xy(48, 264), clut | 0x0010]);
        // A sprite of part of it, and a 6x5 one.
        words.extend([0x7480_8080, xy(68, 264), clut | 0x0414]);
        words.extend([0x6580_8080, xy(80, 264), clut | 0x0213, xy(6, 5)]);
        // A flat textured quad, a Gouraud one (semi), a skewed triangle.
        let page = 0x0005u32 << 16;
        words.extend([
            0x2c80_8080,
            xy(4, 290),
            clut | 0x0010,
            xy(24, 290),
            page | 0x001f,
            xy(4, 310),
            0x0f10,
            xy(24, 310),
            0x0f1f,
        ]);
        words.extend([
            0x3e40_8080,
            xy(30, 290),
            clut | 0x0010,
            0x0080_ff40,
            xy(62, 296),
            page | 0x001f,
            0x00ff_4080,
            xy(30, 318),
            0x0f10,
            0x0040_40ff,
            xy(58, 314),
            0x0f1f,
        ]);
        words.extend([
            0x2480_8080,
            xy(70, 288),
            clut | 0x0311,
            xy(110, 300),
            page | 0x041e,
            xy(76, 318),
            0x0e13,
        ]);
        // The palette animates: the same sprite with the next version.
        words.extend(upload(0, 480, 16, 1, &test_palette(9)));
        words.extend([0x7d00_0000, xy(100, 264), clut | 0x0010]);
        words
    }

    fn pack_folder(name: &str, files: &[(String, Vec<u8>)]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ac3-execute-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (file, png) in files {
            std::fs::write(dir.join(file), png).unwrap();
        }
        dir
    }

    /// [`run`] with the texture pack in `pack`; also the primitives replaced.
    fn run_with_pack(
        scale: u32,
        words: &[u32],
        pack: Option<&std::path::Path>,
    ) -> Option<(Vec<u16>, Vec<u16>, u64)> {
        let mut executor = executor(scale)?;
        let mut gpu = Gpu::new(Vram::new());
        let mut recorder = Recorder::new(executor_options(scale), &gpu);
        if let Some(pack) = pack {
            recorder.set_texture_pack(Some(TexturePack::open(pack).unwrap()), gpu.vram());
        }
        for (i, &word) in words.iter().enumerate() {
            recorder.gp0(word, 0x8010_0004 + 4 * i as u32, gpu.vram());
            gpu.write_gp0(word).expect("software GPU");
        }
        executor.execute(&recorder.take_frame(gpu.vram()));
        let replaced = recorder.replacer().map_or(0, |r| r.stats.replaced);
        Some((gpu.vram().words().to_vec(), executor.read_vram(), replaced))
    }

    /// The test texture's indices `factor` times larger (nearest).
    fn upscaled_indices(factor: usize) -> Vec<u8> {
        (0..16 * factor)
            .flat_map(|y| (0..16 * factor).map(move |x| test_index(x / factor, y / factor)))
            .collect()
    }

    #[test]
    fn replacements_of_the_same_texels_draw_the_same_picture() {
        let _gpu = exclusive();
        let texture = key::texture_hash_words(4, 16, &test_texture());
        let (lo, hi) = key::index_range_words(&test_texture(), key::Mode::P4).unwrap();
        let palette = key::palette_hash(lo, hi, &test_palette(0));
        let indexed = |factor: usize, palette: &[u16]| {
            png_io::encode(
                16 * factor,
                16 * factor,
                Pixels::Indexed {
                    bits: 4,
                    indices: &upscaled_indices(factor),
                    palette,
                },
            )
            .unwrap()
        };
        // Exact key (its own palette, for the first version) 2x, and an
        // indexed `any` 3x drawn through the live palette (both versions).
        let exact = pack_folder(
            "exact",
            &[(
                format!("ac3tex-p4-{texture}-{palette}-16x16.png"),
                indexed(2, &test_palette(0)),
            )],
        );
        let live = pack_folder(
            "live",
            &[(
                format!("ac3tex-p4-{texture}-any-16x16.png"),
                indexed(3, &[0x7fff; 16]),
            )],
        );
        let words = replacement_scene();
        for scale in [1, 2, 3] {
            let Some((software, plain, _)) = run_with_pack(scale, &words, None) else {
                return;
            };
            if scale == 1 {
                assert_eq!(software, plain, "1x without a pack");
            }
            // The exact key replaces the 8 textured primitives drawn with
            // the first palette version (a folded quad would count twice),
            // `any` also the last sprite.
            for (name, pack, replaced) in [("exact", &exact, 8), ("live", &live, 9)] {
                let (_, card, count) = run_with_pack(scale, &words, Some(pack)).unwrap();
                let wrong = plain.iter().zip(&card).filter(|(a, b)| a != b).count();
                assert_eq!(wrong, 0, "{scale}x, {name} pack: {wrong} samples differ");
                assert!(count >= replaced, "{scale}x {name}: {count} replaced");
            }
        }
        for dir in [exact, live] {
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn replacements_draw_their_own_pixels_at_n_times() {
        let _gpu = exclusive();
        let texture = key::texture_hash_words(4, 16, &test_texture());
        // 32x32 for the 16x16 texels: transparent, STP and opaque columns.
        let pixel = |x: u32, y: u32| -> [u8; 4] {
            let alpha = match x {
                0..=3 => 0,
                4..=7 => 128,
                _ => 255,
            };
            [(x * 8) as u8, (y * 8) as u8, (255 - x * 6) as u8, alpha]
        };
        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, 32, 32);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let data: Vec<u8> = (0..32)
                .flat_map(|y| (0..32).flat_map(move |x| pixel(x, y)))
                .collect();
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&data).unwrap();
            writer.finish().unwrap();
        }
        let dir = pack_folder(
            "rgba",
            &[(format!("hud-ac3tex-p4-{texture}-any-16x16.png"), png)],
        );
        let clut = 480u32 << 22;
        let mut words = AREA.to_vec();
        words.extend([0x0210_4080, xy(0, 256), xy(128, 64)]);
        words.extend(upload(324, 0, 4, 16, &test_texture()));
        words.extend(upload(0, 480, 16, 1, &test_palette(0)));
        words.extend([0xe100_0005, 0x7d00_0000, xy(8, 264), clut | 0x0010]);
        for scale in [2u32, 4] {
            let Some((software, card, replaced)) = run_with_pack(scale, &words, Some(&dir)) else {
                return;
            };
            assert_eq!(replaced, 1);
            let n = scale as usize;
            let background = software[256 * 1024];
            for sy in 0..16 * n {
                for sx in 0..16 * n {
                    // The replacement pixel under this sample.
                    let (px, py) = (sx * 32 / (16 * n), sy * 32 / (16 * n));
                    let texel = replace::texel_of(pixel(px as u32, py as u32));
                    let expected = if texel == 0 { background } else { texel };
                    let got = card[(264 * n + sy) * 1024 * n + 8 * n + sx];
                    assert_eq!(got, expected, "{scale}x sample {sx},{sy}");
                }
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Movie frame `file`'s image: 64x32 pixels for 16x8 texels.
    fn movie_pixel(x: u32, y: u32, file: u32) -> [u8; 4] {
        [(x * 4) as u8, (y * 8) as u8, (file * 20) as u8, 255]
    }

    struct MovieFrames;
    impl replace::MovieImages for MovieFrames {
        fn image(
            &self,
            _: &std::sync::Arc<str>,
            file: u32,
        ) -> Option<std::sync::Arc<crate::movie_pack::RgbaImage>> {
            let pixels = (0..32)
                .flat_map(|y| (0..64).flat_map(move |x| movie_pixel(x, y, file)))
                .collect();
            Some(std::sync::Arc::new(crate::movie_pack::RgbaImage {
                width: 64,
                height: 32,
                pixels,
            }))
        }
    }

    #[test]
    fn movie_frames_draw_the_image_of_each_frame() {
        let _gpu = exclusive();
        for scale in [2u32, 4] {
            let Some(mut executor) = executor(scale) else {
                return;
            };
            let mut gpu = Gpu::new(Vram::new());
            let mut recorder = Recorder::new(executor_options(scale), &gpu);
            recorder.set_movie_images(Some(Box::new(MovieFrames)), gpu.vram());
            let feed = |recorder: &mut Recorder, gpu: &mut Gpu, words: &[u32]| {
                for &word in words {
                    recorder.gp0(word, 0, gpu.vram());
                    gpu.write_gp0(word).expect("software GPU");
                }
            };
            feed(&mut recorder, &mut gpu, &AREA);
            // More frames than movie slots: each pass decodes a 16x8 frame
            // at (832, 256) and draws it with a raw 15-bit sprite.
            for file in 0..replace::MOVIE_SLOTS + 4 {
                let texels: Vec<u16> = (0..128).map(|i| 0x8000 | (i as u16 * 37)).collect();
                feed(&mut recorder, &mut gpu, &upload(832, 256, 16, 8, &texels));
                recorder.movie_frame(replace::MovieFrame {
                    rect: super::super::tiles::Rect::new(832, 256, 16, 8),
                    key: "movie_0810".into(),
                    file,
                    stp: true,
                });
                feed(
                    &mut recorder,
                    &mut gpu,
                    &[0xe100_011d, 0x6580_8080, xy(8, 264), 0, xy(16, 8)],
                );
                executor.execute(&recorder.take_frame(gpu.vram()));
                let card = executor.read_vram();
                let n = scale as usize;
                for sy in 0..8 * n {
                    for sx in 0..16 * n {
                        // The image pixel under the sample's centre.
                        let (px, py) = ((2 * sx + 1) * 64 / (32 * n), (2 * sy + 1) * 32 / (16 * n));
                        let expected =
                            replace::movie_texel(movie_pixel(px as u32, py as u32, file), true);
                        let got = card[(264 * n + sy) * 1024 * n + 8 * n + sx];
                        assert_eq!(got, expected, "{scale}x frame {file} sample {sx},{sy}");
                    }
                }
            }
            let stats = recorder.replacer().unwrap().movie_stats;
            assert_eq!(stats.images, u64::from(replace::MOVIE_SLOTS) + 4);
            assert_eq!(executor.stats.replacements.movie_frames, stats.images);
        }
    }

    // ---- perspective-correct textures ----

    use super::super::VertexPrecision;
    use super::super::record::{self, mode};
    use std::rc::Rc;

    /// A vertex word's position.
    fn word_xy(word: u32) -> (f32, f32) {
        let x = ((word << 21) as i32) >> 21;
        let y = (((word >> 16) << 21) as i32) >> 21;
        (x as f32, y as f32)
    }

    /// A plane receding up the screen: 1/z grows linearly with y (a floor
    /// seen from above, its far edge at the top). Positions as the words.
    struct Plane;
    impl VertexPrecision for Plane {
        fn precise_vertex(&self, _: u32, word: u32) -> Option<[f32; 3]> {
            let (x, y) = word_xy(word);
            Some([x, y, 4096.0 / (y + 20.0)])
        }
    }

    /// Depths from a hash of the vertex word, spread over 1..65535 (ratios
    /// far beyond any real surface's).
    struct Scattered;
    impl VertexPrecision for Scattered {
        fn precise_vertex(&self, _: u32, word: u32) -> Option<[f32; 3]> {
            let (x, y) = word_xy(word);
            let hash = word.wrapping_mul(0x9e37_79b9) >> 8;
            let z = 65535f32.powf(hash as f32 / (1 << 24) as f32);
            Some([x, y, z.max(1.0)])
        }
    }

    /// How [`run_with`] records.
    #[derive(Clone, Default)]
    struct Recording<'a> {
        precision: Option<Rc<dyn VertexPrecision>>,
        perspective: bool,
        pack: Option<&'a std::path::Path>,
        /// Dithering above 1x.
        dither: bool,
        /// Send every textured polygon down the shader's perspective path
        /// with w = 1 (the affine mapping, through the other code).
        force: bool,
    }

    /// [`run`] recorded as `recording` says; also the recorder's counts and
    /// the vertices forced.
    fn run_with(
        scale: u32,
        words: &[u32],
        recording: &Recording,
    ) -> Option<(Vec<u16>, Vec<u16>, record::Stats, usize)> {
        let mut executor = executor(scale)?;
        let mut gpu = Gpu::new(Vram::new());
        let options = Options {
            scale,
            dither: recording.dither,
        };
        let mut recorder = Recorder::new(options, &gpu);
        recorder.precision = recording.precision.clone();
        recorder.perspective = recording.perspective;
        if let Some(pack) = recording.pack {
            recorder.set_texture_pack(Some(TexturePack::open(pack).unwrap()), gpu.vram());
        }
        for (i, &word) in words.iter().enumerate() {
            recorder.gp0(word, 0x8010_0004 + 4 * i as u32, gpu.vram());
            gpu.write_gp0(word).expect("software GPU");
        }
        let mut frame = recorder.take_frame(gpu.vram());
        let mut forced = 0;
        if recording.force {
            for vertex in frame.vertices.chunks_exact_mut(VERTEX_WORDS) {
                let bits = vertex[5];
                let polygon = bits & 7 == mode::KIND_POLY && bits & mode::EXACT == 0;
                if polygon && bits & mode::TEXTURED != 0 {
                    vertex[5] |= mode::PERSPECTIVE;
                    forced += 1;
                }
            }
        }
        executor.execute(&frame);
        Some((
            gpu.vram().words().to_vec(),
            executor.read_vram(),
            recorder.stats().clone(),
            forced,
        ))
    }

    #[test]
    fn the_perspective_path_draws_every_feature_alike() {
        let _gpu = exclusive();
        // Through the perspective path with w = 1, every textured polygon
        // must draw exactly as on the affine path: semi-transparency (each
        // function), masks, dithering, 4/8/15-bit texels, windows,
        // modulation, raw texels, and replacements.
        let texture = key::texture_hash_words(4, 16, &test_texture());
        let indexed = png_io::encode(
            32,
            32,
            Pixels::Indexed {
                bits: 4,
                indices: &upscaled_indices(2),
                palette: &[0x7fff; 16],
            },
        )
        .unwrap();
        let pack = pack_folder(
            "forced",
            &[(format!("ac3tex-p4-{texture}-any-16x16.png"), indexed)],
        );
        let mut scenes = vec![
            ("scene", scene(), None, false),
            ("scene dithered", scene(), None, true),
            (
                "replacements",
                replacement_scene(),
                Some(pack.as_path()),
                false,
            ),
        ];
        for seed in 1..=3 {
            scenes.push(("random", random_scene(seed, 2500), None, seed == 3));
        }
        // Apple GPUs (Metal) interpolate the constant 1/w with rounding, so
        // a texel boundary may flip at a sample in a million; elsewhere
        // (an NVIDIA card, lavapipe) the paths are exact.
        let metal = device()
            .is_some_and(|(device, _)| device.adapter_info().backend == wgpu::Backend::Metal);
        for scale in [2, 4] {
            let allowed = if metal {
                (1024 * 512 * scale * scale) as usize / 1_000_000 + 1
            } else {
                0
            };
            for (name, words, pack, dither) in &scenes {
                let plain = Recording {
                    pack: *pack,
                    dither: *dither,
                    ..Recording::default()
                };
                let Some((_, affine, _, _)) = run_with(scale, words, &plain) else {
                    return;
                };
                let forced = Recording {
                    force: true,
                    ..plain.clone()
                };
                let (_, card, _, count) = run_with(scale, words, &forced).unwrap();
                assert!(count >= 12, "{name}: {count} vertices");
                let wrong = affine.iter().zip(&card).filter(|(a, b)| a != b).count();
                if wrong > 0 {
                    eprintln!("{scale}x {name}: {wrong} of {} samples differ", card.len());
                }
                assert!(wrong <= allowed, "{scale}x {name}: {wrong} samples differ");
            }
        }
        std::fs::remove_dir_all(pack).unwrap();
    }

    /// A 64x64 15-bit texture at (512, 256) (page 0x118) whose texel holds
    /// its coordinates: v in red and green bit 0, u in blue (never 0).
    fn coordinate_texture() -> Vec<u32> {
        let texel = |u: u32, v: u32| (v & 31) | (((v >> 5) | 16) << 5) | ((u & 31) << 10);
        let mut words = vec![0xa000_0000, xy(512, 256), xy(64, 64)];
        for v in 0..64 {
            for u in (0..64).step_by(2) {
                words.push(texel(u, v) | (texel(u + 1, v) << 16));
            }
        }
        words
    }

    /// The texture coordinate v a sample of [`coordinate_texture`] shows.
    fn texel_v(sample: u16) -> i32 {
        i32::from(sample & 31) | (i32::from((sample >> 5) & 1) << 5)
    }

    #[test]
    fn perspective_maps_a_receding_plane() {
        let _gpu = exclusive();
        // A raw-textured quad from (100, 100) to (164, 300), v 0..63 down
        // the screen, on the receding plane: its far (top) edge is 2.7 times
        // as deep as its near edge.
        let mut words = AREA.to_vec();
        words.extend(coordinate_texture());
        words.extend([
            0x2d80_8080,
            xy(100, 100),
            0x0000_0000,
            xy(164, 100),
            (0x118 << 16) | 0x003f,
            xy(100, 300),
            0x3f00,
            xy(164, 300),
            0x3f3f,
        ]);
        let scale = 4;
        let off = Recording {
            precision: Some(Rc::new(Plane)),
            ..Recording::default()
        };
        let on = Recording {
            perspective: true,
            ..off.clone()
        };
        let Some((_, card, stats, _)) = run_with(scale, &words, &on) else {
            return;
        };
        assert_eq!(stats.perspective_triangles, 2);
        let (_, affine, _, _) = run_with(scale, &words, &off).unwrap();
        // Down the middle column (x 132, over both halves of the quad):
        // texture v follows the perspective division, smoothly (no seam on
        // the diagonal), and differs from the affine mapping.
        let n = scale as usize;
        let width = 1024 * n;
        let column = 132 * n + n / 2;
        let (q0, q1) = (120.0 / 4096.0, 320.0 / 4096.0);
        let (mut previous, mut furthest) = (None, 0);
        for row in 100 * n + 2..300 * n - 2 {
            // The sample's 1x position, as a fraction from the top edge.
            let y = (row as f64 + 0.5) / f64::from(scale) - 0.5;
            let s = (y - 100.0) / 200.0;
            let q = (1.0 - s) * q0 + s * q1;
            let expected = (63.0 * s * q1 / q + 0.5).floor() as i32;
            let got = texel_v(card[row * width + column]);
            assert!(
                (got - expected).abs() <= 1,
                "row {row}: v {got}, expected {expected}"
            );
            if let Some(previous) = previous {
                assert!(
                    (0..=1).contains(&(got - previous)),
                    "row {row}: {previous} to {got}"
                );
            }
            previous = Some(got);
            furthest = furthest.max((got - texel_v(affine[row * width + column])).abs());
        }
        // At the middle the perspective v is about 46, the affine 32.
        assert!(furthest >= 12, "{furthest}");
        // The quad covers the same samples with perspective as without.
        let covered = |vram: &[u16]| vram.iter().filter(|&&p| p != 0).count();
        assert_eq!(covered(&card), covered(&affine));
    }

    #[test]
    fn perspective_keeps_coverage_and_the_triangles_texels() {
        let _gpu = exclusive();
        // A 4-bit page (x 640) whose texels the triangles map (u, v 16..47)
        // use palette entries 1..14; every other texel is entry 15, red,
        // which must never be drawn.
        let mut words = AREA.to_vec();
        words.extend([0x0210_1010, xy(0, 0), xy(512, 256)]);
        let index = |u: u32, v: u32| {
            if (16..48).contains(&u) && (16..48).contains(&v) {
                1 + (u + v) % 14
            } else {
                15
            }
        };
        words.extend([0xa000_0000, xy(640, 0), xy(64, 64)]);
        for v in 0..64 {
            for word in 0..32 {
                let u = word * 8;
                words.push((0..8).fold(0, |acc, k| acc | (index(u + k, v) << (4 * k))));
            }
        }
        let red = 0x001f;
        let palette: Vec<u16> = (0..16u16)
            .map(|i| match i {
                0 => 0,
                15 => red,
                _ => 0x7c00 | (i << 5),
            })
            .collect();
        words.extend(upload(0, 480, 16, 1, &palette));
        // Raw-textured triangles and quads, large, small and slivers, with
        // depths up to 65535 times apart.
        let mut r = Random(5);
        let clut = 480u32 << 22;
        let page = 0x000au32 << 16;
        let uv = |r: &mut Random| r.range(16, 47) as u32 | ((r.range(16, 47) as u32) << 8);
        for i in 0..300 {
            let (cx, cy) = (r.range(20, 480), r.range(20, 230));
            let size = [4, 30, 120][i % 3];
            let height = if i % 5 == 0 { 2 } else { size };
            let corner = |r: &mut Random| {
                xy(
                    (cx + r.range(-size, size)).clamp(0, 511),
                    (cy + r.range(-height, height)).clamp(0, 255),
                )
            };
            let op = if i % 2 == 0 { 0x2500_0000 } else { 0x2d00_0000 };
            words.extend([op, corner(&mut r), clut | uv(&mut r)]);
            words.extend([
                corner(&mut r),
                page | uv(&mut r),
                corner(&mut r),
                uv(&mut r),
            ]);
            if i % 2 == 1 {
                words.extend([corner(&mut r), uv(&mut r)]);
            }
        }
        let off = Recording {
            precision: Some(Rc::new(Scattered)),
            ..Recording::default()
        };
        let on = Recording {
            perspective: true,
            ..off.clone()
        };
        let Some((_, card, stats, _)) = run_with(4, &words, &on) else {
            return;
        };
        assert!(stats.perspective_triangles > 250, "{stats:?}");
        let (_, affine, _, _) = run_with(4, &words, &off).unwrap();
        // The drawn area (x 0..511, y 0..255 at 4x).
        let background = 0x0842;
        let mut changed = 0;
        for row in 0..256 * 4 {
            for column in 0..512 * 4 {
                let at = row * 4096 + column;
                let (a, b) = (affine[at], card[at]);
                assert_eq!(
                    a == background,
                    b == background,
                    "coverage differs at sample ({column}, {row})"
                );
                assert_ne!(
                    b & 0x7fff,
                    red,
                    "a texel outside the triangles at sample ({column}, {row})"
                );
                changed += usize::from(a != b);
            }
        }
        assert!(changed > 10_000, "{changed}");
    }

    #[test]
    fn perspective_replacements_draw_the_same_texels() {
        let _gpu = exclusive();
        // Identity packs (2x exact, 3x `any` through the live palette) draw
        // the same picture as the original textures with perspective too:
        // the clamped coordinates stay inside each replacement.
        let texture = key::texture_hash_words(4, 16, &test_texture());
        let (lo, hi) = key::index_range_words(&test_texture(), key::Mode::P4).unwrap();
        let palette = key::palette_hash(lo, hi, &test_palette(0));
        let indexed = |factor: usize, palette: &[u16]| {
            png_io::encode(
                16 * factor,
                16 * factor,
                Pixels::Indexed {
                    bits: 4,
                    indices: &upscaled_indices(factor),
                    palette,
                },
            )
            .unwrap()
        };
        let exact = pack_folder(
            "perspective-exact",
            &[(
                format!("ac3tex-p4-{texture}-{palette}-16x16.png"),
                indexed(2, &test_palette(0)),
            )],
        );
        let live = pack_folder(
            "perspective-live",
            &[(
                format!("ac3tex-p4-{texture}-any-16x16.png"),
                indexed(3, &[0x7fff; 16]),
            )],
        );
        let words = replacement_scene();
        for scale in [2, 3] {
            let off = Recording {
                precision: Some(Rc::new(Plane)),
                ..Recording::default()
            };
            let on = Recording {
                perspective: true,
                ..off.clone()
            };
            let Some((_, plain, stats, _)) = run_with(scale, &words, &on) else {
                return;
            };
            assert!(stats.perspective_triangles >= 4, "{stats:?}");
            let (_, affine, _, _) = run_with(scale, &words, &off).unwrap();
            assert!(plain != affine, "{scale}x: perspective changed nothing");
            for pack in [&exact, &live] {
                let with_pack = Recording {
                    pack: Some(pack.as_path()),
                    ..on.clone()
                };
                let (_, card, stats, _) = run_with(scale, &words, &with_pack).unwrap();
                assert!(stats.perspective_triangles >= 4);
                let wrong = plain.iter().zip(&card).filter(|(a, b)| a != b).count();
                assert_eq!(
                    wrong,
                    0,
                    "{scale}x {}: {wrong} samples differ",
                    pack.display()
                );
            }
        }
        for dir in [exact, live] {
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn display_picture_widens_channels_like_the_software_path() {
        let _gpu = exclusive();
        let Some(mut executor) = executor(2) else {
            return;
        };
        let mut gpu = Gpu::new(Vram::new());
        let mut recorder = Recorder::new(executor_options(2), &gpu);
        let mut words = AREA.to_vec();
        words.extend([0x6000_80f8, 0x0000_0000, 0x0002_0004]);
        for word in words {
            recorder.gp0(word, 0, gpu.vram());
            gpu.write_gp0(word).unwrap();
        }
        executor.execute(&recorder.take_frame(gpu.vram()));
        let area = DisplayArea {
            x: 0,
            y: 0,
            width: 8,
            height: 4,
        };
        let (rgb, width, height) = executor.read_display(area).expect("picture");
        assert_eq!((width, height), (16, 8));
        let [r, g, b, _] = ac3_graphics::rgba8(gpu.vram().words()[0]);
        let expected = (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b);
        assert_eq!(rgb[0], expected);
        assert_eq!(rgb[7], expected, "x 3 (1x) is in the tile");
        assert_eq!(rgb[8], 0, "x 4 (1x) is not");
        assert!(
            executor
                .read_display(DisplayArea { x: 1020, ..area })
                .is_none()
        );
        assert_eq!(scaled(Rect::new(3, 4, 5, 6), 3), [9, 12, 15, 18]);
    }

    /// The JP credits: a single 480i buffer drawn before the VBlank and
    /// cleared after it (frame end's PutDrawEnv). The screen shows the
    /// field, the picture before the clear; a VBlank with no write after it
    /// shows the area again.
    #[test]
    fn the_shown_field_is_the_picture_before_the_clear() {
        let _gpu = exclusive();
        for scale in [1, 2] {
            let Some(mut executor) = executor(scale) else {
                return;
            };
            let mut gpu = Gpu::new(Vram::new());
            let mut recorder = Recorder::new(executor_options(scale), &gpu);
            fn feed(recorder: &mut Recorder, gpu: &mut Gpu, words: &[u32]) {
                for &word in words {
                    recorder.gp0(word, 0, gpu.vram());
                    gpu.write_gp0(word).unwrap();
                }
            }
            let area = DisplayArea {
                x: 0,
                y: 0,
                width: 320,
                height: 480,
            };
            let red = 0x00ff_0000;
            feed(&mut recorder, &mut gpu, &AREA);
            // The credits: red; the VBlank; the clear: black.
            let fill = |rgb: u32| [0x0200_0000 | rgb, 0x0000_0000, 0x01e0_0140];
            feed(&mut recorder, &mut gpu, &fill(0xff));
            recorder.vblank();
            recorder.show(Some(area), gpu.vram());
            feed(&mut recorder, &mut gpu, &fill(0));
            let frame = recorder.take_frame(gpu.vram());
            assert_eq!(frame.shown, Shown::Copy);
            executor.execute(&frame);
            let (rgb, width, height) = executor.read_display(area).expect("picture");
            assert_eq!(
                (width, height),
                (320 * scale as usize, 480 * scale as usize)
            );
            assert!(rgb.iter().all(|&p| p == red), "{scale}x: the field");
            // Another area (the display moved) is drawn live.
            let other = DisplayArea {
                height: 240,
                ..area
            };
            let (rgb, ..) = executor.read_display(other).expect("picture");
            assert!(rgb.iter().all(|&p| p == 0), "{scale}x: the area now");
            // The next pass: its VBlank, no write after it.
            recorder.vblank();
            let frame = recorder.take_frame(gpu.vram());
            assert_eq!(frame.shown, Shown::Live);
            executor.execute(&frame);
            let (rgb, ..) = executor.read_display(area).expect("picture");
            assert!(rgb.iter().all(|&p| p == 0), "{scale}x: the area now");
        }
    }

    // ---- effects ----

    use super::super::fx::{self, DebugView, SKY_Z};

    /// `commands` (the first one fed before the list: the set-up and the
    /// big uploads) as a JP flight frame's world, one command per
    /// ordering-table entry from 4094 down, after a sky fill; run through a
    /// recorder with `effects` at `scale` and the software GPU. Returns the
    /// software VRAM, the card's, the frame and the effects' counts.
    /// The software VRAM, the card's, the frame and the effects' counts.
    type Flight = (Vec<u16>, Vec<u16>, Frame, Option<fx::FxStats>);

    fn run_flight(
        scale: u32,
        commands: &[Vec<u32>],
        effects: Option<fx::Config>,
        precision: Option<Rc<dyn VertexPrecision>>,
    ) -> Option<Flight> {
        let mut packets = vec![(4095, vec![0x0230_2010, 0, 0x00ff_0400])];
        for (index, command) in commands[1..].iter().enumerate() {
            if !command.is_empty() {
                packets.push((4094 - index as u32, command.clone()));
            }
        }
        packets.push((1, vec![0x6000_ffff, 0x0004_0004, 0x0004_0004]));
        let (mut executor, gpu, frame, stats) =
            run_packets(scale, &commands[0], &packets, effects, precision)?;
        Some((
            gpu.vram().words().to_vec(),
            executor.read_vram(),
            frame,
            stats,
        ))
    }

    /// `setup` fed directly, then the flight list of `packets` ((entry,
    /// words); `fx::walk` tests), through a recorder with `effects` at
    /// `scale` and the software GPU; the executor ran the frame.
    fn run_packets(
        scale: u32,
        setup: &[u32],
        packets: &[(u32, Vec<u32>)],
        effects: Option<fx::Config>,
        precision: Option<Rc<dyn VertexPrecision>>,
    ) -> Option<(Executor, Gpu, Frame, Option<fx::FxStats>)> {
        let mut executor = executor(scale)?;
        let mut gpu = Gpu::new(Vram::new());
        let mut recorder = Recorder::new(executor_options(scale), &gpu);
        recorder.precision = precision;
        recorder.set_effects(effects);
        for &word in setup {
            recorder.gp0(word, 0, gpu.vram());
            gpu.write_gp0(word).expect("software GPU");
        }
        let mut ram = fx::scene::tests::flight_ram();
        let start = fx::walk::tests::flight_list_in(&mut ram, packets);
        recorder.list_begin(&ram, start, gpu.vram(), false);
        for (address, words) in crate::flight_host::walk_list_addressed(&ram, start).unwrap() {
            for (i, &word) in words.iter().enumerate() {
                recorder.gp0(word, address + 4 * (i as u32 + 1), gpu.vram());
                gpu.write_gp0(word).expect("software GPU");
            }
        }
        recorder.list_end(gpu.vram());
        let frame = recorder.take_frame(gpu.vram());
        executor.execute(&frame);
        let stats = recorder.effects_stats().cloned();
        Some((executor, gpu, frame, stats))
    }

    /// A floor receding up the screen: 1/z = (y + 120) / 4096, linear in
    /// screen y, from y -100 (far) to 100 (near). Positions as the words.
    struct Floor;
    impl VertexPrecision for Floor {
        fn precise_vertex(&self, _: u32, word: u32) -> Option<[f32; 3]> {
            let (x, y) = word_xy(word);
            Some([x, y, 4096.0 / (y + 120.0)])
        }
    }

    fn effects(debug: DebugView) -> Option<fx::Config> {
        Some(fx::Config {
            enabled: true,
            debug,
            x_scale: None,
        })
    }

    /// Effects on with no effect (a no-op post) draw exactly the picture
    /// without them: deferred semi-transparency under the later opaque
    /// primitives (the draw-order test), the early replays before reads of
    /// their pixels, across random commands of every kind (masks, copies,
    /// uploads, captures, texture windows, draw areas).
    #[test]
    fn effects_with_a_no_op_post_keep_the_picture() {
        let _gpu = exclusive();
        // As the world draws: in one draw area, without mask bits (most
        // semi-transparent primitives deferred); and as random as before.
        // Without lines: above 1x a line reaches half a pixel past its ends,
        // outside the rectangle the recorder places it by (an approximation
        // older than effects), so a copy reading beside it may run before
        // or after it as the layering falls, and effects change the
        // layering. `deferred_lines_keep_the_picture` has lines.
        let calm = |commands: Vec<Vec<u32>>, calmed: bool| -> Vec<Vec<u32>> {
            commands
                .into_iter()
                .enumerate()
                .map(|(index, command)| match command.first().map(|w| w >> 24) {
                    Some(0x40..=0x5f) if index > 0 => Vec::new(),
                    Some(0xe3) if index > 0 && calmed => Vec::new(),
                    Some(0xe1) if index > 0 && calmed => {
                        command.into_iter().filter(|w| w >> 24 != 0xe6).collect()
                    }
                    _ => command,
                })
                .collect()
        };
        let mut deferred = 0;
        let mut cases = vec![
            (11, 2, true),
            (12, 2, true),
            (13, 3, true),
            (14, 4, true),
            (15, 2, false),
            (16, 4, false),
        ];
        // `AC3_FX_SEEDS=N`: N more of each.
        let more: u64 = std::env::var("AC3_FX_SEEDS")
            .ok()
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        for seed in 100..100 + more {
            cases.push((seed, 2 + (seed % 3) as u32, true));
            cases.push((seed + 1000, 2 + (seed % 3) as u32, false));
        }
        for (seed, scale, calmed) in cases {
            let commands = calm(random_commands(seed, 1500), calmed);
            let Some((_, off, frame, _)) = run_flight(scale, &commands, None, None) else {
                return;
            };
            assert!(frame.fx.is_none());
            let (_, on, frame, stats) =
                run_flight(scale, &commands, effects(DebugView::Off), None).unwrap();
            let stats = stats.unwrap();
            assert!(frame.fx.is_some(), "an effects frame");
            assert_eq!(stats.replayed, stats.deferred);
            deferred += stats.deferred;
            let differing = off.iter().zip(&on).filter(|(a, b)| a != b).count();
            if differing == 0 {
                continue;
            }
            // The first command after which the two differ.
            let differs = |count: usize| {
                let (_, off, ..) = run_flight(scale, &commands[..count], None, None).unwrap();
                let (_, on, ..) =
                    run_flight(scale, &commands[..count], effects(DebugView::Off), None).unwrap();
                off.iter().zip(&on).filter(|(a, b)| a != b).count()
            };
            let (mut good, mut bad) = (1, commands.len());
            while bad - good > 1 {
                let middle = (good + bad) / 2;
                if differs(middle) > 0 {
                    bad = middle;
                } else {
                    good = middle;
                }
            }
            let context: Vec<String> = commands[bad.saturating_sub(6)..bad]
                .iter()
                .map(|command| format!("{:08x?}", &command[..command.len().min(12)]))
                .collect();
            let (software, off, ..) = run_flight(scale, &commands[..bad], None, None).unwrap();
            let (_, on, ..) =
                run_flight(scale, &commands[..bad], effects(DebugView::Off), None).unwrap();
            let width = 1024 * scale as usize;
            let pixels: Vec<String> = off
                .iter()
                .zip(&on)
                .enumerate()
                .filter(|(_, (a, b))| a != b)
                .take(8)
                .map(|(i, (a, b))| {
                    let (x, y) = (i % width, i / width);
                    let (x1, y1) = (x / scale as usize, y / scale as usize);
                    format!(
                        "({x}, {y}) = 1x ({x1}, {y1}): off {a:04x} on {b:04x} software {:04x}",
                        software[y1 * 1024 + x1]
                    )
                })
                .collect();
            panic!(
                "seed {seed} at {scale}x: {differing} pixels differ ({stats:?}); command {} \
                 starts it ({} pixels: {}); the commands before it:\n{}",
                bad - 1,
                differs(bad),
                pixels.join(", "),
                context.join("\n")
            );
        }
        assert!(deferred > 100, "{deferred} deferred");
    }

    /// Deferred semi-transparent lines, polylines, tiles and polygons, some
    /// partly under later opaque ones, some crossing opaque lines: the
    /// picture is the one without effects.
    #[test]
    fn deferred_lines_keep_the_picture() {
        let _gpu = exclusive();
        let setup = vec![
            0xe300_0000,
            0xe400_0000 | 319 | (239 << 10),
            0xe500_0000 | 160 | (120 << 11),
            0xe100_0020,
        ];
        let commands = vec![
            setup,
            vec![
                0x3800_2040,
                xy(-150, -100),
                0x0060_4020,
                xy(150, -100),
                0x0020_8040,
                xy(-150, 100),
                0x0040_2060,
                xy(150, 100),
            ],
            vec![0x5200_ff00, xy(-140, -90), 0x0000_00ff, xy(140, 90)],
            vec![
                0x2800_8080,
                xy(0, -100),
                xy(150, -100),
                xy(0, 100),
                xy(150, 100),
            ],
            vec![
                0x4a40_4040,
                xy(-140, 80),
                xy(-20, -80),
                xy(120, 60),
                0x5555_5555,
            ],
            vec![0x4000_ffff, xy(-100, 0), xy(100, 10)],
            vec![0x6280_2000, xy(-50, -50), xy(64, 64)],
            vec![0x6000_00ff, xy(-30, -30), xy(16, 16)],
            vec![
                0x3200_ff00,
                xy(-120, -60),
                0x0000_00ff,
                xy(110, -20),
                0x00ff_0000,
                xy(-10, 90),
            ],
            vec![0x4200_8080, xy(-150, -95), xy(150, -95)],
        ];
        for scale in [2, 3, 4] {
            let Some((_, off, ..)) = run_flight(scale, &commands, None, None) else {
                return;
            };
            let (_, on, _, stats) =
                run_flight(scale, &commands, effects(DebugView::Off), None).unwrap();
            let stats = stats.unwrap();
            assert_eq!(stats.deferred, 5, "{stats:?}");
            let differing = off.iter().zip(&on).filter(|(a, b)| a != b).count();
            assert_eq!(differing, 0, "{scale}x: {stats:?}");
        }
    }

    /// The z target: a receding plane (1/z linear in screen y) drawn in the
    /// world holds its perspective-interpolated view z; the rest of the area
    /// holds the sky; the depth view draws over the world's area only.
    #[test]
    fn the_z_target_holds_view_depths_and_the_depth_view_shows_them() {
        let _gpu = exclusive();
        let scale = 2;
        // The draw area 320 x 240 at (0, 256), the offset at its centre.
        let setup = [
            0xe300_0000 | (256 << 10),
            0xe400_013f | (495 << 10),
            0xe500_0000 | 160 | (376 << 11),
        ];
        // The sky (a fill over the area), then a Gouraud quad from y -100
        // (far) to 100 (near), x -150..150, in the world.
        let packets = vec![
            (4095, vec![0x0230_2010, 0x0100_0000, 0x00f0_0140]),
            (
                3000,
                vec![
                    0x3800_8080,
                    xy(-150, -100),
                    0x0080_8080,
                    xy(150, -100),
                    0x0080_8080,
                    xy(-150, 100),
                    0x0080_8080,
                    xy(150, 100),
                ],
            ),
        ];
        let floor: Option<Rc<dyn VertexPrecision>> = Some(Rc::new(Floor));
        let Some((mut executor, ..)) = run_packets(scale, &setup, &packets, None, floor.clone())
        else {
            return;
        };
        let off = executor.read_vram();
        let width = 1024 * scale as usize;
        let vram = |pixels: &[u16], x: usize, y: usize| pixels[(512 + y) * width + x];
        for (debug, changes) in [(DebugView::Off, false), (DebugView::Depth, true)] {
            let (mut executor, _, frame, stats) =
                run_packets(scale, &setup, &packets, effects(debug), floor.clone()).unwrap();
            assert_eq!(stats.unwrap().depth_triangles, 2);
            assert_eq!(
                frame.fx.as_deref().unwrap().world_area,
                Rect::new(0, 256, 320, 240)
            );
            let (snapshot, rect) = executor.read_fx_snapshot().expect("a snapshot");
            assert_eq!(rect, [0, 512, 640, 480]);
            let at = |x: usize, y: usize| snapshot[y * 640 + x];
            for row in [60, 120, 240, 360, 420] {
                let [z, order] = at(320, row);
                assert_eq!(order, 1.0, "row {row}: the quad");
                // Screen y of the N x row (from the offset), and its 1/z.
                let y = (row as f32 + 0.5) / scale as f32 - 120.0;
                let expected = (y + 120.0) / 4096.0;
                assert!(
                    (1.0 / z - expected).abs() < 0.03 * expected,
                    "row {row}: 1/z {} expected {expected}",
                    1.0 / z
                );
            }
            // Beside the quad (screen x beyond 150): the sky.
            assert_eq!(at(630, 240), [SKY_Z, 0.0]);
            // The depth view draws over the world's area, nothing else.
            let on = executor.read_vram();
            let inside = |i: usize| (i % width) < 640 && (512..992).contains(&(i / width));
            let (mut changed_inside, mut changed_outside) = (0, 0);
            for (i, (a, b)) in off.iter().zip(&on).enumerate() {
                if a != b {
                    if inside(i) {
                        changed_inside += 1;
                    } else {
                        changed_outside += 1;
                    }
                }
            }
            assert_eq!(changed_outside, 0, "{debug:?}");
            assert_eq!(changed_inside > 0, changes, "{debug:?}: {changed_inside}");
            if changes {
                // Near (bottom) is whiter than far (top): more blue; the
                // sky is dark blue.
                let blue = |p: u16| (p >> 10) & 31;
                assert!(blue(vram(&on, 320, 420)) > blue(vram(&on, 320, 60)));
                let sky = vram(&on, 630, 240);
                assert!(blue(sky) >= 10 && sky & 31 <= 2, "{sky:#06x}");
            }
        }
    }

    /// The write-back helper: whole 5-bit steps come back exactly; values
    /// between two steps dither to both, on average the value.
    #[test]
    fn the_write_back_dither_keeps_whole_steps_and_averages_the_rest() {
        let _gpu = exclusive();
        let Some((device, queue)) = device() else {
            return;
        };
        let shader = format!(
            "{}\n{}",
            fx::card::FX_COMMON_WGSL,
            r#"
@vertex
fn vs(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let p = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
}
@fragment
fn fs(@builtin(position) position: vec4<f32>) -> @location(0) u32 {
    let p = vec2<u32>(position.xy);
    if p.y < 128u {
        // Every colour: decoded and encoded again.
        return fx_encode(fx_decode(p.x + p.y * 256u), p);
    }
    // A third of the way from step 10 to 11.
    return fx_encode(vec3<f32>((10.0 + 1.0 / 3.0) / 31.0), p);
}
"#
        );
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(shader.into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: None,
            layout: None,
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(VRAM_FORMAT.into())],
            }),
            multiview_mask: None,
            cache: None,
        });
        let target = texture(
            device,
            "test",
            (256, 192),
            VRAM_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let view = target.create_view(&Default::default());
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
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
            pass.set_pipeline(&pipeline);
            pass.draw(0..3, 0..1);
        }
        queue.submit([encoder.finish()]);
        let mut executor = executor(1).unwrap();
        let words: Vec<u16> = executor
            .read_texture(&target, 256, 192, 2)
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect();
        for (i, &word) in words[..256 * 128].iter().enumerate() {
            assert_eq!(word, i as u16 & 0x7fff, "colour {i:#06x}");
        }
        let red: Vec<u16> = words[256 * 128..].iter().map(|w| w & 31).collect();
        assert!(red.iter().all(|&r| r == 10 || r == 11));
        let mean = red.iter().map(|&r| f64::from(r)).sum::<f64>() / red.len() as f64;
        assert!((mean - (10.0 + 1.0 / 3.0)).abs() < 0.02, "{mean}");
    }
}
