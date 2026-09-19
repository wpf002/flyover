//! Layer value tile: `.flv`.
//!
//! One value per feature, in the same order as the geometry tile's feature table. That alignment
//! is why binding a new layer never rewrites geometry. Scalar layers store `f32`, categorical
//! layers store a `u16` index into the layer's category list in the manifest.
//!
//! Layout (little-endian): magic "FLV1", format version u32, z u8, x u32, y u32, dtype u8,
//! count u32, then the packed array. Not compressed: the arrays are small and read as a block.

use std::io::Read;

use crate::FORMAT_VERSION;

pub const FLV_MAGIC: [u8; 4] = *b"FLV1";

/// The values for one layer over one tile, aligned to that tile's features.
#[derive(Debug, Clone, PartialEq)]
pub enum LayerValues {
    Scalar(Vec<f32>),
    Category(Vec<u16>),
}

impl LayerValues {
    pub fn len(&self) -> usize {
        match self {
            LayerValues::Scalar(v) => v.len(),
            LayerValues::Category(v) => v.len(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn dtype(&self) -> u8 {
        match self {
            LayerValues::Scalar(_) => 0,
            LayerValues::Category(_) => 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LayerTile {
    pub z: u8,
    pub x: u32,
    pub y: u32,
    pub values: LayerValues,
}

#[derive(Debug, thiserror::Error)]
pub enum LayerError {
    #[error("not an FLV1 layer tile (bad magic)")]
    BadMagic,
    #[error("layer tile is format v{found}, this build reads v{expected}")]
    Version { found: u32, expected: u32 },
    #[error("unknown layer dtype {0}")]
    BadDtype(u8),
    #[error("layer tile is truncated")]
    Truncated,
}

impl LayerTile {
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&FLV_MAGIC);
        buf.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        buf.push(self.z);
        buf.extend_from_slice(&self.x.to_le_bytes());
        buf.extend_from_slice(&self.y.to_le_bytes());
        buf.push(self.values.dtype());
        buf.extend_from_slice(&(self.values.len() as u32).to_le_bytes());
        match &self.values {
            LayerValues::Scalar(v) => {
                for f in v {
                    buf.extend_from_slice(&f.to_le_bytes());
                }
            }
            LayerValues::Category(v) => {
                for c in v {
                    buf.extend_from_slice(&c.to_le_bytes());
                }
            }
        }
        buf
    }

    pub fn decode(bytes: &[u8]) -> Result<LayerTile, LayerError> {
        let mut r = bytes;
        let mut magic = [0u8; 4];
        r.read_exact(&mut magic)
            .map_err(|_| LayerError::Truncated)?;
        if magic != FLV_MAGIC {
            return Err(LayerError::BadMagic);
        }
        let version = read_u32(&mut r)?;
        if version != FORMAT_VERSION {
            return Err(LayerError::Version {
                found: version,
                expected: FORMAT_VERSION,
            });
        }
        let z = read_u8(&mut r)?;
        let x = read_u32(&mut r)?;
        let y = read_u32(&mut r)?;
        let dtype = read_u8(&mut r)?;
        let count = read_u32(&mut r)? as usize;
        let values = match dtype {
            0 => {
                let mut v = Vec::with_capacity(count);
                for _ in 0..count {
                    let mut b = [0u8; 4];
                    r.read_exact(&mut b).map_err(|_| LayerError::Truncated)?;
                    v.push(f32::from_le_bytes(b));
                }
                LayerValues::Scalar(v)
            }
            1 => {
                let mut v = Vec::with_capacity(count);
                for _ in 0..count {
                    let mut b = [0u8; 2];
                    r.read_exact(&mut b).map_err(|_| LayerError::Truncated)?;
                    v.push(u16::from_le_bytes(b));
                }
                LayerValues::Category(v)
            }
            other => return Err(LayerError::BadDtype(other)),
        };
        Ok(LayerTile { z, x, y, values })
    }
}

fn read_u8(r: &mut &[u8]) -> Result<u8, LayerError> {
    let mut b = [0u8; 1];
    r.read_exact(&mut b).map_err(|_| LayerError::Truncated)?;
    Ok(b[0])
}

fn read_u32(r: &mut &[u8]) -> Result<u32, LayerError> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b).map_err(|_| LayerError::Truncated)?;
    Ok(u32::from_le_bytes(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_round_trips() {
        let t = LayerTile {
            z: 1,
            x: 0,
            y: 1,
            values: LayerValues::Scalar(vec![1.0, 2.5, 42.0]),
        };
        assert_eq!(LayerTile::decode(&t.encode()).unwrap(), t);
    }

    #[test]
    fn category_round_trips() {
        let t = LayerTile {
            z: 3,
            x: 2,
            y: 5,
            values: LayerValues::Category(vec![0, 1, 1, 7]),
        };
        assert_eq!(LayerTile::decode(&t.encode()).unwrap(), t);
    }

    #[test]
    fn deterministic() {
        let t = LayerTile {
            z: 0,
            x: 0,
            y: 0,
            values: LayerValues::Scalar(vec![3.0, 1.0]),
        };
        assert_eq!(t.encode(), t.encode());
    }
}
