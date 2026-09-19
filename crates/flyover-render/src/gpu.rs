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
use crate::text::{GlyphInstance, PreparedText, Tier};
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

/// Text on the roofs. One instance per quad: a glyph sampled from the SDF atlas, or a flat bar
/// when `cell` is `SOLID`. Six vertices per instance, no vertex or index buffer of its own.
const TEXT_SHADER: &str = r#"
struct Uniforms { view_proj: mat4x4<f32> };
@group(0) @binding(0) var<uniform> u: Uniforms;

const THICKEN: f32 = 0.16;

struct AtlasParams { uv_size: vec2<f32>, cols: u32, solid: u32 };
@group(1) @binding(0) var atlas: texture_2d<f32>;
@group(1) @binding(1) var atlas_sampler: sampler;
@group(1) @binding(2) var<uniform> atlas_params: AtlasParams;

struct InstIn {
    @location(0) rect: vec4<f32>,
    @location(1) z: f32,
    @location(2) cell: u32,
    @location(3) color: u32,
};

struct TextOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) @interpolate(flat) cell: u32,
};

fn unpack_color(c: u32) -> vec4<f32> {
    return vec4<f32>(
        f32(c & 0xffu),
        f32((c >> 8u) & 0xffu),
        f32((c >> 16u) & 0xffu),
        f32((c >> 24u) & 0xffu),
    ) / 255.0;
}

@vertex
fn vs_text(@builtin(vertex_index) vi: u32, inst: InstIn) -> TextOut {
    // Two triangles: (0,0) (1,0) (1,1) and (0,0) (1,1) (0,1).
    var corner = vec2<f32>(0.0, 0.0);
    switch vi {
        case 1u: { corner = vec2<f32>(1.0, 0.0); }
        case 2u, 4u: { corner = vec2<f32>(1.0, 1.0); }
        case 5u: { corner = vec2<f32>(0.0, 1.0); }
        default: { corner = vec2<f32>(0.0, 0.0); }
    }
    let world = inst.rect.xy + corner * inst.rect.zw;

    var out: TextOut;
    out.pos = u.view_proj * vec4<f32>(world.x, world.y, inst.z, 1.0);
    out.color = unpack_color(inst.color);
    out.cell = inst.cell;
    if (inst.cell == atlas_params.solid) {
        out.uv = vec2<f32>(0.0, 0.0);
    } else {
        let grid = vec2<f32>(
            f32(inst.cell % atlas_params.cols),
            f32(inst.cell / atlas_params.cols),
        );
        // The atlas grows downward, so the quad's top edge is the cell's first row.
        let origin = grid * atlas_params.uv_size;
        out.uv = origin + vec2<f32>(corner.x, 1.0 - corner.y) * atlas_params.uv_size;
    }
    return out;
}

// The sample and its derivative must sit in uniform control flow, so solid quads sample too and
// their result is discarded by the select rather than by a branch.
@fragment
fn fs_text(v: TextOut) -> @location(0) vec4<f32> {
    // Signed distance in 0..1 with 0.5 on the glyph edge; widen by one screen pixel for AA.
    // THICKEN biases the edge outward by a fraction of a pixel, which keeps thin stems from
    // washing out once a line is only a handful of pixels tall.
    let signed = textureSample(atlas, atlas_sampler, v.uv).r - 0.5;
    let width = max(fwidth(signed), 1e-5);
    let glyph_alpha = clamp(signed / width + 0.5 + THICKEN, 0.0, 1.0);
    let alpha = select(glyph_alpha, 1.0, v.cell == atlas_params.solid);
    return vec4<f32>(v.color.rgb, v.color.a * alpha);
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
    #[cfg(not(target_arch = "wasm32"))]
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
    /// Request an adapter and device, optionally compatible with a surface. Async so it also runs
    /// in the browser, where WebGPU hands these out through promises.
    pub async fn request(
        instance: &wgpu::Instance,
        surface: Option<&wgpu::Surface<'_>>,
    ) -> Result<Gpu, RenderError> {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: surface,
                ..Default::default()
            })
            .await?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("flyover"),
                required_limits: adapter.limits(),
                ..Default::default()
            })
            .await?;
        let adapter_info = adapter.get_info();
        Ok(Gpu {
            adapter,
            device,
            queue,
            adapter_info,
        })
    }

    /// Blocking [`Gpu::request`] for native callers.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open(
        instance: &wgpu::Instance,
        surface: Option<&wgpu::Surface<'_>>,
    ) -> Result<Gpu, RenderError> {
        pollster::block_on(Gpu::request(instance, surface))
    }

    /// Block until submitted GPU work finishes (native; the browser drives this itself).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn wait(&self) {
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
    }
}

pub struct Pipelines {
    feature_layout: wgpu::BindGroupLayout,
    color: wgpu::RenderPipeline,
    pick: wgpu::RenderPipeline,
    text: wgpu::RenderPipeline,
    atlas_group: wgpu::BindGroup,
    uniform_buffer: wgpu::Buffer,
    uniform_group: wgpu::BindGroup,
}

impl Pipelines {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_format: wgpu::TextureFormat,
    ) -> Self {
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

        let (atlas_layout, atlas_group) = upload_atlas(device, queue);
        let text = text_pipeline(
            device,
            color_format,
            &uniform_layout,
            &atlas_layout,
            depth.clone(),
        );

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
            text,
            atlas_group,
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

/// Upload the baked font atlas as an R8 texture and build the bind group the text shader reads:
/// the texture, a linear sampler, and the cell grid it needs to turn a cell index into uvs.
fn upload_atlas(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
) -> (wgpu::BindGroupLayout, wgpu::BindGroup) {
    let atlas = crate::font::Atlas::bundled();
    let size = wgpu::Extent3d {
        width: atlas.width,
        height: atlas.height,
        depth_or_array_layers: 1,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("font-atlas"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        atlas.pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(atlas.width),
            rows_per_image: Some(atlas.height),
        },
        size,
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("font-atlas"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    });
    // uv_size (x, y), cols, and the sentinel marking an instance as a flat quad.
    let params: [u32; 4] = [
        (atlas.cell_w as f32 / atlas.width as f32).to_bits(),
        (atlas.cell_h as f32 / atlas.height as f32).to_bits(),
        atlas.cols,
        crate::text::SOLID,
    ];
    let params = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("font-atlas-params"),
        contents: bytemuck::cast_slice(&params),
        usage: wgpu::BufferUsages::UNIFORM,
    });

    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("font-atlas"),
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
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("font-atlas"),
        layout: &layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: params.as_entire_binding(),
            },
        ],
    });
    (layout, group)
}

/// The text pipeline: instanced quads, alpha blended, depth-tested against the buildings but not
/// writing depth (the text lies a hair above a roof it can never be occluded by).
fn text_pipeline(
    device: &wgpu::Device,
    color_format: wgpu::TextureFormat,
    uniform_layout: &wgpu::BindGroupLayout,
    atlas_layout: &wgpu::BindGroupLayout,
    depth: Option<wgpu::DepthStencilState>,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("text"),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(TEXT_SHADER)),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("text"),
        bind_group_layouts: &[Some(uniform_layout), Some(atlas_layout)],
        immediate_size: 0,
    });
    let attributes =
        wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32, 2 => Uint32, 3 => Uint32];
    let depth = depth.map(|d| wgpu::DepthStencilState {
        depth_write_enabled: Some(false),
        ..d
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("text"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_text"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<GlyphInstance>() as wgpu::BufferAddress,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &attributes,
            })],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: depth,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_text"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: color_format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
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

    /// Drop least-recently-used tiles until under budget, returning what was dropped so callers
    /// can release anything they keep alongside a tile. Tiles used this frame are kept.
    pub fn evict(&mut self, frame: u64) -> Vec<TileKey> {
        let mut dropped = Vec::new();
        if self.bytes <= self.budget {
            return dropped;
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
                dropped.push(key);
            }
        }
        dropped
    }
}

struct GpuText {
    instances: wgpu::Buffer,
    glyphs: std::ops::Range<u32>,
    strips: std::ops::Range<u32>,
    line_world: f32,
    first_line: u32,
    bytes: u64,
    last_used: u64,
}

/// Uploaded per-file text, evicted least-recently-used under its own byte budget. Separate from
/// [`GpuCache`] so a burst of text near the camera cannot evict the geometry being flown over.
pub struct TextCache {
    files: HashMap<u32, GpuText>,
    bytes: u64,
    budget: u64,
}

impl TextCache {
    pub fn new(budget_bytes: u64) -> Self {
        TextCache {
            files: HashMap::new(),
            bytes: 0,
            budget: budget_bytes,
        }
    }

    pub fn contains(&self, file_id: u32) -> bool {
        self.files.contains_key(&file_id)
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Upload a laid-out file. One buffer copy; the layout itself ran on a worker.
    pub fn upload(&mut self, device: &wgpu::Device, text: PreparedText, frame: u64) {
        if text.instances.is_empty() {
            return;
        }
        let instances = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("text-instances"),
            contents: bytemuck::cast_slice(&text.instances),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let bytes = text.bytes();
        if let Some(old) = self.files.insert(
            text.file_id,
            GpuText {
                instances,
                glyphs: text.glyph_range(),
                strips: text.strip_range(),
                line_world: text.line_world,
                first_line: text.first_line,
                bytes,
                last_used: frame,
            },
        ) {
            self.bytes -= old.bytes;
        }
        self.bytes += bytes;
    }

    pub fn touch(&mut self, file_id: u32, frame: u64) {
        if let Some(t) = self.files.get_mut(&file_id) {
            t.last_used = frame;
        }
    }

    /// World height of one text line for a resident file, or `None` if it is not resident.
    pub fn line_world(&self, file_id: u32) -> Option<f32> {
        self.files.get(&file_id).map(|t| t.line_world)
    }

    /// First line of the slice a resident file holds quads for.
    pub fn window(&self, file_id: u32) -> Option<u32> {
        self.files.get(&file_id).map(|t| t.first_line)
    }

    /// Drop least-recently-used files until under budget. Files drawn this frame are kept.
    pub fn evict(&mut self, frame: u64) {
        if self.bytes <= self.budget {
            return;
        }
        let mut order: Vec<(u64, u32)> =
            self.files.iter().map(|(k, t)| (t.last_used, *k)).collect();
        order.sort();
        for (last_used, id) in order {
            if self.bytes <= self.budget || last_used >= frame {
                break;
            }
            if let Some(t) = self.files.remove(&id) {
                self.bytes -= t.bytes;
            }
        }
    }
}

/// Record the text pass: alpha-blended quads over the already-drawn buildings, loading the color
/// and depth attachments rather than clearing them. `draws` pairs a file id with its tier.
pub fn encode_text_pass(
    encoder: &mut wgpu::CommandEncoder,
    pipelines: &Pipelines,
    cache: &TextCache,
    draws: &[(u32, Tier)],
    color_view: &wgpu::TextureView,
    depth_view: &wgpu::TextureView,
) {
    if draws.is_empty() {
        return;
    }
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("text"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: color_view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Load,
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
            view: depth_view,
            depth_ops: Some(wgpu::Operations {
                load: wgpu::LoadOp::Load,
                store: wgpu::StoreOp::Store,
            }),
            stencil_ops: None,
        }),
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    pass.set_pipeline(&pipelines.text);
    pass.set_bind_group(0, &pipelines.uniform_group, &[]);
    pass.set_bind_group(1, &pipelines.atlas_group, &[]);
    for (file_id, tier) in draws {
        let Some(t) = cache.files.get(file_id) else {
            continue;
        };
        let range = match tier {
            Tier::Glyphs => t.glyphs.clone(),
            Tier::Strips => t.strips.clone(),
            Tier::None => continue,
        };
        if range.is_empty() {
            continue;
        }
        pass.set_vertex_buffer(0, t.instances.slice(..));
        pass.draw(0..6, range);
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
    // Read back only by native screenshots; the browser renders to its canvas instead.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
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

    /// Read the color target back as tightly packed RGBA8 rows (native, blocking).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn read_rgba(&self, gpu: &Gpu) -> Result<Vec<u8>, RenderError> {
        let unpadded = self.width * 4;
        let padded = unpadded.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = copy_region(gpu, &self.color, 0, 0, self.width, self.height, padded);
        let data = map_blocking(gpu, &buffer)?;
        let mut out = Vec::with_capacity((unpadded * self.height) as usize);
        for row in 0..self.height {
            let start = (row * padded) as usize;
            out.extend_from_slice(&data[start..start + unpadded as usize]);
        }
        Ok(out)
    }

    /// Submit a copy of the pick-target pixel at (x, y) into a new mappable buffer. The first 4
    /// bytes, once mapped, are the feature id (0 = background). Callers map it however their
    /// platform allows: blocking natively ([`Offscreen::read_id`]), with a promise in the browser.
    pub fn copy_id(&self, gpu: &Gpu, x: u32, y: u32) -> wgpu::Buffer {
        let x = x.min(self.width.saturating_sub(1));
        let y = y.min(self.height.saturating_sub(1));
        copy_region(
            gpu,
            &self.pick,
            x,
            y,
            1,
            1,
            wgpu::COPY_BYTES_PER_ROW_ALIGNMENT,
        )
    }

    /// Read one feature id from the pick target (native, blocking). 0 means empty background.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn read_id(&self, gpu: &Gpu, x: u32, y: u32) -> Result<u32, RenderError> {
        let data = map_blocking(gpu, &self.copy_id(gpu, x, y))?;
        Ok(u32::from_le_bytes([data[0], data[1], data[2], data[3]]))
    }
}

/// Copy a texture region into a new mappable buffer (rows padded to `padded_row`) and submit.
fn copy_region(
    gpu: &Gpu,
    texture: &wgpu::Texture,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    padded_row: u32,
) -> wgpu::Buffer {
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: (padded_row * height) as wgpu::BufferAddress,
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
    buffer
}

/// Map a readback buffer and wait for it (native only: blocks the calling thread).
#[cfg(not(target_arch = "wasm32"))]
fn map_blocking(gpu: &Gpu, buffer: &wgpu::Buffer) -> Result<Vec<u8>, RenderError> {
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
