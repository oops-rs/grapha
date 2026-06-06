use std::collections::{HashMap, HashSet};
use std::path::Path;

use grapha_core::graph::{EdgeKind, Graph, Node, NodeKind};

#[derive(Debug, Clone, Default)]
pub struct SymbolLocatorIndex {
    locators_by_id: HashMap<String, String>,
}

impl SymbolLocatorIndex {
    pub fn new(graph: &Graph) -> Self {
        let node_index: HashMap<&str, &Node> = graph
            .nodes
            .iter()
            .map(|node| (node.id.as_str(), node))
            .collect();
        let parent_by_child = select_parents(graph, &node_index);
        let mut locators = HashMap::with_capacity(graph.nodes.len());

        for node in &graph.nodes {
            let locator = build_locator(
                node.id.as_str(),
                &node_index,
                &parent_by_child,
                &mut HashMap::new(),
            )
            .unwrap_or_else(|| fallback_locator(node));
            locators.insert(node.id.clone(), locator);
        }

        Self {
            locators_by_id: locators,
        }
    }

    pub fn locator_for_id(&self, id: &str) -> Option<&str> {
        self.locators_by_id.get(id).map(String::as_str)
    }

    pub fn locator_for_node(&self, node: &Node) -> String {
        self.locators_by_id
            .get(node.id.as_str())
            .cloned()
            .unwrap_or_else(|| fallback_locator(node))
    }
}

pub fn fallback_locator(node: &Node) -> String {
    let mut parts = Vec::new();
    let file = file_label(&node.file);
    if let Some(module) = node.module.as_deref()
        && !module.is_empty()
        && module != file
    {
        parts.push(module.to_string());
    }
    parts.push(file.to_string());
    parts.push(node.name.clone());
    parts.join("::")
}

pub fn locator_matches_suffix(locator: &str, query: &str) -> bool {
    locator == query || locator.ends_with(&format!("::{query}"))
}

pub fn file_label(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| path.to_string_lossy().to_string())
}

fn select_parents<'a>(
    graph: &'a Graph,
    node_index: &HashMap<&'a str, &'a Node>,
) -> HashMap<&'a str, &'a str> {
    let mut candidates: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in &graph.edges {
        if edge.kind != EdgeKind::Contains {
            continue;
        }
        if node_index.contains_key(edge.source.as_str())
            && node_index.contains_key(edge.target.as_str())
        {
            candidates
                .entry(edge.target.as_str())
                .or_default()
                .push(edge.source.as_str());
        }
    }

    candidates
        .into_iter()
        .filter_map(|(child_id, mut parents)| {
            parents.sort_by(|left_id, right_id| {
                let left = node_index
                    .get(left_id)
                    .copied()
                    .expect("contains source should exist");
                let right = node_index
                    .get(right_id)
                    .copied()
                    .expect("contains source should exist");
                parent_priority(left)
                    .cmp(&parent_priority(right))
                    .then_with(|| left.span.start.cmp(&right.span.start))
                    .then_with(|| left.span.end.cmp(&right.span.end))
                    .then_with(|| left.id.cmp(&right.id))
            });
            parents
                .into_iter()
                .next()
                .map(|parent_id| (child_id, parent_id))
        })
        .collect()
}

fn parent_priority(node: &Node) -> usize {
    match node.kind {
        NodeKind::View | NodeKind::Branch => 1,
        _ => 0,
    }
}

fn build_locator<'a>(
    node_id: &'a str,
    node_index: &HashMap<&'a str, &'a Node>,
    parent_by_child: &HashMap<&'a str, &'a str>,
    cache: &mut HashMap<&'a str, String>,
) -> Option<String> {
    if let Some(existing) = cache.get(node_id) {
        return Some(existing.clone());
    }

    let node = node_index.get(node_id).copied()?;
    let mut seen = HashSet::new();
    let mut owner_ids = Vec::new();
    let mut current = node_id;
    while let Some(parent_id) = parent_by_child.get(current).copied() {
        if !seen.insert(parent_id) {
            break;
        }
        owner_ids.push(parent_id);
        current = parent_id;
    }
    owner_ids.reverse();

    let mut parts = Vec::new();
    let file = file_label(&node.file);
    if let Some(module) = node.module.as_deref()
        && !module.is_empty()
        && module != file
    {
        parts.push(module.to_string());
    }
    parts.push(file);
    for owner_id in owner_ids {
        let owner = node_index.get(owner_id).copied()?;
        parts.push(owner.name.clone());
    }
    parts.push(node.name.clone());

    let locator = parts.join("::");
    cache.insert(node_id, locator.clone());
    Some(locator)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use grapha_core::graph::{Edge, Span, Visibility};

    use super::*;

    fn node(id: &str, name: &str, kind: NodeKind, file: &str) -> Node {
        Node {
            id: id.to_string(),
            kind,
            name: name.to_string(),
            file: PathBuf::from(file),
            span: Span {
                start: [1, 0],
                end: [1, 0],
            },
            visibility: Visibility::Public,
            metadata: HashMap::new(),
            role: None,
            signature: None,
            doc_comment: None,
            module: Some("ModuleExport".to_string()),
            snippet: None,
            repo: None,
        }
    }

    fn contains(source: &str, target: &str) -> Edge {
        Edge {
            source: source.to_string(),
            target: target.to_string(),
            kind: EdgeKind::Contains,
            confidence: 1.0,
            direction: None,
            operation: None,
            condition: None,
            async_boundary: None,
            provenance: Vec::new(),
            repo: None,
        }
    }

    #[test]
    fn builds_rust_style_locator_from_contains_chain() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                node("type", "Test", NodeKind::Struct, "Sources/Hello.swift"),
                node(
                    "method",
                    "hello(name:)",
                    NodeKind::Function,
                    "Sources/Hello.swift",
                ),
            ],
            edges: vec![contains("type", "method")],
        };

        let locators = SymbolLocatorIndex::new(&graph);
        assert_eq!(
            locators.locator_for_id("method"),
            Some("ModuleExport::Hello.swift::Test::hello(name:)")
        );
    }

    #[test]
    fn suffix_matching_requires_segment_boundary() {
        assert!(locator_matches_suffix(
            "ModuleExport::Hello.swift::Test::hello(name:)",
            "Hello.swift::Test::hello(name:)"
        ));
        assert!(!locator_matches_suffix(
            "ModuleExport::Hello.swift::Test::hello(name:)",
            "ello.swift::Test::hello(name:)"
        ));
    }
}
