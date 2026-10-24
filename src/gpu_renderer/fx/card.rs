//! The card side of the effects: the z target, the effects frames'
//! pipelines, the depth snapshot, the post pass's effects and debug views
//! ([`FxCard`], owned by the [`Executor`](super::super::Executor), made at
//! the first effects frame), and what a [`CardEffect`] gets ([`Context`]).
use super::super::execute::{self, DRAW_SHADER, VRAM_FORMAT, scaled, texture, uniform_entry};
use super::super::record::VERTEX_WORDS;
use super::super::tiles::{HEIGHT, Rect, WIDTH};
use super::{DebugView, FX_VERTEX_WORDS, FxFrame, Pipe, Scene};
use std::any::Any;

/// The z target's format: r view z, g draw order.
pub const ZORDER_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rg32Float;
/// The effects' shared WGSL (the scene uniform `FxScene`, `fx_encode` and
/// `fx_decode`); `docs/EFFECTS.md`.
pub const FX_COMMON_WGSL: &str = include_str!("fx_common.wgsl");
pub const FX_DRAW_SHADER: &str = include_str!("fx_draw.wgsl");
const FX_DEBUG_SHADER: &str = include_str!("fx_debug.wgsl");

/// The effects frames' drawing shader: `draw.wgsl` with its entry points as
/// functions (the renderer's own module stays exactly that file), then
/// [`FX_COMMON_WGSL`] and [`FX_DRAW_SHADER`].
pub fn fx_draw_source() -> String {
    let draw = DRAW_SHADER.replace("\r\n", "\n");
    let renamed = draw
        .replacen(
            "@vertex\nfn vs_draw(v: VertexInput) -> Varyings {",
            "fn draw_vertex(v: VertexInput) -> Varyings {",
            1,
        )
        .replacen(
            "@fragment\nfn fs_draw(in: Varyings) -> @location(0) u32 {",
            "fn draw_pixel(in: Varyings) -> u32 {",
            1,
        );
    format!("{renamed}\n{FX_COMMON_WGSL}\n{FX_DRAW_SHADER}")
}

/// The card side of an effect: work at world begin (drawing into VRAM and
/// the z target, under the game's world) and in the post pass (reading the
/// picture and the depth snapshot, writing the picture). Registered with
/// [`Executor::add_effect`](super::super::Executor::add_effect).
pub trait CardEffect {
    /// Its name: the data its recorder side ([`super::RecordEffect::take`])
    /// handed over arrives in [`Context::data`].
    fn name(&self) -> &'static str;
    /// World begin: `ctx.vram` and `ctx.zorder` are the targets (draw with
    /// two colour targets, [`VRAM_FORMAT`] and [`ZORDER_FORMAT`]; write the
    /// draw order [`super::EFFECT_ORDER`] for effect geometry, or 0 for
    /// geometry every world primitive covers, like the far terrain).
    fn begin(&mut self, _ctx: &mut Context<'_>) {}
    /// World end: `ctx.shadow` holds the picture over the area,
    /// `ctx.snapshot` its depths; write `ctx.vram` (through `fx_encode`).
    fn post(&mut self, _ctx: &mut Context<'_>) {}
    /// After the deferred primitives (smoke, explosions, trails), before
    /// the flare and the HUD slots: `ctx.shadow` holds the picture with
    /// them over the area, `ctx.snapshot` the depths of the post; write
    /// `ctx.vram` (through `fx_encode`). Bloom and lens flares run here.
    fn late(&mut self, _ctx: &mut Context<'_>) {}
}

/// What a [`CardEffect`] works with.
pub struct Context<'a> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub encoder: &'a mut wgpu::CommandEncoder,
    /// The internal resolution N.
    pub scale: u32,
    /// The world's draw area (1x VRAM pixels), and at N x (x, y, w, h).
    pub area: Rect,
    pub pixels: [u32; 4],
    /// The N x VRAM ([`VRAM_FORMAT`]: PlayStation pixels), the target.
    pub vram: &'a wgpu::TextureView,
    pub vram_texture: &'a wgpu::Texture,
    /// Its copy, up to date over the area at the post (after one effect
    /// writes, [`Context::sync_area`] brings it up to date again).
    pub shadow: &'a wgpu::TextureView,
    pub shadow_texture: &'a wgpu::Texture,
    /// The z target (N x, the whole VRAM; [`ZORDER_FORMAT`]).
    pub zorder: &'a wgpu::TextureView,
    /// At the post and the late step: the area's depths, its pixel 0, 0 at
    /// the area's origin.
    pub snapshot: Option<&'a wgpu::TextureView>,
    pub scene: &'a Scene,
    /// The whole effects frame: the world area, the markers, the sky
    /// slot's commands, the draw orders' classes.
    pub frame: &'a FxFrame,
    /// The scene as a uniform (`FxScene` of [`FX_COMMON_WGSL`]).
    pub scene_buffer: &'a wgpu::Buffer,
    pub debug: DebugView,
    /// What the effect's recorder side handed over this frame.
    pub data: Option<&'a (dyn Any + Send + Sync)>,
}

impl Context<'_> {
    /// Copy the area of `vram` into `shadow`.
    pub fn sync_area(&mut self) {
        let [x, y, w, h] = self.pixels;
        self.encoder.copy_texture_to_texture(
            region(self.vram_texture, x, y),
            region(self.shadow_texture, x, y),
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
    }
}

fn region(texture: &wgpu::Texture, x: u32, y: u32) -> wgpu::TexelCopyTextureInfo<'_> {
    wgpu::TexelCopyTextureInfo {
        texture,
        mip_level: 0,
        origin: wgpu::Origin3d { x, y, z: 0 },
        aspect: wgpu::TextureAspect::All,
    }
}

/// The depth snapshot: the world area's z and draw order at N x.
struct Snapshot {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    /// The deferred pipeline's group 1, and the debug view's group.
    group: wgpu::BindGroup,
    debug_group: wgpu::BindGroup,
}

/// What the card has done for effects.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct CardStats {
    pub frames: u64,
    pub snapshots: u64,
    pub posts: u64,
    pub debug_views: u64,
    pub effect_calls: u64,
}

/// The effects' card resources; see the module documentation.
pub struct FxCard {
    scale: u32,
    zorder: wgpu::Texture,
    pub zorder_view: wgpu::TextureView,
    snapshot: Option<Snapshot>,
    /// The size (N x) and origin (N x) of the last snapshot.
    snapshot_rect: [u32; 4],
    fx_layout: wgpu::BindGroupLayout,
    debug_layout: wgpu::BindGroupLayout,
    globals: wgpu::Buffer,
    pub scene_buffer: wgpu::Buffer,
    pub world: wgpu::RenderPipeline,
    pub zclear: wgpu::RenderPipeline,
    pub deferred: wgpu::RenderPipeline,
    debug: wgpu::RenderPipeline,
    pub vertex_buffer: wgpu::Buffer,
    pub stats: CardStats,
}

fn float_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

impl FxCard {
    /// Set up on `device` at `scale`, the renderer's draw bind group layout
    /// `draw_layout` (group 0 of the drawing pipelines).
    pub fn new(device: &wgpu::Device, scale: u32, draw_layout: &wgpu::BindGroupLayout) -> Self {
        use wgpu::TextureUsages as Usage;
        let zorder = texture(
            device,
            "ac3 fx zorder",
            (WIDTH as u32 * scale, HEIGHT as u32 * scale),
            ZORDER_FORMAT,
            Usage::RENDER_ATTACHMENT | Usage::TEXTURE_BINDING | Usage::COPY_SRC,
        );
        let zorder_view = zorder.create_view(&Default::default());
        let fx_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ac3 fx deferred"),
            entries: &[
                float_entry(0),
                uniform_entry(1, wgpu::ShaderStages::FRAGMENT),
            ],
        });
        let debug_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ac3 fx debug"),
            entries: &[
                uniform_entry(0, wgpu::ShaderStages::FRAGMENT),
                float_entry(1),
                execute::uint_texture_entry(2),
            ],
        });
        let globals = execute::buffer(device, "ac3 fx globals", 32, wgpu::BufferUsages::UNIFORM);
        let scene_buffer = execute::buffer(
            device,
            "ac3 fx scene",
            (super::scene::UNIFORM_WORDS * 4) as u64,
            wgpu::BufferUsages::UNIFORM,
        );
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ac3 fx draw"),
            source: wgpu::ShaderSource::Wgsl(fx_draw_source().into()),
        });
        let plain = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ac3 fx draw"),
            bind_group_layouts: &[Some(draw_layout)],
            immediate_size: 0,
        });
        let with_snapshot = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ac3 fx deferred"),
            bind_group_layouts: &[Some(draw_layout), Some(&fx_layout)],
            immediate_size: 0,
        });
        let base = wgpu::vertex_attr_array![
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
        let extra = wgpu::vertex_attr_array![9 => Float32, 10 => Uint32];
        let buffers = [
            Some(wgpu::VertexBufferLayout {
                array_stride: (VERTEX_WORDS * 4) as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &base,
            }),
            Some(wgpu::VertexBufferLayout {
                array_stride: (FX_VERTEX_WORDS * 4) as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &extra,
            }),
        ];
        let target = |format, write_mask| {
            Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask,
            })
        };
        let all = wgpu::ColorWrites::ALL;
        let none = wgpu::ColorWrites::empty();
        let pipeline = |label: &str,
                        layout: &wgpu::PipelineLayout,
                        entry: &str,
                        targets: &[Option<wgpu::ColorTargetState>]| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs_fx"),
                    compilation_options: Default::default(),
                    buffers: &buffers,
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    targets,
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let world = pipeline(
            "ac3 fx world",
            &plain,
            "fs_fx_world",
            &[target(VRAM_FORMAT, all), target(ZORDER_FORMAT, all)],
        );
        let zclear = pipeline(
            "ac3 fx zclear",
            &plain,
            "fs_fx_zclear",
            &[target(VRAM_FORMAT, none), target(ZORDER_FORMAT, all)],
        );
        let deferred = pipeline(
            "ac3 fx deferred",
            &with_snapshot,
            "fs_fx_deferred",
            &[target(VRAM_FORMAT, all)],
        );
        let debug_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ac3 fx debug"),
            source: wgpu::ShaderSource::Wgsl(format!("{FX_COMMON_WGSL}\n{FX_DEBUG_SHADER}").into()),
        });
        let debug_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("ac3 fx debug"),
                bind_group_layouts: &[Some(&debug_layout)],
                immediate_size: 0,
            });
        let debug = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ac3 fx debug"),
            layout: Some(&debug_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &debug_module,
                entry_point: Some("vs_debug"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &debug_module,
                entry_point: Some("fs_debug"),
                compilation_options: Default::default(),
                targets: &[target(VRAM_FORMAT, all)],
            }),
            multiview_mask: None,
            cache: None,
        });
        let vertex_buffer = execute::buffer(
            device,
            "ac3 fx vertices",
            1 << 18,
            wgpu::BufferUsages::VERTEX,
        );
        Self {
            scale,
            zorder,
            zorder_view,
            snapshot: None,
            snapshot_rect: [0; 4],
            fx_layout,
            debug_layout,
            globals,
            scene_buffer,
            world,
            zclear,
            deferred,
            debug,
            vertex_buffer,
            stats: CardStats::default(),
        }
    }

    /// The deferred pipeline's group 1 (the snapshot), once one was taken.
    pub fn snapshot_group(&self) -> Option<&wgpu::BindGroup> {
        self.snapshot.as_ref().map(|snapshot| &snapshot.group)
    }

    /// The snapshot's texture (tests).
    pub fn snapshot_texture(&self) -> Option<(&wgpu::Texture, [u32; 4])> {
        self.snapshot
            .as_ref()
            .map(|snapshot| (&snapshot.texture, self.snapshot_rect))
    }

    /// The pipeline of `pipe`, and whether it draws into the z target too.
    pub fn pipeline(&self, pipe: Pipe) -> (&wgpu::RenderPipeline, bool) {
        match pipe {
            Pipe::World => (&self.world, true),
            Pipe::ZClear => (&self.zclear, true),
            Pipe::Deferred => (&self.deferred, false),
        }
    }

    /// Copy the z target over `area` into the snapshot (made again when its
    /// size changes), and note where it is for the shaders.
    #[allow(clippy::too_many_arguments)]
    pub fn snapshot(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        shadow: &wgpu::TextureView,
        area: Rect,
        debug: DebugView,
    ) {
        let [x, y, w, h] = scaled(area, self.scale);
        if w == 0 || h == 0 {
            return;
        }
        let fits = self.snapshot.as_ref().is_some_and(|snapshot| {
            (snapshot.texture.width(), snapshot.texture.height()) == (w, h)
        });
        if !fits {
            let texture = texture(
                device,
                "ac3 fx snapshot",
                (w, h),
                ZORDER_FORMAT,
                wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::COPY_SRC,
            );
            let view = texture.create_view(&Default::default());
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("ac3 fx deferred"),
                layout: &self.fx_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: self.globals.as_entire_binding(),
                    },
                ],
            });
            let debug_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("ac3 fx debug"),
                layout: &self.debug_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.globals.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(shadow),
                    },
                ],
            });
            self.snapshot = Some(Snapshot {
                texture,
                view,
                group,
                debug_group,
            });
        }
        if self.snapshot_rect != [x, y, w, h] || !fits {
            self.snapshot_rect = [x, y, w, h];
        }
        let words = [x, y, w, h, debug.index(), self.scale, 0, 0];
        let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
        queue.write_buffer(&self.globals, 0, &bytes);
        let snapshot = self.snapshot.as_ref().expect("made above");
        encoder.copy_texture_to_texture(
            region(&self.zorder, x, y),
            region(&snapshot.texture, 0, 0),
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        self.stats.snapshots += 1;
    }

    /// The debug view over `area` (after a snapshot of it).
    pub fn debug_view(&mut self, encoder: &mut wgpu::CommandEncoder, vram: &wgpu::TextureView) {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return;
        };
        let [x, y, w, h] = self.snapshot_rect;
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("ac3 fx debug"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: vram,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_pipeline(&self.debug);
        pass.set_bind_group(0, &snapshot.debug_group, &[]);
        pass.set_scissor_rect(x, y, w, h);
        pass.draw(0..3, 0..1);
        self.stats.debug_views += 1;
    }

    /// The snapshot's view (for a [`Context`]).
    pub fn snapshot_view(&self) -> Option<&wgpu::TextureView> {
        self.snapshot.as_ref().map(|snapshot| &snapshot.view)
    }

    /// Write `frame`'s scene into the scene uniform.
    pub fn write_scene(&self, queue: &wgpu::Queue, frame: &FxFrame) {
        let words = frame
            .scene
            .uniform(self.scale, [frame.world_area.x, frame.world_area.y]);
        let bytes: Vec<u8> = words.iter().flat_map(|v| v.to_le_bytes()).collect();
        queue.write_buffer(&self.scene_buffer, 0, &bytes);
    }

    pub fn zorder(&self) -> &wgpu::Texture {
        &self.zorder
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_draw_shader_keeps_its_entry_points_as_functions() {
        let source = fx_draw_source();
        assert!(source.contains("fn draw_vertex(v: VertexInput) -> Varyings {"));
        assert!(source.contains("fn draw_pixel(in: Varyings) -> u32 {"));
        assert!(!source.contains("fn vs_draw("), "renamed");
        assert!(!source.contains("fn fs_draw("), "renamed");
        // The renderer's own shader is untouched (its pictures are the
        // same); the GPU tests compile both (`execute.rs`).
        assert!(DRAW_SHADER.contains("fn fs_draw(in: Varyings) -> @location(0) u32 {"));
    }
}
