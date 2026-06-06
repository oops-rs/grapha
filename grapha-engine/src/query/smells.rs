use std::collections::{HashMap, HashSet};

use serde::Serialize;

use grapha_core::graph::{EdgeKind, Graph, Node, NodeKind};

use super::{
    SymbolRef, complexity, file_matches_query_path, is_swiftui_invalidation_source,
    normalize_symbol_name,
};

#[derive(Debug, Serialize)]
pub struct SmellsResult {
    pub smells: Vec<Smell>,
    pub total: usize,
    pub by_severity: HashMap<String, usize>,
}

#[derive(Debug, Serialize)]
pub struct Smell {
    pub kind: String,
    pub severity: String,
    pub symbol: SymbolRef,
    pub message: String,
    pub metric_value: usize,
    pub threshold: usize,
}

fn file_matches_query(node: &Node, file_query: &str) -> bool {
    file_matches_query_path(&node.file, file_query)
}

fn module_matches_query(node: &Node, module_query_lower: &str) -> bool {
    node.module
        .as_ref()
        .is_some_and(|module| module.to_lowercase() == module_query_lower)
}

fn collect_contains_descendants(graph: &Graph, root_ids: &HashSet<String>) -> HashSet<String> {
    let contains_adj: HashMap<&str, Vec<&str>> = graph
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Contains)
        .fold(HashMap::new(), |mut acc, edge| {
            acc.entry(edge.source.as_str())
                .or_default()
                .push(edge.target.as_str());
            acc
        });

    let mut scoped_ids = root_ids.clone();
    let mut stack: Vec<String> = root_ids.iter().cloned().collect();
    while let Some(node_id) = stack.pop() {
        if let Some(children) = contains_adj.get(node_id.as_str()) {
            for child in children {
                if scoped_ids.insert((*child).to_string()) {
                    stack.push((*child).to_string());
                }
            }
        }
    }
    scoped_ids
}

fn build_scoped_graph(graph: &Graph, scoped_node_ids: &HashSet<String>) -> Graph {
    let nodes = graph
        .nodes
        .iter()
        .filter(|node| scoped_node_ids.contains(&node.id))
        .cloned()
        .collect();
    let edges = graph
        .edges
        .iter()
        .filter(|edge| {
            scoped_node_ids.contains(&edge.source) || scoped_node_ids.contains(&edge.target)
        })
        .cloned()
        .collect();

    Graph {
        version: graph.version.clone(),
        nodes,
        edges,
    }
}

fn recompute_totals(result: &mut SmellsResult) {
    result.total = result.smells.len();
    result.by_severity.clear();
    for smell in &result.smells {
        *result
            .by_severity
            .entry(smell.severity.clone())
            .or_default() += 1;
    }
}

fn detect_smells_for_root_ids(graph: &Graph, root_ids: HashSet<String>) -> SmellsResult {
    let scoped_node_ids = collect_contains_descendants(graph, &root_ids);
    let scoped_graph = build_scoped_graph(graph, &scoped_node_ids);
    let mut result = detect_smells(&scoped_graph);
    result
        .smells
        .retain(|smell| root_ids.contains(smell.symbol.id.as_str()));
    recompute_totals(&mut result);
    result
}

fn body_scope_expansion(graph: &Graph, symbol_id: &str) -> (HashSet<String>, Option<SymbolRef>) {
    let node_index: HashMap<&str, &Node> = graph.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let Some(node) = node_index.get(symbol_id).copied() else {
        return (HashSet::new(), None);
    };

    let mut root_ids = HashSet::from([symbol_id.to_string()]);
    let mut lift_target = None;

    if is_type_node(node.kind) {
        let body_ids: Vec<&str> = graph
            .edges
            .iter()
            .filter_map(|edge| match edge.kind {
                EdgeKind::Contains if edge.source == symbol_id => {
                    node_index.get(edge.target.as_str()).copied()
                }
                EdgeKind::Implements if edge.target == symbol_id => {
                    node_index.get(edge.source.as_str()).copied()
                }
                EdgeKind::TypeRef if edge.source == symbol_id => {
                    node_index.get(edge.target.as_str()).copied()
                }
                _ => None,
            })
            .filter(|candidate| normalize_symbol_name(&candidate.name) == "body")
            .map(|candidate| candidate.id.as_str())
            .collect();

        if !body_ids.is_empty() {
            root_ids.extend(body_ids.into_iter().map(str::to_string));
            lift_target = Some(to_symbol_ref(node));
        }
    }

    (root_ids, lift_target)
}

fn lift_associated_smells(
    result: &mut SmellsResult,
    root_ids: &HashSet<String>,
    lift_target: &SymbolRef,
) {
    for smell in &mut result.smells {
        if root_ids.contains(smell.symbol.id.as_str()) && smell.symbol.id != lift_target.id {
            smell.symbol = lift_target.clone();
            if let Some(rest) = smell.message.strip_prefix("getter:body ") {
                smell.message = format!("{} body {rest}", lift_target.name);
            } else if let Some(rest) = smell.message.strip_prefix("body ") {
                smell.message = format!("{} body {rest}", lift_target.name);
            }
        }
    }
}

fn to_symbol_ref(node: &Node) -> SymbolRef {
    SymbolRef::from_node(node)
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

fn count_init_params(name: &str) -> usize {
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

struct SmellConfig {
    god_type_property_threshold: usize,
    god_type_dependency_threshold: usize,
    wide_invalidation_threshold: usize,
    massive_init_threshold: usize,
    deep_nesting_threshold: usize,
    high_fan_out_threshold: usize,
    high_fan_in_threshold: usize,
    many_extensions_threshold: usize,
    complex_swiftui_body_threshold: usize,
    critical_swiftui_body_threshold: usize,
}

impl Default for SmellConfig {
    fn default() -> Self {
        Self {
            god_type_property_threshold: 15,
            god_type_dependency_threshold: 10,
            wide_invalidation_threshold: 5,
            massive_init_threshold: 8,
            deep_nesting_threshold: 5,
            high_fan_out_threshold: 15,
            high_fan_in_threshold: 15,
            many_extensions_threshold: 5,
            complex_swiftui_body_threshold: complexity::SWIFTUI_BODY_COMPLEXITY_SMELL_THRESHOLD,
            critical_swiftui_body_threshold: 5,
        }
    }
}

pub fn detect_smells(graph: &Graph) -> SmellsResult {
    let config = SmellConfig::default();
    let node_index: HashMap<&str, &Node> = graph.nodes.iter().map(|n| (n.id.as_str(), n)).collect();

    // Build adjacency maps
    let mut implements_targets: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut contains_adj: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut type_ref_sources: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut callee_count: HashMap<&str, usize> = HashMap::new();
    let mut caller_count: HashMap<&str, usize> = HashMap::new();
    let mut reads_adj: HashMap<&str, Vec<&str>> = HashMap::new();

    for edge in &graph.edges {
        match edge.kind {
            EdgeKind::Implements => {
                implements_targets
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
                type_ref_sources
                    .entry(edge.target.as_str())
                    .or_default()
                    .push(edge.source.as_str());
            }
            EdgeKind::Calls => {
                *callee_count.entry(edge.source.as_str()).or_default() += 1;
                *caller_count.entry(edge.target.as_str()).or_default() += 1;
            }
            EdgeKind::Reads => {
                reads_adj
                    .entry(edge.source.as_str())
                    .or_default()
                    .push(edge.target.as_str());
            }
            _ => {}
        }
    }

    let mut smells: Vec<Smell> = Vec::new();

    for node in &graph.nodes {
        if !is_type_node(node.kind) {
            continue;
        }

        let implementors = implements_targets
            .get(node.id.as_str())
            .cloned()
            .unwrap_or_default();

        // God type: too many properties
        let property_count = implementors
            .iter()
            .filter(|id| {
                node_index
                    .get(*id)
                    .is_some_and(|n| matches!(n.kind, NodeKind::Property | NodeKind::Field))
            })
            .count();

        if property_count > config.god_type_property_threshold {
            smells.push(Smell {
                kind: "god_type".to_string(),
                severity: if property_count > 25 {
                    "critical"
                } else {
                    "warning"
                }
                .to_string(),
                symbol: to_symbol_ref(node),
                message: format!(
                    "{} has {property_count} properties (threshold: {})",
                    node.name, config.god_type_property_threshold
                ),
                metric_value: property_count,
                threshold: config.god_type_property_threshold,
            });
        }

        // God type: too many dependencies
        let mut dep_set: HashSet<&str> = HashSet::new();
        for impl_id in &implementors {
            if let Some(reads) = reads_adj.get(*impl_id) {
                for r in reads {
                    dep_set.insert(r);
                }
            }
        }
        if dep_set.len() > config.god_type_dependency_threshold {
            smells.push(Smell {
                kind: "excessive_dependencies".to_string(),
                severity: "warning".to_string(),
                symbol: to_symbol_ref(node),
                message: format!(
                    "{} reads {} unique dependencies (threshold: {})",
                    node.name,
                    dep_set.len(),
                    config.god_type_dependency_threshold
                ),
                metric_value: dep_set.len(),
                threshold: config.god_type_dependency_threshold,
            });
        }

        // Wide invalidation surface
        let invalidation_count = implementors
            .iter()
            .filter_map(|id| node_index.get(*id).copied())
            .filter(|n| is_swiftui_invalidation_source(n))
            .count();

        if invalidation_count > config.wide_invalidation_threshold {
            smells.push(Smell {
                kind: "wide_invalidation".to_string(),
                severity: if invalidation_count > 8 {
                    "critical"
                } else {
                    "warning"
                }
                .to_string(),
                symbol: to_symbol_ref(node),
                message: format!(
                    "{} has {invalidation_count} invalidation sources (threshold: {})",
                    node.name, config.wide_invalidation_threshold
                ),
                metric_value: invalidation_count,
                threshold: config.wide_invalidation_threshold,
            });
        }

        // Massive init
        let max_init_params = implementors
            .iter()
            .filter_map(|id| node_index.get(*id).copied())
            .filter(|n| n.kind == NodeKind::Function && n.name.starts_with("init("))
            .map(|n| count_init_params(&n.name))
            .max()
            .unwrap_or(0);

        if max_init_params > config.massive_init_threshold {
            smells.push(Smell {
                kind: "massive_init".to_string(),
                severity: if max_init_params > 12 {
                    "critical"
                } else {
                    "warning"
                }
                .to_string(),
                symbol: to_symbol_ref(node),
                message: format!(
                    "{} init has {max_init_params} parameters (threshold: {})",
                    node.name, config.massive_init_threshold
                ),
                metric_value: max_init_params,
                threshold: config.massive_init_threshold,
            });
        }

        // Deep nesting (contains tree depth)
        let depth = measure_contains_depth(node.id.as_str(), &contains_adj, &mut HashSet::new());
        if depth > config.deep_nesting_threshold {
            smells.push(Smell {
                kind: "deep_nesting".to_string(),
                severity: "warning".to_string(),
                symbol: to_symbol_ref(node),
                message: format!(
                    "{} has contains-tree depth {depth} (threshold: {})",
                    node.name, config.deep_nesting_threshold
                ),
                metric_value: depth,
                threshold: config.deep_nesting_threshold,
            });
        }

        // Many extensions
        let ext_count = type_ref_sources
            .get(node.id.as_str())
            .map(|sources| {
                sources
                    .iter()
                    .filter(|id| {
                        node_index
                            .get(*id)
                            .is_some_and(|n| n.kind == NodeKind::Extension)
                    })
                    .count()
            })
            .unwrap_or(0);

        if ext_count > config.many_extensions_threshold {
            smells.push(Smell {
                kind: "many_extensions".to_string(),
                severity: "warning".to_string(),
                symbol: to_symbol_ref(node),
                message: format!(
                    "{} has {ext_count} extensions (threshold: {})",
                    node.name, config.many_extensions_threshold
                ),
                metric_value: ext_count,
                threshold: config.many_extensions_threshold,
            });
        }
    }

    for node in &graph.nodes {
        if !matches!(node.kind, NodeKind::Property | NodeKind::Function)
            || normalize_symbol_name(&node.name) != "body"
        {
            continue;
        }

        let Some(body_metrics) = complexity::swiftui_body_metrics_for_node(graph, node) else {
            continue;
        };
        let body_score = complexity::swiftui_body_complexity_score(&body_metrics);
        if body_score < config.complex_swiftui_body_threshold {
            continue;
        }

        let severity = if body_score >= config.critical_swiftui_body_threshold {
            "critical"
        } else {
            "warning"
        };

        smells.push(Smell {
            kind: "complex_swiftui_body".to_string(),
            severity: severity.to_string(),
            symbol: to_symbol_ref(node),
            message: format!(
                "{} has {} view nodes, {} branches, depth {}, and {} dependencies (score {} threshold: {})",
                normalize_symbol_name(&node.name),
                body_metrics.view_count,
                body_metrics.branch_count,
                body_metrics.nesting_depth,
                body_metrics.dependency_count,
                body_score,
                config.complex_swiftui_body_threshold,
            ),
            metric_value: body_score,
            threshold: config.complex_swiftui_body_threshold,
        });
    }

    // Function-level smells: fan-out and fan-in
    for node in &graph.nodes {
        if node.kind != NodeKind::Function {
            continue;
        }

        let fan_out = callee_count.get(node.id.as_str()).copied().unwrap_or(0);
        if fan_out > config.high_fan_out_threshold {
            smells.push(Smell {
                kind: "high_fan_out".to_string(),
                severity: "warning".to_string(),
                symbol: to_symbol_ref(node),
                message: format!(
                    "{} calls {fan_out} other symbols (threshold: {})",
                    node.name, config.high_fan_out_threshold
                ),
                metric_value: fan_out,
                threshold: config.high_fan_out_threshold,
            });
        }

        let fan_in = caller_count.get(node.id.as_str()).copied().unwrap_or(0);
        if fan_in > config.high_fan_in_threshold {
            smells.push(Smell {
                kind: "high_fan_in".to_string(),
                severity: "warning".to_string(),
                symbol: to_symbol_ref(node),
                message: format!(
                    "{} is called by {fan_in} symbols (threshold: {})",
                    node.name, config.high_fan_in_threshold
                ),
                metric_value: fan_in,
                threshold: config.high_fan_in_threshold,
            });
        }
    }

    // Sort: critical first, then by metric_value desc
    smells.sort_by(|a, b| {
        severity_rank(&a.severity)
            .cmp(&severity_rank(&b.severity))
            .then_with(|| b.metric_value.cmp(&a.metric_value))
    });

    let mut by_severity: HashMap<String, usize> = HashMap::new();
    for smell in &smells {
        *by_severity.entry(smell.severity.clone()).or_default() += 1;
    }

    let total = smells.len();
    SmellsResult {
        smells,
        total,
        by_severity,
    }
}

pub fn detect_smells_for_file(graph: &Graph, file_query: &str) -> SmellsResult {
    let root_ids: HashSet<String> = graph
        .nodes
        .iter()
        .filter(|node| file_matches_query(node, file_query))
        .map(|node| node.id.clone())
        .collect();
    detect_smells_for_root_ids(graph, root_ids)
}

pub fn detect_smells_for_symbol(graph: &Graph, symbol_id: &str) -> SmellsResult {
    let (root_ids, lift_target) = body_scope_expansion(graph, symbol_id);
    let mut result = detect_smells_for_root_ids(graph, root_ids.clone());
    if let Some(lift_target) = lift_target.as_ref() {
        lift_associated_smells(&mut result, &root_ids, lift_target);
    }
    result
}

pub fn detect_smells_for_module(graph: &Graph, module_name: &str) -> SmellsResult {
    let module_lower = module_name.to_lowercase();
    let root_ids: HashSet<String> = graph
        .nodes
        .iter()
        .filter(|node| module_matches_query(node, &module_lower))
        .map(|node| node.id.clone())
        .collect();
    detect_smells_for_root_ids(graph, root_ids)
}

fn severity_rank(severity: &str) -> usize {
    match severity {
        "critical" => 0,
        "warning" => 1,
        _ => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapha_core::graph::{Edge, Node, Span, Visibility};
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
    fn detects_god_type() {
        let mut nodes = vec![make_node("s:Big", "BigStruct", NodeKind::Struct, "B.swift")];
        let mut edges = Vec::new();

        for i in 0..20 {
            let id = format!("s:prop{i}");
            nodes.push(make_node(
                &id,
                &format!("prop{i}"),
                NodeKind::Property,
                "B.swift",
            ));
            edges.push(make_edge(&id, "s:Big", EdgeKind::Implements));
        }

        let graph = Graph {
            version: String::new(),
            nodes,
            edges,
        };

        let result = detect_smells(&graph);
        assert!(result.smells.iter().any(|s| s.kind == "god_type"));
    }

    #[test]
    fn no_smells_for_small_type() {
        let graph = Graph {
            version: String::new(),
            nodes: vec![
                make_node("s:T", "SmallType", NodeKind::Struct, "T.swift"),
                make_node("s:p1", "name", NodeKind::Property, "T.swift"),
            ],
            edges: vec![make_edge("s:p1", "s:T", EdgeKind::Implements)],
        };

        let result = detect_smells(&graph);
        assert_eq!(result.total, 0);
    }

    #[test]
    fn file_scope_matches_repo_relative_query_when_graph_stores_basename() {
        let mut nodes = vec![make_node(
            "view",
            "RoomPageCenterContentView",
            NodeKind::Struct,
            "RoomPage+Layout.swift",
        )];
        let mut edges = vec![make_edge("view::body", "view", EdgeKind::Implements)];
        nodes.push(make_node(
            "view::body",
            "getter:body",
            NodeKind::Function,
            "RoomPage+Layout.swift",
        ));
        for i in 0..16 {
            let id = format!("dep::{i}");
            nodes.push(make_node(
                &id,
                &format!("dep{i}"),
                NodeKind::Function,
                "Deps.swift",
            ));
            edges.push(make_edge("view::body", &id, EdgeKind::Calls));
        }

        let graph = Graph {
            version: String::new(),
            nodes,
            edges,
        };

        let result = detect_smells_for_file(
            &graph,
            "Modules/Room/Sources/Room/View/RoomPage+Layout.swift",
        );
        assert_eq!(result.total, 1);
        assert_eq!(result.smells[0].kind, "high_fan_out");
    }

    #[test]
    fn module_scope_runs_on_matching_module_only() {
        let mut nodes = vec![
            make_node("room::view", "RoomPage", NodeKind::Struct, "RoomPage.swift"),
            make_node(
                "share::view",
                "SharePage",
                NodeKind::Struct,
                "SharePage.swift",
            ),
        ];
        nodes[0].module = Some("Room".to_string());
        nodes[1].module = Some("Share".to_string());

        let mut edges = vec![
            make_edge("room::body", "room::view", EdgeKind::Implements),
            make_edge("share::body", "share::view", EdgeKind::Implements),
        ];

        let mut room_body = make_node(
            "room::body",
            "getter:body",
            NodeKind::Function,
            "RoomPage.swift",
        );
        room_body.module = Some("Room".to_string());
        nodes.push(room_body);

        let mut share_body = make_node(
            "share::body",
            "getter:body",
            NodeKind::Function,
            "SharePage.swift",
        );
        share_body.module = Some("Share".to_string());
        nodes.push(share_body);

        for i in 0..16 {
            let id = format!("room::dep::{i}");
            let mut dep = make_node(&id, &format!("dep{i}"), NodeKind::Function, "Deps.swift");
            dep.module = Some("Room".to_string());
            nodes.push(dep);
            edges.push(make_edge("room::body", &id, EdgeKind::Calls));
        }

        for i in 0..16 {
            let id = format!("share::dep::{i}");
            let mut dep = make_node(&id, &format!("dep{i}"), NodeKind::Function, "Deps.swift");
            dep.module = Some("Share".to_string());
            nodes.push(dep);
            edges.push(make_edge("share::body", &id, EdgeKind::Calls));
        }

        let graph = Graph {
            version: String::new(),
            nodes,
            edges,
        };

        let result = detect_smells_for_module(&graph, "Room");
        assert_eq!(result.total, 1);
        assert_eq!(result.smells[0].symbol.id, "room::body");
        assert_eq!(result.smells[0].kind, "high_fan_out");
    }

    #[test]
    fn symbol_scope_on_view_type_includes_body_smell_and_lifts_it_to_type() {
        let mut nodes = vec![make_node(
            "view",
            "RoomPageCenterContentView",
            NodeKind::Struct,
            "RoomPage+Layout.swift",
        )];
        let mut edges = vec![
            make_edge("view::body", "view", EdgeKind::Implements),
            make_edge("view", "view::getter:body", EdgeKind::TypeRef),
        ];
        nodes.push(make_node(
            "view::body",
            "body",
            NodeKind::Property,
            "RoomPage+Layout.swift",
        ));
        nodes.push(make_node(
            "view::getter:body",
            "getter:body",
            NodeKind::Function,
            "RoomPage+Layout.swift",
        ));
        for i in 0..16 {
            let id = format!("dep::{i}");
            nodes.push(make_node(
                &id,
                &format!("dep{i}"),
                NodeKind::Function,
                "Deps.swift",
            ));
            edges.push(make_edge("view::getter:body", &id, EdgeKind::Calls));
        }

        let graph = Graph {
            version: String::new(),
            nodes,
            edges,
        };

        let result = detect_smells_for_symbol(&graph, "view");
        assert_eq!(result.total, 1);
        assert_eq!(result.smells[0].kind, "high_fan_out");
        assert_eq!(result.smells[0].symbol.id, "view");
        assert_eq!(result.smells[0].symbol.name, "RoomPageCenterContentView");
        assert!(result.smells[0].message.contains("body"));
    }

    #[test]
    fn detects_complex_swiftui_body_smell() {
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

        let result = detect_smells(&graph);
        let smell = result
            .smells
            .iter()
            .find(|smell| smell.kind == "complex_swiftui_body")
            .expect("expected complex SwiftUI body smell");

        assert_eq!(smell.symbol.id, "view::body");
        assert_eq!(smell.metric_value, 3);
        assert_eq!(smell.threshold, 3);
        assert!(smell.message.contains("2 branches"));
        assert!(smell.message.contains("depth 4"));
    }

    #[test]
    fn symbol_scope_on_view_type_lifts_complex_swiftui_body_to_type() {
        let graph = Graph {
            version: String::new(),
            nodes: vec![
                make_node(
                    "view",
                    "RoomPageCenterContentView",
                    NodeKind::Struct,
                    "RoomPage+Layout.swift",
                ),
                make_node(
                    "view::body",
                    "body",
                    NodeKind::Property,
                    "RoomPage+Layout.swift",
                ),
                make_node(
                    "view::body::view:VStack@1:0",
                    "VStack",
                    NodeKind::View,
                    "RoomPage+Layout.swift",
                ),
                make_node(
                    "view::body::branch:if@2:0",
                    "if isLoading",
                    NodeKind::Branch,
                    "RoomPage+Layout.swift",
                ),
                make_node(
                    "view::body::view:ProgressView@3:0",
                    "ProgressView",
                    NodeKind::View,
                    "RoomPage+Layout.swift",
                ),
                make_node(
                    "view::body::branch:else@4:0",
                    "else",
                    NodeKind::Branch,
                    "RoomPage+Layout.swift",
                ),
                make_node(
                    "view::body::view:List@5:0",
                    "List",
                    NodeKind::View,
                    "RoomPage+Layout.swift",
                ),
                make_node(
                    "view::body::view:Row@6:0",
                    "Row",
                    NodeKind::View,
                    "RoomPage+Layout.swift",
                ),
                make_node(
                    "view::title",
                    "title",
                    NodeKind::Property,
                    "RoomPage+Layout.swift",
                ),
                make_node(
                    "view::items",
                    "items",
                    NodeKind::Property,
                    "RoomPage+Layout.swift",
                ),
                make_node(
                    "view::load",
                    "load()",
                    NodeKind::Function,
                    "RoomPage+Layout.swift",
                ),
            ],
            edges: vec![
                make_edge("view::body", "view", EdgeKind::Implements),
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
            ],
        };

        let result = detect_smells_for_symbol(&graph, "view");
        let smell = result
            .smells
            .iter()
            .find(|smell| smell.kind == "complex_swiftui_body")
            .expect("expected lifted complex SwiftUI body smell");

        assert_eq!(smell.symbol.id, "view");
        assert_eq!(smell.symbol.name, "RoomPageCenterContentView");
        assert!(smell.message.starts_with("RoomPageCenterContentView body"));
    }

    #[test]
    fn symbol_scope_on_view_type_lifts_complex_body_found_via_contains_edge() {
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
                make_edge("view::body", "view::title", EdgeKind::Reads),
                make_edge("view::body", "view::items", EdgeKind::Reads),
                make_edge("view::body", "view::load", EdgeKind::Calls),
            ],
        };

        let result = detect_smells_for_symbol(&graph, "view");
        let smell = result
            .smells
            .iter()
            .find(|smell| smell.kind == "complex_swiftui_body")
            .expect("expected lifted body smell via contains edge");

        assert_eq!(smell.symbol.id, "view");
        assert!(smell.message.starts_with("ContentView body"));
    }
}
