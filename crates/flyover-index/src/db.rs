//! The `index.db` writer.
//!
//! One SQLite file with tables `meta`, `files`, `symbols`, `imports`, `edges`, `excluded`. Rows
//! carry explicit ids assigned in path order, and everything is written in one transaction, so the
//! same tree produces byte-identical output. `edges` is created empty; heuristic resolvers land in
//! M7.

use std::path::Path;

use rusqlite::{params, Connection};

use crate::index::{ExcludedEntry, FileRecord};

/// Bumped whenever the on-disk table layout changes.
/// v2: the `text` table, so the layout stage can write text tiles from index.db alone.
pub const SCHEMA_VERSION: u32 = 2;

const SCHEMA: &str = r#"
CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) WITHOUT ROWID;

CREATE TABLE files (
    id          INTEGER PRIMARY KEY,
    path        TEXT NOT NULL UNIQUE,
    language    TEXT NOT NULL,
    bytes       INTEGER NOT NULL,
    lines       INTEGER NOT NULL,
    hash        TEXT NOT NULL,
    is_binary   INTEGER NOT NULL,
    is_oversize INTEGER NOT NULL,
    parsed      INTEGER NOT NULL
);

CREATE TABLE symbols (
    id         INTEGER PRIMARY KEY,
    file_id    INTEGER NOT NULL REFERENCES files(id),
    kind       TEXT NOT NULL,
    name       TEXT NOT NULL,
    start_line INTEGER NOT NULL,
    end_line   INTEGER NOT NULL
);

CREATE TABLE imports (
    id      INTEGER PRIMARY KEY,
    file_id INTEGER NOT NULL REFERENCES files(id),
    module  TEXT NOT NULL,
    line    INTEGER NOT NULL
);

-- Dependency edges. Populated by the M7 resolvers; created here so the schema is stable.
CREATE TABLE edges (
    id          INTEGER PRIMARY KEY,
    src_file_id INTEGER NOT NULL REFERENCES files(id),
    dst_file_id INTEGER NOT NULL REFERENCES files(id),
    kind        TEXT NOT NULL,
    confidence  TEXT NOT NULL CHECK (confidence IN ('exact', 'heuristic'))
);

-- Source text and token spans for the files that were parsed, so `flyover layout` can write
-- text tiles without the repo. Content is zstd-compressed UTF-8; tokens are packed spans.
CREATE TABLE text (
    file_id INTEGER PRIMARY KEY REFERENCES files(id),
    content BLOB NOT NULL,
    tokens  BLOB NOT NULL
);

CREATE TABLE excluded (
    id     INTEGER PRIMARY KEY,
    path   TEXT NOT NULL UNIQUE,
    reason TEXT NOT NULL
);

CREATE INDEX idx_symbols_file ON symbols(file_id);
CREATE INDEX idx_imports_file ON imports(file_id);
"#;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// Write the whole index to `path`, replacing any existing file. Rows are inserted in the order
/// the slices are given, which the indexer guarantees is path order.
pub fn write(
    path: &Path,
    files: &[FileRecord],
    excluded: &[ExcludedEntry],
    include_vendored: bool,
) -> Result<(), DbError> {
    // A full rebuild. Remove any prior file so output depends only on the input tree.
    let _ = std::fs::remove_file(path);

    let mut conn = Connection::open(path)?;
    // No journal or fsync side effects: a full rebuild is cheap to repeat, and this keeps the
    // output file free of -wal/-journal companions that would break byte-for-byte comparison.
    conn.execute_batch("PRAGMA journal_mode=MEMORY; PRAGMA synchronous=OFF;")?;

    let tx = conn.transaction()?;
    tx.execute_batch(SCHEMA)?;

    {
        let mut meta = tx.prepare("INSERT INTO meta (key, value) VALUES (?1, ?2)")?;
        // Deterministic metadata only: no timestamps, no absolute paths.
        meta.execute(params!["schema_version", SCHEMA_VERSION.to_string()])?;
        meta.execute(params![
            "include_vendored",
            u8::from(include_vendored).to_string()
        ])?;
    }

    {
        let mut ins_file = tx.prepare(
            "INSERT INTO files (id, path, language, bytes, lines, hash, is_binary, is_oversize, parsed)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )?;
        let mut ins_sym = tx.prepare(
            "INSERT INTO symbols (id, file_id, kind, name, start_line, end_line)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        let mut ins_imp =
            tx.prepare("INSERT INTO imports (id, file_id, module, line) VALUES (?1, ?2, ?3, ?4)")?;
        let mut ins_text =
            tx.prepare("INSERT INTO text (file_id, content, tokens) VALUES (?1, ?2, ?3)")?;

        let mut symbol_id: i64 = 0;
        let mut import_id: i64 = 0;
        for (index, file) in files.iter().enumerate() {
            let file_id = index as i64 + 1;
            ins_file.execute(params![
                file_id,
                file.path,
                file.language,
                file.bytes as i64,
                file.lines as i64,
                file.hash,
                i64::from(file.is_binary),
                i64::from(file.is_oversize),
                i64::from(file.parsed),
            ])?;
            for sym in &file.symbols {
                symbol_id += 1;
                ins_sym.execute(params![
                    symbol_id,
                    file_id,
                    sym.kind,
                    sym.name,
                    sym.start_line,
                    sym.end_line,
                ])?;
            }
            for imp in &file.imports {
                import_id += 1;
                ins_imp.execute(params![import_id, file_id, imp.module, imp.line])?;
            }
            if let Some(text) = &file.text {
                ins_text.execute(params![file_id, text, file.tokens])?;
            }
        }

        let mut ins_excl =
            tx.prepare("INSERT INTO excluded (id, path, reason) VALUES (?1, ?2, ?3)")?;
        for (index, entry) in excluded.iter().enumerate() {
            ins_excl.execute(params![index as i64 + 1, entry.path, entry.reason])?;
        }
    }

    tx.commit()?;
    Ok(())
}
