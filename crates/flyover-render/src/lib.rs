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
//! Source text rides on the same loop: a second provider fetches a file's `.ftx` when its roof is
//! large enough on screen, lays it out on a worker, and the text pass draws glyphs or token bars
//! over the roofs ([`text`]).

pub mod camera;
pub mod font;
pub mod gpu;
pub mod lod;
pub mod mesh;
pub mod text;
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
use gpu::{Gpu, GpuCache, Pipelines, TextCache};
use mesh::{PreparedTile, Roof};
use text::{Placement, PreparedText, TextRequest, Tier};
use tileset::{TileKey, TileSet};

pub use gpu::RenderError as Error;

pub const DEFAULT_COLOR_LAYER: &str = "language";
pub const DEFAULT_HEIGHT_LAYER: &str = "lines";
/// GPU memory budget for uploaded tiles (SPEC: 1.5 GB native, 512 MB web).
#[cfg(not(target_arch = "wasm32"))]
const GPU_BUDGET: u64 = 1536 * 1024 * 1024;
#[cfg(target_arch = "wasm32")]
const GPU_BUDGET: u64 = 512 * 1024 * 1024;
/// Text has its own budget so a street of readable roofs cannot evict the city around them.
#[cfg(not(target_arch = "wasm32"))]
const TEXT_BUDGET: u64 = 256 * 1024 * 1024;
#[cfg(target_arch = "wasm32")]
const TEXT_BUDGET: u64 = 96 * 1024 * 1024;
/// New text fetches started per frame. A fast pan would otherwise queue thousands of files.
const TEXT_REQUESTS_PER_FRAME: usize = 8;
/// Line height in pixels that [`Scene::focus_camera`] aims for on a file too long to frame whole.
const FOCUS_LINE_PX: f32 = 14.0;

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
    /// Start the camera over the first file whose path contains this, close enough to read it.
    pub focus: Option<String>,
    /// Draw source text on the roofs. Off renders buildings only.
    pub text: bool,
}

impl Default for ViewOptions {
    fn default() -> Self {
        ViewOptions {
            color_layer: DEFAULT_COLOR_LAYER.into(),
            height_layer: DEFAULT_HEIGHT_LAYER.into(),
            threads: std::thread::available_parallelism()
                .map_or(4, |n| n.get().saturating_sub(1).max(1)),
            focus: None,
            text: true,
        }
    }
}

/// Where prepared text comes from. Implementations fetch and lay out off the render thread.
pub trait TextProvider {
    fn request(&mut self, request: TextRequest);
    fn in_flight(&self, file_id: u32) -> bool;
    fn drain(&mut self) -> Vec<PreparedText>;
}

#[cfg(not(target_arch = "wasm32"))]
impl TextProvider for cache::TextLoader {
    fn request(&mut self, request: TextRequest) {
        cache::TextLoader::request(self, request);
    }
    fn in_flight(&self, file_id: u32) -> bool {
        cache::TextLoader::in_flight(self, file_id)
    }
    fn drain(&mut self) -> Vec<PreparedText> {
        cache::TextLoader::drain(self)
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
    // Text. `roofs` is dropped in lockstep with the tile cache, so the per-frame scan only ever
    // visits files that are actually on screen.
    text_provider: Option<Box<dyn TextProvider>>,
    text_cache: TextCache,
    roofs: std::collections::HashMap<TileKey, Vec<Roof>>,
    text_draw: Vec<(u32, Tier)>,
    metrics: font::Metrics,
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
            pipelines: Pipelines::new(&gpu.device, &gpu.queue, color_format),
            max_height: params.max_height,
            frame: 0,
            draw: Vec::new(),
            text_provider: None,
            text_cache: TextCache::new(TEXT_BUDGET),
            roofs: std::collections::HashMap::new(),
            text_draw: Vec::new(),
            metrics: font::Atlas::bundled().metrics,
        }
    }

    /// Attach the source of `.ftx` text tiles. Without one the scene draws buildings only.
    pub fn set_text_provider(&mut self, provider: Box<dyn TextProvider>) {
        self.text_provider = Some(provider);
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
        let mut scene = Scene::with_provider(
            gpu,
            Arc::clone(&tiles),
            color_format,
            &params,
            Box::new(loader),
        );
        if opts.text && tiles.manifest.has_text {
            scene.set_text_provider(Box::new(cache::TextLoader::new(
                tiles,
                text::Palette::default(),
                opts.threads,
            )));
        }
        scene
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
            if !prepared.roofs.is_empty() {
                self.roofs.insert(prepared.key, prepared.roofs.clone());
            }
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
        for key in self.cache.evict(self.frame) {
            self.roofs.remove(&key);
        }
        self.draw = draw;
        self.update_text(gpu, camera, aspect, viewport_h);
    }

    /// Decide which visible files show source this frame, fetch the ones that are missing, and
    /// upload whatever has arrived. Nothing here decodes: the scan is over roof rectangles that
    /// are already in memory, and the layout happened on a worker.
    fn update_text(&mut self, gpu: &Gpu, camera: &Camera, aspect: f32, viewport_h: f32) {
        let Some(mut provider) = self.text_provider.take() else {
            return;
        };
        let _ = aspect;
        let px_per_world = viewport_h / (2.0 * (camera.fovy * 0.5).tan());
        let eye = camera.eye();
        // Where the view ray meets a roof is the line the camera is reading, and that picks the
        // window that roof lays out. The plane differs per roof, so only the ray is shared.
        let dir = camera.forward();
        let mut draws = Vec::new();
        let mut started = 0usize;

        for key in &self.draw {
            let Some(roofs) = self.roofs.get(key) else {
                continue;
            };
            for roof in roofs {
                let center = glam::Vec3::new(
                    roof.rect[0] + roof.rect[2] * 0.5,
                    roof.rect[1] + roof.rect[3] * 0.5,
                    roof.z,
                );
                let dist = (center - eye).length().max(1e-6);
                let per_world = px_per_world / dist;
                // Cheapest possible rejection first. A drawn tile over a repo the size of
                // Chromium carries thousands of roofs and almost none of them are readable, so
                // anything past this multiply — a cache probe, a search through paths.bin — would
                // be paid per roof per frame for text that never draws.
                if roof.rect[3] * per_world < text::ROOF_MIN_PX {
                    continue;
                }
                let resident = self.text_cache.line_world(roof.file_id);
                let total_lines = self.tiles.lines_of(roof.file_id).unwrap_or(1);
                let line_world = resident.unwrap_or_else(|| {
                    text::estimated_line_height(&self.metrics, roof.rect, total_lines)
                });
                let tier = text::tier(line_world * per_world);
                if tier == Tier::None {
                    continue;
                }
                let aim_y = if dir.z.abs() > 1e-6 {
                    let t = (roof.z - eye.z) / dir.z;
                    if t > 0.0 {
                        eye.y + dir.y * t
                    } else {
                        center.y
                    }
                } else {
                    center.y
                };
                let want = window_at(roof, line_world, total_lines, aim_y);
                if resident == Some(line_world)
                    && self.text_cache.window(roof.file_id) == Some(want)
                {
                    self.text_cache.touch(roof.file_id, self.frame);
                    draws.push((roof.file_id, tier));
                    continue;
                }
                // Either the file has no text yet or the camera has moved off the slice it holds.
                // A resident file keeps drawing its old window until the new one lands.
                if resident.is_some() {
                    self.text_cache.touch(roof.file_id, self.frame);
                    draws.push((roof.file_id, tier));
                }
                if started < TEXT_REQUESTS_PER_FRAME && !provider.in_flight(roof.file_id) {
                    provider.request(TextRequest {
                        placement: Placement {
                            file_id: roof.file_id,
                            rect: roof.rect,
                            z: roof.z,
                            first_line: want,
                        },
                        // Nearest first: the heap pops the largest priority.
                        priority: -(dist * 1000.0) as i64,
                    });
                    started += 1;
                }
            }
        }

        for prepared in provider.drain() {
            self.text_cache.upload(&gpu.device, prepared, self.frame);
        }
        self.text_cache.evict(self.frame);
        self.text_draw = draws;
        self.text_provider = Some(provider);
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
        // Picking reads feature ids off the buildings; text must not overwrite them.
        if !pick {
            gpu::encode_text_pass(
                encoder,
                &self.pipelines,
                &self.text_cache,
                &self.text_draw,
                color,
                depth,
            );
        }
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

    /// Files whose source is drawn this frame, and how many of those are at the glyph tier.
    pub fn text_stats(&self) -> (usize, usize, u64) {
        let glyphs = self
            .text_draw
            .iter()
            .filter(|(_, t)| *t == Tier::Glyphs)
            .count();
        (self.text_draw.len(), glyphs, self.text_cache.bytes())
    }

    /// A camera placed over the first resident file whose path contains `needle`, close enough
    /// for its source to be readable. `None` until that file's tile has loaded.
    ///
    /// A short file is framed whole. A long one does not fit at a readable size — 600 lines on a
    /// 900 px viewport is about one pixel a line — so the camera drops closer until a line is
    /// [`FOCUS_LINE_PX`] tall and shows that part of the file instead.
    pub fn focus_camera(&self, needle: &str, aspect: f32, viewport_h: f32) -> Option<Camera> {
        let mut best: Option<&Roof> = None;
        for roof in self.roofs.values().flatten() {
            match self.describe(roof.file_id) {
                Some((path, _)) if path.contains(needle) => {}
                _ => continue,
            }
            // Ties go to the larger roof, so a `needle` matching several files picks the one
            // whose text is easiest to read.
            if best.is_none_or(|b| roof.rect[2] * roof.rect[3] > b.rect[2] * b.rect[3]) {
                best = Some(roof);
            }
        }
        let roof = best?;
        let b = self.tiles.manifest.bounds;
        let world_span = ((b.max_x - b.min_x).max(b.max_y - b.min_y)) as f32;
        let mut camera = Camera::over(
            glam::Vec3::new(
                roof.rect[0] + roof.rect[2] * 0.5,
                roof.rect[1] + roof.rect[3] * 0.5,
                roof.z,
            ),
            (roof.rect[2], roof.rect[3]),
            aspect,
            world_span,
        );
        // Exact once the file's text is resident; the estimate is used before it arrives.
        let line_world = self.text_cache.line_world(roof.file_id).unwrap_or_else(|| {
            let lines = self.tiles.lines_of(roof.file_id).unwrap_or(1);
            text::estimated_line_height(&self.metrics, roof.rect, lines)
        });
        if line_world > 0.0 {
            let px_per_world = viewport_h / (2.0 * (camera.fovy * 0.5).tan());
            let readable = line_world * px_per_world / FOCUS_LINE_PX;
            camera.distance = camera.distance.min(readable);
            camera.near = (camera.distance * 0.002).max(1e-5);
        }
        Some(camera)
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

/// The window of lines a file should lay out, given where on its roof the camera is aimed.
/// The block is centred on the roof, so its top edge follows from the file's total height.
fn window_at(roof: &Roof, line_world: f32, total_lines: u32, aim_y: f32) -> u32 {
    if line_world <= 0.0 {
        return 0;
    }
    let block_h = total_lines as f32 * line_world;
    let y_top = roof.rect[1] + (roof.rect[3] + block_h) * 0.5;
    let line = ((y_top - aim_y) / line_world).clamp(0.0, total_lines.saturating_sub(1) as f32);
    text::Placement::window_for(line as u32)
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

/// Ease `t` in and out, so a scripted path starts and ends at rest.
fn ease(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// A dive from the overview onto one file's roof, crossing every text tier on the way: the
/// distance closes geometrically (constant perceived speed) while the camera swings to the
/// target's heading. Used by `--bench --focus` to time the descent that makes source readable.
pub fn dive_camera(start: &Camera, target: &Camera, t: f32) -> Camera {
    let e = ease(t.clamp(0.0, 1.0));
    let mut cam = *start;
    cam.center = start.center.lerp(target.center, e);
    cam.distance = start.distance * (target.distance / start.distance).powf(e);
    cam.yaw = start.yaw + (target.yaw - start.yaw) * e;
    cam.pitch = start.pitch + (target.pitch - start.pitch) * e;
    cam.near = start.near * (target.near / start.near).powf(e);
    cam.far = target.far.max(start.far);
    cam
}

/// The scripted path: start at the overview, orbit a quarter turn while descending toward the
/// center and tilting down, ending close over the city but above its tallest roofs (the eye
/// stays at roughly 2x the maximum extrusion height, so the path never clips into buildings).
pub fn bench_camera(start: &Camera, t: f32) -> Camera {
    let mut cam = *start;
    let e = ease(t);
    cam.yaw = start.yaw + e * std::f32::consts::FRAC_PI_2;
    cam.distance = start.distance * (1.0 - 0.78 * e);
    cam.pitch = start.pitch - e * 0.25;
    cam
}
