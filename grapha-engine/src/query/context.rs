use std::collections::{HashMap, HashSet};

use grapha_core::graph::{EdgeKind, Graph};

use crate::symbol_locator::SymbolLocatorIndex;

use super::{
    ContextResult, QueryResolveError, SymbolInfo, SymbolRef, SymbolTreeRef,
    is_swiftui_invalidation_source, truncate_with_total,
};

/// Per-section caps for [`query_context_with_options`].
///
/// Each result vector (`callers`, `callees`, …) is truncated to at most
/// `limit` items independently, *after* sort/dedup, so the kept items are
/// deterministic. The default leaves results unbounded so internal callers
/// (web UI, MCP, in-process consumers) keep their existing behavior; only the
/// CLI dispatch path passes the user-supplied limit.
#[derive(Debug, Clone)]
pub struct ContextQueryOptions {
    pub limit: usize,
}

impl Default for ContextQueryOptions {
    fn default() -> Self {
        // Internal callers stay unbounded.
        Self { limit: usize::MAX }
    }
}

fn to_symbol_ref(node: &grapha_core::graph::Node, locators: &SymbolLocatorIndex) -> SymbolRef {
    SymbolRef::from_node(node).with_locator(locators.locator_for_node(node))
}

fn to_symbol_tree_ref(
    node: &grapha_core::graph::Node,
    contains: Vec<SymbolTreeRef>,
    locators: &SymbolLocatorIndex,
) -> SymbolTreeRef {
    SymbolTreeRef::from_node(node, contains).with_locator(locators.locator_for_node(node))
}

fn sort_refs_by_name(symbols: &mut [SymbolRef]) {
    symbols.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.file.cmp(&right.file))
            .then_with(|| left.id.cmp(&right.id))
    });
}

fn sort_ids_by_span<'a>(
    node_ids: &mut [&'a str],
    node_index: &HashMap<&'a str, &'a grapha_core::graph::Node>,
) {
    node_ids.sort_by(
        |left, right| match (node_index.get(*left), node_index.get(*right)) {
            (Some(left_node), Some(right_node)) => left_node
                .span
                .start
                .cmp(&right_node.span.start)
                .then_with(|| left_node.span.end.cmp(&right_node.span.end))
                .then_with(|| left_node.name.cmp(&right_node.name))
                .then_with(|| left_node.id.cmp(&right_node.id)),
            _ => left.cmp(right),
        },
    );
}

fn build_contains_tree<'a>(
    node_id: &'a str,
    contains_adj: &HashMap<&'a str, Vec<&'a str>>,
    type_ref_adj: &HashMap<&'a str, Vec<&'a str>>,
    node_index: &HashMap<&'a str, &'a grapha_core::graph::Node>,
    ancestors: &mut Vec<&'a str>,
    locators: &SymbolLocatorIndex,
) -> Option<SymbolTreeRef> {
    if ancestors.contains(&node_id) {
        return None;
    }

    let node = node_index.get(node_id).copied()?;
    ancestors.push(node_id);

    let mut child_ids = contains_adj.get(node_id).cloned().unwrap_or_default();
    sort_ids_by_span(&mut child_ids, node_index);

    let mut contains: Vec<_> = child_ids
        .into_iter()
        .filter_map(|child_id| {
            build_contains_tree(
                child_id,
                contains_adj,
                type_ref_adj,
                node_index,
                ancestors,
                locators,
            )
        })
        .collect();

    if node.kind == grapha_core::graph::NodeKind::View {
        let mut type_ref_ids = type_ref_adj.get(node_id).cloned().unwrap_or_default();
        sort_ids_by_span(&mut type_ref_ids, node_index);
        for type_ref_id in type_ref_ids {
            let Some(target) = node_index.get(type_ref_id).copied() else {
                continue;
            };
            if !matches!(
                target.kind,
                grapha_core::graph::NodeKind::Property | grapha_core::graph::NodeKind::Function
            ) {
                continue;
            }

            let mut inline_child_ids = contains_adj.get(type_ref_id).cloned().unwrap_or_default();
            sort_ids_by_span(&mut inline_child_ids, node_index);
            contains.extend(inline_child_ids.into_iter().filter_map(|child_id| {
                build_contains_tree(
                    child_id,
                    contains_adj,
                    type_ref_adj,
                    node_index,
                    ancestors,
                    locators,
                )
            }));
        }
    }

    ancestors.pop();
    Some(to_symbol_tree_ref(node, contains, locators))
}

fn collect_invalidation_sources<'a>(
    start_id: &'a str,
    reads_adj: &HashMap<&'a str, Vec<&'a str>>,
    node_index: &HashMap<&'a str, &'a grapha_core::graph::Node>,
    locators: &SymbolLocatorIndex,
) -> Vec<SymbolRef> {
    let mut visited = HashSet::new();
    let mut invalidation_sources = Vec::new();
    let mut stack = vec![start_id];

    while let Some(node_id) = stack.pop() {
        if !visited.insert(node_id) {
            continue;
        }

        let Some(node) = node_index.get(node_id).copied() else {
            continue;
        };
        if is_swiftui_invalidation_source(node) {
            invalidation_sources.push(to_symbol_ref(node, locators));
        }

        if let Some(next_ids) = reads_adj.get(node_id) {
            stack.extend(next_ids.iter().copied());
        }
    }

    invalidation_sources
}

pub fn query_context(graph: &Graph, query: &str) -> Result<ContextResult, QueryResolveError> {
    query_context_with_options(graph, query, &ContextQueryOptions::default())
}

pub fn query_context_with_options(
    graph: &Graph,
    query: &str,
    options: &ContextQueryOptions,
) -> Result<ContextResult, QueryResolveError> {
    let node = crate::query::resolve_node(graph, query)?;
    let locators = SymbolLocatorIndex::new(graph);

    let node_index: HashMap<&str, &grapha_core::graph::Node> =
        graph.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut contains_adj: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut type_ref_adj: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut reads_adj: HashMap<&str, Vec<&str>> = HashMap::new();

    // If the queried node is a type, collect its direct member IDs so that
    // edges targeting any member also count as callers/read_by of the type.
    let is_type_node = matches!(
        node.kind,
        grapha_core::graph::NodeKind::Struct
            | grapha_core::graph::NodeKind::Class
            | grapha_core::graph::NodeKind::Enum
            | grapha_core::graph::NodeKind::Protocol
            | grapha_core::graph::NodeKind::Trait
    );
    let member_ids: HashSet<&str> = if is_type_node {
        graph
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Contains && e.source == node.id)
            .map(|e| e.target.as_str())
            .collect()
    } else {
        HashSet::new()
    };

    let mut callers = Vec::new();
    let mut callees = Vec::new();
    let mut reads = Vec::new();
    let mut read_by = Vec::new();
    let mut contains_ids = Vec::new();
    let mut contained_by_ids = Vec::new();
    let mut implementors = Vec::new();
    let mut implements = Vec::new();
    let mut type_refs = Vec::new();

    for edge in &graph.edges {
        if edge.source == node.id
            && let Some(target) = node_index.get(edge.target.as_str())
        {
            let sym_ref = to_symbol_ref(target, &locators);
            match edge.kind {
                EdgeKind::Calls => callees.push(sym_ref),
                EdgeKind::Reads => reads.push(sym_ref),
                EdgeKind::Contains => contains_ids.push(target.id.as_str()),
                EdgeKind::Implements => implements.push(sym_ref),
                EdgeKind::TypeRef => type_refs.push(sym_ref),
                _ => {}
            }
        }
        if edge.target == node.id
            && let Some(source) = node_index.get(edge.source.as_str())
        {
            let sym_ref = to_symbol_ref(source, &locators);
            match edge.kind {
                EdgeKind::Calls => callers.push(sym_ref),
                EdgeKind::Reads => read_by.push(sym_ref),
                EdgeKind::Contains => contained_by_ids.push(source.id.as_str()),
                EdgeKind::Implements => implementors.push(sym_ref),
                _ => {}
            }
        }
        // For type nodes: edges targeting a direct member also count as
        // callers/read_by of the type itself (changing a type affects its members).
        if !member_ids.is_empty()
            && member_ids.contains(edge.target.as_str())
            && !matches!(edge.kind, EdgeKind::Contains)
            && edge.source != node.id
            && let Some(source) = node_index.get(edge.source.as_str())
        {
            // Skip if source is also a member (internal edges within the type)
            if !member_ids.contains(edge.source.as_str()) {
                let sym_ref = to_symbol_ref(source, &locators);
                match edge.kind {
                    EdgeKind::Calls => callers.push(sym_ref),
                    EdgeKind::Reads => read_by.push(sym_ref),
                    EdgeKind::Implements => implementors.push(sym_ref),
                    _ => {}
                }
            }
        }

        if edge.kind == EdgeKind::Contains {
            contains_adj
                .entry(edge.source.as_str())
                .or_default()
                .push(edge.target.as_str());
        } else if edge.kind == EdgeKind::Reads {
            reads_adj
                .entry(edge.source.as_str())
                .or_default()
                .push(edge.target.as_str());
        } else if edge.kind == EdgeKind::TypeRef {
            type_ref_adj
                .entry(edge.source.as_str())
                .or_default()
                .push(edge.target.as_str());
        }
    }

    sort_refs_by_name(&mut callers);
    callers.dedup_by(|a, b| a.id == b.id);
    sort_refs_by_name(&mut callees);
    sort_refs_by_name(&mut reads);
    sort_refs_by_name(&mut read_by);
    read_by.dedup_by(|a, b| a.id == b.id);
    sort_refs_by_name(&mut implementors);
    implementors.dedup_by(|a, b| a.id == b.id);
    sort_refs_by_name(&mut implements);
    sort_refs_by_name(&mut type_refs);
    let mut invalidation_sources =
        collect_invalidation_sources(&node.id, &reads_adj, &node_index, &locators);
    sort_refs_by_name(&mut invalidation_sources);
    sort_ids_by_span(&mut contains_ids, &node_index);
    sort_ids_by_span(&mut contained_by_ids, &node_index);

    let mut contains_tree: Vec<SymbolTreeRef> = contains_ids
        .iter()
        .filter_map(|node_id| {
            build_contains_tree(
                node_id,
                &contains_adj,
                &type_ref_adj,
                &node_index,
                &mut vec![],
                &locators,
            )
        })
        .collect();
    let mut contains: Vec<SymbolRef> = contains_ids
        .into_iter()
        .filter_map(|node_id| node_index.get(node_id).copied())
        .map(|node| to_symbol_ref(node, &locators))
        .collect();
    let mut contained_by: Vec<SymbolRef> = contained_by_ids
        .into_iter()
        .filter_map(|node_id| node_index.get(node_id).copied())
        .map(|node| to_symbol_ref(node, &locators))
        .collect();

    // Truncate per-section after sort/dedup so the kept items are deterministic.
    let limit = options.limit;
    let total_callers = truncate_with_total(&mut callers, limit);
    let total_callees = truncate_with_total(&mut callees, limit);
    let total_reads = truncate_with_total(&mut reads, limit);
    let total_read_by = truncate_with_total(&mut read_by, limit);
    let total_invalidation_sources = truncate_with_total(&mut invalidation_sources, limit);
    let total_contains = truncate_with_total(&mut contains, limit);
    let total_contained_by = truncate_with_total(&mut contained_by, limit);
    let total_implementors = truncate_with_total(&mut implementors, limit);
    let total_implements = truncate_with_total(&mut implements, limit);
    let total_type_refs = truncate_with_total(&mut type_refs, limit);

    // Keep `contains_tree` aligned with the truncated `contains` vector so
    // tree-mode and brief-mode rendering agree on which children survived
    // `--limit`. Top-level entries not in `contains` are dropped; deeper
    // descendants are unaffected (they're displayed inline under a kept root).
    let kept_contains_ids: HashSet<&str> =
        contains.iter().map(|symbol| symbol.id.as_str()).collect();
    contains_tree.retain(|tree_ref| kept_contains_ids.contains(tree_ref.id.as_str()));

    Ok(ContextResult {
        symbol: SymbolInfo::from_node(node).with_locator(locators.locator_for_node(node)),
        callers,
        total_callers,
        callees,
        total_callees,
        reads,
        total_reads,
        read_by,
        total_read_by,
        invalidation_sources,
        total_invalidation_sources,
        contains,
        total_contains,
        contains_tree,
        contained_by,
        total_contained_by,
        implementors,
        total_implementors,
        implements,
        total_implements,
        type_refs,
        total_type_refs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapha_core::graph::*;
    use std::collections::HashMap as StdHashMap;

    fn make_graph() -> Graph {
        Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                Node {
                    id: "a.rs::main".into(),
                    kind: NodeKind::Function,
                    name: "main".into(),
                    file: "a.rs".into(),
                    span: Span {
                        start: [0, 0],
                        end: [10, 0],
                    },
                    visibility: Visibility::Public,
                    metadata: StdHashMap::new(),
                    role: None,
                    signature: None,
                    doc_comment: None,
                    module: None,
                    snippet: None,
                    repo: None,
                },
                Node {
                    id: "a.rs::helper".into(),
                    kind: NodeKind::Function,
                    name: "helper".into(),
                    file: "a.rs".into(),
                    span: Span {
                        start: [12, 0],
                        end: [15, 0],
                    },
                    visibility: Visibility::Public,
                    metadata: StdHashMap::new(),
                    role: None,
                    signature: None,
                    doc_comment: None,
                    module: None,
                    snippet: None,
                    repo: None,
                },
            ],
            edges: vec![Edge {
                source: "a.rs::main".into(),
                target: "a.rs::helper".into(),
                kind: EdgeKind::Calls,
                confidence: 0.9,
                direction: None,
                operation: None,
                condition: None,
                async_boundary: None,
                provenance: Vec::new(),
                repo: None,
            }],
        }
    }

    #[test]
    fn context_finds_callers_and_callees() {
        let graph = make_graph();
        let ctx = query_context(&graph, "main").unwrap();
        assert_eq!(ctx.callees.len(), 1);
        assert_eq!(ctx.callees[0].name, "helper");
        assert_eq!(ctx.callers.len(), 0);

        let ctx2 = query_context(&graph, "helper").unwrap();
        assert_eq!(ctx2.callers.len(), 1);
        assert_eq!(ctx2.callers[0].name, "main");
    }

    #[test]
    fn context_tracks_read_dependencies() {
        let mk = |id: &str, name: &str, metadata: StdHashMap<String, String>| Node {
            id: id.into(),
            kind: NodeKind::Property,
            name: name.into(),
            file: "view.swift".into(),
            span: Span {
                start: [0, 0],
                end: [1, 0],
            },
            visibility: Visibility::Public,
            metadata,
            role: None,
            signature: None,
            doc_comment: None,
            module: None,
            snippet: None,
            repo: None,
        };

        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                mk("view.swift::RoomPage::body", "body", StdHashMap::new()),
                mk(
                    "view.swift::RoomPage::canShowGameRoom",
                    "canShowGameRoom",
                    StdHashMap::new(),
                ),
                mk(
                    "view.swift::RoomPage::roomMode",
                    "roomMode",
                    StdHashMap::from([(
                        "swiftui.invalidation_source".to_string(),
                        "true".to_string(),
                    )]),
                ),
            ],
            edges: vec![
                Edge {
                    source: "view.swift::RoomPage::body".into(),
                    target: "view.swift::RoomPage::canShowGameRoom".into(),
                    kind: EdgeKind::Reads,
                    confidence: 0.85,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: Vec::new(),
                    repo: None,
                },
                Edge {
                    source: "view.swift::RoomPage::canShowGameRoom".into(),
                    target: "view.swift::RoomPage::roomMode".into(),
                    kind: EdgeKind::Reads,
                    confidence: 0.85,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: Vec::new(),
                    repo: None,
                },
            ],
        };

        let body_ctx = query_context(&graph, "body").unwrap();
        assert_eq!(body_ctx.reads.len(), 1);
        assert_eq!(body_ctx.reads[0].name, "canShowGameRoom");
        assert_eq!(body_ctx.invalidation_sources.len(), 1);
        assert_eq!(body_ctx.invalidation_sources[0].name, "roomMode");

        let property_ctx = query_context(&graph, "canShowGameRoom").unwrap();
        assert_eq!(property_ctx.read_by.len(), 1);
        assert_eq!(property_ctx.read_by[0].name, "body");
        assert_eq!(property_ctx.invalidation_sources.len(), 1);
        assert_eq!(property_ctx.invalidation_sources[0].name, "roomMode");

        let source_ctx = query_context(&graph, "roomMode").unwrap();
        assert_eq!(source_ctx.invalidation_sources.len(), 1);
        assert_eq!(source_ctx.invalidation_sources[0].name, "roomMode");
    }

    #[test]
    fn context_includes_structural_relationships_in_span_order() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                Node {
                    id: "view.swift::ContentView".into(),
                    kind: NodeKind::Struct,
                    name: "ContentView".into(),
                    file: "view.swift".into(),
                    span: Span {
                        start: [0, 0],
                        end: [20, 0],
                    },
                    visibility: Visibility::Public,
                    metadata: StdHashMap::new(),
                    role: None,
                    signature: None,
                    doc_comment: None,
                    module: None,
                    snippet: None,
                    repo: None,
                },
                Node {
                    id: "view.swift::ContentView::body".into(),
                    kind: NodeKind::Property,
                    name: "body".into(),
                    file: "view.swift".into(),
                    span: Span {
                        start: [2, 4],
                        end: [12, 4],
                    },
                    visibility: Visibility::Public,
                    metadata: StdHashMap::new(),
                    role: Some(NodeRole::EntryPoint),
                    signature: None,
                    doc_comment: None,
                    module: None,
                    snippet: None,
                    repo: None,
                },
                Node {
                    id: "view.swift::ContentView::body::view:VStack@3:8".into(),
                    kind: NodeKind::View,
                    name: "VStack".into(),
                    file: "view.swift".into(),
                    span: Span {
                        start: [3, 8],
                        end: [10, 9],
                    },
                    visibility: Visibility::Private,
                    metadata: StdHashMap::new(),
                    role: None,
                    signature: None,
                    doc_comment: None,
                    module: None,
                    snippet: None,
                    repo: None,
                },
                Node {
                    id: "view.swift::ContentView::body::view:Text@4:12".into(),
                    kind: NodeKind::View,
                    name: "Text".into(),
                    file: "view.swift".into(),
                    span: Span {
                        start: [4, 12],
                        end: [4, 25],
                    },
                    visibility: Visibility::Private,
                    metadata: StdHashMap::new(),
                    role: None,
                    signature: None,
                    doc_comment: None,
                    module: None,
                    snippet: None,
                    repo: None,
                },
                Node {
                    id: "view.swift::ContentView::body::view:Row@5:12".into(),
                    kind: NodeKind::View,
                    name: "Row".into(),
                    file: "view.swift".into(),
                    span: Span {
                        start: [5, 12],
                        end: [5, 28],
                    },
                    visibility: Visibility::Private,
                    metadata: StdHashMap::new(),
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
                    source: "view.swift::ContentView".into(),
                    target: "view.swift::ContentView::body".into(),
                    kind: EdgeKind::Contains,
                    confidence: 1.0,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: Vec::new(),
                    repo: None,
                },
                Edge {
                    source: "view.swift::ContentView::body".into(),
                    target: "view.swift::ContentView::body::view:VStack@3:8".into(),
                    kind: EdgeKind::Contains,
                    confidence: 1.0,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: Vec::new(),
                    repo: None,
                },
                Edge {
                    source: "view.swift::ContentView::body::view:VStack@3:8".into(),
                    target: "view.swift::ContentView::body::view:Text@4:12".into(),
                    kind: EdgeKind::Contains,
                    confidence: 1.0,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: Vec::new(),
                    repo: None,
                },
                Edge {
                    source: "view.swift::ContentView::body::view:VStack@3:8".into(),
                    target: "view.swift::ContentView::body::view:Row@5:12".into(),
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
        };

        let ctx = query_context(&graph, "body").unwrap();
        assert_eq!(ctx.contains.len(), 1);
        assert_eq!(
            ctx.contains
                .iter()
                .map(|symbol| symbol.name.as_str())
                .collect::<Vec<_>>(),
            vec!["VStack"]
        );
        assert_eq!(ctx.contains_tree.len(), 1);
        assert_eq!(ctx.contains_tree[0].name, "VStack");
        assert_eq!(
            ctx.contains_tree[0]
                .contains
                .iter()
                .map(|symbol| symbol.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Text", "Row"]
        );
        assert_eq!(ctx.contained_by.len(), 1);
        assert_eq!(ctx.contained_by[0].name, "ContentView");
    }

    #[test]
    fn context_inlines_property_and_function_view_helpers_but_not_subview_types() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                Node {
                    id: "view.swift::ContentView".into(),
                    kind: NodeKind::Struct,
                    name: "ContentView".into(),
                    file: "view.swift".into(),
                    span: Span {
                        start: [0, 0],
                        end: [30, 0],
                    },
                    visibility: Visibility::Public,
                    metadata: StdHashMap::new(),
                    role: None,
                    signature: None,
                    doc_comment: None,
                    module: None,
                    snippet: None,
                    repo: None,
                },
                Node {
                    id: "view.swift::ContentView::body".into(),
                    kind: NodeKind::Property,
                    name: "body".into(),
                    file: "view.swift".into(),
                    span: Span {
                        start: [2, 4],
                        end: [16, 4],
                    },
                    visibility: Visibility::Public,
                    metadata: StdHashMap::new(),
                    role: Some(NodeRole::EntryPoint),
                    signature: None,
                    doc_comment: None,
                    module: None,
                    snippet: None,
                    repo: None,
                },
                Node {
                    id: "view.swift::ContentView::body::view:VStack@3:8".into(),
                    kind: NodeKind::View,
                    name: "VStack".into(),
                    file: "view.swift".into(),
                    span: Span {
                        start: [3, 8],
                        end: [12, 9],
                    },
                    visibility: Visibility::Private,
                    metadata: StdHashMap::new(),
                    role: None,
                    signature: None,
                    doc_comment: None,
                    module: None,
                    snippet: None,
                    repo: None,
                },
                Node {
                    id: "view.swift::ContentView::body::view:panel@4:12".into(),
                    kind: NodeKind::View,
                    name: "panel".into(),
                    file: "view.swift".into(),
                    span: Span {
                        start: [4, 12],
                        end: [4, 17],
                    },
                    visibility: Visibility::Private,
                    metadata: StdHashMap::new(),
                    role: None,
                    signature: None,
                    doc_comment: None,
                    module: None,
                    snippet: None,
                    repo: None,
                },
                Node {
                    id: "view.swift::ContentView::panel".into(),
                    kind: NodeKind::Property,
                    name: "panel".into(),
                    file: "view.swift".into(),
                    span: Span {
                        start: [18, 4],
                        end: [20, 4],
                    },
                    visibility: Visibility::Private,
                    metadata: StdHashMap::new(),
                    role: None,
                    signature: None,
                    doc_comment: None,
                    module: None,
                    snippet: None,
                    repo: None,
                },
                Node {
                    id: "view.swift::ContentView::panel::view:Text@19:8".into(),
                    kind: NodeKind::View,
                    name: "Text".into(),
                    file: "view.swift".into(),
                    span: Span {
                        start: [19, 8],
                        end: [19, 20],
                    },
                    visibility: Visibility::Private,
                    metadata: StdHashMap::new(),
                    role: None,
                    signature: None,
                    doc_comment: None,
                    module: None,
                    snippet: None,
                    repo: None,
                },
                Node {
                    id: "view.swift::ContentView::helper@5:12".into(),
                    kind: NodeKind::View,
                    name: "helper".into(),
                    file: "view.swift".into(),
                    span: Span {
                        start: [5, 12],
                        end: [5, 18],
                    },
                    visibility: Visibility::Private,
                    metadata: StdHashMap::new(),
                    role: None,
                    signature: None,
                    doc_comment: None,
                    module: None,
                    snippet: None,
                    repo: None,
                },
                Node {
                    id: "view.swift::ContentView::helper".into(),
                    kind: NodeKind::Function,
                    name: "helper".into(),
                    file: "view.swift".into(),
                    span: Span {
                        start: [22, 4],
                        end: [24, 4],
                    },
                    visibility: Visibility::Private,
                    metadata: StdHashMap::new(),
                    role: None,
                    signature: None,
                    doc_comment: None,
                    module: None,
                    snippet: None,
                    repo: None,
                },
                Node {
                    id: "view.swift::ContentView::helper::view:Image@23:8".into(),
                    kind: NodeKind::View,
                    name: "Image".into(),
                    file: "view.swift".into(),
                    span: Span {
                        start: [23, 8],
                        end: [23, 24],
                    },
                    visibility: Visibility::Private,
                    metadata: StdHashMap::new(),
                    role: None,
                    signature: None,
                    doc_comment: None,
                    module: None,
                    snippet: None,
                    repo: None,
                },
                Node {
                    id: "view.swift::ContentView::body::view:Row@6:12".into(),
                    kind: NodeKind::View,
                    name: "Row".into(),
                    file: "view.swift".into(),
                    span: Span {
                        start: [6, 12],
                        end: [6, 26],
                    },
                    visibility: Visibility::Private,
                    metadata: StdHashMap::new(),
                    role: None,
                    signature: None,
                    doc_comment: None,
                    module: None,
                    snippet: None,
                    repo: None,
                },
                Node {
                    id: "view.swift::Row".into(),
                    kind: NodeKind::Struct,
                    name: "Row".into(),
                    file: "view.swift".into(),
                    span: Span {
                        start: [26, 0],
                        end: [29, 0],
                    },
                    visibility: Visibility::Public,
                    metadata: StdHashMap::new(),
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
                    source: "view.swift::ContentView".into(),
                    target: "view.swift::ContentView::body".into(),
                    kind: EdgeKind::Contains,
                    confidence: 1.0,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: Vec::new(),
                    repo: None,
                },
                Edge {
                    source: "view.swift::ContentView::body".into(),
                    target: "view.swift::ContentView::body::view:VStack@3:8".into(),
                    kind: EdgeKind::Contains,
                    confidence: 1.0,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: Vec::new(),
                    repo: None,
                },
                Edge {
                    source: "view.swift::ContentView::body::view:VStack@3:8".into(),
                    target: "view.swift::ContentView::body::view:panel@4:12".into(),
                    kind: EdgeKind::Contains,
                    confidence: 1.0,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: Vec::new(),
                    repo: None,
                },
                Edge {
                    source: "view.swift::ContentView::body::view:VStack@3:8".into(),
                    target: "view.swift::ContentView::helper@5:12".into(),
                    kind: EdgeKind::Contains,
                    confidence: 1.0,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: Vec::new(),
                    repo: None,
                },
                Edge {
                    source: "view.swift::ContentView::body::view:VStack@3:8".into(),
                    target: "view.swift::ContentView::body::view:Row@6:12".into(),
                    kind: EdgeKind::Contains,
                    confidence: 1.0,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: Vec::new(),
                    repo: None,
                },
                Edge {
                    source: "view.swift::ContentView::panel".into(),
                    target: "view.swift::ContentView::panel::view:Text@19:8".into(),
                    kind: EdgeKind::Contains,
                    confidence: 1.0,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: Vec::new(),
                    repo: None,
                },
                Edge {
                    source: "view.swift::ContentView::helper".into(),
                    target: "view.swift::ContentView::helper::view:Image@23:8".into(),
                    kind: EdgeKind::Contains,
                    confidence: 1.0,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: Vec::new(),
                    repo: None,
                },
                Edge {
                    source: "view.swift::ContentView::body::view:panel@4:12".into(),
                    target: "view.swift::ContentView::panel".into(),
                    kind: EdgeKind::TypeRef,
                    confidence: 0.9,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: Vec::new(),
                    repo: None,
                },
                Edge {
                    source: "view.swift::ContentView::helper@5:12".into(),
                    target: "view.swift::ContentView::helper".into(),
                    kind: EdgeKind::TypeRef,
                    confidence: 0.9,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: Vec::new(),
                    repo: None,
                },
                Edge {
                    source: "view.swift::ContentView::body::view:Row@6:12".into(),
                    target: "view.swift::Row".into(),
                    kind: EdgeKind::TypeRef,
                    confidence: 0.9,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: Vec::new(),
                    repo: None,
                },
            ],
        };

        let ctx = query_context(&graph, "body").unwrap();
        let vstack = &ctx.contains_tree[0];
        assert_eq!(vstack.name, "VStack");
        assert_eq!(vstack.contains.len(), 3);
        assert_eq!(vstack.contains[0].name, "panel");
        assert_eq!(vstack.contains[0].contains[0].name, "Text");
        assert_eq!(vstack.contains[1].name, "helper");
        assert_eq!(vstack.contains[1].contains[0].name, "Image");
        assert_eq!(vstack.contains[2].name, "Row");
        assert!(
            vstack.contains[2].contains.is_empty(),
            "custom subview types should stay as component boundaries"
        );
    }

    #[test]
    fn context_returns_none_for_unknown() {
        let graph = make_graph();
        assert!(matches!(
            query_context(&graph, "nonexistent"),
            Err(QueryResolveError::NotFound { .. })
        ));
    }

    #[test]
    fn context_type_query_includes_member_callers() {
        let mk = |id: &str, name: &str, kind: NodeKind| Node {
            id: id.into(),
            kind,
            name: name.into(),
            file: "m.swift".into(),
            span: Span {
                start: [0, 0],
                end: [1, 0],
            },
            visibility: Visibility::Public,
            metadata: StdHashMap::new(),
            role: None,
            signature: None,
            doc_comment: None,
            module: None,
            snippet: None,
            repo: None,
        };

        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                mk("m.swift::Mutex", "Mutex", NodeKind::Struct),
                mk("m.swift::Mutex::withLock", "withLock", NodeKind::Function),
                mk("m.swift::Mutex::init", "init", NodeKind::Function),
                mk("caller.swift::useMutex", "useMutex", NodeKind::Function),
                mk("caller.swift::initMutex", "initMutex", NodeKind::Function),
                mk(
                    "m.swift::Mutex::internalHelper",
                    "internalHelper",
                    NodeKind::Function,
                ),
            ],
            edges: vec![
                Edge {
                    source: "m.swift::Mutex".into(),
                    target: "m.swift::Mutex::withLock".into(),
                    kind: EdgeKind::Contains,
                    confidence: 1.0,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: Vec::new(),
                    repo: None,
                },
                Edge {
                    source: "m.swift::Mutex".into(),
                    target: "m.swift::Mutex::init".into(),
                    kind: EdgeKind::Contains,
                    confidence: 1.0,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: Vec::new(),
                    repo: None,
                },
                Edge {
                    source: "m.swift::Mutex".into(),
                    target: "m.swift::Mutex::internalHelper".into(),
                    kind: EdgeKind::Contains,
                    confidence: 1.0,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: Vec::new(),
                    repo: None,
                },
                // External caller → member
                Edge {
                    source: "caller.swift::useMutex".into(),
                    target: "m.swift::Mutex::withLock".into(),
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
                    source: "caller.swift::initMutex".into(),
                    target: "m.swift::Mutex::init".into(),
                    kind: EdgeKind::Calls,
                    confidence: 0.9,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: Vec::new(),
                    repo: None,
                },
                // Internal call within the type (should NOT appear as a caller)
                Edge {
                    source: "m.swift::Mutex::internalHelper".into(),
                    target: "m.swift::Mutex::withLock".into(),
                    kind: EdgeKind::Calls,
                    confidence: 0.9,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: Vec::new(),
                    repo: None,
                },
            ],
        };

        let ctx = query_context(&graph, "Mutex").unwrap();
        // External callers of members should appear as callers of the type
        let caller_names: Vec<&str> = ctx.callers.iter().map(|c| c.name.as_str()).collect();
        assert!(
            caller_names.contains(&"useMutex"),
            "callers should include external caller of withLock"
        );
        assert!(
            caller_names.contains(&"initMutex"),
            "callers should include external caller of init"
        );
        // Internal member-to-member calls should NOT appear
        assert!(
            !caller_names.contains(&"internalHelper"),
            "internal member calls should not count as type callers"
        );
    }

    #[test]
    fn context_type_query_dedups_callers_that_hit_multiple_members() {
        let mk = |id: &str, name: &str, kind: NodeKind| Node {
            id: id.into(),
            kind,
            name: name.into(),
            file: "m.swift".into(),
            span: Span {
                start: [0, 0],
                end: [1, 0],
            },
            visibility: Visibility::Public,
            metadata: StdHashMap::new(),
            role: None,
            signature: None,
            doc_comment: None,
            module: None,
            snippet: None,
            repo: None,
        };

        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                mk("m.swift::Type", "Type", NodeKind::Struct),
                mk("m.swift::Type::a", "a", NodeKind::Function),
                mk("m.swift::Type::b", "b", NodeKind::Function),
                mk("c.swift::caller", "caller", NodeKind::Function),
            ],
            edges: vec![
                Edge {
                    source: "m.swift::Type".into(),
                    target: "m.swift::Type::a".into(),
                    kind: EdgeKind::Contains,
                    confidence: 1.0,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: Vec::new(),
                    repo: None,
                },
                Edge {
                    source: "m.swift::Type".into(),
                    target: "m.swift::Type::b".into(),
                    kind: EdgeKind::Contains,
                    confidence: 1.0,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: Vec::new(),
                    repo: None,
                },
                // caller calls BOTH members — should appear only once
                Edge {
                    source: "c.swift::caller".into(),
                    target: "m.swift::Type::a".into(),
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
                    source: "c.swift::caller".into(),
                    target: "m.swift::Type::b".into(),
                    kind: EdgeKind::Calls,
                    confidence: 0.9,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: Vec::new(),
                    repo: None,
                },
            ],
        };

        let ctx = query_context(&graph, "Type").unwrap();
        let caller_ids: Vec<&str> = ctx.callers.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            caller_ids,
            vec!["c.swift::caller"],
            "caller should appear only once even if it hits multiple members"
        );
    }

    #[test]
    fn context_truncates_each_section_independently() {
        let mk = |id: &str, name: &str, kind: NodeKind| Node {
            id: id.into(),
            kind,
            name: name.into(),
            file: "graph.swift".into(),
            span: Span {
                start: [0, 0],
                end: [1, 0],
            },
            visibility: Visibility::Public,
            metadata: StdHashMap::new(),
            role: None,
            signature: None,
            doc_comment: None,
            module: None,
            snippet: None,
            repo: None,
        };

        let calls = |source: &str, target: &str| Edge {
            source: source.into(),
            target: target.into(),
            kind: EdgeKind::Calls,
            confidence: 0.9,
            direction: None,
            operation: None,
            condition: None,
            async_boundary: None,
            provenance: Vec::new(),
            repo: None,
        };
        let implements = |source: &str, target: &str| Edge {
            source: source.into(),
            target: target.into(),
            kind: EdgeKind::Implements,
            confidence: 1.0,
            direction: None,
            operation: None,
            condition: None,
            async_boundary: None,
            provenance: Vec::new(),
            repo: None,
        };

        // The queried node ("Service") has 3 callers (caller_a/b/c), 3 callees
        // (callee_a/b/c), and 3 implementors (impl_a/b/c). NodeKind::Trait is
        // used so the type-aware caller-from-member path is triggered without
        // adding spurious callers (no Contains edges).
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                mk("Service", "Service", NodeKind::Trait),
                mk("caller_a", "caller_a", NodeKind::Function),
                mk("caller_b", "caller_b", NodeKind::Function),
                mk("caller_c", "caller_c", NodeKind::Function),
                mk("callee_a", "callee_a", NodeKind::Function),
                mk("callee_b", "callee_b", NodeKind::Function),
                mk("callee_c", "callee_c", NodeKind::Function),
                mk("impl_a", "ImplA", NodeKind::Struct),
                mk("impl_b", "ImplB", NodeKind::Struct),
                mk("impl_c", "ImplC", NodeKind::Struct),
            ],
            edges: vec![
                calls("caller_a", "Service"),
                calls("caller_b", "Service"),
                calls("caller_c", "Service"),
                calls("Service", "callee_a"),
                calls("Service", "callee_b"),
                calls("Service", "callee_c"),
                implements("impl_a", "Service"),
                implements("impl_b", "Service"),
                implements("impl_c", "Service"),
            ],
        };

        let truncated =
            query_context_with_options(&graph, "Service", &ContextQueryOptions { limit: 1 })
                .unwrap();
        assert_eq!(truncated.callers.len(), 1, "callers should be truncated");
        assert_eq!(truncated.total_callers, 3);
        assert_eq!(truncated.callees.len(), 1, "callees should be truncated");
        assert_eq!(truncated.total_callees, 3);
        assert_eq!(
            truncated.implementors.len(),
            1,
            "implementors should be truncated"
        );
        assert_eq!(truncated.total_implementors, 3);

        let unbounded = query_context_with_options(
            &graph,
            "Service",
            &ContextQueryOptions { limit: usize::MAX },
        )
        .unwrap();
        assert_eq!(unbounded.callers.len(), 3);
        assert_eq!(unbounded.total_callers, 3);
        assert_eq!(unbounded.callees.len(), 3);
        assert_eq!(unbounded.total_callees, 3);
        assert_eq!(unbounded.implementors.len(), 3);
        assert_eq!(unbounded.total_implementors, 3);
    }

    #[test]
    fn context_contains_tree_pruned_to_kept_contains() {
        // A struct with 5 contained members. Under `--limit=2`, both the
        // truncated `contains` vector AND `contains_tree` must drop the same
        // top-level entries, and surviving entries must align in order.
        let mk = |id: &str, name: &str, kind: NodeKind, span_start: [usize; 2]| Node {
            id: id.into(),
            kind,
            name: name.into(),
            file: "model.rs".into(),
            span: Span {
                start: span_start,
                end: [span_start[0] + 1, 0],
            },
            visibility: Visibility::Public,
            metadata: StdHashMap::new(),
            role: None,
            signature: None,
            doc_comment: None,
            module: None,
            snippet: None,
            repo: None,
        };
        let contains = |source: &str, target: &str| Edge {
            source: source.into(),
            target: target.into(),
            kind: EdgeKind::Contains,
            confidence: 1.0,
            direction: None,
            operation: None,
            condition: None,
            async_boundary: None,
            provenance: Vec::new(),
            repo: None,
        };

        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                mk("Holder", "Holder", NodeKind::Struct, [0, 0]),
                mk("Holder::a", "a", NodeKind::Property, [2, 4]),
                mk("Holder::b", "b", NodeKind::Property, [3, 4]),
                mk("Holder::c", "c", NodeKind::Property, [4, 4]),
                mk("Holder::d", "d", NodeKind::Property, [5, 4]),
                mk("Holder::e", "e", NodeKind::Property, [6, 4]),
            ],
            edges: vec![
                contains("Holder", "Holder::a"),
                contains("Holder", "Holder::b"),
                contains("Holder", "Holder::c"),
                contains("Holder", "Holder::d"),
                contains("Holder", "Holder::e"),
            ],
        };

        // Unbounded baseline: all 5 entries in both `contains` and `contains_tree`.
        let unbounded = query_context_with_options(
            &graph,
            "Holder",
            &ContextQueryOptions { limit: usize::MAX },
        )
        .unwrap();
        assert_eq!(unbounded.contains.len(), 5);
        assert_eq!(unbounded.contains_tree.len(), 5);
        assert_eq!(unbounded.total_contains, 5);

        let truncated =
            query_context_with_options(&graph, "Holder", &ContextQueryOptions { limit: 2 })
                .unwrap();
        assert_eq!(truncated.contains.len(), 2, "contains capped at limit=2");
        assert_eq!(
            truncated.contains_tree.len(),
            2,
            "contains_tree must be pruned to match `contains` truncation"
        );
        // The two vectors must agree on which top-level IDs survived.
        for (i, kept) in truncated.contains.iter().enumerate() {
            assert_eq!(
                truncated.contains_tree[i].id, kept.id,
                "contains_tree[{i}] must match contains[{i}] ID after truncation"
            );
        }
        // Pre-truncation total stays.
        assert_eq!(truncated.total_contains, 5);
    }
}
