use std::collections::{BTreeMap, HashMap, HashSet};

use grapha_core::graph::{Edge, EdgeKind, Graph, Node, NodeKind};
use serde::Serialize;

use crate::symbol_locator::SymbolLocatorIndex;

use super::{
    QueryResolveError, SymbolInfo, SymbolRef,
    api_surface::{ApiSurfaceOptions, query_api_surface},
};

#[derive(Debug, Clone, Default)]
pub struct SymbolUsagesOptions {
    pub exclude_files: Vec<String>,
    pub limit_per_group: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct SymbolUsagesResult {
    pub symbol: SymbolInfo,
    pub groups: Vec<UsageGroup>,
    pub total_groups: usize,
    pub total_usages: usize,
}

#[derive(Debug, Serialize)]
pub struct UsageGroup {
    pub target: SymbolRef,
    pub usages: Vec<UsageSite>,
    pub total: usize,
}

#[derive(Debug, Serialize)]
pub struct UsageSite {
    pub source: SymbolRef,
    pub edge_kind: EdgeKind,
    pub confidence: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
}

fn to_symbol_ref(node: &Node, locators: &SymbolLocatorIndex) -> SymbolRef {
    SymbolRef::from_node(node).with_locator(locators.locator_for_node(node))
}

fn to_symbol_info(node: &Node, locators: &SymbolLocatorIndex) -> SymbolInfo {
    SymbolInfo::from_node(node).with_locator(locators.locator_for_node(node))
}

fn is_type_node(node: &Node) -> bool {
    matches!(
        node.kind,
        NodeKind::Class | NodeKind::Struct | NodeKind::Enum | NodeKind::Protocol | NodeKind::Trait
    )
}

fn is_usage_edge(edge: &Edge) -> bool {
    matches!(
        edge.kind,
        EdgeKind::Calls
            | EdgeKind::Reads
            | EdgeKind::Writes
            | EdgeKind::Uses
            | EdgeKind::TypeRef
            | EdgeKind::References
            | EdgeKind::Implements
            | EdgeKind::Instantiates
            | EdgeKind::Returns
            | EdgeKind::TypeOf
    )
}

fn excluded_by_file(node: &Node, excludes: &[String]) -> bool {
    if excludes.is_empty() {
        return false;
    }
    let file = node.file.to_string_lossy();
    excludes.iter().any(|exclude| file.contains(exclude))
}

fn usage_targets(graph: &Graph, target: &Node) -> HashSet<String> {
    let mut targets = HashSet::from([target.id.clone()]);
    if is_type_node(target)
        && let Ok(surface) = query_api_surface(
            graph,
            &target.id,
            ApiSurfaceOptions {
                include_private: true,
            },
        )
    {
        targets.extend(surface.members.into_iter().map(|member| member.symbol.id));
    }
    targets
}

pub fn query_symbol_usages(
    graph: &Graph,
    symbol: &str,
    options: SymbolUsagesOptions,
) -> Result<SymbolUsagesResult, QueryResolveError> {
    let target = crate::query::resolve_node(graph, symbol)?;
    let locators = SymbolLocatorIndex::new(graph);
    let node_index: HashMap<&str, &Node> = graph
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    let target_ids = usage_targets(graph, target);

    let mut groups: BTreeMap<String, Vec<UsageSite>> = BTreeMap::new();
    for edge in &graph.edges {
        if !is_usage_edge(edge) || !target_ids.contains(edge.target.as_str()) {
            continue;
        }
        if target_ids.contains(edge.source.as_str()) {
            continue;
        }
        let Some(source) = node_index.get(edge.source.as_str()).copied() else {
            continue;
        };
        let Some(target_node) = node_index.get(edge.target.as_str()).copied() else {
            continue;
        };
        if excluded_by_file(source, &options.exclude_files) {
            continue;
        }
        groups
            .entry(target_node.id.clone())
            .or_default()
            .push(UsageSite {
                source: to_symbol_ref(source, &locators),
                edge_kind: edge.kind,
                confidence: edge.confidence,
                operation: edge.operation.clone(),
            });
    }

    let mut usage_groups: Vec<UsageGroup> = groups
        .into_iter()
        .filter_map(|(target_id, mut usages)| {
            usages.sort_by(|left, right| {
                left.source
                    .file
                    .cmp(&right.source.file)
                    .then_with(|| left.source.name.cmp(&right.source.name))
                    .then_with(|| left.source.id.cmp(&right.source.id))
            });
            let total = usages.len();
            if let Some(limit) = options.limit_per_group {
                usages.truncate(limit);
            }
            let target_node = node_index.get(target_id.as_str()).copied()?;
            Some(UsageGroup {
                target: to_symbol_ref(target_node, &locators),
                total,
                usages,
            })
        })
        .collect();
    usage_groups.sort_by(|left, right| {
        left.target
            .name
            .cmp(&right.target.name)
            .then_with(|| left.target.file.cmp(&right.target.file))
            .then_with(|| left.target.id.cmp(&right.target.id))
    });

    let total_usages = usage_groups.iter().map(|group| group.total).sum();
    Ok(SymbolUsagesResult {
        symbol: to_symbol_info(target, &locators),
        total_groups: usage_groups.len(),
        groups: usage_groups,
        total_usages,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapha_core::graph::{Span, Visibility};

    fn node(id: &str, name: &str, kind: NodeKind, file: &str) -> Node {
        Node {
            id: id.to_string(),
            kind,
            name: name.to_string(),
            file: file.into(),
            span: Span {
                start: [1, 0],
                end: [1, 0],
            },
            visibility: Visibility::Public,
            metadata: HashMap::new(),
            role: None,
            signature: None,
            doc_comment: None,
            module: Some("Game".to_string()),
            snippet: None,
            repo: None,
        }
    }

    fn edge(source: &str, target: &str, kind: EdgeKind) -> Edge {
        Edge {
            source: source.to_string(),
            target: target.to_string(),
            kind,
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
    fn groups_usages_by_target_member_for_type_queries() {
        let graph = Graph {
            version: String::new(),
            nodes: vec![
                node("type", "GameManager", NodeKind::Class, "GameManager.swift"),
                node(
                    "member_a",
                    "fetchReward()",
                    NodeKind::Function,
                    "GameManager.swift",
                ),
                node(
                    "member_b",
                    "presentDialog()",
                    NodeKind::Function,
                    "GameManager.swift",
                ),
                node("caller_a", "screenA()", NodeKind::Function, "ScreenA.swift"),
                node("caller_b", "screenB()", NodeKind::Function, "ScreenB.swift"),
            ],
            edges: vec![
                edge("type", "member_a", EdgeKind::Contains),
                edge("type", "member_b", EdgeKind::Contains),
                edge("caller_a", "member_a", EdgeKind::Calls),
                edge("caller_b", "member_b", EdgeKind::Calls),
            ],
        };

        let result =
            query_symbol_usages(&graph, "GameManager", SymbolUsagesOptions::default()).unwrap();

        assert_eq!(result.total_groups, 2);
        assert_eq!(result.total_usages, 2);
        assert_eq!(result.groups[0].target.name, "fetchReward()");
        assert_eq!(result.groups[1].target.name, "presentDialog()");
    }

    #[test]
    fn excludes_matching_source_files() {
        let graph = Graph {
            version: String::new(),
            nodes: vec![
                node("target", "target()", NodeKind::Function, "Target.swift"),
                node(
                    "external",
                    "external()",
                    NodeKind::Function,
                    "External.swift",
                ),
                node(
                    "internal",
                    "internal()",
                    NodeKind::Function,
                    "ModuleExport/Internal.swift",
                ),
            ],
            edges: vec![
                edge("external", "target", EdgeKind::Calls),
                edge("internal", "target", EdgeKind::Calls),
            ],
        };

        let result = query_symbol_usages(
            &graph,
            "target",
            SymbolUsagesOptions {
                exclude_files: vec!["ModuleExport/".to_string()],
                limit_per_group: None,
            },
        )
        .unwrap();

        assert_eq!(result.total_usages, 1);
        assert_eq!(result.groups[0].usages[0].source.name, "external()");
    }

    #[test]
    fn limits_each_group_without_losing_totals() {
        let graph = Graph {
            version: String::new(),
            nodes: vec![
                node("target", "target()", NodeKind::Function, "Target.swift"),
                node("caller_a", "callerA()", NodeKind::Function, "A.swift"),
                node("caller_b", "callerB()", NodeKind::Function, "B.swift"),
            ],
            edges: vec![
                edge("caller_a", "target", EdgeKind::Calls),
                edge("caller_b", "target", EdgeKind::Calls),
            ],
        };

        let result = query_symbol_usages(
            &graph,
            "target",
            SymbolUsagesOptions {
                exclude_files: Vec::new(),
                limit_per_group: Some(1),
            },
        )
        .unwrap();

        assert_eq!(result.total_usages, 2);
        assert_eq!(result.groups[0].total, 2);
        assert_eq!(result.groups[0].usages.len(), 1);
    }
}
