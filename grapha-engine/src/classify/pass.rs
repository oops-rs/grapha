use std::collections::HashMap;
use std::path::PathBuf;

use crate::classify::{ClassifyContext, CompositeClassifier};
use grapha_core::graph::{EdgeKind, FlowDirection, Graph, NodeRole, TerminalKind};

/// Extract the module name from a Swift USR string.
/// USR format: `s:<len><ModuleName><rest>` where len is the character count of the module name.
fn module_from_usr(usr: &str) -> Option<&str> {
    let rest = usr.strip_prefix("s:")?;
    let len_end = rest.find(|c: char| !c.is_ascii_digit())?;
    let len: usize = rest[..len_end].parse().ok()?;
    let name_start = len_end;
    if name_start + len <= rest.len() {
        Some(&rest[name_start..name_start + len])
    } else {
        None
    }
}

/// Classify a call edge target by its module (for USR-based index store data).
fn classify_by_module(target_usr: &str) -> Option<(TerminalKind, FlowDirection, String)> {
    let module = module_from_usr(target_usr)?;
    match module {
        // Network
        "FrameNetwork" | "FrameNetworkCore" | "Moya" | "Alamofire" => {
            let dir = FlowDirection::ReadWrite;
            Some((TerminalKind::Network, dir, "request".to_string()))
        }
        // Persistence — database
        "FrameStorage"
        | "FrameStorageCore"
        | "FrameStorageDatabase"
        | "GRDB"
        | "CoreData"
        | "RealmSwift" => Some((
            TerminalKind::Persistence,
            FlowDirection::ReadWrite,
            "db".to_string(),
        )),
        // Persistence — file/download
        "FrameDownload" | "Tiercel" => Some((
            TerminalKind::Persistence,
            FlowDirection::Write,
            "download".to_string(),
        )),
        // Cache / resources
        "FrameResources" | "AppResource" | "Kingfisher" | "SDWebImageSwiftUI" | "FrameImage" => {
            Some((
                TerminalKind::Cache,
                FlowDirection::Read,
                "resource".to_string(),
            ))
        }
        // Events / WebSocket
        "FrameWebView" | "WEKit" => Some((
            TerminalKind::Event,
            FlowDirection::ReadWrite,
            "webview".to_string(),
        )),
        // Statistics / analytics
        "FrameStat" => Some((
            TerminalKind::Event,
            FlowDirection::Write,
            "stat".to_string(),
        )),
        // Media
        "FrameMedia" | "FrameMediaShared" => Some((
            TerminalKind::Cache,
            FlowDirection::ReadWrite,
            "media".to_string(),
        )),
        // Router
        "FrameRouter" => Some((
            TerminalKind::Event,
            FlowDirection::Write,
            "navigate".to_string(),
        )),
        _ => None,
    }
}

pub fn classify_graph(graph: &Graph, classifier: &CompositeClassifier) -> Graph {
    let node_file_map: HashMap<&str, &PathBuf> = graph
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), &n.file))
        .collect();

    let node_ids: std::collections::HashSet<&str> =
        graph.nodes.iter().map(|n| n.id.as_str()).collect();
    let mut terminal_nodes: HashMap<String, grapha_core::graph::TerminalKind> = HashMap::new();

    let new_edges: Vec<_> = graph
        .edges
        .iter()
        .map(|edge| {
            if edge.kind != EdgeKind::Calls {
                return edge.clone();
            }

            let source_file = node_file_map
                .get(edge.source.as_str())
                .cloned()
                .cloned()
                .unwrap_or_default();

            let context = ClassifyContext {
                source_node: edge.source.clone(),
                file: source_file,
                arguments: vec![],
            };

            // Try string-based classifier first, then USR module-based fallback
            if let Some(classification) = classifier.classify(&edge.target, &context) {
                terminal_nodes.insert(edge.target.clone(), classification.terminal_kind);
                let mut new_edge = edge.clone();
                new_edge.direction = Some(classification.direction);
                new_edge.operation = Some(classification.operation);
                new_edge
            } else if let Some((kind, direction, operation)) = classify_by_module(&edge.target) {
                // For external framework calls, mark the SOURCE (local function) as terminal
                // since the target is an external symbol not in our graph
                if !node_ids.contains(edge.target.as_str()) {
                    terminal_nodes.entry(edge.source.clone()).or_insert(kind);
                } else {
                    terminal_nodes.insert(edge.target.clone(), kind);
                }
                let mut new_edge = edge.clone();
                new_edge.direction = Some(direction);
                new_edge.operation = Some(operation);
                new_edge
            } else {
                edge.clone()
            }
        })
        .collect();

    // Detect entry points from protocol conformances (index store data)
    let mut entry_point_nodes: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Detect entry points from protocol conformances (works for index store USR-based data)
    // SwiftUI View body, App body, Observable/ObservableObject public methods
    let entry_patterns: &[&str] = &[
        "SwiftUI",           // SwiftUI.View, SwiftUI.App conformances
        "ObservableObjectP", // Combine.ObservableObject
        "10ObservableP",     // Observation.Observable (@Observable)
    ];

    for edge in &graph.edges {
        if edge.kind == EdgeKind::Implements
            && entry_patterns.iter().any(|p| edge.target.contains(p))
        {
            entry_point_nodes.insert(edge.source.clone());
        }
    }

    // For tree-sitter data: fn main, #[test], View.body are already detected by extractors

    let new_nodes: Vec<_> = graph
        .nodes
        .iter()
        .map(|node| {
            if let Some(kind) = terminal_nodes.get(&node.id) {
                let mut new_node = node.clone();
                new_node.role = Some(NodeRole::Terminal { kind: *kind });
                new_node
            } else if entry_point_nodes.contains(&node.id) && node.role.is_none() {
                let mut new_node = node.clone();
                new_node.role = Some(NodeRole::EntryPoint);
                new_node
            } else {
                node.clone()
            }
        })
        .collect();

    Graph {
        version: graph.version.clone(),
        nodes: new_nodes,
        edges: new_edges,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::{Classification, Classifier};
    use grapha_core::graph::*;
    use std::collections::HashMap;

    struct MockClassifier;

    impl Classifier for MockClassifier {
        fn classify(
            &self,
            call_target: &str,
            _context: &crate::classify::ClassifyContext,
        ) -> Option<Classification> {
            if call_target.contains("fetch") {
                Some(Classification {
                    terminal_kind: TerminalKind::Network,
                    direction: FlowDirection::Read,
                    operation: "HTTP_GET".to_string(),
                })
            } else {
                None
            }
        }
    }

    fn test_graph() -> Graph {
        Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                Node {
                    id: "src::caller".to_string(),
                    kind: NodeKind::Function,
                    name: "caller".to_string(),
                    file: PathBuf::from("src/main.rs"),
                    span: Span {
                        start: [0, 0],
                        end: [5, 0],
                    },
                    visibility: Visibility::Public,
                    metadata: HashMap::new(),
                    role: None,
                    signature: None,
                    doc_comment: None,
                    module: None,
                    snippet: None,
                    repo: None,
                },
                Node {
                    id: "api::fetch_data".to_string(),
                    kind: NodeKind::Function,
                    name: "fetch_data".to_string(),
                    file: PathBuf::from("src/api.rs"),
                    span: Span {
                        start: [0, 0],
                        end: [10, 0],
                    },
                    visibility: Visibility::Public,
                    metadata: HashMap::new(),
                    role: None,
                    signature: None,
                    doc_comment: None,
                    module: None,
                    snippet: None,
                    repo: None,
                },
                Node {
                    id: "util::helper".to_string(),
                    kind: NodeKind::Function,
                    name: "helper".to_string(),
                    file: PathBuf::from("src/util.rs"),
                    span: Span {
                        start: [0, 0],
                        end: [3, 0],
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
            ],
            edges: vec![
                Edge {
                    source: "src::caller".to_string(),
                    target: "api::fetch_data".to_string(),
                    kind: EdgeKind::Calls,
                    confidence: 0.9,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: Vec::new(),
                    repo: None,
                },
                Edge {
                    source: "src::caller".to_string(),
                    target: "util::helper".to_string(),
                    kind: EdgeKind::Calls,
                    confidence: 0.9,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: Vec::new(),
                    repo: None,
                },
                Edge {
                    source: "src::caller".to_string(),
                    target: "api::fetch_data".to_string(),
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
        }
    }

    #[test]
    fn enriches_matching_calls_edges() {
        let graph = test_graph();
        let classifier = CompositeClassifier::new(vec![Box::new(MockClassifier)]);
        let result = classify_graph(&graph, &classifier);

        // The first Calls edge targets "fetch_data" which contains "fetch"
        let calls_edge = &result.edges[0];
        assert_eq!(calls_edge.direction, Some(FlowDirection::Read));
        assert_eq!(calls_edge.operation.as_deref(), Some("HTTP_GET"));

        // The second Calls edge targets "helper" — no match
        let helper_edge = &result.edges[1];
        assert!(helper_edge.direction.is_none());
        assert!(helper_edge.operation.is_none());
    }

    #[test]
    fn marks_terminal_nodes() {
        let graph = test_graph();
        let classifier = CompositeClassifier::new(vec![Box::new(MockClassifier)]);
        let result = classify_graph(&graph, &classifier);

        // "api::fetch_data" should be marked as terminal
        let fetch_node = result
            .nodes
            .iter()
            .find(|n| n.id == "api::fetch_data")
            .unwrap();
        assert_eq!(
            fetch_node.role,
            Some(NodeRole::Terminal {
                kind: TerminalKind::Network,
            })
        );

        // Others should not be marked
        let caller_node = result.nodes.iter().find(|n| n.id == "src::caller").unwrap();
        assert!(caller_node.role.is_none());

        let helper_node = result
            .nodes
            .iter()
            .find(|n| n.id == "util::helper")
            .unwrap();
        assert!(helper_node.role.is_none());
    }

    #[test]
    fn skips_non_calls_edges() {
        let graph = test_graph();
        let classifier = CompositeClassifier::new(vec![Box::new(MockClassifier)]);
        let result = classify_graph(&graph, &classifier);

        // The Contains edge should be untouched
        let contains_edge = &result.edges[2];
        assert_eq!(contains_edge.kind, EdgeKind::Contains);
        assert!(contains_edge.direction.is_none());
        assert!(contains_edge.operation.is_none());
    }

    #[test]
    fn preserves_graph_version() {
        let graph = test_graph();
        let classifier = CompositeClassifier::new(vec![]);
        let result = classify_graph(&graph, &classifier);
        assert_eq!(result.version, "0.1.0");
    }

    #[test]
    fn empty_classifier_leaves_graph_unchanged() {
        let graph = test_graph();
        let classifier = CompositeClassifier::new(vec![]);
        let result = classify_graph(&graph, &classifier);

        for node in &result.nodes {
            assert!(node.role.is_none());
        }
        for edge in &result.edges {
            assert!(edge.direction.is_none());
            assert!(edge.operation.is_none());
        }
    }
}
