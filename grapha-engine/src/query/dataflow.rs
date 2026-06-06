use std::collections::{HashMap, VecDeque};

use serde::Serialize;

use grapha_core::graph::{
    Edge, EdgeKind, EdgeProvenance, FlowDirection, Graph, Node, NodeKind, NodeRole,
};

use super::flow::{is_dataflow_edge, terminal_kind_to_string};
use super::{QueryResolveError, SymbolRef, normalize_symbol_name};

/// Cap for the `nodes` and `edges` vectors returned by
/// [`query_dataflow_with_options`].
///
/// The default leaves results unbounded; the CLI overrides this with the
/// user-supplied `--limit`. `total_nodes` and `total_edges` always reflect the
/// pre-truncation count so callers can detect truncation.
#[derive(Debug, Clone)]
pub struct DataflowQueryOptions {
    pub limit: usize,
}

impl Default for DataflowQueryOptions {
    fn default() -> Self {
        // Internal callers stay unbounded.
        Self { limit: usize::MAX }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DataflowNodeKind {
    Entry,
    Symbol,
    Effect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DataflowEdgeKind {
    Call,
    Read,
    Write,
    Publish,
    Subscribe,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DataflowNode {
    pub id: String,
    pub name: String,
    pub kind: DataflowNodeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effect_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DataflowEdge {
    pub source: String,
    pub target: String,
    pub kind: DataflowEdgeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub async_boundary: Option<bool>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub provenance: Vec<EdgeProvenance>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DataflowSummary {
    pub symbols: usize,
    pub effects: usize,
    pub edges: usize,
    pub calls: usize,
    pub reads: usize,
    pub writes: usize,
    pub publishes: usize,
    pub subscribes: usize,
}

#[derive(Debug, Serialize)]
pub struct DataflowResult {
    pub entry: String,
    pub nodes: Vec<DataflowNode>,
    pub total_nodes: usize,
    pub edges: Vec<DataflowEdge>,
    pub total_edges: usize,
    pub summary: DataflowSummary,
    #[serde(skip)]
    pub entry_ref: SymbolRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct EffectKey {
    kind: String,
    target: String,
    operation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct SemanticEdgeKey {
    source: String,
    target: String,
    kind: DataflowEdgeKind,
    operation: Option<String>,
}

#[derive(Debug, Clone)]
struct SemanticEdgeAccumulator {
    source: String,
    target: String,
    kind: DataflowEdgeKind,
    operation: Option<String>,
    conditions: Vec<String>,
    async_boundary: Option<bool>,
    provenance: Vec<EdgeProvenance>,
}

fn node_ref(node: &Node) -> SymbolRef {
    SymbolRef::from_node(node)
}

fn kind_order(kind: DataflowNodeKind) -> usize {
    match kind {
        DataflowNodeKind::Entry => 0,
        DataflowNodeKind::Symbol => 1,
        DataflowNodeKind::Effect => 2,
    }
}

fn edge_kind_label(kind: EdgeKind) -> &'static str {
    match kind {
        EdgeKind::Calls => "call",
        EdgeKind::Reads => "read",
        EdgeKind::Writes => "write",
        EdgeKind::Publishes => "publish",
        EdgeKind::Subscribes => "subscribe",
        EdgeKind::Uses
        | EdgeKind::Imports
        | EdgeKind::Exports
        | EdgeKind::Implements
        | EdgeKind::Contains
        | EdgeKind::TypeRef
        | EdgeKind::References
        | EdgeKind::TypeOf
        | EdgeKind::Returns
        | EdgeKind::Inherits
        | EdgeKind::Extends
        | EdgeKind::Instantiates
        | EdgeKind::Overrides
        | EdgeKind::Decorates => "effect",
    }
}

fn semantic_edge_kinds(edge: &Edge) -> Vec<DataflowEdgeKind> {
    match edge.direction {
        Some(FlowDirection::Read) => vec![DataflowEdgeKind::Read],
        Some(FlowDirection::Write) => vec![DataflowEdgeKind::Write],
        Some(FlowDirection::ReadWrite) => vec![DataflowEdgeKind::Read, DataflowEdgeKind::Write],
        Some(FlowDirection::Pure) => vec![DataflowEdgeKind::Call],
        None => match edge.kind {
            EdgeKind::Calls => vec![DataflowEdgeKind::Call],
            EdgeKind::Reads => vec![DataflowEdgeKind::Read],
            EdgeKind::Writes => vec![DataflowEdgeKind::Write],
            EdgeKind::Publishes => vec![DataflowEdgeKind::Publish],
            EdgeKind::Subscribes => vec![DataflowEdgeKind::Subscribe],
            EdgeKind::Instantiates => vec![DataflowEdgeKind::Call],
            EdgeKind::Uses
            | EdgeKind::Imports
            | EdgeKind::Exports
            | EdgeKind::Implements
            | EdgeKind::Contains
            | EdgeKind::TypeRef
            | EdgeKind::References
            | EdgeKind::TypeOf
            | EdgeKind::Returns
            | EdgeKind::Inherits
            | EdgeKind::Extends
            | EdgeKind::Overrides
            | EdgeKind::Decorates => Vec::new(),
        },
    }
}

fn is_terminal(node: Option<&Node>) -> bool {
    matches!(
        node.and_then(|node| node.role.as_ref()),
        Some(NodeRole::Terminal { .. })
    )
}

fn terminal_kind_for_effect(source: &Node, target: Option<&Node>) -> Option<String> {
    target
        .and_then(|node| match node.role.as_ref() {
            Some(NodeRole::Terminal { kind }) => Some(kind),
            _ => None,
        })
        .or(match source.role.as_ref() {
            Some(NodeRole::Terminal { kind }) => Some(kind),
            _ => None,
        })
        .map(terminal_kind_to_string)
}

fn normalize_effect_component(value: &str) -> String {
    let normalized = normalize_symbol_name(value)
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    let trimmed = normalized.trim_matches('_');
    if trimmed.is_empty() {
        "unknown".to_string()
    } else {
        trimmed.to_string()
    }
}

fn effect_target_name(edge: &Edge, target: Option<&Node>) -> String {
    target.map(|node| node.name.clone()).unwrap_or_else(|| {
        edge.target
            .rsplit("::")
            .next()
            .unwrap_or(&edge.target)
            .to_string()
    })
}

fn effect_operation(edge: &Edge) -> Option<String> {
    edge.operation
        .as_ref()
        .map(|operation| operation.trim().to_string())
        .filter(|operation| !operation.is_empty())
}

fn effect_key(edge: &Edge, source: &Node, target: Option<&Node>) -> EffectKey {
    let effect_kind = terminal_kind_for_effect(source, target)
        .unwrap_or_else(|| edge_kind_label(edge.kind).to_string());
    let target_name = effect_target_name(edge, target);
    let operation = effect_operation(edge).unwrap_or_default();

    EffectKey {
        kind: effect_kind,
        target: normalize_effect_component(&target_name),
        operation: normalize_effect_component(&operation),
    }
}

fn effect_node_id(key: &EffectKey) -> String {
    format!("effect::{}::{}::{}", key.kind, key.target, key.operation)
}

fn ensure_entry_or_symbol(
    node: &Node,
    entry_id: &str,
    semantic_nodes: &mut HashMap<String, DataflowNode>,
) {
    semantic_nodes
        .entry(node.id.clone())
        .or_insert_with(|| DataflowNode {
            id: node.id.clone(),
            name: node.name.clone(),
            kind: if node.id == entry_id {
                DataflowNodeKind::Entry
            } else {
                DataflowNodeKind::Symbol
            },
            file: Some(node.file.to_string_lossy().to_string()),
            effect_kind: None,
            operation: None,
            target: None,
        });
}

fn ensure_effect_node(
    edge: &Edge,
    source: &Node,
    target: Option<&Node>,
    semantic_nodes: &mut HashMap<String, DataflowNode>,
    effect_ids: &mut HashMap<EffectKey, String>,
) -> String {
    let key = effect_key(edge, source, target);
    if let Some(effect_id) = effect_ids.get(&key) {
        return effect_id.clone();
    }

    let effect_id = effect_node_id(&key);
    let target_name = effect_target_name(edge, target);
    let operation = effect_operation(edge);
    let effect_kind = key.kind.clone();
    let name = operation
        .as_ref()
        .map(|operation| format!("{operation} {target_name}"))
        .unwrap_or_else(|| target_name.clone());

    semantic_nodes.insert(
        effect_id.clone(),
        DataflowNode {
            id: effect_id.clone(),
            name,
            kind: DataflowNodeKind::Effect,
            file: None,
            effect_kind: Some(effect_kind),
            operation,
            target: Some(target_name),
        },
    );
    effect_ids.insert(key, effect_id.clone());
    effect_id
}

fn push_unique_string(values: &mut Vec<String>, value: &str) {
    if !values.iter().any(|existing| existing == value) {
        values.push(value.to_string());
    }
}

fn push_unique_provenance(values: &mut Vec<EdgeProvenance>, provenance: &[EdgeProvenance]) {
    for item in provenance {
        if !values.iter().any(|existing| existing == item) {
            values.push(item.clone());
        }
    }
}

fn record_semantic_edge(
    semantic_edges: &mut HashMap<SemanticEdgeKey, SemanticEdgeAccumulator>,
    source: &str,
    target: &str,
    kind: DataflowEdgeKind,
    edge: &Edge,
) {
    let key = SemanticEdgeKey {
        source: source.to_string(),
        target: target.to_string(),
        kind,
        operation: edge.operation.clone(),
    };
    let accumulator = semantic_edges
        .entry(key)
        .or_insert_with(|| SemanticEdgeAccumulator {
            source: source.to_string(),
            target: target.to_string(),
            kind,
            operation: edge.operation.clone(),
            conditions: Vec::new(),
            async_boundary: None,
            provenance: Vec::new(),
        });

    if let Some(condition) = edge.condition.as_deref() {
        push_unique_string(&mut accumulator.conditions, condition);
    }
    if edge.async_boundary == Some(true) {
        accumulator.async_boundary = Some(true);
    }
    push_unique_provenance(&mut accumulator.provenance, &edge.provenance);
}

fn semantic_edges_from_calls(
    source: &Node,
    edge: &Edge,
    target: Option<&Node>,
    entry_id: &str,
    semantic_nodes: &mut HashMap<String, DataflowNode>,
    semantic_edges: &mut HashMap<SemanticEdgeKey, SemanticEdgeAccumulator>,
    effect_ids: &mut HashMap<EffectKey, String>,
) -> bool {
    if edge.kind != EdgeKind::Calls {
        return false;
    }

    if is_terminal(target) || terminal_kind_for_effect(source, target).is_some() && target.is_none()
    {
        let effect_id = ensure_effect_node(edge, source, target, semantic_nodes, effect_ids);
        for kind in semantic_edge_kinds(edge) {
            record_semantic_edge(semantic_edges, &source.id, &effect_id, kind, edge);
        }
        return false;
    }

    let Some(target) = target else {
        return false;
    };

    ensure_entry_or_symbol(source, entry_id, semantic_nodes);
    ensure_entry_or_symbol(target, entry_id, semantic_nodes);
    record_semantic_edge(
        semantic_edges,
        &source.id,
        &target.id,
        DataflowEdgeKind::Call,
        edge,
    );
    true
}

fn semantic_edges_from_effects(
    source: &Node,
    edge: &Edge,
    target: Option<&Node>,
    semantic_nodes: &mut HashMap<String, DataflowNode>,
    semantic_edges: &mut HashMap<SemanticEdgeKey, SemanticEdgeAccumulator>,
    effect_ids: &mut HashMap<EffectKey, String>,
) {
    let effect_id = ensure_effect_node(edge, source, target, semantic_nodes, effect_ids);
    for kind in semantic_edge_kinds(edge) {
        record_semantic_edge(semantic_edges, &source.id, &effect_id, kind, edge);
    }
}

#[cfg(test)]
pub fn query_dataflow(
    graph: &Graph,
    entry: &str,
    max_depth: usize,
) -> Result<DataflowResult, QueryResolveError> {
    query_dataflow_with_options(graph, entry, max_depth, &DataflowQueryOptions::default())
}

pub fn query_dataflow_with_options(
    graph: &Graph,
    entry: &str,
    max_depth: usize,
    options: &DataflowQueryOptions,
) -> Result<DataflowResult, QueryResolveError> {
    let entry_node = crate::query::resolve_node(graph, entry)?;

    // If the resolved node is a type (struct/enum/protocol), it won't have
    // outgoing dataflow edges. Suggest its member functions instead.
    if matches!(
        entry_node.kind,
        NodeKind::Class
            | NodeKind::Struct
            | NodeKind::Enum
            | NodeKind::Protocol
            | NodeKind::Extension
    ) {
        let members: Vec<&str> = graph
            .edges
            .iter()
            .filter(|e| e.source == entry_node.id && e.kind == EdgeKind::Contains)
            .filter_map(|e| {
                graph.nodes.iter().find(|n| n.id == e.target).and_then(|n| {
                    if matches!(n.kind, NodeKind::Function | NodeKind::Property) {
                        Some(n.name.as_str())
                    } else {
                        None
                    }
                })
            })
            .collect();

        let hint = if members.is_empty() {
            format!(
                "{} is a {} — `flow graph` traces dataflow from functions, not types",
                entry_node.name,
                serde_json::to_string(&entry_node.kind)
                    .unwrap_or_default()
                    .trim_matches('"'),
            )
        } else {
            let suggestions: Vec<&str> = members.iter().copied().take(5).collect();
            format!(
                "{} is a {} — `flow graph` traces dataflow from functions. Try one of its members: {}",
                entry_node.name,
                serde_json::to_string(&entry_node.kind)
                    .unwrap_or_default()
                    .trim_matches('"'),
                suggestions.join(", "),
            )
        };
        return Err(QueryResolveError::NotFunction { hint });
    }
    let node_index: HashMap<&str, &Node> = graph
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();

    let mut adjacency: HashMap<&str, Vec<&Edge>> = HashMap::new();
    for edge in &graph.edges {
        if is_dataflow_edge(edge.kind) {
            adjacency
                .entry(edge.source.as_str())
                .or_default()
                .push(edge);
        }
    }

    let mut semantic_nodes = HashMap::new();
    let mut semantic_edges = HashMap::new();
    let mut effect_ids = HashMap::new();
    ensure_entry_or_symbol(entry_node, &entry_node.id, &mut semantic_nodes);

    let mut queue = VecDeque::from([(entry_node.id.as_str(), 0usize)]);
    let mut best_depth: HashMap<&str, usize> = HashMap::from([(entry_node.id.as_str(), 0usize)]);

    while let Some((source_id, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }

        let Some(source) = node_index.get(source_id).copied() else {
            continue;
        };
        ensure_entry_or_symbol(source, &entry_node.id, &mut semantic_nodes);

        if let Some(edges) = adjacency.get(source_id) {
            for edge in edges {
                let target = node_index.get(edge.target.as_str()).copied();
                match edge.kind {
                    EdgeKind::Calls => {
                        if semantic_edges_from_calls(
                            source,
                            edge,
                            target,
                            &entry_node.id,
                            &mut semantic_nodes,
                            &mut semantic_edges,
                            &mut effect_ids,
                        ) && let Some(target) = target
                        {
                            let next_depth = depth + 1;
                            let should_enqueue = best_depth
                                .get(target.id.as_str())
                                .map(|existing| next_depth < *existing)
                                .unwrap_or(true);
                            if should_enqueue {
                                best_depth.insert(target.id.as_str(), next_depth);
                                queue.push_back((target.id.as_str(), next_depth));
                            }
                        }
                    }
                    EdgeKind::Reads
                    | EdgeKind::Writes
                    | EdgeKind::Publishes
                    | EdgeKind::Subscribes => {
                        semantic_edges_from_effects(
                            source,
                            edge,
                            target,
                            &mut semantic_nodes,
                            &mut semantic_edges,
                            &mut effect_ids,
                        );
                    }
                    EdgeKind::Uses
                    | EdgeKind::Imports
                    | EdgeKind::Exports
                    | EdgeKind::Implements
                    | EdgeKind::Contains
                    | EdgeKind::TypeRef
                    | EdgeKind::References
                    | EdgeKind::TypeOf
                    | EdgeKind::Returns
                    | EdgeKind::Inherits
                    | EdgeKind::Extends
                    | EdgeKind::Instantiates
                    | EdgeKind::Overrides
                    | EdgeKind::Decorates => {}
                }
            }
        }
    }

    let mut nodes: Vec<_> = semantic_nodes.into_values().collect();
    nodes.sort_by(|left, right| {
        kind_order(left.kind)
            .cmp(&kind_order(right.kind))
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.id.cmp(&right.id))
    });

    let mut edges: Vec<_> = semantic_edges
        .into_values()
        .map(|edge| DataflowEdge {
            source: edge.source,
            target: edge.target,
            kind: edge.kind,
            operation: edge.operation,
            conditions: edge.conditions,
            async_boundary: edge.async_boundary,
            provenance: edge.provenance,
        })
        .collect();
    edges.sort_by(|left, right| {
        left.source
            .cmp(&right.source)
            .then_with(|| left.target.cmp(&right.target))
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.operation.cmp(&right.operation))
    });

    let summary = DataflowSummary {
        symbols: nodes
            .iter()
            .filter(|node| node.kind == DataflowNodeKind::Symbol)
            .count(),
        effects: nodes
            .iter()
            .filter(|node| node.kind == DataflowNodeKind::Effect)
            .count(),
        edges: edges.len(),
        calls: edges
            .iter()
            .filter(|edge| edge.kind == DataflowEdgeKind::Call)
            .count(),
        reads: edges
            .iter()
            .filter(|edge| edge.kind == DataflowEdgeKind::Read)
            .count(),
        writes: edges
            .iter()
            .filter(|edge| edge.kind == DataflowEdgeKind::Write)
            .count(),
        publishes: edges
            .iter()
            .filter(|edge| edge.kind == DataflowEdgeKind::Publish)
            .count(),
        subscribes: edges
            .iter()
            .filter(|edge| edge.kind == DataflowEdgeKind::Subscribe)
            .count(),
    };

    let limit = options.limit;
    // Truncate `edges` to the user-supplied cap, then prune `nodes` down to
    // only the IDs referenced by surviving edges (plus the entry node, which
    // always stays). This guarantees the returned subgraph is internally
    // consistent: every edge's `source`/`target` resolves to a node in
    // `nodes`. `total_nodes` and `total_edges` continue to report the
    // pre-truncation counts so callers can detect that --limit reshaped the
    // result.
    let total_edges = edges.len();
    let total_nodes = nodes.len();
    edges.truncate(limit);

    let mut referenced_ids: std::collections::HashSet<&str> = std::collections::HashSet::new();
    referenced_ids.insert(entry_node.id.as_str());
    for edge in &edges {
        referenced_ids.insert(edge.source.as_str());
        referenced_ids.insert(edge.target.as_str());
    }
    nodes.retain(|node| referenced_ids.contains(node.id.as_str()));

    Ok(DataflowResult {
        entry: entry_node.id.clone(),
        nodes,
        total_nodes,
        edges,
        total_edges,
        summary,
        entry_ref: node_ref(entry_node),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapha_core::graph::{NodeKind, Span, TerminalKind, Visibility};
    use std::collections::HashMap as StdHashMap;
    use std::path::PathBuf;

    fn make_node(id: &str, role: Option<NodeRole>) -> Node {
        Node {
            id: id.into(),
            kind: NodeKind::Function,
            name: id.into(),
            file: PathBuf::from("test.rs"),
            span: Span {
                start: [0, 0],
                end: [1, 0],
            },
            visibility: Visibility::Public,
            metadata: StdHashMap::new(),
            role,
            signature: None,
            doc_comment: None,
            module: None,
            snippet: None,
            repo: None,
        }
    }

    fn make_edge(source: &str, target: &str, kind: EdgeKind) -> Edge {
        Edge {
            source: source.into(),
            target: target.into(),
            kind,
            confidence: 0.9,
            direction: None,
            operation: None,
            condition: None,
            async_boundary: None,
            provenance: Vec::new(),
            repo: None,
        }
    }

    fn provenance(file: &str, line: usize, symbol_id: &str) -> EdgeProvenance {
        EdgeProvenance {
            file: PathBuf::from(file),
            span: Span {
                start: [line, 0],
                end: [line, 8],
            },
            symbol_id: symbol_id.to_string(),
        }
    }

    #[test]
    fn deduplicates_effect_nodes_across_paths() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                make_node("entry", Some(NodeRole::EntryPoint)),
                make_node("service_a", None),
                make_node("service_b", None),
                make_node(
                    "persist",
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Persistence,
                    }),
                ),
            ],
            edges: vec![
                make_edge("entry", "service_a", EdgeKind::Calls),
                make_edge("entry", "service_b", EdgeKind::Calls),
                {
                    let mut edge = make_edge("service_a", "persist", EdgeKind::Calls);
                    edge.direction = Some(FlowDirection::Write);
                    edge.operation = Some("UPSERT".to_string());
                    edge
                },
                {
                    let mut edge = make_edge("service_b", "persist", EdgeKind::Calls);
                    edge.direction = Some(FlowDirection::Write);
                    edge.operation = Some("UPSERT".to_string());
                    edge
                },
            ],
        };

        let result = query_dataflow(&graph, "entry", 10).unwrap();
        assert_eq!(result.summary.effects, 1);
        assert_eq!(
            result
                .nodes
                .iter()
                .filter(|node| node.kind == DataflowNodeKind::Effect)
                .count(),
            1
        );
        assert_eq!(
            result
                .edges
                .iter()
                .filter(|edge| edge.kind == DataflowEdgeKind::Write)
                .count(),
            2
        );
    }

    #[test]
    fn splits_read_write_edges_into_two_semantic_edges() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                make_node("entry", Some(NodeRole::EntryPoint)),
                make_node(
                    "storage",
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Persistence,
                    }),
                ),
            ],
            edges: vec![{
                let mut edge = make_edge("entry", "storage", EdgeKind::Calls);
                edge.direction = Some(FlowDirection::ReadWrite);
                edge.operation = Some("SYNC".to_string());
                edge
            }],
        };

        let result = query_dataflow(&graph, "entry", 10).unwrap();
        assert!(
            result
                .edges
                .iter()
                .any(|edge| edge.kind == DataflowEdgeKind::Read)
        );
        assert!(
            result
                .edges
                .iter()
                .any(|edge| edge.kind == DataflowEdgeKind::Write)
        );
    }

    #[test]
    fn derives_effect_nodes_from_terminal_call_edges() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                make_node("entry", Some(NodeRole::EntryPoint)),
                make_node(
                    "fetch",
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Network,
                    }),
                ),
            ],
            edges: vec![{
                let mut edge = make_edge("entry", "fetch", EdgeKind::Calls);
                edge.direction = Some(FlowDirection::Read);
                edge.operation = Some("HTTP_GET".to_string());
                edge
            }],
        };

        let result = query_dataflow(&graph, "entry", 10).unwrap();
        let effect_node = result
            .nodes
            .iter()
            .find(|node| node.kind == DataflowNodeKind::Effect)
            .expect("should derive an effect node");
        assert_eq!(effect_node.effect_kind.as_deref(), Some("network"));
        assert_eq!(effect_node.operation.as_deref(), Some("HTTP_GET"));
        assert!(
            result
                .edges
                .iter()
                .any(|edge| edge.kind == DataflowEdgeKind::Read)
        );
    }

    #[test]
    fn aggregates_provenance_and_conditions_on_collapsed_edges() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                make_node("entry", Some(NodeRole::EntryPoint)),
                make_node("service", None),
                make_node(
                    "persist",
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Persistence,
                    }),
                ),
            ],
            edges: vec![
                make_edge("entry", "service", EdgeKind::Calls),
                {
                    let mut edge = make_edge("service", "persist", EdgeKind::Calls);
                    edge.direction = Some(FlowDirection::Write);
                    edge.operation = Some("UPSERT".to_string());
                    edge.condition = Some("tenant == primary".to_string());
                    edge.provenance = vec![provenance("test.rs", 4, "service")];
                    edge
                },
                {
                    let mut edge = make_edge("service", "persist", EdgeKind::Calls);
                    edge.direction = Some(FlowDirection::Write);
                    edge.operation = Some("UPSERT".to_string());
                    edge.condition = Some("tenant == backup".to_string());
                    edge.provenance = vec![provenance("test.rs", 8, "service")];
                    edge
                },
            ],
        };

        let result = query_dataflow(&graph, "entry", 10).unwrap();
        let semantic_edge = result
            .edges
            .iter()
            .find(|edge| edge.kind == DataflowEdgeKind::Write)
            .expect("should derive a write edge");
        assert_eq!(semantic_edge.conditions.len(), 2);
        assert_eq!(semantic_edge.provenance.len(), 2);
    }

    #[test]
    fn dataflow_truncates_to_consistent_subgraph() {
        // Entry plus three distinct terminals, each producing its own effect
        // node (different `target`) and its own semantic edge.
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                make_node("entry", Some(NodeRole::EntryPoint)),
                make_node(
                    "persist_a",
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Persistence,
                    }),
                ),
                make_node(
                    "persist_b",
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Persistence,
                    }),
                ),
                make_node(
                    "persist_c",
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Persistence,
                    }),
                ),
            ],
            edges: vec![
                {
                    let mut edge = make_edge("entry", "persist_a", EdgeKind::Calls);
                    edge.direction = Some(FlowDirection::Write);
                    edge.operation = Some("save_a".to_string());
                    edge
                },
                {
                    let mut edge = make_edge("entry", "persist_b", EdgeKind::Calls);
                    edge.direction = Some(FlowDirection::Write);
                    edge.operation = Some("save_b".to_string());
                    edge
                },
                {
                    let mut edge = make_edge("entry", "persist_c", EdgeKind::Calls);
                    edge.direction = Some(FlowDirection::Write);
                    edge.operation = Some("save_c".to_string());
                    edge
                },
            ],
        };

        let unbounded =
            query_dataflow_with_options(&graph, "entry", 10, &DataflowQueryOptions::default())
                .unwrap();
        // 1 entry + 3 effect nodes = 4 semantic nodes; 3 write edges.
        assert!(
            unbounded.nodes.len() >= 3,
            "fixture must yield >= 3 nodes, got {}",
            unbounded.nodes.len()
        );
        assert!(
            unbounded.edges.len() >= 3,
            "fixture must yield >= 3 edges, got {}",
            unbounded.edges.len()
        );
        let unbounded_node_total = unbounded.nodes.len();
        let unbounded_edge_total = unbounded.edges.len();
        assert_eq!(unbounded.total_nodes, unbounded_node_total);
        assert_eq!(unbounded.total_edges, unbounded_edge_total);

        let truncated =
            query_dataflow_with_options(&graph, "entry", 10, &DataflowQueryOptions { limit: 1 })
                .unwrap();
        // `--limit` caps `edges` directly. After capping, `nodes` is filtered
        // to only those referenced by a surviving edge plus the entry node.
        // For this fixture: 1 surviving write edge (entry -> effect::*) keeps
        // the entry node and exactly one effect node => 2 nodes.
        assert_eq!(truncated.edges.len(), 1, "edges should be truncated");
        assert_eq!(
            truncated.nodes.len(),
            2,
            "nodes should be the consistent subgraph: entry + endpoints of the surviving edge"
        );
        // The surviving edge's endpoints must both exist in `nodes`.
        let kept_ids: std::collections::HashSet<&str> =
            truncated.nodes.iter().map(|n| n.id.as_str()).collect();
        let edge = &truncated.edges[0];
        assert!(
            kept_ids.contains(edge.source.as_str()),
            "edge.source must be present in nodes"
        );
        assert!(
            kept_ids.contains(edge.target.as_str()),
            "edge.target must be present in nodes"
        );
        // The entry node always stays.
        assert!(
            kept_ids.contains("entry"),
            "entry node must always be retained"
        );
        // Pre-truncation counts must still be reported.
        assert_eq!(truncated.total_nodes, unbounded_node_total);
        assert_eq!(truncated.total_edges, unbounded_edge_total);
    }

    #[test]
    fn dataflow_limit_zero_keeps_entry_only() {
        // Same fixture shape as `dataflow_truncates_to_consistent_subgraph`:
        // entry plus three distinct write-effect terminals.
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                make_node("entry", Some(NodeRole::EntryPoint)),
                make_node(
                    "persist_a",
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Persistence,
                    }),
                ),
                make_node(
                    "persist_b",
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Persistence,
                    }),
                ),
                make_node(
                    "persist_c",
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Persistence,
                    }),
                ),
            ],
            edges: vec![
                {
                    let mut edge = make_edge("entry", "persist_a", EdgeKind::Calls);
                    edge.direction = Some(FlowDirection::Write);
                    edge.operation = Some("save_a".to_string());
                    edge
                },
                {
                    let mut edge = make_edge("entry", "persist_b", EdgeKind::Calls);
                    edge.direction = Some(FlowDirection::Write);
                    edge.operation = Some("save_b".to_string());
                    edge
                },
                {
                    let mut edge = make_edge("entry", "persist_c", EdgeKind::Calls);
                    edge.direction = Some(FlowDirection::Write);
                    edge.operation = Some("save_c".to_string());
                    edge
                },
            ],
        };

        let unbounded =
            query_dataflow_with_options(&graph, "entry", 10, &DataflowQueryOptions::default())
                .unwrap();
        let unbounded_node_total = unbounded.nodes.len();
        let unbounded_edge_total = unbounded.edges.len();
        assert!(
            unbounded_edge_total >= 3,
            "fixture must yield >= 3 edges, got {unbounded_edge_total}"
        );

        let zero =
            query_dataflow_with_options(&graph, "entry", 10, &DataflowQueryOptions { limit: 0 })
                .unwrap();
        // With --limit=0 we keep no edges and therefore only the always-retained
        // entry node.
        assert!(
            zero.edges.is_empty(),
            "edges must be empty under --limit=0, got {}",
            zero.edges.len()
        );
        assert_eq!(
            zero.nodes.len(),
            1,
            "only the entry node should survive under --limit=0"
        );
        assert_eq!(
            zero.nodes[0].id, "entry",
            "the surviving node must be the entry"
        );
        // Pre-truncation counts must still be reported.
        assert_eq!(zero.total_nodes, unbounded_node_total);
        assert_eq!(zero.total_edges, unbounded_edge_total);
    }
}
