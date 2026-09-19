//! Binary geometry tile: `.fly` v1.
//!
//! Layout (little-endian), zstd-compressed as a whole:
//! ```text
//! header    magic "FLY1", format version u32, z u8, x u32, y u32, feature count u32
//! features  per feature: id u32, kind u8, depth u8, parent id u32, lines u32,
//!           vertex offset u32, vertex count u32, index offset u32, index count u32
//! vertices  f32 x, f32 y pairs, tile-local coordinates
//! indices   u32, pre-triangulated so the client does no geometry work
//! ```
//! Offsets are in element units: a vertex offset counts vertices (each two f32), an index offset
//! counts u32 indices. Encoding is deterministic, so the same tile round-trips to the same bytes.
//! Encoding (C zstd) is native-only; decoding uses pure-Rust ruzstd so it also runs in wasm32.

use std::io::Read;

use crate::FORMAT_VERSION;

pub const FLY_MAGIC: [u8; 4] = *b"FLY1";
#[cfg(not(target_arch = "wasm32"))]
const ZSTD_LEVEL: i32 = 19;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeatureKind {
    Dir,
    File,
}

impl FeatureKind {
    #[cfg(not(target_arch = "wasm32"))]
    fn to_u8(self) -> u8 {
        match self {
            FeatureKind::Dir => 0,
            FeatureKind::File => 1,
        }
    }

    fn from_u8(v: u8) -> Result<Self, TileError> {
        match v {
            0 => Ok(FeatureKind::Dir),
            1 => Ok(FeatureKind::File),
            other => Err(TileError::BadKind(other)),
        }
    }
}

/// One drawable cell: a directory slab or a file block, pre-triangulated in tile-local space.
#[derive(Debug, Clone, PartialEq)]
pub struct Feature {
    pub id: u32,
    pub kind: FeatureKind,
    pub depth: u8,
    pub parent_id: u32,
    pub lines: u32,
    /// Tile-local (x, y) positions.
    pub vertices: Vec<[f32; 2]>,
    /// Triangle indices into `vertices`.
    pub indices: Vec<u32>,
}

/// A geometry tile at quadtree address `(z, x, y)`.
#[derive(Debug, Clone, PartialEq)]
pub struct Tile {
    pub z: u8,
    pub x: u32,
    pub y: u32,
    pub features: Vec<Feature>,
}

#[derive(Debug, thiserror::Error)]
pub enum TileError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("not a FLY1 tile (bad magic)")]
    BadMagic,
    #[error("tile is format v{found}, this build reads v{expected}")]
    Version { found: u32, expected: u32 },
    #[error("unknown feature kind {0}")]
    BadKind(u8),
    #[error("tile is truncated or its offsets are out of range")]
    Truncated,
    #[error("tile is not valid zstd: {0}")]
    Compression(String),
}

impl Tile {
    /// Encode and zstd-compress. Deterministic for a given tile. Native only.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn encode(&self) -> Result<Vec<u8>, TileError> {
        use std::io::Write;
        let mut buf = Vec::new();
        buf.extend_from_slice(&FLY_MAGIC);
        buf.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        buf.push(self.z);
        buf.extend_from_slice(&self.x.to_le_bytes());
        buf.extend_from_slice(&self.y.to_le_bytes());
        buf.extend_from_slice(&(self.features.len() as u32).to_le_bytes());

        let mut vertices: Vec<[f32; 2]> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();
        for f in &self.features {
            let voff = vertices.len() as u32;
            let ioff = indices.len() as u32;
            buf.extend_from_slice(&f.id.to_le_bytes());
            buf.push(f.kind.to_u8());
            buf.push(f.depth);
            buf.extend_from_slice(&f.parent_id.to_le_bytes());
            buf.extend_from_slice(&f.lines.to_le_bytes());
            buf.extend_from_slice(&voff.to_le_bytes());
            buf.extend_from_slice(&(f.vertices.len() as u32).to_le_bytes());
            buf.extend_from_slice(&ioff.to_le_bytes());
            buf.extend_from_slice(&(f.indices.len() as u32).to_le_bytes());
            vertices.extend_from_slice(&f.vertices);
            indices.extend_from_slice(&f.indices);
        }
        for [x, y] in &vertices {
            buf.extend_from_slice(&x.to_le_bytes());
            buf.extend_from_slice(&y.to_le_bytes());
        }
        for i in &indices {
            buf.extend_from_slice(&i.to_le_bytes());
        }

        let mut out = Vec::new();
        let mut enc = zstd::stream::Encoder::new(&mut out, ZSTD_LEVEL)?;
        enc.write_all(&buf)?;
        enc.finish()?;
        Ok(out)
    }

    /// Decompress and decode bytes produced by [`Tile::encode`].
    pub fn decode(bytes: &[u8]) -> Result<Tile, TileError> {
        let mut raw = Vec::new();
        ruzstd::decoding::StreamingDecoder::new(bytes)
            .map_err(|e| TileError::Compression(e.to_string()))?
            .read_to_end(&mut raw)?;
        let mut r = Reader::new(&raw);

        if r.take(4)? != FLY_MAGIC {
            return Err(TileError::BadMagic);
        }
        let version = r.u32()?;
        if version != FORMAT_VERSION {
            return Err(TileError::Version {
                found: version,
                expected: FORMAT_VERSION,
            });
        }
        let z = r.u8()?;
        let x = r.u32()?;
        let y = r.u32()?;
        let feature_count = r.u32()? as usize;

        struct Meta {
            id: u32,
            kind: FeatureKind,
            depth: u8,
            parent_id: u32,
            lines: u32,
            voff: u32,
            vcount: u32,
            ioff: u32,
            icount: u32,
        }
        let mut metas = Vec::with_capacity(feature_count);
        for _ in 0..feature_count {
            metas.push(Meta {
                id: r.u32()?,
                kind: FeatureKind::from_u8(r.u8()?)?,
                depth: r.u8()?,
                parent_id: r.u32()?,
                lines: r.u32()?,
                voff: r.u32()?,
                vcount: r.u32()?,
                ioff: r.u32()?,
                icount: r.u32()?,
            });
        }

        let total_v = metas.iter().map(|m| m.vcount as usize).sum();
        let mut vertices = Vec::with_capacity(total_v);
        for _ in 0..total_v {
            vertices.push([r.f32()?, r.f32()?]);
        }
        let total_i = metas.iter().map(|m| m.icount as usize).sum();
        let mut indices = Vec::with_capacity(total_i);
        for _ in 0..total_i {
            indices.push(r.u32()?);
        }

        let mut features = Vec::with_capacity(feature_count);
        for m in metas {
            let vs = slice(&vertices, m.voff, m.vcount)?;
            let is = slice(&indices, m.ioff, m.icount)?;
            features.push(Feature {
                id: m.id,
                kind: m.kind,
                depth: m.depth,
                parent_id: m.parent_id,
                lines: m.lines,
                vertices: vs,
                indices: is,
            });
        }
        Ok(Tile { z, x, y, features })
    }
}

fn slice<T: Clone>(all: &[T], off: u32, count: u32) -> Result<Vec<T>, TileError> {
    let start = off as usize;
    let end = start
        .checked_add(count as usize)
        .ok_or(TileError::Truncated)?;
    all.get(start..end)
        .map(<[T]>::to_vec)
        .ok_or(TileError::Truncated)
}

/// Minimal cursor over a byte slice.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], TileError> {
        let end = self.pos.checked_add(n).ok_or(TileError::Truncated)?;
        let s = self.data.get(self.pos..end).ok_or(TileError::Truncated)?;
        self.pos = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, TileError> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, TileError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn f32(&mut self) -> Result<f32, TileError> {
        let b = self.take(4)?;
        Ok(f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Tile {
        Tile {
            z: 2,
            x: 1,
            y: 3,
            features: vec![
                Feature {
                    id: 1,
                    kind: FeatureKind::File,
                    depth: 2,
                    parent_id: 7,
                    lines: 42,
                    vertices: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
                    indices: vec![0, 1, 2, 0, 2, 3],
                },
                Feature {
                    id: 7,
                    kind: FeatureKind::Dir,
                    depth: 1,
                    parent_id: 0,
                    lines: 100,
                    vertices: vec![[0.0, 0.0], [2.0, 0.0], [2.0, 2.0]],
                    indices: vec![0, 1, 2],
                },
            ],
        }
    }

    #[test]
    fn round_trips() {
        let tile = sample();
        let bytes = tile.encode().unwrap();
        assert_eq!(Tile::decode(&bytes).unwrap(), tile);
    }

    #[test]
    fn encoding_is_deterministic() {
        assert_eq!(sample().encode().unwrap(), sample().encode().unwrap());
    }

    #[test]
    fn empty_tile_round_trips() {
        let tile = Tile {
            z: 0,
            x: 0,
            y: 0,
            features: vec![],
        };
        let bytes = tile.encode().unwrap();
        assert_eq!(Tile::decode(&bytes).unwrap(), tile);
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(Tile::decode(b"not a tile").is_err());
    }
}
