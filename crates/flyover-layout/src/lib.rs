//! Layout and tiling. Nothing is implemented yet.
//!
//! TODO(M2): squarified treemap over the directory tree, weights = lines per file, inside a
//! rectangle. Then the quadtree tiler: cut the map into `tiles/{z}/{x}/{y}.fly` with a feature
//! and byte budget per tile, pre-triangulated so the renderer does no geometry work.
//! Needs: the SQLite index from flyover-index (M1) and the binary tile encoding in flyover-tiles.
//!
//! TODO(M6): swap the rectangle for a weighted Voronoi treemap clipped to an arbitrary polygon.
//! Shape comes from an uploaded SVG path or a silhouette generated from the repo's identity.
//! Seed every re-layout from the previous one so files don't move between commits.
//!
//! Rule for this crate: layout is deterministic. Seed every PRNG from a hash of the path.
