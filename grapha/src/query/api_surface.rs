use std::collections::{HashMap, HashSet};

use grapha_core::graph::{EdgeKind, Graph, Node, NodeKind, Visibility};
use serde::Serialize;

use crate::symbol_locator::SymbolLocatorIndex;

use super::{QueryResolveError, SymbolInfo, SymbolRef, normalize_symbol_name};

#[derive(Debug, Clone, Copy)]
pub struct ApiSurfaceOptions {
    pub include_private: bool,
}

impl Default for ApiSurfaceOptions {
    fn default() -> Self {
        Self {
            include_private: false,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ApiSurfaceResult {
    pub symbol: SymbolInfo,
    pub members: Vec<ApiMember>,
    pub total_members: usize,
    pub referenced_types: Vec<SymbolRef>,
    pub total_referenced_types: usize,
}

#[derive(Debug, Serialize)]
pub struct ApiMember {
    pub symbol: SymbolRef,
    pub owner: SymbolRef,
}

fn to_symbol_ref(node: &Node, locators: &SymbolLocatorIndex) -> SymbolRef {
    SymbolRef::from_node(node).with_locator(locators.locator_for_node(node))
}

fn to_symbol_info(node: &Node, locators: &SymbolLocatorIndex) -> SymbolInfo {
    SymbolInfo::from_node(node).with_locator(locators.locator_for_node(node))
}

fn is_api_member_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Function
            | NodeKind::Method
            | NodeKind::Property
            | NodeKind::Field
            | NodeKind::Constant
            | NodeKind::TypeAlias
            | NodeKind::Struct
            | NodeKind::Class
            | NodeKind::Enum
            | NodeKind::Protocol
            | NodeKind::Trait
    )
}

fn is_visible_member(node: &Node, options: ApiSurfaceOptions) -> bool {
    options.include_private || node.visibility != Visibility::Private
}

fn owner_node_mentions_type(owner: &Node, target: &Node) -> bool {
    matches!(owner.kind, NodeKind::Extension | NodeKind::Impl)
        && (normalize_symbol_name(&owner.name) == normalize_symbol_name(&target.name)
            || owner.name.contains(&target.name))
}

pub fn query_api_surface(
    graph: &Graph,
    symbol: &str,
    options: ApiSurfaceOptions,
) -> Result<ApiSurfaceResult, QueryResolveError> {
    let target = crate::query::resolve_node(graph, symbol)?;
    let locators = SymbolLocatorIndex::new(graph);
    let node_index: HashMap<&str, &Node> = graph
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();

    let mut contains_by_owner: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut referenced_by_member: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut owner_ids: HashSet<&str> = HashSet::from([target.id.as_str()]);

    for edge in &graph.edges {
        if edge.kind == EdgeKind::Contains {
            contains_by_owner
                .entry(edge.source.as_str())
                .or_default()
                .push(edge.target.as_str());
        }

        if matches!(
            edge.kind,
            EdgeKind::TypeRef | EdgeKind::Returns | EdgeKind::TypeOf
        ) {
            referenced_by_member
                .entry(edge.source.as_str())
                .or_default()
                .push(edge.target.as_str());
        }

        if matches!(edge.kind, EdgeKind::Extends | EdgeKind::TypeRef)
            && edge.target == target.id
            && let Some(source) = node_index.get(edge.source.as_str()).copied()
            && matches!(source.kind, NodeKind::Extension | NodeKind::Impl)
        {
            owner_ids.insert(source.id.as_str());
        }
    }

    for node in &graph.nodes {
        if owner_node_mentions_type(node, target) {
            owner_ids.insert(node.id.as_str());
        }
    }

    let mut members = Vec::new();
    let mut member_ids = HashSet::new();
    let mut referenced_type_ids = HashSet::new();

    let mut sorted_owner_ids: Vec<&str> = owner_ids.into_iter().collect();
    sorted_owner_ids.sort_unstable();
    for owner_id in sorted_owner_ids {
        let Some(owner) = node_index.get(owner_id).copied() else {
            continue;
        };
        let Some(child_ids) = contains_by_owner.get(owner_id) else {
            continue;
        };
        for child_id in child_ids {
            let Some(member) = node_index.get(*child_id).copied() else {
                continue;
            };
            if !is_api_member_kind(member.kind) || !is_visible_member(member, options) {
                continue;
            }
            if !member_ids.insert(member.id.as_str()) {
                continue;
            }
            if let Some(type_ids) = referenced_by_member.get(member.id.as_str()) {
                referenced_type_ids.extend(type_ids.iter().copied());
            }
            members.push(ApiMember {
                symbol: to_symbol_ref(member, &locators),
                owner: to_symbol_ref(owner, &locators),
            });
        }
    }

    members.sort_by(|left, right| {
        left.symbol
            .file
            .cmp(&right.symbol.file)
            .then_with(|| left.symbol.span.cmp(&right.symbol.span))
            .then_with(|| left.symbol.name.cmp(&right.symbol.name))
    });

    let mut referenced_types: Vec<SymbolRef> = referenced_type_ids
        .into_iter()
        .filter_map(|id| node_index.get(id).copied())
        .map(|node| to_symbol_ref(node, &locators))
        .collect();
    referenced_types.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.file.cmp(&right.file))
            .then_with(|| left.id.cmp(&right.id))
    });

    Ok(ApiSurfaceResult {
        symbol: to_symbol_info(target, &locators),
        total_members: members.len(),
        members,
        total_referenced_types: referenced_types.len(),
        referenced_types,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapha_core::graph::{Edge, Span};

    fn node(id: &str, name: &str, kind: NodeKind, visibility: Visibility) -> Node {
        Node {
            id: id.to_string(),
            kind,
            name: name.to_string(),
            file: "src/GameManager.swift".into(),
            span: Span {
                start: [1, 0],
                end: [1, 0],
            },
            visibility,
            metadata: HashMap::new(),
            role: None,
            signature: Some(format!("func {name}()")),
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
    fn api_surface_includes_extension_members_and_referenced_types() {
        let graph = Graph {
            version: String::new(),
            nodes: vec![
                node("type", "GameManager", NodeKind::Class, Visibility::Public),
                node(
                    "member",
                    "fetchReward()",
                    NodeKind::Function,
                    Visibility::Public,
                ),
                node(
                    "hidden",
                    "debugOnly()",
                    NodeKind::Function,
                    Visibility::Private,
                ),
                node(
                    "extension",
                    "GameManager",
                    NodeKind::Extension,
                    Visibility::Public,
                ),
                node(
                    "extension_member",
                    "presentDialog()",
                    NodeKind::Function,
                    Visibility::Crate,
                ),
                node(
                    "reward",
                    "GameRewardInfo",
                    NodeKind::Struct,
                    Visibility::Public,
                ),
            ],
            edges: vec![
                edge("type", "member", EdgeKind::Contains),
                edge("type", "hidden", EdgeKind::Contains),
                edge("extension", "extension_member", EdgeKind::Contains),
                edge("extension_member", "reward", EdgeKind::Returns),
            ],
        };

        let result =
            query_api_surface(&graph, "GameManager", ApiSurfaceOptions::default()).unwrap();

        let names: Vec<_> = result
            .members
            .iter()
            .map(|member| member.symbol.name.as_str())
            .collect();
        assert_eq!(names, vec!["fetchReward()", "presentDialog()"]);
        assert_eq!(result.referenced_types[0].name, "GameRewardInfo");
    }

    #[test]
    fn api_surface_can_include_private_members() {
        let graph = Graph {
            version: String::new(),
            nodes: vec![
                node("type", "GameManager", NodeKind::Class, Visibility::Public),
                node(
                    "hidden",
                    "debugOnly()",
                    NodeKind::Function,
                    Visibility::Private,
                ),
            ],
            edges: vec![edge("type", "hidden", EdgeKind::Contains)],
        };

        let result = query_api_surface(
            &graph,
            "GameManager",
            ApiSurfaceOptions {
                include_private: true,
            },
        )
        .unwrap();

        assert_eq!(result.members[0].symbol.name, "debugOnly()");
    }
}
