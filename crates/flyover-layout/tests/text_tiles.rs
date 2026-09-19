//! M4: text tiles. Index a small repo for real, lay it out, and check each `.ftx` still holds the
//! exact source bytes with token spans inside them.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use flyover_layout::{run, Options};
use flyover_tiles::text::{text_key, TextTile};
use flyover_tiles::Manifest;

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn scratch(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("flyover-m4-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

#[test]
fn text_tiles_hold_the_source_byte_for_byte() {
    let dir = scratch("text");
    let repo = dir.join("repo");
    let sources = [
        (
            "src/main.rs",
            "// entry\nfn main() {\n    println!(\"hi\");\n}\n",
        ),
        ("src/lib.rs", "pub struct Point {\n    pub x: i32,\n}\n"),
        (
            "app.py",
            "import os\n\n\ndef go():\n    return os.getcwd()\n",
        ),
    ];
    for (path, body) in sources {
        write(&repo.join(path), body);
    }

    let index = dir.join("index");
    flyover_index::run(&repo, &index, &flyover_index::Options::default()).unwrap();
    let out = dir.join("tiles");
    let summary = run(&index.join("index.db"), &out, &Options::default()).unwrap();
    assert_eq!(summary.text_tiles, sources.len() as u64);

    // The manifest advertises text, so a renderer knows to fetch .ftx tiles.
    let manifest =
        Manifest::from_json(&std::fs::read_to_string(out.join("manifest.json")).unwrap()).unwrap();
    assert!(manifest.has_text);

    // paths.bin maps file ids to paths; each id's text tile must match that file's bytes.
    let paths = flyover_tiles::paths::PathsIndex::decode(
        &std::fs::read(out.join("index/paths.bin")).unwrap(),
    )
    .unwrap();
    assert_eq!(paths.entries.len(), sources.len());

    for entry in &paths.entries {
        let tile_path = out.join(text_key(entry.id));
        let tile = TextTile::decode(&std::fs::read(&tile_path).unwrap()).unwrap();
        assert_eq!(tile.file_id, entry.id);
        let on_disk = std::fs::read_to_string(repo.join(&entry.path)).unwrap();
        assert_eq!(tile.text, on_disk, "text differs for {}", entry.path);
        assert!(!tile.spans.is_empty(), "no spans for {}", entry.path);
        for span in &tile.spans {
            assert!(
                (span.start + span.len) as usize <= tile.text.len(),
                "span past end of {}",
                entry.path
            );
        }
    }

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn two_runs_write_identical_text_tiles() {
    let dir = scratch("det");
    let repo = dir.join("repo");
    write(&repo.join("a.rs"), "fn a() -> u8 {\n    7\n}\n");
    let index = dir.join("index");
    flyover_index::run(&repo, &index, &flyover_index::Options::default()).unwrap();

    let a = dir.join("a");
    let b = dir.join("b");
    run(&index.join("index.db"), &a, &Options::default()).unwrap();
    run(&index.join("index.db"), &b, &Options::default()).unwrap();

    let key = text_key(1);
    assert_eq!(
        std::fs::read(a.join(&key)).unwrap(),
        std::fs::read(b.join(&key)).unwrap()
    );
    std::fs::remove_dir_all(&dir).ok();
}
