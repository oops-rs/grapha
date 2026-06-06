use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use grapha_core::graph::{Graph, Node, NodeKind, NodeRole};
use serde::{Deserialize, Serialize};

const INFERRED_SNAPSHOT_VERSION: &str = "1";
const INFERRED_SNAPSHOT_FILE: &str = "inferred.json";
const INFERENCE_SOURCE: &str = "heuristic";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InferredIndex {
    pub version: String,
    #[serde(default)]
    pub records: Vec<InferredRecord>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InferredRecord {
    pub id: String,
    pub kind: InferredRecordKind,
    pub target: InferredTarget,
    pub value: String,
    pub confidence: f64,
    pub source: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<InferredEvidence>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InferredRecordKind {
    ModuleSummary,
    Ownership,
    DocCodeLink,
}

impl InferredRecordKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ModuleSummary => "module_summary",
            Self::Ownership => "ownership",
            Self::DocCodeLink => "doc_code_link",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InferredTarget {
    pub kind: InferredTargetKind,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InferredTargetKind {
    Module,
    File,
    Symbol,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InferredEvidence {
    pub kind: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InferredBuildResult {
    pub enabled: bool,
    pub saved: bool,
    pub store_path: String,
    pub total_records: usize,
    pub by_kind: BTreeMap<String, usize>,
    pub records: Vec<InferredRecord>,
}

#[derive(Debug, Default)]
struct ModuleAccum {
    symbol_count: usize,
    files: BTreeSet<String>,
    entry_points: usize,
    terminals: usize,
}

impl Default for InferredIndex {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl InferredIndex {
    pub fn new(mut records: Vec<InferredRecord>) -> Self {
        sort_records(&mut records);
        Self {
            version: INFERRED_SNAPSHOT_VERSION.to_string(),
            records,
        }
    }
}

pub fn build_inferred_index(graph: &Graph) -> InferredIndex {
    let mut records = Vec::new();
    records.extend(infer_module_summaries(graph));
    records.extend(infer_ownership(graph));
    records.extend(infer_doc_code_links(graph));
    InferredIndex::new(records)
}

pub fn build_result(
    enabled: bool,
    saved: bool,
    store_path: &Path,
    index: &InferredIndex,
) -> InferredBuildResult {
    let mut by_kind = BTreeMap::new();
    for record in &index.records {
        *by_kind.entry(record.kind.as_str().to_string()).or_default() += 1;
    }

    InferredBuildResult {
        enabled,
        saved,
        store_path: store_path.to_string_lossy().to_string(),
        total_records: index.records.len(),
        by_kind,
        records: index.records.clone(),
    }
}

pub fn inferred_store_path(project_root: &Path) -> PathBuf {
    project_root.join(".grapha").join(INFERRED_SNAPSHOT_FILE)
}

pub fn save_inferred_index(project_root: &Path, index: &InferredIndex) -> anyhow::Result<PathBuf> {
    let path = inferred_store_path(project_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(index)?;
    std::fs::write(&path, json).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

pub fn load_inferred_index(project_root: &Path) -> anyhow::Result<InferredIndex> {
    let path = inferred_store_path(project_root);
    if !path.exists() {
        return Ok(InferredIndex::default());
    }

    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let index: InferredIndex = serde_json::from_str(&contents)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    if index.version != INFERRED_SNAPSHOT_VERSION {
        bail!(
            "unsupported inferred snapshot version {} in {}",
            index.version,
            path.display()
        );
    }
    Ok(index)
}

fn infer_module_summaries(graph: &Graph) -> Vec<InferredRecord> {
    let mut modules: BTreeMap<String, ModuleAccum> = BTreeMap::new();

    for node in graph.nodes.iter().filter(|node| !is_synthetic(node)) {
        let module_name = node
            .module
            .clone()
            .unwrap_or_else(|| "(unknown)".to_string());
        let entry = modules.entry(module_name).or_default();
        entry.symbol_count += 1;
        entry.files.insert(path_string(&node.file));
        match &node.role {
            Some(NodeRole::EntryPoint) => entry.entry_points += 1,
            Some(NodeRole::Terminal { .. }) => entry.terminals += 1,
            _ => {}
        }
    }

    modules
        .into_iter()
        .map(|(module, accum)| {
            let confidence = 0.55;
            InferredRecord {
                id: format!("module:{module}:summary"),
                kind: InferredRecordKind::ModuleSummary,
                target: InferredTarget {
                    kind: InferredTargetKind::Module,
                    id: module.clone(),
                    name: Some(module.clone()),
                    file: None,
                    module: Some(module.clone()),
                },
                value: format!(
                    "Module {module}: {} symbols across {} files; {} entry points; {} terminal effects.",
                    accum.symbol_count,
                    accum.files.len(),
                    accum.entry_points,
                    accum.terminals
                ),
                confidence,
                source: INFERENCE_SOURCE.to_string(),
                evidence: vec![
                    evidence("symbol_count", accum.symbol_count),
                    evidence("file_count", accum.files.len()),
                    evidence("entry_points", accum.entry_points),
                    evidence("terminals", accum.terminals),
                ],
                metadata: inferred_metadata(confidence),
            }
        })
        .collect()
}

fn infer_ownership(graph: &Graph) -> Vec<InferredRecord> {
    let mut files: BTreeMap<String, (Option<String>, String, f64)> = BTreeMap::new();

    for node in graph.nodes.iter().filter(|node| !is_synthetic(node)) {
        let file = path_string(&node.file);
        let Some((owner, confidence)) = infer_owner(node) else {
            continue;
        };

        files
            .entry(file)
            .and_modify(|(existing_module, existing_owner, existing_confidence)| {
                if confidence > *existing_confidence {
                    *existing_module = node.module.clone();
                    *existing_owner = owner.clone();
                    *existing_confidence = confidence;
                }
            })
            .or_insert((node.module.clone(), owner, confidence));
    }

    files
        .into_iter()
        .map(|(file, (module, owner, confidence))| InferredRecord {
            id: format!("file:{file}:ownership"),
            kind: InferredRecordKind::Ownership,
            target: InferredTarget {
                kind: InferredTargetKind::File,
                id: file.clone(),
                name: Some(file_name(&file).to_string()),
                file: Some(file.clone()),
                module,
            },
            value: format!("Likely owner: {owner}"),
            confidence,
            source: INFERENCE_SOURCE.to_string(),
            evidence: vec![InferredEvidence {
                kind: "path_or_module".to_string(),
                value: owner,
            }],
            metadata: inferred_metadata(confidence),
        })
        .collect()
}

fn infer_doc_code_links(graph: &Graph) -> Vec<InferredRecord> {
    graph
        .nodes
        .iter()
        .filter(|node| !is_synthetic(node))
        .filter_map(|node| {
            let comment = normalize_doc_comment(node.doc_comment.as_deref()?)?;
            let confidence = 0.7;
            let file = path_string(&node.file);
            Some(InferredRecord {
                id: format!("symbol:{}:doc", node.id),
                kind: InferredRecordKind::DocCodeLink,
                target: InferredTarget {
                    kind: InferredTargetKind::Symbol,
                    id: node.id.clone(),
                    name: Some(node.name.clone()),
                    file: Some(file),
                    module: node.module.clone(),
                },
                value: comment.clone(),
                confidence,
                source: INFERENCE_SOURCE.to_string(),
                evidence: vec![InferredEvidence {
                    kind: "doc_comment".to_string(),
                    value: comment,
                }],
                metadata: inferred_metadata(confidence),
            })
        })
        .collect()
}

fn inferred_metadata(confidence: f64) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("confidence".to_string(), format!("{confidence:.2}")),
        ("inferred".to_string(), "true".to_string()),
        ("source".to_string(), INFERENCE_SOURCE.to_string()),
    ])
}

fn infer_owner(node: &Node) -> Option<(String, f64)> {
    let file = path_string(&node.file);
    if let Some(owner) = owner_from_file(&file) {
        return Some((owner, 0.45));
    }
    node.module
        .as_ref()
        .filter(|module| !module.is_empty() && module.as_str() != "(unknown)")
        .map(|module| (module.clone(), 0.35))
}

fn owner_from_file(file: &str) -> Option<String> {
    let components = file
        .replace('\\', "/")
        .split('/')
        .filter(|component| {
            !component.is_empty()
                && *component != "."
                && *component != "src"
                && !component.ends_with(".rs")
                && !component.ends_with(".swift")
        })
        .map(str::to_string)
        .collect::<Vec<_>>();
    components.last().cloned()
}

fn normalize_doc_comment(comment: &str) -> Option<String> {
    let joined = comment
        .lines()
        .map(|line| {
            line.trim()
                .trim_start_matches("///")
                .trim_start_matches("//!")
                .trim_start_matches("/**")
                .trim_start_matches("/*")
                .trim_start_matches('*')
                .trim_end_matches("*/")
                .trim()
        })
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if joined.is_empty() {
        None
    } else if joined.len() > 180 {
        Some(format!(
            "{}...",
            joined.chars().take(177).collect::<String>()
        ))
    } else {
        Some(joined)
    }
}

fn evidence(kind: &str, value: usize) -> InferredEvidence {
    InferredEvidence {
        kind: kind.to_string(),
        value: value.to_string(),
    }
}

fn is_synthetic(node: &Node) -> bool {
    matches!(node.kind, NodeKind::View | NodeKind::Branch)
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn sort_records(records: &mut [InferredRecord]) {
    records.sort_by(|left, right| {
        (left.kind, &left.target.id, &left.id).cmp(&(right.kind, &right.target.id, &right.id))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapha_core::graph::{Span, Visibility};
    use std::collections::HashMap;

    fn node(id: &str, name: &str, file: &str, module: &str) -> Node {
        Node {
            id: id.to_string(),
            kind: NodeKind::Function,
            name: name.to_string(),
            file: PathBuf::from(file),
            span: Span {
                start: [1, 0],
                end: [2, 0],
            },
            visibility: Visibility::Public,
            metadata: HashMap::new(),
            role: None,
            signature: None,
            doc_comment: None,
            module: Some(module.to_string()),
            snippet: None,
            repo: None,
        }
    }

    #[test]
    fn build_inferred_index_generates_module_owner_and_doc_records() {
        let mut documented = node("app::run", "run", "Features/Gifts/Run.swift", "App");
        documented.role = Some(NodeRole::EntryPoint);
        documented.doc_comment = Some("/// Starts the gift flow.".to_string());
        let mut terminal = node("app::save", "save", "Features/Gifts/Save.swift", "App");
        terminal.role = Some(NodeRole::Terminal {
            kind: grapha_core::graph::TerminalKind::Persistence,
        });
        let graph = Graph {
            version: "test".to_string(),
            nodes: vec![documented, terminal],
            edges: Vec::new(),
        };

        let index = build_inferred_index(&graph);

        assert!(
            index
                .records
                .iter()
                .any(|record| record.kind == InferredRecordKind::ModuleSummary
                    && record.value.contains("2 symbols"))
        );
        assert!(
            index
                .records
                .iter()
                .any(|record| record.kind == InferredRecordKind::Ownership
                    && record.value == "Likely owner: Gifts"
                    && record.confidence == 0.45)
        );
        let doc_record = index
            .records
            .iter()
            .find(|record| record.kind == InferredRecordKind::DocCodeLink)
            .unwrap();
        assert_eq!(doc_record.value, "Starts the gift flow.");
        assert_eq!(
            doc_record.metadata.get("inferred").map(String::as_str),
            Some("true")
        );
    }

    #[test]
    fn inferred_index_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let graph = Graph {
            version: "test".to_string(),
            nodes: vec![node("app::run", "run", "src/main.rs", "App")],
            edges: Vec::new(),
        };
        let index = build_inferred_index(&graph);

        save_inferred_index(dir.path(), &index).unwrap();
        let loaded = load_inferred_index(dir.path()).unwrap();

        assert_eq!(loaded, index);
    }

    #[test]
    fn missing_inferred_index_loads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = load_inferred_index(dir.path()).unwrap();
        assert!(loaded.records.is_empty());
    }
}
