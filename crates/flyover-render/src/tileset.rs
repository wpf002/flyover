//! A tile set as the renderer sees it: the manifest, the `paths.bin` index, and the set of tile
//! addresses (from `index/tiles.bin`). Built from disk natively ([`TileSet::open`]) or from bytes
//! fetched over HTTP in the browser ([`TileSet::from_bytes`]). Decoding a tile is
//! platform-neutral ([`decode_tile`]); reading one from disk is native-only and runs on worker
//! threads, never the render thread.

use std::collections::HashSet;

use flyover_tiles::keys;
use flyover_tiles::layer::{LayerTile, LayerValues};
use flyover_tiles::paths::PathsIndex;
use flyover_tiles::tile::Tile;
use flyover_tiles::Manifest;

/// Quadtree tile address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileKey {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

impl TileKey {
    pub fn new(z: u8, x: u32, y: u32) -> Self {
        Self { z, x, y }
    }
    /// The four children one zoom deeper.
    pub fn children(self) -> [TileKey; 4] {
        let (z, x, y) = (self.z + 1, self.x * 2, self.y * 2);
        [
            TileKey::new(z, x, y),
            TileKey::new(z, x + 1, y),
            TileKey::new(z, x, y + 1),
            TileKey::new(z, x + 1, y + 1),
        ]
    }
    pub fn parent(self) -> Option<TileKey> {
        (self.z > 0).then(|| TileKey::new(self.z - 1, self.x / 2, self.y / 2))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TileSetError {
    #[error("io reading {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("manifest: {0}")]
    Manifest(#[from] flyover_tiles::ManifestError),
    #[error("paths index: {0}")]
    Paths(#[from] flyover_tiles::paths::PathsError),
    #[error("tile index: {0}")]
    Keys(#[from] flyover_tiles::keys::KeysError),
    #[error("tile {0:?}: {1}")]
    Tile(TileKey, flyover_tiles::tile::TileError),
    #[error("layer tile {0:?}/{1}: {2}")]
    Layer(TileKey, String, flyover_tiles::layer::LayerError),
}

/// A tile's geometry plus the values of the color and height layers, aligned to the features.
pub struct LoadedTile {
    pub key: TileKey,
    pub tile: Tile,
    pub height: Vec<f32>,
    pub color: Vec<u16>,
}

/// An open tile set: its manifest, path index, and the addresses of every tile.
pub struct TileSet {
    #[cfg(not(target_arch = "wasm32"))]
    root: Option<std::path::PathBuf>,
    pub manifest: Manifest,
    pub paths: PathsIndex,
    pub keys: HashSet<TileKey>,
}

impl TileSet {
    /// Build from the three index files' bytes (manifest.json, index/paths.bin, index/tiles.bin).
    pub fn from_bytes(
        manifest_json: &str,
        paths: &[u8],
        tiles: &[u8],
    ) -> Result<Self, TileSetError> {
        let manifest = Manifest::from_json(manifest_json)?;
        let paths = PathsIndex::decode(paths)?;
        let keys = keys::decode(tiles)?
            .into_iter()
            .map(|(z, x, y)| TileKey::new(z, x, y))
            .collect();
        Ok(TileSet {
            #[cfg(not(target_arch = "wasm32"))]
            root: None,
            manifest,
            paths,
            keys,
        })
    }

    /// Open a tile set directory on disk.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open(root: &std::path::Path) -> Result<Self, TileSetError> {
        let manifest = read(&root.join("manifest.json"))?;
        let paths = read(&root.join("index/paths.bin"))?;
        let tiles = read(&root.join(keys::KEYS_PATH))?;
        let mut set = TileSet::from_bytes(&String::from_utf8_lossy(&manifest), &paths, &tiles)?;
        set.root = Some(root.to_path_buf());
        Ok(set)
    }

    pub fn has(&self, key: TileKey) -> bool {
        self.keys.contains(&key)
    }

    /// The root tiles (zoom 0) that exist. Usually just (0,0,0).
    pub fn roots(&self) -> Vec<TileKey> {
        self.keys.iter().copied().filter(|k| k.z == 0).collect()
    }

    /// Read and decode one tile and its color/height layers from disk. Runs on a worker thread;
    /// debug builds panic if it is ever called on the render thread.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn load(
        &self,
        key: TileKey,
        color_layer: &str,
        height_layer: &str,
    ) -> Result<LoadedTile, TileSetError> {
        use flyover_tiles::{layer_tile_key, tile_key};
        crate::assert_not_render_thread();
        let root = self.root.as_ref().ok_or_else(|| TileSetError::Io {
            path: "<memory>".into(),
            source: std::io::Error::other("tile set was not opened from disk"),
        })?;
        let fly = read(&root.join(tile_key(key.z, key.x, key.y)))?;
        let height = read(&root.join(layer_tile_key(height_layer, key.z, key.x, key.y)))?;
        let color = read(&root.join(layer_tile_key(color_layer, key.z, key.x, key.y)))?;
        decode_tile(key, &fly, &height, &color, height_layer, color_layer)
    }
}

/// Decode a geometry tile and its height/color layer tiles into a [`LoadedTile`]. Pure: no IO,
/// so it runs the same on a native worker thread and in a browser web worker.
pub fn decode_tile(
    key: TileKey,
    fly: &[u8],
    height: &[u8],
    color: &[u8],
    height_layer: &str,
    color_layer: &str,
) -> Result<LoadedTile, TileSetError> {
    let tile = Tile::decode(fly).map_err(|e| TileSetError::Tile(key, e))?;
    let n = tile.features.len();
    let height = match LayerTile::decode(height)
        .map_err(|e| TileSetError::Layer(key, height_layer.into(), e))?
        .values
    {
        LayerValues::Scalar(v) => v,
        LayerValues::Category(v) => v.into_iter().map(f32::from).collect(),
    };
    let color = match LayerTile::decode(color)
        .map_err(|e| TileSetError::Layer(key, color_layer.into(), e))?
        .values
    {
        LayerValues::Category(v) => v,
        LayerValues::Scalar(v) => v.into_iter().map(|f| f as u16).collect(),
    };
    Ok(LoadedTile {
        key,
        tile,
        height: fit(height, n),
        color: fit(color, n),
    })
}

/// Force a layer vector to the feature count (defensive against a mismatched layer tile).
fn fit<T: Clone + Default>(mut v: Vec<T>, n: usize) -> Vec<T> {
    v.resize(n, T::default());
    v
}

#[cfg(not(target_arch = "wasm32"))]
fn read(path: &std::path::Path) -> Result<Vec<u8>, TileSetError> {
    std::fs::read(path).map_err(|source| TileSetError::Io {
        path: path.display().to_string(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn children_and_parent_are_inverse() {
        let k = TileKey::new(2, 1, 3);
        for c in k.children() {
            assert_eq!(c.parent(), Some(k));
            assert_eq!(c.z, 3);
        }
        assert_eq!(TileKey::new(0, 0, 0).parent(), None);
    }
}
