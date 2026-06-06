use std::collections::HashMap;

use grapha_core::graph::{EdgeKind, Graph, Node, NodeKind};

use super::SymbolInfo;

pub(crate) fn to_symbol_info(node: &Node) -> SymbolInfo {
    SymbolInfo::from_node(node)
}

pub(crate) fn contains_adjacency(graph: &Graph) -> HashMap<&str, Vec<&str>> {
    let mut map: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in &graph.edges {
        if edge.kind == EdgeKind::Contains {
            map.entry(edge.source.as_str())
                .or_default()
                .push(edge.target.as_str());
        }
    }
    map
}

pub(crate) fn contains_parents(graph: &Graph) -> HashMap<&str, &str> {
    let mut map = HashMap::new();
    for edge in &graph.edges {
        if edge.kind == EdgeKind::Contains {
            map.insert(edge.target.as_str(), edge.source.as_str());
        }
    }
    map
}

pub(crate) fn ui_path<'a>(
    usage_id: &'a str,
    stop_id: &'a str,
    parents: &HashMap<&'a str, &'a str>,
    node_index: &HashMap<&'a str, &'a Node>,
) -> Vec<String> {
    let mut path = Vec::new();
    let mut current = Some(usage_id);

    while let Some(node_id) = current {
        if node_id == stop_id {
            break;
        }
        let Some(node) = node_index.get(node_id).copied() else {
            break;
        };
        if matches!(node.kind, NodeKind::View | NodeKind::Branch) {
            path.push(node.name.clone());
        }
        current = parents.get(node_id).copied();
    }

    path.reverse();
    path
}
