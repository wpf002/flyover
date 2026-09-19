//! Renderer. Nothing is implemented yet, and wgpu isn't a dependency until M3.
//!
//! TODO(M3): wgpu + winit native viewer. Load a tile set from disk, fly camera, quadtree
//! frustum culling, LOD selection by screen-space error, LRU tile cache under a GPU memory
//! budget, picking through an id buffer. Colored extruded blocks only, no text.
//! Needs: tile sets from flyover-layout (M2).
//!
//! TODO(M4): source text. MSDF glyph atlas, instanced glyph quads for files whose projected
//! line height is readable, per-line token strips below that.
//!
//! TODO(M5): wasm32 target on WebGPU, tile fetch over HTTP, decode in workers.
//!
//! Rule for this crate: no tile decode, file IO, or allocation spikes on the render thread.
//! The frame budget is 8.3 ms.
