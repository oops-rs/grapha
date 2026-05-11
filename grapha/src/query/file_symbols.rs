use serde::Serialize;

use grapha_core::graph::{Graph, NodeKind};

use crate::symbol_locator::SymbolLocatorIndex;

use super::{SymbolRef, file_matches_query_path};

#[derive(Debug, Serialize)]
pub struct FileSymbolsResult {
    pub file: String,
    pub symbols: Vec<FileSymbol>,
    pub total: usize,
}

#[derive(Debug, Serialize)]
pub struct BatchFileSymbolsResult {
    pub files: Vec<FileSymbolsResult>,
    pub missing: Vec<String>,
    pub total_files: usize,
    pub total_symbols: usize,
}

#[derive(Debug, Serialize)]
pub struct FileSymbol {
    #[serde(flatten)]
    pub symbol: SymbolRef,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub span: [usize; 2],
}

fn role_string(node: &grapha_core::graph::Node) -> Option<String> {
    node.role.as_ref().map(|r| match r {
        grapha_core::graph::NodeRole::EntryPoint => "entry_point".to_string(),
        grapha_core::graph::NodeRole::Terminal { kind } => {
            format!("terminal:{kind:?}").to_lowercase()
        }
        grapha_core::graph::NodeRole::Internal => "internal".to_string(),
    })
}

pub fn query_file_symbols(graph: &Graph, file_query: &str) -> FileSymbolsResult {
    let locators = SymbolLocatorIndex::new(graph);
    let mut symbols: Vec<FileSymbol> = graph
        .nodes
        .iter()
        .filter(|node| file_matches_query_path(&node.file, file_query))
        .filter(|node| !matches!(node.kind, NodeKind::View | NodeKind::Branch))
        .map(|node| FileSymbol {
            symbol: SymbolRef::from_node(node).with_locator(locators.locator_for_node(node)),
            module: node.module.clone(),
            role: role_string(node),
            span: [node.span.start[0], node.span.end[0]],
        })
        .collect();

    symbols.sort_by(|a, b| {
        a.span[0]
            .cmp(&b.span[0])
            .then_with(|| a.symbol.name.cmp(&b.symbol.name))
    });

    let total = symbols.len();
    FileSymbolsResult {
        file: file_query.to_string(),
        symbols,
        total,
    }
}

pub fn query_batch_file_symbols(graph: &Graph, file_queries: &[String]) -> BatchFileSymbolsResult {
    let mut seen = std::collections::HashSet::new();
    let mut files = Vec::new();
    let mut missing = Vec::new();

    for file_query in file_queries {
        if !seen.insert(file_query.as_str()) {
            continue;
        }
        let result = query_file_symbols(graph, file_query);
        if result.total == 0 {
            missing.push(file_query.clone());
        } else {
            files.push(result);
        }
    }

    let total_symbols = files.iter().map(|file| file.total).sum();
    let total_files = files.len();
    BatchFileSymbolsResult {
        files,
        missing,
        total_files,
        total_symbols,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapha_core::graph::{Node, Span, Visibility};
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn make_node(id: &str, name: &str, kind: NodeKind, file: &str) -> Node {
        Node {
            id: id.into(),
            kind,
            name: name.into(),
            file: PathBuf::from(file),
            span: Span {
                start: [10, 0],
                end: [20, 0],
            },
            visibility: Visibility::Public,
            metadata: HashMap::new(),
            role: None,
            signature: None,
            doc_comment: None,
            module: Some("Room".into()),
            snippet: None,
            repo: None,
        }
    }

    #[test]
    fn finds_symbols_by_file_suffix() {
        let graph = Graph {
            version: String::new(),
            nodes: vec![
                make_node("a", "Foo", NodeKind::Struct, "src/Foo.swift"),
                make_node("b", "Bar", NodeKind::Function, "src/Bar.swift"),
                make_node("c", "Baz", NodeKind::Property, "src/Foo.swift"),
            ],
            edges: vec![],
        };

        let result = query_file_symbols(&graph, "Foo.swift");
        assert_eq!(result.total, 2);
        // Same span, so sorted by name: Baz < Foo
        assert_eq!(result.symbols[0].symbol.name, "Baz");
        assert_eq!(result.symbols[1].symbol.name, "Foo");
    }

    #[test]
    fn matches_repo_relative_query_when_graph_stores_basename() {
        let graph = Graph {
            version: String::new(),
            nodes: vec![
                make_node("a", "Foo", NodeKind::Struct, "Foo.swift"),
                make_node("b", "Bar", NodeKind::Function, "Bar.swift"),
            ],
            edges: vec![],
        };

        let result = query_file_symbols(&graph, "Modules/Room/Sources/Room/View/Foo.swift");
        assert_eq!(result.total, 1);
        assert_eq!(result.symbols[0].symbol.name, "Foo");
    }

    #[test]
    fn excludes_view_and_branch_nodes() {
        let graph = Graph {
            version: String::new(),
            nodes: vec![
                make_node("a", "Foo", NodeKind::Struct, "src/Foo.swift"),
                make_node("v", "VStack", NodeKind::View, "src/Foo.swift"),
                make_node("br", "if x", NodeKind::Branch, "src/Foo.swift"),
            ],
            edges: vec![],
        };

        let result = query_file_symbols(&graph, "Foo.swift");
        assert_eq!(result.total, 1);
    }

    #[test]
    fn batch_file_symbols_dedupes_queries_and_reports_missing() {
        let graph = Graph {
            version: String::new(),
            nodes: vec![
                make_node("a", "Foo", NodeKind::Struct, "src/Foo.swift"),
                make_node("b", "Bar", NodeKind::Function, "src/Bar.swift"),
            ],
            edges: vec![],
        };

        let result = query_batch_file_symbols(
            &graph,
            &[
                "Foo.swift".to_string(),
                "Foo.swift".to_string(),
                "Missing.swift".to_string(),
                "Bar.swift".to_string(),
            ],
        );

        assert_eq!(result.total_files, 2);
        assert_eq!(result.total_symbols, 2);
        assert_eq!(result.files[0].file, "Foo.swift");
        assert_eq!(result.files[1].file, "Bar.swift");
        assert_eq!(result.missing, vec!["Missing.swift"]);
    }
}
