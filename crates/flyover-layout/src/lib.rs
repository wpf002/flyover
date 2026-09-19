//! Layout and tiling (M2, rectangular).
//!
//! Reads an `index.db`, builds the directory tree (weight = lines per file, minimum 1), lays it
//! out as a squarified treemap in a square whose area equals the total weight, then cuts a
//! quadtree tile pyramid over it. Each node emerges at the zoom where its cell is large enough;
//! coarser zooms show ancestor slabs, so every zoom is a clean partition of the world. Writes a
//! tile set: `manifest.json`, `tiles/{z}/{x}/{y}.fly`, `layers/{language,lines}/…`,
//! `index/paths.bin`, and `index/tiles.bin` (every tile address, for readers that can't list).
//!
//! Output is deterministic: the tree is path-sorted, ids are assigned in that order, the treemap
//! and tiler are pure f64, and no wall-clock time enters the output (`generated_at` is supplied by
//! the caller).
//!
//! TODO(M6): replace the rectangle with a weighted Voronoi treemap clipped to a generated or
//! uploaded silhouette, seeded from a hash of each node's path.

pub mod palette;
pub mod treemap;

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use flyover_tiles::layer::{LayerTile, LayerValues};
use flyover_tiles::paths::{PathEntry, PathsIndex};
use flyover_tiles::text::{decode_spans, TextTile};
use flyover_tiles::tile::{Feature, FeatureKind, Tile};
use flyover_tiles::{
    layer_tile_key, tile_key, Bounds, Category, LayerDescriptor, LayerKind, Manifest, Range,
    RepoInfo, Shape, ShapeSource, Stats, FORMAT_VERSION,
};
use rusqlite::OptionalExtension;
use treemap::{squarify, Rect};

/// Target features per tile. Drives the zoom at which a cell emerges, which keeps per-tile counts
/// far under the 50K feature budget.
const DENSITY: f64 = 512.0;
/// Hard cap on quadtree depth, a safety net for pathological inputs.
const MAX_ZOOM: u8 = 24;
/// Deterministic placeholder when the caller supplies no timestamp (keeps output byte-stable).
pub const DEFAULT_GENERATED_AT: &str = "1970-01-01T00:00:00Z";

#[derive(Debug, Clone)]
pub struct Options {
    pub repo_name: String,
    pub repo_source: String,
    pub commit_sha: String,
    pub generated_at: String,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            repo_name: "repo".into(),
            repo_source: String::new(),
            commit_sha: String::new(),
            generated_at: DEFAULT_GENERATED_AT.into(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Summary {
    pub files: u64,
    pub directories: u64,
    pub tiles: u64,
    pub text_tiles: u64,
    pub max_zoom: u8,
}

#[derive(Debug, thiserror::Error)]
pub enum LayoutError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io writing {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("encoding a tile: {0}")]
    Tile(#[from] flyover_tiles::tile::TileError),
    #[error("encoding a text tile: {0}")]
    Text(#[from] flyover_tiles::text::TextError),
    #[error("writing the manifest: {0}")]
    Manifest(#[from] flyover_tiles::ManifestError),
}

struct FileRow {
    id: u32,
    path: String,
    language: String,
    bytes: u64,
    lines: u64,
}

struct Node {
    id: u32,
    kind: FeatureKind,
    path: String,
    depth: u8,
    parent_id: u32,
    children: Vec<usize>,
    lines: u64,
    bytes: u64,
    weight: f64,
    language: String,
    rect: Rect,
    appear_z: u8,
    split_z: u8,
}

/// Lay out `index_db` and write a tile set into `out_dir`.
pub fn run(index_db: &Path, out_dir: &Path, opts: &Options) -> Result<Summary, LayoutError> {
    let files = read_files(index_db)?;
    let (arena, root) = build_tree(&files);

    let world = world_bounds(&arena, root);
    let mut arena = arena;
    if !arena[root].children.is_empty() {
        layout_node(&mut arena, root, world);
        assign_zooms(&mut arena, root);
    }
    let max_zoom = arena
        .iter()
        .map(|n| n.appear_z)
        .max()
        .unwrap_or(0)
        .min(MAX_ZOOM);

    let categories = category_list(&arena);
    let cat_index: HashMap<&str, u16> = categories
        .iter()
        .enumerate()
        .map(|(i, c)| (c.as_str(), i as u16))
        .collect();

    let keys = write_tiles(&arena, root, out_dir, &world, max_zoom, &cat_index)?;
    write_bytes(
        out_dir,
        flyover_tiles::keys::KEYS_PATH,
        &flyover_tiles::keys::encode(&keys),
    )?;
    let tile_count = keys.len() as u64;
    write_paths(&files, out_dir)?;
    let text_tiles = write_text(index_db, out_dir)?;
    let stats = stats(&arena, &files);
    write_manifest(
        out_dir,
        opts,
        &world,
        max_zoom,
        &categories,
        &files,
        stats,
        text_tiles > 0,
    )?;

    Ok(Summary {
        files: stats.files,
        directories: stats.directories,
        tiles: tile_count,
        text_tiles,
        max_zoom,
    })
}

fn read_files(index_db: &Path) -> Result<Vec<FileRow>, LayoutError> {
    let conn = rusqlite::Connection::open(index_db)?;
    let mut stmt =
        conn.prepare("SELECT id, path, language, bytes, lines FROM files ORDER BY path")?;
    let rows = stmt.query_map([], |r| {
        Ok(FileRow {
            id: r.get::<_, i64>(0)? as u32,
            path: r.get(1)?,
            language: r.get(2)?,
            bytes: r.get::<_, i64>(3)? as u64,
            lines: r.get::<_, i64>(4)? as u64,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(LayoutError::from)
}

/// Build the directory tree. Node 0 is a synthetic root (feature id 0, never emitted). Directory
/// feature ids are assigned after the largest file id, in path order, so they are stable.
fn build_tree(files: &[FileRow]) -> (Vec<Node>, usize) {
    let mut arena: Vec<Node> = Vec::new();
    arena.push(Node {
        id: 0,
        kind: FeatureKind::Dir,
        path: String::new(),
        depth: 0,
        parent_id: 0,
        children: Vec::new(),
        lines: 0,
        bytes: 0,
        weight: 0.0,
        language: "Other".into(),
        rect: Rect {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
        },
        appear_z: 0,
        split_z: 0,
    });
    let mut dir_of: HashMap<String, usize> = HashMap::new();
    dir_of.insert(String::new(), 0);
    let mut next_dir_id = files.iter().map(|f| f.id).max().unwrap_or(0) + 1;

    for f in files {
        let comps: Vec<&str> = f.path.split('/').filter(|s| !s.is_empty()).collect();
        if comps.is_empty() {
            continue;
        }
        // Ensure each ancestor directory exists.
        let mut parent = 0usize;
        let mut acc = String::new();
        for comp in &comps[..comps.len() - 1] {
            if !acc.is_empty() {
                acc.push('/');
            }
            acc.push_str(comp);
            if let Some(&idx) = dir_of.get(&acc) {
                parent = idx;
                continue;
            }
            let depth = arena[parent].depth + 1;
            let parent_id = arena[parent].id;
            let idx = arena.len();
            arena.push(Node {
                id: next_dir_id,
                kind: FeatureKind::Dir,
                path: acc.clone(),
                depth,
                parent_id,
                children: Vec::new(),
                lines: 0,
                bytes: 0,
                weight: 0.0,
                language: "Other".into(),
                rect: Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 0.0,
                    h: 0.0,
                },
                appear_z: 0,
                split_z: 0,
            });
            next_dir_id += 1;
            arena[parent].children.push(idx);
            dir_of.insert(acc.clone(), idx);
            parent = idx;
        }
        let depth = arena[parent].depth + 1;
        let parent_id = arena[parent].id;
        let idx = arena.len();
        arena.push(Node {
            id: f.id,
            kind: FeatureKind::File,
            path: f.path.clone(),
            depth,
            parent_id,
            children: Vec::new(),
            lines: f.lines,
            bytes: f.bytes,
            weight: f.lines.max(1) as f64,
            language: f.language.clone(),
            rect: Rect {
                x: 0.0,
                y: 0.0,
                w: 0.0,
                h: 0.0,
            },
            appear_z: 0,
            split_z: 0,
        });
        arena[parent].children.push(idx);
    }

    aggregate(&mut arena, 0);
    (arena, 0)
}

/// Post-order: sum weights/lines/bytes into directories and pick each directory's dominant
/// language (by line count, ties broken by name). Returns the subtree's language->lines tally.
fn aggregate(arena: &mut [Node], idx: usize) -> HashMap<String, u64> {
    if arena[idx].kind == FeatureKind::File {
        let mut m = HashMap::new();
        m.insert(arena[idx].language.clone(), arena[idx].lines.max(1));
        return m;
    }
    let children = arena[idx].children.clone();
    let mut tally: HashMap<String, u64> = HashMap::new();
    let mut weight = 0.0;
    let mut lines = 0u64;
    let mut bytes = 0u64;
    for c in children {
        let sub = aggregate(arena, c);
        for (lang, n) in sub {
            *tally.entry(lang).or_insert(0) += n;
        }
        weight += arena[c].weight;
        lines += arena[c].lines;
        bytes += arena[c].bytes;
    }
    let dominant = tally
        .iter()
        .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)))
        .map(|(lang, _)| lang.clone())
        .unwrap_or_else(|| "Other".into());
    let node = &mut arena[idx];
    node.weight = weight.max(1.0);
    node.lines = lines;
    node.bytes = bytes;
    node.language = dominant;
    tally
}

fn world_bounds(arena: &[Node], root: usize) -> Rect {
    let side = arena[root].weight.max(1.0).sqrt();
    Rect {
        x: 0.0,
        y: 0.0,
        w: side,
        h: side,
    }
}

/// Squarified treemap, recursing into each directory's cell. Children are laid out largest-first
/// for aspect ratio, but the arena keeps them in path order for deterministic emission.
fn layout_node(arena: &mut Vec<Node>, idx: usize, rect: Rect) {
    arena[idx].rect = rect;
    let children = arena[idx].children.clone();
    if children.is_empty() {
        return;
    }
    let mut order: Vec<usize> = children.clone();
    order.sort_by(|&a, &b| {
        arena[b]
            .weight
            .partial_cmp(&arena[a].weight)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| arena[a].path.cmp(&arena[b].path))
    });
    let areas: Vec<f64> = order.iter().map(|&c| arena[c].weight).collect();
    let rects = squarify(&areas, rect);
    for (child, r) in order.iter().zip(rects) {
        layout_node(arena, *child, r);
    }
}

/// Assign each node an appear zoom (when it becomes visible) and, for directories, a split zoom
/// (when children replace it). A directory's children all appear at its split zoom, so each zoom
/// level is a gap-free partition of the world.
fn assign_zooms(arena: &mut Vec<Node>, idx: usize) {
    let world_area = arena[idx].rect.area().max(1.0);
    // Root: appears at 0, splits at 0.
    arena[idx].appear_z = 0;
    arena[idx].split_z = 0;
    assign_recurse(arena, idx, world_area);
}

fn assign_recurse(arena: &mut Vec<Node>, idx: usize, world_area: f64) {
    let split = arena[idx].split_z;
    let children = arena[idx].children.clone();
    for c in &children {
        arena[*c].appear_z = split;
    }
    for c in children {
        if arena[c].kind == FeatureKind::Dir && !arena[c].children.is_empty() {
            let grandkids = arena[c].children.clone();
            let min_natural = grandkids
                .iter()
                .map(|&g| natural_z(arena[g].rect.area(), world_area))
                .min()
                .unwrap_or(arena[c].appear_z);
            arena[c].split_z = arena[c].appear_z.max(min_natural).min(MAX_ZOOM);
            assign_recurse(arena, c, world_area);
        } else {
            arena[c].split_z = arena[c].appear_z;
        }
    }
}

/// Smallest zoom at which a cell of `area` is worth drawing on its own.
fn natural_z(area: f64, world_area: f64) -> u8 {
    if area <= 0.0 {
        return MAX_ZOOM;
    }
    let ratio = world_area / (area * DENSITY);
    if ratio <= 1.0 {
        return 0;
    }
    (0.5 * ratio.log2()).ceil().clamp(0.0, MAX_ZOOM as f64) as u8
}

/// The visible cut at zoom `z`: the shallowest nodes whose cells are the right size, forming a
/// partition of the world.
fn collect_visible(arena: &[Node], idx: usize, z: u8, out: &mut Vec<usize>) {
    for &c in &arena[idx].children {
        match arena[c].kind {
            FeatureKind::File => out.push(c),
            FeatureKind::Dir => {
                if z < arena[c].split_z || arena[c].children.is_empty() {
                    out.push(c);
                } else {
                    collect_visible(arena, c, z, out);
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn write_tiles(
    arena: &[Node],
    root: usize,
    out_dir: &Path,
    world: &Rect,
    max_zoom: u8,
    cat_index: &HashMap<&str, u16>,
) -> Result<Vec<(u8, u32, u32)>, LayoutError> {
    let mut keys = Vec::new();
    if arena[root].children.is_empty() {
        return Ok(keys);
    }
    for z in 0..=max_zoom {
        let mut visible = Vec::new();
        collect_visible(arena, root, z, &mut visible);

        let n = 1u64 << z;
        let mut buckets: BTreeMap<(u32, u32), Vec<usize>> = BTreeMap::new();
        for idx in visible {
            let (cx, cy) = arena[idx].rect.centroid();
            let tx = tile_coord(cx - world.x, world.w, n);
            let ty = tile_coord(cy - world.y, world.h, n);
            buckets.entry((tx, ty)).or_default().push(idx);
        }

        let tile_w = world.w / n as f64;
        let tile_h = world.h / n as f64;
        for ((x, y), idxs) in buckets {
            let origin_x = world.x + x as f64 * tile_w;
            let origin_y = world.y + y as f64 * tile_h;
            let mut features = Vec::with_capacity(idxs.len());
            let mut lines = Vec::with_capacity(idxs.len());
            let mut langs = Vec::with_capacity(idxs.len());
            for idx in &idxs {
                let node = &arena[*idx];
                features.push(feature_geometry(node, origin_x, origin_y));
                lines.push(node.lines as f32);
                langs.push(*cat_index.get(node.language.as_str()).unwrap_or(&0));
            }
            let tile = Tile { z, x, y, features };
            write_bytes(out_dir, &tile_key(z, x, y), &tile.encode()?)?;
            write_bytes(
                out_dir,
                &layer_tile_key("lines", z, x, y),
                &LayerTile {
                    z,
                    x,
                    y,
                    values: LayerValues::Scalar(lines),
                }
                .encode(),
            )?;
            write_bytes(
                out_dir,
                &layer_tile_key("language", z, x, y),
                &LayerTile {
                    z,
                    x,
                    y,
                    values: LayerValues::Category(langs),
                }
                .encode(),
            )?;
            keys.push((z, x, y));
        }
    }
    Ok(keys)
}

fn feature_geometry(node: &Node, origin_x: f64, origin_y: f64) -> Feature {
    let r = node.rect;
    let x0 = (r.x - origin_x) as f32;
    let y0 = (r.y - origin_y) as f32;
    let x1 = (r.x + r.w - origin_x) as f32;
    let y1 = (r.y + r.h - origin_y) as f32;
    Feature {
        id: node.id,
        kind: node.kind,
        depth: node.depth,
        parent_id: node.parent_id,
        lines: node.lines.min(u32::MAX as u64) as u32,
        vertices: vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]],
        indices: vec![0, 1, 2, 0, 2, 3],
    }
}

fn tile_coord(offset: f64, span: f64, n: u64) -> u32 {
    if span <= 0.0 {
        return 0;
    }
    let t = (offset / span * n as f64).floor();
    t.clamp(0.0, (n - 1) as f64) as u32
}

fn category_list(arena: &[Node]) -> Vec<String> {
    let mut set: BTreeMap<String, ()> = BTreeMap::new();
    for node in arena.iter().skip(1) {
        set.insert(node.language.clone(), ());
    }
    set.into_keys().collect()
}

fn stats(arena: &[Node], files: &[FileRow]) -> Stats {
    let directories = arena
        .iter()
        .skip(1)
        .filter(|n| n.kind == FeatureKind::Dir)
        .count() as u64;
    let languages = files
        .iter()
        .map(|f| f.language.as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .len() as u32;
    Stats {
        files: files.len() as u64,
        directories,
        lines: files.iter().map(|f| f.lines).sum(),
        bytes: files.iter().map(|f| f.bytes).sum(),
        languages,
    }
}

/// Write one `.ftx` per parsed file, streaming rows so a large repo's text never sits in memory
/// all at once. The index stores the source zstd-compressed; the tile recompresses it with its
/// token spans.
fn write_text(index_db: &Path, out_dir: &Path) -> Result<u64, LayoutError> {
    let conn = rusqlite::Connection::open(index_db)?;
    // An index without a `text` table is valid: the tile set simply has no text (hasText: false).
    let has_table = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'text'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !has_table {
        return Ok(0);
    }
    let mut stmt = conn.prepare("SELECT file_id, content, tokens FROM text ORDER BY file_id")?;
    let mut rows = stmt.query([])?;
    let mut count = 0u64;
    while let Some(row) = rows.next()? {
        let file_id: i64 = row.get(0)?;
        let content: Vec<u8> = row.get(1)?;
        let packed: Vec<u8> = row.get(2)?;
        let Ok(text) = zstd::stream::decode_all(&content[..]) else {
            continue;
        };
        let Ok(text) = String::from_utf8(text) else {
            continue;
        };
        let tile = TextTile {
            file_id: file_id as u32,
            text,
            spans: decode_spans(&packed),
        };
        write_bytes(
            out_dir,
            &flyover_tiles::text::text_key(file_id as u32),
            &tile.encode()?,
        )?;
        count += 1;
    }
    Ok(count)
}

fn write_paths(files: &[FileRow], out_dir: &Path) -> Result<(), LayoutError> {
    let index = PathsIndex {
        entries: files
            .iter()
            .map(|f| PathEntry {
                id: f.id,
                lines: f.lines.min(u32::MAX as u64) as u32,
                bytes: f.bytes.min(u32::MAX as u64) as u32,
                path: f.path.clone(),
            })
            .collect(),
    };
    write_bytes(out_dir, "index/paths.bin", &index.encode())
}

#[allow(clippy::too_many_arguments)]
fn write_manifest(
    out_dir: &Path,
    opts: &Options,
    world: &Rect,
    max_zoom: u8,
    categories: &[String],
    files: &[FileRow],
    stats: Stats,
    has_text: bool,
) -> Result<(), LayoutError> {
    let (min_lines, max_lines) = files
        .iter()
        .map(|f| f.lines)
        .fold((u64::MAX, 0u64), |(lo, hi), n| (lo.min(n), hi.max(n)));
    let (min_lines, max_lines) = if files.is_empty() {
        (0, 0)
    } else {
        (min_lines, max_lines)
    };

    let manifest = Manifest {
        format_version: FORMAT_VERSION,
        repo: RepoInfo {
            name: opts.repo_name.clone(),
            source: opts.repo_source.clone(),
            commit_sha: opts.commit_sha.clone(),
        },
        generated_at: opts.generated_at.clone(),
        bounds: Bounds {
            min_x: world.x,
            min_y: world.y,
            max_x: world.x + world.w,
            max_y: world.y + world.h,
        },
        max_zoom,
        shape: Shape {
            source: ShapeSource::Rectangle,
            seed: None,
        },
        stats,
        layers: vec![
            LayerDescriptor {
                key: "language".into(),
                label: "Language".into(),
                kind: LayerKind::Categorical,
                unit: None,
                categories: Some(
                    categories
                        .iter()
                        .map(|name| Category {
                            label: name.clone(),
                            color: palette::color_for(name),
                        })
                        .collect(),
                ),
                range: None,
            },
            LayerDescriptor {
                key: "lines".into(),
                label: "Lines".into(),
                kind: LayerKind::Scalar,
                unit: Some("lines".into()),
                categories: None,
                range: Some(Range {
                    min: min_lines as f64,
                    max: max_lines as f64,
                }),
            },
        ],
        has_edges: false,
        has_text,
    };
    write_bytes(out_dir, "manifest.json", manifest.to_json()?.as_bytes())
}

fn write_bytes(out_dir: &Path, rel: &str, bytes: &[u8]) -> Result<(), LayoutError> {
    let path = out_dir.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| LayoutError::Io {
            path: parent.display().to_string(),
            source,
        })?;
    }
    std::fs::write(&path, bytes).map_err(|source| LayoutError::Io {
        path: path.display().to_string(),
        source,
    })
}
