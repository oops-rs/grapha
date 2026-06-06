use std::collections::{HashMap, HashSet};

use grapha_core::extract::ExtractionResult;
use grapha_core::graph::{EdgeKind, Graph, NodeKind};

/// Entry in the name-to-candidates index: (node_id, module).
struct NameEntry {
    id: String,
    module: Option<String>,
}

/// Merge multiple `ExtractionResult`s into a single `Graph`.
///
/// Edges whose target matches a known node ID are kept as-is.
/// Uses edges are always kept (external references).
/// For unresolved targets, cross-file resolution is attempted by matching
/// the symbol name (last `::` segment) against all known nodes.
///
/// Resolution priority:
/// 1. Same-module, unique candidate (confidence 0.9x)
/// 2. Same-module, narrowed by access-chain hint (confidence 0.85x)
/// 3. Imported-module, unique candidate (confidence 0.8x)
/// 4. Single unambiguous candidate in a different module with import (confidence 0.7x)
/// 5. Multiple same-module candidates — keep all (confidence 0.4x)
/// 6. No viable candidates — drop edge
pub fn merge(results: Vec<ExtractionResult>) -> Graph {
    let mut graph = Graph::new();

    // Build file → set of imported module names from import statements.
    let mut file_imports: HashMap<String, HashSet<String>> = HashMap::new();
    for r in &results {
        for import in &r.imports {
            if let Some(first_node) = r.nodes.first() {
                let file_key = first_node.file.to_string_lossy().to_string();
                let module_name = import
                    .path
                    .strip_prefix("import ")
                    .unwrap_or(&import.path)
                    .to_string();
                file_imports
                    .entry(file_key)
                    .or_default()
                    .insert(module_name);
            }
        }
    }

    for r in &results {
        graph.nodes.extend(r.nodes.iter().cloned());
    }

    let node_ids: HashSet<&str> = graph.nodes.iter().map(|n| n.id.as_str()).collect();

    // Build name → vec of (node_id, module) for cross-file lookup
    let mut name_to_entries: HashMap<&str, Vec<NameEntry>> = HashMap::new();
    for node in &graph.nodes {
        if matches!(node.kind, NodeKind::View | NodeKind::Branch) {
            continue;
        }
        name_to_entries
            .entry(node.name.as_str())
            .or_default()
            .push(NameEntry {
                id: node.id.clone(),
                module: node.module.clone(),
            });
    }

    // Build node_id → (module, file) for source lookups
    let id_to_info: HashMap<&str, (Option<&str>, &str)> = graph
        .nodes
        .iter()
        .map(|n| {
            (
                n.id.as_str(),
                (n.module.as_deref(), n.file.to_str().unwrap_or("")),
            )
        })
        .collect();

    // Build candidate_id → owner type names from structural edges for hint-based disambiguation.
    let id_to_name: HashMap<&str, &str> = graph
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n.name.as_str()))
        .collect();
    let mut candidate_to_owner_names: HashMap<String, Vec<String>> = HashMap::new();
    for r in &results {
        for edge in &r.edges {
            if edge.kind == EdgeKind::Contains
                && let Some(parent_name) = id_to_name.get(edge.source.as_str())
            {
                candidate_to_owner_names
                    .entry(edge.target.clone())
                    .or_default()
                    .push(parent_name.to_string());
            } else if edge.kind == EdgeKind::Implements
                && let Some(owner_name) = id_to_name.get(edge.target.as_str())
            {
                candidate_to_owner_names
                    .entry(edge.source.clone())
                    .or_default()
                    .push(owner_name.to_string());
            }
        }
    }

    // Collect all edges to process (need to borrow results for candidate_to_owner_names above)
    let all_edges: Vec<_> = results.into_iter().flat_map(|r| r.edges).collect();

    for mut edge in all_edges {
        // Keep edges that resolve to known nodes, or are Uses/Implements (external refs),
        // or are USR-based calls to external frameworks (for terminal classification)
        let is_external_usr_call = edge.target.starts_with("s:")
            && edge.kind == EdgeKind::Calls
            && !node_ids.contains(edge.target.as_str());

        if node_ids.contains(edge.target.as_str())
            || edge.kind == EdgeKind::Uses
            || edge.kind == EdgeKind::Implements
            || is_external_usr_call
        {
            graph.edges.push(edge);
        } else {
            let target_name = edge.target.rsplit("::").next().unwrap_or(&edge.target);
            if let Some(candidates) = name_to_entries.get(target_name) {
                if candidates.is_empty() {
                    continue;
                }

                let (source_module, source_file) = id_to_info
                    .get(edge.source.as_str())
                    .copied()
                    .unwrap_or((None, ""));
                let source_imports = file_imports.get(source_file);
                let prefix_hint = edge.operation.as_deref();

                if candidates.len() == 1 {
                    let candidate = &candidates[0];
                    let same_module = modules_match(source_module, candidate.module.as_deref());
                    if same_module {
                        edge.target = candidate.id.clone();
                        edge.confidence *= 0.9;
                        graph.edges.push(edge);
                    } else {
                        let imported = source_imports
                            .and_then(|imports| {
                                candidate.module.as_deref().map(|m| imports.contains(m))
                            })
                            .unwrap_or(false);
                        if imported {
                            edge.target = candidate.id.clone();
                            edge.confidence *= 0.7;
                            graph.edges.push(edge);
                        }
                    }
                } else {
                    let resolved = resolve_candidates(
                        candidates,
                        source_module,
                        source_imports,
                        prefix_hint,
                        &candidate_to_owner_names,
                    );

                    for (candidate_id, factor) in resolved {
                        let mut resolved_edge = edge.clone();
                        resolved_edge.target = candidate_id;
                        resolved_edge.confidence *= factor;
                        graph.edges.push(resolved_edge);
                    }
                }
            }
        }
    }

    graph
}

/// Pick the best candidate(s) from multiple matches.
/// Returns a vec of (candidate_id, confidence_factor).
fn resolve_candidates(
    candidates: &[NameEntry],
    source_module: Option<&str>,
    source_imports: Option<&HashSet<String>>,
    prefix_hint: Option<&str>,
    candidate_to_owner_names: &HashMap<String, Vec<String>>,
) -> Vec<(String, f64)> {
    // 1. Same-module candidates
    let same_module: Vec<&NameEntry> = candidates
        .iter()
        .filter(|c| modules_match(source_module, c.module.as_deref()))
        .collect();
    if same_module.len() == 1 {
        return vec![(same_module[0].id.clone(), 0.9)];
    }

    // 1b. Multiple same-module — try prefix hint disambiguation
    if same_module.len() > 1 {
        if let Some(hint) = prefix_hint {
            // Last segment of the prefix is the receiver name (e.g., "gift" from "AppContext.gift")
            let hint_name = hint.rsplit('.').next().unwrap_or(hint).to_lowercase();
            let exact: Vec<&&NameEntry> = same_module
                .iter()
                .filter(|c| {
                    candidate_to_owner_names.get(&c.id).is_some_and(|parents| {
                        parents.iter().any(|p| p.eq_ignore_ascii_case(&hint_name))
                    })
                })
                .collect();
            if exact.len() == 1 {
                return vec![(exact[0].id.clone(), 0.85)];
            }

            let narrowed: Vec<&&NameEntry> = same_module
                .iter()
                .filter(|c| {
                    candidate_to_owner_names.get(&c.id).is_some_and(|parents| {
                        parents
                            .iter()
                            .any(|p| p.to_lowercase().contains(&hint_name))
                    })
                })
                .collect();
            if narrowed.len() == 1 {
                return vec![(narrowed[0].id.clone(), 0.85)];
            }
        }

        // Fallback: keep all same-module candidates with low confidence
        return same_module.iter().map(|c| (c.id.clone(), 0.4)).collect();
    }

    // 2. Imported-module candidates
    if let Some(imports) = source_imports {
        let imported: Vec<&NameEntry> = candidates
            .iter()
            .filter(|c| c.module.as_deref().is_some_and(|m| imports.contains(m)))
            .collect();
        if imported.len() == 1 {
            return vec![(imported[0].id.clone(), 0.8)];
        }
        if imported.len() > 1 {
            return imported.iter().map(|c| (c.id.clone(), 0.3)).collect();
        }
    }

    // 3. No viable candidates — drop
    Vec::new()
}

/// Check if two modules match. Two `None` modules are considered matching
/// (both in the default/root module).
fn modules_match(a: Option<&str>, b: Option<&str>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a == b,
        (None, None) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapha_core::graph::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn make_node(id: &str, name: &str, kind: NodeKind) -> Node {
        Node {
            id: id.to_string(),
            kind,
            name: name.to_string(),
            file: PathBuf::from("test.rs"),
            span: Span {
                start: [0, 0],
                end: [0, 0],
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
    fn merges_nodes_from_multiple_results() {
        let r1 = ExtractionResult {
            nodes: vec![make_node("a::Foo", "Foo", NodeKind::Struct)],
            edges: vec![],
            imports: vec![],
        };
        let r2 = ExtractionResult {
            nodes: vec![make_node("b::Bar", "Bar", NodeKind::Struct)],
            edges: vec![],
            imports: vec![],
        };
        let graph = merge(vec![r1, r2]);
        assert_eq!(graph.nodes.len(), 2);
    }

    #[test]
    fn drops_edges_with_unresolved_targets() {
        let r1 = ExtractionResult {
            nodes: vec![make_node("a::main", "main", NodeKind::Function)],
            edges: vec![Edge {
                source: "a::main".to_string(),
                target: "nonexistent::foo".to_string(),
                kind: EdgeKind::Calls,
                confidence: 1.0,
                direction: None,
                operation: None,
                condition: None,
                async_boundary: None,
                provenance: Vec::new(),
                repo: None,
            }],
            imports: vec![],
        };
        let graph = merge(vec![r1]);
        assert_eq!(graph.edges.len(), 0);
    }

    #[test]
    fn keeps_edges_with_resolved_targets() {
        let r1 = ExtractionResult {
            nodes: vec![
                make_node("a::main", "main", NodeKind::Function),
                make_node("a::helper", "helper", NodeKind::Function),
            ],
            edges: vec![Edge {
                source: "a::main".to_string(),
                target: "a::helper".to_string(),
                kind: EdgeKind::Calls,
                confidence: 1.0,
                direction: None,
                operation: None,
                condition: None,
                async_boundary: None,
                provenance: Vec::new(),
                repo: None,
            }],
            imports: vec![],
        };
        let graph = merge(vec![r1]);
        assert_eq!(graph.edges.len(), 1);
    }

    #[test]
    fn resolves_cross_file_edges() {
        let r1 = ExtractionResult {
            nodes: vec![make_node("a::main", "main", NodeKind::Function)],
            edges: vec![Edge {
                source: "a::main".to_string(),
                target: "b::helper".to_string(),
                kind: EdgeKind::Calls,
                confidence: 1.0,
                direction: None,
                operation: None,
                condition: None,
                async_boundary: None,
                provenance: Vec::new(),
                repo: None,
            }],
            imports: vec![],
        };
        let r2 = ExtractionResult {
            nodes: vec![make_node("b::helper", "helper", NodeKind::Function)],
            edges: vec![],
            imports: vec![],
        };
        let graph = merge(vec![r1, r2]);
        assert_eq!(graph.edges.len(), 1);
    }

    #[test]
    fn keeps_uses_edges_even_if_target_unresolved() {
        let r1 = ExtractionResult {
            nodes: vec![],
            edges: vec![Edge {
                source: "a.rs".to_string(),
                target: "use std::collections::HashMap;".to_string(),
                kind: EdgeKind::Uses,
                confidence: 1.0,
                direction: None,
                operation: None,
                condition: None,
                async_boundary: None,
                provenance: Vec::new(),
                repo: None,
            }],
            imports: vec![],
        };
        let graph = merge(vec![r1]);
        assert_eq!(graph.edges.len(), 1);
    }

    #[test]
    fn cross_module_resolution_gets_lower_confidence() {
        // Two nodes with the same name in different modules
        let mut node_a = make_node("mod_a::helper", "helper", NodeKind::Function);
        node_a.module = Some("mod_a".to_string());

        let mut node_b = make_node("mod_b::helper", "helper", NodeKind::Function);
        node_b.module = Some("mod_b".to_string());

        let mut caller = make_node("mod_a::main", "main", NodeKind::Function);
        caller.module = Some("mod_a".to_string());

        let r1 = ExtractionResult {
            nodes: vec![caller],
            edges: vec![Edge {
                source: "mod_a::main".to_string(),
                target: "unknown::helper".to_string(),
                kind: EdgeKind::Calls,
                confidence: 1.0,
                direction: None,
                operation: None,
                condition: None,
                async_boundary: None,
                provenance: Vec::new(),
                repo: None,
            }],
            imports: vec![],
        };
        let r2 = ExtractionResult {
            nodes: vec![node_a, node_b],
            edges: vec![],
            imports: vec![],
        };

        let graph = merge(vec![r1, r2]);
        let call_edge = graph
            .edges
            .iter()
            .find(|e| e.kind == EdgeKind::Calls)
            .expect("should have a call edge");

        // Multiple candidates but same-module match uniquely preferred → 0.9x
        assert_eq!(call_edge.target, "mod_a::helper");
        assert!(
            (call_edge.confidence - 0.9).abs() < 0.001,
            "expected 0.9, got {}",
            call_edge.confidence
        );
    }

    #[test]
    fn cross_module_single_candidate_different_module() {
        let mut node = make_node("mod_b::helper", "helper", NodeKind::Function);
        node.module = Some("mod_b".to_string());

        let mut caller = make_node("mod_a::main", "main", NodeKind::Function);
        caller.module = Some("mod_a".to_string());

        let r1 = ExtractionResult {
            nodes: vec![caller],
            edges: vec![Edge {
                source: "mod_a::main".to_string(),
                target: "unknown::helper".to_string(),
                kind: EdgeKind::Calls,
                confidence: 1.0,
                direction: None,
                operation: None,
                condition: None,
                async_boundary: None,
                provenance: Vec::new(),
                repo: None,
            }],
            imports: vec![],
        };
        let r2 = ExtractionResult {
            nodes: vec![node],
            edges: vec![],
            imports: vec![],
        };

        let graph = merge(vec![r1, r2]);
        let call_edge = graph.edges.iter().find(|e| e.kind == EdgeKind::Calls);

        // Single candidate, different module, no import → edge dropped
        assert!(
            call_edge.is_none(),
            "cross-module edge without import should be dropped"
        );
    }

    #[test]
    fn same_module_single_candidate_gets_highest_confidence() {
        let mut node = make_node("mod_a::helper", "helper", NodeKind::Function);
        node.module = Some("mod_a".to_string());

        let mut caller = make_node("mod_a::main", "main", NodeKind::Function);
        caller.module = Some("mod_a".to_string());

        let r1 = ExtractionResult {
            nodes: vec![caller],
            edges: vec![Edge {
                source: "mod_a::main".to_string(),
                target: "unknown::helper".to_string(),
                kind: EdgeKind::Calls,
                confidence: 1.0,
                direction: None,
                operation: None,
                condition: None,
                async_boundary: None,
                provenance: Vec::new(),
                repo: None,
            }],
            imports: vec![],
        };
        let r2 = ExtractionResult {
            nodes: vec![node],
            edges: vec![],
            imports: vec![],
        };

        let graph = merge(vec![r1, r2]);
        let call_edge = graph
            .edges
            .iter()
            .find(|e| e.kind == EdgeKind::Calls)
            .expect("should have a call edge");

        // Single candidate, same module → 0.9x
        assert_eq!(call_edge.target, "mod_a::helper");
        assert!(
            (call_edge.confidence - 0.9).abs() < 0.001,
            "expected 0.9, got {}",
            call_edge.confidence
        );
    }

    #[test]
    fn resolves_cross_file_calls_by_name() {
        let r1 = ExtractionResult {
            nodes: vec![make_node("a.rs::main", "main", NodeKind::Function)],
            edges: vec![Edge {
                source: "a.rs::main".to_string(),
                target: "a.rs::helper".to_string(),
                kind: EdgeKind::Calls,
                confidence: 0.8,
                direction: None,
                operation: None,
                condition: None,
                async_boundary: None,
                provenance: Vec::new(),
                repo: None,
            }],
            imports: vec![],
        };
        let r2 = ExtractionResult {
            nodes: vec![make_node("b.rs::helper", "helper", NodeKind::Function)],
            edges: vec![],
            imports: vec![],
        };
        let graph = merge(vec![r1, r2]);
        // Edge target should be rewritten to b.rs::helper
        let call_edge = graph.edges.iter().find(|e| e.kind == EdgeKind::Calls);
        assert!(call_edge.is_some());
        let e = call_edge.unwrap();
        assert_eq!(e.target, "b.rs::helper");
        assert!(e.confidence < 0.8); // reduced confidence
    }

    #[test]
    fn owner_hint_disambiguates_same_module_helper_view_refs() {
        let mut room_page_ext = make_node("room::RoomPageExt", "RoomPage", NodeKind::Extension);
        room_page_ext.module = Some("Room".to_string());
        let mut kroom_page_ext = make_node("room::KRoomPageExt", "KRoomPage", NodeKind::Extension);
        kroom_page_ext.module = Some("Room".to_string());

        let mut room_helper = make_node(
            "room::RoomPage::chatRoomFragViewPanel",
            "chatRoomFragViewPanel",
            NodeKind::Property,
        );
        room_helper.module = Some("Room".to_string());
        let mut kroom_helper = make_node(
            "room::KRoomPage::chatRoomFragViewPanel",
            "chatRoomFragViewPanel",
            NodeKind::Property,
        );
        kroom_helper.module = Some("Room".to_string());

        let mut body_view = make_node(
            "room::RoomPage::body::view:chatRoomFragViewPanel",
            "chatRoomFragViewPanel",
            NodeKind::View,
        );
        body_view.module = Some("Room".to_string());

        let source = body_view.id.clone();
        let r1 = ExtractionResult {
            nodes: vec![body_view],
            edges: vec![Edge {
                source: source.clone(),
                target: "room::RoomPage.swift::chatRoomFragViewPanel".to_string(),
                kind: EdgeKind::TypeRef,
                confidence: 1.0,
                direction: None,
                operation: Some("RoomPage".to_string()),
                condition: None,
                async_boundary: None,
                provenance: Vec::new(),
                repo: None,
            }],
            imports: vec![],
        };
        let r2 = ExtractionResult {
            nodes: vec![room_page_ext, kroom_page_ext, room_helper, kroom_helper],
            edges: vec![
                Edge {
                    source: "room::RoomPage::chatRoomFragViewPanel".to_string(),
                    target: "room::RoomPageExt".to_string(),
                    kind: EdgeKind::Implements,
                    confidence: 1.0,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: Vec::new(),
                    repo: None,
                },
                Edge {
                    source: "room::KRoomPage::chatRoomFragViewPanel".to_string(),
                    target: "room::KRoomPageExt".to_string(),
                    kind: EdgeKind::Implements,
                    confidence: 1.0,
                    direction: None,
                    operation: None,
                    condition: None,
                    async_boundary: None,
                    provenance: Vec::new(),
                    repo: None,
                },
            ],
            imports: vec![],
        };

        let graph = merge(vec![r1, r2]);
        let type_refs: Vec<_> = graph
            .edges
            .iter()
            .filter(|edge| edge.source == source && edge.kind == EdgeKind::TypeRef)
            .collect();

        assert_eq!(type_refs.len(), 1);
        assert_eq!(type_refs[0].target, "room::RoomPage::chatRoomFragViewPanel");
        assert!((type_refs[0].confidence - 0.85).abs() < 0.001);
    }
}
