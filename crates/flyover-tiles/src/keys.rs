//! `index/tiles.bin`: the address of every geometry tile in the set.
//!
//! A reader that can list directories (the native viewer) could scan `tiles/`, but one fetching
//! over HTTP (the browser) can't, and LOD selection needs to know which children exist before it
//! descends. Layout writes this list once; it is small (9 bytes per tile) and immutable.
//!
//! Layout (little-endian): magic "FTIX", format version u32, count u32, then per tile (sorted by
//! z, x, y): z u8, x u32, y u32.

use crate::FORMAT_VERSION;

pub const KEYS_MAGIC: [u8; 4] = *b"FTIX";
/// Storage key of the index, relative to the tile set prefix.
pub const KEYS_PATH: &str = "index/tiles.bin";

#[derive(Debug, thiserror::Error)]
pub enum KeysError {
    #[error("not an FTIX tile index (bad magic)")]
    BadMagic,
    #[error("tile index is format v{found}, this build reads v{expected}")]
    Version { found: u32, expected: u32 },
    #[error("tile index is truncated")]
    Truncated,
}

/// Encode tile addresses, sorted, so the bytes are deterministic.
pub fn encode(keys: &[(u8, u32, u32)]) -> Vec<u8> {
    let mut sorted = keys.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut buf = Vec::with_capacity(12 + sorted.len() * 9);
    buf.extend_from_slice(&KEYS_MAGIC);
    buf.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    buf.extend_from_slice(&(sorted.len() as u32).to_le_bytes());
    for (z, x, y) in sorted {
        buf.push(z);
        buf.extend_from_slice(&x.to_le_bytes());
        buf.extend_from_slice(&y.to_le_bytes());
    }
    buf
}

pub fn decode(bytes: &[u8]) -> Result<Vec<(u8, u32, u32)>, KeysError> {
    let u32_at = |at: usize| -> Result<u32, KeysError> {
        bytes
            .get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .ok_or(KeysError::Truncated)
    };
    if bytes.get(..4) != Some(&KEYS_MAGIC[..]) {
        return Err(if bytes.len() < 4 {
            KeysError::Truncated
        } else {
            KeysError::BadMagic
        });
    }
    let version = u32_at(4)?;
    if version != FORMAT_VERSION {
        return Err(KeysError::Version {
            found: version,
            expected: FORMAT_VERSION,
        });
    }
    let count = u32_at(8)? as usize;
    let body = bytes.get(12..).ok_or(KeysError::Truncated)?;
    if body.len() < count * 9 {
        return Err(KeysError::Truncated);
    }
    Ok(body
        .chunks_exact(9)
        .take(count)
        .map(|c| {
            (
                c[0],
                u32::from_le_bytes([c[1], c[2], c[3], c[4]]),
                u32::from_le_bytes([c[5], c[6], c[7], c[8]]),
            )
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_sorted_and_deduplicated() {
        let bytes = encode(&[(1, 1, 0), (0, 0, 0), (1, 0, 0), (1, 1, 0)]);
        assert_eq!(
            decode(&bytes).unwrap(),
            vec![(0, 0, 0), (1, 0, 0), (1, 1, 0)]
        );
    }

    #[test]
    fn deterministic() {
        assert_eq!(
            encode(&[(2, 3, 1), (0, 0, 0)]),
            encode(&[(0, 0, 0), (2, 3, 1)])
        );
    }

    #[test]
    fn rejects_garbage_and_truncation() {
        assert!(decode(b"nope").is_err());
        let mut bytes = encode(&[(0, 0, 0), (1, 0, 1)]);
        bytes.truncate(bytes.len() - 3);
        assert!(matches!(decode(&bytes), Err(KeysError::Truncated)));
    }
}
