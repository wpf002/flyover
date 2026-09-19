//! The indexer: walk a repo, read each file, extract tier-1 symbols and imports, and assemble a
//! deterministic [`IndexData`] the [`crate::db`] writer turns into `index.db`.
//!
//! Safety rules enforced here (docs/SPEC.md section 6):
//! - never follow symlinks (`follow_links(false)`), so no file entry can escape the root;
//! - only paths under the root are read (checked by `strip_prefix`);
//! - files over [`crate::MAX_TEXT_BYTES`] or with a NUL in the first 8 KB are recorded, not parsed;
//! - a per-file parse-time budget abandons a long parse, keeping file-level data.
//!
//! Determinism: the walk is sorted by path, ids are assigned in that order, and parsing runs in
//! parallel but never reorders the results.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ignore::WalkBuilder;
use rayon::prelude::*;

use crate::grammars::{self, Import, Registry, Symbol};
use crate::{count_lines, exclude, is_binary, language, MAX_TEXT_BYTES, SNIFF_BYTES};

/// Compression level for stored source text; it is decompressed again at layout time.
const TEXT_ZSTD_LEVEL: i32 = 9;

/// One row of the `files` table plus the symbols and imports found in it.
pub struct FileRecord {
    pub path: String,
    pub language: String,
    pub bytes: u64,
    pub lines: u64,
    pub hash: String,
    pub is_binary: bool,
    pub is_oversize: bool,
    pub parsed: bool,
    pub symbols: Vec<Symbol>,
    pub imports: Vec<Import>,
    /// zstd-compressed UTF-8 source, kept only for parsed files so text tiles can be written
    /// later from index.db alone. Held compressed so large repos stay affordable in memory.
    pub text: Option<Vec<u8>>,
    /// Packed token spans (see [`crate::tokens`]).
    pub tokens: Vec<u8>,
}

/// One row of the `excluded` table.
pub struct ExcludedEntry {
    pub path: String,
    pub reason: String,
}

/// Everything the db writer needs, already in deterministic (path) order.
pub struct IndexData {
    pub files: Vec<FileRecord>,
    pub excluded: Vec<ExcludedEntry>,
}

/// Knobs for one index run.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Include vendored and generated code that would otherwise be excluded.
    pub include_vendored: bool,
    /// Per-file parse-time cap. `None` (the default) keeps output identical across machines;
    /// a value guards against pathological files but can make output machine-dependent for them.
    pub parse_timeout: Option<Duration>,
}

#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("{0} is not a directory")]
    NotADirectory(PathBuf),
    #[error("resolving {path}: {source}")]
    Canonicalize {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("building tree-sitter grammars: {0}")]
    Grammar(#[from] grammars::BuildError),
    #[error(transparent)]
    Db(#[from] crate::db::DbError),
}

enum Outcome {
    File(Box<FileRecord>),
    Excluded(ExcludedEntry),
    Skip,
}

/// Walk `root` and build the index in memory. Does not touch disk output.
pub fn build(root: &Path, options: &Options) -> Result<IndexData, IndexError> {
    if !root.is_dir() {
        return Err(IndexError::NotADirectory(root.to_path_buf()));
    }
    let root = std::fs::canonicalize(root).map_err(|source| IndexError::Canonicalize {
        path: root.to_path_buf(),
        source,
    })?;

    let registry = grammars::registry()?;

    // Directories pruned by the built-in exclusion list, recorded once each. An Arc<Mutex<_>>
    // because ignore's filter closure must be Send + Sync + 'static.
    let excluded_dirs: Arc<Mutex<BTreeSet<(String, String)>>> =
        Arc::new(Mutex::new(BTreeSet::new()));

    let paths = collect_paths(&root, options.include_vendored, &excluded_dirs);

    // Parse in parallel; collect() preserves the sorted input order, so ids stay deterministic.
    let outcomes: Vec<Outcome> = paths
        .par_iter()
        .map(|path| process(path, &root, &registry, options))
        .collect();

    let mut files = Vec::new();
    let mut excluded: Vec<ExcludedEntry> = Vec::new();
    for outcome in outcomes {
        match outcome {
            Outcome::File(record) => files.push(*record),
            Outcome::Excluded(entry) => excluded.push(entry),
            Outcome::Skip => {}
        }
    }

    // Merge pruned directories with file-level exclusions and sort by path.
    let dirs = excluded_dirs.lock().expect("exclusion set not poisoned");
    for (path, reason) in dirs.iter() {
        excluded.push(ExcludedEntry {
            path: path.clone(),
            reason: reason.clone(),
        });
    }
    excluded.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(IndexData { files, excluded })
}

/// Build the index and write it to `<out_dir>/index.db`.
pub fn run(root: &Path, out_dir: &Path, options: &Options) -> Result<IndexData, IndexError> {
    let data = build(root, options)?;
    std::fs::create_dir_all(out_dir).map_err(|source| IndexError::Canonicalize {
        path: out_dir.to_path_buf(),
        source,
    })?;
    let db_path = out_dir.join("index.db");
    crate::db::write(
        &db_path,
        &data.files,
        &data.excluded,
        options.include_vendored,
    )?;
    Ok(data)
}

/// Walk the tree once, in sorted order, collecting file paths and recording pruned directories.
fn collect_paths(
    root: &Path,
    include_vendored: bool,
    excluded_dirs: &Arc<Mutex<BTreeSet<(String, String)>>>,
) -> Vec<PathBuf> {
    let root_owned = root.to_path_buf();
    let sink = Arc::clone(excluded_dirs);

    let walker = WalkBuilder::new(root)
        .follow_links(false)
        .hidden(false)
        .parents(false)
        .git_global(false)
        .git_ignore(true)
        .git_exclude(true)
        .require_git(false)
        .sort_by_file_path(|a, b| a.cmp(b))
        .filter_entry(move |entry| {
            // Always skip the .git directory.
            if entry.file_name() == ".git" {
                return false;
            }
            if include_vendored || entry.depth() == 0 {
                return true;
            }
            let is_dir = entry.file_type().is_some_and(|t| t.is_dir());
            if is_dir {
                if let Some(name) = entry.file_name().to_str() {
                    if let Some(reason) = exclude::excluded_dir(name) {
                        if let Ok(rel) = entry.path().strip_prefix(&root_owned) {
                            sink.lock()
                                .expect("exclusion set not poisoned")
                                .insert((to_slash(rel), reason.as_str().to_string()));
                        }
                        return false; // prune the whole subtree
                    }
                }
            }
            true
        })
        .build();

    let mut paths = Vec::new();
    for entry in walker.flatten() {
        if entry.file_type().is_some_and(|t| t.is_file()) {
            paths.push(entry.into_path());
        }
    }
    paths
}

/// Read and classify one file. Any IO error skips the file rather than failing the whole run.
fn process(path: &Path, root: &Path, registry: &Registry, options: &Options) -> Outcome {
    let Ok(rel) = path.strip_prefix(root) else {
        return Outcome::Skip; // never read outside the root
    };
    let rel = to_slash(rel);
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

    if !options.include_vendored {
        if let Some(reason) = exclude::excluded_by_name(file_name) {
            return Outcome::Excluded(ExcludedEntry {
                path: rel,
                reason: reason.as_str().to_string(),
            });
        }
    }

    let Ok(meta) = std::fs::metadata(path) else {
        return Outcome::Skip;
    };
    let size = meta.len();
    let language = language::detect(path).to_string();

    if size > MAX_TEXT_BYTES {
        return match hash_oversize(path) {
            Some((hash, is_binary)) => Outcome::File(Box::new(FileRecord {
                path: rel,
                language,
                bytes: size,
                lines: 0, // oversize files are recorded but not read line by line
                hash,
                is_binary,
                is_oversize: true,
                parsed: false,
                symbols: Vec::new(),
                imports: Vec::new(),
                text: None,
                tokens: Vec::new(),
            })),
            None => Outcome::Skip,
        };
    }

    let mut bytes = Vec::with_capacity(size as usize);
    if File::open(path)
        .and_then(|mut f| f.read_to_end(&mut bytes))
        .is_err()
    {
        return Outcome::Skip;
    }

    let hash = blake3::hash(&bytes).to_hex().to_string();

    if is_binary(&bytes) {
        return Outcome::File(Box::new(FileRecord {
            path: rel,
            language,
            bytes: size,
            lines: 0,
            hash,
            is_binary: true,
            is_oversize: false,
            parsed: false,
            symbols: Vec::new(),
            imports: Vec::new(),
            text: None,
            tokens: Vec::new(),
        }));
    }

    if !options.include_vendored {
        let sniff = &bytes[..bytes.len().min(SNIFF_BYTES)];
        if exclude::excluded_by_header(sniff).is_some() {
            return Outcome::Excluded(ExcludedEntry {
                path: rel,
                reason: exclude::Reason::Generated.as_str().to_string(),
            });
        }
    }

    let lines = count_lines(&bytes);

    // Parse only valid UTF-8 with a known grammar. Everything else keeps file-level data.
    let extension = path.extension().and_then(|e| e.to_str());
    let key = grammars::grammar_key(&language, extension);
    let mut text_blob = None;
    let mut token_blob = Vec::new();
    let (parsed, symbols, imports) = match (std::str::from_utf8(&bytes).ok(), registry.get(key)) {
        (Some(text), Some(grammar)) => {
            match grammars::parse(grammar, text, options.parse_timeout) {
                Some(result) => {
                    // Keep the source and its spans for the text tiles (SPEC 2.4).
                    text_blob = zstd::stream::encode_all(text.as_bytes(), TEXT_ZSTD_LEVEL).ok();
                    token_blob = flyover_tiles::text::encode_spans(&result.tokens);
                    (true, result.symbols, result.imports)
                }
                None => (false, Vec::new(), Vec::new()), // parse abandoned by the time budget
            }
        }
        _ => (false, Vec::new(), Vec::new()),
    };

    Outcome::File(Box::new(FileRecord {
        path: rel,
        language,
        bytes: size,
        lines,
        hash,
        is_binary: false,
        is_oversize: false,
        parsed,
        symbols,
        imports,
        text: text_blob,
        tokens: token_blob,
    }))
}

/// Hash an oversize file in bounded memory, sniffing the first chunk for binary content.
fn hash_oversize(path: &Path) -> Option<(String, bool)> {
    let mut file = File::open(path).ok()?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut is_binary = false;
    let mut first = true;
    loop {
        let n = file.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        if first {
            let sniff = &buf[..n.min(SNIFF_BYTES)];
            is_binary = sniff.contains(&0);
            first = false;
        }
        hasher.update(&buf[..n]);
    }
    Some((hasher.finalize().to_hex().to_string(), is_binary))
}

/// Repo-relative path with forward slashes, so output is identical across platforms.
fn to_slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
