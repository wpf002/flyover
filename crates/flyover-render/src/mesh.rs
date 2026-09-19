//! Build a GPU-ready mesh from a decoded tile: extrude each footprint into a prism (roof + walls)
//! and pack per-feature color, height, and id. Runs on worker threads (off the render thread), so
//! only the cheap buffer upload happens on the render thread.

use bytemuck::{Pod, Zeroable};

use crate::lod::tile_rect;
use crate::tileset::{LoadedTile, TileKey};
use flyover_tiles::tile::FeatureKind;
use flyover_tiles::Bounds;

/// One prism vertex. `feature_index` selects per-feature data in the storage buffer; `height_factor`
/// is 0 at the base and 1 at the roof so the shader extrudes without new geometry.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct Vertex {
    pub world: [f32; 2],
    pub height_factor: f32,
    pub shade: f32,
    pub feature_index: u32,
    pub _pad: u32,
}

/// Per-feature data indexed by `feature_index`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct FeatureRaw {
    pub color: [f32; 4],
    pub height: f32,
    pub id: u32,
    pub _pad: [u32; 2],
}

/// A tile's mesh, ready to upload.
pub struct PreparedTile {
    pub key: TileKey,
    pub vertices: Vec<Vertex>,
    pub features: Vec<FeatureRaw>,
    /// Roof rectangles of the file features in this tile, so the scene knows where source text
    /// goes without touching the geometry again. A file's rectangle is the same at every zoom.
    pub roofs: Vec<Roof>,
}

/// Where one file's source text can be drawn: the inset roof it sits on. `repr(C)` and `Pod` so
/// the browser build can transfer a tile's roofs from a worker as plain bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct Roof {
    pub file_id: u32,
    /// World-space `[x, y, w, h]`, lower-left origin.
    pub rect: [f32; 4],
    pub z: f32,
}

const ROOF_SHADE: f32 = 1.0;
/// Walls are lit by a fixed directional light in the ground plane; this is the unlit floor.
const WALL_AMBIENT: f32 = 0.42;
const WALL_DIFFUSE: f32 = 0.42;
/// Light direction in XY (from the default camera's side), normalized below.
const LIGHT: [f32; 2] = [-0.35, -0.94];
/// Footprints are drawn at this fraction of their size, leaving "streets" between cells so
/// neighbors of the same color stay distinct. Visual only: tiles keep the exact cells.
const INSET: f32 = 0.9;

/// Extrude a decoded tile. `palette` maps a color-layer category index to an rgb triple;
/// `height_scale` turns a height-layer value into world height via `scale * log2(value + 1)`.
pub fn build(
    loaded: &LoadedTile,
    bounds: Bounds,
    palette: &[[f32; 3]],
    height_scale: f32,
) -> PreparedTile {
    let (ox, oy, _, _) = tile_rect(bounds, loaded.key);
    let mut vertices = Vec::new();
    let mut features = Vec::with_capacity(loaded.tile.features.len());
    let mut roofs = Vec::new();

    for (fi, feature) in loaded.tile.features.iter().enumerate() {
        let raw_h = loaded.height.get(fi).copied().unwrap_or(0.0).max(0.0);
        let height = height_scale * (raw_h + 1.0).log2();
        let cat = loaded.color.get(fi).copied().unwrap_or(0) as usize;
        let color = palette.get(cat).copied().unwrap_or([0.5, 0.5, 0.5]);
        // Directories read a touch dimmer so file blocks stand out.
        let dim = if feature.kind == FeatureKind::Dir {
            0.8
        } else {
            1.0
        };
        features.push(FeatureRaw {
            color: [color[0] * dim, color[1] * dim, color[2] * dim, 1.0],
            height,
            id: feature.id,
            _pad: [0, 0],
        });

        let fidx = fi as u32;
        // World-space footprint vertices (tile-local + tile origin), inset toward the centroid.
        let world: Vec<[f32; 2]> = feature
            .vertices
            .iter()
            .map(|[x, y]| [x + ox, y + oy])
            .collect();
        let n = world.len().max(1) as f32;
        let (cx, cy) = world
            .iter()
            .fold((0.0, 0.0), |(sx, sy), [x, y]| (sx + x, sy + y));
        let (cx, cy) = (cx / n, cy / n);
        let ring: Vec<[f32; 2]> = world
            .iter()
            .map(|[x, y]| [cx + (x - cx) * INSET, cy + (y - cy) * INSET])
            .collect();

        if feature.kind == FeatureKind::File {
            let (mut x0, mut y0) = (f32::MAX, f32::MAX);
            let (mut x1, mut y1) = (f32::MIN, f32::MIN);
            for [x, y] in &ring {
                x0 = x0.min(*x);
                y0 = y0.min(*y);
                x1 = x1.max(*x);
                y1 = y1.max(*y);
            }
            if x1 > x0 && y1 > y0 {
                roofs.push(Roof {
                    file_id: feature.id,
                    rect: [x0, y0, x1 - x0, y1 - y0],
                    z: height,
                });
            }
        }

        // Roof: reuse the pre-triangulated footprint at height_factor 1.
        for &i in &feature.indices {
            let p = ring[i as usize];
            vertices.push(Vertex {
                world: p,
                height_factor: 1.0,
                shade: ROOF_SHADE,
                feature_index: fidx,
                _pad: 0,
            });
        }

        // Walls: one quad per ring edge, base (0) to roof (1), shaded by how much the wall faces
        // the light. Rings are counter-clockwise, so (dy, -dx) is the outward normal.
        let light_len = (LIGHT[0] * LIGHT[0] + LIGHT[1] * LIGHT[1]).sqrt();
        let count = ring.len();
        for i in 0..count {
            let a = ring[i];
            let b = ring[(i + 1) % count];
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let len = (dx * dx + dy * dy).sqrt().max(f32::EPSILON);
            let facing = (dy * LIGHT[0] - dx * LIGHT[1]) / (len * light_len);
            let wall_shade = WALL_AMBIENT + WALL_DIFFUSE * facing.max(0.0);
            let quad = [(a, 0.0), (b, 0.0), (b, 1.0), (a, 0.0), (b, 1.0), (a, 1.0)];
            for (p, hf) in quad {
                vertices.push(Vertex {
                    world: p,
                    height_factor: hf,
                    shade: wall_shade,
                    feature_index: fidx,
                    _pad: 0,
                });
            }
        }
    }

    PreparedTile {
        key: loaded.key,
        vertices,
        features,
        roofs,
    }
}

/// Parse `#rrggbb` into an rgb triple in 0..1. Unknown input falls back to mid-grey.
pub fn parse_color(hex: &str) -> [f32; 3] {
    let s = hex.trim_start_matches('#');
    if s.len() == 6 {
        if let (Ok(r), Ok(g), Ok(b)) = (
            u8::from_str_radix(&s[0..2], 16),
            u8::from_str_radix(&s[2..4], 16),
            u8::from_str_radix(&s[4..6], 16),
        ) {
            return [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0];
        }
    }
    [0.5, 0.5, 0.5]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_parses() {
        assert_eq!(parse_color("#ffffff"), [1.0, 1.0, 1.0]);
        assert_eq!(parse_color("#000000"), [0.0, 0.0, 0.0]);
        let g = parse_color("nope");
        assert_eq!(g, [0.5, 0.5, 0.5]);
    }
}
