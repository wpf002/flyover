//! Shared test helper: build a small real tile set (index.db -> flyover layout) in a temp dir.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A fresh tile set with ~100 files across nested directories, laid out by flyover-layout.
pub fn tileset(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("flyover-m3-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let db = dir.join("index.db");
    make_index(&db);
    let out = dir.join("tiles");
    flyover_layout::run(&db, &out, &flyover_layout::Options::default()).unwrap();
    out
}

fn make_index(db: &Path) {
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.execute_batch(
        "CREATE TABLE files (id INTEGER PRIMARY KEY, path TEXT, language TEXT, bytes INTEGER, lines INTEGER);",
    )
    .unwrap();
    let langs = ["Rust", "TypeScript", "Python", "Go", "C"];
    let mut id = 1i64;
    for d in 0..8 {
        for f in 0..12 {
            let lines = 5 + ((d * 37 + f * 13) % 400) as i64;
            conn.execute(
                "INSERT INTO files (id, path, language, bytes, lines) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    id,
                    format!("src/mod{d}/file{f}.rs"),
                    langs[(d + f) % langs.len()],
                    lines * 20,
                    lines
                ],
            )
            .unwrap();
            id += 1;
        }
    }
}
