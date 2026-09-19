//! `index/paths.bin`: a compact fileId -> path and stats table for picking and search.
//!
//! Layout (little-endian):
//! ```text
//! header  magic "FPTH", format version u32, entry count u32
//! entries per entry (sorted by id): id u32, lines u32, bytes u32, path len u32, path bytes (UTF-8)
//! ```
//! Not compressed: it is fetched once and read randomly. Entries are sorted by id, so output is
//! deterministic.

use std::io::Read;

use crate::FORMAT_VERSION;

pub const PATHS_MAGIC: [u8; 4] = *b"FPTH";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathEntry {
    pub id: u32,
    pub lines: u32,
    pub bytes: u32,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PathsIndex {
    pub entries: Vec<PathEntry>,
}

#[derive(Debug, thiserror::Error)]
pub enum PathsError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("not an FPTH index (bad magic)")]
    BadMagic,
    #[error("paths index is format v{found}, this build reads v{expected}")]
    Version { found: u32, expected: u32 },
    #[error("paths index is truncated")]
    Truncated,
    #[error("path is not valid UTF-8")]
    Utf8,
}

impl PathsIndex {
    /// Encode, sorting entries by id first so the bytes are deterministic.
    pub fn encode(&self) -> Vec<u8> {
        let mut entries: Vec<&PathEntry> = self.entries.iter().collect();
        entries.sort_by_key(|e| e.id);

        let mut buf = Vec::new();
        buf.extend_from_slice(&PATHS_MAGIC);
        buf.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        buf.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        for e in entries {
            let path = e.path.as_bytes();
            buf.extend_from_slice(&e.id.to_le_bytes());
            buf.extend_from_slice(&e.lines.to_le_bytes());
            buf.extend_from_slice(&e.bytes.to_le_bytes());
            buf.extend_from_slice(&(path.len() as u32).to_le_bytes());
            buf.extend_from_slice(path);
        }
        buf
    }

    pub fn decode(bytes: &[u8]) -> Result<PathsIndex, PathsError> {
        let mut r = bytes;
        let mut magic = [0u8; 4];
        r.read_exact(&mut magic)
            .map_err(|_| PathsError::Truncated)?;
        if magic != PATHS_MAGIC {
            return Err(PathsError::BadMagic);
        }
        let version = read_u32(&mut r)?;
        if version != FORMAT_VERSION {
            return Err(PathsError::Version {
                found: version,
                expected: FORMAT_VERSION,
            });
        }
        let count = read_u32(&mut r)? as usize;
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            let id = read_u32(&mut r)?;
            let lines = read_u32(&mut r)?;
            let byte_count = read_u32(&mut r)?;
            let path_len = read_u32(&mut r)? as usize;
            let mut path_bytes = vec![0u8; path_len];
            r.read_exact(&mut path_bytes)
                .map_err(|_| PathsError::Truncated)?;
            let path = String::from_utf8(path_bytes).map_err(|_| PathsError::Utf8)?;
            entries.push(PathEntry {
                id,
                lines,
                bytes: byte_count,
                path,
            });
        }
        Ok(PathsIndex { entries })
    }
}

fn read_u32(r: &mut &[u8]) -> Result<u32, PathsError> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b).map_err(|_| PathsError::Truncated)?;
    Ok(u32::from_le_bytes(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> PathsIndex {
        PathsIndex {
            entries: vec![
                PathEntry {
                    id: 2,
                    lines: 10,
                    bytes: 200,
                    path: "src/b.rs".into(),
                },
                PathEntry {
                    id: 1,
                    lines: 5,
                    bytes: 90,
                    path: "src/a.rs".into(),
                },
            ],
        }
    }

    #[test]
    fn round_trips_sorted_by_id() {
        let bytes = sample().encode();
        let decoded = PathsIndex::decode(&bytes).unwrap();
        assert_eq!(decoded.entries.len(), 2);
        assert_eq!(decoded.entries[0].id, 1);
        assert_eq!(decoded.entries[1].id, 2);
        assert_eq!(decoded.entries[0].path, "src/a.rs");
    }

    #[test]
    fn deterministic() {
        assert_eq!(sample().encode(), sample().encode());
    }

    #[test]
    fn rejects_garbage() {
        assert!(PathsIndex::decode(b"xxxx....").is_err());
    }
}
