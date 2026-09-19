//! Level-of-detail selection: walk the quadtree, frustum-cull, and pick the zoom per region by
//! screen-space error. Pure geometry over the in-memory tile-key set, so it never touches disk.

use glam::{Mat4, Vec3, Vec4};

use crate::camera::Camera;
use crate::tileset::{TileKey, TileSet};
use flyover_tiles::Bounds;

/// A tile is subdivided when its projected size exceeds this fraction of the viewport height.
const SPLIT_FRACTION: f32 = 0.6;

/// World-space rectangle covered by a tile.
pub fn tile_rect(bounds: Bounds, key: TileKey) -> (f32, f32, f32, f32) {
    let n = (1u32 << key.z) as f32;
    let tw = (bounds.max_x - bounds.min_x) as f32 / n;
    let th = (bounds.max_y - bounds.min_y) as f32 / n;
    let x0 = bounds.min_x as f32 + key.x as f32 * tw;
    let y0 = bounds.min_y as f32 + key.y as f32 * th;
    (x0, y0, tw, th)
}

/// Choose the tiles to draw this frame, most-detailed within view.
pub fn select(
    tiles: &TileSet,
    camera: &Camera,
    aspect: f32,
    viewport_h: f32,
    max_height: f32,
) -> Vec<TileKey> {
    let frustum = Frustum::from(camera.view_proj(aspect));
    let eye = camera.eye();
    let px_per_world = viewport_h / (2.0 * (camera.fovy * 0.5).tan());
    let mut out = Vec::new();
    let mut roots = tiles.roots();
    roots.sort();
    for root in roots {
        walk(
            tiles,
            root,
            &frustum,
            eye,
            px_per_world,
            viewport_h,
            max_height,
            &mut out,
        );
    }
    if out.is_empty() {
        // Never render nothing: fall back to whatever roots exist.
        out = tiles.roots();
        out.sort();
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn walk(
    tiles: &TileSet,
    key: TileKey,
    frustum: &Frustum,
    eye: Vec3,
    px_per_world: f32,
    viewport_h: f32,
    max_height: f32,
    out: &mut Vec<TileKey>,
) {
    if !tiles.has(key) {
        return;
    }
    let (x0, y0, tw, th) = tile_rect(tiles.manifest.bounds, key);
    let min = Vec3::new(x0, y0, 0.0);
    let max = Vec3::new(x0 + tw, y0 + th, max_height);
    if !frustum.aabb_visible(min, max) {
        return;
    }

    let center = Vec3::new(x0 + tw * 0.5, y0 + th * 0.5, 0.0);
    let dist = (center - eye).length().max(0.001);
    let projected_px = (tw.max(th) / dist) * px_per_world;

    let has_children =
        key.z < tiles.manifest.max_zoom && key.children().iter().any(|c| tiles.has(*c));

    if projected_px <= viewport_h * SPLIT_FRACTION || !has_children {
        out.push(key);
    } else {
        for child in key.children() {
            walk(
                tiles,
                child,
                frustum,
                eye,
                px_per_world,
                viewport_h,
                max_height,
                out,
            );
        }
    }
}

/// Six frustum planes extracted from a view-projection matrix (Gribb–Hartmann, 0..1 clip z).
struct Frustum {
    planes: [Vec4; 6],
}

impl Frustum {
    fn from(m: Mat4) -> Self {
        let r0 = m.row(0);
        let r1 = m.row(1);
        let r2 = m.row(2);
        let r3 = m.row(3);
        let planes = [
            r3 + r0, // left
            r3 - r0, // right
            r3 + r1, // bottom
            r3 - r1, // top
            r2,      // near (z >= 0)
            r3 - r2, // far
        ]
        .map(normalize_plane);
        Frustum { planes }
    }

    /// True unless the box is entirely outside one plane.
    fn aabb_visible(&self, min: Vec3, max: Vec3) -> bool {
        for p in &self.planes {
            // The AABB corner most in the direction of the plane normal.
            let positive = Vec3::new(
                if p.x >= 0.0 { max.x } else { min.x },
                if p.y >= 0.0 { max.y } else { min.y },
                if p.z >= 0.0 { max.z } else { min.z },
            );
            if p.x * positive.x + p.y * positive.y + p.z * positive.z + p.w < 0.0 {
                return false;
            }
        }
        true
    }
}

fn normalize_plane(p: Vec4) -> Vec4 {
    let n = Vec3::new(p.x, p.y, p.z).length();
    if n > 0.0 {
        p / n
    } else {
        p
    }
}
