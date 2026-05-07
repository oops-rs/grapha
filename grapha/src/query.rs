pub mod arch;
pub mod complexity;
pub mod context;
pub mod dataflow;
pub mod entries;
pub mod file_symbols;
pub(crate) mod flow;
pub mod impact;
pub(crate) mod l10n;
pub mod localize;
pub mod map;
pub mod module_summary;
pub mod origin;
pub mod reverse;
pub mod smells;
pub mod trace;
pub mod usages;

use serde::Serialize;
use thiserror::Error;

use grapha_core::graph::{Graph, Node, NodeKind, NodeRole, Visibility};

use crate::annotations::{AnnotationIndex, SymbolAnnotationView};
use crate::symbol_locator::{SymbolLocatorIndex, locator_matches_suffix};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QueryCandidate {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locator: Option<String>,
    pub name: String,
    pub kind: NodeKind,
    pub file: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Error)]
#[serde(tag = "error", rename_all = "snake_case")]
pub enum QueryResolveError {
    #[error("not found")]
    NotFound { query: String },
    #[error("ambiguous query")]
    Ambiguous {
        query: String,
        candidates: Vec<QueryCandidate>,
    },
    #[error("{hint}")]
    NotFunction { hint: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum MatchTier {
    ExactLocator,
    LocatorSuffix,
    ExactNormalizedName,
    NormalizedPrefix,
    IdSuffix,
    CaseInsensitiveNormalizedExact,
}

/// Truncate `items` to at most `limit` entries and return the original
/// (pre-truncation) length.
pub(crate) fn truncate_with_total<T>(items: &mut Vec<T>, limit: usize) -> usize {
    let total = items.len();
    items.truncate(limit);
    total
}

pub(crate) fn strip_accessor_prefix(name: &str) -> &str {
    name.strip_prefix("getter:")
        .or_else(|| name.strip_prefix("setter:"))
        .unwrap_or(name)
}

pub(crate) fn normalize_symbol_name(name: &str) -> &str {
    let without_accessor = strip_accessor_prefix(name);
    without_accessor
        .split_once('(')
        .map(|(head, _)| head)
        .unwrap_or(without_accessor)
}

pub(crate) fn file_matches_query_path(node_file: &std::path::Path, file_query: &str) -> bool {
    let normalize = |value: &str| value.replace('\\', "/");

    let file = normalize(&node_file.to_string_lossy());
    let query = normalize(file_query);

    if file == query || file.ends_with(&query) || file.contains(&query) || query.ends_with(&file) {
        return true;
    }

    let file_name = file.rsplit('/').next().unwrap_or(file.as_str());
    let query_name = query.rsplit('/').next().unwrap_or(query.as_str());
    file_name == query_name
}

pub(crate) fn file_matches_path_or_suffix(node_file: &std::path::Path, file_query: &str) -> bool {
    let normalize = |value: &str| value.replace('\\', "/");

    let file = normalize(&node_file.to_string_lossy());
    let query = normalize(file_query);

    if file == query || file.ends_with(&query) || query.ends_with(&file) {
        return true;
    }

    let file_name = file.rsplit('/').next().unwrap_or(file.as_str());
    let query_name = query.rsplit('/').next().unwrap_or(query.as_str());
    file_name == query_name
}

pub(crate) fn is_swiftui_invalidation_source(node: &Node) -> bool {
    node.metadata
        .get("swiftui.invalidation_source")
        .is_some_and(|value| value == "true")
}

fn kind_preference(kind: NodeKind) -> usize {
    match kind {
        NodeKind::Function | NodeKind::Method | NodeKind::Route => 0,
        NodeKind::Property | NodeKind::Variable => 1,
        NodeKind::Variant | NodeKind::EnumMember | NodeKind::Field | NodeKind::Parameter => 2,
        NodeKind::Class
        | NodeKind::Struct
        | NodeKind::Enum
        | NodeKind::Trait
        | NodeKind::Module
        | NodeKind::Namespace
        | NodeKind::Constant
        | NodeKind::TypeAlias
        | NodeKind::Protocol => 3,
        NodeKind::Impl | NodeKind::Extension | NodeKind::Import | NodeKind::Export => 4,
        NodeKind::File | NodeKind::Component | NodeKind::View | NodeKind::Branch => 5,
    }
}

fn split_file_symbol_query(query: &str) -> Option<(&str, &str)> {
    let (file_part, symbol_part) = query.rsplit_once("::")?;
    if file_part.is_empty() || symbol_part.is_empty() {
        return None;
    }
    let looks_like_file = file_part.contains('/')
        || file_part.contains('\\')
        || file_part.ends_with(".swift")
        || file_part.ends_with(".rs");
    if !looks_like_file {
        return None;
    }
    Some((file_part, symbol_part))
}

fn match_tier(node: &Node, locator: &str, query: &str) -> Option<MatchTier> {
    if locator == query {
        return Some(MatchTier::ExactLocator);
    }
    if query.contains("::") && locator_matches_suffix(locator, query) {
        return Some(MatchTier::LocatorSuffix);
    }

    let normalized_query = normalize_symbol_name(query);
    let normalized_name = normalize_symbol_name(&node.name);

    if normalized_name == normalized_query {
        Some(MatchTier::ExactNormalizedName)
    } else if normalized_name.starts_with(normalized_query) {
        Some(MatchTier::NormalizedPrefix)
    } else if node.id.ends_with(query) {
        Some(MatchTier::IdSuffix)
    } else if normalized_name.eq_ignore_ascii_case(normalized_query) {
        Some(MatchTier::CaseInsensitiveNormalizedExact)
    } else {
        None
    }
}

fn to_candidate(node: &Node, locators: &SymbolLocatorIndex) -> QueryCandidate {
    QueryCandidate {
        id: node.id.clone(),
        locator: Some(locators.locator_for_node(node)),
        name: node.name.clone(),
        kind: node.kind,
        file: node.file.to_string_lossy().to_string(),
    }
}

pub fn ambiguity_hint() -> &'static str {
    "Retry with Module::File.swift::Type::symbol, file.swift::symbol, or the full symbol id."
}

/// Resolve a node by query using scored matching and explicit ambiguity
/// reporting.
pub fn resolve_node<'a>(graph: &'a Graph, query: &str) -> Result<&'a Node, QueryResolveError> {
    if let Some(node) = graph.nodes.iter().find(|node| node.id == query) {
        return Ok(node);
    }

    let locators = SymbolLocatorIndex::new(graph);

    let path_matches: Vec<_> = if query.contains("::") {
        graph
            .nodes
            .iter()
            .filter_map(|node| {
                let locator = locators.locator_for_id(&node.id)?;
                let tier = match_tier(node, locator, query)?;
                Some((node, tier, kind_preference(node.kind)))
            })
            .collect()
    } else {
        Vec::new()
    };

    if !path_matches.is_empty() {
        let best_tier = path_matches
            .iter()
            .map(|(_, tier, _)| *tier)
            .min()
            .expect("path_matches is not empty");
        let best_kind = path_matches
            .iter()
            .filter(|(_, tier, _)| *tier == best_tier)
            .map(|(_, _, kind)| *kind)
            .min()
            .expect("path_matches is not empty");
        let best_matches: Vec<&Node> = path_matches
            .iter()
            .filter(|(_, tier, kind)| *tier == best_tier && *kind == best_kind)
            .map(|(node, _, _)| *node)
            .collect();
        if best_matches.len() == 1 {
            return Ok(best_matches[0]);
        }
        return Err(QueryResolveError::Ambiguous {
            query: query.to_string(),
            candidates: best_matches
                .into_iter()
                .map(|node| to_candidate(node, &locators))
                .collect(),
        });
    }

    let (candidate_nodes, match_query): (Vec<&Node>, &str) = match split_file_symbol_query(query) {
        Some((file_part, symbol_part)) => (
            graph
                .nodes
                .iter()
                .filter(|node| node.file.to_string_lossy().ends_with(file_part))
                .collect(),
            symbol_part,
        ),
        None => (graph.nodes.iter().collect(), query),
    };

    let matches: Vec<_> = candidate_nodes
        .into_iter()
        .filter_map(|node| {
            let locator = locators.locator_for_id(&node.id)?;
            let tier = match_tier(node, locator, match_query)?;
            Some((node, tier, kind_preference(node.kind)))
        })
        .collect();

    if matches.is_empty() {
        return Err(QueryResolveError::NotFound {
            query: query.to_string(),
        });
    }

    let best_tier = matches
        .iter()
        .map(|(_, tier, _)| *tier)
        .min()
        .expect("matches is not empty");
    let best_kind = matches
        .iter()
        .filter(|(_, tier, _)| *tier == best_tier)
        .map(|(_, _, kind)| *kind)
        .min()
        .expect("matches is not empty");

    let best_matches: Vec<&Node> = matches
        .iter()
        .filter(|(_, tier, kind)| *tier == best_tier && *kind == best_kind)
        .map(|(node, _, _)| *node)
        .collect();

    if best_matches.len() == 1 {
        return Ok(best_matches[0]);
    }

    Err(QueryResolveError::Ambiguous {
        query: query.to_string(),
        candidates: best_matches
            .into_iter()
            .map(|node| to_candidate(node, &locators))
            .collect(),
    })
}

#[derive(Debug, Serialize)]
pub struct ContextResult {
    pub symbol: SymbolInfo,
    pub callers: Vec<SymbolRef>,
    pub total_callers: usize,
    pub callees: Vec<SymbolRef>,
    pub total_callees: usize,
    pub reads: Vec<SymbolRef>,
    pub total_reads: usize,
    pub read_by: Vec<SymbolRef>,
    pub total_read_by: usize,
    pub invalidation_sources: Vec<SymbolRef>,
    pub total_invalidation_sources: usize,
    pub contains: Vec<SymbolRef>,
    pub total_contains: usize,
    pub contains_tree: Vec<SymbolTreeRef>,
    pub contained_by: Vec<SymbolRef>,
    pub total_contained_by: usize,
    pub implementors: Vec<SymbolRef>,
    pub total_implementors: usize,
    pub implements: Vec<SymbolRef>,
    pub total_implements: usize,
    pub type_refs: Vec<SymbolRef>,
    pub total_type_refs: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SymbolInfo {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locator: Option<String>,
    pub name: String,
    pub kind: NodeKind,
    pub file: String,
    pub span: [usize; 2],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visibility: Option<Visibility>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<NodeRole>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc_comment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotation: Option<SymbolAnnotationView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SymbolRef {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locator: Option<String>,
    pub name: String,
    pub kind: NodeKind,
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span: Option<[usize; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visibility: Option<Visibility>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<NodeRole>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc_comment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotation: Option<SymbolAnnotationView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SymbolTreeRef {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locator: Option<String>,
    pub name: String,
    pub kind: NodeKind,
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span: Option<[usize; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visibility: Option<Visibility>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<NodeRole>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc_comment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotation: Option<SymbolAnnotationView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub contains: Vec<SymbolTreeRef>,
}

fn node_span_lines(node: &Node) -> [usize; 2] {
    [node.span.start[0], node.span.end[0]]
}

impl SymbolInfo {
    pub(crate) fn from_node(node: &Node) -> Self {
        Self {
            id: node.id.clone(),
            locator: Some(crate::symbol_locator::fallback_locator(node)),
            name: node.name.clone(),
            kind: node.kind,
            file: node.file.to_string_lossy().to_string(),
            span: node_span_lines(node),
            visibility: Some(node.visibility),
            role: node.role.clone(),
            signature: node.signature.clone(),
            doc_comment: node.doc_comment.clone(),
            annotation: None,
            module: node.module.clone(),
            snippet: node.snippet.clone(),
            repo: node.repo.clone(),
        }
    }

    pub(crate) fn with_locator(mut self, locator: String) -> Self {
        self.locator = Some(locator);
        self
    }
}

impl SymbolRef {
    pub(crate) fn from_node(node: &Node) -> Self {
        Self {
            id: node.id.clone(),
            locator: Some(crate::symbol_locator::fallback_locator(node)),
            name: node.name.clone(),
            kind: node.kind,
            file: node.file.to_string_lossy().to_string(),
            span: Some(node_span_lines(node)),
            visibility: Some(node.visibility),
            role: node.role.clone(),
            signature: node.signature.clone(),
            doc_comment: node.doc_comment.clone(),
            annotation: None,
            module: node.module.clone(),
            snippet: node.snippet.clone(),
            repo: node.repo.clone(),
        }
    }

    pub(crate) fn with_locator(mut self, locator: String) -> Self {
        self.locator = Some(locator);
        self
    }
}

impl SymbolTreeRef {
    pub(crate) fn from_node(node: &Node, contains: Vec<SymbolTreeRef>) -> Self {
        Self {
            id: node.id.clone(),
            locator: Some(crate::symbol_locator::fallback_locator(node)),
            name: node.name.clone(),
            kind: node.kind,
            file: node.file.to_string_lossy().to_string(),
            span: Some(node_span_lines(node)),
            visibility: Some(node.visibility),
            role: node.role.clone(),
            signature: node.signature.clone(),
            doc_comment: node.doc_comment.clone(),
            annotation: None,
            module: node.module.clone(),
            snippet: node.snippet.clone(),
            repo: node.repo.clone(),
            contains,
        }
    }

    pub(crate) fn with_locator(mut self, locator: String) -> Self {
        self.locator = Some(locator);
        self
    }
}

impl ContextResult {
    pub(crate) fn apply_annotations(&mut self, graph: &Graph, annotations: &AnnotationIndex) {
        if annotations.is_empty() {
            return;
        }
        let node_index: std::collections::HashMap<&str, &Node> = graph
            .nodes
            .iter()
            .map(|node| (node.id.as_str(), node))
            .collect();

        annotate_symbol_info(&mut self.symbol, &node_index, annotations);
        for symbol in self
            .callers
            .iter_mut()
            .chain(self.callees.iter_mut())
            .chain(self.reads.iter_mut())
            .chain(self.read_by.iter_mut())
            .chain(self.invalidation_sources.iter_mut())
            .chain(self.contains.iter_mut())
            .chain(self.contained_by.iter_mut())
            .chain(self.implementors.iter_mut())
            .chain(self.implements.iter_mut())
            .chain(self.type_refs.iter_mut())
        {
            annotate_symbol_ref(symbol, &node_index, annotations);
        }
        for symbol in &mut self.contains_tree {
            annotate_symbol_tree_ref(symbol, &node_index, annotations);
        }
    }
}

fn annotate_symbol_info(
    symbol: &mut SymbolInfo,
    node_index: &std::collections::HashMap<&str, &Node>,
    annotations: &AnnotationIndex,
) {
    symbol.annotation = node_index
        .get(symbol.id.as_str())
        .and_then(|node| annotations.get_for_node(node));
}

fn annotate_symbol_ref(
    symbol: &mut SymbolRef,
    node_index: &std::collections::HashMap<&str, &Node>,
    annotations: &AnnotationIndex,
) {
    symbol.annotation = node_index
        .get(symbol.id.as_str())
        .and_then(|node| annotations.get_for_node(node));
}

fn annotate_symbol_tree_ref(
    symbol: &mut SymbolTreeRef,
    node_index: &std::collections::HashMap<&str, &Node>,
    annotations: &AnnotationIndex,
) {
    symbol.annotation = node_index
        .get(symbol.id.as_str())
        .and_then(|node| annotations.get_for_node(node));
    for child in &mut symbol.contains {
        annotate_symbol_tree_ref(child, node_index, annotations);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapha_core::graph::{NodeRole, Span, Visibility};
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn make_node(id: &str, name: &str, kind: NodeKind, file: &str) -> Node {
        Node {
            id: id.into(),
            kind,
            name: name.into(),
            file: PathBuf::from(file),
            span: Span {
                start: [0, 0],
                end: [1, 0],
            },
            visibility: Visibility::Public,
            metadata: HashMap::new(),
            role: None::<NodeRole>,
            signature: None,
            doc_comment: None,
            module: None,
            snippet: None,
            repo: None,
        }
    }

    #[test]
    fn bare_send_gift_prefers_functions_over_variants_and_properties() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                make_node(
                    "variant-id",
                    "sendGift",
                    NodeKind::Variant,
                    "FamilyServiceCore.swift",
                ),
                make_node(
                    "property-id",
                    "sendGift",
                    NodeKind::Property,
                    "GiftView.swift",
                ),
                make_node(
                    "function-id",
                    "sendGift(req:)",
                    NodeKind::Function,
                    "GiftServiceCore.swift",
                ),
            ],
            edges: vec![],
        };

        let resolved = resolve_node(&graph, "sendGift").unwrap();
        assert_eq!(resolved.id, "function-id");
    }

    #[test]
    fn bare_send_gift_returns_ambiguous_when_functions_share_top_rank() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                make_node(
                    "function-1",
                    "sendGift(req:)",
                    NodeKind::Function,
                    "GiftServiceCore.swift",
                ),
                make_node(
                    "function-2",
                    "sendGift(goods:targetId:)",
                    NodeKind::Function,
                    "StoreModule.swift",
                ),
                make_node(
                    "variant-id",
                    "sendGift",
                    NodeKind::Variant,
                    "HeadlineData.swift",
                ),
            ],
            edges: vec![],
        };

        let err = resolve_node(&graph, "sendGift").unwrap_err();
        match err {
            QueryResolveError::Ambiguous { query, candidates } => {
                assert_eq!(query, "sendGift");
                assert_eq!(candidates.len(), 2);
                assert!(
                    candidates
                        .iter()
                        .all(|candidate| candidate.kind == NodeKind::Function)
                );
            }
            other => panic!("expected ambiguity, got {other:?}"),
        }
    }

    #[test]
    fn swift_file_symbol_query_matches_against_node_file_suffix() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                make_node(
                    "s:12ModuleExport15GiftServiceCoreC04sendC03reqy...",
                    "sendGift(req:)",
                    NodeKind::Function,
                    "GiftServiceCore.swift",
                ),
                make_node(
                    "s:5Store0A6ModuleC8sendGift5goods8targetIdy...",
                    "sendGift(goods:targetId:)",
                    NodeKind::Function,
                    "StoreModule.swift",
                ),
            ],
            edges: vec![],
        };

        let resolved = resolve_node(&graph, "GiftServiceCore.swift::sendGift").unwrap();
        assert_eq!(resolved.file, PathBuf::from("GiftServiceCore.swift"));
    }

    #[test]
    fn bare_symbol_prefers_real_declarations_over_swiftui_synthetic_nodes() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                make_node(
                    "ContentView.swift::ContentView::body::view:Row@10:12",
                    "Row",
                    NodeKind::View,
                    "ContentView.swift",
                ),
                make_node(
                    "ContentView.swift::Row",
                    "Row",
                    NodeKind::Struct,
                    "ContentView.swift",
                ),
            ],
            edges: vec![],
        };

        let resolved = resolve_node(&graph, "Row").unwrap();
        assert_eq!(resolved.kind, NodeKind::Struct);
        assert_eq!(resolved.id, "ContentView.swift::Row");
    }

    #[test]
    fn bare_symbol_prefers_real_declarations_over_framework_components() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                make_node(
                    "component:ContentView.swift:Row:1",
                    "Row",
                    NodeKind::Component,
                    "ContentView.swift",
                ),
                make_node(
                    "ContentView.swift::Row",
                    "Row",
                    NodeKind::Struct,
                    "ContentView.swift",
                ),
            ],
            edges: vec![],
        };

        let resolved = resolve_node(&graph, "Row").unwrap();
        assert_eq!(resolved.kind, NodeKind::Struct);
        assert_eq!(resolved.id, "ContentView.swift::Row");
    }

    #[test]
    fn rust_style_locator_resolves_member() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                make_node("type-id", "Test", NodeKind::Struct, "Hello.swift"),
                make_node(
                    "method-id",
                    "hello(name:)",
                    NodeKind::Function,
                    "Hello.swift",
                ),
            ],
            edges: vec![grapha_core::graph::Edge {
                source: "type-id".to_string(),
                target: "method-id".to_string(),
                kind: grapha_core::graph::EdgeKind::Contains,
                confidence: 1.0,
                direction: None,
                operation: None,
                condition: None,
                async_boundary: None,
                provenance: Vec::new(),
                repo: None,
            }],
        };
        let mut graph = graph;
        graph.nodes[0].module = Some("ModuleExport".to_string());
        graph.nodes[1].module = Some("ModuleExport".to_string());

        let resolved = resolve_node(&graph, "ModuleExport::Hello.swift::Test::hello(name:)")
            .expect("locator should resolve");
        assert_eq!(resolved.id, "method-id");
    }

    #[test]
    fn strict_file_match_requires_full_path_or_suffix() {
        let node_file = PathBuf::from("Modules/Room/Sources/Room/View/RoomPage.swift");

        assert!(file_matches_path_or_suffix(&node_file, "RoomPage.swift"));
        assert!(file_matches_path_or_suffix(
            &node_file,
            "Modules/Room/Sources/Room/View/RoomPage.swift"
        ));
        assert!(!file_matches_path_or_suffix(&node_file, "Page"));
    }
}
