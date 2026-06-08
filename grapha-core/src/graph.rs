use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    File,
    Function,
    Method,
    Class,
    Struct,
    Enum,
    EnumMember,
    Trait,
    Impl,
    Module,
    Namespace,
    Field,
    Variant,
    Property,
    Variable,
    Constant,
    TypeAlias,
    Parameter,
    Import,
    Export,
    Package,
    Route,
    Component,
    Protocol,  // Swift protocols
    Extension, // Swift extensions
    View,      // Synthetic SwiftUI view tree node
    Branch,    // Synthetic SwiftUI branch node
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    Calls,
    Uses,
    Imports,
    Exports,
    Implements,
    Contains,
    TypeRef,
    References,
    TypeOf,
    Returns,
    Inherits,
    Extends,
    Instantiates,
    Overrides,
    Decorates,
    Reads,
    Writes,
    Publishes,
    Subscribes,
    DependsOn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    Public,
    Crate,
    Private,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalKind {
    Network,
    Persistence,
    Cache,
    Event,
    Keychain,
    Search,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum NodeRole {
    EntryPoint,
    Terminal { kind: TerminalKind },
    Internal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowDirection {
    Read,
    Write,
    ReadWrite,
    Pure,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub start: [usize; 2],
    pub end: [usize; 2],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EdgeProvenance {
    pub file: PathBuf,
    pub span: Span,
    pub symbol_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub kind: NodeKind,
    pub name: String,
    pub file: PathBuf,
    pub span: Span,
    pub visibility: Visibility,
    pub metadata: HashMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<NodeRole>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc_comment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    pub source: String,
    pub target: String,
    pub kind: EdgeKind,
    pub confidence: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<FlowDirection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub async_boundary: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provenance: Vec<EdgeProvenance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Graph {
    pub version: String,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

impl Default for Graph {
    fn default() -> Self {
        Self::new()
    }
}

impl Graph {
    pub fn new() -> Self {
        Self {
            version: "0.1.0".to_string(),
            nodes: Vec::new(),
            edges: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_kind_serializes_as_snake_case() {
        let json = serde_json::to_string(&NodeKind::Function).unwrap();
        assert_eq!(json, "\"function\"");

        let json = serde_json::to_string(&NodeKind::Class).unwrap();
        assert_eq!(json, "\"class\"");

        let json = serde_json::to_string(&NodeKind::Struct).unwrap();
        assert_eq!(json, "\"struct\"");

        let json = serde_json::to_string(&NodeKind::Package).unwrap();
        assert_eq!(json, "\"package\"");
    }

    #[test]
    fn edge_kind_serializes_as_snake_case() {
        let json = serde_json::to_string(&EdgeKind::TypeRef).unwrap();
        assert_eq!(json, "\"type_ref\"");

        let json = serde_json::to_string(&EdgeKind::TypeOf).unwrap();
        assert_eq!(json, "\"type_of\"");

        let json = serde_json::to_string(&EdgeKind::Imports).unwrap();
        assert_eq!(json, "\"imports\"");

        let json = serde_json::to_string(&EdgeKind::DependsOn).unwrap();
        assert_eq!(json, "\"depends_on\"");
    }

    #[test]
    fn visibility_serializes_as_snake_case() {
        let json = serde_json::to_string(&Visibility::Public).unwrap();
        assert_eq!(json, "\"public\"");
    }

    #[test]
    fn span_serializes_as_arrays() {
        let span = Span {
            start: [10, 0],
            end: [15, 1],
        };
        let json = serde_json::to_string(&span).unwrap();
        assert_eq!(json, r#"{"start":[10,0],"end":[15,1]}"#);
    }

    #[test]
    fn graph_serializes_with_version() {
        let graph = Graph::new();
        let json = serde_json::to_string_pretty(&graph).unwrap();
        assert!(json.contains("\"version\": \"0.1.0\""));
        assert!(json.contains("\"nodes\": []"));
        assert!(json.contains("\"edges\": []"));
    }

    #[test]
    fn edge_serializes_with_confidence() {
        let edge = Edge {
            source: "a".to_string(),
            target: "b".to_string(),
            kind: EdgeKind::Calls,
            confidence: 0.95,
            direction: None,
            operation: None,
            condition: None,
            async_boundary: None,
            provenance: Vec::new(),
            repo: None,
        };
        let json = serde_json::to_string(&edge).unwrap();
        assert!(json.contains("\"confidence\":0.95"));
    }

    #[test]
    fn new_node_kinds_serialize_correctly() {
        assert_eq!(serde_json::to_string(&NodeKind::File).unwrap(), "\"file\"");
        assert_eq!(
            serde_json::to_string(&NodeKind::Method).unwrap(),
            "\"method\""
        );
        assert_eq!(
            serde_json::to_string(&NodeKind::EnumMember).unwrap(),
            "\"enum_member\""
        );
        assert_eq!(
            serde_json::to_string(&NodeKind::Namespace).unwrap(),
            "\"namespace\""
        );
        assert_eq!(
            serde_json::to_string(&NodeKind::Property).unwrap(),
            "\"property\""
        );
        assert_eq!(
            serde_json::to_string(&NodeKind::Variable).unwrap(),
            "\"variable\""
        );
        assert_eq!(
            serde_json::to_string(&NodeKind::Constant).unwrap(),
            "\"constant\""
        );
        assert_eq!(
            serde_json::to_string(&NodeKind::TypeAlias).unwrap(),
            "\"type_alias\""
        );
        assert_eq!(
            serde_json::to_string(&NodeKind::Parameter).unwrap(),
            "\"parameter\""
        );
        assert_eq!(
            serde_json::to_string(&NodeKind::Import).unwrap(),
            "\"import\""
        );
        assert_eq!(
            serde_json::to_string(&NodeKind::Export).unwrap(),
            "\"export\""
        );
        assert_eq!(
            serde_json::to_string(&NodeKind::Route).unwrap(),
            "\"route\""
        );
        assert_eq!(
            serde_json::to_string(&NodeKind::Component).unwrap(),
            "\"component\""
        );
        assert_eq!(
            serde_json::to_string(&NodeKind::Protocol).unwrap(),
            "\"protocol\""
        );
        assert_eq!(
            serde_json::to_string(&NodeKind::Extension).unwrap(),
            "\"extension\""
        );
        assert_eq!(serde_json::to_string(&NodeKind::View).unwrap(), "\"view\"");
        assert_eq!(
            serde_json::to_string(&NodeKind::Branch).unwrap(),
            "\"branch\""
        );
    }

    #[test]
    fn full_graph_round_trips() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![Node {
                id: "src/main.rs::main".to_string(),
                kind: NodeKind::Function,
                name: "main".to_string(),
                file: PathBuf::from("src/main.rs"),
                span: Span {
                    start: [0, 0],
                    end: [3, 1],
                },
                visibility: Visibility::Private,
                metadata: HashMap::new(),
                role: None,
                signature: None,
                doc_comment: None,
                module: None,
                snippet: None,
                repo: None,
            }],
            edges: vec![Edge {
                source: "src/main.rs::main".to_string(),
                target: "src/main.rs::helper".to_string(),
                kind: EdgeKind::Calls,
                confidence: 0.8,
                direction: None,
                operation: None,
                condition: None,
                async_boundary: None,
                provenance: Vec::new(),
                repo: None,
            }],
        };
        let json = serde_json::to_string(&graph).unwrap();
        let deserialized: Graph = serde_json::from_str(&json).unwrap();
        assert_eq!(graph, deserialized);
    }

    #[test]
    fn terminal_kind_serializes_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&TerminalKind::Network).unwrap(),
            "\"network\""
        );
        assert_eq!(
            serde_json::to_string(&TerminalKind::Persistence).unwrap(),
            "\"persistence\""
        );
        assert_eq!(
            serde_json::to_string(&TerminalKind::Cache).unwrap(),
            "\"cache\""
        );
        assert_eq!(
            serde_json::to_string(&TerminalKind::Keychain).unwrap(),
            "\"keychain\""
        );
    }

    #[test]
    fn node_role_serializes_with_tag() {
        let entry = NodeRole::EntryPoint;
        let json = serde_json::to_string(&entry).unwrap();
        assert_eq!(json, r#"{"type":"entry_point"}"#);

        let terminal = NodeRole::Terminal {
            kind: TerminalKind::Network,
        };
        let json = serde_json::to_string(&terminal).unwrap();
        assert!(json.contains(r#""type":"terminal""#));
        assert!(json.contains(r#""kind":"network""#));

        let internal = NodeRole::Internal;
        let json = serde_json::to_string(&internal).unwrap();
        assert_eq!(json, r#"{"type":"internal"}"#);
    }

    #[test]
    fn node_role_round_trips() {
        let roles = vec![
            NodeRole::EntryPoint,
            NodeRole::Terminal {
                kind: TerminalKind::Persistence,
            },
            NodeRole::Internal,
        ];
        for role in roles {
            let json = serde_json::to_string(&role).unwrap();
            let deserialized: NodeRole = serde_json::from_str(&json).unwrap();
            assert_eq!(role, deserialized);
        }
    }

    #[test]
    fn flow_direction_serializes_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&FlowDirection::Read).unwrap(),
            "\"read\""
        );
        assert_eq!(
            serde_json::to_string(&FlowDirection::Write).unwrap(),
            "\"write\""
        );
        assert_eq!(
            serde_json::to_string(&FlowDirection::ReadWrite).unwrap(),
            "\"read_write\""
        );
        assert_eq!(
            serde_json::to_string(&FlowDirection::Pure).unwrap(),
            "\"pure\""
        );
    }

    #[test]
    fn new_edge_kinds_serialize_correctly() {
        assert_eq!(
            serde_json::to_string(&EdgeKind::Reads).unwrap(),
            "\"reads\""
        );
        assert_eq!(
            serde_json::to_string(&EdgeKind::Writes).unwrap(),
            "\"writes\""
        );
        assert_eq!(
            serde_json::to_string(&EdgeKind::Publishes).unwrap(),
            "\"publishes\""
        );
        assert_eq!(
            serde_json::to_string(&EdgeKind::Subscribes).unwrap(),
            "\"subscribes\""
        );
    }

    #[test]
    fn optional_node_fields_skipped_when_none() {
        let node = Node {
            id: "test::foo".to_string(),
            kind: NodeKind::Function,
            name: "foo".to_string(),
            file: PathBuf::from("test.rs"),
            span: Span {
                start: [0, 0],
                end: [1, 0],
            },
            visibility: Visibility::Public,
            metadata: HashMap::new(),
            role: None,
            signature: None,
            doc_comment: None,
            module: None,
            snippet: None,
            repo: None,
        };
        let json = serde_json::to_string(&node).unwrap();
        assert!(!json.contains("role"));
        assert!(!json.contains("signature"));
        assert!(!json.contains("doc_comment"));
        assert!(!json.contains("module"));
    }

    #[test]
    fn optional_node_fields_present_when_set() {
        let node = Node {
            id: "test::foo".to_string(),
            kind: NodeKind::Function,
            name: "foo".to_string(),
            file: PathBuf::from("test.rs"),
            span: Span {
                start: [0, 0],
                end: [1, 0],
            },
            visibility: Visibility::Public,
            metadata: HashMap::new(),
            role: Some(NodeRole::EntryPoint),
            signature: Some("fn foo(x: i32) -> bool".to_string()),
            doc_comment: Some("Does foo things".to_string()),
            module: Some("my_module".to_string()),
            snippet: None,
            repo: None,
        };
        let json = serde_json::to_string(&node).unwrap();
        let deserialized: Node = serde_json::from_str(&json).unwrap();
        assert_eq!(node, deserialized);
        assert!(json.contains("entry_point"));
        assert!(json.contains("fn foo(x: i32) -> bool"));
        assert!(json.contains("Does foo things"));
        assert!(json.contains("my_module"));
    }

    #[test]
    fn optional_edge_fields_skipped_when_none() {
        let edge = Edge {
            source: "a".to_string(),
            target: "b".to_string(),
            kind: EdgeKind::Reads,
            confidence: 0.9,
            direction: None,
            operation: None,
            condition: None,
            async_boundary: None,
            provenance: Vec::new(),
            repo: None,
        };
        let json = serde_json::to_string(&edge).unwrap();
        assert!(!json.contains("direction"));
        assert!(!json.contains("operation"));
        assert!(!json.contains("condition"));
        assert!(!json.contains("async_boundary"));
        assert!(!json.contains("provenance"));
    }

    #[test]
    fn optional_edge_fields_present_when_set() {
        let edge = Edge {
            source: "a".to_string(),
            target: "b".to_string(),
            kind: EdgeKind::Writes,
            confidence: 0.85,
            direction: Some(FlowDirection::Write),
            operation: Some("INSERT".to_string()),
            condition: Some("user.isAdmin".to_string()),
            async_boundary: Some(true),
            provenance: vec![EdgeProvenance {
                file: PathBuf::from("main.rs"),
                span: Span {
                    start: [1, 0],
                    end: [1, 12],
                },
                symbol_id: "main.rs::main".to_string(),
            }],
            repo: None,
        };
        let json = serde_json::to_string(&edge).unwrap();
        let deserialized: Edge = serde_json::from_str(&json).unwrap();
        assert_eq!(edge, deserialized);
        assert!(json.contains("\"write\""));
        assert!(json.contains("INSERT"));
        assert!(json.contains("user.isAdmin"));
        assert!(json.contains("true"));
        assert!(json.contains("provenance"));
    }

    #[test]
    fn extended_graph_round_trips() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![Node {
                id: "api::handler".to_string(),
                kind: NodeKind::Function,
                name: "handler".to_string(),
                file: PathBuf::from("api.rs"),
                span: Span {
                    start: [0, 0],
                    end: [10, 0],
                },
                visibility: Visibility::Public,
                metadata: HashMap::new(),
                role: Some(NodeRole::Terminal {
                    kind: TerminalKind::Network,
                }),
                signature: Some("async fn handler(req: Request) -> Response".to_string()),
                doc_comment: Some("Handles HTTP requests".to_string()),
                module: Some("api".to_string()),
                snippet: None,
                repo: None,
            }],
            edges: vec![Edge {
                source: "api::handler".to_string(),
                target: "db::query".to_string(),
                kind: EdgeKind::Reads,
                confidence: 0.9,
                direction: Some(FlowDirection::Read),
                operation: Some("SELECT".to_string()),
                condition: None,
                async_boundary: Some(true),
                provenance: vec![EdgeProvenance {
                    file: PathBuf::from("api.rs"),
                    span: Span {
                        start: [2, 4],
                        end: [2, 18],
                    },
                    symbol_id: "api::handler".to_string(),
                }],
                repo: None,
            }],
        };
        let json = serde_json::to_string(&graph).unwrap();
        let deserialized: Graph = serde_json::from_str(&json).unwrap();
        assert_eq!(graph, deserialized);
    }
}
