use std::collections::HashMap;
use std::time::Instant;

use grapha_core::graph::Node;

use crate::query::{self, QueryCandidate, QueryResolveError};

/// Session-scoped query history that remembers successful symbol resolutions.
/// When a query is ambiguous, recall suggests the previously resolved candidate.
pub struct Recall {
    /// Maps normalized base name → most recently resolved node ID
    history: HashMap<String, RecallEntry>,
}

struct RecallEntry {
    resolved_id: String,
    _timestamp: Instant,
}

impl Recall {
    pub fn new() -> Self {
        Self {
            history: HashMap::new(),
        }
    }

    /// Record a successful resolution for later disambiguation.
    pub fn record(&mut self, query: &str, resolved_id: &str) {
        let base = base_name(query).to_lowercase();
        self.history.insert(
            base,
            RecallEntry {
                resolved_id: resolved_id.to_string(),
                _timestamp: Instant::now(),
            },
        );
    }

    /// Given an ambiguous set of candidates, suggest the one that was previously resolved.
    pub fn suggest(&self, query: &str, candidates: &[QueryCandidate]) -> Option<String> {
        let base = base_name(query).to_lowercase();
        let entry = self.history.get(&base)?;
        // Only suggest if the previously resolved ID is still among the candidates
        candidates
            .iter()
            .find(|c| c.id == entry.resolved_id)
            .map(|c| c.id.clone())
    }

    /// Remove entries whose resolved IDs no longer exist in the graph.
    pub fn prune(&mut self, valid_ids: &std::collections::HashSet<&str>) {
        self.history
            .retain(|_, entry| valid_ids.contains(entry.resolved_id.as_str()));
    }
}

impl Default for Recall {
    fn default() -> Self {
        Self::new()
    }
}

/// Extract the trailing symbol name from a query like "File.swift::symbolName" or a full USR.
fn base_name(query: &str) -> &str {
    // Handle file::symbol syntax
    if let Some((_file, symbol)) = query.rsplit_once("::") {
        return query::normalize_symbol_name(symbol);
    }
    query::normalize_symbol_name(query)
}

/// Resolve a node, using recall to break ties on ambiguous queries.
/// On success, records the resolution for future lookups.
pub fn resolve_with_recall<'a>(
    graph: &'a grapha_core::graph::Graph,
    query_str: &str,
    recall: &mut Recall,
) -> Result<&'a Node, QueryResolveError> {
    match query::resolve_node(graph, query_str) {
        Ok(node) => {
            recall.record(query_str, &node.id);
            Ok(node)
        }
        Err(QueryResolveError::Ambiguous {
            ref query,
            ref candidates,
        }) => {
            if let Some(suggested_id) = recall.suggest(query, candidates)
                && let Some(node) = graph.nodes.iter().find(|n| n.id == suggested_id)
            {
                recall.record(query_str, &node.id);
                return Ok(node);
            }
            Err(QueryResolveError::Ambiguous {
                query: query.clone(),
                candidates: candidates.clone(),
            })
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapha_core::graph::{NodeKind, Span, Visibility};
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
            role: None,
            signature: None,
            doc_comment: None,
            module: None,
            snippet: None,
            repo: None,
        }
    }

    #[test]
    fn recall_resolves_ambiguity_from_history() {
        let graph = grapha_core::graph::Graph {
            version: "0.1.0".to_string(),
            nodes: vec![
                make_node("func-a", "sendGift(req:)", NodeKind::Function, "A.swift"),
                make_node(
                    "func-b",
                    "sendGift(goods:targetId:)",
                    NodeKind::Function,
                    "B.swift",
                ),
            ],
            edges: vec![],
        };

        let mut recall = Recall::new();

        // First call: ambiguous
        let result = resolve_with_recall(&graph, "sendGift", &mut recall);
        assert!(result.is_err());

        // User disambiguates with full query
        let result = resolve_with_recall(&graph, "A.swift::sendGift", &mut recall);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().id, "func-a");

        // Second call with bare name: recall resolves it
        let result = resolve_with_recall(&graph, "sendGift", &mut recall);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().id, "func-a");
    }

    #[test]
    fn prune_removes_stale_entries() {
        let mut recall = Recall::new();
        recall.record("foo", "id-1");
        recall.record("bar", "id-2");

        let valid: std::collections::HashSet<&str> = ["id-1"].into_iter().collect();
        recall.prune(&valid);

        assert!(recall.history.contains_key("foo"));
        assert!(!recall.history.contains_key("bar"));
    }
}
