use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;
use serde::Serialize;

use crate::fields::FieldSet;
use crate::snippet::{LineIndex, trim_snippet_indentation};
use grapha_core::graph::{EdgeKind, Graph, Node, NodeKind, NodeRole, TerminalKind};
use ignore::WalkBuilder;

use super::flow::{is_dataflow_edge, terminal_kind_to_string};
use super::{QueryResolveError, SymbolRef, normalize_symbol_name, strip_accessor_prefix};

/// Cap for the `origins` vector returned by [`query_origin_with_options`].
///
/// The default leaves results unbounded (matching prior behavior); the CLI
/// overrides this with the user-supplied `--limit`. `total_origins` always
/// reflects the pre-truncation count so callers can detect truncation.
#[derive(Debug, Clone)]
pub struct OriginQueryOptions {
    pub limit: usize,
}

impl Default for OriginQueryOptions {
    fn default() -> Self {
        // Internal callers stay unbounded.
        Self { limit: usize::MAX }
    }
}

#[derive(Debug, Serialize)]
pub struct OriginResult {
    pub symbol: String,
    pub origins: Vec<OriginPath>,
    /// Pre-`--limit` count of origins (after `--terminal-kind` filtering, if
    /// any). Use this to detect whether the `--limit` flag truncated the
    /// caller-visible `origins` vector. Distinct from [`Self::truncated`],
    /// which signals the BFS exploration cap.
    pub total_origins: usize,
    /// Set when the internal BFS exploration cap was hit
    /// (`max_explored_states` or `max_origins`). Orthogonal to the
    /// `--limit` flag and [`Self::total_origins`].
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    #[serde(skip)]
    pub target_ref: SymbolRef,
}

#[derive(Debug, Clone, Serialize)]
pub struct OriginPath {
    pub api: SymbolRef,
    pub terminal_kind: String,
    pub path: Vec<String>,
    pub field_candidates: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub code_snippets: Vec<OriginSnippet>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_method: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub request_keys: Vec<String>,
    pub confidence: f32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OriginSnippet {
    pub symbol: SymbolRef,
    pub reason: String,
    pub snippet: String,
}

#[derive(Debug, Clone)]
struct EndpointEvidence {
    endpoint: String,
    request_method: Option<String>,
    request_keys: Vec<String>,
}

struct StackFrame<'a> {
    node_id: &'a str,
    path_ids: Vec<&'a str>,
    visited: HashSet<&'a str>,
}

fn to_symbol_ref(node: &Node) -> SymbolRef {
    SymbolRef::from_node(node)
}

fn terminal_kind(node: &Node) -> Option<TerminalKind> {
    match node.role {
        Some(NodeRole::Terminal { kind }) => Some(kind),
        _ => None,
    }
}

fn is_origin_terminal(node: &Node) -> bool {
    terminal_kind(node).is_some()
}

fn fieldish_name(node: &Node) -> Option<String> {
    match node.kind {
        NodeKind::Property | NodeKind::Field | NodeKind::Constant => {
            let name = normalize_symbol_name(&node.name).trim();
            (!name.is_empty() && name != "body").then(|| name.to_string())
        }
        NodeKind::Function => {
            let stripped = strip_accessor_prefix(&node.name);
            let normalized = normalize_symbol_name(stripped).trim();
            (stripped != node.name && !normalized.is_empty() && normalized != "body")
                .then(|| normalized.to_string())
        }
        _ => None,
    }
}

fn type_label_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(
            r"(?m)\b(?:struct|class|enum|protocol|extension|typealias)\s+([A-Za-z_][A-Za-z0-9_]*)",
        )
        .expect("valid type label regex")
    })
}

fn type_label(node: &Node) -> Option<String> {
    node.snippet
        .as_deref()
        .map(str::trim)
        .and_then(|snippet| {
            type_label_regex()
                .captures(snippet)
                .and_then(|captures| captures.get(1))
                .map(|value| value.as_str().to_string())
        })
        .or_else(|| {
            let normalized = normalize_symbol_name(&node.name).trim();
            (!normalized.is_empty()).then(|| normalized.to_string())
        })
}

fn parse_alias_targets(snippet: &str) -> Vec<String> {
    let Some((_, rhs)) = snippet.split_once('=') else {
        return Vec::new();
    };
    rhs.split('&')
        .filter_map(|part| {
            let token = part
                .trim()
                .trim_end_matches('{')
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_');
            (!token.is_empty()).then(|| token.to_string())
        })
        .collect()
}

fn request_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r#"request(?:[A-Za-z0-9_]*)?\(\s*"([^"]+)""#).expect("valid request regex")
    })
}

fn request_method_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r#"method:\s*\.([A-Za-z_][A-Za-z0-9_]*)"#).expect("valid method regex")
    })
}

fn request_key_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r#""([A-Za-z_][A-Za-z0-9_]*)"\s*:"#).expect("valid key regex"))
}

fn parse_request_evidence(snippet: &str) -> Option<EndpointEvidence> {
    let captures = request_regex().captures(snippet)?;
    let endpoint = captures.get(1)?.as_str().to_string();

    let request_method = request_method_regex()
        .captures(snippet)
        .and_then(|captures| captures.get(1))
        .map(|value| value.as_str().to_string());

    let data_start = snippet.find("data:");
    let request_keys = if let Some(start) = data_start {
        let payload = &snippet[start..];
        let mut keys: Vec<String> = request_key_regex()
            .captures_iter(payload)
            .filter_map(|captures| captures.get(1).map(|value| value.as_str().to_string()))
            .collect();
        keys.sort();
        keys.dedup();
        keys
    } else {
        Vec::new()
    };

    Some(EndpointEvidence {
        endpoint,
        request_method,
        request_keys,
    })
}

fn expand_interface_labels<'a>(
    label: &'a str,
    alias_targets: &'a HashMap<String, Vec<String>>,
) -> Vec<String> {
    let mut result = Vec::new();
    let mut stack = vec![label.to_string()];
    let mut seen = HashSet::new();

    while let Some(current) = stack.pop() {
        if !seen.insert(current.clone()) {
            continue;
        }
        result.push(current.clone());
        if let Some(targets) = alias_targets.get(&current) {
            stack.extend(targets.iter().rev().cloned());
        }
    }

    result
}

fn label_from_symbol_id(symbol_id: &str) -> Option<String> {
    let tail = symbol_id.rsplit("::").next().unwrap_or(symbol_id).trim();
    let normalized = normalize_symbol_name(tail).trim();
    (!normalized.is_empty()).then(|| normalized.to_string())
}

fn is_property_family(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Property | NodeKind::Field | NodeKind::Constant
    )
}

fn is_accessor_function(node: &Node) -> bool {
    node.kind == NodeKind::Function && strip_accessor_prefix(&node.name) != node.name
}

fn member_key(node: &Node) -> Option<String> {
    match node.kind {
        NodeKind::Function => {
            let stripped = strip_accessor_prefix(&node.name).trim();
            if stripped.is_empty() {
                None
            } else if is_accessor_function(node) {
                Some(normalize_symbol_name(stripped).to_string())
            } else {
                Some(stripped.to_string())
            }
        }
        NodeKind::Property | NodeKind::Field | NodeKind::Constant => {
            let normalized = normalize_symbol_name(&node.name).trim();
            (!normalized.is_empty()).then(|| normalized.to_string())
        }
        _ => None,
    }
}

fn member_kind_matches(abstract_member: &Node, implementor_member: &Node) -> bool {
    abstract_member.kind == implementor_member.kind
        || (is_property_family(abstract_member.kind) && is_accessor_function(implementor_member))
        || (is_accessor_function(abstract_member) && is_property_family(implementor_member.kind))
}

fn allows_origin_type_ref(source_node: &Node, target_node: &Node) -> bool {
    is_property_family(source_node.kind) || is_property_family(target_node.kind)
}

fn is_sibling_overload_crossover<'a>(
    path_ids: &[&'a str],
    target_node: &Node,
    node_index: &HashMap<&'a str, &'a Node>,
    member_owner_by_id: &HashMap<&'a str, String>,
) -> bool {
    if target_node.kind != NodeKind::Function {
        return false;
    }

    let target_name = normalize_symbol_name(&target_node.name);
    let Some(current_id) = path_ids.last().copied() else {
        return false;
    };
    let Some(current_node) = node_index.get(current_id).copied() else {
        return false;
    };
    if normalize_symbol_name(&current_node.name) == target_name {
        return false;
    }

    path_ids.iter().any(|node_id| {
        let Some(node) = node_index.get(*node_id).copied() else {
            return false;
        };
        let owner_matches = match (
            member_owner_by_id.get(node.id.as_str()),
            member_owner_by_id.get(target_node.id.as_str()),
        ) {
            (Some(node_owner), Some(target_owner)) => node_owner == target_owner,
            _ => true,
        };
        node.kind == NodeKind::Function
            && node.id != target_node.id
            && normalize_symbol_name(&node.name) == target_name
            && owner_matches
    })
}

fn is_function_root_property_detour(
    root_node: &Node,
    current_node: &Node,
    target_node: &Node,
) -> bool {
    root_node.kind == NodeKind::Function
        && target_node.kind == NodeKind::Function
        && (is_property_family(current_node.kind) || is_accessor_function(current_node))
}

fn normalized_origin_api_name(origin: &OriginPath) -> String {
    normalize_symbol_name(&origin.api.name).to_string()
}

fn origin_specificity_rank(origin: &OriginPath) -> (usize, usize, usize) {
    let has_endpoint = usize::from(origin.endpoint.is_some());
    let request_named = usize::from(normalized_origin_api_name(origin).starts_with("request"));
    let has_request_keys = usize::from(!origin.request_keys.is_empty());
    (has_endpoint, request_named, has_request_keys)
}

fn choose_preferred_origin<'a>(
    current: &'a OriginPath,
    candidate: &'a OriginPath,
) -> &'a OriginPath {
    let candidate_ordering = origin_specificity_rank(candidate)
        .cmp(&origin_specificity_rank(current))
        .then_with(|| {
            candidate
                .confidence
                .partial_cmp(&current.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .then_with(|| current.api.name.cmp(&candidate.api.name).reverse());

    if candidate_ordering.is_gt() {
        candidate
    } else {
        current
    }
}

fn merge_equivalent_origins(origins: Vec<OriginPath>) -> Vec<OriginPath> {
    let mut merged: HashMap<(Vec<String>, String, String), OriginPath> = HashMap::new();

    for origin in origins {
        let key = (
            origin.path.clone(),
            normalized_origin_api_name(&origin),
            origin.terminal_kind.clone(),
        );
        match merged.remove(&key) {
            Some(existing) => {
                let preferred = choose_preferred_origin(&existing, &origin).clone();
                merged.insert(key, preferred);
            }
            None => {
                merged.insert(key, origin);
            }
        }
    }

    merged.into_values().collect()
}

fn suppress_request_wrapper_siblings(origins: Vec<OriginPath>) -> Vec<OriginPath> {
    let request_prefixes: HashSet<Vec<String>> = origins
        .iter()
        .filter(|origin| {
            origin.path.len() > 1
                && (origin.endpoint.is_some()
                    || normalized_origin_api_name(origin).starts_with("request"))
        })
        .map(|origin| origin.path[..origin.path.len() - 1].to_vec())
        .collect();

    origins
        .into_iter()
        .filter(|origin| {
            if origin.path.len() <= 1 {
                return true;
            }

            let prefix = origin.path[..origin.path.len() - 1].to_vec();
            let api_name = normalized_origin_api_name(origin);
            let is_request_specific = origin.endpoint.is_some() || api_name.starts_with("request");
            let is_generic_wrapper = api_name.starts_with("with") || api_name.starts_with("_get");

            is_request_specific || !is_generic_wrapper || !request_prefixes.contains(&prefix)
        })
        .collect()
}

fn candidate_field_paths(path_nodes: &[&Node]) -> Vec<String> {
    let mut names = Vec::new();
    for node in path_nodes {
        if let Some(name) = fieldish_name(node)
            && names.last() != Some(&name)
        {
            names.push(name);
        }
    }

    let mut candidates = Vec::new();
    if !names.is_empty() {
        candidates.push(names.join("."));
        if names.len() > 1 {
            candidates.push(names[names.len() - 2..].join("."));
        }
        candidates.push(names[names.len() - 1].clone());
    }
    candidates.sort();
    candidates.dedup();
    candidates
}

fn path_display(path_nodes: &[&Node]) -> Vec<String> {
    let mut path = Vec::new();
    for node in path_nodes {
        let display = match node.kind {
            NodeKind::Function => strip_accessor_prefix(&node.name).to_string(),
            _ => normalize_symbol_name(&node.name).to_string(),
        };
        if path.last() != Some(&display) {
            path.push(display);
        }
    }
    path
}

fn load_cached_source<'a>(
    path: &Path,
    source_cache: &'a mut HashMap<PathBuf, Option<String>>,
) -> Option<&'a str> {
    source_cache
        .entry(path.to_path_buf())
        .or_insert_with(|| std::fs::read_to_string(path).ok())
        .as_deref()
}

fn find_source_path_under_root(root: &Path, needle: &Path) -> Option<PathBuf> {
    let file_name = needle.file_name()?;
    let mut basename_match = None;

    for entry in WalkBuilder::new(root).hidden(false).build() {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        if path.ends_with(needle) {
            return Some(path.to_path_buf());
        }

        if basename_match.is_none() && path.file_name() == Some(file_name) {
            basename_match = Some(path.to_path_buf());
        }
    }

    basename_match
}

fn resolve_source_path(
    node: &Node,
    project_root: Option<&Path>,
    path_cache: &mut HashMap<PathBuf, PathBuf>,
) -> PathBuf {
    if let Some(cached) = path_cache.get(&node.file) {
        return cached.clone();
    }

    if node.file.is_absolute() {
        path_cache.insert(node.file.clone(), node.file.clone());
        node.file.clone()
    } else if let Some(root) = project_root {
        let direct = root.join(&node.file);
        let resolved = if direct.exists() {
            direct
        } else {
            find_source_path_under_root(root, &node.file).unwrap_or(direct)
        };
        path_cache.insert(node.file.clone(), resolved.clone());
        resolved
    } else {
        path_cache.insert(node.file.clone(), node.file.clone());
        node.file.clone()
    }
}

fn resolve_node_snippet(
    node: &Node,
    project_root: Option<&Path>,
    snippet_cache: &mut HashMap<String, Option<String>>,
    source_cache: &mut HashMap<PathBuf, Option<String>>,
    path_cache: &mut HashMap<PathBuf, PathBuf>,
) -> Option<String> {
    if let Some(cached) = snippet_cache.get(&node.id) {
        return cached.clone();
    }

    let resolved_path = resolve_source_path(node, project_root, path_cache);
    let snippet = load_cached_source(&resolved_path, source_cache)
        .and_then(|source| {
            let index = LineIndex::new(source);
            index.extract_symbol_snippet(&node.span, &node.name, node.kind)
        })
        .or_else(|| node.snippet.clone())
        .map(|snippet| trim_snippet_indentation(&snippet));

    snippet_cache.insert(node.id.clone(), snippet.clone());
    snippet
}

fn push_origin_snippet(
    snippets: &mut Vec<OriginSnippet>,
    node: &Node,
    reason: &str,
    project_root: Option<&Path>,
    snippet_cache: &mut HashMap<String, Option<String>>,
    source_cache: &mut HashMap<PathBuf, Option<String>>,
    path_cache: &mut HashMap<PathBuf, PathBuf>,
) {
    let Some(snippet) =
        resolve_node_snippet(node, project_root, snippet_cache, source_cache, path_cache)
    else {
        return;
    };
    if snippet.trim().is_empty() {
        return;
    }
    if snippets.iter().any(|entry| entry.symbol.id == node.id) {
        return;
    }
    snippets.push(OriginSnippet {
        symbol: to_symbol_ref(node),
        reason: reason.to_string(),
        snippet,
    });
}

fn collect_origin_code_snippets(
    path_nodes: &[&Node],
    endpoint: Option<&EndpointEvidence>,
    project_root: Option<&Path>,
    snippet_cache: &mut HashMap<String, Option<String>>,
    source_cache: &mut HashMap<PathBuf, Option<String>>,
    path_cache: &mut HashMap<PathBuf, PathBuf>,
) -> Vec<OriginSnippet> {
    const MAX_SNIPPETS: usize = 3;

    let mut snippets = Vec::new();
    if let Some(root) = path_nodes.first().copied() {
        push_origin_snippet(
            &mut snippets,
            root,
            "query_symbol",
            project_root,
            snippet_cache,
            source_cache,
            path_cache,
        );
    }

    if let Some(field_node) = path_nodes
        .iter()
        .copied()
        .skip(1)
        .take(path_nodes.len().saturating_sub(2))
        .find(|node| is_property_family(node.kind) || is_accessor_function(node))
    {
        push_origin_snippet(
            &mut snippets,
            field_node,
            "path_symbol",
            project_root,
            snippet_cache,
            source_cache,
            path_cache,
        );
    }

    if let Some(leaf) = path_nodes.last().copied() {
        let reason = if endpoint.is_some() {
            "request_leaf"
        } else {
            "terminal_leaf"
        };
        push_origin_snippet(
            &mut snippets,
            leaf,
            reason,
            project_root,
            snippet_cache,
            source_cache,
            path_cache,
        );
    }

    snippets.truncate(MAX_SNIPPETS);
    snippets
}

fn confidence_for(path_nodes: &[&Node], field_candidates: &[String]) -> f32 {
    let mut confidence = 0.35f32;
    if !field_candidates.is_empty() {
        confidence += 0.25;
    }
    if path_nodes.len() <= 6 {
        confidence += 0.15;
    }
    if path_nodes
        .iter()
        .any(|node| matches!(node.kind, NodeKind::Property | NodeKind::Field))
    {
        confidence += 0.15;
    }
    if path_nodes.iter().any(|node| {
        node.snippet
            .as_deref()
            .is_some_and(|snippet| snippet.contains("request("))
    }) {
        confidence += 0.15;
    }
    if path_nodes
        .iter()
        .any(|node| matches!(terminal_kind(node), Some(TerminalKind::Network)))
    {
        confidence += 0.15;
    } else if path_nodes.iter().any(|node| is_origin_terminal(node)) {
        confidence += 0.05;
    }
    confidence.min(0.95)
}

fn notes_for(
    path_nodes: &[&Node],
    field_candidates: &[String],
    endpoint: Option<&EndpointEvidence>,
) -> Vec<String> {
    let mut notes = Vec::new();
    if let Some(endpoint) = endpoint {
        notes.push(format!("reached request endpoint {}", endpoint.endpoint));
        if let Some(method) = &endpoint.request_method {
            notes.push(format!("request method {}", method));
        }
    } else if let Some(terminal_node) = path_nodes
        .iter()
        .rev()
        .find(|node| is_origin_terminal(node))
        && let Some(kind) = terminal_kind(terminal_node)
    {
        notes.push(format!(
            "reached {} terminal {}",
            terminal_kind_to_string(&kind),
            terminal_node.name
        ));
    }
    if let Some(candidate) = field_candidates.first() {
        notes.push(format!("candidate field path {}", candidate));
    }
    if path_nodes.iter().any(|node| {
        node.kind == NodeKind::Function && strip_accessor_prefix(&node.name) != node.name
    }) {
        notes.push("path crosses accessor/computed-property logic".to_string());
    }
    notes
}

fn resolve_endpoint_evidence(
    node: &Node,
    project_root: Option<&Path>,
    endpoint_cache: &mut HashMap<String, Option<EndpointEvidence>>,
    snippet_cache: &mut HashMap<String, Option<String>>,
    source_cache: &mut HashMap<PathBuf, Option<String>>,
    path_cache: &mut HashMap<PathBuf, PathBuf>,
) -> Option<EndpointEvidence> {
    if let Some(cached) = endpoint_cache.get(&node.id) {
        return cached.clone();
    }

    let endpoint =
        resolve_node_snippet(node, project_root, snippet_cache, source_cache, path_cache)
            .as_deref()
            .and_then(parse_request_evidence);
    endpoint_cache.insert(node.id.clone(), endpoint.clone());
    endpoint
}

#[cfg(test)]
pub fn query_origin(
    graph: &Graph,
    symbol: &str,
    max_depth: usize,
) -> Result<OriginResult, QueryResolveError> {
    // Internal callers stay unbounded.
    query_origin_with_limits(graph, symbol, max_depth, None, 20_000, 200)
        .map(|result| apply_origin_limit(result, &OriginQueryOptions::default()))
}

/// Build the unbounded origin result for `symbol`.
///
/// `--limit` is intentionally NOT applied here — the dispatcher must apply
/// [`apply_origin_limit`] AFTER any post-query filters (e.g.
/// [`filter_origin_result_by_terminal_kind`]) so that filter-then-truncate
/// returns the correct number of post-filter origins instead of pre-filter
/// origins that may be entirely discarded by the filter.
///
/// `OriginResult.total_origins` is set to the pre-filter origin count by this
/// function. Filters may overwrite it to the post-filter count.
pub fn query_origin_with_path_and_options(
    graph: &Graph,
    symbol: &str,
    max_depth: usize,
    project_root: Option<&Path>,
    _options: &OriginQueryOptions,
) -> Result<OriginResult, QueryResolveError> {
    query_origin_with_limits(graph, symbol, max_depth, project_root, 20_000, 200)
}

/// Truncate `result.origins` to `options.limit` without mutating
/// `result.total_origins`, so callers retain the pre-truncation count.
///
/// Apply this LAST in the pipeline (after [`filter_origin_result_by_terminal_kind`])
/// so the limit operates on the filtered, user-visible set.
pub fn apply_origin_limit(mut result: OriginResult, options: &OriginQueryOptions) -> OriginResult {
    result.origins.truncate(options.limit);
    result
}

fn query_origin_with_limits(
    graph: &Graph,
    symbol: &str,
    max_depth: usize,
    project_root: Option<&Path>,
    max_explored_states: usize,
    max_origins: usize,
) -> Result<OriginResult, QueryResolveError> {
    let root_node = crate::query::resolve_node(graph, symbol)?;
    let node_index: HashMap<&str, &Node> = graph.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut contains_parent: HashMap<&str, &str> = HashMap::new();
    for edge in &graph.edges {
        if edge.kind == EdgeKind::Contains {
            contains_parent.insert(edge.target.as_str(), edge.source.as_str());
        }
    }

    let mut alias_targets: HashMap<String, Vec<String>> = HashMap::new();
    let mut members_by_owner: HashMap<String, Vec<&Node>> = HashMap::new();
    let mut member_owner_by_id: HashMap<&str, String> = HashMap::new();
    let mut endpoint_by_node: HashMap<String, Option<EndpointEvidence>> = HashMap::new();
    let mut snippet_by_node: HashMap<String, Option<String>> = HashMap::new();
    let mut source_cache: HashMap<PathBuf, Option<String>> = HashMap::new();
    let mut path_cache: HashMap<PathBuf, PathBuf> = HashMap::new();

    for node in &graph.nodes {
        if node.kind == NodeKind::TypeAlias
            && let Some(label) = type_label(node)
        {
            let targets = node
                .snippet
                .as_deref()
                .map(parse_alias_targets)
                .unwrap_or_default();
            if !targets.is_empty() {
                alias_targets.insert(label, targets);
            }
        }

        if matches!(
            node.kind,
            NodeKind::Function | NodeKind::Property | NodeKind::Constant
        ) && let Some(parent_id) = contains_parent.get(node.id.as_str())
            && let Some(parent_node) = node_index.get(parent_id).copied()
            && let Some(owner_label) = type_label(parent_node)
        {
            member_owner_by_id.insert(node.id.as_str(), owner_label.clone());
            members_by_owner.entry(owner_label).or_default().push(node);
        }
    }

    let mut implementor_labels_by_interface: HashMap<String, HashSet<String>> = HashMap::new();
    for edge in &graph.edges {
        if edge.kind != EdgeKind::Implements {
            continue;
        }

        let Some(source_node) = node_index.get(edge.source.as_str()).copied() else {
            continue;
        };
        let Some(owner_label) = type_label(source_node) else {
            continue;
        };
        let Some(interface_label) = node_index
            .get(edge.target.as_str())
            .copied()
            .and_then(type_label)
            .or_else(|| label_from_symbol_id(edge.target.as_str()))
        else {
            continue;
        };

        for expanded_label in expand_interface_labels(&interface_label, &alias_targets) {
            implementor_labels_by_interface
                .entry(expanded_label)
                .or_default()
                .insert(owner_label.clone());
        }
    }

    let mut traversal_adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in &graph.edges {
        if !node_index.contains_key(edge.source.as_str())
            || !node_index.contains_key(edge.target.as_str())
        {
            continue;
        }

        if is_dataflow_edge(edge.kind) {
            traversal_adj
                .entry(edge.source.as_str())
                .or_default()
                .push(edge.target.as_str());
            if matches!(edge.kind, EdgeKind::Calls | EdgeKind::Writes) {
                let Some(source_node) = node_index.get(edge.source.as_str()).copied() else {
                    continue;
                };
                let Some(target_node) = node_index.get(edge.target.as_str()).copied() else {
                    continue;
                };
                if source_node.file == target_node.file && !is_accessor_function(target_node) {
                    traversal_adj
                        .entry(edge.target.as_str())
                        .or_default()
                        .push(edge.source.as_str());
                }
            }
        } else if edge.kind == EdgeKind::TypeRef {
            let Some(source_node) = node_index.get(edge.source.as_str()).copied() else {
                continue;
            };
            let Some(target_node) = node_index.get(edge.target.as_str()).copied() else {
                continue;
            };
            if source_node.file != target_node.file {
                continue;
            }
            if !allows_origin_type_ref(source_node, target_node) {
                continue;
            }

            traversal_adj
                .entry(edge.source.as_str())
                .or_default()
                .push(edge.target.as_str());
            traversal_adj
                .entry(edge.target.as_str())
                .or_default()
                .push(edge.source.as_str());
        } else if edge.kind == EdgeKind::Implements {
            traversal_adj
                .entry(edge.target.as_str())
                .or_default()
                .push(edge.source.as_str());
        }
    }

    for (interface_label, implementor_labels) in &implementor_labels_by_interface {
        let abstract_owners = expand_interface_labels(interface_label, &alias_targets);

        for owner_label in abstract_owners {
            let Some(abstract_members) = members_by_owner.get(&owner_label) else {
                continue;
            };

            for abstract_member in abstract_members {
                let Some(abstract_key) = member_key(abstract_member) else {
                    continue;
                };

                for implementor_label in implementor_labels {
                    let Some(impl_members) = members_by_owner.get(implementor_label) else {
                        continue;
                    };
                    for impl_member in impl_members {
                        if !member_kind_matches(abstract_member, impl_member) {
                            continue;
                        }
                        let Some(impl_key) = member_key(impl_member) else {
                            continue;
                        };
                        if impl_key != abstract_key {
                            continue;
                        }
                        traversal_adj
                            .entry(abstract_member.id.as_str())
                            .or_default()
                            .push(impl_member.id.as_str());
                    }
                }
            }
        }
    }

    for targets in traversal_adj.values_mut() {
        targets.sort_unstable();
        targets.dedup();
    }

    let mut queue = VecDeque::from([StackFrame {
        node_id: root_node.id.as_str(),
        path_ids: vec![root_node.id.as_str()],
        visited: HashSet::from([root_node.id.as_str()]),
    }]);
    let mut origins_by_api: HashMap<&str, OriginPath> = HashMap::new();
    let mut best_depth_by_node: HashMap<&str, usize> = HashMap::from([(root_node.id.as_str(), 0)]);
    let mut explored_states = 0usize;
    let mut truncated = false;

    while let Some(frame) = queue.pop_front() {
        explored_states += 1;
        if explored_states > max_explored_states || origins_by_api.len() >= max_origins {
            truncated = true;
            break;
        }

        let Some(node) = node_index.get(frame.node_id).copied() else {
            continue;
        };
        let endpoint = resolve_endpoint_evidence(
            node,
            project_root,
            &mut endpoint_by_node,
            &mut snippet_by_node,
            &mut source_cache,
            &mut path_cache,
        );
        let unvisited_targets: Vec<&str> = traversal_adj
            .get(frame.node_id)
            .into_iter()
            .flat_map(|targets| targets.iter().copied())
            .filter(|target_id| !frame.visited.contains(target_id))
            .collect();

        let should_record_origin = endpoint.is_some()
            || matches!(terminal_kind(node), Some(TerminalKind::Network))
            || (is_origin_terminal(node) && unvisited_targets.is_empty());

        if frame.path_ids.len() > 1 && should_record_origin {
            let path_nodes: Vec<&Node> = frame
                .path_ids
                .iter()
                .filter_map(|node_id| node_index.get(*node_id).copied())
                .collect();
            let field_candidates = candidate_field_paths(&path_nodes);
            let code_snippets = collect_origin_code_snippets(
                &path_nodes,
                endpoint.as_ref(),
                project_root,
                &mut snippet_by_node,
                &mut source_cache,
                &mut path_cache,
            );
            let confidence = confidence_for(&path_nodes, &field_candidates);
            let notes = notes_for(&path_nodes, &field_candidates, endpoint.as_ref());
            let origin = OriginPath {
                api: to_symbol_ref(node),
                terminal_kind: endpoint
                    .as_ref()
                    .map(|_| "network".to_string())
                    .unwrap_or_else(|| {
                        terminal_kind_to_string(
                            &terminal_kind(node).expect("terminal checked above"),
                        )
                    }),
                path: path_display(&path_nodes),
                field_candidates,
                code_snippets,
                endpoint: endpoint.as_ref().map(|value| value.endpoint.clone()),
                request_method: endpoint
                    .as_ref()
                    .and_then(|value| value.request_method.clone()),
                request_keys: endpoint
                    .as_ref()
                    .map(|value| value.request_keys.clone())
                    .unwrap_or_default(),
                confidence,
                notes,
            };
            match origins_by_api.get(node.id.as_str()) {
                Some(existing)
                    if existing.confidence > origin.confidence
                        || (existing.confidence == origin.confidence
                            && existing.path.len() <= origin.path.len()) => {}
                _ => {
                    origins_by_api.insert(node.id.as_str(), origin);
                }
            }
            continue;
        }

        if frame.path_ids.len() > max_depth + 1 {
            continue;
        }

        for target_id in unvisited_targets {
            let next_depth = frame.path_ids.len();
            let Some(next_node) = node_index.get(target_id).copied() else {
                continue;
            };
            if is_function_root_property_detour(root_node, node, next_node) {
                continue;
            }
            if is_sibling_overload_crossover(
                &frame.path_ids,
                next_node,
                &node_index,
                &member_owner_by_id,
            ) {
                continue;
            }
            if best_depth_by_node
                .get(target_id)
                .is_some_and(|best_depth| *best_depth <= next_depth)
            {
                continue;
            }
            best_depth_by_node.insert(target_id, next_depth);
            let mut visited = frame.visited.clone();
            visited.insert(target_id);
            let mut path_ids = frame.path_ids.clone();
            path_ids.push(target_id);
            queue.push_back(StackFrame {
                node_id: target_id,
                path_ids,
                visited,
            });
        }
    }

    let mut origins = suppress_request_wrapper_siblings(merge_equivalent_origins(
        origins_by_api.into_values().collect(),
    ));
    origins.sort_by(|left, right| {
        origin_specificity_rank(right)
            .cmp(&origin_specificity_rank(left))
            .then_with(|| {
                right
                    .confidence
                    .partial_cmp(&left.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| left.api.name.cmp(&right.api.name))
            .then_with(|| left.path.len().cmp(&right.path.len()))
    });

    Ok(OriginResult {
        symbol: root_node.id.clone(),
        total_origins: origins.len(),
        truncated,
        origins,
        target_ref: SymbolRef::from_node(root_node),
    })
}

pub fn filter_origin_result_by_terminal_kind(
    mut result: OriginResult,
    terminal_kind: Option<&str>,
) -> OriginResult {
    if let Some(terminal_kind) = terminal_kind {
        result
            .origins
            .retain(|origin| origin.terminal_kind == terminal_kind);
        result.total_origins = result.origins.len();
    }
    result
}

pub fn project_origin_result(mut result: OriginResult, fields: FieldSet) -> OriginResult {
    if !fields.snippet {
        for origin in &mut result.origins {
            origin.code_snippets.clear();
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapha_core::graph::{Edge, EdgeKind, FlowDirection, NodeKind, Span, Visibility};
    use std::collections::HashMap as StdHashMap;
    use std::path::PathBuf;

    fn node(id: &str, name: &str, kind: NodeKind, role: Option<NodeRole>) -> Node {
        Node {
            id: id.into(),
            kind,
            name: name.into(),
            file: PathBuf::from("test.swift"),
            span: Span {
                start: [0, 0],
                end: [1, 0],
            },
            visibility: Visibility::Public,
            metadata: StdHashMap::new(),
            role,
            signature: None,
            doc_comment: None,
            module: Some("App".into()),
            snippet: None,
            repo: None,
        }
    }

    fn node_with_snippet(
        id: &str,
        name: &str,
        kind: NodeKind,
        role: Option<NodeRole>,
        snippet: &str,
    ) -> Node {
        let mut node = node(id, name, kind, role);
        node.snippet = Some(snippet.to_string());
        node
    }

    fn edge(source: &str, target: &str, kind: EdgeKind) -> Edge {
        Edge {
            source: source.into(),
            target: target.into(),
            kind,
            confidence: 1.0,
            direction: Some(FlowDirection::Read),
            operation: None,
            condition: None,
            async_boundary: None,
            provenance: Vec::new(),
            repo: None,
        }
    }

    #[test]
    fn origin_finds_network_terminal_and_field_candidates() {
        let graph = Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                node("ui::titleText", "titleText", NodeKind::Property, None),
                node("vm::displayName", "displayName", NodeKind::Property, None),
                node("model::nickname", "nickname", NodeKind::Property, None),
                node(
                    "api::fetchProfile",
                    "fetchProfile",
                    NodeKind::Function,
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Network,
                    }),
                ),
            ],
            edges: vec![
                edge("ui::titleText", "vm::displayName", EdgeKind::Reads),
                edge("vm::displayName", "model::nickname", EdgeKind::Reads),
                edge("model::nickname", "api::fetchProfile", EdgeKind::Reads),
            ],
        };

        let result = query_origin(&graph, "titleText", 10).unwrap();
        assert_eq!(result.total_origins, 1);
        let origin = &result.origins[0];
        assert_eq!(origin.api.name, "fetchProfile");
        assert!(
            origin
                .field_candidates
                .iter()
                .any(|v| v.contains("nickname"))
        );
        assert_eq!(
            origin.path,
            vec!["titleText", "displayName", "nickname", "fetchProfile"]
        );
    }

    #[test]
    fn origin_traverses_property_helper_caller_chain_to_non_network_terminal() {
        let graph = Graph {
            version: "0.1.0".into(),
            nodes: vec![
                node("remark", "remarkName", NodeKind::Property, None),
                node(
                    "helper",
                    "handleUserInfoUpdate(_:)",
                    NodeKind::Function,
                    None,
                ),
                node(
                    "refresh",
                    "refreshUserInfo(force:)",
                    NodeKind::Function,
                    None,
                ),
                node(
                    "fetch",
                    "fetchUserInfo",
                    NodeKind::Function,
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Event,
                    }),
                ),
            ],
            edges: vec![
                edge("remark", "helper", EdgeKind::TypeRef),
                edge("refresh", "helper", EdgeKind::Calls),
                edge("refresh", "fetch", EdgeKind::Calls),
            ],
        };

        let result = query_origin(&graph, "remarkName", 10).unwrap();
        assert_eq!(result.total_origins, 1);
        let origin = &result.origins[0];
        assert_eq!(origin.api.name, "fetchUserInfo");
        assert_eq!(origin.terminal_kind, "event");
        assert!(origin.path.iter().any(|step| step == "remarkName"));
        assert!(
            origin
                .path
                .iter()
                .any(|step| step.starts_with("refreshUserInfo"))
        );
    }

    #[test]
    fn expand_interface_labels_recurses_and_stops_on_cycles() {
        let aliases = HashMap::from([
            (
                "ProfileAPI".to_string(),
                vec!["UserAPI".to_string(), "ServiceEventProtocol".to_string()],
            ),
            (
                "PublicProfileAPI".to_string(),
                vec!["ProfileAPI".to_string()],
            ),
            ("LoopAPI".to_string(), vec!["LoopAPI".to_string()]),
        ]);

        assert_eq!(
            expand_interface_labels("PublicProfileAPI", &aliases),
            vec![
                "PublicProfileAPI".to_string(),
                "ProfileAPI".to_string(),
                "UserAPI".to_string(),
                "ServiceEventProtocol".to_string(),
            ]
        );
        assert_eq!(
            expand_interface_labels("LoopAPI", &aliases),
            vec!["LoopAPI".to_string()]
        );
    }

    #[test]
    fn origin_resolves_typealias_implementor_and_endpoint_without_di_registration() {
        let graph = Graph {
            version: "0.1.0".into(),
            nodes: vec![
                node_with_snippet(
                    "public-profile-api",
                    "PublicProfileAPI",
                    NodeKind::TypeAlias,
                    None,
                    "public typealias PublicProfileAPI = ProfileAPI",
                ),
                node_with_snippet(
                    "typealias-profile-api",
                    "ProfileAPI",
                    NodeKind::TypeAlias,
                    None,
                    "public typealias ProfileAPI = UserAPI & ServiceEventProtocol",
                ),
                node_with_snippet(
                    "user-api",
                    "UserAPI",
                    NodeKind::Protocol,
                    None,
                    "public protocol UserAPI: Sendable {",
                ),
                node_with_snippet(
                    "profile-service-ext",
                    "ProfileService",
                    NodeKind::Extension,
                    None,
                    "extension ProfileService: PublicProfileAPI {",
                ),
                node_with_snippet(
                    "fetch-user-info",
                    "fetchUserInfo(for:with:forceUpdate:withNotification:)",
                    NodeKind::Function,
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Event,
                    }),
                    "func fetchUserInfo(...) async throws -> UserInfo { try await _getUser(id: id, attrs: Array(requestAttributes)) }",
                ),
                node_with_snippet(
                    "abstract-get-user",
                    "_getUser(id:attrs:)",
                    NodeKind::Function,
                    None,
                    "func _getUser(id: UserID, attrs: UserAttributes) async throws(RequestError) -> UserInfo",
                ),
                node_with_snippet(
                    "impl-get-user",
                    "_getUser(id:attrs:)",
                    NodeKind::Function,
                    None,
                    "func _getUser(id: UserID, attrs: [UserCommonConfigType]) async throws(RequestError) -> UserInfo {\n    try await requestGetUser(requestIdentifier)\n}",
                ),
                node_with_snippet(
                    "request-get-user",
                    "requestGetUser(_:)",
                    NodeKind::Function,
                    None,
                    "private func requestGetUser(_ data: SingleUserRequest) async throws(RequestError) -> UserInfo {\n    try await request(\"user/getUserInfoByUid/\\(data.id)\", data: [\"attrs\": data.attrs.map(\\.rawValue).sorted()])\n}",
                ),
                node_with_snippet(
                    "refresh",
                    "refreshUserInfo(force:)",
                    NodeKind::Function,
                    None,
                    "func refreshUserInfo(force: Bool = false) async { let userInfo = try await fetchUserInfo(...) }",
                ),
                node_with_snippet(
                    "handle",
                    "handleUserInfoUpdate(_:)",
                    NodeKind::Function,
                    None,
                    "private func handleUserInfoUpdate(_ userInfo: UserInfo) { self.homeEffect = userInfo.commonConfigInfo?.homeEffect }",
                ),
                node_with_snippet(
                    "home-effect",
                    "homeEffect",
                    NodeKind::Property,
                    None,
                    "@Published private(set) var homeEffect: UserHomeDynamicInfo?",
                ),
            ],
            edges: vec![
                edge("user-api", "abstract-get-user", EdgeKind::Contains),
                edge("profile-service-ext", "impl-get-user", EdgeKind::Contains),
                edge(
                    "profile-service-ext",
                    "request-get-user",
                    EdgeKind::Contains,
                ),
                edge(
                    "profile-service-ext",
                    "public-profile-api",
                    EdgeKind::Implements,
                ),
                edge("refresh", "fetch-user-info", EdgeKind::Calls),
                edge("fetch-user-info", "abstract-get-user", EdgeKind::Calls),
                edge("impl-get-user", "request-get-user", EdgeKind::Calls),
                edge("refresh", "handle", EdgeKind::Calls),
                edge("home-effect", "handle", EdgeKind::TypeRef),
                edge("refresh", "fetch-user-info", EdgeKind::Reads),
            ],
        };

        let result = query_origin(&graph, "homeEffect", 10).unwrap();
        assert_eq!(result.total_origins, 1);
        let origin = &result.origins[0];
        assert_eq!(origin.api.name, "requestGetUser(_:)");
        assert_eq!(
            origin.endpoint.as_deref(),
            Some("user/getUserInfoByUid/\\(data.id)")
        );
        assert_eq!(origin.request_keys, vec!["attrs".to_string()]);
        assert!(origin.path.iter().any(|step| step.starts_with("_getUser")));
        assert!(
            origin
                .path
                .iter()
                .any(|step| step.starts_with("requestGetUser"))
        );
        assert!(
            origin
                .code_snippets
                .iter()
                .any(|snippet| snippet.reason == "query_symbol"
                    && snippet.symbol.name == "homeEffect"
                    && snippet.snippet.contains("var homeEffect"))
        );
        assert!(
            origin
                .code_snippets
                .iter()
                .any(|snippet| snippet.reason == "request_leaf"
                    && snippet.symbol.name == "requestGetUser(_:)"
                    && snippet.snippet.contains("user/getUserInfoByUid"))
        );
    }

    #[test]
    fn origin_returns_multiple_implementor_candidates() {
        let graph = Graph {
            version: "0.1.0".into(),
            nodes: vec![
                node_with_snippet(
                    "user-api",
                    "UserAPI",
                    NodeKind::Protocol,
                    None,
                    "protocol UserAPI {",
                ),
                node_with_snippet(
                    "fetch-user-info",
                    "fetchUserInfo(id:)",
                    NodeKind::Function,
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Event,
                    }),
                    "func fetchUserInfo(id: Int) async throws -> String { try await _getUser(id: id, attrs: []) }",
                ),
                node_with_snippet(
                    "abstract-get-user",
                    "_getUser(id:attrs:)",
                    NodeKind::Function,
                    None,
                    "func _getUser(id: Int, attrs: [String]) async throws -> String",
                ),
                node_with_snippet(
                    "service-a-ext",
                    "ProfileService",
                    NodeKind::Extension,
                    None,
                    "extension ProfileService: UserAPI {",
                ),
                node_with_snippet(
                    "service-b-ext",
                    "MockProfileService",
                    NodeKind::Extension,
                    None,
                    "extension MockProfileService: UserAPI {",
                ),
                node_with_snippet(
                    "impl-get-user-a",
                    "_getUser(id:attrs:)",
                    NodeKind::Function,
                    None,
                    "func _getUser(id: Int, attrs: [String]) async throws -> String { try await requestGetUser(id) }",
                ),
                node_with_snippet(
                    "impl-get-user-b",
                    "_getUser(id:attrs:)",
                    NodeKind::Function,
                    None,
                    "func _getUser(id: Int, attrs: [String]) async throws -> String { try await requestMockUser(id) }",
                ),
                node_with_snippet(
                    "request-a",
                    "requestGetUser(_:)",
                    NodeKind::Function,
                    None,
                    "func requestGetUser(_ id: Int) async throws -> String { try await request(\"user/getUserInfoByUid/\\(id)\") }",
                ),
                node_with_snippet(
                    "request-b",
                    "requestMockUser(_:)",
                    NodeKind::Function,
                    None,
                    "func requestMockUser(_ id: Int) async throws -> String { try await request(\"mock/user/\\(id)\") }",
                ),
            ],
            edges: vec![
                edge("user-api", "abstract-get-user", EdgeKind::Contains),
                edge("service-a-ext", "user-api", EdgeKind::Implements),
                edge("service-b-ext", "user-api", EdgeKind::Implements),
                edge("service-a-ext", "impl-get-user-a", EdgeKind::Contains),
                edge("service-b-ext", "impl-get-user-b", EdgeKind::Contains),
                edge("service-a-ext", "request-a", EdgeKind::Contains),
                edge("service-b-ext", "request-b", EdgeKind::Contains),
                edge("fetch-user-info", "abstract-get-user", EdgeKind::Calls),
                edge("impl-get-user-a", "request-a", EdgeKind::Calls),
                edge("impl-get-user-b", "request-b", EdgeKind::Calls),
            ],
        };

        let result = query_origin(&graph, "fetchUserInfo", 10).unwrap();
        assert_eq!(result.total_origins, 2);
        let endpoints: Vec<&str> = result
            .origins
            .iter()
            .filter_map(|origin| origin.endpoint.as_deref())
            .collect();
        assert!(endpoints.contains(&"user/getUserInfoByUid/\\(id)"));
        assert!(endpoints.contains(&"mock/user/\\(id)"));
    }

    #[test]
    fn origin_traverses_direct_member_implements_edges() {
        let graph = Graph {
            version: "0.1.0".into(),
            nodes: vec![
                node_with_snippet(
                    "fetch-user-info",
                    "fetchUserInfo(id:)",
                    NodeKind::Function,
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Event,
                    }),
                    "func fetchUserInfo(id: Int) async throws -> String { try await _getUser(id: id, attrs: []) }",
                ),
                node_with_snippet(
                    "abstract-get-user",
                    "_getUser(id:attrs:)",
                    NodeKind::Function,
                    None,
                    "func _getUser(id: Int, attrs: [String]) async throws -> String",
                ),
                node_with_snippet(
                    "impl-get-user",
                    "_getUser(id:attrs:)",
                    NodeKind::Function,
                    None,
                    "func _getUser(id: Int, attrs: [String]) async throws -> String { try await requestGetUser(id) }",
                ),
                node_with_snippet(
                    "request-get-user",
                    "requestGetUser(_:)",
                    NodeKind::Function,
                    None,
                    "func requestGetUser(_ id: Int) async throws -> String { try await request(\"user/getUserInfoByUid/\\(id)\") }",
                ),
            ],
            edges: vec![
                edge("fetch-user-info", "abstract-get-user", EdgeKind::Calls),
                edge("impl-get-user", "abstract-get-user", EdgeKind::Implements),
                edge("impl-get-user", "request-get-user", EdgeKind::Calls),
            ],
        };

        let result = query_origin(&graph, "fetchUserInfo", 10).unwrap();
        assert_eq!(result.total_origins, 1);
        let origin = &result.origins[0];
        assert_eq!(origin.api.name, "requestGetUser(_:)");
        assert_eq!(
            origin.endpoint.as_deref(),
            Some("user/getUserInfoByUid/\\(id)")
        );
        assert!(origin.path.iter().any(|step| step.starts_with("_getUser")));
    }

    #[test]
    fn filter_origin_result_by_terminal_kind_keeps_only_matching_origins() {
        let result = OriginResult {
            symbol: "symbol".to_string(),
            origins: vec![
                OriginPath {
                    api: SymbolRef {
                        id: "net".to_string(),
                        locator: None,
                        name: "requestGetUser".to_string(),
                        kind: NodeKind::Function,
                        file: "ProfileService.swift".to_string(),
                        module: None,
                        span: None,
                        visibility: None,
                        signature: None,
                        doc_comment: None,
                        annotation: None,
                        role: None,
                        snippet: None,
                        repo: None,
                    },
                    terminal_kind: "network".to_string(),
                    path: vec!["fetchUserInfo".to_string(), "requestGetUser".to_string()],
                    field_candidates: Vec::new(),
                    code_snippets: Vec::new(),
                    endpoint: Some("user/getUserInfoByUid/\\(id)".to_string()),
                    request_method: None,
                    request_keys: Vec::new(),
                    confidence: 0.95,
                    notes: Vec::new(),
                },
                OriginPath {
                    api: SymbolRef {
                        id: "persist".to_string(),
                        locator: None,
                        name: "getter:uid".to_string(),
                        kind: NodeKind::Function,
                        file: "AppContext.swift".to_string(),
                        module: None,
                        span: None,
                        visibility: None,
                        signature: None,
                        doc_comment: None,
                        annotation: None,
                        role: None,
                        snippet: None,
                        repo: None,
                    },
                    terminal_kind: "persistence".to_string(),
                    path: vec!["fetchUserInfo".to_string(), "uid".to_string()],
                    field_candidates: Vec::new(),
                    code_snippets: Vec::new(),
                    endpoint: None,
                    request_method: None,
                    request_keys: Vec::new(),
                    confidence: 0.8,
                    notes: Vec::new(),
                },
            ],
            total_origins: 2,
            truncated: false,
            target_ref: SymbolRef {
                id: "symbol".to_string(),
                locator: None,
                name: "fetchUserInfo".to_string(),
                kind: NodeKind::Function,
                file: "UserAPI.swift".to_string(),
                module: None,
                span: None,
                visibility: None,
                signature: None,
                doc_comment: None,
                annotation: None,
                role: None,
                snippet: None,
                repo: None,
            },
        };

        let filtered = filter_origin_result_by_terminal_kind(result, Some("network"));
        assert_eq!(filtered.total_origins, 1);
        assert_eq!(filtered.origins[0].terminal_kind, "network");
        assert_eq!(filtered.origins[0].api.name, "requestGetUser");
    }

    #[test]
    fn project_origin_result_hides_code_snippets_by_default() {
        let result = OriginResult {
            symbol: "symbol".to_string(),
            origins: vec![OriginPath {
                api: SymbolRef {
                    id: "net".to_string(),
                    locator: None,
                    name: "requestGetUser".to_string(),
                    kind: NodeKind::Function,
                    file: "ProfileService.swift".to_string(),
                    module: None,
                    span: None,
                    visibility: None,
                    signature: None,
                    doc_comment: None,
                    annotation: None,
                    role: None,
                    snippet: None,
                    repo: None,
                },
                terminal_kind: "network".to_string(),
                path: vec!["fetchUserInfo".to_string(), "requestGetUser".to_string()],
                field_candidates: Vec::new(),
                code_snippets: vec![OriginSnippet {
                    symbol: SymbolRef {
                        id: "leaf".to_string(),
                        locator: None,
                        name: "requestGetUser".to_string(),
                        kind: NodeKind::Function,
                        file: "ProfileService.swift".to_string(),
                        module: None,
                        span: None,
                        visibility: None,
                        signature: None,
                        doc_comment: None,
                        annotation: None,
                        role: None,
                        snippet: None,
                        repo: None,
                    },
                    reason: "request_leaf".to_string(),
                    snippet: "func requestGetUser() {}".to_string(),
                }],
                endpoint: Some("user/getUserInfoByUid/\\(id)".to_string()),
                request_method: None,
                request_keys: Vec::new(),
                confidence: 0.95,
                notes: Vec::new(),
            }],
            total_origins: 1,
            truncated: false,
            target_ref: SymbolRef {
                id: "symbol".to_string(),
                locator: None,
                name: "fetchUserInfo".to_string(),
                kind: NodeKind::Function,
                file: "UserAPI.swift".to_string(),
                module: None,
                span: None,
                visibility: None,
                signature: None,
                doc_comment: None,
                annotation: None,
                role: None,
                snippet: None,
                repo: None,
            },
        };

        let projected = project_origin_result(result, FieldSet::default());
        assert!(projected.origins[0].code_snippets.is_empty());
    }

    #[test]
    fn origin_prefers_shallow_request_path_when_budget_is_tight() {
        let mut nodes = vec![
            node_with_snippet(
                "root",
                "fetchMyNewbieStatus()",
                NodeKind::Function,
                None,
                "func fetchMyNewbieStatus() async { try await fetchUserInfo() }",
            ),
            node_with_snippet(
                "a-short",
                "fetchUserInfo(for:with:forceUpdate:withNotification:)",
                NodeKind::Function,
                Some(NodeRole::Terminal {
                    kind: TerminalKind::Event,
                }),
                "func fetchUserInfo(...) async throws -> UserInfo { try await _getUser(id: id, attrs: attrs) }",
            ),
            node_with_snippet(
                "a-request",
                "requestGetUser(_:)",
                NodeKind::Function,
                None,
                "func requestGetUser(_ id: Int) async throws -> UserInfo { try await request(\"user/getUserInfoByUid/\\(id)\") }",
            ),
            node_with_snippet(
                "z-detour-1",
                "detour1()",
                NodeKind::Function,
                None,
                "func detour1() { detour2() }",
            ),
            node_with_snippet(
                "z-detour-2",
                "detour2()",
                NodeKind::Function,
                None,
                "func detour2() { detour3() }",
            ),
            node_with_snippet(
                "z-detour-3",
                "detour3()",
                NodeKind::Function,
                None,
                "func detour3() { detour4() }",
            ),
            node_with_snippet(
                "z-detour-4",
                "detour4()",
                NodeKind::Function,
                None,
                "func detour4() { detour5() }",
            ),
            node_with_snippet(
                "z-detour-5",
                "detour5()",
                NodeKind::Function,
                None,
                "func detour5() { detour6() }",
            ),
            node_with_snippet(
                "z-detour-6",
                "detour6()",
                NodeKind::Function,
                None,
                "func detour6() { detour7() }",
            ),
            node_with_snippet(
                "z-detour-7",
                "detour7()",
                NodeKind::Function,
                None,
                "func detour7() { detour8() }",
            ),
            node_with_snippet(
                "z-detour-8",
                "detour8()",
                NodeKind::Function,
                None,
                "func detour8() { detour9() }",
            ),
            node_with_snippet(
                "z-detour-9",
                "detour9()",
                NodeKind::Function,
                None,
                "func detour9() {}",
            ),
        ];
        let mut edges = vec![
            edge("root", "a-short", EdgeKind::Calls),
            edge("a-short", "a-request", EdgeKind::Calls),
            edge("root", "z-detour-1", EdgeKind::Calls),
        ];
        for index in 1..9 {
            edges.push(edge(
                &format!("z-detour-{index}"),
                &format!("z-detour-{}", index + 1),
                EdgeKind::Calls,
            ));
        }

        let graph = Graph {
            version: "0.1.0".into(),
            nodes: std::mem::take(&mut nodes),
            edges,
        };

        let result =
            query_origin_with_limits(&graph, "fetchMyNewbieStatus()", 9, None, 4, 200).unwrap();
        assert_eq!(result.total_origins, 1);
        assert_eq!(result.origins[0].api.name, "requestGetUser(_:)");
        assert_eq!(
            result.origins[0].endpoint.as_deref(),
            Some("user/getUserInfoByUid/\\(id)")
        );
    }

    #[test]
    fn merge_equivalent_origins_collapses_overload_like_duplicates() {
        let origins = vec![
            OriginPath {
                api: SymbolRef {
                    id: "a".to_string(),
                    locator: None,
                    name: "_getUser(ids:attrs:)".to_string(),
                    kind: NodeKind::Function,
                    file: "ProfileService.swift".to_string(),
                    module: None,
                    span: None,
                    visibility: None,
                    signature: None,
                    doc_comment: None,
                    annotation: None,
                    role: None,
                    snippet: None,
                    repo: None,
                },
                terminal_kind: "network".to_string(),
                path: vec![
                    "fetchUserInfo".to_string(),
                    "userInfoCache".to_string(),
                    "fetchUserInfo".to_string(),
                    "_getUser".to_string(),
                ],
                field_candidates: vec!["userInfoCache".to_string()],
                code_snippets: Vec::new(),
                endpoint: None,
                request_method: None,
                request_keys: Vec::new(),
                confidence: 0.95,
                notes: Vec::new(),
            },
            OriginPath {
                api: SymbolRef {
                    id: "b".to_string(),
                    locator: None,
                    name: "_getUser(sids:attrs:)".to_string(),
                    kind: NodeKind::Function,
                    file: "ProfileService.swift".to_string(),
                    module: None,
                    span: None,
                    visibility: None,
                    signature: None,
                    doc_comment: None,
                    annotation: None,
                    role: None,
                    snippet: None,
                    repo: None,
                },
                terminal_kind: "network".to_string(),
                path: vec![
                    "fetchUserInfo".to_string(),
                    "userInfoCache".to_string(),
                    "fetchUserInfo".to_string(),
                    "_getUser".to_string(),
                ],
                field_candidates: vec!["userInfoCache".to_string()],
                code_snippets: Vec::new(),
                endpoint: None,
                request_method: None,
                request_keys: Vec::new(),
                confidence: 0.95,
                notes: Vec::new(),
            },
        ];

        let merged = merge_equivalent_origins(origins);
        assert_eq!(merged.len(), 1);
        assert_eq!(normalized_origin_api_name(&merged[0]), "_getUser");
    }

    #[test]
    fn suppress_request_wrapper_siblings_prefers_request_leaf() {
        let origins = vec![
            OriginPath {
                api: SymbolRef {
                    id: "request".to_string(),
                    locator: None,
                    name: "requestGetUser(_:)".to_string(),
                    kind: NodeKind::Function,
                    file: "ProfileService.swift".to_string(),
                    module: None,
                    span: None,
                    visibility: None,
                    signature: None,
                    doc_comment: None,
                    annotation: None,
                    role: None,
                    snippet: None,
                    repo: None,
                },
                terminal_kind: "network".to_string(),
                path: vec![
                    "fetchUserInfo".to_string(),
                    "_getUser".to_string(),
                    "requestGetUser".to_string(),
                ],
                field_candidates: Vec::new(),
                code_snippets: Vec::new(),
                endpoint: None,
                request_method: None,
                request_keys: Vec::new(),
                confidence: 0.65,
                notes: Vec::new(),
            },
            OriginPath {
                api: SymbolRef {
                    id: "wrapper".to_string(),
                    locator: None,
                    name: "withRequestErrorThrowing(_:)".to_string(),
                    kind: NodeKind::Function,
                    file: "ProfileService.swift".to_string(),
                    module: None,
                    span: None,
                    visibility: None,
                    signature: None,
                    doc_comment: None,
                    annotation: None,
                    role: None,
                    snippet: None,
                    repo: None,
                },
                terminal_kind: "network".to_string(),
                path: vec![
                    "fetchUserInfo".to_string(),
                    "_getUser".to_string(),
                    "withRequestErrorThrowing".to_string(),
                ],
                field_candidates: Vec::new(),
                code_snippets: Vec::new(),
                endpoint: None,
                request_method: None,
                request_keys: Vec::new(),
                confidence: 0.65,
                notes: Vec::new(),
            },
        ];

        let suppressed = suppress_request_wrapper_siblings(origins);
        assert_eq!(suppressed.len(), 1);
        assert_eq!(suppressed[0].api.name, "requestGetUser(_:)");
    }

    #[test]
    fn origin_does_not_cross_shared_accessor_into_sibling_overload_family() {
        let graph = Graph {
            version: "0.1.0".into(),
            nodes: vec![
                node_with_snippet(
                    "user-api-ext",
                    "UserAPI",
                    NodeKind::Extension,
                    None,
                    "extension UserAPI {",
                ),
                node_with_snippet(
                    "profile-service-ext",
                    "ProfileService",
                    NodeKind::Extension,
                    None,
                    "extension ProfileService {",
                ),
                node_with_snippet(
                    "fetch-by-id",
                    "fetchUserInfo(for:with:forceUpdate:withNotification:)",
                    NodeKind::Function,
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Event,
                    }),
                    "func fetchUserInfo(...) async throws -> UserInfo { _ = userInfoCache; return try await _getUser(id: id, attrs: attrs) }",
                ),
                node_with_snippet(
                    "cache-getter",
                    "getter:userInfoCache",
                    NodeKind::Function,
                    None,
                    "var userInfoCache: UserInfoCache { cache }",
                ),
                node_with_snippet(
                    "fetch-by-good-id",
                    "fetchUserInfo(goodOrShortID:)",
                    NodeKind::Function,
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Event,
                    }),
                    "func fetchUserInfo(goodOrShortID: String) async throws -> [UserInfo] { _ = userInfoCache; return try await _getUser(sids: [goodOrShortID], attrs: attrs) }",
                ),
                node_with_snippet(
                    "abstract-get-user-id",
                    "_getUser(id:attrs:)",
                    NodeKind::Function,
                    None,
                    "func _getUser(id: UserID, attrs: [UserCommonConfigType]) async throws -> UserInfo",
                ),
                node_with_snippet(
                    "abstract-get-user-sids",
                    "_getUser(sids:attrs:)",
                    NodeKind::Function,
                    None,
                    "func _getUser(sids: [String], attrs: [UserCommonConfigType]) async throws -> [UserInfo]",
                ),
                node_with_snippet(
                    "impl-get-user-id",
                    "_getUser(id:attrs:)",
                    NodeKind::Function,
                    None,
                    "func _getUser(id: UserID, attrs: [UserCommonConfigType]) async throws -> UserInfo { try await requestGetUser(id) }",
                ),
                node_with_snippet(
                    "impl-get-user-sids",
                    "_getUser(sids:attrs:)",
                    NodeKind::Function,
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Network,
                    }),
                    "func _getUser(sids: [String], attrs: [UserCommonConfigType]) async throws -> [UserInfo] { try await requestBatchUsers(sids) }",
                ),
                node_with_snippet(
                    "request-get-user",
                    "requestGetUser(_:)",
                    NodeKind::Function,
                    None,
                    "func requestGetUser(_ id: UserID) async throws -> UserInfo { try await request(\"user/getUserInfoByUid/\\(id)\") }",
                ),
            ],
            edges: vec![
                edge("user-api-ext", "fetch-by-id", EdgeKind::Contains),
                edge("user-api-ext", "cache-getter", EdgeKind::Contains),
                edge("user-api-ext", "fetch-by-good-id", EdgeKind::Contains),
                edge("user-api-ext", "abstract-get-user-id", EdgeKind::Contains),
                edge("user-api-ext", "abstract-get-user-sids", EdgeKind::Contains),
                edge(
                    "profile-service-ext",
                    "impl-get-user-id",
                    EdgeKind::Contains,
                ),
                edge(
                    "profile-service-ext",
                    "impl-get-user-sids",
                    EdgeKind::Contains,
                ),
                edge(
                    "profile-service-ext",
                    "request-get-user",
                    EdgeKind::Contains,
                ),
                edge("fetch-by-id", "cache-getter", EdgeKind::Calls),
                edge("fetch-by-id", "abstract-get-user-id", EdgeKind::Calls),
                edge("fetch-by-good-id", "cache-getter", EdgeKind::Calls),
                edge(
                    "fetch-by-good-id",
                    "abstract-get-user-sids",
                    EdgeKind::Calls,
                ),
                edge(
                    "impl-get-user-id",
                    "abstract-get-user-id",
                    EdgeKind::Implements,
                ),
                edge(
                    "impl-get-user-sids",
                    "abstract-get-user-sids",
                    EdgeKind::Implements,
                ),
                edge("impl-get-user-id", "request-get-user", EdgeKind::Calls),
            ],
        };

        let result = query_origin(&graph, "fetch-by-id", 10).unwrap();
        assert_eq!(result.total_origins, 1);
        let origin = &result.origins[0];
        assert_eq!(origin.api.name, "requestGetUser(_:)");
        assert!(
            origin
                .path
                .iter()
                .all(|step| !step.contains("goodOrShortID"))
        );
        assert!(
            result
                .origins
                .iter()
                .all(|candidate| candidate.api.name != "_getUser(sids:attrs:)")
        );
    }

    #[test]
    fn origin_ignores_function_to_type_to_function_detours() {
        let graph = Graph {
            version: "0.1.0".into(),
            nodes: vec![
                node_with_snippet(
                    "user-api-ext",
                    "UserAPI",
                    NodeKind::Extension,
                    None,
                    "extension UserAPI {",
                ),
                node_with_snippet(
                    "profile-service-ext",
                    "ProfileService",
                    NodeKind::Extension,
                    None,
                    "extension ProfileService {",
                ),
                node_with_snippet(
                    "fetch-by-id",
                    "fetchUserInfo(for:with:forceUpdate:withNotification:)",
                    NodeKind::Function,
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Event,
                    }),
                    "func fetchUserInfo(...) async throws -> UserInfo { return try await _getUser(id: id, attrs: attrs) }",
                ),
                node_with_snippet(
                    "user-id",
                    "UserID",
                    NodeKind::TypeAlias,
                    None,
                    "typealias UserID = Int",
                ),
                node_with_snippet(
                    "user-attrs",
                    "UserAttributes",
                    NodeKind::TypeAlias,
                    None,
                    "typealias UserAttributes = [UserCommonConfigType]",
                ),
                node_with_snippet(
                    "abstract-get-user-id",
                    "_getUser(id:attrs:)",
                    NodeKind::Function,
                    None,
                    "func _getUser(id: UserID, attrs: UserAttributes) async throws -> UserInfo",
                ),
                node_with_snippet(
                    "abstract-get-user-sids",
                    "_getUser(sids:attrs:)",
                    NodeKind::Function,
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Network,
                    }),
                    "func _getUser(sids: [String], attrs: UserAttributes) async throws -> [UserInfo]",
                ),
                node_with_snippet(
                    "abstract-get-user-sid",
                    "_getUser(sid:)",
                    NodeKind::Function,
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Network,
                    }),
                    "func _getUser(sid: String) async throws -> UserInfo",
                ),
                node_with_snippet(
                    "impl-get-user-id",
                    "_getUser(id:attrs:)",
                    NodeKind::Function,
                    None,
                    "func _getUser(id: UserID, attrs: UserAttributes) async throws -> UserInfo { try await requestGetUser(id) }",
                ),
                node_with_snippet(
                    "request-get-user",
                    "requestGetUser(_:)",
                    NodeKind::Function,
                    None,
                    "func requestGetUser(_ id: UserID) async throws -> UserInfo { try await request(\"user/getUserInfoByUid/\\(id)\") }",
                ),
            ],
            edges: vec![
                edge("user-api-ext", "fetch-by-id", EdgeKind::Contains),
                edge("user-api-ext", "abstract-get-user-id", EdgeKind::Contains),
                edge("user-api-ext", "abstract-get-user-sids", EdgeKind::Contains),
                edge("user-api-ext", "abstract-get-user-sid", EdgeKind::Contains),
                edge(
                    "profile-service-ext",
                    "impl-get-user-id",
                    EdgeKind::Contains,
                ),
                edge(
                    "profile-service-ext",
                    "request-get-user",
                    EdgeKind::Contains,
                ),
                edge("fetch-by-id", "abstract-get-user-id", EdgeKind::Calls),
                edge(
                    "impl-get-user-id",
                    "abstract-get-user-id",
                    EdgeKind::Implements,
                ),
                edge("impl-get-user-id", "request-get-user", EdgeKind::Calls),
                edge("fetch-by-id", "user-id", EdgeKind::TypeRef),
                edge("fetch-by-id", "user-attrs", EdgeKind::TypeRef),
                edge("abstract-get-user-sids", "user-attrs", EdgeKind::TypeRef),
                edge("abstract-get-user-sid", "user-id", EdgeKind::TypeRef),
            ],
        };

        let result = query_origin(&graph, "fetch-by-id", 10).unwrap();
        assert_eq!(result.total_origins, 1);
        let origin = &result.origins[0];
        assert_eq!(origin.api.name, "requestGetUser(_:)");
        assert!(
            result
                .origins
                .iter()
                .all(|candidate| !candidate.api.name.starts_with("_getUser(sid"))
        );
        assert!(
            result
                .origins
                .iter()
                .all(|candidate| !candidate.api.name.starts_with("_getUser(sids"))
        );
    }

    #[test]
    fn origin_does_not_reverse_call_through_accessor_hubs() {
        let graph = Graph {
            version: "0.1.0".into(),
            nodes: vec![
                node_with_snippet(
                    "fetch",
                    "fetchUserInfo(id:)",
                    NodeKind::Function,
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Event,
                    }),
                    "func fetchUserInfo(id: Int) async throws -> UserInfo { _ = userInfoCache; return try await requestGetUser(id) }",
                ),
                node_with_snippet(
                    "cache-getter",
                    "getter:userInfoCache",
                    NodeKind::Function,
                    None,
                    "var userInfoCache: UserInfoCache { cache }",
                ),
                node_with_snippet(
                    "update",
                    "updateUserInfo(_:)",
                    NodeKind::Function,
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Event,
                    }),
                    "func updateUserInfo(_ info: UserInfo) async throws -> UserInfo { _ = userInfoCache; return try await _updateUser(info: info) }",
                ),
                node_with_snippet(
                    "request",
                    "requestGetUser(_:)",
                    NodeKind::Function,
                    None,
                    "func requestGetUser(_ id: Int) async throws -> UserInfo { try await request(\"user/getUserInfoByUid/\\(id)\") }",
                ),
                node_with_snippet(
                    "network-update",
                    "_updateUser(info:)",
                    NodeKind::Function,
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Network,
                    }),
                    "func _updateUser(info: UserInfo) async throws -> UserInfo",
                ),
            ],
            edges: vec![
                edge("fetch", "cache-getter", EdgeKind::Calls),
                edge("fetch", "request", EdgeKind::Calls),
                edge("update", "cache-getter", EdgeKind::Calls),
                edge("update", "network-update", EdgeKind::Calls),
                edge("cache-getter", "update", EdgeKind::TypeRef),
            ],
        };

        let result = query_origin(&graph, "fetch", 10).unwrap();
        assert_eq!(result.total_origins, 1);
        assert_eq!(result.origins[0].api.name, "requestGetUser(_:)");
        assert!(
            result
                .origins
                .iter()
                .all(|candidate| candidate.api.name != "_updateUser(info:)")
        );
    }

    #[test]
    fn origin_function_root_does_not_reenter_through_shared_property_hub() {
        let graph = Graph {
            version: "0.1.0".into(),
            nodes: vec![
                node_with_snippet(
                    "fetch",
                    "fetchUserInfo(id:)",
                    NodeKind::Function,
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Event,
                    }),
                    "func fetchUserInfo(id: Int) async throws -> UserInfo { return try await requestGetUser(id) }",
                ),
                node_with_snippet(
                    "cache-property",
                    "userInfoCache",
                    NodeKind::Property,
                    None,
                    "var userInfoCache: UserInfoCache",
                ),
                node_with_snippet(
                    "update",
                    "updateUserInfo(_:)",
                    NodeKind::Function,
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Event,
                    }),
                    "func updateUserInfo(_ info: UserInfo) async throws -> UserInfo { return try await _updateUser(info: info) }",
                ),
                node_with_snippet(
                    "request",
                    "requestGetUser(_:)",
                    NodeKind::Function,
                    None,
                    "func requestGetUser(_ id: Int) async throws -> UserInfo { try await request(\"user/getUserInfoByUid/\\(id)\") }",
                ),
                node_with_snippet(
                    "network-update",
                    "_updateUser(info:)",
                    NodeKind::Function,
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Network,
                    }),
                    "func _updateUser(info: UserInfo) async throws -> UserInfo",
                ),
            ],
            edges: vec![
                edge("fetch", "cache-property", EdgeKind::Reads),
                edge("cache-property", "update", EdgeKind::TypeRef),
                edge("fetch", "request", EdgeKind::Calls),
                edge("update", "network-update", EdgeKind::Calls),
            ],
        };

        let result = query_origin(&graph, "fetch", 10).unwrap();
        assert_eq!(result.total_origins, 1);
        assert_eq!(result.origins[0].api.name, "requestGetUser(_:)");
        assert!(
            result
                .origins
                .iter()
                .all(|candidate| candidate.api.name != "_updateUser(info:)")
        );
    }

    #[test]
    fn origin_truncates_origins() {
        let graph = Graph {
            version: "0.1.0".into(),
            nodes: vec![
                node("root", "rootProperty", NodeKind::Property, None),
                node(
                    "fetch_a",
                    "fetchA",
                    NodeKind::Function,
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Network,
                    }),
                ),
                node(
                    "fetch_b",
                    "fetchB",
                    NodeKind::Function,
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Network,
                    }),
                ),
                node(
                    "fetch_c",
                    "fetchC",
                    NodeKind::Function,
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Network,
                    }),
                ),
            ],
            edges: vec![
                edge("root", "fetch_a", EdgeKind::Reads),
                edge("root", "fetch_b", EdgeKind::Reads),
                edge("root", "fetch_c", EdgeKind::Reads),
            ],
        };

        let unbounded = query_origin(&graph, "rootProperty", 10).unwrap();
        assert_eq!(unbounded.origins.len(), 3);
        assert_eq!(unbounded.total_origins, 3);
        // The internal-traversal `truncated` flag tracks the BFS exploration
        // cap (max_explored_states / max_origins) and must remain false here
        // because the fixture is small.
        assert!(
            !unbounded.truncated,
            "unbounded query on a small fixture should not set the BFS-cap truncation flag"
        );

        // `query_origin_with_path_and_options` is intentionally unbounded; the
        // dispatcher applies `apply_origin_limit` after any post-query filter
        // so `--terminal-kind=... --limit=N` truncates the filtered set.
        let opts = OriginQueryOptions { limit: 1 };
        let unfiltered =
            query_origin_with_path_and_options(&graph, "rootProperty", 10, None, &opts).unwrap();
        assert_eq!(unfiltered.origins.len(), 3);
        assert_eq!(unfiltered.total_origins, 3);

        let truncated = apply_origin_limit(unfiltered, &opts);
        assert_eq!(
            truncated.origins.len(),
            1,
            "origins should be truncated by --limit"
        );
        assert_eq!(
            truncated.total_origins, 3,
            "total_origins must remain the pre-truncation count after `apply_origin_limit`"
        );
        // The `truncated: bool` field is the internal-traversal cap, distinct
        // from `--limit` truncation, and must keep its prior false value here.
        assert!(
            !truncated.truncated,
            "--limit truncation must not flip the internal-traversal `truncated` flag"
        );
    }

    #[test]
    fn origin_terminal_kind_filter_then_limit_returns_filtered_count() {
        // Mix of network and persistence origins. With `--terminal-kind=network
        // --limit=2`, the user must see up to 2 network origins (not "<= 2 of
        // any kind, then filtered to network").
        let graph = Graph {
            version: "0.1.0".into(),
            nodes: vec![
                node("root", "rootProperty", NodeKind::Property, None),
                node(
                    "net_a",
                    "fetchA",
                    NodeKind::Function,
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Network,
                    }),
                ),
                node(
                    "net_b",
                    "fetchB",
                    NodeKind::Function,
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Network,
                    }),
                ),
                node(
                    "net_c",
                    "fetchC",
                    NodeKind::Function,
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Network,
                    }),
                ),
                node(
                    "persist_x",
                    "savePersist",
                    NodeKind::Function,
                    Some(NodeRole::Terminal {
                        kind: TerminalKind::Persistence,
                    }),
                ),
            ],
            edges: vec![
                edge("root", "net_a", EdgeKind::Reads),
                edge("root", "net_b", EdgeKind::Reads),
                edge("root", "net_c", EdgeKind::Reads),
                edge("root", "persist_x", EdgeKind::Reads),
            ],
        };

        let opts = OriginQueryOptions { limit: 2 };
        let result =
            query_origin_with_path_and_options(&graph, "rootProperty", 10, None, &opts).unwrap();
        let result = filter_origin_result_by_terminal_kind(result, Some("network"));
        // After filter, total_origins reflects the post-filter count (3 network
        // origins), independent of the upcoming `--limit` truncation.
        assert_eq!(
            result.total_origins, 3,
            "total_origins must reflect post-filter, pre-truncation network count"
        );
        let result = apply_origin_limit(result, &opts);
        assert_eq!(
            result.origins.len(),
            2,
            "limit must truncate the filtered (network-only) set, not pre-filter origins"
        );
        assert!(
            result
                .origins
                .iter()
                .all(|origin| origin.terminal_kind == "network"),
            "every kept origin must be a network origin"
        );
        assert_eq!(
            result.total_origins, 3,
            "total_origins must stay at the post-filter count after `apply_origin_limit`"
        );
    }
}
