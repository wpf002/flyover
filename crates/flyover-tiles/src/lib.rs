//! Tile set format.
//!
//! `Manifest` mirrors `TileSetManifest` in packages/types/src/index.ts. Change both together and
//! bump `FORMAT_VERSION` whenever the on-disk layout changes. Binary tile encoding (M2) lives here
//! too, so the layout crate writes tiles and the render crate reads them through one definition.

use serde::{Deserialize, Serialize};

/// Bump on any breaking change to manifest.json or the binary tile layout.
pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bounds {
    pub min_x: f64,
    pub min_y: f64,
    pub max_x: f64,
    pub max_y: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ShapeSource {
    Generated,
    Uploaded,
    Rectangle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Shape {
    pub source: ShapeSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoInfo {
    pub name: String,
    pub source: String,
    pub commit_sha: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub files: u64,
    pub directories: u64,
    pub lines: u64,
    pub bytes: u64,
    pub languages: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LayerKind {
    Categorical,
    Scalar,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Category {
    pub label: String,
    pub color: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Range {
    pub min: f64,
    pub max: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayerDescriptor {
    pub key: String,
    pub label: String,
    pub kind: LayerKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub categories: Option<Vec<Category>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<Range>,
}

/// manifest.json at the root of every tile set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub format_version: u32,
    pub repo: RepoInfo,
    /// RFC 3339.
    pub generated_at: String,
    pub bounds: Bounds,
    pub max_zoom: u8,
    pub shape: Shape,
    pub stats: Stats,
    pub layers: Vec<LayerDescriptor>,
    pub has_edges: bool,
    pub has_text: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("manifest is not valid JSON for this format: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("tile set is format v{found}, this build reads v{expected}")]
    Version { found: u32, expected: u32 },
}

impl Manifest {
    pub fn from_json(json: &str) -> Result<Self, ManifestError> {
        let manifest: Manifest = serde_json::from_str(json)?;
        if manifest.format_version != FORMAT_VERSION {
            return Err(ManifestError::Version {
                found: manifest.format_version,
                expected: FORMAT_VERSION,
            });
        }
        Ok(manifest)
    }

    pub fn to_json(&self) -> Result<String, ManifestError> {
        Ok(serde_json::to_string_pretty(self)?)
    }
}

/// Storage key of a geometry tile, relative to the tile set prefix.
pub fn tile_key(z: u8, x: u32, y: u32) -> String {
    format!("tiles/{z}/{x}/{y}.fly")
}

/// Storage key of a layer tile. Values line up with feature order in the geometry tile.
pub fn layer_tile_key(layer: &str, z: u8, x: u32, y: u32) -> String {
    format!("layers/{layer}/{z}/{x}/{y}.flv")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Manifest {
        Manifest {
            format_version: FORMAT_VERSION,
            repo: RepoInfo {
                name: "flyover".into(),
                source: "https://github.com/wpf002/flyover".into(),
                commit_sha: "0".repeat(40),
            },
            generated_at: "2026-01-01T00:00:00Z".into(),
            bounds: Bounds {
                min_x: 0.0,
                min_y: 0.0,
                max_x: 1024.0,
                max_y: 1024.0,
            },
            max_zoom: 6,
            shape: Shape {
                source: ShapeSource::Rectangle,
                seed: None,
            },
            stats: Stats {
                files: 10,
                directories: 3,
                lines: 1200,
                bytes: 40_000,
                languages: 2,
            },
            layers: vec![LayerDescriptor {
                key: "language".into(),
                label: "Language".into(),
                kind: LayerKind::Categorical,
                unit: None,
                categories: Some(vec![Category {
                    label: "Rust".into(),
                    color: "#dea584".into(),
                }]),
                range: None,
            }],
            has_edges: false,
            has_text: false,
        }
    }

    #[test]
    fn manifest_round_trips() {
        let json = sample().to_json().unwrap();
        assert_eq!(Manifest::from_json(&json).unwrap(), sample());
    }

    #[test]
    fn manifest_uses_the_same_field_names_as_the_typescript_type() {
        let json = sample().to_json().unwrap();
        for field in [
            "formatVersion",
            "commitSha",
            "generatedAt",
            "maxZoom",
            "hasEdges",
            "minX",
        ] {
            assert!(json.contains(field), "missing {field}");
        }
        assert!(json.contains("\"RECTANGLE\""));
        assert!(json.contains("\"categorical\""));
    }

    #[test]
    fn wrong_version_is_refused() {
        let mut manifest = sample();
        manifest.format_version = FORMAT_VERSION + 1;
        let json = serde_json::to_string(&manifest).unwrap();
        assert!(matches!(
            Manifest::from_json(&json),
            Err(ManifestError::Version { .. })
        ));
    }

    #[test]
    fn keys_are_stable() {
        assert_eq!(tile_key(3, 5, 6), "tiles/3/5/6.fly");
        assert_eq!(layer_tile_key("churn", 3, 5, 6), "layers/churn/3/5/6.flv");
    }
}
