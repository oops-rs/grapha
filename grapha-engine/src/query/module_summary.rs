use std::collections::{BTreeMap, HashMap, HashSet};

use serde::Serialize;

use grapha_core::graph::{Graph, NodeKind, NodeRole};

#[derive(Debug, Serialize)]
pub struct ModuleSummaryResult {
    pub modules: Vec<ModuleSummary>,
    pub total_modules: usize,
}

#[derive(Debug, Serialize)]
pub struct ModuleSummary {
    pub name: String,
    pub symbol_count: usize,
    pub file_count: usize,
    pub symbols_by_kind: BTreeMap<String, usize>,
    pub edge_count: usize,
    pub cross_module_edges: usize,
    pub coupling_ratio: f64,
    pub entry_points: usize,
    pub terminals: usize,
}

pub fn query_module_summary(graph: &Graph) -> ModuleSummaryResult {
    // Group nodes by module
    let mut module_nodes: HashMap<String, Vec<usize>> = HashMap::new();
    let mut node_module: HashMap<&str, &str> = HashMap::new();

    for (idx, node) in graph.nodes.iter().enumerate() {
        if matches!(node.kind, NodeKind::View | NodeKind::Branch) {
            continue;
        }
        let module_name = node.module.as_deref().unwrap_or("(unknown)");
        module_nodes
            .entry(module_name.to_string())
            .or_default()
            .push(idx);
        node_module.insert(node.id.as_str(), module_name);
    }

    // Build per-module edge counts
    let mut module_edge_count: HashMap<&str, usize> = HashMap::new();
    let mut module_cross_edges: HashMap<&str, usize> = HashMap::new();

    for edge in &graph.edges {
        let src_module = node_module.get(edge.source.as_str()).copied();
        let tgt_module = node_module.get(edge.target.as_str()).copied();

        if let Some(sm) = src_module {
            *module_edge_count.entry(sm).or_default() += 1;
            if tgt_module.is_some_and(|tm| tm != sm) {
                *module_cross_edges.entry(sm).or_default() += 1;
            }
        }
    }

    let mut modules: Vec<ModuleSummary> = module_nodes
        .into_iter()
        .map(|(name, node_indices)| {
            let mut symbols_by_kind: BTreeMap<String, usize> = BTreeMap::new();
            let mut files: HashSet<String> = HashSet::new();
            let mut entry_points = 0usize;
            let mut terminals = 0usize;

            for &idx in &node_indices {
                let node = &graph.nodes[idx];
                *symbols_by_kind
                    .entry(format!("{:?}", node.kind).to_lowercase())
                    .or_default() += 1;
                files.insert(node.file.to_string_lossy().to_string());

                match &node.role {
                    Some(NodeRole::EntryPoint) => entry_points += 1,
                    Some(NodeRole::Terminal { .. }) => terminals += 1,
                    _ => {}
                }
            }

            let edge_count = module_edge_count.get(name.as_str()).copied().unwrap_or(0);
            let cross_module_edges = module_cross_edges.get(name.as_str()).copied().unwrap_or(0);
            let coupling_ratio = if edge_count > 0 {
                cross_module_edges as f64 / edge_count as f64
            } else {
                0.0
            };

            ModuleSummary {
                name,
                symbol_count: node_indices.len(),
                file_count: files.len(),
                symbols_by_kind,
                edge_count,
                cross_module_edges,
                coupling_ratio: (coupling_ratio * 100.0).round() / 100.0,
                entry_points,
                terminals,
            }
        })
        .collect();

    modules.sort_by_key(|module| std::cmp::Reverse(module.symbol_count));
    let total_modules = modules.len();

    ModuleSummaryResult {
        modules,
        total_modules,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapha_core::graph::{Edge, EdgeKind, Node, Span, Visibility};
    use std::path::PathBuf;

    fn make_node(id: &str, name: &str, kind: NodeKind, file: &str, module: &str) -> Node {
        Node {
            id: id.into(),
            kind,
            name: name.into(),
            file: PathBuf::from(file),
            span: Span {
                start: [1, 0],
                end: [10, 0],
            },
            visibility: Visibility::Public,
            metadata: HashMap::new(),
            role: None,
            signature: None,
            doc_comment: None,
            module: Some(module.into()),
            snippet: None,
            repo: None,
        }
    }

    #[test]
    fn groups_by_module() {
        let graph = Graph {
            version: String::new(),
            nodes: vec![
                make_node("a", "Foo", NodeKind::Struct, "Foo.swift", "ModA"),
                make_node("b", "Bar", NodeKind::Struct, "Bar.swift", "ModA"),
                make_node("c", "Baz", NodeKind::Function, "Baz.swift", "ModB"),
            ],
            edges: vec![Edge {
                source: "a".into(),
                target: "c".into(),
                kind: EdgeKind::Calls,
                confidence: 1.0,
                direction: None,
                operation: None,
                condition: None,
                async_boundary: None,
                provenance: vec![],
                repo: None,
            }],
        };

        let result = query_module_summary(&graph);
        assert_eq!(result.total_modules, 2);

        let mod_a = result.modules.iter().find(|m| m.name == "ModA").unwrap();
        assert_eq!(mod_a.symbol_count, 2);
        assert_eq!(mod_a.cross_module_edges, 1);
    }
}
