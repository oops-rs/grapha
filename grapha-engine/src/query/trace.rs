use std::collections::{HashMap, HashSet};

use serde::Serialize;

use grapha_core::graph::{EdgeKind, FlowDirection, Graph, Node, NodeRole};

use super::flow::{is_dataflow_edge, terminal_kind_to_string};
use super::{QueryResolveError, SymbolRef, normalize_symbol_name, truncate_with_total};

/// Cap for the `flows` vector returned by [`query_trace_with_options`].
///
/// The default leaves results unbounded; the CLI overrides this with the
/// user-supplied `--limit` so AI-agent callers don't drown in token-heavy
/// flow listings on entry points with large fan-out.
#[derive(Debug, Clone)]
pub struct TraceQueryOptions {
    pub limit: usize,
}

impl Default for TraceQueryOptions {
    fn default() -> Self {
        // Internal callers stay unbounded.
        Self { limit: usize::MAX }
    }
}

#[derive(Debug, Serialize)]
pub struct TraceResult {
    pub entry: String,
    pub requested_symbol: String,
    pub traced_roots: Vec<String>,
    pub fallback_used: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    pub flows: Vec<Flow>,
    pub total_flows: usize,
    pub summary: TraceSummary,
    #[serde(skip)]
    pub entry_ref: SymbolRef,
}

#[derive(Debug, Serialize)]
pub struct Flow {
    pub path: Vec<String>,
    pub terminal: Option<TerminalInfo>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub async_boundaries: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct TerminalInfo {
    pub kind: String,
    pub operation: String,
    pub direction: String,
}

#[derive(Debug, Serialize)]
pub struct TraceSummary {
    pub total_flows: usize,
    pub reads: usize,
    pub writes: usize,
    pub async_crossings: usize,
}

fn direction_from_edge(edge_kind: EdgeKind, direction: Option<&FlowDirection>) -> String {
    match direction {
        Some(FlowDirection::Read) => "read".to_string(),
        Some(FlowDirection::Write) => "write".to_string(),
        Some(FlowDirection::ReadWrite) => "read_write".to_string(),
        Some(FlowDirection::Pure) => "pure".to_string(),
        None => match edge_kind {
            EdgeKind::Reads => "read".to_string(),
            EdgeKind::Writes => "write".to_string(),
            _ => "unknown".to_string(),
        },
    }
}

fn summarize_flows(flows: &[Flow]) -> TraceSummary {
    let mut reads = 0;
    let mut writes = 0;
    let mut async_crossings = 0;

    for flow in flows {
        if let Some(terminal) = &flow.terminal {
            match terminal.direction.as_str() {
                "read" => reads += 1,
                "write" => writes += 1,
                "read_write" => {
                    reads += 1;
                    writes += 1;
                }
                _ => {}
            }
        }
        async_crossings += flow.async_boundaries.len();
    }

    TraceSummary {
        total_flows: flows.len(),
        reads,
        writes,
        async_crossings,
    }
}

fn trace_from_root<'a>(
    graph: &'a Graph,
    root_id: &'a str,
    max_depth: usize,
    node_index: &HashMap<&'a str, usize>,
    forward_adj: &HashMap<&'a str, Vec<(&'a str, usize)>>,
) -> Vec<Flow> {
    let Some(&root_index) = node_index.get(root_id) else {
        return Vec::new();
    };
    let root_node = &graph.nodes[root_index];

    struct StackFrame<'a> {
        node_id: &'a str,
        path: Vec<String>,
        conditions: Vec<String>,
        async_boundaries: Vec<String>,
        visited_edges: HashSet<(&'a str, &'a str)>,
    }

    let mut flows = Vec::new();
    let mut stack: Vec<StackFrame> = vec![StackFrame {
        node_id: root_id,
        path: vec![root_node.name.clone()],
        conditions: Vec::new(),
        async_boundaries: Vec::new(),
        visited_edges: HashSet::new(),
    }];

    while let Some(frame) = stack.pop() {
        if frame.path.len() > max_depth + 1 {
            continue;
        }

        if frame.path.len() > 1
            && let Some(&ni) = node_index.get(frame.node_id)
        {
            let node = &graph.nodes[ni];
            if let Some(NodeRole::Terminal { kind }) = &node.role {
                let last_edge_direction = graph
                    .edges
                    .iter()
                    .find(|e| e.target == frame.node_id && is_dataflow_edge(e.kind));
                let direction = last_edge_direction
                    .map(|e| direction_from_edge(e.kind, e.direction.as_ref()))
                    .unwrap_or_else(|| "unknown".to_string());
                let operation = last_edge_direction
                    .and_then(|e| e.operation.clone())
                    .unwrap_or_else(|| "unknown".to_string());

                flows.push(Flow {
                    path: frame.path,
                    terminal: Some(TerminalInfo {
                        kind: terminal_kind_to_string(kind),
                        operation,
                        direction,
                    }),
                    conditions: frame.conditions,
                    async_boundaries: frame.async_boundaries,
                });
                continue;
            }
        }

        if let Some(neighbors) = forward_adj.get(frame.node_id) {
            for &(target_id, ei) in neighbors {
                let edge_pair = (frame.node_id, target_id);
                if frame.visited_edges.contains(&edge_pair) {
                    continue;
                }

                let edge = &graph.edges[ei];
                let target_node = node_index.get(target_id).map(|&i| &graph.nodes[i]);
                let target_name = target_node
                    .map(|n| n.name.clone())
                    .unwrap_or_else(|| target_id.to_string());

                let mut new_path = frame.path.clone();
                new_path.push(target_name);

                let mut new_conditions = frame.conditions.clone();
                if let Some(cond) = &edge.condition {
                    new_conditions.push(cond.clone());
                }

                let mut new_async = frame.async_boundaries.clone();
                if edge.async_boundary == Some(true) {
                    new_async.push(format!("{} -> {}", frame.node_id, target_id));
                }

                let mut new_visited = frame.visited_edges.clone();
                new_visited.insert(edge_pair);

                stack.push(StackFrame {
                    node_id: target_id,
                    path: new_path,
                    conditions: new_conditions,
                    async_boundaries: new_async,
                    visited_edges: new_visited,
                });
            }
        }
    }

    flows
}

fn fallback_trace_roots<'a>(
    graph: &'a Graph,
    requested: &'a Node,
    node_index: &HashMap<&'a str, usize>,
    forward_adj: &HashMap<&'a str, Vec<(&'a str, usize)>>,
) -> Vec<&'a Node> {
    let mut ranked_roots: Vec<(usize, &'a str, &'a Node)> = graph
        .edges
        .iter()
        .filter_map(|edge| {
            let candidate = match edge.kind {
                EdgeKind::Contains if edge.source == requested.id => {
                    let child_index = node_index.get(edge.target.as_str())?;
                    &graph.nodes[*child_index]
                }
                EdgeKind::Implements if edge.target == requested.id => {
                    let child_index = node_index.get(edge.source.as_str())?;
                    &graph.nodes[*child_index]
                }
                EdgeKind::TypeRef if edge.source == requested.id => {
                    let child_index = node_index.get(edge.target.as_str())?;
                    &graph.nodes[*child_index]
                }
                _ => return None,
            };

            if candidate.file != requested.file {
                return None;
            }

            let is_body = normalize_symbol_name(&candidate.name) == "body";
            let is_action = matches!(candidate.kind, grapha_core::graph::NodeKind::Function)
                && matches!(
                    normalize_symbol_name(&candidate.name),
                    name if name.starts_with("on")
                        || name.starts_with("handle")
                        || name.starts_with("did")
                        || name.starts_with("go")
                        || name.starts_with("goto")
                );
            let has_dataflow_edges = forward_adj
                .get(candidate.id.as_str())
                .is_some_and(|edges| !edges.is_empty());

            let priority = if is_body {
                Some(0)
            } else if is_action {
                Some(1)
            } else if has_dataflow_edges {
                Some(2)
            } else {
                None
            }?;

            Some((priority, candidate.name.as_str(), candidate))
        })
        .collect();

    ranked_roots.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.cmp(right.1))
            .then_with(|| left.2.id.cmp(&right.2.id))
    });

    let mut roots = Vec::new();
    let mut seen = HashSet::new();
    for (_, _, child) in ranked_roots {
        if seen.insert(child.id.as_str()) {
            roots.push(child);
        }
    }

    roots
}

pub fn query_trace(
    graph: &Graph,
    entry: &str,
    max_depth: usize,
) -> Result<TraceResult, QueryResolveError> {
    query_trace_with_options(graph, entry, max_depth, &TraceQueryOptions::default())
}

pub fn query_trace_with_options(
    graph: &Graph,
    entry: &str,
    max_depth: usize,
    options: &TraceQueryOptions,
) -> Result<TraceResult, QueryResolveError> {
    let entry_node = crate::query::resolve_node(graph, entry)?;

    let node_index: HashMap<&str, usize> = graph
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id.as_str(), i))
        .collect();

    // Build forward adjacency: source -> [(target_id, edge_index)]
    let mut forward_adj: HashMap<&str, Vec<(&str, usize)>> = HashMap::new();
    for (ei, edge) in graph.edges.iter().enumerate() {
        if is_dataflow_edge(edge.kind) {
            forward_adj
                .entry(&edge.source)
                .or_default()
                .push((&edge.target, ei));
        }
    }
    let mut direct_flows = trace_from_root(
        graph,
        entry_node.id.as_str(),
        max_depth,
        &node_index,
        &forward_adj,
    );
    if !direct_flows.is_empty() {
        let summary = summarize_flows(&direct_flows);
        let total_flows = truncate_with_total(&mut direct_flows, options.limit);
        return Ok(TraceResult {
            entry: entry_node.id.clone(),
            requested_symbol: entry_node.name.clone(),
            traced_roots: vec![entry_node.name.clone()],
            fallback_used: false,
            hint: None,
            flows: direct_flows,
            total_flows,
            summary,
            entry_ref: SymbolRef::from_node(entry_node),
        });
    }

    let fallback_roots = fallback_trace_roots(graph, entry_node, &node_index, &forward_adj);
    let fallback_used = !fallback_roots.is_empty();
    let traced_roots: Vec<String> = if fallback_used {
        fallback_roots
            .iter()
            .map(|node| node.name.clone())
            .collect()
    } else {
        vec![entry_node.name.clone()]
    };

    let mut flows = Vec::new();
    for root in &fallback_roots {
        flows.extend(trace_from_root(
            graph,
            root.id.as_str(),
            max_depth,
            &node_index,
            &forward_adj,
        ));
    }

    let hint = if fallback_used && flows.is_empty() {
        Some("no dataflow edges were found from this symbol or its local SwiftUI roots".to_string())
    } else {
        None
    };

    let summary = summarize_flows(&flows);
    let total_flows = truncate_with_total(&mut flows, options.limit);
    Ok(TraceResult {
        entry: entry_node.id.clone(),
        requested_symbol: entry_node.name.clone(),
        traced_roots,
        fallback_used,
        hint,
        summary,
        flows,
        total_flows,
        entry_ref: SymbolRef::from_node(entry_node),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapha_core::graph::*;
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

    #[test]
    fn traces_entry_to_terminal() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                make_node("entry", Some(NodeRole::EntryPoint)),
                make_node("service", None),
                make_node(
                    "db_save",
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Persistence,
                    }),
                ),
            ],
            edges: vec![make_edge("entry", "service", EdgeKind::Calls), {
                let mut e = make_edge("service", "db_save", EdgeKind::Writes);
                e.direction = Some(FlowDirection::Write);
                e.operation = Some("save".to_string());
                e
            }],
        };

        let result = query_trace(&graph, "entry", 10).unwrap();
        assert_eq!(result.entry, "entry");
        assert_eq!(result.requested_symbol, "entry");
        assert_eq!(result.traced_roots, vec!["entry"]);
        assert!(!result.fallback_used);
        assert!(result.hint.is_none());
        assert_eq!(result.flows.len(), 1);

        let flow = &result.flows[0];
        assert_eq!(flow.path, vec!["entry", "service", "db_save"]);
        let terminal = flow.terminal.as_ref().unwrap();
        assert_eq!(terminal.kind, "persistence");
        assert_eq!(terminal.operation, "save");
        assert_eq!(terminal.direction, "write");

        assert_eq!(result.summary.total_flows, 1);
        assert_eq!(result.summary.writes, 1);
        assert_eq!(result.summary.reads, 0);
    }

    #[test]
    fn captures_conditions_on_edges() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                make_node("entry", Some(NodeRole::EntryPoint)),
                make_node(
                    "db",
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Persistence,
                    }),
                ),
            ],
            edges: vec![{
                let mut e = make_edge("entry", "db", EdgeKind::Writes);
                e.condition = Some("user.isAdmin".to_string());
                e.direction = Some(FlowDirection::Write);
                e.operation = Some("INSERT".to_string());
                e
            }],
        };

        let result = query_trace(&graph, "entry", 10).unwrap();
        assert_eq!(result.flows.len(), 1);
        assert_eq!(result.flows[0].conditions, vec!["user.isAdmin"]);
    }

    #[test]
    fn returns_none_for_unknown_entry() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![make_node("a", None)],
            edges: vec![],
        };
        assert!(matches!(
            query_trace(&graph, "nonexistent", 10),
            Err(QueryResolveError::NotFound { .. })
        ));
    }

    #[test]
    fn ignores_swiftui_structural_edges() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                Node {
                    id: "body".into(),
                    kind: NodeKind::Property,
                    name: "body".into(),
                    file: PathBuf::from("ContentView.swift"),
                    span: Span {
                        start: [0, 0],
                        end: [1, 0],
                    },
                    visibility: Visibility::Public,
                    metadata: StdHashMap::new(),
                    role: Some(NodeRole::EntryPoint),
                    signature: None,
                    doc_comment: None,
                    module: None,
                    snippet: None,
                    repo: None,
                },
                Node {
                    id: "body::view:VStack@1:0".into(),
                    kind: NodeKind::View,
                    name: "VStack".into(),
                    file: PathBuf::from("ContentView.swift"),
                    span: Span {
                        start: [1, 0],
                        end: [2, 0],
                    },
                    visibility: Visibility::Private,
                    metadata: StdHashMap::new(),
                    role: None,
                    signature: None,
                    doc_comment: None,
                    module: None,
                    snippet: None,
                    repo: None,
                },
                make_node(
                    "db_save",
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Persistence,
                    }),
                ),
            ],
            edges: vec![
                make_edge("body", "body::view:VStack@1:0", EdgeKind::Contains),
                make_edge("body::view:VStack@1:0", "db_save", EdgeKind::TypeRef),
            ],
        };

        let result = query_trace(&graph, "body", 10).unwrap();
        assert!(result.flows.is_empty());
        assert_eq!(result.summary.total_flows, 0);
        assert_eq!(result.requested_symbol, "body");
        assert_eq!(result.traced_roots, vec!["body"]);
        assert!(!result.fallback_used);
        assert!(result.hint.is_none());
    }

    #[test]
    fn respects_max_depth() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                make_node("a", Some(NodeRole::EntryPoint)),
                make_node("b", None),
                make_node("c", None),
                make_node(
                    "d",
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Network,
                    }),
                ),
            ],
            edges: vec![
                make_edge("a", "b", EdgeKind::Calls),
                make_edge("b", "c", EdgeKind::Calls),
                {
                    let mut e = make_edge("c", "d", EdgeKind::Reads);
                    e.direction = Some(FlowDirection::Read);
                    e.operation = Some("fetch".to_string());
                    e
                },
            ],
        };

        // max_depth=2 means path can be at most 3 nodes (entry + 2 hops)
        // a->b->c is 2 hops, but c->d is hop 3, so depth 1 won't reach d
        let result = query_trace(&graph, "a", 1).unwrap();
        assert_eq!(result.flows.len(), 0);

        // depth 3 should reach d
        let result = query_trace(&graph, "a", 5).unwrap();
        assert_eq!(result.flows.len(), 1);
    }

    #[test]
    fn captures_async_boundaries() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                make_node("entry", Some(NodeRole::EntryPoint)),
                make_node(
                    "api",
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Network,
                    }),
                ),
            ],
            edges: vec![{
                let mut e = make_edge("entry", "api", EdgeKind::Reads);
                e.async_boundary = Some(true);
                e.direction = Some(FlowDirection::Read);
                e.operation = Some("fetch".to_string());
                e
            }],
        };

        let result = query_trace(&graph, "entry", 10).unwrap();
        assert_eq!(result.summary.async_crossings, 1);
        assert_eq!(result.flows[0].async_boundaries.len(), 1);
    }

    #[test]
    fn trace_falls_back_to_contained_action_when_view_root_has_no_direct_flows() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                Node {
                    id: "room_view".into(),
                    kind: NodeKind::Struct,
                    name: "RoomPageCenterContentView".into(),
                    file: PathBuf::from("RoomPageCenterContentView.swift"),
                    span: Span {
                        start: [0, 0],
                        end: [1, 0],
                    },
                    visibility: Visibility::Public,
                    metadata: StdHashMap::new(),
                    role: None,
                    signature: None,
                    doc_comment: None,
                    module: None,
                    snippet: None,
                    repo: None,
                },
                Node {
                    id: "room_body".into(),
                    kind: NodeKind::Property,
                    name: "body".into(),
                    file: PathBuf::from("RoomPageCenterContentView.swift"),
                    span: Span {
                        start: [1, 0],
                        end: [4, 0],
                    },
                    visibility: Visibility::Public,
                    metadata: StdHashMap::new(),
                    role: None,
                    signature: Some("var body: some View".into()),
                    doc_comment: None,
                    module: None,
                    snippet: None,
                    repo: None,
                },
                Node {
                    id: "room_share".into(),
                    kind: NodeKind::Function,
                    name: "onShare()".into(),
                    file: PathBuf::from("RoomPageCenterContentView.swift"),
                    span: Span {
                        start: [5, 0],
                        end: [8, 0],
                    },
                    visibility: Visibility::Public,
                    metadata: StdHashMap::new(),
                    role: None,
                    signature: Some("func onShare()".into()),
                    doc_comment: None,
                    module: None,
                    snippet: None,
                    repo: None,
                },
                make_node(
                    "save_terminal",
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Persistence,
                    }),
                ),
            ],
            edges: vec![
                make_edge("room_view", "room_body", EdgeKind::Contains),
                make_edge("room_view", "room_share", EdgeKind::Contains),
                {
                    let mut e = make_edge("room_share", "save_terminal", EdgeKind::Writes);
                    e.direction = Some(FlowDirection::Write);
                    e.operation = Some("save".to_string());
                    e
                },
            ],
        };

        let result = query_trace(&graph, "RoomPageCenterContentView", 10).unwrap();
        assert_eq!(result.requested_symbol, "RoomPageCenterContentView");
        assert!(result.fallback_used);
        assert_eq!(result.traced_roots, vec!["body", "onShare()"]);
        assert!(result.hint.is_none());
        assert_eq!(result.summary.total_flows, 1);
        assert_eq!(result.flows.len(), 1);
        assert_eq!(
            result.flows[0].path,
            vec!["onShare()".to_string(), "save_terminal".to_string()]
        );
        assert_eq!(result.summary.writes, 1);
    }

    #[test]
    fn trace_returns_hint_when_fallback_roots_have_no_flows() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                Node {
                    id: "room_view".into(),
                    kind: NodeKind::Struct,
                    name: "RoomPageCenterContentView".into(),
                    file: PathBuf::from("RoomPageCenterContentView.swift"),
                    span: Span {
                        start: [0, 0],
                        end: [1, 0],
                    },
                    visibility: Visibility::Public,
                    metadata: StdHashMap::new(),
                    role: None,
                    signature: None,
                    doc_comment: None,
                    module: None,
                    snippet: None,
                    repo: None,
                },
                Node {
                    id: "room_body".into(),
                    kind: NodeKind::Property,
                    name: "body".into(),
                    file: PathBuf::from("RoomPageCenterContentView.swift"),
                    span: Span {
                        start: [1, 0],
                        end: [4, 0],
                    },
                    visibility: Visibility::Public,
                    metadata: StdHashMap::new(),
                    role: None,
                    signature: Some("var body: some View".into()),
                    doc_comment: None,
                    module: None,
                    snippet: None,
                    repo: None,
                },
            ],
            edges: vec![Edge {
                source: "room_view".into(),
                target: "room_body".into(),
                kind: EdgeKind::Contains,
                confidence: 1.0,
                direction: None,
                operation: None,
                condition: None,
                async_boundary: None,
                provenance: Vec::new(),
                repo: None,
            }],
        };

        let result = query_trace(&graph, "RoomPageCenterContentView", 10).unwrap();
        assert!(result.fallback_used);
        assert_eq!(result.traced_roots, vec!["body"]);
        assert_eq!(result.summary.total_flows, 0);
        assert_eq!(
            result.hint.as_deref(),
            Some("no dataflow edges were found from this symbol or its local SwiftUI roots")
        );
    }

    #[test]
    fn trace_falls_back_to_swiftui_implementors_and_body_getter_roots() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                Node {
                    id: "room_view".into(),
                    kind: NodeKind::Struct,
                    name: "RoomPageHeaderView".into(),
                    file: PathBuf::from("RoomPage+Layout.swift"),
                    span: Span {
                        start: [0, 0],
                        end: [1, 0],
                    },
                    visibility: Visibility::Public,
                    metadata: StdHashMap::new(),
                    role: None,
                    signature: None,
                    doc_comment: None,
                    module: Some("Room".into()),
                    snippet: None,
                    repo: None,
                },
                Node {
                    id: "room_body".into(),
                    kind: NodeKind::Property,
                    name: "body".into(),
                    file: PathBuf::from("RoomPage+Layout.swift"),
                    span: Span {
                        start: [1, 0],
                        end: [6, 0],
                    },
                    visibility: Visibility::Public,
                    metadata: StdHashMap::new(),
                    role: None,
                    signature: Some("var body: some View".into()),
                    doc_comment: None,
                    module: Some("Room".into()),
                    snippet: None,
                    repo: None,
                },
                Node {
                    id: "room_body_getter".into(),
                    kind: NodeKind::Function,
                    name: "getter:body".into(),
                    file: PathBuf::from("RoomPage+Layout.swift"),
                    span: Span {
                        start: [1, 0],
                        end: [6, 0],
                    },
                    visibility: Visibility::Public,
                    metadata: StdHashMap::new(),
                    role: None,
                    signature: Some("getter:body".into()),
                    doc_comment: None,
                    module: Some("Room".into()),
                    snippet: None,
                    repo: None,
                },
                Node {
                    id: "room_share".into(),
                    kind: NodeKind::Function,
                    name: "onShare()".into(),
                    file: PathBuf::from("RoomPage+Layout.swift"),
                    span: Span {
                        start: [7, 0],
                        end: [10, 0],
                    },
                    visibility: Visibility::Public,
                    metadata: StdHashMap::new(),
                    role: None,
                    signature: Some("func onShare()".into()),
                    doc_comment: None,
                    module: Some("Room".into()),
                    snippet: None,
                    repo: None,
                },
                make_node(
                    "cache_terminal",
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Cache,
                    }),
                ),
            ],
            edges: vec![
                Edge {
                    source: "room_body".into(),
                    target: "room_view".into(),
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
                    source: "room_share".into(),
                    target: "room_view".into(),
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
                    source: "room_view".into(),
                    target: "room_body_getter".into(),
                    kind: EdgeKind::TypeRef,
                    confidence: 1.0,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: Vec::new(),
                    repo: None,
                },
                {
                    let mut e = make_edge("room_share", "cache_terminal", EdgeKind::Reads);
                    e.direction = Some(FlowDirection::Read);
                    e.operation = Some("resource".to_string());
                    e
                },
            ],
        };

        let result = query_trace(&graph, "RoomPageHeaderView", 10).unwrap();
        assert!(result.fallback_used);
        assert_eq!(
            result.traced_roots,
            vec![
                "body".to_string(),
                "getter:body".to_string(),
                "onShare()".to_string()
            ]
        );
        assert_eq!(result.summary.total_flows, 1);
        assert_eq!(result.summary.reads, 1);
        assert!(result.hint.is_none());
    }

    #[test]
    fn trace_truncates_flows_and_preserves_summary() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                make_node("entry", Some(NodeRole::EntryPoint)),
                make_node(
                    "terminal_a",
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Persistence,
                    }),
                ),
                make_node(
                    "terminal_b",
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Network,
                    }),
                ),
                make_node(
                    "terminal_c",
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Cache,
                    }),
                ),
            ],
            edges: vec![
                {
                    let mut e = make_edge("entry", "terminal_a", EdgeKind::Writes);
                    e.direction = Some(FlowDirection::Write);
                    e.operation = Some("save".to_string());
                    e
                },
                {
                    let mut e = make_edge("entry", "terminal_b", EdgeKind::Reads);
                    e.direction = Some(FlowDirection::Read);
                    e.operation = Some("fetch".to_string());
                    e
                },
                {
                    let mut e = make_edge("entry", "terminal_c", EdgeKind::Reads);
                    e.direction = Some(FlowDirection::Read);
                    e.operation = Some("lookup".to_string());
                    e
                },
            ],
        };

        let truncated =
            query_trace_with_options(&graph, "entry", 10, &TraceQueryOptions { limit: 1 }).unwrap();
        assert_eq!(truncated.flows.len(), 1, "flows should be truncated");
        assert_eq!(truncated.total_flows, 3);
        // Summary is computed from the pre-truncation flow set, so total_flows
        // there must remain at the un-truncated count.
        assert_eq!(truncated.summary.total_flows, 3);
        // Read/write counts also reflect the pre-truncation flow set.
        assert_eq!(truncated.summary.reads, 2);
        assert_eq!(truncated.summary.writes, 1);

        let unbounded =
            query_trace_with_options(&graph, "entry", 10, &TraceQueryOptions::default()).unwrap();
        assert_eq!(unbounded.flows.len(), 3);
        assert_eq!(unbounded.total_flows, 3);
        assert_eq!(unbounded.summary.total_flows, 3);
    }
}
