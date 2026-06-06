use std::collections::HashSet;

use grapha_core::graph::{Graph, NodeKind};

/// Parse a comma-separated filter string into a set of `NodeKind`s.
pub fn parse_filter(filter: &str) -> anyhow::Result<HashSet<NodeKind>> {
    let mut kinds = HashSet::new();
    for part in filter.split(',') {
        let kind = match part.trim() {
            "file" => NodeKind::File,
            "fn" | "function" => NodeKind::Function,
            "method" => NodeKind::Method,
            "class" => NodeKind::Class,
            "struct" => NodeKind::Struct,
            "enum" => NodeKind::Enum,
            "enum_member" | "variant_member" => NodeKind::EnumMember,
            "trait" => NodeKind::Trait,
            "impl" => NodeKind::Impl,
            "mod" | "module" => NodeKind::Module,
            "namespace" => NodeKind::Namespace,
            "field" => NodeKind::Field,
            "variant" => NodeKind::Variant,
            "property" => NodeKind::Property,
            "var" | "variable" => NodeKind::Variable,
            "const" | "constant" => NodeKind::Constant,
            "typealias" | "type_alias" => NodeKind::TypeAlias,
            "parameter" | "param" => NodeKind::Parameter,
            "import" => NodeKind::Import,
            "export" => NodeKind::Export,
            "route" => NodeKind::Route,
            "component" => NodeKind::Component,
            "protocol" => NodeKind::Protocol,
            "extension" | "ext" => NodeKind::Extension,
            "view" => NodeKind::View,
            "branch" => NodeKind::Branch,
            other => anyhow::bail!("unknown node kind: '{other}'"),
        };
        kinds.insert(kind);
    }
    Ok(kinds)
}

/// Filter a graph to only include nodes of the given kinds.
/// Prunes edges that reference removed nodes.
pub fn filter_graph(graph: Graph, kinds: &HashSet<NodeKind>) -> Graph {
    let kept_ids: HashSet<String> = graph
        .nodes
        .iter()
        .filter(|n| kinds.contains(&n.kind))
        .map(|n| n.id.clone())
        .collect();

    let nodes = graph
        .nodes
        .into_iter()
        .filter(|n| kinds.contains(&n.kind))
        .collect();

    let edges = graph
        .edges
        .into_iter()
        .filter(|e| kept_ids.contains(&e.source) && kept_ids.contains(&e.target))
        .collect();

    Graph {
        version: graph.version,
        nodes,
        edges,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapha_core::graph::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn make_node(id: &str, kind: NodeKind) -> Node {
        Node {
            id: id.to_string(),
            kind,
            name: id.to_string(),
            file: PathBuf::from("test.rs"),
            span: Span {
                start: [0, 0],
                end: [0, 0],
            },
            visibility: Visibility::Public,
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
    fn parse_filter_parses_valid_kinds() {
        let kinds = parse_filter("fn,struct").unwrap();
        assert!(kinds.contains(&NodeKind::Function));
        assert!(kinds.contains(&NodeKind::Struct));
        assert_eq!(kinds.len(), 2);
    }

    #[test]
    fn parse_filter_rejects_unknown_kind() {
        let result = parse_filter("fn,bogus");
        assert!(result.is_err());
    }

    #[test]
    fn filter_keeps_only_matching_nodes() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                make_node("a", NodeKind::Function),
                make_node("b", NodeKind::Struct),
                make_node("c", NodeKind::Enum),
            ],
            edges: vec![],
        };
        let mut kinds = HashSet::new();
        kinds.insert(NodeKind::Function);
        let filtered = filter_graph(graph, &kinds);
        assert_eq!(filtered.nodes.len(), 1);
        assert_eq!(filtered.nodes[0].id, "a");
    }

    #[test]
    fn filter_prunes_orphaned_edges() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                make_node("a", NodeKind::Function),
                make_node("b", NodeKind::Struct),
            ],
            edges: vec![
                Edge {
                    source: "a".to_string(),
                    target: "b".to_string(),
                    kind: EdgeKind::TypeRef,
                    confidence: 1.0,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: Vec::new(),
                    repo: None,
                },
                Edge {
                    source: "a".to_string(),
                    target: "a".to_string(),
                    kind: EdgeKind::Calls,
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
        let mut kinds = HashSet::new();
        kinds.insert(NodeKind::Function);
        let filtered = filter_graph(graph, &kinds);
        assert_eq!(filtered.edges.len(), 1);
        assert_eq!(filtered.edges[0].kind, EdgeKind::Calls);
    }
}
