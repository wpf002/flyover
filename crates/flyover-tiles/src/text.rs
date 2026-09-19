//! Text tile: `.ftx`.
//!
//! One file's UTF-8 source plus the token spans produced at index time, so the renderer draws
//! colored source without shipping a parser (docs/SPEC.md 2.4). Fetched per file on demand when
//! the camera gets close enough for lines to be readable.
//!
//! Layout (little-endian), zstd-compressed as a whole:
//! ```text
//! header  magic "FTX1", format version u32, file id u32, text length u32, span bytes length u32
//! text    UTF-8 source
//! spans   packed: varint gap from the previous span's end, varint length, class u8
//! ```
//! Encoding is native-only (it uses the C zstd encoder); decoding is pure Rust and runs in wasm.

use std::io::Read;

use crate::FORMAT_VERSION;

pub const FTX_MAGIC: [u8; 4] = *b"FTX1";
#[cfg(not(target_arch = "wasm32"))]
const ZSTD_LEVEL: i32 = 11;

/// Storage key of a file's text tile, relative to the tile set prefix. Files are grouped 4096 to a
/// directory so no directory grows unbounded.
pub fn text_key(file_id: u32) -> String {
    format!("text/{}/{}.ftx", file_id >> 12, file_id)
}

/// A run of source bytes with one token class. Offsets are bytes into the file's text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenSpan {
    pub start: u32,
    pub len: u32,
    /// Token class; see `flyover_index::tokens::TokenClass`.
    pub class: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextTile {
    pub file_id: u32,
    pub text: String,
    pub spans: Vec<TokenSpan>,
}

#[derive(Debug, thiserror::Error)]
pub enum TextError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("not an FTX1 text tile (bad magic)")]
    BadMagic,
    #[error("text tile is format v{found}, this build reads v{expected}")]
    Version { found: u32, expected: u32 },
    #[error("text tile is truncated")]
    Truncated,
    #[error("text tile is not valid UTF-8")]
    Utf8,
    #[error("text tile is not valid zstd: {0}")]
    Compression(String),
}

impl TextTile {
    /// Encode and zstd-compress. Deterministic for a given tile. Native only.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn encode(&self) -> Result<Vec<u8>, TextError> {
        use std::io::Write;
        let spans = encode_spans(&self.spans);
        let text = self.text.as_bytes();
        let mut buf = Vec::with_capacity(20 + text.len() + spans.len());
        buf.extend_from_slice(&FTX_MAGIC);
        buf.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        buf.extend_from_slice(&self.file_id.to_le_bytes());
        buf.extend_from_slice(&(text.len() as u32).to_le_bytes());
        buf.extend_from_slice(&(spans.len() as u32).to_le_bytes());
        buf.extend_from_slice(text);
        buf.extend_from_slice(&spans);

        let mut out = Vec::new();
        let mut enc = zstd::stream::Encoder::new(&mut out, ZSTD_LEVEL)?;
        enc.write_all(&buf)?;
        enc.finish()?;
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<TextTile, TextError> {
        let mut raw = Vec::new();
        ruzstd::decoding::StreamingDecoder::new(bytes)
            .map_err(|e| TextError::Compression(e.to_string()))?
            .read_to_end(&mut raw)?;

        let u32_at = |at: usize| -> Result<u32, TextError> {
            raw.get(at..at + 4)
                .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .ok_or(TextError::Truncated)
        };
        if raw.get(..4) != Some(&FTX_MAGIC[..]) {
            return Err(if raw.len() < 4 {
                TextError::Truncated
            } else {
                TextError::BadMagic
            });
        }
        let version = u32_at(4)?;
        if version != FORMAT_VERSION {
            return Err(TextError::Version {
                found: version,
                expected: FORMAT_VERSION,
            });
        }
        let file_id = u32_at(8)?;
        let text_len = u32_at(12)? as usize;
        let span_len = u32_at(16)? as usize;
        let text_end = 20usize.checked_add(text_len).ok_or(TextError::Truncated)?;
        let span_end = text_end.checked_add(span_len).ok_or(TextError::Truncated)?;
        let text_bytes = raw.get(20..text_end).ok_or(TextError::Truncated)?;
        let span_bytes = raw.get(text_end..span_end).ok_or(TextError::Truncated)?;
        Ok(TextTile {
            file_id,
            text: String::from_utf8(text_bytes.to_vec()).map_err(|_| TextError::Utf8)?,
            spans: decode_spans(span_bytes),
        })
    }
}

/// Pack spans: varint gap since the previous span's end, varint length, class byte.
pub fn encode_spans(spans: &[TokenSpan]) -> Vec<u8> {
    let mut out = Vec::with_capacity(spans.len() * 3);
    let mut cursor = 0u32;
    for span in spans {
        put_varint(&mut out, span.start.saturating_sub(cursor));
        put_varint(&mut out, span.len);
        out.push(span.class);
        cursor = span.start.saturating_add(span.len);
    }
    out
}

/// Inverse of [`encode_spans`]. Trailing garbage stops the walk rather than failing.
pub fn decode_spans(bytes: &[u8]) -> Vec<TokenSpan> {
    let mut out = Vec::new();
    let mut at = 0usize;
    let mut cursor = 0u32;
    while at < bytes.len() {
        let (Some(gap), Some(len)) = (get_varint(bytes, &mut at), get_varint(bytes, &mut at))
        else {
            break;
        };
        let Some(&class) = bytes.get(at) else {
            break;
        };
        at += 1;
        let start = cursor.saturating_add(gap);
        out.push(TokenSpan { start, len, class });
        cursor = start.saturating_add(len);
    }
    out
}

fn put_varint(out: &mut Vec<u8>, mut value: u32) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn get_varint(bytes: &[u8], at: &mut usize) -> Option<u32> {
    let mut value = 0u32;
    let mut shift = 0u32;
    loop {
        let byte = *bytes.get(*at)?;
        *at += 1;
        value |= u32::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
        shift += 7;
        if shift > 28 {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> TextTile {
        TextTile {
            file_id: 4242,
            text: "fn main() {\n    let x = 1;\n}\n".into(),
            spans: vec![
                TokenSpan {
                    start: 0,
                    len: 2,
                    class: 1,
                },
                TokenSpan {
                    start: 3,
                    len: 4,
                    class: 6,
                },
                TokenSpan {
                    start: 24,
                    len: 1,
                    class: 4,
                },
            ],
        }
    }

    #[test]
    fn round_trips() {
        let tile = sample();
        assert_eq!(TextTile::decode(&tile.encode().unwrap()).unwrap(), tile);
    }

    #[test]
    fn deterministic() {
        assert_eq!(sample().encode().unwrap(), sample().encode().unwrap());
    }

    #[test]
    fn spans_round_trip_including_empty() {
        let spans = sample().spans;
        assert_eq!(decode_spans(&encode_spans(&spans)), spans);
        assert_eq!(decode_spans(&encode_spans(&[])), vec![]);
    }

    #[test]
    fn keys_group_files_by_4096() {
        assert_eq!(text_key(0), "text/0/0.ftx");
        assert_eq!(text_key(4095), "text/0/4095.ftx");
        assert_eq!(text_key(4096), "text/1/4096.ftx");
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(TextTile::decode(b"not a tile").is_err());
    }
}
