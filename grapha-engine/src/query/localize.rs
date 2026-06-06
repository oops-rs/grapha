use std::collections::{HashMap, HashSet};

use serde::Serialize;

use grapha_core::graph::{Graph, Node};

use crate::localization::{
    LocalizationCatalogIndex, LocalizationReference, edges_by_source, localization_usage_nodes,
    node_index, parse_wrapper_binding, resolve_usage_with, wrapper_binding_nodes,
};

use super::l10n::{contains_adjacency, contains_parents, to_symbol_info, ui_path};
use super::{QueryResolveError, SymbolInfo, normalize_symbol_name, resolve_node};

#[derive(Debug, Serialize)]
pub struct LocalizeResult {
    pub symbol: SymbolInfo,
    pub matches: Vec<LocalizationMatch>,
    pub unmatched: Vec<UnmatchedLocalizationUsage>,
}

#[derive(Debug, Serialize)]
pub struct LocalizationMatch {
    pub view: SymbolInfo,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ui_path: Vec<String>,
    pub reference: LocalizationReference,
    pub record: crate::localization::LocalizationCatalogRecord,
    pub match_kind: String,
}

#[derive(Debug, Serialize)]
pub struct UnmatchedLocalizationUsage {
    pub view: SymbolInfo,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ui_path: Vec<String>,
    pub reference: LocalizationReference,
    pub reason: String,
}

pub fn query_localize(
    graph: &Graph,
    catalogs: &LocalizationCatalogIndex,
    symbol: &str,
) -> Result<LocalizeResult, QueryResolveError> {
    let node_index = node_index(graph);
    let edges_by_source = edges_by_source(graph);
    let contains_adj = contains_adjacency(graph);
    let parents = contains_parents(graph);
    let wrapper_nodes = wrapper_binding_nodes(&node_index);
    let root = resolve_localize_root(
        graph,
        catalogs,
        &node_index,
        &edges_by_source,
        &contains_adj,
        &wrapper_nodes,
        symbol,
    )?;

    let mut matches = Vec::new();
    let mut unmatched = Vec::new();

    match root {
        LocalizeRoot::Subtree(root) => {
            let usage_ids = usage_ids_in_subtree(root.id.as_str(), &contains_adj, &node_index);
            for usage_id in usage_ids {
                let Some(usage_node) = node_index.get(usage_id).copied() else {
                    continue;
                };
                let Some(resolution) = resolve_usage_with(
                    usage_node,
                    &edges_by_source,
                    &node_index,
                    &wrapper_nodes,
                    catalogs,
                ) else {
                    continue;
                };

                let ui_path = ui_path(usage_id, root.id.as_str(), &parents, &node_index);
                for item in resolution.matches {
                    matches.push(LocalizationMatch {
                        view: to_symbol_info(usage_node),
                        ui_path: ui_path.clone(),
                        reference: item.reference,
                        record: item.record,
                        match_kind: item.match_kind,
                    });
                }
                if let Some(item) = resolution.unmatched {
                    unmatched.push(UnmatchedLocalizationUsage {
                        view: to_symbol_info(usage_node),
                        ui_path,
                        reference: item.reference,
                        reason: item.reason,
                    });
                }
            }
        }
        LocalizeRoot::Wrapper(root) => {
            for usage_node in localization_usage_nodes(graph) {
                let Some(resolution) = resolve_usage_with(
                    usage_node,
                    &edges_by_source,
                    &node_index,
                    &wrapper_nodes,
                    catalogs,
                ) else {
                    continue;
                };

                let ui_path = ui_path(
                    usage_node.id.as_str(),
                    usage_node.id.as_str(),
                    &parents,
                    &node_index,
                );
                for item in resolution.matches {
                    if item.reference.wrapper_symbol.as_deref() != Some(root.id.as_str()) {
                        continue;
                    }
                    matches.push(LocalizationMatch {
                        view: to_symbol_info(usage_node),
                        ui_path: ui_path.clone(),
                        reference: item.reference,
                        record: item.record,
                        match_kind: item.match_kind,
                    });
                }
            }

            if matches.is_empty()
                && let Some(binding) = parse_wrapper_binding(root)
            {
                for record in catalogs.records_for(&binding.table, &binding.key) {
                    matches.push(LocalizationMatch {
                        view: to_symbol_info(root),
                        ui_path: Vec::new(),
                        reference: LocalizationReference {
                            ref_kind: "wrapper".to_string(),
                            wrapper_name: Some(root.name.clone()),
                            wrapper_base: None,
                            wrapper_symbol: Some(root.id.clone()),
                            table: Some(binding.table.clone()),
                            key: Some(binding.key.clone()),
                            fallback: binding.fallback.clone(),
                            arg_count: binding.arg_count,
                            literal: None,
                        },
                        record,
                        match_kind: "wrapper_binding".to_string(),
                    });
                }
            }
        }
    }

    Ok(LocalizeResult {
        symbol: to_symbol_info(root.node()),
        matches,
        unmatched,
    })
}

enum LocalizeRoot<'a> {
    Subtree(&'a Node),
    Wrapper(&'a Node),
}

struct LocalizeResolver<'a> {
    graph: &'a Graph,
    catalogs: &'a LocalizationCatalogIndex,
    node_index: &'a HashMap<&'a str, &'a Node>,
    edges_by_source: &'a HashMap<&'a str, Vec<&'a grapha_core::graph::Edge>>,
    wrapper_nodes: &'a [&'a Node],
}

impl LocalizeRoot<'_> {
    fn node(&self) -> &Node {
        match self {
            Self::Subtree(node) | Self::Wrapper(node) => node,
        }
    }
}

fn resolve_localize_root<'a>(
    graph: &'a Graph,
    catalogs: &'a LocalizationCatalogIndex,
    node_index: &'a HashMap<&'a str, &'a Node>,
    edges_by_source: &'a HashMap<&'a str, Vec<&'a grapha_core::graph::Edge>>,
    contains_adj: &HashMap<&'a str, Vec<&'a str>>,
    wrapper_nodes: &'a [&'a Node],
    symbol: &str,
) -> Result<LocalizeRoot<'a>, QueryResolveError> {
    let resolver = LocalizeResolver {
        graph,
        catalogs,
        node_index,
        edges_by_source,
        wrapper_nodes,
    };

    match resolve_node(graph, symbol) {
        Ok(node) => Ok(classify_localize_root(&resolver, node, contains_adj)),
        Err(QueryResolveError::Ambiguous { query, candidates }) => {
            let candidate_nodes: Vec<_> = candidates
                .iter()
                .filter_map(|candidate| node_index.get(candidate.id.as_str()).copied())
                .collect();
            let mut wrapper_matches: Vec<_> = candidate_nodes
                .iter()
                .copied()
                .filter(|node| parse_wrapper_binding(node).is_some())
                .collect();
            if let Some(wrapper) =
                choose_wrapper_candidate(&resolver, &candidate_nodes, &mut wrapper_matches, symbol)
            {
                return Ok(LocalizeRoot::Wrapper(wrapper));
            }
            Err(QueryResolveError::Ambiguous { query, candidates })
        }
        Err(error) => Err(error),
    }
}

fn classify_localize_root<'a>(
    resolver: &LocalizeResolver<'a>,
    node: &'a Node,
    contains_adj: &HashMap<&'a str, Vec<&'a str>>,
) -> LocalizeRoot<'a> {
    if parse_wrapper_binding(node).is_some() {
        return LocalizeRoot::Wrapper(node);
    }

    if usage_ids_in_subtree(node.id.as_str(), contains_adj, resolver.node_index).is_empty() {
        let candidate_nodes = vec![node];
        let mut wrapper_matches = associated_wrapper_candidates(
            &candidate_nodes,
            resolver.wrapper_nodes,
            node.name.as_str(),
        );
        if let Some(wrapper) = choose_wrapper_candidate(
            resolver,
            &candidate_nodes,
            &mut wrapper_matches,
            node.name.as_str(),
        ) {
            return LocalizeRoot::Wrapper(wrapper);
        }
    }

    LocalizeRoot::Subtree(node)
}

fn choose_wrapper_candidate<'a>(
    resolver: &LocalizeResolver<'a>,
    candidate_nodes: &[&'a Node],
    wrapper_matches: &mut Vec<&'a Node>,
    symbol: &str,
) -> Option<&'a Node> {
    if wrapper_matches.len() == 1 {
        return wrapper_matches.first().copied();
    }

    if wrapper_matches.is_empty() {
        *wrapper_matches =
            associated_wrapper_candidates(candidate_nodes, resolver.wrapper_nodes, symbol);
        if wrapper_matches.len() == 1 {
            return wrapper_matches.first().copied();
        }
    }

    best_used_wrapper_candidate(
        resolver.graph,
        resolver.catalogs,
        wrapper_matches,
        resolver.node_index,
        resolver.edges_by_source,
        resolver.wrapper_nodes,
    )
    .or_else(|| canonical_wrapper_candidate(wrapper_matches))
}

fn associated_wrapper_candidates<'a>(
    candidate_nodes: &[&'a Node],
    wrapper_nodes: &[&'a Node],
    symbol: &str,
) -> Vec<&'a Node> {
    let mut logical_names: HashSet<&str> = candidate_nodes
        .iter()
        .map(|node| normalize_symbol_name(node.name.as_str()))
        .collect();
    logical_names.insert(normalize_symbol_name(symbol));

    wrapper_nodes
        .iter()
        .copied()
        .filter(|node| logical_names.contains(normalize_symbol_name(node.name.as_str())))
        .collect()
}

fn best_used_wrapper_candidate<'a>(
    graph: &'a Graph,
    catalogs: &LocalizationCatalogIndex,
    wrapper_candidates: &[&'a Node],
    node_index: &HashMap<&'a str, &'a Node>,
    edges_by_source: &HashMap<&'a str, Vec<&'a grapha_core::graph::Edge>>,
    wrapper_nodes: &[&'a Node],
) -> Option<&'a Node> {
    if wrapper_candidates.is_empty() {
        return None;
    }

    let candidate_ids: HashSet<&str> = wrapper_candidates
        .iter()
        .map(|node| node.id.as_str())
        .collect();
    let mut usage_counts: HashMap<String, usize> = HashMap::new();

    for usage_node in localization_usage_nodes(graph) {
        let Some(resolution) = resolve_usage_with(
            usage_node,
            edges_by_source,
            node_index,
            wrapper_nodes,
            catalogs,
        ) else {
            continue;
        };
        for item in resolution.matches {
            let Some(wrapper_symbol) = item.reference.wrapper_symbol.as_deref() else {
                continue;
            };
            if candidate_ids.contains(wrapper_symbol) {
                *usage_counts.entry(wrapper_symbol.to_string()).or_default() += 1;
            }
        }
    }

    let mut scored: Vec<_> = wrapper_candidates
        .iter()
        .copied()
        .map(|node| {
            (
                node,
                usage_counts
                    .get(node.id.as_str())
                    .copied()
                    .unwrap_or_default(),
            )
        })
        .collect();
    scored.sort_by(|(left_node, left_count), (right_node, right_count)| {
        right_count
            .cmp(left_count)
            .then_with(|| left_node.id.cmp(&right_node.id))
    });

    let (best_node, best_count) = scored.first().copied()?;
    if best_count == 0 {
        return None;
    }
    if scored.iter().skip(1).any(|(_, count)| *count == best_count) {
        return None;
    }

    Some(best_node)
}

fn canonical_wrapper_candidate<'a>(wrapper_candidates: &[&'a Node]) -> Option<&'a Node> {
    let first = wrapper_candidates.first().copied()?;
    let first_binding = parse_wrapper_binding(first)?;
    if !wrapper_candidates
        .iter()
        .copied()
        .all(|node| parse_wrapper_binding(node).as_ref() == Some(&first_binding))
    {
        return None;
    }

    wrapper_candidates.iter().copied().min_by(|left, right| {
        left.id
            .cmp(&right.id)
            .then_with(|| left.file.cmp(&right.file))
            .then_with(|| left.name.cmp(&right.name))
    })
}

fn usage_ids_in_subtree<'a>(
    root_id: &'a str,
    contains_adj: &HashMap<&'a str, Vec<&'a str>>,
    node_index: &HashMap<&'a str, &'a Node>,
) -> Vec<&'a str> {
    let mut stack = vec![root_id];
    let mut seen = HashSet::new();
    let mut usage_ids = Vec::new();

    while let Some(current_id) = stack.pop() {
        if !seen.insert(current_id) {
            continue;
        }
        if node_index
            .get(current_id)
            .is_some_and(|node| node.metadata.contains_key("l10n.ref_kind"))
        {
            usage_ids.push(current_id);
        }

        if let Some(children) = contains_adj.get(current_id) {
            for &child in children.iter().rev() {
                stack.push(child);
            }
        }
    }

    usage_ids
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::localization::LocalizationCatalogRecord;
    use grapha_core::graph::{Edge, EdgeKind, NodeKind, Span, Visibility};
    use std::collections::BTreeMap;
    use std::collections::HashMap as StdHashMap;
    use std::path::PathBuf;

    fn node(id: &str, name: &str, kind: NodeKind) -> Node {
        Node {
            id: id.to_string(),
            kind,
            name: name.to_string(),
            file: PathBuf::from("ContentView.swift"),
            span: Span {
                start: [0, 0],
                end: [0, 1],
            },
            visibility: Visibility::Private,
            metadata: StdHashMap::new(),
            role: None,
            signature: None,
            doc_comment: None,
            module: None,
            snippet: None,
            repo: None,
        }
    }

    #[test]
    fn localize_reports_matches_for_usage_nodes() {
        let mut body = node("body", "body", NodeKind::Property);
        body.role = Some(grapha_core::graph::NodeRole::EntryPoint);
        let mut text = node("text", "Text", NodeKind::View);
        text.metadata
            .insert("l10n.ref_kind".to_string(), "wrapper".to_string());
        text.metadata
            .insert("l10n.wrapper_name".to_string(), "welcomeTitle".to_string());
        let mut wrapper = node("wrapper", "welcomeTitle", NodeKind::Property);
        wrapper
            .metadata
            .insert("l10n.wrapper.table".to_string(), "Localizable".to_string());
        wrapper
            .metadata
            .insert("l10n.wrapper.key".to_string(), "welcome_title".to_string());

        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![body, text, wrapper],
            edges: vec![
                Edge {
                    source: "body".to_string(),
                    target: "text".to_string(),
                    kind: EdgeKind::Contains,
                    confidence: 1.0,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: vec![],
                    repo: None,
                },
                Edge {
                    source: "text".to_string(),
                    target: "wrapper".to_string(),
                    kind: EdgeKind::TypeRef,
                    confidence: 1.0,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: vec![],
                    repo: None,
                },
            ],
        };

        let mut catalogs = LocalizationCatalogIndex::default();
        catalogs.insert(LocalizationCatalogRecord {
            table: "Localizable".to_string(),
            key: "welcome_title".to_string(),
            catalog_file: "Localizable.xcstrings".to_string(),
            catalog_dir: ".".to_string(),
            source_language: "en".to_string(),
            source_value: "Welcome".to_string(),
            status: "translated".to_string(),
            comment: None,
            translations: BTreeMap::new(),
        });

        let result = query_localize(&graph, &catalogs, "body").unwrap();
        assert_eq!(result.matches.len(), 1);
        assert!(result.unmatched.is_empty());
        assert_eq!(result.matches[0].record.key, "welcome_title");
    }

    #[test]
    fn localize_prefers_unique_wrapper_binding_when_symbol_query_is_ambiguous() {
        let mut body = node("body", "body", NodeKind::Property);
        body.role = Some(grapha_core::graph::NodeRole::EntryPoint);

        let mut text = node("text", "Text", NodeKind::View);
        text.metadata
            .insert("l10n.ref_kind".to_string(), "wrapper".to_string());
        text.metadata
            .insert("l10n.wrapper_name".to_string(), "welcomeTitle".to_string());

        let mut generated_accessor = node("generated", "welcomeTitle", NodeKind::Property);
        generated_accessor.module.replace("AppUI".to_string());

        let mut wrapper = node("wrapper", "welcomeTitle", NodeKind::Property);
        wrapper.module = Some("FrameResources".to_string());
        wrapper
            .metadata
            .insert("l10n.wrapper.table".to_string(), "Localizable".to_string());
        wrapper
            .metadata
            .insert("l10n.wrapper.key".to_string(), "welcome_title".to_string());

        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![body, text, generated_accessor, wrapper],
            edges: vec![
                Edge {
                    source: "body".to_string(),
                    target: "text".to_string(),
                    kind: EdgeKind::Contains,
                    confidence: 1.0,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: vec![],
                    repo: None,
                },
                Edge {
                    source: "text".to_string(),
                    target: "wrapper".to_string(),
                    kind: EdgeKind::TypeRef,
                    confidence: 1.0,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: vec![],
                    repo: None,
                },
            ],
        };

        let mut catalogs = LocalizationCatalogIndex::default();
        catalogs.insert(LocalizationCatalogRecord {
            table: "Localizable".to_string(),
            key: "welcome_title".to_string(),
            catalog_file: "Localizable.xcstrings".to_string(),
            catalog_dir: ".".to_string(),
            source_language: "en".to_string(),
            source_value: "Welcome".to_string(),
            status: "translated".to_string(),
            comment: None,
            translations: BTreeMap::new(),
        });

        let result = query_localize(&graph, &catalogs, "welcomeTitle").unwrap();
        assert_eq!(result.symbol.id, "wrapper");
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].record.key, "welcome_title");
        assert_eq!(
            result.matches[0].reference.wrapper_symbol.as_deref(),
            Some("wrapper")
        );
    }

    #[test]
    fn localize_wrapper_symbol_query_reports_usage_matches() {
        let mut body = node("body", "body", NodeKind::Property);
        body.role = Some(grapha_core::graph::NodeRole::EntryPoint);

        let mut text = node("text", "Text", NodeKind::View);
        text.metadata
            .insert("l10n.ref_kind".to_string(), "wrapper".to_string());
        text.metadata
            .insert("l10n.wrapper_name".to_string(), "welcomeTitle".to_string());

        let mut wrapper = node("wrapper", "welcomeTitle", NodeKind::Property);
        wrapper
            .metadata
            .insert("l10n.wrapper.table".to_string(), "Localizable".to_string());
        wrapper
            .metadata
            .insert("l10n.wrapper.key".to_string(), "welcome_title".to_string());

        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![body, text, wrapper],
            edges: vec![
                Edge {
                    source: "body".to_string(),
                    target: "text".to_string(),
                    kind: EdgeKind::Contains,
                    confidence: 1.0,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: vec![],
                    repo: None,
                },
                Edge {
                    source: "text".to_string(),
                    target: "wrapper".to_string(),
                    kind: EdgeKind::TypeRef,
                    confidence: 1.0,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: vec![],
                    repo: None,
                },
            ],
        };

        let mut catalogs = LocalizationCatalogIndex::default();
        catalogs.insert(LocalizationCatalogRecord {
            table: "Localizable".to_string(),
            key: "welcome_title".to_string(),
            catalog_file: "Localizable.xcstrings".to_string(),
            catalog_dir: ".".to_string(),
            source_language: "en".to_string(),
            source_value: "Welcome".to_string(),
            status: "translated".to_string(),
            comment: None,
            translations: BTreeMap::new(),
        });

        let result = query_localize(&graph, &catalogs, "wrapper").unwrap();
        assert_eq!(result.symbol.id, "wrapper");
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].view.id, "text");
        assert_eq!(
            result.matches[0].reference.wrapper_symbol.as_deref(),
            Some("wrapper")
        );
    }

    #[test]
    fn localize_prefers_most_used_wrapper_when_generated_symbols_share_name() {
        let mut body = node("body", "body", NodeKind::Property);
        body.role = Some(grapha_core::graph::NodeRole::EntryPoint);

        let mut text_primary_a = node("text_primary_a", "Text", NodeKind::View);
        text_primary_a
            .metadata
            .insert("l10n.ref_kind".to_string(), "wrapper".to_string());
        text_primary_a
            .metadata
            .insert("l10n.wrapper_name".to_string(), "welcomeTitle".to_string());

        let mut text_primary_b = node("text_primary_b", "Text", NodeKind::View);
        text_primary_b
            .metadata
            .insert("l10n.ref_kind".to_string(), "wrapper".to_string());
        text_primary_b
            .metadata
            .insert("l10n.wrapper_name".to_string(), "welcomeTitle".to_string());

        let mut text_secondary = node("text_secondary", "Text", NodeKind::View);
        text_secondary
            .metadata
            .insert("l10n.ref_kind".to_string(), "wrapper".to_string());
        text_secondary
            .metadata
            .insert("l10n.wrapper_name".to_string(), "welcomeTitle".to_string());

        let getter_primary = node("getter_primary", "getter:welcomeTitle", NodeKind::Function);
        let getter_secondary = node(
            "getter_secondary",
            "getter:welcomeTitle",
            NodeKind::Function,
        );

        let mut wrapper_primary = node("wrapper_primary", "welcomeTitle", NodeKind::Property);
        wrapper_primary
            .metadata
            .insert("l10n.wrapper.table".to_string(), "Localizable".to_string());
        wrapper_primary
            .metadata
            .insert("l10n.wrapper.key".to_string(), "welcome_title".to_string());

        let mut wrapper_secondary = node("wrapper_secondary", "welcomeTitle", NodeKind::Property);
        wrapper_secondary
            .metadata
            .insert("l10n.wrapper.table".to_string(), "Localizable".to_string());
        wrapper_secondary
            .metadata
            .insert("l10n.wrapper.key".to_string(), "welcome_title".to_string());

        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                body,
                text_primary_a,
                text_primary_b,
                text_secondary,
                getter_primary,
                getter_secondary,
                wrapper_primary,
                wrapper_secondary,
            ],
            edges: vec![
                Edge {
                    source: "body".to_string(),
                    target: "text_primary_a".to_string(),
                    kind: EdgeKind::Contains,
                    confidence: 1.0,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: vec![],
                    repo: None,
                },
                Edge {
                    source: "body".to_string(),
                    target: "text_primary_b".to_string(),
                    kind: EdgeKind::Contains,
                    confidence: 1.0,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: vec![],
                    repo: None,
                },
                Edge {
                    source: "body".to_string(),
                    target: "text_secondary".to_string(),
                    kind: EdgeKind::Contains,
                    confidence: 1.0,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: vec![],
                    repo: None,
                },
                Edge {
                    source: "text_primary_a".to_string(),
                    target: "wrapper_primary".to_string(),
                    kind: EdgeKind::TypeRef,
                    confidence: 1.0,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: vec![],
                    repo: None,
                },
                Edge {
                    source: "text_primary_b".to_string(),
                    target: "wrapper_primary".to_string(),
                    kind: EdgeKind::TypeRef,
                    confidence: 1.0,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: vec![],
                    repo: None,
                },
                Edge {
                    source: "text_secondary".to_string(),
                    target: "wrapper_secondary".to_string(),
                    kind: EdgeKind::TypeRef,
                    confidence: 1.0,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: vec![],
                    repo: None,
                },
            ],
        };

        let mut catalogs = LocalizationCatalogIndex::default();
        catalogs.insert(LocalizationCatalogRecord {
            table: "Localizable".to_string(),
            key: "welcome_title".to_string(),
            catalog_file: "Localizable.xcstrings".to_string(),
            catalog_dir: ".".to_string(),
            source_language: "en".to_string(),
            source_value: "Welcome".to_string(),
            status: "translated".to_string(),
            comment: None,
            translations: BTreeMap::new(),
        });

        let result = query_localize(&graph, &catalogs, "welcomeTitle").unwrap();
        assert_eq!(result.symbol.id, "wrapper_primary");
        assert_eq!(result.matches.len(), 2);
        assert!(
            result.matches.iter().all(|item| {
                item.reference.wrapper_symbol.as_deref() == Some("wrapper_primary")
            })
        );
    }

    #[test]
    fn localize_collapses_duplicate_wrapper_bindings_without_usage_sites() {
        let mut wrapper_a = node("wrapper_a", "welcomeTitle", NodeKind::Property);
        wrapper_a
            .metadata
            .insert("l10n.wrapper.table".to_string(), "Localizable".to_string());
        wrapper_a
            .metadata
            .insert("l10n.wrapper.key".to_string(), "welcome_title".to_string());

        let mut wrapper_b = node("wrapper_b", "welcomeTitle", NodeKind::Property);
        wrapper_b
            .metadata
            .insert("l10n.wrapper.table".to_string(), "Localizable".to_string());
        wrapper_b
            .metadata
            .insert("l10n.wrapper.key".to_string(), "welcome_title".to_string());

        let getter_a = node("getter_a", "getter:welcomeTitle", NodeKind::Function);
        let getter_b = node("getter_b", "getter:welcomeTitle", NodeKind::Function);

        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![wrapper_a, wrapper_b, getter_a, getter_b],
            edges: vec![],
        };

        let mut catalogs = LocalizationCatalogIndex::default();
        catalogs.insert(LocalizationCatalogRecord {
            table: "Localizable".to_string(),
            key: "welcome_title".to_string(),
            catalog_file: "Localizable.xcstrings".to_string(),
            catalog_dir: ".".to_string(),
            source_language: "en".to_string(),
            source_value: "Welcome".to_string(),
            status: "translated".to_string(),
            comment: None,
            translations: BTreeMap::new(),
        });

        let result = query_localize(&graph, &catalogs, "welcomeTitle").unwrap();
        assert_eq!(result.symbol.id, "wrapper_a");
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].match_kind, "wrapper_binding");
        assert_eq!(
            result.matches[0].reference.wrapper_symbol.as_deref(),
            Some("wrapper_a")
        );
        assert_eq!(result.matches[0].view.id, "wrapper_a");
    }
}
