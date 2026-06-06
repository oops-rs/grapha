use serde::Serialize;

use grapha_core::graph::{Graph, NodeRole};

use super::{SymbolRef, file_matches_path_or_suffix};

#[derive(Debug, Clone, Default)]
pub struct EntriesQueryOptions {
    pub module: Option<String>,
    pub file: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct EntriesResult {
    pub entries: Vec<SymbolRef>,
    pub shown: usize,
    pub total: usize,
}

fn sort_entries(entries: &mut [SymbolRef]) {
    entries.sort_by(|left, right| {
        left.module
            .as_deref()
            .unwrap_or("")
            .cmp(right.module.as_deref().unwrap_or(""))
            .then_with(|| left.file.cmp(&right.file))
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.id.cmp(&right.id))
    });
}

pub fn query_entries_with_options(graph: &Graph, options: &EntriesQueryOptions) -> EntriesResult {
    let mut entries: Vec<SymbolRef> = graph
        .nodes
        .iter()
        .filter(|n| n.role == Some(NodeRole::EntryPoint))
        .filter(|node| {
            options
                .module
                .as_deref()
                .is_none_or(|module| node.module.as_deref() == Some(module))
        })
        .filter(|node| {
            options
                .file
                .as_deref()
                .is_none_or(|file_query| file_matches_path_or_suffix(&node.file, file_query))
        })
        .map(SymbolRef::from_node)
        .collect();

    sort_entries(&mut entries);

    let total = entries.len();
    let shown = options.limit.map(|limit| limit.min(total)).unwrap_or(total);
    entries.truncate(shown);

    EntriesResult {
        entries,
        shown,
        total,
    }
}

pub fn query_entries(graph: &Graph) -> EntriesResult {
    query_entries_with_options(graph, &EntriesQueryOptions::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapha_core::graph::*;
    use std::collections::HashMap;
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
            metadata: HashMap::new(),
            role,
            signature: None,
            doc_comment: None,
            module: None,
            snippet: None,
            repo: None,
        }
    }

    fn entry_node(id: &str, name: &str, file: &str, module: Option<&str>) -> Node {
        Node {
            id: id.into(),
            kind: NodeKind::Function,
            name: name.into(),
            file: PathBuf::from(file),
            span: Span {
                start: [0, 0],
                end: [1, 0],
            },
            visibility: Visibility::Public,
            metadata: HashMap::new(),
            role: Some(NodeRole::EntryPoint),
            signature: None,
            doc_comment: None,
            module: module.map(str::to_string),
            snippet: None,
            repo: None,
        }
    }

    #[test]
    fn lists_entry_points() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                make_node("entry1", Some(NodeRole::EntryPoint)),
                make_node("entry2", Some(NodeRole::EntryPoint)),
                make_node("internal", Some(NodeRole::Internal)),
            ],
            edges: vec![],
        };

        let result = query_entries(&graph);
        assert_eq!(result.total, 2);
        let names: Vec<&str> = result.entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"entry1"));
        assert!(names.contains(&"entry2"));
        assert!(!names.contains(&"internal"));
    }

    #[test]
    fn returns_empty_when_no_entries() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                make_node("a", None),
                make_node("b", Some(NodeRole::Internal)),
            ],
            edges: vec![],
        };

        let result = query_entries(&graph);
        assert_eq!(result.total, 0);
        assert!(result.entries.is_empty());
    }

    #[test]
    fn filters_entries_by_module_and_file_and_limit() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                entry_node(
                    "room_body",
                    "body",
                    "Modules/Room/Sources/Room/View/RoomPage.swift",
                    Some("Room"),
                ),
                entry_node(
                    "room_share",
                    "onShare",
                    "Modules/Room/Sources/Room/View/RoomPage.swift",
                    Some("Room"),
                ),
                entry_node(
                    "chat_body",
                    "body",
                    "Modules/Chat/Sources/Chat/View/ChatPage.swift",
                    Some("Chat"),
                ),
            ],
            edges: vec![],
        };

        let result = query_entries_with_options(
            &graph,
            &EntriesQueryOptions {
                module: Some("Room".to_string()),
                file: Some("RoomPage.swift".to_string()),
                limit: Some(1),
            },
        );
        let actual: Vec<(&str, &str, &str, Option<&str>)> = result
            .entries
            .iter()
            .map(|entry| {
                (
                    entry.id.as_str(),
                    entry.name.as_str(),
                    entry.file.rsplit('/').next().unwrap_or(entry.file.as_str()),
                    entry.module.as_deref(),
                )
            })
            .collect();
        let expected = vec![("room_body", "body", "RoomPage.swift", Some("Room"))];

        assert_eq!(actual, expected);
        assert_eq!(result.total, 2);
        assert_eq!(result.shown, 1);
    }

    #[test]
    fn file_filter_does_not_match_partial_fragments() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![entry_node(
                "room_body",
                "body",
                "Modules/Room/Sources/Room/View/RoomPage.swift",
                Some("Room"),
            )],
            edges: vec![],
        };

        let result = query_entries_with_options(
            &graph,
            &EntriesQueryOptions {
                file: Some("Page".to_string()),
                ..EntriesQueryOptions::default()
            },
        );

        assert_eq!(result.total, 0);
        assert_eq!(result.shown, 0);
        assert!(result.entries.is_empty());
    }
}
