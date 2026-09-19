//! SPEC M3: no tile decode on the render thread, asserted in debug builds. This file is its own
//! test binary (own process), because the render-thread marker is process-global.

mod common;

use flyover_render::tileset::TileSet;

#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "tile decode on the render thread")]
fn decoding_on_the_render_thread_panics_in_debug() {
    let dir = common::tileset("assert");
    let tiles = TileSet::open(&dir).unwrap();
    flyover_render::mark_render_thread();
    let key = *tiles.keys.iter().next().expect("tile set has tiles");
    let _ = tiles.load(key, "language", "lines");
}
