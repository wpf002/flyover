//! End-to-end indexer tests: golden row counts on the committed polyglot fixture, determinism,
//! the built-in exclusion list, and the security rules from docs/SPEC.md section 6 (symlink
//! escape, oversize, binary, parse-time budget).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use flyover_index::index::{build, run, ExcludedEntry, FileRecord, Options};
use rusqlite::Connection;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A fresh, empty scratch directory unique across parallel tests.
fn scratch(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("flyover-m1-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn write(path: &Path, contents: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn file<'a>(data: &'a flyover_index::IndexData, path: &str) -> Option<&'a FileRecord> {
    data.files.iter().find(|f| f.path == path)
}

fn excluded_reason<'a>(data: &'a flyover_index::IndexData, path: &str) -> Option<&'a str> {
    data.excluded
        .iter()
        .find(|e: &&ExcludedEntry| e.path == path)
        .map(|e| e.reason.as_str())
}

// --- golden counts ------------------------------------------------------------------------

#[test]
fn polyglot_fixture_has_golden_counts() {
    let out = scratch("golden");
    run(&fixture("polyglot"), &out, &Options::default()).unwrap();
    let conn = Connection::open(out.join("index.db")).unwrap();

    let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
    assert_eq!(count("SELECT count(*) FROM files"), 12, "files");
    assert_eq!(count("SELECT count(*) FROM symbols"), 49, "symbols");
    assert_eq!(count("SELECT count(*) FROM imports"), 22, "imports");
    assert_eq!(count("SELECT count(*) FROM excluded"), 0, "excluded");
    assert_eq!(
        count("SELECT count(*) FROM edges"),
        0,
        "edges empty until M7"
    );
    assert_eq!(
        count("SELECT count(*) FROM files WHERE parsed = 1"),
        12,
        "all parsed"
    );

    // Per-file symbol/import counts, verified by hand against each fixture source.
    let per_file: Vec<(String, i64, i64)> = {
        let mut stmt = conn
            .prepare(
                "SELECT f.path,
                    (SELECT count(*) FROM symbols s WHERE s.file_id = f.id),
                    (SELECT count(*) FROM imports i WHERE i.file_id = f.id)
                 FROM files f ORDER BY f.path",
            )
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    let expected = [
        ("c/util.c", 3, 2),
        ("cpp/util.cpp", 4, 2),
        ("csharp/App.cs", 5, 2),
        ("go/main.go", 3, 2),
        ("java/App.java", 5, 2),
        ("js/app.js", 3, 2),
        ("php/app.php", 5, 2),
        ("py/mod.py", 3, 2),
        ("ruby/app.rb", 4, 2),
        ("rust/lib.rs", 6, 2),
        ("ts/app.ts", 6, 1),
        ("tsx/view.tsx", 2, 1),
    ];
    assert_eq!(per_file.len(), expected.len());
    for (got, want) in per_file.iter().zip(expected.iter()) {
        assert_eq!(
            (got.0.as_str(), got.1, got.2),
            *want,
            "counts for {}",
            got.0
        );
    }

    // Spot-check a specific symbol and a specific import string.
    let origin_kind: String = conn
        .query_row("SELECT kind FROM symbols WHERE name = 'origin'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(origin_kind, "function");
    let zod: i64 = conn
        .query_row(
            "SELECT count(*) FROM imports WHERE module = 'zod'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(zod, 1);

    // Meta is deterministic (no timestamps or paths).
    let schema_version: String = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(schema_version, "1");

    std::fs::remove_dir_all(&out).ok();
}

// --- determinism --------------------------------------------------------------------------

#[test]
fn two_runs_produce_identical_bytes() {
    let a = scratch("det-a");
    let b = scratch("det-b");
    run(&fixture("polyglot"), &a, &Options::default()).unwrap();
    run(&fixture("polyglot"), &b, &Options::default()).unwrap();

    let bytes_a = std::fs::read(a.join("index.db")).unwrap();
    let bytes_b = std::fs::read(b.join("index.db")).unwrap();
    assert_eq!(
        bytes_a, bytes_b,
        "index.db must be byte-identical across runs"
    );

    std::fs::remove_dir_all(&a).ok();
    std::fs::remove_dir_all(&b).ok();
}

// --- exclusions ---------------------------------------------------------------------------

fn exclusion_tree(tag: &str) -> PathBuf {
    let root = scratch(tag);
    write(&root.join("keep.rs"), b"pub fn keep() {}\n");
    write(&root.join("src/app.ts"), b"export const x = 1;\n");
    write(&root.join("package-lock.json"), b"{}\n");
    write(&root.join("bundle.min.js"), b"var a=1;\n");
    write(
        &root.join("gen.rs"),
        b"// @generated by tool\npub fn g() {}\n",
    );
    write(
        &root.join("node_modules/dep/x.js"),
        b"module.exports = 1;\n",
    );
    write(&root.join("vendor/y.rb"), b"puts 1\n");
    write(&root.join("dist/z.ts"), b"export const z = 2;\n");
    write(&root.join("third_party/w.c"), b"int w() { return 0; }\n");
    root
}

#[test]
fn built_in_exclusions_are_recorded_not_dropped() {
    let root = exclusion_tree("excl");
    let data = build(&root, &Options::default()).unwrap();

    // Only the two real source files are indexed.
    let mut paths: Vec<_> = data.files.iter().map(|f| f.path.as_str()).collect();
    paths.sort_unstable();
    assert_eq!(paths, vec!["keep.rs", "src/app.ts"]);

    // File-level exclusions carry their reason.
    assert_eq!(
        excluded_reason(&data, "package-lock.json"),
        Some("lockfile")
    );
    assert_eq!(excluded_reason(&data, "bundle.min.js"), Some("minified"));
    assert_eq!(excluded_reason(&data, "gen.rs"), Some("generated"));

    // Vendored directories are pruned and recorded once each, by directory name.
    assert_eq!(excluded_reason(&data, "node_modules"), Some("node_modules"));
    assert_eq!(excluded_reason(&data, "vendor"), Some("vendor"));
    assert_eq!(excluded_reason(&data, "dist"), Some("dist"));
    assert_eq!(excluded_reason(&data, "third_party"), Some("third_party"));
    // The pruned directory's contents are not walked.
    assert!(file(&data, "node_modules/dep/x.js").is_none());

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn include_vendored_turns_exclusions_off() {
    let root = exclusion_tree("incl");
    let data = build(
        &root,
        &Options {
            include_vendored: true,
            parse_timeout: None,
        },
    )
    .unwrap();

    assert!(
        data.excluded.is_empty(),
        "nothing excluded with --include-vendored"
    );
    assert!(file(&data, "node_modules/dep/x.js").is_some());
    assert!(file(&data, "package-lock.json").is_some());
    assert!(file(&data, "gen.rs").is_some());
    assert_eq!(data.files.len(), 9);

    std::fs::remove_dir_all(&root).ok();
}

// --- security: symlinks ------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn symlink_to_outside_file_is_not_followed() {
    use std::os::unix::fs::symlink;

    let outside = scratch("secret-store");
    let secret = outside.join("secret.txt");
    write(&secret, b"TOPSECRET-do-not-read\n");

    let root = scratch("symlink-file");
    write(&root.join("ok.rs"), b"pub fn ok() {}\n");
    symlink(&secret, root.join("leak.txt")).unwrap();

    let data = build(&root, &Options::default()).unwrap();

    // Only the real file is indexed; the symlink and its target are never read.
    let paths: Vec<_> = data.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, vec!["ok.rs"]);
    assert!(file(&data, "leak.txt").is_none());

    std::fs::remove_dir_all(&root).ok();
    std::fs::remove_dir_all(&outside).ok();
}

#[cfg(unix)]
#[test]
fn symlinked_directory_escape_is_not_descended() {
    use std::os::unix::fs::symlink;

    let outside = scratch("secret-dir");
    write(&outside.join("hidden.rs"), b"pub fn hidden() {}\n");

    let root = scratch("symlink-dir");
    write(&root.join("real.rs"), b"pub fn real() {}\n");
    symlink(&outside, root.join("escape")).unwrap();

    let data = build(&root, &Options::default()).unwrap();

    let paths: Vec<_> = data.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths,
        vec!["real.rs"],
        "must not descend into a symlinked dir"
    );
    assert!(!data.files.iter().any(|f| f.path.contains("hidden")));

    std::fs::remove_dir_all(&root).ok();
    std::fs::remove_dir_all(&outside).ok();
}

// --- security: oversize, binary, parse budget --------------------------------------------

#[test]
fn oversize_file_is_recorded_but_not_parsed() {
    let root = scratch("oversize");
    // Just over the 2 MB cap.
    let big = vec![b'a'; (2 * 1024 * 1024 + 16) as usize];
    write(&root.join("big.rs"), &big);
    write(&root.join("small.rs"), b"pub fn small() {}\n");

    let data = build(&root, &Options::default()).unwrap();

    let big = file(&data, "big.rs").expect("oversize file is recorded");
    assert!(big.is_oversize);
    assert!(!big.parsed);
    assert_eq!(big.lines, 0, "oversize files are not read line by line");
    assert!(
        !big.hash.is_empty(),
        "oversize files still get a content hash"
    );
    assert!(big.symbols.is_empty());

    let small = file(&data, "small.rs").unwrap();
    assert!(!small.is_oversize);
    assert!(small.parsed);

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn binary_file_is_recorded_but_not_parsed() {
    let root = scratch("binary");
    write(&root.join("blob.dat"), &[0u8, 1, 2, 3, 4, 0, 9]);
    write(&root.join("code.rs"), b"pub fn c() {}\n");

    let data = build(&root, &Options::default()).unwrap();

    let blob = file(&data, "blob.dat").expect("binary file is recorded");
    assert!(blob.is_binary);
    assert!(!blob.parsed);
    assert!(blob.symbols.is_empty());
    assert!(file(&data, "code.rs").unwrap().parsed);

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn parse_budget_abandons_long_parses_but_keeps_the_file() {
    let root = scratch("budget");
    // A file big enough that any real parse exceeds a 1 ns budget.
    let src = "pub fn f() {}\n".repeat(20_000);
    write(&root.join("many.rs"), src.as_bytes());

    let data = build(
        &root,
        &Options {
            include_vendored: false,
            parse_timeout: Some(Duration::from_nanos(1)),
        },
    )
    .unwrap();

    let f = file(&data, "many.rs").expect("file is still recorded");
    assert!(!f.parsed, "parse was abandoned by the time budget");
    assert!(f.symbols.is_empty());
    assert!(f.lines > 0, "file-level data is kept");

    std::fs::remove_dir_all(&root).ok();
}
