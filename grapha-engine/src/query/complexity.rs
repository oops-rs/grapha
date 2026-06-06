use std::collections::{HashMap, HashSet};

use serde::Serialize;

use grapha_core::graph::{EdgeKind, Graph, Node, NodeKind};

use super::{
    QueryResolveError, SymbolInfo, SymbolRef, is_swiftui_invalidation_source, normalize_symbol_name,
};

#[derive(Debug, Serialize)]
pub struct ComplexityResult {
    pub symbol: SymbolInfo,
    pub metrics: ComplexityMetrics,
    pub severity: String,
}

#[derive(Debug, Serialize)]
pub struct ComplexityMetrics {
    pub property_count: usize,
    pub method_count: usize,
    pub dependency_count: usize,
    pub invalidation_source_count: usize,
    pub init_parameter_count: usize,
    pub extension_count: usize,
    pub contains_depth: usize,
    pub direct_child_count: usize,
    pub blast_radius: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub invalidation_sources: Vec<SymbolRef>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub heaviest_dependencies: Vec<SymbolRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub swiftui_body: Option<SwiftUiBodyMetrics>,
}

#[derive(Debug, Serialize)]
pub struct SwiftUiBodyMetrics {
    pub root_count: usize,
    pub view_count: usize,
    pub branch_count: usize,
    pub nesting_depth: usize,
    pub direct_child_count: usize,
    pub dependency_count: usize,
    pub invalidation_source_count: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub invalidation_sources: Vec<SymbolRef>,
}

pub(crate) const SWIFTUI_BODY_COMPLEXITY_SMELL_THRESHOLD: usize = 3;

fn to_symbol_ref(node: &Node) -> SymbolRef {
    SymbolRef::from_node(node)
}

fn sort_refs_by_name(symbols: &mut [SymbolRef]) {
    symbols.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.file.cmp(&right.file))
            .then_with(|| left.id.cmp(&right.id))
    });
}

fn count_init_params(node: &Node) -> usize {
    let name = &node.name;
    if !name.starts_with("init(") {
        return 0;
    }
    let inner = name
        .strip_prefix("init(")
        .and_then(|s| s.strip_suffix(')'))
        .unwrap_or("");
    if inner.is_empty() {
        return 0;
    }
    inner.split(':').filter(|s| !s.is_empty()).count()
}

fn measure_contains_depth<'a>(
    node_id: &'a str,
    contains_adj: &HashMap<&'a str, Vec<&'a str>>,
    visited: &mut HashSet<&'a str>,
) -> usize {
    if !visited.insert(node_id) {
        return 0;
    }
    let children = match contains_adj.get(node_id) {
        Some(c) => c,
        None => return 0,
    };
    let max_child_depth = children
        .iter()
        .map(|child| measure_contains_depth(child, contains_adj, visited))
        .max()
        .unwrap_or(0);
    1 + max_child_depth
}

fn measure_descendant_depth<'a>(
    node_id: &'a str,
    contains_adj: &HashMap<&'a str, Vec<&'a str>>,
    visited: &mut HashSet<&'a str>,
) -> usize {
    if !visited.insert(node_id) {
        return 0;
    }

    let Some(children) = contains_adj.get(node_id) else {
        return 0;
    };

    let max_child_depth = children
        .iter()
        .map(|child| measure_descendant_depth(child, contains_adj, visited))
        .max()
        .unwrap_or(0);
    1 + max_child_depth
}

fn is_type_node(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Class
            | NodeKind::Struct
            | NodeKind::Enum
            | NodeKind::Trait
            | NodeKind::Protocol
            | NodeKind::Extension
    )
}

fn is_swiftui_body_node(node: &Node) -> bool {
    matches!(node.kind, NodeKind::Property | NodeKind::Function)
        && normalize_symbol_name(&node.name) == "body"
}

fn collect_swiftui_body_root_ids<'a>(
    node: &'a Node,
    node_index: &HashMap<&'a str, &'a Node>,
    contains_adj: &HashMap<&'a str, Vec<&'a str>>,
    type_ref_adj: &HashMap<&'a str, Vec<&'a str>>,
    implemented_by_adj: &HashMap<&'a str, Vec<&'a str>>,
) -> Vec<&'a str> {
    if is_swiftui_body_node(node) {
        return vec![node.id.as_str()];
    }

    if !is_type_node(node.kind) {
        return Vec::new();
    }

    let mut body_root_ids = Vec::new();
    let mut seen = HashSet::new();

    for candidate_id in contains_adj
        .get(node.id.as_str())
        .into_iter()
        .flatten()
        .chain(
            implemented_by_adj
                .get(node.id.as_str())
                .into_iter()
                .flatten(),
        )
        .chain(type_ref_adj.get(node.id.as_str()).into_iter().flatten())
    {
        let Some(candidate) = node_index.get(candidate_id).copied() else {
            continue;
        };
        if is_swiftui_body_node(candidate) && seen.insert(candidate.id.as_str()) {
            body_root_ids.push(candidate.id.as_str());
        }
    }

    body_root_ids
}

fn collect_invalidation_sources_from_roots<'a>(
    root_ids: &[&'a str],
    reads_adj: &HashMap<&'a str, Vec<&'a str>>,
    node_index: &HashMap<&'a str, &'a Node>,
) -> Vec<SymbolRef> {
    let mut visited = HashSet::new();
    let mut invalidation_sources = Vec::new();
    let mut seen_sources = HashSet::new();
    let mut stack: Vec<&'a str> = root_ids.to_vec();

    while let Some(node_id) = stack.pop() {
        if !visited.insert(node_id) {
            continue;
        }

        let Some(node) = node_index.get(node_id).copied() else {
            continue;
        };
        if is_swiftui_invalidation_source(node) && seen_sources.insert(node.id.as_str()) {
            invalidation_sources.push(to_symbol_ref(node));
        }

        if let Some(next_ids) = reads_adj.get(node_id) {
            stack.extend(next_ids.iter().copied());
        }
    }

    sort_refs_by_name(&mut invalidation_sources);
    invalidation_sources
}

fn collect_swiftui_body_metrics<'a>(
    root_ids: &[&'a str],
    node_index: &HashMap<&'a str, &'a Node>,
    contains_adj: &HashMap<&'a str, Vec<&'a str>>,
    reads_adj: &HashMap<&'a str, Vec<&'a str>>,
    callee_adj: &HashMap<&'a str, Vec<&'a str>>,
) -> Option<SwiftUiBodyMetrics> {
    if root_ids.is_empty() {
        return None;
    }

    let mut scoped_ids = HashSet::new();
    let mut stack: Vec<&'a str> = root_ids.to_vec();
    while let Some(node_id) = stack.pop() {
        if !scoped_ids.insert(node_id) {
            continue;
        }

        if let Some(children) = contains_adj.get(node_id) {
            stack.extend(children.iter().copied());
        }
    }

    let mut direct_child_ids = HashSet::new();
    for root_id in root_ids {
        if let Some(children) = contains_adj.get(root_id) {
            direct_child_ids.extend(children.iter().copied());
        }
    }

    let mut view_count = 0usize;
    let mut branch_count = 0usize;
    for node_id in &scoped_ids {
        let Some(node) = node_index.get(node_id).copied() else {
            continue;
        };
        match node.kind {
            NodeKind::View => view_count += 1,
            NodeKind::Branch => branch_count += 1,
            _ => {}
        }
    }

    let nesting_depth = root_ids
        .iter()
        .map(|root_id| measure_descendant_depth(root_id, contains_adj, &mut HashSet::new()))
        .max()
        .unwrap_or(0);

    let mut dependency_ids = HashSet::new();
    for node_id in &scoped_ids {
        if let Some(reads) = reads_adj.get(node_id) {
            for target_id in reads {
                if !scoped_ids.contains(target_id) {
                    dependency_ids.insert(*target_id);
                }
            }
        }
        if let Some(callees) = callee_adj.get(node_id) {
            for target_id in callees {
                if !scoped_ids.contains(target_id) {
                    dependency_ids.insert(*target_id);
                }
            }
        }
    }

    let invalidation_sources =
        collect_invalidation_sources_from_roots(root_ids, reads_adj, node_index);
    let invalidation_source_count = invalidation_sources.len();

    Some(SwiftUiBodyMetrics {
        root_count: root_ids.len(),
        view_count,
        branch_count,
        nesting_depth,
        direct_child_count: direct_child_ids.len(),
        dependency_count: dependency_ids.len(),
        invalidation_source_count,
        invalidation_sources,
    })
}

pub(crate) fn swiftui_body_complexity_score(body_metrics: &SwiftUiBodyMetrics) -> usize {
    let mut score = 0usize;

    if body_metrics.view_count > 20 {
        score += 2;
    } else if body_metrics.view_count > 10 {
        score += 1;
    }
    if body_metrics.branch_count > 3 {
        score += 2;
    } else if body_metrics.branch_count > 1 {
        score += 1;
    }
    if body_metrics.nesting_depth > 5 {
        score += 2;
    } else if body_metrics.nesting_depth > 2 {
        score += 1;
    }
    if body_metrics.dependency_count > 8 {
        score += 2;
    } else if body_metrics.dependency_count > 2 {
        score += 1;
    }

    score
}

pub(crate) fn swiftui_body_metrics_for_node(
    graph: &Graph,
    node: &Node,
) -> Option<SwiftUiBodyMetrics> {
    let node_index: HashMap<&str, &Node> = graph.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut implemented_by_adj: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut contains_adj: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut type_ref_adj: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut reads_adj: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut callee_adj: HashMap<&str, Vec<&str>> = HashMap::new();

    for edge in &graph.edges {
        match edge.kind {
            EdgeKind::Implements => {
                implemented_by_adj
                    .entry(edge.target.as_str())
                    .or_default()
                    .push(edge.source.as_str());
            }
            EdgeKind::Contains => {
                contains_adj
                    .entry(edge.source.as_str())
                    .or_default()
                    .push(edge.target.as_str());
            }
            EdgeKind::TypeRef => {
                type_ref_adj
                    .entry(edge.source.as_str())
                    .or_default()
                    .push(edge.target.as_str());
            }
            EdgeKind::Reads => {
                reads_adj
                    .entry(edge.source.as_str())
                    .or_default()
                    .push(edge.target.as_str());
            }
            EdgeKind::Calls => {
                callee_adj
                    .entry(edge.source.as_str())
                    .or_default()
                    .push(edge.target.as_str());
            }
            _ => {}
        }
    }

    collect_swiftui_body_metrics(
        &collect_swiftui_body_root_ids(
            node,
            &node_index,
            &contains_adj,
            &type_ref_adj,
            &implemented_by_adj,
        ),
        &node_index,
        &contains_adj,
        &reads_adj,
        &callee_adj,
    )
}

fn severity_from_score(score: usize) -> &'static str {
    match score {
        0..=2 => "low",
        3..=5 => "medium",
        6..=8 => "high",
        _ => "critical",
    }
}

pub fn query_complexity(graph: &Graph, query: &str) -> Result<ComplexityResult, QueryResolveError> {
    let node = super::resolve_node(graph, query)?;
    let node_id = &node.id;
    let node_index: HashMap<&str, &Node> = graph.nodes.iter().map(|n| (n.id.as_str(), n)).collect();

    // Build adjacency maps
    let mut implemented_by_adj: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut contains_adj: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut type_ref_adj: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut reads_adj: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut callee_adj: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut reverse_adj: HashMap<&str, Vec<&str>> = HashMap::new();

    for edge in &graph.edges {
        match edge.kind {
            EdgeKind::Implements => {
                implemented_by_adj
                    .entry(edge.target.as_str())
                    .or_default()
                    .push(edge.source.as_str());
            }
            EdgeKind::Contains => {
                contains_adj
                    .entry(edge.source.as_str())
                    .or_default()
                    .push(edge.target.as_str());
            }
            EdgeKind::TypeRef => {
                type_ref_adj
                    .entry(edge.source.as_str())
                    .or_default()
                    .push(edge.target.as_str());
            }
            EdgeKind::Reads => {
                reads_adj
                    .entry(edge.source.as_str())
                    .or_default()
                    .push(edge.target.as_str());
            }
            EdgeKind::Calls => {
                callee_adj
                    .entry(edge.source.as_str())
                    .or_default()
                    .push(edge.target.as_str());
            }
            _ => {}
        }
        // Reverse edges for blast radius (all edge kinds)
        reverse_adj
            .entry(edge.target.as_str())
            .or_default()
            .push(edge.source.as_str());
    }

    // Implementors: symbols that implement this type (properties, methods)
    let implementors: Vec<&str> = implemented_by_adj
        .get(node_id.as_str())
        .cloned()
        .unwrap_or_default();

    let property_count = implementors
        .iter()
        .filter(|id| {
            node_index
                .get(*id)
                .is_some_and(|n| matches!(n.kind, NodeKind::Property | NodeKind::Field))
        })
        .count();

    let method_count = implementors
        .iter()
        .filter(|id| {
            node_index
                .get(*id)
                .is_some_and(|n| n.kind == NodeKind::Function)
        })
        .count();

    // Init parameter count: find the longest init among implementors
    let init_parameter_count = implementors
        .iter()
        .filter_map(|id| node_index.get(*id).copied())
        .filter(|n| n.kind == NodeKind::Function && n.name.starts_with("init("))
        .map(count_init_params)
        .max()
        .unwrap_or(0);

    // Extension count: type_refs that are extensions pointing to this node
    let extension_count = type_ref_adj
        .iter()
        .filter(|(source, _)| {
            node_index
                .get(*source)
                .is_some_and(|n| n.kind == NodeKind::Extension)
        })
        .filter(|(_, targets)| targets.contains(&node_id.as_str()))
        .count();

    // Dependency count: unique symbols read by body or methods of this type
    let mut dependencies: HashSet<&str> = HashSet::new();
    for impl_id in &implementors {
        if let Some(reads) = reads_adj.get(*impl_id) {
            for read in reads {
                dependencies.insert(read);
            }
        }
        if let Some(callees) = callee_adj.get(*impl_id) {
            for callee in callees {
                dependencies.insert(callee);
            }
        }
    }

    // Invalidation sources: observable properties that trigger re-evaluation
    let invalidation_sources: Vec<SymbolRef> = implementors
        .iter()
        .filter_map(|id| node_index.get(*id).copied())
        .filter(|n| is_swiftui_invalidation_source(n))
        .map(to_symbol_ref)
        .collect();
    let invalidation_source_count = invalidation_sources.len();

    // Contains depth
    let contains_depth =
        measure_contains_depth(node_id.as_str(), &contains_adj, &mut HashSet::new());

    // Direct children in contains tree
    let direct_child_count = contains_adj
        .get(node_id.as_str())
        .map(|c| c.len())
        .unwrap_or(0);

    // Blast radius: BFS depth-1 from this node via reverse adjacency
    let mut blast_radius_set: HashSet<&str> = HashSet::new();
    if let Some(neighbors) = reverse_adj.get(node_id.as_str()) {
        for n in neighbors {
            blast_radius_set.insert(n);
        }
    }
    // Also count implementors as depth-1 blast radius
    for impl_id in &implementors {
        blast_radius_set.insert(impl_id);
    }
    let blast_radius = blast_radius_set.len();

    // Heaviest dependencies (top 10 by kind preference: types before functions)
    let mut heaviest_dependencies: Vec<SymbolRef> = dependencies
        .iter()
        .filter_map(|id| node_index.get(*id).copied())
        .filter(|n| !matches!(n.kind, NodeKind::View | NodeKind::Branch))
        .map(to_symbol_ref)
        .collect();
    sort_refs_by_name(&mut heaviest_dependencies);
    heaviest_dependencies.truncate(10);

    let swiftui_body = collect_swiftui_body_metrics(
        &collect_swiftui_body_root_ids(
            node,
            &node_index,
            &contains_adj,
            &type_ref_adj,
            &implemented_by_adj,
        ),
        &node_index,
        &contains_adj,
        &reads_adj,
        &callee_adj,
    );

    // Severity scoring
    let mut severity_score = 0usize;
    if property_count > 15 {
        severity_score += 3;
    } else if property_count > 8 {
        severity_score += 2;
    } else if property_count > 5 {
        severity_score += 1;
    }
    if invalidation_source_count > 5 {
        severity_score += 3;
    } else if invalidation_source_count > 3 {
        severity_score += 2;
    } else if invalidation_source_count > 1 {
        severity_score += 1;
    }
    if init_parameter_count > 8 {
        severity_score += 2;
    } else if init_parameter_count > 5 {
        severity_score += 1;
    }
    if extension_count > 4 {
        severity_score += 2;
    } else if extension_count > 2 {
        severity_score += 1;
    }
    if contains_depth > 5 {
        severity_score += 2;
    } else if contains_depth > 3 {
        severity_score += 1;
    }
    if let Some(body_metrics) = &swiftui_body {
        severity_score += swiftui_body_complexity_score(body_metrics);
    }

    let metrics = ComplexityMetrics {
        property_count,
        method_count,
        dependency_count: dependencies.len(),
        invalidation_source_count,
        init_parameter_count,
        extension_count,
        contains_depth,
        direct_child_count,
        blast_radius,
        invalidation_sources,
        heaviest_dependencies,
        swiftui_body,
    };

    Ok(ComplexityResult {
        symbol: SymbolInfo::from_node(node),
        metrics,
        severity: severity_from_score(severity_score).to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapha_core::graph::{Edge, Node, Span, Visibility};
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn make_node(id: &str, name: &str, kind: NodeKind, file: &str) -> Node {
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
            module: None,
            snippet: None,
            repo: None,
        }
    }

    fn make_node_with_metadata(
        id: &str,
        name: &str,
        kind: NodeKind,
        file: &str,
        metadata: HashMap<String, String>,
    ) -> Node {
        Node {
            metadata,
            ..make_node(id, name, kind, file)
        }
    }

    fn make_edge(source: &str, target: &str, kind: EdgeKind) -> Edge {
        Edge {
            source: source.into(),
            target: target.into(),
            kind,
            confidence: 1.0,
            direction: None,
            operation: None,
            condition: None,
            async_boundary: None,
            provenance: vec![],
            repo: None,
        }
    }

    #[test]
    fn counts_properties_and_methods() {
        let graph = Graph {
            version: String::new(),
            nodes: vec![
                make_node("s:MyType", "MyType", NodeKind::Struct, "MyType.swift"),
                make_node("s:prop1", "name", NodeKind::Property, "MyType.swift"),
                make_node("s:prop2", "age", NodeKind::Property, "MyType.swift"),
                make_node("s:func1", "greet()", NodeKind::Function, "MyType.swift"),
            ],
            edges: vec![
                make_edge("s:prop1", "s:MyType", EdgeKind::Implements),
                make_edge("s:prop2", "s:MyType", EdgeKind::Implements),
                make_edge("s:func1", "s:MyType", EdgeKind::Implements),
            ],
        };

        let result = query_complexity(&graph, "MyType").unwrap();
        assert_eq!(result.metrics.property_count, 2);
        assert_eq!(result.metrics.method_count, 1);
        assert_eq!(result.severity, "low");
    }

    #[test]
    fn counts_init_parameters() {
        let graph = Graph {
            version: String::new(),
            nodes: vec![
                make_node("s:T", "T", NodeKind::Struct, "T.swift"),
                make_node(
                    "s:init",
                    "init(a:b:c:d:e:f:g:h:i:)",
                    NodeKind::Function,
                    "T.swift",
                ),
            ],
            edges: vec![make_edge("s:init", "s:T", EdgeKind::Implements)],
        };

        let result = query_complexity(&graph, "T").unwrap();
        assert_eq!(result.metrics.init_parameter_count, 9);
    }

    #[test]
    fn reports_swiftui_body_structure_metrics_for_view_types() {
        let graph = Graph {
            version: String::new(),
            nodes: vec![
                make_node("view", "ContentView", NodeKind::Struct, "ContentView.swift"),
                make_node(
                    "view::body",
                    "body",
                    NodeKind::Property,
                    "ContentView.swift",
                ),
                make_node(
                    "view::body::view:VStack@1:0",
                    "VStack",
                    NodeKind::View,
                    "ContentView.swift",
                ),
                make_node(
                    "view::body::branch:if@2:0",
                    "if isLoading",
                    NodeKind::Branch,
                    "ContentView.swift",
                ),
                make_node(
                    "view::body::view:ProgressView@3:0",
                    "ProgressView",
                    NodeKind::View,
                    "ContentView.swift",
                ),
                make_node(
                    "view::body::branch:else@4:0",
                    "else",
                    NodeKind::Branch,
                    "ContentView.swift",
                ),
                make_node(
                    "view::body::view:List@5:0",
                    "List",
                    NodeKind::View,
                    "ContentView.swift",
                ),
                make_node(
                    "view::body::view:Row@6:0",
                    "Row",
                    NodeKind::View,
                    "ContentView.swift",
                ),
                make_node(
                    "view::title",
                    "title",
                    NodeKind::Property,
                    "ContentView.swift",
                ),
                make_node(
                    "view::items",
                    "items",
                    NodeKind::Property,
                    "ContentView.swift",
                ),
                make_node_with_metadata(
                    "view::isLoading",
                    "isLoading",
                    NodeKind::Property,
                    "ContentView.swift",
                    HashMap::from([(
                        "swiftui.invalidation_source".to_string(),
                        "true".to_string(),
                    )]),
                ),
                make_node(
                    "view::load",
                    "load()",
                    NodeKind::Function,
                    "ContentView.swift",
                ),
            ],
            edges: vec![
                make_edge("view", "view::body", EdgeKind::Contains),
                make_edge(
                    "view::body",
                    "view::body::view:VStack@1:0",
                    EdgeKind::Contains,
                ),
                make_edge(
                    "view::body::view:VStack@1:0",
                    "view::body::branch:if@2:0",
                    EdgeKind::Contains,
                ),
                make_edge(
                    "view::body::branch:if@2:0",
                    "view::body::view:ProgressView@3:0",
                    EdgeKind::Contains,
                ),
                make_edge(
                    "view::body::view:VStack@1:0",
                    "view::body::branch:else@4:0",
                    EdgeKind::Contains,
                ),
                make_edge(
                    "view::body::branch:else@4:0",
                    "view::body::view:List@5:0",
                    EdgeKind::Contains,
                ),
                make_edge(
                    "view::body::view:List@5:0",
                    "view::body::view:Row@6:0",
                    EdgeKind::Contains,
                ),
                make_edge("view::body", "view::title", EdgeKind::Reads),
                make_edge("view::body", "view::items", EdgeKind::Reads),
                make_edge("view::body", "view::load", EdgeKind::Calls),
                make_edge("view::title", "view::isLoading", EdgeKind::Reads),
            ],
        };

        let result = query_complexity(&graph, "ContentView").unwrap();
        let body = result
            .metrics
            .swiftui_body
            .as_ref()
            .expect("expected SwiftUI body metrics");

        assert_eq!(body.root_count, 1);
        assert_eq!(body.view_count, 4);
        assert_eq!(body.branch_count, 2);
        assert_eq!(body.nesting_depth, 4);
        assert_eq!(body.direct_child_count, 1);
        assert_eq!(body.dependency_count, 3);
        assert_eq!(body.invalidation_source_count, 1);
        assert_eq!(body.invalidation_sources[0].name, "isLoading");
        assert_eq!(result.severity, "medium");
    }

    #[test]
    fn reports_swiftui_body_metrics_when_querying_body_directly() {
        let graph = Graph {
            version: String::new(),
            nodes: vec![
                make_node(
                    "view::body",
                    "body",
                    NodeKind::Property,
                    "ContentView.swift",
                ),
                make_node(
                    "view::body::view:VStack@1:0",
                    "VStack",
                    NodeKind::View,
                    "ContentView.swift",
                ),
                make_node(
                    "view::body::view:Text@2:0",
                    "Text",
                    NodeKind::View,
                    "ContentView.swift",
                ),
            ],
            edges: vec![
                make_edge(
                    "view::body",
                    "view::body::view:VStack@1:0",
                    EdgeKind::Contains,
                ),
                make_edge(
                    "view::body::view:VStack@1:0",
                    "view::body::view:Text@2:0",
                    EdgeKind::Contains,
                ),
            ],
        };

        let result = query_complexity(&graph, "view::body").unwrap();
        let body = result
            .metrics
            .swiftui_body
            .as_ref()
            .expect("expected direct body metrics");

        assert_eq!(body.view_count, 2);
        assert_eq!(body.branch_count, 0);
        assert_eq!(body.nesting_depth, 2);
    }
}
