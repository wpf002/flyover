//! wgpu device, pipelines, the GPU tile cache (LRU under a memory budget), drawing, and offscreen
//! targets with readback for screenshots and picking.
//!
//! One pipeline draws extruded cells; a second writes feature ids into an `R32Uint` target for
//! picking. Per-feature color, height, and id live in a storage buffer, so rebinding a layer is a
//! buffer swap, not a geometry rebuild.

use std::borrow::Cow;
use std::collections::HashMap;

use wgpu::util::DeviceExt;

use crate::mesh::{FeatureRaw, PreparedTile, Vertex};
use crate::tileset::TileKey;

pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
pub const PICK_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Uint;
pub const OFFSCREEN_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
/// Background color (linear values written straight into an Unorm target).
pub const CLEAR_RGB: [f64; 3] = [0.035, 0.043, 0.058];
const CLEAR: wgpu::Color = wgpu::Color {
    r: CLEAR_RGB[0],
    g: CLEAR_RGB[1],
    b: CLEAR_RGB[2],
    a: 1.0,
};

const SHADER: &str = r#"
struct Uniforms { view_proj: mat4x4<f32> };
@group(0) @binding(0) var<uniform> u: Uniforms;

struct Feature { color: vec4<f32>, height: f32, id: u32, p0: u32, p1: u32 };
@group(1) @binding(0) var<storage, read> features: array<Feature>;

struct VsIn {
    @location(0) world: vec2<f32>,
    @location(1) height_factor: f32,
    @location(2) shade: f32,
    @location(3) feature_index: u32,
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec3<f32>,
    @location(1) @interpolate(flat) id: u32,
};

@vertex
fn vs_main(v: VsIn) -> VsOut {
    let f = features[v.feature_index];
    var out: VsOut;
    let z = v.height_factor * f.height;
    out.pos = u.view_proj * vec4<f32>(v.world.x, v.world.y, z, 1.0);
    out.color = f.color.rgb * v.shade;
    out.id = f.id;
    return out;
}

@fragment
fn fs_color(v: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(v.color, 1.0);
}

@fragment
fn fs_pick(v: VsOut) -> @location(0) u32 {
    return v.id;
}
"#;

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("no GPU adapter found: {0}")]
    Adapter(#[from] wgpu::RequestAdapterError),
    #[error("could not open the GPU device: {0}")]
    Device(#[from] wgpu::RequestDeviceError),
    #[error("could not create a window surface: {0}")]
    Surface(#[from] wgpu::CreateSurfaceError),
    #[error("reading back a GPU buffer failed")]
    Readback,
    #[error("tile set: {0}")]
    TileSet(#[from] crate::tileset::TileSetError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("png: {0}")]
    Png(#[from] png::EncodingError),
    #[error("window: {0}")]
    Window(String),
}

pub struct Gpu {
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub adapter_info: wgpu::AdapterInfo,
}

impl Gpu {
    /// Open an adapter and device, optionally compatible with a window surface.
    pub fn open(
        instance: &wgpu::Instance,
        surface: Option<&wgpu::Surface<'_>>,
    ) -> Result<Gpu, RenderError> {
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: surface,
            ..Default::default()
        }))?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("flyover"),
                required_limits: adapter.limits(),
                ..Default::default()
            }))?;
        let adapter_info = adapter.get_info();
        Ok(Gpu {
            adapter,
            device,
            queue,
            adapter_info,
        })
    }

    pub fn wait(&self) {
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
    }
}

pub struct Pipelines {
    feature_layout: wgpu::BindGroupLayout,
    color: wgpu::RenderPipeline,
    pick: wgpu::RenderPipeline,
    uniform_buffer: wgpu::Buffer,
    uniform_group: wgpu::BindGroup,
}

impl Pipelines {
    pub fn new(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("cells"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(SHADER)),
        });

        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("uniforms"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let feature_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("features"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("cells"),
            bind_group_layouts: &[Some(&uniform_layout), Some(&feature_layout)],
            immediate_size: 0,
        });

        let attributes =
            wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32, 2 => Float32, 3 => Uint32];
        let buffers = [Some(wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &attributes,
        })];
        let depth = Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Less),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        });

        let make = |entry: &str, format: wgpu::TextureFormat, label: &str| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &buffers,
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: depth.clone(),
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(entry),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
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
        let color = make("fs_color", color_format, "cells-color");
        let pick = make("fs_pick", PICK_FORMAT, "cells-pick");

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("uniforms"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let uniform_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("uniforms"),
            layout: &uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        Pipelines {
            feature_layout,
            color,
            pick,
            uniform_buffer,
            uniform_group,
        }
    }

    pub fn set_view_proj(&self, queue: &wgpu::Queue, view_proj: glam::Mat4) {
        queue.write_buffer(
            &self.uniform_buffer,
            0,
            bytemuck::cast_slice(&view_proj.to_cols_array()),
        );
    }
}

struct GpuTile {
    vertices: wgpu::Buffer,
    group: wgpu::BindGroup,
    count: u32,
    bytes: u64,
    last_used: u64,
}

/// Uploaded tiles, evicted least-recently-used once over the byte budget.
pub struct GpuCache {
    tiles: HashMap<TileKey, GpuTile>,
    bytes: u64,
    budget: u64,
}

impl GpuCache {
    pub fn new(budget_bytes: u64) -> Self {
        GpuCache {
            tiles: HashMap::new(),
            bytes: 0,
            budget: budget_bytes,
        }
    }

    pub fn contains(&self, key: TileKey) -> bool {
        self.tiles.contains_key(&key)
    }

    pub fn len(&self) -> usize {
        self.tiles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Upload a prepared tile. This is the only per-tile work on the render thread: two buffer
    /// copies. Decode and mesh building already happened on a worker.
    pub fn upload(
        &mut self,
        device: &wgpu::Device,
        pipelines: &Pipelines,
        tile: PreparedTile,
        frame: u64,
    ) {
        if tile.vertices.is_empty() || tile.features.is_empty() {
            return;
        }
        let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("tile-vertices"),
            contents: bytemuck::cast_slice(&tile.vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let features = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("tile-features"),
            contents: bytemuck::cast_slice(&tile.features),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tile-features"),
            layout: &pipelines.feature_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: features.as_entire_binding(),
            }],
        });
        let bytes = (tile.vertices.len() * std::mem::size_of::<Vertex>()
            + tile.features.len() * std::mem::size_of::<FeatureRaw>()) as u64;
        if let Some(old) = self.tiles.insert(
            tile.key,
            GpuTile {
                vertices,
                group,
                count: tile.vertices.len() as u32,
                bytes,
                last_used: frame,
            },
        ) {
            self.bytes -= old.bytes;
        }
        self.bytes += bytes;
    }

    pub fn touch(&mut self, key: TileKey, frame: u64) {
        if let Some(t) = self.tiles.get_mut(&key) {
            t.last_used = frame;
        }
    }

    /// Drop least-recently-used tiles until under budget. Tiles used this frame are kept.
    pub fn evict(&mut self, frame: u64) {
        if self.bytes <= self.budget {
            return;
        }
        let mut order: Vec<(u64, TileKey)> =
            self.tiles.iter().map(|(k, t)| (t.last_used, *k)).collect();
        order.sort();
        for (last_used, key) in order {
            if self.bytes <= self.budget || last_used >= frame {
                break;
            }
            if let Some(t) = self.tiles.remove(&key) {
                self.bytes -= t.bytes;
            }
        }
    }
}

/// Record one pass drawing `keys` (already resolved to loaded tiles) into `color_view`.
pub fn encode_pass(
    encoder: &mut wgpu::CommandEncoder,
    pipelines: &Pipelines,
    cache: &GpuCache,
    keys: &[TileKey],
    color_view: &wgpu::TextureView,
    depth_view: &wgpu::TextureView,
    pick: bool,
) {
    let clear = if pick {
        wgpu::Color::TRANSPARENT
    } else {
        CLEAR
    };
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(if pick { "pick" } else { "color" }),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: color_view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(clear),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
            view: depth_view,
            depth_ops: Some(wgpu::Operations {
                load: wgpu::LoadOp::Clear(1.0),
                store: wgpu::StoreOp::Store,
            }),
            stencil_ops: None,
        }),
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    pass.set_pipeline(if pick {
        &pipelines.pick
    } else {
        &pipelines.color
    });
    pass.set_bind_group(0, &pipelines.uniform_group, &[]);
    for key in keys {
        if let Some(t) = cache.tiles.get(key) {
            pass.set_bind_group(1, &t.group, &[]);
            pass.set_vertex_buffer(0, t.vertices.slice(..));
            pass.draw(0..t.count, 0..1);
        }
    }
}

/// Depth texture sized to a target.
pub fn depth_view(device: &wgpu::Device, width: u32, height: u32) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("depth"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        })
        .create_view(&wgpu::TextureViewDescriptor::default())
}

/// Offscreen color + pick targets for headless rendering and readback.
pub struct Offscreen {
    pub width: u32,
    pub height: u32,
    color: wgpu::Texture,
    pub color_view: wgpu::TextureView,
    pick: wgpu::Texture,
    pub pick_view: wgpu::TextureView,
    pub depth_view: wgpu::TextureView,
}

impl Offscreen {
    pub fn new(device: &wgpu::Device, width: u32, height: u32) -> Self {
        let make = |format, label| {
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
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
        };
        let color = make(OFFSCREEN_FORMAT, "offscreen-color");
        let pick = make(PICK_FORMAT, "offscreen-pick");
        let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
        let pick_view = pick.create_view(&wgpu::TextureViewDescriptor::default());
        let depth_view = depth_view(device, width, height);
        Offscreen {
            width,
            height,
            color,
            color_view,
            pick,
            pick_view,
            depth_view,
        }
    }

    /// Read the color target back as tightly packed RGBA8 rows.
    pub fn read_rgba(&self, gpu: &Gpu) -> Result<Vec<u8>, RenderError> {
        let unpadded = self.width * 4;
        let padded = unpadded.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let data = copy_out(gpu, &self.color, 0, 0, self.width, self.height, padded)?;
        let mut out = Vec::with_capacity((unpadded * self.height) as usize);
        for row in 0..self.height {
            let start = (row * padded) as usize;
            out.extend_from_slice(&data[start..start + unpadded as usize]);
        }
        Ok(out)
    }

    /// Read one feature id from the pick target. 0 means empty background.
    pub fn read_id(&self, gpu: &Gpu, x: u32, y: u32) -> Result<u32, RenderError> {
        let x = x.min(self.width.saturating_sub(1));
        let y = y.min(self.height.saturating_sub(1));
        let data = copy_out(
            gpu,
            &self.pick,
            x,
            y,
            1,
            1,
            wgpu::COPY_BYTES_PER_ROW_ALIGNMENT,
        )?;
        Ok(u32::from_le_bytes([data[0], data[1], data[2], data[3]]))
    }
}

/// Copy a texture region into a mappable buffer and return its bytes (row-padded).
fn copy_out(
    gpu: &Gpu,
    texture: &wgpu::Texture,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    padded_row: u32,
) -> Result<Vec<u8>, RenderError> {
    let size = (padded_row * height) as wgpu::BufferAddress;
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("readback"),
        });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d { x, y, z: 0 },
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit(Some(encoder.finish()));

    let (tx, rx) = std::sync::mpsc::channel();
    buffer.map_async(wgpu::MapMode::Read, .., move |r| {
        let _ = tx.send(r.is_ok());
    });
    gpu.wait();
    if !rx.recv().unwrap_or(false) {
        return Err(RenderError::Readback);
    }
    let bytes = buffer
        .get_mapped_range(..)
        .map_err(|_| RenderError::Readback)?
        .to_vec();
    buffer.unmap();
    Ok(bytes)
}
