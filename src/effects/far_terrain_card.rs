//! The card half of far terrain ([`super`]): the map on the card, and the
//! three passes at world begin (`far_terrain.wgsl`): the mesh ring and the
//! ray march into area-sized intermediates (colour, view z, a reversed
//! depth buffer), then their composite into the N x VRAM and the z target.
use super::{
    EARTH_RADIUS, FarFrame, LEVELS, MapData, NAME, Params, RELIEF, TOPS_LEVELS, TOPS_SIDE,
    VERTEX_BYTES, View, earth_drop, level_size,
};
use crate::effects::terrain_map::{ATLAS_COLUMNS, CELL_TEXELS, GRID_SIZE, TILE_HALF, TILE_UNITS};
use crate::gpu_renderer::execute::VRAM_FORMAT;
use crate::gpu_renderer::fx::card::{FX_COMMON_WGSL, ZORDER_FORMAT};
use crate::gpu_renderer::fx::{CardEffect, Context, Scene};
use serde::Serialize;
use std::sync::{Arc, Mutex};

const SHADER: &str = include_str!("far_terrain.wgsl");
/// The intermediates: display colour, view z, depth (reversed: near / z).
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const Z_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Float;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// The uniform `Far`: 15 vec4s.
const UNIFORM_WORDS: usize = 60;
/// Per instance: the cell's mesh origin relative to the eye (vec4 f32) and
/// its cell (vec4 i32, floor(world / 2048)).
const INSTANCE_BYTES: usize = 32;

/// The shader's whole source: `fx_common.wgsl`, then `far_terrain.wgsl`.
pub fn shader_source() -> String {
    format!("{FX_COMMON_WGSL}\n{SHADER}")
}

/// What the card side did.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct CardStats {
    pub frames: u64,
    /// Whole maps and changed tiles or grids uploaded, and their bytes.
    pub full_uploads: u64,
    pub partial_uploads: u64,
    pub uploaded_bytes: u64,
    /// The last frame: cells drawn, cells culled, draw calls.
    pub cells: u64,
    pub culled: u64,
    pub draws: u64,
    /// CPU time of `begin` (uploads, culling, encoding), in microseconds:
    /// the sum over the frames and the most in one.
    pub cpu_micros: u64,
    pub cpu_micros_max: u64,
    /// Why the card side gave up, if it did.
    pub failed: Option<String>,
}

/// The GPU time of the far terrain's passes, from timestamp queries when
/// the device has `TIMESTAMP_QUERY` (tests ask for it; the game does not).
#[derive(Clone, Default)]
pub struct GpuTimer(Arc<Mutex<Option<TimerShared>>>);

struct TimerShared {
    device: wgpu::Device,
    read: wgpu::Buffer,
    /// Nanoseconds per tick.
    period: f32,
}

/// One frame's far terrain GPU times, in milliseconds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct PassTimes {
    pub meshes: f64,
    pub march: f64,
    /// The VRAM copy for the mask bits and the composite.
    pub composite: f64,
}

impl PassTimes {
    pub fn total(&self) -> f64 {
        self.meshes + self.march + self.composite
    }
}

impl GpuTimer {
    /// The last frame's far terrain GPU times (after the frame was
    /// submitted and finished; waits for the card).
    pub fn read(&self) -> Option<PassTimes> {
        let guard = self.0.lock().ok()?;
        let shared = guard.as_ref()?;
        shared.read.map_async(wgpu::MapMode::Read, .., |_| {});
        let _ = shared.device.poll(wgpu::PollType::wait_indefinitely());
        let ticks: [u64; 4] = {
            let mapped = shared.read.get_mapped_range(..).ok()?;
            std::array::from_fn(|k| {
                u64::from_le_bytes(mapped[8 * k..8 * k + 8].try_into().unwrap_or([0; 8]))
            })
        };
        shared.read.unmap();
        let ms = |a: usize, b: usize| {
            ticks[b].saturating_sub(ticks[a]) as f64 * f64::from(shared.period) / 1e6
        };
        Some(PassTimes {
            meshes: ms(0, 1),
            march: ms(1, 2),
            composite: ms(2, 3),
        })
    }
}

/// The card half; see the module documentation of [`super`].
#[derive(Default)]
pub struct FarTerrainCard {
    gpu: Option<Gpu>,
    failed: bool,
    stats: Arc<Mutex<CardStats>>,
    timer: GpuTimer,
}

/// The map's resources on the card.
struct MapGpu {
    version: u64,
    grid: wgpu::Texture,
    heights: wgpu::Texture,
    baked: wgpu::Texture,
    neutral: wgpu::Texture,
    tops: wgpu::Texture,
    vertices: wgpu::Buffer,
    group: wgpu::BindGroup,
}

/// The area-sized intermediates.
struct Targets {
    size: (u32, u32),
    color: wgpu::TextureView,
    z: wgpu::TextureView,
    depth: wgpu::TextureView,
}

struct Queries {
    set: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    read: wgpu::Buffer,
}

struct Gpu {
    layout: wgpu::BindGroupLayout,
    composite_layout: wgpu::BindGroupLayout,
    mesh: wgpu::RenderPipeline,
    mesh_near: wgpu::RenderPipeline,
    march: wgpu::RenderPipeline,
    composite: wgpu::RenderPipeline,
    uniform: wgpu::Buffer,
    instances: wgpu::Buffer,
    map: Option<MapGpu>,
    targets: Option<Targets>,
    queries: Option<Queries>,
}

fn texture_entry(binding: u32, sample_type: wgpu::TextureSampleType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type,
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

const FLOAT: wgpu::TextureSampleType = wgpu::TextureSampleType::Float { filterable: false };

fn map_texture(
    device: &wgpu::Device,
    label: &str,
    (width, height): (u32, u32),
    levels: u32,
    format: wgpu::TextureFormat,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: levels,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn target(
    device: &wgpu::Device,
    label: &str,
    (w, h): (u32, u32),
    format: wgpu::TextureFormat,
) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
        .create_view(&Default::default())
}

/// Write a rectangle of `texture`'s level `level` from `data` (rows of
/// `width * bytes` bytes).
#[allow(clippy::too_many_arguments)]
fn write_region(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    level: u32,
    (x, y): (u32, u32),
    (width, height): (u32, u32),
    bytes: u32,
    data: &[u8],
) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: level,
            origin: wgpu::Origin3d { x, y, z: 0 },
            aspect: wgpu::TextureAspect::All,
        },
        data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width * bytes),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
}

/// Every level of the cell tops; returns the bytes written.
fn write_tops(queue: &wgpu::Queue, texture: &wgpu::Texture, map: &MapData) -> u64 {
    let mut bytes = 0;
    for (level, tops) in map.tops.iter().enumerate() {
        let side = (TOPS_SIDE >> level) as u32;
        let data = f32_bytes(tops);
        write_region(queue, texture, level as u32, (0, 0), (side, side), 4, &data);
        bytes += data.len() as u64;
    }
    bytes
}

fn f32_bytes(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn rgba_bytes(values: &[[u8; 4]]) -> Vec<u8> {
    values.iter().flatten().copied().collect()
}

impl Gpu {
    fn new(device: &wgpu::Device) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ac3 far terrain"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                texture_entry(1, wgpu::TextureSampleType::Uint),
                texture_entry(2, FLOAT),
                texture_entry(3, FLOAT),
                texture_entry(4, FLOAT),
                texture_entry(5, FLOAT),
            ],
        });
        let composite_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ac3 far terrain composite"),
            entries: &[
                texture_entry(0, FLOAT),
                texture_entry(1, FLOAT),
                texture_entry(2, wgpu::TextureSampleType::Uint),
            ],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ac3 far terrain"),
            source: wgpu::ShaderSource::Wgsl(shader_source().into()),
        });
        let plain = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ac3 far terrain"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let both = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ac3 far terrain composite"),
            bind_group_layouts: &[Some(&layout), Some(&composite_layout)],
            immediate_size: 0,
        });
        let vertex = wgpu::vertex_attr_array![0 => Float32x3, 1 => Unorm8x4, 2 => Unorm8x4];
        let instance = wgpu::vertex_attr_array![3 => Float32x4, 4 => Sint32x4];
        let plain_target = |format| {
            Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })
        };
        let terrain_targets = [plain_target(COLOR_FORMAT), plain_target(Z_FORMAT)];
        let depth = |write: bool, compare| {
            Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(write),
                depth_compare: Some(compare),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            })
        };
        let mesh_pipeline = |label: &str, entry: &str| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&plain),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs_mesh"),
                    compilation_options: Default::default(),
                    buffers: &[
                        Some(wgpu::VertexBufferLayout {
                            array_stride: VERTEX_BYTES as u64,
                            step_mode: wgpu::VertexStepMode::Vertex,
                            attributes: &vertex,
                        }),
                        Some(wgpu::VertexBufferLayout {
                            array_stride: INSTANCE_BYTES as u64,
                            step_mode: wgpu::VertexStepMode::Instance,
                            attributes: &instance,
                        }),
                    ],
                },
                // The game's front faces have NCLIP > 0 on its screen (y down):
                // with y up in clip space the signed area turns negative, so
                // they are clockwise there. The game culls back faces.
                primitive: wgpu::PrimitiveState {
                    front_face: wgpu::FrontFace::Cw,
                    cull_mode: Some(wgpu::Face::Back),
                    ..Default::default()
                },
                depth_stencil: depth(true, wgpu::CompareFunction::Greater),
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    targets: &terrain_targets,
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let mesh = mesh_pipeline("ac3 far terrain meshes", "fs_mesh");
        let mesh_near = mesh_pipeline("ac3 far terrain near meshes", "fs_mesh_near");
        let full = |label: &str,
                    layout: &wgpu::PipelineLayout,
                    entry: &str,
                    targets: &[Option<wgpu::ColorTargetState>],
                    depth_stencil| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs_full"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil,
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
        // Only where no mesh drew: the depth is still the clear value 0.
        let march = full(
            "ac3 far terrain march",
            &plain,
            "fs_march",
            &terrain_targets,
            depth(false, wgpu::CompareFunction::Equal),
        );
        let composite = full(
            "ac3 far terrain composite",
            &both,
            "fs_composite",
            &[plain_target(VRAM_FORMAT), plain_target(ZORDER_FORMAT)],
            None,
        );
        let buffer = |label: &str, size: u64, usage: wgpu::BufferUsages| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage: usage | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let uniform = buffer(
            "ac3 far terrain uniform",
            (UNIFORM_WORDS * 4) as u64,
            wgpu::BufferUsages::UNIFORM,
        );
        let instances = buffer("ac3 far terrain cells", 1 << 16, wgpu::BufferUsages::VERTEX);
        let queries = device
            .features()
            .contains(wgpu::Features::TIMESTAMP_QUERY)
            .then(|| Queries {
                set: device.create_query_set(&wgpu::QuerySetDescriptor {
                    label: Some("ac3 far terrain time"),
                    ty: wgpu::QueryType::Timestamp,
                    count: 4,
                }),
                resolve: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("ac3 far terrain time"),
                    size: 32,
                    usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                }),
                read: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("ac3 far terrain time read"),
                    size: 32,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }),
            });
        Self {
            layout,
            composite_layout,
            mesh,
            mesh_near,
            march,
            composite,
            uniform,
            instances,
            map: None,
            targets: None,
            queries,
        }
    }

    /// The whole map onto the card.
    fn upload_all(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, frame: &FarFrame) -> u64 {
        let map = &frame.map;
        let (w0, h0) = level_size(0);
        let size = (w0 as u32, h0 as u32);
        let grid = map_texture(
            device,
            "ac3 far terrain grid",
            (GRID_SIZE as u32, GRID_SIZE as u32),
            1,
            wgpu::TextureFormat::R16Uint,
        );
        let heights = map_texture(
            device,
            "ac3 far terrain heights",
            size,
            LEVELS as u32,
            wgpu::TextureFormat::R32Float,
        );
        let baked = map_texture(
            device,
            "ac3 far terrain colours",
            size,
            LEVELS as u32,
            wgpu::TextureFormat::Rgba8UnormSrgb,
        );
        let neutral = map_texture(
            device,
            "ac3 far terrain neutral",
            size,
            LEVELS as u32,
            wgpu::TextureFormat::Rgba8UnormSrgb,
        );
        let tops = map_texture(
            device,
            "ac3 far terrain tops",
            (TOPS_SIDE as u32, TOPS_SIDE as u32),
            TOPS_LEVELS as u32,
            wgpu::TextureFormat::R32Float,
        );
        let vertices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ac3 far terrain meshes"),
            size: map.vertices.len().max(VERTEX_BYTES) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut bytes = 0u64;
        let grid_bytes: Vec<u8> = map.grid.iter().flat_map(|c| c.to_le_bytes()).collect();
        write_region(
            queue,
            &grid,
            0,
            (0, 0),
            (GRID_SIZE as u32, GRID_SIZE as u32),
            2,
            &grid_bytes,
        );
        bytes += grid_bytes.len() as u64;
        for level in 0..LEVELS {
            let (w, h) = level_size(level);
            let extent = (w as u32, h as u32);
            let data = f32_bytes(&map.heights[level]);
            write_region(queue, &heights, level as u32, (0, 0), extent, 4, &data);
            bytes += data.len() as u64;
            for (texture, colours) in [(&baked, &map.baked), (&neutral, &map.neutral)] {
                let data = rgba_bytes(&colours[level]);
                write_region(queue, texture, level as u32, (0, 0), extent, 4, &data);
                bytes += data.len() as u64;
            }
        }
        bytes += write_tops(queue, &tops, map);
        if !map.vertices.is_empty() {
            queue.write_buffer(&vertices, 0, &map.vertices);
            bytes += map.vertices.len() as u64;
        }
        let view = |texture: &wgpu::Texture| texture.create_view(&Default::default());
        let views = [
            view(&grid),
            view(&heights),
            view(&baked),
            view(&neutral),
            view(&tops),
        ];
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ac3 far terrain"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&views[0]),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&views[1]),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&views[2]),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&views[3]),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(&views[4]),
                },
            ],
        });
        self.map = Some(MapGpu {
            version: frame.version,
            grid,
            heights,
            baked,
            neutral,
            tops,
            vertices,
            group,
        });
        bytes
    }

    /// The changes of `frame` onto the card holding its base version.
    fn upload_changes(&mut self, queue: &wgpu::Queue, frame: &FarFrame) -> u64 {
        let (Some(gpu), Some(changes)) = (self.map.as_mut(), frame.changes.as_ref()) else {
            return 0;
        };
        let map = &frame.map;
        let mut bytes = 0u64;
        if changes.grid {
            let data: Vec<u8> = map.grid.iter().flat_map(|c| c.to_le_bytes()).collect();
            write_region(
                queue,
                &gpu.grid,
                0,
                (0, 0),
                (GRID_SIZE as u32, GRID_SIZE as u32),
                2,
                &data,
            );
            bytes += data.len() as u64;
        }
        for &tile in &changes.tiles {
            let t = usize::from(tile);
            let Some(data) = map.tiles.get(t) else {
                continue;
            };
            for level in 0..LEVELS {
                let n = CELL_TEXELS >> level;
                let (width, _) = level_size(level);
                let x = (t % ATLAS_COLUMNS) * n;
                let y = (t / ATLAS_COLUMNS) * n;
                let rows = |values: &dyn Fn(usize) -> Vec<u8>| -> Vec<u8> {
                    (0..n)
                        .flat_map(|j| (0..n).flat_map(move |i| values((y + j) * width + x + i)))
                        .collect()
                };
                let origin = (x as u32, y as u32);
                let extent = (n as u32, n as u32);
                let heights = rows(&|at| map.heights[level][at].to_le_bytes().to_vec());
                write_region(
                    queue,
                    &gpu.heights,
                    level as u32,
                    origin,
                    extent,
                    4,
                    &heights,
                );
                let baked = rows(&|at| map.baked[level][at].to_vec());
                write_region(queue, &gpu.baked, level as u32, origin, extent, 4, &baked);
                let neutral = rows(&|at| map.neutral[level][at].to_vec());
                write_region(
                    queue,
                    &gpu.neutral,
                    level as u32,
                    origin,
                    extent,
                    4,
                    &neutral,
                );
                bytes += (heights.len() + baked.len() + neutral.len()) as u64;
            }
            let first = data.first as usize * VERTEX_BYTES;
            let end = (data.first + data.count) as usize * VERTEX_BYTES;
            if end > first && end <= map.vertices.len() {
                queue.write_buffer(&gpu.vertices, first as u64, &map.vertices[first..end]);
                bytes += (end - first) as u64;
            }
        }
        bytes += write_tops(queue, &gpu.tops, map);
        gpu.version = frame.version;
        bytes
    }

    fn targets(&mut self, device: &wgpu::Device, size: (u32, u32)) -> &Targets {
        if self.targets.as_ref().is_none_or(|t| t.size != size) {
            self.targets = Some(Targets {
                size,
                color: target(device, "ac3 far terrain colour", size, COLOR_FORMAT),
                z: target(device, "ac3 far terrain z", size, Z_FORMAT),
                depth: target(device, "ac3 far terrain depth", size, DEPTH_FORMAT),
            });
        }
        self.targets.as_ref().expect("made above")
    }
}

/// The uniform `Far` of a frame.
pub fn uniform(
    scene: &Scene,
    params: &Params,
    map: &MapData,
    scale: u32,
    area: [i32; 4],
) -> [u32; UNIFORM_WORDS] {
    let view = View::of(scene);
    let mut f = [0.0f32; UNIFORM_WORDS];
    for (r, row) in view.rows.iter().enumerate() {
        for (c, v) in row.iter().enumerate() {
            f[4 * r + c] = *v as f32;
        }
    }
    f[3] = view.h as f32;
    f[7] = view.k as f32;
    f[11] = scale as f32;
    for (c, v) in view.eye.iter().enumerate() {
        f[12 + c] = *v as f32;
    }
    f[15] = params.near as f32;
    let [x, y, w, h] = area;
    f[16] = (view.centre[0] - f64::from(x)) as f32;
    f[17] = (view.centre[1] - f64::from(y)) as f32;
    f[18] = w as f32;
    f[19] = h as f32;
    f[20] = (x * scale as i32) as f32;
    f[21] = (y * scale as i32) as f32;
    f[24] = map.top;
    f[25] = params.drop_start as f32;
    f[26] = (1.0 / (2.0 * EARTH_RADIUS)) as f32;
    if let Some((near, far)) = params.cue {
        f[28] = near as f32;
        f[29] = far as f32;
        f[30] = 1.0;
    }
    f[31] = RELIEF;
    f[32] = params.relief.0 as f32;
    f[33] = params.relief.1 as f32;
    f[36] = params.march_start as f32;
    f[37] = params.march_end as f32;
    f[38] = params.step;
    let sun = scene.sun.map(|v| f32::from(v) / 4096.0);
    let length = (sun[0] * sun[0] + sun[1] * sun[1] + sun[2] * sun[2])
        .sqrt()
        .max(1e-6);
    for (c, v) in sun.iter().enumerate() {
        f[40 + c] = v / length;
    }
    // The inverse by columns: world direction = sum of column j times v_j.
    for (r, row) in view.inverse.iter().enumerate() {
        for (c, v) in row.iter().enumerate() {
            f[48 + 4 * c + r] = *v as f32;
        }
    }
    let mut words = f.map(f32::to_bits);
    words[44] = params.max_steps;
    words[45] = params.bisect;
    words
}

/// One draw of the ring: a tile type's vertices, `count` cells from
/// instance `first`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Draw {
    /// Wholly within the game's range (drawn first, with the cheap
    /// shader: only the game's cracks show them).
    pub near: bool,
    pub tile: u16,
    pub first: u32,
    pub count: u32,
}

/// The cells of the ring around the eye that the view frustum keeps,
/// grouped by tile type: their instance bytes, the draws, and the cells
/// culled.
pub fn ring_cells(
    map: &MapData,
    view: &View,
    params: &Params,
    tiles: u32,
    area: [i32; 4],
) -> (Vec<u8>, Vec<Draw>, u64) {
    let unit = f64::from(TILE_UNITS);
    let half = f64::from(TILE_HALF);
    let cx = (view.eye[0] / unit).floor() as i32 + 128;
    let cz = (view.eye[2] / unit).floor() as i32 + 128;
    let n = tiles as i32;
    let [_, _, w, h] = area;
    let centre = [
        view.centre[0] - f64::from(area[0]),
        view.centre[1] - f64::from(area[1]),
    ];
    // The frustum's side planes in view space: x / z and y / z ranges.
    let x_range = [
        -centre[0] / (view.k * view.h),
        (f64::from(w) - centre[0]) / (view.k * view.h),
    ];
    let y_range = [-centre[1] / view.h, (f64::from(h) - centre[1]) / view.h];
    let mut kept: Vec<(bool, u16, [f32; 4], [i32; 4])> = Vec::new();
    let mut culled = 0u64;
    for z in cz - n..=cz + n {
        for x in cx - n..=cx + n {
            let tile = map.tile_at(x, z);
            let Some(data) = map
                .tiles
                .get(usize::from(tile))
                .filter(|d| tile != 0 && d.count > 0)
            else {
                continue;
            };
            let origin = [
                f64::from((x - 128) * TILE_UNITS + TILE_HALF),
                f64::from((z - 128) * TILE_UNITS + TILE_HALF),
            ];
            let rel = [origin[0] - view.eye[0], origin[1] - view.eye[2]];
            // The Earth's drop over the cell: least at its nearest point,
            // most at its farthest corner.
            let nearest = [
                (rel[0].abs() - half).max(0.0),
                (rel[1].abs() - half).max(0.0),
            ];
            let farthest = [rel[0].abs() + half, rel[1].abs() + half];
            let drop_min = earth_drop(nearest[0].hypot(nearest[1]), params.drop_start);
            let drop_max = earth_drop(farthest[0].hypot(farthest[1]), params.drop_start);
            let y0 = f64::from(data.top) - view.eye[1] + drop_min;
            let y1 = f64::from(data.floor) - view.eye[1] + drop_max;
            let mut outside = [true; 5];
            let mut deepest = f64::MIN;
            for corner in 0..8 {
                let p = [
                    rel[0] + if corner & 1 == 0 { -half } else { half },
                    if corner & 2 == 0 { y0 } else { y1 },
                    rel[1] + if corner & 4 == 0 { -half } else { half },
                ];
                let v = view.to_view(p);
                deepest = deepest.max(v[2]);
                let tests = [
                    v[2] < params.near,
                    v[0] < x_range[0] * v[2],
                    v[0] > x_range[1] * v[2],
                    v[1] < y_range[0] * v[2],
                    v[1] > y_range[1] * v[2],
                ];
                for (out, test) in outside.iter_mut().zip(tests) {
                    *out &= test;
                }
            }
            if outside.iter().any(|&o| o) {
                culled += 1;
                continue;
            }
            kept.push((
                deepest < params.near_cut,
                tile,
                [rel[0] as f32, (-view.eye[1]) as f32, rel[1] as f32, 0.0],
                [x - 128, z - 128, 0, 0],
            ));
        }
    }
    // Near cells first (front to back helps the depth test), by tile type.
    kept.sort_by_key(|(near, tile, _, _)| (!near, *tile));
    let mut bytes = Vec::with_capacity(kept.len() * INSTANCE_BYTES);
    let mut draws: Vec<Draw> = Vec::new();
    for (index, (near, tile, offset, cell)) in kept.iter().enumerate() {
        for v in offset {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        for v in cell {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        match draws.last_mut() {
            Some(draw) if draw.tile == *tile && draw.near == *near => draw.count += 1,
            _ => draws.push(Draw {
                near: *near,
                tile: *tile,
                first: index as u32,
                count: 1,
            }),
        }
    }
    (bytes, draws, culled)
}

impl FarTerrainCard {
    pub fn new() -> Self {
        Self::default()
    }

    /// A handle on the statistics (shared with the boxed effect).
    pub fn stats(&self) -> Arc<Mutex<CardStats>> {
        self.stats.clone()
    }

    /// A handle on the GPU time (timestamp queries, when the device has
    /// them).
    pub fn timer(&self) -> GpuTimer {
        self.timer.clone()
    }

    fn note(&self, f: impl FnOnce(&mut CardStats)) {
        if let Ok(mut stats) = self.stats.lock() {
            f(&mut stats);
        }
    }

    /// Set up the pipelines (once); `false` when the card cannot.
    fn ready(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> bool {
        if self.failed {
            return false;
        }
        if self.gpu.is_some() {
            return true;
        }
        let scopes = [
            wgpu::ErrorFilter::Validation,
            wgpu::ErrorFilter::OutOfMemory,
            wgpu::ErrorFilter::Internal,
        ]
        .map(|filter| device.push_error_scope(filter));
        let gpu = Gpu::new(device);
        let mut failed = None;
        for scope in scopes.into_iter().rev() {
            if let Some(error) = pollster::block_on(scope.pop()) {
                failed.get_or_insert(error);
            }
        }
        if let Some(error) = failed {
            let error = format!("far terrain could not be set up: {error}");
            eprintln!("ac3: {error}; far terrain off");
            self.failed = true;
            self.note(|s| s.failed = Some(error));
            return false;
        }
        if let Some(queries) = &gpu.queries
            && let Ok(mut timer) = self.timer.0.lock()
        {
            *timer = Some(TimerShared {
                device: device.clone(),
                read: queries.read.clone(),
                period: queue.get_timestamp_period(),
            });
        }
        eprintln!("ac3: far terrain: the card side is set up");
        self.gpu = Some(gpu);
        true
    }
}

impl CardEffect for FarTerrainCard {
    fn name(&self) -> &'static str {
        NAME
    }

    fn begin(&mut self, ctx: &mut Context<'_>) {
        let Some(frame) = ctx.data.and_then(|data| data.downcast_ref::<FarFrame>()) else {
            return;
        };
        let [px, py, pw, ph] = ctx.pixels;
        if pw == 0 || ph == 0 || !self.ready(ctx.device, ctx.queue) {
            return;
        }
        let started = std::time::Instant::now();
        let gpu = self.gpu.as_mut().expect("ready");
        // The map: a new version line uploads everything, the next version
        // of the one held only what changed.
        let held = gpu.map.as_ref().map(|map| map.version);
        let (full, bytes) = match (held, &frame.changes) {
            (Some(version), _) if version == frame.version => (None, 0),
            (Some(version), Some(changes)) if version == changes.base => {
                (Some(false), gpu.upload_changes(ctx.queue, frame))
            }
            _ => (Some(true), gpu.upload_all(ctx.device, ctx.queue, frame)),
        };
        let scene = ctx.scene;
        let map = &frame.map;
        let view = View::of(scene);
        let params = Params::new(scene, map, &frame.settings);
        let area = [ctx.area.x, ctx.area.y, ctx.area.w, ctx.area.h];
        let words = uniform(scene, &params, map, ctx.scale, area);
        let bytes_uniform: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        ctx.queue.write_buffer(&gpu.uniform, 0, &bytes_uniform);
        let (instances, draws, culled) =
            ring_cells(map, &view, &params, frame.settings.ring.tiles(), area);
        if !instances.is_empty() {
            if (gpu.instances.size() as usize) < instances.len() {
                gpu.instances = ctx.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("ac3 far terrain cells"),
                    size: (instances.len() as u64).next_power_of_two(),
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
            }
            ctx.queue.write_buffer(&gpu.instances, 0, &instances);
        }
        gpu.targets(ctx.device, (pw, ph));
        let (Some(map_gpu), Some(targets)) = (gpu.map.as_ref(), gpu.targets.as_ref()) else {
            return;
        };
        let timestamps = |begin: Option<u32>, end: Option<u32>| {
            gpu.queries
                .as_ref()
                .map(|q| wgpu::RenderPassTimestampWrites {
                    query_set: &q.set,
                    beginning_of_pass_write_index: begin,
                    end_of_pass_write_index: end,
                })
        };
        // Pass 1, the meshes; pass 2, the march where none drew (the depth
        // still 0). Two passes, so the timer tells them apart.
        let attachment = |view, load| {
            Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load,
                    store: wgpu::StoreOp::Store,
                },
            })
        };
        let clear = wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT);
        {
            let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ac3 far terrain meshes"),
                color_attachments: &[
                    attachment(&targets.color, clear),
                    attachment(&targets.z, clear),
                ],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &targets.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: timestamps(Some(0), Some(1)),
                ..Default::default()
            });
            pass.set_bind_group(0, &map_gpu.group, &[]);
            if !draws.is_empty() {
                pass.set_vertex_buffer(0, map_gpu.vertices.slice(..));
                let mut bound = None;
                for draw in &draws {
                    if bound != Some(draw.near) {
                        pass.set_pipeline(if draw.near { &gpu.mesh_near } else { &gpu.mesh });
                        bound = Some(draw.near);
                    }
                    let tile = map.tiles[usize::from(draw.tile)];
                    pass.set_vertex_buffer(
                        1,
                        gpu.instances
                            .slice(u64::from(draw.first) * INSTANCE_BYTES as u64..),
                    );
                    pass.draw(tile.first..tile.first + tile.count, 0..draw.count);
                }
            }
        }
        {
            let load = wgpu::LoadOp::Load;
            let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ac3 far terrain march"),
                color_attachments: &[
                    attachment(&targets.color, load),
                    attachment(&targets.z, load),
                ],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &targets.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: timestamps(None, Some(2)),
                ..Default::default()
            });
            pass.set_bind_group(0, &map_gpu.group, &[]);
            pass.set_pipeline(&gpu.march);
            pass.draw(0..3, 0..1);
        }
        // Pass 3: into VRAM (the old mask bits from `shadow`) and z.
        ctx.sync_area();
        let composite_group = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ac3 far terrain composite"),
            layout: &gpu.composite_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&targets.color),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&targets.z),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(ctx.shadow),
                },
            ],
        });
        {
            let load = |view| {
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
            let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ac3 far terrain composite"),
                color_attachments: &[load(ctx.vram), load(ctx.zorder)],
                timestamp_writes: timestamps(None, Some(3)),
                ..Default::default()
            });
            pass.set_pipeline(&gpu.composite);
            pass.set_bind_group(0, &map_gpu.group, &[]);
            pass.set_bind_group(1, &composite_group, &[]);
            pass.set_scissor_rect(px, py, pw, ph);
            pass.draw(0..3, 0..1);
        }
        if let Some(q) = &gpu.queries {
            ctx.encoder.resolve_query_set(&q.set, 0..4, &q.resolve, 0);
            ctx.encoder
                .copy_buffer_to_buffer(&q.resolve, 0, &q.read, 0, 32);
        }
        let cells = (instances.len() / INSTANCE_BYTES) as u64;
        let draw_count = draws.len() as u64;
        let micros = started.elapsed().as_micros() as u64;
        self.note(|s| {
            s.frames += 1;
            s.cpu_micros += micros;
            s.cpu_micros_max = s.cpu_micros_max.max(micros);
            match full {
                Some(true) => s.full_uploads += 1,
                Some(false) => s.partial_uploads += 1,
                None => {}
            }
            s.uploaded_bytes += bytes;
            s.cells = cells;
            s.culled = culled;
            s.draws = draw_count;
        });
    }
}
