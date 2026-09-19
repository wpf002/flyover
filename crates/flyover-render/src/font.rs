//! The baked monospace font atlas: one signed-distance-field cell per printable ASCII character.
//!
//! The bytes come from `assets/font-atlas.bin`, produced by `cargo run -p flyover-atlas` and
//! committed. Baking offline keeps a font parser and a distance-field generator out of both the
//! native binary and the wasm bundle; this module only reads a header and hands the pixels to the
//! GPU.
//!
//! Header (little-endian): magic `FATL`, version, then `atlas_w, atlas_h, cell_w, cell_h, cols,
//! count, first_char` as u32, then `quad_w, quad_h, origin_x, origin_y, advance, line_height,
//! range` as f32 in em units, then `atlas_w * atlas_h` bytes of R8 distance field (128 = on the
//! edge, higher = inside).

pub const ATLAS_BYTES: &[u8] = include_bytes!("../assets/font-atlas.bin");
const MAGIC: &[u8; 4] = b"FATL";
const VERSION: u32 = 1;
const HEADER: usize = 8 + 7 * 4 + 7 * 4;

/// Glyph geometry in em units, all a text layout needs from the atlas.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    /// Size of one glyph quad (larger than the advance: the cell is padded for the SDF).
    pub quad_w: f32,
    pub quad_h: f32,
    /// Lower-left corner of the quad relative to the pen position on the baseline.
    pub origin_x: f32,
    pub origin_y: f32,
    /// Pen advance per character. Monospace, so it is the same for every glyph.
    pub advance: f32,
    /// Baseline-to-baseline distance.
    pub line_height: f32,
    /// Distance from the glyph edge that the stored field spans, at full black to full white.
    pub range: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct Atlas<'a> {
    pub width: u32,
    pub height: u32,
    pub cell_w: u32,
    pub cell_h: u32,
    pub cols: u32,
    pub count: u32,
    pub first_char: u32,
    pub metrics: Metrics,
    pub pixels: &'a [u8],
}

#[derive(Debug, thiserror::Error)]
pub enum AtlasError {
    #[error("not a FATL font atlas (bad magic)")]
    BadMagic,
    #[error("font atlas is v{found}, this build reads v{expected}")]
    Version { found: u32, expected: u32 },
    #[error("font atlas is truncated")]
    Truncated,
}

impl Atlas<'static> {
    /// The atlas compiled into this binary.
    pub fn bundled() -> Atlas<'static> {
        Atlas::parse(ATLAS_BYTES).expect("the bundled font atlas is valid")
    }
}

impl<'a> Atlas<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Atlas<'a>, AtlasError> {
        if bytes.len() < HEADER {
            return Err(AtlasError::Truncated);
        }
        if &bytes[..4] != MAGIC {
            return Err(AtlasError::BadMagic);
        }
        let u32_at = |at: usize| {
            u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
        };
        let f32_at = |at: usize| f32::from_bits(u32_at(at));
        let version = u32_at(4);
        if version != VERSION {
            return Err(AtlasError::Version {
                found: version,
                expected: VERSION,
            });
        }
        let width = u32_at(8);
        let height = u32_at(12);
        let pixels = bytes.get(HEADER..).ok_or(AtlasError::Truncated)?;
        if pixels.len() < (width as usize) * (height as usize) {
            return Err(AtlasError::Truncated);
        }
        Ok(Atlas {
            width,
            height,
            cell_w: u32_at(16),
            cell_h: u32_at(20),
            cols: u32_at(24),
            count: u32_at(28),
            first_char: u32_at(32),
            metrics: Metrics {
                quad_w: f32_at(36),
                quad_h: f32_at(40),
                origin_x: f32_at(44),
                origin_y: f32_at(48),
                advance: f32_at(52),
                line_height: f32_at(56),
                range: f32_at(60),
            },
            pixels: &pixels[..(width as usize) * (height as usize)],
        })
    }

    /// Atlas cell for a character, or `None` for anything outside the baked range (drawn as a gap).
    pub fn cell(&self, ch: char) -> Option<u32> {
        let code = ch as u32;
        (code >= self.first_char && code < self.first_char + self.count)
            .then(|| code - self.first_char)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundled_atlas_parses() {
        let atlas = Atlas::bundled();
        assert_eq!(atlas.first_char, 32);
        assert_eq!(atlas.count, 95);
        assert_eq!(atlas.pixels.len(), (atlas.width * atlas.height) as usize);
        assert!(atlas.metrics.advance > 0.0 && atlas.metrics.line_height > 0.0);
        // Padded cells: the drawn quad is wider and taller than the advance box.
        assert!(atlas.metrics.quad_w > atlas.metrics.advance);
        assert!(atlas.metrics.quad_h > atlas.metrics.line_height * 0.9);
    }

    #[test]
    fn cells_cover_printable_ascii_only() {
        let atlas = Atlas::bundled();
        assert_eq!(atlas.cell(' '), Some(0));
        assert_eq!(atlas.cell('A'), Some(33));
        assert_eq!(atlas.cell('~'), Some(94));
        assert_eq!(atlas.cell('\t'), None);
        assert_eq!(atlas.cell('é'), None);
    }

    #[test]
    fn bad_input_is_rejected() {
        assert!(matches!(Atlas::parse(b"nope"), Err(AtlasError::Truncated)));
        let mut bad = ATLAS_BYTES.to_vec();
        bad[0] = b'X';
        assert!(matches!(Atlas::parse(&bad), Err(AtlasError::BadMagic)));
    }
}
