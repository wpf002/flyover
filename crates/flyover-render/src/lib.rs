//! Native renderer (M3): opens a tile set, streams quadtree tiles by camera position, and draws
//! extruded cells colored and heightened by bound layers.
//!
//! Frame loop: select tiles by screen-space error ([`lod`]), request missing ones from a worker
//! pool that decodes and meshes them off the render thread ([`cache`]), upload finished tiles into
//! an LRU GPU cache ([`gpu`]), and draw each wanted tile or its nearest loaded ancestor while it
//! streams in. Three front ends share that loop: an interactive window, a headless screenshot,
//! and a headless `--bench` over a scripted camera path.
//!
//! TODO(M4): source text (MSDF glyphs above 6 px line height, token strips between 1 and 6 px).
//! TODO(M5): wasm32 + WebGPU build, tile fetch over HTTP, decode in web workers.

pub mod cache;
pub mod camera;
pub mod gpu;
pub mod lod;
pub mod mesh;
pub mod tileset;

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use cache::TileLoader;
use camera::{Camera, CameraMode};
use gpu::{Gpu, GpuCache, Offscreen, Pipelines, RenderError};
use tileset::{TileKey, TileSet};

pub use gpu::RenderError as Error;

pub const DEFAULT_COLOR_LAYER: &str = "language";
pub const DEFAULT_HEIGHT_LAYER: &str = "lines";
/// GPU memory budget for uploaded tiles (SPEC: 1.5 GB native).
const NATIVE_BUDGET: u64 = 1536 * 1024 * 1024;

static RENDER_THREAD: OnceLock<ThreadId> = OnceLock::new();

/// Record the calling thread as the render thread. [`Scene::new`] does this; an embedder that
/// drives its own loop calls it once from that loop's thread. Only the first call takes effect.
pub fn mark_render_thread() {
    let _ = RENDER_THREAD.set(std::thread::current().id());
}

/// Debug builds panic if tile decoding ever runs on the render thread.
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

/// Everything needed to draw one tile set: streaming, the GPU cache, and the draw list.
pub struct Scene {
    pub tiles: Arc<TileSet>,
    loader: TileLoader,
    cache: GpuCache,
    pipelines: Pipelines,
    max_height: f32,
    frame: u64,
    draw: Vec<TileKey>,
}

impl Scene {
    pub fn new(
        gpu: &Gpu,
        tiles: Arc<TileSet>,
        color_format: wgpu::TextureFormat,
        opts: &ViewOptions,
    ) -> Self {
        mark_render_thread();
        let bounds = tiles.manifest.bounds;
        let span = ((bounds.max_x - bounds.min_x).max(bounds.max_y - bounds.min_y)) as f32;
        let palette = palette_for(&tiles.manifest, &opts.color_layer);
        let max_value = layer_max(&tiles.manifest, &opts.height_layer);
        let max_height = span * 0.12;
        let height_scale = max_height / (max_value + 1.0).log2().max(1.0);
        let loader = TileLoader::new(
            Arc::clone(&tiles),
            opts.color_layer.clone(),
            opts.height_layer.clone(),
            bounds,
            palette,
            height_scale,
            opts.threads,
        );
        Scene {
            tiles,
            loader,
            cache: GpuCache::new(NATIVE_BUDGET),
            pipelines: Pipelines::new(&gpu.device, color_format),
            max_height,
            frame: 0,
            draw: Vec::new(),
        }
    }

    fn wanted(&self, camera: &Camera, aspect: f32, viewport_h: f32) -> Vec<TileKey> {
        lod::select(&self.tiles, camera, aspect, viewport_h, self.max_height)
    }

    /// Select tiles, request the missing ones, upload finished ones, and resolve the draw list.
    pub fn update(&mut self, gpu: &Gpu, camera: &Camera, aspect: f32, viewport_h: f32) {
        self.frame += 1;
        let wanted = self.wanted(camera, aspect, viewport_h);
        for key in &wanted {
            if !self.cache.contains(*key) && !self.loader.in_flight(*key) {
                self.loader.request(*key, i64::from(key.z));
            }
        }
        for prepared in self.loader.drain() {
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
    pub fn settle(
        &mut self,
        gpu: &Gpu,
        camera: &Camera,
        aspect: f32,
        viewport_h: f32,
        timeout: Duration,
    ) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            self.update(gpu, camera, aspect, viewport_h);
            let wanted = self.wanted(camera, aspect, viewport_h);
            if wanted.iter().all(|k| self.cache.contains(*k)) {
                return true;
            }
            if Instant::now() > deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
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

fn headless(tileset: &Path, opts: &ViewOptions) -> Result<(Gpu, Scene), RenderError> {
    let tiles = Arc::new(TileSet::open(tileset)?);
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let gpu = Gpu::open(&instance, None)?;
    let scene = Scene::new(&gpu, tiles, gpu::OFFSCREEN_FORMAT, opts);
    Ok((gpu, scene))
}

/// Result of a headless screenshot.
pub struct Shot {
    pub adapter: String,
    pub drawn_tiles: usize,
    pub settled: bool,
    /// Fraction of pixels that are not background, 0..1.
    pub coverage: f32,
    /// What's under the center pixel: feature id, and path/lines if it's a file.
    pub center: (u32, Option<(String, u32)>),
}

/// Render one frame to a PNG, headless. `path_t` places the camera along the `--bench` path
/// (0 = overview, 1 = low over the map).
pub fn screenshot(
    tileset: &Path,
    out_png: &Path,
    width: u32,
    height: u32,
    path_t: f32,
    opts: &ViewOptions,
) -> Result<Shot, RenderError> {
    let (gpu, mut scene) = headless(tileset, opts)?;
    let offscreen = Offscreen::new(&gpu.device, width, height);
    let camera = bench_camera(
        &Camera::framing(scene.tiles.manifest.bounds),
        path_t.clamp(0.0, 1.0),
    );
    let aspect = width as f32 / height as f32;
    let settled = scene.settle(
        &gpu,
        &camera,
        aspect,
        height as f32,
        Duration::from_secs(120),
    );
    scene.set_camera(&gpu, &camera, aspect);

    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("shot"),
        });
    scene.encode(
        &mut encoder,
        &offscreen.color_view,
        &offscreen.depth_view,
        false,
    );
    scene.encode(
        &mut encoder,
        &offscreen.pick_view,
        &offscreen.depth_view,
        true,
    );
    gpu.queue.submit(Some(encoder.finish()));

    let rgba = offscreen.read_rgba(&gpu)?;
    write_png(out_png, width, height, &rgba)?;
    let id = offscreen.read_id(&gpu, width / 2, height / 2)?;
    Ok(Shot {
        adapter: gpu.adapter_info.name.clone(),
        drawn_tiles: scene.drawn_tiles(),
        settled,
        coverage: coverage(&rgba),
        center: (id, scene.describe(id)),
    })
}

/// Fraction of pixels that differ from the clear color.
fn coverage(rgba: &[u8]) -> f32 {
    let bg = [
        (gpu::CLEAR_RGB[0] * 255.0).round() as i32,
        (gpu::CLEAR_RGB[1] * 255.0).round() as i32,
        (gpu::CLEAR_RGB[2] * 255.0).round() as i32,
    ];
    let px = rgba.len() / 4;
    let drawn = rgba
        .chunks_exact(4)
        .filter(|p| (0..3).any(|c| (i32::from(p[c]) - bg[c]).abs() > 3))
        .count();
    drawn as f32 / px.max(1) as f32
}

fn write_png(path: &Path, width: u32, height: u32, rgba: &[u8]) -> Result<(), RenderError> {
    let file = std::fs::File::create(path)?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(rgba)?;
    Ok(())
}

/// Frame-time statistics from a scripted camera path.
pub struct BenchReport {
    pub adapter: String,
    pub frames: usize,
    pub width: u32,
    pub height: u32,
    pub avg_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
    pub avg_tiles_drawn: f64,
    pub peak_resident_tiles: usize,
    pub peak_resident_mb: f64,
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

/// Render `frames` frames headlessly along [`bench_camera`], timing each frame end to end
/// (select + stream + upload + encode + submit + GPU wait). Tiles stream in asynchronously, as
/// they would in the window.
pub fn bench(
    tileset: &Path,
    frames: usize,
    width: u32,
    height: u32,
    opts: &ViewOptions,
) -> Result<BenchReport, RenderError> {
    let (gpu, mut scene) = headless(tileset, opts)?;
    let offscreen = Offscreen::new(&gpu.device, width, height);
    let start = Camera::framing(scene.tiles.manifest.bounds);
    let aspect = width as f32 / height as f32;

    let mut times = Vec::with_capacity(frames);
    let mut drawn = 0usize;
    let mut peak_tiles = 0usize;
    let mut peak_bytes = 0u64;
    for i in 0..frames {
        let t = if frames > 1 {
            i as f32 / (frames - 1) as f32
        } else {
            0.0
        };
        let camera = bench_camera(&start, t);
        let t0 = Instant::now();
        scene.update(&gpu, &camera, aspect, height as f32);
        scene.set_camera(&gpu, &camera, aspect);
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("bench"),
            });
        scene.encode(
            &mut encoder,
            &offscreen.color_view,
            &offscreen.depth_view,
            false,
        );
        gpu.queue.submit(Some(encoder.finish()));
        gpu.wait();
        times.push(t0.elapsed().as_secs_f64() * 1000.0);
        drawn += scene.drawn_tiles();
        peak_tiles = peak_tiles.max(scene.resident_tiles());
        peak_bytes = peak_bytes.max(scene.resident_bytes());
    }

    let mut sorted = times.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let pct = |p: f64| sorted[((sorted.len() as f64 - 1.0) * p).round() as usize];
    Ok(BenchReport {
        adapter: gpu.adapter_info.name.clone(),
        frames,
        width,
        height,
        avg_ms: times.iter().sum::<f64>() / times.len().max(1) as f64,
        p50_ms: pct(0.50),
        p95_ms: pct(0.95),
        p99_ms: pct(0.99),
        max_ms: *sorted.last().unwrap_or(&0.0),
        avg_tiles_drawn: drawn as f64 / frames.max(1) as f64,
        peak_resident_tiles: peak_tiles,
        peak_resident_mb: peak_bytes as f64 / (1024.0 * 1024.0),
    })
}

/// Open an interactive window. Map mode: drag to orbit, scroll to zoom, WASD to pan.
/// Fly mode (Tab): WASD to move, Q/E down/up, drag to look, scroll to change speed.
/// Hovering a block shows its path and line count in the title bar.
pub fn run_window(tileset: &Path, opts: ViewOptions) -> Result<(), RenderError> {
    let tiles = Arc::new(TileSet::open(tileset)?);
    let event_loop =
        winit::event_loop::EventLoop::new().map_err(|e| RenderError::Window(e.to_string()))?;
    let mut app = window::App::new(tiles, opts);
    event_loop
        .run_app(&mut app)
        .map_err(|e| RenderError::Window(e.to_string()))?;
    app.error.map_or(Ok(()), Err)
}

mod window {
    use std::sync::Arc;
    use std::time::Instant;

    use glam::Vec3;
    use winit::application::ApplicationHandler;
    use winit::dpi::LogicalSize;
    use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
    use winit::event_loop::ActiveEventLoop;
    use winit::keyboard::{KeyCode, PhysicalKey};
    use winit::window::{Window, WindowId};

    use super::{Camera, CameraMode, Gpu, Offscreen, RenderError, Scene, TileSet, ViewOptions};

    #[derive(Default)]
    struct Input {
        forward: bool,
        back: bool,
        left: bool,
        right: bool,
        up: bool,
        down: bool,
        dragging: bool,
        cursor: (f64, f64),
        last_cursor: Option<(f64, f64)>,
        pick_pending: bool,
    }

    struct Live {
        window: Arc<Window>,
        surface: wgpu::Surface<'static>,
        gpu: Gpu,
        config: wgpu::SurfaceConfiguration,
        depth: wgpu::TextureView,
        pick: Offscreen,
        scene: Scene,
        camera: Camera,
        input: Input,
        last: Instant,
        span: f32,
        fly_speed: f32,
        title: String,
    }

    pub struct App {
        tiles: Arc<TileSet>,
        opts: ViewOptions,
        live: Option<Live>,
        pub error: Option<RenderError>,
    }

    impl App {
        pub fn new(tiles: Arc<TileSet>, opts: ViewOptions) -> Self {
            App {
                tiles,
                opts,
                live: None,
                error: None,
            }
        }

        fn start(&mut self, event_loop: &ActiveEventLoop) -> Result<(), RenderError> {
            let attrs = Window::default_attributes()
                .with_title(format!("Flyover — {}", self.tiles.manifest.repo.name))
                .with_inner_size(LogicalSize::new(1440.0, 900.0));
            let window = Arc::new(
                event_loop
                    .create_window(attrs)
                    .map_err(|e| RenderError::Window(e.to_string()))?,
            );
            let instance =
                wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
            let surface = instance.create_surface(Arc::clone(&window))?;
            let gpu = Gpu::open(&instance, Some(&surface))?;
            let size = window.inner_size();
            let (w, h) = (size.width.max(1), size.height.max(1));
            let config = surface
                .get_default_config(&gpu.adapter, w, h)
                .ok_or_else(|| {
                    RenderError::Window("surface is not supported by this adapter".into())
                })?;
            surface.configure(&gpu.device, &config);
            let scene = Scene::new(&gpu, Arc::clone(&self.tiles), config.format, &self.opts);
            let bounds = self.tiles.manifest.bounds;
            let span = ((bounds.max_x - bounds.min_x).max(bounds.max_y - bounds.min_y)) as f32;
            self.live = Some(Live {
                depth: super::gpu::depth_view(&gpu.device, w, h),
                pick: Offscreen::new(&gpu.device, w, h),
                camera: Camera::framing(bounds),
                window,
                surface,
                gpu,
                config,
                scene,
                input: Input::default(),
                last: Instant::now(),
                span,
                fly_speed: 1.0,
                title: String::new(),
            });
            Ok(())
        }
    }

    impl ApplicationHandler for App {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            if self.live.is_none() {
                if let Err(e) = self.start(event_loop) {
                    self.error = Some(e);
                    event_loop.exit();
                }
            }
        }

        fn window_event(
            &mut self,
            event_loop: &ActiveEventLoop,
            _id: WindowId,
            event: WindowEvent,
        ) {
            let Some(live) = self.live.as_mut() else {
                return;
            };
            match event {
                WindowEvent::CloseRequested => event_loop.exit(),
                WindowEvent::Resized(size) => {
                    live.config.width = size.width.max(1);
                    live.config.height = size.height.max(1);
                    live.surface.configure(&live.gpu.device, &live.config);
                    live.depth = super::gpu::depth_view(
                        &live.gpu.device,
                        live.config.width,
                        live.config.height,
                    );
                    live.pick =
                        Offscreen::new(&live.gpu.device, live.config.width, live.config.height);
                }
                WindowEvent::KeyboardInput { event, .. } => {
                    let down = event.state == ElementState::Pressed;
                    if let PhysicalKey::Code(code) = event.physical_key {
                        match code {
                            KeyCode::KeyW => live.input.forward = down,
                            KeyCode::KeyS => live.input.back = down,
                            KeyCode::KeyA => live.input.left = down,
                            KeyCode::KeyD => live.input.right = down,
                            KeyCode::KeyE => live.input.up = down,
                            KeyCode::KeyQ => live.input.down = down,
                            KeyCode::Tab if down => toggle_mode(&mut live.camera),
                            KeyCode::Escape if down => event_loop.exit(),
                            _ => {}
                        }
                    }
                }
                WindowEvent::MouseInput {
                    state,
                    button: MouseButton::Left,
                    ..
                } => {
                    live.input.dragging = state == ElementState::Pressed;
                    live.input.last_cursor = None;
                }
                WindowEvent::CursorMoved { position, .. } => {
                    live.input.cursor = (position.x, position.y);
                    if live.input.dragging {
                        if let Some((lx, ly)) = live.input.last_cursor {
                            let (dx, dy) = ((position.x - lx) as f32, (position.y - ly) as f32);
                            look(&mut live.camera, dx, dy);
                        }
                        live.input.last_cursor = Some((position.x, position.y));
                    } else {
                        live.input.pick_pending = true;
                    }
                }
                WindowEvent::MouseWheel { delta, .. } => {
                    let lines = match delta {
                        MouseScrollDelta::LineDelta(_, y) => y,
                        MouseScrollDelta::PixelDelta(p) => (p.y / 40.0) as f32,
                    };
                    match live.camera.mode {
                        CameraMode::Map => {
                            live.camera.distance = (live.camera.distance * 0.9f32.powf(lines))
                                .clamp(live.span * 0.005, live.span * 6.0)
                        }
                        CameraMode::Fly => {
                            live.fly_speed = (live.fly_speed * 1.2f32.powf(lines)).clamp(0.05, 50.0)
                        }
                    }
                }
                WindowEvent::RedrawRequested => {
                    if let Err(e) = frame(live) {
                        self.error = Some(e);
                        event_loop.exit();
                    }
                }
                _ => {}
            }
        }

        fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
            if let Some(live) = self.live.as_ref() {
                live.window.request_redraw();
            }
        }
    }

    fn toggle_mode(cam: &mut Camera) {
        match cam.mode {
            CameraMode::Map => {
                let eye = cam.eye();
                let dir = (cam.center - eye).normalize_or_zero();
                cam.pos = eye;
                cam.fly_yaw = dir.y.atan2(dir.x);
                cam.fly_pitch = dir.z.asin();
                cam.mode = CameraMode::Fly;
            }
            CameraMode::Fly => cam.mode = CameraMode::Map,
        }
    }

    fn look(cam: &mut Camera, dx: f32, dy: f32) {
        let k = 0.005;
        match cam.mode {
            CameraMode::Map => {
                cam.yaw -= dx * k;
                cam.pitch = (cam.pitch + dy * k).clamp(-1.55, -0.05);
            }
            CameraMode::Fly => {
                cam.fly_yaw -= dx * k;
                cam.fly_pitch = (cam.fly_pitch - dy * k).clamp(-1.55, 1.55);
            }
        }
    }

    fn movement(live: &mut Live, dt: f32) {
        let i = &live.input;
        let axis = |pos: bool, neg: bool| f32::from(u8::from(pos)) - f32::from(u8::from(neg));
        let (fwd, strafe, lift) = (
            axis(i.forward, i.back),
            axis(i.right, i.left),
            axis(i.up, i.down),
        );
        let cam = &mut live.camera;
        match cam.mode {
            CameraMode::Map => {
                let (sy, cy) = cam.yaw.sin_cos();
                let forward = Vec3::new(cy, sy, 0.0);
                let right = Vec3::new(sy, -cy, 0.0);
                let speed = cam.distance * 0.8 * dt;
                cam.center += (forward * fwd + right * strafe) * speed;
            }
            CameraMode::Fly => {
                let forward = cam.fly_forward();
                let right = forward.cross(Vec3::Z).normalize_or_zero();
                // Speed scales with altitude so skimming the roofs stays controllable.
                let speed = cam.pos.z.abs().max(live.span * 0.01) * live.fly_speed * dt;
                cam.pos += (forward * fwd + right * strafe + Vec3::Z * lift) * speed;
            }
        }
    }

    fn frame(live: &mut Live) -> Result<(), RenderError> {
        let now = Instant::now();
        let dt = (now - live.last).as_secs_f32().min(0.1);
        live.last = now;
        movement(live, dt);

        let (w, h) = (live.config.width, live.config.height);
        let aspect = w as f32 / h as f32;
        live.scene.update(&live.gpu, &live.camera, aspect, h as f32);
        live.scene.set_camera(&live.gpu, &live.camera, aspect);

        let texture = match live.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                live.surface.configure(&live.gpu.device, &live.config);
                return Ok(());
            }
            _ => return Ok(()),
        };
        let view = texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = live
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });
        live.scene.encode(&mut encoder, &view, &live.depth, false);
        let picking = std::mem::take(&mut live.input.pick_pending);
        if picking {
            live.scene.encode(
                &mut encoder,
                &live.pick.pick_view,
                &live.pick.depth_view,
                true,
            );
        }
        live.gpu.queue.submit(Some(encoder.finish()));
        live.gpu.queue.present(texture);

        if picking {
            let (cx, cy) = live.input.cursor;
            let id = live
                .pick
                .read_id(&live.gpu, cx.max(0.0) as u32, cy.max(0.0) as u32)?;
            let title = match live.scene.describe(id) {
                Some((path, lines)) => format!("Flyover — {path} · {lines} lines"),
                None => format!("Flyover — {}", live.scene.tiles.manifest.repo.name),
            };
            if title != live.title {
                live.window.set_title(&title);
                live.title = title;
            }
        }
        Ok(())
    }
}
