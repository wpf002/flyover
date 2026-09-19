//! Off-thread tile loading. Worker threads pull the highest-priority requested tile, read and
//! zstd-decode it, and build its prism mesh, then send the prepared result back over a channel.
//! The render thread only enqueues requests and drains results, so neither decode nor mesh build
//! happens on it (asserted in debug builds inside `TileSet::load`).

use std::collections::{BinaryHeap, HashSet};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use crate::mesh::{self, PreparedTile};
use crate::text::{self, PreparedText, TextRequest};
use crate::tileset::{TileKey, TileSet, TileSetError};
use flyover_tiles::Bounds;

struct Job {
    priority: i64,
    key: TileKey,
}

impl PartialEq for Job {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority && self.key == other.key
    }
}
impl Eq for Job {}
impl Ord for Job {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.priority
            .cmp(&other.priority)
            .then_with(|| self.key.cmp(&other.key))
    }
}
impl PartialOrd for Job {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

struct Queue {
    heap: BinaryHeap<Job>,
    shutdown: bool,
}

struct Loaded {
    key: TileKey,
    result: Result<PreparedTile, TileSetError>,
}

/// Parameters the workers need to turn a decoded tile into a mesh.
#[derive(Clone)]
struct BuildParams {
    bounds: Bounds,
    palette: Arc<Vec<[f32; 3]>>,
    height_scale: f32,
    color_layer: String,
    height_layer: String,
}

/// A pool of worker threads decoding and meshing tiles for one tile set.
pub struct TileLoader {
    queue: Arc<(Mutex<Queue>, Condvar)>,
    results: Receiver<Loaded>,
    inflight: HashSet<TileKey>,
    workers: Vec<JoinHandle<()>>,
}

impl TileLoader {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tiles: Arc<TileSet>,
        color_layer: String,
        height_layer: String,
        bounds: Bounds,
        palette: Vec<[f32; 3]>,
        height_scale: f32,
        threads: usize,
    ) -> Self {
        let queue = Arc::new((
            Mutex::new(Queue {
                heap: BinaryHeap::new(),
                shutdown: false,
            }),
            Condvar::new(),
        ));
        let (tx, rx) = channel();
        let params = BuildParams {
            bounds,
            palette: Arc::new(palette),
            height_scale,
            color_layer,
            height_layer,
        };
        let workers = (0..threads.max(1))
            .map(|_| {
                spawn_worker(
                    Arc::clone(&tiles),
                    queue.clone(),
                    tx.clone(),
                    params.clone(),
                )
            })
            .collect();
        TileLoader {
            queue,
            results: rx,
            inflight: HashSet::new(),
            workers,
        }
    }

    /// Enqueue a tile to load if it is not already in flight. Higher `priority` loads sooner.
    pub fn request(&mut self, key: TileKey, priority: i64) {
        if !self.inflight.insert(key) {
            return;
        }
        let (lock, cvar) = &*self.queue;
        lock.lock().unwrap().heap.push(Job { priority, key });
        cvar.notify_one();
    }

    pub fn in_flight(&self, key: TileKey) -> bool {
        self.inflight.contains(&key)
    }

    /// Drain finished loads. Errors are dropped after clearing the key so the tile can be retried.
    pub fn drain(&mut self) -> Vec<PreparedTile> {
        let mut out = Vec::new();
        while let Ok(loaded) = self.results.try_recv() {
            self.inflight.remove(&loaded.key);
            if let Ok(prepared) = loaded.result {
                out.push(prepared);
            }
        }
        out
    }
}

/// The same pattern for `.ftx` text: workers read a file's source, lay it out on the roof
/// rectangle carried by the request, and return the instance buffer. Decode and layout, the two
/// expensive parts, stay off the render thread.
pub struct TextLoader {
    queue: Arc<(Mutex<TextQueue>, Condvar)>,
    results: Receiver<LoadedText>,
    inflight: HashSet<u32>,
    workers: Vec<JoinHandle<()>>,
}

struct TextQueue {
    heap: BinaryHeap<TextJob>,
    shutdown: bool,
}

struct TextJob {
    priority: i64,
    request: TextRequest,
}

impl PartialEq for TextJob {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority
            && self.request.placement.file_id == other.request.placement.file_id
    }
}
impl Eq for TextJob {}
impl Ord for TextJob {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.priority.cmp(&other.priority).then_with(|| {
            other
                .request
                .placement
                .file_id
                .cmp(&self.request.placement.file_id)
        })
    }
}
impl PartialOrd for TextJob {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

struct LoadedText {
    file_id: u32,
    result: Result<PreparedText, TileSetError>,
}

impl TextLoader {
    pub fn new(tiles: Arc<TileSet>, palette: text::Palette, threads: usize) -> Self {
        let queue = Arc::new((
            Mutex::new(TextQueue {
                heap: BinaryHeap::new(),
                shutdown: false,
            }),
            Condvar::new(),
        ));
        let (tx, rx) = channel();
        let workers = (0..threads.max(1))
            .map(|_| spawn_text_worker(Arc::clone(&tiles), queue.clone(), tx.clone(), palette))
            .collect();
        TextLoader {
            queue,
            results: rx,
            inflight: HashSet::new(),
            workers,
        }
    }

    pub fn request(&mut self, request: TextRequest) {
        if !self.inflight.insert(request.placement.file_id) {
            return;
        }
        let (lock, cvar) = &*self.queue;
        lock.lock().unwrap().heap.push(TextJob {
            priority: request.priority,
            request,
        });
        cvar.notify_one();
    }

    pub fn in_flight(&self, file_id: u32) -> bool {
        self.inflight.contains(&file_id)
    }

    /// Drain finished layouts. A file whose `.ftx` is missing (an unparsed or binary file has
    /// none) simply never appears; the key is cleared so it is not retried every frame.
    pub fn drain(&mut self) -> Vec<PreparedText> {
        let mut out = Vec::new();
        while let Ok(loaded) = self.results.try_recv() {
            self.inflight.remove(&loaded.file_id);
            if let Ok(prepared) = loaded.result {
                out.push(prepared);
            }
        }
        out
    }
}

impl Drop for TextLoader {
    fn drop(&mut self) {
        {
            let (lock, cvar) = &*self.queue;
            lock.lock().unwrap().shutdown = true;
            cvar.notify_all();
        }
        for w in self.workers.drain(..) {
            let _ = w.join();
        }
    }
}

fn spawn_text_worker(
    tiles: Arc<TileSet>,
    queue: Arc<(Mutex<TextQueue>, Condvar)>,
    tx: Sender<LoadedText>,
    palette: text::Palette,
) -> JoinHandle<()> {
    let metrics = crate::font::Atlas::bundled().metrics;
    std::thread::spawn(move || loop {
        let request = {
            let (lock, cvar) = &*queue;
            let mut q = lock.lock().unwrap();
            loop {
                if q.shutdown {
                    return;
                }
                if let Some(job) = q.heap.pop() {
                    break job.request;
                }
                q = cvar.wait(q).unwrap();
            }
        };
        let file_id = request.placement.file_id;
        let result = tiles
            .load_text(file_id)
            .map(|tile| text::layout(&metrics, &tile, request.placement, &palette));
        if tx.send(LoadedText { file_id, result }).is_err() {
            return; // loader dropped
        }
    })
}

impl Drop for TileLoader {
    fn drop(&mut self) {
        {
            let (lock, cvar) = &*self.queue;
            lock.lock().unwrap().shutdown = true;
            cvar.notify_all();
        }
        for w in self.workers.drain(..) {
            let _ = w.join();
        }
    }
}

fn spawn_worker(
    tiles: Arc<TileSet>,
    queue: Arc<(Mutex<Queue>, Condvar)>,
    tx: Sender<Loaded>,
    params: BuildParams,
) -> JoinHandle<()> {
    std::thread::spawn(move || loop {
        let key = {
            let (lock, cvar) = &*queue;
            let mut q = lock.lock().unwrap();
            loop {
                if q.shutdown {
                    return;
                }
                if let Some(job) = q.heap.pop() {
                    break job.key;
                }
                q = cvar.wait(q).unwrap();
            }
        };
        let result = tiles
            .load(key, &params.color_layer, &params.height_layer)
            .map(|loaded| {
                mesh::build(&loaded, params.bounds, &params.palette, params.height_scale)
            });
        if tx.send(Loaded { key, result }).is_err() {
            return; // loader dropped
        }
    })
}
