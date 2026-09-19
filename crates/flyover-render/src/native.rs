//! Native front ends: a headless screenshot, the `--bench` scripted camera path, and the
//! interactive winit window. All three drive the same [`Scene`] loop as the browser build.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::camera::Camera;
use crate::gpu::{self, Gpu, Offscreen, RenderError};
use crate::tileset::TileSet;
use crate::{bench_camera, Scene, ViewOptions};

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

    use winit::application::ApplicationHandler;
    use winit::dpi::LogicalSize;
    use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
    use winit::event_loop::ActiveEventLoop;
    use winit::keyboard::{KeyCode, PhysicalKey};
    use winit::window::{Window, WindowId};

    use super::{Camera, Gpu, Offscreen, RenderError, Scene, TileSet, ViewOptions};
    use crate::camera::Controls;

    #[derive(Default)]
    struct Input {
        held: Controls,
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
                            KeyCode::KeyW => live.input.held.forward = down,
                            KeyCode::KeyS => live.input.held.back = down,
                            KeyCode::KeyA => live.input.held.left = down,
                            KeyCode::KeyD => live.input.held.right = down,
                            KeyCode::KeyE => live.input.held.up = down,
                            KeyCode::KeyQ => live.input.held.down = down,
                            KeyCode::Tab if down => live.camera.toggle_mode(),
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
                            live.camera.look(dx, dy);
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
                    live.camera.scroll(lines, live.span, &mut live.fly_speed);
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

    fn frame(live: &mut Live) -> Result<(), RenderError> {
        let now = Instant::now();
        let dt = (now - live.last).as_secs_f32().min(0.1);
        live.last = now;
        live.camera
            .step(&live.input.held, dt, live.span, live.fly_speed);

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
