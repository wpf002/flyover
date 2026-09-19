//! M2 acceptance tests: round-trip, every file in exactly one leaf tile, cell areas sum to world
//! area, no tile over budget, and identical output across two runs.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use flyover_layout::{run, Options};
use flyover_tiles::layer::LayerTile;
use flyover_tiles::tile::{FeatureKind, Tile};
use flyover_tiles::Manifest;

static COUNTER: AtomicU64 = AtomicU64::new(0);
const FEATURE_BUDGET: usize = 50_000;
const BYTE_BUDGET: usize = 256 * 1024;

fn scratch(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("flyover-m2-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Build a minimal index.db (just the columns layout reads) with the given files.
fn make_index(dir: &Path, files: &[(u32, &str, &str, u64, u64)]) -> PathBuf {
    let db = dir.join("index.db");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE files (id INTEGER PRIMARY KEY, path TEXT, language TEXT, bytes INTEGER, lines INTEGER);",
    )
    .unwrap();
    for (id, path, lang, bytes, lines) in files {
        conn.execute(
            "INSERT INTO files (id, path, language, bytes, lines) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![*id as i64, path, lang, *bytes as i64, *lines as i64],
        )
        .unwrap();
    }
    db
}

/// A spread of files across nested directories, big enough to force several zoom levels.
fn sample_files() -> Vec<(u32, String, String, u64, u64)> {
    let langs = ["Rust", "TypeScript", "Python", "Go", "C"];
    let mut files = Vec::new();
    let mut id = 1u32;
    for d in 0..8 {
        for f in 0..12 {
            let lang = langs[(d + f) % langs.len()];
            let lines = 5 + ((d * 37 + f * 13) % 400) as u64;
            files.push((
                id,
                format!("src/mod{d}/file{f}.rs"),
                lang.to_string(),
                lines * 20,
                lines,
            ));
            id += 1;
        }
    }
    // a couple of top-level files too
    files.push((id, "README.md".into(), "Markdown".into(), 400, 20));
    files.push((id + 1, "build.rs".into(), "Rust".into(), 100, 8));
    files
}

fn as_refs(files: &[(u32, String, String, u64, u64)]) -> Vec<(u32, &str, &str, u64, u64)> {
    files
        .iter()
        .map(|(id, p, l, b, n)| (*id, p.as_str(), l.as_str(), *b, *n))
        .collect()
}

fn manifest(dir: &Path) -> Manifest {
    Manifest::from_json(&std::fs::read_to_string(dir.join("manifest.json")).unwrap()).unwrap()
}

/// Every geometry tile in the set, decoded, with its (z, x, y).
fn all_tiles(dir: &Path) -> Vec<Tile> {
    let mut out = Vec::new();
    let tiles_root = dir.join("tiles");
    let mut stack = vec![tiles_root];
    while let Some(p) = stack.pop() {
        if p.is_dir() {
            let mut entries: Vec<_> = std::fs::read_dir(&p)
                .unwrap()
                .map(|e| e.unwrap().path())
                .collect();
            entries.sort();
            stack.extend(entries);
        } else if p.extension().and_then(|e| e.to_str()) == Some("fly") {
            out.push(Tile::decode(&std::fs::read(&p).unwrap()).unwrap());
        }
    }
    out
}

fn list_files(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(p) = stack.pop() {
        if p.is_dir() {
            for e in std::fs::read_dir(&p).unwrap() {
                stack.push(e.unwrap().path());
            }
        } else {
            out.push(
                p.strip_prefix(dir)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
    out.sort();
    out
}

#[test]
fn every_file_appears_in_exactly_one_leaf_tile() {
    let dir = scratch("leaf");
    let files = sample_files();
    let db = make_index(&dir, &as_refs(&files));
    let out = dir.join("tiles-out");
    let summary = run(&db, &out, &Options::default()).unwrap();

    let m = manifest(&out);
    let mut seen: Vec<u32> = Vec::new();
    for tile in all_tiles(&out) {
        if tile.z != m.max_zoom {
            continue;
        }
        for f in tile.features {
            if f.kind == FeatureKind::File {
                seen.push(f.id);
            }
        }
    }
    let unique: BTreeSet<u32> = seen.iter().copied().collect();
    assert_eq!(
        seen.len(),
        unique.len(),
        "a file appeared in more than one leaf tile"
    );
    assert_eq!(
        unique.len() as u64,
        summary.files,
        "not every file reached the leaf zoom"
    );
}

#[test]
fn cell_areas_sum_to_world_area() {
    let dir = scratch("area");
    let files = sample_files();
    let db = make_index(&dir, &as_refs(&files));
    let out = dir.join("tiles-out");
    run(&db, &out, &Options::default()).unwrap();

    let m = manifest(&out);
    let world = (m.bounds.max_x - m.bounds.min_x) * (m.bounds.max_y - m.bounds.min_y);

    let mut sum = 0.0f64;
    for tile in all_tiles(&out) {
        if tile.z != m.max_zoom {
            continue;
        }
        for f in tile.features.iter().filter(|f| f.kind == FeatureKind::File) {
            // axis-aligned rect: vertices are [x0,y0],[x1,y0],[x1,y1],[x0,y1]
            let w = (f.vertices[1][0] - f.vertices[0][0]) as f64;
            let h = (f.vertices[2][1] - f.vertices[1][1]) as f64;
            sum += w * h;
        }
    }
    let err = (sum - world).abs() / world;
    assert!(err < 0.001, "area error {err} (sum {sum}, world {world})");
}

#[test]
fn no_tile_exceeds_budget() {
    let dir = scratch("budget");
    let files = sample_files();
    let db = make_index(&dir, &as_refs(&files));
    let out = dir.join("tiles-out");
    run(&db, &out, &Options::default()).unwrap();

    let tiles_root = out.join("tiles");
    let mut stack = vec![tiles_root];
    while let Some(p) = stack.pop() {
        if p.is_dir() {
            for e in std::fs::read_dir(&p).unwrap() {
                stack.push(e.unwrap().path());
            }
        } else if p.extension().and_then(|e| e.to_str()) == Some("fly") {
            let bytes = std::fs::read(&p).unwrap();
            assert!(
                bytes.len() <= BYTE_BUDGET,
                "{} over byte budget: {}",
                p.display(),
                bytes.len()
            );
            let tile = Tile::decode(&bytes).unwrap();
            assert!(
                tile.features.len() <= FEATURE_BUDGET,
                "{} over feature budget",
                p.display()
            );
        }
    }
}

#[test]
fn layer_tiles_align_with_geometry() {
    let dir = scratch("layers");
    let files = sample_files();
    let db = make_index(&dir, &as_refs(&files));
    let out = dir.join("tiles-out");
    run(&db, &out, &Options::default()).unwrap();

    for tile in all_tiles(&out) {
        let (z, x, y) = (tile.z, tile.x, tile.y);
        for key in ["language", "lines"] {
            let path = out.join(format!("layers/{key}/{z}/{x}/{y}.flv"));
            let lt = LayerTile::decode(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(
                lt.values.len(),
                tile.features.len(),
                "{key} tile {z}/{x}/{y} misaligned"
            );
        }
    }

    let m = manifest(&out);
    assert!(m
        .layers
        .iter()
        .any(|l| l.key == "language" && l.categories.is_some()));
    assert!(m
        .layers
        .iter()
        .any(|l| l.key == "lines" && l.range.is_some()));
}

#[test]
fn two_runs_are_byte_identical() {
    let dir = scratch("det");
    let files = sample_files();
    let db = make_index(&dir, &as_refs(&files));
    let a = dir.join("a");
    let b = dir.join("b");
    run(&db, &a, &Options::default()).unwrap();
    run(&db, &b, &Options::default()).unwrap();

    let files_a = list_files(&a);
    let files_b = list_files(&b);
    assert_eq!(files_a, files_b, "different file sets across runs");
    for rel in files_a {
        let ba = std::fs::read(a.join(&rel)).unwrap();
        let bb = std::fs::read(b.join(&rel)).unwrap();
        assert_eq!(ba, bb, "{rel} differs across runs");
    }
}
