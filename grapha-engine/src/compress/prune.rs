use grapha_core::graph::{Edge, EdgeKind, Graph, Node, NodeKind, Visibility};
use std::collections::HashSet;

/// Prune a graph for LLM consumption:
/// 1. Preserve ownership edges so grouped output keeps symbol structure
/// 2. Drop Uses edges with raw text targets (not graph-traversable)
/// 3. Optionally drop private leaf nodes (fields, variants)
pub fn prune(graph: Graph, keep_private_leaves: bool) -> Graph {
    let kept_nodes: Vec<Node> = if keep_private_leaves {
        graph.nodes
    } else {
        graph
            .nodes
            .into_iter()
            .filter(|n| {
                !matches!(n.kind, NodeKind::Field | NodeKind::Variant)
                    || n.visibility == Visibility::Public
            })
            .collect()
    };

    let kept_ids: HashSet<&str> = kept_nodes.iter().map(|n| n.id.as_str()).collect();
    let edges: Vec<Edge> = graph
        .edges
        .into_iter()
        .filter(|e| {
            if e.kind == EdgeKind::Uses && !kept_ids.contains(e.target.as_str()) {
                return false;
            }
            kept_ids.contains(e.source.as_str())
                && (kept_ids.contains(e.target.as_str()) || e.kind == EdgeKind::Uses)
        })
        .collect();

    Graph {
        version: graph.version,
        nodes: kept_nodes,
        edges,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapha_core::graph::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn make_node(id: &str, kind: NodeKind, vis: Visibility) -> Node {
        Node {
            id: id.to_string(),
            kind,
            name: id.to_string(),
            file: PathBuf::from("test.rs"),
            span: Span {
                start: [0, 0],
                end: [1, 0],
            },
            visibility: vis,
            metadata: HashMap::new(),
            role: None,
            signature: None,
            doc_comment: None,
            module: None,
            snippet: None,
            repo: None,
        }
    }

    #[test]
    fn prune_keeps_contains_edges_between_kept_nodes() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                make_node("a", NodeKind::Struct, Visibility::Public),
                make_node("b", NodeKind::Function, Visibility::Public),
            ],
            edges: vec![
                Edge {
                    source: "a".into(),
                    target: "b".into(),
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
                    source: "b".into(),
                    target: "a".into(),
                    kind: EdgeKind::Calls,
                    confidence: 0.8,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: Vec::new(),
                    repo: None,
                },
            ],
        };
        let pruned = prune(graph, true);
        assert_eq!(pruned.edges.len(), 2);
        assert!(
            pruned
                .edges
                .iter()
                .any(|edge| edge.kind == EdgeKind::Contains)
        );
        assert!(pruned.edges.iter().any(|edge| edge.kind == EdgeKind::Calls));
    }

    #[test]
    fn prune_drops_unresolved_uses_edges() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![make_node("a", NodeKind::Function, Visibility::Public)],
            edges: vec![Edge {
                source: "file.rs".into(),
                target: "use std::collections::HashMap;".into(),
                kind: EdgeKind::Uses,
                confidence: 0.7,
                direction: None,
                operation: None,
                condition: None,
                async_boundary: None,
                provenance: Vec::new(),
                repo: None,
            }],
        };
        let pruned = prune(graph, true);
        assert_eq!(pruned.edges.len(), 0);
    }

    #[test]
    fn prune_drops_private_leaves_when_requested() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                make_node("s", NodeKind::Struct, Visibility::Public),
                make_node("f1", NodeKind::Field, Visibility::Private),
                make_node("f2", NodeKind::Field, Visibility::Public),
            ],
            edges: vec![],
        };
        let pruned = prune(graph, false);
        assert_eq!(pruned.nodes.len(), 2);
    }

    #[test]
    fn prune_keeps_private_leaves_when_requested() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                make_node("s", NodeKind::Struct, Visibility::Public),
                make_node("f1", NodeKind::Field, Visibility::Private),
            ],
            edges: vec![],
        };
        let pruned = prune(graph, true);
        assert_eq!(pruned.nodes.len(), 2);
    }

    #[test]
    fn prune_keeps_swiftui_contains_edges() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                make_node("body", NodeKind::Property, Visibility::Public),
                make_node("vstack", NodeKind::View, Visibility::Private),
                make_node("text", NodeKind::View, Visibility::Private),
            ],
            edges: vec![
                Edge {
                    source: "body".into(),
                    target: "vstack".into(),
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
                    source: "vstack".into(),
                    target: "text".into(),
                    kind: EdgeKind::Contains,
                    confidence: 1.0,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: Vec::new(),
                    repo: None,
                },
            ],
        };

        let pruned = prune(graph, true);
        assert_eq!(pruned.edges.len(), 2);
        assert!(
            pruned
                .edges
                .iter()
                .all(|edge| edge.kind == EdgeKind::Contains)
        );
    }
}
