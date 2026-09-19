//! Repo indexer.
//!
//! What works today: `scan`, a .gitignore-aware walk that counts files, lines, and bytes per
//! language. M1 grows this into the full index (content hashes, symbols, imports) written to
//! SQLite. See docs/SPEC.md.
//!
//! Rules that hold for everything in this crate:
//! - Never execute anything from the repo being indexed. Read bytes, nothing else.
//! - Never follow symlinks. A repo can point one at /etc.
//! - Output is deterministic: same tree in, same bytes out.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;
use serde::Serialize;

pub mod db;
pub mod exclude;
pub mod grammars;
pub mod index;
pub mod language;
pub mod tokens;

pub use index::{build, run, IndexData, IndexError, Options};

/// Files larger than this are counted but not read line by line.
pub const MAX_TEXT_BYTES: u64 = 2 * 1024 * 1024;

pub(crate) const SNIFF_BYTES: usize = 8 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("{0} is not a directory")]
    NotADirectory(PathBuf),
    #[error("walking the tree failed: {0}")]
    Walk(#[from] ignore::Error),
    #[error("reading {path} failed: {source}")]
    Io { path: PathBuf, source: io::Error },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LanguageStats {
    pub files: u64,
    pub lines: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanReport {
    /// Text files that were read and counted.
    pub files: u64,
    pub lines: u64,
    pub bytes: u64,
    pub skipped_binary: u64,
    pub skipped_oversize: u64,
    /// Sorted by language name so output is stable.
    pub by_language: BTreeMap<String, LanguageStats>,
}

/// Walk `root`, honoring .gitignore, and count text files per language.
pub fn scan(root: &Path) -> Result<ScanReport, ScanError> {
    if !root.is_dir() {
        return Err(ScanError::NotADirectory(root.to_path_buf()));
    }

    let mut report = ScanReport::default();

    let walker = WalkBuilder::new(root)
        .follow_links(false)
        .hidden(false)
        .git_ignore(true)
        .git_exclude(true)
        .require_git(false)
        .filter_entry(|entry| entry.file_name() != ".git")
        .sort_by_file_path(|a, b| a.cmp(b))
        .build();

    for entry in walker {
        let entry = entry?;
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }

        let path = entry.path();
        let io_err = |source| ScanError::Io {
            path: path.to_path_buf(),
            source,
        };
        let size = entry.metadata()?.len();

        if size > MAX_TEXT_BYTES {
            report.skipped_oversize += 1;
            continue;
        }

        let mut bytes = Vec::with_capacity(size as usize);
        File::open(path)
            .map_err(io_err)?
            .read_to_end(&mut bytes)
            .map_err(io_err)?;

        if is_binary(&bytes) {
            report.skipped_binary += 1;
            continue;
        }

        let lines = count_lines(&bytes);
        let stats = report
            .by_language
            .entry(language::detect(path).to_string())
            .or_default();
        stats.files += 1;
        stats.lines += lines;
        stats.bytes += size;
        report.files += 1;
        report.lines += lines;
        report.bytes += size;
    }

    Ok(report)
}

/// Same rule git uses: a NUL byte near the start means binary.
pub(crate) fn is_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(SNIFF_BYTES)].contains(&0)
}

/// Newline count, plus one for a final line with no trailing newline.
pub(crate) fn count_lines(bytes: &[u8]) -> u64 {
    if bytes.is_empty() {
        return 0;
    }
    let newlines = bytes.iter().filter(|&&b| b == b'\n').count() as u64;
    if bytes.last() == Some(&b'\n') {
        newlines
    } else {
        newlines + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("flyover-index-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn counts_lines_with_and_without_trailing_newline() {
        assert_eq!(count_lines(b""), 0);
        assert_eq!(count_lines(b"a"), 1);
        assert_eq!(count_lines(b"a\n"), 1);
        assert_eq!(count_lines(b"a\nb"), 2);
        assert_eq!(count_lines(b"a\nb\n"), 2);
    }

    #[test]
    fn scan_counts_text_skips_binary_and_honors_gitignore() {
        let dir = temp_dir("scan");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::create_dir_all(dir.join("build")).unwrap();
        fs::write(dir.join(".gitignore"), "build/\n").unwrap();
        fs::write(dir.join("src/main.rs"), "fn main() {\n}\n").unwrap();
        fs::write(dir.join("src/app.ts"), "export {};\n").unwrap();
        fs::write(dir.join("src/blob.bin"), [0u8, 1, 2, 3]).unwrap();
        fs::write(dir.join("build/out.js"), "ignored();\n").unwrap();

        let report = scan(&dir).unwrap();

        // .gitignore itself, main.rs, app.ts
        assert_eq!(report.files, 3);
        assert_eq!(report.lines, 4);
        assert_eq!(report.skipped_binary, 1);
        assert_eq!(
            report.by_language["Rust"],
            LanguageStats {
                files: 1,
                lines: 2,
                bytes: 14
            }
        );
        assert_eq!(report.by_language["TypeScript"].files, 1);
        assert!(!report.by_language.contains_key("JavaScript"));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn scan_is_deterministic() {
        let dir = temp_dir("det");
        fs::write(dir.join("b.py"), "x = 1\n").unwrap();
        fs::write(dir.join("a.go"), "package a\n").unwrap();
        assert_eq!(scan(&dir).unwrap(), scan(&dir).unwrap());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn scan_refuses_a_file_path() {
        let dir = temp_dir("file");
        let file = dir.join("x.txt");
        fs::write(&file, "x").unwrap();
        assert!(matches!(scan(&file), Err(ScanError::NotADirectory(_))));
        fs::remove_dir_all(&dir).unwrap();
    }
}
