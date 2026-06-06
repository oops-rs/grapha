use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, bail};
use grapha_core::graph::{Edge, EdgeKind, Graph, Node};
use langcodec::Codec;
use langcodec::types::Translation;
use serde::{Deserialize, Serialize};

const META_REF_KIND: &str = "l10n.ref_kind";
const META_WRAPPER_NAME: &str = "l10n.wrapper_name";
const META_WRAPPER_BASE: &str = "l10n.wrapper_base";
const META_WRAPPER_SYMBOL: &str = "l10n.wrapper_symbol";
const META_TABLE: &str = "l10n.table";
const META_KEY: &str = "l10n.key";
const META_FALLBACK: &str = "l10n.fallback";
const META_ARG_COUNT: &str = "l10n.arg_count";
const META_LITERAL: &str = "l10n.literal";
const META_WRAPPER_TABLE: &str = "l10n.wrapper.table";
const META_WRAPPER_KEY: &str = "l10n.wrapper.key";
const META_WRAPPER_FALLBACK: &str = "l10n.wrapper.fallback";
const META_WRAPPER_ARG_COUNT: &str = "l10n.wrapper.arg_count";
const LOCALIZATION_SNAPSHOT_VERSION: &str = "1";
const LOCALIZATION_SNAPSHOT_FILE: &str = "localization.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalizationCatalogRecord {
    pub table: String,
    pub key: String,
    pub catalog_file: String,
    pub catalog_dir: String,
    pub source_language: String,
    pub source_value: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub translations: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LocalizationSnapshot {
    version: String,
    records: Vec<LocalizationCatalogRecord>,
}

impl LocalizationSnapshot {
    fn new(mut records: Vec<LocalizationCatalogRecord>) -> Self {
        sort_records(&mut records);
        Self {
            version: LOCALIZATION_SNAPSHOT_VERSION.to_string(),
            records,
        }
    }

    fn record_count(&self) -> usize {
        self.records.len()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalizationSnapshotWarning {
    pub catalog_file: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalizationSnapshotBuildStats {
    pub record_count: usize,
    pub warnings: Vec<LocalizationSnapshotWarning>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalizationReference {
    pub ref_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wrapper_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wrapper_base: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wrapper_symbol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arg_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub literal: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalizationWrapperBinding {
    pub table: String,
    pub key: String,
    pub fallback: Option<String>,
    pub arg_count: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLocalizationMatch {
    pub reference: LocalizationReference,
    pub record: LocalizationCatalogRecord,
    pub match_kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnmatchedLocalizationReference {
    pub reference: LocalizationReference,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageResolution {
    pub matches: Vec<ResolvedLocalizationMatch>,
    pub unmatched: Option<UnmatchedLocalizationReference>,
}

#[derive(Debug, Default, Clone)]
pub struct LocalizationCatalogIndex {
    records: Vec<LocalizationCatalogRecord>,
    by_table_key: HashMap<(String, String), Vec<usize>>,
    by_key: HashMap<String, Vec<usize>>,
    by_value: HashMap<String, Vec<usize>>,
}

impl LocalizationCatalogIndex {
    pub(crate) fn insert(&mut self, record: LocalizationCatalogRecord) {
        let index = self.records.len();
        self.by_table_key
            .entry((record.table.clone(), record.key.clone()))
            .or_default()
            .push(index);
        self.by_key
            .entry(record.key.clone())
            .or_default()
            .push(index);
        if !record.source_value.is_empty() {
            self.by_value
                .entry(record.source_value.clone())
                .or_default()
                .push(index);
        }
        for value in record.translations.values() {
            if !value.is_empty() {
                self.by_value.entry(value.clone()).or_default().push(index);
            }
        }
        self.records.push(record);
    }

    pub(crate) fn from_records(mut records: Vec<LocalizationCatalogRecord>) -> Self {
        sort_records(&mut records);
        let mut index = Self::default();
        for record in records {
            index.insert(record);
        }
        index
    }

    pub fn records_for(&self, table: &str, key: &str) -> Vec<LocalizationCatalogRecord> {
        self.by_table_key
            .get(&(table.to_string(), key.to_string()))
            .into_iter()
            .flatten()
            .filter_map(|index| self.records.get(*index))
            .cloned()
            .collect()
    }

    pub fn records_for_key(&self, key: &str) -> Vec<LocalizationCatalogRecord> {
        self.by_key
            .get(key)
            .into_iter()
            .flatten()
            .filter_map(|index| self.records.get(*index))
            .cloned()
            .collect()
    }

    pub fn records_for_value(&self, value: &str) -> Vec<LocalizationCatalogRecord> {
        self.by_value
            .get(value)
            .into_iter()
            .flatten()
            .filter_map(|index| self.records.get(*index))
            .cloned()
            .collect()
    }

    pub fn all_records(&self) -> &[LocalizationCatalogRecord] {
        &self.records
    }
}

pub fn build_and_save_catalog_snapshot(
    root: &Path,
    store_dir: &Path,
) -> anyhow::Result<LocalizationSnapshotBuildStats> {
    let (snapshot, warnings) = build_catalog_snapshot(root)?;
    let count = snapshot.record_count();
    save_catalog_snapshot(store_dir, &snapshot)?;
    Ok(LocalizationSnapshotBuildStats {
        record_count: count,
        warnings,
    })
}

pub fn load_catalog_index(project_root: &Path) -> anyhow::Result<LocalizationCatalogIndex> {
    load_catalog_index_from_store(&project_root.join(".grapha"))
}

pub fn load_catalog_index_from_store(store_dir: &Path) -> anyhow::Result<LocalizationCatalogIndex> {
    let snapshot = load_catalog_snapshot(store_dir)?;
    Ok(LocalizationCatalogIndex::from_records(snapshot.records))
}

pub(crate) fn snapshot_exists(store_dir: &Path) -> bool {
    catalog_snapshot_path(store_dir).exists()
}

fn build_catalog_snapshot(
    root: &Path,
) -> anyhow::Result<(LocalizationSnapshot, Vec<LocalizationSnapshotWarning>)> {
    if root.is_file() {
        return Ok((LocalizationSnapshot::new(Vec::new()), Vec::new()));
    }

    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let files = grapha_core::discover::discover_files(
        &root,
        &["xcstrings".to_string(), "strings".to_string()],
    )?;
    let mut records = Vec::new();
    let mut warnings = Vec::new();
    for catalog in snapshot_catalog_inputs(&files) {
        let mut codec = Codec::new();
        let language_hint = strings_language_hint(&catalog.path);
        if let Err(error) = codec
            .read_file_by_extension(&catalog.path, language_hint)
            .with_context(|| {
                format!(
                    "failed to read {} catalog {}",
                    catalog.format.label(),
                    catalog.path.display()
                )
            })
        {
            warnings.push(LocalizationSnapshotWarning {
                catalog_file: path_to_snapshot_string(&path_relative_to_root(&root, &catalog.path)),
                reason: error.to_string(),
            });
            continue;
        }

        let Some(source_resource) = source_resource_for_codec(&codec) else {
            continue;
        };
        let source_language = source_resource.metadata.language.clone();
        let table = catalog
            .path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("Localizable")
            .to_string();
        let catalog_file = path_relative_to_root(&root, &catalog.path);
        let catalog_dir = path_to_snapshot_string(&path_relative_to_root(&root, &catalog.base_dir));

        let mut translations_by_key: HashMap<String, BTreeMap<String, String>> = HashMap::new();
        for resource in &codec.resources {
            let lang = &resource.metadata.language;
            if lang == &source_language {
                continue;
            }
            for entry in &resource.entries {
                let value = translation_plain_string(&entry.value);
                if !value.is_empty() {
                    translations_by_key
                        .entry(entry.id.clone())
                        .or_default()
                        .insert(lang.clone(), value);
                }
            }
        }

        for entry in &source_resource.entries {
            records.push(LocalizationCatalogRecord {
                table: table.clone(),
                key: entry.id.clone(),
                catalog_file: path_to_snapshot_string(&catalog_file),
                catalog_dir: catalog_dir.clone(),
                source_language: source_language.clone(),
                source_value: translation_plain_string(&entry.value),
                status: serde_json::to_string(&entry.status)
                    .unwrap_or_else(|_| "\"unknown\"".to_string())
                    .trim_matches('"')
                    .to_string(),
                comment: entry.comment.clone(),
                translations: translations_by_key.remove(&entry.id).unwrap_or_default(),
            });
        }
    }

    Ok((LocalizationSnapshot::new(records), warnings))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SnapshotCatalogFormat {
    Xcstrings,
    Strings,
}

impl SnapshotCatalogFormat {
    fn label(self) -> &'static str {
        match self {
            Self::Xcstrings => "xcstrings",
            Self::Strings => "strings",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SnapshotCatalogInput {
    path: PathBuf,
    base_dir: PathBuf,
    format: SnapshotCatalogFormat,
}

fn snapshot_catalog_inputs(files: &[PathBuf]) -> Vec<SnapshotCatalogInput> {
    let mut inputs: Vec<SnapshotCatalogInput> = files
        .iter()
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("xcstrings"))
        .map(|path| SnapshotCatalogInput {
            path: path.clone(),
            base_dir: path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf(),
            format: SnapshotCatalogFormat::Xcstrings,
        })
        .collect();

    let mut strings_groups: BTreeMap<(PathBuf, String), Vec<PathBuf>> = BTreeMap::new();
    for path in files
        .iter()
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("strings"))
    {
        let base_dir = strings_catalog_base_dir(path);
        let table = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("Localizable")
            .to_string();
        strings_groups
            .entry((base_dir, table))
            .or_default()
            .push(path.clone());
    }

    for ((base_dir, _table), candidates) in strings_groups {
        let Some(path) = preferred_strings_catalog_path(&candidates) else {
            continue;
        };
        inputs.push(SnapshotCatalogInput {
            path,
            base_dir,
            format: SnapshotCatalogFormat::Strings,
        });
    }

    inputs.sort_by(|left, right| left.path.cmp(&right.path));
    inputs
}

fn strings_catalog_base_dir(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    if parent
        .file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.ends_with(".lproj"))
    {
        parent
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf()
    } else {
        parent.to_path_buf()
    }
}

fn preferred_strings_catalog_path(candidates: &[PathBuf]) -> Option<PathBuf> {
    let mut ranked = candidates.to_vec();
    ranked.sort_by(|left, right| {
        strings_catalog_preference(left)
            .cmp(&strings_catalog_preference(right))
            .then_with(|| left.cmp(right))
    });
    ranked.into_iter().next()
}

fn strings_catalog_preference(path: &Path) -> (u8, String) {
    let language = strings_language_hint(path).map(|value| value.to_ascii_lowercase());
    let rank = match language.as_deref() {
        Some("base") => 0,
        Some("en") => 1,
        Some(value) if value.starts_with("en-") => 2,
        Some(_) => 3,
        None => 4,
    };

    (rank, language.unwrap_or_default())
}

fn strings_language_hint(path: &Path) -> Option<String> {
    if path.extension().and_then(|value| value.to_str()) != Some("strings") {
        return None;
    }

    path.parent()
        .and_then(Path::file_name)
        .and_then(|value| value.to_str())
        .and_then(|value| value.strip_suffix(".lproj"))
        .map(ToOwned::to_owned)
}

fn save_catalog_snapshot(store_dir: &Path, snapshot: &LocalizationSnapshot) -> anyhow::Result<()> {
    std::fs::create_dir_all(store_dir)
        .with_context(|| format!("failed to create store dir {}", store_dir.display()))?;
    let path = catalog_snapshot_path(store_dir);
    let payload = serde_json::to_string_pretty(snapshot)?;
    std::fs::write(&path, payload)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn load_catalog_snapshot(store_dir: &Path) -> anyhow::Result<LocalizationSnapshot> {
    let path = catalog_snapshot_path(store_dir);
    if !path.exists() {
        bail!("no localization index found — run `grapha index` first");
    }

    let payload = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let snapshot: LocalizationSnapshot = serde_json::from_str(&payload)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    if snapshot.version != LOCALIZATION_SNAPSHOT_VERSION {
        bail!(
            "unsupported localization snapshot version: {} (expected {})",
            snapshot.version,
            LOCALIZATION_SNAPSHOT_VERSION
        );
    }

    Ok(snapshot)
}

fn catalog_snapshot_path(store_dir: &Path) -> PathBuf {
    store_dir.join(LOCALIZATION_SNAPSHOT_FILE)
}

fn source_resource_for_codec(codec: &Codec) -> Option<&langcodec::types::Resource> {
    let source_language = codec
        .resources
        .iter()
        .find_map(|resource| resource.metadata.custom.get("source_language").cloned())
        .unwrap_or_else(|| "en".to_string());

    codec
        .resources
        .iter()
        .find(|resource| resource.metadata.language == source_language)
        .or_else(|| {
            codec
                .resources
                .iter()
                .find(|resource| resource.has_language(&source_language))
        })
        .or_else(|| codec.resources.first())
}

fn translation_plain_string(value: &Translation) -> String {
    value.plain_translation_string()
}

fn path_relative_to_root(root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(root).unwrap_or(path).to_path_buf()
}

fn path_to_snapshot_string(path: &Path) -> String {
    let value = path.to_string_lossy();
    if value.is_empty() {
        ".".to_string()
    } else {
        value.to_string()
    }
}

fn sort_records(records: &mut [LocalizationCatalogRecord]) {
    records.sort_by(|left, right| {
        left.table
            .cmp(&right.table)
            .then_with(|| left.key.cmp(&right.key))
            .then_with(|| left.catalog_file.cmp(&right.catalog_file))
            .then_with(|| left.source_language.cmp(&right.source_language))
    });
}

fn closest_records(
    usage_file: &Path,
    candidates: Vec<LocalizationCatalogRecord>,
) -> Vec<LocalizationCatalogRecord> {
    if candidates.is_empty() {
        return Vec::new();
    }

    let mut ranked: Vec<(usize, LocalizationCatalogRecord)> = candidates
        .into_iter()
        .map(|record| (directory_distance(usage_file, &record.catalog_dir), record))
        .collect();
    ranked.sort_by(
        |(left_distance, left_record), (right_distance, right_record)| {
            left_distance
                .cmp(right_distance)
                .then_with(|| left_record.catalog_file.cmp(&right_record.catalog_file))
                .then_with(|| left_record.table.cmp(&right_record.table))
                .then_with(|| left_record.key.cmp(&right_record.key))
        },
    );

    let Some(best_distance) = ranked.first().map(|(distance, _)| *distance) else {
        return Vec::new();
    };

    ranked
        .into_iter()
        .take_while(|(distance, _)| *distance == best_distance)
        .map(|(_, record)| record)
        .collect()
}

pub fn directory_distance(usage_file: &Path, catalog_dir: &str) -> usize {
    let usage_dir = usage_file.parent().unwrap_or_else(|| Path::new("."));
    let usage_components = normalized_components(usage_dir);
    let catalog_components = normalized_components(Path::new(catalog_dir));

    let common_prefix = usage_components
        .iter()
        .zip(&catalog_components)
        .take_while(|(left, right)| left == right)
        .count();

    (usage_components.len() - common_prefix) + (catalog_components.len() - common_prefix)
}

fn normalized_components(path: &Path) -> Vec<OsString> {
    path.components()
        .filter_map(|component| match component {
            Component::CurDir => None,
            Component::Normal(value) => Some(value.to_os_string()),
            Component::ParentDir => Some(OsString::from("..")),
            Component::RootDir => Some(OsString::from("/")),
            Component::Prefix(prefix) => Some(prefix.as_os_str().to_os_string()),
        })
        .collect()
}

pub fn localization_usage_nodes(graph: &Graph) -> Vec<&Node> {
    graph
        .nodes
        .iter()
        .filter(|node| node.metadata.contains_key(META_REF_KIND))
        .collect()
}

pub fn parse_usage_reference(node: &Node) -> Option<LocalizationReference> {
    let ref_kind = node.metadata.get(META_REF_KIND)?.clone();
    Some(LocalizationReference {
        ref_kind,
        wrapper_name: node.metadata.get(META_WRAPPER_NAME).cloned(),
        wrapper_base: node.metadata.get(META_WRAPPER_BASE).cloned(),
        wrapper_symbol: node.metadata.get(META_WRAPPER_SYMBOL).cloned(),
        table: node.metadata.get(META_TABLE).cloned(),
        key: node.metadata.get(META_KEY).cloned(),
        fallback: node.metadata.get(META_FALLBACK).cloned(),
        arg_count: node
            .metadata
            .get(META_ARG_COUNT)
            .and_then(|value| value.parse::<usize>().ok()),
        literal: node.metadata.get(META_LITERAL).cloned(),
    })
}

pub fn parse_wrapper_binding(node: &Node) -> Option<LocalizationWrapperBinding> {
    Some(LocalizationWrapperBinding {
        table: node.metadata.get(META_WRAPPER_TABLE)?.clone(),
        key: node.metadata.get(META_WRAPPER_KEY)?.clone(),
        fallback: node.metadata.get(META_WRAPPER_FALLBACK).cloned(),
        arg_count: node
            .metadata
            .get(META_WRAPPER_ARG_COUNT)
            .and_then(|value| value.parse::<usize>().ok()),
    })
}

pub fn edges_by_source(graph: &Graph) -> HashMap<&str, Vec<&Edge>> {
    let mut map: HashMap<&str, Vec<&Edge>> = HashMap::new();
    for edge in &graph.edges {
        map.entry(edge.source.as_str()).or_default().push(edge);
    }
    map
}

pub fn node_index(graph: &Graph) -> HashMap<&str, &Node> {
    graph
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect()
}

/// Pre-filter nodes that carry wrapper bindings (l10n.wrapper.table + l10n.wrapper.key).
/// Build once, pass to [`resolve_usage_with`] for batch resolution.
pub fn wrapper_binding_nodes<'a>(node_index: &HashMap<&str, &'a Node>) -> Vec<&'a Node> {
    node_index
        .values()
        .copied()
        .filter(|node| parse_wrapper_binding(node).is_some())
        .collect()
}

/// Resolve a single usage node. Convenient but re-scans all nodes for wrapper candidates
/// on every call. For batch resolution, use [`resolve_usage_with`] with pre-built
/// [`wrapper_binding_nodes`].
#[allow(dead_code)]
pub fn resolve_usage(
    usage_node: &Node,
    edges_by_source: &HashMap<&str, Vec<&Edge>>,
    node_index: &HashMap<&str, &Node>,
    catalogs: &LocalizationCatalogIndex,
) -> Option<UsageResolution> {
    let wrapper_nodes = wrapper_binding_nodes(node_index);
    resolve_usage_with(
        usage_node,
        edges_by_source,
        node_index,
        &wrapper_nodes,
        catalogs,
    )
}

/// Resolve a usage node using pre-built wrapper nodes for efficient batch resolution.
pub fn resolve_usage_with(
    usage_node: &Node,
    edges_by_source: &HashMap<&str, Vec<&Edge>>,
    node_index: &HashMap<&str, &Node>,
    wrapper_nodes: &[&Node],
    catalogs: &LocalizationCatalogIndex,
) -> Option<UsageResolution> {
    let base_reference = parse_usage_reference(usage_node)?;
    let mut matches = Vec::new();
    let mut seen = HashSet::new();

    if let (Some(table), Some(key)) = (
        base_reference.table.as_deref(),
        base_reference.key.as_deref(),
    ) {
        for record in closest_records(&usage_node.file, catalogs.records_for(table, key)) {
            let dedupe_key = (
                String::new(),
                record.catalog_file.clone(),
                record.table.clone(),
                record.key.clone(),
            );
            if seen.insert(dedupe_key) {
                matches.push(ResolvedLocalizationMatch {
                    reference: base_reference.clone(),
                    record,
                    match_kind: "direct_metadata".to_string(),
                });
            }
        }
    }

    // Literal-as-key: in SwiftUI, Text("Foo") treats the literal as a LocalizedStringKey
    if matches.is_empty()
        && let Some(literal) = base_reference.literal.as_deref()
    {
        let literal_records = if let Some(table) = base_reference.table.as_deref() {
            catalogs.records_for(table, literal)
        } else {
            catalogs.records_for_key(literal)
        };
        for record in closest_records(&usage_node.file, literal_records) {
            let mut reference = base_reference.clone();
            reference.table = Some(record.table.clone());
            reference.key = Some(record.key.clone());
            let dedupe_key = (
                String::new(),
                record.catalog_file.clone(),
                record.table.clone(),
                record.key.clone(),
            );
            if seen.insert(dedupe_key) {
                matches.push(ResolvedLocalizationMatch {
                    reference,
                    record,
                    match_kind: "literal_key".to_string(),
                });
            }
        }
    }

    if let Some(edges) = edges_by_source.get(usage_node.id.as_str()) {
        for edge in edges {
            if edge.kind != EdgeKind::TypeRef {
                continue;
            }
            let Some(wrapper_node) = node_index.get(edge.target.as_str()).copied() else {
                continue;
            };
            let Some(binding) = parse_wrapper_binding(wrapper_node) else {
                continue;
            };

            for record in closest_records(
                &usage_node.file,
                catalogs.records_for(&binding.table, &binding.key),
            ) {
                let mut reference = base_reference.clone();
                reference.wrapper_symbol = Some(wrapper_node.id.clone());
                reference.table = Some(binding.table.clone());
                reference.key = Some(binding.key.clone());
                if reference.fallback.is_none() {
                    reference.fallback = binding.fallback.clone();
                }
                if reference.arg_count.is_none() {
                    reference.arg_count = binding.arg_count;
                }

                let dedupe_key = (
                    wrapper_node.id.clone(),
                    record.catalog_file.clone(),
                    record.table.clone(),
                    record.key.clone(),
                );
                if seen.insert(dedupe_key) {
                    matches.push(ResolvedLocalizationMatch {
                        reference,
                        record,
                        match_kind: "wrapper_symbol".to_string(),
                    });
                }
            }
        }
    }

    if matches.is_empty()
        && let Some(wrapper_name) = base_reference.wrapper_name.as_deref()
    {
        for wrapper_node in candidate_wrapper_nodes(
            wrapper_nodes,
            wrapper_name,
            base_reference.wrapper_base.as_deref(),
        ) {
            let Some(binding) = parse_wrapper_binding(wrapper_node) else {
                continue;
            };

            for record in closest_records(
                &usage_node.file,
                catalogs.records_for(&binding.table, &binding.key),
            ) {
                let mut reference = base_reference.clone();
                reference.wrapper_symbol = Some(wrapper_node.id.clone());
                reference.table = Some(binding.table.clone());
                reference.key = Some(binding.key.clone());
                if reference.fallback.is_none() {
                    reference.fallback = binding.fallback.clone();
                }
                if reference.arg_count.is_none() {
                    reference.arg_count = binding.arg_count;
                }

                let dedupe_key = (
                    wrapper_node.id.clone(),
                    record.catalog_file.clone(),
                    record.table.clone(),
                    record.key.clone(),
                );
                if seen.insert(dedupe_key) {
                    matches.push(ResolvedLocalizationMatch {
                        reference,
                        record,
                        match_kind: wrapper_name_match_kind(
                            wrapper_node.name.as_str(),
                            wrapper_name,
                        ),
                    });
                }
            }
        }
    }

    if matches.is_empty()
        && let Some(wrapper_name) = base_reference.wrapper_name.as_deref()
    {
        for record in closest_records(
            &usage_node.file,
            records_for_wrapper_name(catalogs, wrapper_name, base_reference.table.as_deref()),
        ) {
            let mut reference = base_reference.clone();
            reference.table = Some(record.table.clone());
            reference.key = Some(record.key.clone());
            let dedupe_key = (
                format!("wrapper_name_key:{wrapper_name}"),
                record.catalog_file.clone(),
                record.table.clone(),
                record.key.clone(),
            );
            if seen.insert(dedupe_key) {
                matches.push(ResolvedLocalizationMatch {
                    reference,
                    record,
                    match_kind: "wrapper_name_key".to_string(),
                });
            }
        }
    }

    let unmatched = if matches.is_empty() {
        Some(UnmatchedLocalizationReference {
            reference: base_reference.clone(),
            reason: unmatched_reason(&base_reference, edges_by_source, usage_node, node_index),
        })
    } else {
        None
    };

    Some(UsageResolution { matches, unmatched })
}

fn wrapper_node_matches_base(node: &Node, wrapper_base: Option<&str>) -> bool {
    let Some(wrapper_base) = wrapper_base else {
        return true;
    };

    node.id.contains(&format!("::{wrapper_base}::"))
        || node.id.contains(&format!("::ext_{wrapper_base}::"))
        || node.id.contains(wrapper_base)
}

fn candidate_wrapper_nodes<'a>(
    wrapper_nodes: &[&'a Node],
    wrapper_name: &str,
    wrapper_base: Option<&str>,
) -> Vec<&'a Node> {
    let mut exact = Vec::new();
    let mut normalized = Vec::new();
    let mut approximate = Vec::new();
    for &wrapper_node in wrapper_nodes
        .iter()
        .filter(|node| wrapper_node_matches_base(node, wrapper_base))
    {
        if wrapper_node.name == wrapper_name {
            exact.push(wrapper_node);
        } else if wrapper_names_token_equivalent(wrapper_node.name.as_str(), wrapper_name) {
            normalized.push(wrapper_node);
        } else if approximate_wrapper_score(wrapper_node.name.as_str(), wrapper_name).is_some() {
            approximate.push(wrapper_node);
        }
    }

    if !exact.is_empty() {
        exact
    } else if !normalized.is_empty() {
        normalized
    } else {
        best_approximate_wrapper_nodes(approximate, wrapper_name)
    }
}

fn wrapper_name_match_kind(candidate_name: &str, requested_name: &str) -> String {
    if candidate_name == requested_name {
        "wrapper_name".to_string()
    } else if wrapper_names_token_equivalent(candidate_name, requested_name) {
        "wrapper_name_tokens".to_string()
    } else {
        "wrapper_name_approximate".to_string()
    }
}

fn wrapper_names_token_equivalent(left: &str, right: &str) -> bool {
    let left_tokens = localization_name_tokens(left);
    let right_tokens = localization_name_tokens(right);
    !left_tokens.is_empty() && left_tokens == right_tokens
}

pub(crate) fn normalize_wrapper_name_to_key(name: &str) -> String {
    let mut normalized = String::new();
    let mut previous: Option<char> = None;

    for ch in name.chars() {
        if ch == '_' || ch == '-' {
            if !normalized.ends_with('_') && !normalized.is_empty() {
                normalized.push('_');
            }
            previous = Some(ch);
            continue;
        }

        let starts_new_token = previous.is_some_and(|prev| {
            (prev.is_ascii_lowercase() && ch.is_ascii_uppercase())
                || (prev.is_ascii_alphabetic() && ch.is_ascii_digit())
                || (prev.is_ascii_digit() && ch.is_ascii_alphabetic())
        });
        if starts_new_token && !normalized.ends_with('_') && !normalized.is_empty() {
            normalized.push('_');
        }

        normalized.push(ch.to_ascii_lowercase());
        previous = Some(ch);
    }

    normalized.trim_matches('_').to_string()
}

pub(crate) fn wrapper_name_matches_catalog_key(wrapper_name: &str, key: &str) -> bool {
    normalize_wrapper_name_to_key(wrapper_name) == key.to_ascii_lowercase()
        || wrapper_names_token_equivalent(wrapper_name, key)
}

fn localization_name_tokens(name: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut previous: Option<char> = None;

    for ch in name.chars() {
        if ch == '_' || ch == '-' {
            if !current.is_empty() {
                tokens.push(current.to_ascii_lowercase());
                current.clear();
            }
            previous = None;
            continue;
        }

        let starts_new_token = previous.is_some_and(|prev| {
            (prev.is_ascii_lowercase() && ch.is_ascii_uppercase())
                || (prev.is_ascii_alphabetic() && ch.is_ascii_digit())
                || (prev.is_ascii_digit() && ch.is_ascii_alphabetic())
        });
        if starts_new_token && !current.is_empty() {
            tokens.push(current.to_ascii_lowercase());
            current.clear();
        }

        current.push(ch);
        previous = Some(ch);
    }

    if !current.is_empty() {
        tokens.push(current.to_ascii_lowercase());
    }

    tokens.sort();
    tokens
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ApproximateWrapperScore {
    shared_tokens: usize,
    common_prefix: usize,
    edit_distance: usize,
}

impl Ord for ApproximateWrapperScore {
    fn cmp(&self, other: &Self) -> Ordering {
        self.shared_tokens
            .cmp(&other.shared_tokens)
            .then_with(|| self.common_prefix.cmp(&other.common_prefix))
            .then_with(|| other.edit_distance.cmp(&self.edit_distance))
    }
}

impl PartialOrd for ApproximateWrapperScore {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn best_approximate_wrapper_nodes<'a>(
    candidates: Vec<&'a Node>,
    requested_name: &str,
) -> Vec<&'a Node> {
    let mut scored = Vec::new();
    for node in candidates {
        let Some(score) = approximate_wrapper_score(node.name.as_str(), requested_name) else {
            continue;
        };
        scored.push((node, score));
    }

    scored.sort_by(|(left_node, left_score), (right_node, right_score)| {
        right_score
            .cmp(left_score)
            .then_with(|| left_node.id.cmp(&right_node.id))
    });

    let Some((_, best_score)) = scored.first().copied() else {
        return Vec::new();
    };

    let top: Vec<_> = scored
        .into_iter()
        .take_while(|(_, score)| *score == best_score)
        .map(|(node, _)| node)
        .collect();

    if top.len() == 1 { top } else { Vec::new() }
}

fn approximate_wrapper_score(
    candidate_name: &str,
    requested_name: &str,
) -> Option<ApproximateWrapperScore> {
    let candidate_lower = candidate_name.to_ascii_lowercase();
    let requested_lower = requested_name.to_ascii_lowercase();
    let candidate_tokens = localization_name_tokens(candidate_name);
    let requested_tokens = localization_name_tokens(requested_name);
    let shared_tokens = candidate_tokens
        .iter()
        .filter(|token| requested_tokens.iter().any(|candidate| candidate == *token))
        .count();
    let common_prefix = common_prefix_len(&candidate_lower, &requested_lower);
    let edit_distance = levenshtein_distance(&candidate_lower, &requested_lower);
    let max_len = candidate_lower.len().max(requested_lower.len());

    if shared_tokens < 2 || common_prefix < 4 || edit_distance > max_len / 2 {
        return None;
    }

    Some(ApproximateWrapperScore {
        shared_tokens,
        common_prefix,
        edit_distance,
    })
}

fn records_for_wrapper_name(
    catalogs: &LocalizationCatalogIndex,
    wrapper_name: &str,
    table: Option<&str>,
) -> Vec<LocalizationCatalogRecord> {
    let normalized_key = normalize_wrapper_name_to_key(wrapper_name);
    let direct = if let Some(table) = table {
        catalogs.records_for(table, &normalized_key)
    } else {
        catalogs.records_for_key(&normalized_key)
    };
    if !direct.is_empty() {
        return direct;
    }

    catalogs
        .all_records()
        .iter()
        .filter(|record| table.is_none_or(|table| record.table == table))
        .filter(|record| wrapper_name_matches_catalog_key(wrapper_name, &record.key))
        .cloned()
        .collect()
}

fn common_prefix_len(left: &str, right: &str) -> usize {
    left.chars()
        .zip(right.chars())
        .take_while(|(left, right)| left == right)
        .count()
}

fn levenshtein_distance(left: &str, right: &str) -> usize {
    let left_chars: Vec<char> = left.chars().collect();
    let right_chars: Vec<char> = right.chars().collect();
    let mut previous: Vec<usize> = (0..=right_chars.len()).collect();
    let mut current = vec![0usize; right_chars.len() + 1];

    for (left_index, left_char) in left_chars.iter().enumerate() {
        current[0] = left_index + 1;
        for (right_index, right_char) in right_chars.iter().enumerate() {
            let substitution_cost = usize::from(left_char != right_char);
            current[right_index + 1] = (previous[right_index + 1] + 1)
                .min(current[right_index] + 1)
                .min(previous[right_index] + substitution_cost);
        }
        std::mem::swap(&mut previous, &mut current);
    }

    previous[right_chars.len()]
}

fn unmatched_reason(
    reference: &LocalizationReference,
    edges_by_source: &HashMap<&str, Vec<&Edge>>,
    usage_node: &Node,
    node_index: &HashMap<&str, &Node>,
) -> String {
    if reference.ref_kind == "literal" {
        return "literal text has no stable catalog key".to_string();
    }
    if reference.ref_kind == "possible_wrapper" {
        return "L10nResource-backed text may be localized but no stable key was resolved"
            .to_string();
    }
    if reference.ref_kind == "possible_string" {
        return "string-backed text may be localized but no stable key was resolved".to_string();
    }

    if let Some(edges) = edges_by_source.get(usage_node.id.as_str()) {
        let has_wrapper_target = edges.iter().any(|edge| {
            edge.kind == EdgeKind::TypeRef
                && node_index
                    .get(edge.target.as_str())
                    .is_some_and(|node| parse_wrapper_binding(node).is_some())
        });
        if has_wrapper_target {
            return "catalog record not found".to_string();
        }
    }

    "no wrapper symbol resolved".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapha_core::graph::{NodeKind, Span, Visibility};
    use std::collections::HashMap;
    use std::fs;

    #[test]
    fn builds_and_loads_localization_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let store_dir = dir.path().join(".grapha");
        let file = dir.path().join("Localizable.xcstrings");
        fs::write(
            &file,
            r#"{
              "sourceLanguage" : "en",
              "strings" : {
                "welcome_title" : {
                  "comment" : "Shown on the welcome screen",
                  "localizations" : {
                    "en" : {
                      "stringUnit" : {
                        "state" : "translated",
                        "value" : "Welcome"
                      }
                    }
                  }
                }
              },
              "version" : "1.0"
            }"#,
        )
        .unwrap();

        let stats = build_and_save_catalog_snapshot(dir.path(), &store_dir).unwrap();
        assert_eq!(stats.record_count, 1);
        assert!(stats.warnings.is_empty());
        assert!(store_dir.join("localization.json").exists());

        let index = load_catalog_index_from_store(&store_dir).unwrap();
        let records = index.records_for("Localizable", "welcome_title");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].source_value, "Welcome");
        assert_eq!(records[0].status, "translated");
        assert_eq!(records[0].catalog_file, "Localizable.xcstrings");
        assert_eq!(records[0].catalog_dir, ".");
        assert_eq!(
            records[0].comment.as_deref(),
            Some("Shown on the welcome screen")
        );
    }

    #[test]
    fn lookup_by_source_value() {
        let index = LocalizationCatalogIndex::from_records(vec![LocalizationCatalogRecord {
            table: "Localizable".to_string(),
            key: "entrance_effect".to_string(),
            catalog_file: "Localizable.xcstrings".to_string(),
            catalog_dir: ".".to_string(),
            source_language: "zh-Hans".to_string(),
            source_value: "进场特效".to_string(),
            status: "translated".to_string(),
            comment: None,
            translations: BTreeMap::new(),
        }]);

        assert!(index.records_for_key("进场特效").is_empty());

        let records = index.records_for_value("进场特效");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].key, "entrance_effect");
    }

    #[test]
    fn lookup_by_translation_value() {
        let mut translations = BTreeMap::new();
        translations.insert("zh-Hans".to_string(), "欢迎".to_string());
        translations.insert("fr".to_string(), "Bienvenue".to_string());

        let index = LocalizationCatalogIndex::from_records(vec![LocalizationCatalogRecord {
            table: "Localizable".to_string(),
            key: "welcome_title".to_string(),
            catalog_file: "Localizable.xcstrings".to_string(),
            catalog_dir: ".".to_string(),
            source_language: "en".to_string(),
            source_value: "Welcome".to_string(),
            status: "translated".to_string(),
            comment: None,
            translations,
        }]);

        assert!(index.records_for_key("欢迎").is_empty());

        let records = index.records_for_value("欢迎");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].key, "welcome_title");

        let records = index.records_for_value("Bienvenue");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].key, "welcome_title");

        let records = index.records_for_value("Welcome");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].key, "welcome_title");
    }

    #[test]
    fn snapshot_stores_non_source_translations() {
        let dir = tempfile::tempdir().unwrap();
        let store_dir = dir.path().join(".grapha");
        let file = dir.path().join("Localizable.xcstrings");
        fs::write(
            &file,
            r#"{
              "sourceLanguage" : "en",
              "strings" : {
                "welcome_title" : {
                  "localizations" : {
                    "en" : {
                      "stringUnit" : {
                        "state" : "translated",
                        "value" : "Welcome"
                      }
                    },
                    "zh-Hans" : {
                      "stringUnit" : {
                        "state" : "translated",
                        "value" : "欢迎"
                      }
                    }
                  }
                }
              },
              "version" : "1.0"
            }"#,
        )
        .unwrap();

        build_and_save_catalog_snapshot(dir.path(), &store_dir).unwrap();
        let index = load_catalog_index_from_store(&store_dir).unwrap();

        let records = index.records_for_key("welcome_title");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].source_value, "Welcome");
        assert_eq!(
            records[0].translations.get("zh-Hans").map(String::as_str),
            Some("欢迎")
        );

        let by_value = index.records_for_value("欢迎");
        assert_eq!(by_value.len(), 1);
        assert_eq!(by_value[0].key, "welcome_title");
    }

    #[test]
    fn builds_and_loads_strings_catalogs() {
        let dir = tempfile::tempdir().unwrap();
        let store_dir = dir.path().join(".grapha");
        let en_dir = dir.path().join("en.lproj");
        fs::create_dir_all(&en_dir).unwrap();
        fs::write(
            en_dir.join("Localizable.strings"),
            r#""welcome_title" = "Welcome";
"farewell_title" = "Bye";"#,
        )
        .unwrap();

        let stats = build_and_save_catalog_snapshot(dir.path(), &store_dir).unwrap();
        assert_eq!(stats.record_count, 2);
        assert!(stats.warnings.is_empty());

        let index = load_catalog_index_from_store(&store_dir).unwrap();
        let welcome_records = index.records_for("Localizable", "welcome_title");
        assert_eq!(welcome_records.len(), 1);
        assert_eq!(welcome_records[0].source_language, "en");
        assert_eq!(welcome_records[0].source_value, "Welcome");
        assert_eq!(
            welcome_records[0].catalog_file,
            "en.lproj/Localizable.strings"
        );
        assert_eq!(welcome_records[0].catalog_dir, ".");
    }

    #[test]
    fn rejects_unsupported_snapshot_version() {
        let dir = tempfile::tempdir().unwrap();
        let store_dir = dir.path().join(".grapha");
        fs::create_dir_all(&store_dir).unwrap();
        fs::write(
            store_dir.join("localization.json"),
            r#"{"version":"999","records":[]}"#,
        )
        .unwrap();

        let error = load_catalog_index_from_store(&store_dir).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unsupported localization snapshot version"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn skips_invalid_xcstrings_catalogs_and_keeps_valid_ones() {
        let dir = tempfile::tempdir().unwrap();
        let store_dir = dir.path().join(".grapha");
        fs::write(
            dir.path().join("Localizable.xcstrings"),
            r#"{
              "sourceLanguage" : "en",
              "strings" : {
                "welcome_title" : {
                  "localizations" : {
                    "en" : {
                      "stringUnit" : {
                        "state" : "translated",
                        "value" : "Welcome"
                      }
                    }
                  }
                }
              },
              "version" : "1.0"
            }"#,
        )
        .unwrap();
        fs::write(
            dir.path().join("Broken.xcstrings"),
            r#"{
              "sourceLanguage" : "en",
              "strings" : {
                "broken" : {},
              },
              "version" : "1.0"
            }"#,
        )
        .unwrap();

        let stats = build_and_save_catalog_snapshot(dir.path(), &store_dir).unwrap();
        assert_eq!(stats.record_count, 1);
        assert_eq!(stats.warnings.len(), 1);
        assert_eq!(stats.warnings[0].catalog_file, "Broken.xcstrings");
        assert!(
            stats.warnings[0]
                .reason
                .contains("failed to read xcstrings catalog"),
            "unexpected warning: {}",
            stats.warnings[0].reason
        );

        let index = load_catalog_index_from_store(&store_dir).unwrap();
        let records = index.records_for("Localizable", "welcome_title");
        assert_eq!(records.len(), 1);
    }

    #[test]
    fn builds_snapshot_from_strings_catalogs_using_preferred_source_locale() {
        let dir = tempfile::tempdir().unwrap();
        let store_dir = dir.path().join(".grapha");
        let en_dir = dir.path().join("Feature/Resources/en.lproj");
        let fr_dir = dir.path().join("Feature/Resources/fr.lproj");
        fs::create_dir_all(&en_dir).unwrap();
        fs::create_dir_all(&fr_dir).unwrap();
        fs::write(
            en_dir.join("Localizable.strings"),
            r#""welcome_title" = "Welcome";"#,
        )
        .unwrap();
        fs::write(
            fr_dir.join("Localizable.strings"),
            r#""welcome_title" = "Bienvenue";"#,
        )
        .unwrap();

        let stats = build_and_save_catalog_snapshot(dir.path(), &store_dir).unwrap();
        assert_eq!(stats.record_count, 1);
        assert!(stats.warnings.is_empty());

        let index = load_catalog_index_from_store(&store_dir).unwrap();
        let records = index.records_for("Localizable", "welcome_title");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].source_language, "en");
        assert_eq!(records[0].source_value, "Welcome");
        assert_eq!(
            records[0].catalog_file,
            "Feature/Resources/en.lproj/Localizable.strings"
        );
        assert_eq!(records[0].catalog_dir, "Feature/Resources");
    }

    #[test]
    fn snapshot_catalog_inputs_group_strings_by_catalog_root() {
        let inputs = snapshot_catalog_inputs(&[
            PathBuf::from("Feature/Resources/en.lproj/Localizable.strings"),
            PathBuf::from("Feature/Resources/fr.lproj/Localizable.strings"),
            PathBuf::from("Shared/Localizable.xcstrings"),
        ]);

        assert_eq!(inputs.len(), 2);
        assert_eq!(
            inputs[0].path,
            PathBuf::from("Feature/Resources/en.lproj/Localizable.strings")
        );
        assert_eq!(inputs[0].base_dir, PathBuf::from("Feature/Resources"));
        assert_eq!(
            inputs[1].path,
            PathBuf::from("Shared/Localizable.xcstrings")
        );
        assert_eq!(inputs[1].base_dir, PathBuf::from("Shared"));
    }

    #[test]
    fn closest_records_prefers_nearest_catalog() {
        let candidates = vec![
            LocalizationCatalogRecord {
                table: "Localizable".to_string(),
                key: "shared_title".to_string(),
                catalog_file: "Features/Auth/Localizable.xcstrings".to_string(),
                catalog_dir: "Features/Auth".to_string(),
                source_language: "en".to_string(),
                source_value: "Auth".to_string(),
                status: "translated".to_string(),
                comment: None,
                translations: BTreeMap::new(),
            },
            LocalizationCatalogRecord {
                table: "Localizable".to_string(),
                key: "shared_title".to_string(),
                catalog_file: "Features/Profile/Localizable.xcstrings".to_string(),
                catalog_dir: "Features/Profile".to_string(),
                source_language: "en".to_string(),
                source_value: "Profile".to_string(),
                status: "translated".to_string(),
                comment: None,
                translations: BTreeMap::new(),
            },
        ];

        let matches = closest_records(
            Path::new("Features/Auth/Sources/Login/ContentView.swift"),
            candidates,
        );
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].source_value, "Auth");
    }

    #[test]
    fn closest_records_keep_equal_distance_ties() {
        let candidates = vec![
            LocalizationCatalogRecord {
                table: "Localizable".to_string(),
                key: "shared_title".to_string(),
                catalog_file: "Features/A/Localizable.xcstrings".to_string(),
                catalog_dir: "Features/A".to_string(),
                source_language: "en".to_string(),
                source_value: "A".to_string(),
                status: "translated".to_string(),
                comment: None,
                translations: BTreeMap::new(),
            },
            LocalizationCatalogRecord {
                table: "Localizable".to_string(),
                key: "shared_title".to_string(),
                catalog_file: "Features/B/Localizable.xcstrings".to_string(),
                catalog_dir: "Features/B".to_string(),
                source_language: "en".to_string(),
                source_value: "B".to_string(),
                status: "translated".to_string(),
                comment: None,
                translations: BTreeMap::new(),
            },
        ];

        let matches = closest_records(Path::new("Features/Common/ContentView.swift"), candidates);
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].catalog_file, "Features/A/Localizable.xcstrings");
        assert_eq!(matches[1].catalog_file, "Features/B/Localizable.xcstrings");
    }

    #[test]
    fn resolve_usage_falls_back_to_wrapper_name_across_files() {
        let mut usage = Node {
            id: "Features/Share/ShareView.swift::ShareView::titleView::view:Text@1:1:1:20"
                .to_string(),
            kind: NodeKind::View,
            name: "Text".to_string(),
            file: PathBuf::from("Features/Share/ShareView.swift"),
            span: Span {
                start: [1, 1],
                end: [1, 20],
            },
            visibility: Visibility::Private,
            metadata: HashMap::new(),
            role: None,
            signature: None,
            doc_comment: None,
            module: Some("Share".to_string()),
            snippet: None,
            repo: None,
        };
        usage
            .metadata
            .insert(META_REF_KIND.to_string(), "wrapper".to_string());
        usage
            .metadata
            .insert(META_WRAPPER_NAME.to_string(), "welcomeTitle".to_string());
        usage
            .metadata
            .insert(META_WRAPPER_BASE.to_string(), "L10nResource".to_string());

        let mut l10n_wrapper = Node {
            id: "AppUI/Sources/AppResource/Generated/Strings.generated.swift::L10n::welcomeTitle"
                .to_string(),
            kind: NodeKind::Property,
            name: "welcomeTitle".to_string(),
            file: PathBuf::from("AppUI/Sources/AppResource/Generated/Strings.generated.swift"),
            span: Span {
                start: [1, 1],
                end: [1, 2],
            },
            visibility: Visibility::Public,
            metadata: HashMap::new(),
            role: None,
            signature: None,
            doc_comment: None,
            module: Some("AppUI".to_string()),
            snippet: None,
            repo: None,
        };
        l10n_wrapper
            .metadata
            .insert(META_WRAPPER_TABLE.to_string(), "Localizable".to_string());
        l10n_wrapper.metadata.insert(
            META_WRAPPER_KEY.to_string(),
            "welcome_title_wrong".to_string(),
        );

        let mut resource_wrapper = Node {
            id: "AppUI/Sources/AppResource/Generated/Strings.generated.swift::ext_L10nResource::welcomeTitle"
                .to_string(),
            kind: NodeKind::Property,
            name: "welcomeTitle".to_string(),
            file: PathBuf::from("AppUI/Sources/AppResource/Generated/Strings.generated.swift"),
            span: Span {
                start: [2, 1],
                end: [2, 2],
            },
            visibility: Visibility::Public,
            metadata: HashMap::new(),
            role: None,
            signature: None,
            doc_comment: None,
            module: Some("AppUI".to_string()),
            snippet: None,
            repo: None,
        };
        resource_wrapper
            .metadata
            .insert(META_WRAPPER_TABLE.to_string(), "Localizable".to_string());
        resource_wrapper
            .metadata
            .insert(META_WRAPPER_KEY.to_string(), "welcome_title".to_string());

        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![usage.clone(), l10n_wrapper, resource_wrapper.clone()],
            edges: Vec::new(),
        };
        let catalogs = LocalizationCatalogIndex::from_records(vec![LocalizationCatalogRecord {
            table: "Localizable".to_string(),
            key: "welcome_title".to_string(),
            catalog_file: "AppUI/Localizable.xcstrings".to_string(),
            catalog_dir: "AppUI".to_string(),
            source_language: "en".to_string(),
            source_value: "Welcome".to_string(),
            status: "translated".to_string(),
            comment: None,
            translations: BTreeMap::new(),
        }]);

        let resolution = resolve_usage(
            &usage,
            &edges_by_source(&graph),
            &node_index(&graph),
            &catalogs,
        )
        .expect("usage should resolve");

        assert_eq!(resolution.matches.len(), 1);
        assert_eq!(
            resolution.matches[0].reference.wrapper_symbol.as_deref(),
            Some(resource_wrapper.id.as_str())
        );
        assert_eq!(resolution.matches[0].match_kind, "wrapper_name");
        assert!(resolution.unmatched.is_none());
    }

    #[test]
    fn resolve_usage_falls_back_to_wrapper_name_for_usr_wrapper_ids() {
        let mut usage = Node {
            id: "Modules/Room/Sources/Room/View/RoomPage+Layout.swift::RoomPageHeaderView::onShare::l10n:shareText"
                .to_string(),
            kind: NodeKind::Property,
            name: "shareText".to_string(),
            file: PathBuf::from("RoomPage+Layout.swift"),
            span: Span {
                start: [265, 12],
                end: [269, 13],
            },
            visibility: Visibility::Private,
            metadata: HashMap::new(),
            role: None,
            signature: None,
            doc_comment: None,
            module: Some("Room".to_string()),
            snippet: None,
            repo: None,
        };
        usage
            .metadata
            .insert(META_REF_KIND.to_string(), "wrapper".to_string());
        usage
            .metadata
            .insert(META_WRAPPER_NAME.to_string(), "roomShareDesc".to_string());
        usage
            .metadata
            .insert(META_WRAPPER_BASE.to_string(), "L10n".to_string());

        let mut l10n_wrapper = Node {
            id: "s:11AppResource4L10nO13roomShareDescSSvpZ".to_string(),
            kind: NodeKind::Property,
            name: "roomShareDesc".to_string(),
            file: PathBuf::from("Strings.generated.swift"),
            span: Span {
                start: [1, 1],
                end: [1, 2],
            },
            visibility: Visibility::Public,
            metadata: HashMap::new(),
            role: None,
            signature: None,
            doc_comment: None,
            module: Some("AppUI".to_string()),
            snippet: None,
            repo: None,
        };
        l10n_wrapper
            .metadata
            .insert(META_WRAPPER_TABLE.to_string(), "Localizable".to_string());
        l10n_wrapper
            .metadata
            .insert(META_WRAPPER_KEY.to_string(), "room_share_desc".to_string());

        let mut resource_wrapper = Node {
            id: "s:14FrameResources12L10nResourceV03AppD0E13roomShareDescACvpZ".to_string(),
            kind: NodeKind::Property,
            name: "roomShareDesc".to_string(),
            file: PathBuf::from("Strings.generated.swift"),
            span: Span {
                start: [2, 1],
                end: [2, 2],
            },
            visibility: Visibility::Public,
            metadata: HashMap::new(),
            role: None,
            signature: None,
            doc_comment: None,
            module: Some("AppUI".to_string()),
            snippet: None,
            repo: None,
        };
        resource_wrapper
            .metadata
            .insert(META_WRAPPER_TABLE.to_string(), "Localizable".to_string());
        resource_wrapper.metadata.insert(
            META_WRAPPER_KEY.to_string(),
            "room_share_desc_resource".to_string(),
        );

        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![usage.clone(), l10n_wrapper.clone(), resource_wrapper],
            edges: Vec::new(),
        };
        let catalogs = LocalizationCatalogIndex::from_records(vec![LocalizationCatalogRecord {
            table: "Localizable".to_string(),
            key: "room_share_desc".to_string(),
            catalog_file: "Modules/Room/Resources/Localizable.xcstrings".to_string(),
            catalog_dir: "Modules/Room/Resources".to_string(),
            source_language: "en".to_string(),
            source_value: "Share room".to_string(),
            status: "translated".to_string(),
            comment: None,
            translations: BTreeMap::new(),
        }]);

        let resolution = resolve_usage(
            &usage,
            &edges_by_source(&graph),
            &node_index(&graph),
            &catalogs,
        )
        .expect("usage should resolve");

        assert_eq!(resolution.matches.len(), 1);
        assert_eq!(
            resolution.matches[0].reference.wrapper_symbol.as_deref(),
            Some(l10n_wrapper.id.as_str())
        );
        assert_eq!(resolution.matches[0].match_kind, "wrapper_name");
        assert!(resolution.unmatched.is_none());
    }

    #[test]
    fn resolve_usage_falls_back_to_approximate_wrapper_name() {
        let mut usage = Node {
            id: "Features/Share/ShareView.swift::ShareView::emptyState::view:Text@1:1:1:20"
                .to_string(),
            kind: NodeKind::View,
            name: "Text".to_string(),
            file: PathBuf::from("Features/Share/ShareView.swift"),
            span: Span {
                start: [1, 1],
                end: [1, 20],
            },
            visibility: Visibility::Private,
            metadata: HashMap::new(),
            role: None,
            signature: None,
            doc_comment: None,
            module: Some("Share".to_string()),
            snippet: None,
            repo: None,
        };
        usage
            .metadata
            .insert(META_REF_KIND.to_string(), "wrapper".to_string());
        usage.metadata.insert(
            META_WRAPPER_NAME.to_string(),
            "commonuiSearchListEmpty".to_string(),
        );
        usage
            .metadata
            .insert(META_WRAPPER_BASE.to_string(), "L10nResource".to_string());

        let mut search_empty_wrapper = Node {
            id: "AppUI/Sources/AppResource/Generated/Strings.generated.swift::ext_L10nResource::commonuiSearchEmpty"
                .to_string(),
            kind: NodeKind::Property,
            name: "commonuiSearchEmpty".to_string(),
            file: PathBuf::from("AppUI/Sources/AppResource/Generated/Strings.generated.swift"),
            span: Span {
                start: [2, 1],
                end: [2, 2],
            },
            visibility: Visibility::Public,
            metadata: HashMap::new(),
            role: None,
            signature: None,
            doc_comment: None,
            module: Some("AppUI".to_string()),
            snippet: None,
            repo: None,
        };
        search_empty_wrapper
            .metadata
            .insert(META_WRAPPER_TABLE.to_string(), "Localizable".to_string());
        search_empty_wrapper.metadata.insert(
            META_WRAPPER_KEY.to_string(),
            "commonui_search_empty".to_string(),
        );

        let mut list_empty_wrapper = Node {
            id: "AppUI/Sources/AppResource/Generated/Strings.generated.swift::ext_L10nResource::commonuiListEmpty"
                .to_string(),
            kind: NodeKind::Property,
            name: "commonuiListEmpty".to_string(),
            file: PathBuf::from("AppUI/Sources/AppResource/Generated/Strings.generated.swift"),
            span: Span {
                start: [3, 1],
                end: [3, 2],
            },
            visibility: Visibility::Public,
            metadata: HashMap::new(),
            role: None,
            signature: None,
            doc_comment: None,
            module: Some("AppUI".to_string()),
            snippet: None,
            repo: None,
        };
        list_empty_wrapper
            .metadata
            .insert(META_WRAPPER_TABLE.to_string(), "Localizable".to_string());
        list_empty_wrapper.metadata.insert(
            META_WRAPPER_KEY.to_string(),
            "commonui_list_empty".to_string(),
        );

        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                usage.clone(),
                search_empty_wrapper.clone(),
                list_empty_wrapper,
            ],
            edges: Vec::new(),
        };
        let catalogs = LocalizationCatalogIndex::from_records(vec![
            LocalizationCatalogRecord {
                table: "Localizable".to_string(),
                key: "commonui_search_empty".to_string(),
                catalog_file: "AppUI/Localizable.strings".to_string(),
                catalog_dir: "AppUI".to_string(),
                source_language: "en".to_string(),
                source_value: "The ID you entered does not exist".to_string(),
                status: "translated".to_string(),
                comment: None,
                translations: BTreeMap::new(),
            },
            LocalizationCatalogRecord {
                table: "Localizable".to_string(),
                key: "commonui_list_empty".to_string(),
                catalog_file: "AppUI/Localizable.strings".to_string(),
                catalog_dir: "AppUI".to_string(),
                source_language: "en".to_string(),
                source_value: "List is empty".to_string(),
                status: "translated".to_string(),
                comment: None,
                translations: BTreeMap::new(),
            },
        ]);

        let resolution = resolve_usage(
            &usage,
            &edges_by_source(&graph),
            &node_index(&graph),
            &catalogs,
        )
        .expect("usage should resolve");

        assert_eq!(resolution.matches.len(), 1);
        assert_eq!(
            resolution.matches[0].reference.wrapper_symbol.as_deref(),
            Some(search_empty_wrapper.id.as_str())
        );
        assert_eq!(resolution.matches[0].match_kind, "wrapper_name_approximate");
        assert_eq!(resolution.matches[0].record.key, "commonui_search_empty");
    }

    #[test]
    fn resolve_usage_matches_literal_as_catalog_key() {
        let mut usage = Node {
            id:
                "Features/Tournament/TournamentView.swift::TournamentView::body::view:Text@5:8:5:30"
                    .to_string(),
            kind: NodeKind::View,
            name: "Text".to_string(),
            file: PathBuf::from("Features/Tournament/TournamentView.swift"),
            span: Span {
                start: [5, 8],
                end: [5, 30],
            },
            visibility: Visibility::Private,
            metadata: HashMap::new(),
            role: None,
            signature: None,
            doc_comment: None,
            module: Some("Tournament".to_string()),
            snippet: None,
            repo: None,
        };
        usage
            .metadata
            .insert(META_REF_KIND.to_string(), "literal".to_string());
        usage
            .metadata
            .insert(META_LITERAL.to_string(), "Tournament".to_string());

        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![usage.clone()],
            edges: Vec::new(),
        };
        let catalogs = LocalizationCatalogIndex::from_records(vec![LocalizationCatalogRecord {
            table: "Localizable".to_string(),
            key: "Tournament".to_string(),
            catalog_file: "Features/Tournament/Localizable.xcstrings".to_string(),
            catalog_dir: "Features/Tournament".to_string(),
            source_language: "en".to_string(),
            source_value: "Tournament".to_string(),
            status: "translated".to_string(),
            comment: None,
            translations: BTreeMap::new(),
        }]);

        let resolution = resolve_usage(
            &usage,
            &edges_by_source(&graph),
            &node_index(&graph),
            &catalogs,
        )
        .expect("usage should resolve");

        assert_eq!(resolution.matches.len(), 1);
        assert_eq!(resolution.matches[0].record.key, "Tournament");
        assert_eq!(resolution.matches[0].record.table, "Localizable");
        assert_eq!(resolution.matches[0].match_kind, "literal_key");
        assert_eq!(
            resolution.matches[0].reference.key.as_deref(),
            Some("Tournament")
        );
        assert!(resolution.unmatched.is_none());
    }

    #[test]
    fn resolve_usage_matches_wrapper_name_against_catalog_key_without_wrapper_symbol() {
        let mut usage = Node {
            id: "Store/BuySuccessView.swift::body::view:Text@10:8:10:30".to_string(),
            kind: NodeKind::View,
            name: "Text".to_string(),
            file: PathBuf::from("Modules/Store/Sources/Store/View/Dailog/BuySuccessView.swift"),
            span: Span {
                start: [10, 8],
                end: [10, 30],
            },
            visibility: Visibility::Private,
            metadata: HashMap::new(),
            role: None,
            signature: None,
            doc_comment: None,
            module: Some("Store".to_string()),
            snippet: None,
            repo: None,
        };
        usage
            .metadata
            .insert(META_REF_KIND.to_string(), "wrapper".to_string());
        usage
            .metadata
            .insert(META_WRAPPER_NAME.to_string(), "storeUseNow".to_string());
        usage
            .metadata
            .insert(META_WRAPPER_BASE.to_string(), "L10nResource".to_string());

        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![usage.clone()],
            edges: Vec::new(),
        };
        let catalogs = LocalizationCatalogIndex::from_records(vec![LocalizationCatalogRecord {
            table: "Localizable".to_string(),
            key: "store_use_now".to_string(),
            catalog_file: "AppUI/Sources/AppResource/Resources/Localizable.xcstrings".to_string(),
            catalog_dir: "AppUI/Sources/AppResource/Resources".to_string(),
            source_language: "en".to_string(),
            source_value: "Use now".to_string(),
            status: "translated".to_string(),
            comment: None,
            translations: BTreeMap::new(),
        }]);

        let resolution = resolve_usage(
            &usage,
            &edges_by_source(&graph),
            &node_index(&graph),
            &catalogs,
        )
        .expect("usage should resolve");

        assert_eq!(resolution.matches.len(), 1);
        assert_eq!(resolution.matches[0].record.key, "store_use_now");
        assert_eq!(resolution.matches[0].match_kind, "wrapper_name_key");
        assert_eq!(
            resolution.matches[0].reference.key.as_deref(),
            Some("store_use_now")
        );
        assert!(resolution.unmatched.is_none());
    }

    #[test]
    fn resolve_usage_literal_without_catalog_record_remains_unmatched() {
        let mut usage = Node {
            id: "Features/Home/HomeView.swift::HomeView::body::view:Text@3:8:3:25".to_string(),
            kind: NodeKind::View,
            name: "Text".to_string(),
            file: PathBuf::from("Features/Home/HomeView.swift"),
            span: Span {
                start: [3, 8],
                end: [3, 25],
            },
            visibility: Visibility::Private,
            metadata: HashMap::new(),
            role: None,
            signature: None,
            doc_comment: None,
            module: Some("Home".to_string()),
            snippet: None,
            repo: None,
        };
        usage
            .metadata
            .insert(META_REF_KIND.to_string(), "literal".to_string());
        usage
            .metadata
            .insert(META_LITERAL.to_string(), "Hello World".to_string());

        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![usage.clone()],
            edges: Vec::new(),
        };
        let catalogs = LocalizationCatalogIndex::from_records(Vec::new());

        let resolution = resolve_usage(
            &usage,
            &edges_by_source(&graph),
            &node_index(&graph),
            &catalogs,
        )
        .expect("usage should resolve");

        assert!(resolution.matches.is_empty());
        assert!(resolution.unmatched.is_some());
    }
}
