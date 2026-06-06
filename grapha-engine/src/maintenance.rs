use std::collections::{BTreeMap, HashSet};

use grapha_core::graph::{Edge, EdgeKind, EdgeProvenance, Graph, NodeKind, Span};
use serde::Serialize;

use crate::inferred::{InferredIndex, InferredRecord, InferredTargetKind};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MaintenanceReport {
    pub total: usize,
    pub by_severity: BTreeMap<String, usize>,
    pub checks: Vec<MaintenanceCheck>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MaintenanceCheck {
    pub kind: MaintenanceCheckKind,
    pub severity: MaintenanceSeverity,
    pub target: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub details: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MaintenanceCheckKind {
    MissingRelation,
    OrphanEdge,
    OrphanEntity,
    InconsistentProvenance,
    StaleInferredLink,
}

impl MaintenanceCheckKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MissingRelation => "missing_relation",
            Self::OrphanEdge => "orphan_edge",
            Self::OrphanEntity => "orphan_entity",
            Self::InconsistentProvenance => "inconsistent_provenance",
            Self::StaleInferredLink => "stale_inferred_link",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MaintenanceSeverity {
    Error,
    Warning,
    Info,
}

impl MaintenanceSeverity {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info => "info",
        }
    }

    const fn rank(self) -> usize {
        match self {
            Self::Error => 0,
            Self::Warning => 1,
            Self::Info => 2,
        }
    }
}

pub fn run_maintenance_checks(graph: &Graph, inferred: &InferredIndex) -> MaintenanceReport {
    let node_ids = graph
        .nodes
        .iter()
        .map(|node| node.id.as_str())
        .collect::<HashSet<_>>();
    let modules = graph
        .nodes
        .iter()
        .filter_map(|node| node.module.as_deref())
        .collect::<HashSet<_>>();
    let files = graph
        .nodes
        .iter()
        .map(|node| normalize_path(&node.file.to_string_lossy()))
        .collect::<HashSet<_>>();
    let contained_targets = graph
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Contains)
        .map(|edge| edge.target.as_str())
        .collect::<HashSet<_>>();

    let mut checks = Vec::new();
    checks.extend(check_edges(graph, &node_ids));
    checks.extend(check_orphan_entities(graph, &contained_targets));
    checks.extend(check_stale_inferred_links(
        inferred, &node_ids, &modules, &files,
    ));
    sort_checks(&mut checks);
    build_report(checks)
}

fn check_edges<'a>(graph: &'a Graph, node_ids: &HashSet<&'a str>) -> Vec<MaintenanceCheck> {
    let mut checks = Vec::new();
    for edge in &graph.edges {
        let source_exists = node_ids.contains(edge.source.as_str());
        let target_exists = node_ids.contains(edge.target.as_str());
        if !source_exists || !target_exists {
            let kind = if edge.kind == EdgeKind::Contains {
                MaintenanceCheckKind::MissingRelation
            } else {
                MaintenanceCheckKind::OrphanEdge
            };
            checks.push(MaintenanceCheck {
                kind,
                severity: MaintenanceSeverity::Error,
                target: format!("{} -> {}", edge.source, edge.target),
                message: format!(
                    "{:?} edge references {}{}",
                    edge.kind,
                    if source_exists { "" } else { "missing source" },
                    if target_exists {
                        ""
                    } else if source_exists {
                        "missing target"
                    } else {
                        " and missing target"
                    }
                ),
                details: edge_details(edge),
            });
        }

        for provenance in &edge.provenance {
            if let Some(check) = check_provenance(edge, provenance, node_ids) {
                checks.push(check);
            }
        }
    }
    checks
}

fn check_orphan_entities(
    graph: &Graph,
    contained_targets: &HashSet<&str>,
) -> Vec<MaintenanceCheck> {
    graph
        .nodes
        .iter()
        .filter(|node| {
            matches!(
                node.kind,
                NodeKind::Field | NodeKind::Property | NodeKind::Variant
            )
        })
        .filter(|node| !contained_targets.contains(node.id.as_str()))
        .map(|node| MaintenanceCheck {
            kind: MaintenanceCheckKind::OrphanEntity,
            severity: MaintenanceSeverity::Info,
            target: node.id.clone(),
            message: format!("{:?} has no containing symbol relation", node.kind),
            details: BTreeMap::from([
                (
                    "file".to_string(),
                    normalize_path(&node.file.to_string_lossy()),
                ),
                ("name".to_string(), node.name.clone()),
            ]),
        })
        .collect()
}

fn check_provenance(
    edge: &Edge,
    provenance: &EdgeProvenance,
    node_ids: &HashSet<&str>,
) -> Option<MaintenanceCheck> {
    let mut reason = None;
    if provenance.file.as_os_str().is_empty() {
        reason = Some("empty provenance file".to_string());
    } else if span_is_invalid(&provenance.span) {
        reason = Some("invalid provenance span".to_string());
    } else if !provenance.symbol_id.is_empty()
        && !node_ids.contains(provenance.symbol_id.as_str())
        && provenance.symbol_id != edge.source
        && provenance.symbol_id != edge.target
    {
        reason = Some("provenance symbol_id is not present in the graph".to_string());
    }

    reason.map(|message| MaintenanceCheck {
        kind: MaintenanceCheckKind::InconsistentProvenance,
        severity: MaintenanceSeverity::Warning,
        target: format!("{} -> {}", edge.source, edge.target),
        message,
        details: BTreeMap::from([
            (
                "file".to_string(),
                normalize_path(&provenance.file.to_string_lossy()),
            ),
            ("symbol_id".to_string(), provenance.symbol_id.clone()),
        ]),
    })
}

fn check_stale_inferred_links<'a>(
    inferred: &InferredIndex,
    node_ids: &HashSet<&'a str>,
    modules: &HashSet<&'a str>,
    files: &HashSet<String>,
) -> Vec<MaintenanceCheck> {
    inferred
        .records
        .iter()
        .filter(|record| !inferred_target_exists(record, node_ids, modules, files))
        .map(|record| MaintenanceCheck {
            kind: MaintenanceCheckKind::StaleInferredLink,
            severity: MaintenanceSeverity::Warning,
            target: record.target.id.clone(),
            message: format!(
                "inferred {} record points to a missing {} target",
                record.kind.as_str(),
                target_kind_label(record.target.kind)
            ),
            details: BTreeMap::from([
                ("record_id".to_string(), record.id.clone()),
                (
                    "confidence".to_string(),
                    format!("{:.2}", record.confidence),
                ),
            ]),
        })
        .collect()
}

fn inferred_target_exists<'a>(
    record: &InferredRecord,
    node_ids: &HashSet<&'a str>,
    modules: &HashSet<&'a str>,
    files: &HashSet<String>,
) -> bool {
    match record.target.kind {
        InferredTargetKind::Symbol => node_ids.contains(record.target.id.as_str()),
        InferredTargetKind::Module => modules.contains(record.target.id.as_str()),
        InferredTargetKind::File => files.contains(&normalize_path(&record.target.id)),
    }
}

fn build_report(checks: Vec<MaintenanceCheck>) -> MaintenanceReport {
    let mut by_severity = BTreeMap::new();
    for check in &checks {
        *by_severity
            .entry(check.severity.as_str().to_string())
            .or_default() += 1;
    }

    MaintenanceReport {
        total: checks.len(),
        by_severity,
        checks,
    }
}

fn sort_checks(checks: &mut [MaintenanceCheck]) {
    checks.sort_by(|left, right| {
        (
            left.severity.rank(),
            left.kind,
            left.target.as_str(),
            left.message.as_str(),
        )
            .cmp(&(
                right.severity.rank(),
                right.kind,
                right.target.as_str(),
                right.message.as_str(),
            ))
    });
}

fn edge_details(edge: &Edge) -> BTreeMap<String, String> {
    BTreeMap::from([
        (
            "kind".to_string(),
            format!("{:?}", edge.kind).to_lowercase(),
        ),
        ("source".to_string(), edge.source.clone()),
        ("target".to_string(), edge.target.clone()),
    ])
}

fn span_is_invalid(span: &Span) -> bool {
    span.start[0] > span.end[0] || (span.start[0] == span.end[0] && span.start[1] > span.end[1])
}

fn target_kind_label(kind: InferredTargetKind) -> &'static str {
    match kind {
        InferredTargetKind::Module => "module",
        InferredTargetKind::File => "file",
        InferredTargetKind::Symbol => "symbol",
    }
}

fn normalize_path(path: &str) -> String {
    path.replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inferred::{InferredRecord, InferredRecordKind, InferredTarget, InferredTargetKind};
    use grapha_core::graph::{EdgeProvenance, FlowDirection, Node, Visibility};
    use std::path::PathBuf;

    fn node(id: &str, kind: NodeKind, file: &str, module: &str) -> Node {
        Node {
            id: id.to_string(),
            kind,
            name: id.to_string(),
            file: PathBuf::from(file),
            span: Span {
                start: [1, 0],
                end: [1, 1],
            },
            visibility: Visibility::Public,
            metadata: Default::default(),
            role: None,
            signature: None,
            doc_comment: None,
            module: Some(module.to_string()),
            snippet: None,
            repo: None,
        }
    }

    #[test]
    fn detects_missing_relations_orphan_edges_and_bad_provenance() {
        let graph = Graph {
            version: "test".to_string(),
            nodes: vec![
                node("parent", NodeKind::Struct, "src/lib.rs", "App"),
                node("field", NodeKind::Field, "src/lib.rs", "App"),
            ],
            edges: vec![
                Edge {
                    source: "parent".to_string(),
                    target: "missing_child".to_string(),
                    kind: EdgeKind::Contains,
                    confidence: 1.0,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: Vec::new(),
                    repo: None,
                },
                Edge {
                    source: "parent".to_string(),
                    target: "missing_call".to_string(),
                    kind: EdgeKind::Calls,
                    confidence: 0.7,
                    direction: Some(FlowDirection::Pure),
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: vec![EdgeProvenance {
                        file: PathBuf::from("src/lib.rs"),
                        span: Span {
                            start: [5, 4],
                            end: [5, 2],
                        },
                        symbol_id: "parent".to_string(),
                    }],
                    repo: None,
                },
            ],
        };

        let report = run_maintenance_checks(&graph, &InferredIndex::default());

        assert!(
            report
                .checks
                .iter()
                .any(|check| check.kind == MaintenanceCheckKind::MissingRelation)
        );
        assert!(
            report
                .checks
                .iter()
                .any(|check| check.kind == MaintenanceCheckKind::OrphanEdge)
        );
        assert!(
            report
                .checks
                .iter()
                .any(|check| check.kind == MaintenanceCheckKind::InconsistentProvenance)
        );
        assert!(
            report
                .checks
                .iter()
                .any(|check| check.kind == MaintenanceCheckKind::OrphanEntity)
        );
    }

    #[test]
    fn detects_stale_inferred_links() {
        let graph = Graph {
            version: "test".to_string(),
            nodes: vec![node("live", NodeKind::Function, "src/lib.rs", "App")],
            edges: Vec::new(),
        };
        let inferred = InferredIndex::new(vec![
            inferred_record("symbol:missing", InferredTargetKind::Symbol, "missing"),
            inferred_record("module:Other:summary", InferredTargetKind::Module, "Other"),
            inferred_record("file:old.rs:ownership", InferredTargetKind::File, "old.rs"),
        ]);

        let report = run_maintenance_checks(&graph, &inferred);
        let stale_count = report
            .checks
            .iter()
            .filter(|check| check.kind == MaintenanceCheckKind::StaleInferredLink)
            .count();

        assert_eq!(stale_count, 3);
        assert_eq!(report.by_severity.get("warning"), Some(&3));
    }

    fn inferred_record(id: &str, target_kind: InferredTargetKind, target: &str) -> InferredRecord {
        InferredRecord {
            id: id.to_string(),
            kind: InferredRecordKind::DocCodeLink,
            target: InferredTarget {
                kind: target_kind,
                id: target.to_string(),
                name: None,
                file: None,
                module: None,
            },
            value: "stale".to_string(),
            confidence: 0.5,
            source: "heuristic".to_string(),
            evidence: Vec::new(),
            metadata: Default::default(),
        }
    }
}
