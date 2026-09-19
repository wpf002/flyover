//! Browser build of the renderer (SPEC M5): the same `Scene` loop as the native viewer, on
//! WebGPU, exported to JS with wasm-bindgen.
//!
//! Split of work, so nothing heavy runs on the page's main thread:
//! - the page's JS owns the animation frame loop and input events, and calls [`WebViewer`];
//! - [`WebViewer`] selects tiles (LOD), draws, and hands back the addresses it wants;
//! - JS web workers fetch those tiles over HTTP and call [`prepare_tile`] (decode + mesh) in their
//!   own wasm instance, then transfer the finished vertex/feature bytes back;
//! - [`WebViewer::deliver`] only uploads those bytes to the GPU.
//!
//! Source text takes the same route: [`WebViewer::take_text_requests`] names the files whose roofs
//! are large enough to read, a worker fetches the `.ftx` and calls [`prepare_text`], and
//! [`WebViewer::deliver_text`] uploads the instance buffer.
//!
//! On native targets this crate is empty.

#![cfg(target_arch = "wasm32")]

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use flyover_render::camera::{Camera, CameraMode, Controls};
use flyover_render::gpu::{depth_view, Gpu, Offscreen};
use flyover_render::mesh::{self, FeatureRaw, PreparedTile, Roof, Vertex};
use flyover_render::text::{self, GlyphInstance, Placement, PreparedText, TextRequest};
use flyover_render::tileset::{decode_text, decode_tile, TileKey, TileSet};
use flyover_render::{RenderParams, Scene, TextProvider, TileProvider, ViewOptions};
use flyover_tiles::Bounds;
use wasm_bindgen::prelude::*;

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
}

fn js_err(e: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&e.to_string())
}

/// Tiles requested by the scene and not yet delivered, plus deliveries awaiting upload.
#[derive(Default)]
struct Queue {
    requested: Vec<TileKey>,
    inflight: HashSet<TileKey>,
    ready: Vec<PreparedTile>,
}

/// Text requested by the scene and not yet delivered, plus deliveries awaiting upload.
#[derive(Default)]
struct TextQueue {
    requested: Vec<Placement>,
    inflight: HashSet<u32>,
    ready: Vec<PreparedText>,
}

/// A [`TextProvider`] whose fetch and layout happen in JS web workers.
struct JsTextProvider(Rc<RefCell<TextQueue>>);

impl TextProvider for JsTextProvider {
    fn request(&mut self, request: TextRequest) {
        let mut q = self.0.borrow_mut();
        if q.inflight.insert(request.placement.file_id) {
            q.requested.push(request.placement);
        }
    }
    fn in_flight(&self, file_id: u32) -> bool {
        self.0.borrow().inflight.contains(&file_id)
    }
    fn drain(&mut self) -> Vec<PreparedText> {
        std::mem::take(&mut self.0.borrow_mut().ready)
    }
}

/// A [`TileProvider`] whose work happens in JS web workers.
struct JsProvider(Rc<RefCell<Queue>>);

impl TileProvider for JsProvider {
    fn request(&mut self, key: TileKey, _priority: i64) {
        let mut q = self.0.borrow_mut();
        if q.inflight.insert(key) {
            q.requested.push(key);
        }
    }
    fn in_flight(&self, key: TileKey) -> bool {
        self.0.borrow().inflight.contains(&key)
    }
    fn drain(&mut self) -> Vec<PreparedTile> {
        std::mem::take(&mut self.0.borrow_mut().ready)
    }
}

/// The renderer bound to one canvas and one tile set.
#[wasm_bindgen]
pub struct WebViewer {
    surface: wgpu::Surface<'static>,
    gpu: Gpu,
    config: wgpu::SurfaceConfiguration,
    depth: wgpu::TextureView,
    pick: Offscreen,
    scene: Scene,
    camera: Camera,
    held: Controls,
    queue: Rc<RefCell<Queue>>,
    text_queue: Rc<RefCell<TextQueue>>,
    params: RenderParams,
    opts: ViewOptions,
    span: f32,
    fly_speed: f32,
    dragging: bool,
    last_pointer: Option<(f32, f32)>,
}

#[wasm_bindgen]
impl WebViewer {
    /// Open WebGPU on `canvas` for the tile set described by its three index files.
    pub async fn create(
        canvas: web_sys::HtmlCanvasElement,
        manifest_json: String,
        paths: Vec<u8>,
        tiles: Vec<u8>,
    ) -> Result<WebViewer, JsValue> {
        let set = TileSet::from_bytes(&manifest_json, &paths, &tiles).map_err(js_err)?;
        let (w, h) = (canvas.width().max(1), canvas.height().max(1));
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let surface = instance
            .create_surface(wgpu::SurfaceTarget::Canvas(canvas))
            .map_err(js_err)?;
        let gpu = Gpu::request(&instance, Some(&surface))
            .await
            .map_err(js_err)?;
        let config = surface
            .get_default_config(&gpu.adapter, w, h)
            .ok_or_else(|| js_err("this browser's WebGPU cannot present to a canvas"))?;
        surface.configure(&gpu.device, &config);

        let opts = ViewOptions::default();
        let params = RenderParams::new(&set.manifest, &opts);
        let bounds = set.manifest.bounds;
        let span = ((bounds.max_x - bounds.min_x).max(bounds.max_y - bounds.min_y)) as f32;
        let queue = Rc::new(RefCell::new(Queue::default()));
        let text_queue = Rc::new(RefCell::new(TextQueue::default()));
        let has_text = set.manifest.has_text;
        let mut scene = Scene::with_provider(
            &gpu,
            Arc::new(set),
            config.format,
            &params,
            Box::new(JsProvider(Rc::clone(&queue))),
        );
        if has_text {
            scene.set_text_provider(Box::new(JsTextProvider(Rc::clone(&text_queue))));
        }
        Ok(WebViewer {
            depth: depth_view(&gpu.device, w, h),
            pick: Offscreen::new(&gpu.device, w, h),
            camera: Camera::framing(bounds),
            surface,
            gpu,
            config,
            scene,
            held: Controls::default(),
            queue,
            text_queue,
            params,
            opts,
            span,
            fly_speed: 1.0,
            dragging: false,
            last_pointer: None,
        })
    }

    /// Advance the camera by `dt` seconds, stream, and draw one frame to the canvas.
    pub fn frame(&mut self, dt: f32) {
        self.camera
            .step(&self.held, dt.clamp(0.0, 0.1), self.span, self.fly_speed);
        let (w, h) = (self.config.width, self.config.height);
        let aspect = w as f32 / h as f32;
        self.scene.update(&self.gpu, &self.camera, aspect, h as f32);
        self.scene.set_camera(&self.gpu, &self.camera, aspect);

        let texture = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.gpu.device, &self.config);
                return;
            }
            _ => return,
        };
        let view = texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });
        self.scene.encode(&mut encoder, &view, &self.depth, false);
        self.gpu.queue.submit(Some(encoder.finish()));
        self.gpu.queue.present(texture);
    }

    /// Tile addresses to fetch, flattened as `[z, x, y, z, x, y, ...]`. Each is returned once.
    pub fn take_requests(&mut self) -> Vec<u32> {
        let mut q = self.queue.borrow_mut();
        q.requested
            .drain(..)
            .flat_map(|k| [u32::from(k.z), k.x, k.y])
            .collect()
    }

    /// Hand over a tile prepared by a worker ([`prepare_tile`]). Uploaded on the next frame.
    pub fn deliver(
        &mut self,
        z: u8,
        x: u32,
        y: u32,
        vertices: &[u8],
        features: &[u8],
        roofs: &[u8],
    ) {
        let key = TileKey::new(z, x, y);
        let mut q = self.queue.borrow_mut();
        q.inflight.remove(&key);
        q.ready.push(PreparedTile {
            key,
            // Copy into correctly aligned storage; bytes from JS carry no alignment guarantee.
            vertices: bytemuck::pod_collect_to_vec::<u8, Vertex>(vertices),
            features: bytemuck::pod_collect_to_vec::<u8, FeatureRaw>(features),
            roofs: bytemuck::pod_collect_to_vec::<u8, Roof>(roofs),
        });
    }

    /// Files whose source to fetch, flattened as `[id, x, y, w, h, z, first_line, ...]` — the file
    /// id, the roof rectangle to lay its text out on, and the first line of the slice to build.
    /// Each file is returned once.
    pub fn take_text_requests(&mut self) -> Vec<f64> {
        let mut q = self.text_queue.borrow_mut();
        q.requested
            .drain(..)
            .flat_map(|p| {
                [
                    f64::from(p.file_id),
                    f64::from(p.rect[0]),
                    f64::from(p.rect[1]),
                    f64::from(p.rect[2]),
                    f64::from(p.rect[3]),
                    f64::from(p.z),
                    f64::from(p.first_line),
                ]
            })
            .collect()
    }

    /// Hand over text laid out by a worker ([`prepare_text`]). Uploaded on the next frame.
    #[allow(clippy::too_many_arguments)]
    pub fn deliver_text(
        &mut self,
        file_id: u32,
        instances: &[u8],
        glyph_count: u32,
        strip_count: u32,
        first_line: u32,
        line_world: f32,
    ) {
        let mut q = self.text_queue.borrow_mut();
        q.inflight.remove(&file_id);
        q.ready.push(PreparedText {
            file_id,
            instances: bytemuck::pod_collect_to_vec::<u8, GlyphInstance>(instances),
            glyph_count,
            strip_count,
            first_line,
            line_world,
        });
    }

    /// A text fetch failed, usually because the file has no `.ftx` (binary, oversize, or a
    /// language with no grammar). Forget it so the scene can ask again later.
    pub fn fail_text(&mut self, file_id: u32) {
        self.text_queue.borrow_mut().inflight.remove(&file_id);
    }

    /// A fetch failed; forget the request so the scene asks again later.
    pub fn fail(&mut self, z: u8, x: u32, y: u32) {
        self.queue
            .borrow_mut()
            .inflight
            .remove(&TileKey::new(z, x, y));
    }

    /// The canvas backing store changed size (CSS size times devicePixelRatio).
    pub fn resize(&mut self, width: u32, height: u32) {
        let (w, h) = (width.max(1), height.max(1));
        self.config.width = w;
        self.config.height = h;
        self.surface.configure(&self.gpu.device, &self.config);
        self.depth = depth_view(&self.gpu.device, w, h);
        self.pick = Offscreen::new(&self.gpu.device, w, h);
    }

    pub fn pointer_down(&mut self, x: f32, y: f32) {
        self.dragging = true;
        self.last_pointer = Some((x, y));
    }

    pub fn pointer_up(&mut self) {
        self.dragging = false;
        self.last_pointer = None;
    }

    pub fn pointer_move(&mut self, x: f32, y: f32) {
        if !self.dragging {
            return;
        }
        if let Some((lx, ly)) = self.last_pointer {
            self.camera.look(x - lx, y - ly);
        }
        self.last_pointer = Some((x, y));
    }

    /// Wheel notches (positive = zoom in / speed up).
    pub fn wheel(&mut self, notches: f32) {
        self.camera.scroll(notches, self.span, &mut self.fly_speed);
    }

    /// A `KeyboardEvent.code` went down or up. Returns true if the viewer used it.
    pub fn key(&mut self, code: &str, down: bool) -> bool {
        match code {
            "KeyW" | "ArrowUp" => self.held.forward = down,
            "KeyS" | "ArrowDown" => self.held.back = down,
            "KeyA" | "ArrowLeft" => self.held.left = down,
            "KeyD" | "ArrowRight" => self.held.right = down,
            "KeyE" => self.held.up = down,
            "KeyQ" => self.held.down = down,
            "Tab" => {
                if down {
                    self.camera.toggle_mode();
                }
            }
            _ => return false,
        }
        true
    }

    /// "map" or "fly".
    pub fn mode(&self) -> String {
        match self.camera.mode {
            CameraMode::Map => "map".into(),
            CameraMode::Fly => "fly".into(),
        }
    }

    /// Resolve the feature id under canvas pixel (x, y); 0 is background. Renders the id pass
    /// now and resolves when the GPU hands the pixel back.
    pub fn pick(&self, x: u32, y: u32) -> js_sys::Promise {
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("pick"),
            });
        self.scene.encode(
            &mut encoder,
            &self.pick.pick_view,
            &self.pick.depth_view,
            true,
        );
        self.gpu.queue.submit(Some(encoder.finish()));
        let buffer = self.pick.copy_id(&self.gpu, x, y);
        js_sys::Promise::new(&mut |resolve, reject| {
            let mapped = buffer.clone();
            buffer.map_async(wgpu::MapMode::Read, .., move |result| {
                if result.is_err() {
                    let _ =
                        reject.call1(&JsValue::NULL, &JsValue::from_str("pick readback failed"));
                    return;
                }
                let id = mapped
                    .get_mapped_range(..)
                    .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                    .unwrap_or(0);
                mapped.unmap();
                let _ = resolve.call1(&JsValue::NULL, &JsValue::from(id));
            });
        })
    }

    /// "path · N lines" for a picked file id, or undefined.
    pub fn describe(&self, id: u32) -> Option<String> {
        self.scene
            .describe(id)
            .map(|(path, lines)| format!("{path} · {lines} lines"))
    }

    /// What the workers need to prepare tiles, as JSON:
    /// `{ bounds, palette, heightScale, colorLayer, heightLayer }`.
    pub fn worker_config(&self) -> String {
        let b = self.scene.tiles.manifest.bounds;
        let palette: Vec<f32> = self.params.palette.iter().flatten().copied().collect();
        format!(
            "{{\"bounds\":[{},{},{},{}],\"palette\":{:?},\"heightScale\":{},\"colorLayer\":{:?},\"heightLayer\":{:?}}}",
            b.min_x,
            b.min_y,
            b.max_x,
            b.max_y,
            palette,
            self.params.height_scale,
            self.opts.color_layer,
            self.opts.height_layer
        )
    }

    pub fn drawn_tiles(&self) -> usize {
        self.scene.drawn_tiles()
    }

    pub fn resident_tiles(&self) -> usize {
        self.scene.resident_tiles()
    }

    /// Files drawing source this frame, how many of those are at the glyph tier, and the bytes
    /// their instance buffers hold.
    pub fn text_stats(&self) -> Vec<f64> {
        let (files, glyphs, bytes) = self.scene.text_stats();
        vec![files as f64, glyphs as f64, bytes as f64]
    }
}

/// Decode one tile and its two layer tiles and extrude it into GPU-ready bytes. Called from a
/// web worker. Returns `[vertices, features]` as two `Uint8Array`s the worker can transfer.
#[allow(clippy::too_many_arguments)]
#[wasm_bindgen]
pub fn prepare_tile(
    z: u8,
    x: u32,
    y: u32,
    fly: &[u8],
    height: &[u8],
    color: &[u8],
    bounds: &[f64],
    palette: &[f32],
    height_scale: f32,
) -> Result<js_sys::Array, JsValue> {
    let [min_x, min_y, max_x, max_y] =
        <[f64; 4]>::try_from(bounds).map_err(|_| js_err("bounds must have four numbers"))?;
    let loaded = decode_tile(TileKey::new(z, x, y), fly, height, color, "height", "color")
        .map_err(js_err)?;
    let palette: Vec<[f32; 3]> = palette
        .chunks_exact(3)
        .map(|c| [c[0], c[1], c[2]])
        .collect();
    let prepared = mesh::build(
        &loaded,
        Bounds {
            min_x,
            min_y,
            max_x,
            max_y,
        },
        &palette,
        height_scale,
    );
    let vertices = js_sys::Uint8Array::from(bytemuck::cast_slice::<Vertex, u8>(&prepared.vertices));
    let features =
        js_sys::Uint8Array::from(bytemuck::cast_slice::<FeatureRaw, u8>(&prepared.features));
    let roofs = js_sys::Uint8Array::from(bytemuck::cast_slice::<Roof, u8>(&prepared.roofs));
    Ok(js_sys::Array::of3(&vertices, &features, &roofs))
}

/// Decode one file's `.ftx` and lay the slice starting at `first_line` out on `rect` at height
/// `z`. Called from a web worker.
/// Returns `[instances, [glyph_count, strip_count, first_line, line_world]]`.
#[wasm_bindgen]
pub fn prepare_text(
    file_id: u32,
    ftx: &[u8],
    rect: &[f32],
    z: f32,
    first_line: u32,
) -> Result<js_sys::Array, JsValue> {
    let rect = <[f32; 4]>::try_from(rect).map_err(|_| js_err("rect must have four numbers"))?;
    let tile = decode_text(file_id, ftx).map_err(js_err)?;
    let metrics = flyover_render::font::Atlas::bundled().metrics;
    let prepared = text::layout(
        &metrics,
        &tile,
        Placement {
            file_id,
            rect,
            z,
            first_line,
        },
        &text::Palette::default(),
    );
    let instances = js_sys::Uint8Array::from(bytemuck::cast_slice::<GlyphInstance, u8>(
        &prepared.instances,
    ));
    let meta = js_sys::Float64Array::from(
        &[
            f64::from(prepared.glyph_count),
            f64::from(prepared.strip_count),
            f64::from(prepared.first_line),
            f64::from(prepared.line_world),
        ][..],
    );
    Ok(js_sys::Array::of2(&instances, &meta))
}
