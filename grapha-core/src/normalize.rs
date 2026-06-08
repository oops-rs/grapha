use std::collections::HashMap;

use crate::graph::{Edge, EdgeKind, FlowDirection, Graph, Node, NodeKind, Visibility};

pub fn normalize_graph(mut graph: Graph) -> Graph {
    fn visibility_rank(visibility: &Visibility) -> u8 {
        match visibility {
            Visibility::Private => 0,
            Visibility::Crate => 1,
            Visibility::Public => 2,
        }
    }

    fn merged_kind(existing: NodeKind, incoming: NodeKind) -> NodeKind {
        match (existing, incoming) {
            (NodeKind::Struct, NodeKind::Class) => NodeKind::Class,
            _ => existing,
        }
    }

    fn merge_node(existing: &mut Node, incoming: Node) {
        existing.kind = merged_kind(existing.kind, incoming.kind);
        if visibility_rank(&incoming.visibility) > visibility_rank(&existing.visibility) {
            existing.visibility = incoming.visibility;
        }
        if existing.role.is_none() {
            existing.role = incoming.role;
        }
        if existing.signature.is_none() {
            existing.signature = incoming.signature;
        }
        if existing.doc_comment.is_none() {
            existing.doc_comment = incoming.doc_comment;
        }
        if existing.module.is_none() {
            existing.module = incoming.module;
        }
        if existing.repo.is_none() {
            existing.repo = incoming.repo;
        }
        for (key, value) in incoming.metadata {
            existing.metadata.entry(key).or_insert(value);
        }
    }

    let mut node_index = HashMap::new();
    let mut normalized_nodes = Vec::with_capacity(graph.nodes.len());
    for node in graph.nodes {
        if let Some(existing_index) = node_index.get(&node.id).copied() {
            merge_node(&mut normalized_nodes[existing_index], node);
        } else {
            node_index.insert(node.id.clone(), normalized_nodes.len());
            normalized_nodes.push(node);
        }
    }

    let mut edge_index = HashMap::new();
    let mut normalized_edges = Vec::with_capacity(graph.edges.len());
    for edge in graph.edges {
        let fingerprint = edge_fingerprint(&edge);
        if let Some(existing_index) = edge_index.get(&fingerprint).copied() {
            let existing: &mut Edge = &mut normalized_edges[existing_index];
            existing.confidence = existing.confidence.max(edge.confidence);
            for provenance in edge.provenance {
                if !existing
                    .provenance
                    .iter()
                    .any(|current| current == &provenance)
                {
                    existing.provenance.push(provenance);
                }
            }
        } else {
            edge_index.insert(fingerprint, normalized_edges.len());
            normalized_edges.push(edge);
        }
    }

    graph.nodes = normalized_nodes;
    graph.edges = normalized_edges;
    graph
}

pub fn edge_fingerprint(edge: &Edge) -> String {
    let mut hasher = Fnv1a64::default();
    hasher.write_component(&edge.source);
    hasher.write_component(&edge.target);
    hasher.write_component(edge_kind_tag(edge.kind));
    hasher.write_component(direction_tag(edge.direction.as_ref()));
    hasher.write_component(edge.operation.as_deref().unwrap_or(""));
    hasher.write_component(edge.condition.as_deref().unwrap_or(""));
    hasher.write_component(bool_tag(edge.async_boundary));
    hasher.write_component(edge.repo.as_deref().unwrap_or(""));
    // Fast hex encoding without format! allocation overhead
    let hash = hasher.finish();
    let mut buf = [0u8; 16];
    let bytes = hash.to_be_bytes();
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for (i, &b) in bytes.iter().enumerate() {
        buf[i * 2] = HEX[(b >> 4) as usize];
        buf[i * 2 + 1] = HEX[(b & 0xf) as usize];
    }
    // SAFETY: buf only contains ASCII hex chars
    unsafe { String::from_utf8_unchecked(buf.to_vec()) }
}

fn edge_kind_tag(kind: EdgeKind) -> &'static str {
    match kind {
        EdgeKind::Calls => "calls",
        EdgeKind::Uses => "uses",
        EdgeKind::Imports => "imports",
        EdgeKind::Exports => "exports",
        EdgeKind::Implements => "implements",
        EdgeKind::Contains => "contains",
        EdgeKind::TypeRef => "type_ref",
        EdgeKind::References => "references",
        EdgeKind::TypeOf => "type_of",
        EdgeKind::Returns => "returns",
        EdgeKind::Inherits => "inherits",
        EdgeKind::Extends => "extends",
        EdgeKind::Instantiates => "instantiates",
        EdgeKind::Overrides => "overrides",
        EdgeKind::Decorates => "decorates",
        EdgeKind::Reads => "reads",
        EdgeKind::Writes => "writes",
        EdgeKind::Publishes => "publishes",
        EdgeKind::Subscribes => "subscribes",
        EdgeKind::DependsOn => "depends_on",
    }
}

fn direction_tag(direction: Option<&FlowDirection>) -> &'static str {
    match direction {
        Some(FlowDirection::Read) => "read",
        Some(FlowDirection::Write) => "write",
        Some(FlowDirection::ReadWrite) => "read_write",
        Some(FlowDirection::Pure) => "pure",
        None => "",
    }
}

fn bool_tag(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "1",
        Some(false) => "0",
        None => "",
    }
}

#[derive(Default)]
struct Fnv1a64 {
    state: u64,
}

impl Fnv1a64 {
    const OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;

    fn write_component(&mut self, value: &str) {
        if self.state == 0 {
            self.state = Self::OFFSET_BASIS;
        }
        for byte in value.as_bytes() {
            self.state ^= u64::from(*byte);
            self.state = self.state.wrapping_mul(Self::PRIME);
        }
        self.state ^= u64::from(0xff_u8);
        self.state = self.state.wrapping_mul(Self::PRIME);
    }

    fn finish(self) -> u64 {
        if self.state == 0 {
            Self::OFFSET_BASIS
        } else {
            self.state
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{EdgeKind, EdgeProvenance, NodeKind, NodeRole, Span, TerminalKind};
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[test]
    fn normalize_graph_merges_duplicate_edges_and_provenance() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![],
            edges: vec![
                Edge {
                    source: "a".to_string(),
                    target: "b".to_string(),
                    kind: EdgeKind::Calls,
                    confidence: 0.4,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: vec![EdgeProvenance {
                        file: PathBuf::from("a.swift"),
                        span: Span {
                            start: [1, 0],
                            end: [1, 4],
                        },
                        symbol_id: "a".to_string(),
                    }],
                    repo: None,
                },
                Edge {
                    source: "a".to_string(),
                    target: "b".to_string(),
                    kind: EdgeKind::Calls,
                    confidence: 0.9,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: vec![EdgeProvenance {
                        file: PathBuf::from("a.swift"),
                        span: Span {
                            start: [2, 0],
                            end: [2, 4],
                        },
                        symbol_id: "a".to_string(),
                    }],
                    repo: None,
                },
            ],
        };

        let normalized = normalize_graph(graph);
        assert_eq!(normalized.edges.len(), 1);
        assert_eq!(normalized.edges[0].confidence, 0.9);
        assert_eq!(normalized.edges[0].provenance.len(), 2);
    }

    #[test]
    fn normalize_graph_merges_duplicate_nodes_by_id() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                Node {
                    id: "s:RoomPage.centerContentView".to_string(),
                    kind: NodeKind::Property,
                    name: "centerContentView".to_string(),
                    file: PathBuf::from("RoomPage.swift"),
                    span: Span {
                        start: [0, 0],
                        end: [0, 0],
                    },
                    visibility: Visibility::Private,
                    metadata: HashMap::new(),
                    role: None,
                    signature: None,
                    doc_comment: None,
                    module: None,
                    snippet: None,
                    repo: None,
                },
                Node {
                    id: "s:RoomPage.centerContentView".to_string(),
                    kind: NodeKind::Property,
                    name: "centerContentView".to_string(),
                    file: PathBuf::from("RoomPage.swift"),
                    span: Span {
                        start: [10, 4],
                        end: [10, 20],
                    },
                    visibility: Visibility::Public,
                    metadata: HashMap::new(),
                    role: Some(NodeRole::EntryPoint),
                    signature: Some("var centerContentView: some View".to_string()),
                    doc_comment: Some("helper".to_string()),
                    module: Some("Room".to_string()),
                    snippet: None,
                    repo: None,
                },
            ],
            edges: vec![],
        };

        let normalized = normalize_graph(graph);
        assert_eq!(normalized.nodes.len(), 1);
        assert_eq!(normalized.nodes[0].visibility, Visibility::Public);
        assert_eq!(normalized.nodes[0].role, Some(NodeRole::EntryPoint));
        assert_eq!(
            normalized.nodes[0].signature.as_deref(),
            Some("var centerContentView: some View")
        );
        assert_eq!(normalized.nodes[0].doc_comment.as_deref(), Some("helper"));
        assert_eq!(normalized.nodes[0].module.as_deref(), Some("Room"));
    }

    #[test]
    fn normalize_graph_prefers_class_over_struct_for_same_symbol() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                Node {
                    id: "AppDelegate".to_string(),
                    kind: NodeKind::Struct,
                    name: "AppDelegate".to_string(),
                    file: PathBuf::from("AppDelegate.swift"),
                    span: Span {
                        start: [0, 0],
                        end: [1, 0],
                    },
                    visibility: Visibility::Crate,
                    metadata: HashMap::new(),
                    role: None,
                    signature: None,
                    doc_comment: None,
                    module: None,
                    snippet: None,
                    repo: None,
                },
                Node {
                    id: "AppDelegate".to_string(),
                    kind: NodeKind::Class,
                    name: "AppDelegate".to_string(),
                    file: PathBuf::from("AppDelegate.swift"),
                    span: Span {
                        start: [0, 0],
                        end: [1, 0],
                    },
                    visibility: Visibility::Crate,
                    metadata: HashMap::new(),
                    role: None,
                    signature: None,
                    doc_comment: None,
                    module: None,
                    snippet: None,
                    repo: None,
                },
            ],
            edges: vec![],
        };

        let normalized = normalize_graph(graph);
        assert_eq!(normalized.nodes.len(), 1);
        assert_eq!(normalized.nodes[0].kind, NodeKind::Class);
    }

    #[test]
    fn fingerprint_changes_when_effect_shape_changes() {
        let base = Edge {
            source: "a".to_string(),
            target: "b".to_string(),
            kind: EdgeKind::Calls,
            confidence: 1.0,
            direction: None,
            operation: None,
            condition: None,
            async_boundary: None,
            provenance: Vec::new(),
            repo: None,
        };
        let mut changed = base.clone();
        changed.direction = Some(FlowDirection::Read);

        assert_ne!(edge_fingerprint(&base), edge_fingerprint(&changed));
    }

    #[test]
    fn fingerprint_ignores_confidence_and_provenance() {
        let base = Edge {
            source: "a".to_string(),
            target: "b".to_string(),
            kind: EdgeKind::Calls,
            confidence: 0.2,
            direction: Some(FlowDirection::Read),
            operation: Some("HTTP".to_string()),
            condition: None,
            async_boundary: None,
            provenance: Vec::new(),
            repo: None,
        };
        let mut changed = base.clone();
        changed.confidence = 0.9;
        changed.provenance = vec![EdgeProvenance {
            file: PathBuf::from("a.swift"),
            span: Span {
                start: [1, 0],
                end: [1, 2],
            },
            symbol_id: "a".to_string(),
        }];

        assert_eq!(edge_fingerprint(&base), edge_fingerprint(&changed));
    }

    #[test]
    fn terminal_role_is_preserved_by_normalization() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![Node {
                id: "terminal".to_string(),
                kind: NodeKind::Function,
                name: "terminal".to_string(),
                file: PathBuf::from("main.rs"),
                span: Span {
                    start: [0, 0],
                    end: [1, 0],
                },
                visibility: Visibility::Public,
                metadata: HashMap::new(),
                role: Some(NodeRole::Terminal {
                    kind: TerminalKind::Network,
                }),
                signature: None,
                doc_comment: None,
                module: None,
                snippet: None,
                repo: None,
            }],
            edges: vec![],
        };

        let normalized = normalize_graph(graph);
        assert_eq!(
            normalized.nodes[0].role,
            Some(NodeRole::Terminal {
                kind: TerminalKind::Network,
            })
        );
    }
}
