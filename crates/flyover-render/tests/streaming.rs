//! Off-thread streaming: worker threads decode and mesh every tile while the test thread is
//! marked as the render thread (so any decode on it would panic in debug builds).

mod common;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use flyover_render::cache::TileLoader;
use flyover_render::tileset::TileSet;

/// Each rectangular footprint extrudes to 6 roof vertices + 4 walls x 6 vertices.
const VERTICES_PER_CELL: usize = 6 + 4 * 6;

#[test]
fn workers_decode_and_mesh_every_tile_off_the_render_thread() {
    let dir = common::tileset("stream");
    let tiles = Arc::new(TileSet::open(&dir).unwrap());
    flyover_render::mark_render_thread();

    let palette = vec![[0.5, 0.5, 0.5]; 16];
    let mut loader = TileLoader::new(
        Arc::clone(&tiles),
        "language".into(),
        "lines".into(),
        tiles.manifest.bounds,
        palette,
        1.0,
        4,
    );
    let keys: Vec<_> = tiles.keys.iter().copied().collect();
    assert!(!keys.is_empty());
    for k in &keys {
        loader.request(*k, i64::from(k.z));
    }

    let mut got = HashMap::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    while got.len() < keys.len() && Instant::now() < deadline {
        for prepared in loader.drain() {
            got.insert(prepared.key, prepared);
        }
        std::thread::sleep(Duration::from_millis(1));
    }

    assert_eq!(got.len(), keys.len(), "every requested tile was prepared");
    for prepared in got.values() {
        assert!(!prepared.features.is_empty());
        assert_eq!(
            prepared.vertices.len(),
            prepared.features.len() * VERTICES_PER_CELL
        );
    }
}
