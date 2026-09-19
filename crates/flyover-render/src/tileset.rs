//! Reading a tile set from disk: the manifest, the `paths.bin` index, the set of tile addresses
//! that exist, and decoding individual tiles. Tile decoding (disk read + zstd) is intended to run
//! on worker threads, never the render thread; see [`crate::cache`].

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use flyover_tiles::layer::{LayerTile, LayerValues};
use flyover_tiles::paths::PathsIndex;
use flyover_tiles::tile::Tile;
use flyover_tiles::{layer_tile_key, tile_key, Manifest};

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

/// An open tile set: its manifest, path index, and the addresses of every tile on disk.
pub struct TileSet {
    root: PathBuf,
    pub manifest: Manifest,
    pub paths: PathsIndex,
    pub keys: HashSet<TileKey>,
}

impl TileSet {
    pub fn open(root: &Path) -> Result<Self, TileSetError> {
        let manifest_path = root.join("manifest.json");
        let manifest_json = read(&manifest_path)?;
        let manifest = Manifest::from_json(&String::from_utf8_lossy(&manifest_json))?;

        let paths_path = root.join("index/paths.bin");
        let paths = PathsIndex::decode(&read(&paths_path)?)?;

        let keys = scan_keys(&root.join("tiles"));
        Ok(TileSet {
            root: root.to_path_buf(),
            manifest,
            paths,
            keys,
        })
    }

    pub fn has(&self, key: TileKey) -> bool {
        self.keys.contains(&key)
    }

    /// The root tiles (zoom 0) that exist. Usually just (0,0,0).
    pub fn roots(&self) -> Vec<TileKey> {
        self.keys.iter().copied().filter(|k| k.z == 0).collect()
    }

    /// Read and decode one tile and its color/height layers. Runs on a worker thread; debug builds
    /// panic if it is ever called on the render thread.
    pub fn load(
        &self,
        key: TileKey,
        color_layer: &str,
        height_layer: &str,
    ) -> Result<LoadedTile, TileSetError> {
        crate::assert_not_render_thread();
        let tile = Tile::decode(&read(&self.root.join(tile_key(key.z, key.x, key.y)))?)
            .map_err(|e| TileSetError::Tile(key, e))?;
        let height = self.scalar_layer(key, height_layer, tile.features.len())?;
        let color = self.category_layer(key, color_layer, tile.features.len())?;
        Ok(LoadedTile {
            key,
            tile,
            height,
            color,
        })
    }

    fn scalar_layer(&self, key: TileKey, layer: &str, n: usize) -> Result<Vec<f32>, TileSetError> {
        let path = self.root.join(layer_tile_key(layer, key.z, key.x, key.y));
        match LayerTile::decode(&read(&path)?)
            .map_err(|e| TileSetError::Layer(key, layer.into(), e))?
        {
            LayerTile {
                values: LayerValues::Scalar(v),
                ..
            } => Ok(v),
            LayerTile {
                values: LayerValues::Category(v),
                ..
            } => Ok(v.into_iter().map(f32::from).collect()),
        }
        .map(|v| fit(v, n))
    }

    fn category_layer(
        &self,
        key: TileKey,
        layer: &str,
        n: usize,
    ) -> Result<Vec<u16>, TileSetError> {
        let path = self.root.join(layer_tile_key(layer, key.z, key.x, key.y));
        match LayerTile::decode(&read(&path)?)
            .map_err(|e| TileSetError::Layer(key, layer.into(), e))?
        {
            LayerTile {
                values: LayerValues::Category(v),
                ..
            } => Ok(v),
            LayerTile {
                values: LayerValues::Scalar(v),
                ..
            } => Ok(v.into_iter().map(|f| f as u16).collect()),
        }
        .map(|v| fit(v, n))
    }
}

/// Force a layer vector to the feature count (defensive against a mismatched layer tile).
fn fit<T: Clone + Default>(mut v: Vec<T>, n: usize) -> Vec<T> {
    v.resize(n, T::default());
    v
}

fn read(path: &Path) -> Result<Vec<u8>, TileSetError> {
    std::fs::read(path).map_err(|source| TileSetError::Io {
        path: path.display().to_string(),
        source,
    })
}

/// Walk `tiles/` once and collect every `(z, x, y)` present. In-memory afterwards, so LOD
/// selection never touches the filesystem.
fn scan_keys(tiles_root: &Path) -> HashSet<TileKey> {
    let mut keys = HashSet::new();
    let Ok(zdirs) = std::fs::read_dir(tiles_root) else {
        return keys;
    };
    for z in zdirs.flatten() {
        let Some(zn) = z.file_name().to_str().and_then(|s| s.parse::<u8>().ok()) else {
            continue;
        };
        let Ok(xdirs) = std::fs::read_dir(z.path()) else {
            continue;
        };
        for x in xdirs.flatten() {
            let Some(xn) = x.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
                continue;
            };
            let Ok(yfiles) = std::fs::read_dir(x.path()) else {
                continue;
            };
            for yf in yfiles.flatten() {
                let name = yf.file_name();
                let Some(stem) = name.to_str().and_then(|s| s.strip_suffix(".fly")) else {
                    continue;
                };
                if let Ok(yn) = stem.parse::<u32>() {
                    keys.insert(TileKey::new(zn, xn, yn));
                }
            }
        }
    }
    keys
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
