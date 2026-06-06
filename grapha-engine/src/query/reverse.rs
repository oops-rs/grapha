use std::collections::{HashMap, HashSet, VecDeque};

use serde::Serialize;

use grapha_core::graph::{EdgeKind, Graph, Node, NodeKind, NodeRole};

use super::flow::is_dataflow_edge;
use super::{
    QueryResolveError, SymbolRef, normalize_symbol_name, strip_accessor_prefix, truncate_with_total,
};

/// Cap for the `affected_entries` vector returned by [`query_reverse_with_options`].
///
/// The default leaves results unbounded; the CLI overrides this with the
/// user-supplied `--limit`. `total_entries` always reflects the pre-truncation
/// count so callers can detect truncation.
#[derive(Debug, Clone)]
pub struct ReverseQueryOptions {
    pub limit: usize,
}

impl Default for ReverseQueryOptions {
    fn default() -> Self {
        // Internal callers stay unbounded.
        Self { limit: usize::MAX }
    }
}

#[derive(Debug, Serialize)]
pub struct ReverseResult {
    pub symbol: String,
    pub affected_entries: Vec<AffectedEntry>,
    pub total_entries: usize,
    #[serde(skip)]
    pub target_ref: SymbolRef,
}

#[derive(Debug, Serialize)]
pub struct AffectedEntry {
    pub entry: SymbolRef,
    pub distance: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<String>,
}

#[derive(Debug)]
struct AccessorCluster<'a> {
    display_name: String,
    entry_nodes: Vec<&'a Node>,
}

fn is_accessor_function(node: &Node) -> bool {
    node.kind == NodeKind::Function && strip_accessor_prefix(&node.name) != node.name
}

fn accessor_property_pair<'a>(left: &'a Node, right: &'a Node) -> Option<(&'a Node, &'a Node)> {
    if is_accessor_function(left)
        && right.kind == NodeKind::Property
        && normalize_symbol_name(&left.name) == normalize_symbol_name(&right.name)
    {
        Some((left, right))
    } else if is_accessor_function(right)
        && left.kind == NodeKind::Property
        && normalize_symbol_name(&right.name) == normalize_symbol_name(&left.name)
    {
        Some((right, left))
    } else {
        None
    }
}

fn node_kind_preference(kind: NodeKind) -> usize {
    match kind {
        NodeKind::Property => 0,
        NodeKind::Function => 1,
        NodeKind::Variant | NodeKind::Field => 2,
        _ => 3,
    }
}

fn canonical_cluster_name(cluster_nodes: &[&Node]) -> String {
    if let Some(property_node) = cluster_nodes
        .iter()
        .find(|node| node.kind == NodeKind::Property)
    {
        return normalize_symbol_name(&property_node.name).to_string();
    }

    let preferred = cluster_nodes
        .iter()
        .min_by_key(|node| {
            (
                node_kind_preference(node.kind),
                normalize_symbol_name(&node.name),
                node.id.as_str(),
            )
        })
        .expect("cluster_nodes is not empty");

    normalize_symbol_name(&preferred.name).to_string()
}

fn to_symbol_ref(node: &Node) -> SymbolRef {
    SymbolRef::from_node(node)
}

fn build_accessor_adjacency<'a>(
    graph: &'a Graph,
    node_index: &HashMap<&'a str, &'a Node>,
) -> HashMap<&'a str, Vec<&'a str>> {
    let mut adjacency: HashMap<&str, Vec<&str>> = HashMap::new();

    for edge in &graph.edges {
        if edge.kind != EdgeKind::Implements {
            continue;
        }

        let Some(source_node) = node_index.get(edge.source.as_str()).copied() else {
            continue;
        };
        let Some(target_node) = node_index.get(edge.target.as_str()).copied() else {
            continue;
        };
        let Some((accessor, property)) = accessor_property_pair(source_node, target_node) else {
            continue;
        };

        adjacency
            .entry(accessor.id.as_str())
            .or_default()
            .push(property.id.as_str());
        adjacency
            .entry(property.id.as_str())
            .or_default()
            .push(accessor.id.as_str());
    }

    adjacency
}

fn build_accessor_clusters<'a>(
    graph: &'a Graph,
    node_index: &HashMap<&'a str, &'a Node>,
    accessor_adjacency: &HashMap<&'a str, Vec<&'a str>>,
) -> (
    HashMap<&'a str, AccessorCluster<'a>>,
    HashMap<&'a str, &'a str>,
) {
    let mut clusters = HashMap::new();
    let mut node_to_cluster = HashMap::new();
    let mut visited = HashSet::new();

    for node in &graph.nodes {
        let start_id = node.id.as_str();
        if !visited.insert(start_id) {
            continue;
        }

        let mut stack = vec![start_id];
        let mut members = Vec::new();

        while let Some(current_id) = stack.pop() {
            members.push(current_id);
            if let Some(neighbors) = accessor_adjacency.get(current_id) {
                for &neighbor_id in neighbors {
                    if visited.insert(neighbor_id) {
                        stack.push(neighbor_id);
                    }
                }
            }
        }

        members.sort_unstable();
        let cluster_id = members[0];
        let cluster_nodes: Vec<&Node> = members
            .iter()
            .filter_map(|member_id| node_index.get(member_id).copied())
            .collect();
        let display_name = canonical_cluster_name(&cluster_nodes);
        let entry_nodes = cluster_nodes
            .iter()
            .copied()
            .filter(|cluster_node| cluster_node.role == Some(NodeRole::EntryPoint))
            .collect();

        for &member_id in &members {
            node_to_cluster.insert(member_id, cluster_id);
        }

        clusters.insert(
            cluster_id,
            AccessorCluster {
                display_name,
                entry_nodes,
            },
        );
    }

    (clusters, node_to_cluster)
}

fn build_reverse_cluster_adjacency<'a>(
    graph: &'a Graph,
    node_to_cluster: &HashMap<&'a str, &'a str>,
) -> HashMap<&'a str, Vec<&'a str>> {
    let mut reverse_adjacency: HashMap<&str, HashSet<&str>> = HashMap::new();

    for edge in &graph.edges {
        if !is_dataflow_edge(edge.kind) {
            continue;
        }

        let Some(&source_cluster) = node_to_cluster.get(edge.source.as_str()) else {
            continue;
        };
        let Some(&target_cluster) = node_to_cluster.get(edge.target.as_str()) else {
            continue;
        };

        if source_cluster == target_cluster {
            continue;
        }

        reverse_adjacency
            .entry(target_cluster)
            .or_default()
            .insert(source_cluster);
    }

    reverse_adjacency
        .into_iter()
        .map(|(cluster_id, cluster_sources)| {
            let mut sources: Vec<_> = cluster_sources.into_iter().collect();
            sources.sort_unstable();
            (cluster_id, sources)
        })
        .collect()
}

pub fn query_reverse(
    graph: &Graph,
    symbol: &str,
    max_depth: Option<usize>,
) -> Result<ReverseResult, QueryResolveError> {
    query_reverse_with_options(graph, symbol, max_depth, &ReverseQueryOptions::default())
}

pub fn query_reverse_with_options(
    graph: &Graph,
    symbol: &str,
    max_depth: Option<usize>,
    options: &ReverseQueryOptions,
) -> Result<ReverseResult, QueryResolveError> {
    let target_node = crate::query::resolve_node(graph, symbol)?;

    let node_index: HashMap<&str, &Node> = graph.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let accessor_adjacency = build_accessor_adjacency(graph, &node_index);
    let (clusters, node_to_cluster) =
        build_accessor_clusters(graph, &node_index, &accessor_adjacency);
    let reverse_adjacency = build_reverse_cluster_adjacency(graph, &node_to_cluster);

    let target_cluster_id = *node_to_cluster
        .get(target_node.id.as_str())
        .expect("resolved node must belong to a cluster");
    let target_cluster = clusters
        .get(target_cluster_id)
        .expect("target cluster must exist");

    let mut visited_clusters: HashSet<&str> = HashSet::new();
    visited_clusters.insert(target_cluster_id);

    // BFS backward over transparent accessor clusters:
    // (cluster_id, dataflow_distance, path_from_target)
    let mut queue: VecDeque<(&str, usize, Vec<String>)> = VecDeque::new();
    queue.push_back((
        target_cluster_id,
        0,
        vec![target_cluster.display_name.clone()],
    ));

    let mut affected_entries = Vec::new();
    let mut seen_entries: HashSet<&str> = HashSet::new();
    let max_depth = max_depth.unwrap_or(usize::MAX);

    while let Some((cluster_id, distance, path)) = queue.pop_front() {
        let cluster = clusters.get(cluster_id).expect("cluster must exist");

        for entry_node in &cluster.entry_nodes {
            if seen_entries.insert(entry_node.id.as_str()) {
                let reversed_path: Vec<String> = path.iter().rev().cloned().collect();
                affected_entries.push(AffectedEntry {
                    entry: to_symbol_ref(entry_node),
                    distance,
                    path: reversed_path,
                });
            }
        }

        if distance >= max_depth {
            continue;
        }

        if let Some(source_clusters) = reverse_adjacency.get(cluster_id) {
            for &source_cluster_id in source_clusters {
                if visited_clusters.contains(source_cluster_id) {
                    continue;
                }
                visited_clusters.insert(source_cluster_id);

                let source_cluster = clusters
                    .get(source_cluster_id)
                    .expect("source cluster must exist");
                let mut new_path = path.clone();
                new_path.push(source_cluster.display_name.clone());
                queue.push_back((source_cluster_id, distance + 1, new_path));
            }
        }
    }

    let total_entries = truncate_with_total(&mut affected_entries, options.limit);
    Ok(ReverseResult {
        symbol: target_node.id.clone(),
        affected_entries,
        total_entries,
        target_ref: to_symbol_ref(target_node),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapha_core::graph::*;
    use std::collections::HashMap as StdHashMap;
    use std::path::PathBuf;

    fn make_node(id: &str, name: &str, kind: NodeKind, role: Option<NodeRole>) -> Node {
        Node {
            id: id.into(),
            kind,
            name: name.into(),
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

    fn make_edge(source: &str, target: &str) -> Edge {
        Edge {
            source: source.into(),
            target: target.into(),
            kind: EdgeKind::Calls,
            confidence: 0.9,
            direction: None,
            operation: None,
            condition: None,
            async_boundary: None,
            provenance: Vec::new(),
            repo: None,
        }
    }

    #[test]
    fn finds_entry_points_that_reach_symbol() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                make_node(
                    "entry1",
                    "entry1",
                    NodeKind::Function,
                    Some(NodeRole::EntryPoint),
                ),
                make_node("service", "service", NodeKind::Function, None),
                make_node(
                    "db",
                    "db",
                    NodeKind::Function,
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Persistence,
                    }),
                ),
            ],
            edges: vec![make_edge("entry1", "service"), make_edge("service", "db")],
        };

        let result = query_reverse(&graph, "db", None).unwrap();
        assert_eq!(result.symbol, "db");
        assert_eq!(result.affected_entries.len(), 1);
        assert_eq!(result.affected_entries[0].entry.name, "entry1");
        assert_eq!(result.affected_entries[0].distance, 2);
        assert_eq!(
            result.affected_entries[0].path,
            vec!["entry1", "service", "db"]
        );
    }

    #[test]
    fn finds_multiple_entry_points() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                make_node(
                    "entry1",
                    "entry1",
                    NodeKind::Function,
                    Some(NodeRole::EntryPoint),
                ),
                make_node(
                    "entry2",
                    "entry2",
                    NodeKind::Function,
                    Some(NodeRole::EntryPoint),
                ),
                make_node("shared", "shared", NodeKind::Function, None),
            ],
            edges: vec![make_edge("entry1", "shared"), make_edge("entry2", "shared")],
        };

        let result = query_reverse(&graph, "shared", None).unwrap();
        assert_eq!(result.total_entries, 2);
        let names: Vec<&str> = result
            .affected_entries
            .iter()
            .map(|e| e.entry.name.as_str())
            .collect();
        assert!(names.contains(&"entry1"));
        assert!(names.contains(&"entry2"));
    }

    #[test]
    fn returns_none_for_unknown_symbol() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![make_node("a", "a", NodeKind::Function, None)],
            edges: vec![],
        };
        assert!(matches!(
            query_reverse(&graph, "nonexistent", None),
            Err(QueryResolveError::NotFound { .. })
        ));
    }

    #[test]
    fn includes_path_from_entry_to_symbol() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                make_node(
                    "entry",
                    "entry",
                    NodeKind::Function,
                    Some(NodeRole::EntryPoint),
                ),
                make_node("mid", "mid", NodeKind::Function, None),
                make_node("target", "target", NodeKind::Function, None),
            ],
            edges: vec![make_edge("entry", "mid"), make_edge("mid", "target")],
        };

        let result = query_reverse(&graph, "target", None).unwrap();
        assert_eq!(result.affected_entries.len(), 1);
        assert_eq!(
            result.affected_entries[0].path,
            vec!["entry", "mid", "target"]
        );
    }

    #[test]
    fn respects_max_depth_when_walking_upstream() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                make_node(
                    "entry",
                    "entry",
                    NodeKind::Function,
                    Some(NodeRole::EntryPoint),
                ),
                make_node("mid", "mid", NodeKind::Function, None),
                make_node("target", "target", NodeKind::Function, None),
            ],
            edges: vec![make_edge("entry", "mid"), make_edge("mid", "target")],
        };

        let result = query_reverse(&graph, "target", Some(1)).unwrap();
        assert!(result.affected_entries.is_empty());

        let result = query_reverse(&graph, "target", Some(2)).unwrap();
        assert_eq!(result.affected_entries.len(), 1);
        assert_eq!(result.affected_entries[0].distance, 2);
    }

    #[test]
    fn traverses_accessor_clusters_without_exposing_accessor_hops() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                make_node("getter:body", "getter:body", NodeKind::Function, None),
                make_node(
                    "body",
                    "body",
                    NodeKind::Property,
                    Some(NodeRole::EntryPoint),
                ),
                make_node(
                    "getter:viewModel",
                    "getter:viewModel",
                    NodeKind::Function,
                    None,
                ),
                make_node("viewModel", "viewModel", NodeKind::Property, None),
                make_node(
                    "sendMessage",
                    "sendMessage(message:)",
                    NodeKind::Function,
                    None,
                ),
                make_node(
                    "handleSendResult",
                    "handleSendResult(_:receiveValue:)",
                    NodeKind::Function,
                    None,
                ),
            ],
            edges: vec![
                Edge {
                    source: "getter:body".into(),
                    target: "body".into(),
                    kind: EdgeKind::Implements,
                    confidence: 1.0,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: Vec::new(),
                    repo: None,
                },
                Edge {
                    source: "getter:viewModel".into(),
                    target: "viewModel".into(),
                    kind: EdgeKind::Implements,
                    confidence: 1.0,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: Vec::new(),
                    repo: None,
                },
                make_edge("getter:body", "getter:viewModel"),
                make_edge("getter:viewModel", "sendMessage"),
                make_edge("sendMessage", "handleSendResult"),
            ],
        };

        let result = query_reverse(&graph, "handleSendResult", None).unwrap();
        assert_eq!(result.affected_entries.len(), 1);
        assert_eq!(result.affected_entries[0].entry.id, "body");
        assert_eq!(result.affected_entries[0].distance, 3);
        assert_eq!(
            result.affected_entries[0].path,
            vec!["body", "viewModel", "sendMessage", "handleSendResult"]
        );
    }

    #[test]
    fn ignores_swiftui_structural_edges() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                make_node(
                    "body",
                    "body",
                    NodeKind::Property,
                    Some(NodeRole::EntryPoint),
                ),
                make_node("body::view:Row@10:12", "Row", NodeKind::View, None),
                make_node("row_decl", "Row", NodeKind::Struct, None),
            ],
            edges: vec![
                Edge {
                    source: "body".into(),
                    target: "body::view:Row@10:12".into(),
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
                    source: "body::view:Row@10:12".into(),
                    target: "row_decl".into(),
                    kind: EdgeKind::TypeRef,
                    confidence: 0.9,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: Vec::new(),
                    repo: None,
                },
            ],
        };

        let result = query_reverse(&graph, "Row", None).unwrap();
        assert_eq!(result.total_entries, 0);
        assert!(result.affected_entries.is_empty());
    }

    #[test]
    fn reverse_truncates_affected_entries() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                make_node(
                    "entry_a",
                    "entry_a",
                    NodeKind::Function,
                    Some(NodeRole::EntryPoint),
                ),
                make_node(
                    "entry_b",
                    "entry_b",
                    NodeKind::Function,
                    Some(NodeRole::EntryPoint),
                ),
                make_node(
                    "entry_c",
                    "entry_c",
                    NodeKind::Function,
                    Some(NodeRole::EntryPoint),
                ),
                make_node("shared", "shared", NodeKind::Function, None),
            ],
            edges: vec![
                make_edge("entry_a", "shared"),
                make_edge("entry_b", "shared"),
                make_edge("entry_c", "shared"),
            ],
        };

        let truncated =
            query_reverse_with_options(&graph, "shared", None, &ReverseQueryOptions { limit: 1 })
                .unwrap();
        assert_eq!(
            truncated.affected_entries.len(),
            1,
            "affected_entries should be truncated"
        );
        assert_eq!(truncated.total_entries, 3);

        let unbounded =
            query_reverse_with_options(&graph, "shared", None, &ReverseQueryOptions::default())
                .unwrap();
        assert_eq!(unbounded.affected_entries.len(), 3);
        assert_eq!(unbounded.total_entries, 3);
    }
}
