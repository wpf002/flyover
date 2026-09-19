//! Renderer: streams quadtree tiles by camera position and draws extruded cells colored and
//! heightened by bound layers. One codebase, two targets: native (wgpu on Metal/Vulkan/DX12, a
//! winit window, headless screenshot and bench) and the browser (wgpu on WebGPU, driven from JS
//! through `crates/flyover-web`).
//!
//! Frame loop, shared by every front end: select tiles by screen-space error ([`lod`]), ask a
//! [`TileProvider`] for the missing ones, upload what it has prepared into an LRU GPU cache
//! ([`gpu`]), and draw each wanted tile or its nearest loaded ancestor while it streams in.
//! Providers decode and mesh tiles off the render thread: a native thread pool ([`cache`]) or
//! browser web workers.
//!
//! TODO(M4): source text (MSDF glyphs above 6 px line height, token strips between 1 and 6 px).

pub mod camera;
pub mod gpu;
pub mod lod;
pub mod mesh;
pub mod tileset;

#[cfg(not(target_arch = "wasm32"))]
pub mod cache;
#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
pub use native::{bench, run_window, screenshot, BenchReport, Shot};

use std::collections::HashSet;
use std::sync::Arc;

use camera::Camera;
use gpu::{Gpu, GpuCache, Pipelines};
use mesh::PreparedTile;
use tileset::{TileKey, TileSet};

pub use gpu::RenderError as Error;

pub const DEFAULT_COLOR_LAYER: &str = "language";
pub const DEFAULT_HEIGHT_LAYER: &str = "lines";
/// GPU memory budget for uploaded tiles (SPEC: 1.5 GB native, 512 MB web).
#[cfg(not(target_arch = "wasm32"))]
const GPU_BUDGET: u64 = 1536 * 1024 * 1024;
#[cfg(target_arch = "wasm32")]
const GPU_BUDGET: u64 = 512 * 1024 * 1024;

#[cfg(not(target_arch = "wasm32"))]
static RENDER_THREAD: std::sync::OnceLock<std::thread::ThreadId> = std::sync::OnceLock::new();

/// Record the calling thread as the render thread. [`Scene::new`] does this; an embedder that
/// drives its own loop calls it once from that loop's thread. Only the first call takes effect.
#[cfg(not(target_arch = "wasm32"))]
pub fn mark_render_thread() {
    let _ = RENDER_THREAD.set(std::thread::current().id());
}

/// Debug builds panic if tile decoding ever runs on the render thread.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn assert_not_render_thread() {
    if let Some(render) = RENDER_THREAD.get() {
        debug_assert!(
            *render != std::thread::current().id(),
            "tile decode on the render thread"
        );
    }
}

#[derive(Debug, Clone)]
pub struct ViewOptions {
    pub color_layer: String,
    pub height_layer: String,
    /// Worker threads for tile decode (native).
    pub threads: usize,
}

impl Default for ViewOptions {
    fn default() -> Self {
        ViewOptions {
            color_layer: DEFAULT_COLOR_LAYER.into(),
            height_layer: DEFAULT_HEIGHT_LAYER.into(),
            threads: std::thread::available_parallelism()
                .map_or(4, |n| n.get().saturating_sub(1).max(1)),
        }
    }
}

/// Where prepared tiles come from. Implementations decode and mesh off the render thread.
pub trait TileProvider {
    /// Ask for a tile. Higher `priority` should load sooner. Repeat requests are ignored.
    fn request(&mut self, key: TileKey, priority: i64);
    fn in_flight(&self, key: TileKey) -> bool;
    /// Tiles that finished since the last call.
    fn drain(&mut self) -> Vec<PreparedTile>;
}

#[cfg(not(target_arch = "wasm32"))]
impl TileProvider for cache::TileLoader {
    fn request(&mut self, key: TileKey, priority: i64) {
        cache::TileLoader::request(self, key, priority);
    }
    fn in_flight(&self, key: TileKey) -> bool {
        cache::TileLoader::in_flight(self, key)
    }
    fn drain(&mut self) -> Vec<PreparedTile> {
        cache::TileLoader::drain(self)
    }
}

/// How layer values become color and height, derived from the manifest and view options.
#[derive(Debug, Clone)]
pub struct RenderParams {
    /// rgb per category of the color layer.
    pub palette: Vec<[f32; 3]>,
    /// World height per log2 unit of the height layer.
    pub height_scale: f32,
    /// Tallest possible extrusion, for culling bounds.
    pub max_height: f32,
}

impl RenderParams {
    pub fn new(manifest: &flyover_tiles::Manifest, opts: &ViewOptions) -> Self {
        let b = manifest.bounds;
        let span = ((b.max_x - b.min_x).max(b.max_y - b.min_y)) as f32;
        let max_value = layer_max(manifest, &opts.height_layer);
        let max_height = span * 0.12;
        RenderParams {
            palette: palette_for(manifest, &opts.color_layer),
            height_scale: max_height / (max_value + 1.0).log2().max(1.0),
            max_height,
        }
    }
}

/// Everything needed to draw one tile set: streaming, the GPU cache, and the draw list.
pub struct Scene {
    pub tiles: Arc<TileSet>,
    provider: Box<dyn TileProvider>,
    cache: GpuCache,
    pipelines: Pipelines,
    max_height: f32,
    frame: u64,
    draw: Vec<TileKey>,
}

impl Scene {
    /// A scene fed by any [`TileProvider`] (the browser build passes a JS-backed one).
    pub fn with_provider(
        gpu: &Gpu,
        tiles: Arc<TileSet>,
        color_format: wgpu::TextureFormat,
        params: &RenderParams,
        provider: Box<dyn TileProvider>,
    ) -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        mark_render_thread();
        Scene {
            tiles,
            provider,
            cache: GpuCache::new(GPU_BUDGET),
            pipelines: Pipelines::new(&gpu.device, color_format),
            max_height: params.max_height,
            frame: 0,
            draw: Vec::new(),
        }
    }

    /// A scene fed by a native worker-thread pool reading the tile set from disk.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new(
        gpu: &Gpu,
        tiles: Arc<TileSet>,
        color_format: wgpu::TextureFormat,
        opts: &ViewOptions,
    ) -> Self {
        let params = RenderParams::new(&tiles.manifest, opts);
        let loader = cache::TileLoader::new(
            Arc::clone(&tiles),
            opts.color_layer.clone(),
            opts.height_layer.clone(),
            tiles.manifest.bounds,
            params.palette.clone(),
            params.height_scale,
            opts.threads,
        );
        Scene::with_provider(gpu, tiles, color_format, &params, Box::new(loader))
    }

    fn wanted(&self, camera: &Camera, aspect: f32, viewport_h: f32) -> Vec<TileKey> {
        lod::select(&self.tiles, camera, aspect, viewport_h, self.max_height)
    }

    /// Select tiles, request the missing ones, upload finished ones, and resolve the draw list.
    pub fn update(&mut self, gpu: &Gpu, camera: &Camera, aspect: f32, viewport_h: f32) {
        self.frame += 1;
        let wanted = self.wanted(camera, aspect, viewport_h);
        for key in &wanted {
            if !self.cache.contains(*key) && !self.provider.in_flight(*key) {
                self.provider.request(*key, i64::from(key.z));
            }
        }
        for prepared in self.provider.drain() {
            self.cache
                .upload(&gpu.device, &self.pipelines, prepared, self.frame);
        }
        let mut seen = HashSet::new();
        let mut draw = Vec::new();
        for key in wanted {
            let mut cur = Some(key);
            while let Some(k) = cur {
                if self.cache.contains(k) {
                    if seen.insert(k) {
                        draw.push(k);
                    }
                    break;
                }
                cur = k.parent();
            }
        }
        for k in &draw {
            self.cache.touch(*k, self.frame);
        }
        self.cache.evict(self.frame);
        self.draw = draw;
    }

    /// Keep updating until every wanted tile is resident (headless paths). False on timeout.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn settle(
        &mut self,
        gpu: &Gpu,
        camera: &Camera,
        aspect: f32,
        viewport_h: f32,
        timeout: std::time::Duration,
    ) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            self.update(gpu, camera, aspect, viewport_h);
            let wanted = self.wanted(camera, aspect, viewport_h);
            if wanted.iter().all(|k| self.cache.contains(*k)) {
                return true;
            }
            if std::time::Instant::now() > deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    pub fn set_camera(&self, gpu: &Gpu, camera: &Camera, aspect: f32) {
        self.pipelines
            .set_view_proj(&gpu.queue, camera.view_proj(aspect));
    }

    pub fn encode(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        color: &wgpu::TextureView,
        depth: &wgpu::TextureView,
        pick: bool,
    ) {
        gpu::encode_pass(
            encoder,
            &self.pipelines,
            &self.cache,
            &self.draw,
            color,
            depth,
            pick,
        );
    }

    pub fn drawn_tiles(&self) -> usize {
        self.draw.len()
    }

    pub fn resident_tiles(&self) -> usize {
        self.cache.len()
    }

    pub fn resident_bytes(&self) -> u64 {
        self.cache.bytes()
    }

    /// Path and line count for a picked feature id, if it is a file.
    pub fn describe(&self, id: u32) -> Option<(String, u32)> {
        self.tiles
            .paths
            .entries
            .binary_search_by_key(&id, |e| e.id)
            .ok()
            .map(|i| {
                (
                    self.tiles.paths.entries[i].path.clone(),
                    self.tiles.paths.entries[i].lines,
                )
            })
    }
}

fn palette_for(manifest: &flyover_tiles::Manifest, layer: &str) -> Vec<[f32; 3]> {
    manifest
        .layers
        .iter()
        .find(|l| l.key == layer)
        .and_then(|l| l.categories.as_ref())
        .map(|cats| cats.iter().map(|c| mesh::parse_color(&c.color)).collect())
        .unwrap_or_default()
}

fn layer_max(manifest: &flyover_tiles::Manifest, layer: &str) -> f32 {
    manifest
        .layers
        .iter()
        .find(|l| l.key == layer)
        .and_then(|l| l.range)
        .map_or(1.0, |r| r.max as f32)
}

/// The scripted path: start at the overview, orbit a quarter turn while descending toward the
/// center and tilting down, ending close over the city but above its tallest roofs (the eye
/// stays at roughly 2x the maximum extrusion height, so the path never clips into buildings).
pub fn bench_camera(start: &Camera, t: f32) -> Camera {
    let mut cam = *start;
    let ease = t * t * (3.0 - 2.0 * t);
    cam.yaw = start.yaw + ease * std::f32::consts::FRAC_PI_2;
    cam.distance = start.distance * (1.0 - 0.78 * ease);
    cam.pitch = start.pitch - ease * 0.25;
    cam
}
