use std::collections::{HashMap, HashSet, VecDeque};

use serde::Serialize;

use grapha_core::graph::{EdgeKind, Graph, Node, NodeKind, Visibility};

use super::{QueryResolveError, SymbolRef, truncate_with_total};

/// Per-bucket caps for [`query_impact_with_options`].
///
/// The default leaves the result vectors unbounded; the CLI overrides this with
/// the user-supplied `--limit` so each of `depth_1`, `depth_2`, and
/// `depth_3_plus` is capped independently.
#[derive(Debug, Clone)]
pub struct ImpactQueryOptions {
    pub limit: usize,
}

impl Default for ImpactQueryOptions {
    fn default() -> Self {
        // Internal callers stay unbounded.
        Self { limit: usize::MAX }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImpactTreeNode {
    pub symbol: SymbolRef,
    pub children: Vec<ImpactTreeNode>,
}

#[derive(Debug, Serialize)]
pub struct ImpactModuleCount {
    pub module: String,
    pub count: usize,
}

#[derive(Debug, Serialize)]
pub struct ImpactSummary {
    pub direct_dependent_count: usize,
    pub direct_file_count: usize,
    pub direct_module_count: usize,
    pub top_direct_modules: Vec<ImpactModuleCount>,
    pub public_dependent_count: usize,
    pub internal_dependent_count: usize,
}

#[derive(Debug, Serialize)]
pub struct ImpactResult {
    pub source: String,
    pub summary: ImpactSummary,
    pub depth_1: Vec<SymbolRef>,
    pub total_depth_1: usize,
    pub depth_2: Vec<SymbolRef>,
    pub total_depth_2: usize,
    pub depth_3_plus: Vec<SymbolRef>,
    pub total_depth_3_plus: usize,
    pub total_affected: usize,
    #[serde(skip)]
    pub source_ref: SymbolRef,
    #[serde(skip)]
    pub tree: ImpactTreeNode,
}

fn to_symbol_ref(node: &Node) -> SymbolRef {
    SymbolRef::from_node(node)
}

fn is_structural_node(node: &Node) -> bool {
    matches!(node.kind, NodeKind::View | NodeKind::Branch)
}

fn node_sort_key(node_id: &str, node_index: &HashMap<&str, &Node>) -> (String, String, String) {
    match node_index.get(node_id).copied() {
        Some(node) => (
            node.name.clone(),
            node.file.to_string_lossy().to_string(),
            node.id.clone(),
        ),
        None => (node_id.to_string(), String::new(), node_id.to_string()),
    }
}

fn build_impact_tree<'a>(
    node_id: &'a str,
    node_index: &HashMap<&'a str, &'a Node>,
    children_by_parent: &HashMap<&'a str, Vec<&'a str>>,
) -> ImpactTreeNode {
    let node = node_index
        .get(node_id)
        .copied()
        .expect("tree nodes must exist in the node index");
    let children = children_by_parent
        .get(node_id)
        .into_iter()
        .flat_map(|children| children.iter().copied())
        .map(|child_id| build_impact_tree(child_id, node_index, children_by_parent))
        .collect();

    ImpactTreeNode {
        symbol: to_symbol_ref(node),
        children,
    }
}

fn summarize_dependents(direct_nodes: &[&Node], all_nodes: &[&Node]) -> ImpactSummary {
    let direct_files: HashSet<String> = direct_nodes
        .iter()
        .map(|node| node.file.to_string_lossy().to_string())
        .collect();
    let direct_modules: HashSet<String> = direct_nodes
        .iter()
        .map(|node| node.module.as_deref().unwrap_or("<unknown>").to_string())
        .collect();

    let mut module_counts: HashMap<String, usize> = HashMap::new();
    for node in direct_nodes {
        let module = node.module.as_deref().unwrap_or("<unknown>").to_string();
        *module_counts.entry(module).or_default() += 1;
    }
    let mut top_direct_modules: Vec<ImpactModuleCount> = module_counts
        .into_iter()
        .map(|(module, count)| ImpactModuleCount { module, count })
        .collect();
    top_direct_modules.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.module.cmp(&right.module))
    });
    top_direct_modules.truncate(5);

    let (public_dependent_count, internal_dependent_count) =
        all_nodes
            .iter()
            .fold((0usize, 0usize), |(public, internal), node| {
                if node.visibility == Visibility::Public {
                    (public + 1, internal)
                } else {
                    (public, internal + 1)
                }
            });

    ImpactSummary {
        direct_dependent_count: direct_nodes.len(),
        direct_file_count: direct_files.len(),
        direct_module_count: direct_modules.len(),
        top_direct_modules,
        public_dependent_count,
        internal_dependent_count,
    }
}

#[cfg(test)]
pub fn query_impact(
    graph: &Graph,
    symbol: &str,
    max_depth: usize,
) -> Result<ImpactResult, QueryResolveError> {
    query_impact_with_options(graph, symbol, max_depth, &ImpactQueryOptions::default())
}

pub fn query_impact_with_options(
    graph: &Graph,
    symbol: &str,
    max_depth: usize,
    options: &ImpactQueryOptions,
) -> Result<ImpactResult, QueryResolveError> {
    let node = crate::query::resolve_node(graph, symbol)?;

    let node_index: HashMap<&str, &grapha_core::graph::Node> =
        graph.nodes.iter().map(|n| (n.id.as_str(), n)).collect();

    let mut reverse_adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in &graph.edges {
        if !matches!(
            edge.kind,
            EdgeKind::Calls
                | EdgeKind::Reads
                | EdgeKind::Implements
                | EdgeKind::TypeRef
                | EdgeKind::Inherits
        ) {
            continue;
        }

        let Some(source_node) = node_index.get(edge.source.as_str()).copied() else {
            continue;
        };
        let Some(target_node) = node_index.get(edge.target.as_str()).copied() else {
            continue;
        };
        if is_structural_node(source_node) || is_structural_node(target_node) {
            continue;
        }

        reverse_adj
            .entry(&edge.target)
            .or_default()
            .push(&edge.source);
    }
    for dependents in reverse_adj.values_mut() {
        dependents.sort_unstable_by_key(|node_id| node_sort_key(node_id, &node_index));
    }

    let mut visited: HashSet<&str> = HashSet::new();
    visited.insert(&node.id);

    let mut depth_1 = Vec::new();
    let mut depth_2 = Vec::new();
    let mut depth_3_plus = Vec::new();
    let mut direct_nodes = Vec::new();
    let mut all_nodes = Vec::new();
    let mut parents: HashMap<&str, &str> = HashMap::new();

    let mut queue: VecDeque<(&str, usize)> = VecDeque::new();
    queue.push_back((&node.id, 0));

    // If the queried node is a type, also seed BFS with its direct members
    // at depth 0 so that callers of members appear as depth_1 dependents.
    let is_type_node = matches!(
        node.kind,
        NodeKind::Struct | NodeKind::Class | NodeKind::Enum | NodeKind::Protocol | NodeKind::Trait
    );
    if is_type_node {
        for edge in &graph.edges {
            if edge.kind == EdgeKind::Contains
                && edge.source == node.id
                && visited.insert(edge.target.as_str())
            {
                queue.push_back((edge.target.as_str(), 0));
            }
        }
    }

    while let Some((current, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }
        if let Some(dependents) = reverse_adj.get(current) {
            for dep_id in dependents {
                if visited.contains(dep_id) {
                    continue;
                }
                visited.insert(dep_id);
                if let Some(dep_node) = node_index.get(dep_id).copied() {
                    parents.insert(dep_id, current);
                    let sym_ref = to_symbol_ref(dep_node);
                    match depth + 1 {
                        1 => {
                            depth_1.push(sym_ref);
                            direct_nodes.push(dep_node);
                        }
                        2 => depth_2.push(sym_ref),
                        _ => depth_3_plus.push(sym_ref),
                    }
                    all_nodes.push(dep_node);
                    queue.push_back((dep_id, depth + 1));
                }
            }
        }
    }

    let total = depth_1.len() + depth_2.len() + depth_3_plus.len();
    let summary = summarize_dependents(&direct_nodes, &all_nodes);
    let mut children_by_parent: HashMap<&str, Vec<&str>> = HashMap::new();
    for (child, parent) in parents {
        children_by_parent.entry(parent).or_default().push(child);
    }
    for children in children_by_parent.values_mut() {
        children.sort_unstable_by_key(|node_id| node_sort_key(node_id, &node_index));
    }
    let source_ref = to_symbol_ref(node);
    let mut tree = build_impact_tree(&node.id, &node_index, &children_by_parent);

    let limit = options.limit;
    let total_depth_1 = truncate_with_total(&mut depth_1, limit);
    let total_depth_2 = truncate_with_total(&mut depth_2, limit);
    let total_depth_3_plus = truncate_with_total(&mut depth_3_plus, limit);

    // Prune the tree so tree-mode rendering only sees nodes that survived
    // per-bucket truncation. The source node always stays.
    let kept_ids: HashSet<&str> = depth_1
        .iter()
        .chain(depth_2.iter())
        .chain(depth_3_plus.iter())
        .map(|symbol| symbol.id.as_str())
        .collect();
    prune_impact_tree(&mut tree, &kept_ids);

    Ok(ImpactResult {
        source: node.id.clone(),
        summary,
        depth_1,
        total_depth_1,
        depth_2,
        total_depth_2,
        depth_3_plus,
        total_depth_3_plus,
        total_affected: total,
        source_ref,
        tree,
    })
}

/// Recursively drop tree children whose symbol ID is not in `kept_ids`.
///
/// The root of the tree is the queried symbol itself and is always retained;
/// only descendants are pruned. This keeps `result.tree` consistent with the
/// truncated `depth_1`/`depth_2`/`depth_3_plus` vectors so tree-mode and
/// brief-mode rendering agree on which dependents survived `--limit`.
fn prune_impact_tree(node: &mut ImpactTreeNode, kept_ids: &HashSet<&str>) {
    node.children
        .retain(|child| kept_ids.contains(child.symbol.id.as_str()));
    for child in &mut node.children {
        prune_impact_tree(child, kept_ids);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapha_core::graph::*;
    use std::collections::HashMap as StdHashMap;

    fn make_chain_graph() -> Graph {
        let mk = |id: &str| Node {
            id: id.into(),
            kind: NodeKind::Function,
            name: id.into(),
            file: "test.rs".into(),
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
        Graph {
            version: "0.1.0".to_string(),
            nodes: vec![mk("a"), mk("b"), mk("c"), mk("d")],
            edges: vec![
                Edge {
                    source: "a".into(),
                    target: "b".into(),
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
                    source: "b".into(),
                    target: "c".into(),
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
                    source: "c".into(),
                    target: "d".into(),
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
        }
    }

    #[test]
    fn impact_finds_transitive_dependents() {
        let graph = make_chain_graph();
        let result = query_impact(&graph, "d", 5).unwrap();
        assert_eq!(result.depth_1.len(), 1);
        assert_eq!(result.depth_1[0].name, "c");
        assert_eq!(result.depth_2.len(), 1);
        assert_eq!(result.depth_2[0].name, "b");
        assert_eq!(result.depth_3_plus.len(), 1);
        assert_eq!(result.depth_3_plus[0].name, "a");
        assert_eq!(result.total_affected, 3);
        assert_eq!(result.summary.direct_dependent_count, 1);
        assert_eq!(result.summary.public_dependent_count, 3);
    }

    #[test]
    fn impact_respects_max_depth() {
        let graph = make_chain_graph();
        let result = query_impact(&graph, "d", 1).unwrap();
        assert_eq!(result.depth_1.len(), 1);
        assert_eq!(result.total_affected, 1);
    }

    #[test]
    fn impact_traverses_read_dependencies() {
        let mk = |id: &str, name: &str| Node {
            id: id.into(),
            kind: NodeKind::Property,
            name: name.into(),
            file: "view.swift".into(),
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
                mk("view.swift::RoomPage::roomMode", "roomMode"),
                mk("view.swift::RoomPage::canShowGameRoom", "canShowGameRoom"),
                mk("view.swift::RoomPage::body", "body"),
            ],
            edges: vec![
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
            ],
        };

        let result = query_impact(&graph, "roomMode", 5).unwrap();
        assert_eq!(result.depth_1.len(), 1);
        assert_eq!(result.depth_1[0].name, "canShowGameRoom");
        assert_eq!(result.depth_2.len(), 1);
        assert_eq!(result.depth_2[0].name, "body");
        assert_eq!(result.summary.direct_dependent_count, 1);
    }

    #[test]
    fn impact_returns_none_for_unknown() {
        let graph = make_chain_graph();
        assert!(matches!(
            query_impact(&graph, "z", 5),
            Err(QueryResolveError::NotFound { .. })
        ));
    }

    #[test]
    fn impact_tree_reflects_bfs_parentage() {
        let graph = make_chain_graph();
        let result = query_impact(&graph, "d", 5).unwrap();

        assert_eq!(result.source_ref.name, "d");
        assert_eq!(result.tree.symbol.name, "d");
        assert_eq!(result.tree.children.len(), 1);
        assert_eq!(result.tree.children[0].symbol.name, "c");
        assert_eq!(result.tree.children[0].children[0].symbol.name, "b");
        assert_eq!(
            result.tree.children[0].children[0].children[0].symbol.name,
            "a"
        );
    }

    #[test]
    fn impact_tree_is_deterministic_when_multiple_dependents_exist() {
        let mk = |id: &str| Node {
            id: id.into(),
            kind: NodeKind::Function,
            name: id.into(),
            file: "test.rs".into(),
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
            nodes: vec![mk("alpha"), mk("beta"), mk("source")],
            edges: vec![
                Edge {
                    source: "beta".into(),
                    target: "source".into(),
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
                    source: "alpha".into(),
                    target: "source".into(),
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

        let result = query_impact(&graph, "source", 5).unwrap();
        let child_names: Vec<_> = result
            .tree
            .children
            .iter()
            .map(|child| child.symbol.name.as_str())
            .collect();
        assert_eq!(child_names, vec!["alpha", "beta"]);
    }

    #[test]
    fn impact_ignores_unresolved_dependents_without_panicking_during_sort() {
        let mk = |id: &str| Node {
            id: id.into(),
            kind: NodeKind::Function,
            name: id.into(),
            file: "test.rs".into(),
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
            nodes: vec![mk("alpha"), mk("source")],
            edges: vec![
                Edge {
                    source: "ghost".into(),
                    target: "source".into(),
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
                    source: "alpha".into(),
                    target: "source".into(),
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

        let result = query_impact(&graph, "source", 5).unwrap();
        assert_eq!(result.total_affected, 1);
        assert_eq!(result.depth_1[0].name, "alpha");
    }

    #[test]
    fn impact_type_query_traverses_through_members() {
        let mk = |id: &str, name: &str, kind: NodeKind| Node {
            id: id.into(),
            kind,
            name: name.into(),
            file: "test.swift".into(),
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
                mk("Mutex", "Mutex", NodeKind::Struct),
                mk("Mutex::init", "init", NodeKind::Function),
                mk("Mutex::withLock", "withLock", NodeKind::Function),
                mk("useMutex", "useMutex", NodeKind::Function),
                mk("app", "app", NodeKind::Function),
            ],
            edges: vec![
                Edge {
                    source: "Mutex".into(),
                    target: "Mutex::init".into(),
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
                    source: "Mutex".into(),
                    target: "Mutex::withLock".into(),
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
                    source: "useMutex".into(),
                    target: "Mutex::init".into(),
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
                    source: "app".into(),
                    target: "useMutex".into(),
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

        let result = query_impact(&graph, "Mutex", 3).unwrap();
        // useMutex calls Mutex::init, so it should be at depth 1
        assert_eq!(result.depth_1.len(), 1);
        assert_eq!(result.depth_1[0].name, "useMutex");
        // app calls useMutex, so it should be at depth 2
        assert_eq!(result.depth_2.len(), 1);
        assert_eq!(result.depth_2[0].name, "app");
        assert_eq!(result.total_affected, 2);
        assert_eq!(result.summary.direct_file_count, 1);
    }

    #[test]
    fn impact_ignores_swiftui_structural_nodes() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                Node {
                    id: "source".into(),
                    kind: NodeKind::Function,
                    name: "source".into(),
                    file: "test.swift".into(),
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
                },
                Node {
                    id: "body::view:Row@10:12".into(),
                    kind: NodeKind::View,
                    name: "Row".into(),
                    file: "test.swift".into(),
                    span: Span {
                        start: [10, 12],
                        end: [10, 28],
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
            edges: vec![Edge {
                source: "body::view:Row@10:12".into(),
                target: "source".into(),
                kind: EdgeKind::TypeRef,
                confidence: 0.9,
                direction: None,
                operation: None,
                condition: None,
                async_boundary: None,
                provenance: Vec::new(),
                repo: None,
            }],
        };

        let result = query_impact(&graph, "source", 5).unwrap();
        assert_eq!(result.total_affected, 0);
        assert!(result.depth_1.is_empty());
        assert!(result.depth_2.is_empty());
        assert!(result.depth_3_plus.is_empty());
    }

    #[test]
    fn impact_summary_counts_direct_files_modules_and_visibility() {
        let mk = |id: &str,
                  name: &str,
                  file: &str,
                  module: Option<&str>,
                  visibility: Visibility|
         -> Node {
            Node {
                id: id.into(),
                kind: NodeKind::Function,
                name: name.into(),
                file: file.into(),
                span: Span {
                    start: [0, 0],
                    end: [1, 0],
                },
                visibility,
                metadata: StdHashMap::new(),
                role: None,
                signature: None,
                doc_comment: None,
                module: module.map(str::to_string),
                snippet: None,
                repo: None,
            }
        };

        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                mk(
                    "source",
                    "source",
                    "a.swift",
                    Some("App"),
                    Visibility::Public,
                ),
                mk(
                    "direct_a",
                    "directA",
                    "b.swift",
                    Some("FeatureA"),
                    Visibility::Public,
                ),
                mk(
                    "direct_b",
                    "directB",
                    "c.swift",
                    Some("FeatureA"),
                    Visibility::Private,
                ),
                mk(
                    "indirect",
                    "indirect",
                    "d.swift",
                    Some("FeatureB"),
                    Visibility::Private,
                ),
            ],
            edges: vec![
                Edge {
                    source: "direct_a".into(),
                    target: "source".into(),
                    kind: EdgeKind::Calls,
                    confidence: 1.0,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: Vec::new(),
                    repo: None,
                },
                Edge {
                    source: "direct_b".into(),
                    target: "source".into(),
                    kind: EdgeKind::Calls,
                    confidence: 1.0,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: Vec::new(),
                    repo: None,
                },
                Edge {
                    source: "indirect".into(),
                    target: "direct_a".into(),
                    kind: EdgeKind::Calls,
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

        let result = query_impact(&graph, "source", 3).unwrap();
        assert_eq!(result.summary.direct_dependent_count, 2);
        assert_eq!(result.summary.direct_file_count, 2);
        assert_eq!(result.summary.direct_module_count, 1);
        assert_eq!(result.summary.top_direct_modules[0].module, "FeatureA");
        assert_eq!(result.summary.top_direct_modules[0].count, 2);
        assert_eq!(result.summary.public_dependent_count, 1);
        assert_eq!(result.summary.internal_dependent_count, 2);
    }

    #[test]
    fn impact_truncates_each_depth_bucket() {
        let mk = |id: &str| Node {
            id: id.into(),
            kind: NodeKind::Function,
            name: id.into(),
            file: "test.rs".into(),
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

        // Three direct callers at depth_1 of `target`.
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![mk("target"), mk("d1_a"), mk("d1_b"), mk("d1_c")],
            edges: vec![
                calls("d1_a", "target"),
                calls("d1_b", "target"),
                calls("d1_c", "target"),
            ],
        };

        // Baseline: unbounded so we can assert total_affected stays put.
        let unbounded =
            query_impact_with_options(&graph, "target", 5, &ImpactQueryOptions::default()).unwrap();
        assert_eq!(unbounded.depth_1.len(), 3);
        assert_eq!(unbounded.total_depth_1, 3);
        assert_eq!(unbounded.total_affected, 3);

        let truncated =
            query_impact_with_options(&graph, "target", 5, &ImpactQueryOptions { limit: 1 })
                .unwrap();
        assert_eq!(truncated.depth_1.len(), 1, "depth_1 should be truncated");
        assert_eq!(truncated.total_depth_1, 3);
        // total_affected is the union pre-truncation count and must not change
        // when --limit shrinks the visible vectors.
        assert_eq!(
            truncated.total_affected, unbounded.total_affected,
            "total_affected must reflect the pre-truncation union"
        );
    }

    #[test]
    fn impact_tree_pruned_to_kept_depth_buckets() {
        // 5 direct callers of `target`. With `--limit=2` the per-bucket vectors
        // are capped at 2; the tree must be pruned to mirror the surviving set.
        let mk = |id: &str| Node {
            id: id.into(),
            kind: NodeKind::Function,
            name: id.into(),
            file: "test.rs".into(),
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

        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                mk("target"),
                mk("d1_a"),
                mk("d1_b"),
                mk("d1_c"),
                mk("d1_d"),
                mk("d1_e"),
            ],
            edges: vec![
                calls("d1_a", "target"),
                calls("d1_b", "target"),
                calls("d1_c", "target"),
                calls("d1_d", "target"),
                calls("d1_e", "target"),
            ],
        };

        // Unbounded baseline: all 5 children present in tree and depth_1.
        let unbounded =
            query_impact_with_options(&graph, "target", 3, &ImpactQueryOptions::default()).unwrap();
        assert_eq!(unbounded.depth_1.len(), 5);
        assert_eq!(
            unbounded.tree.children.len(),
            5,
            "tree should preserve all 5 direct children when unbounded"
        );

        let truncated =
            query_impact_with_options(&graph, "target", 3, &ImpactQueryOptions { limit: 2 })
                .unwrap();
        assert_eq!(
            truncated.depth_1.len(),
            2,
            "depth_1 should be capped at limit=2"
        );
        assert_eq!(
            truncated.tree.children.len(),
            2,
            "tree must be pruned to match per-bucket truncation"
        );
        // Source/root always stays.
        assert_eq!(truncated.tree.symbol.id, "target");

        // Every kept child in the tree must also be in depth_1.
        let kept_depth_1_ids: HashSet<&str> = truncated
            .depth_1
            .iter()
            .map(|symbol| symbol.id.as_str())
            .collect();
        for child in &truncated.tree.children {
            assert!(
                kept_depth_1_ids.contains(child.symbol.id.as_str()),
                "tree child {} not present in depth_1 after truncation",
                child.symbol.id,
            );
        }
        // Pre-truncation total preserved.
        assert_eq!(truncated.total_depth_1, 5);
    }
}
