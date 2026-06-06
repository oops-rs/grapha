use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};

use grapha_core::graph::Graph;

const ASSET_SNAPSHOT_VERSION: &str = "1";
const ASSET_SNAPSHOT_FILE: &str = "assets.json";

const META_ASSET_REF_KIND: &str = "asset.ref_kind";
const META_ASSET_NAME: &str = "asset.name";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetRecord {
    pub name: String,
    pub group_path: String,
    pub catalog: String,
    pub catalog_dir: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template_intent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provides_namespace: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct AssetSnapshot {
    version: String,
    records: Vec<AssetRecord>,
}

impl AssetSnapshot {
    fn new(mut records: Vec<AssetRecord>) -> Self {
        sort_records(&mut records);
        Self {
            version: ASSET_SNAPSHOT_VERSION.to_string(),
            records,
        }
    }

    fn record_count(&self) -> usize {
        self.records.len()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetSnapshotWarning {
    pub catalog_path: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetSnapshotBuildStats {
    pub record_count: usize,
    pub warnings: Vec<AssetSnapshotWarning>,
}

#[derive(Debug, Default, Clone)]
pub struct AssetCatalogIndex {
    records: Vec<AssetRecord>,
    by_name: HashMap<String, Vec<usize>>,
}

impl AssetCatalogIndex {
    fn insert(&mut self, record: AssetRecord) {
        let index = self.records.len();
        self.by_name
            .entry(record.name.clone())
            .or_default()
            .push(index);
        self.records.push(record);
    }

    pub(crate) fn from_records(mut records: Vec<AssetRecord>) -> Self {
        sort_records(&mut records);
        let mut index = Self::default();
        for record in records {
            index.insert(record);
        }
        index
    }

    #[allow(dead_code)]
    pub fn records_for_name(&self, name: &str) -> Vec<AssetRecord> {
        self.by_name
            .get(name)
            .into_iter()
            .flatten()
            .filter_map(|index| self.records.get(*index))
            .cloned()
            .collect()
    }

    pub fn all_records(&self) -> &[AssetRecord] {
        &self.records
    }
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

fn discover_xcassets_dirs(root: &Path) -> Vec<PathBuf> {
    if root.is_file() {
        return Vec::new();
    }

    let mut dirs = Vec::new();
    let walker = WalkBuilder::new(root).hidden(true).git_ignore(true).build();

    for entry in walker.flatten() {
        let path = entry.path();
        if path.is_dir()
            && path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|ext| ext == "xcassets")
        {
            dirs.push(path.to_path_buf());
        }
    }

    dirs.sort();
    dirs
}

// ---------------------------------------------------------------------------
// Build snapshot
// ---------------------------------------------------------------------------

pub fn build_and_save_snapshot(
    root: &Path,
    store_dir: &Path,
) -> anyhow::Result<AssetSnapshotBuildStats> {
    let (snapshot, warnings) = build_snapshot(root)?;
    let count = snapshot.record_count();
    save_snapshot(store_dir, &snapshot)?;
    Ok(AssetSnapshotBuildStats {
        record_count: count,
        warnings,
    })
}

fn build_snapshot(root: &Path) -> anyhow::Result<(AssetSnapshot, Vec<AssetSnapshotWarning>)> {
    if root.is_file() {
        return Ok((AssetSnapshot::new(Vec::new()), Vec::new()));
    }

    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let catalog_dirs = discover_xcassets_dirs(&root);
    let mut records = Vec::new();
    let mut warnings = Vec::new();

    for catalog_path in catalog_dirs {
        let report = match xcassets::parse_catalog(&catalog_path) {
            Ok(report) => report,
            Err(error) => {
                warnings.push(AssetSnapshotWarning {
                    catalog_path: path_relative_string(&root, &catalog_path),
                    reason: format!("failed to parse xcassets catalog: {error}"),
                });
                continue;
            }
        };

        for diagnostic in &report.diagnostics {
            if matches!(diagnostic.severity, xcassets::Severity::Error) {
                warnings.push(AssetSnapshotWarning {
                    catalog_path: path_relative_string(&root, &catalog_path),
                    reason: diagnostic.message.clone(),
                });
            }
        }

        let catalog_name = report.catalog.name.clone();
        let catalog_dir = catalog_path
            .parent()
            .map(|p| path_relative_string(&root, p))
            .unwrap_or_else(|| ".".to_string());

        collect_image_sets(
            &report.catalog.children,
            &catalog_name,
            &catalog_dir,
            "",
            false,
            &mut records,
        );
    }

    Ok((AssetSnapshot::new(records), warnings))
}

fn collect_image_sets(
    nodes: &[xcassets::Node],
    catalog: &str,
    catalog_dir: &str,
    group_path: &str,
    parent_namespaced: bool,
    records: &mut Vec<AssetRecord>,
) {
    for node in nodes {
        match node {
            xcassets::Node::ImageSet(image_set) => {
                let raw_name = image_set.name.trim_end_matches(".imageset");
                let name = if parent_namespaced && !group_path.is_empty() {
                    format!("{group_path}/{raw_name}")
                } else {
                    raw_name.to_string()
                };

                let template_intent = image_set
                    .contents
                    .as_ref()
                    .and_then(|c| c.properties.template_rendering_intent.clone());

                let provides_namespace = image_set
                    .contents
                    .as_ref()
                    .and_then(|c| c.properties.provides_namespace);

                records.push(AssetRecord {
                    name,
                    group_path: if group_path.is_empty() {
                        ".".to_string()
                    } else {
                        group_path.to_string()
                    },
                    catalog: catalog.to_string(),
                    catalog_dir: catalog_dir.to_string(),
                    template_intent,
                    provides_namespace,
                });
            }
            xcassets::Node::Group(group) => {
                let folder_name = group.name.clone();
                let child_group_path = if group_path.is_empty() {
                    folder_name.clone()
                } else {
                    format!("{group_path}/{folder_name}")
                };

                let is_namespaced = group
                    .contents
                    .as_ref()
                    .and_then(|c| c.properties.provides_namespace)
                    .unwrap_or(false);

                collect_image_sets(
                    &group.children,
                    catalog,
                    catalog_dir,
                    &child_group_path,
                    is_namespaced || parent_namespaced,
                    records,
                );
            }
            // Skip ColorSet, AppIconSet, Opaque
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Save / Load
// ---------------------------------------------------------------------------

fn save_snapshot(store_dir: &Path, snapshot: &AssetSnapshot) -> anyhow::Result<()> {
    std::fs::create_dir_all(store_dir)
        .with_context(|| format!("failed to create store dir {}", store_dir.display()))?;
    let path = snapshot_path(store_dir);
    let payload = serde_json::to_string_pretty(snapshot)?;
    std::fs::write(&path, payload)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn load_snapshot(store_dir: &Path) -> anyhow::Result<AssetSnapshot> {
    let path = snapshot_path(store_dir);
    if !path.exists() {
        bail!("no asset index found — run `grapha index` first");
    }

    let payload = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let snapshot: AssetSnapshot = serde_json::from_str(&payload)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    if snapshot.version != ASSET_SNAPSHOT_VERSION {
        bail!(
            "unsupported asset snapshot version: {} (expected {})",
            snapshot.version,
            ASSET_SNAPSHOT_VERSION
        );
    }
    Ok(snapshot)
}

fn snapshot_path(store_dir: &Path) -> PathBuf {
    store_dir.join(ASSET_SNAPSHOT_FILE)
}

// ---------------------------------------------------------------------------
// Index loading
// ---------------------------------------------------------------------------

pub fn load_asset_index(project_root: &Path) -> anyhow::Result<AssetCatalogIndex> {
    load_asset_index_from_store(&project_root.join(".grapha"))
}

pub fn load_asset_index_from_store(store_dir: &Path) -> anyhow::Result<AssetCatalogIndex> {
    let snapshot = load_snapshot(store_dir)?;
    Ok(AssetCatalogIndex::from_records(snapshot.records))
}

pub(crate) fn snapshot_exists(store_dir: &Path) -> bool {
    snapshot_path(store_dir).exists()
}

// ---------------------------------------------------------------------------
// Query: find usages in graph
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct AssetUsage {
    pub asset_name: String,
    pub node_id: String,
    pub node_name: String,
    pub file: String,
}

pub fn find_usages(graph: &Graph, asset_name: &str) -> Vec<AssetUsage> {
    graph
        .nodes
        .iter()
        .filter(|node| {
            node.metadata.contains_key(META_ASSET_REF_KIND)
                && node
                    .metadata
                    .get(META_ASSET_NAME)
                    .is_some_and(|n| n == asset_name)
        })
        .map(|node| AssetUsage {
            asset_name: asset_name.to_string(),
            node_id: node.id.clone(),
            node_name: node.name.clone(),
            file: node.file.to_string_lossy().to_string(),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Query: find unused assets
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct UnusedAsset {
    pub name: String,
    pub catalog: String,
    pub catalog_dir: String,
    pub group_path: String,
}

pub fn find_unused(index: &AssetCatalogIndex, graph: &Graph) -> Vec<UnusedAsset> {
    let referenced_names: std::collections::HashSet<&str> = graph
        .nodes
        .iter()
        .filter_map(|node| {
            if node.metadata.contains_key(META_ASSET_REF_KIND) {
                node.metadata.get(META_ASSET_NAME).map(|s| s.as_str())
            } else {
                None
            }
        })
        .collect();

    index
        .all_records()
        .iter()
        .filter(|record| !referenced_names.contains(record.name.as_str()))
        .map(|record| UnusedAsset {
            name: record.name.clone(),
            catalog: record.catalog.clone(),
            catalog_dir: record.catalog_dir.clone(),
            group_path: record.group_path.clone(),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn path_relative_string(root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(root).unwrap_or(path);
    let value = relative.to_string_lossy();
    if value.is_empty() {
        ".".to_string()
    } else {
        value.to_string()
    }
}

fn sort_records(records: &mut [AssetRecord]) {
    records.sort_by(|left, right| {
        left.catalog
            .cmp(&right.catalog)
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.group_path.cmp(&right.group_path))
    });
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn builds_and_loads_asset_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let store_dir = dir.path().join(".grapha");
        let assets_dir = dir.path().join("Assets.xcassets");
        fs::create_dir_all(&assets_dir).unwrap();

        // Create a minimal Contents.json at catalog root
        fs::write(
            assets_dir.join("Contents.json"),
            r#"{ "info": { "author": "xcode", "version": 1 } }"#,
        )
        .unwrap();

        // Create an image set
        let image_dir = assets_dir.join("icon_gift.imageset");
        fs::create_dir_all(&image_dir).unwrap();
        fs::write(
            image_dir.join("Contents.json"),
            r#"{
              "images": [
                { "idiom": "universal", "filename": "icon_gift.png", "scale": "1x" }
              ],
              "info": { "author": "xcode", "version": 1 }
            }"#,
        )
        .unwrap();
        // Create the referenced image file so xcassets doesn't emit a warning
        fs::write(image_dir.join("icon_gift.png"), b"fake-png").unwrap();

        let stats = build_and_save_snapshot(dir.path(), &store_dir).unwrap();
        assert_eq!(stats.record_count, 1);
        assert!(stats.warnings.is_empty());
        assert!(store_dir.join("assets.json").exists());

        let index = load_asset_index_from_store(&store_dir).unwrap();
        let records = index.records_for_name("icon_gift");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].catalog, "Assets");
    }

    #[test]
    fn rejects_unsupported_asset_snapshot_version() {
        let dir = tempfile::tempdir().unwrap();
        let store_dir = dir.path().join(".grapha");
        fs::create_dir_all(&store_dir).unwrap();
        fs::write(
            store_dir.join("assets.json"),
            r#"{"version":"999","records":[]}"#,
        )
        .unwrap();

        let error = load_asset_index_from_store(&store_dir).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unsupported asset snapshot version"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn discover_xcassets_finds_catalogs() {
        let dir = tempfile::tempdir().unwrap();
        let assets_dir = dir.path().join("Resources/Assets.xcassets");
        fs::create_dir_all(&assets_dir).unwrap();
        fs::write(
            assets_dir.join("Contents.json"),
            r#"{ "info": { "author": "xcode", "version": 1 } }"#,
        )
        .unwrap();

        let dirs = discover_xcassets_dirs(dir.path());
        assert_eq!(dirs.len(), 1);
        assert!(dirs[0].ends_with("Assets.xcassets"));
    }

    #[test]
    fn find_unused_reports_unreferenced_assets() {
        let records = vec![
            AssetRecord {
                name: "used_icon".to_string(),
                group_path: ".".to_string(),
                catalog: "Assets".to_string(),
                catalog_dir: ".".to_string(),
                template_intent: None,
                provides_namespace: None,
            },
            AssetRecord {
                name: "unused_icon".to_string(),
                group_path: ".".to_string(),
                catalog: "Assets".to_string(),
                catalog_dir: ".".to_string(),
                template_intent: None,
                provides_namespace: None,
            },
        ];
        let index = AssetCatalogIndex::from_records(records);

        let mut graph = Graph::default();
        let mut node = grapha_core::graph::Node {
            id: "test_node".to_string(),
            kind: grapha_core::graph::NodeKind::Function,
            name: "someView".to_string(),
            file: PathBuf::from("test.swift"),
            span: grapha_core::graph::Span {
                start: [0, 0],
                end: [0, 0],
            },
            visibility: grapha_core::graph::Visibility::Crate,
            metadata: HashMap::new(),
            role: None,
            signature: None,
            doc_comment: None,
            module: None,
            snippet: None,
            repo: None,
        };
        node.metadata
            .insert(META_ASSET_REF_KIND.to_string(), "image".to_string());
        node.metadata
            .insert(META_ASSET_NAME.to_string(), "used_icon".to_string());
        graph.nodes.push(node);

        let unused = find_unused(&index, &graph);
        assert_eq!(unused.len(), 1);
        assert_eq!(unused[0].name, "unused_icon");
    }
}
